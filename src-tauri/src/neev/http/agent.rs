//! Native chat agent endpoints: run a personal agent (streamed over SSE) and
//! cancel/restart it, user-scoped by the caller's native session. Execution goes
//! through `state.agents` — in any bootable server that is `Disabled` (the config
//! layer refuses `NEEV_RUNTIME_ENABLED`), so these routes exist but return
//! "disabled" until an isolated runtime is bound. Cancel/restart use the shared
//! `state.agent_jobs` registry so a separate request can interrupt an in-flight
//! run (restart = cancel + a fresh run).

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::{cookies, ApiError};
use crate::neev::agents::execution::{ExecutionError, RunEvent, RunRequest};
use crate::neev::agents::library_runtime::LibraryRunItem;
use crate::neev::auth::session;
use crate::neev::startup::ServerState;
use crate::neev::types::OrgContext;

#[derive(Deserialize)]
struct RunPayload {
    chat_id: String,
    prompt: String,
    provider: String,
    model: String,
}

#[derive(Deserialize)]
struct CancelPayload {
    run_id: String,
}

/// Authenticate a native org session (Origin + CSRF + scoped session), returning
/// the server-authoritative actor. Identical policy to `collaboration_issue`.
async fn authorize(
    state: &ServerState,
    parts: &axum::http::request::Parts,
) -> Result<OrgContext, ApiError> {
    if !cookies::exact_origin(&parts.headers, &state.config.origin_header) {
        return Err(ApiError::Forbidden);
    }
    if !cookies::is_json(&parts.headers) {
        return Err(ApiError::Invalid);
    }
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&parts.headers, names.session)?.to_owned();
    let cookie_csrf = cookies::required(&parts.headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&parts.headers)? {
        return Err(ApiError::Unauthorized);
    }
    let now = state.clock.now_unix();
    session::view(&state.database, &token, Some(cookie_csrf), now).await?;
    Ok(session::organization(&state.database, &token, now).await?)
}

/// `None` for an event this legacy stream does not carry.
///
/// The one-shot run stream is text only. Tool activity is a property of a
/// durable TURN — numbered and stored per turn so a reload can reproduce it —
/// so it is served by the session stream rather than reinvented here without
/// any of that.
fn event_json(event: &RunEvent) -> Option<String> {
    let quote = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
    Some(match event {
        RunEvent::Started(id) => format!("{{\"type\":\"started\",\"run_id\":{}}}", quote(id)),
        // Both go out as `chunk`. This stream has no accumulator to hold a
        // seam against — it is one-shot text with nothing stored — so the
        // distinction has nowhere to be applied here, and collapsing it keeps
        // this endpoint's wire format exactly as it was.
        RunEvent::Chunk(text) | RunEvent::Message(text) => {
            format!("{{\"type\":\"chunk\",\"text\":{}}}", quote(text))
        }
        RunEvent::Activity(_) => return None,
        RunEvent::Completed => "{\"type\":\"completed\"}".into(),
        RunEvent::Failed(reason) => format!("{{\"type\":\"failed\",\"reason\":{}}}", quote(reason)),
    })
}

