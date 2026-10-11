//! Runtime settings for the Strom NMOS node.

use std::collections::HashMap;
use std::path::PathBuf;

use strom_types::mxl::DEFAULT_MXL_DOMAIN;
use uuid::Uuid;

/// UUIDv5 namespace for NMOS ids derived from `NMOS_SEED`.
/// This is `UUIDv5(NAMESPACE_OID, "leeo86.strom.nmos.v1")`.
pub const NMOS_NAMESPACE: Uuid = Uuid::from_u128(0x8c76aff5_a8f9_53ce_97d0_766f1074b699);

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
    /// When set, every NMOS id is UUIDv5 of this seed.
    pub seed: Option<String>,
    /// Tag name to string values, copied onto the node and each device.
    pub tags: HashMap<String, Vec<String>>,
    /// Browse for registries and advertise this node on mDNS.
    /// Default is off. A configured registry URL is used directly.
    pub dns_sd: bool,
    /// Parent directory whose children are MXL domains, mirrors included.
    pub scan_path: PathBuf,
    /// This function's own output domain directory.
    pub output_domain_dir: Option<PathBuf>,
    /// This function's own output domain id. Seed-derived when unset.
    pub output_domain_id: Option<Uuid>,
    /// Domain `history_duration` in nanoseconds, written only when `options.json` is created.
    pub history_duration_ns: u64,
    /// Remove `output_domain_dir` on SIGTERM.
    pub cleanup_on_exit: bool,
    /// IS-04 Query API base, used to confirm registration.
    pub query_base: Option<String>,
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
            seed: None,
            tags: HashMap::new(),
            dns_sd: false,
            scan_path: PathBuf::from("/Volumes/mxl"),
            output_domain_dir: None,
            output_domain_id: None,
            history_duration_ns: 200_000_000,
            cleanup_on_exit: false,
            query_base: None,
        }
    }
}

/// Node id for `seed`. The same seed always yields the same id.
pub fn node_id_from_seed(seed: &str) -> Uuid {
    Uuid::new_v5(&NMOS_NAMESPACE, seed.as_bytes())
}

/// Default output-domain id for `seed`.
pub fn output_domain_id_from_seed(seed: &str) -> Uuid {
    Uuid::new_v5(
        &NMOS_NAMESPACE,
        format!("{seed}/mxl-output-domain").as_bytes(),
    )
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
