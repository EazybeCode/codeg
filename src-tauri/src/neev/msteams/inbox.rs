//! The durable inbox (`neev_msteams_inbox`): every accepted Teams message is
//! committed here before the HTTP acknowledgement, and the worker
//! (`worker::tick`) carries it to exactly one run — after a crash too.
//!
//! States: `queued` (a channel message waiting for its thread's live run),
//! `pending` (due to launch; `next_attempt_at` is both the retry backoff and
//! the claim lease), `launched` (a run exists), `settled` (its final answer is
//! enqueued), `refused` (with the stable refusal code). A message is launched
//! with a request id derived from its activity, so even a launch repeated
//! after a crash between "run created" and "state recorded" replays the same
//! run instead of starting another.

use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait};

use super::threads::ThreadKey;
use crate::neev::content::types::ContentError;
use crate::neev::db;
use crate::neev::error::database_error;

/// How long a claimed item is reserved for the worker that claimed it. A
/// worker that dies mid-launch releases it by letting this pass.
pub(crate) const LEASE_SECS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) org_id: i32,
    pub(crate) tenant_id: String,
    pub(crate) conversation_id: String,
    pub(crate) activity_id: String,
    pub(crate) service_url: String,
    pub(crate) user_id: i32,
    pub(crate) aad_object_id: String,
    pub(crate) author_name: String,
    pub(crate) body: String,
    /// `Some` for a channel thread message.
    pub(crate) channel: Option<ChannelOrigin>,
    pub(crate) state: String,
    pub(crate) run_id: Option<String>,
    pub(crate) attempts: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelOrigin {
    pub(crate) team_id: String,
    pub(crate) key: ThreadKey,
    pub(crate) sequence: i64,
}

const COLUMNS: &str = "org_id,tenant_id,conversation_id,activity_id,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,attempts";

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContentError> {
    row.try_get("", column).map_err(|error| database_error(error).into())
}

fn item(row: &QueryResult) -> Result<Item, ContentError> {
    let tenant_id: String = get(row, "tenant_id")?;
    let channel = match (
        get::<Option<String>>(row, "team_id")?,
        get::<Option<String>>(row, "channel_id")?,
        get::<Option<String>>(row, "root_id")?,
        get::<Option<i64>>(row, "sequence")?,
    ) {
        (Some(team_id), Some(channel_id), Some(root_id), Some(sequence)) => Some(ChannelOrigin {
            team_id,
            key: ThreadKey {
                tenant_id: tenant_id.clone(),
                channel_id,
                root_id,
            },
            sequence,
        }),
        _ => None,
    };
    Ok(Item {
        org_id: get(row, "org_id")?,
        tenant_id,
        conversation_id: get(row, "conversation_id")?,
        activity_id: get(row, "activity_id")?,
        service_url: get(row, "service_url")?,
        user_id: get(row, "user_id")?,
        aad_object_id: get(row, "aad_object_id")?,
        author_name: get(row, "author_name")?,
        body: get(row, "body")?,
        channel,
        state: get(row, "state")?,
        run_id: get(row, "run_id")?,
        attempts: get::<i32>(row, "attempts")? as i64,
    })
}

/// One personal (1:1) message, already attributed to a verified member.
pub(crate) struct Personal<'a> {
    pub(crate) org_id: i32,
    pub(crate) tenant_id: &'a str,
    pub(crate) conversation_id: &'a str,
    pub(crate) activity_id: &'a str,
    pub(crate) service_url: &'a str,
    pub(crate) user_id: i32,
    pub(crate) aad_object_id: &'a str,
    pub(crate) author_name: &'a str,
    pub(crate) body: &'a str,
}

