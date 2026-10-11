//! Node settings and the platform environment names (G1, G3, G4, G5).

use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

/// How this process presents itself as an IS-04 Node.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Node label (`NMOS_LABEL`); also the prefix of every Device label.
    pub label: String,
    /// Node description.
    pub description: String,
    /// `NMOS_SEED`. When set, every id is derived from it (see [`crate::ids`]).
    pub seed: Option<String>,
    /// Node id when no seed is set.
    pub node_id: Option<Uuid>,
    /// File that keeps a generated node id across restarts when neither a seed nor
    /// a node id is set.
    pub id_path: Option<PathBuf>,
    /// `NMOS_TAGS`, added to the Node and every Device.
    pub tags: BTreeMap<String, Vec<String>>,
    /// `NMOS_HOST_ADDRESS`: the address announced in `href` and `api.endpoints`.
    /// Unset: the first non-loopback IPv4 address of this host.
    pub host: Option<Ipv4Addr>,
    /// `NMOS_PORT`: the port announced. The application binds it with
    /// [`crate::Node::bind`] or serves [`crate::Node::router`] on it.
    pub port: u16,
    /// Announce `https` instead of `http`.
    pub https: bool,
    /// Register with a registry at all. The Node and Connection APIs are served
    /// either way.
    pub registration: bool,
    /// Registration API base URL, for example `http://192.0.2.10:8010`
    /// (`NMOS_REGISTRY_ADDRESS`, `NMOS_REGISTRY_PORT`).
    pub registry: Option<String>,
    /// Query API base URL used to confirm the registration
    /// (`NMOS_QUERY_ADDRESS`, `NMOS_QUERY_PORT`). Unset: readiness is the
    /// registration and heartbeat result alone.
    pub query: Option<String>,
    /// `NMOS_DNS_SD`: browse for registries and advertise the node over mDNS.
    /// Needs the `dns-sd` feature; default off.
    pub dns_sd: bool,
    /// Instance name prefix of the mDNS node advertisement.
    pub service_name_prefix: String,
    /// Time between registration heartbeats.
    pub heartbeat: Duration,
    /// How long the activation callback may take before the activation fails.
    pub activation_timeout: Duration,
    /// Timeout of each request to the registry.
    pub registry_timeout: Duration,
}

impl Settings {
    /// Settings with the crate defaults: no seed, no registry, DNS-SD off,
    /// 5 s heartbeat, 2 s activation timeout.
    pub fn new(label: impl Into<String>, port: u16) -> Self {
        Self {
            label: label.into(),
            description: String::new(),
            seed: None,
            node_id: None,
            id_path: None,
            tags: BTreeMap::new(),
            host: None,
            port,
            https: false,
            registration: true,
            registry: None,
            query: None,
            dns_sd: false,
            service_name_prefix: "mxl-nmos".to_string(),
            heartbeat: Duration::from_secs(5),
            activation_timeout: Duration::from_secs(2),
            registry_timeout: Duration::from_secs(2),
        }
    }

