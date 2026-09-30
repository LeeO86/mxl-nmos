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
use serde_json::Value;

use super::node::NmosNode;

pub fn router(node: NmosNode) -> Router {
    Router::new()
        .route("/", get(dispatch))
        .route("/{*path}", get(dispatch).patch(dispatch).post(dispatch))
        .with_state(node)
}

async fn dispatch(
    State(node): State<NmosNode>,
    method: Method,
    uri: Uri,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path();
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
