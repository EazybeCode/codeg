//! The Neev-side client for the Docs backend.
//!
//! Two jobs: provision a document, and project a Neev role onto the Docs access
//! row that upstream reads when it decides whether a socket may write.
//!
//! **Nothing here is ever called while a native database transaction is open.**
//! `db/mod.rs` states the rule — no network calls inside transactions — and it
//! is load-bearing rather than stylistic: the native path takes an admission
//! guard and an ordered lock set, and holding those across an upstream round
//! trip would let a slow or unreachable Docs stall unrelated native work.
//!
//! The consequence is stated rather than papered over: **there is no atomicity
//! across the two stores, and none is claimed.** The resource and its binding
//! commit together in Neev; the Docs document is created afterwards. If that
//! second step fails, the outcome is a bound resource whose Docs document does
//! not exist yet — repaired by retrying with the *same* id, because the Docs
//! document id *is* the Neev resource id. An upstream document nobody can reach,
//! retried idempotently, is a better failure than held locks or a half-visible
//! blank native document.

use std::time::Duration;

use reqwest::{redirect::Policy, StatusCode};
use tokio::sync::Mutex;
use url::Url;

use super::identity::Assertion;
use crate::neev::error::AuthError;

/// The service principal that owns every engine document.
///
/// It is an ordinary Docs user — not staff, not a superuser, no admin access.
/// It exists because upstream requires `accesses_manage` (owner) to grant a
/// `DocumentAccess`, so a plain member asserting their own identity cannot
/// self-grant the editor role that makes `abilities.update` true. The
/// alternatives were asserting the *creator's* identity for an action they did
/// not take, which is impersonation, or a superuser, which is worse. This
/// principal is owner only of documents it created.
pub(crate) const SERVICE_SUBJECT: &str = "neev:service:engine";

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
    /// So a writable resource projects as editor and everything else a caller
    /// can still see projects as reader.
    pub(crate) fn from_neev(writable: bool) -> Self {
        if writable {
            Projected::Editor
        } else {
            Projected::Reader
        }
    }
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
    /// This side built something Docs would not accept.
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

/// The service identity's Django session and CSRF token.
///
/// Held because upstream's unsafe methods are session-authenticated and Django's
/// `CsrfViewMiddleware` applies. CSRF is **not** disabled, exempted or routed
/// around anywhere in this design — this struct exists precisely so that the
/// ordinary Django exchange can be performed properly.
#[derive(Clone, Debug)]
struct ServiceSession {
    session: String,
    csrf: String,
}

/// Read one `Set-Cookie` value by name. Only the `name=value` pair is taken;
/// the attributes belong to whoever sets the cookie on the browser, not to us.
fn set_cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
        let Ok(value) = value.to_str() else { continue };
        let pair = value.split(';').next().unwrap_or(value);
        if let Some((key, found)) = pair.split_once('=') {
            if key.trim() == name && !found.is_empty() {
                return Some(found.to_owned());
            }
        }
    }
    None
}

fn session_cookie(response: &reqwest::Response) -> Option<String> {
    // Upstream hardcodes this name and y-provider looks for exactly it, so it is
    // not configurable here either.
    set_cookie(response, "docs_sessionid")
}

pub(crate) struct Upstream {
    client: reqwest::Client,
    backend: Url,
    /// The browser-facing origin, sent as `Origin` so Django's CSRF check and
    /// `CSRF_TRUSTED_ORIGINS` agree with what the browser would have sent.
    origin: String,
    service: Mutex<Option<ServiceSession>>,
}

