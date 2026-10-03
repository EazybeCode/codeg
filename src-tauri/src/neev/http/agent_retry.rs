//! Retry a run that failed — the SAME ask, run again, and nothing posted twice.
//!
//! `POST /api/neev/agent/runs/{run_id}/retry` with `{ "requestId" }`.
//!
//! What is re-run comes from STORAGE, never from the request: the failed turn's
//! own prompt and the attachment references it was started with. So a retry
//! cannot change the ask, and cannot smuggle in a file the original did not
//! carry.
//!
//! - **Team chat.** The ask is a canonical, team-visible human message. A retry
//!   starts a NEW turn in the invoker's holder for that chat, fed from that same
//!   message's persisted files, and appends NOTHING to the chat: no second copy
//!   of the ask, and no second copy of any follow-up that was accepted into the
//!   failed run (those are already their own messages in the thread, which the
//!   retry reads as context like any other).
//! - **Agent Chat.** A new turn in the same session with the same prompt and
//!   the same attachment references.
//!
//! `requestId` makes the retry itself idempotent: a double click, or a network
//! retry of the retry, resolves to the one turn it created. Only a turn that
//! ended `failed` or `canceled` can be retried; a running one is refused rather
//! than raced, and a completed one has nothing to retry.
//!
//! A child module of `agent_sessions` so it reuses that module's private
//! helpers (DTOs, attachment reads, library loading) instead of copying them.

use super::*;

/// Receipt: this failed run was retried, by which request, into which run.
/// Keyed by the SOURCE run, so a failed run is retried at most once — a stale
/// tab or an old run id cannot re-run an ask that a retry already answered —
/// while the same request replays to the same new run.
const RETRY_ACTION: &str = "agent.run.retry";
/// Receipt: which canonical team message a retry run answers. Keyed by the NEW
/// run, so a retry that fails can itself be retried (its own turn carries the
/// retry's request id, which no message-append receipt ever has).
const RETRY_ASK_ACTION: &str = "agent.run.retry.ask";

fn retry_hash(request_id: &str) -> String {
    crate::neev::org::digest_parts(&[RETRY_ACTION, request_id])
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::neev::http) struct RetryInput {
    request_id: String,
}

/// What the failed turn was, read back from storage.
struct Source {
    turn: AgentTurn,
    session: AgentSession,
    /// For a team turn: the team-visible message the ask was.
    message_id: Option<String>,
}

pub(in crate::neev::http) async fn retry_run(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let run_id = safe_path(path)?;
    canonical_run_id(&run_id)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<RetryInput>(&state, request, TURN_BODY_LIMIT).await?;
    canonical_uuid(&input.request_id)?;
    // Before ANY preflight: a request that already made its retry gets that
    // turn back, even if credentials, files or settings changed since — a
    // dropped response must never turn into an error or a second run.
    if let Some(turn) = recorded_retry(&state, &actor, &run_id, &input.request_id).await? {
        let carried = turn_attachments(&state, &actor, &turn.session_id, turn.sequence).await?;
        return created(turn, attachment_dtos(carried));
    }
    let source = load_source(&state, &actor, &run_id).await?;
    match (&source.session.kind, source.message_id.clone()) {
        (sessions::SessionKind::Team, Some(message_id)) => {
            retry_team(state.clone(), actor, source, message_id, &input.request_id).await
        }
        (sessions::SessionKind::Chat, None) => {
            retry_private(state.clone(), actor, source, &input.request_id).await
        }
        // A team turn whose ask cannot be found (or a private one claiming a
        // message) is not something to guess about.
        _ => Err(ApiError::Conflict),
    }
}