    /// Apply the platform environment variables on top of these settings.
    ///
    /// Reads `NMOS_SEED`, `NMOS_LABEL`, `NMOS_TAGS`, `NMOS_DNS_SD`,
    /// `NMOS_REGISTRY_ADDRESS`, `NMOS_REGISTRY_PORT`, `NMOS_QUERY_ADDRESS`,
    /// `NMOS_QUERY_PORT`, `NMOS_HOST_ADDRESS` and `NMOS_PORT`. Blank values count as
    /// unset. Returns the names that were set, so the application can show where
    /// each setting came from. An invalid value is an error; the platform contract
    /// says the application then exits with code 78.
    pub fn apply_env(&mut self) -> Result<Vec<&'static str>, SettingsError> {
        self.apply_vars(|name| std::env::var(name).ok())
    }

    /// [`Settings::apply_env`] with a custom variable lookup.
    pub fn apply_vars(
        &mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<&'static str>, SettingsError> {
        let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        let mut used = Vec::new();
        if let Some(seed) = get("NMOS_SEED") {
            self.seed = Some(seed.trim().to_string());
            used.push("NMOS_SEED");
        }
        if let Some(label) = get("NMOS_LABEL") {
            self.label = label;
            used.push("NMOS_LABEL");
        }
        if let Some(raw) = get("NMOS_TAGS") {
            self.tags = parse_tags(&raw)?;
            used.push("NMOS_TAGS");
        }
        if let Some(raw) = get("NMOS_DNS_SD") {
            self.dns_sd = parse_bool("NMOS_DNS_SD", &raw)?;
            used.push("NMOS_DNS_SD");
        }
        if let Some(raw) = get("NMOS_HOST_ADDRESS") {
            self.host = Some(require_announce_ipv4("NMOS_HOST_ADDRESS", &raw)?);
            used.push("NMOS_HOST_ADDRESS");
        }
        if let Some(raw) = get("NMOS_PORT") {
            self.port = parse_port("NMOS_PORT", &raw)?;
            used.push("NMOS_PORT");
        }
        let registry_address = get("NMOS_REGISTRY_ADDRESS");
        let registry_port = get("NMOS_REGISTRY_PORT");
        let registry = match (registry_address, registry_port) {
            (Some(address), Some(port)) => {
                let address = parse_ipv4("NMOS_REGISTRY_ADDRESS", &address)?;
                let port = parse_port("NMOS_REGISTRY_PORT", &port)?;
                used.push("NMOS_REGISTRY_ADDRESS");
                used.push("NMOS_REGISTRY_PORT");
                Some((address, port))
            }
            (Some(_), None) => {
                return Err(SettingsError::new(
                    "NMOS_REGISTRY_ADDRESS is set but NMOS_REGISTRY_PORT is not",
                ))
            }
            (None, Some(_)) => {
                return Err(SettingsError::new(
                    "NMOS_REGISTRY_PORT is set but NMOS_REGISTRY_ADDRESS is not",
                ))
            }
            (None, None) => None,
        };
        if let Some((address, port)) = registry {
            self.registry = Some(format!("http://{address}:{port}"));
        }
        let query_address = match get("NMOS_QUERY_ADDRESS") {
            Some(raw) => {
                used.push("NMOS_QUERY_ADDRESS");
                Some(parse_ipv4("NMOS_QUERY_ADDRESS", &raw)?)
            }
            None => None,
        };
        let query_port = match get("NMOS_QUERY_PORT") {
            Some(raw) => {
                used.push("NMOS_QUERY_PORT");
                Some(parse_port("NMOS_QUERY_PORT", &raw)?)
            }
            None => None,
        };
        if let Some(query) = query_base_url(self.registry.as_deref(), query_address, query_port) {
            self.query = Some(query);
        }
        Ok(used)
    }
}

/// An invalid setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(String);

impl SettingsError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SettingsError {}

/// Parse `NMOS_TAGS`: a JSON object of tag name to an array of strings.
pub fn parse_tags(raw: &str) -> Result<BTreeMap<String, Vec<String>>, SettingsError> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| SettingsError::new(format!("NMOS_TAGS is not JSON: {err}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| SettingsError::new("NMOS_TAGS must be a JSON object"))?;
    let mut tags = BTreeMap::new();
    for (key, entry) in object {
        let values = entry.as_array().ok_or_else(|| {
            SettingsError::new(format!("NMOS_TAGS.{key} must be an array of strings"))
        })?;
        let mut strings = Vec::new();
        for item in values {
            let text = item.as_str().ok_or_else(|| {
                SettingsError::new(format!("NMOS_TAGS.{key} must contain only strings"))
            })?;
            strings.push(text.to_string());
        }
        tags.insert(key.clone(), strings);
    }
    Ok(tags)
}

/// Accept only an IPv4 literal other systems can route to: not a name, not
/// `0.0.0.0`, not loopback (G5).
pub fn require_announce_ipv4(name: &str, value: &str) -> Result<Ipv4Addr, SettingsError> {
    let ip = parse_ipv4(name, value)?;
    if ip.is_unspecified() || ip.is_loopback() {
        return Err(SettingsError::new(format!(
            "{name}={value} must not be 0.0.0.0 or a loopback address; use a routable IPv4 address"
        )));
    }
    Ok(ip)
}

/// The Query API base: `query_address` (default: the registry's address) and
/// `query_port` (default: the registration port + 1). `None` without a registry
/// unless both query values are given.
pub fn query_base_url(
    registry: Option<&str>,
    query_address: Option<Ipv4Addr>,
    query_port: Option<u16>,
) -> Option<String> {
    let registry = registry.and_then(split_http_host);
    let host = match (query_address, &registry) {
        (Some(address), _) => address.to_string(),
        (None, Some((host, _))) => host.clone(),
        (None, None) => return None,
    };
    let port = match (query_port, &registry) {
        (Some(port), _) => port,
        (None, Some((_, port))) => port.saturating_add(1),
        (None, None) => return None,
    };
    Some(format!("http://{host}:{port}"))
}

fn split_http_host(url: &str) -> Option<(String, u16)> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// Strip a trailing slash and a trailing `/x-nmos/registration/v1.3` from a
/// registry URL.
pub(crate) fn normalize_registry(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .trim_end_matches("/x-nmos/registration/v1.3")
        .trim_end_matches('/')
        .to_string()
}

fn parse_ipv4(name: &str, value: &str) -> Result<Ipv4Addr, SettingsError> {
    value.trim().parse().map_err(|_| {
        SettingsError::new(format!(
            "{name} must be an IPv4 address, not a name: {value}"
        ))
    })
}

fn parse_port(name: &str, value: &str) -> Result<u16, SettingsError> {
    match value.trim().parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(SettingsError::new(format!(
            "{name} is not a valid port: {value}"
        ))),
    }
}

