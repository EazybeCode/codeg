//! Signing-key sources for the two token kinds, cached and refreshed.
//!
//! Microsoft rotates both key sets; a cached set is refreshed at least every
//! 24 hours and again when a token names a kid we do not hold (at most once per
//! five minutes, so a flood of forged kids cannot make us hammer the issuer).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::jwt::{parse_jwks, Jwk, JwtError};

pub(crate) const BOT_FRAMEWORK_METADATA: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";
pub(crate) const ENTRA_COMMON_KEYS: &str =
    "https://login.microsoftonline.com/common/discovery/v2.0/keys";

const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const UNKNOWN_KID_FLOOR: Duration = Duration::from_secs(5 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;

#[async_trait]
pub(crate) trait KeySource: Send + Sync {
    async fn key(&self, kid: &str) -> Result<Jwk, JwtError>;
}

/// Where a key set comes from.
#[derive(Clone, Debug)]
pub(crate) enum Origin {
    /// An OpenID metadata document whose `jwks_uri` must stay on `jwks_host`.
    Metadata { url: String, jwks_host: String },
    /// A JWKS URL used directly.
    Jwks(String),
}

#[derive(Default)]
struct Cached {
    keys: Vec<Jwk>,
    fetched_at: Option<Instant>,
    attempted_at: Option<Instant>,
}

pub(crate) struct JwksCache {
    client: reqwest::Client,
    origin: Origin,
    cached: Mutex<Cached>,
}

impl JwksCache {
    pub(crate) fn new(origin: Origin) -> Result<Self, JwtError> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| JwtError::KeysUnavailable)?;
        Ok(Self {
            client,
            origin,
            cached: Mutex::new(Cached::default()),
        })
    }

    pub(crate) fn bot_framework() -> Result<Self, JwtError> {
        Self::new(Origin::Metadata {
            url: BOT_FRAMEWORK_METADATA.to_owned(),
            jwks_host: "login.botframework.com".to_owned(),
        })
    }

    pub(crate) fn entra() -> Result<Self, JwtError> {
        Self::new(Origin::Jwks(ENTRA_COMMON_KEYS.to_owned()))
    }

    async fn get_json(&self, url: &str) -> Result<Vec<u8>, JwtError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| JwtError::KeysUnavailable)?;
        if !response.status().is_success() {
            return Err(JwtError::KeysUnavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| JwtError::KeysUnavailable)?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(JwtError::KeysUnavailable);
        }
        Ok(bytes.to_vec())
    }

    async fn fetch(&self) -> Result<Vec<Jwk>, JwtError> {
        let jwks_url = match &self.origin {
            Origin::Jwks(url) => url.clone(),
            Origin::Metadata { url, jwks_host } => {
                let metadata: Value = serde_json::from_slice(&self.get_json(url).await?)
                    .map_err(|_| JwtError::KeysUnavailable)?;
                let jwks = metadata
                    .get("jwks_uri")
                    .and_then(Value::as_str)
                    .ok_or(JwtError::KeysUnavailable)?;
                let parsed = url::Url::parse(jwks).map_err(|_| JwtError::KeysUnavailable)?;
                // A metadata document may move the key set, but not off the
                // issuer's own host or onto plain HTTP.
                if parsed.scheme() != "https" || parsed.host_str() != Some(jwks_host.as_str()) {
                    return Err(JwtError::KeysUnavailable);
                }
                jwks.to_owned()
            }
        };
        parse_jwks(&self.get_json(&jwks_url).await?)
    }
}

/// Whether a lookup refreshes the set. The floor bounds EVERY refresh, a stale
/// set's included: while the issuer is down a held set keeps verifying, and
/// only one request per floor waits on a fetch (under the cache lock) instead
/// of every request queueing behind a timeout.
pub(crate) fn should_fetch(stale: bool, known: bool, may_retry: bool) -> bool {
    (stale || !known) && may_retry
}

#[async_trait]
impl KeySource for JwksCache {
    async fn key(&self, kid: &str) -> Result<Jwk, JwtError> {
        let mut cached = self.cached.lock().await;
        let now = Instant::now();
        let stale = cached
            .fetched_at
            .is_none_or(|at| now.duration_since(at) >= MAX_AGE);
        let known = cached.keys.iter().any(|key| key.kid == kid);
        let may_retry = cached
            .attempted_at
            .is_none_or(|at| now.duration_since(at) >= UNKNOWN_KID_FLOOR);
        if should_fetch(stale, known, may_retry) {
            cached.attempted_at = Some(now);
            // A failed refresh keeps the set already held (and retries no
            // sooner than the floor).
            if let Ok(keys) = self.fetch().await {
                cached.keys = keys;
                cached.fetched_at = Some(now);
            }
        }
        // With no set ever fetched there is nothing to verify against: that is
        // an outage (503, the connector retries), not a forged key (401).
        if cached.fetched_at.is_none() {
            return Err(JwtError::KeysUnavailable);
        }
        cached
            .keys
            .iter()
            .find(|key| key.kid == kid)
            .cloned()
            .ok_or(JwtError::UnknownKey)
    }
}

/// A fixed key set, for tests and for a harness that injects its own issuer.
#[cfg(test)]
pub(crate) struct StaticKeys(pub(crate) Vec<Jwk>);

#[cfg(test)]
#[async_trait]
impl KeySource for StaticKeys {
    async fn key(&self, kid: &str) -> Result<Jwk, JwtError> {
        self.0
            .iter()
            .find(|key| key.kid == kid)
            .cloned()
            .ok_or(JwtError::UnknownKey)
    }
}