/// The run a previous retry of `source_run` made, if this request made it.
/// A retry made by a DIFFERENT request is a conflict: that failed run has
/// already been retried, and the newest attempt is the one to retry now.
async fn recorded_retry(
    state: &ServerState,
    actor: &OrgContext,
    source_run: &str,
    request_id: &str,
) -> Result<Option<AgentTurn>, ApiError> {
    use crate::neev::content::resources;
    use crate::neev::db;
    use crate::neev::error::database_error;

    let tx = resources::begin(state, actor).await?;
    let row = db::one(
        &tx,
        "SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            actor.identity.user_id.into(),
            RETRY_ACTION.into(),
            source_run.to_owned().into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let hash: String = row.try_get("", "body_hash").map_err(database_error)?;
    if hash != retry_hash(request_id) {
        return Err(ApiError::Conflict);
    }
    let run: String = row.try_get("", "resource_id").map_err(database_error)?;
    Ok(Some(sessions::turn_by_run(state, actor, &run).await?))
}

/// Record that `source_run` was retried into `new_run` (and, for a team retry,
/// which message it answers). Idempotent for the same pair; a conflict when a
/// different retry of the same source already won.
async fn link_retry(
    state: &ServerState,
    actor: &OrgContext,
    source_run: &str,
    request_id: &str,
    new_run: &str,
    message_id: Option<&str>,
) -> Result<(), ApiError> {
    use crate::neev::content::resources;
    use crate::neev::db;
    use crate::neev::error::database_error;

    const INSERT: &str = "INSERT INTO neev_requests(user_id,action,request_id,body_hash,resource_id,expires_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING";
    let tx = resources::begin(state, actor).await?;
    db::execute(
        &tx,
        INSERT,
        vec![
            actor.identity.user_id.into(),
            RETRY_ACTION.into(),
            source_run.to_owned().into(),
            retry_hash(request_id).into(),
            new_run.to_owned().into(),
            i64::MAX.into(),
        ],
    )
    .await?;
    let owner: String = db::one(
        &tx,
        "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            actor.identity.user_id.into(),
            RETRY_ACTION.into(),
            source_run.to_owned().into(),
        ],
    )
    .await?
    .ok_or(ApiError::Conflict)?
    .try_get("", "resource_id")
    .map_err(database_error)?;
    if owner != new_run {
        return Err(ApiError::Conflict);
    }
    if let Some(message_id) = message_id {
        db::execute(
            &tx,
            INSERT,
            vec![
                actor.identity.user_id.into(),
                RETRY_ASK_ACTION.into(),
                new_run.to_owned().into(),
                crate::neev::org::digest_parts(&[RETRY_ASK_ACTION, message_id]).into(),
                message_id.to_owned().into(),
                i64::MAX.into(),
            ],
        )
        .await?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

/// Link a just-begun retry turn to its source, or — when a different retry of
/// the same source already won a race — settle this one without running it.
async fn claim(
    state: &ServerState,
    actor: &OrgContext,
    source_run: &str,
    request_id: &str,
    turn: &AgentTurn,
    created_now: bool,
    message_id: Option<&str>,
) -> Result<(), ApiError> {
    match link_retry(
        state,
        actor,
        source_run,
        request_id,
        &turn.run_id,
        message_id,
    )
    .await
    {
        // ANY failure to link a turn this request just created settles it: a
        // turn left `running` but never launched would block every later turn
        // in the session until the next startup's reconciliation.
        Err(error) if created_now => Err(abandon(state, turn, error).await),
        other => other,
    }
}

/// Settle a created-but-never-launched retry turn as refused, and hand back
/// the error that stopped it (or the settling error, if even that failed —
/// the startup reconciliation is then the backstop).
async fn abandon(state: &ServerState, turn: &AgentTurn, error: ApiError) -> ApiError {
    match runs::settle_refusal(state, &turn.run_id, "unavailable").await {
        Ok(()) => error,
        Err(settle) => settle.into(),
    }
}

/// The follow-ups a failed turn ACCEPTED, or an error. Unlike the listing
/// helper (which reads a failed lookup as "none"), a retry must not silently
/// run without inputs the reader saw the agent take.
async fn accepted_follow_ups(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    sequence: i64,
) -> Result<Vec<String>, ApiError> {
    Ok(
        crate::neev::db::list_steer_inputs(&state.database, actor.org_id, session_id, sequence)
            .await?
            .into_iter()
            .filter(|input| input.response_offset.is_some())
            .map(|input| input.text)
            .collect(),
    )
}

/// The ask, plus any follow-ups the failed attempt had ACCEPTED. In an Agent
/// Chat those live only on that attempt's turn, so a retry built from the
/// original prompt alone would silently drop them. Bounded by the turn's own
/// prompt cap: follow-ups that do not fit are left out whole, never cut.
fn with_follow_ups<'a>(prompt: &str, follow_ups: impl IntoIterator<Item = &'a str>) -> String {
    const LIMIT: usize = 32_768;
    let mut out = prompt.to_owned();
    let mut listed = false;
    for text in follow_ups {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let block = if listed {
            format!("\n- {text}")
        } else {
            format!("\n\nFollow-ups sent while the earlier attempt was running:\n- {text}")
        };
        if out.chars().count() + block.chars().count() > LIMIT {
            break;
        }
        listed = true;
        out.push_str(&block);
    }
    out
}

/// The failed turn (owner-checked), its session, and — for a team turn — the
/// message it was asked in, proven to belong to the session's chat.
async fn load_source(
    state: &ServerState,
    actor: &OrgContext,
    run_id: &str,
) -> Result<Source, ApiError> {
    // `turn_by_run` proves the actor owns the session this run belongs to.
    let turn = sessions::turn_by_run(state, actor, run_id).await?;
    if !matches!(turn.status, TurnStatus::Failed | TurnStatus::Canceled) {
        return Err(ApiError::Conflict);
    }
    let session = sessions::get(state, actor, &turn.session_id).await?;
    let message_id = match (&session.kind, session.bound_chat_id.as_deref()) {
        (sessions::SessionKind::Team, Some(chat_id)) => {
            Some(team_message(state, actor, &turn, chat_id).await?)
        }
        (sessions::SessionKind::Team, None) => return Err(ApiError::Conflict),
        (sessions::SessionKind::Chat, _) => None,
    };
    Ok(Source {
        turn,
        session,
        message_id,
    })
}

/// The canonical message a team turn was asked in.
///
/// A team turn's `request_id` IS the invocation's request id, which is also the
/// receipt the message append recorded — the one link between the two rows.
/// Proves, in the same transaction, that the actor can still WRITE the chat
/// (an invocation needs write access, so its retry does too).
async fn team_message(
    state: &ServerState,
    actor: &OrgContext,
    turn: &AgentTurn,
    chat_id: &str,
) -> Result<String, ApiError> {
    use crate::neev::content::{access, resources, types::ResourceKind};
    use crate::neev::db;
    use crate::neev::error::database_error;

    let tx = resources::begin(state, actor).await?;
    let chat = access::authorized(&tx, actor, chat_id, true).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ApiError::NotFound);
    }
    let request_id: Option<String> = db::one(
        &tx,
        "SELECT request_id FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND run_id=$3",
        vec![
            actor.org_id.into(),
            turn.session_id.clone().into(),
            turn.run_id.clone().into(),
        ],
    )
    .await?
    .map(|row| row.try_get("", "request_id").map_err(database_error))
    .transpose()?
    .flatten();
    // The original invocation: its request id is the message-append receipt.
    let appended = match request_id {
        Some(request_id) => db::one(
            &tx,
            "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action='content.message.append' AND request_id=$2",
            vec![actor.identity.user_id.into(), request_id.into()],
        )
        .await?,
        None => None,
    };
    // A retry run: linked to the message it answers when it was begun.
    let row = match appended {
        Some(row) => row,
        None => db::one(
            &tx,
            "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
            vec![
                actor.identity.user_id.into(),
                RETRY_ASK_ACTION.into(),
                turn.run_id.clone().into(),
            ],
        )
        .await?
        .ok_or(ApiError::Conflict)?,
    };
    let message_id: String = row.try_get("", "resource_id").map_err(database_error)?;
    let in_chat = db::one(
        &tx,
        "SELECT 1 AS found FROM neev_messages WHERE org_id=$1 AND chat_id=$2 AND id=$3",
        vec![
            actor.org_id.into(),
            chat_id.to_owned().into(),
            message_id.clone().into(),
        ],
    )
    .await?
    .is_some();
    tx.commit().await.map_err(database_error)?;
    if !in_chat {
        return Err(ApiError::Conflict);
    }
    Ok(message_id)
}

