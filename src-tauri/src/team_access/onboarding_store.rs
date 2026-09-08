use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, Set,
};

use super::super::core::{database_error, TeamAuthError};
use crate::db::entities::{
    org, organization_membership, team_oauth_attempt, team_pending_onboarding, team_session, user,
};

pub(super) async fn insert_attempt<C: ConnectionTrait>(
    db: &C,
    state_hash: String,
    browser_hash: String,
    encrypted_verifier: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    team_oauth_attempt::ActiveModel {
        state_hash: Set(state_hash),
        browser_hash: Set(browser_hash),
        encrypted_verifier: Set(encrypted_verifier),
        created_at: Set(created_at),
        expires_at: Set(expires_at),
    }
    .insert(db)
    .await
    .map(|_| ())
    .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn live_attempt<C: ConnectionTrait>(
    db: &C,
    state_hash: &str,
    browser_hash: &str,
    now: DateTime<Utc>,
) -> Result<Option<team_oauth_attempt::Model>, TeamAuthError> {
    team_oauth_attempt::Entity::find_by_id(state_hash)
        .filter(team_oauth_attempt::Column::BrowserHash.eq(browser_hash))
        .filter(team_oauth_attempt::Column::CreatedAt.lte(now))
        .filter(team_oauth_attempt::Column::ExpiresAt.gt(now))
        .one(db)
        .await
        .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn consume_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: &team_oauth_attempt::Model,
    now: DateTime<Utc>,
) -> Result<bool, TeamAuthError> {
    team_oauth_attempt::Entity::delete_many()
        .filter(team_oauth_attempt::Column::StateHash.eq(&attempt.state_hash))
        .filter(team_oauth_attempt::Column::BrowserHash.eq(&attempt.browser_hash))
        .filter(team_oauth_attempt::Column::EncryptedVerifier.eq(&attempt.encrypted_verifier))
        .filter(team_oauth_attempt::Column::CreatedAt.eq(attempt.created_at))
        .filter(team_oauth_attempt::Column::ExpiresAt.eq(attempt.expires_at))
        .filter(team_oauth_attempt::Column::CreatedAt.lte(now))
        .filter(team_oauth_attempt::Column::ExpiresAt.gt(now))
        .exec(db)
        .await
        .map(|result| result.rows_affected == 1)
        .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn insert_pending<C: ConnectionTrait>(
    db: &C,
    token_hash: String,
    csrf_hash: String,
    github_user_id: i64,
    encrypted_payload: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    team_pending_onboarding::ActiveModel {
        token_hash: Set(token_hash),
        csrf_hash: Set(csrf_hash),
        github_user_id: Set(github_user_id),
        encrypted_payload: Set(encrypted_payload),
        created_at: Set(created_at),
        expires_at: Set(expires_at),
    }
    .insert(db)
    .await
    .map(|_| ())
    .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn live_pending<C: ConnectionTrait>(
    db: &C,
    token_hash: &str,
    now: DateTime<Utc>,
) -> Result<Option<team_pending_onboarding::Model>, TeamAuthError> {
    team_pending_onboarding::Entity::find_by_id(token_hash)
        .filter(team_pending_onboarding::Column::CreatedAt.lte(now))
        .filter(team_pending_onboarding::Column::ExpiresAt.gt(now))
        .one(db)
        .await
        .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn consume_pending(
    transaction: &DatabaseTransaction,
    pending: &team_pending_onboarding::Model,
    now: DateTime<Utc>,
) -> Result<bool, TeamAuthError> {
    team_pending_onboarding::Entity::delete_many()
        .filter(team_pending_onboarding::Column::TokenHash.eq(&pending.token_hash))
        .filter(team_pending_onboarding::Column::CsrfHash.eq(&pending.csrf_hash))
        .filter(team_pending_onboarding::Column::GithubUserId.eq(pending.github_user_id))
        .filter(team_pending_onboarding::Column::EncryptedPayload.eq(&pending.encrypted_payload))
        .filter(team_pending_onboarding::Column::CreatedAt.eq(pending.created_at))
        .filter(team_pending_onboarding::Column::ExpiresAt.eq(pending.expires_at))
        .filter(team_pending_onboarding::Column::CreatedAt.lte(now))
        .filter(team_pending_onboarding::Column::ExpiresAt.gt(now))
        .exec(transaction)
        .await
        .map(|result| result.rows_affected == 1)
        .map_err(|error| database_error(&error))
}

pub(super) async fn find_user(
    transaction: &DatabaseTransaction,
    github_user_id: i64,
) -> Result<Option<user::Model>, TeamAuthError> {
    user::Entity::find()
        .filter(user::Column::GithubUserId.eq(github_user_id))
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))
}

pub(super) async fn update_user(
    transaction: &DatabaseTransaction,
    existing: user::Model,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
    now: DateTime<Utc>,
) -> Result<user::Model, TeamAuthError> {
    let mut update: user::ActiveModel = existing.into();
    update.login = Set(login);
    update.name = Set(name);
    update.avatar_url = Set(avatar_url);
    update.updated_at = Set(now);
    update
        .update(transaction)
        .await
        .map_err(|error| database_error(&error))
}

pub(super) async fn insert_user(
    transaction: &DatabaseTransaction,
    github_user_id: i64,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
    org_id: i32,
    now: DateTime<Utc>,
) -> Result<user::Model, TeamAuthError> {
    user::ActiveModel {
        id: Default::default(),
        github_user_id: Set(github_user_id),
        login: Set(login),
        name: Set(name),
        email: Set(None),
        avatar_url: Set(avatar_url),
        org_id: Set(org_id),
        role: Set("member".into()),
        gh_token_enc: Set(None),
        gh_refresh_token_enc: Set(None),
        gh_token_expires_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|error| database_error(&error))
}

pub(super) async fn resolve_org(
    transaction: &DatabaseTransaction,
    github_org_id: i64,
    login: String,
    avatar_url: Option<String>,
    now: DateTime<Utc>,
) -> Result<org::Model, TeamAuthError> {
    let existing = org::Entity::find()
        .filter(org::Column::GithubOrgId.eq(github_org_id))
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))?;
    if let Some(existing) = existing {
        let mut update: org::ActiveModel = existing.into();
        update.login = Set(login);
        update.updated_at = Set(now);
        return update
            .update(transaction)
            .await
            .map_err(|error| database_error(&error));
    }
    org::ActiveModel {
        id: Default::default(),
        github_org_id: Set(github_org_id),
        login: Set(login),
        name: Set(None),
        avatar_url: Set(avatar_url),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|error| database_error(&error))
}

