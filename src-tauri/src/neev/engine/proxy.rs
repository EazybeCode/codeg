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
/// Above upstream's own `DOCUMENT_IMAGE_MAX_SIZE` (10 MB) plus multipart
/// framing, so a legitimate maximum-size attachment is not refused here before
/// Django ever sees it and gets to give the caller its own error.
const MAX_BODY: usize = 12 * 1024 * 1024;

/// Largest attachment relayed. Matches the bridge's own stream bound.
const MAX_MEDIA: usize = 12 * 1024 * 1024;

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

/// Require an exact `Origin` on anything that changes state or opens a socket.
///
/// Django's CSRF check stays on and is passed through untouched; this is an
/// additional native check, not a replacement. It matters because the engine's
/// own boundary must not depend on a downstream service's configuration being
/// right — and because the socket upgrade is not covered by Django's CSRF at
/// all, since y-provider terminates it.
fn origin_ok(engine: &Engine, method: &Method, target: Option<&Target>, headers: &HeaderMap) -> bool {
    let upgrade = matches!(target, Some(Target::Collaboration { .. }));
    let mutating = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if !upgrade && !mutating {
        return true;
    }
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == engine.public_origin)
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
    /// `host[:port]` of the configured public origin. Django matches this
    /// against `ALLOWED_HOSTS`, so it has to be the public name and not the
    /// container address the request is actually sent to.
    pub(crate) public_host: String,
    /// Scheme of the configured public origin, sent as `X-Forwarded-Proto`.
    /// Hardcoding `https` breaks a plain-HTTP loopback canary.
    pub(crate) public_scheme: &'static str,
    /// Serialized origin (`scheme://host[:port]`) that an unsafe request or a
    /// socket upgrade must declare.
    pub(crate) public_origin: String,
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
        let public_host = match origin.port() {
            Some(port) => format!("{}:{port}", origin.host_str().unwrap_or_default()),
            None => origin.host_str().unwrap_or_default().to_owned(),
        };
        Ok(Self {
            upstream: Arc::new(upstream),
            backend: config.backend.clone(),
            collaboration: config.collaboration.clone(),
            artifact_root: config.artifact_root.clone(),
            client,
            public_host,
            public_scheme: if origin.scheme() == "http" { "http" } else { "https" },
            public_origin: origin.origin().ascii_serialization(),
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
    // The official client's upgrade URL is `/collaboration/ws/?room=<uuid>`; it
    // appends nothing else. Putting the id in a path segment would be a shape
    // nothing upstream reads.
    let mut url = base
        .join("collaboration/ws/")
        .map_err(|_| ApiError::Unavailable)?;
    url.set_query(Some(&format!("room={resource}")));
    // Deployment configures one private http(s) base; tungstenite accepts only
    // ws/wss and errors with UnsupportedUrlScheme otherwise, which would have
    // failed every connect with no diagnostic beyond a closed socket.
    let scheme = match url.scheme() {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        _ => return Err(ApiError::Unavailable),
    };
    url.set_scheme(scheme).map_err(|_| ApiError::Unavailable)?;
    Ok(url)
}

/// Identity for the caller, derived from the Neev session and Neev's own tables
/// — never from a request header.
pub(crate) async fn assertion(state: &ServerState, actor: &OrgContext) -> Assertion {
    identity::assertion(&state.database, actor).await
}

/// Authorize a classified request once the body (if any) is in hand.
///
/// Split from classification deliberately: the caller authenticates and
/// classifies **before** reading a body, so an unauthenticated or unroutable
/// request never causes bytes to be buffered on its behalf.
pub(crate) async fn authorize(
    state: &ServerState,
    actor: &OrgContext,
    target: Option<&Target>,
    body: &[u8],
) -> Result<Decision, ApiError> {
    let (grant, engine_backed) = match target {
        Some(Target::Editor { resource })
        | Some(Target::Document { resource, .. })
        | Some(Target::Collaboration { resource })
        | Some(Target::Media { resource, .. }) => grant_for(state, actor, resource).await?,
        _ => (Grant::None, false),
    };
    let keys = match target {
        // Only the JSON policies inspect a body. An attachment upload is
        // multipart, so parsing it as JSON would fail and refuse every upload —
        // it is bounded by size and by its own route instead.
        Some(Target::Document {
            body: routes::Body::TitleOnly,
            ..
        })
        | Some(Target::Document {
            body: routes::Body::Content,
            ..
        }) => body_keys(body),
        _ => Some(Vec::new()),
    };
    // A body we could not parse into a checkable key set is refused where a
    // field policy applies, rather than forwarded unchecked.
    let Some(keys) = keys else {
        return Ok(Decision::Refuse(Refusal::Body));
    };
    Ok(decide(target, true, grant, engine_backed, &keys))
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

    // Classify and authenticate **before** the body is read. An unauthenticated
    // caller, or one naming a route that is not on the allowlist, must not be
    // able to make this process buffer megabytes on their behalf.
    let target = routes::classify(parts.method.as_str(), parts.uri.path(), parts.uri.query());
    // The editor shell is the one route whose failures are rendered inside an
    // iframe, where a JSON body reads as a blank document.
    let shell = matches!(target, Some(Target::Editor { .. }));
    let fail = |error: ApiError| -> Result<Response, ApiError> {
        if shell {
            Ok(editor_error(error))
        } else {
            Err(error)
        }
    };

    if target.is_none() {
        return fail(ApiError::NotFound);
    }
    let Some(actor) = crate::neev::http::catalog::organization_get(&state, &parts.headers)
        .await
        .ok()
    else {
        return fail(ApiError::Unauthorized);
    };
    if !origin_ok(engine, &parts.method, target.as_ref(), &parts.headers) {
        return fail(ApiError::Forbidden);
    }

    // Only now, with a known route and a known caller.
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return fail(ApiError::Invalid),
    };

    let decision = match authorize(&state, &actor, target.as_ref(), bytes.as_ref()).await {
        Ok(decision) => decision,
        Err(error) => return fail(error),
    };

    let served = match decision {
        Decision::Refuse(refusal) => Err(refuse(refusal)),
        Decision::Asset { ref path } => asset(engine, path),
        Decision::Shell { resource, role } => {
            let assertion = assertion(&state, &actor).await;
            // Project before the shell renders, so the editor's very first API
            // call already finds the access row it needs.
            engine
                .upstream
                .project(&resource, &assertion, role)
                .await
                .map_err(crate::neev::error::AuthError::from)?;
            // Provisioning is idempotent and the id is the resource's own, so
            // this repairs a resource whose creation committed natively but
            // whose upstream document never got made — the "retry converges"
            // half of having no atomicity across the two stores. Without it
            // such a document would stay broken for good.
            let title = crate::neev::content::resources::get(&state, &actor, &resource)
                .await
                .map(|resource| resource.title)
                .unwrap_or_default();
            engine
                .upstream
                .provision(&resource, &title, &assertion)
                .await
                .map_err(crate::neev::error::AuthError::from)?;
            asset(engine, &shell_path(parts.uri.path()))
        }
        Decision::Forward { resource: Some(ref resource), .. }
            if matches!(target, Some(Target::Media { .. })) =>
        {
            let Some(Target::Media { key, .. }) = target.as_ref() else {
                return fail(ApiError::NotFound);
            };
            media(engine, resource, key, &assertion(&state, &actor).await).await
        }
        Decision::Forward { resource, role } => {
            let assertion = assertion(&state, &actor).await;
            // Project before forwarding, not only before a socket. A non-creator
            // has no Docs access row of their own, so upstream would answer 403
            // on the document GET and the editor would never start — the socket
            // would then never be reached at all.
            if let (Some(resource), Some(role)) = (resource.as_ref(), role) {
                engine
                    .upstream
                    .project(resource, &assertion, role)
                    .await
                    .map_err(crate::neev::error::AuthError::from)?;
            }
            // A rename needs both orderings at once, and each protects a
            // different failure.
            //
            // The native *precheck* runs first, because a caller Neev would
            // refuse must not change the Docs title and then fail — that leaves
            // Docs ahead of a rename that never happened.
            //
            // The native *write* runs last, because a request Django's CSRF
            // check rejects, or one that never reached an available backend,
            // must not change the native title either. Being the source of
            // truth is not permission to mutate on a refused write.
            //
            // The revision is captured here, before the forward. Reading a
            // fresh one afterwards would let two concurrent renames each read
            // a revision after their own forward and both succeed, leaving the
            // two stores holding each other's titles.
            let rename = pending_title(&parts, bytes.as_ref());
            let expected = match (resource.as_ref(), rename.as_ref()) {
                (Some(resource), Some(_)) => Some(precheck_rename(&state, &actor, resource).await?),
                _ => None,
            };
            let response =
                super::transport::forward(engine, parts, bytes.to_vec(), &assertion).await?;
            if let (Some(resource), Some(title), Some(expected), true) = (
                resource.as_ref(),
                rename,
                expected,
                response.status().is_success(),
            ) {
                sync_title(&state, engine, &actor, &assertion, resource, title, expected).await?;
            }
            Ok(response)
        }
        Decision::Upgrade { resource, role } => {
            upgrade(&state, parts, actor, resource, role).await
        }
    };
    match served {
        Err(error) if shell => Ok(editor_error(error)),
        other => other,
    }
}

