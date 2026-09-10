use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, StatusCode, Uri},
    response::Response,
};

use crate::neev::startup::ServerState;

use super::{add_private_headers, ApiError};

pub(super) async fn page(
    State(state): State<Arc<ServerState>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let file = match uri.path() {
        "/" => "index.html",
        "/login" => "login.html",
        "/onboarding" => "onboarding.html",
        "/workspace" => "workspace.html",
        "/settings" => "settings.html",
        "/auth/email/confirm" => "email-confirm.html",
        _ => return Err(ApiError::NotFound),
    };
    let bytes =
        std::fs::read(state.config.static_root.join(file)).map_err(|_| ApiError::NotFound)?;
    if bytes.len() > 512 * 1024 {
        return Err(ApiError::Invalid);
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    add_private_headers(&mut response);
    Ok(response)
}
