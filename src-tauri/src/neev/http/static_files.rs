use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, StatusCode, Uri},
    response::Response,
};

use crate::neev::startup::ServerState;

use super::{add_private_headers, ApiError};

const CONFIRM_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'";

pub(super) async fn page(
    State(state): State<Arc<ServerState>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    if uri.path() == "/auth/invite" && uri.query().is_some() {
        return Err(ApiError::Invalid);
    }
    let (file, confirmation) = match uri.path() {
        "/" => ("index.html", false),
        "/login" => ("login.html", false),
        "/onboarding" => ("onboarding.html", false),
        "/workspace" => ("workspace.html", false),
        "/settings" => ("settings.html", false),
        "/auth/email/confirm" => ("neev-auth/confirm.html", true),
        "/auth/invite" => ("neev-auth/invite.html", true),
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
    if confirmation {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONFIRM_CSP),
        );
    }
    Ok(response)
}

pub(super) async fn asset(
    State(state): State<Arc<ServerState>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let (file, content_type) = match uri.path() {
        "/neev-auth/confirm.js" => ("neev-auth/confirm.js", "text/javascript; charset=utf-8"),
        "/neev-auth/confirm.css" => ("neev-auth/confirm.css", "text/css; charset=utf-8"),
        "/neev-auth/invite.js" => ("neev-auth/invite.js", "text/javascript; charset=utf-8"),
        "/neev-auth/invite.css" => ("neev-auth/invite.css", "text/css; charset=utf-8"),
        _ => return Err(ApiError::NotFound),
    };
    let bytes =
        std::fs::read(state.config.static_root.join(file)).map_err(|_| ApiError::NotFound)?;
    if bytes.len() > 128 * 1024 {
        return Err(ApiError::Invalid);
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONFIRM_CSP),
    );
    add_private_headers(&mut response);
    Ok(response)
}
