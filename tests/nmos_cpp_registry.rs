//! Integration with a real nmos-cpp registry: register, heartbeat, re-register
//! after a registry restart, readiness through its Query API, deregister on
//! shutdown.
//!
//! Runs only when `MXL_NMOS_IT_REGISTRY` is set (`<ip>:<registration port>`; the
//! Query API is on the next port) and `MXL_NMOS_IT_CONTAINER` names the registry's
//! docker container. `MXL_NMOS_IT_HOST` is the address announced (default: the
//! registry's). See `ci/registry-it.sh`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use mxl_nmos::clock::SystemClock;

struct Env {
    registry: String,
    query: String,
    container: String,
    host: String,
}

fn env() -> Option<Env> {
    let registry = std::env::var("MXL_NMOS_IT_REGISTRY").ok()?;
    let (ip, port) = registry.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some(Env {
        registry: format!("http://{ip}:{port}"),
        query: format!("http://{ip}:{}", port + 1),
        container: std::env::var("MXL_NMOS_IT_CONTAINER").ok()?,
        host: std::env::var("MXL_NMOS_IT_HOST").unwrap_or_else(|_| ip.to_string()),
    })
}

async fn query(env: &Env, path: &str) -> reqwest::StatusCode {
    reqwest::get(format!("{}/x-nmos/query/v1.3/{path}", env.query))
        .await
        .map(|r| r.status())
        .unwrap_or(reqwest::StatusCode::SERVICE_UNAVAILABLE)
}

async fn wait_listed(env: &Env, path: &str, secs: u64) {
    for _ in 0..secs * 10 {
        if query(env, path).await == reqwest::StatusCode::OK {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path} is not listed by the Query API");
}

#[tokio::test(flavor = "multi_thread")]
async fn against_the_nmos_cpp_registry() {
    let Some(env) = env() else {
        eprintln!("MXL_NMOS_IT_REGISTRY / MXL_NMOS_IT_CONTAINER not set; skipped");
        return;
    };
    let mut s = settings();
    s.seed = Some("mxl-nmos-registry-it".into());
    s.host = Some(env.host.parse().unwrap());
    s.port = 3399;
    s.registration = true;
    s.registry = Some(env.registry.clone());
    s.query = Some(env.query.clone());
    let h = harness_with(s, Arc::new(SystemClock));
    h.node.start_registration();
    let node = h.node.node_id();

    wait_for("ready (registered and listed by the Query API)", 15, || {
        h.node.is_registered()
    })
    .await;
    for path in [
        format!("nodes/{node}"),
        format!("senders/{}", h.sender_id()),
        format!("receivers/{}", h.receiver_id()),
    ] {
        wait_listed(&env, &path, 5).await;
    }

    // The registry expires a node 12 s after its last heartbeat.
    tokio::time::sleep(Duration::from_secs(15)).await;
    assert_eq!(
        query(&env, &format!("nodes/{node}")).await,
        200,
        "heartbeats keep the node"
    );
    assert!(h.node.is_registered());

    let restart = std::process::Command::new("docker")
        .args(["restart", "-t", "1", &env.container])
        .status()
        .expect("docker restart");
    assert!(restart.success());
    wait_for("readiness lost after the registry restart", 20, || {
        !h.node.is_registered()
    })
    .await;
    wait_for("ready again", 30, || h.node.is_registered()).await;
    wait_listed(&env, &format!("senders/{}", h.sender_id()), 5).await;

    h.node.shutdown(Duration::from_secs(5)).await;
    assert_eq!(query(&env, &format!("nodes/{node}")).await, 404);
    assert_eq!(
        query(&env, &format!("senders/{}", h.sender_id())).await,
        404
    );
    assert_eq!(
        query(&env, &format!("receivers/{}", h.receiver_id())).await,
        404
    );
}
