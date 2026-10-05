//! "Fork to new tab": copy a conversation up through one reply into a new
//! private conversation the caller can continue (`content::fork`,
//! `agents::session_fork`). Both endpoints answer the new conversation's id and
//! what was copied; the client reads the conversation itself through the
//! existing GET endpoints, so there is one representation of a chat or session.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::neev::{agents::session_fork, content::fork, startup::ServerState};

use super::{
    add_private_headers,
    catalog::{organization_mutation, permit},
    private_json, ApiError,
};

const FORK_BODY_LIMIT: usize = 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatForkInput {
    through_message_id: String,
    request_id: String,
    /// Where the fork lands. Needed only when the source's folder is hidden
    /// from the caller (`fork_no_location`); must be a folder they may place in.
    #[serde(default)]
    node_id: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionForkInput {
    through_turn_id: String,
    request_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatForkDto {
    chat_id: String,
    source_chat_id: String,
    through_message_id: String,
    node_id: i32,
    copied_messages: i64,
    copied_attachments: i64,
    archived_attachments_not_copied: i64,
    replayed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionForkDto {
    session_id: String,
    source_session_id: String,
    through_turn_id: String,
    copied_turns: i64,
    copied_attachments: i64,
    archived_attachments_not_copied: i64,
    skipped_incomplete_turns: i64,
    replayed: bool,
}

/// Refusals carry a stable code the client can explain (`neev-client`
/// surfaces these by name); everything else is the shared taxonomy.
fn refusal(error: fork::ForkError) -> Response {
    let code = match error {
        fork::ForkError::Content(error) => return ApiError::from(error).into_response(),
        fork::ForkError::NotComplete => "fork_not_complete",
        fork::ForkError::TooLarge => "fork_too_large",
        fork::ForkError::AttachmentsTooLarge => "fork_attachments_too_large",
        fork::ForkError::SourceTrashed => "fork_source_trashed",
        fork::ForkError::BoundChatUnavailable => "fork_bound_chat_unavailable",
        fork::ForkError::NoLocation => "fork_no_location",
    };
    let mut response = (StatusCode::CONFLICT, Json(serde_json::json!({ "code": code }))).into_response();
    add_private_headers(&mut response);
    response
}

fn created(value: impl Serialize) -> Response {
    let mut response = private_json(value);
    *response.status_mut() = StatusCode::CREATED;
    response
}

/// POST `/api/neev/chats/{chat_id}/fork` — `{throughMessageId, requestId}`.
pub(super) async fn fork_chat(
    State(state): State<Arc<ServerState>>,
    Path(chat_id): Path<String>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, actor) =
        organization_mutation::<ChatForkInput>(&state, request, FORK_BODY_LIMIT).await?;
    Ok(
        match fork::fork_chat_into(
            &state,
            &actor,
            &chat_id,
            &input.through_message_id,
            &input.request_id,
            input.node_id,
        )
        .await
        {
            Ok(done) => created(ChatForkDto {
                chat_id: done.chat_id,
                source_chat_id: done.source_chat_id,
                through_message_id: done.through_message_id,
                node_id: done.node_id,
                copied_messages: done.messages,
                copied_attachments: done.attachments,
                archived_attachments_not_copied: done.archived_attachments,
                replayed: done.replayed,
            }),
            Err(error) => refusal(error),
        },
    )
}

/// POST `/api/neev/agent/sessions/{session_id}/fork` — `{throughTurnId, requestId}`.
pub(super) async fn fork_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, actor) =
        organization_mutation::<SessionForkInput>(&state, request, FORK_BODY_LIMIT).await?;
    Ok(
        match session_fork::fork_session(
            &state,
            &actor,
            &session_id,
            &input.through_turn_id,
            &input.request_id,
        )
        .await
        {
            Ok(done) => created(SessionForkDto {
                session_id: done.session_id,
                source_session_id: done.source_session_id,
                through_turn_id: done.through_turn_id,
                copied_turns: done.turns,
                copied_attachments: done.attachments,
                archived_attachments_not_copied: done.archived_attachments,
                skipped_incomplete_turns: done.skipped_turns,
                replayed: done.replayed,
            }),
            Err(error) => refusal(error),
        },
    )
}
