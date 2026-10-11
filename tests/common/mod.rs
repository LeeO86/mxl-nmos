//! Shared test helpers: a test clock, a test node, an HTTP helper and a mock
//! registry.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use mxl_nmos::clock::{Clock, TaiTime};
use mxl_nmos::{
    media_types, Activation, ActiveConnection, Channel, Component, Device, Flow, Format, MxlDomain,
    Node, NodeImplementation, Rational, Receiver, ReceiverCaps, Rejected, Role, Sender, Settings,
    Source, StagedConnection,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

pub const DOMAIN: Uuid = Uuid::from_u128(0x3310f209_9351_47c0_b9a2_14c59b6a4c23);
pub const VIDEO_FLOW: Uuid = Uuid::from_u128(0x5b5e1c1a_1111_4111_8111_111111111111);

/// TAI time that follows tokio's clock, so `start_paused` tests control it.
pub struct TestClock {
    base: TaiTime,
    start: tokio::time::Instant,
}

impl TestClock {
    pub fn new(base: TaiTime) -> Arc<Self> {
        Arc::new(Self {
            base,
            start: tokio::time::Instant::now(),
        })
    }
}

impl Clock for TestClock {
    fn now(&self) -> TaiTime {
        self.base.plus(self.start.elapsed())
    }
}

/// How the test activation handler answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    Accept,
    Reject,
    Hang,
}

#[derive(Clone)]
pub struct Harness {
    pub node: Node,
    pub router: Router,
    pub activations: Arc<Mutex<Vec<Activation>>>,
    pub behaviour: Arc<Mutex<Behaviour>>,
    pub saved: Arc<Mutex<Vec<Vec<mxl_nmos::SavedConnection>>>>,
}

impl Harness {
    pub fn activations(&self) -> Vec<Activation> {
        self.activations.lock().unwrap().clone()
    }

    pub fn set_behaviour(&self, b: Behaviour) {
        *self.behaviour.lock().unwrap() = b;
    }

    pub fn sender_id(&self) -> Uuid {
        self.node
            .resource_id(mxl_nmos::ResourceType::Sender, "video-out")
    }

    pub fn receiver_id(&self) -> Uuid {
        self.node
            .resource_id(mxl_nmos::ResourceType::Receiver, "audio-in")
    }

    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        call(&self.router, method, uri, body).await
    }

    pub async fn get(&self, uri: &str) -> Value {
        let (status, _, body) = self.call("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
        body
    }

    pub async fn patch(&self, kind: &str, id: Uuid, body: Value) -> (StatusCode, Value) {
        let (status, _, body) = self
            .call(
                "PATCH",
                &format!("/x-nmos/connection/v1.2/single/{kind}/{id}/staged"),
                Some(body),
            )
            .await;
        (status, body)
    }

    pub async fn version_of(&self, collection: &str, id: Uuid) -> TaiTime {
        let doc = self
            .get(&format!("/x-nmos/node/v1.3/{collection}/{id}"))
            .await;
        doc["version"].as_str().unwrap().parse().unwrap()
    }
}

pub async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header("origin", "http://controller.example");
    let body = match body {
        Some(value) => {
            request = request.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, headers, value)
}

pub fn domains() -> Vec<MxlDomain> {
    vec![MxlDomain {
        id: DOMAIN,
        label: "Red".into(),
        path: "/Volumes/mxl/red".into(),
    }]
}

pub fn settings() -> Settings {
    let mut settings = Settings::new("Test", 3312);
    settings.seed = Some("test-seed".into());
    settings.host = Some("192.0.2.10".parse().unwrap());
    settings.registration = false;
    settings.activation_timeout = Duration::from_millis(500);
    settings
        .tags
        .insert("urn:x-example:production".into(), vec!["sport-sa".into()]);
    settings
}

pub fn harness() -> Harness {
    harness_with(settings(), TestClock::new(TaiTime::new(1_800_000_000, 0)))
}

pub fn harness_with(settings: Settings, clock: Arc<dyn Clock>) -> Harness {
    let activations = Arc::new(Mutex::new(Vec::new()));
    let behaviour = Arc::new(Mutex::new(Behaviour::Accept));
    let saved = Arc::new(Mutex::new(Vec::new()));
    let (record, mode, store) = (activations.clone(), behaviour.clone(), saved.clone());
    let implementation = NodeImplementation::new(move |activation: Activation| {
        let record = record.clone();
        let behaviour = *mode.lock().unwrap();
        Box::pin(async move {
            record.lock().unwrap().push(activation);
            match behaviour {
                Behaviour::Accept => Ok(()),
                Behaviour::Reject => Err(Rejected("flow format not supported".into())),
                Behaviour::Hang => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Ok(())
                }
            }
        })
    })
    .domains(domains)
    .on_connections_changed(move |all| store.lock().unwrap().push(all))
    .clock(clock);
    let node = Node::new(settings, implementation).unwrap();
    build(&node);
    Harness {
        router: node.router(),
        node,
        activations,
        behaviour,
        saved,
    }
}

