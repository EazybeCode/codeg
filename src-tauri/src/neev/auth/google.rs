use std::{str::FromStr, sync::Arc, time::Duration};

use chrono::{TimeZone, Utc};
use futures::StreamExt;
use openidconnect::{
    core::{
        CoreIdToken, CoreIdTokenVerifier, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreTokenResponse,
    },
    ClientId, IssuerUrl, Nonce, PkceCodeChallenge, TokenResponse,
};
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;

use crate::neev::{
    error::AuthError,
    types::{Provider, VerifiedIdentity},
};

const ISSUER: &str = "https://accounts.google.com";
const DISCOVERY: &str = "https://accounts.google.com/.well-known/openid-configuration";
const AUTHORIZATION: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const JWKS: &str = "https://www.googleapis.com/oauth2/v3/certs";
const DOCUMENT_LIMIT: usize = 256 * 1024;
const REFRESH_COOLDOWN: i64 = 30;

pub(crate) struct PreparedAuthorization {
    pub(crate) url: String,
    pub(crate) pkce_verifier: String,
    pub(crate) nonce: String,
}

#[derive(Clone)]
struct Endpoints {
    discovery: String,
    authorization: String,
    token: String,
    jwks: String,
}

#[derive(Clone)]
struct CachedProvider {
    jwks: CoreJsonWebKeySet,
    fetched_at: i64,
}

pub(crate) struct Google {
    client_id: String,
    client_secret: String,
    callback: Url,
    client: reqwest::Client,
    endpoints: Endpoints,
    cache: Arc<Mutex<Option<CachedProvider>>>,
}

#[derive(Deserialize)]
struct MetadataDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    response_types_supported: Vec<String>,
    id_token_signing_alg_values_supported: Vec<String>,
    code_challenge_methods_supported: Option<Vec<String>>,
}

impl Google {
    pub(crate) fn new(
        client_id: String,
        client_secret: String,
        callback: Url,
    ) -> Result<Self, AuthError> {
        Self::with_endpoints(
            client_id,
            client_secret,
            callback,
            Endpoints {
                discovery: DISCOVERY.to_owned(),
                authorization: AUTHORIZATION.to_owned(),
                token: TOKEN.to_owned(),
                jwks: JWKS.to_owned(),
            },
        )
    }

    fn with_endpoints(
        client_id: String,
        client_secret: String,
        callback: Url,
        endpoints: Endpoints,
    ) -> Result<Self, AuthError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| AuthError::Unavailable)?;
        Ok(Self {
            client_id,
            client_secret,
            callback,
            client,
            endpoints,
            cache: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        client_id: String,
        client_secret: String,
        callback: Url,
        base_url: &str,
    ) -> Result<Self, AuthError> {
        let base = base_url.trim_end_matches('/');
        Self::with_endpoints(
            client_id,
            client_secret,
            callback,
            Endpoints {
                discovery: format!("{base}/.well-known/openid-configuration"),
                authorization: format!("{base}/authorize"),
                token: format!("{base}/token"),
                jwks: format!("{base}/jwks"),
            },
        )
    }

    pub(crate) fn authorization(&self, state: &str) -> Result<PreparedAuthorization, AuthError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let nonce = Nonce::new_random();
        let mut url =
            Url::parse(&self.endpoints.authorization).map_err(|_| AuthError::Unavailable)?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", self.callback.as_str())
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email profile")
            .append_pair("state", state)
            .append_pair("nonce", nonce.secret())
            .append_pair("code_challenge", challenge.as_str())
            .append_pair("code_challenge_method", "S256");
        Ok(PreparedAuthorization {
            url: url.into(),
            pkce_verifier: verifier.secret().to_owned(),
            nonce: nonce.secret().to_owned(),
        })
    }

    pub(crate) async fn exchange(
        &self,
        code: &str,
        pkce_verifier: &str,
        nonce: &str,
        now: i64,
    ) -> Result<VerifiedIdentity, AuthError> {
        let cached = self.provider(false, now).await?;
        let response = self
            .client
            .post(&self.endpoints.token)
            .form(&[
                ("code", code),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("redirect_uri", self.callback.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", pkce_verifier),
            ])
            .send()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let body = bounded_json(response).await?;
        let token_response: CoreTokenResponse =
            serde_json::from_slice(&body).map_err(|_| AuthError::Unauthorized)?;
        let id_token = token_response
            .id_token()
            .ok_or(AuthError::Unauthorized)?
            .to_string();
        let jwks = if verification_key_known(&id_token, &cached.jwks, &self.client_id) {
            cached.jwks
        } else if now.saturating_sub(cached.fetched_at) >= REFRESH_COOLDOWN {
            self.provider(true, now).await?.jwks
        } else {
            return Err(AuthError::Unauthorized);
        };
        verify_id_token(&id_token, jwks, &self.client_id, nonce, now)
    }

    async fn provider(&self, force: bool, now: i64) -> Result<CachedProvider, AuthError> {
        let mut cache = self.cache.lock().await;
        if !force {
            if let Some(cached) = cache.as_ref() {
                return Ok(cached.clone());
            }
        }
        if force
            && cache
                .as_ref()
                .is_some_and(|cached| now.saturating_sub(cached.fetched_at) < REFRESH_COOLDOWN)
        {
            return cache.clone().ok_or(AuthError::Unavailable);
        }
        let metadata: MetadataDocument = serde_json::from_slice(
            &bounded_json(
                self.client
                    .get(&self.endpoints.discovery)
                    .send()
                    .await
                    .map_err(|_| AuthError::Unavailable)?,
            )
            .await?,
        )
        .map_err(|_| AuthError::Unavailable)?;
        if metadata.issuer != ISSUER
            || metadata.authorization_endpoint != self.endpoints.authorization
            || metadata.token_endpoint != self.endpoints.token
            || metadata.jwks_uri != self.endpoints.jwks
            || !metadata
                .response_types_supported
                .iter()
                .any(|value| value == "code")
            || !metadata
                .id_token_signing_alg_values_supported
                .iter()
                .any(|value| value == "RS256")
            || !metadata
                .code_challenge_methods_supported
                .as_ref()
                .is_some_and(|values| values.iter().any(|value| value == "S256"))
        {
            return Err(AuthError::Unavailable);
        }
        let jwks: CoreJsonWebKeySet = serde_json::from_slice(
            &bounded_json(
                self.client
                    .get(&self.endpoints.jwks)
                    .send()
                    .await
                    .map_err(|_| AuthError::Unavailable)?,
            )
            .await?,
        )
        .map_err(|_| AuthError::Unavailable)?;
        let fresh = CachedProvider {
            jwks,
            fetched_at: now,
        };
        *cache = Some(fresh.clone());
        Ok(fresh)
    }
}

