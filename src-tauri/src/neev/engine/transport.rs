//! Moving bytes, once [`super::decision`] has said the request may move them.
//!
//! Two paths. HTTP forwarding asserts the caller's identity and passes
//! upstream's own CSRF pair through untouched. The websocket relay uses official
//! clients on both ends, carries a session cookie minted for the authorized
//! subject rather than the one the browser sent, and re-checks Neev's answer on
//! a bounded interval so a revocation closes a socket that is already open.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::ws::{Message, WebSocket},
    http::{header, HeaderMap, HeaderName, HeaderValue, Response, StatusCode},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message as UpstreamMessage;

use super::identity::{self, Assertion};
use super::proxy::Engine;
use crate::neev::{http::ApiError, startup::ServerState, types::OrgContext};

/// How often an open socket re-asks Neev whether it may still exist.
///
/// This is the revocation bound, so it is short enough to matter and long enough
/// not to hammer the database. The same shape as `http/realtime.rs`, which
/// already rechecks session, organization and resource before every chat
/// delivery.
const RECHECK: Duration = Duration::from_secs(15);

/// Bound on the upstream handshake. A stalled connect must not outlive the
/// authorization that permitted it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on a single relayed send. Without it a stalled peer parks the relay
/// inside `send` where no recheck tick can reach it, which would let a stalled
/// socket outlive a revocation.
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// A socket carrying no traffic at all is closed. Idle sockets otherwise
/// accumulate against the collaboration server for as long as a tab is open.
const IDLE_LIMIT: i64 = 30 * 60;

/// Cookies Docs is allowed to see. Everything else in the header — Neev's own
/// session above all — is dropped rather than forwarded to another service.
const DOCS_COOKIES: &[&str] = &[super::upstream::SESSION_COOKIE, "csrftoken"];

/// Keep only Docs' cookies from a browser `Cookie` header.
fn docs_cookies(value: &HeaderValue) -> Option<HeaderValue> {
    let raw = value.to_str().ok()?;
    let kept: Vec<&str> = raw
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            pair.split_once('=')
                .is_some_and(|(name, _)| DOCS_COOKIES.contains(&name.trim()))
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    HeaderValue::from_str(&kept.join("; ")).ok()
}

/// Headers that belong to one hop and must not be copied onto the next.
fn hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

/// Forward an authorized request to the Docs backend.
pub(crate) async fn forward(
    engine: &Engine,
    parts: axum::http::request::Parts,
    body: Vec<u8>,
    assertion: &Assertion,
) -> Result<Response<Body>, ApiError> {
    // The engine prefix is ours; upstream serves these paths at its own root.
    let relative = parts
        .uri
        .path()
        .strip_prefix(super::routes::PREFIX)
        .unwrap_or("")
        .trim_start_matches('/')
        .to_owned();
    let mut url = engine
        .backend
        .join(&relative)
        .map_err(|_| ApiError::NotFound)?;
    url.set_query(parts.uri.query());

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .map_err(|_| ApiError::Invalid)?;
    let mut request = engine.client.request(method, url);
    for (name, value) in parts.headers.iter() {
        if hop_by_hop(name.as_str()) || identity::stripped(name.as_str()) {
            continue;
        }
        if name == header::COOKIE {
            // Only Docs' own cookies cross to Docs. The browser's cookie header
            // also carries Neev's session, and handing a different service the
            // credential that authenticates Neev would be giving away exactly
            // the thing this proxy exists to hold.
            if let Some(filtered) = docs_cookies(value) {
                request = request.header(header::COOKIE, filtered);
            }
            continue;
        }
        request = request.header(name.clone(), value.clone());
    }
    // Set after the copy, so nothing a client sent can survive underneath.
    // Host and scheme come from the configured public origin: Django checks
    // Host against ALLOWED_HOSTS and reads the scheme from X-Forwarded-Proto,
    // so dropping Host or hardcoding https breaks a plain-HTTP loopback canary.
    request = request
        .header(identity::USER, &assertion.subject)
        .header(header::HOST, engine.public_host.clone())
        .header("X-Forwarded-Proto", engine.public_scheme);
    if !body.is_empty() {
        request = request.body(body);
    }

    let response = request
        .send()
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let status = StatusCode::from_u16(response.status().as_u16()).map_err(|_| ApiError::Unavailable)?;
    let mut headers = HeaderMap::new();
    for (name, value) in response.headers().iter() {
        if hop_by_hop(name.as_str()) {
            continue;
        }
        // Upstream's Set-Cookie is passed through: Django's session and CSRF
        // cookies are how the browser keeps working, and rewriting them here
        // would be inventing a session rather than relaying one.
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            headers.append(name, value);
        }
    }
    let bytes = response.bytes().await.map_err(|_| ApiError::Unavailable)?;
    let mut out = Response::new(Body::from(bytes));
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    Ok(out)
}

