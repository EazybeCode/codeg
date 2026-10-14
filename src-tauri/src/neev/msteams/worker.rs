//! The Teams worker: carries every accepted message from the durable inbox to
//! exactly one run, enqueues each settled run's final answer, and delivers the
//! outbox — re-authorizing at each step, and resuming after a restart from
//! what is stored (nothing lives only in memory).
//!
//! One [`tick`] does three passes:
//! 1. **Launch** due inbox items. The member is re-proved on BOTH sides first
//!    (binding enabled, principal link unchanged, Teams membership and channel
//!    audience via Graph, an ordinary delegated Neev org session), then the turn
//!    is begun through the web chat's own launcher with a request id derived
//!    from the activity: a launch repeated after a crash replays the same run.
//!    Channel runs are launched with the PUBLISHED briefing audience.
//! 2. **Settle** launched items whose run finished: enqueue the final answer
//!    and pass the thread's claim to the next waiting message, atomically.
//! 3. **Deliver** due outbox rows: re-check the destination (binding enabled,
//!    channel audience, the executor's Teams and Neev membership for a final
//!    answer), send, record Teams' message id, and retry 429/5xx/timeouts with
//!    backoff. A delivery retry never re-runs the agent.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::TransactionTrait;
use serde_json::Value;

use super::access::{self, ChannelAccess};
use super::activity;
use super::bot::{Connector, ConnectorError};
use super::cards;
use super::codes::Refusal;
use super::directory::Directory;
use super::graph::Membership;
use super::inbox::{self, Item};
use super::outbox::{self, Delivery, Kind};
use super::threads;
use super::TeamsRuntime;
use crate::neev::agents::sessions::{self, SessionInput, TurnStatus};
use crate::neev::content::types::ContentError;
use crate::neev::error::{database_error, AuthError};
use crate::neev::http::{launch_session_turn, ApiError, LaunchAudience};
use crate::neev::startup::ServerState;
use crate::neev::types::OrgContext;

/// Launch attempts (transient failures only) before a message is refused.
pub(crate) const MAX_LAUNCH_ATTEMPTS: i64 = 8;
/// The delegated session a launched turn runs under. The run driver rechecks it
/// when it stores generated files at completion (`resources::begin`), and runs
/// have no wall-clock cap, so it gets the same horizon as a browser org session
/// (`auth::session::ORGANIZATION_TTL`) rather than one a long run would outlive.
/// Its token is discarded, so the length widens nothing: a membership, role or
/// account change still refuses it on the next recheck.
pub(crate) const RUN_DELEGATED_TTL: i64 = 12 * 60 * 60;
const BATCH: u64 = 16;
/// How often the worker looks without being woken.
const IDLE_TICK: Duration = Duration::from_secs(3);

/// What one tick did (for logs and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) launched: u32,
    pub(crate) refused: u32,
    pub(crate) deferred: u32,
    pub(crate) settled: u32,
    pub(crate) sent: u32,
    pub(crate) withheld: u32,
    pub(crate) failed: u32,
    pub(crate) retrying: u32,
    pub(crate) documents_published: u32,
    pub(crate) documents_withheld: u32,
    pub(crate) documents_failed: u32,
}

/// Run the worker for the life of the process. Woken by the message handler
/// after each commit; otherwise looks every few seconds.
pub(crate) fn spawn(state: Arc<ServerState>, runtime: Arc<TeamsRuntime>) {
    tokio::spawn(async move {
        loop {
            let _ = tokio::time::timeout(IDLE_TICK, runtime.wake.notified()).await;
            let report = tick(&state, &runtime).await;
            if report != Report::default() {
                tracing::info!(?report, "teams worker tick");
            }
        }
    });
}

