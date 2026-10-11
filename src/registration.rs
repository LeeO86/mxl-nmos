//! IS-04 registration client: registration in order, heartbeats, re-registration
//! after a 404, backoff, failover between registries, the Query API readiness
//! check, and deregistration.
//!
//! The node is a Node, not a registry. A configured registry comes first;
//! registries found by DNS-SD (feature `dns-sd`) follow in priority order.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_LENGTH};
use serde_json::json;
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

use crate::clock::TaiTime;
use crate::model::Doc;
use crate::node::{lock, Node};
use crate::resources::ResourceType;
use crate::settings::normalize_registry;

const MAX_BACKOFF: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(1);

/// Shared registration state.
#[derive(Debug, Default)]
pub(crate) struct RegistrationState {
    /// Registry base URLs: the configured one, then discovered ones by priority.
    pub candidates: Vec<String>,
    pub index: usize,
    /// Registry discovered over DNS-SD: (priority, base URL).
    pub discovered: Vec<(u32, String)>,
    /// What the current registry holds: resource → version.
    pub registered: HashMap<(ResourceType, Uuid), TaiTime>,
    /// The Node resource is registered with the current registry.
    pub node_registered: bool,
}

#[derive(Debug)]
enum RegistryError {
    /// 404: the registry does not know the node.
    NotFound,
    Other(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("HTTP 404"),
            Self::Other(text) => f.write_str(text),
        }
    }
}

fn client(node: &Node) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(node.settings().registry_timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub(crate) async fn run(node: Node) {
    let client = client(&node);
    #[cfg(feature = "dns-sd")]
    let mut discovery = crate::dns_sd::Discovery::start(&node);
    let heartbeat_every = node.settings().heartbeat.max(Duration::from_millis(100));
    let mut next_heartbeat = Instant::now();
    let mut retry_at = Instant::now();
    let mut backoff = Duration::from_secs(1);
    let mut heartbeat_ok = false;
    let mut query_ok = false;

    while !node.is_shutdown() {
        #[cfg(feature = "dns-sd")]
        if let Some(discovery) = discovery.as_mut() {
            let found = discovery.poll();
            let peer_to_peer = found.is_empty() && node.settings().registry.is_none();
            lock(&node.inner.registration).discovered = found;
            discovery.advertise(&node, peer_to_peer);
        }
        refresh_candidates(&node);
        let base = current(&node);
        let now = Instant::now();
        match base {
            None => {
                heartbeat_ok = false;
            }
            Some(base) if !lock(&node.inner.registration).node_registered => {
                if now >= retry_at {
                    match register_all(&node, &client, &base).await {
                        Ok(()) => {
                            info!("NMOS node {} registered with {base}", node.node_id());
                            backoff = Duration::from_secs(1);
                            next_heartbeat = Instant::now();
                            query_ok = false;
                        }
                        Err(err) => {
                            warn!("NMOS registration with {base} failed: {err}");
                            heartbeat_ok = false;
                            retry_at = after_failure(&node, &mut backoff);
                        }
                    }
                }
            }
            Some(base) => {
                if let Err(err) = sync_changes(&node, &client, &base).await {
                    warn!("NMOS registry update at {base} failed: {err}");
                    reset(&node);
                    heartbeat_ok = false;
                    retry_at = Instant::now();
                } else if now >= next_heartbeat {
                    match heartbeat(&client, &base, node.node_id()).await {
                        Ok(()) => {
                            heartbeat_ok = true;
                            backoff = Duration::from_secs(1);
                            next_heartbeat += heartbeat_every;
                            if next_heartbeat <= Instant::now() {
                                next_heartbeat = Instant::now() + heartbeat_every;
                            }
                            if !query_ok {
                                query_ok = query_lists_node(&node, &client).await;
                            }
                        }
                        Err(RegistryError::NotFound) => {
                            warn!("NMOS registry {base} does not know node {}; registering again", node.node_id());
                            reset(&node);
                            heartbeat_ok = false;
                            query_ok = false;
                            retry_at = Instant::now();
                        }
                        Err(err) => {
                            warn!("NMOS heartbeat to {base} failed: {err}");
                            heartbeat_ok = false;
                            // A heartbeat to the next registry tells whether it
                            // already holds the node (IS-04 failover).
                            next_heartbeat = after_failure(&node, &mut backoff);
                        }
                    }
                }
            }
        }
        let registered = lock(&node.inner.registration).node_registered
            && heartbeat_ok
            && (node.settings().query.is_none() || query_ok);
        node.inner.registered.store(registered, Ordering::SeqCst);

        let node_registered = lock(&node.inner.registration).node_registered;
        let mut wake = Instant::now() + TICK;
        if node_registered && next_heartbeat < wake {
            wake = next_heartbeat;
        }
        if !node_registered && retry_at < wake {
            wake = retry_at;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(wake) => {}
            _ = node.inner.changed.notified() => {}
        }
    }
    #[cfg(feature = "dns-sd")]
    if let Some(discovery) = discovery {
        discovery.shutdown();
    }
}

/// Move to the next registry. Returns when to try again: at once when there is
/// another registry to try, after the backoff when all were tried.
fn after_failure(node: &Node, backoff: &mut Duration) -> Instant {
    let wrapped = {
        let mut state = lock(&node.inner.registration);
        let count = state.candidates.len();
        if count > 1 {
            state.index = (state.index + 1) % count;
            info!("NMOS registration failing over to {}", state.candidates[state.index]);
        }
        count <= 1 || state.index == 0
    };
    if wrapped {
        let at = Instant::now() + *backoff;
        *backoff = (*backoff * 2).min(MAX_BACKOFF);
        at
    } else {
        Instant::now()
    }
}

fn reset(node: &Node) {
    let mut state = lock(&node.inner.registration);
    state.node_registered = false;
    state.registered.clear();
}

fn current(node: &Node) -> Option<String> {
    let state = lock(&node.inner.registration);
    state.candidates.get(state.index).cloned()
}

fn refresh_candidates(node: &Node) {
    let mut state = lock(&node.inner.registration);
    let mut next = Vec::new();
    if let Some(configured) = node
        .settings()
        .registry
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        next.push(normalize_registry(configured));
    }
    let mut discovered = state.discovered.clone();
    discovered.sort_by_key(|(pri, _)| *pri);
    for (_, base) in discovered {
        if !next.contains(&base) {
            next.push(base);
        }
    }
    if state.candidates != next {
        let current = state.candidates.get(state.index).cloned();
        state.index = current
            .and_then(|base| next.iter().position(|b| *b == base))
            .unwrap_or(0);
        state.candidates = next;
    }
}