/// Relay an authorized socket to the collaboration server.
///
/// `session` is the cookie minted for the subject Neev just authorized. The
/// client's own `Cookie` header is **not** forwarded: y-provider decides whose
/// socket this is purely from the cookie it receives, so forwarding the client's
/// would let native actor A open a socket as whoever their cookie named.
pub(crate) async fn relay(
    client: WebSocket,
    url: url::Url,
    session: &str,
    state: Arc<ServerState>,
    actor: OrgContext,
    resource: String,
    // The capability the socket was opened with. A reader's socket must not be
    // torn down for failing a write check it was never granted, and a writer's
    // must be torn down the moment it stops being a writer.
    opened_writable: bool,
) {
    let request = match upstream_request(&url, session) {
        Ok(request) => request,
        Err(()) => {
            close(client).await;
            return;
        }
    };
    // Bounded, so a stalled connect cannot sit past the point where the
    // authorization that permitted it has been withdrawn.
    let connect = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request));
    let (upstream, _) = match connect.await {
        Ok(Ok(connected)) => connected,
        // Unreachable, refused or stalled upstream closes the socket. It never
        // becomes a socket that silently does nothing, which would look like a
        // working editor that quietly loses edits.
        Ok(Err(_)) | Err(_) => {
            close(client).await;
            return;
        }
    };
    let (mut to_client, mut from_client) = client.split();
    let (mut to_upstream, mut from_upstream) = upstream.split();
    let mut recheck = tokio::time::interval(RECHECK);
    // The first tick is immediate and would re-ask what we just asked.
    recheck.tick().await;
    // Rechecking on the timer alone is not enough: a socket that is busy
    // relaying could carry a whole editing session between two ticks. So each
    // direction is gated too, rate-limited to RECHECK so a chatty socket does
    // not become a query storm.
    let mut checked_at = state.clock.now_unix();
    let mut active_at = state.clock.now_unix();

    loop {
        let permitted = tokio::select! {
            incoming = from_client.next() => {
                let Some(Ok(message)) = incoming else { break };
                // Checked before the gate and before anything is relayed: a
                // frame naming another document must never reach upstream, and
                // the recheck answers a different question ("is that
                // authorization still valid") than this one ("is this frame
                // about the document we authorized").
                let forwarded = match message {
                    Message::Binary(bytes) => {
                        if super::frames::check_binary_frame(&bytes, &resource).is_err() {
                            break;
                        }
                        UpstreamMessage::Binary(bytes.into())
                    }
                    // The protocol is binary. A text frame is either another
                    // protocol or an attempt to sidestep the check, and neither
                    // is something to relay.
                    Message::Text(_) => break,
                    Message::Ping(bytes) => UpstreamMessage::Ping(bytes.into()),
                    Message::Pong(bytes) => UpstreamMessage::Pong(bytes.into()),
                    Message::Close(_) => break,
                };
                if gate(&state, &actor, &resource, opened_writable, &mut checked_at).await {
                    active_at = state.clock.now_unix();
                    // Bounded, so a stalled peer cannot hold the relay past the
                    // point where revocation should have ended it.
                    match tokio::time::timeout(SEND_TIMEOUT, to_upstream.send(forwarded)).await {
                        Ok(Ok(())) => true,
                        _ => break,
                    }
                } else {
                    false
                }
            }
            outgoing = from_upstream.next() => {
                let Some(Ok(message)) = outgoing else { break };
                // A server that starts talking about another document is as
                // wrong as a client that does, so the same check runs outbound.
                let forwarded = match message {
                    UpstreamMessage::Binary(bytes) => {
                        if super::frames::check_binary_frame(&bytes, &resource).is_err() {
                            break;
                        }
                        Message::Binary(bytes.into())
                    }
                    UpstreamMessage::Text(_) => break,
                    UpstreamMessage::Ping(bytes) => Message::Ping(bytes.into()),
                    UpstreamMessage::Pong(bytes) => Message::Pong(bytes.into()),
                    UpstreamMessage::Close(_) => break,
                    // Raw frames are not produced by a read half.
                    UpstreamMessage::Frame(_) => continue,
                };
                if gate(&state, &actor, &resource, opened_writable, &mut checked_at).await {
                    active_at = state.clock.now_unix();
                    match tokio::time::timeout(SEND_TIMEOUT, to_client.send(forwarded)).await {
                        Ok(Ok(())) => true,
                        _ => break,
                    }
                } else {
                    false
                }
            }
            _ = recheck.tick() => {
                if state.clock.now_unix().saturating_sub(active_at) > IDLE_LIMIT {
                    break;
                }
                checked_at = state.clock.now_unix();
                still_permitted(&state, &actor, &resource, opened_writable).await
            }
        };
        if !permitted {
            break;
        }
    }
    let _ = to_client.close().await;
    let _ = to_upstream.close().await;
}

