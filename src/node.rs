//! IS-04 resource tree and IS-05 connection state for MXL endpoints.
//!
//! One Strom process is one Node. Each flow that contains MXL blocks is one
//! Device. An output block is a writer (Source, Flow, Sender). An input block
//! is a reader (Receiver).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde_json::{json, Value};
use strom_types::block::BlockInstance;
use strom_types::element::PropertyValue;
use strom_types::mxl::{
    MXL_AUDIO_INPUT_ID, MXL_AUDIO_OUTPUT_ID, MXL_VIDEO_INPUT_ID, MXL_VIDEO_OUTPUT_ID,
};
use strom_types::{Flow, FlowId};
use uuid::Uuid;

use super::domain::{domain_has_flow, scan_domain_root, scan_domains, MxlDomain};
use super::settings::NmosSettings;

pub type SnapshotFn = Arc<dyn Fn() -> BoxFuture<'static, Vec<Flow>> + Send + Sync>;
pub type ApplyFn = Arc<dyn Fn(MxlApply) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// What an IS-05 activation asks the pipeline to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxlApply {
    pub flow_id: FlowId,
    pub block_id: String,
    pub kind: EndpointKind,
    pub domain_path: String,
    pub mxl_flow_id: String,
    pub master_enable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    VideoSender,
    AudioSender,
    VideoReceiver,
    AudioReceiver,
}

impl EndpointKind {
    fn is_sender(self) -> bool {
        matches!(self, Self::VideoSender | Self::AudioSender)
    }

    fn from_definition(id: &str) -> Option<Self> {
        match id {
            MXL_VIDEO_OUTPUT_ID => Some(Self::VideoSender),
            MXL_AUDIO_OUTPUT_ID => Some(Self::AudioSender),
            MXL_VIDEO_INPUT_ID => Some(Self::VideoReceiver),
            MXL_AUDIO_INPUT_ID => Some(Self::AudioReceiver),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Param {
    Null,
    Auto,
    Id(Uuid),
}

impl Param {
    fn from_json(value: &Value) -> Result<Self, String> {
        if value.is_null() {
            return Ok(Self::Null);
        }
        let Some(text) = value.as_str() else {
            return Err("transport parameter must be null, \"auto\", or a UUID".to_string());
        };
        if text == "auto" {
            return Ok(Self::Auto);
        }
        Uuid::parse_str(text)
            .map(Self::Id)
            .map_err(|_| format!("'{text}' is not an MXL UUID"))
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Auto => json!("auto"),
            Self::Id(id) => json!(id.to_string()),
        }
    }
}

struct Endpoint {
    kind: EndpointKind,
    flow_id: FlowId,
    block_id: String,
    flow_name: String,
    label: String,
    group_hint: String,
    preferred_domain_path: String,
    configured_mxl_flow: Option<Uuid>,
    running: bool,
    dirty: bool,
    generated_flow: Option<Uuid>,
    staged_peer: Option<Uuid>,
    staged_master: bool,
    staged_domain: Param,
    staged_flow: Param,
    staged_activation_time: Option<String>,
    active_peer: Option<Uuid>,
    active_domain: Option<Uuid>,
    active_flow: Option<Uuid>,
    active_activation_time: Option<String>,
}

impl Endpoint {
    fn master_enable(&self) -> bool {
        self.running && self.active_flow.is_some()
    }

    fn nmos_id(&self, node_id: Uuid, role: &str) -> Uuid {
        Uuid::new_v5(
            &node_id,
            format!("{role}:{}:{}", self.flow_id, self.block_id).as_bytes(),
        )
    }

    fn sender_id(&self, node_id: Uuid) -> Uuid {
        self.nmos_id(node_id, "sender")
    }

    fn receiver_id(&self, node_id: Uuid) -> Uuid {
        self.nmos_id(node_id, "receiver")
    }

    fn source_id(&self, node_id: Uuid) -> Uuid {
        self.nmos_id(node_id, "source")
    }

    fn is04_flow_id(&self, node_id: Uuid) -> Uuid {
        self.nmos_id(node_id, "flow")
    }

    fn device_id(node_id: Uuid, flow_id: FlowId) -> Uuid {
        Uuid::new_v5(&node_id, format!("device:{flow_id}").as_bytes())
    }
}

struct Model {
    node_id: Uuid,
    endpoints: HashMap<String, Endpoint>,
    domains: Vec<MxlDomain>,
    versions: HashMap<String, (u64, String)>,
    host: String,
    hostname: String,
}

/// In-memory NMOS node. HTTP handlers and the registration task share it.
#[derive(Clone)]
pub struct NmosNode {
    inner: Arc<Inner>,
}

struct Inner {
    settings: NmosSettings,
    model: Mutex<Model>,
    snapshot: SnapshotFn,
    apply: ApplyFn,
    shutdown: AtomicBool,
    started: AtomicBool,
    /// Last registration POST and heartbeat succeeded.
    registered_ok: AtomicBool,
    /// `type:id` -> version last accepted by the registry.
    registered: Mutex<HashMap<String, String>>,
    registry: Mutex<RegistryState>,
}

pub struct RegistryState {
    pub candidates: Vec<String>,
    pub index: usize,
}

impl NmosNode {
    pub fn new(settings: NmosSettings, snapshot: SnapshotFn, apply: ApplyFn) -> Self {
        let node_id = ensure_node_id(&settings);
        let host = advertise_host(&settings);
        let hostname = hostname_string();
        Self {
            inner: Arc::new(Inner {
                settings,
                model: Mutex::new(Model {
                    node_id,
                    endpoints: HashMap::new(),
                    domains: Vec::new(),
                    versions: HashMap::new(),
                    host,
                    hostname,
                }),
                snapshot,
                apply,
                shutdown: AtomicBool::new(false),
                started: AtomicBool::new(false),
                registered_ok: AtomicBool::new(false),
                registered: Mutex::new(HashMap::new()),
                registry: Mutex::new(RegistryState {
                    candidates: Vec::new(),
                    index: 0,
                }),
            }),
        }
    }

    pub fn settings(&self) -> &NmosSettings {
        &self.inner.settings
    }

    pub fn node_id(&self) -> Uuid {
        lock(&self.inner.model).node_id
    }

    pub fn api_host(&self) -> String {
        lock(&self.inner.model).host.clone()
    }

    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }

    pub fn request_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn mark_started(&self) -> bool {
        self.inner.started.swap(true, Ordering::SeqCst)
    }

    pub fn set_registered(&self, ok: bool) {
        self.inner.registered_ok.store(ok, Ordering::SeqCst);
    }

    /// True after the node resource was accepted by the registry and the
    /// following heartbeat succeeded. False when no registry is configured.
    pub fn is_registered(&self) -> bool {
        self.inner.registered_ok.load(Ordering::SeqCst)
    }

    pub fn registry_required(&self) -> bool {
        self.inner
            .settings
            .registry
            .as_ref()
            .is_some_and(|url| !url.trim().is_empty())
    }

    pub async fn sync(&self) {
        let flows = (self.inner.snapshot)().await;
        let domains = discover_domains(&self.inner.settings);
        let mut model = lock(&self.inner.model);
        model.domains = domains;
        model.sync_flows(&flows);
    }

    pub fn registry_state(&self) -> std::sync::MutexGuard<'_, RegistryState> {
        lock(&self.inner.registry)
    }

