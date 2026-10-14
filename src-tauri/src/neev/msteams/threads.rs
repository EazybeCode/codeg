//! Shared channel threads (schema 25): one Teams channel root message is one
//! shared thread, whoever replies in it.
//!
//! `ThreadKey = (tenant, channel, root message id)`. The sender is NOT part of
//! the key: two members mentioning Neev under the same root share one ledger,
//! and two roots in one channel never share anything.
//!
//! Each accepted message is committed in ONE transaction, before the HTTP
//! acknowledgement, to both the ledger (`neev_msteams_thread_messages`, the
//! thread's shared context) and the durable inbox (`neev_msteams_inbox`, the
//! work the worker carries to a run). Both are keyed by the Bot Connector's
//! `(tenant, conversation, activity)`: a retried activity is a
//! [`Admission::Replay`] and never runs twice.
//!
//! Execution identity stays per member. Each run executes in the SUBMITTING
//! member's own Neev session with their own connection; the thread contributes
//! only text that was said in the thread and answers Neev actually POSTED there
//! (never tool traces), so no credential or private context crosses from one
//! member's run into another's. `author_user_id` (who said it) and
//! `executed_as` (whose session ran it) are stored separately.
//!
//! One live run per thread, across all participants, claimed in the admitting
//! transaction (`live_sequence`) before the run exists. A message arriving
//! while the thread is busy (or behind other waiting messages) is `queued`,
//! labelled honestly — it did not change the running request — and runs, as
//! its own author, when the claim is released (first in, first out).

use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, QueryResult, TransactionTrait};

use super::activity::{Envelope, Scope};
use crate::neev::content::types::ContentError;
use crate::neev::db;
use crate::neev::error::database_error;

/// Ledger entries replayed as context, newest kept.
const CONTEXT_ENTRIES: u64 = 24;
/// Characters of thread context composed into one run's prompt. With the
/// request itself (≤ 8000) this stays well under the session prompt cap.
const CONTEXT_CHARS: usize = 16_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ThreadKey {
    pub(crate) tenant_id: String,
    pub(crate) channel_id: String,
    pub(crate) root_id: String,
}

/// The thread a channel activity belongs to. A channel thread's conversation id
/// is `<channel id>;messageid=<root message id>` (for the root post itself the
/// root id is its own activity id). Anything else is not a channel thread.
pub(crate) fn thread_key(envelope: &Envelope) -> Option<ThreadKey> {
    let Scope::Channel { channel_id, .. } = &envelope.scope else {
        return None;
    };
    let (conversation_channel, root) = envelope.conversation_id.split_once(";messageid=")?;
    if conversation_channel != channel_id
        || root.is_empty()
        || root.len() > 256
        || !root.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(ThreadKey {
        tenant_id: envelope.tenant_id.clone(),
        channel_id: channel_id.clone(),
        root_id: root.to_owned(),
    })
}

/// What happened to one inbound message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Recorded and holding the thread's live claim: due to launch.
    Start { sequence: i64 },
    /// Recorded behind a live run (or other waiting messages): it runs, as
    /// its author, after them.
    Queued { sequence: i64 },
    /// Already recorded by an earlier delivery of the same activity.
    Replay,
}

/// One inbound message, already attributed to a member who passed BOTH the
/// Teams and the Neev checks.
pub(crate) struct Inbound<'a> {
    pub(crate) org_id: i32,
    pub(crate) team_id: &'a str,
    pub(crate) conversation_id: &'a str,
    pub(crate) activity_id: &'a str,
    pub(crate) service_url: &'a str,
    pub(crate) author_user_id: i32,
    pub(crate) author_object_id: &'a str,
    pub(crate) author_name: &'a str,
    pub(crate) body: &'a str,
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContentError> {
    row.try_get("", column).map_err(|error| database_error(error).into())
}

pub(crate) fn author(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(128).collect();
    if name.trim().is_empty() {
        "A teammate".to_owned()
    } else {
        name
    }
}