async fn bounded_json(response: reqwest::Response) -> Result<Vec<u8>, AuthError> {
    if !response.status().is_success()
        || !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        || response
            .content_length()
            .is_some_and(|length| length > DOCUMENT_LIMIT as u64)
    {
        return Err(AuthError::Unavailable);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AuthError::Unavailable)?;
        if body.len().saturating_add(chunk.len()) > DOCUMENT_LIMIT {
            return Err(AuthError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn verification_key_known(token: &str, jwks: &CoreJsonWebKeySet, client_id: &str) -> bool {
    let Ok(id_token) = CoreIdToken::from_str(token) else {
        return false;
    };
    let Ok(issuer) = IssuerUrl::new(ISSUER.to_owned()) else {
        return false;
    };
    let verifier = CoreIdTokenVerifier::new_public_client(
        ClientId::new(client_id.to_owned()),
        issuer,
        jwks.clone(),
    )
    .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256]);
    id_token.signing_key(&verifier).is_ok()
}

pub(crate) fn verify_id_token(
    token: &str,
    jwks: CoreJsonWebKeySet,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<VerifiedIdentity, AuthError> {
    let now = Utc
        .timestamp_opt(now, 0)
        .single()
        .ok_or(AuthError::Unauthorized)?;
    let verifier = CoreIdTokenVerifier::new_public_client(
        ClientId::new(client_id.to_owned()),
        IssuerUrl::new(ISSUER.to_owned()).map_err(|_| AuthError::Unavailable)?,
        jwks,
    )
    .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256])
    .set_time_fn(move || now);
    let id_token = CoreIdToken::from_str(token).map_err(|_| AuthError::Unauthorized)?;
    let claims = id_token
        .claims(&verifier, &Nonce::new(nonce.to_owned()))
        .map_err(|_| AuthError::Unauthorized)?;
    let subject = claims.subject().as_str().to_owned();
    if subject.is_empty() || subject.len() > 255 || subject.chars().any(char::is_control) {
        return Err(AuthError::Unauthorized);
    }
    let verified_email = if claims.email_verified() == Some(true) {
        claims.email().map(|value| value.as_str().to_owned())
    } else {
        None
    };
    let display_name = verified_email.clone().unwrap_or_else(|| subject.clone());
    Ok(VerifiedIdentity {
        provider: Provider::Google,
        issuer: ISSUER.to_owned(),
        subject,
        verified_email,
        display_name,
        avatar_url: None,
    })
}

#[cfg(test)]
pub(crate) struct SignedGoogleFixture {
    pub(crate) jwks: CoreJsonWebKeySet,
    pub(crate) id_token: String,
    pub(crate) client_id: String,
    pub(crate) nonce: String,
    pub(crate) now: i64,
}

#[cfg(test)]
pub(crate) fn verify_fixture(fixture: &SignedGoogleFixture) -> Result<VerifiedIdentity, AuthError> {
    verify_id_token(
        &fixture.id_token,
        fixture.jwks.clone(),
        &fixture.client_id,
        &fixture.nonce,
        fixture.now,
    )
}
