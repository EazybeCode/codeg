use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, RawQuery, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::neev::{
    auth::{magic, oauth, session},
    error::AuthError,
    startup::ServerState,
    types::{Provider, Role, SignInIntent},
};

use super::{cookies, private_json, ApiError};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDto {
    user: UserDto,
    organization: Option<OrganizationDto>,
    role: Option<&'static str>,
    scope: &'static str,
    csrf: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserDto {
    id: i32,
    display_name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationDto {
    id: i32,
    name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
struct MethodsDto {
    methods: Vec<String>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailRequest {
    email: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {
    token: String,
}

#[derive(Serialize)]
struct AcceptedDto {
    status: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct ConfirmationDto {
    next: &'static str,
}

const ACCEPTED_MESSAGE: &str =
    "If delivery is available, your sign-in email may arrive shortly. Delivery can be delayed; retry later if needed.";
// Even the largest native JSON request (a <=1.5 MiB branding payload) must
// finish uploading promptly enough that slow clients cannot retain all 32
// shared request permits. This deadline covers body collection only; database,
// OAuth and mail work retain their existing cancellation boundaries.
const BODY_READ_DEADLINE: Duration = Duration::from_secs(30);

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
    let now = state.clock.now_unix();
    let view = session::view(&state.database, token, Some(raw_csrf), now).await?;
    let organization = view.organization.map(|organization| OrganizationDto {
        id: organization.id,
        name: organization.name,
        avatar_url: organization.logo_data,
    });
    let role = view.role.map(role_name);
    let scope = if organization.is_some() {
        "organization"
    } else {
        "identity"
    };
    Ok(private_json(SessionDto {
        user: UserDto {
            id: view.user.id,
            display_name: view.user.display_name,
            avatar_url: view.user.avatar_url,
        },
        organization,
        role,
        scope,
        csrf: raw_csrf.to_owned(),
    }))
}

pub(super) async fn methods(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&headers, names.session)?;
    let methods = session::methods(&state.database, token, state.clock.now_unix()).await?;
    Ok(private_json(MethodsDto { methods }))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
    }
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
    let body = read_body(body, 1024).await?;
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
    session::revoke_with_gateway(
        &state.database,
        &identity,
        state.clock.as_ref(),
        state.config.gateway_events,
    )
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    cookies::clear_browser_state(response.headers_mut(), names);
    super::add_private_headers(&mut response);
    Ok(response)
}

/// Browser-facing gateway exchange-issue (integration-gated). Native org session
/// + exact Origin + session CSRF; the bounded wire envelope is handled by the
/// gateway codec. Returns the one-use code envelope with the wire HTTP status.
pub(super) async fn collaboration_issue(
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
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&parts.headers, names.session)?.to_owned();
    let cookie_csrf = cookies::required(&parts.headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&parts.headers)? {
        return Err(ApiError::Unauthorized);
    }
    let now = state.clock.now_unix();
    session::view(&state.database, &token, Some(cookie_csrf), now).await?;
    let context = session::organization(&state.database, &token, now).await?;
    let body = read_body(body, 16_384).await?;
    let encoded =
        crate::neev::gateway::routes::issue(&state.database, &context, state.clock.as_ref(), &body)
            .await;
    let status = StatusCode::from_u16(encoded.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(encoded.body))
        .map_err(|_| ApiError::Unavailable)?;
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
    let result = google_callback_result(&state, &headers, raw.as_deref()).await;
    match result {
        Err(error) if accepts_html(&headers) => {
            // Browser recovery is a fixed UI hint, never a redirect target or
            // provider diagnostic. API callers retain their safe status/JSON.
            let destination = match error {
                ApiError::Conflict => "/login?auth_error=account_conflict",
                _ => "/login?auth_error=sign_in_failed",
            };
            let mut response = Redirect::to(destination).into_response();
            super::add_private_headers(&mut response);
            Ok(response)
        }
        response => response,
    }
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get_all(axum::http::header::ACCEPT)
        .iter()
        .any(|value| {
            value.to_str().is_ok_and(|value| {
                value.split(',').any(|range| {
                    let mut parts = range.split(';');
                    if !parts
                        .next()
                        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
                    {
                        return false;
                    }
                    parts
                        .filter_map(|part| part.trim().split_once('='))
                        .all(|(key, value)| {
                            !key.trim().eq_ignore_ascii_case("q")
                                || value
                                    .trim()
                                    .parse::<f32>()
                                    .is_ok_and(|quality| quality > 0.0 && quality <= 1.0)
                        })
                })
            })
        })
}

async fn google_callback_result(
    state: &ServerState,
    headers: &HeaderMap,
    raw: Option<&str>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (state_token, result) = callback_query(raw)?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(headers, names.browser)?;
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

pub(super) async fn email_login(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (headers, input) =
        json_request::<EmailRequest>(request, &state.config.origin_header).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(&headers, names.browser)?;
    let cookie_csrf = cookies::required(&headers, names.preauth_csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let source = source_ip(&headers, peer, &state.config.trusted_proxy_ips)?;
    let service = state.magic.as_ref().ok_or(ApiError::Unavailable)?;
    let outcome = magic::request(service, &input.email, browser, source, SignInIntent::Login).await;
    record_mail_request(outcome);
    let mut response = private_json(AcceptedDto {
        status: "accepted",
        message: ACCEPTED_MESSAGE,
    });
    *response.status_mut() = StatusCode::ACCEPTED;
    cookies::refresh_preauth(response.headers_mut(), names, browser, cookie_csrf);
    Ok(response)
}

pub(super) async fn email_link(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (headers, input) =
        json_request::<EmailRequest>(request, &state.config.origin_header).await?;
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
    let source = source_ip(&headers, peer, &state.config.trusted_proxy_ips)?;
    let service = state.magic.as_ref().ok_or(ApiError::Unavailable)?;
    let outcome = magic::request(
        service,
        &input.email,
        browser,
        source,
        SignInIntent::Link {
            user_id: identity.user_id,
            session_id: identity.session_id,
            auth_revision: identity.auth_revision,
        },
    )
    .await;
    record_mail_request(outcome);
    let mut response = private_json(AcceptedDto {
        status: "accepted",
        message: ACCEPTED_MESSAGE,
    });
    *response.status_mut() = StatusCode::ACCEPTED;
    cookies::refresh_browser(response.headers_mut(), names, browser);
    Ok(response)
}

pub(super) async fn email_confirm(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)?;
    let (headers, input) =
        json_request::<ConfirmRequest>(request, &state.config.origin_header).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let browser = cookies::required(&headers, names.browser)?;
    let cookie_csrf = cookies::required(&headers, names.preauth_csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let service = state.magic.as_ref().ok_or(ApiError::Unavailable)?;
    match magic::confirm(service, &input.token, browser).await? {
        magic::Completion::LoggedIn(secrets) => {
            let mut response = private_json(ConfirmationDto {
                next: "/onboarding",
            });
            cookies::set_session(
                response.headers_mut(),
                names,
                &secrets.session,
                &secrets.csrf,
                secrets.expires_at.saturating_sub(state.clock.now_unix()),
            );
            Ok(response)
        }
        magic::Completion::Linked => Ok(private_json(ConfirmationDto { next: "/settings" })),
    }
}

fn source_ip(
    headers: &HeaderMap,
    peer: SocketAddr,
    trusted_proxies: &[IpAddr],
) -> Result<IpAddr, ApiError> {
    let mut forwarded = headers.get_all("x-forwarded-for").iter();
    let first = forwarded.next();
    let peer_ip = crate::neev::config::normalize_ip(peer.ip());
    if !trusted_proxies.contains(&peer_ip) {
        return if first.is_none() {
            Ok(peer_ip)
        } else {
            Err(ApiError::Forbidden)
        };
    }
    let value = first
        .filter(|_| forwarded.next().is_none())
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.contains(','))
        .ok_or(ApiError::Forbidden)?;
    value
        .parse()
        .map(crate::neev::config::normalize_ip)
        .map_err(|_| ApiError::Forbidden)
}

fn record_mail_request(result: Result<(), AuthError>) {
    let reason = match result {
        Ok(()) => "submitted",
        Err(AuthError::Limited) => "limited",
        Err(AuthError::Invalid) => "invalid",
        Err(_) => "unavailable",
    };
    tracing::info!(reason, "Neev email request handled");
}

async fn json_request<T: DeserializeOwned>(
    request: Request<Body>,
    expected_origin: &str,
) -> Result<(HeaderMap, T), ApiError> {
    json_request_limited(request, expected_origin, 1024).await
}

pub(super) async fn json_request_limited<T: DeserializeOwned>(
    request: Request<Body>,
    expected_origin: &str,
    limit: usize,
) -> Result<(HeaderMap, T), ApiError> {
    let (parts, body) = request.into_parts();
    if !cookies::exact_origin(&parts.headers, expected_origin) {
        return Err(ApiError::Forbidden);
    }
    if !cookies::is_json(&parts.headers) {
        return Err(ApiError::Invalid);
    }
    let body = read_body(body, limit).await?;
    let input = serde_json::from_slice(&body).map_err(|_| ApiError::Invalid)?;
    Ok((parts.headers, input))
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
    let body = read_body(body, 1024).await?;
    if serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_none_or(|object| !object.is_empty())
    {
        return Err(ApiError::Invalid);
    }
    Ok(parts.headers)
}

async fn read_body(body: Body, limit: usize) -> Result<axum::body::Bytes, ApiError> {
    tokio::time::timeout(BODY_READ_DEADLINE, to_bytes(body, limit))
        .await
        .map_err(|_| ApiError::RequestTimeout)?
        .map_err(|_| ApiError::Invalid)
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
            "state" => {
                if state.is_some() || value.len() > 96 {
                    return Err(ApiError::Invalid);
                }
                state = Some(value.into_owned());
            }
            "code" => {
                if code.is_some() || value.len() > 2048 {
                    return Err(ApiError::Invalid);
                }
                code = Some(value.into_owned());
            }
            "error" => {
                if denied.is_some() || value.is_empty() || value.len() > 128 {
                    return Err(ApiError::Invalid);
                }
                denied = Some(());
            }
            "error_description" => {
                if error_description.is_some() || value.len() > 512 {
                    return Err(ApiError::Invalid);
                }
                error_description = Some(())
            }
            "error_uri" => {
                if error_uri.is_some() || value.len() > 2048 {
                    return Err(ApiError::Invalid);
                }
                error_uri = Some(());
            }
            _ => {}
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
    fn callback_metadata_is_ignored_but_core_parameters_remain_unambiguous() {
        let parsed = callback_query(Some(
            "state=n1.oauth.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&error=access_denied&error_description=cancelled&error_uri=https%3A%2F%2Faccounts.google.com%2Fhelp&future_google_extension=opaque-value",
        ));
        assert!(matches!(parsed, Ok((_, oauth::CallbackResult::Denied))));
        assert!(callback_query(Some("state=one&state=two&error=access_denied")).is_err());
        assert!(callback_query(Some("state=one&code=first&code=second")).is_err());
        assert!(callback_query(Some("state=one&error=first&error=second")).is_err());
        assert!(callback_query(Some("state=one&code=code&error=access_denied")).is_err());
        assert!(callback_query(Some(&format!(
            "state=one&code=code&unused={}",
            "a".repeat(4096)
        )))
        .is_err());
    }

    #[test]
    fn forwarded_ip_is_accepted_only_from_an_exact_trusted_peer() {
        use axum::http::HeaderValue;
        use std::net::SocketAddr;

        let direct: SocketAddr = "127.0.0.1:40100".parse().unwrap();
        let trusted = vec![direct.ip()];
        let empty = HeaderMap::new();
        assert_eq!(source_ip(&empty, direct, &[]).unwrap(), direct.ip());
        assert!(source_ip(&empty, direct, &trusted).is_err());

        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.44"));
        assert!(source_ip(&forwarded, direct, &[]).is_err());
        assert_eq!(
            source_ip(&forwarded, direct, &trusted).unwrap(),
            "203.0.113.44".parse::<IpAddr>().unwrap()
        );
        forwarded.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.44, 198.51.100.2"),
        );
        assert!(source_ip(&forwarded, direct, &trusted).is_err());
        forwarded.append("x-forwarded-for", HeaderValue::from_static("198.51.100.2"));
        assert!(source_ip(&forwarded, direct, &trusted).is_err());
    }
}