impl Upstream {
    pub(crate) fn new(backend: Url, origin: String) -> Result<Self, EngineError> {
        if !matches!(backend.scheme(), "http" | "https") || backend.cannot_be_a_base() {
            return Err(EngineError::Invalid);
        }
        let client = reqwest::Client::builder()
            // A redirect would drop the identity header onto somewhere else.
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| EngineError::Unavailable)?;
        Ok(Self {
            client,
            backend,
            origin,
            service: Mutex::new(None),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, EngineError> {
        self.backend.join(path).map_err(|_| EngineError::Invalid)
    }

    /// Establish (or re-establish) the service identity's session and CSRF
    /// token by the ordinary route: a safe request that sets both cookies.
    async fn bootstrap(&self) -> Result<ServiceSession, EngineError> {
        let response = self
            .client
            .get(self.endpoint("api/v1.0/users/me/")?)
            .header(super::identity::USER, SERVICE_SUBJECT)
            .header("X-Forwarded-Proto", "https")
            .header("Origin", &self.origin)
            .send()
            .await
            .map_err(|_| EngineError::Unavailable)?;
        if !response.status().is_success() {
            return Err(if response.status().is_server_error() {
                EngineError::Unavailable
            } else {
                EngineError::Refused
            });
        }
        let session = session_cookie(&response);
        let csrf = set_cookie(&response, "csrftoken");
        match (session, csrf) {
            (Some(session), Some(csrf)) => Ok(ServiceSession { session, csrf }),
            // Missing either half means the bridge is misconfigured — for
            // example CSRF turned off, which this design requires to stay on.
            // Treating that as "proceed without CSRF" would hide exactly the
            // misconfiguration worth failing on.
            _ => Err(EngineError::Unavailable),
        }
    }

    async fn session(&self) -> Result<ServiceSession, EngineError> {
        let mut held = self.service.lock().await;
        if let Some(existing) = held.as_ref() {
            return Ok(existing.clone());
        }
        let fresh = self.bootstrap().await?;
        *held = Some(fresh.clone());
        Ok(fresh)
    }

    async fn discard(&self) {
        *self.service.lock().await = None;
    }

    /// One unsafe request as the service identity, carrying Django's own CSRF
    /// token and cookie. A `403` discards the session and retries **once** —
    /// the token may simply have rotated — and a second `403` is reported as a
    /// refusal rather than retried into a loop.
    async fn unsafe_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<StatusCode, EngineError> {
        for attempt in 0..2 {
            let session = self.session().await?;
            let response = self
                .client
                .request(method.clone(), self.endpoint(path)?)
                .header(super::identity::USER, SERVICE_SUBJECT)
                .header("X-Forwarded-Proto", "https")
                .header("Origin", &self.origin)
                .header("X-CSRFToken", &session.csrf)
                .header(
                    reqwest::header::COOKIE,
                    format!(
                        "docs_sessionid={}; csrftoken={}",
                        session.session, session.csrf
                    ),
                )
                .json(&body)
                .send()
                .await
                .map_err(|_| EngineError::Unavailable)?;
            let status = response.status();
            if status == StatusCode::FORBIDDEN && attempt == 0 {
                self.discard().await;
                continue;
            }
            if status.is_server_error() {
                return Err(EngineError::Unavailable);
            }
            return Ok(status);
        }
        Err(EngineError::Refused)
    }

    /// Create the Docs document for a Neev resource, using the resource's own
    /// id. Verified writable on POST: `DocumentSerializer.get_fields` makes `id`
    /// "read-only on PUT requests but writable on POST requests".
    ///
    /// Idempotent by construction. A repeat for an id that already exists is
    /// upstream's conflict, which is success from this side's point of view —
    /// the document we wanted exists — so a retry after a partial failure
    /// converges instead of producing a second document.
    pub(crate) async fn provision(&self, resource_id: &str, title: &str) -> Result<(), EngineError> {
        let status = self
            .unsafe_request(
                reqwest::Method::POST,
                "api/v1.0/documents/",
                serde_json::json!({ "id": resource_id, "title": title }),
            )
            .await?;
        match status {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            // Already there: the desired state, reached by an earlier attempt.
            StatusCode::CONFLICT | StatusCode::BAD_REQUEST => {
                if self.exists(resource_id).await? {
                    Ok(())
                } else {
                    Err(EngineError::Invalid)
                }
            }
            _ => Err(EngineError::Refused),
        }
    }

    async fn exists(&self, resource_id: &str) -> Result<bool, EngineError> {
        let session = self.session().await?;
        let response = self
            .client
            .get(self.endpoint(&format!("api/v1.0/documents/{resource_id}/"))?)
            .header(super::identity::USER, SERVICE_SUBJECT)
            .header("X-Forwarded-Proto", "https")
            .header("Origin", &self.origin)
            .header(
                reqwest::header::COOKIE,
                format!("docs_sessionid={}", session.session),
            )
            .send()
            .await
            .map_err(|_| EngineError::Unavailable)?;
        if response.status().is_server_error() {
            return Err(EngineError::Unavailable);
        }
        Ok(response.status().is_success())
    }

    /// Mint a Docs session **for a named subject** and return its
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
    /// Rather than *validate* the browser's cookie, the proxy does not trust it
    /// at all: it mints the session here for the subject Neev just authorized
    /// and forwards that cookie to y-provider, discarding the client's. There is
    /// then no cookie for a caller to borrow, forge or leave stale, and no new
    /// endpoint had to be added to Docs to check one.
    ///
    /// The mechanism is upstream's own. `RemoteUserMiddleware.process_request`
    /// logs out and re-authenticates whenever the header names a different user
    /// than the current session, so sending A's header — with B's cookie or with
    /// none — yields a session belonging to A. Failure returns an error and the
    /// upgrade does not happen: fail-closed, never "proceed with what the client
    /// sent".
    pub(crate) async fn mint(&self, subject: &Assertion) -> Result<String, EngineError> {
        let response = self
            .client
            .get(self.endpoint("api/v1.0/users/me/")?)
            .header(super::identity::USER, &subject.subject)
            .header("X-Forwarded-Proto", "https")
            .header("Origin", &self.origin)
            .send()
            .await
            .map_err(|_| EngineError::Unavailable)?;
        if response.status().is_server_error() {
            return Err(EngineError::Unavailable);
        }
        if !response.status().is_success() {
            return Err(EngineError::Refused);
        }
        session_cookie(&response).ok_or(EngineError::Unavailable)
    }

    /// Project a caller's Neev-derived role onto their Docs access row.
    ///
    /// Re-derived from `access::authorized` on every connect and never cached,
    /// so this is a projection of Neev's answer rather than a second source of
    /// truth. It is performed as the service identity, which owns the document
    /// and may therefore manage its accesses.
    pub(crate) async fn project(
        &self,
        resource_id: &str,
        subject: &Assertion,
        role: Projected,
    ) -> Result<(), EngineError> {
        let status = self
            .unsafe_request(
                reqwest::Method::POST,
                &format!("api/v1.0/documents/{resource_id}/accesses/"),
                serde_json::json!({ "user_sub": subject.subject, "role": role.role() }),
            )
            .await?;
        match status {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            // An access row already exists. Upstream reports this as a conflict
            // or a validation error depending on the shape; either way the row
            // must be moved to the role Neev currently says, not left as it was.
            StatusCode::CONFLICT | StatusCode::BAD_REQUEST => {
                self.reproject(resource_id, subject, role).await
            }
            _ => Err(EngineError::Refused),
        }
    }

    async fn reproject(
        &self,
        resource_id: &str,
        subject: &Assertion,
        role: Projected,
    ) -> Result<(), EngineError> {
        let status = self
            .unsafe_request(
                reqwest::Method::PATCH,
                &format!("api/v1.0/documents/{resource_id}/accesses/"),
                serde_json::json!({ "user_sub": subject.subject, "role": role.role() }),
            )
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(EngineError::Refused)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_projection_follows_neevs_own_read_write_decision() {
        assert_eq!(Projected::from_neev(true), Projected::Editor);
        assert_eq!(Projected::from_neev(false), Projected::Reader);
        assert_eq!(Projected::Editor.role(), "editor");
        assert_eq!(Projected::Reader.role(), "reader");
    }

    #[test]
    fn the_service_principal_is_namespaced_away_from_every_real_user() {
        // Real subjects are `neev:<org_id>:<user_id>` with numeric parts, so a
        // literal `service` segment cannot be produced by any organization or
        // account id and no real user can ever be handed the owner principal.
        assert_eq!(SERVICE_SUBJECT, "neev:service:engine");
        let segments: Vec<_> = SERVICE_SUBJECT.split(':').collect();
        assert_eq!(segments.len(), 3);
        assert!(segments[1].parse::<i64>().is_err());
    }

    #[test]
    fn an_upstream_base_must_be_a_usable_http_origin() {
        assert!(Upstream::new(
            Url::parse("http://backend:8000/").unwrap(),
            "https://app.neev.so".into()
        )
        .is_ok());
        for bad in ["file:///etc/passwd", "mailto:someone@example.test"] {
            assert!(
                Upstream::new(Url::parse(bad).unwrap(), "https://app.neev.so".into()).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn unavailability_never_becomes_a_permission_answer() {
        // A caller must not be able to tell "Docs is down" apart from "you may
        // not" in a way that turns the former into access. Unavailable maps to
        // unavailable; only an actual refusal maps to invalid.
        assert_eq!(AuthError::from(EngineError::Unavailable), AuthError::Unavailable);
        assert_eq!(AuthError::from(EngineError::Refused), AuthError::Invalid);
        assert_eq!(AuthError::from(EngineError::Invalid), AuthError::Invalid);
    }
}
