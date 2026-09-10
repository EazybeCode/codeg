use axum::http::{header, HeaderMap, HeaderValue};

use crate::neev::{
    auth::secrets::{self, TokenPurpose},
    error::AuthError,
};

const CSRF_HEADER: &str = "x-neev-csrf";

#[derive(Clone, Copy)]
pub(crate) struct CookieNames {
    pub(crate) session: &'static str,
    pub(crate) csrf: &'static str,
    pub(crate) browser: &'static str,
    pub(crate) preauth_csrf: &'static str,
    pub(crate) secure: bool,
}

impl CookieNames {
    pub(crate) fn for_preview(preview: bool) -> Self {
        if preview {
            Self {
                session: "neev_preview_session",
                csrf: "neev_preview_csrf",
                browser: "neev_preview_browser",
                preauth_csrf: "neev_preview_preauth_csrf",
                secure: false,
            }
        } else {
            Self {
                session: "__Host-neev_session",
                csrf: "__Host-neev_csrf",
                browser: "__Host-neev_browser",
                preauth_csrf: "__Host-neev_preauth_csrf",
                secure: true,
            }
        }
    }
}

pub(crate) struct PreauthSecrets {
    pub(crate) browser: String,
    pub(crate) csrf: String,
}

pub(crate) fn named<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AuthError> {
    let mut found = None;
    for raw in headers.get_all(header::COOKIE).iter() {
        let raw = raw.to_str().map_err(|_| AuthError::Unauthorized)?;
        for part in raw.split(';') {
            let Some((key, value)) = part.trim().split_once('=') else {
                return Err(AuthError::Unauthorized);
            };
            if key == name {
                if found.is_some() || value.is_empty() || value.chars().any(char::is_whitespace) {
                    return Err(AuthError::Unauthorized);
                }
                found = Some(value);
            }
        }
    }
    Ok(found)
}

pub(crate) fn required<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, AuthError> {
    named(headers, name)?.ok_or(AuthError::Unauthorized)
}

pub(crate) fn csrf_header(headers: &HeaderMap) -> Result<&str, AuthError> {
    let mut values = headers.get_all(CSRF_HEADER).iter();
    let value = values.next().ok_or(AuthError::Unauthorized)?;
    if values.next().is_some() {
        return Err(AuthError::Unauthorized);
    }
    let value = value.to_str().map_err(|_| AuthError::Unauthorized)?;
    secrets::digest(TokenPurpose::Csrf, value).map_err(|_| AuthError::Unauthorized)?;
    Ok(value)
}

pub(crate) fn exact_origin(headers: &HeaderMap, expected: &str) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return false;
    };
    origins.next().is_none() && origin.as_bytes() == expected.as_bytes()
}

pub(crate) fn is_json(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) fn set_session(
    headers: &mut HeaderMap,
    names: CookieNames,
    session: &str,
    csrf: &str,
    max_age: i64,
) {
    append(headers, names.session, session, max_age, names.secure);
    append(headers, names.csrf, csrf, max_age, names.secure);
}

pub(crate) fn clear_browser_state(headers: &mut HeaderMap, names: CookieNames) {
    for name in [names.session, names.csrf, names.browser, names.preauth_csrf] {
        append(headers, name, "", 0, names.secure);
    }
}

pub(crate) fn refresh_preauth(
    headers: &mut HeaderMap,
    names: CookieNames,
    browser: &str,
    csrf: &str,
) {
    append(headers, names.browser, browser, 600, names.secure);
    append(headers, names.preauth_csrf, csrf, 600, names.secure);
}

pub(crate) fn refresh_browser(headers: &mut HeaderMap, names: CookieNames, browser: &str) {
    append(headers, names.browser, browser, 600, names.secure);
}

pub(crate) fn ensure_preauth(
    request: &HeaderMap,
    response: &mut HeaderMap,
    names: CookieNames,
) -> Result<PreauthSecrets, AuthError> {
    let browser = match named(request, names.browser)? {
        Some(value) => {
            secrets::digest(TokenPurpose::Browser, value).map_err(|_| AuthError::Unauthorized)?;
            value.to_owned()
        }
        None => {
            let value = secrets::random_token(TokenPurpose::Browser)?;
            append(response, names.browser, &value, 600, names.secure);
            value
        }
    };
    let csrf = match named(request, names.preauth_csrf)? {
        Some(value) => {
            secrets::digest(TokenPurpose::Csrf, value).map_err(|_| AuthError::Unauthorized)?;
            value.to_owned()
        }
        None => {
            let value = secrets::random_token(TokenPurpose::Csrf)?;
            append(response, names.preauth_csrf, &value, 600, names.secure);
            value
        }
    };
    Ok(PreauthSecrets { browser, csrf })
}

fn append(headers: &mut HeaderMap, name: &str, value: &str, max_age: i64, secure: bool) {
    let secure = if secure { "; Secure" } else { "" };
    let value = format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        max_age.max(0)
    );
    if let Ok(value) = HeaderValue::from_str(&value) {
        headers.append(header::SET_COOKIE, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_cookies_are_host_only_secure_and_preauth_is_separate_and_stable() {
        let names = CookieNames::for_preview(false);
        let mut response = HeaderMap::new();
        set_session(&mut response, names, "session", "csrf", 1800);
        for value in response.get_all(header::SET_COOKIE).iter() {
            let value = value.to_str().unwrap();
            assert!(value.starts_with("__Host-neev_"));
            assert!(value.contains("; Secure"));
            assert!(value.contains("; HttpOnly"));
            assert!(value.contains("; SameSite=Lax"));
            assert!(!value.contains("Domain="));
        }

        let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
        let auth_csrf = secrets::random_token(TokenPurpose::Csrf).unwrap();
        let mut request = HeaderMap::new();
        request.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "{}={browser}; {}={auth_csrf}",
                names.browser, names.csrf
            ))
            .unwrap(),
        );
        let mut preauth_response = HeaderMap::new();
        let preauth = ensure_preauth(&request, &mut preauth_response, names).unwrap();
        assert_eq!(preauth.browser, browser);
        assert_ne!(names.csrf, names.preauth_csrf);
        assert_ne!(preauth.csrf, auth_csrf);
        assert_eq!(
            preauth_response.get_all(header::SET_COOKIE).iter().count(),
            1
        );
    }

    #[test]
    fn duplicate_cookie_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("a=1; a=2"));
        assert_eq!(named(&headers, "a"), Err(AuthError::Unauthorized));
    }

    #[test]
    fn preview_cookie_cannot_fill_a_production_session_slot() {
        let preview = CookieNames::for_preview(true);
        let production = CookieNames::for_preview(false);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}=preview", preview.session)).unwrap(),
        );
        assert_eq!(named(&headers, production.session), Ok(None));
    }
}
