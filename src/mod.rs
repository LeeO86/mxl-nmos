//! AMWA IS-04 / IS-05 node for MXL (BCP-007-03).
//!
//! This process is a Node. Registration and query services stay in an external
//! registry. Activation of an MXL sender or receiver restarts that Strom flow
//! so `mxlsink` / `mxlsrc` pick up the domain path and flow id.

mod domain;
mod http;
mod node;
mod register;
mod settings;

pub use node::{EndpointKind, MxlApply, NmosNode};
pub use settings::NmosSettings;

use std::sync::Arc;

/// Build a node whose snapshot and activation callbacks use `app`.
pub fn node_for_app(settings: NmosSettings, app: crate::state::AppState) -> NmosNode {
    let app_snapshot = app.clone();
    let snapshot: node::SnapshotFn = Arc::new(move || {
        let app_snapshot = app_snapshot.clone();
        Box::pin(async move { app_snapshot.get_flows().await })
    });
    let app_apply = app;
    let apply: node::ApplyFn = Arc::new(move |command| {
        let app_apply = app_apply.clone();
        Box::pin(async move { app_apply.apply_nmos_mxl(command).await })
    });
    NmosNode::new(settings, snapshot, apply)
}

use std::time::Duration;

use axum::Router;

/// IS-04 and IS-05 routes. Already bound to `node`; nest this at `/x-nmos`.
pub fn router(node: NmosNode) -> Router {
    http::router(node)
}

/// Announce the node and register it when `settings.enabled` is set.
pub fn start(node: NmosNode) {
    if !node.settings().enabled {
        tracing::info!("NMOS discovery and registration are disabled");
        return;
    }
    if node.mark_started() {
        return;
    }
    tracing::info!(
        "NMOS node {} registering MXL senders and receivers",
        node.node_id()
    );
    tokio::spawn(register::run(node));
}