/// Recheck before relaying a message, rate-limited to [`RECHECK`].
///
/// Gating each direction matters because a busy socket can carry an entire
/// editing session between two timer ticks — the tick alone would make the
/// revocation bound depend on how quiet the connection happened to be. The rate
/// limit keeps a chatty socket from turning into a query storm.
async fn gate(
    state: &ServerState,
    actor: &OrgContext,
    resource: &str,
    opened_writable: bool,
    checked_at: &mut i64,
) -> bool {
    let now = state.clock.now_unix();
    if now.saturating_sub(*checked_at) < RECHECK.as_secs() as i64 {
        return true;
    }
    *checked_at = now;
    still_permitted(state, actor, resource, opened_writable).await
}

/// Re-ask Neev, against the capability the socket was **opened** with.
///
/// Asking for write unconditionally would close every reader's socket on the
/// first tick — they never had write and never will. A socket opened writable
/// that is no longer writable *is* closed: the client reconnects and is
/// projected as a reader, which is the only way upstream's read-only flag gets
/// reapplied to a live connection.
async fn still_permitted(
    state: &ServerState,
    actor: &OrgContext,
    resource: &str,
    opened_writable: bool,
) -> bool {
    let Ok(tx) = crate::neev::db::begin(&state.database).await else {
        // Cannot evaluate: close. Fail-closed, never fail-open.
        return false;
    };
    let permitted = crate::neev::content::access::authorized(
        &tx,
        actor,
        resource,
        // A reader is rechecked as a reader; a writer as a writer, so a
        // downgrade ends the socket.
        opened_writable,
    )
    .await
    .is_ok()
        && crate::neev::auth::session::recheck(&tx, actor, state.clock.now_unix())
            .await
            .is_ok();
    tx.rollback().await.ok();
    permitted
}

fn upstream_request(
    url: &url::Url,
    session: &str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, ()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.as_str().into_client_request().map_err(|_| ())?;
    let cookie = format!("{}={session}", super::upstream::SESSION_COOKIE);
    request.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ())?,
    );
    Ok(request)
}

async fn close(socket: WebSocket) {
    let mut socket = socket;
    let _ = socket.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_headers_are_not_relayed() {
        for name in ["connection", "transfer-encoding", "upgrade", "host", "content-length"] {
            assert!(hop_by_hop(name), "relayed hop-by-hop header {name}");
        }
        // Everything the editor actually needs is relayed, including both
        // halves of Django's CSRF check.
        for name in ["cookie", "x-csrftoken", "content-type", "accept", "set-cookie"] {
            assert!(!hop_by_hop(name), "wrongly dropped {name}");
        }
    }

    #[test]
    fn the_upstream_socket_carries_only_the_minted_cookie() {
        let url = url::Url::parse("ws://y-provider:4444/collaboration/ws/x?room=x").unwrap();
        let request = upstream_request(&url, "minted-key").unwrap();
        let cookie = request.headers().get(header::COOKIE).unwrap();
        assert_eq!(cookie, "docs_sessionid=minted-key");
        // Exactly one cookie header, so nothing of the client's can ride along.
        assert_eq!(request.headers().get_all(header::COOKIE).iter().count(), 1);
    }

    #[test]
    fn the_revocation_bound_is_short_enough_to_matter() {
        assert!(RECHECK <= Duration::from_secs(30));
    }
}
