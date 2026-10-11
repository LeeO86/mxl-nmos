//! IS-04 registration client and DNS-SD advertisement.
//!
//! Strom is a Node, not a registry. A configured registry URL is tried first.
//! mDNS `_nmos-registration._tcp` is the fallback, and a failed heartbeat moves
//! to the next advertised registry.

use std::net::Ipv4Addr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use super::node::{NmosNode, Published};

const REGISTRATION_SERVICE: &str = "_nmos-registration._tcp.local.";
const NODE_SERVICE: &str = "_nmos-node._tcp.local.";

pub async fn run(node: NmosNode) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let discovery = Discovery::start(&node);
    let mut ticks: u64 = 0;
    let every = node.settings().heartbeat_secs.max(1);
    while !node.is_shutdown() {
        if let Some(discovery) = discovery.as_ref() {
            discovery.poll(&node);
        }
        node.sync().await;
        if ticks.is_multiple_of(every) {
            register_once(&node, &client).await;
        }
        ticks = ticks.wrapping_add(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    unregister(&node, &client).await;
    if let Some(discovery) = discovery {
        discovery.shutdown();
    }
}

pub async fn unregister(node: &NmosNode, client: &reqwest::Client) {
    let base = {
        let state = node.registry_state();
        state.candidates.get(state.index).cloned()
    };
    let Some(base) = base else {
        return;
    };
    let url = format!(
        "{}/x-nmos/registration/v1.3/resource/node/{}",
        base,
        node.node_id()
    );
    match client.delete(&url).send().await {
        Ok(response) if response.status().is_success() || response.status().as_u16() == 404 => {
            info!("Unregistered NMOS node {}", node.node_id());
        }
        Ok(response) => warn!(
            "NMOS unregister returned HTTP {} for {}",
            response.status(),
            node.node_id()
        ),
        Err(err) => warn!("NMOS unregister failed: {err}"),
    }
    node.registered().clear();
}

async fn register_once(node: &NmosNode, client: &reqwest::Client) {
    refresh_candidates(node);
    let base = {
        let state = node.registry_state();
        state.candidates.get(state.index).cloned()
    };
    let Some(base) = base else {
        node.set_registered(false);
        return;
    };
    let published = node.publish();
    let mut registered = node.registered().clone();
    let mut failed = false;
    for resource in &published {
        let key = resource.registry_key();
        if registered.get(&key) == Some(&resource.version()) {
            continue;
        }
        match post_resource(client, &base, resource).await {
            Ok(()) => {
                registered.insert(key, resource.version());
            }
            Err(err) => {
                warn!(
                    "NMOS register {} {} failed: {err}",
                    resource.typ, resource.id
                );
                failed = true;
                break;
            }
        }
    }
    if failed {
        node.set_registered(false);
        advance_registry(node);
        return;
    }
    let current: std::collections::HashSet<String> =
        published.iter().map(Published::registry_key).collect();
    let stale: Vec<String> = registered
        .keys()
        .filter(|key| !current.contains(*key))
        .cloned()
        .collect();
    for key in stale {
        let Some((typ, id)) = key.split_once(':') else {
            registered.remove(&key);
            continue;
        };
        let Ok(id) = Uuid::parse_str(id) else {
            registered.remove(&key);
            continue;
        };
        match delete_resource(client, &base, typ, id).await {
            Ok(()) => {
                registered.remove(&key);
            }
            Err(err) => warn!("NMOS delete {typ} {id} failed: {err}"),
        }
    }
    if let Err(err) = heartbeat(client, &base, node.node_id()).await {
        warn!("NMOS heartbeat failed: {err}");
        node.set_registered(false);
        advance_registry(node);
        return;
    }
    *node.registered() = registered;
    node.set_registered(true);
}

fn refresh_candidates(node: &NmosNode) {
    let mut state = node.registry_state();
    let mut next = Vec::new();
    if let Some(configured) = node
        .settings()
        .registry
        .as_ref()
        .map(|url| url.trim())
        .filter(|url| !url.is_empty())
    {
        next.push(normalize_registry(configured));
    }
    // Discovered registries are appended by poll() into the same list after the
    // configured URL. Keep any http(s) entries already recorded past index 0
    // when a configured URL occupies index 0, otherwise keep the whole list.
    let discovered: Vec<String> = state
        .candidates
        .iter()
        .filter(|url| next.first() != Some(*url))
        .cloned()
        .collect();
    next.extend(discovered);
    if state.candidates != next {
        state.candidates = next;
        state.index = 0;
    }
}

fn advance_registry(node: &NmosNode) {
    let mut state = node.registry_state();
    if state.candidates.len() > 1 {
        state.index = (state.index + 1) % state.candidates.len();
        info!(
            "NMOS registration failing over to {}",
            state.candidates[state.index]
        );
    }
}

async fn post_resource(
    client: &reqwest::Client,
    base: &str,
    resource: &Published,
) -> Result<(), String> {
    let url = format!("{base}/x-nmos/registration/v1.3/resource");
    let response = client
        .post(&url)
        .json(&json!({"type": resource.typ, "data": resource.data}))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(format!("HTTP {status} {body}"))
    }
}

