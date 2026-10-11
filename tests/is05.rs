//! IS-05 Connection API: the staged → active state machine and the activation
//! callback.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use mxl_nmos::clock::TaiTime;
use mxl_nmos::{ActiveConnection, Cause, Role, SavedConnection};
use serde_json::{json, Value};

const BASE: TaiTime = TaiTime {
    secs: 1_800_000_000,
    nanos: 0,
};

fn leg(value: &Value) -> &Value {
    &value["transport_params"][0]
}

#[tokio::test]
async fn immediate_sender_activation_resolves_auto_and_calls_back_once() {
    let h = harness();
    let id = h.sender_id();
    let before = h.version_of("senders", id).await;
    let (status, body) = h
        .patch(
            "senders",
            id,
            json!({
                "master_enable": true,
                "activation": {"mode": "activate_immediate"},
                "transport_params": [{"mxl_domain_id": "auto", "mxl_flow_id": "auto"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["activation"]["mode"], "activate_immediate");
    assert!(body["activation"]["requested_time"].is_null());
    assert!(body["activation"]["activation_time"].is_string());
    assert_eq!(leg(&body)["mxl_flow_id"], "auto", "staged keeps auto");

    let calls = h.activations();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cause, Cause::Immediate);
    assert_eq!(calls[0].key, "video-out");
    assert_eq!(calls[0].mxl_domain_id, Some(DOMAIN));
    assert_eq!(calls[0].mxl_flow_id, Some(VIDEO_FLOW));
    assert_eq!(
        calls[0].domain_path.as_deref(),
        Some(std::path::Path::new("/Volumes/mxl/red"))
    );

    let active = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/active"
        ))
        .await;
    assert_eq!(leg(&active)["mxl_flow_id"], VIDEO_FLOW.to_string());
    assert_eq!(leg(&active)["mxl_domain_id"], DOMAIN.to_string());
    assert_eq!(active["activation"]["mode"], "activate_immediate");

    let staged = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/staged"
        ))
        .await;
    assert_eq!(
        staged["activation"],
        json!({"mode": null, "requested_time": null, "activation_time": null})
    );

    assert!(
        h.version_of("senders", id).await > before,
        "activation bumps the IS-04 version"
    );
}

#[tokio::test]
async fn receiver_activation_updates_the_subscription() {
    let h = harness();
    let id = h.receiver_id();
    let sender = uuid::Uuid::new_v4();
    let flow = uuid::Uuid::new_v4();
    let (status, body) = h
        .patch(
            "receivers",
            id,
            json!({
                "sender_id": sender.to_string(),
                "master_enable": true,
                "activation": {"mode": "activate_immediate"},
                "transport_params": [{"mxl_domain_id": DOMAIN.to_string(), "mxl_flow_id": flow.to_string()}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let receiver = h.get(&format!("/x-nmos/node/v1.3/receivers/{id}")).await;
    assert_eq!(
        receiver["subscription"],
        json!({"sender_id": sender.to_string(), "active": true})
    );
    let calls = h.activations();
    assert_eq!(calls[0].peer_id, Some(sender));
    assert_eq!(calls[0].mxl_flow_id, Some(flow));
    assert_eq!(calls[0].role, Role::Receiver);
}

#[tokio::test]
async fn master_enable_false_is_a_normal_activation() {
    let h = harness();
    let id = h.sender_id();
    let (status, body) = h
        .patch(
            "senders",
            id,
            json!({"master_enable": false, "activation": {"mode": "activate_immediate"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.activations().len(), 1);
    assert!(!h.activations()[0].master_enable);
    let active = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/active"
        ))
        .await;
    assert_eq!(active["master_enable"], false);
    let sender = h.get(&format!("/x-nmos/node/v1.3/senders/{id}")).await;
    assert_eq!(sender["subscription"]["active"], false);
}

#[tokio::test]
async fn a_rejected_activation_leaves_active_unchanged() {
    let h = harness();
    h.set_behaviour(Behaviour::Reject);
    let id = h.receiver_id();
    let before = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/receivers/{id}/active"
        ))
        .await;
    let version = h.version_of("receivers", id).await;
    let (status, body) = h
        .patch(
            "receivers",
            id,
            json!({"master_enable": true, "activation": {"mode": "activate_immediate"},
                   "transport_params": [{"mxl_flow_id": uuid::Uuid::new_v4().to_string()}]}),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], 500);
    assert_eq!(body["error"], "Internal Server Error");
    assert_eq!(body["debug"], "flow format not supported");
    let after = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/receivers/{id}/active"
        ))
        .await;
    assert_eq!(before, after);
    assert_eq!(version, h.version_of("receivers", id).await);
}

#[tokio::test(start_paused = true)]
async fn a_callback_that_does_not_answer_times_out() {
    let h = harness();
    h.set_behaviour(Behaviour::Hang);
    let id = h.sender_id();
    let started = tokio::time::Instant::now();
    let (status, body) = h
        .patch(
            "senders",
            id,
            json!({"master_enable": false, "activation": {"mode": "activate_immediate"}}),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body["debug"].as_str().unwrap().contains("500 ms"), "{body}");
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert!(
        h.node
            .active(Role::Sender, "video-out")
            .unwrap()
            .master_enable
    );
}

