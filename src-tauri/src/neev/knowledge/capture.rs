//! Bounded, deterministic capture of eligible shared sources (no LLM).
//!
//! Spec §3 as amended (B4, B5, D2, G1):
//! - orgs with capture disabled (or a disabled org) are never touched;
//! - each tick runs one change scan per enabled org, then extracts from a
//!   queue ordered by `next_check_at, checked_at, source_id`, with a fair
//!   per-org share of `MAX_EXTRACTIONS_PER_TICK`;
//! - a native/chat source is extracted only once the same new marker has been
//!   seen on `DEBOUNCE_TICKS` consecutive ticks (engine documents debounce on
//!   the bridge's `content_sha256`);
//! - failures back off exponentially from 2 min to 6 h;
//! - an org over `ORG_BYTE_BUDGET` is blocked with a visible `quota_exceeded`
//!   and gets no writes (compile is unaffected);
//! - eligibility, `auth_revision` and the marker are re-checked in the write
//!   transaction; a blob is written only inside that step, and deleted (best
//!   effort) if the commit fails.

use super::access::{eligible_predicate, EligibleSource};
use super::render::{self, ChatMessage, Unit};
use super::store;
use super::types::*;
use crate::neev::{
    content::{rich, types::Resource, types::ResourceKind, types::Visibility},
    db,
    engine::upstream::{EngineError, EngineMarkdown, MarkdownRefusal, Upstream},
    startup::ServerState,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult, TransactionTrait};
use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Exponential backoff after `failures` consecutive failures.
pub(crate) fn backoff(failures: i32) -> i64 {
    let exponent = failures.clamp(1, 30) - 1;
    BACKOFF_MIN_SECS
        .saturating_mul(1i64 << exponent)
        .min(BACKOFF_MAX_SECS)
}

#[cfg(test)]
thread_local! {
    static BUDGET_OVERRIDE: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Tests only: replace `ORG_BYTE_BUDGET` on this thread (a 2 GiB budget
/// cannot be filled with 64 KiB rows in a unit test).
#[cfg(test)]
pub(crate) fn set_budget_override(value: Option<i64>) {
    BUDGET_OVERRIDE.with(|cell| cell.set(value));
}

fn budget() -> i64 {
    #[cfg(test)]
    if let Some(value) = BUDGET_OVERRIDE.with(|cell| cell.get()) {
        return value;
    }
    ORG_BYTE_BUDGET
}

/// Bytes an org's knowledge holds locally (snapshots + revisions).
pub(crate) async fn bytes_used<C: ConnectionTrait>(db: &C, org_id: i32) -> Result<i64, KnowledgeError> {
    let row = db::one(
        db,
        "SELECT CAST((SELECT COALESCE(SUM(byte_length),0) FROM neev_knowledge_snapshots WHERE org_id=$1) + (SELECT COALESCE(SUM(byte_length),0) FROM neev_knowledge_revisions WHERE org_id=$1) AS BIGINT) AS used",
        vec![org_id.into()],
    )
    .await?
    .ok_or_else(KnowledgeError::storage)?;
    row.try_get::<i64>("", "used").map_err(db_error)
}

/// Capture-state row of one source.
#[derive(Clone, Debug, Default)]
struct SourceRow {
    exists: bool,
    marker: Option<String>,
    pending_marker: Option<String>,
    pending_seen: i32,
    next_check_at: i64,
    checked_at: i64,
    failures: i32,
}

/// A source the scan found ready for extraction.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    source: EligibleSource,
    engine: bool,
    row: SourceRow,
}

/// What one extraction attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Snapshot rows written (`units` new current snapshots).
    Written { units: usize },
    Unchanged,
    /// Engine content changed; waiting for the second sighting.
    Pending,
    /// Something moved between extraction and write; nothing written.
    Raced,
    Problem(CaptureState),
    QuotaBlocked,
}

