//! Admin read models (plan Task 9 as amended: A1.4 DTOs, A3 withheld opacity).
//!
//! Every response is built from readability evaluated in the read's own
//! transaction. A withheld page is opaque: its id, revision and a count —
//! never its slug, title or body — in the page list, page view, index, log,
//! export and provenance.

use super::access::{eligible_predicate, readable_heads};
use super::compile;
use super::jobs::{self, Job};
use super::output::{footnotes, is_cite_key};
use super::render::iso;
use super::store;
use super::types::*;
use crate::neev::{db, startup::ServerState};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) const LOG_PAGE: i64 = 100;
pub(crate) const SOURCE_PAGE: usize = 100;

async fn count<C: ConnectionTrait>(db: &C, sql: &str, org_id: i32) -> Result<i64, KnowledgeError> {
    db::one(db, sql, vec![org_id.into()])
        .await?
        .ok_or_else(KnowledgeError::storage)?
        .try_get("", "n")
        .map_err(db_error)
}

pub(crate) async fn job_dto<C: ConnectionTrait>(db: &C, job: &Job) -> Result<Value, KnowledgeError> {
    let done = db::one(
        db,
        "SELECT COUNT(*) AS n FROM neev_knowledge_batches WHERE org_id=$1 AND job_id=$2 AND state='accepted'",
        vec![job.org_id.into(), job.id.clone().into()],
    )
    .await?
    .ok_or_else(KnowledgeError::storage)?
    .try_get::<i64>("", "n")
    .map_err(db_error)?;
    let remaining = if matches!(job.state, JobState::Running | JobState::Paused) {
        compile::units_remaining(db, job.org_id, job).await?
    } else {
        0
    };
    Ok(json!({
        "id": job.id,
        "mode": job.mode.as_str(),
        "state": job.state.as_str(),
        "provider": job.provider,
        "model": job.model,
        "pauseReason": job.pause_reason,
        "waitReason": job.wait_reason,
        "sessionId": job.session_id,
        "batchesDone": done,
        "unitsRemaining": remaining,
        "startedBy": job.started_by,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
    }))
}

/// A readable active page head with its body.
#[derive(Clone, Debug)]
pub(crate) struct ReadablePage {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub revision: i32,
    pub updated_at: i64,
    pub body: String,
    pub stale: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WithheldPage {
    pub id: String,
    /// Server-side only: used to redact links to this page from readable
    /// bodies. Never serialized.
    pub slug: String,
    pub revision: i32,
    pub unreadable_sources: i64,
    pub updated_at: i64,
}

/// Every active page, split by readability, readable bodies loaded.
pub(crate) async fn pages_split<C: ConnectionTrait>(
    state: &ServerState,
    db: &C,
    org_id: i32,
) -> Result<(Vec<ReadablePage>, Vec<WithheldPage>), KnowledgeError> {
    let blobs = store::store(state)?;
    let readable = readable_heads(db, org_id).await?;
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT p.id,p.slug,p.title,p.head_revision,p.updated_at,v.blob_key,v.sha256,v.byte_length,\
             EXISTS (SELECT 1 FROM neev_knowledge_revision_deps d JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id WHERE d.org_id=p.org_id AND d.page_id=p.id AND d.revision=p.head_revision AND s.state='superseded') AS stale \
             FROM neev_knowledge_pages p JOIN neev_knowledge_revisions v ON v.org_id=p.org_id AND v.page_id=p.id AND v.revision=p.head_revision \
             WHERE p.org_id=$1 AND p.status='active' ORDER BY p.slug",
            vec![org_id.into()],
        ))
        .await
        .map_err(db_error)?;
    let mut open = Vec::new();
    let mut withheld = Vec::new();
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(db_error)?;
        let revision: i32 = row.try_get("", "head_revision").map_err(db_error)?;
        let updated_at: i64 = row.try_get("", "updated_at").map_err(db_error)?;
        match readable.get(&id) {
            Some(Readable::Readable) => {
                let key: String = row.try_get("", "blob_key").map_err(db_error)?;
                let sha: String = row.try_get("", "sha256").map_err(db_error)?;
                let length: i64 = row.try_get("", "byte_length").map_err(db_error)?;
                let body = String::from_utf8(blobs.read(org_id, &key, length, &sha).await?)
                    .map_err(|_| KnowledgeError::storage())?;
                open.push(ReadablePage {
                    id,
                    slug: row.try_get("", "slug").map_err(db_error)?,
                    title: row.try_get("", "title").map_err(db_error)?,
                    revision,
                    updated_at,
                    body,
                    stale: row.try_get("", "stale").map_err(db_error)?,
                });
            }
            Some(Readable::Withheld { unreadable_sources }) => withheld.push(WithheldPage {
                id,
                slug: row.try_get("", "slug").map_err(db_error)?,
                revision,
                unreadable_sources: *unreadable_sources,
                updated_at,
            }),
            None => {}
        }
    }
    // A readable body may still link a page that has since become withheld
    // (its new head depends on a revoked source). Its slug must not show, so
    // such links are redacted on every read.
    let hidden: BTreeSet<String> = withheld.iter().map(|page| page.slug.clone()).collect();
    if !hidden.is_empty() {
        for page in &mut open {
            page.body = redact_links(&page.body, &hidden);
        }
    }
    Ok((open, withheld))
}

