use std::{net::IpAddr, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::Url;

use super::secrets::pkce_challenge;

#[path = "oauth_http.rs"]
mod oauth_http;

const GITHUB_ORIGIN: &str = "https://github.com";
const PRODUCTION_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_INPUT_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum OAuthError {
    #[error("GitHub reauthentication required")]
    ReauthenticationRequired,
    #[error("GitHub OAuth unavailable")]
    Unavailable,
}

pub(crate) struct GithubTokenSet {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) access_expires_at: Option<DateTime<Utc>>,
    pub(crate) refresh_expires_at: Option<DateTime<Utc>>,
}

#[async_trait::async_trait]
pub(crate) trait GithubOAuthSource: Send + Sync {
    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError>;

    async fn refresh(
        &self,
        refresh_token: &str,
        now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError>;
}

pub(crate) struct GithubOAuthClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    callback_url: Url,
    origin: Url,
    timeout: Duration,
}

impl GithubOAuthClient {
    pub(crate) fn new(
        client_id: String,
        client_secret: String,
        callback_url: Url,
    ) -> Result<Self, OAuthError> {
        let origin = Url::parse(GITHUB_ORIGIN).map_err(|_| OAuthError::Unavailable)?;
        Self::build(
            client_id,
            client_secret,
            callback_url,
            origin,
            PRODUCTION_TIMEOUT,
            false,
        )
    }

    pub(crate) fn authorization_url(
        &self,
        state: &str,
        verifier: &str,
    ) -> Result<Url, OAuthError> {
        validate_browser_secret(state)?;
        let challenge =
            pkce_challenge(verifier).map_err(|_| OAuthError::ReauthenticationRequired)?;
        let mut url = endpoint_url(&self.origin, &["login", "oauth", "authorize"])?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", self.callback_url.as_str())
            .append_pair("state", state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url)
    }

    #[cfg(test)]
    pub(super) fn new_for_test(
        client_id: String,
        client_secret: String,
        callback_url: Url,
        origin: &str,
        timeout: Duration,
    ) -> Result<Self, OAuthError> {
        let origin = Url::parse(origin).map_err(|_| OAuthError::Unavailable)?;
        if !valid_test_origin(&origin) || timeout.is_zero() {
            return Err(OAuthError::Unavailable);
        }
        Self::build(
            client_id,
            client_secret,
            callback_url,
            origin,
            timeout,
            true,
        )
    }

    fn build(
        client_id: String,
        client_secret: String,
        callback_url: Url,
        origin: Url,
        timeout: Duration,
        no_proxy: bool,
    ) -> Result<Self, OAuthError> {
        if !valid_graphic_secret(&client_id)
            || !valid_graphic_secret(&client_secret)
            || !valid_callback_url(&callback_url)
        {
            return Err(OAuthError::Unavailable);
        }
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .user_agent("EazyWorkOS");
        if no_proxy {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(|_| OAuthError::Unavailable)?;
        Ok(Self {
            http,
            client_id,
            client_secret,
            callback_url,
            origin,
            timeout,
        })
    }
}

#[async_trait::async_trait]
impl GithubOAuthSource for GithubOAuthClient {
    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError> {
        validate_provider_secret(code)?;
        validate_browser_secret(verifier)?;
        tokio::time::timeout(
            self.timeout,
            oauth_http::exchange_code(self, code, verifier, now),
        )
        .await
        .map_err(|_| OAuthError::Unavailable)?
    }

    async fn refresh(
        &self,
        refresh_token: &str,
        now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError> {
        validate_provider_secret(refresh_token)?;
        tokio::time::timeout(
            self.timeout,
            oauth_http::refresh(self, refresh_token, now),
        )
        .await
        .map_err(|_| OAuthError::Unavailable)?
    }
}

fn endpoint_url(origin: &Url, segments: &[&str]) -> Result<Url, OAuthError> {
    let mut url = origin.clone();
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| OAuthError::Unavailable)?;
    path.clear();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

fn valid_graphic_secret(value: &str) -> bool {
    (1..=MAX_INPUT_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn validate_provider_secret(value: &str) -> Result<(), OAuthError> {
    if valid_graphic_secret(value) {
        Ok(())
    } else {
        Err(OAuthError::ReauthenticationRequired)
    }
}

fn validate_browser_secret(value: &str) -> Result<(), OAuthError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(OAuthError::ReauthenticationRequired)
    }
}

fn valid_callback_url(url: &Url) -> bool {
    let allowed_scheme =
        url.scheme() == "https" || (url.scheme() == "http" && explicit_loopback_host(url));
    allowed_scheme
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/auth/github/callback"
}

fn explicit_loopback_host(url: &Url) -> bool {
    match url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
    {
        Some(IpAddr::V4(address)) => address.octets() == [127, 0, 0, 1],
        Some(IpAddr::V6(address)) => address == std::net::Ipv6Addr::LOCALHOST,
        None => false,
    }
}

#[cfg(test)]
fn valid_test_origin(url: &Url) -> bool {
    url.scheme() == "http"
        && explicit_loopback_host(url)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/"
}