    pub fn registered(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        lock(&self.inner.registered)
    }

    pub fn publish(&self) -> Vec<Published> {
        lock(&self.inner.model).publish(&self.inner.settings)
    }

    pub async fn dispatch(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<(u16, Value), (u16, Value)> {
        self.sync().await;
        let parts = split_path(path);
        match method {
            "GET" => self.get(&parts),
            "PATCH" => self.patch(&parts, body).await,
            "POST" => self.post(&parts, body).await,
            _ => Err(api_error(405, "method not allowed")),
        }
    }

    fn get(&self, parts: &[&str]) -> Result<(u16, Value), (u16, Value)> {
        let mut model = lock(&self.inner.model);
        let settings = &self.inner.settings;
        match parts {
            ["node"] => Ok((200, json!(["v1.3/"]))),
            ["connection"] => Ok((200, json!(["v1.2/"]))),
            ["node", "v1.3"] => Ok((
                200,
                json!([
                    "self/",
                    "devices/",
                    "sources/",
                    "flows/",
                    "senders/",
                    "receivers/"
                ]),
            )),
            ["node", "v1.3", "self"] => Ok((200, model.node_resource(settings))),
            ["node", "v1.3", "devices"] => Ok((200, Value::Array(model.devices(settings)))),
            ["node", "v1.3", "sources"] => Ok((200, Value::Array(model.sources()))),
            ["node", "v1.3", "flows"] => Ok((200, Value::Array(model.flows()))),
            ["node", "v1.3", "senders"] => Ok((200, Value::Array(model.senders()))),
            ["node", "v1.3", "receivers"] => Ok((200, Value::Array(model.receivers()))),
            ["node", "v1.3", "devices", id] => resource_by_id(&model.devices(settings), id),
            ["node", "v1.3", "sources", id] => resource_by_id(&model.sources(), id),
            ["node", "v1.3", "flows", id] => resource_by_id(&model.flows(), id),
            ["node", "v1.3", "senders", id] => resource_by_id(&model.senders(), id),
            ["node", "v1.3", "receivers", id] => resource_by_id(&model.receivers(), id),
            ["connection", "v1.2"] => Ok((200, json!(["bulk/", "single/"]))),
            ["connection", "v1.2", "bulk"] => Ok((200, json!(["senders/", "receivers/"]))),
            ["connection", "v1.2", "single"] => Ok((200, json!(["senders/", "receivers/"]))),
            ["connection", "v1.2", "single", "senders"] => Ok((200, model.id_list(true))),
            ["connection", "v1.2", "single", "receivers"] => Ok((200, model.id_list(false))),
            // IS-05 bulk resources are POST (and OPTIONS). GET is defined as 405.
            ["connection", "v1.2", "bulk", "senders"]
            | ["connection", "v1.2", "bulk", "receivers"] => {
                Err(api_error(405, "bulk staging is POST only"))
            }
            ["connection", "v1.2", "single", kind, id] => model.single_index(kind, id),
            ["connection", "v1.2", "single", kind, id, leaf] => model.single_get(kind, id, leaf),
            _ => Err(api_error(404, "not found")),
        }
    }

    async fn patch(&self, parts: &[&str], body: &[u8]) -> Result<(u16, Value), (u16, Value)> {
        let ["connection", "v1.2", "single", kind, id, "staged"] = parts else {
            return Err(api_error(404, "not found"));
        };
        let is_sender = kind_is_sender(kind)?;
        let id = parse_id(id)?;
        let patch: Value = parse_body(body)?;
        self.activate_patch(is_sender, id, patch).await
    }

    async fn post(&self, parts: &[&str], body: &[u8]) -> Result<(u16, Value), (u16, Value)> {
        let is_sender = match parts {
            ["connection", "v1.2", "bulk", "senders"] => true,
            ["connection", "v1.2", "bulk", "receivers"] => false,
            _ => return Err(api_error(404, "not found")),
        };
        let items = parse_body(body)?;
        let Some(items) = items.as_array() else {
            return Err(api_error(400, "bulk body must be an array"));
        };
        let mut results = Vec::new();
        for item in items {
            let Some(id_text) = item.get("id").and_then(|v| v.as_str()) else {
                results.push(json!({"id": Value::Null, "code": 400}));
                continue;
            };
            let params = item.get("params").cloned().unwrap_or_else(|| json!({}));
            let code = match Uuid::parse_str(id_text) {
                Ok(id) => match self.activate_patch(is_sender, id, params).await {
                    Ok((status, _)) => status,
                    Err((status, _)) => status,
                },
                Err(_) => 400,
            };
            results.push(json!({"id": id_text, "code": code}));
        }
        Ok((200, Value::Array(results)))
    }

    async fn activate_patch(
        &self,
        is_sender: bool,
        id: Uuid,
        patch: Value,
    ) -> Result<(u16, Value), (u16, Value)> {
        let prepared = {
            let mut model = lock(&self.inner.model);
            model.prepare_patch(is_sender, id, &patch)?
        };
        if let Some(command) = prepared.command {
            if let Err(err) = (self.inner.apply)(command).await {
                return Err(api_error(500, &err));
            }
            self.sync().await;
            let mut model = lock(&self.inner.model);
            model.commit_activation(is_sender, id, &prepared.resolved);
        }
        let model = lock(&self.inner.model);
        let staged = model
            .staged_by_nmos_id(is_sender, id)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        Ok((prepared.status, staged))
    }
}

struct PreparedPatch {
    status: u16,
    command: Option<MxlApply>,
    resolved: Option<Resolved>,
}

struct Resolved {
    domain: Option<Uuid>,
    flow: Option<Uuid>,
    peer: Option<Uuid>,
    master_enable: bool,
    activation_time: String,
}

fn kind_is_sender(kind: &str) -> Result<bool, (u16, Value)> {
    match kind {
        "senders" => Ok(true),
        "receivers" => Ok(false),
        _ => Err(api_error(404, "not found")),
    }
}

fn parse_id(id: &str) -> Result<Uuid, (u16, Value)> {
    Uuid::parse_str(id).map_err(|_| api_error(404, "not found"))
}

fn parse_body(body: &[u8]) -> Result<Value, (u16, Value)> {
    if body.is_empty() {
        return Err(api_error(400, "request body is empty"));
    }
    serde_json::from_slice(body).map_err(|err| api_error(400, &err.to_string()))
}

fn resource_by_id(items: &[Value], id: &str) -> Result<(u16, Value), (u16, Value)> {
    items
        .iter()
        .find(|item| item.get("id").and_then(|v| v.as_str()) == Some(id))
        .cloned()
        .map(|item| (200, item))
        .ok_or_else(|| api_error(404, "not found"))
}

fn split_path(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

pub fn api_error(code: u16, debug: &str) -> (u16, Value) {
    let error = match code {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    (code, json!({"code": code, "error": error, "debug": debug}))
}

impl Model {
    fn sync_flows(&mut self, flows: &[Flow]) {
        let mut seen = HashSet::new();
        for flow in flows {
            for block in &flow.blocks {
                let Some(kind) = EndpointKind::from_definition(&block.block_definition_id) else {
                    continue;
                };
                let key = endpoint_key(flow.id, &block.id);
                seen.insert(key.clone());
                let configured = configured_flow_id(block, kind);
                let path = prop_string(&block.properties, "domain");
                let label = block
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| default_label(kind));
                let hint = prop_string(&block.properties, "group_hint");
                if let Some(endpoint) = self.endpoints.get_mut(&key) {
                    endpoint.flow_name = flow.name.clone();
                    endpoint.label = label;
                    endpoint.group_hint = hint;
                    endpoint.preferred_domain_path = path;
                    endpoint.configured_mxl_flow = configured;
                    endpoint.running = flow.running;
                    if !endpoint.dirty {
                        seed_from_block(endpoint, &self.domains);
                    }
                } else {
                    let mut endpoint = Endpoint {
                        kind,
                        flow_id: flow.id,
                        block_id: block.id.clone(),
                        flow_name: flow.name.clone(),
                        label,
                        group_hint: hint,
                        preferred_domain_path: path,
                        configured_mxl_flow: configured,
                        running: flow.running,
                        dirty: false,
                        generated_flow: None,
                        staged_peer: None,
                        staged_master: false,
                        staged_domain: Param::Null,
                        staged_flow: Param::Null,
                        staged_activation_time: None,
                        active_peer: None,
                        active_domain: None,
                        active_flow: None,
                        active_activation_time: None,
                    };
                    seed_from_block(&mut endpoint, &self.domains);
                    self.endpoints.insert(key, endpoint);
                }
            }
        }
        self.endpoints.retain(|key, _| seen.contains(key));
    }

    fn prepare_patch(
        &mut self,
        is_sender: bool,
        id: Uuid,
        patch: &Value,
    ) -> Result<PreparedPatch, (u16, Value)> {
        let key = self
            .key_for(is_sender, id)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        let endpoint = self
            .endpoints
            .get(&key)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        if endpoint.kind.is_sender() != is_sender {
            return Err(api_error(404, "sender or receiver not found"));
        }
        let mut updated = clone_connection(endpoint);
        apply_patch_fields(&mut updated, is_sender, patch, &self.domains)
            .map_err(|err| api_error(400, &err))?;
        let immediate = patch_is_immediate(patch).map_err(|err| api_error(400, &err))?;
        if !immediate {
            if let Some(endpoint) = self.endpoints.get_mut(&key) {
                *endpoint = updated;
                endpoint.dirty = true;
            }
            return Ok(PreparedPatch {
                status: 200,
                command: None,
                resolved: None,
            });
        }

        let resolved =
            resolve_activation(&mut updated, &self.domains).map_err(|err| api_error(500, &err))?;
        let domain_path = match resolved.domain {
            Some(domain_id) => self
                .domains
                .iter()
                .find(|domain| domain.id == domain_id)
                .map(|domain| domain.path.display().to_string())
                .ok_or_else(|| api_error(500, "resolved MXL domain is not mounted"))?,
            None => String::new(),
        };
        let mxl_flow_id = resolved.flow.map(|id| id.to_string()).unwrap_or_default();
        let command = if resolved.master_enable || updated.running {
            Some(MxlApply {
                flow_id: updated.flow_id,
                block_id: updated.block_id.clone(),
                kind: updated.kind,
                domain_path,
                mxl_flow_id,
                master_enable: resolved.master_enable,
            })
        } else {
            None
        };
        if let Some(endpoint) = self.endpoints.get_mut(&key) {
            endpoint.generated_flow = updated.generated_flow;
            endpoint.staged_peer = updated.staged_peer;
            endpoint.staged_master = updated.staged_master;
            endpoint.staged_domain = updated.staged_domain.clone();
            endpoint.staged_flow = updated.staged_flow.clone();
            endpoint.dirty = true;
        }
        Ok(PreparedPatch {
            status: 200,
            command,
            resolved: Some(resolved),
        })
    }

    fn commit_activation(&mut self, is_sender: bool, id: Uuid, resolved: &Option<Resolved>) {
        let Some(resolved) = resolved else {
            return;
        };
        let Some(key) = self.key_for(is_sender, id) else {
            return;
        };
        let Some(endpoint) = self.endpoints.get_mut(&key) else {
            return;
        };
        endpoint.active_peer = resolved.peer;
        endpoint.active_domain = resolved.domain;
        endpoint.active_flow = resolved.flow;
        endpoint.active_activation_time = Some(resolved.activation_time.clone());
        endpoint.staged_activation_time = Some(resolved.activation_time.clone());
        endpoint.staged_master = resolved.master_enable;
        if let Some(domain) = resolved.domain {
            endpoint.staged_domain = Param::Id(domain);
        }
        if let Some(flow) = resolved.flow {
            endpoint.staged_flow = Param::Id(flow);
        }
        endpoint.running = resolved.master_enable;
    }

    fn key_for(&self, is_sender: bool, id: Uuid) -> Option<String> {
        self.endpoints.iter().find_map(|(key, endpoint)| {
            let nmos_id = if is_sender {
                endpoint
                    .kind
                    .is_sender()
                    .then(|| endpoint.sender_id(self.node_id))
            } else {
                (!endpoint.kind.is_sender()).then(|| endpoint.receiver_id(self.node_id))
            }?;
            (nmos_id == id).then(|| key.clone())
        })
    }

    fn staged_by_nmos_id(&self, is_sender: bool, id: Uuid) -> Option<Value> {
        let key = self.key_for(is_sender, id)?;
        let endpoint = self.endpoints.get(&key)?;
        Some(staged_json(endpoint))
    }

    fn single_get(&self, kind: &str, id: &str, leaf: &str) -> Result<(u16, Value), (u16, Value)> {
        let is_sender = kind_is_sender(kind)?;
        let id = parse_id(id)?;
        let key = self
            .key_for(is_sender, id)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        let endpoint = self
            .endpoints
            .get(&key)
            .ok_or_else(|| api_error(404, "sender or receiver not found"))?;
        match leaf {
            "staged" => Ok((200, staged_json(endpoint))),
            "active" => Ok((200, active_json(endpoint))),
            "constraints" => Ok((200, constraints_json(&self.domains))),
            "transporttype" => Ok((200, json!("urn:x-nmos:transport:mxl"))),
            "transportfile" if is_sender => Err(api_error(
                404,
                "MXL senders do not provide a transport file",
            )),
            _ => Err(api_error(404, "not found")),
        }
    }

    fn single_index(&self, kind: &str, id: &str) -> Result<(u16, Value), (u16, Value)> {
        let is_sender = kind_is_sender(kind)?;
        let id = parse_id(id)?;
        if self.key_for(is_sender, id).is_none() {
            return Err(api_error(404, "sender or receiver not found"));
        }
        if is_sender {
            Ok((
                200,
                json!([
                    "constraints/",
                    "staged/",
                    "active/",
                    "transportfile/",
                    "transporttype/"
                ]),
            ))
        } else {
            Ok((
                200,
                json!(["constraints/", "staged/", "active/", "transporttype/"]),
            ))
        }
    }

    fn id_list(&self, senders: bool) -> Value {
        let mut ids: Vec<String> = self
            .endpoints
            .values()
            .filter(|endpoint| endpoint.kind.is_sender() == senders)
            .map(|endpoint| {
                let id = if senders {
                    endpoint.sender_id(self.node_id)
                } else {
                    endpoint.receiver_id(self.node_id)
                };
                format!("{id}/")
            })
            .collect();
        ids.sort();
        Value::Array(ids.into_iter().map(Value::String).collect())
    }

    fn publish(&mut self, settings: &NmosSettings) -> Vec<Published> {
        let mut out = vec![Published {
            typ: "node",
            id: self.node_id,
            data: self.node_resource(settings),
        }];
        out.extend(self.devices(settings).into_iter().map(|data| Published {
            typ: "device",
            id: uuid_field(&data),
            data,
        }));
        out.extend(self.sources().into_iter().map(|data| Published {
            typ: "source",
            id: uuid_field(&data),
            data,
        }));
        out.extend(self.flows().into_iter().map(|data| Published {
            typ: "flow",
            id: uuid_field(&data),
            data,
        }));
        out.extend(self.senders().into_iter().map(|data| Published {
            typ: "sender",
            id: uuid_field(&data),
            data,
        }));
        out.extend(self.receivers().into_iter().map(|data| Published {
            typ: "receiver",
            id: uuid_field(&data),
            data,
        }));
        out
    }

    fn node_resource(&mut self, settings: &NmosSettings) -> Value {
        let protocol = if settings.https { "https" } else { "http" };
        let mut value = json!({
            "id": self.node_id.to_string(),
            "label": settings.label,
            "description": "Strom NMOS node for MXL",
            "tags": tags_json(&settings.tags),
            "hostname": self.hostname,
            "href": format!("{protocol}://{}:{}/x-nmos/node/v1.3/self", self.host, settings.port),
            "api": {
                "versions": ["v1.3"],
                "endpoints": [{
                    "host": self.host,
                    "port": settings.port,
                    "protocol": protocol,
                    "authorization": false
                }]
            },
            "caps": {},
            "services": [],
            "clocks": [{"name": "clk0", "ref_type": "internal"}],
            "interfaces": interfaces_json()
        });
        self.stamp("node", &mut value);
        value
    }

    fn devices(&mut self, settings: &NmosSettings) -> Vec<Value> {
        let mut by_flow: HashMap<FlowId, (String, Vec<Uuid>, Vec<Uuid>)> = HashMap::new();
        for endpoint in self.endpoints.values() {
            let entry = by_flow
                .entry(endpoint.flow_id)
                .or_insert_with(|| (endpoint.flow_name.clone(), Vec::new(), Vec::new()));
            if endpoint.kind.is_sender() {
                entry.1.push(endpoint.sender_id(self.node_id));
            } else {
                entry.2.push(endpoint.receiver_id(self.node_id));
            }
        }
        let mut devices: Vec<Value> = by_flow
            .into_iter()
            .map(|(flow_id, (name, mut senders, mut receivers))| {
                senders.sort();
                receivers.sort();
                let id = Endpoint::device_id(self.node_id, flow_id);
                let label = if settings.label.is_empty() {
                    name.clone()
                } else {
                    format!("{} {name}", settings.label)
                };
                let mut value = json!({
                    "id": id.to_string(),
                    "label": label,
                    "description": "Strom flow",
                    "tags": tags_json(&settings.tags),
                    "type": "urn:x-nmos:device:generic",
                    "node_id": self.node_id.to_string(),
                    "senders": senders.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    "receivers": receivers.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    "controls": []
                });
                self.stamp(&format!("device:{id}"), &mut value);
                value
            })
            .collect();
        devices.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        devices
    }

    fn sources(&mut self) -> Vec<Value> {
        self.writer_resources(|endpoint, node_id| {
            let id = endpoint.source_id(node_id);
            let value = json!({
                "id": id.to_string(),
                "label": endpoint.label,
                "description": format!("Source for {}", endpoint.flow_name),
                "tags": tags(&endpoint.group_hint),
                "format": format_urn(endpoint.kind),
                "caps": {},
                "device_id": Endpoint::device_id(node_id, endpoint.flow_id).to_string(),
                "parents": [],
                "clock_name": "clk0"
            });
            (id, value)
        })
    }

    fn flows(&mut self) -> Vec<Value> {
        self.writer_resources(|endpoint, node_id| {
            let id = endpoint.is04_flow_id(node_id);
            let mut value = json!({
                "id": id.to_string(),
                "label": endpoint.label,
                "description": format!("Flow for {}", endpoint.flow_name),
                "tags": tags(&endpoint.group_hint),
                "source_id": endpoint.source_id(node_id).to_string(),
                "device_id": Endpoint::device_id(node_id, endpoint.flow_id).to_string(),
                "parents": []
            });
            match endpoint.kind {
                EndpointKind::VideoSender => {
                    value["format"] = json!("urn:x-nmos:format:video");
                    value["media_type"] = json!("video/v210");
                    value["frame_width"] = json!(1920);
                    value["frame_height"] = json!(1080);
                    value["interlace_mode"] = json!("progressive");
                    value["colorspace"] = json!("BT709");
                    value["grain_rate"] = json!({"numerator": 50, "denominator": 1});
                    value["components"] = json!([
                        {"name": "Y", "width": 1920, "height": 1080, "bit_depth": 10},
                        {"name": "Cb", "width": 960, "height": 1080, "bit_depth": 10},
                        {"name": "Cr", "width": 960, "height": 1080, "bit_depth": 10}
                    ]);
                }
                EndpointKind::AudioSender => {
                    value["format"] = json!("urn:x-nmos:format:audio");
                    value["media_type"] = json!("audio/float32");
                    value["sample_rate"] = json!({"numerator": 48000, "denominator": 1});
                    value["bit_depth"] = json!(32);
                    value["channels"] = json!([
                        {"label": "Channel 1"},
                        {"label": "Channel 2"}
                    ]);
                }
                _ => {}
            }
            (id, value)
        })
    }

    fn senders(&mut self) -> Vec<Value> {
        self.writer_resources(|endpoint, node_id| {
            let id = endpoint.sender_id(node_id);
            let active = endpoint.master_enable();
            let value = json!({
                "id": id.to_string(),
                "label": endpoint.label,
                "description": format!("MXL sender in {}", endpoint.flow_name),
                "tags": tags(&endpoint.group_hint),
                "flow_id": endpoint.is04_flow_id(node_id).to_string(),
                "device_id": Endpoint::device_id(node_id, endpoint.flow_id).to_string(),
                "transport": "urn:x-nmos:transport:mxl",
                "interface_bindings": [],
                "manifest_href": Value::Null,
                "subscription": {
                    "receiver_id": if active {
                        endpoint.active_peer.map(|id| json!(id.to_string())).unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    },
                    "active": active
                },
                "caps": {}
            });
            (id, value)
        })
    }

    fn receivers(&mut self) -> Vec<Value> {
        let mut rows = Vec::new();
        let node_id = self.node_id;
        let keys: Vec<String> = self
            .endpoints
            .iter()
            .filter(|(_, endpoint)| !endpoint.kind.is_sender())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let Some(built) = self.endpoints.get(&key).map(|endpoint| {
                let id = endpoint.receiver_id(node_id);
                let active = endpoint.master_enable();
                let (media_type, constraints) = receiver_caps(endpoint.kind);
                let value = json!({
                    "id": id.to_string(),
                    "label": endpoint.label,
                    "description": format!("MXL receiver in {}", endpoint.flow_name),
                    "tags": tags(&endpoint.group_hint),
                    "format": format_urn(endpoint.kind),
                    "caps": {
                        "media_types": [media_type],
                        // BCP-004-01 requires a TAI timestamp whenever constraint_sets
                        // is present. These caps are fixed for the life of the node.
                        "version": "0:0",
                        "constraint_sets": [constraints]
                    },
                    "device_id": Endpoint::device_id(node_id, endpoint.flow_id).to_string(),
                    "transport": "urn:x-nmos:transport:mxl",
                    "interface_bindings": [],
                    "subscription": {
                        "sender_id": if active {
                            endpoint.active_peer.map(|peer| json!(peer.to_string())).unwrap_or(Value::Null)
                        } else {
                            Value::Null
                        },
                        "active": active
                    }
                });
                (id, value)
            }) else {
                continue;
            };
            let (id, mut value) = built;
            self.stamp(&format!("receiver:{id}"), &mut value);
            rows.push(value);
        }
        rows.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        rows
    }

    fn writer_resources(&mut self, build: impl Fn(&Endpoint, Uuid) -> (Uuid, Value)) -> Vec<Value> {
        let node_id = self.node_id;
        let keys: Vec<String> = self
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.kind.is_sender())
            .map(|(key, _)| key.clone())
            .collect();
        let mut rows = Vec::new();
        for key in keys {
            let Some(endpoint) = self.endpoints.get(&key) else {
                continue;
            };
            let (id, mut value) = build(endpoint, node_id);
            self.stamp(&format!("resource:{id}"), &mut value);
            rows.push(value);
        }
        rows.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        rows
    }

