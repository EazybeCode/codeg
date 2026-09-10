use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait,
    QuerySelect, Set, TransactionTrait,
};

use crate::{
    db::entities::{org, team_catalog_admission, user},
    team_access::{core::lock_user_identity, principal::AuthContext},
    team_server::{handlers::ApiError, state::TeamState},
};

pub(super) const REPOSITORY: &str = "repository";
pub(super) const NODE: &str = "node";
pub(super) const CHAT: &str = "chat";

pub(super) async fn begin_authorized(
    state: &TeamState,
    context: &AuthContext,
) -> Result<DatabaseTransaction, ApiError> {
    let transaction = state.db.begin().await.map_err(|_| ApiError::Unavailable)?;
    let github_user_id = user::Entity::find_by_id(context.user_id)
        .select_only()
        .column(user::Column::GithubUserId)
        .into_tuple::<i64>()
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .filter(|value| *value > 0)
        .ok_or(ApiError::Unavailable)?;
    lock_user_identity(&transaction, context.user_id, github_user_id)
        .await
        .map_err(ApiError::from)?;
    let query = org::Entity::find_by_id(context.org_id);
    let organization = match transaction.get_database_backend() {
        DbBackend::Postgres => query.lock(LockType::Update).one(&transaction).await,
        DbBackend::Sqlite => query.one(&transaction).await,
        DbBackend::MySql => return Err(ApiError::Unavailable),
    }
    .map_err(|_| ApiError::Unavailable)?;
    if organization.is_none() {
        return Err(ApiError::Unavailable);
    }
    state
        .principals
        .recheck_in_transaction(&transaction, context)
        .await
        .map_err(ApiError::from)?;
    Ok(transaction)
}

pub(super) async fn commit_rechecked(
    state: &TeamState,
    context: &AuthContext,
    transaction: DatabaseTransaction,
) -> Result<(), ApiError> {
    state
        .principals
        .recheck_in_transaction(&transaction, context)
        .await
        .map_err(ApiError::from)?;
    transaction
        .commit()
        .await
        .map_err(|_| ApiError::Unavailable)
}

pub(super) async fn admission(
    transaction: &DatabaseTransaction,
    kind: &str,
    resource_id: i32,
    org_id: i32,
) -> Result<Option<team_catalog_admission::Model>, ApiError> {
    let row = team_catalog_admission::Entity::find_by_id((kind.to_owned(), resource_id))
        .one(transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    Ok(row.filter(|row| row.org_id == org_id))
}

pub(super) async fn require_admission(
    transaction: &DatabaseTransaction,
    kind: &str,
    resource_id: i32,
    org_id: i32,
) -> Result<team_catalog_admission::Model, ApiError> {
    admission(transaction, kind, resource_id, org_id)
        .await?
        .ok_or(ApiError::NotFound)
}

pub(super) async fn admit(
    state: &TeamState,
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    kind: &str,
    resource_id: i32,
) -> Result<(), ApiError> {
    team_catalog_admission::ActiveModel {
        resource_kind: Set(kind.to_owned()),
        resource_id: Set(resource_id),
        org_id: Set(context.org_id),
        creator_user_id: Set(context.user_id),
        created_at: Set(state.clock.now()),
    }
    .insert(transaction)
    .await
    .map_err(|_| ApiError::Unavailable)?;
    Ok(())
}