fn key_values(key: &ThreadKey) -> Vec<sea_orm::Value> {
    vec![key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into()]
}

/// Record one inbound message in its thread and the inbox, deciding
/// atomically whether it starts a run, waits, or is a replay.
pub(crate) async fn admit(
    database: &DatabaseConnection,
    key: &ThreadKey,
    message: &Inbound<'_>,
    now: i64,
) -> Result<Admission, ContentError> {
    let tx = database.begin().await.map_err(database_error)?;
    // Serializes with every other content write (and so with every other
    // admission): two first deliveries cannot both see "no live run".
    db::lock_admission_guard(&tx).await?;
    let replay = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_inbox WHERE tenant_id=$1 AND conversation_id=$2 AND activity_id=$3 UNION ALL SELECT 1 FROM neev_msteams_thread_messages WHERE tenant_id=$1 AND conversation_id=$2 AND activity_id=$3",
        vec![key.tenant_id.clone().into(), message.conversation_id.into(), message.activity_id.into()],
    )
    .await?;
    if replay.is_some() {
        tx.commit().await.map_err(database_error)?;
        return Ok(Admission::Replay);
    }
    db::execute(
        &tx,
        "INSERT INTO neev_msteams_threads(org_id,tenant_id,team_id,channel_id,root_id,service_url,started_by,live_sequence,next_sequence,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,NULL,1,$8,$8) ON CONFLICT DO NOTHING",
        vec![message.org_id.into(), key.tenant_id.clone().into(), message.team_id.into(), key.channel_id.clone().into(), key.root_id.clone().into(), message.service_url.into(), message.author_user_id.into(), now.into()],
    )
    .await?;
    let thread = db::one(
        &tx,
        "SELECT org_id,team_id,live_sequence,next_sequence FROM neev_msteams_threads WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3",
        key_values(key),
    )
    .await?
    .ok_or(ContentError::Storage)?;
    // A thread belongs to the org and Team its channel was bound to when it
    // began; anything else is not this thread.
    if get::<i32>(&thread, "org_id")? != message.org_id || get::<String>(&thread, "team_id")? != message.team_id {
        return Err(ContentError::NotFound);
    }
    let live: Option<i64> = get(&thread, "live_sequence")?;
    let sequence: i64 = get(&thread, "next_sequence")?;
    let waiting = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_inbox WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND state='queued' LIMIT 1",
        key_values(key),
    )
    .await?
    .is_some();
    let busy = live.is_some() || waiting;
    let body: String = message.body.chars().take(8_000).collect();
    let name = author(message.author_name);
    db::execute(
        &tx,
        "INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,'input',$8,$9,$10,$11,NULL,NULL,$12)",
        vec![
            message.org_id.into(),
            key.tenant_id.clone().into(),
            message.conversation_id.into(),
            message.activity_id.into(),
            key.channel_id.clone().into(),
            key.root_id.clone().into(),
            sequence.into(),
            message.author_user_id.into(),
            name.clone().into(),
            body.clone().into(),
            (if busy { "queued" } else { "started" }).into(),
            now.into(),
        ],
    )
    .await?;
    db::execute(
        &tx,
        "INSERT INTO neev_msteams_inbox(org_id,tenant_id,conversation_id,activity_id,scope,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,refusal,attempts,next_attempt_at,created_at,updated_at) VALUES($1,$2,$3,$4,'channel',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,NULL,NULL,0,$15,$15,$15)",
        vec![
            message.org_id.into(),
            key.tenant_id.clone().into(),
            message.conversation_id.into(),
            message.activity_id.into(),
            message.service_url.into(),
            message.author_user_id.into(),
            message.author_object_id.into(),
            name.into(),
            body.into(),
            message.team_id.into(),
            key.channel_id.clone().into(),
            key.root_id.clone().into(),
            sequence.into(),
            (if busy { "queued" } else { "pending" }).into(),
            now.into(),
        ],
    )
    .await?;
    db::execute(
        &tx,
        "UPDATE neev_msteams_threads SET next_sequence=$1,live_sequence=$2,service_url=$3,updated_at=$4 WHERE tenant_id=$5 AND channel_id=$6 AND root_id=$7",
        vec![
            (sequence + 1).into(),
            (if busy { live } else { Some(sequence) }).into(),
            message.service_url.into(),
            now.into(),
            key.tenant_id.clone().into(),
            key.channel_id.clone().into(),
            key.root_id.clone().into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(if busy {
        Admission::Queued { sequence }
    } else {
        Admission::Start { sequence }
    })
}

/// Bind the run launched for the claimed input: whose session ran it, and
/// which run it is. Runs in the worker's launch transaction.
pub(crate) async fn started<C: ConnectionTrait>(
    tx: &C,
    key: &ThreadKey,
    sequence: i64,
    run_id: &str,
    executed_as: i32,
) -> Result<(), ContentError> {
    db::execute(
        tx,
        "UPDATE neev_msteams_thread_messages SET run_id=$1,executed_as=$2,outcome='started' WHERE tenant_id=$3 AND channel_id=$4 AND root_id=$5 AND sequence=$6 AND kind='input'",
        vec![run_id.into(), executed_as.into(), key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), sequence.into()],
    )
    .await?;
    Ok(())
}

