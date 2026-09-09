use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde::{Deserialize, Serialize};

use super::workspace_catalog::BrowserMutationPolicy;
use crate::{
    app_state::AppState,
    db::entities::org,
    organization_branding::{normalize_logo_data_uri, validate_display_name},
};

#[derive(Debug, Serialize)]
pub struct OrganizationSettings {
    pub id: i32,
    pub login: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub can_edit: bool,
}

#[derive(Debug)]
pub(crate) enum SettingsError {
    Unauthorized,
    Forbidden,
    BadRequest(String),
    Internal,
}

impl SettingsError {
    fn response(self) -> Response<Body> {
        let (status, code) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::BadRequest(message) => {
                tracing::debug!("invalid organization settings: {message}");
                (StatusCode::BAD_REQUEST, "invalid_input")
            }
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        (status, Json(serde_json::json!({ "error": code }))).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchInput {
    name: String,
    #[serde(default)]
    logo: Option<LogoInput>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum LogoInput {
    Keep,
    Remove,
    Upload { data: String },
}

fn can_edit(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

fn settings(row: org::Model, role: &str) -> OrganizationSettings {
    OrganizationSettings {
        id: row.id,
        login: row.login,
        name: row.name.unwrap_or_default(),
        avatar_url: row.avatar_url,
        can_edit: can_edit(role),
    }
}

async fn current(
    headers: &HeaderMap,
    db: &DatabaseConnection,
) -> Result<(crate::db::entities::user::Model, org::Model), SettingsError> {
    let person = super::github_auth::current_user_result(headers, db)
        .await
        .map_err(|_| SettingsError::Internal)?
        .ok_or(SettingsError::Unauthorized)?;
    let organization = org::Entity::find_by_id(person.org_id)
        .one(db)
        .await
        .map_err(|_| SettingsError::Internal)?
        .ok_or(SettingsError::Unauthorized)?;
    Ok((person, organization))
}

pub(crate) async fn get_core(
    headers: &HeaderMap,
    db: &DatabaseConnection,
) -> Result<OrganizationSettings, SettingsError> {
    let (person, organization) = current(headers, db).await?;
    Ok(settings(organization, &person.role))
}

fn validate_name(name: &str) -> Result<String, SettingsError> {
    validate_display_name(name)
        .map_err(|_| SettingsError::BadRequest("invalid display name".into()))
}

fn normalize_logo(data_uri: &str) -> Result<String, SettingsError> {
    normalize_logo_data_uri(data_uri)
        .map_err(|_| SettingsError::BadRequest("invalid logo".into()))
}

pub(crate) async fn patch_core(
    headers: &HeaderMap,
    db: &DatabaseConnection,
    policy: &BrowserMutationPolicy,
    body: &[u8],
) -> Result<OrganizationSettings, SettingsError> {
    let (person, organization) = current(headers, db).await?;
    if !can_edit(&person.role) {
        return Err(SettingsError::Forbidden);
    }
    policy
        .validate(headers)
        .map_err(|error| SettingsError::BadRequest(error.to_string()))?;
    let input: PatchInput = serde_json::from_slice(body)
        .map_err(|_| SettingsError::BadRequest("invalid JSON".into()))?;
    let name = validate_name(&input.name)?;
    let avatar_url = match input.logo {
        None | Some(LogoInput::Keep) => organization.avatar_url.clone(),
        Some(LogoInput::Remove) => None,
        Some(LogoInput::Upload { data }) => Some(normalize_logo(&data)?),
    };

    // Update the current session's organization row only. GitHub's stable
    // numeric identity/login and all repository mappings remain untouched.
    let mut active: org::ActiveModel = organization.into();
    active.name = Set(Some(name));
    active.avatar_url = Set(avatar_url);
    active.updated_at = Set(chrono::Utc::now());
    let updated = active.update(db).await.map_err(|_| SettingsError::Internal)?;
    Ok(settings(updated, &person.role))
}

pub async fn get(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
) -> Response<Body> {
    match get_core(&headers, &state.db.conn).await {
        Ok(value) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Err(error) => error.response(),
    }
}

pub async fn patch(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        != Some("application/json")
    {
        return SettingsError::BadRequest("content type must be application/json".into())
            .response();
    }
    let policy = BrowserMutationPolicy::from_environment();
    match patch_core(&headers, &state.db.conn, &policy, &body).await {
        Ok(value) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Err(error) => error.response(),
    }
}
