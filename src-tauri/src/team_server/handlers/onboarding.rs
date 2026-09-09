use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{json, ApiError};
use crate::team_server::{
    cookies,
    projection::{parse_github_id, OnboardingDto},
    state::TeamState,
};

const MAX_BODY_BYTES: usize = 16 * 1024;

pub(in crate::team_server) async fn choices(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let pending_secret = match cookies::required(request.headers(), cookies::PENDING) {
        Ok(secret) => secret,
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let csrf_secret = match cookies::required(request.headers(), cookies::PENDING_CSRF) {
        Ok(secret) => secret,
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let choices = match state.onboarding.choices(pending_secret).await {
        Ok(choices) => choices,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !cookies::csrf_matches(csrf_secret, &choices.csrf_hash) {
        return ApiError::Unauthorized.into_response();
    }
    json(OnboardingDto::new(choices, csrf_secret))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    github_org_id: String,
}

pub(in crate::team_server) async fn select(
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
    let csrf_header = match cookies::csrf_header(request.headers()) {
        Ok(secret) => secret.to_owned(),
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let pending_secret = match cookies::required(request.headers(), cookies::PENDING) {
        Ok(secret) => secret.to_owned(),
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    let csrf_cookie = match cookies::required(request.headers(), cookies::PENDING_CSRF) {
        Ok(secret) => secret,
        Err(_) => return ApiError::Unauthorized.into_response(),
    };
    if csrf_cookie != csrf_header {
        return ApiError::Unauthorized.into_response();
    }
    let previous_session = match cookies::named(request.headers(), cookies::SESSION) {
        Ok(value) => value.map(str::to_owned),
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    let (_, body) = request.into_parts();
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    let selection: Selection = match serde_json::from_slice(&body) {
        Ok(selection) => selection,
        Err(_) => return ApiError::InvalidRequest.into_response(),
    };
    let Some(github_org_id) = parse_github_id(&selection.github_org_id) else {
        return ApiError::InvalidRequest.into_response();
    };
    let session = match state
        .onboarding
        .select(
            &pending_secret,
            &csrf_header,
            github_org_id,
            previous_session.as_deref(),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if session.user_id <= 0 || session.org_id <= 0 {
        return ApiError::Unavailable.into_response();
    }
    let mut response = json(serde_json::json!({"redirect":"/workspace"}));
    let secure = cookies::secure(&state.config.origin);
    let now = state.clock.now();
    cookies::expire_cookie(response.headers_mut(), cookies::PENDING, secure);
    cookies::expire_cookie(response.headers_mut(), cookies::PENDING_CSRF, secure);
    cookies::set_cookie(
        response.headers_mut(),
        cookies::SESSION,
        &session.session_secret,
        session.expires_at,
        now,
        secure,
    );
    cookies::set_cookie(
        response.headers_mut(),
        cookies::CSRF,
        &session.csrf_secret,
        session.expires_at,
        now,
        secure,
    );
    response
}