async fn set_blocked<C: ConnectionTrait>(db: &C, org_id: i32, blocked: bool) -> Result<(), KnowledgeError> {
    db::execute(
        db,
        if blocked {
            "UPDATE neev_knowledge_settings SET capture_blocked='quota_exceeded' WHERE org_id=$1 AND capture_blocked IS NULL"
        } else {
            "UPDATE neev_knowledge_settings SET capture_blocked=NULL WHERE org_id=$1 AND capture_blocked IS NOT NULL"
        },
        vec![org_id.into()],
    )
    .await?;
    Ok(())
}

fn engine_resource(org_id: i32, source: &EligibleSource) -> Resource {
    Resource {
        id: source.id.clone(),
        org_id,
        node_id: source.node_id,
        kind: ResourceKind::Document,
        creator_id: source.creator_id,
        title: source.title.clone(),
        visibility: Visibility::Organization,
        origin_chat_id: None,
        auth_revision: source.auth_revision,
        content_revision: source.content_revision,
        archived: false,
        created_at: 0,
        updated_at: 0,
    }
}

fn row_source(row: &QueryResult) -> Result<(EligibleSource, Option<i64>, SourceRow), KnowledgeError> {
    let kind: String = row.try_get("", "kind").map_err(db_error)?;
    let source = EligibleSource {
        id: row.try_get("", "id").map_err(db_error)?,
        kind: SourceKind::parse(&kind).ok_or_else(KnowledgeError::storage)?,
        title: row.try_get("", "title").map_err(db_error)?,
        node_id: row.try_get("", "node_id").map_err(db_error)?,
        auth_revision: row.try_get("", "auth_revision").map_err(db_error)?,
        content_revision: row.try_get("", "content_revision").map_err(db_error)?,
        creator_id: row.try_get("", "creator_id").map_err(db_error)?,
    };
    let max_seq: Option<i64> = row.try_get("", "max_seq").map_err(db_error)?;
    let state: Option<String> = row.try_get("", "cstate").map_err(db_error)?;
    let stored = SourceRow {
        exists: state.is_some(),
        marker: row.try_get("", "marker").map_err(db_error)?,
        pending_marker: row.try_get("", "pending_marker").map_err(db_error)?,
        pending_seen: row.try_get::<Option<i32>>("", "pending_seen").map_err(db_error)?.unwrap_or(0),
        next_check_at: row.try_get::<Option<i64>>("", "next_check_at").map_err(db_error)?.unwrap_or(0),
        checked_at: row.try_get::<Option<i64>>("", "checked_at").map_err(db_error)?.unwrap_or(0),
        failures: row.try_get::<Option<i32>>("", "failures").map_err(db_error)?.unwrap_or(0),
    };
    Ok((source, max_seq, stored))
}