/// Release the thread's claim held by `sequence` (if it still holds it) and
/// hand it to the oldest waiting message, which becomes due to launch.
/// Returns the promoted sequence. Runs inside the caller's transaction.
pub(crate) async fn release(
    tx: &DatabaseTransaction,
    key: &ThreadKey,
    sequence: i64,
    now: i64,
) -> Result<Option<i64>, ContentError> {
    let released = db::execute(
        tx,
        "UPDATE neev_msteams_threads SET live_sequence=NULL,updated_at=$1 WHERE tenant_id=$2 AND channel_id=$3 AND root_id=$4 AND live_sequence=$5",
        vec![now.into(), key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), sequence.into()],
    )
    .await?;
    if released.rows_affected() != 1 {
        return Ok(None);
    }
    let next: Option<i64> = db::one(
        tx,
        "SELECT sequence FROM neev_msteams_inbox WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND state='queued' ORDER BY sequence LIMIT 1",
        key_values(key),
    )
    .await?
    .map(|row| get(&row, "sequence"))
    .transpose()?;
    let Some(next) = next else {
        return Ok(None);
    };
    db::execute(
        tx,
        "UPDATE neev_msteams_inbox SET state='pending',next_attempt_at=$1,updated_at=$1 WHERE tenant_id=$2 AND channel_id=$3 AND root_id=$4 AND sequence=$5 AND state='queued'",
        vec![now.into(), key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), next.into()],
    )
    .await?;
    db::execute(
        tx,
        "UPDATE neev_msteams_thread_messages SET outcome='started' WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND sequence=$4 AND kind='input'",
        vec![key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), next.into()],
    )
    .await?;
    db::execute(
        tx,
        "UPDATE neev_msteams_threads SET live_sequence=$1 WHERE tenant_id=$2 AND channel_id=$3 AND root_id=$4",
        vec![next.into(), key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into()],
    )
    .await?;
    Ok(Some(next))
}

/// A claimed input that will never run: mark it refused (dropping its text —
/// it never ran and is not context) and pass the claim on. Runs inside the
/// caller's transaction.
pub(crate) async fn refused(
    tx: &DatabaseTransaction,
    key: &ThreadKey,
    sequence: i64,
    now: i64,
) -> Result<Option<i64>, ContentError> {
    db::execute(
        tx,
        "UPDATE neev_msteams_thread_messages SET outcome='refused',body='' WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND sequence=$4 AND kind='input' AND run_id IS NULL",
        vec![key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), sequence.into()],
    )
    .await?;
    release(tx, key, sequence, now).await
}

/// One ledger line as context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) sequence: i64,
    /// `None` for Neev's own answers.
    pub(crate) author: Option<String>,
    pub(crate) body: String,
}