/// POST `/api/neev/agent/run` — start an author-scoped run, stream events as SSE.
pub(super) async fn run(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state.auth_limit.try_acquire().map_err(|_| ApiError::Limited)?;
    let (parts, body) = request.into_parts();
    let context = authorize(&state, &parts).await?;
    let raw = axum::body::to_bytes(body, 16_384)
        .await
        .map_err(|_| ApiError::Invalid)?;
    let payload: RunPayload = serde_json::from_slice(&raw).map_err(|_| ApiError::Invalid)?;
    if payload.prompt.trim().is_empty() {
        return Err(ApiError::Invalid);
    }
    // Per-chat authorization BEFORE any launch: `resources::get` proves the chat
    // exists in the actor's org and this user may access it (private chats only by
    // their creator), and rechecks the session. Cross-org / cross-user / unknown
    // chats fail here (mapped to NotFound), so no agent is spawned on denial.
    let chat = crate::neev::content::resources::get(&state, &context, &payload.chat_id).await?;
    if chat.kind != crate::neev::content::types::ResourceKind::Chat {
        return Err(ApiError::Invalid);
    }
    let user_id = context.identity.user_id;
    // The invoker's OWN per-user credential for the requested provider (§5.4);
    // an unknown provider resolves to `None`, which the sandboxed runtime
    // refuses rather than launching under a host identity.
    // An ACP provider that is not gated on resolves to `None` like an unknown one.
    let credential_handle =
        crate::neev::agents::types::Provider::parse_offered(payload.provider.as_str());
    let credential_handle = match credential_handle {
        Some(provider) => crate::neev::agents::intents::credential(&state, &context, provider)
            .await
            .map_err(super::agent_error)?,
        None => None,
    };
    // Load library items for the org.
    let library_items = load_library_items(&state, &context).await?;

    let request = RunRequest {
        // Server-authoritative identity: never trust a client-supplied user/org.
        user_id,
        org_id: context.org_id,
        chat_id: payload.chat_id,
        provider: payload.provider,
        model: payload.model,
        prompt: payload.prompt,
        credential_handle,
        // The legacy one-shot route carries no session and no placement, so
        // there is no project to brief from. Left unbriefed rather than given
        // a half-context that would describe the wrong thing.
        briefing: None,
        effort: None,
        fast: false,
        // The legacy one-shot route carries no attachments.
        attachments: Vec::new(),
        // Nor any conversation: it has no session and no thread to be the
        // continuation of, so there is no transcript to stage.
        context_files: Vec::new(),
        // Shared org library items (skills and MCP configs).
        library_items,
        min_free_lanes: 1,
    };
    let mut handle = match state.agents.run(request).await {
        Ok(handle) => handle,
        Err(ExecutionError::Disabled) => {
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                "{\"error\":\"agent_runtime_disabled\"}",
            )
                .into_response())
        }
        Err(_) => return Err(ApiError::Unavailable),
    };
    let run_id = handle.run_id.clone();
    if let Some(canceler) = handle.take_canceler() {
        if let Ok(mut jobs) = state.agent_jobs.lock() {
            jobs.insert(run_id.clone(), (user_id, canceler));
        }
    }
    let stream = futures::stream::unfold(
        (handle, state.clone(), run_id, false),
        |(mut handle, state, run_id, ended)| async move {
            if ended {
                return None;
            }
            match handle.next().await {
                Some(event) => {
                    let done = matches!(event, RunEvent::Completed | RunEvent::Failed(_));
                    if done {
                        if let Ok(mut jobs) = state.agent_jobs.lock() {
                            jobs.remove(&run_id);
                        }
                    }
                    // An event this stream does not carry becomes a comment
                    // frame: the stream stays alive and well-formed, and a
                    // client parsing `data:` as JSON never sees an empty body.
                    let sse = match event_json(&event) {
                        Some(json) => Event::default().data(json),
                        None => Event::default().comment("skip"),
                    };
                    Some((Ok::<Event, Infallible>(sse), (handle, state, run_id, done)))
                }
                None => {
                    if let Ok(mut jobs) = state.agent_jobs.lock() {
                        jobs.remove(&run_id);
                    }
                    None
                }
            }
        },
    );
    Ok(Sse::new(stream).into_response())
}

/// POST `/api/neev/agent/cancel` — cancel an in-flight run by id (restart is a
/// cancel followed by a fresh `run`). Idempotent.
pub(super) async fn cancel(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state.auth_limit.try_acquire().map_err(|_| ApiError::Limited)?;
    let (parts, body) = request.into_parts();
    let context = authorize(&state, &parts).await?;
    let user_id = context.identity.user_id;
    let raw = axum::body::to_bytes(body, 4_096)
        .await
        .map_err(|_| ApiError::Invalid)?;
    let payload: CancelPayload = serde_json::from_slice(&raw).map_err(|_| ApiError::Invalid)?;
    // Ownership: only the user who started the run may cancel it.
    let canceled = state
        .agent_jobs
        .lock()
        .ok()
        .and_then(|mut jobs| match jobs.get(&payload.run_id) {
            Some((owner, _)) if *owner == user_id => {
                jobs.remove(&payload.run_id).map(|(_, tx)| tx.send(()).is_ok())
            }
            _ => None,
        })
        .unwrap_or(false);
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        format!("{{\"canceled\":{canceled}}}"),
    )
        .into_response())
}

/// Load enabled library items for the caller's org.
async fn load_library_items(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<LibraryRunItem>, ApiError> {
    let items = crate::neev::library::load_enabled(state, actor)
        .await
        .map_err(|_| ApiError::Unavailable)?;

    Ok(items.into_iter().map(LibraryRunItem::from).collect())
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;