    fn stamp(&mut self, key: &str, value: &mut Value) {
        let fingerprint = fingerprint_without_version(value);
        let version = match self.versions.get(key) {
            Some((previous, version)) if *previous == fingerprint => version.clone(),
            _ => {
                let version = tai_now();
                self.versions
                    .insert(key.to_string(), (fingerprint, version.clone()));
                version
            }
        };
        value["version"] = json!(version);
    }
}

/// A resource ready to POST to an IS-04 registry.
pub struct Published {
    pub typ: &'static str,
    pub id: Uuid,
    pub data: Value,
}

impl Published {
    pub fn version(&self) -> String {
        self.data
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    pub fn registry_key(&self) -> String {
        format!("{}:{}", self.typ, self.id)
    }
}

fn clone_connection(endpoint: &Endpoint) -> Endpoint {
    Endpoint {
        kind: endpoint.kind,
        flow_id: endpoint.flow_id,
        block_id: endpoint.block_id.clone(),
        flow_name: endpoint.flow_name.clone(),
        label: endpoint.label.clone(),
        group_hint: endpoint.group_hint.clone(),
        preferred_domain_path: endpoint.preferred_domain_path.clone(),
        configured_mxl_flow: endpoint.configured_mxl_flow,
        running: endpoint.running,
        dirty: endpoint.dirty,
        generated_flow: endpoint.generated_flow,
        staged_peer: endpoint.staged_peer,
        staged_master: endpoint.staged_master,
        staged_domain: endpoint.staged_domain.clone(),
        staged_flow: endpoint.staged_flow.clone(),
        staged_activation_time: endpoint.staged_activation_time.clone(),
        active_peer: endpoint.active_peer,
        active_domain: endpoint.active_domain,
        active_flow: endpoint.active_flow,
        active_activation_time: endpoint.active_activation_time.clone(),
    }
}

fn apply_patch_fields(
    endpoint: &mut Endpoint,
    is_sender: bool,
    patch: &Value,
    domains: &[MxlDomain],
) -> Result<(), String> {
    if !patch.is_object() {
        return Err("patch body must be an object".to_string());
    }
    if !is_sender {
        if let Some(file) = patch.get("transport_file") {
            let data_null = file.get("data").is_none_or(|value| value.is_null());
            let type_null = file.get("type").is_none_or(|value| value.is_null());
            if !(data_null && type_null) {
                return Err(
                    "MXL receivers do not accept a transport file; omit it or set data and type to null"
                        .to_string(),
                );
            }
        }
    }
    let peer_field = if is_sender {
        "receiver_id"
    } else {
        "sender_id"
    };
    if let Some(peer) = patch.get(peer_field) {
        endpoint.staged_peer = optional_uuid(peer)?;
    }
    if let Some(enable) = patch.get("master_enable") {
        endpoint.staged_master = enable
            .as_bool()
            .ok_or_else(|| "master_enable must be a boolean".to_string())?;
    }
    if let Some(params) = patch.get("transport_params") {
        let legs = params
            .as_array()
            .ok_or_else(|| "transport_params must be an array".to_string())?;
        if legs.len() != 1 {
            return Err("MXL transport_params must contain exactly one leg".to_string());
        }
        let leg = &legs[0];
        if let Some(domain) = leg.get("mxl_domain_id") {
            let parsed = Param::from_json(domain)?;
            check_param(is_sender, "mxl_domain_id", &parsed, domains)?;
            endpoint.staged_domain = parsed;
        }
        if let Some(flow) = leg.get("mxl_flow_id") {
            let parsed = Param::from_json(flow)?;
            check_param(is_sender, "mxl_flow_id", &parsed, domains)?;
            endpoint.staged_flow = parsed;
        }
    }
    Ok(())
}

fn check_param(
    is_sender: bool,
    name: &str,
    param: &Param,
    domains: &[MxlDomain],
) -> Result<(), String> {
    match param {
        Param::Null => Ok(()),
        Param::Auto => {
            if !is_sender && name == "mxl_flow_id" {
                Err("receivers must not accept auto for mxl_flow_id".to_string())
            } else {
                Ok(())
            }
        }
        Param::Id(id) => {
            if name == "mxl_domain_id"
                && !domains.is_empty()
                && !domains.iter().any(|d| d.id == *id)
            {
                Err(format!(
                    "mxl_domain_id {id} is not a domain this node can access"
                ))
            } else if name == "mxl_domain_id" && domains.is_empty() {
                Err("no MXL domains with domain_def.json are visible".to_string())
            } else {
                Ok(())
            }
        }
    }
}

fn patch_is_immediate(patch: &Value) -> Result<bool, String> {
    let Some(activation) = patch.get("activation") else {
        return Ok(false);
    };
    let mode = activation.get("mode").unwrap_or(&Value::Null);
    if mode.is_null() {
        return Ok(false);
    }
    match mode.as_str() {
        Some("activate_immediate") => Ok(true),
        Some("activate_scheduled_absolute") | Some("activate_scheduled_relative") => {
            Err("scheduled activation is not implemented".to_string())
        }
        _ => Err("activation.mode is not a recognised IS-05 mode".to_string()),
    }
}

fn resolve_activation(endpoint: &mut Endpoint, domains: &[MxlDomain]) -> Result<Resolved, String> {
    let domain = resolve_domain(endpoint, domains)?;
    let flow = resolve_flow(endpoint, domains)?;
    if endpoint.staged_master && flow.is_none() {
        return Err("mxl_flow_id could not be resolved".to_string());
    }
    if endpoint.staged_master && domain.is_none() {
        return Err("mxl_domain_id could not be resolved".to_string());
    }
    Ok(Resolved {
        domain,
        flow,
        peer: endpoint.staged_peer,
        master_enable: endpoint.staged_master,
        activation_time: tai_now(),
    })
}

fn resolve_domain(endpoint: &Endpoint, domains: &[MxlDomain]) -> Result<Option<Uuid>, String> {
    match &endpoint.staged_domain {
        Param::Null => Ok(None),
        Param::Id(id) => Ok(Some(*id)),
        Param::Auto => {
            if let Some(domain) = domains
                .iter()
                .find(|domain| domain.path.display().to_string() == endpoint.preferred_domain_path)
            {
                return Ok(Some(domain.id));
            }
            if domains.len() == 1 {
                return Ok(Some(domains[0].id));
            }
            if !endpoint.kind.is_sender() {
                if let Param::Id(flow_id) = &endpoint.staged_flow {
                    if let Some(domain) = domains
                        .iter()
                        .find(|domain| domain_has_flow(&domain.path, *flow_id))
                    {
                        return Ok(Some(domain.id));
                    }
                }
            }
            Err("auto mxl_domain_id could not be resolved".to_string())
        }
    }
}

fn resolve_flow(endpoint: &mut Endpoint, _domains: &[MxlDomain]) -> Result<Option<Uuid>, String> {
    match endpoint.staged_flow {
        Param::Null => Ok(None),
        Param::Id(id) => Ok(Some(id)),
        Param::Auto if endpoint.kind.is_sender() => {
            if let Some(id) = endpoint.configured_mxl_flow.or(endpoint.generated_flow) {
                endpoint.generated_flow = Some(id);
                return Ok(Some(id));
            }
            let id = Uuid::new_v4();
            endpoint.generated_flow = Some(id);
            Ok(Some(id))
        }
        Param::Auto => Err("receivers must not accept auto for mxl_flow_id".to_string()),
    }
}

fn seed_from_block(endpoint: &mut Endpoint, domains: &[MxlDomain]) {
    let domain = domains
        .iter()
        .find(|domain| domain.path.display().to_string() == endpoint.preferred_domain_path)
        .map(|domain| domain.id)
        .or_else(|| (domains.len() == 1).then(|| domains[0].id));
    endpoint.staged_domain = match domain {
        Some(id) => Param::Id(id),
        None => Param::Null,
    };
    endpoint.staged_flow = match endpoint.configured_mxl_flow {
        Some(id) => Param::Id(id),
        None => Param::Null,
    };
    endpoint.staged_master = endpoint.running && endpoint.configured_mxl_flow.is_some();
    endpoint.staged_peer = None;
    endpoint.active_domain = domain;
    endpoint.active_flow = endpoint.configured_mxl_flow;
    endpoint.active_peer = None;
}

fn staged_json(endpoint: &Endpoint) -> Value {
    let mut value = json!({
        "master_enable": endpoint.staged_master,
        "activation": activation_json(endpoint.staged_activation_time.as_deref()),
        "transport_params": [{
            "mxl_domain_id": endpoint.staged_domain.to_json(),
            "mxl_flow_id": endpoint.staged_flow.to_json()
        }]
    });
    if endpoint.kind.is_sender() {
        value["receiver_id"] = optional_uuid_json(endpoint.staged_peer);
    } else {
        value["sender_id"] = optional_uuid_json(endpoint.staged_peer);
        value["transport_file"] = json!({"data": Value::Null, "type": Value::Null});
    }
    value
}

fn active_json(endpoint: &Endpoint) -> Value {
    let mut value = json!({
        "master_enable": endpoint.master_enable(),
        "activation": activation_json(endpoint.active_activation_time.as_deref()),
        "transport_params": [{
            "mxl_domain_id": optional_uuid_json(endpoint.active_domain),
            "mxl_flow_id": optional_uuid_json(endpoint.active_flow)
        }]
    });
    let peer = if endpoint.master_enable() {
        endpoint.active_peer
    } else {
        None
    };
    if endpoint.kind.is_sender() {
        value["receiver_id"] = optional_uuid_json(peer.or(endpoint.active_peer));
    } else {
        value["sender_id"] = optional_uuid_json(peer.or(endpoint.active_peer));
        value["transport_file"] = json!({"data": Value::Null, "type": Value::Null});
    }
    value
}

fn activation_json(activation_time: Option<&str>) -> Value {
    json!({
        "mode": Value::Null,
        "requested_time": Value::Null,
        "activation_time": activation_time.map(|t| json!(t)).unwrap_or(Value::Null)
    })
}

fn constraints_json(domains: &[MxlDomain]) -> Value {
    let domain_constraint = if domains.is_empty() {
        json!({})
    } else {
        json!({"enum": domains.iter().map(|d| d.id.to_string()).collect::<Vec<_>>()})
    };
    json!([{
        "mxl_domain_id": domain_constraint,
        "mxl_flow_id": {}
    }])
}

fn optional_uuid(value: &Value) -> Result<Option<Uuid>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .ok_or_else(|| "id must be a UUID or null".to_string())?;
    Uuid::parse_str(text)
        .map(Some)
        .map_err(|_| format!("'{text}' is not a UUID"))
}

