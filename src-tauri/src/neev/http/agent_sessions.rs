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

use super::content::{
    attachment_dtos, canonical_uuid, requested_keys, safe_path, safe_query, sent_text,
    AttachmentDto,
};
use super::{catalog, private_json, ApiError};
use crate::neev::agents::execution::{RunAttachment, RunRequest};
use crate::neev::agents::runs::{self, RunSignal};
use crate::neev::agents::sessions::{self, AgentSession, AgentTurn, SessionInput, TurnStatus};
use crate::neev::agents::{intents, supervisor, types::Provider};
use crate::neev::content::attachment::{self, Owner, StoredAttachment};
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
    /// The session's reasoning effort, or `null` for the provider's own default.
    effort: Option<String>,
    /// Whether it runs in the provider's fast mode. Always present; never
    /// inherited, so `false` is the answer for every session that predates it.
    fast: bool,
    bound_chat_id: Option<String>,
    /// Where this session sits in the workspace tree; `null` when unfiled.
    ///
    /// Serialised as a JSON number rather than the string treatment `revision`
    /// gets, because a node id is a plain tree key the client compares for
    /// equality, not a value it echoes back as an opaque token.
    node_id: Option<i32>,
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
    /// The turn's recorded tool trace. Always present, `[]` when the turn used
    /// no tools — a client seeding from this must not have to tell "no trace"
    /// apart from "field missing".
    activity: Vec<crate::neev::agents::activity::ActivityItem>,
    /// The files this turn was started with, in send order. Always present,
    /// `[]` when none — same reason as `activity`.
    attachments: Vec<AttachmentDto>,
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
    /// Optional placement. `#[serde(default)]` so a client that predates the
    /// field still posts a valid body and gets an unfiled session.
    #[serde(default)]
    node_id: Option<i32>,
    /// Optional reasoning effort. `#[serde(default)]` so an older client still
    /// posts a valid body and gets the provider's own default.
    #[serde(default)]
    effort: Option<String>,
    /// Fast mode. `#[serde(default)]` so an older client posts a valid body and
    /// gets the safe answer: off.
    #[serde(default)]
    fast: bool,
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

/// Body of the re-file request.
///
/// `node_id` is deliberately NOT `#[serde(default)]`: it must be present, and it
/// may be `null`. Folding this into `UpdateSessionInput` instead would make
/// "field omitted" and "explicitly null" indistinguishable, and unfiling a
/// session is exactly the second one.
/// Body of the effort change.
///
/// `effort` is NOT `#[serde(default)]`: it must be present, and may be `null`.
/// Omitted and explicitly-null would otherwise be indistinguishable, and
/// clearing a level back to the provider default is exactly the second one.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EffortInput {
    effort: Option<String>,
    expected_revision: String,
}