/// The change scan for one org: one SQL read of every eligible source with its
/// native marker, then the debounce bookkeeping. Returns the ready queue.
pub(crate) async fn scan(state: &ServerState, org_id: i32, now: i64) -> Result<Vec<Candidate>, KnowledgeError> {
    let tx = db::begin(&state.database).await?;
    let sql = format!(
        "SELECT r.id,r.kind,r.title,r.node_id,r.auth_revision,r.content_revision,r.creator_id,\
         (SELECT MAX(m.sequence) FROM neev_messages m WHERE m.org_id=r.org_id AND m.chat_id=r.id) AS max_seq,\
         s.state AS cstate,s.marker,s.pending_marker,s.pending_seen,s.next_check_at,s.checked_at,s.failures \
         FROM neev_resources r LEFT JOIN neev_knowledge_sources s ON s.org_id=r.org_id AND s.source_id=r.id \
         WHERE r.org_id=$1 AND {} ORDER BY r.id",
        eligible_predicate("r")
    );
    let rows = tx
        .query_all(db::statement(&tx, &sql, vec![org_id.into()]))
        .await
        .map_err(db_error)?;
    let mut sources = Vec::with_capacity(rows.len());
    for row in &rows {
        sources.push(row_source(row)?);
    }
    let documents: Vec<Resource> = sources
        .iter()
        .filter(|(source, _, _)| source.kind == SourceKind::Document)
        .map(|(source, _, _)| engine_resource(org_id, source))
        .collect();
    let bound = crate::neev::engine::binding::bound_set(&tx, &documents).await?;
    let mut ready = Vec::new();
    for (source, max_seq, mut row) in sources {
        let engine = bound.contains(&source.id);
        if !row.exists {
            db::execute(
                &tx,
                "INSERT INTO neev_knowledge_sources(org_id,source_id,state,detail,marker,pending_marker,pending_seen,next_check_at,failures,checked_at) VALUES($1,$2,'ok','',NULL,NULL,0,0,0,0)",
                vec![org_id.into(), source.id.clone().into()],
            )
            .await?;
            row.exists = true;
        }
        if engine {
            // Content is only knowable through the bridge; due checks queue.
            if row.next_check_at <= now {
                ready.push(Candidate { source, engine, row });
            }
            continue;
        }
        let marker = match source.kind {
            SourceKind::Chat => max_seq.unwrap_or(0).to_string(),
            SourceKind::Document => source.content_revision.to_string(),
        };
        if row.marker.as_deref() == Some(marker.as_str()) {
            if row.pending_marker.is_some() {
                db::execute(
                    &tx,
                    "UPDATE neev_knowledge_sources SET pending_marker=NULL,pending_seen=0 WHERE org_id=$1 AND source_id=$2",
                    vec![org_id.into(), source.id.clone().into()],
                )
                .await?;
            }
            continue;
        }
        let seen = if row.pending_marker.as_deref() == Some(marker.as_str()) {
            (row.pending_seen + 1).min(1_000)
        } else {
            1
        };
        db::execute(
            &tx,
            "UPDATE neev_knowledge_sources SET pending_marker=$1,pending_seen=$2 WHERE org_id=$3 AND source_id=$4",
            vec![marker.clone().into(), seen.into(), org_id.into(), source.id.clone().into()],
        )
        .await?;
        row.pending_marker = Some(marker.clone());
        row.pending_seen = seen;
        if seen >= DEBOUNCE_TICKS && row.next_check_at <= now {
            ready.push(Candidate { source, engine, row });
        }
    }
    tx.commit().await.map_err(db_error)?;
    ready.sort_by(|a, b| {
        (a.row.next_check_at, a.row.checked_at, &a.source.id).cmp(&(b.row.next_check_at, b.row.checked_at, &b.source.id))
    });
    Ok(ready)
}

/// What a source currently looks like, freshly extracted.
struct Extracted {
    marker: String,
    units: Vec<Unit>,
    /// Chat: the max sequence included, re-checked at write time.
    max_sequence: Option<i64>,
}

async fn extract_chat(state: &ServerState, org_id: i32, source: &EligibleSource) -> Result<Extracted, KnowledgeError> {
    let rows = state
        .database
        .query_all(db::statement(
            &state.database,
            "SELECT m.id,m.sequence,m.body,m.author_kind,m.agent_label,m.created_at,a.display_name AS author_name FROM neev_messages m JOIN neev_accounts a ON a.id=m.author_id WHERE m.org_id=$1 AND m.chat_id=$2 ORDER BY m.sequence",
            vec![org_id.into(), source.id.clone().into()],
        ))
        .await
        .map_err(db_error)?;
    let files = state
        .database
        .query_all(db::statement(
            &state.database,
            "SELECT ar.message_id,a.file_name FROM neev_attachment_refs ar JOIN neev_attachments a ON a.org_id=ar.org_id AND a.key=ar.attachment_key JOIN neev_messages m ON m.org_id=ar.org_id AND m.id=ar.message_id WHERE ar.org_id=$1 AND m.chat_id=$2 AND a.archived=false",
            vec![org_id.into(), source.id.clone().into()],
        ))
        .await
        .map_err(db_error)?;
    let mut by_message: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for row in &files {
        let message: String = row.try_get("", "message_id").map_err(db_error)?;
        let name: String = row.try_get("", "file_name").map_err(db_error)?;
        by_message.entry(message).or_default().push(name);
    }
    let mut messages = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(db_error)?;
        let kind: String = row.try_get("", "author_kind").map_err(db_error)?;
        let label: Option<String> = row.try_get("", "agent_label").map_err(db_error)?;
        let author: String = row.try_get("", "author_name").map_err(db_error)?;
        // Same labelling as agents::transcript, so an agent reply is never
        // presented as a person's words.
        let who = if kind == "agent" {
            format!("{} (assistant)", label.unwrap_or_else(|| "agent".to_owned()))
        } else {
            format!("{author} (person)")
        };
        messages.push(ChatMessage {
            sequence: row.try_get("", "sequence").map_err(db_error)?,
            at: row.try_get("", "created_at").map_err(db_error)?,
            who,
            body: row.try_get("", "body").map_err(db_error)?,
            files: by_message.remove(&id).unwrap_or_default(),
        });
    }
    let max_sequence = messages.last().map(|m| m.sequence);
    Ok(Extracted {
        marker: max_sequence.unwrap_or(0).to_string(),
        units: render::chat_segments(&source.title, &messages),
        max_sequence: Some(max_sequence.unwrap_or(0)),
    })
}

