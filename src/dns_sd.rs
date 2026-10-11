//! mDNS/DNS-SD: registry browsing and node advertisement (feature `dns-sd`).
//!
//! Pure Rust (`mdns-sd`); no Avahi or D-Bus. Only active when
//! `Settings::dns_sd` is true.

use std::collections::HashMap;

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use tracing::{info, warn};

use crate::node::Node;
use crate::settings::normalize_registry;

/// IS-04 v1.3 registration service, and its pre-v1.3 name.
const REGISTRATION_SERVICES: [&str; 2] = ["_nmos-register._tcp.local.", "_nmos-registration._tcp.local."];
const NODE_SERVICE: &str = "_nmos-node._tcp.local.";

pub(crate) struct Discovery {
    daemon: ServiceDaemon,
    browsers: Vec<Receiver<ServiceEvent>>,
    found: HashMap<String, (u32, String)>,
    advertised: Option<String>,
    protocol: &'static str,
}

impl Discovery {
    pub(crate) fn start(node: &Node) -> Option<Self> {
        if !node.settings().dns_sd {
            info!("NMOS DNS-SD browsing and node advertisement are off");
            return None;
        }
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => daemon,
            Err(err) => {
                warn!("NMOS mDNS is unavailable: {err}");
                return None;
            }
        };
        let browsers = REGISTRATION_SERVICES
            .iter()
            .filter_map(|service| match daemon.browse(service) {
                Ok(receiver) => Some(receiver),
                Err(err) => {
                    warn!("NMOS registry browse for {service} failed: {err}");
                    None
                }
            })
            .collect();
        Some(Self {
            daemon,
            browsers,
            found: HashMap::new(),
            advertised: None,
            protocol: if node.settings().https { "https" } else { "http" },
        })
    }

    /// Registries found so far: (priority, base URL).
    pub(crate) fn poll(&mut self) -> Vec<(u32, String)> {
        for browser in &self.browsers {
            while let Ok(event) = browser.try_recv() {
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let txt = |key: &str| service.get_property_val_str(key).map(str::to_string);
                        let version_ok = txt("api_ver")
                            .is_some_and(|v| v.split(',').any(|v| v.trim() == "v1.3"));
                        let proto = txt("api_proto").unwrap_or_else(|| "http".to_string());
                        let auth_ok = txt("api_auth").is_none_or(|a| a == "false");
                        let Some(ip) = service.get_addresses_v4().into_iter().next() else {
                            continue;
                        };
                        if !version_ok || proto != self.protocol || !auth_ok {
                            self.found.remove(&service.fullname);
                            continue;
                        }
                        let pri = txt("pri").and_then(|p| p.parse().ok()).unwrap_or(100);
                        let base = normalize_registry(&format!("{proto}://{ip}:{}", service.port));
                        if self.found.get(&service.fullname).map(|f| &f.1) != Some(&base) {
                            info!("Discovered NMOS registry {base} (pri {pri})");
                        }
                        self.found.insert(service.fullname.clone(), (pri, base));
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        self.found.remove(&fullname);
                    }
                    _ => {}
                }
            }
        }
        self.found.values().cloned().collect()
    }

    /// Advertise `_nmos-node._tcp` while `on`: only in peer-to-peer mode, when no
    /// registry is known (IS-04 v1.3).
    pub(crate) fn advertise(&mut self, node: &Node, on: bool) {
        match (on, self.advertised.is_some()) {
            (true, false) => {
                let short_id = node.node_id().simple().to_string();
                let prefix = &node.settings().service_name_prefix;
                let name = format!("{prefix}-{short_id}");
                let hostname = format!("{prefix}-{short_id}.local.");
                let properties = [
                    ("api_ver", "v1.3"),
                    ("api_proto", self.protocol),
                    ("api_auth", "false"),
                ];
                match ServiceInfo::new(
                    NODE_SERVICE,
                    &name,
                    &hostname,
                    node.host().to_string(),
                    node.settings().port,
                    &properties[..],
                ) {
                    Ok(info) => {
                        let fullname = info.get_fullname().to_string();
                        match self.daemon.register(info) {
                            Ok(()) => self.advertised = Some(fullname),
                            Err(err) => warn!("NMOS node advertisement failed: {err}"),
                        }
                    }
                    Err(err) => warn!("NMOS node advertisement failed: {err}"),
                }
            }
            (false, true) => {
                if let Some(fullname) = self.advertised.take() {
                    let _ = self.daemon.unregister(&fullname);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn shutdown(mut self) {
        if let Some(fullname) = self.advertised.take() {
            let _ = self.daemon.unregister(&fullname);
        }
        let _ = self.daemon.shutdown();
    }
}
