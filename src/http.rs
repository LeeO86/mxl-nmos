//! Axum routes for the IS-04 Node API and the IS-05 Connection API.
//!
//! These routes are the AMWA APIs, mounted outside `/api` and without Strom
//! authentication. Controllers on the network have to be able to reach them.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

use super::node::NmosNode;

/// IS-04 and IS-05 routes relative to `/x-nmos`.
pub fn router(node: NmosNode) -> Router {
    api_router(node).layer(cors_layer())
}

/// Routes mounted at `/x-nmos`, including the slash-less API root.
pub fn mounted(node: NmosNode) -> Router {
    Router::new()
        // Nest maps the inner `/` onto `/x-nmos`. The trailing-slash form is a
        // different route and otherwise 404s.
        .route("/x-nmos/", get(api_index))
        .nest("/x-nmos", api_router(node))
        .layer(cors_layer())
}

async fn api_index() -> Response {
    json_response(200, api_root())
}

fn api_router(node: NmosNode) -> Router {
    Router::new()
        .route("/", get(dispatch))
        .route("/{*path}", get(dispatch).patch(dispatch).post(dispatch))
        .with_state(node)
}

fn cors_layer() -> CorsLayer {
    // List methods and headers. A `*` value fails the suite, which checks that
    // each declared method and `Content-Type` appear in the allow lists.
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
}

fn api_root() -> Value {
    json!(["connection/", "node/"])
}

async fn dispatch(
    State(node): State<NmosNode>,
    method: Method,
    uri: Uri,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path();
    if path.is_empty() || path == "/" {
        return json_response(200, api_root());
    }
    match node.dispatch(method.as_str(), path, &body).await {
        Ok((status, value)) => json_response(status, value),
        Err((status, value)) => json_response(status, value),
    }
}

fn json_response(status: u16, value: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (code, axum::Json(value)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}
