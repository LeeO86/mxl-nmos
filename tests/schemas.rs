//! Every served IS-04 resource and IS-05 document validated against the official
//! AMWA schemas (IS-04 v1.3.3, IS-05 v1.2.0, BCP-007-03 v1.0.0) in `tests/schemas`.

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::{json, Value};

fn schema_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/schemas")
        .join(name)
}

fn validate(dir: &str, file: &str, instance: &Value) {
    let path = schema_dir(dir).join(file);
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .with_base_uri(file_uri(&path))
        .build(&schema)
        .unwrap_or_else(|err| panic!("{file}: {err}"));
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{dir}/{file}: {errors:#?}\n{instance:#}");
}

fn file_uri(path: &std::path::Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

const IS04: &str = "is-04-v1.3.3";
const IS05: &str = "is-05-v1.2.0";
const MXL: &str = "bcp-007-03-v1.0.0";

#[tokio::test]
async fn node_api_resources_match_is04() {
    let h = harness();
    validate(IS04, "node.json", &h.get("/x-nmos/node/v1.3/self").await);
    for (collection, single, list) in [
        ("devices", "device.json", "devices.json"),
        ("sources", "source.json", "sources.json"),
        ("flows", "flow.json", "flows.json"),
        ("senders", "sender.json", "senders.json"),
        ("receivers", "receiver.json", "receivers.json"),
    ] {
        let all = h.get(&format!("/x-nmos/node/v1.3/{collection}")).await;
        assert!(!all.as_array().unwrap().is_empty(), "{collection}");
        validate(IS04, list, &all);
        for item in all.as_array().unwrap() {
            validate(IS04, single, item);
        }
    }
    validate(
        IS04,
        "nodeapi-base.json",
        &h.get("/x-nmos/node/v1.3/").await,
    );
    // What the registry receives.
    for doc in h.node.resources() {
        let typ = if doc.get("api").is_some() {
            "node"
        } else if doc.get("node_id").is_some() {
            "device"
        } else if doc.get("transport").is_some() && doc.get("flow_id").is_some() {
            "sender"
        } else if doc.get("transport").is_some() {
            "receiver"
        } else if doc.get("source_id").is_some() {
            "flow"
        } else {
            "source"
        };
        validate(
            IS04,
            "registrationapi-resource-post-request.json",
            &json!({"type": typ, "data": doc}),
        );
    }
}

#[tokio::test]
async fn connection_api_documents_match_is05_and_bcp_007_03() {
    let h = harness();
    validate(
        IS05,
        "connectionapi-base.json",
        &h.get("/x-nmos/connection/v1.2/").await,
    );
    validate(
        IS05,
        "connectionapi-single.json",
        &h.get("/x-nmos/connection/v1.2/single/").await,
    );
    validate(
        IS05,
        "connectionapi-bulk.json",
        &h.get("/x-nmos/connection/v1.2/bulk/").await,
    );
    for (kind, id, role) in [
        ("senders", h.sender_id(), "sender"),
        ("receivers", h.receiver_id(), "receiver"),
    ] {
        let base = format!("/x-nmos/connection/v1.2/single/{kind}/{id}");
        validate(
            IS05,
            "sender-receiver-base.json",
            &h.get(&format!("/x-nmos/connection/v1.2/single/{kind}"))
                .await,
        );
        let constraints = h.get(&format!("{base}/constraints")).await;
        validate(IS05, "constraints-schema.json", &constraints);
        validate(MXL, "constraints-schema-mxl.json", &constraints[0]);
        validate(
            IS05,
            "transporttype-response-schema.json",
            &h.get(&format!("{base}/transporttype")).await,
        );
        for leaf in ["staged", "active"] {
            let doc = h.get(&format!("{base}/{leaf}")).await;
            validate(IS05, &format!("{role}-response-schema.json"), &doc);
            validate(
                MXL,
                &format!("{role}_transport_params_mxl.json"),
                &doc["transport_params"][0],
            );
        }
        let patch = json!({"master_enable": true, "activation": {"mode": "activate_immediate"},
                           "transport_params": [{"mxl_domain_id": DOMAIN.to_string(),
                                                 "mxl_flow_id": VIDEO_FLOW.to_string()}]});
        validate(IS05, &format!("{role}-stage-schema.json"), &patch);
        let (_, response) = h.patch(kind, id, patch).await;
        validate(IS05, &format!("{role}-response-schema.json"), &response);
        let scheduled = json!({"activation": {"mode": "activate_scheduled_relative", "requested_time": "60:0"}});
        let (status, response) = h.patch(kind, id, scheduled).await;
        assert_eq!(status, 202);
        validate(IS05, &format!("{role}-response-schema.json"), &response);
        validate(
            IS05,
            &format!("{role}-response-schema.json"),
            &h.get(&format!("{base}/staged")).await,
        );
        h.patch(kind, id, json!({"activation": {"mode": null}}))
            .await;
    }
    let (_, _, bulk) = h
        .call(
            "POST",
            "/x-nmos/connection/v1.2/bulk/senders",
            Some(
                json!([{"id": h.sender_id().to_string(), "params": {"master_enable": false}},
                          {"id": uuid::Uuid::new_v4().to_string(), "params": {}}]),
            ),
        )
        .await;
    validate(IS05, "bulk-response-schema.json", &bulk);
    let (_, error) = h.patch("senders", h.sender_id(), json!({"nope": 1})).await;
    validate(IS05, "error.json", &error);
    let (_, _, error) = h.call("GET", "/x-nmos/node/v1.3/flows/x", None).await;
    validate(IS04, "error.json", &error);
}

fn is_valid(dir: &str, file: &str, instance: &Value) -> bool {
    let path = schema_dir(dir).join(file);
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .with_base_uri(file_uri(&path))
        .build(&schema)
        .unwrap()
        .is_valid(instance)
}

#[tokio::test]
async fn the_validator_rejects_broken_documents() {
    let h = harness();
    let mut node = h.get("/x-nmos/node/v1.3/self").await;
    node["api"]["endpoints"][0]["port"] = json!("3312");
    assert!(
        !is_valid(IS04, "node.json", &node),
        "a $ref'd field is checked"
    );
    let sources = h.get("/x-nmos/node/v1.3/sources").await;
    let mut audio = sources
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["format"] == "urn:x-nmos:format:audio")
        .unwrap()
        .clone();
    audio.as_object_mut().unwrap().remove("channels");
    assert!(
        !is_valid(IS04, "source.json", &audio),
        "audio sources need channels"
    );
    let mut staged = h
        .get(&format!(
            "/x-nmos/connection/v1.2/single/receivers/{}/staged",
            h.receiver_id()
        ))
        .await;
    staged["transport_params"][0]["mxl_flow_id"] = json!("auto");
    assert!(!is_valid(
        MXL,
        "receiver_transport_params_mxl.json",
        &staged["transport_params"][0]
    ));
}
