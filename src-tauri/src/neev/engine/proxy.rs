//! The engine's HTTP surface: classify, authorize, then move bytes.
//!
//! All of the judgement lives in [`super::routes`] and [`super::decision`]; this
//! module is deliberately the boring part. What it is responsible for is the
//! handling that cannot be expressed as a pure decision:
//!
//! - deleting every header a client must not be able to assert, **before**
//!   anything reads the request;
//! - asserting the caller's identity from the Neev session, never from a header;
//! - serving the frontend's static export from the artifact directory, with the
//!   framing headers Django does not provide for it;
//! - rebuilding the collaboration URL from the authorized resource so the client
//!   never names a document, and forwarding a session cookie minted for the
//!   subject just authorized rather than the one the browser sent.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri},
    response::Response,
};

use super::{
    binding,
    decision::{decide, Decision, Grant, Refusal},
    identity::{self, Assertion},
    routes::{self, Target},
    upstream::Projected,
};
use crate::neev::{
    content::{access, types::ResourceKind},
    db,
    http::ApiError,
    startup::ServerState,
    types::OrgContext,
};

/// Bodies the engine will buffer in order to inspect their field set. The
/// durable Yjs save is the large one; anything past this is refused rather than
/// forwarded unchecked, because forwarding a body too large to inspect would
/// make the field policy optional in exactly the case that matters.
const MAX_BODY: usize = 10 * 1024 * 1024;

/// Framing headers for the engine's own responses.
///
/// Django keeps upstream's `frame-ancestors 'none'`, and the framed page is the
/// **static artifact served here**, not a Django response — so if this is not
/// set here the iframe simply does not render, and assuming the bridge provides
/// it would be wrong in both directions.
fn framing(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'self'"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("SAMEORIGIN"),
    );
}

/// Remove every header a client must never be able to assert.
///
/// Done first, on the owned request, so nothing downstream can read a forged
/// value — a forged identity header has to be indistinguishable from an absent
/// one, not merely overwritten later.
fn strip(headers: &mut HeaderMap) {
    let forged: Vec<HeaderName> = headers
        .keys()
        .filter(|name| identity::stripped(name.as_str()))
        .cloned()
        .collect();
    for name in forged {
        headers.remove(&name);
    }
}

/// Neev's own answer for a resource, re-derived for this request.
///
/// Never cached across requests: the whole point of evaluating it here is that
/// there is no interval in which Neev's answer and the enforced answer disagree.
async fn grant_for(
    state: &ServerState,
    actor: &OrgContext,
    resource_id: &str,
) -> Result<(Grant, bool), ApiError> {
    let tx = db::begin(&state.database).await?;
    let result = async {
        // Write first: the write answer distinguishes read-only (archived) from
        // full access, and the read answer alone cannot.
        let writable = access::authorized(&tx, actor, resource_id, true).await;
        let readable = access::authorized(&tx, actor, resource_id, false).await;
        let (grant, engine_backed) = match (writable, readable) {
            (Ok(resource), _) => {
                let bound = binding::bound(&tx, &resource).await?;
                (
                    if resource.kind == ResourceKind::Document {
                        Grant::Write
                    } else {
                        Grant::None
                    },
                    bound,
                )
            }
            (Err(_), Ok(resource)) => {
                let bound = binding::bound(&tx, &resource).await?;
                (
                    if resource.kind == ResourceKind::Document {
                        Grant::Read
                    } else {
                        Grant::None
                    },
                    bound,
                )
            }
            // No access, absent, or another organization's — all one answer.
            (Err(_), Err(_)) => (Grant::None, false),
        };
        Ok::<_, crate::neev::content::types::ContentError>((grant, engine_backed))
    }
    .await;
    // Read-only: nothing to commit, and rolling back keeps the native path's
    // lock discipline untouched by a proxy request.
    tx.rollback().await.ok();
    Ok(result?)
}

fn refuse(refusal: Refusal) -> ApiError {
    match refusal {
        Refusal::Unauthenticated => ApiError::Unauthorized,
        // A route that is not on the allowlist is simply not here.
        Refusal::NotAllowed => ApiError::NotFound,
        // Not "forbidden": answering forbidden would confirm the id exists.
        Refusal::Unknown | Refusal::NotEngineBacked => ApiError::NotFound,
        Refusal::ReadOnly => ApiError::Forbidden,
        Refusal::Body => ApiError::Invalid,
    }
}

/// Top-level JSON keys of a buffered body, for the field policy.
///
/// A body that is not a JSON object has no keys to check; it is admitted only
/// where the policy permits no keys at all, so a non-object cannot slip past a
/// policy that expects one.
fn body_keys(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(map)) => Some(map.keys().cloned().collect()),
        // Anything else is reported as "one unknown key", so only `Body::None`
        // — which permits nothing — could accept it, and it is refused wherever
        // a field policy applies.
        Ok(_) | Err(_) => None,
    }
}