async fn register_all(node: &Node, client: &reqwest::Client, base: &str) -> Result<(), RegistryError> {
    reset(node);
    let docs = node.docs();
    let Some(node_doc) = docs.iter().find(|d| d.typ == ResourceType::Node) else {
        return Err(RegistryError::Other("no node resource".to_string()));
    };
    if post(client, base, node_doc).await? == 200 {
        // The registry still holds an earlier registration of this node: remove it
        // and register afresh (IS-04 "Node encounters HTTP 200 on first
        // registration").
        info!("NMOS registry {base} already held node {}; re-registering", node_doc.id);
        delete(client, base, ResourceType::Node, node_doc.id).await?;
        post(client, base, node_doc).await?;
    }
    {
        let mut state = lock(&node.inner.registration);
        state.registered.insert((node_doc.typ, node_doc.id), node_doc.version);
        state.node_registered = true;
    }
    sync_changes(node, client, base).await
}

/// POST every document whose version the registry does not have yet, in
/// registration order, and DELETE what is gone, in reverse order.
async fn sync_changes(node: &Node, client: &reqwest::Client, base: &str) -> Result<(), RegistryError> {
    let docs = node.docs();
    let registered = lock(&node.inner.registration).registered.clone();
    for doc in &docs {
        if registered.get(&(doc.typ, doc.id)) == Some(&doc.version) {
            continue;
        }
        post(client, base, doc).await?;
        lock(&node.inner.registration)
            .registered
            .insert((doc.typ, doc.id), doc.version);
    }
    let mut stale: Vec<(ResourceType, Uuid)> = registered
        .keys()
        .filter(|key| !docs.iter().any(|d| (d.typ, d.id) == **key))
        .copied()
        .collect();
    stale.sort_by_key(|(typ, id)| (std::cmp::Reverse(typ.rank()), *id));
    for (typ, id) in stale {
        delete(client, base, typ, id).await?;
        lock(&node.inner.registration).registered.remove(&(typ, id));
    }
    Ok(())
}