async fn extract_native_document(
    state: &ServerState,
    org_id: i32,
    source: &EligibleSource,
) -> Result<Extracted, KnowledgeError> {
    let row = db::one(
        &state.database,
        "SELECT r.revision,r.blob_key,r.sha256,r.byte_length,f.format FROM neev_document_revisions r LEFT JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE r.org_id=$1 AND r.document_id=$2 ORDER BY r.revision DESC LIMIT 1",
        vec![org_id.into(), source.id.clone().into()],
    )
    .await?;
    let markdown = match row {
        None => String::new(),
        Some(row) => {
            let key: String = row.try_get("", "blob_key").map_err(db_error)?;
            let sha: String = row.try_get("", "sha256").map_err(db_error)?;
            let length: i64 = row.try_get("", "byte_length").map_err(db_error)?;
            let format: Option<String> = row.try_get("", "format").map_err(db_error)?;
            let bytes = state.blobs.read(org_id, &key, length, &sha).await?;
            if format.as_deref() == Some("blocks") {
                rich::export_markdown(&rich::decode(&bytes)?)?
            } else {
                String::from_utf8(bytes).map_err(|_| KnowledgeError::storage())?
            }
        }
    };
    Ok(Extracted {
        marker: source.content_revision.to_string(),
        units: render::doc_segments(&source.title, &markdown),
        max_sequence: None,
    })
}

async fn record_problem(
    state: &ServerState,
    org_id: i32,
    candidate: &Candidate,
    problem: CaptureState,
    now: i64,
) -> Result<Outcome, KnowledgeError> {
    let failures = candidate.row.failures.saturating_add(1);
    db::execute(
        &state.database,
        "UPDATE neev_knowledge_sources SET state=$1,detail=$2,failures=$3,next_check_at=$4,checked_at=$5 WHERE org_id=$6 AND source_id=$7",
        vec![
            problem.as_str().into(),
            problem.as_str().into(),
            failures.into(),
            (now + backoff(failures)).into(),
            now.into(),
            org_id.into(),
            candidate.source.id.clone().into(),
        ],
    )
    .await?;
    Ok(Outcome::Problem(problem))
}

/// Allocate a cite key: 12 lowercase hex characters, unique per org (pre-checked
/// so a PostgreSQL transaction never has to survive a unique violation).
async fn cite_key(tx: &DatabaseTransaction, org_id: i32) -> Result<String, KnowledgeError> {
    for _ in 0..5 {
        let candidate: String = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
        let taken = db::one(
            tx,
            "SELECT 1 AS taken FROM neev_knowledge_snapshots WHERE org_id=$1 AND cite_key=$2",
            vec![org_id.into(), candidate.clone().into()],
        )
        .await?
        .is_some();
        if !taken {
            return Ok(candidate);
        }
    }
    Err(KnowledgeError::conflict())
}

struct Current {
    id: String,
    segment: i32,
    sha256: String,
    from_sequence: Option<i64>,
    to_sequence: Option<i64>,
}