fn optional_uuid_json(id: Option<Uuid>) -> Value {
    id.map(|id| json!(id.to_string())).unwrap_or(Value::Null)
}

fn configured_flow_id(block: &BlockInstance, kind: EndpointKind) -> Option<Uuid> {
    let key = match kind {
        EndpointKind::VideoSender | EndpointKind::AudioSender => "flow_id",
        EndpointKind::VideoReceiver => "video_flow_id",
        EndpointKind::AudioReceiver => "audio_flow_id",
    };
    let text = prop_string(&block.properties, key);
    Uuid::parse_str(text.trim()).ok()
}

fn prop_string(properties: &HashMap<String, PropertyValue>, key: &str) -> String {
    match properties.get(key) {
        Some(PropertyValue::String(value)) => value.clone(),
        _ => String::new(),
    }
}

fn endpoint_key(flow_id: FlowId, block_id: &str) -> String {
    format!("{flow_id}:{block_id}")
}

fn default_label(kind: EndpointKind) -> String {
    match kind {
        EndpointKind::VideoSender => "MXL Video Output".to_string(),
        EndpointKind::AudioSender => "MXL Audio Output".to_string(),
        EndpointKind::VideoReceiver => "MXL Video Input".to_string(),
        EndpointKind::AudioReceiver => "MXL Audio Input".to_string(),
    }
}

