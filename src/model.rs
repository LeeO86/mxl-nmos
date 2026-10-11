//! The resource store: what the application inserted, the IS-05 connection state
//! of every Sender and Receiver, and the IS-04 documents with their versions.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::activation::{ActiveConnection, MxlDomain, Param, Role, StagedConnection};
use crate::clock::TaiTime;
use crate::ids::resource_id;
use crate::resources::{
    Device, Flow, FlowFormat, Rational, Receiver, ResourceType, Sender, Source, Tags,
};
use crate::settings::Settings;
use crate::Error;

/// IS-05 activation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Immediate,
    ScheduledAbsolute,
    ScheduledRelative,
}

impl Mode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Immediate => "activate_immediate",
            Self::ScheduledAbsolute => "activate_scheduled_absolute",
            Self::ScheduledRelative => "activate_scheduled_relative",
        }
    }
}

/// The `activation` object of `/staged` or `/active`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActivationRecord {
    pub mode: Option<Mode>,
    pub requested_time: Option<TaiTime>,
    pub activation_time: Option<TaiTime>,
}

impl ActivationRecord {
    pub(crate) fn json(&self) -> Value {
        json!({
            "mode": self.mode.map(Mode::name),
            "requested_time": self.requested_time.map(|t| t.to_string()),
            "activation_time": self.activation_time.map(|t| t.to_string()),
        })
    }
}

/// IS-05 state of one Sender or Receiver.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectionState {
    pub staged: StagedConnection,
    /// A pending scheduled activation, shown in `/staged` and locking it.
    pub pending: Option<Pending>,
    pub active: ActiveConnection,
    pub active_activation: ActivationRecord,
    /// The random MXL flow id a Sender without a configured id resolved `auto` to.
    pub generated_flow: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pending {
    pub seq: u64,
    pub record: ActivationRecord,
}

#[derive(Debug, Clone)]
pub(crate) struct SenderEntry {
    pub spec: Sender,
    pub conn: ConnectionState,
}

#[derive(Debug, Clone)]
pub(crate) struct ReceiverEntry {
    pub spec: Receiver,
    pub conn: ConnectionState,
    pub caps_version: TaiTime,
}

/// A published IS-04 document.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Doc {
    pub typ: ResourceType,
    pub id: Uuid,
    /// The document without `version`.
    pub body: Value,
    pub version: TaiTime,
}

impl Doc {
    pub(crate) fn json(&self) -> Value {
        let mut value = self.body.clone();
        value["version"] = json!(self.version.to_string());
        value
    }
}

pub(crate) struct Model {
    pub node_id: Uuid,
    pub host: Ipv4Addr,
    pub hostname: Option<String>,
    pub interfaces: Value,
    pub devices: BTreeMap<String, Device>,
    pub sources: BTreeMap<String, Source>,
    pub flows: BTreeMap<String, Flow>,
    pub senders: BTreeMap<String, SenderEntry>,
    pub receivers: BTreeMap<String, ReceiverEntry>,
    pub docs: BTreeMap<(ResourceType, Uuid), Doc>,
    /// Last version of every id ever published, so a re-inserted resource still
    /// gets a later version.
    last_versions: HashMap<Uuid, TaiTime>,
    /// Ids whose version the next refresh bumps even when unchanged (activations).
    touched: Vec<Uuid>,
    pub schedule_seq: u64,
}

impl Model {
    pub(crate) fn new(
        node_id: Uuid,
        host: Ipv4Addr,
        hostname: Option<String>,
        interfaces: Value,
    ) -> Self {
        Self {
            node_id,
            host,
            hostname,
            interfaces,
            devices: BTreeMap::new(),
            sources: BTreeMap::new(),
            flows: BTreeMap::new(),
            senders: BTreeMap::new(),
            receivers: BTreeMap::new(),
            docs: BTreeMap::new(),
            last_versions: HashMap::new(),
            touched: Vec::new(),
            schedule_seq: 0,
        }
    }