/// The thread so far, as context for the input at `sequence`: every answer
/// Neev posted in the thread, and every input said BEFORE it (inputs queued
/// after it are not yet its context). Refused inputs are not context. Oldest
/// first, newest kept.
pub(crate) async fn context(
    database: &DatabaseConnection,
    key: &ThreadKey,
    sequence: i64,
) -> Result<Vec<Entry>, ContentError> {
    let rows = database
        .query_all(db::statement(
            database,
            &format!("SELECT sequence,kind,author_name,body FROM neev_msteams_thread_messages WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND outcome<>'refused' AND ((kind='input' AND sequence<$4) OR kind='answer') ORDER BY sequence DESC LIMIT {CONTEXT_ENTRIES}"),
            vec![key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows.iter().rev() {
        let kind: String = get(row, "kind")?;
        entries.push(Entry {
            sequence: get(row, "sequence")?,
            author: (kind == "input").then(|| get(row, "author_name")).transpose()?,
            body: get(row, "body")?,
        });
    }
    Ok(entries)
}

/// What the agent receives for a run in a shared thread: the thread so far
/// (attributed, newest kept within budget, whole entries only) and the new
/// request with its author. The run still executes as the requester.
pub(crate) fn compose(entries: &[Entry], requester: &str, request: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut budget = CONTEXT_CHARS;
    for entry in entries.iter().rev() {
        let line = match &entry.author {
            Some(name) => format!("[{name}]: {}", entry.body),
            None => format!("[Neev]: {}", entry.body),
        };
        let cost = line.chars().count() + 1;
        if cost > budget {
            break;
        }
        budget -= cost;
        kept.push(line);
    }
    kept.reverse();
    let requester = author(requester);
    if kept.is_empty() {
        return format!("Request from {requester} in a Microsoft Teams channel thread:\n{request}");
    }
    format!(
        "This request was made in a Microsoft Teams channel thread shared by several people. Earlier messages in the thread, oldest first:\n{}\n\nNew request from {requester}:\n{request}",
        kept.join("\n")
    )
}

/// Record an answer that was POSTED into the thread, so later runs see it as
/// context. Idempotent per run: posting the same answer again records nothing.
pub(crate) async fn record_answer(
    database: &DatabaseConnection,
    key: &ThreadKey,
    org_id: i32,
    conversation_id: &str,
    run_id: &str,
    body: &str,
    now: i64,
) -> Result<(), ContentError> {
    let activity_id = format!("neev-answer:{run_id}");
    let tx = database.begin().await.map_err(database_error)?;
    db::lock_admission_guard(&tx).await?;
    let present = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_thread_messages WHERE tenant_id=$1 AND conversation_id=$2 AND activity_id=$3",
        vec![key.tenant_id.clone().into(), conversation_id.into(), activity_id.clone().into()],
    )
    .await?;
    if present.is_some() {
        tx.commit().await.map_err(database_error)?;
        return Ok(());
    }
    let sequence: i64 = get(
        &db::one(
            &tx,
            "SELECT next_sequence FROM neev_msteams_threads WHERE tenant_id=$1 AND channel_id=$2 AND root_id=$3 AND org_id=$4",
            vec![key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into(), org_id.into()],
        )
        .await?
        .ok_or(ContentError::NotFound)?,
        "next_sequence",
    )?;
    let body: String = body.chars().take(40_000).collect();
    db::execute(
        &tx,
        "INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,'answer',NULL,'Neev',$8,'answered',NULL,$9,$10)",
        vec![org_id.into(), key.tenant_id.clone().into(), conversation_id.into(), activity_id.into(), key.channel_id.clone().into(), key.root_id.clone().into(), sequence.into(), body.into(), run_id.into(), now.into()],
    )
    .await?;
    db::execute(
        &tx,
        "UPDATE neev_msteams_threads SET next_sequence=$1,updated_at=$2 WHERE tenant_id=$3 AND channel_id=$4 AND root_id=$5",
        vec![(sequence + 1).into(), now.into(), key.tenant_id.clone().into(), key.channel_id.clone().into(), key.root_id.clone().into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}
