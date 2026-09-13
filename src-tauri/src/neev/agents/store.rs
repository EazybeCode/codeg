use crate::neev::{
    auth::session, db, error::database_error, startup::ServerState, types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult};

use super::types::{AgentError, ConnectionState, ConnectionView, Provider};

pub(super) struct ConnectionRow {
    pub(super) id: String,
    pub(super) org_id: i32,
    pub(super) user_id: i32,
    pub(super) provider: Provider,
    pub(super) generation: i64,
    pub(super) state: ConnectionState,
    pub(super) account_label: Option<String>,
    pub(super) checked_at: Option<i64>,
    pub(super) error_code: Option<String>,
    pub(super) storage_handle: Option<String>,
}

pub(super) struct AttemptRow {
    pub(super) id: String,
    pub(super) connection_id: String,
    pub(super) generation: i64,
    pub(super) session_id: String,
    pub(super) state: String,
    pub(super) created_at: i64,
    pub(super) expires_at: i64,
    pub(super) updated_at: i64,
}

pub(super) struct OperationRow {
    pub(super) id: String,
    pub(super) connection_id: String,
    pub(super) kind: String,
    pub(super) request_key: String,
    pub(super) target_attempt_id: Option<String>,
    pub(super) target_generation: i64,
    pub(super) result_generation: i64,
    pub(super) state: String,
    pub(super) lease_owner: Option<String>,
    pub(super) lease_expires_at: Option<i64>,
    pub(super) expires_at: i64,
    pub(super) updated_at: i64,
}

fn unavailable<T>(_error: T) -> AgentError {
    AgentError::Unavailable
}

fn lock_suffix<C: ConnectionTrait>(db: &C) -> &'static str {
    if db.get_database_backend() == DbBackend::Postgres {
        " FOR UPDATE"
    } else {
        ""
    }
}

