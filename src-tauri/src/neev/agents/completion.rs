use super::{
    recovery, store,
    types::{AgentError, ConnectionState, LoginTicket, VerifiedSubscription},
};
use crate::neev::{db, startup::ServerState, types::OrgContext};
use sea_orm::DatabaseTransaction;

/// Untrusted values used only to find and compare native committed state.
pub(crate) struct CompletionLookup {
    org_id: i32,
    user_id: i32,
    provider: super::types::Provider,
    connection_id: String,
    generation: i64,
    attempt_id: String,
    operation_id: String,
    storage_handle: String,
}

impl CompletionLookup {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        org_id: i32,
        user_id: i32,
        provider: super::types::Provider,
        connection_id: String,
        generation: i64,
        attempt_id: String,
        operation_id: String,
        storage_handle: String,
    ) -> Result<Self, AgentError> {
        if org_id <= 0
            || user_id <= 0
            || generation <= 0
            || !store::simple_id(&connection_id)
            || !store::simple_id(&attempt_id)
            || !store::simple_id(&operation_id)
            || !store::simple_id(&storage_handle)
        {
            return Err(AgentError::InvalidInput);
        }
        Ok(Self {
            org_id,
            user_id,
            provider,
            connection_id,
            generation,
            attempt_id,
            operation_id,
            storage_handle,
        })
    }
}

/// Historical proof of an exact native login commit. It grants no current authority.
pub(crate) struct CommittedLoginWitness {
    org_id: i32,
    user_id: i32,
    provider: super::types::Provider,
    connection_id: String,
    generation: i64,
    session_id: String,
    attempt_id: String,
    operation_id: String,
    storage_handle: String,
    completed_at: i64,
}

#[allow(dead_code)] // Consumed by the later reviewed coordinator adapter.
impl CommittedLoginWitness {
    pub(crate) fn org_id(&self) -> i32 {
        self.org_id
    }

    pub(crate) fn user_id(&self) -> i32 {
        self.user_id
    }

    pub(crate) fn provider(&self) -> super::types::Provider {
        self.provider
    }

    pub(crate) fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub(crate) fn generation(&self) -> i64 {
        self.generation
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn storage_handle(&self) -> &str {
        &self.storage_handle
    }

    pub(crate) fn completed_at(&self) -> i64 {
        self.completed_at
    }
}

#[allow(dead_code)] // Consumed by the later reviewed coordinator adapter.
pub(crate) async fn read_completed_witness(
    state: &ServerState,
    expected: &CompletionLookup,
) -> Result<Option<CommittedLoginWitness>, AgentError> {
    let tx = db::begin(&state.database)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    db::lock_admission_guard(&tx)
        .await
        .map_err(|_| AgentError::Unavailable)?;

    let mut witness = None;
    if let Some(connection) = store::lock_owned_connection(
        &tx,
        &expected.connection_id,
        expected.org_id,
        expected.user_id,
    )
    .await?
    {
        if connection.provider == expected.provider
            && connection.generation == expected.generation
            && matches!(
                connection.state,
                ConnectionState::Connected | ConnectionState::ReconnectRequired
            )
            && connection.storage_handle.as_deref() == Some(expected.storage_handle.as_str())
        {
            if let Some(attempt) =
                store::lock_attempt_for_connection(&tx, &connection.id, &expected.attempt_id)
                    .await?
            {
                if attempt.connection_id == connection.id
                    && attempt.generation == expected.generation
                    && attempt.state == "completed"
                {
                    if let Some(operation) =
                        store::lock_operation(&tx, &connection.id, &expected.operation_id).await?
                    {
                        let completed_at = attempt.updated_at;
                        if operation.connection_id == connection.id
                            && operation.kind == "connect"
                            && operation.target_attempt_id.as_deref()
                                == Some(expected.attempt_id.as_str())
                            && operation.target_generation == expected.generation
                            && operation.result_generation == expected.generation
                            && operation.state == "completed"
                            && operation.lease_owner.is_none()
                            && operation.lease_expires_at.is_none()
                            && operation.updated_at == completed_at
                            && completed_at >= attempt.created_at
                            && completed_at < attempt.expires_at
                            && completed_at < operation.expires_at
                        {
                            witness = Some(CommittedLoginWitness {
                                org_id: connection.org_id,
                                user_id: connection.user_id,
                                provider: connection.provider,
                                connection_id: connection.id,
                                generation: connection.generation,
                                session_id: attempt.session_id,
                                attempt_id: attempt.id,
                                operation_id: operation.id,
                                storage_handle: expected.storage_handle.clone(),
                                completed_at,
                            });
                        }
                    }
                }
            }
        }
    }
    tx.commit().await.map_err(|_| AgentError::Unavailable)?;
    Ok(witness)
}

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
    // A new sign-in is a different account: nothing observed before applies.
    state
        .agent_usage
        .forget(actor.org_id, actor.identity.user_id, ticket.provider.name());
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