/// Record a personal message. `false` = a replay of an activity already
/// recorded (nothing written).
pub(crate) async fn admit_personal(
    database: &DatabaseConnection,
    message: &Personal<'_>,
    now: i64,
) -> Result<bool, ContentError> {
    let tx = database.begin().await.map_err(database_error)?;
    db::lock_admission_guard(&tx).await?;
    let replay = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_inbox WHERE tenant_id=$1 AND conversation_id=$2 AND activity_id=$3",
        vec![message.tenant_id.into(), message.conversation_id.into(), message.activity_id.into()],
    )
    .await?;
    if replay.is_some() {
        tx.commit().await.map_err(database_error)?;
        return Ok(false);
    }
    let body: String = message.body.chars().take(8_000).collect();
    db::execute(
        &tx,
        "INSERT INTO neev_msteams_inbox(org_id,tenant_id,conversation_id,activity_id,scope,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,refusal,attempts,next_attempt_at,created_at,updated_at) VALUES($1,$2,$3,$4,'personal',$5,$6,$7,$8,$9,NULL,NULL,NULL,NULL,'pending',NULL,NULL,0,$10,$10,$10)",
        vec![
            message.org_id.into(),
            message.tenant_id.into(),
            message.conversation_id.into(),
            message.activity_id.into(),
            message.service_url.into(),
            message.user_id.into(),
            message.aad_object_id.into(),
            super::threads::author(message.author_name).into(),
            body.into(),
            now.into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(true)
}

/// Claim up to `limit` items due to launch: each claim is a conditional
/// update, so two workers never launch one item at the same time.
pub(crate) async fn claim_due(database: &DatabaseConnection, now: i64, limit: u64) -> Result<Vec<Item>, ContentError> {
    let rows = database
        .query_all(db::statement(
            database,
            &format!("SELECT {COLUMNS} FROM neev_msteams_inbox WHERE state='pending' AND next_attempt_at<=$1 ORDER BY created_at,activity_id LIMIT {limit}"),
            vec![now.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut claimed = Vec::new();
    for row in rows {
        let found = item(&row)?;
        let won = db::execute(
            database,
            "UPDATE neev_msteams_inbox SET attempts=attempts+1,next_attempt_at=$1,updated_at=$2 WHERE tenant_id=$3 AND conversation_id=$4 AND activity_id=$5 AND state='pending' AND next_attempt_at<=$2",
            vec![(now + LEASE_SECS).into(), now.into(), found.tenant_id.clone().into(), found.conversation_id.clone().into(), found.activity_id.clone().into()],
        )
        .await?;
        if won.rows_affected() == 1 {
            claimed.push(Item {
                attempts: found.attempts + 1,
                ..found
            });
        }
    }
    Ok(claimed)
}

fn key_of(item: &Item) -> Vec<sea_orm::Value> {
    vec![item.tenant_id.clone().into(), item.conversation_id.clone().into(), item.activity_id.clone().into()]
}

/// The claimed item launched `run_id`.
pub(crate) async fn launched<C: ConnectionTrait>(tx: &C, item: &Item, run_id: &str, now: i64) -> Result<(), ContentError> {
    let mut values = vec![run_id.into(), now.into()];
    values.extend(key_of(item));
    db::execute(
        tx,
        "UPDATE neev_msteams_inbox SET state='launched',run_id=$1,updated_at=$2 WHERE tenant_id=$3 AND conversation_id=$4 AND activity_id=$5 AND state='pending'",
        values,
    )
    .await?;
    Ok(())
}

/// The claimed item will never run.
pub(crate) async fn refuse<C: ConnectionTrait>(tx: &C, item: &Item, code: &str, now: i64) -> Result<(), ContentError> {
    let mut values = vec![code.into(), now.into()];
    values.extend(key_of(item));
    db::execute(
        tx,
        "UPDATE neev_msteams_inbox SET state='refused',refusal=$1,updated_at=$2 WHERE tenant_id=$3 AND conversation_id=$4 AND activity_id=$5 AND state IN ('pending','queued')",
        values,
    )
    .await?;
    Ok(())
}

/// Try the claimed item again at `at` (a transient failure).
pub(crate) async fn retry_at(database: &DatabaseConnection, item: &Item, at: i64, now: i64) -> Result<(), ContentError> {
    let mut values = vec![at.into(), now.into()];
    values.extend(key_of(item));
    db::execute(
        database,
        "UPDATE neev_msteams_inbox SET next_attempt_at=$1,updated_at=$2 WHERE tenant_id=$3 AND conversation_id=$4 AND activity_id=$5 AND state='pending'",
        values,
    )
    .await?;
    Ok(())
}

/// Launched items whose run has settled (completed, failed or canceled).
pub(crate) async fn settled_runs(database: &DatabaseConnection, limit: u64) -> Result<Vec<Item>, ContentError> {
    let rows = database
        .query_all(db::statement(
            database,
            &format!("SELECT {} FROM neev_msteams_inbox i JOIN neev_agent_turns t ON t.org_id=i.org_id AND t.run_id=i.run_id WHERE i.state='launched' AND t.status<>'running' ORDER BY i.updated_at LIMIT {limit}", COLUMNS.split(',').map(|c| format!("i.{c}")).collect::<Vec<_>>().join(",")),
            vec![],
        ))
        .await
        .map_err(database_error)?;
    rows.iter().map(item).collect()
}

/// Mark a launched item settled (its final answer was enqueued in the same
/// transaction). `false` = another worker already did.
pub(crate) async fn settle<C: ConnectionTrait>(tx: &C, item: &Item, now: i64) -> Result<bool, ContentError> {
    let mut values = vec![now.into()];
    values.extend(key_of(item));
    Ok(db::execute(
        tx,
        "UPDATE neev_msteams_inbox SET state='settled',updated_at=$1 WHERE tenant_id=$2 AND conversation_id=$3 AND activity_id=$4 AND state='launched'",
        values,
    )
    .await?
    .rows_affected()
        == 1)
}

/// One inbox item by its activity key.
pub(crate) async fn by_activity(database: &DatabaseConnection, tenant_id: &str, conversation_id: &str, activity_id: &str) -> Result<Option<Item>, ContentError> {
    db::one(
        database,
        &format!("SELECT {COLUMNS} FROM neev_msteams_inbox WHERE tenant_id=$1 AND conversation_id=$2 AND activity_id=$3"),
        vec![tenant_id.into(), conversation_id.into(), activity_id.into()],
    )
    .await?
    .map(|row| item(&row))
    .transpose()
}

/// The message a run was launched for, scoped to the org.
pub(crate) async fn by_run(database: &DatabaseConnection, org_id: i32, run_id: &str) -> Result<Option<Item>, ContentError> {
    db::one(
        database,
        &format!("SELECT {COLUMNS} FROM neev_msteams_inbox WHERE org_id=$1 AND run_id=$2"),
        vec![org_id.into(), run_id.into()],
    )
    .await?
    .map(|row| item(&row))
    .transpose()
}

/// Turns in this Neev session that were NOT launched from a Teams channel
/// thread (for example, the member chatted in it on the web, with a private
/// briefing). `except_request` is the launch being attempted now: after a
/// crash its turn may exist before the inbox recorded it, and it is not
/// foreign. Used to prove a session holds only channel-published turns.
pub(crate) async fn foreign_turns(database: &DatabaseConnection, org_id: i32, session_id: &str, except_request: &str) -> Result<i64, ContentError> {
    let row = db::one(
        database,
        "SELECT COUNT(*) AS n FROM neev_agent_turns t WHERE t.org_id=$1 AND t.session_id=$2 AND (t.request_id IS NULL OR t.request_id<>$3) AND NOT EXISTS (SELECT 1 FROM neev_msteams_inbox i WHERE i.org_id=t.org_id AND i.run_id=t.run_id AND i.scope='channel')",
        vec![org_id.into(), session_id.into(), except_request.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    get(&row, "n")
}
