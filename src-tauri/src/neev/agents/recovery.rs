use super::{
    store,
    types::{AgentError, CleanupTicket, ConnectionState},
};
use crate::neev::{clock::Clock, db, startup::ServerState};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

fn now(clock: &dyn Clock) -> Result<i64, AgentError> {
    let now = clock.now_unix();
    if now < 0 {
        Err(AgentError::InvalidInput)
    } else {
        Ok(now)
    }
}

async fn cleanup_rows(
    tx: &DatabaseTransaction,
    operation_id: &str,
) -> Result<Option<(store::ConnectionRow, store::OperationRow)>, AgentError> {
    let Some(row) = db::one(
        tx,
        "SELECT connection_id FROM neev_agent_operations WHERE id=$1",
        vec![operation_id.into()],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?
    else {
        return Ok(None);
    };
    let connection_id: String = row
        .try_get("", "connection_id")
        .map_err(|_| AgentError::Unavailable)?;
    let (org_id, user_id) = store::stored_ownership(tx, &connection_id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    let connection = store::lock_owned_connection(tx, &connection_id, org_id, user_id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    let operation = store::lock_operation(tx, &connection.id, operation_id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    Ok(Some((connection, operation)))
}

pub(crate) async fn claim_cleanup(
    state: &ServerState,
    operation_id: &str,
) -> Result<Option<CleanupTicket>, AgentError> {
    let tx = db::begin(&state.database)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    db::lock_admission_guard(&tx)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    let Some((connection, operation)) = cleanup_rows(&tx, operation_id).await? else {
        return Ok(None);
    };
    let now = now(state.clock.as_ref())?;
    if !matches!(operation.kind.as_str(), "cancel" | "disconnect")
        || !(operation.state == "queued"
            || operation.state == "leased"
                && operation
                    .lease_expires_at
                    .is_some_and(|deadline| deadline <= now))
    {
        return Ok(None);
    }
    let deadline = now.checked_add(30).ok_or(AgentError::InvalidInput)?;
    let owner = store::random_id();
    db::execute(
        &tx,
        "UPDATE neev_agent_operations SET state='leased',lease_owner=$1,lease_expires_at=$2,updated_at=$3 WHERE connection_id=$4 AND id=$5",
        vec![
            owner.clone().into(),
            deadline.into(),
            now.into(),
            connection.id.clone().into(),
            operation.id.clone().into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    let ticket = CleanupTicket {
        operation_id: operation.id,
        connection_id: connection.id,
        provider: connection.provider,
        org_id: connection.org_id,
        user_id: connection.user_id,
        target_generation: operation.target_generation,
        result_generation: operation.result_generation,
        lease_owner: owner,
        lease_expires_at: deadline,
    };
    tx.commit().await.map_err(|_| AgentError::Unavailable)?;
    Ok(Some(ticket))
}

pub(crate) async fn finish_cleanup(
    state: &ServerState,
    ticket: &CleanupTicket,
    succeeded: bool,
) -> Result<(), AgentError> {
    let tx = db::begin(&state.database)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    db::lock_admission_guard(&tx)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    let (connection, operation) = cleanup_rows(&tx, &ticket.operation_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    let now = now(state.clock.as_ref())?;
    if connection.id != ticket.connection_id
        || connection.provider != ticket.provider
        || connection.org_id != ticket.org_id
        || connection.user_id != ticket.user_id
        || operation.target_generation != ticket.target_generation
        || operation.result_generation != ticket.result_generation
        || !matches!(operation.kind.as_str(), "cancel" | "disconnect")
        || operation.state != "leased"
        || operation.lease_owner.as_deref() != Some(ticket.lease_owner.as_str())
        || operation.lease_expires_at != Some(ticket.lease_expires_at)
        || ticket.lease_expires_at <= now
    {
        return Err(AgentError::Conflict);
    }
    let (status, error) = if succeeded {
        ("completed", None)
    } else {
        ("queued", Some("cleanup_failed"))
    };
    db::execute(
        &tx,
        "UPDATE neev_agent_operations SET state=$1,lease_owner=NULL,lease_expires_at=NULL,error_code=$2,updated_at=$3 WHERE connection_id=$4 AND id=$5",
        vec![
            status.into(),
            error.into(),
            now.into(),
            connection.id.clone().into(),
            operation.id.into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    // The cleanup can settle its old operation, but it never owns a newer connection.
    if connection.generation == ticket.result_generation
        && connection.state == ConnectionState::Disconnecting
    {
        if succeeded {
            db::execute(
                &tx,
                "UPDATE neev_agent_connections SET state='disconnected',account_label=NULL,checked_at=NULL,storage_handle=NULL,error_code=NULL,updated_at=$1 WHERE id=$2 AND org_id=$3 AND user_id=$4 AND generation=$5 AND state='disconnecting'",
                vec![
                    now.into(),
                    connection.id.into(),
                    connection.org_id.into(),
                    connection.user_id.into(),
                    ticket.result_generation.into(),
                ],
            )
            .await
            .map_err(|_| AgentError::Unavailable)?;
        } else {
            db::execute(
                &tx,
                "UPDATE neev_agent_connections SET error_code='cleanup_failed',updated_at=$1 WHERE id=$2 AND org_id=$3 AND user_id=$4 AND generation=$5 AND state='disconnecting'",
                vec![
                    now.into(),
                    connection.id.into(),
                    connection.org_id.into(),
                    connection.user_id.into(),
                    ticket.result_generation.into(),
                ],
            )
            .await
            .map_err(|_| AgentError::Unavailable)?;
        }
    }
    tx.commit().await.map_err(|_| AgentError::Unavailable)
}

/// Caller holds connection, attempt and operation locks, and sampled time afterward.
pub(super) async fn invalidate_locked(
    tx: &DatabaseTransaction,
    connection: &store::ConnectionRow,
    attempt: &store::AttemptRow,
    operations: &[store::OperationRow],
    attempt_state: &str,
    error: &str,
    now: i64,
) -> Result<(), AgentError> {
    if attempt.state != "pending" {
        return Ok(());
    }
    let current = connection.generation == attempt.generation
        && connection.state == ConnectionState::Connecting;
    // Compute every fallible arithmetic value before writing; transaction rollback is still atomic.
    let next = if current {
        Some((
            store::checked_generation(connection.generation)?,
            store::checked_deadline(now)?,
        ))
    } else {
        None
    };
    db::execute(
        tx,
        "UPDATE neev_agent_attempts SET state=$1,updated_at=$2 WHERE connection_id=$3 AND id=$4 AND session_id=$5 AND state='pending'",
        vec![
            attempt_state.into(),
            now.into(),
            connection.id.clone().into(),
            attempt.id.clone().into(),
            attempt.session_id.clone().into(),
        ],
    )
    .await
    .map_err(|_| AgentError::Unavailable)?;
    for operation in operations.iter().filter(|op| {
        op.kind == "connect"
            && op.target_attempt_id.as_deref() == Some(attempt.id.as_str())
            && op.target_generation == attempt.generation
            && op.result_generation == attempt.generation
    }) {
        db::execute(
            tx,
            "UPDATE neev_agent_operations SET state='abandoned',lease_owner=NULL,lease_expires_at=NULL,error_code=$1,updated_at=$2 WHERE connection_id=$3 AND id=$4",
            vec![
                error.into(),
                now.into(),
                connection.id.clone().into(),
                operation.id.clone().into(),
            ],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
    }
    if let Some((generation, deadline)) = next {
        db::execute(
            tx,
            "UPDATE neev_agent_connections SET generation=$1,state='disconnecting',account_label=NULL,checked_at=NULL,error_code=$2,updated_at=$3 WHERE id=$4 AND org_id=$5 AND user_id=$6 AND generation=$7 AND state='connecting'",
            vec![
                generation.into(),
                error.into(),
                now.into(),
                connection.id.clone().into(),
                connection.org_id.into(),
                connection.user_id.into(),
                attempt.generation.into(),
            ],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
        db::execute(
            tx,
            "INSERT INTO neev_agent_operations(id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,error_code,created_at,expires_at,updated_at) VALUES($1,$2,'disconnect',$3,NULL,$4,$5,'queued',NULL,NULL,NULL,$6,$7,$6)",
            vec![
                store::random_id().into(),
                connection.id.clone().into(),
                format!("__neev_{}",attempt.id).into(),
                attempt.generation.into(),
                generation.into(),
                now.into(),
                deadline.into(),
            ],
        )
        .await
        .map_err(|_| AgentError::Unavailable)?;
    }
    Ok(())
}

pub(crate) async fn expire_pending(state: &ServerState) -> Result<u64, AgentError> {
    recover(state, false).await
}

/// Explicitly called by the future single supervisor; constructors must not run this.
pub(crate) async fn recover_startup(state: &ServerState) -> Result<u64, AgentError> {
    recover(state, true).await
}

async fn recover(state: &ServerState, startup: bool) -> Result<u64, AgentError> {
    let mut cursor = String::new();
    let mut count = 0;
    loop {
        let scan_time = now(state.clock.as_ref())?;
        let rows=state.database.query_all(db::statement(&state.database,"SELECT id,connection_id FROM neev_agent_attempts WHERE state='pending' AND id>$1 AND ($2 OR expires_at<=$3) ORDER BY id LIMIT 100",vec![cursor.clone().into(),startup.into(),scan_time.into()])).await.map_err(|_|AgentError::Unavailable)?;
        if rows.is_empty() {
            return Ok(count);
        }
        for row in rows {
            let id: String = row.try_get("", "id").map_err(|_| AgentError::Unavailable)?;
            let connection_id: String = row
                .try_get("", "connection_id")
                .map_err(|_| AgentError::Unavailable)?;
            cursor = id.clone();
            let tx = db::begin(&state.database)
                .await
                .map_err(|_| AgentError::Unavailable)?;
            db::lock_admission_guard(&tx)
                .await
                .map_err(|_| AgentError::Unavailable)?;
            let (org, user) = store::stored_ownership(&tx, &connection_id)
                .await?
                .ok_or(AgentError::Unavailable)?;
            let connection = store::lock_owned_connection(&tx, &connection_id, org, user)
                .await?
                .ok_or(AgentError::Unavailable)?;
            let attempt = store::lock_attempt_for_connection(&tx, &connection.id, &id)
                .await?
                .ok_or(AgentError::Unavailable)?;
            let operations = store::lock_active_operations(&tx, &connection.id).await?;
            let now = now(state.clock.as_ref())?;
            if attempt.state == "pending" && (startup || attempt.expires_at <= now) {
                invalidate_locked(
                    &tx,
                    &connection,
                    &attempt,
                    &operations,
                    if startup { "failed" } else { "expired" },
                    if startup { "interrupted" } else { "expired" },
                    now,
                )
                .await?;
                count += 1;
            }
            tx.commit().await.map_err(|_| AgentError::Unavailable)?;
        }
    }
}

/// Revoke already owns admission/account/session locks. Never reacquire authority here.
pub(crate) async fn invalidate_session_in_transaction(
    tx: &DatabaseTransaction,
    session_id: &str,
    clock: &dyn Clock,
) -> Result<(), AgentError> {
    let rows=tx.query_all(db::statement(tx,"SELECT DISTINCT connection_id FROM neev_agent_attempts WHERE session_id=$1 AND state='pending' ORDER BY connection_id",vec![session_id.into()])).await.map_err(|_|AgentError::Unavailable)?;
    let mut connections = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row
            .try_get("", "connection_id")
            .map_err(|_| AgentError::Unavailable)?;
        let (org, user) = store::stored_ownership(tx, &id)
            .await?
            .ok_or(AgentError::Unavailable)?;
        connections.push(
            store::lock_owned_connection(tx, &id, org, user)
                .await?
                .ok_or(AgentError::Unavailable)?,
        );
    }
    for connection in connections {
        let attempts = store::lock_pending_attempts(tx, &connection.id).await?;
        let operations = store::lock_active_operations(tx, &connection.id).await?;
        let now = now(clock)?;
        for attempt in attempts
            .iter()
            .filter(|attempt| attempt.session_id == session_id)
        {
            invalidate_locked(
                tx,
                &connection,
                attempt,
                &operations,
                "canceled",
                "canceled",
                now,
            )
            .await?;
        }
    }
    Ok(())
}