pub(crate) struct Engine {
    pub(crate) upstream: Arc<super::upstream::Upstream>,
    /// Private base of the Docs backend. Reachable only on the compose network.
    pub(crate) backend: url::Url,
    /// Private base of the collaboration server.
    pub(crate) collaboration: url::Url,
    pub(crate) artifact_root: std::path::PathBuf,
    /// Shared client for forwarded requests. Redirects are refused: following
    /// one would carry the asserted identity somewhere it was not meant for.
    pub(crate) client: reqwest::Client,
}

impl Engine {
    pub(crate) fn new(
        config: &crate::neev::config::EngineConfig,
        origin: &url::Url,
    ) -> Result<Self, ApiError> {
        let upstream = super::upstream::Upstream::new(
            config.backend.clone(),
            origin.origin().ascii_serialization(),
            config.bridge_key.clone(),
        )
        .map_err(crate::neev::error::AuthError::from)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| ApiError::Unavailable)?;
        Ok(Self {
            upstream: Arc::new(upstream),
            backend: config.backend.clone(),
            collaboration: config.collaboration.clone(),
            artifact_root: config.artifact_root.clone(),
            client,
        })
    }
}

/// Build the upstream collaboration URL from the **authorized** resource.
///
/// The client's own path and `room` are discarded. Hocuspocus 3.4.4 fixes a
/// connection's document from the request URL once, at connect time
/// (`getDocumentNameFromRequest`), and upstream additionally rejects a mismatch
/// between that and `room` — so rebuilding the URL here means the client never
/// names a document at all, which is stronger than inspecting frames for a
/// document field that this version does not carry.
pub(crate) fn collaboration_url(base: &url::Url, resource: &str) -> Result<url::Url, ApiError> {
    let mut url = base
        .join(&format!("collaboration/ws/{resource}"))
        .map_err(|_| ApiError::Unavailable)?;
    url.set_query(Some(&format!("room={resource}")));
    Ok(url)
}

/// Identity for the caller, derived from the Neev session and nothing else.
pub(crate) fn assertion(actor: &OrgContext) -> Assertion {
    Assertion {
        subject: identity::subject(actor),
    }
}

/// Classify, authorize and decide. Separated from the forwarding so the handler
/// and the socket upgrade share one path to a decision.
pub(crate) async fn authorize(
    state: &ServerState,
    actor: Option<&OrgContext>,
    method: &Method,
    uri: &Uri,
    body: &[u8],
) -> Result<(Decision, Option<Target>), ApiError> {
    let target = routes::classify(method.as_str(), uri.path(), uri.query());
    let Some(actor) = actor else {
        return Ok((Decision::Refuse(Refusal::Unauthenticated), target));
    };
    let (grant, engine_backed) = match target.as_ref() {
        Some(Target::Editor { resource })
        | Some(Target::Document { resource, .. })
        | Some(Target::Collaboration { resource })
        | Some(Target::Media { resource, .. }) => grant_for(state, actor, resource).await?,
        _ => (Grant::None, false),
    };
    let keys = match target.as_ref() {
        Some(Target::Document { .. }) => body_keys(body),
        _ => Some(Vec::new()),
    };
    // A body we could not parse into a checkable key set is refused where a
    // field policy applies, rather than forwarded unchecked.
    let Some(keys) = keys else {
        return Ok((Decision::Refuse(Refusal::Body), target));
    };
    Ok((
        decide(target.as_ref(), true, grant, engine_backed, &keys),
        target,
    ))
}

/// The engine's HTTP handler.
pub(crate) async fn handle(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let (mut parts, body) = request.into_parts();
    // Before anything else reads the request.
    strip(&mut parts.headers);
    let engine = state.engine.as_ref().ok_or(ApiError::NotFound)?;

    let bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| ApiError::Invalid)?;

    let actor = crate::neev::http::catalog::organization_get(&state, &parts.headers)
        .await
        .ok();
    let (decision, _) = authorize(
        &state,
        actor.as_ref(),
        &parts.method,
        &parts.uri,
        bytes.as_ref(),
    )
    .await?;

    match decision {
        Decision::Refuse(refusal) => Err(refuse(refusal)),
        Decision::Asset => asset(engine, parts.uri.path()),
        Decision::Forward { .. } => {
            let actor = actor.ok_or(ApiError::Unauthorized)?;
            let assertion = assertion(&actor);
            super::transport::forward(engine, parts, bytes.to_vec(), &assertion).await
        }
        Decision::Upgrade { resource, role } => {
            let actor = actor.ok_or(ApiError::Unauthorized)?;
            upgrade(&state, parts, actor, resource, role).await
        }
    }
}

/// Serve a file from the frontend's static export.
///
/// Read from the artifact directory and never proxied, so a name that is not in
/// the artifact is absent rather than something that reaches Docs.
fn asset(engine: &Engine, path: &str) -> Result<Response, ApiError> {
    let relative = path
        .strip_prefix(routes::PREFIX)
        .unwrap_or("")
        .trim_start_matches('/');
    let mut response = super::assets::serve(&engine.artifact_root, relative)?;
    framing(response.headers_mut());
    Ok(response)
}