pub(crate) async fn tick(state: &Arc<ServerState>, runtime: &TeamsRuntime) -> Report {
    let directory = &runtime.directory;
    let connector = &runtime.connector;
    let mut report = Report::default();
    let now = state.clock.now_unix();
    match inbox::claim_due(&state.database, now, BATCH).await {
        Ok(items) => {
            for item in items {
                if let Err(error) = launch_one(state, directory, &item, &mut report).await {
                    tracing::warn!(%error, "teams launch bookkeeping failed; the lease expires and it is retried");
                }
            }
        }
        Err(error) => tracing::warn!(%error, "teams inbox unavailable"),
    }
    match inbox::settled_runs(&state.database, BATCH * 4).await {
        Ok(items) => {
            for item in items {
                match settle_one(state, runtime.files.is_some(), &item).await {
                    Ok(true) => report.settled += 1,
                    Ok(false) => {}
                    Err(error) => tracing::warn!(%error, "teams settle failed; retried next tick"),
                }
            }
        }
        Err(error) => tracing::warn!(%error, "teams settle scan failed"),
    }
    if let Some(files) = &runtime.files {
        let now = state.clock.now_unix();
        match super::documents::claim_due(&state.database, now, BATCH).await {
            Ok(documents) => {
                for document in documents {
                    if let Err(error) = publish_document(state, directory, files, &document, &mut report).await {
                        tracing::warn!(%error, "teams document bookkeeping failed; the lease expires and it is retried");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "teams documents unavailable"),
        }
    }
    let now = state.clock.now_unix();
    match outbox::claim_due(&state.database, now, BATCH).await {
        Ok(deliveries) => {
            for delivery in deliveries {
                if let Err(error) = deliver_one(state, directory, connector, &delivery, &mut report).await {
                    tracing::warn!(%error, "teams delivery bookkeeping failed; the lease expires and it is retried");
                }
            }
        }
        Err(error) => tracing::warn!(%error, "teams outbox unavailable"),
    }
    report
}

// ---------------------------------------------------------------- launch

enum Launch {
    Launched { run_id: String, ack: Option<Value> },
    /// Refused with a reason the person is told.
    Refused(Refusal),
    /// Refused without a message (a suspended binding sends nothing).
    Silent(Refusal),
    /// A transient failure: try again later.
    Retry,
}

/// What a person is told for a refusal from the shared Neev stack.
pub(in crate::neev) fn refusal_of(error: ApiError) -> Refusal {
    match error {
        ApiError::Conflict => Refusal::Busy,
        ApiError::Limited | ApiError::ContentQuota => Refusal::Limited,
        ApiError::Unauthorized | ApiError::Forbidden => Refusal::NotAMember,
        ApiError::NotFound | ApiError::Archived | ApiError::Invalid => Refusal::SessionUnavailable,
        _ => Refusal::Unavailable,
    }
}

/// The linked member as an ordinary, fully re-checked org session.
pub(crate) async fn delegated(state: &ServerState, org_id: i32, user_id: i32, ttl: i64) -> Result<OrgContext, Refusal> {
    let now = state.clock.now_unix();
    crate::neev::auth::session::issue_delegated(&state.database, user_id, org_id, now, ttl)
        .await
        .map_err(|error| match error {
            AuthError::Unauthorized | AuthError::Forbidden | AuthError::Invalid => Refusal::NotAMember,
            AuthError::Limited => Refusal::Limited,
            _ => Refusal::Unavailable,
        })
}

/// Transient refusals are retried; the rest are final.
fn transient(reason: Refusal) -> bool {
    matches!(reason, Refusal::Unavailable | Refusal::Limited | Refusal::Busy)
}

async fn launch_one(state: &Arc<ServerState>, directory: &Directory, item: &Item, report: &mut Report) -> Result<(), ContentError> {
    let outcome = match authorize_and_launch(state, directory, item).await {
        Err(reason) if transient(reason) => Launch::Retry,
        Err(Refusal::Suspended) => Launch::Silent(Refusal::Suspended),
        Err(reason) => Launch::Refused(reason),
        Ok(launched) => launched,
    };
    let now = state.clock.now_unix();
    let outcome = match outcome {
        Launch::Retry if item.attempts >= MAX_LAUNCH_ATTEMPTS => Launch::Refused(Refusal::Unavailable),
        Launch::Retry => {
            inbox::retry_at(&state.database, item, now + outbox::backoff(item.attempts, None), now).await?;
            report.deferred += 1;
            return Ok(());
        }
        other => other,
    };
    let tx = state.database.begin().await.map_err(database_error)?;
    match &outcome {
        Launch::Launched { run_id, ack } => {
            inbox::launched(&tx, item, run_id, now).await?;
            if let Some(origin) = &item.channel {
                threads::started(&tx, &origin.key, origin.sequence, run_id, item.user_id).await?;
            }
            if let Some(card) = ack {
                outbox::enqueue(&tx, &outbox::id(run_id, "ack"), item, Kind::Notice, None, Some(card), now).await?;
            }
            report.launched += 1;
        }
        Launch::Refused(reason) | Launch::Silent(reason) => {
            inbox::refuse(&tx, item, reason.code(), now).await?;
            if let Some(origin) = &item.channel {
                threads::refused(&tx, &origin.key, origin.sequence, now).await?;
            }
            if let Launch::Refused(reason) = &outcome {
                let cause = activity::digest(&[&item.tenant_id, &item.conversation_id, &item.activity_id]);
                outbox::enqueue(&tx, &outbox::id(&cause, "refusal"), item, Kind::Notice, None, Some(&cards::refusal(*reason)), now).await?;
            }
            tracing::info!(code = reason.code(), tenant = %item.tenant_id, "teams request refused at launch");
            report.refused += 1;
        }
        Launch::Retry => unreachable!("handled above"),
    }
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

/// The request id of the turn an inbox item launches: derived from the Bot
/// Connector activity, so a repeated launch replays instead of running twice.
pub(crate) fn turn_request_id(item: &Item) -> String {
    activity::request_uuid(&activity::digest(&[
        "msteams-activity-v1",
        &item.tenant_id,
        &item.conversation_id,
        &item.activity_id,
    ]))
}

/// A stable request id for "this member's Neev session for this place".
fn session_request_id(parts: &[&str]) -> String {
    activity::request_uuid(&activity::digest(parts))
}

async fn authorize_and_launch(state: &Arc<ServerState>, directory: &Directory, item: &Item) -> Result<Launch, Refusal> {
    let tenant = access::tenant(&state.database, &item.tenant_id).await?;
    if tenant.org_id != item.org_id {
        return Err(Refusal::UnmappedTenant);
    }
    // The Teams identity must still be linked to the SAME Neev member.
    let user_id = access::principal(&state.database, &item.tenant_id, tenant.org_id, Some(&item.aad_object_id)).await?;
    if user_id != item.user_id {
        return Err(Refusal::UnmappedUser);
    }
    if !state.agents.is_available() {
        return Err(Refusal::RuntimeUnavailable);
    }
    let channel = match &item.channel {
        Some(origin) => {
            let access = access::channel(&state.database, &item.tenant_id, Some(&origin.team_id), &origin.key.channel_id).await?;
            if access.tenant.org_id != item.org_id {
                return Err(Refusal::UnmappedChannel);
            }
            access::teams_member(directory, &access, Some(&item.aad_object_id), false).await?;
            Some(access)
        }
        None => None,
    };
    let actor = delegated(state, tenant.org_id, user_id, RUN_DELEGATED_TTL).await?;
    let request_id = turn_request_id(item);
    let (session_id, session_title, prompt, audience) = match (&item.channel, &channel) {
        (Some(origin), Some(access)) => {
            let session = thread_session(state, &actor, access, &origin.key, &request_id).await?;
            let context = threads::context(&state.database, &origin.key, origin.sequence)
                .await
                .map_err(|_| Refusal::Unavailable)?;
            let prompt = threads::compose(&context, &item.author_name, &item.body);
            (session.0, session.1, prompt, LaunchAudience::Published)
        }
        _ => {
            let user = user_id.to_string();
            let session = sessions::create(
                state,
                &actor,
                SessionInput {
                    title: "Teams · chat".to_owned(),
                    provider: tenant.provider.clone(),
                    model: tenant.model.clone(),
                    bound_chat_id: None,
                    node_id: None,
                    effort: None,
                    fast: false,
                    request_id: session_request_id(&[
                        "msteams-session-v2",
                        &item.tenant_id,
                        &item.conversation_id,
                        &user,
                        &tenant.provider,
                        &tenant.model,
                    ]),
                },
            )
            .await
            .map_err(|error| refusal_of(error.into()))?;
            (session.id, session.title, item.body.clone(), LaunchAudience::Owner)
        }
    };
    let launched = launch_session_turn(state, &actor, &session_id, prompt, Vec::new(), true, None, Some(&request_id), audience)
        .await
        .map_err(refusal_of)?;
    let run_id = launched.turn.run_id.clone();
    // A turn that settled at once (refused by the runtime) gets only its final
    // answer; announcing "working on it" would be untrue.
    let ack = (launched.turn.status == TurnStatus::Running).then(|| match &item.channel {
        Some(_) => cards::thread_started(&item.author_name, &run_id),
        None => cards::started(&session_title, &run_id),
    });
    Ok(Launch::Launched { run_id, ack })
}

/// The member's OWN Neev session for this thread, filed under the linked
/// project (`sessions::create` applies their folder access; the run uses their
/// own connection). It must hold ONLY channel-published turns: if the member
/// also chatted in it on the web (with a private briefing), a fresh session is
/// used instead, so nothing private can be replayed into the channel.
async fn thread_session(
    state: &Arc<ServerState>,
    actor: &OrgContext,
    access: &ChannelAccess,
    key: &threads::ThreadKey,
    request_id: &str,
) -> Result<(String, String), Refusal> {
    let user = actor.identity.user_id.to_string();
    let node = access.node_id.to_string();
    let mut generation = String::new();
    // Bounded: each fork is a new, empty session; one fork always suffices
    // unless the member writes into it on the web between these steps.
    for _ in 0..4 {
        let session = sessions::create(
            state,
            actor,
            SessionInput {
                title: "Teams · channel thread".to_owned(),
                provider: access.tenant.provider.clone(),
                model: access.tenant.model.clone(),
                bound_chat_id: None,
                node_id: Some(access.node_id),
                effort: None,
                fast: false,
                request_id: session_request_id(&[
                    "msteams-thread-session-v2",
                    &key.tenant_id,
                    &key.channel_id,
                    &key.root_id,
                    &user,
                    &node,
                    &access.tenant.provider,
                    &access.tenant.model,
                    &generation,
                ]),
            },
        )
        .await
        .map_err(|error| refusal_of(error.into()))?;
        let foreign = inbox::foreign_turns(&state.database, actor.org_id, &session.id, request_id)
            .await
            .map_err(|_| Refusal::Unavailable)?;
        if foreign == 0 {
            return Ok((session.id, session.title));
        }
        generation = format!("fork:{}:{foreign}", session.id);
    }
    Err(Refusal::SessionUnavailable)
}

// ---------------------------------------------------------------- settle

async fn settle_one(state: &Arc<ServerState>, files_authorized: bool, item: &Item) -> Result<bool, ContentError> {
    let Some(run_id) = item.run_id.as_deref() else {
        return Ok(false);
    };
    let turn = sessions::turn_by_run_id(&state.database, run_id).await?;
    let now = state.clock.now_unix();
    let tx = state.database.begin().await.map_err(database_error)?;
    if !inbox::settle(&tx, item, now).await? {
        tx.commit().await.map_err(database_error)?;
        return Ok(false);
    }
    outbox::enqueue(&tx, &outbox::id(run_id, "final"), item, Kind::Final, Some(run_id), None, now).await?;
    if let Some(origin) = &item.channel {
        // The generated files of a completed channel run, for publication
        // into the channel's Files — only when that is authorized.
        if let Some(turn) = turn.as_ref().filter(|turn| files_authorized && turn.status == TurnStatus::Completed) {
            super::documents::enqueue_for_run(&tx, item, run_id, &turn.session_id, turn.sequence, now).await?;
        }
        threads::release(&tx, &origin.key, origin.sequence, now).await?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(true)
}

/// Publish one generated file, and link it in the thread when it lands.
async fn publish_document(
    state: &Arc<ServerState>,
    directory: &Directory,
    files: &super::documents::FilesAuthority,
    document: &super::documents::Document,
    report: &mut Report,
) -> Result<(), ContentError> {
    use super::documents::Published;
    let outcome = super::documents::publish_one(state, directory, files, document).await?;
    let now = state.clock.now_unix();
    let name = super::documents::destination_name(&document.file_name, &document.attachment_key[..8]);
    let card = match &outcome {
        Published::Done { web_url } => {
            report.documents_published += 1;
            Some(cards::document(&name, web_url))
        }
        _ => None,
    };
    let ended = super::documents::record(&state.database, document, &outcome, now).await?;
    match (&outcome, ended) {
        (Published::Withheld(_), _) => report.documents_withheld += 1,
        (_, Some(code)) => {
            report.documents_failed += 1;
            tracing::warn!(code, tenant = %document.tenant_id, "teams document not published");
        }
        _ => {}
    }
    // The thread was told the file would be linked: say where it landed, or
    // that it stays in Neev. A WITHHELD file's name is not posted either (it
    // came from a run the channel may not see). Delivery rechecks the
    // destination again.
    let failed = ended.is_some() && !matches!(outcome, Published::Withheld(_));
    let card = card.or_else(|| failed.then(|| cards::document_not_saved(&name)));
    if let Some(card) = card {
        if let Some(item) = inbox::by_run(&state.database, document.org_id, &document.run_id).await? {
            outbox::enqueue(&state.database, &outbox::id(&document.id, "document"), &item, Kind::Notice, None, Some(&card), now).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- deliver

enum Destination {
    Open,
    /// Never send (binding suspended, audience ineligible, access revoked).
    Withhold(Refusal),
    /// Could not decide now; try again.
    Retry,
}

fn destination_of(reason: Refusal) -> Destination {
    if reason == Refusal::Unavailable {
        Destination::Retry
    } else {
        Destination::Withhold(reason)
    }
}

async fn destination(state: &Arc<ServerState>, directory: &Directory, delivery: &Delivery) -> Destination {
    let tenant = match access::tenant(&state.database, &delivery.tenant_id).await {
        Ok(tenant) if tenant.org_id == delivery.org_id => tenant,
        Ok(_) => return Destination::Withhold(Refusal::UnmappedTenant),
        Err(reason) => return destination_of(reason),
    };
    // A final answer goes out only while the person it ran as still has BOTH
    // Neev membership and (in a channel) Teams membership: revocation stops
    // result delivery, not just new runs.
    let executor_object = if delivery.kind == Kind::Final {
        match crate::neev::auth::session::member_active(&state.database, delivery.user_id, tenant.org_id).await {
            Ok(()) => {}
            Err(AuthError::Unavailable) => return Destination::Retry,
            Err(_) => return Destination::Withhold(Refusal::NotAMember),
        }
        match principal_of(state, &delivery.tenant_id, tenant.org_id, delivery.user_id).await {
            Ok(Some(object)) => Some(object),
            Ok(None) => return Destination::Withhold(Refusal::UnmappedUser),
            Err(_) => return Destination::Retry,
        }
    } else {
        None
    };
    let Some((channel_id, _)) = &delivery.channel else {
        return Destination::Open;
    };
    let access = match access::channel(&state.database, &delivery.tenant_id, None, channel_id).await {
        Ok(access) if access.tenant.org_id == delivery.org_id => access,
        Ok(_) => return Destination::Withhold(Refusal::UnmappedChannel),
        Err(reason) => return destination_of(reason),
    };
    if let Some(object) = executor_object {
        return match access::teams_member(directory, &access, Some(&object), true).await {
            Ok(Membership::Owner | Membership::Member) => Destination::Open,
            Ok(_) => Destination::Withhold(Refusal::NotATeamMember),
            Err(reason) => destination_of(reason),
        };
    }
    match access::channel_audience(directory, &access, true).await {
        Ok(()) => Destination::Open,
        Err(reason) => destination_of(reason),
    }
}

async fn principal_of(state: &ServerState, tenant_id: &str, org_id: i32, user_id: i32) -> Result<Option<String>, ContentError> {
    crate::neev::db::one(
        &state.database,
        "SELECT aad_object_id FROM neev_msteams_principals WHERE tenant_id=$1 AND org_id=$2 AND user_id=$3",
        vec![tenant_id.into(), org_id.into(), user_id.into()],
    )
    .await?
    .map(|row| row.try_get::<String>("", "aad_object_id").map_err(|error| database_error(error).into()))
    .transpose()
}

/// What one delivery posts.
struct Composed {
    card: Value,
    /// The answer text to record in the thread ledger once posted (a
    /// completed, publishable channel answer only).
    answer: Option<String>,
    /// Why the answer content was withheld from the channel, if it was.
    withheld: Option<&'static str>,
}

/// The card a delivery posts. A channel answer is posted only when its run's
/// complete tool trace shows no source-reading tool (`sources::classify`);
/// otherwise the thread is told it was kept private and why.
async fn compose(state: &Arc<ServerState>, delivery: &Delivery) -> Result<Option<Composed>, ContentError> {
    match delivery.kind {
        Kind::Notice => Ok(delivery.card.clone().map(|card| Composed {
            card,
            answer: None,
            withheld: None,
        })),
        Kind::Final => {
            let Some(run_id) = delivery.run_id.as_deref() else {
                return Ok(None);
            };
            let Some(turn) = sessions::turn_by_run_id(&state.database, run_id).await? else {
                return Ok(None);
            };
            if delivery.channel.is_none() {
                let card = cards::final_answer(turn.status.name(), &turn.response, turn.error_code.as_deref(), None);
                return Ok(Some(Composed { card, answer: None, withheld: None }));
            }
            let produced = if turn.status == TurnStatus::Completed {
                super::documents::outputs_of(&state.database, delivery.org_id, &turn.session_id, turn.sequence).await?
            } else {
                0
            };
            let queued: i64 = crate::neev::db::one(&state.database, "SELECT COUNT(*) AS n FROM neev_msteams_documents WHERE org_id=$1 AND run_id=$2", vec![delivery.org_id.into(), run_id.into()])
                .await?
                .map(|row| row.try_get::<i64>("", "n").map_err(|error| ContentError::from(database_error(error))))
                .transpose()?
                .unwrap_or(0);
            let files_note = match (produced, queued) {
                (0, _) => None,
                (n, q) if q == n => Some(format!("{n} file(s) will be saved to this channel's Files and linked here.")),
                (n, _) => Some(format!("{n} file(s) were produced. They stay in the requester's Neev chat: saving to Teams files is not enabled here.")),
            };
            let requester = inbox::by_run(&state.database, delivery.org_id, run_id)
                .await?
                .map(|item| item.author_name)
                .unwrap_or_default();
            match super::sources::classify(&turn.activity) {
                super::sources::Sources::SourceFree => {
                    let card = cards::final_with_files(turn.status.name(), &turn.response, turn.error_code.as_deref(), Some(&requester), files_note.as_deref());
                    let answer = (turn.status == TurnStatus::Completed && !turn.response.trim().is_empty()).then(|| turn.response.clone());
                    Ok(Some(Composed { card, answer, withheld: None }))
                }
                super::sources::Sources::Unclassified(code) => Ok(Some(Composed {
                    card: cards::withheld(&requester),
                    answer: None,
                    withheld: Some(code),
                })),
            }
        }
    }
}

async fn deliver_one(
    state: &Arc<ServerState>,
    directory: &Directory,
    connector: &Connector,
    delivery: &Delivery,
    report: &mut Report,
) -> Result<(), ContentError> {
    let now = state.clock.now_unix();
    let retry = |reason: &'static str, after: Option<u64>| {
        let failed = delivery.attempts >= outbox::MAX_ATTEMPTS;
        (if failed { "failed" } else { "pending" }, now + outbox::backoff(delivery.attempts, after), reason)
    };
    match destination(state, directory, delivery).await {
        Destination::Open => {}
        Destination::Withhold(reason) => {
            outbox::record(&state.database, &delivery.id, "withheld", now, None, Some(reason.code()), now).await?;
            report.withheld += 1;
            return Ok(());
        }
        Destination::Retry => {
            let (next, at, code) = retry("destination_unverified", None);
            outbox::record(&state.database, &delivery.id, next, at, None, Some(code), now).await?;
            if next == "failed" { report.failed += 1 } else { report.retrying += 1 }
            return Ok(());
        }
    }
    let Some(Composed { card, answer, withheld }) = compose(state, delivery).await? else {
        outbox::record(&state.database, &delivery.id, "failed", now, None, Some("nothing_to_send"), now).await?;
        report.failed += 1;
        return Ok(());
    };
    let message = cards::message(card, Some(&delivery.reply_to_id));
    let sent = connector
        .reply(&delivery.service_url, &delivery.conversation_id, &delivery.reply_to_id, &message)
        .await;
    let now = state.clock.now_unix();
    match sent {
        Ok(message_id) => {
            // A withheld answer is still a sent message (the notice), with the
            // reason the content stayed private recorded for operators.
            outbox::record(&state.database, &delivery.id, "sent", now, message_id.as_deref(), withheld, now).await?;
            report.sent += 1;
            if let (Some(answer), Some((channel_id, root_id)), Some(run_id)) = (answer, &delivery.channel, &delivery.run_id) {
                let key = threads::ThreadKey {
                    tenant_id: delivery.tenant_id.clone(),
                    channel_id: channel_id.clone(),
                    root_id: root_id.clone(),
                };
                // Posted, so it is now part of what the thread said.
                threads::record_answer(&state.database, &key, delivery.org_id, &delivery.conversation_id, run_id, &answer, now).await?;
            }
        }
        Err(error) => {
            let (next, at, code) = match error {
                ConnectorError::Retry { retry_after_secs, .. } => retry("throttled_or_server_error", retry_after_secs),
                ConnectorError::Unreachable | ConnectorError::Token => retry("unreachable", None),
                // Possibly posted: at-least-once rather than a lost answer.
                ConnectorError::Uncertain => retry("uncertain", None),
                ConnectorError::Refused(_) | ConnectorError::ServiceUrl | ConnectorError::Invalid => ("failed", now, "refused"),
            };
            tracing::warn!(%error, code, tenant = %delivery.tenant_id, "teams delivery not completed");
            outbox::record(&state.database, &delivery.id, next, at, None, Some(code), now).await?;
            if next == "failed" { report.failed += 1 } else { report.retrying += 1 }
        }
    }
    Ok(())
}
