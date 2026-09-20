use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Serialize;

use crate::neev::{auth::github, error::AuthError, startup::ServerState};

mod agent;
mod auth;
mod catalog;
mod content;
#[cfg(test)]
mod content_tests;
pub(crate) mod cookies;
mod org;
mod static_files;
#[cfg(test)]
mod tests;

const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");

#[derive(Clone, Copy, Debug)]
pub(super) enum ApiError {
    Invalid,
    Unauthorized,
    Forbidden,
    Conflict,
    Limited,
    ContentQuota,
    Archived,
    RequestTimeout,
    Unavailable,
    StorageUnavailable,
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
            Self::ContentQuota => (StatusCode::TOO_MANY_REQUESTS, "content_quota"),
            Self::Archived => (StatusCode::CONFLICT, "archived"),
            Self::RequestTimeout => (StatusCode::REQUEST_TIMEOUT, "request_timeout"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::StorageUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
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
    catalog: CatalogCapabilities,
}

#[derive(Serialize)]
struct Providers {
    google: bool,
    email: bool,
    github: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogCapabilities {
    node_kinds: [&'static str; 2],
    repository_defaults: bool,
    chats: bool,
    documents: bool,
}

pub(crate) fn router(state: Arc<ServerState>) -> Router {
    let email_available = state.magic.is_some();
    let mut router = Router::new()
        .route("/healthz", get(health))
        .route("/api/workos/capabilities", get(capabilities))
        .route("/api/neev/preauth", get(auth::preauth))
        .route("/api/neev/session", get(auth::bootstrap))
        .route("/api/neev/auth/methods", get(auth::methods))
        .route("/api/neev/nodes", get(catalog::list).post(catalog::create))
        .route("/api/neev/nodes/{node_id}", patch(catalog::update))
        .route(
            "/api/neev/nodes/{node_id}/chats",
            get(content::list_chats).post(content::create_chat),
        )
        .route(
            "/api/neev/nodes/{node_id}/documents",
            get(content::list_documents).post(content::create_document),
        )
        .route(
            "/api/neev/resources/{id}",
            get(content::get_resource).patch(content::update_resource),
        )
        .route(
            "/api/neev/chats/{id}/messages",
            get(content::list_messages).post(content::append_message),
        )
        .route(
            "/api/neev/documents/{id}/revisions",
            get(content::list_revisions).post(content::save_revision),
        )
        .route(
            "/api/neev/documents/{id}/content",
            get(content::get_document_content),
        )
        .route(
            "/api/neev/documents/{id}/download",
            get(content::download_document),
        )
        .route(
            "/api/neev/documents/{id}/restore",
            post(content::restore_revision),
        )
        .route(
            "/api/neev/documents/{id}/images",
            post(content::upload_document_image),
        )
        .route("/api/neev/images/{key}", get(content::serve_image))
        .route("/api/neev/organizations", get(org::list).post(org::create))
        .route("/api/neev/organizations/select", post(org::select))
        .route(
            "/api/neev/organization",
            get(org::details).patch(org::update_branding),
        )
        .route("/api/neev/organization/members", get(org::members))
        .route(
            "/api/neev/organization/members/{member_id}/role",
            patch(org::set_role),
        )
        .route(
            "/api/neev/organization/members/{member_id}",
            delete(org::remove),
        )
        .route(
            "/api/neev/organization/invitations",
            post(org::send_invitation),
        )
        .route(
            "/api/neev/organization/invitations/{invite_id}",
            delete(org::revoke_invitation),
        )
        .route(
            "/api/neev/invitations/preview",
            post(org::preview_invitation),
        )
        .route("/api/neev/invitations/accept", post(org::accept_invitation))
        .route(
            "/api/neev/organization/join-code",
            get(org::view_join_code),
        )
        .route(
            "/api/neev/organization/join-code/rotate",
            post(org::rotate_join_code),
        )
        .route(
            "/api/neev/organization/join-code/enabled",
            post(org::set_join_code_enabled),
        )
        .route("/api/neev/join/preview", post(org::preview_join))
        .route("/api/neev/join", post(org::accept_join))
        .route("/api/neev/agent/run", post(agent::run))
        .route("/api/neev/agent/cancel", post(agent::cancel))
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
        .route("/auth/invite", get(static_files::page))
        .route("/neev-auth/confirm.js", get(static_files::asset))
        .route("/neev-auth/confirm.css", get(static_files::asset))
        .route("/neev-auth/invite.js", get(static_files::asset))
        .route("/neev-auth/invite.css", get(static_files::asset));
    if email_available {
        router = router
            .route("/api/neev/email/login", post(auth::email_login))
            .route("/api/neev/email/link", post(auth::email_link))
            .route("/api/neev/email/confirm", post(auth::email_confirm));
    }
    // Gateway browser exchange-issue is mounted only when the integration is
    // enabled (schema8 + `NEEV_GATEWAY_EVENTS_ENABLED`); otherwise it 404s.
    if state.config.gateway_events {
        router = router.route(
            "/api/neev/collaboration/issue",
            post(auth::collaboration_issue),
        );
    }
    router.fallback(static_files::fallback).with_state(state)
}

async fn health() -> Response {
    private_json(serde_json::json!({"status":"ok"}))
}

async fn capabilities(State(state): State<Arc<ServerState>>) -> Response {
    private_json(Capabilities {
        version: 1,
        identity_mode: "native",
        // Reflects an actually-bound runtime. Production is always false (the
        // config layer refuses `NEEV_RUNTIME_ENABLED`); a test/isolated harness
        // that injects a runtime reports true, which enables the chat control.
        runtime_enabled: state.agents.is_available(),
        providers: Providers {
            google: state.oauth.google_available(),
            email: state.magic.is_some(),
            github: github::available(),
        },
        catalog: CatalogCapabilities {
            node_kinds: ["project", "folder"],
            repository_defaults: false,
            chats: true,
            documents: true,
        },
    })
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