pub(super) fn simple_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn safe_label(value: &str) -> bool {
    (1..=160).contains(&value.chars().count())
        && !value.chars().any(|ch| {
            ch.is_control()
                || matches!(
                    ch,
                    '\u{061c}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
}

pub(super) fn error_code(value: &str) -> bool {
    matches!(
        value,
        "provider_unavailable"
            | "reauth_required"
            | "unsupported_account"
            | "expired"
            | "canceled"
            | "cleanup_failed"
            | "interrupted"
    )
}

pub(super) fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn decode_provider(value: &str) -> Result<Provider, AgentError> {
    match value {
        "claude" => Ok(Provider::Claude),
        "codex" => Ok(Provider::Codex),
        _ => Err(AgentError::Unavailable),
    }
}

fn decode_state(value: &str) -> Result<ConnectionState, AgentError> {
    match value {
        "disconnected" => Ok(ConnectionState::Disconnected),
        "connecting" => Ok(ConnectionState::Connecting),
        "connected" => Ok(ConnectionState::Connected),
        "reconnect_required" => Ok(ConnectionState::ReconnectRequired),
        "disconnecting" => Ok(ConnectionState::Disconnecting),
        "unavailable" => Ok(ConnectionState::Unavailable),
        _ => Err(AgentError::Unavailable),
    }
}

fn connection(row: &QueryResult) -> Result<ConnectionRow, AgentError> {
    let id: String = row.try_get("", "id").map_err(unavailable)?;
    let org_id: i32 = row.try_get("", "org_id").map_err(unavailable)?;
    let user_id: i32 = row.try_get("", "user_id").map_err(unavailable)?;
    let provider_value: String = row.try_get("", "provider").map_err(unavailable)?;
    let generation: i64 = row.try_get("", "generation").map_err(unavailable)?;
    let state_value: String = row.try_get("", "state").map_err(unavailable)?;
    let account_label: Option<String> = row.try_get("", "account_label").map_err(unavailable)?;
    let checked_at: Option<i64> = row.try_get("", "checked_at").map_err(unavailable)?;
    let error: Option<String> = row.try_get("", "error_code").map_err(unavailable)?;
    let storage_handle: Option<String> = row.try_get("", "storage_handle").map_err(unavailable)?;
    let provider = decode_provider(&provider_value)?;
    let state = decode_state(&state_value)?;
    if !simple_id(&id)
        || org_id <= 0
        || user_id <= 0
        || generation <= 0
        || checked_at.is_some_and(|value| value < 0)
        || error.as_deref().is_some_and(|value| !error_code(value))
        || storage_handle
            .as_deref()
            .is_some_and(|value| !simple_id(value))
        || account_label
            .as_deref()
            .is_some_and(|value| !safe_label(value))
    {
        return Err(AgentError::Unavailable);
    }
    match state {
        ConnectionState::Connected
            if account_label.is_some()
                && checked_at.is_some()
                && storage_handle.is_some()
                && error.is_none() => {}
        ConnectionState::Disconnected
            if account_label.is_none()
                && checked_at.is_none()
                && storage_handle.is_none()
                && error.is_none() => {}
        ConnectionState::Connecting
        | ConnectionState::ReconnectRequired
        | ConnectionState::Disconnecting
        | ConnectionState::Unavailable
            if account_label.is_none() && checked_at.is_none() => {}
        _ => return Err(AgentError::Unavailable),
    }
    Ok(ConnectionRow {
        id,
        org_id,
        user_id,
        provider,
        generation,
        state,
        account_label,
        checked_at,
        error_code: error,
        storage_handle,
    })
}

fn attempt_row(row: &QueryResult) -> Result<AttemptRow, AgentError> {
    let result = AttemptRow {
        id: row.try_get("", "id").map_err(unavailable)?,
        connection_id: row.try_get("", "connection_id").map_err(unavailable)?,
        generation: row.try_get("", "generation").map_err(unavailable)?,
        session_id: row.try_get("", "session_id").map_err(unavailable)?,
        state: row.try_get("", "state").map_err(unavailable)?,
        created_at: row.try_get("", "created_at").map_err(unavailable)?,
        expires_at: row.try_get("", "expires_at").map_err(unavailable)?,
        updated_at: row.try_get("", "updated_at").map_err(unavailable)?,
    };
    if !simple_id(&result.id)
        || !simple_id(&result.connection_id)
        || result.generation <= 0
        || result.session_id.is_empty()
        || result.session_id.len() > 128
        || !matches!(
            result.state.as_str(),
            "pending" | "completed" | "canceled" | "expired" | "failed"
        )
        || result.created_at < 0
        || result.expires_at <= result.created_at
        || result.updated_at < result.created_at
    {
        return Err(AgentError::Unavailable);
    }
    Ok(result)
}

/// Stored ownership permits cleanup without manufacturing native member authority.
pub(super) async fn lock_owned_connection(
    tx: &DatabaseTransaction,
    id: &str,
    org_id: i32,
    user_id: i32,
) -> Result<Option<ConnectionRow>, AgentError> {
    let sql = format!("SELECT id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle FROM neev_agent_connections WHERE id=$1 AND org_id=$2 AND user_id=$3{}", lock_suffix(tx));
    db::one(tx, &sql, vec![id.into(), org_id.into(), user_id.into()])
        .await
        .map_err(unavailable)?
        .map(|row| connection(&row))
        .transpose()
}

pub(super) async fn stored_ownership<C: ConnectionTrait>(
    database: &C,
    connection_id: &str,
) -> Result<Option<(i32, i32)>, AgentError> {
    db::one(
        database,
        "SELECT org_id,user_id FROM neev_agent_connections WHERE id=$1",
        vec![connection_id.into()],
    )
    .await
    .map_err(unavailable)?
    .map(|row| {
        Ok((
            row.try_get("", "org_id").map_err(unavailable)?,
            row.try_get("", "user_id").map_err(unavailable)?,
        ))
    })
    .transpose()
}

fn operation_row(row: &QueryResult) -> Result<OperationRow, AgentError> {
    let result = OperationRow {
        id: row.try_get("", "id").map_err(unavailable)?,
        connection_id: row.try_get("", "connection_id").map_err(unavailable)?,
        kind: row.try_get("", "kind").map_err(unavailable)?,
        request_key: row.try_get("", "request_key").map_err(unavailable)?,
        target_attempt_id: row.try_get("", "target_attempt_id").map_err(unavailable)?,
        target_generation: row.try_get("", "target_generation").map_err(unavailable)?,
        result_generation: row.try_get("", "result_generation").map_err(unavailable)?,
        state: row.try_get("", "state").map_err(unavailable)?,
        lease_owner: row.try_get("", "lease_owner").map_err(unavailable)?,
        lease_expires_at: row.try_get("", "lease_expires_at").map_err(unavailable)?,
        expires_at: row.try_get("", "expires_at").map_err(unavailable)?,
        updated_at: row.try_get("", "updated_at").map_err(unavailable)?,
    };
    if !simple_id(&result.id)
        || !simple_id(&result.connection_id)
        || !matches!(result.kind.as_str(), "connect" | "cancel" | "disconnect")
        || result.request_key.is_empty()
        || result.request_key.len() > 64
        || result
            .target_attempt_id
            .as_deref()
            .is_some_and(|value| !simple_id(value))
        || result.target_generation <= 0
        || result.result_generation < result.target_generation
        || !matches!(
            result.state.as_str(),
            "queued" | "leased" | "completed" | "abandoned"
        )
        || result
            .lease_owner
            .as_deref()
            .is_some_and(|value| !simple_id(value))
        || result.lease_expires_at.is_some_and(|value| value < 0)
        || result.expires_at < 0
        || result.updated_at < 0
        || (if result.state == "leased" {
            result.lease_owner.is_none() || result.lease_expires_at.is_none()
        } else {
            result.lease_owner.is_some() || result.lease_expires_at.is_some()
        })
        || (result.kind == "disconnect") != result.target_attempt_id.is_none()
    {
        return Err(AgentError::Unavailable);
    }
    Ok(result)
}

pub(super) async fn authorized_transaction(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<DatabaseTransaction, AgentError> {
    let tx = db::begin(&state.database).await.map_err(unavailable)?;
    db::lock_admission_guard(&tx).await.map_err(unavailable)?;
    session::recheck(&tx, actor, state.clock.now_unix())
        .await
        .map_err(AgentError::Auth)?;
    Ok(tx)
}

pub(super) async fn commit_authorized(
    tx: DatabaseTransaction,
    state: &ServerState,
    actor: &OrgContext,
) -> Result<(), AgentError> {
    session::recheck(&tx, actor, state.clock.now_unix())
        .await
        .map_err(AgentError::Auth)?;
    tx.commit()
        .await
        .map_err(database_error)
        .map_err(unavailable)
}

pub(super) fn now(state: &ServerState) -> Result<i64, AgentError> {
    let now = state.clock.now_unix();
    if now < 0 {
        Err(AgentError::InvalidInput)
    } else {
        Ok(now)
    }
}

pub(super) async fn lock_connection(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    provider: Provider,
) -> Result<Option<ConnectionRow>, AgentError> {
    let sql = format!(
        "SELECT id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle FROM neev_agent_connections WHERE org_id=$1 AND user_id=$2 AND provider=$3{}",
        lock_suffix(tx)
    );
    let row = db::one(
        tx,
        &sql,
        vec![
            actor.org_id.into(),
            actor.identity.user_id.into(),
            provider_name(provider).into(),
        ],
    )
    .await
    .map_err(unavailable)?;
    let result = row.map(|row| connection(&row)).transpose()?;
    if result
        .as_ref()
        .is_some_and(|row| row.org_id != actor.org_id || row.user_id != actor.identity.user_id)
    {
        return Err(AgentError::Unavailable);
    }
    Ok(result)
}

pub(super) async fn lock_connections(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
) -> Result<Vec<ConnectionRow>, AgentError> {
    let sql = format!(
        "SELECT id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle FROM neev_agent_connections WHERE org_id=$1 AND user_id=$2 ORDER BY id{}",
        lock_suffix(tx)
    );
    tx.query_all(db::statement(
        tx,
        &sql,
        vec![actor.org_id.into(), actor.identity.user_id.into()],
    ))
    .await
    .map_err(unavailable)?
    .iter()
    .map(connection)
    .collect()
}

pub(super) async fn lock_attempt(
    tx: &DatabaseTransaction,
    connection_id: &str,
    session_id: &str,
    attempt_id: &str,
) -> Result<Option<AttemptRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,generation,session_id,state,created_at,expires_at,updated_at FROM neev_agent_attempts WHERE connection_id=$1 AND id=$2 AND session_id=$3{}",
        lock_suffix(tx)
    );
    db::one(
        tx,
        &sql,
        vec![connection_id.into(), attempt_id.into(), session_id.into()],
    )
    .await
    .map_err(unavailable)?
    .map(|row| attempt_row(&row))
    .transpose()
}

pub(super) async fn lock_attempt_for_connection(
    tx: &DatabaseTransaction,
    connection_id: &str,
    attempt_id: &str,
) -> Result<Option<AttemptRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,generation,session_id,state,created_at,expires_at,updated_at FROM neev_agent_attempts WHERE connection_id=$1 AND id=$2{}",
        lock_suffix(tx)
    );
    db::one(tx, &sql, vec![connection_id.into(), attempt_id.into()])
        .await
        .map_err(unavailable)?
        .map(|row| attempt_row(&row))
        .transpose()
}

