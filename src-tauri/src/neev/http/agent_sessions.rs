//! HTTP surface for Conductor-style **Agent Chats** (spec §5).
//!
//! Sessions, their turn history, run streaming and per-user provider
//! connections. Everything here is scoped to the caller's own account by the
//! service layer — `agents::sessions` puts `owner_id` in the WHERE clause — so
//! these routes cannot be made to read another member's session even with a
//! guessed id.
//!
//! Streaming is deliberately split in two:
//! - `POST …/turns` starts the run and returns its durable `runId`;
//! - `GET /api/neev/agent/runs/{run_id}/events` is the ONLY stream.
//!
//! That means the reattach path is the normal path, not a rarely-exercised
//! fallback: the first view of a run and a view after a dropped connection run
//! exactly the same code, which is what §5.2 requires ("reattach + replay,
//! never silent re-run").

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::content::{canonical_uuid, safe_path, safe_query};
use super::{catalog, private_json, ApiError};
use crate::neev::agents::execution::RunRequest;
use crate::neev::agents::runs::{self, RunSignal};
use crate::neev::agents::sessions::{self, AgentSession, AgentTurn, SessionInput, TurnStatus};
use crate::neev::agents::{intents, types::Provider};
use crate::neev::content::types::Page;
use crate::neev::startup::ServerState;
use crate::neev::types::OrgContext;