#[tokio::test]
async fn auto_rules() {
    let h = harness();
    let (status, _) = h
        .patch(
            "receivers",
            h.receiver_id(),
            json!({"transport_params": [{"mxl_flow_id": "auto"}]}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "receivers reject auto for mxl_flow_id"
    );
    let (status, body) = h
        .patch(
            "receivers",
            h.receiver_id(),
            json!({"master_enable": true, "activation": {"mode": "activate_immediate"},
                   "transport_params": [{"mxl_domain_id": "auto", "mxl_flow_id": null}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let active = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/receivers/{}/active",
            h.receiver_id()
        ))
        .await;
    assert_eq!(
        leg(&active)["mxl_domain_id"],
        DOMAIN.to_string(),
        "the only domain"
    );
    let (status, _) = h
        .patch(
            "senders",
            h.sender_id(),
            json!({"transport_params": [{"mxl_domain_id": uuid::Uuid::new_v4().to_string()}]}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an invisible domain is rejected"
    );
}

#[tokio::test(start_paused = true)]
async fn scheduled_absolute_activation_uses_the_tai_clock() {
    // The clock starts far from the system time: an implementation that compared
    // with UTC or the system TAI clock would fire at once or never.
    let h = harness_with(settings(), TestClock::new(BASE));
    let id = h.sender_id();
    let at = BASE.plus(Duration::from_secs(2));
    let (status, body) = h
        .patch(
            "senders",
            id,
            json!({"master_enable": false,
                   "activation": {"mode": "activate_scheduled_absolute", "requested_time": at.to_string()}}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["activation"]["mode"], "activate_scheduled_absolute");
    assert_eq!(body["activation"]["requested_time"], at.to_string());
    assert_eq!(body["activation"]["activation_time"], at.to_string());
    let staged = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/staged"
        ))
        .await;
    assert_eq!(staged["activation"]["activation_time"], at.to_string());

    let (status, body) = h.patch("senders", id, json!({"master_enable": true})).await;
    assert_eq!(
        status,
        StatusCode::LOCKED,
        "staged is locked while scheduled: {body}"
    );

    tokio::time::sleep(Duration::from_millis(1990)).await;
    assert!(h.activations().is_empty(), "not before the activation time");
    tokio::time::sleep(Duration::from_millis(20)).await;
    tokio::task::yield_now().await;
    let calls = h.activations();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cause, Cause::Scheduled);
    let active = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/active"
        ))
        .await;
    assert_eq!(active["master_enable"], false);
    assert_eq!(active["activation"]["mode"], "activate_scheduled_absolute");
    assert_eq!(active["activation"]["activation_time"], at.to_string());
    let staged = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{id}/staged"
        ))
        .await;
    assert!(
        staged["activation"]["mode"].is_null(),
        "unlocked again: {staged}"
    );
}

