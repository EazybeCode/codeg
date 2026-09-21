use super::{
    store,
    types::{
        AgentError, AttemptView, BeginOutcome, ConnectionState, ConnectionView, LoginTicket,
        MutationReceipt, Provider,
    },
};
use crate::neev::{db, startup::ServerState, types::OrgContext};

pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<ConnectionView>, AgentError> {
    let tx = store::authorized_transaction(state, actor).await?;
    let stored = store::lock_connections(&tx, actor).await?;
    let mut result = Vec::with_capacity(2);
    for provider in [Provider::Claude, Provider::Codex] {
        result.push(
            stored
                .iter()
                .find(|row| row.provider == provider)
                .map(store::view)
                .unwrap_or(ConnectionView {
                    provider,
                    state: ConnectionState::Disconnected,
                    account_label: None,
                    checked_at: None,
                    error_code: None,
                }),
        );
    }
    store::commit_authorized(tx, state, actor).await?;
    Ok(result)
}

/// The caller's own credential-volume handle for `provider`, if they have a
/// CONNECTED connection (spec §5.4).
///
/// Returns `None` when the user has not signed this provider in, or when the
/// connection is in any non-connected state — a half-finished or reconnect-
/// required connection has no credential to deliver. The handle identifies the
/// caller's own volume only: it is read from the row keyed by
/// `(org_id, user_id, provider)` taken from the server-authoritative actor, so
/// it can never name another user's credentials.
pub(crate) async fn credential(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
) -> Result<Option<String>, AgentError> {
    let tx = store::authorized_transaction(state, actor).await?;
    let connection = store::lock_connection(&tx, actor, provider).await?;
    let handle = connection.and_then(|row| {
        (row.state == ConnectionState::Connected)
            .then_some(row.storage_handle)
            .flatten()
    });
    store::commit_authorized(tx, state, actor).await?;
    Ok(handle)
}

