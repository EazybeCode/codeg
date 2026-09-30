//! Persistence of accepted steering inputs with optional shared chat history.
//!
//! When a user sends follow-up text mid-run (steering), the input must be:
//! 1. Marked as accepted in the turn's steer input table (for display interleaving)
//! 2. For team sessions bound to a shared chat: also published as a user message
//!    in the chat, so the conversation history includes the user's follow-ups
//!
//! This module provides an atomic helper that does both operations, re-proving
//! write authorization at publish time (the invoker may have lost access since
//! the run started).
//!
//! **Contract**: The HTTP layer calls `accept_steer_input_and_publish` AFTER the
//! provider has acknowledged the input. The function is idempotent: a retry with
//! the same `request_id` returns success without duplicating the chat message.

use crate::neev::{
    auth::session,
    content::{access, resources, types::ContentError},
    db,
    error::database_error,
    org,
    startup::ServerState,
    types::OrgContext,
};

/// Largest follow-up text in UTF-8 bytes. Mirrors `STEER_MAX_TEXT_BYTES` in
/// `scripts/operations/neev_{codex,claude}_stream_adapter.py`, which reject
/// anything longer on the provider side.
pub(crate) const STEER_TEXT_MAX_BYTES: usize = 8 * 1024;

/// Result of accepting a steering input with optional shared chat publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SteerAcceptResult {
    /// The response offset where the input was accepted (character position).
    pub(crate) response_offset: i64,
    /// If the session is bound to a shared chat, the sequence of the published
    /// message. `None` for private sessions or if no message was published.
    pub(crate) message_sequence: Option<i64>,
}

/// Accept a steering input and optionally publish it as a user message in shared chat.
///
/// This is the atomic operation that the HTTP layer calls after the provider
/// acknowledges a steering input. It:
/// 1. Marks the steer input as accepted in `neev_steer_inputs`
/// 2. For team sessions with a bound chat: publishes the text as a user message
///
/// **Idempotency**: The `request_id` is the idempotency key. A retry returns the
/// same result without duplicating the chat message. Different text for the same
/// `request_id` is a conflict (the steer_migration layer already enforced this
/// at reservation time).
///
/// **Authorization and atomicity**: Re-prove current access before publishing.
/// The chat message, idempotency receipt, and accepted input commit together.
/// Any failure rolls back all three so a retry cannot mistake partial history
/// for a completed delivery.
///
/// # Arguments
/// * `state` - Server state with database and clock
/// * `actor` - The invoker's org context (for authorization)
/// * `session_id` - The agent session this turn belongs to
/// * `turn_sequence` - The turn's sequence number within the session
/// * `request_id` - Idempotency key for this steering input
/// * `text` - The follow-up text the user sent
/// * `response_offset` - Character offset in the response where input was accepted
/// * `created_at` - Timestamp when the input was created
/// * `bound_chat_id` - If Some, the shared chat to publish the message to
pub(crate) async fn accept_steer_input_and_publish(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    turn_sequence: i64,
    request_id: &str,
    text: &str,
    response_offset: i64,
    created_at: i64,
    bound_chat_id: Option<&str>,
) -> Result<SteerAcceptResult, ContentError> {
    if text.is_empty() || text.len() > STEER_TEXT_MAX_BYTES {
        return Err(ContentError::Invalid);
    }
    let message_sequence = if let Some(chat_id) = bound_chat_id {
        Some(publish_user_followup(state, actor, chat_id, request_id, text,
            created_at, session_id, turn_sequence, response_offset).await?)
    } else {
        let tx = resources::begin(state, actor).await?;
        db::accept_steer_input(&tx, actor.org_id, session_id, turn_sequence,
            request_id, response_offset).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        None
    };

    Ok(SteerAcceptResult {
        response_offset,
        message_sequence,
    })
}

/// Publish a user's steering follow-up as a message in the shared chat.
///
/// This is idempotent: the `request_id` (prefixed to avoid collision with other
/// receipt namespaces) ensures a retry publishes no duplicate.
///
/// The message is authored by the invoker (`author_id`) with `author_kind='human'`,
/// exactly as if they had typed it in the chat UI.
async fn publish_user_followup(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    request_id: &str,
    text: &str,
    _created_at: i64,
    session_id: &str,
    turn_sequence: i64,
    response_offset: i64,
) -> Result<i64, ContentError> {
    // Namespace the request_id to avoid collision with other receipt operations
    let namespaced_request =
        org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let hash = org::digest_parts(&[
        "steer.user.followup",
        &actor.org_id.to_string(),
        chat_id,
        text,
    ]);

    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, chat_id, false).await?;
    access::authorized(&tx, actor, chat_id, false).await?;

    // Check if we already published this message (idempotency)
    if let Some(id) =
        resources::receipt(&tx, actor, "steer.user.followup", &namespaced_request, &hash).await?
    {
        // Already published - retrieve the sequence
        let row = db::one(
            &tx,
            "SELECT sequence FROM neev_messages WHERE org_id=$1 AND id=$2",
            vec![actor.org_id.into(), id.into()],
        )
        .await?
        .ok_or(ContentError::Storage)?;
        let sequence: i64 = row.try_get("", "sequence").map_err(database_error)?;
        db::accept_steer_input(&tx, actor.org_id, session_id, turn_sequence,
            request_id, response_offset).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(sequence);
    }

    // Lock and authorize write access to the chat
    access::lock_resources(&tx, actor, chat_id, false).await?;
    let chat = access::authorized(&tx, actor, chat_id, true).await?;
    if chat.kind != crate::neev::content::types::ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }

    // Get next sequence number
    let sequence = chat
        .content_revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;

    // Charge usage quota
    let bytes = i64::try_from(text.len()).map_err(|_| ContentError::Invalid)?;
    resources::initialize_usage(&tx, actor.org_id).await?;
    let charged = db::execute(
        &tx,
        "UPDATE neev_content_usage SET charged_bytes=charged_bytes+$1,messages=messages+1 WHERE org_id=$2 AND messages<100000 AND charged_bytes<=1073741824-$1",
        vec![bytes.into(), actor.org_id.into()],
    )
    .await?;
    if charged.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }

    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }

    // Insert the message as a human-authored message
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at,author_kind,agent_label) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,$8,'human',NULL)",
        vec![
            actor.org_id.into(),
            chat_id.into(),
            id.clone().into(),
            sequence.into(),
            actor.identity.user_id.into(),
            text.into(),
            bytes.into(),
            now.into(),
        ],
    )
    .await?;

    // Update the chat's content revision
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",
        vec![
            sequence.into(),
            now.into(),
            actor.org_id.into(),
            chat_id.into(),
        ],
    )
    .await?;

    // Record receipt for idempotency
    resources::record_receipt(
        &tx,
        actor,
        "steer.user.followup",
        &namespaced_request,
        &hash,
        &id,
    )
    .await?;

    db::accept_steer_input(&tx, actor.org_id, session_id, turn_sequence,
        request_id, response_offset).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    // Notify subscribers after commit
    state.realtime.publish(actor.org_id, chat_id, sequence);

    Ok(sequence)
}

#[cfg(test)]
#[path = "steering_history_tests.rs"]
mod steering_history_tests;
