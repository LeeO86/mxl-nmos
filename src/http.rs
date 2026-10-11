//! Axum routes for the IS-04 Node API and the IS-05 Connection API.
//!
//! Every response carries CORS headers, errors and `OPTIONS` included; the AMWA
//! IS-04-01 suite checks them.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};

use crate::node::Node;
use crate::Error;

const ALLOWED_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS";

impl Node {
    /// The `/x-nmos` routes with CORS, to merge into an application's router.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/x-nmos", any(dispatch))
            .route("/x-nmos/", any(dispatch))
            .route("/x-nmos/{*path}", any(dispatch))
            .with_state(self.clone())
            .layer(cors_layer())
    }

    /// Bind the node's own listener (for example on `NMOS_PORT`). A bind failure
    /// is [`Error::Bind`]; the platform contract says the application then exits
    /// with code 75.
    pub async fn bind(&self, addr: SocketAddr) -> Result<Server, Error> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| Error::Bind { addr, source })?;
        Ok(Server {
            node: self.clone(),
            listener,
        })
    }
}

/// The node's own HTTP listener, from [`Node::bind`].
#[derive(Debug)]
pub struct Server {
    node: Node,
    listener: tokio::net::TcpListener,
}

impl Server {
    /// The bound address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve until the node shuts down.
    pub async fn run(self) -> std::io::Result<()> {
        let mut shutdown = self.node.inner.shutdown_signal.subscribe();
        axum::serve(self.listener, self.node.router())
            .with_graceful_shutdown(async move {
                let _ = shutdown.wait_for(|stopped| *stopped).await;
            })
            .await
    }
}

fn cors_layer() -> CorsLayer {
    // Methods and headers are listed: a `*` fails the suite, which checks that each
    // declared method and `Content-Type` appear in the allow lists.
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::ACCEPT, header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(std::time::Duration::from_secs(3600))
}

async fn dispatch(State(node): State<Node>, method: Method, uri: Uri, body: Bytes) -> Response {
    if method == Method::OPTIONS {
        let mut response = StatusCode::OK.into_response();
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static(ALLOWED_METHODS));
        return response;
    }
    let (status, value) = node.dispatch(method.as_str(), uri.path(), &body).await;
    json_response(status, value, uri.path())
}

fn allowed_for(path: &str) -> &'static str {
    let path = path.trim_end_matches('/');
    if path.ends_with("/staged") {
        "GET, HEAD, PATCH, OPTIONS"
    } else if path.ends_with("/bulk/senders") || path.ends_with("/bulk/receivers") {
        "POST, OPTIONS"
    } else {
        "GET, HEAD, OPTIONS"
    }
}

fn json_response(status: u16, value: Value, path: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (code, axum::Json(value)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if code == StatusCode::METHOD_NOT_ALLOWED {
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static(allowed_for(path)));
    }
    response
}
