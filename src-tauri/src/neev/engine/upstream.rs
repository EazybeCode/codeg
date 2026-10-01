//! The Neev-side client for the Docs bridge.
//!
//! Two jobs: provision a document, and project a Neev role onto the Docs access
//! row that upstream reads when it decides whether a socket may write. Both go
//! through the bridge's **private, credentialled** API (`/_neev/v1/…`,
//! authenticated by a shared key), not through the browser-facing REST API.
//!
//! That is a deliberate change from this file's first version, which drove
//! upstream's public `documents/` and `accesses/` endpoints as a service user
//! holding a Django session. The bridge refuses the `neev:service:*` namespace
//! on `RemoteUser` headers on purpose, so that flow would simply have been
//! denied — and duplicating an owner-API flow beside a private API built for
//! exactly this would have been two ways to do one thing. The browser's CSRF
//! path is untouched by any of it: these calls are server-to-server on a private
//! network and never traverse the browser session.
//!
//! **Nothing here is ever called while a native database transaction is open.**
//! `db/mod.rs` states the rule — no network calls inside transactions — and it
//! is load-bearing rather than stylistic: the native path holds an admission
//! guard and an ordered lock set, and holding those across an upstream round
//! trip would let a slow or unreachable Docs stall unrelated native work.
//!
//! The consequence is stated rather than papered over: **there is no atomicity
//! across the two stores, and none is claimed.** The resource and its binding
//! commit together in Neev; the Docs document is provisioned afterwards. If that
//! fails, the outcome is a bound resource whose Docs document does not exist
//! yet — repaired by retrying with the *same* id, because the Docs document id
//! *is* the Neev resource id. An unreachable upstream document, retried
//! idempotently, is a better failure than held locks or a half-visible blank
//! native document.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::{redirect::Policy, StatusCode};
use tokio::sync::Mutex;
use url::Url;

use super::identity::Assertion;
use crate::neev::{clock::Clock, error::AuthError};

/// Header carrying the bridge's shared credential. Never sent to a browser and
/// never accepted from one — it is in the stripped set precisely so a client
/// cannot present it.
pub(crate) const BRIDGE_KEY: &str = "x-neev-bridge-key";

/// Requested lifetime for a minted Docs session. The bridge clamps to
/// 60..43200; this asks for an hour.
const SESSION_TTL: i64 = 3_600;

/// Hard cap on remembered sessions, so a burst of distinct subjects cannot grow
/// this map without bound.
const SESSION_CAPACITY: usize = 512;

/// What a caller may do with a document, as Docs models it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Projected {
    Reader,
    Editor,
}

impl Projected {
    pub(crate) fn role(self) -> &'static str {
        match self {
            Projected::Reader => "reader",
            Projected::Editor => "editor",
        }
    }

    /// Neev's decision, translated. Neev's native model has no per-resource
    /// viewer role: access is private/organization and archived is read-only.
    /// So a writable resource projects as editor and anything a caller can
    /// still see but not write projects as reader.
    pub(crate) fn from_neev(writable: bool) -> Self {
        if writable {
            Projected::Editor
        } else {
            Projected::Reader
        }
    }
}

/// Error codes the bridge returns for genuinely invalid *input*. Anything else
/// it answers — a Django `DisallowedHost` page, a proxy error, an unexpected
/// status with no typed code — is infrastructure, and reporting infrastructure
/// as invalid input is how a misconfiguration ends up blaming the user. That is
/// not hypothetical: a missing Host header produced a bare Django 400, which
/// this layer called `Invalid`, which the UI rendered as "names must be 1-100
/// characters" for a name that was fine.
const INPUT_ERRORS: &[&str] = &[
    "invalid_json",
    "invalid_subject",
    "invalid_document_id",
    "invalid_title",
    "invalid_role",
    "invalid_key",
    "body_too_large",
];

/// Classify a failed bridge response: typed input error, or infrastructure.
fn failure(status: StatusCode, body: &serde_json::Value) -> EngineError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => EngineError::Refused,
        _ => {
            let code = body.get("error").and_then(serde_json::Value::as_str);
            match code {
                Some(code) if INPUT_ERRORS.contains(&code) => EngineError::Invalid,
                // Includes every response with no typed code at all, which is
                // exactly the infrastructure case.
                _ => EngineError::Unavailable,
            }
        }
    }
}

/// A bridge markdown answer (plan Task 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EngineMarkdown {
    Unchanged { content_sha256: String },
    Content { content_sha256: String, markdown: String },
}