/// Replace `[[slug]]` links that name a withheld page.
pub(crate) fn redact_links(body: &str, hidden: &BTreeSet<String>) -> String {
    let pattern = regex::Regex::new(r"\[\[([a-z0-9][a-z0-9-]{0,62})\]\]").expect("static regex");
    pattern
        .replace_all(body, |captures: &regex::Captures| {
            if hidden.contains(&captures[1]) {
                "[[withheld page]]".to_owned()
            } else {
                captures[0].to_owned()
            }
        })
        .into_owned()
}

fn links(body: &str) -> Vec<String> {
    let pattern = regex::Regex::new(r"\[\[([a-z0-9][a-z0-9-]{0,62})\]\]").expect("static regex");
    pattern.captures_iter(body).map(|c| c[1].to_owned()).collect()
}

/// Deterministic lint flags for every readable page (computed on read).
pub(crate) fn lint(pages: &[ReadablePage]) -> HashMap<String, Vec<String>> {
    let slugs: BTreeSet<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    let mut inbound: BTreeMap<&str, usize> = BTreeMap::new();
    for page in pages {
        for target in links(&page.body) {
            if target != page.slug {
                if let Some(slug) = slugs.get(target.as_str()) {
                    *inbound.entry(slug).or_default() += 1;
                }
            }
        }
    }
    let mut out = HashMap::new();
    for page in pages {
        let mut flags = Vec::new();
        if links(&page.body).iter().any(|target| !slugs.contains(target.as_str())) {
            flags.push("dead_link".to_owned());
        }
        if pages.len() > 1 && inbound.get(page.slug.as_str()).copied().unwrap_or(0) == 0 {
            flags.push("orphan".to_owned());
        }
        if page.body.lines().count() > MAX_PAGE_LINES_LINT {
            flags.push("too_long".to_owned());
        }
        if page.stale {
            flags.push("stale".to_owned());
        }
        out.insert(page.id.clone(), flags);
    }
    out
}

/// SQL (alias `r`): the source has at least one current snapshot.
const HAS_CURRENT: &str = "EXISTS (SELECT 1 FROM neev_knowledge_snapshots s WHERE s.org_id=r.org_id AND s.source_id=r.id AND s.state='current')";

/// SQL (alias `r`): the source was captured and is KNOWN EMPTY — capture state
/// `ok`, a non-NULL capture marker, and zero current snapshots (an empty
/// document or a chat with no messages). It has nothing to represent, so it
/// counts as captured and represented (frontend contract D1 carve-out); a
/// source never captured has a NULL marker and never matches.
const KNOWN_EMPTY: &str = "(EXISTS (SELECT 1 FROM neev_knowledge_sources k WHERE k.org_id=r.org_id AND k.source_id=r.id AND k.state='ok' AND k.marker IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM neev_knowledge_snapshots s WHERE s.org_id=r.org_id AND s.source_id=r.id AND s.state='current'))";

/// SQL (alias `r`): every current snapshot is cited (at least one exists).
const ALL_CITED: &str = "(EXISTS (SELECT 1 FROM neev_knowledge_snapshots s WHERE s.org_id=r.org_id AND s.source_id=r.id AND s.state='current') AND NOT EXISTS (SELECT 1 FROM neev_knowledge_snapshots s JOIN neev_knowledge_coverage c ON c.org_id=s.org_id AND c.snapshot_id=s.id WHERE s.org_id=r.org_id AND s.source_id=r.id AND s.state='current' AND c.state<>'cited'))";

