use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    extract::{RawQuery, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;

use crate::neev::{
    auth::{oauth, session},
    error::AuthError,
    startup::ServerState,
    types::{Provider, Role, SignInIntent},
};

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OAuthBeginDto {
    authorization_url: String,
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

pub(super) async fn google_login(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let headers = empty_json_request(request, &state.config.origin_header).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(&headers, names.browser)?;
    let cookie_csrf = cookies::required(&headers, names.preauth_csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let authorization_url = state
        .oauth
        .begin(Provider::Google, SignInIntent::Login, browser)
        .await?;
    let mut response = private_json(OAuthBeginDto { authorization_url });
    cookies::refresh_preauth(response.headers_mut(), names, browser, cookie_csrf);
    Ok(response)
}

pub(super) async fn google_link(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let headers = empty_json_request(request, &state.config.origin_header).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(&headers, names.browser)?;
    let token = cookies::required(&headers, names.session)?;
    let cookie_csrf = cookies::required(&headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let now = state.clock.now_unix();
    let identity = session::identity(&state.database, token, now).await?;
    session::validate_csrf(&state.database, &identity, cookie_csrf, now).await?;
    let authorization_url = state
        .oauth
        .begin(
            Provider::Google,
            SignInIntent::Link {
                user_id: identity.user_id,
                session_id: identity.session_id,
                auth_revision: identity.auth_revision,
            },
            browser,
        )
        .await?;
    let mut response = private_json(OAuthBeginDto { authorization_url });
    cookies::refresh_browser(response.headers_mut(), names, browser);
    Ok(response)
}

pub(super) async fn google_callback(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (state_token, result) = callback_query(raw.as_deref())?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(&headers, names.browser)?;
    match state
        .oauth
        .complete(Provider::Google, &state_token, result, browser)
        .await?
    {
        oauth::Completion::Login(secrets) => {
            let mut response = Redirect::to("/onboarding").into_response();
            cookies::set_session(
                response.headers_mut(),
                names,
                &secrets.session,
                &secrets.csrf,
                secrets.expires_at.saturating_sub(state.clock.now_unix()),
            );
            super::add_private_headers(&mut response);
            Ok(response)
        }
        oauth::Completion::Linked => {
            let mut response = Redirect::to("/settings").into_response();
            super::add_private_headers(&mut response);
            Ok(response)
        }
    }
}

async fn empty_json_request(
    request: Request<Body>,
    expected_origin: &str,
) -> Result<HeaderMap, ApiError> {
    let (parts, body) = request.into_parts();
    if !cookies::exact_origin(&parts.headers, expected_origin) {
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
    Ok(parts.headers)
}

fn callback_query(raw: Option<&str>) -> Result<(String, oauth::CallbackResult), ApiError> {
    let raw = raw
        .filter(|value| value.len() <= 4096)
        .ok_or(ApiError::Invalid)?;
    let mut state = None;
    let mut code = None;
    let mut denied = None;
    let mut error_description = None;
    let mut error_uri = None;
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        match key.as_ref() {
            "state" if state.is_none() && value.len() <= 96 => state = Some(value.into_owned()),
            "code" if code.is_none() && value.len() <= 2048 => code = Some(value.into_owned()),
            "error" if denied.is_none() && !value.is_empty() && value.len() <= 128 => {
                denied = Some(())
            }
            "error_description" if error_description.is_none() && value.len() <= 512 => {
                error_description = Some(())
            }
            "error_uri" if error_uri.is_none() && value.len() <= 2048 => error_uri = Some(()),
            _ => return Err(ApiError::Invalid),
        }
    }
    let state = state.ok_or(ApiError::Invalid)?;
    match (code, denied) {
        (Some(code), None) if error_description.is_none() && error_uri.is_none() => {
            Ok((state, oauth::CallbackResult::Code(code)))
        }
        (None, Some(())) => Ok((state, oauth::CallbackResult::Denied)),
        _ => Err(ApiError::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_denial_metadata_is_bounded_but_does_not_prevent_state_claim() {
        let parsed = callback_query(Some(
            "state=n1.oauth.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&error=access_denied&error_description=cancelled&error_uri=https%3A%2F%2Faccounts.google.com%2Fhelp",
        ));
        assert!(matches!(parsed, Ok((_, oauth::CallbackResult::Denied))));
        assert!(callback_query(Some("state=one&state=two&error=access_denied")).is_err());
        assert!(callback_query(Some("state=one&error=access_denied&unexpected=value")).is_err());
    }
}