#[tokio::test(start_paused = true)]
async fn scheduled_relative_activation_counts_from_the_tai_now() {
    let h = harness_with(settings(), TestClock::new(BASE));
    let id = h.receiver_id();
    let (status, body) = h
        .patch(
            "receivers",
            id,
            json!({"master_enable": false,
                   "activation": {"mode": "activate_scheduled_relative", "requested_time": "1:500000000"}}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body["activation"]["activation_time"],
        BASE.plus(Duration::from_millis(1500)).to_string()
    );
    tokio::time::sleep(Duration::from_millis(1490)).await;
    assert!(h.activations().is_empty());
    tokio::time::sleep(Duration::from_millis(20)).await;
    tokio::task::yield_now().await;
    assert_eq!(h.activations().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_scheduled_activation_can_be_cancelled() {
    let h = harness_with(settings(), TestClock::new(BASE));
    let id = h.sender_id();
    let at = BASE.plus(Duration::from_secs(1));
    let (status, _) = h
        .patch(
            "senders",
            id,
            json!({"activation": {"mode": "activate_scheduled_absolute", "requested_time": at.to_string()}}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = h
        .patch(
            "senders",
            id,
            json!({"master_enable": false, "activation": {"mode": null}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["master_enable"], false);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(h.activations().is_empty());
    let (status, _) = h.patch("senders", id, json!({"master_enable": true})).await;
    assert_eq!(status, StatusCode::OK, "unlocked");
}

#[tokio::test]
async fn bulk_results_are_per_entry() {
    let h = harness();
    h.set_behaviour(Behaviour::Reject);
    let unknown = uuid::Uuid::new_v4();
    let (status, _, body) = h
        .call(
            "POST",
            "/x-nmos/connection/v1.2/bulk/senders",
            Some(json!([
                {"id": h.sender_id().to_string(), "params": {"master_enable": false, "activation": {"mode": "activate_immediate"}}},
                {"id": h.sender_id().to_string(), "params": {"master_enable": false}},
                {"id": unknown.to_string(), "params": {"master_enable": false}},
                {"id": h.sender_id().to_string(), "params": {"transport_params": [{"bad": 1}]}},
                {"params": {}}
            ])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let codes: Vec<u64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_u64().unwrap())
        .collect();
    assert_eq!(codes, vec![500, 200, 404, 400, 400], "{body}");
    assert_eq!(body[0]["debug"], "flow format not supported");
    assert_eq!(
        h.activations().len(),
        1,
        "the callback runs once per activated entry"
    );
    assert_eq!(h.activations()[0].cause, Cause::Bulk);

    h.set_behaviour(Behaviour::Accept);
    let (_, _, body) = h
        .call(
            "POST",
            "/x-nmos/connection/v1.2/bulk/receivers",
            Some(json!([{"id": h.receiver_id().to_string(),
                         "params": {"master_enable": true, "activation": {"mode": "activate_immediate"}}}])),
        )
        .await;
    assert_eq!(
        body,
        json!([{"id": h.receiver_id().to_string(), "code": 200}])
    );
}

#[tokio::test]
async fn restore_calls_the_callback_and_persistence_sees_changes() {
    let h = harness();
    let flow = uuid::Uuid::new_v4();
    let sender = uuid::Uuid::new_v4();
    let saved = vec![
        SavedConnection {
            role: Role::Receiver,
            key: "audio-in".into(),
            active: ActiveConnection {
                master_enable: true,
                peer_id: Some(sender),
                mxl_domain_id: Some(DOMAIN),
                mxl_flow_id: Some(flow),
            },
        },
        SavedConnection {
            role: Role::Receiver,
            key: "gone".into(),
            active: ActiveConnection::default(),
        },
    ];
    let text = serde_json::to_string(&saved).unwrap();
    let parsed: Vec<SavedConnection> = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, saved);
    let failed = h.node.restore(parsed).await;
    assert!(failed.is_empty());
    let calls = h.activations();
    assert_eq!(calls.len(), 1, "unknown keys are skipped");
    assert_eq!(calls[0].cause, Cause::Restore);
    assert_eq!(calls[0].mxl_flow_id, Some(flow));
    assert_eq!(
        h.node.active(Role::Receiver, "audio-in").unwrap(),
        saved[0].active
    );
    let receiver = h
        .get(&format!("/x-nmos/node/v1.3/receivers/{}", h.receiver_id()))
        .await;
    assert_eq!(receiver["subscription"]["sender_id"], sender.to_string());

    h.set_behaviour(Behaviour::Reject);
    let failed = h.node.restore(saved.clone()).await;
    assert_eq!(failed.len(), 1);

    h.set_behaviour(Behaviour::Accept);
    h.patch(
        "receivers",
        h.receiver_id(),
        json!({"master_enable": false, "activation": {"mode": "activate_immediate"}}),
    )
    .await;
    let snapshots = h.saved.lock().unwrap().clone();
    let last = snapshots.last().expect("on_connections_changed was called");
    let receiver = last.iter().find(|c| c.key == "audio-in").unwrap();
    assert!(!receiver.active.master_enable);
    assert_eq!(last.len(), 2, "every sender and receiver");
}

#[tokio::test]
async fn connection_api_resources() {
    let h = harness();
    let (s, r) = (h.sender_id(), h.receiver_id());
    assert_eq!(
        h.get("/x-nmos/connection/v1.2/").await,
        json!(["bulk/", "single/"])
    );
    assert_eq!(
        h.get("/x-nmos/connection/v1.2/single/senders").await,
        json!([format!("{s}/")])
    );
    assert_eq!(
        h.get(&format!("/x-nmos/connection/v1.2/single/receivers/{r}"))
            .await,
        json!(["constraints/", "staged/", "active/", "transporttype/"])
    );
    assert_eq!(
        h.get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{s}/transporttype"
        ))
        .await,
        json!("urn:x-nmos:transport:mxl")
    );
    let constraints = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/senders/{s}/constraints"
        ))
        .await;
    assert_eq!(
        constraints,
        json!([{"mxl_domain_id": {"enum": [DOMAIN.to_string()]}, "mxl_flow_id": {}}])
    );
    let (status, _, body) = h
        .call(
            "GET",
            &format!("/x-nmos/connection/v1.2/single/senders/{s}/transportfile"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], 404);
    let (status, headers, _) = h
        .call("GET", "/x-nmos/connection/v1.2/bulk/senders", None)
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "POST, OPTIONS");
    let (status, _, _) = h
        .call(
            "PATCH",
            &format!(
                "/x-nmos/connection/v1.2/single/senders/{}/staged",
                uuid::Uuid::new_v4()
            ),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, body) = h
        .call(
            "PATCH",
            &format!("/x-nmos/connection/v1.2/single/senders/{s}/staged"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "Bad Request");
}
