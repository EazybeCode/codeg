use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    EntityTrait, QueryFilter, QuerySelect, Set,
};

use super::super::core::TeamAuthError;
use crate::db::entities::{organization_membership, team_github_credential, team_session, user};

pub(super) async fn identity_exists<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
    github_user_id: i64,
) -> Result<bool, TeamAuthError> {
    user::Entity::find_by_id(user_id)
        .filter(user::Column::GithubUserId.eq(github_user_id))
        .select_only()
        .column(user::Column::Id)
        .column(user::Column::GithubUserId)
        .into_tuple::<(i32, i64)>()
        .one(db)
        .await
        .map(|identity| identity.is_some())
        .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn load<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
) -> Result<Option<team_github_credential::Model>, TeamAuthError> {
    team_github_credential::Entity::find_by_id(user_id)
        .one(db)
        .await
        .map_err(|_| TeamAuthError::Unavailable)
}

pub(super) async fn replace_active(
    transaction: &DatabaseTransaction,
    existing: Option<team_github_credential::Model>,
    user_id: i32,
    github_user_id: i64,
    generation: i64,
    encrypted_payload: String,
    now: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    if let Some(existing) = existing {
        if existing.github_user_id != github_user_id {
            return Err(TeamAuthError::Unavailable);
        }
        let mut active: team_github_credential::ActiveModel = existing.into();
        active.generation = Set(generation);
        active.state = Set("active".into());
        active.encrypted_payload = Set(Some(encrypted_payload));
        active.refresh_owner = Set(None);
        active.refresh_expires_at = Set(None);
        active.updated_at = Set(now);
        active
            .update(transaction)
            .await
            .map_err(|_| TeamAuthError::Unavailable)?;
    } else {
        team_github_credential::ActiveModel {
            user_id: Set(user_id),
            github_user_id: Set(github_user_id),
            generation: Set(generation),
            state: Set("active".into()),
            encrypted_payload: Set(Some(encrypted_payload)),
            refresh_owner: Set(None),
            refresh_expires_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    }
    Ok(())
}

pub(super) async fn advance_bindings(
    transaction: &DatabaseTransaction,
    user_id: i32,
    previous_generation: i64,
    generation: i64,
    now: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::CredentialGeneration,
            Expr::value(generation),
        )
        .col_expr(
            organization_membership::Column::VerifiedAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            organization_membership::Column::UpdatedAt,
            Expr::value(now),
        )
        .filter(organization_membership::Column::UserId.eq(user_id))
        .filter(
            organization_membership::Column::CredentialGeneration.eq(previous_generation),
        )
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    team_session::Entity::update_many()
        .col_expr(
            team_session::Column::CredentialGeneration,
            Expr::value(generation),
        )
        .filter(team_session::Column::UserId.eq(user_id))
        .filter(team_session::Column::CredentialGeneration.eq(previous_generation))
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Ok(())
}

pub(super) async fn claim_refresh(
    transaction: &DatabaseTransaction,
    user_id: i32,
    github_user_id: i64,
    generation: i64,
    owner: &str,
    deadline: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, TeamAuthError> {
    let changed = team_github_credential::Entity::update_many()
        .col_expr(team_github_credential::Column::State, Expr::value("refreshing"))
        .col_expr(
            team_github_credential::Column::RefreshOwner,
            Expr::value(Some(owner.to_owned())),
        )
        .col_expr(
            team_github_credential::Column::RefreshExpiresAt,
            Expr::value(Some(deadline)),
        )
        .col_expr(team_github_credential::Column::UpdatedAt, Expr::value(now))
        .filter(team_github_credential::Column::UserId.eq(user_id))
        .filter(team_github_credential::Column::GithubUserId.eq(github_user_id))
        .filter(team_github_credential::Column::Generation.eq(generation))
        .filter(team_github_credential::Column::State.eq("active"))
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Ok(changed.rows_affected == 1)
}

pub(super) async fn finish_refresh(
    transaction: &DatabaseTransaction,
    user_id: i32,
    github_user_id: i64,
    previous_generation: i64,
    generation: i64,
    owner: &str,
    deadline: DateTime<Utc>,
    encrypted_payload: String,
    now: DateTime<Utc>,
) -> Result<bool, TeamAuthError> {
    let changed = team_github_credential::Entity::update_many()
        .col_expr(
            team_github_credential::Column::Generation,
            Expr::value(generation),
        )
        .col_expr(team_github_credential::Column::State, Expr::value("active"))
        .col_expr(
            team_github_credential::Column::EncryptedPayload,
            Expr::value(Some(encrypted_payload)),
        )
        .col_expr(
            team_github_credential::Column::RefreshOwner,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            team_github_credential::Column::RefreshExpiresAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(team_github_credential::Column::UpdatedAt, Expr::value(now))
        .filter(team_github_credential::Column::UserId.eq(user_id))
        .filter(team_github_credential::Column::GithubUserId.eq(github_user_id))
        .filter(team_github_credential::Column::Generation.eq(previous_generation))
        .filter(team_github_credential::Column::State.eq("refreshing"))
        .filter(team_github_credential::Column::RefreshOwner.eq(owner))
        .filter(team_github_credential::Column::RefreshExpiresAt.eq(deadline))
        .filter(team_github_credential::Column::RefreshExpiresAt.gt(now))
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Ok(changed.rows_affected == 1)
}

pub(super) async fn invalidate(
    transaction: &DatabaseTransaction,
    user_id: i32,
    github_user_id: i64,
    generation: i64,
    state: &str,
    owner: Option<&str>,
    deadline: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<bool, TeamAuthError> {
    let mut update = team_github_credential::Entity::update_many()
        .col_expr(
            team_github_credential::Column::State,
            Expr::value("reauth_required"),
        )
        .col_expr(
            team_github_credential::Column::EncryptedPayload,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            team_github_credential::Column::RefreshOwner,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            team_github_credential::Column::RefreshExpiresAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(team_github_credential::Column::UpdatedAt, Expr::value(now))
        .filter(team_github_credential::Column::UserId.eq(user_id))
        .filter(team_github_credential::Column::GithubUserId.eq(github_user_id))
        .filter(team_github_credential::Column::Generation.eq(generation))
        .filter(team_github_credential::Column::State.eq(state));
    if let Some(owner) = owner {
        update = update.filter(team_github_credential::Column::RefreshOwner.eq(owner));
    }
    if let Some(deadline) = deadline {
        update = update.filter(team_github_credential::Column::RefreshExpiresAt.eq(deadline));
    }
    let changed = update
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    if changed.rows_affected != 1 {
        return Ok(false);
    }
    organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::VerifiedAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            organization_membership::Column::UpdatedAt,
            Expr::value(now),
        )
        .filter(organization_membership::Column::UserId.eq(user_id))
        .filter(organization_membership::Column::CredentialGeneration.eq(generation))
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    team_session::Entity::delete_many()
        .filter(team_session::Column::UserId.eq(user_id))
        .filter(team_session::Column::CredentialGeneration.eq(generation))
        .exec(transaction)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Ok(true)
}
