//! The [`Node`]: resources, IS-04 Node API and IS-05 Connection API handlers,
//! activations, and persistence.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::activation::{
    Activation, ActiveConnection, AutoRequest, Cause, MxlDomain, NodeImplementation, Param, Role,
    SavedConnection, StagedConnection,
};
use crate::address::{first_routable_ipv4, hostname_string, interfaces_json};
use crate::clock::TaiTime;
use crate::ids;
use crate::is05::{parse_patch, ActivationRequest, StagePatch};
use crate::model::{constraints_json, ActivationRecord, Doc, Mode, Model, Pending};
use crate::registration::RegistrationState;
use crate::resources::{Device, Flow, Receiver, ResourceType, Sender, Source};
use crate::settings::Settings;
use crate::Error;

/// An HTTP status and JSON body.
pub(crate) type Reply = (u16, Value);

/// A scheduled activation is prepared this long before its time.
const FINE_WAIT: Duration = Duration::from_millis(2);

/// An NMOS Node. Cheap to clone; all clones share one node.
#[derive(Clone)]
pub struct Node {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub settings: Settings,
    pub implementation: NodeImplementation,
    pub model: Mutex<Model>,
    /// Serialises activations (IS-05: a PATCH during an activation waits for it).
    pub activation_lock: tokio::sync::Mutex<()>,
    pub registration: Mutex<RegistrationState>,
    pub registered: AtomicBool,
    pub shutdown: AtomicBool,
    pub started: AtomicBool,
    pub changed: Notify,
    pub shutdown_signal: tokio::sync::watch::Sender<bool>,
    pub task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("node_id", &self.node_id())
            .finish_non_exhaustive()
    }
}

impl Node {
    /// Create a node. Fails when registration or DNS-SD is on and the announce
    /// address is missing, `0.0.0.0` or loopback (G5).
    pub fn new(settings: Settings, implementation: NodeImplementation) -> Result<Self, Error> {
        let node_id = ids::node_id(
            settings.seed.as_deref(),
            settings.node_id,
            settings.id_path.as_deref(),
        );
        let announces = settings.registration && (settings.registry.is_some() || settings.dns_sd);
        let host = match settings.host.or_else(first_routable_ipv4) {
            Some(ip) if ip.is_loopback() || ip.is_unspecified() => {
                if announces {
                    return Err(Error::InvalidAddress(format!(
                        "{ip} cannot be announced; set NMOS_HOST_ADDRESS to a routable IPv4 address"
                    )));
                }
                ip
            }
            Some(ip) => ip,
            None if announces => {
                return Err(Error::InvalidAddress(
                    "NMOS_HOST_ADDRESS is unset and this host has no routable IPv4 address"
                        .to_string(),
                ))
            }
            None => Ipv4Addr::LOCALHOST,
        };
        let mut model = Model::new(node_id, host, hostname_string(), interfaces_json());
        model.refresh(&settings, implementation.clock.now());
        Ok(Self {
            inner: Arc::new(Inner {
                settings,
                implementation,
                model: Mutex::new(model),
                activation_lock: tokio::sync::Mutex::new(()),
                registration: Mutex::new(RegistrationState::default()),
                registered: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
                started: AtomicBool::new(false),
                changed: Notify::new(),
                shutdown_signal: tokio::sync::watch::channel(false).0,
                task: Mutex::new(None),
            }),
        })
    }

    /// The settings the node was created with.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// The node id.
    pub fn node_id(&self) -> Uuid {
        self.model().node_id
    }

    /// The announced IPv4 address.
    pub fn host(&self) -> Ipv4Addr {
        self.model().host
    }

    /// The NMOS id of the resource of `typ` with `key` (whether or not it exists).
    pub fn resource_id(&self, typ: ResourceType, key: &str) -> Uuid {
        self.model().id(typ, key)
    }

