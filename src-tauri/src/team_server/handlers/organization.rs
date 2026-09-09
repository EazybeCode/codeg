use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Request, State},
    response::{IntoResponse, Response},
};

use super::{json, ApiError};
use crate::{
    team_access::{principal::ProofRequirement, session::TeamRole},
    team_server::{
        cookies,
        organization_settings::{read_current, update_current, OrganizationPatchInput},
        state::TeamState,
    },
};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub(in crate::team_server) async fn get(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match state
        .principals
        .resolve(request.headers(), ProofRequirement::Cached)
        .await
    {
        Ok(context) => context,
        Err(error) => return ApiError::from(error).into_response(),
    };
    match read_current(&state, &context).await {
        Ok(settings) => json(settings),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn patch(
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
    let context = match state
        .principals
        .resolve(request.headers(), ProofRequirement::Fresh)
        .await
    {
        Ok(context) => context,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !cookies::csrf_matches(&csrf_header, &context.csrf_hash) {
        return ApiError::Unauthorized.into_response();
    }
    if context.role != TeamRole::Admin {
        return ApiError::Forbidden.into_response();
    }
    let (_, body) = request.into_parts();
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    let input = match serde_json::from_slice::<OrganizationPatchInput>(&body) {
        Ok(input) => input,
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    match update_current(&state, &context, input).await {
        Ok(settings) => json(settings),
        Err(error) => error.into_response(),
    }
}