/// The whole desired control state, replaced atomically.
///
/// Every field is required so the request states the full triple rather than a
/// patch whose omissions have to be guessed at; `effort` may be `null` to mean
/// the provider's own default.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsInput {
    model: String,
    effort: Option<String>,
    fast: bool,
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpeedInput {
    fast: bool,
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveSessionInput {
    node_id: Option<i32>,
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartTurnInput {
    prompt: String,
    /// Keys of files already uploaded to THIS session, in send order. Defaulted
    /// so a client that predates attachments posts the body it always did.
    #[serde(default)]
    attachment_keys: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ListSessionsInput {
    #[serde(default)]
    archived: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TeamSessionInput {
    provider: String,
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
        effort: session.effort,
        fast: session.fast,
        bound_chat_id: session.bound_chat_id,
        node_id: session.node_id,
        archived: session.archived,
        revision: session.revision.to_string(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

fn turn_dto(turn: AgentTurn, attachments: Vec<AttachmentDto>) -> TurnDto {
    TurnDto {
        id: turn.id,
        session_id: turn.session_id,
        sequence: turn.sequence.to_string(),
        run_id: turn.run_id,
        prompt: turn.prompt,
        response: turn.response,
        activity: turn.activity,
        attachments,
        status: turn.status.name(),
        error_code: turn.error_code,
        provider: turn.provider,
        model: turn.model,
        created_at: turn.created_at,
        updated_at: turn.updated_at,
        completed_at: turn.completed_at,
    }
}

/// A page of turns with the files each was started with. One lookup for the
/// page, so a long history is not a round trip per turn.
async fn turn_page_dto(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    page: Page<AgentTurn>,
) -> Result<TurnPageDto, ApiError> {
    let sequences: Vec<i64> = page.items.iter().map(|turn| turn.sequence).collect();
    let mut carried = attachment::for_turns(state, actor, session_id, &sequences).await?;
    let mut items = Vec::with_capacity(page.items.len());
    for turn in page.items {
        let attachments = attachment_dtos(carried.remove(&turn.sequence).unwrap_or_default());
        items.push(turn_dto(turn, attachments));
    }
    Ok(TurnPageDto {
        items,
        next_after: page.next_after,
    })
}

/// The files one persisted turn carries, read back from its references.
async fn turn_attachments(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    sequence: i64,
) -> Result<Vec<StoredAttachment>, ApiError> {
    let mut carried = attachment::for_turns(state, actor, session_id, &[sequence]).await?;
    Ok(carried.remove(&sequence).unwrap_or_default())
}

/// Hand the runtime the bytes, not a path: only it knows where this run's
/// working directory is, and a path chosen here would point at nothing.
fn run_attachments(resolved: Vec<attachment::RunAttachment>) -> Vec<RunAttachment> {
    resolved
        .into_iter()
        .map(|item| {
            let is_image = item.is_image();
            RunAttachment {
                key: item.key,
                file_name: item.file_name,
                bytes: item.bytes,
                is_image,
            }
        })
        .collect()
}

/// The keys a set of persisted references actually names, in stored order.
fn stored_keys(stored: &[StoredAttachment]) -> Vec<String> {
    stored.iter().map(|item| item.key.clone()).collect()
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
            node_id: input.node_id,
            effort: input.effort.clone(),
            fast: input.fast,
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

/// File a session under a tree node, or unfile it with `"nodeId": null`.
///
/// Its own route rather than another arm of `update_session` so the body stays
/// unambiguous, and so filing carries its own revision: moving a session and
/// renaming it are separate edits that must not half-apply.
pub(super) async fn move_session(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<MoveSessionInput>(&state, request, SESSION_BODY_LIMIT)
            .await?;
    let expected = catalog::canonical_revision(&input.expected_revision)?;
    let updated =
        sessions::move_to_node(&state, &actor, &id, input.node_id, expected).await?;
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
    let page = sessions::turns(&state, &actor, &id, after).await?;
    Ok(private_json(
        turn_page_dto(&state, &actor, &id, page).await?,
    ))
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
    let keys = requested_keys(&input.attachment_keys)?;
    let prompt = sent_text(input.prompt, keys.len());
    let session = sessions::get(&state, &actor, &session_id).await?;
    // Resolved BEFORE the turn exists: an unreadable key, one uploaded to
    // another conversation, or a set over the per-turn budget is a plain
    // refusal here rather than a turn that has to be settled as failed.
    let resolved = attachment::resolve_for_run(
        &state,
        &actor,
        Owner::Session(&session_id),
        &keys,
    )
    .await?;
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
    // its creator. The references commit with the turn row.
    let (turn, _created) =
        sessions::begin_turn_with_attachments(&state, &actor, &session_id, &prompt, None, &keys)
            .await?;
    // The run is described by STORAGE, not by the request: the references just
    // committed are read back and required to be the set that was resolved. A
    // mismatch settles the turn instead of launching against the wrong files.
    let carried = turn_attachments(&state, &actor, &session_id, turn.sequence).await?;
    if stored_keys(&carried) != keys {
        runs::settle_refusal(&state, &turn.run_id, "unavailable").await?;
        let settled = sessions::turn_by_run(&state, &actor, &turn.run_id).await?;
        let mut response = private_json(StartedTurnDto {
            run_id: settled.run_id.clone(),
            turn: turn_dto(settled, attachment_dtos(carried)),
        });
        *response.status_mut() = StatusCode::CREATED;
        return Ok(response);
    }
    let composed = sessions::conversation_prompt(&history, &turn.prompt);
    // Best effort: a run must not fail because its orientation could not be
    // read. Without it the agent answers about the sandbox it woke up in.
    let briefing = crate::neev::agents::briefing::build(
        &state,
        &actor,
        crate::neev::agents::briefing::Scope::Session {
            node_id: session.node_id,
        },
        &turn.prompt,
    )
    .await;
    let run_request = RunRequest {
        // Server-authoritative identity: never a client-supplied user/org.
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: session_id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        prompt: composed,
        credential_handle,
        briefing,
        // The session's stored choice, re-applied on every turn — which is what
        // makes it a setting rather than something to re-pick each time.
        effort: session.effort.clone(),
        fast: session.fast,
        // The bytes behind the references this turn committed. The runtime
        // stages them inside its own working directory; nothing here guesses a
        // path, and a run that cannot stage them fails rather than mentioning
        // files it does not have.
        attachments: run_attachments(resolved),
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
                turn: turn_dto(turn, attachment_dtos(carried)),
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
                turn: turn_dto(settled, attachment_dtos(carried)),
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

/// One recorded step of the provider's work.
///
/// `seq` is the client's dedupe key: a reattach replays the stored prefix and
/// then streams live items, so the same item may arrive twice and must upsert
/// rather than append.
fn activity_event(item: &crate::neev::agents::activity::ActivityItem) -> Event {
    let mut value = serde_json::to_value(item).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("type".into(), serde_json::json!("activity"));
    }
    json_event(&value)
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
            for item in &turn.activity {
                if deliver(&mut sender, activity_event(item)).await.is_err() {
                    return;
                }
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
    let mut delivered_seq = 0;
    for item in &attachment.activity {
        if deliver(sender, activity_event(item)).await.is_err() {
            return;
        }
        delivered_seq = delivered_seq.max(item.seq);
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
                let mut pending: Vec<crate::neev::agents::activity::ActivityItem> = Vec::new();
                loop {
                    match attachment.events.try_recv() {
                        Ok(RunSignal::Delta(next)) => text.push_str(&splice(next, &mut emitted)),
                        // Pulled off the queue while coalescing text; held so it
                        // is delivered after this frame rather than dropped.
                        Ok(RunSignal::Activity(item)) => pending.push(item),
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
                for item in &pending {
                    if deliver(sender, activity_event(item)).await.is_err() {
                        return;
                    }
                    delivered_seq = delivered_seq.max(item.seq);
                }
                if let Some((status, error_code)) = finished {
                    let _ = deliver(sender, finished_event(status, error_code.as_deref())).await;
                    return;
                }
            }
            Ok(RunSignal::Activity(item)) => {
                if authority.ensure(state).await.is_err() {
                    return;
                }
                if deliver(sender, activity_event(&item)).await.is_err() {
                    return;
                }
                delivered_seq = delivered_seq.max(item.seq);
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
                // The trace shares this channel with the text, so a lagged
                // reader drops activity signals too. The text is recovered from
                // the run's buffer above; recover the trace from the run's log
                // the same way, or the live view keeps a hole no amount of
                // client-side dedupe can fill. Re-emitting from the cursor is
                // safe because items are idempotent by `seq`.
                // Deliberately NOT filtered by `delivered_seq`: a tool call is
                // announced when it starts and re-sent with its arguments when
                // it finishes, under the SAME seq. Skipping seqs already seen
                // would drop exactly that revision and leave the reader looking
                // at a tool that never filled in. Re-sending is safe because the
                // client upserts.
                for item in attachment.run.activity_snapshot() {
                    if authority.ensure(state).await.is_err() {
                        return;
                    }
                    if deliver(sender, activity_event(&item)).await.is_err() {
                        return;
                    }
                    delivered_seq = item.seq;
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

/// How long a `test` request will follow a bounded provider check before
/// reporting that it could not settle. The supervisor polls with its own
/// backoff; this is the request's patience, not the check's lifetime.
const CHECK_POLL_ATTEMPTS: usize = 20;
const CHECK_POLL_INTERVAL_MS: u64 = 250;

/// `POST /api/neev/agent/connections/{provider}/test` — verify the caller's own
/// stored credential against the provider, side-effect free.
///
/// The whole chain is real: the status epoch is adopted (which requires a
/// successful reconcile with the supervisor), `prepare_connection_check` admits
/// the check against a specific connection generation and storage handle, the
/// supervisor performs it, and `finalize_connection_check` re-proves the
/// connection has not changed underneath before storing the observation. The
/// response is the connection as it now stands.
///
/// `503` means the check genuinely could not be performed — no supervisor, or
/// it never settled within the request's patience. It never reports a
/// connection as verified on the strength of having asked.
pub(super) async fn test_connection(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let provider = path_provider(&safe_path(path)?)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<ConnectInput>(&state, request, SESSION_BODY_LIMIT).await?;
    // The request key is validated on the same rule as every other connection
    // mutation, even though a check writes no operation row.
    if input.request_key.is_empty() || input.request_key.len() > 64 {
        return Err(ApiError::Invalid);
    }
    if !state.supervisor.is_available() {
        return Err(ApiError::Unavailable);
    }
    // Adopting the epoch is what proves this process reconciled with the
    // supervisor; without it `prepare_connection_check` would refuse as stale.
    let epoch = supervisor::adopt_status_epoch(&state)
        .await
        .ok_or(ApiError::Unavailable)?;

    let admission =
        crate::neev::agents::status::prepare_connection_check(&state, &actor, &epoch, provider)
            .await
            .map_err(status_error)?;
    let ticket = admission.ticket().clone();

    for attempt in 0..CHECK_POLL_ATTEMPTS {
        match supervisor::check_connection(&state, &ticket).await {
            Ok(supervisor::CheckProgress::Observed(outcome)) => {
                let finalized = crate::neev::agents::status::finalize_connection_check(
                    &state, &actor, &ticket, outcome,
                )
                .await
                .map_err(status_error)?;
                return match finalized {
                    crate::neev::agents::status::CheckFinalized::Applied(view) => {
                        Ok(private_json(ConnectionTestDto {
                            connection: connection_dto(view, state.agents.is_available()),
                        }))
                    }
                    // The provider could not be reached, or the check lapsed:
                    // the stored connection is untouched and we say so.
                    crate::neev::agents::status::CheckFinalized::Unavailable(_) => {
                        Err(ApiError::Unavailable)
                    }
                    // Something else moved the connection while we checked.
                    crate::neev::agents::status::CheckFinalized::Stale => Err(ApiError::Conflict),
                };
            }
            Ok(supervisor::CheckProgress::Pending) => {
                if attempt + 1 < CHECK_POLL_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(CHECK_POLL_INTERVAL_MS))
                        .await;
                }
            }
            // A failed exchange leaves the admitted check to expire on its own
            // deadline rather than being reported as an outcome.
            Err(error) => return Err(super::agent_error(error.into())),
        }
    }
    Err(ApiError::Unavailable)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionTestDto {
    connection: ConnectionDto,
}

/// Map the status module's taxonomy onto HTTP. `Busy` and `Stale` are both
/// "someone else is mid-flight or moved it" — a retry is the answer, not an
/// error the user can fix.
fn status_error(error: crate::neev::agents::status::StatusError) -> ApiError {
    use crate::neev::agents::status::StatusError;
    match error {
        StatusError::Agent(agent) => super::agent_error(agent),
        StatusError::Invalid => ApiError::Invalid,
        StatusError::Busy | StatusError::Stale => ApiError::Conflict,
        StatusError::Unavailable => ApiError::Unavailable,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamInvokeInput {
    prompt: String,
    provider: String,
    model: String,
    /// The controls for THIS ask, applied to the holder before it launches.
    ///
    /// Carried on the invoke rather than requiring a holder to exist first: the
    /// reader has to be able to choose model, effort and speed before the first
    /// paid turn, and a separate "create the holder" round trip would both cost
    /// a request and leave empty holders behind for asks that never happened.
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    fast: bool,
    request_id: String,
    expected_auth_revision: String,
    /// Keys of files already uploaded to THIS chat, in send order. They hang on
    /// the invoker's team-visible MESSAGE — the ask and its files are one thing
    /// the room can see — and the run is fed from those persisted references.
    #[serde(default)]
    attachment_keys: Vec<String>,
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

/// GET `/api/neev/agent/capabilities` — what the installed runners accept.
///
/// Served rather than hardcoded in the client so the composer can only ever
/// offer a model or level the runner will honour. Public to any org member: it
/// describes the deployment's agent capabilities, carries no per-user state and
/// names no credential.
pub(super) async fn capabilities(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    // The CALLER's own catalogue: what a provider offers depends on the account
    // asking, so a shared one would advertise models some readers cannot run.
    Ok(private_json(
        crate::neev::agents::capabilities::catalogue_for(&state, &actor).await,
    ))
}

/// PATCH `/api/neev/agent/sessions/{id}/settings` — model, effort and speed at
/// once.
///
/// The endpoint the composer should use. The single-field routes below stay for
/// compatibility, but changing model and speed through them in sequence can pass
/// through a pairing neither value is valid in.
pub(super) async fn set_settings(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<SettingsInput>(&state, request, SESSION_BODY_LIMIT).await?;
    let expected = catalog::canonical_revision(&input.expected_revision)?;
    Ok(private_json(session_dto(
        sessions::set_settings(
            &state,
            &actor,
            &id,
            &input.model,
            input.effort.as_deref(),
            input.fast,
            expected,
        )
        .await?,
    )))
}

/// PATCH `/api/neev/agent/sessions/{id}/effort` — change the reasoning effort.
pub(super) async fn set_effort(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<EffortInput>(&state, request, SESSION_BODY_LIMIT).await?;
    let expected = catalog::canonical_revision(&input.expected_revision)?;
    Ok(private_json(session_dto(
        sessions::set_effort(&state, &actor, &id, input.effort.as_deref(), expected).await?,
    )))
}

/// PATCH `/api/neev/agent/sessions/{id}/speed` — turn fast mode on or off.
pub(super) async fn set_speed(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<SpeedInput>(&state, request, SESSION_BODY_LIMIT).await?;
    let expected = catalog::canonical_revision(&input.expected_revision)?;
    Ok(private_json(session_dto(
        sessions::set_fast(&state, &actor, &id, input.fast, expected).await?,
    )))
}

/// GET `/api/neev/chats/{chat_id}/agent` — the caller's OWN bound team session
/// for this chat, if one exists.
///
/// Reopening a chat has to show the traces of invocations made in it, and the
/// session holding them is keyed by a server-side digest the client cannot
/// compute. This is the discovery step; the existing
/// `/agent/sessions/{id}/turns` then serves those turns and their activity.
///
/// Strictly the caller's own. `owned` scopes by `(org, id, owner)`, so this
/// cannot reach another member's holder for the same chat even though the chat
/// itself is shared — one person's tool traces are not the room's.
pub(super) async fn team_session(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<TeamSessionInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let chat_id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&chat_id)?;
    let input = safe_query(query)?;
    let provider = provider_of(&input.provider).ok_or(ApiError::Invalid)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    // Prove the CHAT is still readable before resolving anything keyed to it.
    // The session id is derived from the chat id and `sessions::get` checks
    // only session ownership, so without this a member who lost access to the
    // chat — made private, archived, or their membership revoked — could still
    // enumerate their own past invocations in it. Discovery fails closed with
    // the chat it belongs to.
    {
        let tx = crate::neev::content::resources::begin(&state, &actor).await?;
        let chat = crate::neev::content::access::authorized(&tx, &actor, &chat_id, false).await?;
        if chat.kind != crate::neev::content::types::ResourceKind::Chat {
            return Err(ApiError::NotFound);
        }
    }
    let id = sessions::team_session_id(
        actor.org_id,
        actor.identity.user_id,
        &chat_id,
        crate::neev::agents::store::provider_name(provider),
    );
    Ok(private_json(session_dto(
        sessions::get(&state, &actor, &id).await?,
    )))
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
    let keys = requested_keys(&input.attachment_keys)?;
    let prompt = sent_text(input.prompt, keys.len());
    // Resolved before anything is written: a key from another chat, one the
    // caller may not read, or a set over the per-turn budget refuses here
    // rather than after the ask is already team-visible.
    let resolved =
        attachment::resolve_for_run(&state, &actor, Owner::Chat(&chat_id), &keys).await?;

    // 1. The ask becomes team-visible first, with its files bound to it in the
    //    same transaction. This also proves write access to the chat and
    //    applies the org's message quota, so an unauthorized or over-quota
    //    invocation never reaches the runtime.
    let prompt_message = crate::neev::content::messages::append_with_attachments(
        &state,
        &actor,
        &chat_id,
        &input.request_id,
        expected,
        &prompt,
        &keys,
    )
    .await?;
    // What the run is fed comes from the PERSISTED message, never from the
    // request: a retry that resolved to an earlier message carries that
    // message's files, and a key the append did not record cannot be smuggled
    // into this turn.
    let carried = super::content::message_attachments(&state, &actor, &prompt_message.id).await?;
    if stored_keys(&carried) != keys {
        return Err(ApiError::Conflict);
    }

    // 2. The invoker's own durable holder for this chat + provider.
    // Applied to the holder before anything launches, so the FIRST ask runs at
    // the settings the reader chose rather than at defaults they would then be
    // charged to correct.
    let session = sessions::ensure_team_session(
        &state,
        &actor,
        &chat_id,
        &input.provider,
        &input.model,
        "Team invocation",
        input.effort.as_deref(),
        input.fast,
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
    // No attachment references on the TURN: these files belong to the chat and
    // are already recorded on the message the room can see. Referencing them a
    // second time from a private holder would give one attachment two
    // audiences.
    let (turn, created) = sessions::begin_turn(
        &state,
        &actor,
        &session.id,
        &prompt,
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
            message: super::content::message_dto(prompt_message, attachment_dtos(carried))?,
            run_id: turn.run_id.clone(),
            turn: turn_dto(turn, Vec::new()),
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
    // The thread is context the invoker is reading right now, so replaying a
    // bounded tail of it is not a disclosure — it is what makes the answer
    // about the conversation instead of about one sentence pulled out of it.
    // The PRIVATE session's own turns are still never composed in.
    let briefing = crate::neev::agents::briefing::build(
        &state,
        &actor,
        crate::neev::agents::briefing::Scope::Chat { chat_id: &chat_id },
        &turn.prompt,
    )
    .await;
    let run_request = RunRequest {
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: chat_id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        // ONLY the explicit prompt. No private session history is composed in;
        // the thread's own messages arrive through the briefing, which is
        // context the invoker can already read.
        prompt: turn.prompt.clone(),
        credential_handle,
        briefing,
        effort: session.effort.clone(),
        fast: session.fast,
        // The bytes behind the message's persisted references; the runtime
        // stages them into its own workdir.
        attachments: run_attachments(resolved),
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
        message: super::content::message_dto(prompt_message, attachment_dtos(carried))?,
        run_id: turn.run_id.clone(),
        turn: turn_dto(turn, Vec::new()),
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
    /// What the user must do to approve, once the supervisor has told us.
    /// `null` until then. Carries only display data — never a client secret,
    /// PKCE verifier or cookie.
    approval: Option<ApprovalDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalDto {
    /// `"device"` (Codex: enter `userCode` at `verificationUrl`) or
    /// `"authorizationCode"` (Claude: approve at `verificationUrl`, then paste
    /// the resulting code back through the `/code` route).
    kind: &'static str,
    verification_url: String,
    user_code: Option<String>,
    needs_code: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitCodeInput {
    code: String,
}

fn approval_dto(challenge: &crate::neev::agents::control_dto::LoginChallenge) -> ApprovalDto {
    let user_code = challenge.user_code().map(|code| code.as_str().to_owned());
    ApprovalDto {
        kind: if user_code.is_some() {
            "device"
        } else {
            "authorizationCode"
        },
        verification_url: challenge.verification_url().to_owned(),
        needs_code: user_code.is_none(),
        user_code,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptDto {
    operation_id: String,
    generation: String,
    /// The connection AFTER the teardown attempt.
    ///
    /// A `200` here means the request was accepted, not that the credential is
    /// already gone: if the supervisor could not be reached the connection is
    /// still `disconnecting` and the background sweep will retry. Returning the
    /// real state stops the UI showing "Disconnected" over a connection that is
    /// still tearing down.
    connection: ConnectionDto,
}

/// Re-read one provider's connection so a mutation can report what it actually
/// achieved rather than what it asked for.
async fn connection_now(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
) -> Result<ConnectionDto, ApiError> {
    let runtime_enabled = state.agents.is_available();
    let views = intents::list(state, actor)
        .await
        .map_err(super::agent_error)?;
    views
        .into_iter()
        .find(|view| view.provider == provider)
        .map(|view| connection_dto(view, runtime_enabled))
        .ok_or(ApiError::NotFound)
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
        crate::neev::agents::types::BeginOutcome::Pending(ticket) => {
            // Hand the recorded intent to the supervisor, which performs the
            // actual provider sign-in. If it refuses, the attempt is cancelled
            // rather than left pending for ten minutes over a login that was
            // never started.
            if let Err(error) = supervisor::start_login(&state, &actor, &ticket).await {
                let _ = intents::cancel(
                    &state,
                    &actor,
                    provider,
                    &ticket.attempt_id,
                    &format!("{}-abort", input.request_key),
                )
                .await;
                return Err(super::agent_error(error.into()));
            }
            ConnectStartedDto {
                outcome: "pending",
                attempt_id: Some(ticket.attempt_id),
                expires_at: Some(ticket.expires_at),
                connection: None,
            }
        }
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
    // Only a still-pending attempt is worth asking the supervisor about; a
    // settled one already has its answer in storage.
    //
    // A poll is also where a sign-in FINISHES: `advance_login` commits a
    // prepared login through `completion::complete` (storing the user's own
    // credential handle and marking the connection `connected`) and settles a
    // terminal failure, so the state below is re-read afterwards rather than
    // reported from before the step.
    let approval = if attempt.state == "pending" {
        match supervisor::find_login(
            &state.agent_logins,
            &attempt_id,
            actor.org_id,
            actor.identity.user_id,
        ) {
            Some(session) => match supervisor::advance_login(&state, &actor, &session).await {
                Ok(supervisor::LoginProgress::Pending) => supervisor::find_login(
                    &state.agent_logins,
                    &attempt_id,
                    actor.org_id,
                    actor.identity.user_id,
                )
                .and_then(|session| session.approval),
                // Connected or settled: the stored attempt now carries the
                // answer, and there is nothing left for the user to approve.
                Ok(_) => None,
                // The supervisor is briefly unreachable. Serve the cached
                // instructions rather than erasing ones the user is following.
                Err(_) => session.approval,
            },
            None => None,
        }
    } else {
        None
    };
    // Re-read: `advance_login` may just have moved this attempt to its final
    // state, and reporting the pre-step value would be a lie.
    let attempt = intents::attempt(&state, &actor, provider, &attempt_id)
        .await
        .map_err(super::agent_error)?;
    Ok(private_json(AttemptDto {
        state: attempt.state,
        expires_at: attempt.expires_at,
        approval: approval.as_ref().map(approval_dto),
    }))
}

/// `POST /api/neev/agent/connections/{provider}/attempts/{attemptId}/code` —
/// deliver the authorization code a Claude-style login asked the user to paste
/// back.
///
/// The code is forwarded straight to the supervisor, bound to THIS login, and
/// is never stored or logged here.
pub(super) async fn submit_attempt_code(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let (provider, attempt_id) = safe_path(path)?;
    let provider = path_provider(&provider)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<SubmitCodeInput>(&state, request, SESSION_BODY_LIMIT)
            .await?;
    let code = crate::neev::agents::control_dto::AuthorizationCode::new(input.code)
        .map_err(|_| ApiError::Invalid)?;
    // The attempt must still be this caller's, and still pending.
    let attempt = intents::attempt(&state, &actor, provider, &attempt_id)
        .await
        .map_err(super::agent_error)?;
    if attempt.state != "pending" {
        return Err(ApiError::Conflict);
    }
    let session = supervisor::find_login(
        &state.agent_logins,
        &attempt_id,
        actor.org_id,
        actor.identity.user_id,
    )
    .ok_or(ApiError::NotFound)?;
    // Per-ATTEMPT budget, distinct from the connect quota: a pasted-code field
    // must not become an unbounded lever on the provider.
    supervisor::charge_code_attempt(&state.agent_logins, &attempt_id)
        .map_err(|_| ApiError::Limited)?;
    let snapshot = supervisor::submit_code(&state, &session, code)
        .await
        .map_err(|error| super::agent_error(error.into()))?;
    let approval = match &snapshot {
        crate::neev::agents::control_dto::LoginSnapshot::AwaitingUser { challenge, .. } => {
            Some(approval_dto(challenge))
        }
        _ => None,
    };
    let mut response = private_json(AttemptDto {
        state: attempt.state,
        expires_at: attempt.expires_at,
        approval,
    });
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
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
    // Cancelling QUEUES a cleanup; without draining it the connection stays
    // `disconnecting` and a fresh sign-in is refused. Best effort here, and the
    // periodic sweep retries whatever this misses.
    supervisor::drain_cleanups(&state, std::slice::from_ref(&receipt.operation_id)).await;
    forget_login_for(&state, &input.attempt_id);
    Ok(private_json(ReceiptDto {
        operation_id: receipt.operation_id,
        generation: receipt.generation.to_string(),
        connection: connection_now(&state, &actor, provider).await?,
    }))
}

fn forget_login_for(state: &ServerState, attempt_id: &str) {
    supervisor::forget_login(&state.agent_logins, attempt_id);
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
    // The credential volume is torn down by the supervisor; draining here is
    // what actually returns the connection to `disconnected`.
    supervisor::drain_cleanups(&state, std::slice::from_ref(&receipt.operation_id)).await;
    Ok(private_json(ReceiptDto {
        operation_id: receipt.operation_id,
        generation: receipt.generation.to_string(),
        connection: connection_now(&state, &actor, provider).await?,
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
