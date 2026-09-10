use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::neev::{auth::github, error::AuthError, startup::ServerState};

mod auth;
pub(crate) mod cookies;
mod static_files;
#[cfg(test)]
mod tests;

const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");

#[derive(Clone, Copy)]
pub(super) enum ApiError {
    Invalid,
    Unauthorized,
    Forbidden,
    Conflict,
    Limited,
    Unavailable,
    NotFound,
}

impl From<AuthError> for ApiError {
    fn from(value: AuthError) -> Self {
        match value {
            AuthError::Unauthorized => Self::Unauthorized,
            AuthError::Forbidden => Self::Forbidden,
            AuthError::Conflict => Self::Conflict,
            AuthError::Invalid => Self::Invalid,
            AuthError::Limited => Self::Limited,
            AuthError::Unavailable => Self::Unavailable,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Invalid => (StatusCode::BAD_REQUEST, "invalid"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::Limited => (StatusCode::TOO_MANY_REQUESTS, "limited"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        };
        let mut response = (status, Json(serde_json::json!({"code": code}))).into_response();
        add_private_headers(&mut response);
        response
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    version: u8,
    identity_mode: &'static str,
    runtime_enabled: bool,
    providers: Providers,
}

#[derive(Serialize)]
struct Providers {
    google: bool,
    email: bool,
    github: bool,
}

pub(crate) fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/workos/capabilities", get(capabilities))
        .route("/api/neev/preauth", get(auth::preauth))
        .route("/api/neev/session", get(auth::bootstrap))
        .route("/api/neev/logout", post(auth::logout))
        .route("/api/neev/auth/google/login", post(auth::google_login))
        .route("/api/neev/auth/google/link", post(auth::google_link))
        .route("/auth/google/callback", get(auth::google_callback))
        .route("/", get(static_files::page))
        .route("/login", get(static_files::page))
        .route("/onboarding", get(static_files::page))
        .route("/workspace", get(static_files::page))
        .route("/settings", get(static_files::page))
        .route("/auth/email/confirm", get(static_files::page))
        .fallback(not_found)
        .with_state(state)
}

async fn health() -> Response {
    private_json(serde_json::json!({"status":"ok"}))
}

async fn capabilities(State(state): State<Arc<ServerState>>) -> Response {
    private_json(Capabilities {
        version: 1,
        identity_mode: "native",
        runtime_enabled: false,
        providers: Providers {
            google: state.oauth.google_available(),
            email: false,
            github: github::available(),
        },
    })
}

async fn not_found() -> Response {
    ApiError::NotFound.into_response()
}

fn private_json(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    add_private_headers(&mut response);
    response
}

fn add_private_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}