/// Best-effort DELETE of this node from the registry during process shutdown.
pub async fn shutdown(node: &NmosNode) {
    node.request_shutdown();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    register::unregister(node, &client).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nmos::domain::scan_domains;
    use crate::nmos::node::{ApplyFn, SnapshotFn};
    use crate::nmos::register::normalize_registry;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures::future::BoxFuture;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use strom_types::block::{BlockInstance, Position};
    use strom_types::element::PropertyValue;
    use strom_types::mxl::{MXL_AUDIO_INPUT_ID, MXL_VIDEO_OUTPUT_ID};
    use strom_types::Flow;
    use tower::ServiceExt;
    use uuid::Uuid;

    fn node_with(flows: Vec<Flow>, domains: Vec<PathBuf>) -> (NmosNode, Arc<Mutex<Vec<MxlApply>>>) {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let applied_cb = applied.clone();
        let flows = Arc::new(Mutex::new(flows));
        let flows_for_apply = flows.clone();
        let snapshot: SnapshotFn = Arc::new(move || {
            let flows = flows.clone();
            let fut: BoxFuture<'static, Vec<Flow>> =
                Box::pin(async move { flows.lock().unwrap().clone() });
            fut
        });
        let apply: ApplyFn = Arc::new(move |command| {
            let applied_cb = applied_cb.clone();
            let flows_for_apply = flows_for_apply.clone();
            let fut: BoxFuture<'static, Result<(), String>> = Box::pin(async move {
                if let Some(flow) = flows_for_apply
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|flow| flow.id == command.flow_id)
                {
                    flow.running = command.master_enable;
                }
                applied_cb.lock().unwrap().push(command);
                Ok(())
            });
            fut
        });
        let mut settings = NmosSettings::disabled();
        settings.node_id = Some(Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap());
        settings.domain_paths = domains;
        settings.host = Some("192.0.2.10".to_string());
        settings.port = 8080;
        (NmosNode::new(settings, snapshot, apply), applied)
    }

    fn video_flow() -> Flow {
        let mut flow = Flow::new("PGM");
        flow.blocks.push(BlockInstance {
            id: "video-out".to_string(),
            block_definition_id: MXL_VIDEO_OUTPUT_ID.to_string(),
            name: Some("Program".to_string()),
            properties: [(
                "group_hint".to_string(),
                PropertyValue::String("Camera:Video".to_string()),
            )]
            .into_iter()
            .collect(),
            position: Position { x: 0.0, y: 0.0 },
            runtime_data: None,
            computed_external_pads: None,
        });
        flow.blocks.push(BlockInstance {
            id: "audio-in".to_string(),
            block_definition_id: MXL_AUDIO_INPUT_ID.to_string(),
            name: None,
            properties: Default::default(),
            position: Position { x: 1.0, y: 0.0 },
            runtime_data: None,
            computed_external_pads: None,
        });
        flow
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn node_api_is_mounted_at_x_nmos() {
        gstreamer::init().unwrap();
        let app = crate::create_app_with_state(crate::state::AppState::default()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/x-nmos/node/v1.3/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let listed = body_json(response).await;
        assert!(listed.as_array().unwrap().iter().any(|item| item == "self/"));
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
    }

    #[test]
    fn domain_def_json_provides_the_domain_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = "3310f209-9351-47c0-b9a2-14c59b6a4c23";
        std::fs::write(
            dir.path().join("domain_def.json"),
            format!(r#"{{"id":"{id}","label":"Red","description":"studio","tags":{{}}}}"#),
        )
        .unwrap();
        let found = scan_domains(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id.to_string(), id);
        assert_eq!(found[0].label, "Red");
    }

    #[tokio::test]
    async fn node_publishes_mxl_sender_and_receiver() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain_def.json"),
            r#"{"id":"3310f209-9351-47c0-b9a2-14c59b6a4c23","label":"Red","description":"","tags":{}}"#,
        )
        .unwrap();
        let (node, _) = node_with(vec![video_flow()], vec![dir.path().to_path_buf()]);
        let app = router(node);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/node/v1.3/senders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let senders = body_json(response).await;
        let sender = &senders.as_array().unwrap()[0];
        assert_eq!(sender["transport"], "urn:x-nmos:transport:mxl");
        assert_eq!(sender["interface_bindings"], serde_json::json!([]));
        assert!(sender["manifest_href"].is_null());
        assert_eq!(
            sender["tags"]["urn:x-nmos:tag:grouphint/v1.0"][0],
            "Camera:Video"
        );
        assert!(sender["flow_id"].is_string());
        assert_ne!(sender["flow_id"], sender["id"]);
    }

    #[tokio::test]
    async fn receiver_caps_and_transportfile_and_auto_rules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("domain_def.json"),
            r#"{"id":"3310f209-9351-47c0-b9a2-14c59b6a4c23","label":"Red","description":"","tags":{}}"#,
        )
        .unwrap();
        let (node, applied) = node_with(vec![video_flow()], vec![dir.path().to_path_buf()]);
        let app = router(node.clone());

        let receivers = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/node/v1.3/receivers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let receivers = body_json(receivers).await;
        let receiver = &receivers.as_array().unwrap()[0];
        assert_eq!(receiver["transport"], "urn:x-nmos:transport:mxl");
        assert_eq!(receiver["interface_bindings"], serde_json::json!([]));
        assert_eq!(receiver["format"], "urn:x-nmos:format:audio");
        assert!(
            !receiver["caps"]["constraint_sets"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(receiver["caps"]["media_types"][0], "audio/float32");
        let receiver_id = receiver["id"].as_str().unwrap().to_string();

        let constraints = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/connection/v1.2/single/receivers/{receiver_id}/constraints"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let constraints = body_json(constraints).await;
        let text = constraints.to_string();
        assert!(!text.contains("auto"));
        assert!(text.contains("3310f209-9351-47c0-b9a2-14c59b6a4c23"));

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/connection/v1.2/single/receivers/{receiver_id}/staged"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"transport_params":[{"mxl_flow_id":"auto"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

        let staged = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/connection/v1.2/single/receivers/{receiver_id}/staged"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"master_enable":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(staged.status(), StatusCode::OK);
        let staged = body_json(staged).await;
        assert!(staged.get("transport_file").is_some());
        assert!(applied.lock().unwrap().is_empty());

        let senders = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/node/v1.3/senders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let senders = body_json(senders).await;
        let sender_id = senders[0]["id"].as_str().unwrap().to_string();

        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/connection/v1.2/single/senders/{sender_id}/transportfile"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let activated = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/connection/v1.2/single/senders/{sender_id}/staged"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"master_enable":true,"activation":{"mode":"activate_immediate"},"transport_params":[{"mxl_domain_id":"auto","mxl_flow_id":"auto"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(activated.status(), StatusCode::OK);

        let active = router(node)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/connection/v1.2/single/senders/{sender_id}/active"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let active = body_json(active).await;
        let flow = active["transport_params"][0]["mxl_flow_id"]
            .as_str()
            .unwrap();
        let domain = active["transport_params"][0]["mxl_domain_id"]
            .as_str()
            .unwrap();
        assert_ne!(flow, "auto");
        assert_ne!(domain, "auto");
        Uuid::parse_str(flow).unwrap();
        assert_eq!(domain, "3310f209-9351-47c0-b9a2-14c59b6a4c23");
        assert!(active["master_enable"].as_bool().unwrap());

        let commands = applied.lock().unwrap().clone();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].master_enable);
        assert_eq!(commands[0].mxl_flow_id, flow);
        assert_eq!(commands[0].domain_path, dir.path().display().to_string());
        assert_eq!(commands[0].kind, EndpointKind::VideoSender);
    }
}
