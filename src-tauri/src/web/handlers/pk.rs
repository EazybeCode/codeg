use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::pk as core;
use crate::models::PkRoundConfig;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    #[serde(default)]
    pub folder_id: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdParams {
    pub id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateParams {
    pub folder_id: i32,
    pub task: String,
    pub config: PkRoundConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusParams {
    pub id: i32,
    pub status: String,
}

pub async fn pk_round_list(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ListParams>,
) -> Result<Json<Vec<crate::models::PkRoundInfo>>, AppCommandError> {
    let result = core::pk_round_list_core(&state.db, params.folder_id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn pk_round_get(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<IdParams>,
) -> Result<Json<crate::models::PkRoundInfo>, AppCommandError> {
    let result = core::pk_round_get_core(&state.db, params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn pk_round_create(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CreateParams>,
) -> Result<Json<crate::models::PkRoundInfo>, AppCommandError> {
    let result = core::pk_round_create_core(&state.db, params.folder_id, params.task, params.config)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(result))
}

pub async fn pk_round_update_status(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<UpdateStatusParams>,
) -> Result<Json<()>, AppCommandError> {
    core::pk_round_update_status_core(&state.db, params.id, params.status)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(()))
}

pub async fn pk_round_delete(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<IdParams>,
) -> Result<Json<()>, AppCommandError> {
    core::pk_round_delete_core(&state.db, params.id)
        .await
        .map_err(AppCommandError::from)?;
    Ok(Json(()))
}