/// Confirm natively that this caller may rename this resource, and capture the
/// revision the later write must present.
///
/// Runs before anything is sent upstream, so a caller Neev would refuse never
/// changes the Docs title and then fails.
///
/// **The check is deliberately access-and-archived, not creator.** Neev's
/// native model lets any actor who can see a resource rename it —
/// `resources::update` restricts only visibility, archive and node moves to the
/// creator — so requiring creatorship here would be this bridge inventing a
/// stricter policy than the product has, and a shared editor would find the
/// title read-only for no reason they could see. Consistency with the native
/// model is the point; it is not a gap. Archived resources are read-only, and
/// that is the one refusal this adds.
async fn precheck_rename(
    state: &ServerState,
    actor: &OrgContext,
    resource_id: &str,
) -> Result<i64, ApiError> {
    let current = crate::neev::content::resources::get(state, actor, resource_id).await?;
    if current.archived {
        return Err(ApiError::Archived);
    }
    Ok(current.auth_revision)
}

/// The title a request is trying to set, if it is a rename at all.
fn pending_title(parts: &axum::http::request::Parts, body: &[u8]) -> Option<String> {
    if parts.method != Method::PATCH {
        return None;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("title")?
        .as_str()
        .map(str::to_owned)
}

/// Record a rename that upstream has already accepted, through Neev's own
/// resource API so native rename authority, revision conflict handling and the
/// lease checks are the native ones rather than a second implementation.
///
/// Without this the Docs copy and the Neev resource drift apart the first time
/// anyone renames inside the editor, and the Projects list keeps showing the old
/// name with nothing to reconcile it.
///
/// One retry on a revision conflict, because a concurrent native rename is
/// ordinary rather than exceptional. A conflict that survives the retry is
/// reported to the caller instead of being swallowed: the two titles really do
/// disagree at that point, and hiding it would leave a divergence nobody is told
/// about.
#[allow(clippy::too_many_arguments)]
async fn sync_title(
    state: &ServerState,
    engine: &Engine,
    actor: &OrgContext,
    assertion: &Assertion,
    resource_id: &str,
    title: String,
    expected: i64,
) -> Result<(), ApiError> {
    let current = crate::neev::content::resources::get(state, actor, resource_id).await?;
    if current.title == title {
        return Ok(());
    }
    let patch = crate::neev::content::types::ResourcePatch {
        // The revision captured before the forward, not a fresh read: a fresh
        // read here is what would let two concurrent renames both win.
        expected_auth_revision: expected,
        title: title.clone(),
        node_id: current.node_id,
        visibility: current.visibility,
        archived: current.archived,
    };
    match crate::neev::content::resources::update(state, actor, resource_id, patch).await {
        Ok(_) => Ok(()),
        Err(crate::neev::content::types::ContentError::Conflict) => {
            // This rename lost the native race, but Docs already accepted it, so
            // Docs is now ahead of the value Projects shows. Push the native
            // title back over it and report the conflict — leaving the two
            // stores disagreeing silently is the outcome worth avoiding, and
            // Neev's row is the one the ACL is evaluated against.
            let winner = crate::neev::content::resources::get(state, actor, resource_id).await?;
            engine
                .upstream
                .push_title(resource_id, assertion, &winner.title)
                .await
                .map_err(crate::neev::error::AuthError::from)?;
            Err(ApiError::Conflict)
        }
        Err(error) => Err(error.into()),
    }
}

/// The marker the Neev UI looks for to tell an engine failure apart from an
/// ordinary page, so it can offer a retry instead of showing an empty frame.
const ERROR_MARKER: &str = "neev-document-engine-error";

/// Render an editor-shell failure as HTML carrying [`ERROR_MARKER`].
///
/// The editor loads in an iframe, so a JSON error body is invisible: the frame
/// renders it as text or renders nothing, and the failure reads as a blank
/// document. That is the worst available outcome — indistinguishable from a
/// document whose content is gone. This returns a real HTML document instead,
/// with the failure named in a `<meta>` the UI can read and a human-readable
/// line for anyone who sees the frame directly.
///
/// The status is preserved rather than flattened to 200: the marker is for the
/// UI, and the status is still the truth about what happened.
fn editor_error(error: ApiError) -> Response {
    let (status, code) = match error {
        ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
        ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        ApiError::Archived => (StatusCode::CONFLICT, "archived"),
        ApiError::Conflict => (StatusCode::CONFLICT, "conflict"),
        ApiError::Limited => (StatusCode::TOO_MANY_REQUESTS, "limited"),
        ApiError::Invalid => (StatusCode::BAD_REQUEST, "invalid"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    // Retryable states are the ones where trying again can plausibly succeed;
    // a refusal is not one of them, and offering a retry button for it would be
    // a lie the user pays for in repeated clicks.
    let retryable = matches!(
        error,
        ApiError::Unavailable | ApiError::StorageUnavailable | ApiError::RequestTimeout
    );
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"{ERROR_MARKER}\" content=\"{code}\">\
<meta name=\"{ERROR_MARKER}-retryable\" content=\"{retryable}\">\
<title>Document unavailable</title></head>\
<body><p>This document could not be opened ({code}).</p></body></html>"
    );
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    framing(response.headers_mut());
    response
}

/// Relay an attachment from the bridge's private media endpoint.
///
/// The response headers are an **allowlist**, not a filtered copy: the object
/// GET carries `x-amz-meta-owner`, which is a Docs user id, and anything else
/// upstream chooses to add later would be relayed by a denylist that had not
/// been updated for it.
async fn media(
    engine: &Engine,
    resource: &str,
    key: &str,
    assertion: &Assertion,
) -> Result<Response, ApiError> {
    let upstream = engine
        .upstream
        .media(resource, key, assertion)
        .await
        .map_err(crate::neev::error::AuthError::from)?;
    let status = StatusCode::from_u16(upstream.status().as_u16()).map_err(|_| ApiError::Unavailable)?;
    let mut kept = HeaderMap::new();
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_DISPOSITION,
        header::CACHE_CONTROL,
        header::CONTENT_SECURITY_POLICY,
        HeaderName::from_static("x-content-type-options"),
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            kept.insert(name, value.clone());
        }
    }
    // The disposition and the CSP are the XSS control, and they are decided from
    // the stored content type and the `-unsafe` key marker — neither of which is
    // visible here. So they are relayed verbatim and never rewritten. If they
    // are missing on a success, something upstream has changed and serving the
    // bytes unprotected would be the wrong way to find out.
    if status.is_success()
        && !(kept.contains_key(header::CONTENT_SECURITY_POLICY)
            && kept.contains_key(header::CONTENT_DISPOSITION))
    {
        return Err(ApiError::Unavailable);
    }
    // The bridge bounds its own stream at 12 MiB; this is the same bound applied
    // on this side, so an upstream that stopped enforcing it could not make this
    // process read an unbounded attachment into memory.
    let bytes = super::transport::read_bounded(upstream, MAX_MEDIA).await?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    *response.headers_mut() = kept;
    Ok(response)
}