/// A typed bridge refusal for markdown extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkdownRefusal {
    TooLarge,
    /// 409 `not_an_engine_document`.
    NotEngine,
    /// 409 `organization_mismatch`: the document's creator org differs from
    /// the org asking. An integrity fault, never "engine unavailable".
    OrgMismatch,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum EngineError {
    /// Docs could not be reached or answered with a server error. Always
    /// retryable, and never reported to a caller as a permission decision.
    #[error("document engine unavailable")]
    Unavailable,
    /// Docs refused. Surfaced as-is rather than retried, because retrying a
    /// refusal is how a bug becomes a loop.
    #[error("document engine refused the request")]
    Refused,
    /// This side built something the bridge would not accept.
    #[error("invalid document engine request")]
    Invalid,
}

impl From<EngineError> for AuthError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Unavailable => AuthError::Unavailable,
            EngineError::Refused | EngineError::Invalid => AuthError::Invalid,
        }
    }
}

#[derive(Clone, Debug)]
struct Minted {
    session: String,
    /// When this key stops being reused. Set to **half** the lifetime the bridge
    /// reported, so a session is replaced well before Django would expire it —
    /// a key that expires mid-connection would look like a permission failure.
    reuse_until: i64,
}

pub(crate) struct Upstream {
    client: reqwest::Client,
    backend: Url,
    /// The browser-facing origin, sent as `Origin` so the bridge sees the same
    /// value a browser would have sent.
    origin: String,
    /// `host[:port]` of that origin, sent as `Host` on **every** call.
    ///
    /// Without it the request carries the container address the socket was
    /// opened to, Django matches that against `ALLOWED_HOSTS`, and every bridge
    /// call is refused with `DisallowedHost` — which surfaces as a plain 400
    /// HTML page, not a bridge error, so it reads like a malformed body rather
    /// than a rejected host.
    host: String,
    key: String,
    sessions: Mutex<HashMap<String, Minted>>,
}

