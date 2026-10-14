//! RS256 compact-JWS verification for the two token kinds Teams sends us.
//!
//! - **Bot Connector → bot** (`Authorization: Bearer` on every activity):
//!   issuer `https://api.botframework.com`, audience = our bot app id, the
//!   `serviceurl` claim equal to the activity's `serviceUrl`, and the signing
//!   key endorsed for the activity's channel (`msteams`).
//! - **Teams tab SSO** (Entra ID access token for our `access_as_user` scope):
//!   issuer `https://login.microsoftonline.com/{tid}/v2.0` or
//!   `https://sts.windows.net/{tid}/`, audience = our app id or Application ID
//!   URI, the delegated scope present, and `tid`/`oid` well-formed.
//!
//! Only RS256 is accepted; `alg` is read from the header solely to refuse
//! anything else (no `none`, no HMAC confusion). Keys under 2048 bits are
//! refused by `ring` itself. There is deliberately no switch to skip any of it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

use super::config::is_guid;

/// Clock skew Microsoft documents for Bot Framework tokens.
pub(crate) const SKEW_SECS: i64 = 300;
pub(crate) const CONNECTOR_ISSUER: &str = "https://api.botframework.com";
const MAX_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum JwtError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported algorithm")]
    Algorithm,
    #[error("unknown signing key")]
    UnknownKey,
    #[error("signing keys unavailable")]
    KeysUnavailable,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer")]
    Issuer,
    #[error("wrong audience")]
    Audience,
    #[error("expired or not yet valid")]
    Lifetime,
    #[error("serviceUrl mismatch")]
    ServiceUrl,
    #[error("key not endorsed for channel")]
    Endorsement,
    #[error("missing scope")]
    Scope,
    #[error("bad tenant or user claim")]
    Identity,
}

/// One RSA verification key from a JWKS document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Jwk {
    pub(crate) kid: String,
    /// Big-endian modulus, leading zeros stripped.
    pub(crate) n: Vec<u8>,
    pub(crate) e: Vec<u8>,
    /// Bot Framework only: the channel ids this key may sign for.
    pub(crate) endorsements: Vec<String>,
}

#[derive(Deserialize)]
struct RawJwk {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    kid: String,
    #[serde(default)]
    n: String,
    #[serde(default)]
    e: String,
    #[serde(default, rename = "use")]
    usage: Option<String>,
    #[serde(default)]
    endorsements: Vec<String>,
}

#[derive(Deserialize)]
struct RawJwks {
    keys: Vec<RawJwk>,
}

/// Parse a JWKS document, keeping only RSA signature keys with a usable kid.
pub(crate) fn parse_jwks(bytes: &[u8]) -> Result<Vec<Jwk>, JwtError> {
    let raw: RawJwks = serde_json::from_slice(bytes).map_err(|_| JwtError::KeysUnavailable)?;
    let mut keys = Vec::new();
    for key in raw.keys.into_iter().take(64) {
        if key.kty != "RSA"
            || key.kid.is_empty()
            || key.kid.len() > 256
            || key.usage.as_deref().is_some_and(|usage| usage != "sig")
        {
            continue;
        }
        let (Ok(n), Ok(e)) = (
            URL_SAFE_NO_PAD.decode(&key.n),
            URL_SAFE_NO_PAD.decode(&key.e),
        ) else {
            continue;
        };
        keys.push(Jwk {
            kid: key.kid,
            n: strip_zeros(n),
            e: strip_zeros(e),
            endorsements: key.endorsements,
        });
    }
    Ok(keys)
}

fn strip_zeros(mut bytes: Vec<u8>) -> Vec<u8> {
    let leading = bytes.iter().take_while(|byte| **byte == 0).count();
    bytes.drain(..leading);
    bytes
}

/// The parts of a compact JWS, decoded but NOT yet trusted.
pub(crate) struct Unverified {
    pub(crate) kid: String,
    signing_input: String,
    signature: Vec<u8>,
    claims: Value,
}

pub(crate) fn decode(token: &str) -> Result<Unverified, JwtError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(JwtError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(header_part), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(header_part)
            .map_err(|_| JwtError::Malformed)?,
    )
    .map_err(|_| JwtError::Malformed)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(JwtError::Algorithm);
    }
    // `crit` names extensions we would have to understand; we understand none.
    if header.get("crit").is_some() {
        return Err(JwtError::Malformed);
    }
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .filter(|kid| !kid.is_empty() && kid.len() <= 256)
        .ok_or(JwtError::Malformed)?
        .to_owned();
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| JwtError::Malformed)?,
    )
    .map_err(|_| JwtError::Malformed)?;
    if !claims.is_object() {
        return Err(JwtError::Malformed);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| JwtError::Malformed)?;
    Ok(Unverified {
        kid,
        signing_input: format!("{header_part}.{payload}"),
        signature,
        claims,
    })
}