pub(super) async fn lock_pending_attempts(
    tx: &DatabaseTransaction,
    connection_id: &str,
) -> Result<Vec<AttemptRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,generation,session_id,state,created_at,expires_at,updated_at FROM neev_agent_attempts WHERE connection_id=$1 AND state='pending' ORDER BY id{}",
        lock_suffix(tx)
    );
    tx.query_all(db::statement(tx, &sql, vec![connection_id.into()]))
        .await
        .map_err(unavailable)?
        .iter()
        .map(attempt_row)
        .collect()
}

pub(super) async fn lock_operation_key(
    tx: &DatabaseTransaction,
    connection_id: &str,
    kind: &str,
    request_key: &str,
) -> Result<Option<OperationRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,expires_at,updated_at FROM neev_agent_operations WHERE connection_id=$1 AND kind=$2 AND request_key=$3{}",
        lock_suffix(tx)
    );
    db::one(
        tx,
        &sql,
        vec![connection_id.into(), kind.into(), request_key.into()],
    )
    .await
    .map_err(unavailable)?
    .map(|row| operation_row(&row))
    .transpose()
}

#[allow(dead_code)] // Task 3 completion consumes the locked operation lookup.
pub(super) async fn lock_operation(
    tx: &DatabaseTransaction,
    connection_id: &str,
    operation_id: &str,
) -> Result<Option<OperationRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,expires_at,updated_at FROM neev_agent_operations WHERE connection_id=$1 AND id=$2{}",
        lock_suffix(tx)
    );
    db::one(tx, &sql, vec![connection_id.into(), operation_id.into()])
        .await
        .map_err(unavailable)?
        .map(|row| operation_row(&row))
        .transpose()
}

