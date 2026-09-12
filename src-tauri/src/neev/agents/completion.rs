use super::{
    recovery, store,
    types::{AgentError, ConnectionState, LoginTicket, VerifiedSubscription},
};
use crate::neev::{db, startup::ServerState, types::OrgContext};
use sea_orm::DatabaseTransaction;

async fn lock_ticket(
    state: &ServerState,
    actor: &OrgContext,
    ticket: &LoginTicket,
) -> Result<
    (
        DatabaseTransaction,
        store::ConnectionRow,
        store::AttemptRow,
        i64,
    ),
    AgentError,
> {
    let tx = store::authorized_transaction(state, actor).await?;
    // Native scope is checked before lifecycle state or any foreign resource.
    if ticket.org_id != actor.org_id
        || ticket.user_id != actor.identity.user_id
        || ticket.session_id != actor.identity.session_id
    {
        return Err(AgentError::NotFound);
    }
    let connection = store::lock_connection(&tx, actor, ticket.provider)
        .await?
        .ok_or(AgentError::NotFound)?;
    if connection.id != ticket.connection_id {
        return Err(AgentError::NotFound);
    }
    let attempt = store::lock_attempt(
        &tx,
        &connection.id,
        &actor.identity.session_id,
        &ticket.attempt_id,
    )
    .await?
    .ok_or(AgentError::NotFound)?;
    let operation = store::lock_operation(&tx, &connection.id, &ticket.operation_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    let now = store::now(state)?;
    if connection.state != ConnectionState::Connecting
        || connection.generation != ticket.generation
        || attempt.connection_id != connection.id
        || attempt.generation != ticket.generation
        || attempt.state != "pending"
        || attempt.expires_at != ticket.expires_at
        || attempt.expires_at <= now
        || operation.kind != "connect"
        || operation.target_attempt_id.as_deref() != Some(ticket.attempt_id.as_str())
        || operation.target_generation != ticket.generation
        || operation.result_generation != ticket.generation
        || operation.state != "leased"
        || operation.lease_owner.as_deref() != Some(ticket.lease_owner.as_str())
        || operation
            .lease_expires_at
            .is_none_or(|expiry| expiry <= now)
        || operation.expires_at <= now
    {
        return Err(AgentError::Conflict);
    }
    Ok((tx, connection, attempt, now))
}

pub(crate) async fn complete(
    state: &ServerState,
    actor: &OrgContext,
    ticket: &LoginTicket,
    account: VerifiedSubscription,
) -> Result<(), AgentError> {
    let (tx, connection, attempt, now) = lock_ticket(state, actor, ticket).await?;
    if !store::safe_label(&account.account_label) || !store::simple_id(&account.storage_handle) {
        return Err(AgentError::InvalidInput);
    }
    db::execute(
        &tx,
        "UPDATE neev_agent_connections SET state='connected',account_label=$1,checked_at=$2,storage_handle=$3,error_code=NULL,updated_at=$2 WHERE id=$4 AND org_id=$5 AND user_id=$6 AND generation=$7",
        vec![
            account.account_label.into(),
            now.into(),
            account.storage_handle.into(),
            connection.id.clone().into(),
            actor.org_id.into(),
            actor.identity.user_id.into(),
            ticket.generation.into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    db::execute(
        &tx,
        "UPDATE neev_agent_attempts SET state='completed',updated_at=$1 WHERE connection_id=$2 AND id=$3 AND session_id=$4",
        vec![
            now.into(),
            connection.id.clone().into(),
            attempt.id.into(),
            actor.identity.session_id.clone().into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    db::execute(
        &tx,
        "UPDATE neev_agent_operations SET state='completed',lease_owner=NULL,lease_expires_at=NULL,error_code=NULL,updated_at=$1 WHERE connection_id=$2 AND id=$3",
        vec![
            now.into(),
            connection.id.into(),
            ticket.operation_id.clone().into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    store::commit_authorized(tx, state, actor).await
}

pub(crate) async fn fail(
    state: &ServerState,
    actor: &OrgContext,
    ticket: &LoginTicket,
    error_code: &str,
) -> Result<(), AgentError> {
    let (tx, connection, attempt, _) = lock_ticket(state, actor, ticket).await?;
    if !store::error_code(error_code) {
        return Err(AgentError::InvalidInput);
    }
    let operations = store::lock_active_operations(&tx, &connection.id).await?;
    let now = store::now(state)?;
    if attempt.expires_at <= now
        || operations
            .iter()
            .find(|op| op.id == ticket.operation_id)
            .is_none_or(|op| {
                op.lease_expires_at.is_none_or(|deadline| deadline <= now) || op.expires_at <= now
            })
    {
        return Err(AgentError::Conflict);
    }
    recovery::invalidate_locked(
        &tx,
        &connection,
        &attempt,
        &operations,
        "failed",
        error_code,
        now,
    )
    .await?;
    store::commit_authorized(tx, state, actor).await
}
