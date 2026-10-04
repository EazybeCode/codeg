use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post},
    Json, Router,
};
use serde::Serialize;

use crate::neev::{auth::github, error::AuthError, startup::ServerState};

mod agent;
mod agent_sessions;
mod auth;
pub(crate) mod catalog;
mod catalog_trash;
mod content;
#[cfg(test)]
mod content_tests;
pub(crate) mod cookies;
mod library;
mod org;
mod realtime;
mod knowledge;
#[cfg(test)]
mod knowledge_tests;
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

/// Map the agent control plane's error taxonomy onto the HTTP one. `Unavailable`
/// is the honest answer for both a storage problem and a refused provider
/// operation — neither is something the caller can fix by retrying differently.
pub(super) fn agent_error(value: crate::neev::agents::types::AgentError) -> ApiError {
    use crate::neev::agents::types::AgentError;
    match value {
        AgentError::Auth(error) => ApiError::from(error),
        AgentError::NotFound => ApiError::NotFound,
        AgentError::InvalidInput => ApiError::Invalid,
        AgentError::Conflict => ApiError::Conflict,
        AgentError::RateLimited => ApiError::Limited,
        AgentError::Unavailable => ApiError::Unavailable,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    version: u8,
    identity_mode: &'static str,
    runtime_enabled: bool,
    /// True when team messages are pushed (SSE). False is the honest signal that
    /// the client must fall back to polling; it never implies an access change.
    realtime: bool,
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
            "/api/neev/nodes/{node_id}/trash-preview",
            get(catalog_trash::preview),
        )
        .route("/api/neev/nodes/{node_id}/trash", post(catalog_trash::trash))
        .route("/api/neev/nodes/{node_id}/restore", post(catalog_trash::restore))
        .route("/api/neev/trash/nodes", get(catalog_trash::list))
        .route(
            "/api/neev/nodes/{node_id}/sharing",
            get(content::get_node_sharing).patch(content::update_node_sharing),
        )
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
        .route(
            "/api/neev/knowledge/settings",
            get(knowledge::get_settings).put(knowledge::put_settings),
        )
        .route("/api/neev/knowledge/status", get(knowledge::get_status))
        .route("/api/neev/knowledge/sources", get(knowledge::get_sources))
        .route("/api/neev/knowledge/jobs", post(knowledge::start_job))
        .route("/api/neev/knowledge/jobs/current", get(knowledge::current_job))
        .route(
            "/api/neev/knowledge/jobs/{id}/{action}",
            post(knowledge::control_job),
        )
        .route("/api/neev/knowledge/pages", get(knowledge::get_pages))
        .route("/api/neev/knowledge/pages/{id}", get(knowledge::get_page))
        .route("/api/neev/knowledge/index", get(knowledge::get_index))
        .route("/api/neev/knowledge/log", get(knowledge::get_log))
        .route("/api/neev/knowledge/export", get(knowledge::get_export))
        .route(
            "/api/neev/knowledge/uncovered/reset",
            post(knowledge::reset_uncovered),
        )
        .route("/api/neev/knowledge/ask", post(knowledge::start_ask))
        .route("/api/neev/knowledge/ask/{id}", get(knowledge::get_ask))
        .route("/api/neev/knowledge/ask/{id}/stop", post(knowledge::stop_ask))
        .route("/api/neev/mentions", get(content::list_mentions))
        .route("/api/neev/mentions/read", post(content::read_mentions))
        .route(
            "/api/neev/chats/{id}/attachments",
            post(content::upload_chat_attachment).get(content::list_chat_archived_attachments),
        )
        .route(
            "/api/neev/agent/sessions/{id}/attachments",
            post(content::upload_session_attachment)
                .get(content::list_session_archived_attachments),
        )
        .route(
            "/api/neev/attachments/{key}",
            get(content::serve_attachment).patch(content::archive_attachment),
        )
        .route(
            "/api/neev/attachments",
            get(content::list_org_archived_attachments),
        )
        .route("/api/neev/library", get(library::list))
        .route("/api/neev/library/import", post(library::import))
        .route(
            "/api/neev/library/{id}/secrets",
            axum::routing::put(library::update_secrets),
        )
        .route(
            "/api/neev/library/{id}/test",
            post(library::test_integration),
        )
        .route(
            "/api/neev/library/{id}",
            patch(library::update).delete(library::delete),
        )
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
        .route("/api/neev/organization/join-code", get(org::view_join_code))
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
        .route(
            "/api/neev/agent/routing",
            get(agent_sessions::routing_status),
        )
        .route(
            "/api/neev/agent/route",
            post(agent_sessions::route_new_chat),
        )
        .route("/api/neev/agent/cancel", post(agent::cancel))
        // Conductor-style agent sessions (spec §5). The one-shot `run`/`cancel`
        // routes above stay as the ad-hoc team-chat panel path; these own the
        // durable, resumable session surface.
        .route(
            "/api/neev/agent/sessions",
            get(agent_sessions::list_sessions).post(agent_sessions::create_session),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}",
            get(agent_sessions::get_session).patch(agent_sessions::update_session),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}/node",
            patch(agent_sessions::move_session),
        )
        .route(
            "/api/neev/agent/capabilities",
            get(agent_sessions::capabilities),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}/settings",
            patch(agent_sessions::set_settings),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}/effort",
            patch(agent_sessions::set_effort),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}/speed",
            patch(agent_sessions::set_speed),
        )
        .route(
            "/api/neev/agent/sessions/{session_id}/turns",
            get(agent_sessions::list_turns).post(agent_sessions::start_turn),
        )
        .route(
            "/api/neev/agent/runs/{run_id}/events",
            get(agent_sessions::run_events),
        )
        .route(
            "/api/neev/agent/runs/cancel",
            post(agent_sessions::cancel_run),
        )
        .route(
            "/api/neev/agent/runs/{run_id}/steer",
            post(agent_sessions::steer_run),
        )
        // Re-run a failed turn's own stored ask; posts nothing new to a chat.
        .route(
            "/api/neev/agent/runs/{run_id}/retry",
            post(agent_sessions::retry_run),
        )
        // Explicit team invocation (§5.3): posts the ask, then publishes an
        // agent-authored reply into the same chat when the run completes.
        .route(
            "/api/neev/chats/{chat_id}/agent",
            get(agent_sessions::team_session).post(agent_sessions::team_invoke),
        )
        .route(
            "/api/neev/agent/connections",
            get(agent_sessions::list_connections),
        )
        // Per-user provider sign-in (§5.4). These are the state-machine half;
        // completing a sign-in additionally requires the effect executor that
        // performs the login inside the per-user isolation profile.
        .route(
            "/api/neev/agent/connections/{provider}/connect",
            post(agent_sessions::connect),
        )
        .route(
            "/api/neev/agent/connections/{provider}/attempts/{attempt_id}",
            get(agent_sessions::connect_attempt),
        )
        .route(
            "/api/neev/agent/connections/{provider}/attempts/{attempt_id}/code",
            post(agent_sessions::submit_attempt_code),
        )
        .route(
            "/api/neev/agent/connections/{provider}/attempts/{attempt_id}/callback",
            post(agent_sessions::submit_attempt_callback),
        )
        .route(
            "/api/neev/agent/connections/{provider}/cancel",
            post(agent_sessions::cancel_connect),
        )
        .route(
            "/api/neev/agent/connections/{provider}/disconnect",
            post(agent_sessions::disconnect),
        )
        .route(
            "/api/neev/agent/connections/{provider}/test",
            post(agent_sessions::test_connection),
        )
        .route("/api/neev/logout", post(auth::logout))
        .route("/api/neev/auth/google/login", post(auth::google_login))
        .route("/api/neev/auth/google/link", post(auth::google_link))
        .route("/auth/google/callback", get(auth::google_callback))
        .route("/", get(static_files::page))
        .route("/login", get(static_files::page))
        .route("/onboarding", get(static_files::page))
        .route("/workspace", get(static_files::page))
        .route("/settings", get(static_files::page))
        // Direct sign-in return page: the extension redirects the provider's
        // loopback callback here, with the query in the fragment.
        .route("/agents/signin", get(static_files::page))
        .route("/auth/email/confirm", get(static_files::page))
        .route("/auth/invite", get(static_files::page))
        .route("/neev-auth/confirm.js", get(static_files::asset))
        .route("/neev-auth/confirm.css", get(static_files::asset))
        .route("/neev-auth/invite.js", get(static_files::asset))
        .route("/neev-auth/invite.css", get(static_files::asset));

    // The document engine owns one prefix and is mounted only when configured.
    // Unmounted, `/document-engine/*` is simply not a route, so an engine that
    // is off cannot be probed for what it would have served. Caddy already
    // proxies every app path to this server, so no vhost change is needed.
    if state.engine.is_some() {
        router = router.route(
            "/document-engine/{*rest}",
            any(crate::neev::engine::proxy::handle),
        );
        router = router.route("/document-engine", any(crate::neev::engine::proxy::handle));
    }
    if email_available {
        router = router
            .route("/api/neev/email/login", post(auth::email_login))
            .route("/api/neev/email/link", post(auth::email_link))
            .route("/api/neev/email/confirm", post(auth::email_confirm));
    }
    // Gateway browser exchange-issue is mounted only when the integration is
    // enabled (schema8 + `NEEV_GATEWAY_EVENTS_ENABLED`); otherwise it 404s.
    // Phase-2 push delivery. Rolled back by `NEEV_REALTIME_DISABLED=true`: the
    // route disappears, the capability reports false, clients poll. No data change.
    if state.config.realtime_enabled {
        router = router.route("/api/neev/chats/{id}/stream", get(realtime::stream));
    }
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
        realtime: state.config.realtime_enabled,
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

pub(super) fn private_json(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    add_private_headers(&mut response);
    response
}

/// `Cache-Control` for a file whose NAME already identifies its bytes.
///
/// Both static trees Neev serves — its own export and the document engine's —
/// name their chunks after a build id or a content hash, so a given URL can
/// never answer with different bytes than it did last time. A new build means
/// new names, which is what makes a year-long lifetime safe rather than
/// reckless: there is nothing to invalidate.
///
/// Only ever applied to those content-addressed prefixes. HTML shells,
/// navigation payloads and icons keep stable names across builds and must keep
/// revalidating, or a deploy would leave readers pinned to the old app.
pub(crate) const IMMUTABLE_ASSET: &str = "private, max-age=31536000, immutable";

pub(super) fn add_private_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}
