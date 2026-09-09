use std::{sync::Arc, time::Duration};

use axum::{
    extract::{MatchedPath, Request, State},
    http::{header, HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use super::{limiter::LimitError, state::TeamState};
use crate::team_access::core::TeamAuthError;

pub(super) mod oauth;
pub(super) mod onboarding;
pub(super) mod session;

const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");

#[derive(Clone, Copy)]
pub(super) enum ApiError {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Unavailable,
    RateLimited,
}

impl From<TeamAuthError> for ApiError {
    fn from(error: TeamAuthError) -> Self {
        match error {
            TeamAuthError::Unauthorized => Self::Unauthorized,
            TeamAuthError::Forbidden => Self::Forbidden,
            TeamAuthError::Conflict => Self::Conflict,
            TeamAuthError::Unavailable => Self::Unavailable,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid_request", "Invalid request"),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication required",
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Access denied"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", "Not found"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict", "Request conflict"),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Service unavailable",
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "unavailable",
                "Too many authentication requests; retry later",
            ),
        };
        let mut response =
            (status, Json(serde_json::json!({"code":code,"message":message}))).into_response();
        if matches!(self, Self::RateLimited) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        add_private_headers(&mut response);
        response
    }
}

pub(super) fn json<T: serde::Serialize>(value: T) -> Response {
    let mut response = Json(value).into_response();
    add_private_headers(&mut response);
    response
}

pub(super) fn redirect(location: &'static str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static(location));
    add_private_headers(&mut response);
    response
}

pub(super) fn redirect_to(location: &str) -> Result<Response, ApiError> {
    let location = HeaderValue::from_str(location).map_err(|_| ApiError::Unavailable)?;
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    add_private_headers(&mut response);
    Ok(response)
}

pub(super) async fn not_found() -> Response {
    ApiError::NotFound.into_response()
}

pub(super) async fn method_not_allowed() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

pub(super) fn add_private_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}

pub(super) async fn bounded_auth(
    State(state): State<Arc<TeamState>>,
    request: Request,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("unmatched")
        .to_owned();
    let permit = match state.limiter.try_acquire() {
        Ok(permit) => permit,
        Err(LimitError::RateLimited) => return ApiError::RateLimited.into_response(),
        Err(LimitError::Unavailable) => return ApiError::Unavailable.into_response(),
    };
    let response = tokio::time::timeout(Duration::from_secs(60), next.run(request)).await;
    drop(permit);
    let mut response = match response {
        Ok(response) => response,
        Err(_) => ApiError::Unavailable.into_response(),
    };
    add_private_headers(&mut response);
    tracing::info!(route, status = response.status().as_u16(), "team HTTP request");
    response
}
