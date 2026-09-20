//! Team-message push delivery: `GET /api/neev/chats/{id}/stream` (SSE).
//!
//! Spec §4 "real-time delivery" / §9 phase 2. Mounted only when
//! `config.realtime_enabled`; when it is rolled back the route is absent, the
//! capability reports false and clients keep polling. No data change either way.
//!
//! Security shape:
//! - **Authorize before subscribing.** `resources::get` proves the chat exists in
//!   the caller's org and that this user may read it, so an unauthorized caller
//!   never even creates a topic — a foreign tenant cannot learn a chat exists.
//! - **Re-authorize before every delivery** and on every idle heartbeat, by
//!   re-deriving the actor from the session cookie. Logout, member removal, a
//!   role change, an org switch, or the chat flipping to private all end the
//!   stream with `revoked` instead of continuing to deliver.
//! - **Content-free events.** A frame carries a sequence number only; the client
//!   re-reads the conversation through `GET /messages`, which re-runs the same
//!   ACL. There is no second copy of the authorization rules to drift.
//! - **No long-lived database connection.** Each recheck is a short transaction
//!   from the pool; between rechecks the stream holds nothing.
//! - **Bounded.** The hub caps subscribers per chat and per process (over the cap
//!   answers 503 and the client stays on polling), the fan-out buffer is bounded
//!   (overflow becomes a `resync` hint, never unbounded memory), the event sink
//!   is a bounded channel, and every connection has a hard lifetime after which
//!   the client reconnects and closes any gap from its own cursor.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};

use crate::neev::auth::session;
use crate::neev::content::realtime::{Notice, Subscription};
use crate::neev::content::{resources, types::ResourceKind};
use crate::neev::startup::ServerState;

use super::{catalog, content, cookies, ApiError};

/// Queued frames per connection. A stalled reader applies backpressure to its
/// own task rather than buffering without bound.
const SINK_CAPACITY: usize = 8;

fn frame(kind: &str, sequence: Option<i64>) -> Event {
    Event::default().data(match sequence {
        Some(sequence) => format!("{{\"type\":\"{kind}\",\"sequence\":\"{sequence}\"}}"),
        None => format!("{{\"type\":\"{kind}\"}}"),
    })
}

/// Re-derive the caller from the session cookie and re-prove read access,
/// returning the conversation's current sequence. Any failure means "stop".
async fn recheck(state: &ServerState, token: &str, chat_id: &str) -> Result<i64, ()> {
    let now = state.clock.now_unix();
    let actor = session::organization(&state.database, token, now)
        .await
        .map_err(|_| ())?;
    let chat = resources::get(state, &actor, chat_id).await.map_err(|_| ())?;
    if chat.kind != ResourceKind::Chat {
        return Err(());
    }
    Ok(chat.content_revision)
}

/// Deliver notices until access is lost, the client goes away, or the
/// connection's lifetime expires. Never returns content.
async fn pump(
    state: Arc<ServerState>,
    token: String,
    chat_id: String,
    mut subscription: Subscription,
    mut sink: mpsc::Sender<Event>,
) {
    let policy = state.realtime.policy();
    // `interval` fires immediately, so the first authorization recheck doubles
    // as the opening `sync`: the client compares it with its own cursor and
    // closes any gap opened while it was disconnected.
    let mut heartbeat = tokio::time::interval(policy.heartbeat);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let notice = tokio::select! {
            _ = heartbeat.tick() => None,
            notice = subscription.recv() => match notice {
                Some(notice) => Some(notice),
                None => return,
            },
        };
        let notice = match notice {
            Some(notice) => {
                // Collapse a burst into one recheck + one frame.
                tokio::time::sleep(policy.coalesce).await;
                Some(subscription.drain(notice))
            }
            None => None,
        };
        let Ok(sequence) = recheck(&state, &token, &chat_id).await else {
            let _ = sink.send(frame("revoked", None)).await;
            return;
        };
        let event = match notice {
            Some(Notice::Sequence(sequence)) => frame("message", Some(sequence)),
            Some(Notice::Lagged) => frame("resync", None),
            None => frame("sync", Some(sequence)),
        };
        if sink.send(event).await.is_err() {
            return; // the client hung up; dropping `subscription` frees its slot
        }
        if notice.is_some() {
            // A delivery is as good as a heartbeat: don't recheck again at once.
            heartbeat.reset();
        }
    }
}

/// `GET /api/neev/chats/{id}/stream` — content-free push notices for one chat.
pub(super) async fn stream(
    State(state): State<Arc<ServerState>>,
    path: Result<axum::extract::Path<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = content::safe_path(path)?;
    content::canonical_uuid(&id)?;
    // Hold the shared request permit only for the authorization work: a live
    // stream must not occupy one for its whole lifetime.
    let chat = {
        let _permit = catalog::permit(&state)?;
        let actor = catalog::organization_get(&state, &headers).await?;
        resources::get(&state, &actor, &id).await?
    };
    if chat.kind != ResourceKind::Chat {
        return Err(ApiError::NotFound);
    }
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&headers, names.session)?.to_owned();
    // Only now — after access is proven — may this caller occupy a slot.
    let subscription = state
        .realtime
        .subscribe(chat.org_id, &id)
        .ok_or(ApiError::Unavailable)?;
    let (sink, events) = mpsc::channel::<Event>(SINK_CAPACITY);
    let lifetime = state.realtime.policy().lifetime;
    tokio::spawn(async move {
        // The hard lifetime also covers a reader that stops draining mid-send.
        let _ = tokio::time::timeout(lifetime, pump(state, token, id, subscription, sink)).await;
    });
    let mut response = Sse::new(events.map(Ok::<Event, Infallible>)).into_response();
    super::add_private_headers(&mut response);
    Ok(response)
}

#[cfg(test)]
#[path = "realtime_tests.rs"]
mod realtime_tests;