pub(super) async fn lock_active_operations(
    tx: &DatabaseTransaction,
    connection_id: &str,
) -> Result<Vec<OperationRow>, AgentError> {
    let sql = format!(
        "SELECT id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,expires_at,updated_at FROM neev_agent_operations WHERE connection_id=$1 AND state IN ('queued','leased') ORDER BY id{}",
        lock_suffix(tx)
    );
    tx.query_all(db::statement(tx, &sql, vec![connection_id.into()]))
        .await
        .map_err(unavailable)?
        .iter()
        .map(operation_row)
        .collect()
}

pub(super) fn view(row: &ConnectionRow) -> ConnectionView {
    let connected = row.state == ConnectionState::Connected;
    ConnectionView {
        provider: row.provider,
        state: row.state,
        account_label: connected.then(|| row.account_label.clone()).flatten(),
        checked_at: connected.then_some(row.checked_at).flatten(),
        error_code: row.error_code.clone(),
    }
}

pub(super) fn request_key(value: &str) -> Result<(), AgentError> {
    if !(1..=64).contains(&value.len())
        || value.starts_with("__neev_")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AgentError::InvalidInput);
    }
    Ok(())
}

pub(super) fn checked_generation(generation: i64) -> Result<i64, AgentError> {
    generation.checked_add(1).ok_or(AgentError::Conflict)
}

pub(super) fn checked_deadline(now: i64) -> Result<i64, AgentError> {
    now.checked_add(600).ok_or(AgentError::InvalidInput)
}

pub(super) fn random_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