fn parse_bool(name: &str, value: &str) -> Result<bool, SettingsError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(SettingsError::new(format!(
            "{name} must be true or false, got {value}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn apply(vars: &[(&str, &str)]) -> Result<(Settings, Vec<&'static str>), SettingsError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut settings = Settings::new("Default", 3312);
        let used = settings.apply_vars(|name| map.get(name).cloned())?;
        Ok((settings, used))
    }

    #[test]
    fn platform_names_are_applied() {
        let (s, used) = apply(&[
            ("NMOS_SEED", "sport-sa-mix1"),
            ("NMOS_LABEL", "Mixer 1"),
            ("NMOS_TAGS", r#"{"urn:x-example:production":["sport-sa"]}"#),
            ("NMOS_DNS_SD", "false"),
            ("NMOS_HOST_ADDRESS", "10.1.2.3"),
            ("NMOS_PORT", "4000"),
            ("NMOS_REGISTRY_ADDRESS", "10.0.0.5"),
            ("NMOS_REGISTRY_PORT", "8010"),
        ])
        .unwrap();
        assert_eq!(s.seed.as_deref(), Some("sport-sa-mix1"));
        assert_eq!(s.label, "Mixer 1");
        assert_eq!(s.tags["urn:x-example:production"], vec!["sport-sa"]);
        assert!(!s.dns_sd);
        assert_eq!(s.host, Some(Ipv4Addr::new(10, 1, 2, 3)));
        assert_eq!(s.port, 4000);
        assert_eq!(s.registry.as_deref(), Some("http://10.0.0.5:8010"));
        assert_eq!(s.query.as_deref(), Some("http://10.0.0.5:8011"));
        assert!(used.contains(&"NMOS_SEED") && used.contains(&"NMOS_REGISTRY_PORT"));
        assert!(!used.contains(&"NMOS_QUERY_PORT"));
    }

    #[test]
    fn defaults_without_env() {
        let (s, used) = apply(&[("NMOS_SEED", "  ")]).unwrap();
        assert!(used.is_empty());
        assert!(s.seed.is_none() && s.registry.is_none() && s.query.is_none());
        assert!(!s.dns_sd);
        assert_eq!(s.heartbeat, Duration::from_secs(5));
        assert_eq!(s.activation_timeout, Duration::from_secs(2));
    }

    #[test]
    fn query_overrides() {
        let (s, _) = apply(&[
            ("NMOS_REGISTRY_ADDRESS", "10.0.0.5"),
            ("NMOS_REGISTRY_PORT", "8010"),
            ("NMOS_QUERY_ADDRESS", "10.0.0.6"),
            ("NMOS_QUERY_PORT", "9000"),
        ])
        .unwrap();
        assert_eq!(s.query.as_deref(), Some("http://10.0.0.6:9000"));
    }

    #[test]
    fn invalid_values_are_errors() {
        for vars in [
            &[("NMOS_HOST_ADDRESS", "node.local")][..],
            &[("NMOS_HOST_ADDRESS", "0.0.0.0")],
            &[("NMOS_HOST_ADDRESS", "127.0.0.1")],
            &[("NMOS_PORT", "0")],
            &[("NMOS_PORT", "70000")],
            &[("NMOS_DNS_SD", "maybe")],
            &[("NMOS_TAGS", "[]")],
            &[("NMOS_TAGS", r#"{"a":[1]}"#)],
            &[("NMOS_REGISTRY_ADDRESS", "10.0.0.5")],
            &[("NMOS_REGISTRY_PORT", "8010")],
            &[
                ("NMOS_REGISTRY_ADDRESS", "registry"),
                ("NMOS_REGISTRY_PORT", "8010"),
            ],
            &[("NMOS_QUERY_PORT", "x")],
        ] {
            assert!(apply(vars).is_err(), "{vars:?}");
        }
    }

    #[test]
    fn registry_url_is_a_base() {
        assert_eq!(
            normalize_registry("http://192.0.2.10:3210/"),
            "http://192.0.2.10:3210"
        );
        assert_eq!(
            normalize_registry("http://192.0.2.10:3210/x-nmos/registration/v1.3"),
            "http://192.0.2.10:3210"
        );
        assert_eq!(
            query_base_url(Some("http://192.0.2.10:3210/x-nmos"), None, None).as_deref(),
            Some("http://192.0.2.10:3211")
        );
    }
}