    pub(crate) fn model(&self) -> MutexGuard<'_, Model> {
        lock(&self.inner.model)
    }

    fn now(&self) -> TaiTime {
        self.inner.implementation.clock.now()
    }

    fn changed(&self, model: &mut Model) {
        model.refresh(&self.inner.settings, self.now());
        self.inner.changed.notify_one();
    }

    fn mutate<T>(&self, f: impl FnOnce(&mut Model) -> Result<T, Error>) -> Result<T, Error> {
        let mut model = self.model();
        let result = f(&mut model)?;
        self.changed(&mut model);
        Ok(result)
    }

    // ---- resources --------------------------------------------------------

    /// Insert a Device, or replace the one with the same key.
    pub fn insert_device(&self, device: Device) -> Result<(), Error> {
        self.mutate(|m| {
            m.insert_device(device);
            Ok(())
        })
    }

    /// Insert a Source, or replace the one with the same key.
    pub fn insert_source(&self, source: Source) -> Result<(), Error> {
        self.mutate(|m| m.insert_source(source))
    }

    /// Insert a Flow, or replace the one with the same key.
    pub fn insert_flow(&self, flow: Flow) -> Result<(), Error> {
        self.mutate(|m| m.insert_flow(flow))
    }

    /// Insert a Sender, or replace the one with the same key (its connection
    /// state is kept).
    pub fn insert_sender(&self, sender: Sender) -> Result<(), Error> {
        self.mutate(|m| m.insert_sender(sender))
    }

    /// Insert a Receiver, or replace the one with the same key (its connection
    /// state is kept).
    pub fn insert_receiver(&self, receiver: Receiver) -> Result<(), Error> {
        let now = self.now();
        self.mutate(|m| m.insert_receiver(receiver, now))
    }

    /// Remove a resource and every resource that refers to it.
    pub fn erase(&self, typ: ResourceType, key: &str) -> Result<(), Error> {
        self.mutate(|m| m.erase(typ, key))
    }

    /// Set the staged parameters of a Sender or Receiver from the application
    /// (no activation, no callback).
    pub fn set_staged(&self, role: Role, key: &str, staged: StagedConnection) -> Result<(), Error> {
        self.mutate(|m| {
            let conn = m.conn_mut(role, key).ok_or_else(|| unknown(role, key))?;
            conn.staged = staged;
            Ok(())
        })
    }

    /// Set the active state of a Sender or Receiver from the application, for
    /// example when its own configuration connects or stops it (no callback).
    /// Updates the IS-04 `subscription` and `version`.
    pub fn set_active(&self, role: Role, key: &str, active: ActiveConnection) -> Result<(), Error> {
        self.mutate(|m| {
            let conn = m.conn_mut(role, key).ok_or_else(|| unknown(role, key))?;
            conn.active = active;
            Ok(())
        })
    }

    /// The staged state of a Sender or Receiver.
    pub fn staged(&self, role: Role, key: &str) -> Option<StagedConnection> {
        self.model().conn(role, key).map(|c| c.staged.clone())
    }

    /// The active state of a Sender or Receiver.
    pub fn active(&self, role: Role, key: &str) -> Option<ActiveConnection> {
        self.model().conn(role, key).map(|c| c.active.clone())
    }

    /// The current IS-04 documents in registration order (Node first).
    pub fn resources(&self) -> Vec<Value> {
        self.model().ordered_docs().iter().map(Doc::json).collect()
    }

    pub(crate) fn docs(&self) -> Vec<Doc> {
        self.model().ordered_docs()
    }

    // ---- persistence (G9) -------------------------------------------------

    /// The active state of every Sender and Receiver.
    pub fn connections(&self) -> Vec<SavedConnection> {
        let model = self.model();
        let senders = model.senders.values().map(|e| SavedConnection {
            role: Role::Sender,
            key: e.spec.key.clone(),
            active: e.conn.active.clone(),
        });
        let receivers = model.receivers.values().map(|e| SavedConnection {
            role: Role::Receiver,
            key: e.spec.key.clone(),
            active: e.conn.active.clone(),
        });
        senders.chain(receivers).collect()
    }

    /// Re-apply saved connections at start-up: the activation callback is called
    /// for each one (cause [`Cause::Restore`]) and, on accept, it becomes the
    /// active and staged state. Entries for unknown keys are skipped. Returns the
    /// keys whose restore the application rejected.
    pub async fn restore(&self, saved: Vec<SavedConnection>) -> Vec<(Role, String, String)> {
        let _guard = self.inner.activation_lock.lock().await;
        let domains = self.domains();
        let mut failed = Vec::new();
        for entry in saved {
            let typ = resource_type(entry.role);
            let exists = self.model().conn(entry.role, &entry.key).is_some();
            if !exists {
                continue;
            }
            let activation = Activation {
                role: entry.role,
                key: entry.key.clone(),
                nmos_id: self.model().id(typ, &entry.key),
                master_enable: entry.active.master_enable,
                mxl_domain_id: entry.active.mxl_domain_id,
                domain_path: domain_path(&domains, entry.active.mxl_domain_id),
                mxl_flow_id: entry.active.mxl_flow_id,
                peer_id: entry.active.peer_id,
                cause: Cause::Restore,
            };
            match self.call_handler(activation).await {
                Ok(()) => {
                    let now = self.now();
                    let mut model = self.model();
                    if let Some(conn) = model.conn_mut(entry.role, &entry.key) {
                        conn.staged = StagedConnection {
                            master_enable: entry.active.master_enable,
                            peer_id: entry.active.peer_id,
                            mxl_domain_id: entry.active.mxl_domain_id.into(),
                            mxl_flow_id: entry.active.mxl_flow_id.into(),
                        };
                        conn.active = entry.active.clone();
                        conn.active_activation = ActivationRecord {
                            mode: Some(Mode::Immediate),
                            requested_time: None,
                            activation_time: Some(now),
                        };
                    }
                    model.touch(typ, &entry.key);
                    self.changed(&mut model);
                }
                Err(reason) => failed.push((entry.role, entry.key, reason)),
            }
        }
        failed
    }

    fn connections_changed(&self) {
        if let Some(callback) = &self.inner.implementation.on_connections_changed {
            callback(self.connections());
        }
    }

    // ---- activations ------------------------------------------------------

    pub(crate) fn domains(&self) -> Vec<MxlDomain> {
        self.inner
            .implementation
            .domains
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default()
    }

    async fn call_handler(&self, activation: Activation) -> Result<(), String> {
        let handler = self.inner.implementation.on_activation.clone();
        let timeout = self.inner.settings.activation_timeout;
        match tokio::time::timeout(timeout, handler(activation)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(rejected)) => Err(rejected.0),
            Err(_) => Err(format!(
                "the activation did not complete within {} ms",
                timeout.as_millis()
            )),
        }
    }

    fn resolve(
        &self,
        model: &mut Model,
        role: Role,
        key: &str,
        domains: &[MxlDomain],
    ) -> Result<(Option<Uuid>, Option<Uuid>), String> {
        let conn = model
            .conn(role, key)
            .ok_or_else(|| "not found".to_string())?;
        let staged = conn.staged.clone();
        if let Some(resolver) = &self.inner.implementation.on_resolve_auto {
            if staged.mxl_domain_id == Param::Auto || staged.mxl_flow_id == Param::Auto {
                return resolver(&AutoRequest {
                    role,
                    key: key.to_string(),
                    mxl_domain_id: staged.mxl_domain_id,
                    mxl_flow_id: staged.mxl_flow_id,
                    domains: domains.to_vec(),
                })
                .map(|(domain, flow)| {
                    (
                        staged.mxl_domain_id.id().or(domain),
                        staged.mxl_flow_id.id().or(flow),
                    )
                });
            }
        }
        let (own_domain, own_flow) = match role {
            Role::Sender => model
                .senders
                .get(key)
                .map(|s| (s.spec.mxl_domain_id, s.spec.mxl_flow_id))
                .unwrap_or_default(),
            Role::Receiver => (None, None),
        };
        let domain = match staged.mxl_domain_id {
            Param::Null => None,
            Param::Id(id) => Some(id),
            Param::Auto => match (own_domain, domains) {
                (Some(id), _) => Some(id),
                (None, [only]) => Some(only.id),
                _ => return Err("auto mxl_domain_id could not be resolved".to_string()),
            },
        };
        let flow = match staged.mxl_flow_id {
            Param::Null => None,
            Param::Id(id) => Some(id),
            Param::Auto if role == Role::Sender => {
                let conn = model
                    .conn_mut(role, key)
                    .ok_or_else(|| "not found".to_string())?;
                let id = own_flow
                    .or(conn.generated_flow)
                    .unwrap_or_else(Uuid::new_v4);
                if own_flow.is_none() {
                    conn.generated_flow = Some(id);
                }
                Some(id)
            }
            Param::Auto => return Err("receivers must not accept auto for mxl_flow_id".to_string()),
        };
        Ok((domain, flow))
    }

    /// PATCH `/staged` of one Sender or Receiver.
    pub(crate) async fn patch_staged(
        &self,
        role: Role,
        id: Uuid,
        body: &Value,
        bulk: bool,
    ) -> Result<Reply, Reply> {
        let patch = parse_patch(role, body).map_err(|e| api_error(400, &e))?;
        let _guard = self.inner.activation_lock.lock().await;
        let domains = self.domains();
        let key = self
            .model()
            .key_for(role, id)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        let (activation, resolved) = {
            let mut model = self.model();
            let conn = model
                .conn(role, &key)
                .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
            if conn.pending.is_some() && patch.activation != Some(ActivationRequest::Cancel) {
                return Err(api_error(
                    423,
                    "a scheduled activation is pending; set activation.mode to null to cancel it",
                ));
            }
            let mut staged = conn.staged.clone();
            patch
                .apply(role, &mut staged, &domains)
                .map_err(|e| api_error(400, &e))?;
            let conn = model.conn_mut(role, &key).expect("checked above");
            conn.staged = staged;
            match patch.activation {
                None => {
                    let conn = model.conn(role, &key).expect("checked above");
                    return Ok((
                        200,
                        model.staged_json(role, conn, ActivationRecord::default()),
                    ));
                }
                Some(ActivationRequest::Cancel) => {
                    conn.pending = None;
                    let conn = model.conn(role, &key).expect("checked above");
                    return Ok((
                        200,
                        model.staged_json(role, conn, ActivationRecord::default()),
                    ));
                }
                Some(ActivationRequest::Absolute(_)) | Some(ActivationRequest::Relative(_)) => {
                    return Ok(self.schedule(&mut model, role, &key, &patch));
                }
                Some(ActivationRequest::Immediate) => {}
            }
            let resolved = self
                .resolve(&mut model, role, &key, &domains)
                .map_err(|e| api_error(500, &e))?;
            let conn = model.conn(role, &key).expect("checked above");
            let activation = Activation {
                role,
                key: key.clone(),
                nmos_id: id,
                master_enable: conn.staged.master_enable,
                mxl_domain_id: resolved.0,
                domain_path: domain_path(&domains, resolved.0),
                mxl_flow_id: resolved.1,
                peer_id: conn.staged.peer_id,
                cause: if bulk { Cause::Bulk } else { Cause::Immediate },
            };
            (activation, resolved)
        };
        let peer = activation.peer_id;
        let master_enable = activation.master_enable;
        self.call_handler(activation)
            .await
            .map_err(|e| api_error(500, &e))?;
        let now = self.now();
        let record = ActivationRecord {
            mode: Some(Mode::Immediate),
            requested_time: None,
            activation_time: Some(now),
        };
        let reply = {
            let mut model = self.model();
            let conn = model
                .conn_mut(role, &key)
                .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
            conn.active = ActiveConnection {
                master_enable,
                peer_id: peer,
                mxl_domain_id: resolved.0,
                mxl_flow_id: resolved.1,
            };
            conn.active_activation = record;
            model.touch(resource_type(role), &key);
            self.changed(&mut model);
            let conn = model.conn(role, &key).expect("present");
            model.staged_json(role, conn, record)
        };
        self.connections_changed();
        Ok((200, reply))
    }

    fn schedule(&self, model: &mut Model, role: Role, key: &str, patch: &StagePatch) -> Reply {
        let now = self.now();
        let (mode, requested, at) = match patch.activation {
            Some(ActivationRequest::Absolute(t)) => (Mode::ScheduledAbsolute, t, t),
            Some(ActivationRequest::Relative(d)) => {
                (Mode::ScheduledRelative, d, now.plus(d.as_duration()))
            }
            _ => unreachable!("only scheduled modes are passed"),
        };
        model.schedule_seq += 1;
        let seq = model.schedule_seq;
        let record = ActivationRecord {
            mode: Some(mode),
            requested_time: Some(requested),
            activation_time: Some(at),
        };
        let conn = model.conn_mut(role, key).expect("checked by the caller");
        conn.pending = Some(Pending { seq, record });
        let node = self.clone();
        let task_key = key.to_string();
        tokio::spawn(async move { node.run_scheduled(role, task_key, seq, at).await });
        let conn = model.conn(role, key).expect("present");
        (202, model.staged_json(role, conn, record))
    }

    async fn wait_until(&self, at: TaiTime) {
        let clock = self.inner.implementation.clock.clone();
        if tokio::task::spawn_blocking(move || clock.block_until(at))
            .await
            .unwrap_or(false)
        {
            return;
        }
        loop {
            let now = self.now();
            if now >= at {
                return;
            }
            tokio::time::sleep(at.since(now)).await;
        }
    }

    /// Prepares the activation shortly before `at` and calls the handler at `at`:
    /// a reader of `/active` right after `at` sees the new state.
    async fn run_scheduled(&self, role: Role, key: String, seq: u64, at: TaiTime) {
        loop {
            let left = at.since(self.now());
            if left <= FINE_WAIT {
                break;
            }
            tokio::time::sleep(left - FINE_WAIT).await;
        }
        let _guard = self.inner.activation_lock.lock().await;
        let domains = self.domains();
        let prepared = {
            let mut model = self.model();
            let still_pending = model
                .conn(role, &key)
                .and_then(|c| c.pending)
                .is_some_and(|p| p.seq == seq);
            if !still_pending {
                return;
            }
            let id = model.id(resource_type(role), &key);
            let record = model
                .conn(role, &key)
                .and_then(|c| c.pending)
                .map(|p| p.record);
            match self.resolve(&mut model, role, &key, &domains) {
                Ok(resolved) => {
                    let conn = model.conn(role, &key).expect("pending");
                    Ok((
                        Activation {
                            role,
                            key: key.clone(),
                            nmos_id: id,
                            master_enable: conn.staged.master_enable,
                            mxl_domain_id: resolved.0,
                            domain_path: domain_path(&domains, resolved.0),
                            mxl_flow_id: resolved.1,
                            peer_id: conn.staged.peer_id,
                            cause: Cause::Scheduled,
                        },
                        resolved,
                        record.unwrap_or_default(),
                    ))
                }
                Err(err) => Err(err),
            }
        };
        self.wait_until(at).await;
        let result = match prepared {
            Ok((activation, resolved, record)) => {
                let peer = activation.peer_id;
                let master_enable = activation.master_enable;
                self.call_handler(activation).await.map(|()| {
                    (
                        ActiveConnection {
                            master_enable,
                            peer_id: peer,
                            mxl_domain_id: resolved.0,
                            mxl_flow_id: resolved.1,
                        },
                        record,
                    )
                })
            }
            Err(err) => Err(err),
        };
        let mut model = self.model();
        let Some(conn) = model.conn_mut(role, &key) else {
            return;
        };
        if conn.pending.map(|p| p.seq) != Some(seq) {
            return;
        }
        conn.pending = None;
        match result {
            Ok((active, record)) => {
                conn.active = active;
                conn.active_activation = ActivationRecord {
                    activation_time: Some(at),
                    ..record
                };
                model.touch(resource_type(role), &key);
                self.changed(&mut model);
                drop(model);
                self.connections_changed();
            }
            Err(err) => {
                tracing::warn!("scheduled activation of {role:?} {key} failed: {err}");
            }
        }
    }

    /// POST `/bulk/senders` or `/bulk/receivers`: every entry is activated on its
    /// own; one failure fails only that entry.
    pub(crate) async fn bulk(&self, role: Role, body: &Value) -> Result<Reply, Reply> {
        let items = body
            .as_array()
            .ok_or_else(|| api_error(400, "the bulk request body must be an array"))?;
        let mut results = Vec::new();
        for item in items {
            let id_text = item.get("id").and_then(Value::as_str);
            let parsed = id_text.and_then(|t| Uuid::parse_str(t).ok());
            let entry = match (id_text, parsed, item.get("params")) {
                (Some(text), Some(id), Some(params)) => {
                    match Box::pin(self.patch_staged(role, id, params, true)).await {
                        Ok((code, _)) => json!({"id": text, "code": code}),
                        Err((code, body)) => json!({
                            "id": text,
                            "code": code,
                            "error": body["error"],
                            "debug": body["debug"]
                        }),
                    }
                }
                (text, _, _) => json!({
                    "id": text.map(|t| json!(t)).unwrap_or(Value::Null),
                    "code": 400,
                    "error": "Bad Request",
                    "debug": "each entry needs an 'id' (UUID) and 'params'"
                }),
            };
            results.push(entry);
        }
        Ok((200, Value::Array(results)))
    }

    // ---- HTTP dispatch ----------------------------------------------------

    pub(crate) async fn dispatch(&self, method: &str, path: &str, body: &[u8]) -> Reply {
        let parts: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        let result = match method {
            "GET" | "HEAD" => self.get(&parts),
            "PATCH" => match parts.as_slice() {
                ["x-nmos", "connection", "v1.2", "single", kind, id, "staged"] => {
                    match (role_of(kind), Uuid::parse_str(id)) {
                        (Some(role), Ok(id)) => match parse_body(body) {
                            Ok(value) => self.patch_staged(role, id, &value, false).await,
                            Err(err) => Err(err),
                        },
                        _ => Err(api_error(404, "not found")),
                    }
                }
                _ => self.not_allowed(&parts),
            },
            "POST" => match parts.as_slice() {
                ["x-nmos", "connection", "v1.2", "bulk", kind] => match role_of(kind) {
                    Some(role) => match parse_body(body) {
                        Ok(value) => self.bulk(role, &value).await,
                        Err(err) => Err(err),
                    },
                    None => Err(api_error(404, "not found")),
                },
                _ => self.not_allowed(&parts),
            },
            _ => self.not_allowed(&parts),
        };
        match result {
            Ok(reply) | Err(reply) => reply,
        }
    }

    /// 405 for a path that exists, otherwise what GET would answer (404).
    fn not_allowed(&self, parts: &[&str]) -> Result<Reply, Reply> {
        match self.get(parts) {
            Ok(_) => Err(api_error(405, "method not allowed")),
            Err(err) => Err(err),
        }
    }

    fn get(&self, parts: &[&str]) -> Result<Reply, Reply> {
        let model = self.model();
        let list = |items: &[&str]| Ok((200, json!(items)));
        match parts {
            ["x-nmos"] => list(&["node/", "connection/"]),
            ["x-nmos", "node"] => list(&["v1.3/"]),
            ["x-nmos", "connection"] => list(&["v1.2/"]),
            ["x-nmos", "node", "v1.3"] => list(&[
                "self/",
                "devices/",
                "sources/",
                "flows/",
                "senders/",
                "receivers/",
            ]),
            ["x-nmos", "node", "v1.3", "self"] => Ok((
                200,
                model
                    .docs_of(ResourceType::Node)
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Null),
            )),
            ["x-nmos", "node", "v1.3", collection] => match collection_type(collection) {
                Some(typ) => Ok((200, Value::Array(model.docs_of(typ)))),
                None => Err(api_error(404, "not found")),
            },
            ["x-nmos", "node", "v1.3", collection, id] => match collection_type(collection) {
                Some(typ) => model
                    .docs_of(typ)
                    .into_iter()
                    .find(|doc| doc["id"].as_str() == Some(id))
                    .map(|doc| (200, doc))
                    .ok_or_else(|| api_error(404, "not found")),
                None => Err(api_error(404, "not found")),
            },
            ["x-nmos", "node", "v1.3", "receivers", id, "target"] => {
                if model.key_for(Role::Receiver, parse_id(id)?).is_some() {
                    Err(api_error(
                        405,
                        "use the IS-05 Connection API to connect MXL receivers",
                    ))
                } else {
                    Err(api_error(404, "not found"))
                }
            }
            ["x-nmos", "connection", "v1.2"] => list(&["bulk/", "single/"]),
            ["x-nmos", "connection", "v1.2", "bulk"] => list(&["senders/", "receivers/"]),
            ["x-nmos", "connection", "v1.2", "single"] => list(&["senders/", "receivers/"]),
            ["x-nmos", "connection", "v1.2", "bulk", kind] if role_of(kind).is_some() => {
                Err(api_error(405, "bulk activations are POST only"))
            }
            ["x-nmos", "connection", "v1.2", "single", kind] => match role_of(kind) {
                Some(role) => Ok((
                    200,
                    Value::Array(
                        model
                            .ids(role)
                            .iter()
                            .map(|id| json!(format!("{id}/")))
                            .collect(),
                    ),
                )),
                None => Err(api_error(404, "not found")),
            },
            ["x-nmos", "connection", "v1.2", "single", kind, id] => {
                let role = role_of(kind).ok_or_else(|| api_error(404, "not found"))?;
                model
                    .key_for(role, parse_id(id)?)
                    .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
                match role {
                    Role::Sender => list(&[
                        "constraints/",
                        "staged/",
                        "active/",
                        "transportfile/",
                        "transporttype/",
                    ]),
                    Role::Receiver => {
                        list(&["constraints/", "staged/", "active/", "transporttype/"])
                    }
                }
            }
            ["x-nmos", "connection", "v1.2", "single", kind, id, leaf] => {
                let role = role_of(kind).ok_or_else(|| api_error(404, "not found"))?;
                let key = model
                    .key_for(role, parse_id(id)?)
                    .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
                let conn = model.conn(role, &key).expect("key exists");
                match *leaf {
                    "staged" => Ok((
                        200,
                        model.staged_json(
                            role,
                            conn,
                            conn.pending.map(|p| p.record).unwrap_or_default(),
                        ),
                    )),
                    "active" => Ok((200, model.active_json(role, conn))),
                    "constraints" => {
                        drop(model);
                        Ok((200, constraints_json(&self.domains())))
                    }
                    "transporttype" => Ok((200, json!("urn:x-nmos:transport:mxl"))),
                    "transportfile" if role == Role::Sender => Err(api_error(
                        404,
                        "MXL senders do not provide a transport file",
                    )),
                    _ => Err(api_error(404, "not found")),
                }
            }
            _ => Err(api_error(404, "not found")),
        }
    }

    // ---- registration -----------------------------------------------------

    /// True while the node is registered: the last registration and heartbeat
    /// succeeded and, when a Query API is configured, the Query API lists the
    /// node. Use it for `/readyz` only; liveness must never depend on it.
    pub fn is_registered(&self) -> bool {
        self.inner.registered.load(Ordering::SeqCst)
    }

    /// Whether the node registers with a registry (static or DNS-SD).
    pub fn registration_required(&self) -> bool {
        let s = &self.inner.settings;
        s.registration && (s.registry.is_some() || s.dns_sd)
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }

    /// Start the registration task (heartbeats, re-registration, DNS-SD when
    /// enabled). Does nothing when registration is off or already started. Must be
    /// called inside a tokio runtime.
    pub fn start_registration(&self) {
        if !self.registration_required() {
            tracing::info!("NMOS registration is off");
            return;
        }
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::info!("NMOS node {} registering", self.node_id());
        let handle = tokio::spawn(crate::registration::run(self.clone()));
        *lock(&self.inner.task) = Some(handle);
    }

    /// Stop registering and DELETE every registered resource, in reverse
    /// registration order with the Node last, within `timeout` (G8).
    pub async fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.shutdown_signal.send_replace(true);
        self.inner.changed.notify_one();
        let handle = lock(&self.inner.task).take();
        let deregister = async {
            if let Some(handle) = handle {
                let _ = handle.await;
            }
            crate::registration::deregister(self).await;
        };
        if tokio::time::timeout(timeout, deregister).await.is_err() {
            tracing::warn!("NMOS deregistration did not finish within {timeout:?}");
        }
        self.inner.registered.store(false, Ordering::SeqCst);
    }
}

