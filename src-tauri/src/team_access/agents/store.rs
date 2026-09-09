use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::{Expr, LockType}, ActiveModelTrait, ColumnTrait, ConnectionTrait,
    DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait, PaginatorTrait, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};

use super::types::{
    valid_account_label, AgentError, ConnectionState, ConnectionView, LoginTicket, Provider,
};
use crate::{
    db::entities::{
        org, team_agent_attempt, team_agent_connection, team_agent_operation, user,
    },
    team_access::{
        core::lock_user_identity,
        principal::{AuthContext, PrincipalService},
    },
};

pub(super) async fn authorized_transaction(
    db: &DatabaseConnection,
    principals: &PrincipalService,
    context: &AuthContext,
) -> Result<DatabaseTransaction, AgentError> {
    let transaction = db.begin().await.map_err(|_| AgentError::Unavailable)?;
    let github_user_id = user::Entity::find_by_id(context.user_id)
        .select_only()
        .column(user::Column::GithubUserId)
        .into_tuple::<i64>()
        .one(&transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?
        .filter(|value| *value > 0)
        .ok_or(AgentError::Unavailable)?;
    lock_user_identity(&transaction, context.user_id, github_user_id)
        .await
        .map_err(AgentError::Auth)?;
    lock_org(&transaction, context.org_id).await?;
    principals
        .recheck_in_transaction(&transaction, context)
        .await
        .map_err(AgentError::Auth)?;
    Ok(transaction)
}

pub(super) async fn lock_org(
    transaction: &DatabaseTransaction,
    org_id: i32,
) -> Result<(), AgentError> {
    if org_id <= 0 {
        return Err(AgentError::Unavailable);
    }
    let query = org::Entity::find_by_id(org_id)
        .select_only()
        .column(org::Column::Id);
    let found = if transaction.get_database_backend() == DbBackend::Postgres {
        query
            .lock(LockType::Update)
            .into_tuple::<i32>()
            .one(transaction)
            .await
    } else {
        query.into_tuple::<i32>().one(transaction).await
    }
    .map_err(|_| AgentError::Unavailable)?;
    found.map(|_| ()).ok_or(AgentError::Unavailable)
}

pub(super) async fn final_recheck_and_commit(
    transaction: DatabaseTransaction,
    principals: &PrincipalService,
    context: &AuthContext,
) -> Result<(), AgentError> {
    principals
        .recheck_in_transaction(&transaction, context)
        .await
        .map_err(AgentError::Auth)?;
    transaction
        .commit()
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn connection(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    provider: Provider,
) -> Result<Option<team_agent_connection::Model>, AgentError> {
    let query = team_agent_connection::Entity::find()
        .filter(team_agent_connection::Column::OrgId.eq(context.org_id))
        .filter(team_agent_connection::Column::UserId.eq(context.user_id))
        .filter(team_agent_connection::Column::Provider.eq(provider.as_str()));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query
        .one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn connections(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
) -> Result<Vec<team_agent_connection::Model>, AgentError> {
    let query = team_agent_connection::Entity::find()
        .filter(team_agent_connection::Column::OrgId.eq(context.org_id))
        .filter(team_agent_connection::Column::UserId.eq(context.user_id));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query
        .all(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) fn project(
    row: &team_agent_connection::Model,
) -> Result<ConnectionView, AgentError> {
    let provider = Provider::from_str(&row.provider).ok_or(AgentError::Unavailable)?;
    let state = ConnectionState::from_str(&row.state).ok_or(AgentError::Unavailable)?;
    if row.generation <= 0 || row.id.is_empty() || row.id.len() > 64 {
        return Err(AgentError::Unavailable);
    }
    if row.error_code.as_deref().is_some_and(|code| {
        !matches!(
            code,
            "provider_unavailable"
                | "reauth_required"
                | "unsupported_account"
                | "expired"
                | "canceled"
                | "cleanup_failed"
                | "interrupted"
        )
    }) {
        return Err(AgentError::Unavailable);
    }
    let (account_label, checked_at) = if state == ConnectionState::Connected {
        let label = row.account_label.clone().ok_or(AgentError::Unavailable)?;
        let checked = row.checked_at.ok_or(AgentError::Unavailable)?;
        if !valid_account_label(&label) || row.storage_handle.is_none() {
            return Err(AgentError::Unavailable);
        }
        (Some(label), Some(checked))
    } else {
        (None, None)
    };
    Ok(ConnectionView {
        provider,
        state,
        account_label,
        checked_at,
        error_code: row.error_code.clone(),
    })
}

pub(super) async fn operation(
    transaction: &DatabaseTransaction,
    connection_id: &str,
    kind: &str,
    request_key: &str,
) -> Result<Option<team_agent_operation::Model>, AgentError> {
    let query = team_agent_operation::Entity::find()
        .filter(team_agent_operation::Column::ConnectionId.eq(connection_id))
        .filter(team_agent_operation::Column::Kind.eq(kind))
        .filter(team_agent_operation::Column::RequestKey.eq(request_key));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query.one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn attempt(
    transaction: &DatabaseTransaction,
    connection_id: &str,
    attempt_id: &str,
) -> Result<Option<team_agent_attempt::Model>, AgentError> {
    let query = team_agent_attempt::Entity::find_by_id(attempt_id)
        .filter(team_agent_attempt::Column::ConnectionId.eq(connection_id));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query.one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn completion_rows(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    ticket: &LoginTicket,
) -> Result<(
    team_agent_connection::Model,
    team_agent_attempt::Model,
    team_agent_operation::Model,
), AgentError> {
    let connection = connection_by_id(transaction, &ticket.connection_id)
        .await?
        .filter(|row| row.org_id == context.org_id && row.user_id == context.user_id)
        .ok_or(AgentError::NotFound)?;
    let attempt = attempt(transaction, &connection.id, &ticket.attempt_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    let operation = operation_by_id(transaction, &connection.id, &ticket.operation_id)
        .await?
        .ok_or(AgentError::NotFound)?;
    Ok((connection, attempt, operation))
}

pub(super) async fn connection_by_id(
    transaction: &DatabaseTransaction,
    connection_id: &str,
) -> Result<Option<team_agent_connection::Model>, AgentError> {
    let query = team_agent_connection::Entity::find_by_id(connection_id);
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query
        .one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn operation_by_id(
    transaction: &DatabaseTransaction,
    connection_id: &str,
    operation_id: &str,
) -> Result<Option<team_agent_operation::Model>, AgentError> {
    let query = team_agent_operation::Entity::find_by_id(operation_id)
        .filter(team_agent_operation::Column::ConnectionId.eq(connection_id));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(LockType::Update)
    } else {
        query
    };
    query
        .one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn active_operations(
    transaction: &DatabaseTransaction,
    connection_id: &str,
) -> Result<Vec<team_agent_operation::Model>, AgentError> {
    team_agent_operation::Entity::find()
        .filter(team_agent_operation::Column::ConnectionId.eq(connection_id))
        .filter(team_agent_operation::Column::State.is_in(["queued", "leased"]))
        .all(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn recent_attempt_count(
    transaction: &DatabaseTransaction,
    user_id: i32,
    cutoff: DateTime<Utc>,
) -> Result<u64, AgentError> {
    let connection_ids = team_agent_connection::Entity::find()
        .filter(team_agent_connection::Column::UserId.eq(user_id))
        .select_only()
        .column(team_agent_connection::Column::Id)
        .into_tuple::<String>()
        .all(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    if connection_ids.is_empty() {
        return Ok(0);
    }
    team_agent_attempt::Entity::find()
        .filter(team_agent_attempt::Column::ConnectionId.is_in(connection_ids))
        .filter(team_agent_attempt::Column::CreatedAt.gt(cutoff))
        .count(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn insert_connection(
    transaction: &DatabaseTransaction,
    id: String,
    context: &AuthContext,
    provider: Provider,
    generation: i64,
    state: &str,
    now: DateTime<Utc>,
) -> Result<team_agent_connection::Model, AgentError> {
    team_agent_connection::ActiveModel {
        id: Set(id),
        org_id: Set(context.org_id),
        user_id: Set(context.user_id),
        provider: Set(provider.as_str().to_owned()),
        generation: Set(generation),
        state: Set(state.to_owned()),
        account_label: Set(None),
        checked_at: Set(None),
        error_code: Set(None),
        storage_handle: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn set_connection_for_login(
    transaction: &DatabaseTransaction,
    row: team_agent_connection::Model,
    generation: i64,
    now: DateTime<Utc>,
) -> Result<team_agent_connection::Model, AgentError> {
    let mut active: team_agent_connection::ActiveModel = row.into();
    active.generation = Set(generation);
    active.state = Set("connecting".to_owned());
    active.account_label = Set(None);
    active.checked_at = Set(None);
    active.error_code = Set(None);
    active.updated_at = Set(now);
    active
        .update(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn set_connection_after_invalidation(
    transaction: &DatabaseTransaction,
    row: team_agent_connection::Model,
    generation: i64,
    state: &str,
    clear_handle: bool,
    now: DateTime<Utc>,
) -> Result<team_agent_connection::Model, AgentError> {
    let mut active: team_agent_connection::ActiveModel = row.into();
    active.generation = Set(generation);
    active.state = Set(state.to_owned());
    active.account_label = Set(None);
    active.checked_at = Set(None);
    active.error_code = Set(None);
    if clear_handle {
        active.storage_handle = Set(None);
    }
    active.updated_at = Set(now);
    active
        .update(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn set_connection_after_invalidation_with_error(
    transaction: &DatabaseTransaction,
    row: team_agent_connection::Model,
    generation: i64,
    error_code: &str,
    now: DateTime<Utc>,
) -> Result<team_agent_connection::Model, AgentError> {
    let mut active: team_agent_connection::ActiveModel = row.into();
    active.generation = Set(generation);
    active.state = Set("disconnecting".to_owned());
    active.account_label = Set(None);
    active.checked_at = Set(None);
    active.error_code = Set(Some(error_code.to_owned()));
    active.updated_at = Set(now);
    active
        .update(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn lock_cleanup_owner(
    transaction: &DatabaseTransaction,
    user_id: i32,
    org_id: i32,
) -> Result<(), AgentError> {
    let github_user_id = user::Entity::find_by_id(user_id)
        .select_only()
        .column(user::Column::GithubUserId)
        .into_tuple::<i64>()
        .one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?
        .filter(|value| *value > 0)
        .ok_or(AgentError::Unavailable)?;
    lock_user_identity(transaction, user_id, github_user_id)
        .await
        .map_err(AgentError::Auth)?;
    lock_org(transaction, org_id).await
}

pub(super) async fn insert_attempt(
    transaction: &DatabaseTransaction,
    id: String,
    connection_id: String,
    generation: i64,
    session_id: String,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<team_agent_attempt::Model, AgentError> {
    team_agent_attempt::ActiveModel {
        id: Set(id),
        connection_id: Set(connection_id),
        generation: Set(generation),
        session_id: Set(session_id),
        state: Set("pending".to_owned()),
        created_at: Set(now),
        expires_at: Set(expires_at),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn set_attempt_state(
    transaction: &DatabaseTransaction,
    row: team_agent_attempt::Model,
    state: &str,
    now: DateTime<Utc>,
) -> Result<(), AgentError> {
    let mut active: team_agent_attempt::ActiveModel = row.into();
    active.state = Set(state.to_owned());
    active.updated_at = Set(now);
    active
        .update(transaction)
        .await
        .map(|_| ())
        .map_err(|_| AgentError::Unavailable)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_operation(
    transaction: &DatabaseTransaction,
    id: String,
    connection_id: String,
    kind: &str,
    request_key: &str,
    target_attempt_id: Option<String>,
    target_generation: i64,
    result_generation: i64,
    state: &str,
    lease_owner: Option<String>,
    lease_expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<team_agent_operation::Model, AgentError> {
    team_agent_operation::ActiveModel {
        id: Set(id),
        connection_id: Set(connection_id),
        kind: Set(kind.to_owned()),
        request_key: Set(request_key.to_owned()),
        target_attempt_id: Set(target_attempt_id),
        target_generation: Set(target_generation),
        result_generation: Set(result_generation),
        state: Set(state.to_owned()),
        lease_owner: Set(lease_owner),
        lease_expires_at: Set(lease_expires_at),
        error_code: Set(None),
        created_at: Set(now),
        expires_at: Set(expires_at),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn abandon_connect_operations(
    transaction: &DatabaseTransaction,
    connection_id: &str,
    now: DateTime<Utc>,
) -> Result<(), AgentError> {
    team_agent_operation::Entity::update_many()
        .col_expr(
            team_agent_operation::Column::State,
            Expr::value("abandoned"),
        )
        .col_expr(
            team_agent_operation::Column::LeaseOwner,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            team_agent_operation::Column::LeaseExpiresAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            team_agent_operation::Column::UpdatedAt,
            Expr::value(now),
        )
        .filter(team_agent_operation::Column::ConnectionId.eq(connection_id))
        .filter(team_agent_operation::Column::Kind.eq("connect"))
        .filter(team_agent_operation::Column::State.is_in(["queued", "leased"]))
        .exec(transaction)
        .await
        .map(|_| ())
        .map_err(|_| AgentError::Unavailable)
}

pub(super) async fn cancel_pending_attempts(
    transaction: &DatabaseTransaction,
    connection_id: &str,
    now: DateTime<Utc>,
) -> Result<(), AgentError> {
    team_agent_attempt::Entity::update_many()
        .col_expr(
            team_agent_attempt::Column::State,
            Expr::value("canceled"),
        )
        .col_expr(team_agent_attempt::Column::UpdatedAt, Expr::value(now))
        .filter(team_agent_attempt::Column::ConnectionId.eq(connection_id))
        .filter(team_agent_attempt::Column::State.eq("pending"))
        .exec(transaction)
        .await
        .map(|_| ())
        .map_err(|_| AgentError::Unavailable)
}