impl Upstream {
    pub(crate) fn new(backend: Url, origin: String, key: String) -> Result<Self, EngineError> {
        // The public origin decides the Host header; the backend URL only
        // decides where the socket goes.
        let backend_host_source = Url::parse(&origin).map_err(|_| EngineError::Invalid)?;
        if !matches!(backend.scheme(), "http" | "https") || backend.cannot_be_a_base() {
            return Err(EngineError::Invalid);
        }
        if key.trim().is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
            return Err(EngineError::Invalid);
        }
        let client = reqwest::Client::builder()
            // A redirect would carry the bridge credential somewhere else.
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| EngineError::Unavailable)?;
        let host = match (backend_host_source.host_str(), backend_host_source.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            _ => return Err(EngineError::Invalid),
        };
        Ok(Self {
            client,
            backend,
            origin,
            host,
            key,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, EngineError> {
        self.backend.join(path).map_err(|_| EngineError::Invalid)
    }

    /// One call to the private bridge API. These paths take no session and set
    /// no cookie; the credential is the whole authentication.
    async fn private(
        &self,
        method: reqwest::Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<(StatusCode, serde_json::Value), EngineError> {
        let response = self
            .client
            .request(method, self.endpoint(path)?)
            .header(BRIDGE_KEY, &self.key)
            .header(reqwest::header::HOST, &self.host)
            .header("X-Forwarded-Proto", "https")
            .header("Origin", &self.origin)
            .json(&body)
            .send()
            .await
            .map_err(|_| EngineError::Unavailable)?;
        let status = response.status();
        // 503 is the bridge reporting itself disabled (absent or too-short
        // credential). That is unavailability, never a permission answer.
        if status.is_server_error() {
            return Err(EngineError::Unavailable);
        }
        let parsed = response
            .json::<serde_json::Value>()
            .await
            .unwrap_or(serde_json::Value::Null);
        Ok((status, parsed))
    }

    /// Create the Docs document for a Neev resource, using the resource's own
    /// id, through the bridge's private provisioning endpoint.
    ///
    /// Idempotent by contract: provisioning an id that already exists is
    /// success, because the desired state — that document, with that id —
    /// holds. A retry after a partial failure therefore converges rather than
    /// producing a second document.
    pub(crate) async fn provision(
        &self,
        resource_id: &str,
        title: &str,
        creator: &Assertion,
    ) -> Result<(), EngineError> {
        let (status, body) = self
            .private(
                reqwest::Method::POST,
                "_neev/v1/documents/provision",
                serde_json::json!({
                    "id": resource_id,
                    "title": title,
                    "sub": creator.subject,
                    "name": creator.name,
                }),
            )
            .await?;
        match status {
            // 201 created, 200 already present. Both mean the document exists
            // with the id we asked for, which is the whole requirement — so a
            // retry after a partial failure converges instead of producing a
            // second document.
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            _ => Err(failure(status, &body)),
        }
    }

    /// Project a caller's Neev-derived role onto their Docs access row.
    ///
    /// Re-derived from `access::authorized` on every connect and never cached,
    /// so this is a projection of Neev's answer rather than a second source of
    /// truth. The bridge owns the document and performs the change; Neev never
    /// drives upstream's public sharing API, which stays outside the allowlist.
    pub(crate) async fn project(
        &self,
        resource_id: &str,
        subject: &Assertion,
        role: Projected,
    ) -> Result<(), EngineError> {
        let (status, body) = self
            .private(
                reqwest::Method::POST,
                &format!("_neev/v1/documents/{resource_id}/access"),
                serde_json::json!({
                    "sub": subject.subject,
                    "role": role.role(),
                    "name": subject.name,
                }),
            )
            .await?;
        match status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            _ => Err(failure(status, &body)),
        }
    }

    /// Withdraw a caller's Docs access.
    ///
    /// Used when Neev's answer becomes "no access" — closing the socket ends the
    /// live session, and this removes the projected row so a fresh connection
    /// does not find stale access waiting for it.
    pub(crate) async fn revoke(
        &self,
        resource_id: &str,
        subject: &Assertion,
    ) -> Result<(), EngineError> {
        let (status, body) = self
            .private(
                reqwest::Method::DELETE,
                &format!("_neev/v1/documents/{resource_id}/access"),
                serde_json::json!({ "sub": subject.subject }),
            )
            .await?;
        match status {
            // Already absent is the desired state, not a failure.
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
            _ => Err(failure(status, &body)),
        }
    }

    /// Push Neev's title onto the Docs copy.
    ///
    /// Reconciliation, not a user action: Neev's row is what Projects lists and
    /// what the ACL is evaluated against, so Neev's value wins. Any actor with
    /// access may rename — that is the native policy, not a relaxation of one. Used when a
    /// rename was accepted by Docs but then lost a native revision race — Docs
    /// is ahead at that point, and leaving it ahead is the divergence this
    /// pulls back.
    pub(crate) async fn push_title(
        &self,
        resource_id: &str,
        subject: &Assertion,
        title: &str,
    ) -> Result<(), EngineError> {
        let (status, body) = self
            .private(
                reqwest::Method::POST,
                &format!("_neev/v1/documents/{resource_id}/title"),
                serde_json::json!({ "sub": subject.subject, "title": title }),
            )
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(failure(status, &body))
        }
    }

    /// Markdown of an engine document through the bridge's private
    /// `POST /_neev/v1/documents/<id>/markdown` (plan Task 4/5 contract).
    ///
    /// Outer `Err` is infrastructure (retry later); inner `Err` is a typed
    /// refusal the caller records as the source's capture state. A converter
    /// failure is a 503, never an empty success.
    pub(crate) async fn markdown(
        &self,
        resource_id: &str,
        org_id: i32,
        if_none_match: Option<&str>,
    ) -> Result<Result<EngineMarkdown, MarkdownRefusal>, EngineError> {
        let (status, body) = self
            .private(
                reqwest::Method::POST,
                &format!("_neev/v1/documents/{resource_id}/markdown"),
                serde_json::json!({ "org_id": org_id, "if_none_match": if_none_match }),
            )
            .await?;
        match status {
            StatusCode::OK => {
                let sha = body
                    .get("content_sha256")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty() && value.len() <= 128)
                    .ok_or(EngineError::Invalid)?
                    .to_owned();
                if body.get("unchanged").and_then(serde_json::Value::as_bool) == Some(true) {
                    return Ok(Ok(EngineMarkdown::Unchanged {
                        content_sha256: sha,
                    }));
                }
                let markdown = body
                    .get("markdown")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(EngineError::Invalid)?
                    .to_owned();
                if markdown.len() > crate::neev::knowledge::types::MAX_ENGINE_MARKDOWN_BYTES {
                    return Ok(Err(MarkdownRefusal::TooLarge));
                }
                Ok(Ok(EngineMarkdown::Content {
                    content_sha256: sha,
                    markdown,
                }))
            }
            StatusCode::PAYLOAD_TOO_LARGE => Ok(Err(MarkdownRefusal::TooLarge)),
            StatusCode::CONFLICT => Ok(Err(
                if body.get("error").and_then(serde_json::Value::as_str) == Some("organization_mismatch") {
                    MarkdownRefusal::OrgMismatch
                } else {
                    MarkdownRefusal::NotEngine
                },
            )),
            StatusCode::NOT_FOUND => Ok(Err(MarkdownRefusal::Unknown)),
            status if status.is_server_error() => Err(EngineError::Unavailable),
            _ => Err(failure(status, &body)),
        }
    }

    /// Fetch an attachment through the bridge's private media relay.
    ///
    /// The object store is not reachable from here and deliberately so: signing,
    /// MIME policy and streaming live behind the bridge, which already holds
    /// upstream's permission logic. Duplicating nginx's `media-auth` → S3 dance
    /// in Rust would mean maintaining a second copy of upstream's rules.
    ///
    /// The browser is never redirected to the object store — that would turn a
    /// per-request check into a bearer URL that outlives the grant behind it.
    pub(crate) async fn media(
        &self,
        resource_id: &str,
        key: &str,
        subject: &Assertion,
    ) -> Result<reqwest::Response, EngineError> {
        let mut url = self
            .endpoint(&format!("_neev/v1/documents/{resource_id}/media"))?;
        url.query_pairs_mut().append_pair("key", key);
        let response = self
            .client
            .get(url)
            .header(BRIDGE_KEY, &self.key)
            .header(super::identity::USER, &subject.subject)
            .header(reqwest::header::HOST, &self.host)
            .header("X-Forwarded-Proto", "https")
            .send()
            .await
            .map_err(|_| EngineError::Unavailable)?;
        if response.status().is_server_error() {
            return Err(EngineError::Unavailable);
        }
        Ok(response)
    }

    /// Mint a Docs session **for a named native subject** and return its
    /// `docs_sessionid`.
    ///
    /// This exists because the collaboration socket is the one path where the
    /// identity header cannot reach Django: y-provider forwards only `cookie`,
    /// `origin` and its own API key, so whatever cookie arrives decides who the
    /// socket belongs to. Native actor A presenting a stale or borrowed
    /// `docs_sessionid` belonging to B would otherwise connect **as B** — with
    /// B's abilities, including write where A has only read. Projecting A's role
    /// does not help, because the backend never sees A.
    ///
    /// So the proxy does not validate the browser's cookie; it refuses to trust
    /// it. The session returned here is minted from the **native** subject the
    /// proxy just authorized, with the client's cookie withheld, and it is that
    /// cookie which is forwarded to y-provider. There is then no cookie for a
    /// caller to borrow, forge or leave stale.
    ///
    /// The subject is always a real user subject. The `neev:service:*` namespace
    /// is refused by the bridge on the header path by design — a browser must
    /// never be able to assert the principal that owns every document — and it
    /// is never passed here.
    pub(crate) async fn mint(
        &self,
        subject: &Assertion,
        clock: &dyn Clock,
    ) -> Result<String, EngineError> {
        let now = clock.now_unix();
        if let Some(existing) = self.reuse(&subject.subject, now).await {
            return Ok(existing);
        }
        let (status, body) = self
            .private(
                reqwest::Method::POST,
                "_neev/v1/sessions/mint",
                serde_json::json!({ "sub": subject.subject, "ttl_seconds": SESSION_TTL }),
            )
            .await?;
        if !status.is_success() {
            return Err(failure(status, &body));
        }
        let session = body
            .get("session_key")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.is_empty())
            .ok_or(EngineError::Unavailable)?
            .to_owned();
        // Replace the key at half its life, so it cannot expire mid-connection
        // and present as a permission failure.
        let lifetime = body
            .get("expires_in")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(SESSION_TTL)
            .clamp(60, 43_200);
        self.remember(&subject.subject, &session, now, now + lifetime / 2)
            .await;
        Ok(session)
    }

    async fn reuse(&self, subject: &str, now: i64) -> Option<String> {
        let held = self.sessions.lock().await;
        held.get(subject)
            .filter(|minted| now < minted.reuse_until)
            .map(|minted| minted.session.clone())
    }

    async fn remember(&self, subject: &str, session: &str, now: i64, reuse_until: i64) {
        let mut held = self.sessions.lock().await;
        held.retain(|_, minted| now < minted.reuse_until);
        if held.len() >= SESSION_CAPACITY {
            // Full of live entries: keep the map bounded rather than let it grow
            // with distinct subjects. Dropping one costs a mint on the next
            // connect and nothing else — it is a cache, not a grant.
            if let Some(soonest) = held
                .iter()
                .min_by_key(|(_, minted)| minted.reuse_until)
                .map(|(subject, _)| subject.clone())
            {
                held.remove(&soonest);
            }
        }
        held.insert(
            subject.to_owned(),
            Minted {
                session: session.to_owned(),
                reuse_until,
            },
        );
    }
}