fn unknown(role: Role, key: &str) -> Error {
    Error::UnknownResource {
        typ: resource_type(role),
        key: key.to_string(),
    }
}

pub(crate) fn resource_type(role: Role) -> ResourceType {
    match role {
        Role::Sender => ResourceType::Sender,
        Role::Receiver => ResourceType::Receiver,
    }
}

fn domain_path(domains: &[MxlDomain], id: Option<Uuid>) -> Option<std::path::PathBuf> {
    let id = id?;
    domains.iter().find(|d| d.id == id).map(|d| d.path.clone())
}

fn role_of(kind: &str) -> Option<Role> {
    match kind {
        "senders" => Some(Role::Sender),
        "receivers" => Some(Role::Receiver),
        _ => None,
    }
}

fn collection_type(name: &str) -> Option<ResourceType> {
    match name {
        "devices" => Some(ResourceType::Device),
        "sources" => Some(ResourceType::Source),
        "flows" => Some(ResourceType::Flow),
        "senders" => Some(ResourceType::Sender),
        "receivers" => Some(ResourceType::Receiver),
        _ => None,
    }
}

fn parse_id(id: &str) -> Result<Uuid, Reply> {
    Uuid::parse_str(id).map_err(|_| api_error(404, "not found"))
}

fn parse_body(body: &[u8]) -> Result<Value, Reply> {
    if body.is_empty() {
        return Err(api_error(400, "the request body is empty"));
    }
    serde_json::from_slice(body).map_err(|err| api_error(400, &err.to_string()))
}

/// An NMOS error document.
pub(crate) fn api_error(code: u16, debug: &str) -> Reply {
    let error = match code {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        423 => "Locked",
        500 => "Internal Server Error",
        _ => "Error",
    };
    (code, json!({"code": code, "error": error, "debug": debug}))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