/// DELETE every registered resource in reverse registration order, the Node last.
pub(crate) async fn deregister(node: &Node) {
    let (base, mut keys) = {
        let state = lock(&node.inner.registration);
        (
            state.candidates.get(state.index).cloned(),
            state.registered.keys().copied().collect::<Vec<_>>(),
        )
    };
    let Some(base) = base else {
        return;
    };
    if keys.is_empty() {
        return;
    }
    keys.sort_by_key(|(typ, id)| (std::cmp::Reverse(typ.rank()), *id));
    let client = client(node);
    for (typ, id) in keys {
        match delete(&client, &base, typ, id).await {
            Ok(()) => {}
            Err(err) => warn!("NMOS delete {} {id} failed: {err}", typ.name()),
        }
    }
    reset(node);
    info!("NMOS node {} deregistered from {base}", node.node_id());
}

async fn post(client: &reqwest::Client, base: &str, doc: &Doc) -> Result<u16, RegistryError> {
    let url = format!("{base}/x-nmos/registration/v1.3/resource");
    let response = client
        .post(&url)
        .header(ACCEPT, "application/json")
        .json(&json!({"type": doc.typ.name(), "data": doc.json()}))
        .send()
        .await
        .map_err(|err| RegistryError::Other(err.to_string()))?;
    let status = response.status();
    if status.is_success() {
        Ok(status.as_u16())
    } else {
        let body = response.text().await.unwrap_or_default();
        Err(RegistryError::Other(format!(
            "POST {} {}: HTTP {status} {body}",
            doc.typ.name(),
            doc.id
        )))
    }
}

async fn delete(client: &reqwest::Client, base: &str, typ: ResourceType, id: Uuid) -> Result<(), RegistryError> {
    let url = format!("{base}/x-nmos/registration/v1.3/resource/{}s/{id}", typ.name());
    let response = client
        .delete(&url)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| RegistryError::Other(err.to_string()))?;
    let status = response.status();
    if status.is_success() || status.as_u16() == 404 {
        Ok(())
    } else {
        Err(RegistryError::Other(format!("DELETE {} {id}: HTTP {status}", typ.name())))
    }
}

async fn heartbeat(client: &reqwest::Client, base: &str, node_id: Uuid) -> Result<(), RegistryError> {
    let url = format!("{base}/x-nmos/registration/v1.3/health/nodes/{node_id}");
    let response = client
        .post(&url)
        .header(ACCEPT, "application/json")
        .header(CONTENT_LENGTH, "0")
        .body(Vec::<u8>::new())
        .send()
        .await
        .map_err(|err| RegistryError::Other(err.to_string()))?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        404 => Err(RegistryError::NotFound),
        code => Err(RegistryError::Other(format!("HTTP {code}"))),
    }
}

/// The Query API lists the node.
async fn query_lists_node(node: &Node, client: &reqwest::Client) -> bool {
    let Some(query) = node.settings().query.as_deref() else {
        return true;
    };
    let url = format!(
        "{}/x-nmos/query/v1.3/nodes/{}",
        query.trim_end_matches('/'),
        node.node_id()
    );
    match client.get(&url).header(ACCEPT, "application/json").send().await {
        Ok(response) if response.status().is_success() => true,
        Ok(response) => {
            warn!("NMOS Query API {query} does not list the node yet: HTTP {}", response.status());
            false
        }
        Err(err) => {
            warn!("NMOS Query API {query} is unreachable: {err}");
            false
        }
    }
}