async fn current_snapshots(tx: &DatabaseTransaction, org_id: i32, source_id: &str) -> Result<Vec<Current>, KnowledgeError> {
    let rows = tx
        .query_all(db::statement(
            tx,
            "SELECT id,segment,sha256,from_sequence,to_sequence FROM neev_knowledge_snapshots WHERE org_id=$1 AND source_id=$2 AND state='current' ORDER BY segment",
            vec![org_id.into(), source_id.to_owned().into()],
        ))
        .await
        .map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok(Current {
                id: row.try_get("", "id").map_err(db_error)?,
                segment: row.try_get("", "segment").map_err(db_error)?,
                sha256: row.try_get("", "sha256").map_err(db_error)?,
                from_sequence: row.try_get("", "from_sequence").map_err(db_error)?,
                to_sequence: row.try_get("", "to_sequence").map_err(db_error)?,
            })
        })
        .collect()
}

/// Write the extracted units in one transaction, after re-checking that
/// nothing moved. Unchanged units (same segment and bytes) are kept, so their
/// cite keys and coverage stay; changed and surplus segments are superseded.
async fn write(
    state: &ServerState,
    org_id: i32,
    candidate: &Candidate,
    extracted: Extracted,
    now: i64,
) -> Result<Outcome, KnowledgeError> {
    let blobs = store::store(state)?;
    let source = &candidate.source;
    let tx = db::begin(&state.database).await?;
    if tx.get_database_backend() == sea_orm::DbBackend::Postgres {
        db::one(
            &tx,
            "SELECT id FROM neev_resources WHERE org_id=$1 AND id=$2 FOR SHARE",
            vec![org_id.into(), source.id.clone().into()],
        )
        .await?;
    }
    let Some(now_source) = super::access::is_eligible(&tx, org_id, &source.id).await? else {
        tx.rollback().await.map_err(db_error)?;
        return Ok(Outcome::Raced);
    };
    if now_source.auth_revision != source.auth_revision {
        tx.rollback().await.map_err(db_error)?;
        return Ok(Outcome::Raced);
    }
    let moved = match (source.kind, candidate.engine) {
        (SourceKind::Document, false) => now_source.content_revision != source.content_revision,
        (SourceKind::Chat, _) => {
            let max: Option<i64> = db::one(
                &tx,
                "SELECT MAX(sequence) AS n FROM neev_messages WHERE org_id=$1 AND chat_id=$2",
                vec![org_id.into(), source.id.clone().into()],
            )
            .await?
            .ok_or_else(KnowledgeError::storage)?
            .try_get("", "n")
            .map_err(db_error)?;
            max.unwrap_or(0) != extracted.max_sequence.unwrap_or(0)
        }
        (SourceKind::Document, true) => false,
    };
    if moved {
        tx.rollback().await.map_err(db_error)?;
        return Ok(Outcome::Raced);
    }
    let current = current_snapshots(&tx, org_id, &source.id).await?;
    let mut to_write: Vec<(&Unit, String)> = Vec::new();
    let mut supersede: Vec<String> = Vec::new();
    for unit in &extracted.units {
        let sha = sha256_hex(unit.text.as_bytes());
        match current.iter().find(|c| c.segment == unit.segment) {
            Some(existing)
                if existing.sha256 == sha
                    && existing.from_sequence == unit.from_sequence
                    && existing.to_sequence == unit.to_sequence => {}
            Some(existing) => {
                supersede.push(existing.id.clone());
                to_write.push((unit, sha));
            }
            None => to_write.push((unit, sha)),
        }
    }
    let count = extracted.units.len() as i32;
    for existing in &current {
        if existing.segment > count {
            supersede.push(existing.id.clone());
        }
    }
    let new_bytes: i64 = to_write.iter().map(|(unit, _)| unit.text.len() as i64).sum();
    if bytes_used(&tx, org_id).await?.saturating_add(new_bytes) > budget() {
        tx.rollback().await.map_err(db_error)?;
        set_blocked(&state.database, org_id, true).await?;
        // Back this candidate off like a failure (its state stays as it is):
        // it must not be retried first on every tick (M5).
        let failures = candidate.row.failures.saturating_add(1);
        db::execute(
            &state.database,
            "UPDATE neev_knowledge_sources SET failures=$1,next_check_at=$2 WHERE org_id=$3 AND source_id=$4",
            vec![failures.into(), (now + backoff(failures)).into(), org_id.into(), source.id.clone().into()],
        )
        .await?;
        return Ok(Outcome::QuotaBlocked);
    }
    for id in &supersede {
        db::execute(
            &tx,
            "UPDATE neev_knowledge_snapshots SET state='superseded' WHERE org_id=$1 AND id=$2 AND state='current'",
            vec![org_id.into(), id.clone().into()],
        )
        .await?;
    }
    let mut written_keys: Vec<String> = Vec::new();
    let result: Result<(), KnowledgeError> = async {
        for (unit, sha) in &to_write {
            let key = store::new_key();
            let info = blobs.put(org_id, &key, unit.text.as_bytes()).await?;
            written_keys.push(key.clone());
            if &info.sha256 != sha {
                return Err(KnowledgeError::storage());
            }
            let cite = cite_key(&tx, org_id).await?;
            let id = uuid::Uuid::new_v4().to_string();
            db::execute(
                &tx,
                "INSERT INTO neev_knowledge_snapshots(org_id,id,source_id,source_kind,engine,segment,from_sequence,to_sequence,marker,blob_key,sha256,byte_length,state,captured_at,cite_key) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'current',$13,$14)",
                vec![
                    org_id.into(),
                    id.clone().into(),
                    source.id.clone().into(),
                    source.kind.as_str().into(),
                    candidate.engine.into(),
                    unit.segment.into(),
                    unit.from_sequence.into(),
                    unit.to_sequence.into(),
                    extracted.marker.clone().into(),
                    key.into(),
                    info.sha256.into(),
                    info.byte_length.into(),
                    now.into(),
                    cite.into(),
                ],
            )
            .await?;
            db::execute(
                &tx,
                "INSERT INTO neev_knowledge_coverage(org_id,snapshot_id,state,attempts,updated_at) VALUES($1,$2,'pending',0,$3)",
                vec![org_id.into(), id.into(), now.into()],
            )
            .await?;
        }
        db::execute(
            &tx,
            "UPDATE neev_knowledge_sources SET state='ok',detail='',marker=$1,pending_marker=NULL,pending_seen=0,failures=0,next_check_at=0,checked_at=$2 WHERE org_id=$3 AND source_id=$4",
            vec![extracted.marker.clone().into(), now.into(), org_id.into(), source.id.clone().into()],
        )
        .await?;
        if !to_write.is_empty() || !supersede.is_empty() {
            super::log::append(
                &tx,
                org_id,
                None,
                "snapshot",
                None,
                None,
                &format!(
                    "kind={} written={} superseded={}",
                    source.kind.as_str(),
                    to_write.len(),
                    supersede.len()
                ),
                now,
            )
            .await?;
        }
        Ok(())
    }
    .await;
    let committed = match result {
        Ok(()) => tx.commit().await.map_err(db_error),
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    };
    if let Err(error) = committed {
        // The rows never landed; their blobs must not linger.
        for key in &written_keys {
            let _ = blobs.remove_document(org_id, key).await;
        }
        return Err(error);
    }
    Ok(if to_write.is_empty() && supersede.is_empty() {
        Outcome::Unchanged
    } else {
        Outcome::Written {
            units: to_write.len(),
        }
    })
}