/// A prompt is bounded by the turn's stored cap; the envelope allows a little
/// room for JSON overhead.
const TURN_BODY_LIMIT: usize = 40_960;
const SESSION_BODY_LIMIT: usize = 8_192;
/// How long a reattached reader will follow a run it cannot observe in-process
/// (another instance, or a run that settled during the handoff) before giving
/// up and reporting the stored state.
const POLL_ATTEMPTS: usize = 1_200;
const POLL_INTERVAL_MS: u64 = 500;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDto {
    id: String,
    org_id: i32,
    owner_id: i32,
    title: String,
    provider: String,
    model: String,
    bound_chat_id: Option<String>,
    archived: bool,
    revision: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnDto {
    id: String,
    session_id: String,
    sequence: String,
    run_id: String,
    prompt: String,
    response: String,
    status: &'static str,
    error_code: Option<String>,
    provider: String,
    model: String,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionsDto {
    sessions: Vec<SessionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnPageDto {
    items: Vec<TurnDto>,
    next_after: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartedTurnDto {
    turn: TurnDto,
    run_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDto {
    provider: &'static str,
    state: &'static str,
    account_label: Option<String>,
    checked_at: Option<i64>,
    error_code: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionsDto {
    /// Whether a real execution runtime is bound. When false every connection
    /// reads `disabled`, because no provider identity can be exercised at all —
    /// the UI must not show "Connected" over a runtime that cannot run.
    runtime_enabled: bool,
    connections: Vec<ConnectionDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSessionInput {
    title: String,
    provider: String,
    model: String,
    #[serde(default)]
    bound_chat_id: Option<String>,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateSessionInput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartTurnInput {
    prompt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListSessionsInput {
    #[serde(default)]
    archived: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TurnPageInput {
    #[serde(default)]
    after: Option<String>,
}

fn session_dto(session: AgentSession) -> SessionDto {
    SessionDto {
        id: session.id,
        org_id: session.org_id,
        owner_id: session.owner_id,
        title: session.title,
        provider: session.provider,
        model: session.model,
        bound_chat_id: session.bound_chat_id,
        archived: session.archived,
        revision: session.revision.to_string(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

fn turn_dto(turn: AgentTurn) -> TurnDto {
    TurnDto {
        id: turn.id,
        session_id: turn.session_id,
        sequence: turn.sequence.to_string(),
        run_id: turn.run_id,
        prompt: turn.prompt,
        response: turn.response,
        status: turn.status.name(),
        error_code: turn.error_code,
        provider: turn.provider,
        model: turn.model,
        created_at: turn.created_at,
        updated_at: turn.updated_at,
        completed_at: turn.completed_at,
    }
}

fn turn_page_dto(page: Page<AgentTurn>) -> TurnPageDto {
    TurnPageDto {
        items: page.items.into_iter().map(turn_dto).collect(),
        next_after: page.next_after,
    }
}

/// The connection-store provider for a stored session provider name.
fn provider_of(name: &str) -> Option<Provider> {
    match name {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        _ => None,
    }
}

/// A run id is our own dash-less uuid; reject anything else before it reaches a
/// query, so a path segment can never be used to probe storage.
fn canonical_run_id(value: &str) -> Result<(), ApiError> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::Invalid)
    }
}

pub(super) async fn list_sessions(
    State(state): State<Arc<ServerState>>,
    query: Result<Query<ListSessionsInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let input = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let include_archived = match input.archived.as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(ApiError::Invalid),
    };
    let sessions = sessions::list(&state, &actor, include_archived).await?;
    Ok(private_json(SessionsDto {
        sessions: sessions.into_iter().map(session_dto).collect(),
    }))
}

pub(super) async fn create_session(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<CreateSessionInput>(&state, request, SESSION_BODY_LIMIT)
            .await?;
    canonical_uuid(&input.request_id)?;
    if let Some(chat) = input.bound_chat_id.as_deref() {
        canonical_uuid(chat)?;
    }
    let created = sessions::create(
        &state,
        &actor,
        SessionInput {
            title: input.title,
            provider: input.provider,
            model: input.model,
            bound_chat_id: input.bound_chat_id,
            request_id: input.request_id,
        },
    )
    .await?;
    let mut response = private_json(session_dto(created));
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn get_session(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    Ok(private_json(session_dto(
        sessions::get(&state, &actor, &id).await?,
    )))
}

pub(super) async fn update_session(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<UpdateSessionInput>(&state, request, SESSION_BODY_LIMIT)
            .await?;
    let expected = catalog::canonical_revision(&input.expected_revision)?;
    // Exactly one field per request: a combined rename+archive would need two
    // revisions and could half-apply.
    let updated = match (input.title, input.archived) {
        (Some(title), None) => sessions::rename(&state, &actor, &id, &title, expected).await?,
        (None, Some(archived)) => {
            sessions::set_archived(&state, &actor, &id, archived, expected).await?
        }
        _ => return Err(ApiError::Invalid),
    };
    Ok(private_json(session_dto(updated)))
}

pub(super) async fn list_turns(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<TurnPageInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let page = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let after = page
        .after
        .as_deref()
        .map(catalog::canonical_revision)
        .transpose()
        .map_err(|_| ApiError::NotFound)?;
    Ok(private_json(turn_page_dto(
        sessions::turns(&state, &actor, &id, after).await?,
    )))
}

/// Start a turn. The prompt is persisted FIRST, so a refusal by the runtime is
/// recorded as a visible failed turn instead of a request that disappeared.
pub(super) async fn start_turn(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let session_id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&session_id)?;
    let (input, actor) =
        catalog::organization_mutation::<StartTurnInput>(&state, request, TURN_BODY_LIMIT).await?;
    let session = sessions::get(&state, &actor, &session_id).await?;
    // Context is gathered BEFORE the new turn exists so it cannot include
    // itself, and only completed turns contribute (§ `conversation_prompt`).
    let history = sessions::recent_turns(&state, &actor, &session_id).await?;
    // The INVOKER's own per-user credential (§5.4). Looked up from the actor's
    // connection row, never supplied by the request; `None` here makes the
    // sandboxed runtime refuse rather than fall back to a host identity.
    let credential_handle = match provider_of(&session.provider) {
        Some(provider) => intents::credential(&state, &actor, provider)
            .await
            .map_err(super::agent_error)?,
        None => None,
    };
    // A saved-session turn carries no request id, so this caller is always
    // its creator.
    let (turn, _created) = sessions::begin_turn(&state, &actor, &session_id, &input.prompt, None).await?;
    let composed = sessions::conversation_prompt(&history, &turn.prompt);
    let run_request = RunRequest {
        // Server-authoritative identity: never a client-supplied user/org.
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: session_id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        prompt: composed,
        credential_handle,
    };
    match state.agents.run(run_request).await {
        Ok(handle) => {
            runs::start(
                state.clone(),
                turn.run_id.clone(),
                actor.org_id,
                actor.identity.user_id,
                session_id,
                turn.id.clone(),
                handle,
                // A saved Agent Chat is private: no publication, ever.
                None,
            );
            let mut response = private_json(StartedTurnDto {
                run_id: turn.run_id.clone(),
                turn: turn_dto(turn),
            });
            *response.status_mut() = StatusCode::CREATED;
            Ok(response)
        }
        Err(error) => {
            // Honest refusal: settle the durable turn with the real reason and
            // return it, so the UI shows "Disabled" / "Busy — try again" rather
            // than a fake pending state or a fake queue position.
            let code = sessions::execution_error_code(&error);
            runs::settle_refusal(&state, &turn.run_id, code).await?;
            let settled = sessions::turn_by_run(&state, &actor, &turn.run_id).await?;
            let mut response = private_json(StartedTurnDto {
                run_id: settled.run_id.clone(),
                turn: turn_dto(settled),
            });
            *response.status_mut() = StatusCode::CREATED;
            Ok(response)
        }
    }
}

fn json_event(value: &serde_json::Value) -> Event {
    Event::default().data(value.to_string())
}

fn started_event(turn: &AgentTurn) -> Event {
    json_event(&serde_json::json!({
        "type": "started",
        "runId": turn.run_id,
        "turnId": turn.id,
        "sequence": turn.sequence.to_string(),
    }))
}

fn chunk_event(text: &str) -> Event {
    json_event(&serde_json::json!({ "type": "chunk", "text": text }))
}

fn finished_event(status: TurnStatus, error_code: Option<&str>) -> Event {
    json_event(&serde_json::json!({
        "type": "finished",
        "status": status.name(),
        "errorCode": error_code,
    }))
}

/// How long a stream may run before it is closed and the client must reattach.
/// A hard ceiling bounds every per-stream resource (task, channel, DB polling)
/// even if a client never disconnects.
const MAX_STREAM_SECS: u64 = 900;
/// How long an IDLE stream may go without re-proving authority. A producing
/// stream re-proves before every delivery; this bound exists so a revoked user
/// whose run happens to be quiet still has their socket closed promptly instead
/// of lingering until the hard lifetime cap.
const IDLE_RECHECK_SECS: u64 = 10;
/// Events buffered for a reader before the producer is made to wait. A BOUNDED
/// sink is the point: a slow or stalled reader applies backpressure instead of
/// growing an unbounded queue in the server.
const SSE_BUFFER: usize = 32;

type EventSink = futures::channel::mpsc::Sender<Result<Event, Infallible>>;

/// Continuously-revalidated authority for one stream.
///
/// A cached `OrgContext` proves only that the caller *was* authorized when the
/// stream opened. A long-lived stream must stop when the browser session is
/// revoked (logout), the membership is deactivated, or access to the session is
/// lost — so the cookie is re-exchanged for a fresh actor and the run's
/// ownership re-proved against THAT actor, never the stale one.
///
/// The check runs **before every delivery**, with no time-based cache: the cost
/// is bounded instead by coalescing — a burst of deltas is merged into one
/// frame and therefore costs one recheck, not one per token.
struct Authority {
    token: String,
    run_id: String,
}

impl Authority {
    /// Re-prove authority. `Err(())` the moment the caller is no longer
    /// entitled to this run, which ends the stream without delivering.
    async fn ensure(&mut self, state: &ServerState) -> Result<(), ()> {
        reauthorize(state, &self.token, &self.run_id).await.map(|_| ())
    }
}

/// Exchange a session cookie for a current actor and re-prove that the actor
/// may read this run. Both halves matter: the first catches logout/expiry, the
/// second catches losing access to the session itself.
async fn reauthorize(state: &ServerState, token: &str, run_id: &str) -> Result<OrgContext, ()> {
    let now = state.clock.now_unix();
    let actor = crate::neev::auth::session::organization(&state.database, token, now)
        .await
        .map_err(|_| ())?;
    sessions::turn_by_run(state, &actor, run_id)
        .await
        .map_err(|_| ())?;
    Ok(actor)
}

/// An SSE response carrying the same no-store/no-referrer headers as every
/// other private surface. Applied on EVERY branch — a replayed transcript is as
/// private as a live one.
fn private_sse(receiver: futures::channel::mpsc::Receiver<Result<Event, Infallible>>) -> Response {
    let mut response = Sse::new(receiver).into_response();
    super::add_private_headers(&mut response);
    response
}

/// Send one event, honouring backpressure. `Err` means the reader is gone.
async fn deliver(sender: &mut EventSink, event: Event) -> Result<(), ()> {
    use futures::SinkExt;
    sender.send(Ok(event)).await.map_err(|_| ())
}

/// Reattach to a run and replay it. Never starts or restarts execution: if the
/// run already finished, the stored final result is replayed; if it is gone and
/// unrecoverable, the stored terminal status (`interrupted`) is reported and an
/// explicit new turn is required.
pub(super) async fn run_events(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let run_id = safe_path(path)?;
    canonical_run_id(&run_id)?;
    // The session cookie is kept (not just the derived actor) so the stream can
    // re-exchange it later; a cached context cannot detect a logout.
    let names = super::cookies::CookieNames::for_preview(state.config.preview);
    let token = super::cookies::required(&headers, names.session)?.to_owned();
    // The permit is released at the end of this function ON PURPOSE: an SSE
    // stream can live for minutes and must not hold a global API slot for its
    // lifetime. Authorization is proved here, before the stream begins, and
    // re-proved on a bounded cadence for as long as it lasts.
    let actor = {
        let _permit = catalog::permit(&state)?;
        catalog::organization_get(&state, &headers).await?
    };
    let turn = sessions::turn_by_run(&state, &actor, &run_id).await?;
    let (mut sender, receiver) =
        futures::channel::mpsc::channel::<Result<Event, Infallible>>(SSE_BUFFER);
    let authority = Authority {
        token,
        run_id: run_id.clone(),
    };

    if turn.status != TurnStatus::Running {
        // Already settled: replay the stored result exactly once. Spawned so the
        // bounded sink can apply backpressure instead of being pre-filled.
        tokio::spawn(async move {
            if deliver(&mut sender, started_event(&turn)).await.is_err() {
                return;
            }
            if !turn.response.is_empty()
                && deliver(&mut sender, chunk_event(&turn.response))
                    .await
                    .is_err()
            {
                return;
            }
            let _ = deliver(
                &mut sender,
                finished_event(turn.status, turn.error_code.as_deref()),
            )
            .await;
        });
        return Ok(private_sse(receiver));
    }

    match runs::attach(&state.agent_runs, &run_id) {
        Some(attachment) => {
            // Ownership is re-proved against the live run, not just the row, so
            // a run registered by another tenant can never be streamed here.
            if attachment.run.org_id != actor.org_id
                || attachment.run.owner_id != actor.identity.user_id
            {
                return Err(ApiError::NotFound);
            }
            tokio::spawn(bounded_stream(
                state.clone(),
                run_id.clone(),
                sender,
                authority,
                StreamSource::Live(attachment),
            ));
        }
        // Not live in this process: either it settled during the handoff, or it
        // belongs to another instance. Follow the durable row instead of
        // guessing — and never re-run it.
        None => {
            tokio::spawn(bounded_stream(
                state.clone(),
                run_id.clone(),
                sender,
                authority,
                StreamSource::Stored {
                    emitted: turn.response.chars().count(),
                },
            ));
        }
    }
    Ok(private_sse(receiver))
}

enum StreamSource {
    Live(runs::Attachment),
    Stored { emitted: usize },
}

/// Run one stream under a hard lifetime cap. Whatever happens inside, the task
/// ends (and its channel closes) no later than `MAX_STREAM_SECS`, so a client
/// that never disconnects cannot pin a task, a subscription and a DB poller
/// indefinitely.
async fn bounded_stream(
    state: Arc<ServerState>,
    run_id: String,
    mut sender: EventSink,
    mut authority: Authority,
    source: StreamSource,
) {
    let work = async {
        match source {
            StreamSource::Live(attachment) => {
                follow_live(&state, &run_id, attachment, &mut sender, &mut authority).await
            }
            StreamSource::Stored { emitted } => {
                follow_stored(&state, &run_id, emitted, &mut sender, &mut authority).await
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(MAX_STREAM_SECS), work).await;
    // Dropping `sender` closes the response body on every exit path: terminal
    // event, reader gone, revoked authority, or the lifetime cap.
}

/// Splice the live stream onto the snapshot the attachment captured.
async fn follow_live(
    state: &Arc<ServerState>,
    run_id: &str,
    mut attachment: runs::Attachment,
    sender: &mut EventSink,
    authority: &mut Authority,
) {
    if authority.ensure(state).await.is_err() {
        return;
    }
    if deliver(sender, started_event_for(run_id, &attachment))
        .await
        .is_err()
    {
        return;
    }
    if !attachment.snapshot.is_empty()
        && deliver(sender, chunk_event(&attachment.snapshot))
            .await
            .is_err()
    {
        return;
    }
    let mut emitted = attachment.snapshot_chars;
    loop {
        // An IDLE stream still has to notice a revocation. The timeout turns a
        // quiet run into a periodic authority check rather than a socket that
        // lingers unauthenticated until the hard lifetime cap.
        let received = match tokio::time::timeout(
            std::time::Duration::from_secs(IDLE_RECHECK_SECS),
            attachment.events.recv(),
        )
        .await
        {
            Ok(received) => received,
            Err(_idle) => {
                if authority.ensure(state).await.is_err() {
                    return;
                }
                continue;
            }
        };
        match received {
            Ok(RunSignal::Delta(delta)) => {
                let mut text = splice(delta, &mut emitted);
                // Coalesce whatever is already queued into ONE frame, so
                // re-proving authority before every delivery costs one check
                // per burst rather than one per token.
                let mut finished = None;
                loop {
                    match attachment.events.try_recv() {
                        Ok(RunSignal::Delta(next)) => text.push_str(&splice(next, &mut emitted)),
                        Ok(RunSignal::Finished { status, error_code }) => {
                            finished = Some((status, error_code));
                            break;
                        }
                        Err(_) => break,
                    }
                }
                // Authority is re-proved BEFORE the content leaves the server,
                // so a revoked reader receives nothing further.
                if authority.ensure(state).await.is_err() {
                    return;
                }
                if !text.is_empty() && deliver(sender, chunk_event(&text)).await.is_err() {
                    return;
                }
                if let Some((status, error_code)) = finished {
                    let _ = deliver(sender, finished_event(status, error_code.as_deref())).await;
                    return;
                }
            }
            Ok(RunSignal::Finished { status, error_code }) => {
                if authority.ensure(state).await.is_err() {
                    return;
                }
                let _ = deliver(sender, finished_event(status, error_code.as_deref())).await;
                return;
            }
            // Lagged: content is never lost, it lives in the run's buffer.
            // Re-sync from it and carry on.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                let (snapshot, chars) = attachment.run.snapshot();
                if chars > emitted {
                    let tail: String = snapshot.chars().skip(emitted).collect();
                    if authority.ensure(state).await.is_err() {
                        return;
                    }
                    if deliver(sender, chunk_event(&tail)).await.is_err() {
                        return;
                    }
                    emitted = chars;
                }
            }
            // The run ended and its sender was dropped without us seeing the
            // terminal signal: read the durable row, which is written first.
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    finish_from_storage(state, run_id, emitted, sender, authority).await;
}

/// The portion of a delta not already delivered, advancing the emitted cursor.
/// A delta wholly inside what was already sent contributes nothing.
fn splice(delta: runs::RunDelta, emitted: &mut usize) -> String {
    let end = delta.offset + delta.text.chars().count();
    if end <= *emitted {
        return String::new();
    }
    let text: String = if delta.offset >= *emitted {
        delta.text
    } else {
        delta.text.chars().skip(*emitted - delta.offset).collect()
    };
    *emitted = end;
    text
}

fn started_event_for(run_id: &str, attachment: &runs::Attachment) -> Event {
    json_event(&serde_json::json!({
        "type": "started",
        "runId": run_id,
        "turnId": attachment.run.turn_id,
    }))
}

/// Follow a run this process does not own by polling its durable row. This is
/// the honest fallback: it reports what is stored and never re-executes.
async fn follow_stored(
    state: &Arc<ServerState>,
    run_id: &str,
    already_emitted: usize,
    sender: &mut EventSink,
    authority: &mut Authority,
) {
    let mut emitted = already_emitted;
    for _ in 0..POLL_ATTEMPTS {
        // Every poll re-derives the actor when stale, so this loop cannot keep
        // serving a session that has since been revoked.
        if authority.ensure(state).await.is_err() {
            return;
        }
        let turn = match sessions::turn_by_run_id(&state.database, run_id).await {
            Ok(Some(turn)) => turn,
            // The row vanished: stop rather than keep streaming.
            _ => return,
        };
        let chars = turn.response.chars().count();
        if chars > emitted {
            let tail: String = turn.response.chars().skip(emitted).collect();
            if deliver(sender, chunk_event(&tail)).await.is_err() {
                return;
            }
            emitted = chars;
        }
        if turn.status != TurnStatus::Running {
            let _ = deliver(
                sender,
                finished_event(turn.status, turn.error_code.as_deref()),
            )
            .await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    // The bound elapsed without a terminal state. Say so rather than hanging
    // the client on an open connection forever.
    let _ = deliver(
        sender,
        finished_event(TurnStatus::Failed, Some("unavailable")),
    )
    .await;
}

/// Emit the terminal event from the durable row (the driver writes it before
/// deregistering, so it is authoritative by the time we get here).
async fn finish_from_storage(
    state: &Arc<ServerState>,
    run_id: &str,
    emitted: usize,
    sender: &mut EventSink,
    authority: &mut Authority,
) {
    if authority.ensure(state).await.is_err() {
        return;
    }
    match sessions::turn_by_run_id(&state.database, run_id).await {
        Ok(Some(turn)) => {
            let chars = turn.response.chars().count();
            if chars > emitted {
                let tail: String = turn.response.chars().skip(emitted).collect();
                if deliver(sender, chunk_event(&tail)).await.is_err() {
                    return;
                }
            }
            let _ = deliver(
                sender,
                finished_event(turn.status, turn.error_code.as_deref()),
            )
            .await;
        }
        // No durable row to speak for the run: report an interruption rather
        // than an unqualified success.
        _ => {
            let _ = deliver(
                sender,
                finished_event(TurnStatus::Failed, Some("interrupted")),
            )
            .await;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamInvokeInput {
    prompt: String,
    provider: String,
    model: String,
    request_id: String,
    expected_auth_revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamInvokeDto {
    /// The invoker's prompt, already persisted as a normal human message so the
    /// team can see what was asked even before the reply lands.
    message: super::content::MessageDto,
    /// The durable turn holding this invocation, and the run id to stream from
    /// `GET /api/neev/agent/runs/{run_id}/events` — the SAME stream a saved
    /// session uses, so there is one streaming implementation, not two.
    turn: TurnDto,
    run_id: String,
}

/// **Explicit** team invocation (spec §5.3).
///
/// Two things are published, in this order and nothing else:
/// 1. the invoker's prompt, as an ordinary human message (idempotent on
///    `requestId`, guarded by `expectedAuthRevision` exactly like posting);
/// 2. when the run completes, the reply as an **agent-authored** message
///    (`authorKind: "agent"`, `agentLabel: "<provider>"`, `authorId` = the
///    invoker), written by the run driver.
///
/// The run executes under the INVOKER's own per-user credentials (§5.4), and it
/// is sent **only the explicit prompt** — never the user's private Agent Chat
/// history. That is structural, not a convention: the turn is created in a
/// `team`-kind session dedicated to this chat, and the prompt composer that
/// replays history is not on this path at all.
pub(super) async fn team_invoke(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let chat_id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&chat_id)?;
    let (input, actor) =
        catalog::organization_mutation::<TeamInvokeInput>(&state, request, TURN_BODY_LIMIT).await?;
    canonical_uuid(&input.request_id)?;
    let expected = catalog::canonical_revision(&input.expected_auth_revision)?;
    let provider = provider_of(&input.provider).ok_or(ApiError::Invalid)?;

    // 1. The ask becomes team-visible first. This also proves write access to
    //    the chat and applies the org's message quota, so an unauthorized or
    //    over-quota invocation never reaches the runtime.
    let prompt_message = crate::neev::content::messages::append(
        &state,
        &actor,
        &chat_id,
        &input.request_id,
        expected,
        &input.prompt,
    )
    .await?;

    // 2. The invoker's own durable holder for this chat + provider.
    let session = sessions::ensure_team_session(
        &state,
        &actor,
        &chat_id,
        &input.provider,
        &input.model,
        "Team invocation",
    )
    .await?;
    // The credential is resolved BEFORE the turn exists. Anything that can fail
    // between creating a `running` turn and handing it to the runtime would
    // otherwise strand that turn, blocking every later invocation in this chat.
    let credential_handle = intents::credential(&state, &actor, provider)
        .await
        .map_err(super::agent_error)?;
    // `requestId` makes the whole invocation idempotent — a retry (racing or
    // long after the fact) resolves to the SAME turn and run, so the provider
    // never executes one ask twice and no duplicate reply can be published.
    let (turn, created) = sessions::begin_turn(
        &state,
        &actor,
        &session.id,
        &input.prompt,
        Some(&input.request_id),
    )
    .await?;
    // ONLY the caller that created the turn executes it.
    //
    // This is decided inside `begin_turn`'s transaction, arbitrated by the
    // unique `(org_id, session_id, request_id)` index — not by looking the run
    // up in the registry. A registry check here would be a race: two truly
    // simultaneous retries can both miss it while the first is still awaiting
    // the runtime, and the loser could then start a second provider execution
    // (or settle the shared turn as `busy` while the winner is still running).
    if !created {
        let mut response = private_json(TeamInvokeDto {
            message: super::content::message_dto(prompt_message)?,
            run_id: turn.run_id.clone(),
            turn: turn_dto(turn),
        });
        *response.status_mut() = StatusCode::CREATED;
        return Ok(response);
    }
    let publication = runs::TeamPublication {
        chat_id: chat_id.clone(),
        invoker: actor.clone(),
        agent_label: input.provider.clone(),
        turn_id: turn.id.clone(),
    };
    let run_request = RunRequest {
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: chat_id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        // ONLY the explicit prompt. No private session history is composed in.
        prompt: turn.prompt.clone(),
        credential_handle,
    };
    let turn = match state.agents.run(run_request).await {
        Ok(handle) => {
            runs::start(
                state.clone(),
                turn.run_id.clone(),
                actor.org_id,
                actor.identity.user_id,
                session.id.clone(),
                turn.id.clone(),
                handle,
                Some(publication),
            );
            turn
        }
        Err(error) => {
            // Honest refusal: the ask stays visible to the team, the turn
            // records why no reply came, and nothing agent-authored is posted.
            let code = sessions::execution_error_code(&error);
            runs::settle_refusal(&state, &turn.run_id, code).await?;
            sessions::turn_by_run(&state, &actor, &turn.run_id).await?
        }
    };
    let mut response = private_json(TeamInvokeDto {
        message: super::content::message_dto(prompt_message)?,
        run_id: turn.run_id.clone(),
        turn: turn_dto(turn),
    });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelInput {
    run_id: String,
}

/// Cancel a live run. Ownership is checked against the durable turn, so a run
/// id alone is not authority.
pub(super) async fn cancel_run(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<CancelInput>(&state, request, 1_024).await?;
    canonical_run_id(&input.run_id)?;
    let turn = sessions::turn_by_run(&state, &actor, &input.run_id).await?;
    let canceled = match runs::find(&state.agent_runs, &input.run_id) {
        Some(run) if run.org_id == actor.org_id && run.owner_id == actor.identity.user_id => {
            run.cancel();
            true
        }
        // Not live here. If the row is still `running` it belongs to another
        // instance; report honestly rather than claiming a cancel we did not
        // perform.
        _ => false,
    };
    Ok(private_json(serde_json::json!({
        "canceled": canceled,
        "status": turn.status.name(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectInput {
    /// Caller-chosen idempotency key, exactly like every other native
    /// mutation: replaying it returns the SAME attempt instead of starting a
    /// second sign-in.
    request_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelConnectInput {
    attempt_id: String,
    request_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectStartedDto {
    /// `pending` — a sign-in is in progress and `attemptId` identifies it;
    /// `settled` — the connection was already in its final state and
    /// `connection` carries it.
    outcome: &'static str,
    attempt_id: Option<String>,
    expires_at: Option<i64>,
    connection: Option<ConnectionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptDto {
    state: String,
    expires_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptDto {
    operation_id: String,
    generation: String,
}

/// The provider named by a path segment, or `NotFound` — an unknown provider
/// must not be distinguishable from one the caller has no connection for.
fn path_provider(name: &str) -> Result<Provider, ApiError> {
    provider_of(name).ok_or(ApiError::NotFound)
}

/// `POST /api/neev/agent/connections/{provider}/connect` — begin a per-user
/// provider sign-in (§5.4).
///
/// This records the intent and returns an attempt to poll. It deliberately
/// exposes NONE of the login ticket's server-side fields (operation id, lease
/// owner): those authorize completion and belong only to the supervisor that
/// performs the login, never to a browser.
///
/// **Completion requires an effect executor that this build does not bind.**
/// The attempt therefore stays `pending` until it expires (or the caller
/// cancels), and the connection stays `connecting`. That is reported honestly
/// rather than being dressed up as success — see the handoff entry for the
/// services still required.
pub(super) async fn connect(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let provider = path_provider(&safe_path(path)?)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<ConnectInput>(&state, request, SESSION_BODY_LIMIT).await?;
    let outcome = intents::begin(&state, &actor, provider, &input.request_key)
        .await
        .map_err(super::agent_error)?;
    let dto = match outcome {
        crate::neev::agents::types::BeginOutcome::Pending(ticket) => ConnectStartedDto {
            outcome: "pending",
            attempt_id: Some(ticket.attempt_id),
            expires_at: Some(ticket.expires_at),
            connection: None,
        },
        crate::neev::agents::types::BeginOutcome::Settled(view) => ConnectStartedDto {
            outcome: "settled",
            attempt_id: None,
            expires_at: None,
            connection: Some(connection_dto(view, state.agents.is_available())),
        },
    };
    let mut response = private_json(dto);
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

/// `GET /api/neev/agent/connections/{provider}/attempts/{attempt_id}` — poll a
/// sign-in. Visible only to the browser session that started it.
pub(super) async fn connect_attempt(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, String)>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (provider, attempt_id) = safe_path(path)?;
    let provider = path_provider(&provider)?;
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let attempt = intents::attempt(&state, &actor, provider, &attempt_id)
        .await
        .map_err(super::agent_error)?;
    Ok(private_json(AttemptDto {
        state: attempt.state,
        expires_at: attempt.expires_at,
    }))
}

/// `POST /api/neev/agent/connections/{provider}/cancel` — abandon an in-flight
/// sign-in. Queues the same cleanup path a crash-recovered attempt uses.
pub(super) async fn cancel_connect(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let provider = path_provider(&safe_path(path)?)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<CancelConnectInput>(&state, request, SESSION_BODY_LIMIT)
            .await?;
    let receipt = intents::cancel(
        &state,
        &actor,
        provider,
        &input.attempt_id,
        &input.request_key,
    )
    .await
    .map_err(super::agent_error)?;
    Ok(private_json(ReceiptDto {
        operation_id: receipt.operation_id,
        generation: receipt.generation.to_string(),
    }))
}

/// `POST /api/neev/agent/connections/{provider}/disconnect` — sign this user
/// out of this provider. The credential teardown itself is a queued cleanup.
pub(super) async fn disconnect(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let provider = path_provider(&safe_path(path)?)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<ConnectInput>(&state, request, SESSION_BODY_LIMIT).await?;
    let receipt = intents::disconnect(&state, &actor, provider, &input.request_key)
        .await
        .map_err(super::agent_error)?;
    Ok(private_json(ReceiptDto {
        operation_id: receipt.operation_id,
        generation: receipt.generation.to_string(),
    }))
}

fn connection_dto(
    view: crate::neev::agents::types::ConnectionView,
    runtime_enabled: bool,
) -> ConnectionDto {
    ConnectionDto {
        provider: match view.provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        },
        // With no runtime bound, no connection can be exercised: report
        // `disabled` rather than a "Connected" the user cannot use.
        state: if runtime_enabled {
            connection_state(view.state)
        } else {
            "disabled"
        },
        account_label: view.account_label,
        checked_at: view.checked_at,
        error_code: view.error_code,
    }
}

fn connection_state(state: crate::neev::agents::types::ConnectionState) -> &'static str {
    use crate::neev::agents::types::ConnectionState as S;
    match state {
        // "Disconnected" in storage means no provider identity has been
        // established for this user — the honest user-facing state is
        // "not signed in", not a neutral "disconnected".
        S::Disconnected => "not_signed_in",
        S::Connecting => "connecting",
        S::Connected => "connected",
        S::ReconnectRequired => "reconnect_required",
        S::Disconnecting => "disconnecting",
        S::Unavailable => "unavailable",
    }
}

/// The caller's own per-user, per-provider connections (§5.4). Credentials are
/// never returned — only the state, an account label and the last check time.
pub(super) async fn list_connections(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let runtime_enabled = state.agents.is_available();
    let views = intents::list(&state, &actor)
        .await
        .map_err(super::agent_error)?;
    Ok(private_json(ConnectionsDto {
        runtime_enabled,
        connections: views
            .into_iter()
            .map(|view| connection_dto(view, runtime_enabled))
            .collect(),
    }))
}

#[cfg(test)]
#[path = "agent_sessions_tests.rs"]
mod agent_sessions_tests;