pub(crate) async fn compute<C: ConnectionTrait>(state: &ServerState, db: &C, org_id: i32) -> Result<Value, KnowledgeError> {
    let settings = super::settings::get(db, org_id).await?;
    let eligible = eligible_predicate("r");
    let eligible_count = count(db, &format!("SELECT COUNT(*) AS n FROM neev_resources r WHERE r.org_id=$1 AND {eligible}"), org_id).await?;
    let captured = count(
        db,
        &format!("SELECT COUNT(*) AS n FROM neev_resources r WHERE r.org_id=$1 AND {eligible} AND ({HAS_CURRENT} OR {KNOWN_EMPTY})"),
        org_id,
    )
    .await?;
    let represented = count(
        db,
        &format!("SELECT COUNT(*) AS n FROM neev_resources r WHERE r.org_id=$1 AND {eligible} AND ({ALL_CITED} OR {KNOWN_EMPTY})"),
        org_id,
    )
    .await?;
    let mut coverage = BTreeMap::new();
    let rows = db
        .query_all(db::statement(
            db,
            &format!("SELECT c.state,COUNT(*) AS n FROM neev_knowledge_snapshots s JOIN neev_knowledge_coverage c ON c.org_id=s.org_id AND c.snapshot_id=s.id JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id WHERE s.org_id=$1 AND s.state='current' AND {eligible} GROUP BY c.state"),
            vec![org_id.into()],
        ))
        .await
        .map_err(db_error)?;
    for row in &rows {
        let state: String = row.try_get("", "state").map_err(db_error)?;
        let n: i64 = row.try_get("", "n").map_err(db_error)?;
        coverage.insert(state, n);
    }
    let mut problems = BTreeMap::new();
    let rows = db
        .query_all(db::statement(
            db,
            &format!("SELECT k.state,COUNT(*) AS n FROM neev_knowledge_sources k JOIN neev_resources r ON r.org_id=k.org_id AND r.id=k.source_id WHERE k.org_id=$1 AND {eligible} GROUP BY k.state"),
            vec![org_id.into()],
        ))
        .await
        .map_err(db_error)?;
    for row in &rows {
        let state: String = row.try_get("", "state").map_err(db_error)?;
        let n: i64 = row.try_get("", "n").map_err(db_error)?;
        problems.insert(state, n);
    }
    let (open, withheld) = pages_split(state, db, org_id).await?;
    let job = match jobs::current(db, org_id).await? {
        Some(job) => Some(job_dto(db, &job).await?),
        None => None,
    };
    let get = |map: &BTreeMap<String, i64>, key: &str| map.get(key).copied().unwrap_or(0);
    Ok(json!({
        "captureEnabled": settings.capture_enabled,
        "eligible": eligible_count,
        "captured": captured,
        "represented": represented,
        "stale": open.iter().filter(|p| p.stale).count(),
        "pages": open.len() + withheld.len(),
        "withheldPages": withheld.len(),
        "sourceProblems": {
            "tooLarge": get(&problems, "too_large"),
            "engineUnavailable": get(&problems, "engine_unavailable"),
            "failed": get(&problems, "failed"),
        },
        "coverage": {
            "units": coverage.values().sum::<i64>(),
            "cited": get(&coverage, "cited"),
            "skippedByAgent": get(&coverage, "skipped_by_agent"),
            "uncovered": get(&coverage, "uncovered"),
            "pending": get(&coverage, "pending"),
        },
        "captureBlocked": settings.capture_blocked,
        "bytesUsed": super::capture::bytes_used(db, org_id).await?,
        "job": job,
    }))
}