/// One extraction attempt for a ready candidate.
pub(crate) async fn capture_source(
    state: &ServerState,
    upstream: Option<&Upstream>,
    org_id: i32,
    candidate: &Candidate,
    now: i64,
) -> Result<Outcome, KnowledgeError> {
    let source = &candidate.source;
    let extracted = match (source.kind, candidate.engine) {
        (SourceKind::Chat, _) => extract_chat(state, org_id, source).await,
        (SourceKind::Document, false) => extract_native_document(state, org_id, source).await,
        (SourceKind::Document, true) => {
            let Some(upstream) = upstream else {
                return record_problem(state, org_id, candidate, CaptureState::EngineUnavailable, now).await;
            };
            match upstream
                .markdown(&source.id, org_id, candidate.row.marker.as_deref())
                .await
            {
                Ok(Ok(EngineMarkdown::Unchanged { .. })) => {
                    db::execute(
                        &state.database,
                        "UPDATE neev_knowledge_sources SET state='ok',detail='',pending_marker=NULL,pending_seen=0,failures=0,next_check_at=$1,checked_at=$2 WHERE org_id=$3 AND source_id=$4",
                        vec![(now + BACKOFF_MIN_SECS).into(), now.into(), org_id.into(), source.id.clone().into()],
                    )
                    .await?;
                    return Ok(Outcome::Unchanged);
                }
                Ok(Ok(EngineMarkdown::Content { content_sha256, markdown })) => {
                    if candidate.row.marker.as_deref() == Some(content_sha256.as_str()) {
                        return Ok(Outcome::Unchanged);
                    }
                    if candidate.row.pending_marker.as_deref() != Some(content_sha256.as_str()) {
                        // First sighting: debounce, look again next tick.
                        db::execute(
                            &state.database,
                            "UPDATE neev_knowledge_sources SET pending_marker=$1,pending_seen=1,next_check_at=$2,checked_at=$3 WHERE org_id=$4 AND source_id=$5",
                            vec![content_sha256.into(), now.into(), now.into(), org_id.into(), source.id.clone().into()],
                        )
                        .await?;
                        return Ok(Outcome::Pending);
                    }
                    Ok(Extracted {
                        marker: content_sha256,
                        units: render::doc_segments(&source.title, &markdown),
                        max_sequence: None,
                    })
                }
                Ok(Err(MarkdownRefusal::TooLarge)) => {
                    return record_problem(state, org_id, candidate, CaptureState::TooLarge, now).await
                }
                Ok(Err(MarkdownRefusal::NotEngine)) | Err(EngineError::Unavailable) => {
                    return record_problem(state, org_id, candidate, CaptureState::EngineUnavailable, now).await
                }
                Ok(Err(MarkdownRefusal::Unknown | MarkdownRefusal::OrgMismatch)) | Err(_) => {
                    return record_problem(state, org_id, candidate, CaptureState::Failed, now).await
                }
            }
        }
    };
    match extracted {
        Ok(extracted) => write(state, org_id, candidate, extracted, now).await,
        Err(KnowledgeError::Unavailable) => Err(KnowledgeError::Unavailable),
        Err(_) => record_problem(state, org_id, candidate, CaptureState::Failed, now).await,
    }
}

