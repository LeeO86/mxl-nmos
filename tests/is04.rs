//! IS-04 Node API: resources, ids, versions, addresses, CORS and errors.

mod common;

use axum::http::StatusCode;
use common::*;
use mxl_nmos::clock::TaiTime;
use mxl_nmos::{
    ids, Device, Error, Node, NodeImplementation, ResourceType, Role, Sender, Settings,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn ids_derive_from_the_seed_and_the_key() {
    let h = harness();
    let node_id = ids::node_id_from_seed("test-seed");
    assert_eq!(h.node.node_id(), node_id);
    assert_eq!(h.sender_id(), Uuid::new_v5(&node_id, b"sender:video-out"));
    let device = h.get("/x-nmos/node/v1.3/devices").await;
    assert_eq!(
        device[0]["id"],
        Uuid::new_v5(&node_id, b"device:dev").to_string()
    );
    let sender = h
        .get(&format!("/x-nmos/node/v1.3/senders/{}", h.sender_id()))
        .await;
    assert_eq!(
        sender["flow_id"],
        Uuid::new_v5(&node_id, b"flow:video-out").to_string()
    );
    assert_ne!(
        sender["flow_id"],
        VIDEO_FLOW.to_string(),
        "the MXL flow id is the application's"
    );
}

#[tokio::test]
async fn node_api_lists_and_resources() {
    let h = harness();
    assert_eq!(h.get("/x-nmos").await, json!(["node/", "connection/"]));
    assert_eq!(h.get("/x-nmos/").await, json!(["node/", "connection/"]));
    assert_eq!(
        h.get("/x-nmos/node/v1.3/").await,
        json!([
            "self/",
            "devices/",
            "sources/",
            "flows/",
            "senders/",
            "receivers/"
        ])
    );
    let node = h.get("/x-nmos/node/v1.3/self").await;
    assert_eq!(node["label"], "Test");
    assert_eq!(node["href"], "http://192.0.2.10:3312/");
    assert_eq!(node["api"]["endpoints"][0]["host"], "192.0.2.10");
    assert_eq!(
        node["tags"]["urn:x-example:production"],
        json!(["sport-sa"])
    );

    let device = &h.get("/x-nmos/node/v1.3/devices").await[0];
    assert_eq!(
        device["label"], "Test Camera",
        "NMOS_LABEL prefixes device labels"
    );
    assert_eq!(
        device["tags"]["urn:x-example:production"],
        json!(["sport-sa"])
    );
    assert_eq!(device["senders"], json!([h.sender_id().to_string()]));
    assert_eq!(device["receivers"], json!([h.receiver_id().to_string()]));
    assert_eq!(
        device["controls"][0]["type"],
        "urn:x-nmos:control:sr-ctrl/v1.2"
    );
    assert_eq!(
        device["controls"][0]["href"],
        "http://192.0.2.10:3312/x-nmos/connection/v1.2/"
    );

    let sender = h
        .get(&format!("/x-nmos/node/v1.3/senders/{}", h.sender_id()))
        .await;
    assert_eq!(sender["transport"], "urn:x-nmos:transport:mxl");
    assert_eq!(sender["interface_bindings"], json!([]));
    assert!(sender["manifest_href"].is_null());
    assert_eq!(
        sender["tags"]["urn:x-nmos:tag:grouphint/v1.0"],
        json!(["Camera:Video"])
    );
    assert!(sender["tags"].get("urn:x-example:production").is_none());

    let receiver = h
        .get(&format!("/x-nmos/node/v1.3/receivers/{}", h.receiver_id()))
        .await;
    assert_eq!(receiver["caps"]["media_types"], json!(["audio/float32"]));
    assert!(
        receiver["caps"]["version"].as_str().unwrap().contains(':'),
        "BCP-004-01 caps version"
    );
    assert_eq!(
        receiver["subscription"],
        json!({"sender_id": null, "active": false})
    );

    let sources = h.get("/x-nmos/node/v1.3/sources").await;
    let audio = sources
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["format"] == "urn:x-nmos:format:audio")
        .unwrap();
    assert_eq!(audio["channels"], json!([{"label": "L"}, {"label": "R"}]));

    let (status, _, body) = h
        .call(
            "GET",
            &format!("/x-nmos/node/v1.3/senders/{}", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"code": 404, "error": "Not Found", "debug": "not found"})
    );
}