/// One video sender (Source, Flow, Sender) and one audio receiver, active as the
/// siblings' nodes start: the sender writes its own flow, the receiver is off.
pub fn build(node: &Node) {
    node.insert_device(Device::new("dev", "Camera").description("test device"))
        .unwrap();
    let mut source = Source::video("video-out", "dev");
    source.label = "Cam".into();
    node.insert_source(source).unwrap();
    let mut flow = Flow::video(
        "video-out",
        "video-out",
        "dev",
        media_types::VIDEO_V210,
        Rational::new(50, 1),
        1920,
        1080,
        Component::ycbcr_422(1920, 1080, 10),
    );
    flow.label = "Cam".into();
    node.insert_flow(flow).unwrap();
    let mut sender = Sender::mxl("video-out", "video-out", "dev");
    sender.label = "Cam".into();
    mxl_nmos::add_group_hint(&mut sender.tags, "Camera", "Video");
    sender.mxl_domain_id = Some(DOMAIN);
    sender.mxl_flow_id = Some(VIDEO_FLOW);
    node.insert_sender(sender).unwrap();
    node.set_staged(
        Role::Sender,
        "video-out",
        StagedConnection {
            master_enable: true,
            peer_id: None,
            mxl_domain_id: Some(DOMAIN).into(),
            mxl_flow_id: Some(VIDEO_FLOW).into(),
        },
    )
    .unwrap();
    node.set_active(
        Role::Sender,
        "video-out",
        ActiveConnection {
            master_enable: true,
            peer_id: None,
            mxl_domain_id: Some(DOMAIN),
            mxl_flow_id: Some(VIDEO_FLOW),
        },
    )
    .unwrap();
    let mut receiver = Receiver::mxl(
        "audio-in",
        "dev",
        Format::Audio,
        vec![media_types::AUDIO_FLOAT32.into()],
    );
    receiver.label = "Mic".into();
    receiver.caps = ReceiverCaps {
        media_types: vec![media_types::AUDIO_FLOAT32.into()],
        constraint_sets: vec![json!({"urn:x-nmos:cap:format:channel_count": {"enum": [1, 2]}})],
        version: None,
    };
    node.insert_receiver(receiver).unwrap();
    let mut audio = Source::audio(
        "audio-out",
        "dev",
        vec![Channel::new("L"), Channel::new("R")],
    );
    audio.label = "Mix".into();
    node.insert_source(audio).unwrap();
    node.insert_flow(Flow::audio(
        "audio-out",
        "audio-out",
        "dev",
        media_types::AUDIO_FLOAT32,
        Rational::new(48_000, 1),
        Some(32),
    ))
    .unwrap();
}

// ---- mock registry ----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub resource_type: Option<String>,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub at: std::time::Instant,
}

#[derive(Clone)]
pub struct MockRegistry {
    pub addr: SocketAddr,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    /// Status of heartbeats.
    pub heartbeat_status: Arc<AtomicU16>,
    /// Status of the first Node POST (200 simulates a stale registration).
    pub node_post_status: Arc<AtomicU16>,
    /// Status of every resource POST except a 200/201 node override.
    pub post_status: Arc<AtomicU16>,
    pub nodes: Arc<Mutex<Vec<String>>>,
}

impl MockRegistry {
    pub async fn start() -> Self {
        let registry = Self {
            addr: "127.0.0.1:0".parse().unwrap(),
            requests: Arc::default(),
            heartbeat_status: Arc::new(AtomicU16::new(200)),
            node_post_status: Arc::new(AtomicU16::new(201)),
            post_status: Arc::new(AtomicU16::new(201)),
            nodes: Arc::default(),
        };
        let state = registry.clone();
        let app = Router::new().fallback(move |request: Request<Body>| {
            let state = state.clone();
            async move { state.handle(request).await }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { addr, ..registry }
    }

    async fn handle(&self, request: Request<Body>) -> (StatusCode, String) {
        let method = request.method().to_string();
        let path = request.uri().path().to_string();
        let headers = request.headers().clone();
        let body = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let parsed: Option<Value> = serde_json::from_slice(&body).ok();
        let resource_type = parsed
            .as_ref()
            .and_then(|v| v["type"].as_str().map(str::to_string));
        self.requests.lock().unwrap().push(Recorded {
            method: method.clone(),
            path: path.clone(),
            resource_type: resource_type.clone(),
            headers,
            body,
            at: std::time::Instant::now(),
        });
        let code = |c: u16| StatusCode::from_u16(c).unwrap();
        match (method.as_str(), path.as_str()) {
            ("POST", "/x-nmos/registration/v1.3/resource") => {
                if resource_type.as_deref() == Some("node") {
                    let id = parsed.unwrap()["data"]["id"].as_str().unwrap().to_string();
                    let status = self.node_post_status.swap(201, Ordering::SeqCst);
                    self.nodes.lock().unwrap().push(id);
                    (code(status), "{}".into())
                } else {
                    (code(self.post_status.load(Ordering::SeqCst)), "{}".into())
                }
            }
            ("POST", p) if p.starts_with("/x-nmos/registration/v1.3/health/nodes/") => (
                code(self.heartbeat_status.load(Ordering::SeqCst)),
                json!({"health": "0"}).to_string(),
            ),
            ("DELETE", p) if p.starts_with("/x-nmos/registration/v1.3/resource/") => {
                if let Some(id) = p.strip_prefix("/x-nmos/registration/v1.3/resource/nodes/") {
                    self.nodes.lock().unwrap().retain(|n| n != id);
                }
                (StatusCode::NO_CONTENT, String::new())
            }
            ("GET", p) if p.starts_with("/x-nmos/query/v1.3/nodes/") => {
                let id = p.trim_start_matches("/x-nmos/query/v1.3/nodes/");
                if self.nodes.lock().unwrap().iter().any(|n| n == id) {
                    (StatusCode::OK, "{}".into())
                } else {
                    (StatusCode::NOT_FOUND, "{}".into())
                }
            }
            _ => (StatusCode::NOT_FOUND, "{}".into()),
        }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }

    /// The order of POSTed resource types.
    pub fn posted_types(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.resource_type.is_some())
            .map(|r| r.resource_type.unwrap())
            .collect()
    }

    pub fn deleted_paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| r.path)
            .collect()
    }
}

pub async fn wait_for(what: &str, secs: u64, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}