async fn delete_resource(
    client: &reqwest::Client,
    base: &str,
    typ: &str,
    id: Uuid,
) -> Result<(), String> {
    let url = format!("{base}/x-nmos/registration/v1.3/resource/{typ}/{id}");
    let response = client
        .delete(&url)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status().is_success() || response.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(format!("HTTP {}", response.status()))
    }
}

async fn heartbeat(client: &reqwest::Client, base: &str, node_id: Uuid) -> Result<(), String> {
    let url = format!("{base}/x-nmos/registration/v1.3/health/nodes/{node_id}");
    let response = client
        .post(&url)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", response.status()))
    }
}

pub fn normalize_registry(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .trim_end_matches("/x-nmos/registration/v1.3")
        .trim_end_matches('/')
        .to_string()
}

struct Discovery {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
}

impl Discovery {
    fn start(node: &NmosNode) -> Option<Self> {
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
        if let Err(err) = advertise(node, &daemon) {
            warn!("NMOS node advertisement failed: {err}");
        }
        let events = match daemon.browse(REGISTRATION_SERVICE) {
            Ok(events) => events,
            Err(err) => {
                warn!("NMOS registry browse failed: {err}");
                return None;
            }
        };
        info!(
            "NMOS node {} advertising on {}:{}",
            node.node_id(),
            node.api_host(),
            node.settings().port
        );
        Some(Self { daemon, events })
    }

    fn poll(&self, node: &NmosNode) {
        while let Ok(event) = self.events.try_recv() {
            if let ServiceEvent::ServiceResolved(service) = event {
                let version = service.get_property_val_str("api_ver").unwrap_or("");
                if !version.is_empty() && !version.split(',').any(|v| v.trim() == "v1.3") {
                    continue;
                }
                let Some(ip) = service.get_addresses_v4().into_iter().next() else {
                    continue;
                };
                let proto = service.get_property_val_str("api_proto").unwrap_or("http");
                let base = format!("{proto}://{ip}:{}", service.get_port());
                remember_registry(node, base);
            }
        }
    }

    fn shutdown(self) {
        let _ = self.daemon.shutdown();
    }
}

fn remember_registry(node: &NmosNode, base: String) {
    let base = normalize_registry(&base);
    let mut state = node.registry_state();
    if !state.candidates.iter().any(|candidate| candidate == &base) {
        info!("Discovered NMOS registry {base}");
        state.candidates.push(base);
    }
}

fn advertise(node: &NmosNode, daemon: &ServiceDaemon) -> Result<(), String> {
    let host = node
        .publish()
        .into_iter()
        .find(|item| item.typ == "node")
        .and_then(|item| {
            item.data["api"]["endpoints"][0]["host"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let ip: Ipv4Addr = host.parse().unwrap_or(Ipv4Addr::LOCALHOST);
    let short_id = node.node_id().to_string().replace('-', "");
    let hostname = format!("strom-{short_id}.local.");
    let name = format!("strom-{short_id}");
    let info = mdns_sd::ServiceInfo::new(
        NODE_SERVICE,
        &name,
        &hostname,
        ip.to_string(),
        node.settings().port,
        &[
            ("api_ver", "v1.3"),
            (
                "api_proto",
                if node.settings().https {
                    "https"
                } else {
                    "http"
                },
            ),
            ("pri", "100"),
        ][..],
    )
    .map_err(|err| err.to_string())?;
    daemon.register(info).map_err(|err| err.to_string())
}