/// Eligible sources only, 100 per page by id.
pub(crate) async fn sources<C: ConnectionTrait>(db: &C, org_id: i32, cursor: Option<&str>) -> Result<Value, KnowledgeError> {
    let eligible = eligible_predicate("r");
    let sql = format!(
        "SELECT r.id,r.title,r.kind,k.state,k.marker,\
         EXISTS (SELECT 1 FROM neev_requests q WHERE q.action='{}' AND q.request_id=r.id) AS bound,\
         ({ALL_CITED} OR {KNOWN_EMPTY}) AS represented,\
         EXISTS (SELECT 1 FROM neev_knowledge_pages p JOIN neev_knowledge_revision_deps d ON d.org_id=p.org_id AND d.page_id=p.id AND d.revision=p.head_revision JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id WHERE p.org_id=r.org_id AND p.status='active' AND s.source_id=r.id AND s.state='superseded') AS stale \
         FROM neev_resources r LEFT JOIN neev_knowledge_sources k ON k.org_id=r.org_id AND k.source_id=r.id \
         WHERE r.org_id=$1 AND r.id>$2 AND {eligible} ORDER BY r.id LIMIT {}",
        crate::neev::engine::binding::ACTION,
        SOURCE_PAGE + 1
    );
    let rows = db
        .query_all(db::statement(db, &sql, vec![org_id.into(), cursor.unwrap_or("").to_owned().into()]))
        .await
        .map_err(db_error)?;
    let mut items = Vec::new();
    for row in rows.iter().take(SOURCE_PAGE) {
        let state: Option<String> = row.try_get("", "state").map_err(db_error)?;
        let marker: Option<String> = row.try_get("", "marker").map_err(db_error)?;
        let capture_state = match state.as_deref() {
            Some("ok") if marker.is_some() => "ok",
            Some("too_large") => "too_large",
            Some("engine_unavailable") => "engine_unavailable",
            Some("failed") => "failed",
            _ => "pending",
        };
        items.push(json!({
            "id": row.try_get::<String>("", "id").map_err(db_error)?,
            "title": row.try_get::<String>("", "title").map_err(db_error)?,
            "kind": row.try_get::<String>("", "kind").map_err(db_error)?,
            "engine": row.try_get::<bool>("", "bound").map_err(db_error)?,
            "captureState": capture_state,
            "represented": row.try_get::<bool>("", "represented").map_err(db_error)?,
            "stale": row.try_get::<bool>("", "stale").map_err(db_error)?,
        }));
    }
    let next = (rows.len() > SOURCE_PAGE)
        .then(|| items.last().and_then(|item| item["id"].as_str().map(str::to_owned)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

pub(crate) async fn pages<C: ConnectionTrait>(state: &ServerState, db: &C, org_id: i32) -> Result<Value, KnowledgeError> {
    let (open, withheld) = pages_split(state, db, org_id).await?;
    let flags = lint(&open);
    let mut items: Vec<Value> = open
        .iter()
        .map(|page| {
            json!({
                "id": page.id,
                "status": "readable",
                "slug": page.slug,
                "title": page.title,
                "revision": page.revision,
                "stale": page.stale,
                "lint": flags.get(&page.id).cloned().unwrap_or_default(),
                "updatedAt": page.updated_at,
            })
        })
        .collect();
    items.extend(withheld.iter().map(|page| {
        json!({
            "id": page.id,
            "status": "withheld",
            "revision": page.revision,
            "unreadableSources": page.unreadable_sources,
            "updatedAt": page.updated_at,
        })
    }));
    Ok(Value::Array(items))
}

/// Citations of a readable revision, resolved through ITS deps; a source no
/// longer eligible names nothing.
async fn citations<C: ConnectionTrait>(db: &C, org_id: i32, page_id: &str, revision: i32, body: &str) -> Result<Vec<Value>, KnowledgeError> {
    let mut keys: Vec<String> = Vec::new();
    for marker in footnotes(body) {
        if is_cite_key(&marker) && !keys.contains(&marker) {
            keys.push(marker);
        }
    }
    let eligible = eligible_predicate("r");
    let mut out = Vec::new();
    for key in keys {
        let row = db::one(
            db,
            &format!(
                "SELECT s.source_id,s.source_kind,s.segment,r.title,r.node_id,({eligible}) AS eligible FROM neev_knowledge_revision_deps d \
                 JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id \
                 JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id \
                 WHERE d.org_id=$1 AND d.page_id=$2 AND d.revision=$3 AND s.cite_key=$4"
            ),
            vec![org_id.into(), page_id.to_owned().into(), revision.into(), key[2..].to_owned().into()],
        )
        .await?;
        match row {
            Some(row) if row.try_get::<bool>("", "eligible").map_err(db_error)? => out.push(json!({
                "key": key,
                "withheld": false,
                "sourceId": row.try_get::<String>("", "source_id").map_err(db_error)?,
                "title": row.try_get::<String>("", "title").map_err(db_error)?,
                "kind": row.try_get::<String>("", "source_kind").map_err(db_error)?,
                "nodeId": row.try_get::<i32>("", "node_id").map_err(db_error)?,
                "segment": row.try_get::<i32>("", "segment").map_err(db_error)?,
            })),
            _ => out.push(json!({ "key": key, "withheld": true })),
        }
    }
    Ok(out)
}

/// One active page by its opaque id. `None` for unknown, archived or another
/// org's page.
pub(crate) async fn page<C: ConnectionTrait>(state: &ServerState, db: &C, org_id: i32, page_id: &str) -> Result<Option<Value>, KnowledgeError> {
    let (open, withheld) = pages_split(state, db, org_id).await?;
    if let Some(page) = withheld.iter().find(|p| p.id == page_id) {
        return Ok(Some(json!({
            "id": page.id,
            "revision": page.revision,
            "status": "withheld",
            "unreadableSources": page.unreadable_sources,
        })));
    }
    let flags = lint(&open);
    let Some(page) = open.iter().find(|p| p.id == page_id) else {
        return Ok(None);
    };
    Ok(Some(json!({
        "id": page.id,
        "slug": page.slug,
        "title": page.title,
        "revision": page.revision,
        "status": "readable",
        "markdown": page.body,
        "citations": citations(db, org_id, &page.id, page.revision, &page.body).await?,
        "stale": page.stale,
        "lint": flags.get(&page.id).cloned().unwrap_or_default(),
    })))
}

fn summary_line(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("");
    line.chars().take(160).collect()
}

pub(crate) fn index_markdown(open: &[ReadablePage], withheld: &[WithheldPage]) -> String {
    let mut out = String::from("# Knowledge index\n\n");
    for page in open {
        out.push_str(&format!("- [[{}]] {} — {}\n", page.slug, page.title, summary_line(&page.body)));
    }
    for page in withheld {
        out.push_str(&format!(
            "- (withheld page {}, {} sources no longer shared)\n",
            page.id, page.unreadable_sources
        ));
    }
    out
}

pub(crate) async fn log_entries<C: ConnectionTrait>(state: &ServerState, db: &C, org_id: i32, before: Option<i64>) -> Result<Value, KnowledgeError> {
    let (open, _) = pages_split(state, db, org_id).await?;
    let titles: HashMap<&str, &str> = open.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
    let rows = db
        .query_all(db::statement(
            db,
            &format!("SELECT seq,at,actor_id,event,page_id,detail FROM neev_knowledge_log WHERE org_id=$1 AND seq<$2 ORDER BY seq DESC LIMIT {LOG_PAGE}"),
            vec![org_id.into(), before.unwrap_or(i64::MAX).into()],
        ))
        .await
        .map_err(db_error)?;
    let mut items = Vec::new();
    for row in &rows {
        let page_id: Option<String> = row.try_get("", "page_id").map_err(db_error)?;
        let title = page_id.as_deref().and_then(|id| titles.get(id)).map(|t| (*t).to_owned());
        items.push(json!({
            "seq": row.try_get::<i64>("", "seq").map_err(db_error)?,
            "at": row.try_get::<i64>("", "at").map_err(db_error)?,
            "event": row.try_get::<String>("", "event").map_err(db_error)?,
            "actorId": row.try_get::<Option<i32>>("", "actor_id").map_err(db_error)?,
            "pageId": page_id,
            "pageTitle": title,
            "detail": row.try_get::<String>("", "detail").map_err(db_error)?,
        }));
    }
    Ok(Value::Array(items))
}

pub(crate) async fn log_markdown<C: ConnectionTrait>(db: &C, org_id: i32, open: &[ReadablePage]) -> Result<String, KnowledgeError> {
    let titles: HashMap<&str, &str> = open.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT seq,at,event,page_id,detail FROM neev_knowledge_log WHERE org_id=$1 ORDER BY seq",
            vec![org_id.into()],
        ))
        .await
        .map_err(db_error)?;
    let mut out = String::from("# Knowledge log\n\n");
    for row in &rows {
        let page_id: Option<String> = row.try_get("", "page_id").map_err(db_error)?;
        let title = page_id.as_deref().and_then(|id| titles.get(id)).copied().unwrap_or("-");
        let at: i64 = row.try_get("", "at").map_err(db_error)?;
        let event: String = row.try_get("", "event").map_err(db_error)?;
        let detail: String = row.try_get("", "detail").map_err(db_error)?;
        out.push_str(&format!("- {} {event} {title} {detail}\n", iso(at)));
    }
    Ok(out)
}