fn format_urn(kind: EndpointKind) -> &'static str {
    match kind {
        EndpointKind::VideoSender | EndpointKind::VideoReceiver => "urn:x-nmos:format:video",
        EndpointKind::AudioSender | EndpointKind::AudioReceiver => "urn:x-nmos:format:audio",
    }
}

fn tags(group_hint: &str) -> Value {
    if group_hint.is_empty() {
        json!({})
    } else {
        json!({"urn:x-nmos:tag:grouphint/v1.0": [group_hint]})
    }
}

fn receiver_caps(kind: EndpointKind) -> (&'static str, Value) {
    match kind {
        EndpointKind::VideoReceiver | EndpointKind::VideoSender => (
            "video/v210",
            json!({
                "urn:x-nmos:cap:format:frame_width": {"enum": [1920]},
                "urn:x-nmos:cap:format:frame_height": {"enum": [1080]},
                "urn:x-nmos:cap:format:grain_rate": {"enum": [
                    {"numerator": 25, "denominator": 1},
                    {"numerator": 30, "denominator": 1},
                    {"numerator": 50, "denominator": 1},
                    {"numerator": 60, "denominator": 1}
                ]},
                "urn:x-nmos:cap:format:interlace_mode": {"enum": ["progressive"]},
                "urn:x-nmos:cap:format:color_sampling": {"enum": ["YCbCr-4:2:2"]},
                "urn:x-nmos:cap:format:component_depth": {"enum": [10]}
            }),
        ),
        EndpointKind::AudioReceiver | EndpointKind::AudioSender => (
            "audio/float32",
            json!({
                "urn:x-nmos:cap:format:channel_count": {"enum": [1, 2, 4, 8, 16]},
                "urn:x-nmos:cap:format:sample_rate": {"enum": [{"numerator": 48000, "denominator": 1}]},
                "urn:x-nmos:cap:format:sample_depth": {"enum": [32]}
            }),
        ),
    }
}