    pub(crate) fn id(&self, typ: ResourceType, key: &str) -> Uuid {
        match typ {
            ResourceType::Node => self.node_id,
            other => resource_id(self.node_id, other.name(), key),
        }
    }

    fn version_after(&self, id: Uuid, now: TaiTime) -> TaiTime {
        match self.last_versions.get(&id) {
            Some(last) if *last >= now => last.next(),
            _ => now,
        }
    }

    // ---- inserts ----------------------------------------------------------

    pub(crate) fn insert_device(&mut self, device: Device) {
        self.devices.insert(device.key.clone(), device);
    }

    pub(crate) fn insert_source(&mut self, source: Source) -> Result<(), Error> {
        self.require(ResourceType::Device, &source.device_key)?;
        for parent in &source.parents {
            self.require(ResourceType::Source, parent)?;
        }
        self.sources.insert(source.key.clone(), source);
        Ok(())
    }

    pub(crate) fn insert_flow(&mut self, flow: Flow) -> Result<(), Error> {
        self.require(ResourceType::Device, &flow.device_key)?;
        self.require(ResourceType::Source, &flow.source_key)?;
        for parent in &flow.parents {
            self.require(ResourceType::Flow, parent)?;
        }
        self.flows.insert(flow.key.clone(), flow);
        Ok(())
    }

    pub(crate) fn insert_sender(&mut self, sender: Sender) -> Result<(), Error> {
        self.require(ResourceType::Device, &sender.device_key)?;
        if let Some(flow) = &sender.flow_key {
            self.require(ResourceType::Flow, flow)?;
        }
        match self.senders.get_mut(&sender.key) {
            Some(entry) => entry.spec = sender,
            None => {
                self.senders.insert(
                    sender.key.clone(),
                    SenderEntry {
                        spec: sender,
                        conn: ConnectionState::default(),
                    },
                );
            }
        }
        Ok(())
    }

    pub(crate) fn insert_receiver(
        &mut self,
        receiver: Receiver,
        now: TaiTime,
    ) -> Result<(), Error> {
        self.require(ResourceType::Device, &receiver.device_key)?;
        match self.receivers.get_mut(&receiver.key) {
            Some(entry) => {
                if entry.spec.caps != receiver.caps {
                    entry.caps_version = if now > entry.caps_version {
                        now
                    } else {
                        entry.caps_version.next()
                    };
                }
                entry.spec = receiver;
            }
            None => {
                self.receivers.insert(
                    receiver.key.clone(),
                    ReceiverEntry {
                        spec: receiver,
                        conn: ConnectionState::default(),
                        caps_version: now,
                    },
                );
            }
        }
        Ok(())
    }

    fn require(&self, typ: ResourceType, key: &str) -> Result<(), Error> {
        let found = match typ {
            ResourceType::Device => self.devices.contains_key(key),
            ResourceType::Source => self.sources.contains_key(key),
            ResourceType::Flow => self.flows.contains_key(key),
            ResourceType::Sender => self.senders.contains_key(key),
            ResourceType::Receiver => self.receivers.contains_key(key),
            ResourceType::Node => true,
        };
        if found {
            Ok(())
        } else {
            Err(Error::UnknownResource {
                typ,
                key: key.to_string(),
            })
        }
    }

