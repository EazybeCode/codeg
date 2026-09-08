use std::{io::Cursor, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{ImageFormat, ImageReader, Limits};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde::{Deserialize, Serialize};

use super::workspace_catalog::BrowserMutationPolicy;
use crate::{app_state::AppState, db::entities::org};

const MAX_LOGO_BYTES: usize = 1024 * 1024;
const MAX_LOGO_DIMENSION: u32 = 1024;
const MAX_LOGO_ALLOCATION: u64 = 16 * 1024 * 1024;
const MAX_ENCODED_LOGO_BYTES: usize = MAX_LOGO_BYTES.div_ceil(3) * 4;

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
    let name = name.trim();
    let chars = name.chars().count();
    if !(1..=80).contains(&chars) || name.chars().any(char::is_control) {
        return Err(SettingsError::BadRequest("invalid display name".into()));
    }
    Ok(name.to_owned())
}

fn normalize_logo(data_uri: &str) -> Result<String, SettingsError> {
    let (prefix, encoded) = data_uri
        .split_once(',')
        .ok_or_else(|| SettingsError::BadRequest("logo must be a data URI".into()))?;
    let declared = match prefix {
        "data:image/png;base64" => ImageFormat::Png,
        "data:image/jpeg;base64" => ImageFormat::Jpeg,
        "data:image/webp;base64" => ImageFormat::WebP,
        _ => return Err(SettingsError::BadRequest("unsupported logo type".into())),
    };
    if encoded.len() > MAX_ENCODED_LOGO_BYTES + 4 {
        return Err(SettingsError::BadRequest("logo is too large".into()));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| SettingsError::BadRequest("malformed logo encoding".into()))?;
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(SettingsError::BadRequest("logo is too large".into()));
    }
    if image::guess_format(&bytes).ok() != Some(declared) {
        return Err(SettingsError::BadRequest(
            "logo content does not match type".into(),
        ));
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), declared);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_LOGO_DIMENSION);
    limits.max_image_height = Some(MAX_LOGO_DIMENSION);
    limits.max_alloc = Some(MAX_LOGO_ALLOCATION);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| SettingsError::BadRequest("invalid logo image".into()))?;
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|_| SettingsError::BadRequest("could not normalize logo".into()))?;
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(png)))
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