fn interfaces_json() -> Value {
    let discovered = crate::network::discover_interfaces();
    let mut chosen: Vec<_> = discovered
        .interfaces
        .iter()
        .filter(|iface| iface.is_up && !iface.is_loopback)
        .collect();
    if chosen.is_empty() {
        chosen = discovered
            .interfaces
            .iter()
            .filter(|iface| iface.is_loopback)
            .collect();
    }
    // IS-04 requires port_id to be a MAC address. Omit attached_network_device
    // when LLDP data is absent; null is not a valid object there.
    let rows = chosen
        .into_iter()
        .filter_map(|iface| {
            let port_id = nmos_mac(iface.mac_address.as_deref())?;
            Some(json!({
                "name": iface.name,
                "chassis_id": Value::Null,
                "port_id": port_id
            }))
        })
        .collect();
    Value::Array(rows)
}

/// IS-04 `port_id`: six lowercase hex octets separated by hyphens.
fn nmos_mac(mac: Option<&str>) -> Option<String> {
    let mac = mac?;
    let bytes = mac
        .split([':', '-', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let [a, b, c, d, e, f] = bytes.try_into().ok()?;
    Some(format!("{a:02x}-{b:02x}-{c:02x}-{d:02x}-{e:02x}-{f:02x}"))
}

fn uuid_field(value: &Value) -> Uuid {
    value
        .get("id")
        .and_then(|id| id.as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(Uuid::nil)
}

fn fingerprint_without_version(value: &Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut copy = value.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("version");
    }
    let text = copy.to_string();
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn tai_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}:{}", now.as_secs(), now.subsec_nanos())
}

fn discover_domains(settings: &NmosSettings) -> Vec<MxlDomain> {
    let mut domains = scan_domains(&settings.domain_paths);
    domains.extend(scan_domain_root(&settings.scan_path));
    if let Some(dir) = &settings.output_domain_dir {
        domains.extend(scan_domains(std::slice::from_ref(dir)));
    }
    domains.sort_by_key(|domain| domain.id);
    domains.dedup_by_key(|domain| domain.id);
    domains
}

fn tags_json(tags: &std::collections::HashMap<String, Vec<String>>) -> Value {
    let mut object = serde_json::Map::new();
    for (key, values) in tags {
        object.insert(key.clone(), json!(values));
    }
    Value::Object(object)
}

fn ensure_node_id(settings: &NmosSettings) -> Uuid {
    if let Some(seed) = settings.seed.as_ref().filter(|seed| !seed.is_empty()) {
        let id = super::settings::node_id_from_seed(seed);
        if let Some(path) = &settings.id_path {
            let current = std::fs::read_to_string(path).ok();
            if current.as_deref().map(str::trim) != Some(id.to_string().as_str()) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, id.to_string());
            }
        }
        return id;
    }
    if let Some(id) = settings.node_id {
        return id;
    }
    if let Some(path) = &settings.id_path {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(id) = Uuid::parse_str(text.trim()) {
                return id;
            }
        }
        let id = Uuid::new_v4();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, id.to_string());
        return id;
    }
    Uuid::new_v4()
}