    /// Remove a resource and everything that refers to it.
    pub(crate) fn erase(&mut self, typ: ResourceType, key: &str) -> Result<(), Error> {
        self.require(typ, key)?;
        match typ {
            ResourceType::Node => {
                return Err(Error::UnknownResource {
                    typ,
                    key: key.to_string(),
                })
            }
            ResourceType::Device => {
                self.devices.remove(key);
                let sources: Vec<String> = self
                    .sources
                    .values()
                    .filter(|s| s.device_key == key)
                    .map(|s| s.key.clone())
                    .collect();
                for source in sources {
                    let _ = self.erase(ResourceType::Source, &source);
                }
                let flows: Vec<String> = self
                    .flows
                    .values()
                    .filter(|f| f.device_key == key)
                    .map(|f| f.key.clone())
                    .collect();
                for flow in flows {
                    let _ = self.erase(ResourceType::Flow, &flow);
                }
                self.senders.retain(|_, s| s.spec.device_key != key);
                self.receivers.retain(|_, r| r.spec.device_key != key);
            }
            ResourceType::Source => {
                self.sources.remove(key);
                let flows: Vec<String> = self
                    .flows
                    .values()
                    .filter(|f| f.source_key == key)
                    .map(|f| f.key.clone())
                    .collect();
                for flow in flows {
                    let _ = self.erase(ResourceType::Flow, &flow);
                }
                for source in self.sources.values_mut() {
                    source.parents.retain(|p| p != key);
                }
            }
            ResourceType::Flow => {
                self.flows.remove(key);
                self.senders
                    .retain(|_, s| s.spec.flow_key.as_deref() != Some(key));
                for flow in self.flows.values_mut() {
                    flow.parents.retain(|p| p != key);
                }
            }
            ResourceType::Sender => {
                self.senders.remove(key);
            }
            ResourceType::Receiver => {
                self.receivers.remove(key);
            }
        }
        Ok(())
    }

    pub(crate) fn conn_mut(&mut self, role: Role, key: &str) -> Option<&mut ConnectionState> {
        match role {
            Role::Sender => self.senders.get_mut(key).map(|e| &mut e.conn),
            Role::Receiver => self.receivers.get_mut(key).map(|e| &mut e.conn),
        }
    }

    pub(crate) fn conn(&self, role: Role, key: &str) -> Option<&ConnectionState> {
        match role {
            Role::Sender => self.senders.get(key).map(|e| &e.conn),
            Role::Receiver => self.receivers.get(key).map(|e| &e.conn),
        }
    }

    /// The application key of the Sender or Receiver with NMOS id `id`.
    pub(crate) fn key_for(&self, role: Role, id: Uuid) -> Option<String> {
        let (typ, keys): (ResourceType, Vec<&String>) = match role {
            Role::Sender => (ResourceType::Sender, self.senders.keys().collect()),
            Role::Receiver => (ResourceType::Receiver, self.receivers.keys().collect()),
        };
        keys.into_iter()
            .find(|key| self.id(typ, key) == id)
            .cloned()
    }

