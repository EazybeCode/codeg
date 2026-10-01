//! Org knowledge admin API (`/api/neev/knowledge/*`, plan Task 9 + A1.4).
//!
//! Admins only (403 otherwise; another org's ids are 404). When
//! `NEEV_KNOWLEDGE_ROOT` is unset every route answers 503
//! `knowledge_unavailable`. Errors use the codebase's `{"code": …}` body.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{rejection::PathRejection, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::neev::{
    auth::session,
    content::{resources, types::ContentError},
    knowledge::{export, jobs, settings, status, store, types::JobMode, types::KnowledgeError},
    startup::ServerState,
    types::{OrgContext, Role},
};

use super::{add_private_headers, catalog, private_json, ApiError};

const SMALL_BODY_LIMIT: usize = 4096;

fn coded(status: StatusCode, code: &str) -> Response {
    let mut response = (status, Json(serde_json::json!({ "code": code }))).into_response();
    add_private_headers(&mut response);
    response
}

/// The knowledge error taxonomy as HTTP.
fn failure(error: KnowledgeError) -> Response {
    match error {
        KnowledgeError::Unavailable => coded(StatusCode::SERVICE_UNAVAILABLE, "knowledge_unavailable"),
        KnowledgeError::JobActive => coded(StatusCode::CONFLICT, "job_active"),
        KnowledgeError::CredentialMissing => coded(StatusCode::CONFLICT, "credential_missing"),
        KnowledgeError::Content(ContentError::Quota) => coded(StatusCode::PAYLOAD_TOO_LARGE, "export_too_large"),
        KnowledgeError::Content(error) => ApiError::from(error).into_response(),
        KnowledgeError::Auth(error) => ApiError::from(error).into_response(),
    }
}

/// Admin first (403), then configured (503).
fn guard(state: &ServerState, actor: &OrgContext) -> Result<(), Response> {
    if actor.role != Role::Admin {
        return Err(ApiError::Forbidden.into_response());
    }
    store::store(state).map(|_| ()).map_err(failure)
}