fn created(turn: AgentTurn, attachments: Vec<AttachmentDto>) -> Result<Response, ApiError> {
    let mut response = private_json(StartedTurnDto {
        route: None,
        run_id: turn.run_id.clone(),
        turn: turn_dto(turn, attachments, Vec::new()),
    });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn retry_team(
    state: Arc<ServerState>,
    actor: OrgContext,
    source: Source,
    message_id: String,
    request_id: &str,
) -> Result<Response, ApiError> {
    let Source {
        turn: failed,
        session,
        ..
    } = source;
    let chat_id = session.bound_chat_id.clone().ok_or(ApiError::Conflict)?;
    let provider = provider_of(&session.provider).ok_or(ApiError::Invalid)?;
    // The ask's files, from the message the room can see — and re-authorized
    // against the chat's CURRENT access, exactly as a first invocation is.
    let carried = super::super::content::message_attachments(&state, &actor, &message_id).await?;
    let keys = stored_keys(&carried);
    let resolved =
        attachment::resolve_for_run(&state, &actor, Owner::Chat(&chat_id), &keys).await?;
    let library_items = load_library_items(&state, &actor).await?;
    let credential_handle = intents::credential(&state, &actor, provider)
        .await
        .map_err(super::super::agent_error)?;
    let (turn, created_now) = sessions::begin_turn(
        &state,
        &actor,
        &session.id,
        &failed.prompt,
        Some(request_id),
    )
    .await?;
    claim(
        &state,
        &actor,
        &failed.run_id,
        request_id,
        &turn,
        created_now,
        Some(&message_id),
    )
    .await?;
    if !created_now {
        // The same retry again: the turn it already made, not a second run.
        return created(turn, Vec::new());
    }
    let publication = runs::TeamPublication {
        chat_id: chat_id.clone(),
        invoker: actor.clone(),
        agent_label: session.provider.clone(),
        turn_id: turn.id.clone(),
    };
    let context = transcript::build(
        &state,
        &actor,
        transcript::Source::Chat { chat_id: &chat_id },
        &keys,
    )
    .await;
    let briefing = crate::neev::agents::briefing::build(
        &state,
        &actor,
        crate::neev::agents::briefing::Scope::Chat { chat_id: &chat_id },
        &turn.prompt,
        context.as_ref().map(|bundle| &bundle.manifest),
    )
    .await;
    let run_request = RunRequest {
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: chat_id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        prompt: turn.prompt.clone(),
        credential_handle,
        briefing,
        effort: session.effort.clone(),
        fast: session.fast,
        attachments: run_attachments(resolved),
        context_files: context.map(|bundle| bundle.files).unwrap_or_default(),
        library_items,
        min_free_lanes: 1,
    };
    let turn = launch(
        &state,
        &actor,
        &session.id,
        turn,
        run_request,
        Some(publication),
    )
    .await?;
    created(turn, Vec::new())
}

async fn retry_private(
    state: Arc<ServerState>,
    actor: OrgContext,
    source: Source,
    request_id: &str,
) -> Result<Response, ApiError> {
    let Source {
        turn: failed,
        session,
        ..
    } = source;
    let carried = turn_attachments(&state, &actor, &session.id, failed.sequence).await?;
    let keys = stored_keys(&carried);
    let resolved =
        attachment::resolve_for_run(&state, &actor, Owner::Session(&session.id), &keys).await?;
    // Only completed turns contribute history, so the failed attempt is not
    // replayed to the provider as if it had been answered.
    let history = sessions::recent_turns(&state, &actor, &session.id).await?;
    let context = transcript::build(
        &state,
        &actor,
        transcript::Source::Session {
            session_id: &session.id,
        },
        &keys,
    )
    .await;
    let credential_handle = match provider_of(&session.provider) {
        Some(provider) => intents::credential(&state, &actor, provider)
            .await
            .map_err(super::super::agent_error)?,
        None => None,
    };
    let library_items = load_library_items(&state, &actor).await?;
    let accepted = accepted_follow_ups(&state, &actor, &session.id, failed.sequence).await?;
    let prompt = with_follow_ups(&failed.prompt, accepted.iter().map(String::as_str));
    let (turn, created_now) = sessions::begin_turn_with_attachments(
        &state,
        &actor,
        &session.id,
        &prompt,
        Some(request_id),
        &keys,
    )
    .await?;
    claim(
        &state,
        &actor,
        &failed.run_id,
        request_id,
        &turn,
        created_now,
        None,
    )
    .await?;
    let carried = match turn_attachments(&state, &actor, &session.id, turn.sequence).await {
        Ok(carried) => carried,
        Err(error) if created_now => return Err(abandon(&state, &turn, error).await),
        Err(error) => return Err(error),
    };
    if !created_now {
        return created(turn, attachment_dtos(carried));
    }
    if stored_keys(&carried) != keys {
        runs::settle_refusal(&state, &turn.run_id, "unavailable").await?;
        let settled = sessions::turn_by_run(&state, &actor, &turn.run_id).await?;
        return created(settled, attachment_dtos(carried));
    }
    let composed = sessions::conversation_prompt(&history, &turn.prompt);
    let briefing = crate::neev::agents::briefing::build(
        &state,
        &actor,
        crate::neev::agents::briefing::Scope::Session {
            node_id: session.node_id,
        },
        &turn.prompt,
        context.as_ref().map(|bundle| &bundle.manifest),
    )
    .await;
    let run_request = RunRequest {
        user_id: actor.identity.user_id,
        org_id: actor.org_id,
        chat_id: session.id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        prompt: composed,
        credential_handle,
        briefing,
        effort: session.effort.clone(),
        fast: session.fast,
        attachments: run_attachments(resolved),
        context_files: context.map(|bundle| bundle.files).unwrap_or_default(),
        library_items,
        min_free_lanes: 1,
    };
    // A turn that was supervised is retried supervised, re-resolved now on the
    // caller's own Claude connection. If that is no longer possible the retry
    // runs as an ordinary turn and records no supervision step, so nothing on
    // it claims a plan or a review that did not happen.
    let was_supervised = failed
        .activity
        .iter()
        .any(|item| item.id.starts_with("supervision:"));
    if was_supervised {
        let (plan, _) = super::routing::resolve_supervision(&state, &actor).await;
        if !plan.is_empty() {
            let request_text = turn.prompt.clone();
            runs::start_supervised(
                state.clone(),
                turn.run_id.clone(),
                actor.clone(),
                session.id.clone(),
                turn.id.clone(),
                runs::supervision::SupervisedStart {
                    executor: run_request,
                    request_text,
                    plan,
                },
                runs::supervision::SupervisionRuntimes::shared(state.agents.clone()),
            );
            return created(turn, attachment_dtos(carried));
        }
    }
    let turn = launch(&state, &actor, &session.id, turn, run_request, None).await?;
    created(turn, attachment_dtos(carried))
}

/// Hand the new turn to the runtime, or settle it with the honest refusal.
async fn launch(
    state: &Arc<ServerState>,
    actor: &OrgContext,
    session_id: &str,
    turn: AgentTurn,
    run_request: RunRequest,
    publication: Option<runs::TeamPublication>,
) -> Result<AgentTurn, ApiError> {
    match state.agents.run(run_request).await {
        Ok(handle) => {
            runs::start(
                state.clone(),
                turn.run_id.clone(),
                actor.clone(),
                session_id.to_owned(),
                turn.id.clone(),
                handle,
                publication,
            );
            Ok(turn)
        }
        Err(error) => {
            let code = sessions::execution_error_code(&error);
            runs::settle_refusal(state, &turn.run_id, code).await?;
            Ok(sessions::turn_by_run(state, actor, &turn.run_id).await?)
        }
    }
}
