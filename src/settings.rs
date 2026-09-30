//! Runtime settings for the Strom NMOS node.

use std::path::PathBuf;

use strom_types::mxl::DEFAULT_MXL_DOMAIN;
use uuid::Uuid;

/// How this process presents itself as an AMWA IS-04 Node.
#[derive(Debug, Clone)]
pub struct NmosSettings {
    /// Advertise the node on mDNS and register with a registry.
    /// The IS-04 and IS-05 HTTP APIs are served either way.
    pub enabled: bool,
    /// Stable node id. When unset, one is loaded from `id_path` or generated.
    pub node_id: Option<Uuid>,
    /// File that persists the generated node id across restarts.
    pub id_path: Option<PathBuf>,
    pub label: String,
    /// Port the Node API and Connection API are served on.
    pub port: u16,
    /// Host controllers use to reach this node. Empty picks a non-loopback IPv4.
    pub host: Option<String>,
    /// Use `https` in the IS-04 `api.endpoints` advertisement.
    pub https: bool,
    /// Base URL of an IS-04 registry, for example `http://192.0.2.10:3210`.
    /// Tried before registries found by mDNS.
    pub registry: Option<String>,
    /// Filesystem paths of MXL domains. Each directory may contain `domain_def.json`.
    pub domain_paths: Vec<PathBuf>,
    /// Seconds between registration heartbeats.
    pub heartbeat_secs: u64,
}

impl Default for NmosSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            node_id: None,
            id_path: None,
            label: "Strom".to_string(),
            port: strom_types::DEFAULT_PORT,
            host: None,
            https: false,
            registry: None,
            domain_paths: vec![PathBuf::from(DEFAULT_MXL_DOMAIN)],
            heartbeat_secs: 5,
        }
    }
}

impl NmosSettings {
    /// Node API is still mounted, but nothing is announced or registered.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            domain_paths: Vec::new(),
            ..Self::default()
        }
    }
}
