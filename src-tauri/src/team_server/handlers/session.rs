use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sea_orm::{
    sea_query::LockType,
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, TransactionTrait,
};

use super::{json, ApiError};
use crate::{
    db::entities::{organization_membership, team_session},
    team_access::{
        core::{database_error, TeamAuthError},
        principal::ProofRequirement,
        secrets::hash_browser_secret,
    },
    team_server::{cookies, projection::SessionDto, state::TeamState},
};

pub(in crate::team_server) async fn bootstrap(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let csrf_secret = match cookies::required(request.headers(), cookies::CSRF) {
        Ok(secret) => secret,
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let context = match state
        .principals
        .resolve(request.headers(), ProofRequirement::Cached)
        .await
    {
        Ok(context) => context,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !cookies::csrf_matches(csrf_secret, &context.csrf_hash) {
        return ApiError::Unauthorized.into_response();
    }
    match SessionDto::load(&state.db, &context, csrf_secret).await {
        Ok(session) => json(session),
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub(in crate::team_server) async fn logout(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let expected_origin = state.config.origin.origin().ascii_serialization();
    if !cookies::exact_origin(request.headers(), &expected_origin) {
        return ApiError::Forbidden.into_response();
    }
    if !cookies::is_json(request.headers()) {
        return ApiError::InvalidRequest.into_response();
    }
    let session_secret = match cookies::required(request.headers(), cookies::SESSION) {
        Ok(secret) => secret.to_owned(),
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let csrf_cookie = match cookies::required(request.headers(), cookies::CSRF) {
        Ok(secret) => secret.to_owned(),
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let csrf_header = match cookies::csrf_header(request.headers()) {
        Ok(secret) => secret.to_owned(),
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    if csrf_cookie != csrf_header {
        return ApiError::Unauthorized.into_response();
    }
    let (_, body) = request.into_parts();
    let body = match to_bytes(body, 16 * 1024).await {
        Ok(body) => body,
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    if serde_json::from_slice::<LogoutBody>(&body).is_err() {
        return ApiError::InvalidRequest.into_response();
    }
    let result = synchronized_logout(&state.db, &session_secret, &csrf_header).await;
    if let Err(error) = result {
        return ApiError::from(error).into_response();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure = cookies::secure(&state.config.origin);
    cookies::expire_cookie(response.headers_mut(), cookies::SESSION, secure);
    cookies::expire_cookie(response.headers_mut(), cookies::CSRF, secure);
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogoutBody {}

pub(in crate::team_server) async fn synchronized_logout(
    db: &DatabaseConnection,
    session_secret: &str,
    csrf_secret: &str,
) -> Result<(), TeamAuthError> {
    let session_hash =
        hash_browser_secret(session_secret).map_err(|_| TeamAuthError::Unauthorized)?;
    let csrf_hash =
        hash_browser_secret(csrf_secret).map_err(|_| TeamAuthError::Unauthorized)?;
    let transaction = db.begin().await.map_err(|error| database_error(&error))?;
    let result = logout_in_transaction(&transaction, &session_hash, &csrf_hash).await;
    match result {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| database_error(&error)),
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|rollback| database_error(&rollback))?;
            Err(error)
        }
    }
}

async fn logout_in_transaction(
    transaction: &DatabaseTransaction,
    session_hash: &str,
    csrf_hash: &str,
) -> Result<(), TeamAuthError> {
    let initial = team_session::Entity::find_by_id(session_hash)
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))?
        .ok_or(TeamAuthError::Unauthorized)?;

    let membership_exists = organization_membership::Entity::find_by_id(initial.membership_id)
        .select_only()
        .column(organization_membership::Column::Id)
        .into_tuple::<i32>()
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))?
        .is_some();
    if membership_exists {
        lock_membership(transaction, initial.membership_id).await?;
    }

    let query = team_session::Entity::find_by_id(session_hash);
    let current = match transaction.get_database_backend() {
        DbBackend::Postgres => query.lock(LockType::Update).one(transaction).await,
        DbBackend::Sqlite => query.one(transaction).await,
        DbBackend::MySql => return Err(TeamAuthError::Unavailable),
    }
    .map_err(|error| database_error(&error))?
    .ok_or(TeamAuthError::Unauthorized)?;
    if current.token_hash != session_hash
        || current.membership_id != initial.membership_id
        || current.csrf_hash != csrf_hash
    {
        return Err(TeamAuthError::Unauthorized);
    }
    let deleted = team_session::Entity::delete_many()
        .filter(team_session::Column::TokenHash.eq(session_hash))
        .filter(team_session::Column::UserId.eq(current.user_id))
        .filter(team_session::Column::OrgId.eq(current.org_id))
        .filter(team_session::Column::MembershipId.eq(current.membership_id))
        .exec(transaction)
        .await
        .map_err(|error| database_error(&error))?;
    if deleted.rows_affected != 1 {
        return Err(TeamAuthError::Conflict);
    }
    Ok(())
}

async fn lock_membership(
    transaction: &DatabaseTransaction,
    membership_id: i32,
) -> Result<(), TeamAuthError> {
    let query = organization_membership::Entity::find_by_id(membership_id)
        .select_only()
        .column(organization_membership::Column::Id);
    match transaction.get_database_backend() {
        DbBackend::Postgres => query
            .lock(LockType::Update)
            .into_tuple::<i32>()
            .one(transaction)
            .await,
        DbBackend::Sqlite => query.into_tuple::<i32>().one(transaction).await,
        DbBackend::MySql => return Err(TeamAuthError::Unavailable),
    }
    .map_err(|error| database_error(&error))?;
    // A membership that disappeared after the initial session read cannot be
    // locked, but it must not make the exact selected session impossible to
    // revoke. If it is still present, the query above holds the required lock.
    Ok(())
}
