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
        request = request.header(name.clone(), value.clone());
    }
    // Set after the copy, so nothing a client sent can survive underneath.
    request = request
        .header(identity::USER, &assertion.subject)
        .header("X-Forwarded-Proto", "https");
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
) {
    let request = match upstream_request(&url, session) {
        Ok(request) => request,
        Err(()) => {
            close(client).await;
            return;
        }
    };
    let (upstream, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(connected) => connected,
        // Unreachable upstream closes the socket. It never becomes a socket that
        // silently does nothing, which would look like a working editor that
        // loses edits.
        Err(_) => {
            close(client).await;
            return;
        }
    };
    let (mut to_client, mut from_client) = client.split();
    let (mut to_upstream, mut from_upstream) = upstream.split();
    let mut recheck = tokio::time::interval(RECHECK);
    // The first tick is immediate and would re-ask what we just asked.
    recheck.tick().await;

    loop {
        tokio::select! {
            incoming = from_client.next() => {
                let Some(Ok(message)) = incoming else { break };
                let forwarded = match message {
                    Message::Binary(bytes) => UpstreamMessage::Binary(bytes.into()),
                    Message::Text(text) => UpstreamMessage::Text(text.as_str().into()),
                    Message::Ping(bytes) => UpstreamMessage::Ping(bytes.into()),
                    Message::Pong(bytes) => UpstreamMessage::Pong(bytes.into()),
                    Message::Close(_) => break,
                };
                if to_upstream.send(forwarded).await.is_err() { break }
            }
            outgoing = from_upstream.next() => {
                let Some(Ok(message)) = outgoing else { break };
                let forwarded = match message {
                    UpstreamMessage::Binary(bytes) => Message::Binary(bytes.into()),
                    UpstreamMessage::Text(text) => Message::Text(text.as_str().into()),
                    UpstreamMessage::Ping(bytes) => Message::Ping(bytes.into()),
                    UpstreamMessage::Pong(bytes) => Message::Pong(bytes.into()),
                    UpstreamMessage::Close(_) => break,
                    // Raw frames are not produced by a read half.
                    UpstreamMessage::Frame(_) => continue,
                };
                if to_client.send(forwarded).await.is_err() { break }
            }
            _ = recheck.tick() => {
                // Loss of access, session expiry, organization switch or a
                // downgrade to read-only all end the socket. Anything that
                // fails to evaluate ends it too — fail-closed, never fail-open.
                if !still_permitted(&state, &actor, &resource).await { break }
            }
        }
    }
    let _ = to_client.close().await;
    let _ = to_upstream.close().await;
}

/// Re-ask Neev. A downgrade to read-only closes the socket rather than
/// silently leaving a writable one open: the client reconnects and is projected
/// as a reader, which is the only way upstream's read-only flag gets reapplied.
async fn still_permitted(state: &ServerState, actor: &OrgContext, resource: &str) -> bool {
    let Ok(tx) = crate::neev::db::begin(&state.database).await else {
        return false;
    };
    let permitted = crate::neev::content::access::authorized(&tx, actor, resource, true)
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
