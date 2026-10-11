//! Registration against a mock IS-04 registry.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use mxl_nmos::clock::SystemClock;
use mxl_nmos::{ResourceType, Settings};
use std::sync::Arc;

fn rank(typ: &str) -> u8 {
    match typ {
        "node" => 0,
        "device" => 1,
        "source" => 2,
        "flow" => 3,
        _ => 4,
    }
}

fn registering(registry: &MockRegistry, query: bool) -> Settings {
    let mut s = settings();
    s.registration = true;
    s.registry = Some(registry.base());
    s.query = query.then(|| registry.base());
    s.heartbeat = Duration::from_millis(300);
    s.registry_timeout = Duration::from_millis(500);
    s
}

fn start(settings: Settings) -> Harness {
    let h = harness_with(settings, Arc::new(SystemClock));
    h.node.start_registration();
    h
}

#[tokio::test]
async fn registers_in_order_and_heartbeats() {
    let registry = MockRegistry::start().await;
    let h = start(registering(&registry, false));
    wait_for("registration", 5, || h.node.is_registered()).await;
    let types = registry.posted_types();
    assert_eq!(types.len(), 8, "{types:?}");
    assert_eq!(types[0], "node");
    assert!(
        types.windows(2).all(|w| rank(&w[0]) <= rank(&w[1])),
        "{types:?}"
    );

    let posts: Vec<Recorded> = registry
        .requests()
        .into_iter()
        .filter(|r| r.resource_type.is_some())
        .collect();
    for post in &posts {
        assert_eq!(post.headers["accept"], "application/json");
        assert!(post.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json"));
    }
    wait_for("three heartbeats", 5, || {
        registry
            .requests()
            .iter()
            .filter(|r| r.path.contains("/health/nodes/"))
            .count()
            >= 3
    })
    .await;
    let heartbeat = registry
        .requests()
        .into_iter()
        .find(|r| r.path.contains("/health/nodes/"))
        .unwrap();
    assert!(heartbeat.path.ends_with(&h.node.node_id().to_string()));
    assert!(heartbeat.body.is_empty());
    assert_eq!(heartbeat.headers["content-length"], "0");
    assert!(heartbeat.headers.get("content-type").is_none());
    h.node.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn re_registers_everything_after_a_404_heartbeat() {
    let registry = MockRegistry::start().await;
    let h = start(registering(&registry, false));
    wait_for("registration", 5, || h.node.is_registered()).await;
    registry.clear();
    registry.heartbeat_status.store(404, Ordering::SeqCst);
    wait_for("not registered", 5, || !h.node.is_registered()).await;
    registry.heartbeat_status.store(200, Ordering::SeqCst);
    wait_for("registered again", 5, || h.node.is_registered()).await;
    let types = registry.posted_types();
    assert!(types.len() >= 8, "{types:?}");
    assert_eq!(types[0], "node");
    assert!(
        types[..8].windows(2).all(|w| rank(&w[0]) <= rank(&w[1])),
        "{types:?}"
    );
    h.node.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn a_200_on_first_registration_replaces_the_stale_node() {
    let registry = MockRegistry::start().await;
    registry.node_post_status.store(200, Ordering::SeqCst);
    let h = start(registering(&registry, false));
    wait_for("registration", 5, || h.node.is_registered()).await;
    let requests = registry.requests();
    let node_path = format!(
        "/x-nmos/registration/v1.3/resource/nodes/{}",
        h.node.node_id()
    );
    let order: Vec<String> = requests
        .iter()
        .take(3)
        .map(|r| {
            format!(
                "{} {}",
                r.method,
                r.resource_type.clone().unwrap_or_else(|| r.path.clone())
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "POST node".to_string(),
            format!("DELETE {node_path}"),
            "POST node".to_string()
        ]
    );
    assert_eq!(
        registry.deleted_paths(),
        vec![node_path],
        "only the node is deleted"
    );
    h.node.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn backs_off_while_the_registry_fails() {
    let registry = MockRegistry::start().await;
    registry.post_status.store(500, Ordering::SeqCst);
    let h = start(registering(&registry, false));
    wait_for("three registration attempts", 10, || {
        registry
            .posted_types()
            .iter()
            .filter(|t| *t == "node")
            .count()
            >= 3
    })
    .await;
    assert!(!h.node.is_registered());
    let times: Vec<Instant> = registry
        .requests()
        .into_iter()
        .filter(|r| r.resource_type.as_deref() == Some("node"))
        .map(|r| r.at)
        .collect();
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] >= Duration::from_millis(900), "{gaps:?}");
    assert!(
        gaps[1] >= Duration::from_millis(1900),
        "the backoff doubles: {gaps:?}"
    );
    h.node.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn readiness_needs_the_query_api_listing() {
    let registry = MockRegistry::start().await;
    let mut settings = registering(&registry, false);
    settings.query = Some("http://127.0.0.1:9".into());
    let h = start(settings);
    wait_for("heartbeats", 5, || {
        registry
            .requests()
            .iter()
            .filter(|r| r.path.contains("/health/nodes/"))
            .count()
            >= 2
    })
    .await;
    assert!(
        !h.node.is_registered(),
        "the Query API does not list the node"
    );
    h.node.shutdown(Duration::from_secs(1)).await;

    let registry = MockRegistry::start().await;
    let h = start(registering(&registry, true));
    wait_for("ready via the Query API", 5, || h.node.is_registered()).await;
    assert!(registry
        .requests()
        .iter()
        .any(|r| r.path.starts_with("/x-nmos/query/v1.3/nodes/")));
    h.node.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn runtime_changes_reach_the_registry_and_shutdown_deletes_in_reverse_order() {
    let registry = MockRegistry::start().await;
    let h = start(registering(&registry, false));
    wait_for("registration", 5, || h.node.is_registered()).await;
    registry.clear();
    h.node.erase(ResourceType::Receiver, "audio-in").unwrap();
    let receiver_path = format!(
        "/x-nmos/registration/v1.3/resource/receivers/{}",
        h.receiver_id()
    );
    wait_for("receiver deleted", 5, || {
        registry.deleted_paths().contains(&receiver_path)
    })
    .await;
    let mut sender = mxl_nmos::Sender::mxl("video-out", "video-out", "dev");
    sender.label = "Renamed".into();
    h.node.insert_sender(sender).unwrap();
    wait_for("sender updated", 5, || {
        registry.posted_types().iter().any(|t| t == "sender")
    })
    .await;

    registry.clear();
    let started = Instant::now();
    h.node.shutdown(Duration::from_secs(2)).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!h.node.is_registered());
    let deleted = registry.deleted_paths();
    let kinds: Vec<&str> = deleted
        .iter()
        .map(|p| p.split('/').nth(5).unwrap())
        .collect();
    assert_eq!(kinds.last(), Some(&"nodes"), "{kinds:?}");
    let ranks: Vec<u8> = kinds
        .iter()
        .map(|k| rank(k.trim_end_matches('s')))
        .collect();
    assert!(ranks.windows(2).all(|w| w[0] >= w[1]), "{kinds:?}");
    assert_eq!(kinds.len(), 7, "{kinds:?}");
    assert!(registry.nodes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nothing_is_registered_without_a_registry() {
    let h = harness();
    h.node.start_registration();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!h.node.is_registered());
    assert!(!h.node.registration_required());
    h.node.shutdown(Duration::from_millis(100)).await;
}