async fn admin_get(state: &ServerState, headers: &HeaderMap) -> Result<OrgContext, Response> {
    let _permit = catalog::permit(state).map_err(IntoResponse::into_response)?;
    let actor = catalog::organization_get(state, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    guard(state, &actor)?;
    Ok(actor)
}

async fn admin_mutation<T: for<'de> Deserialize<'de>>(state: &ServerState, request: Request<Body>) -> Result<(T, OrgContext), Response> {
    let _permit = catalog::permit(state).map_err(IntoResponse::into_response)?;
    let (input, actor) = catalog::organization_mutation::<T>(state, request, SMALL_BODY_LIMIT)
        .await
        .map_err(IntoResponse::into_response)?;
    guard(state, &actor)?;
    Ok((input, actor))
}

/// Run a read model inside a session-rechecked transaction.
macro_rules! read_tx {
    ($state:expr, $actor:expr, |$tx:ident| $body:expr) => {{
        let $tx = resources::begin($state, $actor).await.map_err(|e| failure(e.into()))?;
        let value = $body.await.map_err(failure)?;
        session::recheck(&$tx, $actor, $state.clock.now_unix())
            .await
            .map_err(|e| failure(e.into()))?;
        $tx.commit()
            .await
            .map_err(|e| failure(KnowledgeError::Auth(crate::neev::error::database_error(e))))?;
        value
    }};
}

fn settings_json(value: &crate::neev::knowledge::types::Settings) -> serde_json::Value {
    serde_json::json!({ "captureEnabled": value.capture_enabled, "revision": value.revision })
}

pub(super) async fn get_settings(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let value = settings::read(&state, &actor).await.map_err(failure)?;
        Ok::<_, Response>(private_json(settings_json(&value)))
    };
    run.await.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsInput {
    capture_enabled: bool,
    expected_revision: i64,
}

pub(super) async fn put_settings(State(state): State<Arc<ServerState>>, request: Request<Body>) -> Response {
    let run = async {
        let (input, actor) = admin_mutation::<SettingsInput>(&state, request).await?;
        let value = settings::set(&state, &actor, input.capture_enabled, input.expected_revision)
            .await
            .map_err(failure)?;
        Ok::<_, Response>(private_json(settings_json(&value)))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn get_status(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let value = read_tx!(&state, &actor, |tx| status::compute(&state, &tx, actor.org_id));
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorInput {
    cursor: Option<String>,
}

fn opaque(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(super) async fn get_sources(
    State(state): State<Arc<ServerState>>,
    query: Result<Query<CursorInput>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let Ok(Query(input)) = query else {
            return Err(ApiError::Invalid.into_response());
        };
        if input.cursor.as_deref().is_some_and(|c| !opaque(c)) {
            return Err(ApiError::Invalid.into_response());
        }
        let value = read_tx!(&state, &actor, |tx| status::sources(&tx, actor.org_id, input.cursor.as_deref()));
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartInput {
    mode: JobMode,
    provider: String,
    model: String,
}

pub(super) async fn start_job(State(state): State<Arc<ServerState>>, request: Request<Body>) -> Response {
    let run = async {
        let (input, actor) = admin_mutation::<StartInput>(&state, request).await?;
        let job = jobs::start(&state, &actor, input.mode, &input.provider, &input.model)
            .await
            .map_err(failure)?;
        let value = status::job_dto(&state.database, &job).await.map_err(failure)?;
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn current_job(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let value = read_tx!(&state, &actor, |tx| async {
            match jobs::current(&tx, actor.org_id).await? {
                Some(job) => status::job_dto(&tx, &job).await,
                None => Ok(serde_json::Value::Null),
            }
        });
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

pub(super) async fn control_job(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let run = async {
        let Ok(Path((id, action))) = path else {
            return Err(ApiError::Invalid.into_response());
        };
        let (_, actor) = admin_mutation::<EmptyInput>(&state, request).await?;
        if !opaque(&id) {
            return Err(ApiError::NotFound.into_response());
        }
        let job = match action.as_str() {
            "pause" => jobs::pause(&state, &actor, &id).await,
            "resume" => jobs::resume(&state, &actor, &id).await,
            "stop" => jobs::stop(&state, &actor, &id).await,
            _ => return Err(ApiError::NotFound.into_response()),
        }
        .map_err(failure)?;
        let value = status::job_dto(&state.database, &job).await.map_err(failure)?;
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn reset_uncovered(State(state): State<Arc<ServerState>>, request: Request<Body>) -> Response {
    let run = async {
        let (_, actor) = admin_mutation::<EmptyInput>(&state, request).await?;
        let changed = jobs::retry_uncovered(&state, &actor).await.map_err(failure)?;
        Ok::<_, Response>(private_json(serde_json::json!({ "reset": changed })))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn get_pages(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let value = read_tx!(&state, &actor, |tx| status::pages(&state, &tx, actor.org_id));
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn get_page(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let Ok(Path(id)) = path else {
            return Err(ApiError::NotFound.into_response());
        };
        if !opaque(&id) {
            return Err(ApiError::NotFound.into_response());
        }
        let value = read_tx!(&state, &actor, |tx| status::page(&state, &tx, actor.org_id, &id));
        match value {
            Some(value) => Ok::<_, Response>(private_json(value)),
            None => Err(ApiError::NotFound.into_response()),
        }
    };
    run.await.unwrap_or_else(|response| response)
}

fn text_response(content_type: &'static str, body: Vec<u8>) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    add_private_headers(&mut response);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
        .headers_mut()
        .insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    response
}

pub(super) async fn get_index(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let text = read_tx!(&state, &actor, |tx| async {
            let (open, withheld) = status::pages_split(&state, &tx, actor.org_id).await?;
            Ok::<_, KnowledgeError>(status::index_markdown(&open, &withheld))
        });
        Ok::<_, Response>(text_response("text/markdown; charset=utf-8", text.into_bytes()))
    };
    run.await.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BeforeInput {
    before: Option<i64>,
}

pub(super) async fn get_log(
    State(state): State<Arc<ServerState>>,
    query: Result<Query<BeforeInput>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let Ok(Query(input)) = query else {
            return Err(ApiError::Invalid.into_response());
        };
        if input.before.is_some_and(|b| b <= 0) {
            return Err(ApiError::Invalid.into_response());
        }
        let value = read_tx!(&state, &actor, |tx| status::log_entries(&state, &tx, actor.org_id, input.before));
        Ok::<_, Response>(private_json(value))
    };
    run.await.unwrap_or_else(|response| response)
}

pub(super) async fn get_export(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    let run = async {
        let actor = admin_get(&state, &headers).await?;
        let bytes = read_tx!(&state, &actor, |tx| export::zip(&state, &tx, actor.org_id));
        let mut response = text_response("application/zip", bytes);
        let name = format!(
            "attachment; filename=\"knowledge-{}-{}.zip\"",
            actor.org_id,
            state.clock.now_unix()
        );
        if let Ok(value) = HeaderValue::from_str(&name) {
            response.headers_mut().insert(header::CONTENT_DISPOSITION, value);
        }
        Ok::<_, Response>(response)
    };
    run.await.unwrap_or_else(|response| response)
}