/// One capture tick (production entry point).
pub(crate) async fn run_tick(state: &ServerState, now: i64) -> Result<usize, KnowledgeError> {
    let upstream = state.engine.as_ref().map(|engine| engine.upstream.clone());
    run_tick_with(state, upstream.as_deref(), now).await
}

/// One capture tick with an explicit bridge client. Returns the number of
/// sources whose snapshots were (re)written.
pub(crate) async fn run_tick_with(
    state: &ServerState,
    upstream: Option<&Upstream>,
    now: i64,
) -> Result<usize, KnowledgeError> {
    store::store(state)?;
    let rows = state
        .database
        .query_all(db::statement(
            &state.database,
            "SELECT k.org_id FROM neev_knowledge_settings k JOIN neev_organizations o ON o.id=k.org_id WHERE k.capture_enabled=true AND o.disabled=false ORDER BY k.org_id",
            vec![],
        ))
        .await
        .map_err(db_error)?;
    let mut queues: Vec<(i32, std::collections::VecDeque<Candidate>)> = Vec::new();
    for row in &rows {
        let org_id: i32 = row.try_get("", "org_id").map_err(db_error)?;
        // Budget first: an org over it gets no scan and no writes.
        if bytes_used(&state.database, org_id).await? >= budget() {
            set_blocked(&state.database, org_id, true).await?;
            continue;
        }
        // Not cleared here: `capture_blocked` is cleared only after a tick in
        // which this org wrote and hit no quota refusal, so it never flaps (M5).
        let ready = scan(state, org_id, now).await?;
        queues.push((org_id, ready.into()));
    }
    if queues.is_empty() {
        return Ok(0);
    }
    // Fair share: every org first gets up to an equal share of the tick's
    // budget; anything left over goes round again in org order.
    let share = (MAX_EXTRACTIONS_PER_TICK / queues.len()).max(1);
    let mut plan: Vec<(i32, Candidate)> = Vec::new();
    for (org_id, queue) in queues.iter_mut() {
        for _ in 0..share {
            if plan.len() >= MAX_EXTRACTIONS_PER_TICK {
                break;
            }
            match queue.pop_front() {
                Some(candidate) => plan.push((*org_id, candidate)),
                None => break,
            }
        }
    }
    'fill: loop {
        let mut progressed = false;
        for (org_id, queue) in queues.iter_mut() {
            if plan.len() >= MAX_EXTRACTIONS_PER_TICK {
                break 'fill;
            }
            if let Some(candidate) = queue.pop_front() {
                plan.push((*org_id, candidate));
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    let mut wrote: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut written = 0usize;
    let mut blocked: std::collections::HashSet<i32> = std::collections::HashSet::new();
    // Sequential on purpose: engine conversions run one at a time.
    for (org_id, candidate) in plan {
        if blocked.contains(&org_id) {
            continue;
        }
        match capture_source(state, upstream, org_id, &candidate, now).await {
            Ok(Outcome::Written { units }) if units > 0 => {
                written += 1;
                wrote.insert(org_id);
            }
            Ok(Outcome::QuotaBlocked) => {
                blocked.insert(org_id);
            }
            Ok(_) => {}
            Err(KnowledgeError::Unavailable) => return Err(KnowledgeError::Unavailable),
            // One source's storage fault never stops the others.
            Err(_) => {
                let _ = record_problem(state, org_id, &candidate, CaptureState::Failed, now).await;
            }
        }
    }
    // Cleared only by evidence: this org wrote a snapshot this tick and hit no
    // quota refusal. A backed-off oversized candidate keeps it set (M5).
    for org_id in wrote {
        if !blocked.contains(&org_id) {
            set_blocked(&state.database, org_id, false).await?;
        }
    }
    Ok(written)
}

/// Delete local knowledge blobs that have no snapshot or revision row and are
/// older than `ORPHAN_AGE_SECS` (a failed commit's leftovers). Returns how
/// many were removed. Remote replicas are not purged in this release.
pub(crate) async fn sweep_orphans(state: &ServerState, now: i64) -> Result<usize, KnowledgeError> {
    let blobs = store::store(state)?;
    let Some(root) = state.config.knowledge_root.clone() else {
        return Err(KnowledgeError::Unavailable);
    };
    let mut removed = 0usize;
    let Ok(orgs) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    for org in orgs.flatten() {
        let Some(org_id) = org.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if org_id <= 0 {
            continue;
        }
        let Ok(files) = std::fs::read_dir(org.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(key) = name.to_str().and_then(|n| n.strip_suffix(".md")) else {
                continue;
            };
            if uuid::Uuid::parse_str(key).map(|u| u.hyphenated().to_string() != key).unwrap_or(true) {
                continue;
            }
            let Ok(meta) = file.metadata() else { continue };
            let Some(modified) = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64)
            else {
                continue;
            };
            if now - modified < ORPHAN_AGE_SECS {
                continue;
            }
            let referenced = db::one(
                &state.database,
                "SELECT 1 AS used FROM neev_knowledge_snapshots WHERE org_id=$1 AND blob_key=$2 UNION ALL SELECT 1 AS used FROM neev_knowledge_revisions WHERE org_id=$1 AND blob_key=$2",
                vec![org_id.into(), key.to_owned().into()],
            )
            .await?
            .is_some();
            if !referenced && blobs.remove_document(org_id, key).await.is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}
