use axum::http::{header, HeaderMap, HeaderValue};
use chrono::{DateTime, Utc};

use crate::team_access::{
    secrets::hash_browser_secret,
    session::{browser_cookie, LocalSessionError},
};

pub(super) const SESSION: &str = "workos_team_session";
pub(super) const CSRF: &str = "workos_team_csrf";
pub(super) const OAUTH_BROWSER: &str = "workos_team_oauth_browser";
pub(super) const PENDING: &str = "workos_team_pending";
pub(super) const PENDING_CSRF: &str = "workos_team_pending_csrf";
const CSRF_HEADER: &str = "x-workos-csrf";

pub(super) fn named<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, LocalSessionError> {
    browser_cookie(headers, name)
}

pub(super) fn required<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<&'a str, LocalSessionError> {
    named(headers, name)?.ok_or(LocalSessionError::Unauthorized)
}

pub(super) fn csrf_header(headers: &HeaderMap) -> Result<&str, LocalSessionError> {
    let mut values = headers.get_all(CSRF_HEADER).iter();
    let value = values.next().ok_or(LocalSessionError::Unauthorized)?;
    if values.next().is_some() {
        return Err(LocalSessionError::Unauthorized);
    }
    let value = value.to_str().map_err(|_| LocalSessionError::Unauthorized)?;
    hash_browser_secret(value).map_err(|_| LocalSessionError::Unauthorized)?;
    Ok(value)
}

pub(super) fn exact_origin(headers: &HeaderMap, expected: &str) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return false;
    };
    origins.next().is_none() && origin.as_bytes() == expected.as_bytes()
}

pub(super) fn csrf_matches(raw: &str, expected_hash: &str) -> bool {
    hash_browser_secret(raw).is_ok_and(|actual| actual == expected_hash)
}

pub(super) fn set_cookie(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
    secure: bool,
) {
    let max_age = expires_at.signed_duration_since(now).num_seconds().max(0);
    append_cookie(headers, name, value, max_age, secure);
}

pub(super) fn expire_cookie(headers: &mut HeaderMap, name: &str, secure: bool) {
    append_cookie(headers, name, "", 0, secure);
}

fn append_cookie(headers: &mut HeaderMap, name: &str, value: &str, max_age: i64, secure: bool) {
    let secure_attribute = if secure { "; Secure" } else { "" };
    let value = format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure_attribute}"
    );
    if let Ok(value) = HeaderValue::from_str(&value) {
        headers.append(header::SET_COOKIE, value);
    }
}

pub(super) fn secure(origin: &reqwest::Url) -> bool {
    origin.scheme() == "https"
}

pub(super) fn is_json(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}