#[tokio::test(start_paused = true)]
async fn versions_change_only_with_the_resource_and_strictly_increase() {
    // A paused clock: every change happens in the same nanosecond.
    let h = harness_with(settings(), TestClock::new(TaiTime::new(1_800_000_000, 0)));
    let id = h.sender_id();
    let v1 = h.version_of("senders", id).await;
    let mut sender = Sender::mxl("video-out", "video-out", "dev");
    sender.label = "Cam".into();
    mxl_nmos::add_group_hint(&mut sender.tags, "Camera", "Video");
    sender.mxl_domain_id = Some(DOMAIN);
    sender.mxl_flow_id = Some(VIDEO_FLOW);
    h.node.insert_sender(sender.clone()).unwrap();
    assert_eq!(
        h.version_of("senders", id).await,
        v1,
        "an identical insert keeps the version"
    );
    sender.label = "Cam 1".into();
    h.node.insert_sender(sender.clone()).unwrap();
    let v2 = h.version_of("senders", id).await;
    sender.label = "Cam 2".into();
    h.node.insert_sender(sender).unwrap();
    let v3 = h.version_of("senders", id).await;
    assert!(v1 < v2 && v2 < v3, "{v1} {v2} {v3}");
    assert_eq!(
        v3,
        v2.next(),
        "two changes in one nanosecond still increase"
    );
    assert_eq!(
        h.get(&format!("/x-nmos/node/v1.3/senders/{id}")).await["label"],
        "Cam 2"
    );
}

#[tokio::test]
async fn resources_are_added_and_removed_at_runtime() {
    let h = harness();
    assert_eq!(
        h.get("/x-nmos/node/v1.3/flows")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    h.node.erase(ResourceType::Source, "video-out").unwrap();
    assert_eq!(
        h.get("/x-nmos/node/v1.3/flows")
            .await
            .as_array()
            .unwrap()
            .len(),
        1,
        "its flow went too"
    );
    assert!(
        h.get("/x-nmos/node/v1.3/senders")
            .await
            .as_array()
            .unwrap()
            .is_empty(),
        "and its sender"
    );
    let device = &h.get("/x-nmos/node/v1.3/devices").await[0];
    assert_eq!(device["senders"], json!([]));

    let err = h
        .node
        .insert_sender(Sender::mxl("x", "missing-flow", "dev"))
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::UnknownResource {
                typ: ResourceType::Flow,
                ..
            }
        ),
        "{err}"
    );
    assert!(h
        .node
        .set_active(Role::Sender, "x", Default::default())
        .is_err());

    h.node.insert_device(Device::new("dev2", "Second")).unwrap();
    assert_eq!(
        h.get("/x-nmos/node/v1.3/devices")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    h.node.erase(ResourceType::Device, "dev").unwrap();
    assert!(h
        .get("/x-nmos/node/v1.3/receivers")
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert!(h
        .get("/x-nmos/node/v1.3/sources")
        .await
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_announced_address_is_a_routable_ipv4_literal() {
    let implementation = || NodeImplementation::new(|_| Box::pin(async { Ok(()) }));
    let mut settings = Settings::new("x", 3312);
    settings.registry = Some("http://192.0.2.1:8010".into());
    settings.host = Some("127.0.0.1".parse().unwrap());
    assert!(matches!(
        Node::new(settings.clone(), implementation()),
        Err(Error::InvalidAddress(_))
    ));
    settings.host = Some("0.0.0.0".parse().unwrap());
    assert!(Node::new(settings.clone(), implementation()).is_err());
    settings.host = None;
    if let Some(found) = mxl_nmos::first_routable_ipv4() {
        let node = Node::new(settings, implementation()).unwrap();
        assert_eq!(node.host(), found);
        assert!(!found.is_loopback() && !found.is_unspecified());
    }
}

#[tokio::test]
async fn cors_on_every_response_errors_and_options_included() {
    let h = harness();
    for (method, uri) in [
        ("GET", "/x-nmos/node/v1.3/self".to_string()),
        (
            "GET",
            format!("/x-nmos/node/v1.3/senders/{}", Uuid::new_v4()),
        ),
        ("GET", "/x-nmos/nothing".to_string()),
        ("PUT", "/x-nmos/node/v1.3/self".to_string()),
        (
            "PATCH",
            format!(
                "/x-nmos/connection/v1.2/single/senders/{}/staged",
                h.sender_id()
            ),
        ),
    ] {
        let (status, headers, _) = h.call(method, &uri, None).await;
        assert_eq!(
            headers["access-control-allow-origin"], "*",
            "{method} {uri} -> {status}"
        );
    }
    let (status, headers, _) = h.call("PUT", "/x-nmos/node/v1.3/self", None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(headers.contains_key("allow"));

    let request = axum::http::Request::builder()
        .method("OPTIONS")
        .uri(format!(
            "/x-nmos/node/v1.3/receivers/{}/target",
            h.receiver_id()
        ))
        .header("origin", "null")
        .header("access-control-request-method", "PUT")
        .header("access-control-request-headers", "Content-Type")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(h.router.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let methods = response.headers()["access-control-allow-methods"]
        .to_str()
        .unwrap();
    for m in ["GET", "PUT", "PATCH", "POST", "DELETE", "OPTIONS"] {
        assert!(methods.split(',').any(|x| x.trim() == m), "{methods}");
    }
    let allowed = response.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(allowed.split(',').any(|x| x.trim() == "content-type"));

    let (status, headers, _) = h.call("OPTIONS", "/x-nmos/node/v1.3/self", None).await;
    assert_eq!(status, StatusCode::OK, "a plain OPTIONS too");
    assert_eq!(headers["access-control-allow-origin"], "*");
}
