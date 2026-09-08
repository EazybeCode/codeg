use chrono::{DateTime, Duration, Utc};
use reqwest::{header, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Deserializer};

use super::{GithubOAuthClient, GithubTokenSet, OAuthError};

const MAX_BODY_BYTES: usize = 65_536;
const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_LIFETIME_SECONDS: i64 = 366 * 24 * 60 * 60;

#[derive(Default)]
enum Field<T> {
    #[default]
    Missing,
    Present(T),
}

impl<'de, T> Deserialize<'de> for Field<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
struct WireTokenResponse {
    #[serde(default)]
    access_token: Field<String>,
    #[serde(default)]
    token_type: Field<String>,
    #[serde(default)]
    expires_in: Field<i64>,
    #[serde(default)]
    refresh_token: Field<String>,
    #[serde(default)]
    refresh_token_expires_in: Field<i64>,
    #[serde(default)]
    error: Field<String>,
}

enum ExchangeKind {
    Code,
    Refresh,
}

pub(super) async fn exchange_code(
    client: &GithubOAuthClient,
    code: &str,
    verifier: &str,
    now: DateTime<Utc>,
) -> Result<GithubTokenSet, OAuthError> {
    let form = [
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", client.callback_url.as_str()),
        ("code_verifier", verifier),
    ];
    request_tokens(client, &form, now, ExchangeKind::Code).await
}

pub(super) async fn refresh(
    client: &GithubOAuthClient,
    refresh_token: &str,
    now: DateTime<Utc>,
) -> Result<GithubTokenSet, OAuthError> {
    let form = [
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    request_tokens(client, &form, now, ExchangeKind::Refresh).await
}

async fn request_tokens(
    client: &GithubOAuthClient,
    form: &[(&str, &str)],
    now: DateTime<Utc>,
    kind: ExchangeKind,
) -> Result<GithubTokenSet, OAuthError> {
    let url = endpoint_url(&client.origin)?;
    let mut response = client
        .http
        .post(url)
        .header(header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|_| OAuthError::Unavailable)?;
    let status = response.status();
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|value| value.as_bytes() == b"0")
                || response.headers().contains_key(header::RETRY_AFTER)));
    if status.is_redirection() {
        return Err(OAuthError::Unavailable);
    }
    let body = bounded_body(&mut response).await?;

    if status == StatusCode::UNAUTHORIZED {
        if deserialize::<WireTokenResponse>(&body)
            .is_ok_and(|wire| has_error(&wire) && has_token_fields(&wire))
        {
            return Err(OAuthError::Unavailable);
        }
        return Err(OAuthError::ReauthenticationRequired);
    }
    if rate_limited || status.is_server_error() {
        return Err(OAuthError::Unavailable);
    }

    let wire: WireTokenResponse = deserialize(&body)?;
    if has_error(&wire) && has_token_fields(&wire) {
        return Err(OAuthError::Unavailable);
    }
    if let Field::Present(error) = &wire.error {
        return Err(oauth_error(error));
    }
    if status != StatusCode::OK {
        return Err(OAuthError::Unavailable);
    }
    validate_tokens(wire, now, kind)
}

async fn bounded_body(response: &mut reqwest::Response) -> Result<Vec<u8>, OAuthError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| OAuthError::Unavailable)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(OAuthError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn deserialize<T: DeserializeOwned>(body: &[u8]) -> Result<T, OAuthError> {
    serde_json::from_slice(body).map_err(|_| OAuthError::Unavailable)
}

fn validate_tokens(
    wire: WireTokenResponse,
    now: DateTime<Utc>,
    kind: ExchangeKind,
) -> Result<GithubTokenSet, OAuthError> {
    let access_token = present(wire.access_token)?;
    let token_type = present(wire.token_type)?;
    if token_type != "bearer" || !valid_token(&access_token) {
        return Err(OAuthError::Unavailable);
    }

    match (
        wire.expires_in,
        wire.refresh_token,
        wire.refresh_token_expires_in,
    ) {
        (Field::Missing, Field::Missing, Field::Missing) if matches!(kind, ExchangeKind::Code) => {
            Ok(GithubTokenSet {
                access_token,
                refresh_token: None,
                access_expires_at: None,
                refresh_expires_at: None,
            })
        }
        (
            Field::Present(access_seconds),
            Field::Present(refresh_token),
            Field::Present(refresh_seconds),
        ) => {
            if !valid_token(&refresh_token) {
                return Err(OAuthError::Unavailable);
            }
            let access_expires_at = checked_expiry(now, access_seconds)?;
            let refresh_expires_at = checked_expiry(now, refresh_seconds)?;
            Ok(GithubTokenSet {
                access_token,
                refresh_token: Some(refresh_token),
                access_expires_at: Some(access_expires_at),
                refresh_expires_at: Some(refresh_expires_at),
            })
        }
        _ => Err(OAuthError::Unavailable),
    }
}

fn present<T>(field: Field<T>) -> Result<T, OAuthError> {
    match field {
        Field::Present(value) => Ok(value),
        Field::Missing => Err(OAuthError::Unavailable),
    }
}

fn checked_expiry(now: DateTime<Utc>, seconds: i64) -> Result<DateTime<Utc>, OAuthError> {
    if seconds <= 0 {
        return Err(OAuthError::Unavailable);
    }
    now.checked_add_signed(Duration::seconds(seconds.min(MAX_LIFETIME_SECONDS)))
        .ok_or(OAuthError::Unavailable)
}

fn valid_token(token: &str) -> bool {
    (1..=MAX_TOKEN_BYTES).contains(&token.len())
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn has_error(wire: &WireTokenResponse) -> bool {
    matches!(wire.error, Field::Present(_))
}

fn has_token_fields(wire: &WireTokenResponse) -> bool {
    matches!(wire.access_token, Field::Present(_))
        || matches!(wire.token_type, Field::Present(_))
        || matches!(wire.expires_in, Field::Present(_))
        || matches!(wire.refresh_token, Field::Present(_))
        || matches!(wire.refresh_token_expires_in, Field::Present(_))
}

fn oauth_error(error: &str) -> OAuthError {
    match error {
        "bad_verification_code" | "bad_refresh_token" | "access_denied" | "expired_token" => {
            OAuthError::ReauthenticationRequired
        }
        _ => OAuthError::Unavailable,
    }
}

fn endpoint_url(origin: &Url) -> Result<Url, OAuthError> {
    let mut url = origin.clone();
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| OAuthError::Unavailable)?;
    path.clear();
    path.extend(["login", "oauth", "access_token"]);
    drop(path);
    Ok(url)
}