pub(crate) async fn attempt(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
    attempt_id: &str,
) -> Result<AttemptView, AgentError> {
    let tx = store::authorized_transaction(state, actor).await?;
    let connection = store::lock_connection(&tx, actor, provider)
        .await?
        .ok_or(AgentError::NotFound)?;
    let attempt = store::lock_attempt(&tx, &connection.id, &actor.identity.session_id, attempt_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    let result = AttemptView {
        state: attempt.state,
        expires_at: attempt.expires_at,
    };
    store::commit_authorized(tx, state, actor).await?;
    Ok(result)
}

pub(crate) async fn begin(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
    request_key: &str,
) -> Result<BeginOutcome, AgentError> {
    store::request_key(request_key)?;
    let tx = store::authorized_transaction(state, actor).await?;
    let connection = store::lock_connection(&tx, actor, provider).await?;
    let (connection_id, generation) = if let Some(connection) = connection {
        if let Some(operation) =
            store::lock_operation_key(&tx, &connection.id, "connect", request_key).await?
        {
            let attempt_id = operation
                .target_attempt_id
                .as_deref()
                .ok_or(AgentError::Conflict)?;
            let attempt = store::lock_attempt_for_connection(&tx, &connection.id, attempt_id)
                .await?
                .ok_or(AgentError::Conflict)?;
            if attempt.session_id != actor.identity.session_id {
                return Err(AgentError::Conflict);
            }
            let now = store::now(state)?;
            if attempt.state == "pending"
                && connection.state == ConnectionState::Connecting
                && attempt.generation == connection.generation
                && attempt.expires_at > now
                && operation.state == "leased"
                && operation.target_generation == connection.generation
                && operation.result_generation == connection.generation
                && operation
                    .lease_expires_at
                    .is_some_and(|deadline| deadline > now)
                && operation.expires_at > now
            {
                let ticket = LoginTicket {
                    connection_id: connection.id,
                    provider,
                    generation: connection.generation,
                    org_id: actor.org_id,
                    user_id: actor.identity.user_id,
                    session_id: actor.identity.session_id.clone(),
                    attempt_id: attempt.id,
                    operation_id: operation.id,
                    lease_owner: operation.lease_owner.ok_or(AgentError::Conflict)?,
                    expires_at: attempt.expires_at,
                };
                store::commit_authorized(tx, state, actor).await?;
                return Ok(BeginOutcome::Pending(ticket));
            }
            if attempt.state == "completed"
                && operation.state == "completed"
                && attempt.generation == connection.generation
                && operation.target_generation == connection.generation
                && operation.result_generation == connection.generation
            {
                let result = BeginOutcome::Settled(store::view(&connection));
                store::commit_authorized(tx, state, actor).await?;
                return Ok(result);
            }
            return Err(AgentError::Conflict);
        }
        let active = store::lock_active_operations(&tx, &connection.id).await?;
        if connection.state != ConnectionState::Disconnected
            || connection.storage_handle.is_some()
            || !active.is_empty()
        {
            return Err(AgentError::Conflict);
        }
        (
            connection.id,
            store::checked_generation(connection.generation)?,
        )
    } else {
        (store::random_id(), 1)
    };

    let now = store::now(state)?;
    let threshold = now.checked_sub(900).ok_or(AgentError::InvalidInput)?;
    let count = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_agent_attempts a JOIN neev_agent_connections c ON c.id=a.connection_id WHERE c.user_id=$1 AND a.created_at>$2",
        vec![actor.identity.user_id.into(), threshold.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?
    .ok_or(AgentError::Unavailable)?
    .try_get::<i64>("", "n")
    .map_err(|_| AgentError::Unavailable)?;
    if count >= 5 {
        return Err(AgentError::RateLimited);
    }
    let expires_at = store::checked_deadline(now)?;
    let attempt_id = store::random_id();
    let operation_id = store::random_id();
    let lease_owner = store::random_id();
    if generation == 1 {
        db::execute(
            &tx,
            "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,1,'connecting',NULL,NULL,NULL,NULL,$5,$5)",
            vec![connection_id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), store::provider_name(provider).into(), now.into()],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
    } else {
        let previous = generation - 1;
        let changed = db::execute(
            &tx,
            "UPDATE neev_agent_connections SET generation=$1,state='connecting',account_label=NULL,checked_at=NULL,error_code=NULL,storage_handle=NULL,updated_at=$2 WHERE id=$3 AND org_id=$4 AND user_id=$5 AND generation=$6 AND state='disconnected' AND storage_handle IS NULL",
            vec![generation.into(), now.into(), connection_id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), previous.into()],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
        if changed.rows_affected() != 1 {
            return Err(AgentError::Conflict);
        }
    }
    db::execute(
        &tx,
        "INSERT INTO neev_agent_attempts(id,connection_id,generation,session_id,state,created_at,expires_at,updated_at) VALUES($1,$2,$3,$4,'pending',$5,$6,$5)",
        vec![attempt_id.clone().into(), connection_id.clone().into(), generation.into(), actor.identity.session_id.clone().into(), now.into(), expires_at.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    db::execute(
        &tx,
        "INSERT INTO neev_agent_operations(id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,error_code,created_at,expires_at,updated_at) VALUES($1,$2,'connect',$3,$4,$5,$5,'leased',$6,$7,NULL,$8,$7,$8)",
        vec![operation_id.clone().into(), connection_id.clone().into(), request_key.into(), attempt_id.clone().into(), generation.into(), lease_owner.clone().into(), expires_at.into(), now.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    let ticket = LoginTicket {
        connection_id,
        provider,
        generation,
        org_id: actor.org_id,
        user_id: actor.identity.user_id,
        session_id: actor.identity.session_id.clone(),
        attempt_id,
        operation_id,
        lease_owner,
        expires_at,
    };
    store::commit_authorized(tx, state, actor).await?;
    Ok(BeginOutcome::Pending(ticket))
}

pub(crate) async fn cancel(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
    attempt_id: &str,
    request_key: &str,
) -> Result<MutationReceipt, AgentError> {
    store::request_key(request_key)?;
    let tx = store::authorized_transaction(state, actor).await?;
    let connection = store::lock_connection(&tx, actor, provider)
        .await?
        .ok_or(AgentError::NotFound)?;
    if let Some(operation) =
        store::lock_operation_key(&tx, &connection.id, "cancel", request_key).await?
    {
        if operation.target_attempt_id.as_deref() != Some(attempt_id) {
            return Err(AgentError::Conflict);
        }
        let attempt =
            store::lock_attempt(&tx, &connection.id, &actor.identity.session_id, attempt_id)
                .await?
                .ok_or(AgentError::NotFound)?;
        if operation.target_generation != attempt.generation
            || operation.result_generation != connection.generation
        {
            return Err(AgentError::Conflict);
        }
        let receipt = MutationReceipt {
            operation_id: operation.id,
            generation: operation.result_generation,
        };
        store::commit_authorized(tx, state, actor).await?;
        return Ok(receipt);
    }
    let attempt = store::lock_attempt(&tx, &connection.id, &actor.identity.session_id, attempt_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    let active = store::lock_active_operations(&tx, &connection.id).await?;
    if connection.state != ConnectionState::Connecting
        || connection.generation != attempt.generation
        || attempt.state != "pending"
        || active
            .iter()
            .any(|operation| matches!(operation.kind.as_str(), "cancel" | "disconnect"))
    {
        return Err(AgentError::Conflict);
    }
    let connect = active
        .iter()
        .find(|operation| {
            operation.kind == "connect"
                && operation.target_attempt_id.as_deref() == Some(attempt_id)
                && operation.target_generation == attempt.generation
                && operation.result_generation == attempt.generation
        })
        .ok_or(AgentError::Conflict)?;
    if active
        .iter()
        .filter(|operation| operation.kind == "connect")
        .count()
        != 1
        || connect.state != "leased"
    {
        return Err(AgentError::Conflict);
    }
    let now = store::now(state)?;
    let deadline = store::checked_deadline(now)?;
    let generation = store::checked_generation(connection.generation)?;
    let operation_id = store::random_id();
    let changed = db::execute(
        &tx,
        "UPDATE neev_agent_attempts SET state='canceled',updated_at=$1 WHERE connection_id=$2 AND id=$3 AND generation=$4 AND session_id=$5 AND state='pending'",
        vec![now.into(), connection.id.clone().into(), attempt.id.clone().into(), attempt.generation.into(), attempt.session_id.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(AgentError::Conflict);
    }
    let changed = db::execute(
        &tx,
        "UPDATE neev_agent_operations SET state='abandoned',lease_owner=NULL,lease_expires_at=NULL,updated_at=$1 WHERE id=$2 AND connection_id=$3 AND kind='connect' AND target_attempt_id=$4 AND target_generation=$5 AND result_generation=$5 AND state='leased'",
        vec![now.into(), connect.id.clone().into(), connection.id.clone().into(), attempt.id.clone().into(), attempt.generation.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(AgentError::Conflict);
    }
    let changed = db::execute(
        &tx,
        "UPDATE neev_agent_connections SET generation=$1,state='disconnecting',account_label=NULL,checked_at=NULL,error_code=NULL,updated_at=$2 WHERE id=$3 AND org_id=$4 AND user_id=$5 AND generation=$6 AND state='connecting'",
        vec![generation.into(), now.into(), connection.id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), connection.generation.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(AgentError::Conflict);
    }
    db::execute(
        &tx,
        "INSERT INTO neev_agent_operations(id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,error_code,created_at,expires_at,updated_at) VALUES($1,$2,'cancel',$3,$4,$5,$6,'queued',NULL,NULL,NULL,$7,$8,$7)",
        vec![operation_id.clone().into(), connection.id.into(), request_key.into(), attempt.id.into(), attempt.generation.into(), generation.into(), now.into(), deadline.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    store::commit_authorized(tx, state, actor).await?;
    Ok(MutationReceipt {
        operation_id,
        generation,
    })
}

pub(crate) async fn disconnect(
    state: &ServerState,
    actor: &OrgContext,
    provider: Provider,
    request_key: &str,
) -> Result<MutationReceipt, AgentError> {
    store::request_key(request_key)?;
    let tx = store::authorized_transaction(state, actor).await?;
    let connection = store::lock_connection(&tx, actor, provider)
        .await?
        .ok_or(AgentError::NotFound)?;
    if let Some(operation) =
        store::lock_operation_key(&tx, &connection.id, "disconnect", request_key).await?
    {
        let receipt = MutationReceipt {
            operation_id: operation.id,
            generation: operation.result_generation,
        };
        store::commit_authorized(tx, state, actor).await?;
        return Ok(receipt);
    }
    let attempts = store::lock_pending_attempts(&tx, &connection.id).await?;
    let active = store::lock_active_operations(&tx, &connection.id).await?;
    if active
        .iter()
        .any(|operation| matches!(operation.kind.as_str(), "cancel" | "disconnect"))
    {
        return Err(AgentError::Conflict);
    }
    let now = store::now(state)?;
    let deadline = store::checked_deadline(now)?;
    let generation = store::checked_generation(connection.generation)?;
    let immediate = connection.state == ConnectionState::Disconnected
        && connection.storage_handle.is_none()
        && attempts.is_empty()
        && active.is_empty();
    for attempt in &attempts {
        let changed = db::execute(
            &tx,
            "UPDATE neev_agent_attempts SET state='canceled',updated_at=$1 WHERE connection_id=$2 AND id=$3 AND state='pending'",
            vec![now.into(), connection.id.clone().into(), attempt.id.clone().into()],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
        if changed.rows_affected() != 1 {
            return Err(AgentError::Conflict);
        }
    }
    for operation in &active {
        if operation.kind != "connect" {
            return Err(AgentError::Conflict);
        }
        let changed = db::execute(
            &tx,
            "UPDATE neev_agent_operations SET state='abandoned',lease_owner=NULL,lease_expires_at=NULL,updated_at=$1 WHERE id=$2 AND connection_id=$3 AND kind='connect' AND state='leased'",
            vec![now.into(), operation.id.clone().into(), connection.id.clone().into()],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
        if changed.rows_affected() != 1 {
            return Err(AgentError::Conflict);
        }
    }
    let next_state = if immediate {
        "disconnected"
    } else {
        "disconnecting"
    };
    let changed = db::execute(
        &tx,
        "UPDATE neev_agent_connections SET generation=$1,state=$2,account_label=NULL,checked_at=NULL,error_code=NULL,storage_handle=CASE WHEN $2='disconnected' THEN NULL ELSE storage_handle END,updated_at=$3 WHERE id=$4 AND org_id=$5 AND user_id=$6 AND generation=$7",
        vec![generation.into(), next_state.into(), now.into(), connection.id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), connection.generation.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(AgentError::Conflict);
    }
    let operation_id = store::random_id();
    let operation_state = if immediate { "completed" } else { "queued" };
    db::execute(
        &tx,
        "INSERT INTO neev_agent_operations(id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,error_code,created_at,expires_at,updated_at) VALUES($1,$2,'disconnect',$3,NULL,$4,$5,$6,NULL,NULL,NULL,$7,$8,$7)",
        vec![operation_id.clone().into(), connection.id.into(), request_key.into(), connection.generation.into(), generation.into(), operation_state.into(), now.into(), deadline.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    store::commit_authorized(tx, state, actor).await?;
    Ok(MutationReceipt {
        operation_id,
        generation,
    })
}