/// The cookie name the minted session is sent under.
///
/// Upstream hardcodes `SESSION_COOKIE_NAME` and y-provider looks for exactly
/// this string when it extracts the session from the upgrade request, so it is
/// not configurable on either side.
pub(crate) const SESSION_COOKIE: &str = "docs_sessionid";

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream() -> Upstream {
        Upstream::new(
            Url::parse("http://backend:8000/").unwrap(),
            "https://app.neev.so".into(),
            "bridge-key".into(),
        )
        .unwrap()
    }

    #[test]
    fn a_role_projection_follows_neevs_own_read_write_decision() {
        assert_eq!(Projected::from_neev(true), Projected::Editor);
        assert_eq!(Projected::from_neev(false), Projected::Reader);
        assert_eq!(Projected::Editor.role(), "editor");
        assert_eq!(Projected::Reader.role(), "reader");
    }

    #[test]
    fn an_upstream_base_and_credential_must_both_be_usable() {
        assert!(upstream_with("http://backend:8000/", "key").is_ok());
        for bad in ["file:///etc/passwd", "mailto:someone@example.test"] {
            assert!(upstream_with(bad, "key").is_err(), "accepted base {bad}");
        }
        // An absent or malformed credential must fail at construction rather
        // than produce a client that sends an empty key and is refused later.
        for bad in ["", "   ", "with\nnewline"] {
            assert!(
                upstream_with("http://backend:8000/", bad).is_err(),
                "accepted key {bad:?}"
            );
        }
    }

    fn upstream_with(base: &str, key: &str) -> Result<Upstream, EngineError> {
        Upstream::new(
            Url::parse(base).unwrap(),
            "https://app.neev.so".into(),
            key.into(),
        )
    }

    #[test]
    fn unavailability_never_becomes_a_permission_answer() {
        // A caller must not be able to turn "Docs is down" into access. Only an
        // actual refusal maps to invalid; unavailability stays unavailable.
        assert_eq!(AuthError::from(EngineError::Unavailable), AuthError::Unavailable);
        assert_eq!(AuthError::from(EngineError::Refused), AuthError::Invalid);
        assert_eq!(AuthError::from(EngineError::Invalid), AuthError::Invalid);
    }

    #[tokio::test]
    async fn a_minted_session_is_reused_until_half_life_and_then_replaced() {
        // Reuse bounds session proliferation across reconnects. It is not an
        // authorization cache: the native decision is re-derived on every
        // connect whether or not the cookie is reused, and revocation closes
        // open sockets regardless of what is held here.
        let upstream = upstream();
        upstream.remember("neev:1:2", "session-a", 1_000, 1_500).await;

        assert_eq!(upstream.reuse("neev:1:2", 1_000).await.as_deref(), Some("session-a"));
        assert_eq!(upstream.reuse("neev:1:2", 1_499).await.as_deref(), Some("session-a"));
        assert_eq!(upstream.reuse("neev:1:2", 1_500).await, None);
        // One subject's session is never handed to another — this is the whole
        // point of keying by subject rather than trusting a presented cookie.
        assert_eq!(upstream.reuse("neev:1:3", 1_000).await, None);
        assert_eq!(upstream.reuse("neev:2:2", 1_000).await, None);
    }

    #[tokio::test]
    async fn remembered_sessions_stay_bounded() {
        let upstream = upstream();
        for index in 0..(SESSION_CAPACITY + 50) {
            upstream
                .remember(&format!("neev:1:{index}"), "session", 1_000, 1_500)
                .await;
        }
        assert!(upstream.sessions.lock().await.len() <= SESSION_CAPACITY);
    }

    #[test]
    fn the_session_cookie_name_matches_what_y_provider_looks_for() {
        assert_eq!(SESSION_COOKIE, "docs_sessionid");
    }
}