/// Address placed in IS-04 `href` and `api.endpoints[].host`.
/// A configured host is used as given. Otherwise the first non-loopback IPv4.
/// Loopback is only a last resort when nothing else exists (NMOS disabled).
fn advertise_host(settings: &NmosSettings) -> String {
    if let Some(host) = settings
        .host
        .as_ref()
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
    {
        return host.to_string();
    }
    first_routable_ipv4().unwrap_or_else(|| "127.0.0.1".to_string())
}

pub fn first_routable_ipv4() -> Option<String> {
    let discovered = crate::network::discover_interfaces();
    discovered
        .interfaces
        .iter()
        .filter(|iface| iface.is_up && !iface.is_loopback)
        .find_map(|iface| {
            iface.ipv4_addresses.iter().find_map(|addr| {
                let ip = addr.address.parse::<std::net::Ipv4Addr>().ok()?;
                (!ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local()).then_some(ip)
            })
        })
        .map(|ip| ip.to_string())
}

/// Accept only an IPv4 literal that other systems can route to.
pub fn require_announce_ipv4(value: &str) -> Result<String, String> {
    let ip: std::net::Ipv4Addr = value
        .trim()
        .parse()
        .map_err(|_| format!("'{value}' is not an IPv4 address"))?;
    if ip.is_unspecified() || ip.is_loopback() {
        return Err(format!(
            "'{value}' must not be 0.0.0.0 or 127.0.0.1; set NMOS_HOST_ADDRESS to a routable address"
        ));
    }
    Ok(ip.to_string())
}

fn hostname_string() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "strom".to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod interface_tests {
    use super::nmos_mac;

    #[test]
    fn mac_uses_is04_hyphen_form() {
        assert_eq!(
            nmos_mac(Some("AA:BB:CC:DD:EE:FF")).as_deref(),
            Some("aa-bb-cc-dd-ee-ff")
        );
        assert_eq!(
            nmos_mac(Some("aa-bb-cc-dd-ee-ff")).as_deref(),
            Some("aa-bb-cc-dd-ee-ff")
        );
        assert!(nmos_mac(None).is_none());
        assert!(nmos_mac(Some("enp0s5")).is_none());
    }
}