impl Unverified {
    /// Verify the signature; only then are the claims returned.
    fn verified(self, key: &Jwk) -> Result<Value, JwtError> {
        if key.kid != self.kid {
            return Err(JwtError::UnknownKey);
        }
        ring::signature::RsaPublicKeyComponents {
            n: key.n.as_slice(),
            e: key.e.as_slice(),
        }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            self.signing_input.as_bytes(),
            &self.signature,
        )
        .map_err(|_| JwtError::Signature)?;
        Ok(self.claims)
    }
}

fn lifetime(claims: &Value, now: i64) -> Result<(), JwtError> {
    let exp = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(JwtError::Lifetime)?;
    if now > exp.saturating_add(SKEW_SECS) {
        return Err(JwtError::Lifetime);
    }
    if let Some(nbf) = claims.get("nbf") {
        let nbf = nbf.as_i64().ok_or(JwtError::Lifetime)?;
        if now < nbf.saturating_sub(SKEW_SECS) {
            return Err(JwtError::Lifetime);
        }
    }
    Ok(())
}

fn audience_matches(claims: &Value, accepted: &[&str]) -> bool {
    match claims.get("aud") {
        Some(Value::String(aud)) => accepted.contains(&aud.as_str()),
        // RFC 7519 allows an array; every listed audience must be ours.
        Some(Value::Array(list)) => {
            !list.is_empty()
                && list
                    .iter()
                    .all(|aud| aud.as_str().is_some_and(|aud| accepted.contains(&aud)))
        }
        _ => false,
    }
}

fn same_service_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim_end_matches('/'))
}

/// Verify a Bot Connector token for one inbound activity.
pub(crate) fn verify_connector(
    token: Unverified,
    key: &Jwk,
    app_id: &str,
    activity_service_url: &str,
    channel_id: &str,
    now: i64,
) -> Result<(), JwtError> {
    if !key
        .endorsements
        .iter()
        .any(|endorsed| endorsed == channel_id)
    {
        return Err(JwtError::Endorsement);
    }
    let claims = token.verified(key)?;
    if claims.get("iss").and_then(Value::as_str) != Some(CONNECTOR_ISSUER) {
        return Err(JwtError::Issuer);
    }
    if !audience_matches(&claims, &[app_id]) {
        return Err(JwtError::Audience);
    }
    lifetime(&claims, now)?;
    // Bot Framework spells it `serviceurl`; accept the camel-case spelling too.
    let claimed = claims
        .get("serviceurl")
        .or_else(|| claims.get("serviceUrl"))
        .and_then(Value::as_str)
        .ok_or(JwtError::ServiceUrl)?;
    if !same_service_url(claimed, activity_service_url) {
        return Err(JwtError::ServiceUrl);
    }
    Ok(())
}

/// The Entra user a verified tab SSO token names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntraUser {
    pub(crate) tenant_id: String,
    pub(crate) object_id: String,
    pub(crate) name: String,
    /// The account was authenticated by ANOTHER identity provider than the
    /// tenant (a B2B guest or external account): Entra sets `idp` to the home
    /// provider when it differs from the issuer. Such an account is never
    /// linked to a Neev member, whatever its object id.
    pub(crate) external: bool,
}

/// Verify a Teams tab SSO (Entra ID) access token for our delegated scope.
pub(crate) fn verify_entra(
    token: Unverified,
    key: &Jwk,
    audiences: &[&str],
    scope: &str,
    now: i64,
) -> Result<EntraUser, JwtError> {
    let claims = token.verified(key)?;
    let tenant_id = claims
        .get("tid")
        .and_then(Value::as_str)
        .filter(|tid| is_guid(tid))
        .ok_or(JwtError::Identity)?
        .to_ascii_lowercase();
    let issuer = claims
        .get("iss")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let v2 = format!("https://login.microsoftonline.com/{tenant_id}/v2.0");
    let v1 = format!("https://sts.windows.net/{tenant_id}/");
    if !issuer.eq_ignore_ascii_case(&v2) && !issuer.eq_ignore_ascii_case(&v1) {
        return Err(JwtError::Issuer);
    }
    if !audience_matches(&claims, audiences) {
        return Err(JwtError::Audience);
    }
    lifetime(&claims, now)?;
    let scopes = claims
        .get("scp")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !scopes.split(' ').any(|granted| granted == scope) {
        return Err(JwtError::Scope);
    }
    let object_id = claims
        .get("oid")
        .and_then(Value::as_str)
        .filter(|oid| is_guid(oid))
        .ok_or(JwtError::Identity)?
        .to_ascii_lowercase();
    let name = claims
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(128)
        .collect();
    // `idp` is absent, or equal to the issuer, for the tenant's own accounts.
    let external = claims
        .get("idp")
        .map(|idp| {
            let idp = idp.as_str().unwrap_or_default();
            !(idp.eq_ignore_ascii_case(&v1) || idp.eq_ignore_ascii_case(&v2) || idp.eq_ignore_ascii_case(issuer))
        })
        .unwrap_or(false);
    Ok(EntraUser {
        tenant_id,
        object_id,
        name,
        external,
    })
}