pub(super) async fn find_membership(
    transaction: &DatabaseTransaction,
    org_id: i32,
    user_id: i32,
) -> Result<Option<organization_membership::Model>, TeamAuthError> {
    let query = organization_membership::Entity::find()
        .filter(organization_membership::Column::OrgId.eq(org_id))
        .filter(organization_membership::Column::UserId.eq(user_id));
    let query = if transaction.get_database_backend() == DbBackend::Postgres {
        query.lock(sea_orm::sea_query::LockType::Update)
    } else {
        query
    };
    query
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))
}

pub(super) async fn insert_membership(
    transaction: &DatabaseTransaction,
    org_id: i32,
    user_id: i32,
    role: &str,
    now: DateTime<Utc>,
) -> Result<organization_membership::Model, TeamAuthError> {
    organization_membership::ActiveModel {
        id: Default::default(),
        org_id: Set(org_id),
        user_id: Set(user_id),
        role: Set(role.into()),
        state: Set("active".into()),
        authorization_revision: Set(1),
        credential_generation: Set(0),
        verified_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(transaction)
    .await
    .map_err(|error| database_error(&error))
}

pub(super) async fn update_membership_authority(
    transaction: &DatabaseTransaction,
    existing: organization_membership::Model,
    role: &str,
    state: &str,
    authorization_revision: i64,
    now: DateTime<Utc>,
) -> Result<organization_membership::Model, TeamAuthError> {
    let mut update: organization_membership::ActiveModel = existing.into();
    update.role = Set(role.into());
    update.state = Set(state.into());
    update.authorization_revision = Set(authorization_revision);
    update.updated_at = Set(now);
    update
        .update(transaction)
        .await
        .map_err(|error| database_error(&error))
}

pub(super) async fn bind_membership_proof(
    transaction: &DatabaseTransaction,
    membership: &organization_membership::Model,
    credential_generation: i64,
    verified_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<organization_membership::Model, TeamAuthError> {
    let changed = organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::CredentialGeneration,
            sea_orm::sea_query::Expr::value(credential_generation),
        )
        .col_expr(
            organization_membership::Column::VerifiedAt,
            sea_orm::sea_query::Expr::value(Some(verified_at)),
        )
        .col_expr(
            organization_membership::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(organization_membership::Column::Id.eq(membership.id))
        .filter(organization_membership::Column::OrgId.eq(membership.org_id))
        .filter(organization_membership::Column::UserId.eq(membership.user_id))
        .filter(
            organization_membership::Column::AuthorizationRevision
                .eq(membership.authorization_revision),
        )
        .filter(organization_membership::Column::State.eq("active"))
        .exec(transaction)
        .await
        .map_err(|error| database_error(&error))?;
    if changed.rows_affected != 1 {
        return Err(TeamAuthError::Conflict);
    }
    organization_membership::Entity::find_by_id(membership.id)
        .one(transaction)
        .await
        .map_err(|error| database_error(&error))?
        .ok_or(TeamAuthError::Conflict)
}

pub(super) async fn delete_previous_session(
    transaction: &DatabaseTransaction,
    token_hash: Option<&str>,
) -> Result<(), TeamAuthError> {
    if let Some(token_hash) = token_hash {
        team_session::Entity::delete_many()
            .filter(team_session::Column::TokenHash.eq(token_hash))
            .exec(transaction)
            .await
            .map_err(|error| database_error(&error))?;
    }
    Ok(())
}

pub(super) async fn insert_session(
    transaction: &DatabaseTransaction,
    token_hash: String,
    csrf_hash: String,
    user_id: i32,
    org_id: i32,
    membership_id: i32,
    authorization_revision: i64,
    credential_generation: i64,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    team_session::ActiveModel {
        token_hash: Set(token_hash),
        user_id: Set(user_id),
        org_id: Set(org_id),
        membership_id: Set(membership_id),
        authorization_revision: Set(authorization_revision),
        credential_generation: Set(credential_generation),
        csrf_hash: Set(csrf_hash),
        created_at: Set(created_at),
        expires_at: Set(expires_at),
    }
    .insert(transaction)
    .await
    .map(|_| ())
    .map_err(|error| database_error(&error))
}

pub(super) async fn cleanup_expired<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    team_oauth_attempt::Entity::delete_many()
        .filter(team_oauth_attempt::Column::ExpiresAt.lte(now))
        .exec(db)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    team_pending_onboarding::Entity::delete_many()
        .filter(team_pending_onboarding::Column::ExpiresAt.lte(now))
        .exec(db)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Ok(())
}
