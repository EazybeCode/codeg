//! In-process fan-out for team-message push delivery (spec §4 "real-time
//! delivery", §9 phase 2).
//!
//! Deliberately **content-free**: a notice carries only the chat's new sequence
//! number, never a body, an author, or a title. Subscribers learn *that* their
//! already-authorized conversation advanced and then re-read it through the
//! normal `messages::list` path, which re-runs the org/private ACL and the
//! session recheck. That keeps exactly one authorization implementation and
//! makes a cross-tenant content leak structurally impossible here.
//!
//! Existence is also scoped: a topic key is `(org_id, chat_id)` and the HTTP
//! layer only subscribes after `resources::get` proved the caller may read that
//! chat, so a foreign-org caller never observes that a chat exists at all.
//!
//! No subscriber holds a database connection: the hub is pure in-memory state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;

/// Per-topic buffered notices. A subscriber that falls this far behind is told
/// to resync instead of being fed an unbounded backlog.
const TOPIC_CAPACITY: usize = 64;
/// Ceiling on concurrent streams for the whole process, and per conversation.
/// Refused subscriptions become an honest 503 and the client keeps polling.
const MAX_TOTAL_SUBSCRIBERS: usize = 512;
const MAX_TOPIC_SUBSCRIBERS: usize = 64;

/// What a live subscriber is told. Never content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// The conversation advanced to at least this sequence.
    Sequence(i64),
    /// This subscriber fell behind the bounded buffer; re-read from its cursor.
    Lagged,
}

/// Timing for a single stream. Held on the hub so tests can shorten it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamPolicy {
    /// Idle interval between authorization rechecks / keep-alives. Deliberately
    /// close to the 5s poll interval it replaces: a live stream must not let a
    /// revoked reader keep a conversation on screen much longer than polling
    /// would have. Delivery is gated on a fresh recheck regardless.
    pub(crate) heartbeat: Duration,
    /// Burst window: notices arriving inside it collapse into one recheck.
    pub(crate) coalesce: Duration,
    /// Hard lifetime of one stream; the client reconnects afterwards. Bounds
    /// how long any single connection can hold a subscriber slot.
    pub(crate) lifetime: Duration,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(10),
            coalesce: Duration::from_millis(150),
            lifetime: Duration::from_secs(600),
        }
    }
}

struct Topic {
    sender: broadcast::Sender<i64>,
    subscribers: usize,
}

/// Fan-out registry keyed by `(org_id, chat_id)`.
pub(crate) struct MessageHub {
    policy: StreamPolicy,
    topics: Mutex<HashMap<(i32, String), Topic>>,
}

impl MessageHub {
    pub(crate) fn new(policy: StreamPolicy) -> Self {
        Self {
            policy,
            topics: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn policy(&self) -> StreamPolicy {
        self.policy
    }

    /// Announce that `chat_id` in `org_id` advanced to `sequence`. Cheap and
    /// non-blocking: with no live subscriber there is no topic and nothing to do.
    pub(crate) fn publish(&self, org_id: i32, chat_id: &str, sequence: i64) {
        let Ok(topics) = self.topics.lock() else {
            return;
        };
        if let Some(topic) = topics.get(&(org_id, chat_id.to_owned())) {
            // Err only means "no receiver right now"; the slot is reclaimed on drop.
            let _ = topic.sender.send(sequence);
        }
    }

    /// Reserve a subscriber slot. `None` when the global or per-conversation cap
    /// is reached — the caller answers 503 and the client stays on polling.
    ///
    /// The caller MUST have proven read access to `chat_id` first.
    pub(crate) fn subscribe(self: &Arc<Self>, org_id: i32, chat_id: &str) -> Option<Subscription> {
        let mut topics = self.topics.lock().ok()?;
        let total: usize = topics.values().map(|topic| topic.subscribers).sum();
        if total >= MAX_TOTAL_SUBSCRIBERS {
            return None;
        }
        let key = (org_id, chat_id.to_owned());
        let topic = topics.entry(key.clone()).or_insert_with(|| Topic {
            sender: broadcast::channel(TOPIC_CAPACITY).0,
            subscribers: 0,
        });
        // A freshly inserted topic starts at zero, so a refusal here always
        // leaves an existing, still-occupied topic behind.
        if topic.subscribers >= MAX_TOPIC_SUBSCRIBERS {
            return None;
        }
        topic.subscribers += 1;
        let receiver = topic.sender.subscribe();
        Some(Subscription {
            hub: self.clone(),
            key,
            receiver,
        })
    }

    #[cfg(test)]
    pub(crate) fn topic_count(&self) -> usize {
        self.topics.lock().unwrap().len()
    }

    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.topics
            .lock()
            .unwrap()
            .values()
            .map(|topic| topic.subscribers)
            .sum()
    }

    fn release(&self, key: &(i32, String)) {
        let Ok(mut topics) = self.topics.lock() else {
            return;
        };
        if let Some(topic) = topics.get_mut(key) {
            topic.subscribers = topic.subscribers.saturating_sub(1);
            if topic.subscribers == 0 {
                topics.remove(key);
            }
        }
    }
}

impl Default for MessageHub {
    fn default() -> Self {
        Self::new(StreamPolicy::default())
    }
}

/// A live subscriber slot. Dropping it (stream end, client disconnect, cancel,
/// task abort) releases the slot and removes the topic once it is empty.
pub(crate) struct Subscription {
    hub: Arc<MessageHub>,
    key: (i32, String),
    receiver: broadcast::Receiver<i64>,
}

impl Subscription {
    /// Next notice, or `None` once the hub side is gone.
    pub(crate) async fn recv(&mut self) -> Option<Notice> {
        match self.receiver.recv().await {
            Ok(sequence) => Some(Notice::Sequence(sequence)),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(Notice::Lagged),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Drain everything already buffered, collapsing a burst into one notice.
    /// `Lagged` wins over a sequence because it demands a full cursor re-read.
    pub(crate) fn drain(&mut self, seen: Notice) -> Notice {
        let mut latest = seen;
        loop {
            match self.receiver.try_recv() {
                Ok(sequence) => {
                    if !matches!(latest, Notice::Lagged) {
                        latest = Notice::Sequence(sequence);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => latest = Notice::Lagged,
                Err(_) => return latest,
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.release(&self.key);
    }
}

#[cfg(test)]
#[path = "realtime_tests.rs"]
mod realtime_tests;