    pub(crate) fn ids(&self, role: Role) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = match role {
            Role::Sender => self
                .senders
                .keys()
                .map(|k| self.id(ResourceType::Sender, k))
                .collect(),
            Role::Receiver => self
                .receivers
                .keys()
                .map(|k| self.id(ResourceType::Receiver, k))
                .collect(),
        };
        ids.sort();
        ids
    }

    // ---- IS-04 documents --------------------------------------------------

    /// Bump the version of a resource at the next refresh even if it did not
    /// change: IS-05 clients expect a new IS-04 version after every activation.
    pub(crate) fn touch(&mut self, typ: ResourceType, key: &str) {
        let id = self.id(typ, key);
        self.touched.push(id);
    }

    /// Rebuild every document and bump the version of each one that changed.
    pub(crate) fn refresh(&mut self, settings: &Settings, now: TaiTime) {
        let touched = std::mem::take(&mut self.touched);
        let mut next = BTreeMap::new();
        for (typ, id, body) in self.build_all(settings) {
            let version = match self.docs.get(&(typ, id)) {
                Some(old) if old.body == body && !touched.contains(&id) => old.version,
                _ => self.version_after(id, now),
            };
            self.last_versions.insert(id, version);
            next.insert(
                (typ, id),
                Doc {
                    typ,
                    id,
                    body,
                    version,
                },
            );
        }
        self.docs = next;
    }

    /// Documents in registration order.
    pub(crate) fn ordered_docs(&self) -> Vec<Doc> {
        let mut docs: Vec<Doc> = self.docs.values().cloned().collect();
        docs.sort_by_key(|doc| (doc.typ.rank(), doc.id));
        docs
    }

    pub(crate) fn docs_of(&self, typ: ResourceType) -> Vec<Value> {
        self.docs
            .values()
            .filter(|doc| doc.typ == typ)
            .map(Doc::json)
            .collect()
    }

    fn build_all(&self, settings: &Settings) -> Vec<(ResourceType, Uuid, Value)> {
        let mut out = vec![(ResourceType::Node, self.node_id, self.node_body(settings))];
        for device in self.devices.values() {
            out.push((
                ResourceType::Device,
                self.id(ResourceType::Device, &device.key),
                self.device_body(settings, device),
            ));
        }
        for source in self.sources.values() {
            out.push((
                ResourceType::Source,
                self.id(ResourceType::Source, &source.key),
                self.source_body(source),
            ));
        }
        for flow in self.flows.values() {
            out.push((
                ResourceType::Flow,
                self.id(ResourceType::Flow, &flow.key),
                self.flow_body(flow),
            ));
        }
        for sender in self.senders.values() {
            out.push((
                ResourceType::Sender,
                self.id(ResourceType::Sender, &sender.spec.key),
                self.sender_body(sender),
            ));
        }
        for receiver in self.receivers.values() {
            out.push((
                ResourceType::Receiver,
                self.id(ResourceType::Receiver, &receiver.spec.key),
                self.receiver_body(receiver),
            ));
        }
        out
    }

    pub(crate) fn base_url(&self, settings: &Settings) -> String {
        let protocol = if settings.https { "https" } else { "http" };
        format!("{protocol}://{}:{}", self.host, settings.port)
    }

    fn node_body(&self, settings: &Settings) -> Value {
        let protocol = if settings.https { "https" } else { "http" };
        let mut value = json!({
            "id": self.node_id.to_string(),
            "label": settings.label,
            "description": settings.description,
            "tags": tags_json(&settings.tags, None),
            "href": format!("{}/", self.base_url(settings)),
            "api": {
                "versions": ["v1.3"],
                "endpoints": [{
                    "host": self.host.to_string(),
                    "port": settings.port,
                    "protocol": protocol,
                    "authorization": false
                }]
            },
            "caps": {},
            "services": [],
            "clocks": [{"name": "clk0", "ref_type": "internal"}],
            "interfaces": self.interfaces,
        });
        if let Some(hostname) = &self.hostname {
            value["hostname"] = json!(hostname);
        }
        value
    }

    fn device_body(&self, settings: &Settings, device: &Device) -> Value {
        let mut senders: Vec<String> = self
            .senders
            .values()
            .filter(|s| s.spec.device_key == device.key)
            .map(|s| self.id(ResourceType::Sender, &s.spec.key).to_string())
            .collect();
        senders.sort();
        let mut receivers: Vec<String> = self
            .receivers
            .values()
            .filter(|r| r.spec.device_key == device.key)
            .map(|r| self.id(ResourceType::Receiver, &r.spec.key).to_string())
            .collect();
        receivers.sort();
        let label = if settings.label.is_empty() {
            device.name.clone()
        } else {
            format!("{} {}", settings.label, device.name)
        };
        json!({
            "id": self.id(ResourceType::Device, &device.key).to_string(),
            "label": label,
            "description": device.description,
            "tags": tags_json(&settings.tags, Some(&device.tags)),
            "type": device.device_type,
            "node_id": self.node_id.to_string(),
            "senders": senders,
            "receivers": receivers,
            "controls": [{
                "type": "urn:x-nmos:control:sr-ctrl/v1.2",
                "href": format!("{}/x-nmos/connection/v1.2/", self.base_url(settings)),
                "authorization": false
            }]
        })
    }

    fn source_body(&self, source: &Source) -> Value {
        let mut value = json!({
            "id": self.id(ResourceType::Source, &source.key).to_string(),
            "label": source.label,
            "description": source.description,
            "tags": tags_json(&Tags::new(), Some(&source.tags)),
            "format": source.format.urn(),
            "caps": {},
            "device_id": self.id(ResourceType::Device, &source.device_key).to_string(),
            "parents": source.parents.iter()
                .map(|p| self.id(ResourceType::Source, p).to_string())
                .collect::<Vec<_>>(),
            "clock_name": "clk0"
        });
        if let Some(rate) = source.grain_rate {
            value["grain_rate"] = rational(rate);
        }
        if source.format == crate::resources::Format::Audio {
            value["channels"] = Value::Array(
                source
                    .channels
                    .iter()
                    .map(|c| {
                        let mut ch = json!({"label": c.label});
                        if let Some(symbol) = &c.symbol {
                            ch["symbol"] = json!(symbol);
                        }
                        ch
                    })
                    .collect(),
            );
        }
        value
    }

    fn flow_body(&self, flow: &Flow) -> Value {
        let mut value = json!({
            "id": self.id(ResourceType::Flow, &flow.key).to_string(),
            "label": flow.label,
            "description": flow.description,
            "tags": tags_json(&Tags::new(), Some(&flow.tags)),
            "format": flow.nmos_format().urn(),
            "media_type": flow.media_type,
            "source_id": self.id(ResourceType::Source, &flow.source_key).to_string(),
            "device_id": self.id(ResourceType::Device, &flow.device_key).to_string(),
            "parents": flow.parents.iter()
                .map(|p| self.id(ResourceType::Flow, p).to_string())
                .collect::<Vec<_>>(),
        });
        if let Some(rate) = flow.grain_rate {
            value["grain_rate"] = rational(rate);
        }
        match &flow.format {
            FlowFormat::Video {
                frame_width,
                frame_height,
                interlace_mode,
                colorspace,
                transfer_characteristic,
                components,
            } => {
                value["frame_width"] = json!(frame_width);
                value["frame_height"] = json!(frame_height);
                value["interlace_mode"] = json!(interlace_mode);
                value["colorspace"] = json!(colorspace);
                if let Some(tc) = transfer_characteristic {
                    value["transfer_characteristic"] = json!(tc);
                }
                value["components"] = Value::Array(
                    components
                        .iter()
                        .map(|c| {
                            json!({"name": c.name, "width": c.width, "height": c.height, "bit_depth": c.bit_depth})
                        })
                        .collect(),
                );
            }
            FlowFormat::Audio {
                sample_rate,
                bit_depth,
            } => {
                value["sample_rate"] = rational(*sample_rate);
                if let Some(depth) = bit_depth {
                    value["bit_depth"] = json!(depth);
                }
            }
            FlowFormat::Data => {}
        }
        value
    }

    fn sender_body(&self, sender: &SenderEntry) -> Value {
        let active = &sender.conn.active;
        json!({
            "id": self.id(ResourceType::Sender, &sender.spec.key).to_string(),
            "label": sender.spec.label,
            "description": sender.spec.description,
            "tags": tags_json(&Tags::new(), Some(&sender.spec.tags)),
            "flow_id": sender.spec.flow_key.as_ref()
                .map(|f| json!(self.id(ResourceType::Flow, f).to_string()))
                .unwrap_or(Value::Null),
            "device_id": self.id(ResourceType::Device, &sender.spec.device_key).to_string(),
            "transport": "urn:x-nmos:transport:mxl",
            "interface_bindings": [],
            "manifest_href": Value::Null,
            "subscription": {
                "receiver_id": if active.master_enable { uuid_json(active.peer_id) } else { Value::Null },
                "active": active.master_enable
            },
            "caps": {}
        })
    }

    fn receiver_body(&self, receiver: &ReceiverEntry) -> Value {
        let active = &receiver.conn.active;
        let spec = &receiver.spec;
        let mut caps = json!({"media_types": spec.caps.media_types});
        if !spec.caps.constraint_sets.is_empty() {
            caps["constraint_sets"] = Value::Array(spec.caps.constraint_sets.clone());
            caps["version"] = json!(spec
                .caps
                .version
                .clone()
                .unwrap_or_else(|| receiver.caps_version.to_string()));
        } else if let Some(version) = &spec.caps.version {
            caps["version"] = json!(version);
        }
        json!({
            "id": self.id(ResourceType::Receiver, &spec.key).to_string(),
            "label": spec.label,
            "description": spec.description,
            "tags": tags_json(&Tags::new(), Some(&spec.tags)),
            "format": spec.format.urn(),
            "caps": caps,
            "device_id": self.id(ResourceType::Device, &spec.device_key).to_string(),
            "transport": "urn:x-nmos:transport:mxl",
            "interface_bindings": [],
            "subscription": {
                "sender_id": if active.master_enable { uuid_json(active.peer_id) } else { Value::Null },
                "active": active.master_enable
            }
        })
    }

    // ---- IS-05 documents --------------------------------------------------

    pub(crate) fn staged_json(
        &self,
        role: Role,
        conn: &ConnectionState,
        activation: ActivationRecord,
    ) -> Value {
        let staged = &conn.staged;
        let mut value = json!({
            "master_enable": staged.master_enable,
            "activation": activation.json(),
            "transport_params": [{
                "mxl_domain_id": param_json(staged.mxl_domain_id),
                "mxl_flow_id": param_json(staged.mxl_flow_id)
            }]
        });
        peer_fields(&mut value, role, staged.peer_id);
        value
    }

    pub(crate) fn active_json(&self, role: Role, conn: &ConnectionState) -> Value {
        let active = &conn.active;
        let mut value = json!({
            "master_enable": active.master_enable,
            "activation": conn.active_activation.json(),
            "transport_params": [{
                "mxl_domain_id": uuid_json(active.mxl_domain_id),
                "mxl_flow_id": uuid_json(active.mxl_flow_id)
            }]
        });
        peer_fields(&mut value, role, active.peer_id);
        value
    }
}

