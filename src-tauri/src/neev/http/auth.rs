use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::neev::{auth::session, error::AuthError, startup::ServerState, types::Role};

use super::{cookies, private_json, ApiError};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDto {
    user_id: i32,
    csrf: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization: Option<OrganizationSessionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationSessionDto {
    org_id: i32,
    member_id: i32,
    role: &'static str,
}

#[derive(Serialize)]
struct PreauthDto {
    csrf: String,
}

pub(super) async fn preauth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let mut cookie_headers = HeaderMap::new();
    let secrets = cookies::ensure_preauth(&headers, &mut cookie_headers, names)?;
    let mut response = private_json(PreauthDto { csrf: secrets.csrf });
    for value in cookie_headers.get_all(axum::http::header::SET_COOKIE) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value.clone());
    }
    Ok(response)
}

pub(super) async fn bootstrap(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&headers, names.session)?;
    let raw_csrf = cookies::required(&headers, names.csrf)?;
    let identity = session::identity(&state.database, token, state.clock.now_unix()).await?;
    session::validate_csrf(&state.database, &identity, raw_csrf, state.clock.now_unix()).await?;
    let organization =
        match session::organization(&state.database, token, state.clock.now_unix()).await {
            Ok(context) => Some(OrganizationSessionDto {
                org_id: context.org_id,
                member_id: context.member_id,
                role: match context.role {
                    Role::Admin => "admin",
                    Role::Member => "member",
                },
            }),
            Err(AuthError::Forbidden) => None,
            Err(error) => return Err(error.into()),
        };
    Ok(private_json(SessionDto {
        user_id: identity.user_id,
        csrf: raw_csrf.to_owned(),
        organization,
    }))
}

pub(super) async fn logout(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (parts, body) = request.into_parts();
    if !cookies::exact_origin(&parts.headers, &state.config.origin_header) {
        return Err(ApiError::Forbidden);
    }
    if !cookies::is_json(&parts.headers) {
        return Err(ApiError::Invalid);
    }
    let body = to_bytes(body, 1024).await.map_err(|_| ApiError::Invalid)?;
    if serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_none_or(|object| !object.is_empty())
    {
        return Err(ApiError::Invalid);
    }
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&parts.headers, names.session)?;
    let cookie_csrf = cookies::required(&parts.headers, names.csrf)?;
    let header_csrf = cookies::csrf_header(&parts.headers)?;
    if cookie_csrf != header_csrf {
        return Err(ApiError::Unauthorized);
    }
    let now = state.clock.now_unix();
    let identity = session::identity(&state.database, token, now).await?;
    session::validate_csrf(&state.database, &identity, cookie_csrf, now).await?;
    session::revoke(&state.database, &identity, now).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    cookies::clear_browser_state(response.headers_mut(), names);
    super::add_private_headers(&mut response);
    Ok(response)
}
