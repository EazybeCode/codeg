use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Request, State},
    response::{IntoResponse, Response},
};

use super::{redirect, redirect_to};
use crate::{
    team_access::core::TeamAuthError,
    team_server::{cookies, state::TeamState},
};

const MAX_CALLBACK_QUERY_BYTES: usize = 8_192;
const MAX_CALLBACK_VALUE_BYTES: usize = 4_096;

pub(in crate::team_server) async fn login(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let malformed_session = cookies::named(request.headers(), cookies::SESSION).is_err()
        || cookies::named(request.headers(), cookies::CSRF).is_err();
    let started = match state.onboarding.begin().await {
        Ok(started) => started,
        Err(_) => return callback_failure(&state, "unavailable"),
    };
    let authorization_url = match state.oauth.authorization_url(&started.state, &started.verifier) {
        Ok(url) => url,
        Err(_) => return callback_failure(&state, "unavailable"),
    };
    let mut response = match redirect_to(authorization_url.as_str()) {
        Ok(response) => response,
        Err(error) => return error.into_response(),
    };
    let now = state.clock.now();
    let secure = cookies::secure(&state.config.origin);
    cookies::set_cookie(
        response.headers_mut(),
        cookies::OAUTH_BROWSER,
        &started.browser_secret,
        started.expires_at,
        now,
        secure,
    );
    if malformed_session {
        cookies::expire_cookie(response.headers_mut(), cookies::SESSION, secure);
        cookies::expire_cookie(response.headers_mut(), cookies::CSRF, secure);
    }
    response
}

pub(in crate::team_server) async fn callback(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let query = match parse_callback_query(request.uri().query()) {
        Ok(query) => query,
        Err(()) => return callback_failure(&state, "invalid_login"),
    };
    let browser_cookie = cookies::named(request.headers(), cookies::OAUTH_BROWSER);
    if query.state.is_none() && matches!(browser_cookie, Ok(None)) {
        return redirect("/auth/github/login");
    }
    let browser_cookie = match browser_cookie {
        Ok(Some(cookie)) => cookie,
        _ => return callback_failure(&state, "invalid_login"),
    };
    let Some(callback_state) = query.state.as_deref() else {
        return callback_failure(&state, "invalid_login");
    };
    if query.error.is_some() {
        let cancellation = state
            .onboarding
            .cancel_attempt(callback_state, browser_cookie)
            .await;
        if let Err(error) = cancellation {
            return match error {
                TeamAuthError::Unavailable => callback_failure(&state, "unavailable"),
                _ => callback_failure(&state, "invalid_login"),
            };
        }
        let reason = if query.error.as_deref() == Some("access_denied") {
            "access_denied"
        } else {
            "invalid_login"
        };
        return callback_failure(&state, reason);
    }
    let Some(code) = query.code.as_deref() else {
        return callback_failure(&state, "invalid_login");
    };
    let pending = match state
        .onboarding
        .complete(callback_state, browser_cookie, code)
        .await
    {
        Ok(pending) => pending,
        Err(TeamAuthError::Unavailable) => return callback_failure(&state, "unavailable"),
        Err(_) => return callback_failure(&state, "invalid_login"),
    };
    let mut response = redirect("/onboarding");
    let now = state.clock.now();
    let secure = cookies::secure(&state.config.origin);
    cookies::expire_cookie(response.headers_mut(), cookies::OAUTH_BROWSER, secure);
    cookies::set_cookie(
        response.headers_mut(),
        cookies::PENDING,
        &pending.browser_secret,
        pending.expires_at,
        now,
        secure,
    );
    cookies::set_cookie(
        response.headers_mut(),
        cookies::PENDING_CSRF,
        &pending.csrf_secret,
        pending.expires_at,
        now,
        secure,
    );
    response
}

fn callback_failure(state: &TeamState, reason: &'static str) -> Response {
    tracing::info!(outcome = reason, "team OAuth callback outcome");
    let location = match reason {
        "access_denied" => "/login?reason=access_denied",
        "unavailable" => "/login?reason=unavailable",
        _ => "/login?reason=invalid_login",
    };
    let mut response = redirect(location);
    cookies::expire_cookie(
        response.headers_mut(),
        cookies::OAUTH_BROWSER,
        cookies::secure(&state.config.origin),
    );
    response
}

#[derive(Default)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

fn parse_callback_query(raw: Option<&str>) -> Result<CallbackQuery, ()> {
    let raw = raw.unwrap_or_default();
    if raw.len() > MAX_CALLBACK_QUERY_BYTES {
        return Err(());
    }
    let mut recognized = HashMap::<String, String>::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decode(name)?;
        if !matches!(name.as_str(), "state" | "code" | "error" | "error_description") {
            continue;
        }
        let value = percent_decode(value)?;
        if value.is_empty() || value.len() > MAX_CALLBACK_VALUE_BYTES {
            return Err(());
        }
        if recognized.insert(name, value).is_some() {
            return Err(());
        }
    }
    let query = CallbackQuery {
        state: recognized.remove("state"),
        code: recognized.remove("code"),
        error: recognized.remove("error"),
    };
    if query.code.is_some() && query.error.is_some() {
        return Err(());
    }
    Ok(query)
}

fn percent_decode(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = *bytes.get(index + 1).ok_or(())?;
                let low = *bytes.get(index + 2).ok_or(())?;
                decoded.push(hex(high)? * 16 + hex(low)?);
                index += 2;
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}