/// Serve a file from the frontend's static export.
///
/// Read from the artifact directory and never proxied, so a name that is not in
/// the artifact is absent rather than something that reaches Docs.
fn asset(engine: &Engine, relative: &str) -> Result<Response, ApiError> {
    let mut response = super::assets::serve(&engine.artifact_root, relative)?;
    framing(response.headers_mut());
    Ok(response)
}

/// The artifact-relative path for an editor shell request.
fn shell_path(path: &str) -> String {
    path.strip_prefix(routes::PREFIX)
        .unwrap_or("")
        .trim_start_matches('/')
        .to_owned()
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
    let assertion = assertion(state, &actor).await;
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
    let origin = engine.public_origin.clone();
    let mut response = upgrade.on_upgrade(move |socket| async move {
        super::transport::relay(
            socket,
            url,
            &session,
            state,
            actor,
            resource,
            matches!(role, Projected::Editor),
            origin,
        )
        .await;
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
        assert_eq!(url.path(), "/collaboration/ws/");
        assert_eq!(url.query(), Some(format!("room={DOC}").as_str()));
        // tungstenite accepts only ws/wss; an http base would have failed every
        // connect with nothing but a closed socket to show for it.
        assert_eq!(url.scheme(), "ws");
        assert_eq!(
            collaboration_url(&url::Url::parse("https://y:4444/").unwrap(), DOC)
                .unwrap()
                .scheme(),
            "wss"
        );
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
