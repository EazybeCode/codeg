//! The durable outbox (`neev_msteams_outbox`): every message Neev posts to
//! Teams after acknowledging an activity. Enqueued in the same transaction as
//! the state change that caused it, keyed by a digest of (cause, kind) so a
//! repeated enqueue is a no-op, and delivered by the worker with bounded
//! retries that honour `Retry-After`.
//!
//! Delivery is at-least-once: when the Bot Connector times out after the
//! request was sent, Teams may or may not have posted it, and the Bot
//! Connector offers no idempotency key or lookup to reconcile with. The worker
//! retries (a possible duplicate post) rather than silently losing a final
//! answer; agent execution is never repeated by a delivery retry.

use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult};
use serde_json::Value;

use super::activity;
use super::inbox::Item;
use crate::neev::content::types::ContentError;
use crate::neev::db;
use crate::neev::error::database_error;

/// Delivery attempts before a message is marked `failed`.
pub(crate) const MAX_ATTEMPTS: i64 = 8;
/// How long a claimed delivery is reserved for its worker.
pub(crate) const LEASE_SECS: i64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A fixed card (acknowledgement, queued note, refusal).
    Notice,
    /// The run's final answer, read and re-authorized at send time.
    Final,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub(crate) id: String,
    pub(crate) org_id: i32,
    pub(crate) tenant_id: String,
    pub(crate) service_url: String,
    pub(crate) conversation_id: String,
    pub(crate) reply_to_id: String,
    pub(crate) channel: Option<(String, String)>,
    pub(crate) kind: Kind,
    pub(crate) run_id: Option<String>,
    pub(crate) user_id: i32,
    pub(crate) card: Option<Value>,
    pub(crate) attempts: i64,
}

/// The outbox id for one cause and kind: a run's acknowledgement, a run's
/// final answer, or an activity's notice.
pub(crate) fn id(cause: &str, what: &str) -> String {
    activity::digest(&["msteams-outbox-v1", cause, what])
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContentError> {
    row.try_get("", column).map_err(|error| database_error(error).into())
}

/// Enqueue a message answering `item` (in its conversation and thread).
/// Idempotent per `id`. Runs in the caller's transaction.
pub(crate) async fn enqueue<C: ConnectionTrait>(
    tx: &C,
    id: &str,
    item: &Item,
    kind: Kind,
    run_id: Option<&str>,
    card: Option<&Value>,
    now: i64,
) -> Result<(), ContentError> {
    let (scope, channel_id, root_id) = match &item.channel {
        Some(origin) => ("channel", Some(origin.key.channel_id.clone()), Some(origin.key.root_id.clone())),
        None => ("personal", None, None),
    };
    let card = card.map(Value::to_string);
    if card.as_ref().is_some_and(|card| card.len() > 60_000) {
        return Err(ContentError::Invalid);
    }
    db::execute(
        tx,
        "INSERT INTO neev_msteams_outbox(id,org_id,tenant_id,service_url,conversation_id,reply_to_id,scope,channel_id,root_id,kind,run_id,user_id,card,state,attempts,next_attempt_at,message_id,error_code,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'pending',0,$14,NULL,NULL,$14,$14) ON CONFLICT DO NOTHING",
        vec![
            id.into(),
            item.org_id.into(),
            item.tenant_id.clone().into(),
            item.service_url.clone().into(),
            item.conversation_id.clone().into(),
            item.activity_id.clone().into(),
            scope.into(),
            channel_id.into(),
            root_id.into(),
            (match kind {
                Kind::Notice => "notice",
                Kind::Final => "final",
            })
            .into(),
            run_id.map(str::to_owned).into(),
            item.user_id.into(),
            card.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

const COLUMNS: &str = "id,org_id,tenant_id,service_url,conversation_id,reply_to_id,channel_id,root_id,kind,run_id,user_id,card,attempts";

fn delivery(row: &QueryResult) -> Result<Delivery, ContentError> {
    let channel = match (get::<Option<String>>(row, "channel_id")?, get::<Option<String>>(row, "root_id")?) {
        (Some(channel), Some(root)) => Some((channel, root)),
        _ => None,
    };
    let card: Option<String> = get(row, "card")?;
    Ok(Delivery {
        id: get(row, "id")?,
        org_id: get(row, "org_id")?,
        tenant_id: get(row, "tenant_id")?,
        service_url: get(row, "service_url")?,
        conversation_id: get(row, "conversation_id")?,
        reply_to_id: get(row, "reply_to_id")?,
        channel,
        kind: match get::<String>(row, "kind")?.as_str() {
            "final" => Kind::Final,
            _ => Kind::Notice,
        },
        run_id: get(row, "run_id")?,
        user_id: get(row, "user_id")?,
        card: card
            .map(|card| serde_json::from_str(&card).map_err(|_| ContentError::Storage))
            .transpose()?,
        attempts: get::<i32>(row, "attempts")? as i64,
    })
}

/// Claim up to `limit` due deliveries (conditional update per row).
pub(crate) async fn claim_due(database: &DatabaseConnection, now: i64, limit: u64) -> Result<Vec<Delivery>, ContentError> {
    let rows = database
        .query_all(db::statement(
            database,
            &format!("SELECT {COLUMNS} FROM neev_msteams_outbox WHERE state='pending' AND next_attempt_at<=$1 ORDER BY created_at,id LIMIT {limit}"),
            vec![now.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut claimed = Vec::new();
    for row in rows {
        let found = delivery(&row)?;
        let won = db::execute(
            database,
            "UPDATE neev_msteams_outbox SET attempts=attempts+1,next_attempt_at=$1,updated_at=$2 WHERE id=$3 AND state='pending' AND next_attempt_at<=$2",
            vec![(now + LEASE_SECS).into(), now.into(), found.id.clone().into()],
        )
        .await?;
        if won.rows_affected() == 1 {
            claimed.push(Delivery {
                attempts: found.attempts + 1,
                ..found
            });
        }
    }
    Ok(claimed)
}

/// Record the outcome of one attempt.
pub(crate) async fn record(
    database: &DatabaseConnection,
    id: &str,
    state: &str,
    next_attempt_at: i64,
    message_id: Option<&str>,
    error_code: Option<&str>,
    now: i64,
) -> Result<(), ContentError> {
    db::execute(
        database,
        "UPDATE neev_msteams_outbox SET state=$1,next_attempt_at=$2,message_id=$3,error_code=$4,updated_at=$5 WHERE id=$6 AND state='pending'",
        vec![
            state.into(),
            next_attempt_at.into(),
            message_id.map(str::to_owned).into(),
            error_code.map(str::to_owned).into(),
            now.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Seconds to wait before attempt `attempts + 1`: exponential from 5 s,
/// capped at 15 min, never sooner than the service asked.
pub(crate) fn backoff(attempts: i64, retry_after: Option<u64>) -> i64 {
    let exponential = 5_i64.saturating_mul(1_i64 << attempts.clamp(0, 8)).min(900);
    exponential.max(retry_after.map(|secs| secs as i64).unwrap_or(0))
}