async fn upgrade(
    state: &Arc<ServerState>,
    mut parts: axum::http::request::Parts,
    actor: OrgContext,
    resource: String,
    role: Projected,
) -> Result<Response, ApiError> {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::extract::FromRequestParts;

    let engine = state.engine.as_ref().ok_or(ApiError::NotFound)?;
    let assertion = assertion(&actor);
    // Project the caller's role *before* the socket exists, so upstream's
    // read-only flag is decided from Neev's current answer rather than from
    // whatever access row happened to be left behind by an earlier connect.
    engine
        .upstream
        .project(&resource, &assertion, role)
        .await
        .map_err(crate::neev::error::AuthError::from)?;
    // Mint the cookie for the subject just authorized. The client's own cookie
    // is never forwarded, so it cannot choose whose socket this is.
    let session = engine
        .upstream
        .mint(&assertion, state.clock.as_ref())
        .await
        .map_err(crate::neev::error::AuthError::from)?;
    let url = collaboration_url(&engine.collaboration, &resource)?;

    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .map_err(|_| ApiError::Invalid)?;
    let state = Arc::clone(state);
    let mut response = upgrade.on_upgrade(move |socket| async move {
        super::transport::relay(socket, url, &session, state, actor, resource).await;
    });
    framing(response.headers_mut());
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "b3b1f0d2-8c4a-4f5e-9a1b-2c3d4e5f6a7b";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn a_forged_identity_header_is_deleted_not_overwritten() {
        // Overwriting would still leave the original readable to anything that
        // looked before we wrote. Absent and forged must be the same state.
        let mut h = headers(&[
            ("x-neev-remote-user", "neev:1:999"),
            ("X-Neev-Remote-Email", "attacker@example.test"),
            ("x-forwarded-proto", "https"),
            ("x-neev-bridge-key", "stolen"),
            ("cookie", "docs_sessionid=abc"),
            ("x-csrftoken", "token"),
        ]);
        strip(&mut h);
        for gone in [
            "x-neev-remote-user",
            "x-neev-remote-email",
            "x-forwarded-proto",
            "x-neev-bridge-key",
        ] {
            assert!(h.get(gone).is_none(), "survived stripping: {gone}");
        }
        // Upstream's own CSRF pair must survive, or the browser's CSRF check
        // could not work and we would be tempted to disable it.
        assert!(h.get("cookie").is_some());
        assert!(h.get("x-csrftoken").is_some());
    }

    #[test]
    fn the_collaboration_url_is_rebuilt_from_the_authorized_resource() {
        let base = url::Url::parse("http://y-provider:4444/").unwrap();
        let url = collaboration_url(&base, DOC).unwrap();
        assert_eq!(url.path(), format!("/collaboration/ws/{DOC}"));
        assert_eq!(url.query(), Some(format!("room={DOC}").as_str()));
        // Whatever the client asked for is irrelevant: the URL is a function of
        // the authorized resource alone, so the client never names a document.
        let other = "11111111-2222-4333-8444-555555555555";
        let rebuilt = collaboration_url(&base, other).unwrap();
        assert!(!rebuilt.as_str().contains(DOC));
    }

    #[test]
    fn a_body_that_cannot_be_checked_is_refused_where_a_policy_applies() {
        assert_eq!(body_keys(b""), Some(Vec::new()));
        assert_eq!(
            body_keys(br#"{"title":"x"}"#),
            Some(vec!["title".to_owned()])
        );
        // Not an object: no key set to check, so it cannot satisfy a policy that
        // names permitted keys.
        assert_eq!(body_keys(b"[1,2,3]"), None);
        assert_eq!(body_keys(b"not json"), None);
    }

    #[test]
    fn refusals_do_not_confirm_that_a_resource_exists() {
        // Unknown and not-engine-backed both answer not-found; only a genuine
        // read-only grant answers forbidden, and that is a caller who already
        // knows the resource exists.
        assert!(matches!(refuse(Refusal::Unknown), ApiError::NotFound));
        assert!(matches!(refuse(Refusal::NotEngineBacked), ApiError::NotFound));
        assert!(matches!(refuse(Refusal::NotAllowed), ApiError::NotFound));
        assert!(matches!(refuse(Refusal::ReadOnly), ApiError::Forbidden));
        assert!(matches!(refuse(Refusal::Unauthenticated), ApiError::Unauthorized));
    }

    #[test]
    fn engine_responses_carry_the_framing_django_does_not_provide() {
        let mut h = HeaderMap::new();
        framing(&mut h);
        assert_eq!(
            h.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "frame-ancestors 'self'"
        );
        assert_eq!(h.get("x-frame-options").unwrap(), "SAMEORIGIN");
    }
}
