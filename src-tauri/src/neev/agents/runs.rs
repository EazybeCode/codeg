//! Live agent runs: the in-process side of a **server-owned** run (spec §5.2).
//!
//! A run is started by a request but is not owned by it. The driver task below
//! consumes the runtime's event stream, accumulates the reply, persists it to
//! the turn row as it grows, and settles the turn on the terminal event — all
//! independently of whether the browser that started it is still listening.
//! That is what makes **reattach + replay** possible instead of a silent
//! re-run: a dropped stream reconnects by run id and is served the already
//! produced output, never a second execution of the prompt.
//!
//! Deltas are broadcast with a **character offset** so an attaching client can
//! splice the live stream onto a persisted snapshot without a gap or a
//! duplicate: it subscribes first, then reads the buffer, so any delta it
//! receives is either already inside its snapshot (and skipped by offset) or
//! entirely after it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot};

use super::execution::{RunEvent, RunHandle};
use super::sessions::{self, TurnStatus, MAX_RESPONSE_CHARS};
use crate::neev::startup::ServerState;

/// Persist the accumulated reply once it has grown by this many characters, so
/// a reload mid-run shows recent output without writing on every token.
const FLUSH_CHARS: usize = 256;
/// Buffered deltas per subscriber before it is considered lagged (it then
/// re-syncs from the shared buffer, so lagging costs latency, never content).
const BROADCAST_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunDelta {
    /// Characters already produced before this delta.
    pub(crate) offset: usize,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunSignal {
    Delta(RunDelta),
    /// One recorded step of the provider's work. Carries its own `seq`, which
    /// is what lets a reattached reader drop an item it already has.
    Activity(super::activity::ActivityItem),
    Finished {
        status: TurnStatus,
        error_code: Option<String>,
    },
}

/// A run currently executing in this process.
pub(crate) struct LiveRun {
    pub(crate) org_id: i32,
    pub(crate) owner_id: i32,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    /// `(text, character count)` — the count is carried so appending stays O(1)
    /// in the length of the reply rather than rescanning it per token.
    buffer: Mutex<(String, usize)>,
    /// The turn's tool trace. Separate from the text so the reply is only ever
    /// the reply, and capped independently of it.
    activity: Mutex<super::activity::ActivityLog>,
    events: broadcast::Sender<RunSignal>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

/// The outcome of appending one runtime chunk to the shared buffer.
struct Appended {
    /// Characters produced before this chunk.
    offset: usize,
    /// The portion that fit under the stored cap — the ONLY portion that may be
    /// broadcast, so a live reader never sees text that replay would trim away.
    accepted: String,
    /// Total characters buffered after the append.
    total: usize,
    /// The chunk did not fit: the run must stop.
    overflowed: bool,
}

impl LiveRun {
    /// Append up to the stored cap. Trimming here — rather than after the fact —
    /// is what keeps the live stream and the persisted replay identical: a
    /// character that cannot be stored is never sent to anyone.
    fn append_capped(&self, text: &str) -> Appended {
        let mut guard = match self.buffer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let offset = guard.1;
        let remaining = MAX_RESPONSE_CHARS.saturating_sub(offset);
        let incoming = text.chars().count();
        let overflowed = incoming > remaining;
        let accepted: String = if overflowed {
            text.chars().take(remaining).collect()
        } else {
            text.to_owned()
        };
        guard.0.push_str(&accepted);
        guard.1 = offset + accepted.chars().count();
        Appended {
            offset,
            total: guard.1,
            accepted,
            overflowed,
        }
    }

    /// Everything produced so far, with its character count.
    pub(crate) fn snapshot(&self) -> (String, usize) {
        let guard = match self.buffer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        (guard.0.clone(), guard.1)
    }

    /// Record one step of the provider's work at the reply's current length.
    ///
    /// The offset is read under the same lock discipline as the text, so an
    /// item can never claim a position later than the text that existed when it
    /// happened. Returns `None` when a cap refused it.
    pub(crate) fn record_activity(
        &self,
        draft: super::activity::ActivityDraft,
    ) -> Option<super::activity::ActivityItem> {
        let offset = {
            let guard = match self.buffer.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.1
        };
        let mut log = match self.activity.lock() {
            Ok(log) => log,
            Err(poisoned) => poisoned.into_inner(),
        };
        log.push(draft, offset)
    }

    /// The trace so far, for a reader attaching mid-run.
    pub(crate) fn activity_snapshot(&self) -> Vec<super::activity::ActivityItem> {
        let log = match self.activity.lock() {
            Ok(log) => log,
            Err(poisoned) => poisoned.into_inner(),
        };
        log.items().to_vec()
    }

    /// The stored form of the trace, written with the turn's terminal state.
    pub(crate) fn activity_stored(&self) -> Option<String> {
        let log = match self.activity.lock() {
            Ok(log) => log,
            Err(poisoned) => poisoned.into_inner(),
        };
        log.stored()
    }

    /// Subscribe to this run's signals without taking a snapshot. Tests use it
    /// to observe exactly what a reader would be told.
    #[cfg(test)]
    pub(crate) fn subscribe_for_test(&self) -> broadcast::Receiver<RunSignal> {
        self.events.subscribe()
    }

    /// Interrupt the turn. Idempotent — a second cancel is a no-op, so a retry
    /// or a double click cannot fail.
    pub(crate) fn cancel(&self) {
        let sender = match self.cancel.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }
}

/// Live runs by run id. A run is registered before the driver starts and
/// removed only after its turn has been settled in the database, so a reattach
/// never sees a window in which the run is neither live nor final.
pub(crate) type RunRegistry = Arc<Mutex<HashMap<String, Arc<LiveRun>>>>;

pub(crate) fn new_registry() -> RunRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

fn lock(registry: &RunRegistry) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveRun>>> {
    match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn find(registry: &RunRegistry, run_id: &str) -> Option<Arc<LiveRun>> {
    lock(registry).get(run_id).cloned()
}

/// What an attaching reader needs: everything produced so far, and a stream of
/// everything after it.
pub(crate) struct Attachment {
    pub(crate) run: Arc<LiveRun>,
    pub(crate) snapshot: String,
    pub(crate) snapshot_chars: usize,
    /// The trace already recorded when this reader attached. Replayed before
    /// live signals; an item that appears in both is identified by `seq`.
    pub(crate) activity: Vec<super::activity::ActivityItem>,
    pub(crate) events: broadcast::Receiver<RunSignal>,
}

/// Attach to a live run. Subscribing *before* snapshotting is what closes the
/// gap: a delta appended after the subscription is delivered, and one already
/// folded into the snapshot is identified by its offset and dropped.
pub(crate) fn attach(registry: &RunRegistry, run_id: &str) -> Option<Attachment> {
    let run = find(registry, run_id)?;
    let events = run.events.subscribe();
    let (snapshot, snapshot_chars) = run.snapshot();
    let activity = run.activity_snapshot();
    Some(Attachment {
        run,
        snapshot,
        snapshot_chars,
        activity,
        events,
    })
}

/// What an explicitly team-invoked run must do with its reply when it finishes
/// (spec §5.3): publish it into the chat as an **agent-authored** message,
/// under the INVOKER's own authority.
///
/// Carrying the invoker's `OrgContext` (not a session token) is deliberate: the
/// publish happens after the run, possibly after the browser has gone, and it
/// re-proves the invoker's *current* write access to the chat rather than the
/// liveness of a cookie.
#[derive(Clone)]
pub(crate) struct TeamPublication {
    pub(crate) chat_id: String,
    pub(crate) invoker: crate::neev::types::OrgContext,
    pub(crate) agent_label: String,
    /// The turn this reply belongs to. It is the publication's identity, so a
    /// repeated settlement cannot post the same answer twice.
    pub(crate) turn_id: String,
}

/// Register a run and start its driver. The returned handle is only needed by
/// callers that want to observe the run synchronously (tests); ordinary request
/// handlers reattach by run id like any other reader.
///
/// `publish` is `None` for a private Agent Chat session — the ONLY way a reply
/// reaches team-visible storage is by being handed a publication here, so a
/// private session cannot leak by omission.
pub(crate) fn start(
    state: Arc<ServerState>,
    run_id: String,
    org_id: i32,
    owner_id: i32,
    session_id: String,
    turn_id: String,
    mut handle: RunHandle,
    publish: Option<TeamPublication>,
) -> Arc<LiveRun> {
    let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
    let live = Arc::new(LiveRun {
        org_id,
        owner_id,
        session_id,
        turn_id,
        buffer: Mutex::new((String::new(), 0)),
        activity: Mutex::new(super::activity::ActivityLog::new()),
        events,
        cancel: Mutex::new(handle.take_canceler()),
    });
    lock(&state.agent_runs).insert(run_id.clone(), live.clone());
    tokio::spawn(drive(state, run_id, live.clone(), handle, publish));
    live
}

/// Consume the runtime's stream: accumulate, broadcast, persist, settle.
async fn drive(
    state: Arc<ServerState>,
    run_id: String,
    live: Arc<LiveRun>,
    mut handle: RunHandle,
    publish: Option<TeamPublication>,
) {
    let database = state.database.clone();
    let mut flushed = 0usize;
    // `None` until the run reaches a terminal state; a stream that simply ends
    // without one is an interruption, not a success.
    let mut settled: Option<(TurnStatus, Option<&'static str>)> = None;
    while let Some(event) = handle.next().await {
        match event {
            // The runtime's own id is internal; the durable identity is ours.
            RunEvent::Started(_) => {}
            RunEvent::Activity(draft) => {
                // Refused only when a cap is spent, in which case the log has
                // already recorded that it truncated.
                if let Some(item) = live.record_activity(draft) {
                    let _ = live.events.send(RunSignal::Activity(item));
                }
            }
            RunEvent::Chunk(text) => {
                if text.is_empty() {
                    continue;
                }
                let appended = live.append_capped(&text);
                let total = appended.total;
                if !appended.accepted.is_empty() {
                    let _ = live.events.send(RunSignal::Delta(RunDelta {
                        offset: appended.offset,
                        text: appended.accepted,
                    }));
                }
                if appended.overflowed {
                    // The reply outgrew what a turn can store. Stop the run
                    // rather than silently dropping the tail, and say so. The
                    // over-limit characters were never broadcast, so the live
                    // view already matches what replay will show.
                    live.cancel();
                    settled = Some((TurnStatus::Failed, Some("too_large")));
                    break;
                }
                if total - flushed >= FLUSH_CHARS {
                    let (snapshot, chars) = live.snapshot();
                    let now = state.clock.now_unix();
                    if sessions::persist_response(&database, &run_id, &snapshot, now)
                        .await
                        .is_ok()
                    {
                        flushed = chars;
                    }
                }
            }
            RunEvent::Completed => {
                settled = Some((TurnStatus::Completed, None));
                break;
            }
            RunEvent::Failed(reason) => {
                let (status, code) = sessions::failure_code(&reason);
                settled = Some((status, Some(code)));
                break;
            }
        }
    }
    let (mut status, mut error_code) = settled.unwrap_or((TurnStatus::Failed, Some("interrupted")));
    // The buffer is capped on append, so it is always storable as-is.
    let response = live.snapshot().0;

    // An explicitly team-invoked run publishes BEFORE it settles: the
    // team-visible message is the product, so a turn is only "completed" once
    // the team can actually see the reply. A publish that fails (the invoker
    // lost write access, the chat was archived, the org hit its quota) turns
    // the run into an honest failure and posts nothing.
    if let (Some(publication), TurnStatus::Completed) = (&publish, status) {
        match publish_reply(&state, publication, &response).await {
            Ok(()) => {}
            Err(code) => {
                status = TurnStatus::Failed;
                error_code = Some(code);
            }
        }
    }

    // Settle the durable row BEFORE leaving the registry, so a reader can never
    // observe a run that is neither live nor final.
    let trace = live.activity_stored();
    let stored = persist_terminal(
        &state,
        &database,
        &run_id,
        &response,
        trace.as_deref(),
        status,
        error_code,
    )
    .await;
    // A success is only reported once it is DURABLE. If the write could not be
    // made, saying "completed" would promise a replay that does not exist, so
    // the reader is told the truth instead; the row stays `running` and
    // `sessions::reconcile_interrupted` settles it at the next startup.
    let (reported_status, reported_code) = if stored {
        (status, error_code)
    } else {
        // The row is still `running`, which would block every future turn in
        // this session (`begin_turn`'s live-turn guard) until the process
        // restarts. Keep trying in the background so a transient outage costs a
        // failed turn, not a wedged session.
        tokio::spawn(reconcile_stranded(
            state.clone(),
            run_id.clone(),
            response.clone(),
        ));
        (TurnStatus::Failed, Some("unavailable"))
    };
    let _ = live.events.send(RunSignal::Finished {
        status: reported_status,
        error_code: reported_code.map(str::to_owned),
    });
    lock(&state.agent_runs).remove(&run_id);
}

/// The largest reply that fits in a team message (`neev_messages.byte_length`).
/// A run may produce far more than a chat message can hold, so the two limits
/// are reconciled here rather than by silently cutting the text.
const MAX_TEAM_MESSAGE_BYTES: usize = 32_768;

/// Publish a completed team-invoked reply, mapping any refusal to the durable
/// error code the turn will carry.
async fn publish_reply(
    state: &ServerState,
    publication: &TeamPublication,
    response: &str,
) -> Result<(), &'static str> {
    use crate::neev::content::types::ContentError;
    if response.trim().is_empty() {
        // Nothing was produced; posting an empty agent message would be noise.
        return Err("provider");
    }
    if response.len() > MAX_TEAM_MESSAGE_BYTES {
        // Truncating would publish something the agent did not say.
        return Err("too_large");
    }
    match crate::neev::content::messages::publish_agent_reply(
        state,
        &publication.invoker,
        &publication.chat_id,
        response,
        &publication.agent_label,
        &publication.turn_id,
    )
    .await
    {
        Ok(_) => Ok(()),
        // The invoker can no longer write here (removed from the org, chat made
        // private to someone else, chat deleted): publish nothing, say so.
        Err(ContentError::NotFound) | Err(ContentError::Auth(_)) => Err("unauthorized"),
        Err(ContentError::Quota) => Err("unavailable"),
        Err(ContentError::Archived) => Err("unavailable"),
        Err(_) => Err("unavailable"),
    }
}

/// Attempts to write a run's terminal state before giving up.
const FINISH_ATTEMPTS: u32 = 5;
const FINISH_BACKOFF_MS: u64 = 50;
/// Background settlement after the fast path failed: keeps trying for roughly
/// ten minutes, which comfortably outlasts a transient database outage while
/// staying bounded.
const STRANDED_ATTEMPTS: u32 = 400;
const STRANDED_MAX_DELAY_MS: u64 = 2_000;

/// Settle a run whose terminal write could not be made while the driver was
/// still holding it.
///
/// Without this, a `running` row outlives its run: `begin_turn` refuses every
/// further turn in that session (one live turn at a time), `cancel_run` cannot
/// clear it (the run has left the registry), and each reattach polls for ten
/// minutes before reporting a failure — so a one-second database blip would
/// make one tenant's session unusable until an operator restarted the server.
///
/// `sessions::finish` only moves rows still marked `running`, so this can never
/// rewrite a turn that settled some other way, and it never re-runs anything:
/// the provider work is long over, only its bookkeeping is outstanding.
async fn reconcile_stranded(state: Arc<ServerState>, run_id: String, response: String) {
    let mut delay = FINISH_BACKOFF_MS;
    for _ in 0..STRANDED_ATTEMPTS {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        delay = delay.saturating_mul(2).min(STRANDED_MAX_DELAY_MS);
        let now = state.clock.now_unix();
        if sessions::finish(
            &state.database,
            &run_id,
            &response,
            // The live log died with the failed write; leave whatever the row
            // already holds rather than overwriting a trace with nothing.
            None,
            TurnStatus::Failed,
            // The provider may well have completed; what failed is our ability
            // to record it. `unavailable` is the honest label for that, and it
            // matches what the reader was already told.
            Some("unavailable"),
            now,
        )
        .await
        .is_ok()
        {
            return;
        }
    }
    // Still unreachable after the whole window: startup reconciliation remains
    // the backstop, exactly as for a crashed process.
}

/// Write the turn's terminal state, retrying a transient storage failure a
/// bounded number of times. Returns whether the state is durable.
///
/// `sessions::finish` only moves a row that is still `running`, so a retry
/// after a partially-observed failure cannot rewrite an already-settled turn.
async fn persist_terminal(
    state: &ServerState,
    database: &sea_orm::DatabaseConnection,
    run_id: &str,
    response: &str,
    activity: Option<&str>,
    status: TurnStatus,
    error_code: Option<&str>,
) -> bool {
    let mut delay = FINISH_BACKOFF_MS;
    for attempt in 0..FINISH_ATTEMPTS {
        let now = state.clock.now_unix();
        if sessions::finish(database, run_id, response, activity, status, error_code, now)
            .await
            .is_ok()
        {
            return true;
        }
        if attempt + 1 < FINISH_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            delay = delay.saturating_mul(2);
        }
    }
    false
}

/// Record a refusal that happened before any stream existed (the runtime is
/// disabled, both isolation lanes are busy, the launch failed). The turn is
/// already durable, so the user sees the prompt and an honest reason instead of
/// a request that vanished.
pub(crate) async fn settle_refusal(
    state: &ServerState,
    run_id: &str,
    code: &str,
) -> Result<(), crate::neev::content::types::ContentError> {
    let now = state.clock.now_unix();
    sessions::finish(
        &state.database,
        run_id,
        "",
        // A refusal happened before any provider stream existed.
        None,
        TurnStatus::Failed,
        Some(code),
        now,
    )
    .await
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod runs_tests;