pub(crate) fn constraints_json(domains: &[MxlDomain]) -> Value {
    let domain = if domains.is_empty() {
        json!({})
    } else {
        let mut ids: Vec<String> = domains.iter().map(|d| d.id.to_string()).collect();
        ids.sort();
        ids.dedup();
        json!({"enum": ids})
    };
    json!([{"mxl_domain_id": domain, "mxl_flow_id": {}}])
}

fn peer_fields(value: &mut Value, role: Role, peer: Option<Uuid>) {
    match role {
        Role::Sender => value["receiver_id"] = uuid_json(peer),
        Role::Receiver => {
            value["sender_id"] = uuid_json(peer);
            value["transport_file"] = json!({"data": Value::Null, "type": Value::Null});
        }
    }
}

pub(crate) fn param_json(param: Param) -> Value {
    match param {
        Param::Null => Value::Null,
        Param::Auto => json!("auto"),
        Param::Id(id) => json!(id.to_string()),
    }
}

pub(crate) fn uuid_json(id: Option<Uuid>) -> Value {
    id.map(|id| json!(id.to_string())).unwrap_or(Value::Null)
}

fn rational(rate: Rational) -> Value {
    json!({"numerator": rate.numerator, "denominator": rate.denominator})
}

fn tags_json(base: &Tags, extra: Option<&Tags>) -> Value {
    let mut object = Map::new();
    for (key, values) in base {
        object.insert(key.clone(), json!(values));
    }
    if let Some(extra) = extra {
        for (key, values) in extra {
            let entry = object.entry(key.clone()).or_insert_with(|| json!([]));
            if let Some(list) = entry.as_array_mut() {
                for value in values {
                    if !list.iter().any(|v| v == value) {
                        list.push(json!(value));
                    }
                }
            }
        }
    }
    Value::Object(object)
}
