use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ConnectionTrait, DatabaseTransaction, DbBackend,
    EntityTrait, IntoActiveModel, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use super::{handlers::ApiError, state::TeamState};
use crate::{
    db::entities::{org, user},
    organization_branding::{normalize_logo_data_uri, validate_display_name},
    team_access::{
        core::lock_user_identity,
        principal::AuthContext,
        session::TeamRole,
    },
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OrganizationSettingsDto {
    pub(super) id: i32,
    pub(super) login: String,
    pub(super) name: String,
    pub(super) avatar_url: Option<String>,
    pub(super) can_edit: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrganizationPatchInput {
    pub(super) name: String,
    pub(super) logo: OrganizationLogoInput,
}

pub(super) enum OrganizationLogoInput {
    Keep,
    Remove,
    Upload { data: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OrganizationLogoAction {
    Keep,
    Remove,
    Upload,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrganizationLogoWire {
    action: OrganizationLogoAction,
    data: Option<String>,
}

impl<'de> Deserialize<'de> for OrganizationLogoInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = OrganizationLogoWire::deserialize(deserializer)?;
        match (wire.action, wire.data) {
            (OrganizationLogoAction::Keep, None) => Ok(Self::Keep),
            (OrganizationLogoAction::Remove, None) => Ok(Self::Remove),
            (OrganizationLogoAction::Upload, Some(data)) => Ok(Self::Upload { data }),
            _ => Err(serde::de::Error::custom("invalid organization logo input")),
        }
    }
}

fn project(row: org::Model, role: TeamRole) -> OrganizationSettingsDto {
    OrganizationSettingsDto {
        id: row.id,
        login: row.login.clone(),
        name: row.name.unwrap_or(row.login),
        avatar_url: row.avatar_url,
        can_edit: role == TeamRole::Admin,
    }
}

async fn begin_authorized(
    state: &TeamState,
    context: &AuthContext,
) -> Result<(DatabaseTransaction, org::Model), ApiError> {
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
    .map_err(|_| ApiError::Unavailable)?
    .ok_or(ApiError::Unavailable)?;
    state
        .principals
        .recheck_in_transaction(&transaction, context)
        .await
        .map_err(ApiError::from)?;
    Ok((transaction, organization))
}

pub(super) async fn read_current(
    state: &TeamState,
    context: &AuthContext,
) -> Result<OrganizationSettingsDto, ApiError> {
    let (transaction, organization) = begin_authorized(state, context).await?;
    let result = project(organization, context.role);
    transaction
        .commit()
        .await
        .map_err(|_| ApiError::Unavailable)?;
    Ok(result)
}

pub(super) async fn update_current(
    state: &TeamState,
    context: &AuthContext,
    input: OrganizationPatchInput,
) -> Result<OrganizationSettingsDto, ApiError> {
    if context.role != TeamRole::Admin {
        return Err(ApiError::Forbidden);
    }
    let name = validate_display_name(&input.name).map_err(|_| ApiError::InvalidRequest)?;
    let normalized_logo = match input.logo {
        OrganizationLogoInput::Keep => None,
        OrganizationLogoInput::Remove => Some(None),
        OrganizationLogoInput::Upload { data } => Some(Some(
            normalize_logo_data_uri(&data).map_err(|_| ApiError::InvalidRequest)?,
        )),
    };

    let (transaction, organization) = begin_authorized(state, context).await?;
    if context.role != TeamRole::Admin {
        return Err(ApiError::Forbidden);
    }
    let avatar_url = normalized_logo.unwrap_or_else(|| organization.avatar_url.clone());
    let mut active = organization.into_active_model();
    active.name = Set(Some(name));
    active.avatar_url = Set(avatar_url);
    active.updated_at = Set(state.clock.now());
    let updated = active
        .update(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    transaction
        .commit()
        .await
        .map_err(|_| ApiError::Unavailable)?;
    Ok(project(updated, context.role))
}
