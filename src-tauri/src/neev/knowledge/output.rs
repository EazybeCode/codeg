//! Output validation and accept (plan Task 8 as amended: A1.3 citations,
//! coverage and accept order; B2/B3, C1/C2, G2/G4).
//!
//! Generated files are NEVER stored as attachments for this purpose: the run
//! driver hands the spool's files here and cleans the spool afterwards.

use super::access::{is_eligible, revision_readable};
use super::store;
use super::types::*;
use crate::neev::{agents::generated::GeneratedFile, db, startup::ServerState};
use sea_orm::{ConnectionTrait, TransactionTrait};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutPage {
    pub slug: String,
    pub title: String,
    pub markdown: String,
    /// Every citation the body makes: `uNN` labels and `k-…` keys.
    pub cited: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedOutput {
    pub pages: Vec<OutPage>,
    pub skipped: Vec<(String, String)>,
    pub ignored: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputError {
    UnknownCitation(String),
    BadTitle(String),
    TooLarge(String),
    TooManyPages,
    NotUtf8(String),
    BadSkipJson,
    DuplicateSlug(String),
}

const SKIP_FILE: &str = "knowledge-skipped.json";
const MAX_SKIP_REASON_CHARS: usize = 200;

fn page_slug(name: &str) -> Option<&str> {
    let slug = name.strip_prefix("wiki--")?.strip_suffix(".md")?;
    let mut bytes = slug.bytes();
    let first = bytes.next()?;
    let valid = (first.is_ascii_lowercase() || first.is_ascii_digit())
        && slug.len() <= 63
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then_some(slug)
}

/// Every `[^…]` footnote marker in a body (the marker text between `[^` and `]`).
pub(crate) fn footnotes(body: &str) -> Vec<String> {
    let pattern = regex::Regex::new(r"\[\^([^\]\s]{1,64})\]").expect("static regex");
    pattern
        .captures_iter(body)
        .map(|captures| captures[1].to_owned())
        .collect()
}

pub(crate) fn is_unit_label(value: &str) -> bool {
    value.len() == 3
        && value.starts_with('u')
        && value[1..].bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn is_cite_key(value: &str) -> bool {
    value.len() == 14
        && value.starts_with("k-")
        && value[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validate a batch's output files.
///
/// - `wiki--<slug>.md` pages: UTF-8, at most `MAX_PAGE_BYTES`, a `# Title` of
///   1–120 chars, at most `MAX_PAGES_PER_BATCH`.
/// - Citations: `[^uNN]` must be a staged label, `[^k-…]` must be in
///   `allowed_keys` (the cite keys of the deps of staged/indexed revisions and
///   of the batch's own units); anything else fails the whole batch.
/// - `knowledge-skipped.json`: `[{"unit":"uNN","reason":"…"}]`.
/// - Any other file is ignored (and logged by the caller).
pub(crate) fn parse_outputs(
    files: &[GeneratedFile],
    labels: &BTreeSet<String>,
    allowed_keys: &BTreeSet<String>,
) -> Result<ParsedOutput, OutputError> {
    let mut parsed = ParsedOutput::default();
    let mut slugs = BTreeSet::new();
    for file in files {
        if file.name == SKIP_FILE {
            let value: serde_json::Value =
                serde_json::from_slice(&file.bytes).map_err(|_| OutputError::BadSkipJson)?;
            let items = value.as_array().ok_or(OutputError::BadSkipJson)?;
            let mut seen = BTreeSet::new();
            for item in items {
                let unit = item
                    .get("unit")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(OutputError::BadSkipJson)?;
                let reason = item
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(OutputError::BadSkipJson)?;
                if !labels.contains(unit) {
                    parsed.ignored.push(format!("skip:{}", unit.chars().take(16).collect::<String>()));
                    continue;
                }
                if seen.insert(unit.to_owned()) {
                    let reason: String = reason
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(MAX_SKIP_REASON_CHARS)
                        .collect();
                    parsed.skipped.push((unit.to_owned(), reason));
                }
            }
            continue;
        }
        let Some(slug) = page_slug(&file.name) else {
            parsed.ignored.push(file.name.clone());
            continue;
        };
        if file.bytes.len() > MAX_PAGE_BYTES {
            return Err(OutputError::TooLarge(slug.to_owned()));
        }
        let markdown =
            String::from_utf8(file.bytes.clone()).map_err(|_| OutputError::NotUtf8(slug.to_owned()))?;
        let first = markdown.lines().next().unwrap_or("");
        let title = first
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|t| (1..=120).contains(&t.chars().count()) && !t.chars().any(char::is_control))
            .ok_or_else(|| OutputError::BadTitle(slug.to_owned()))?
            .to_owned();
        let mut cited = BTreeSet::new();
        for marker in footnotes(&markdown) {
            let ok = (is_unit_label(&marker) && labels.contains(&marker))
                || (is_cite_key(&marker) && allowed_keys.contains(&marker));
            if !ok {
                return Err(OutputError::UnknownCitation(marker));
            }
            cited.insert(marker);
        }
        if !slugs.insert(slug.to_owned()) {
            return Err(OutputError::DuplicateSlug(slug.to_owned()));
        }
        parsed.pages.push(OutPage {
            slug: slug.to_owned(),
            title,
            markdown,
            cited,
        });
        if parsed.pages.len() > MAX_PAGES_PER_BATCH {
            return Err(OutputError::TooManyPages);
        }
    }
    Ok(parsed)
}

/// The staged unit labels of a batch.
pub(crate) async fn batch_labels<C: ConnectionTrait>(db: &C, org_id: i32, batch_id: &str) -> Result<BTreeSet<String>, KnowledgeError> {
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT label FROM neev_knowledge_batch_units WHERE org_id=$1 AND batch_id=$2",
            vec![org_id.into(), batch_id.to_owned().into()],
        ))
        .await
        .map_err(db_error)?;
    rows.iter()
        .map(|row| row.try_get::<String>("", "label").map_err(db_error))
        .collect()
}

/// `k-…` keys a batch may cite: its units' and every dep of its staged and
/// indexed page revisions.
pub(crate) async fn allowed_keys<C: ConnectionTrait>(db: &C, org_id: i32, batch_id: &str) -> Result<BTreeSet<String>, KnowledgeError> {
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT s.cite_key FROM neev_knowledge_batch_units u JOIN neev_knowledge_snapshots s ON s.org_id=u.org_id AND s.id=u.snapshot_id WHERE u.org_id=$1 AND u.batch_id=$2 \
             UNION SELECT s.cite_key FROM neev_knowledge_batch_pages p JOIN neev_knowledge_revision_deps d ON d.org_id=p.org_id AND d.page_id=p.page_id AND d.revision=p.revision JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id WHERE p.org_id=$1 AND p.batch_id=$2",
            vec![org_id.into(), batch_id.to_owned().into()],
        ))
        .await
        .map_err(db_error)?;
    rows.iter()
        .map(|row| row.try_get::<String>("", "cite_key").map(|k| format!("k-{k}")).map_err(db_error))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcceptOutcome {
    Accepted {
        pages: usize,
        cited: usize,
        skipped: usize,
        ignored: usize,
    },
    /// Nothing stored; the batch is `discarded` (or `failed` for `stopped` /
    /// `invalid_output`) and the job paused with `reason` unless stopped.
    Discarded { reason: PauseReason },
    /// The batch was already settled (e.g. timed out); nothing done.
    NotRunning,
}

/// The server-composed reply stored on a knowledge turn: counts and codes
/// only, never content.
pub(crate) fn status_line(seq: i32, outcome: &Result<AcceptOutcome, PauseReason>) -> String {
    match outcome {
        Ok(AcceptOutcome::Accepted { pages, cited, skipped, ignored }) => format!(
            "Knowledge batch {seq} accepted: {pages} page(s) written, {cited} unit(s) cited, {skipped} skipped, {ignored} output(s) ignored."
        ),
        Ok(AcceptOutcome::Discarded { reason }) | Err(reason) => format!(
            "Knowledge batch {seq} not accepted ({}); nothing was stored.",
            reason.as_str()
        ),
        Ok(AcceptOutcome::NotRunning) => {
            format!("Knowledge batch {seq} was already settled; nothing was stored.")
        }
    }
}

/// Settle a running batch as failed and pause its (running) job. A stopped
/// job stays stopped. Idempotent: a batch that is not running is untouched.
pub(crate) async fn settle_failed(
    state: &ServerState,
    org_id: i32,
    batch_id: &str,
    reason: PauseReason,
    now: i64,
) -> Result<(), KnowledgeError> {
    let tx = db::begin(&state.database).await?;
    let Some(row) = db::one(
        &tx,
        "SELECT job_id FROM neev_knowledge_batches WHERE org_id=$1 AND id=$2",
        vec![org_id.into(), batch_id.to_owned().into()],
    )
    .await?
    else {
        tx.rollback().await.map_err(db_error)?;
        return Ok(());
    };
    let job_id: String = row.try_get("", "job_id").map_err(db_error)?;
    // Job, then batch: the order accept and stop use (B1).
    super::jobs::lock_job(&tx, org_id, &job_id).await?;
    super::jobs::lock_batch(&tx, org_id, batch_id).await?;
    let changed = db::execute(
        &tx,
        "UPDATE neev_knowledge_batches SET state='failed',reason=$1,settled_at=$2 WHERE org_id=$3 AND id=$4 AND state='running'",
        vec![reason.as_str().into(), now.into(), org_id.into(), batch_id.to_owned().into()],
    )
    .await?
    .rows_affected();
    if changed != 1 {
        // Already settled by someone else; nothing to do.
        tx.rollback().await.map_err(db_error)?;
        return Ok(());
    }
    pause_running_job(&tx, org_id, &job_id, reason, now).await?;
    super::log::append(&tx, org_id, None, "batch_failed", None, Some(&job_id), &format!("reason={}", reason.as_str()), now).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

pub(crate) async fn pause_running_job<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    job_id: &str,
    reason: PauseReason,
    now: i64,
) -> Result<(), KnowledgeError> {
    db::execute(
        db,
        "UPDATE neev_knowledge_jobs SET state='paused',pause_reason=$1,wait_reason=NULL,updated_at=$2 WHERE org_id=$3 AND id=$4 AND state='running'",
        vec![reason.as_str().into(), now.into(), org_id.into(), job_id.to_owned().into()],
    )
    .await?;
    Ok(())
}

struct UnitRow {
    label: String,
    snapshot_id: String,
    source_id: String,
    cite_key: String,
}

/// Rewrite `[^uNN]` to `[^k-<cite_key>]`.
pub(crate) fn rewrite_labels(markdown: &str, keys: &BTreeMap<String, String>) -> String {
    let pattern = regex::Regex::new(r"\[\^(u[0-9]{2})\]").expect("static regex");
    pattern
        .replace_all(markdown, |captures: &regex::Captures| match keys.get(&captures[1]) {
            Some(key) => format!("[^k-{key}]"),
            None => captures[0].to_owned(),
        })
        .into_owned()
}

/// Accept a batch's validated output, all in one transaction (A1.3 order):
/// re-check unit sources, require staged/indexed revisions still readable AND
/// still heads, union deps, write pages, record coverage, mark accepted. Any
/// failed check discards the batch before a single blob is written.
pub(crate) async fn accept(
    state: &ServerState,
    org_id: i32,
    batch_id: &str,
    parsed: ParsedOutput,
    now: i64,
) -> Result<AcceptOutcome, KnowledgeError> {
    let blobs = store::store(state)?;
    let tx = db::begin(&state.database).await?;
    // Lock the job row, then the batch row, BEFORE reading their states, so a
    // concurrent stop or timeout either commits first (and this sees it) or
    // waits until this accept ends (B1).
    let Some(owner) = db::one(
        &tx,
        "SELECT job_id FROM neev_knowledge_batches WHERE org_id=$1 AND id=$2",
        vec![org_id.into(), batch_id.to_owned().into()],
    )
    .await?
    else {
        tx.rollback().await.map_err(db_error)?;
        return Ok(AcceptOutcome::NotRunning);
    };
    let owner: String = owner.try_get("", "job_id").map_err(db_error)?;
    super::jobs::lock_job(&tx, org_id, &owner).await?;
    super::jobs::lock_batch(&tx, org_id, batch_id).await?;
    let Some(batch) = db::one(
        &tx,
        "SELECT b.job_id,j.mode,j.state FROM neev_knowledge_batches b JOIN neev_knowledge_jobs j ON j.org_id=b.org_id AND j.id=b.job_id WHERE b.org_id=$1 AND b.id=$2 AND b.state='running'",
        vec![org_id.into(), batch_id.to_owned().into()],
    )
    .await?
    else {
        tx.rollback().await.map_err(db_error)?;
        return Ok(AcceptOutcome::NotRunning);
    };
    let job_id: String = batch.try_get("", "job_id").map_err(db_error)?;
    let mode = JobMode::parse(&batch.try_get::<String>("", "mode").map_err(db_error)?)
        .ok_or_else(KnowledgeError::storage)?;
    let job_state: String = batch.try_get("", "state").map_err(db_error)?;

    async fn refuse<C: ConnectionTrait>(
        tx: &C,
        org_id: i32,
        batch_id: &str,
        job_id: &str,
        batch_state: &str,
        reason: PauseReason,
        now: i64,
    ) -> Result<AcceptOutcome, KnowledgeError> {
        db::execute(
            tx,
            "UPDATE neev_knowledge_batches SET state=$1,reason=$2,settled_at=$3 WHERE org_id=$4 AND id=$5 AND state='running'",
            vec![batch_state.into(), reason.as_str().into(), now.into(), org_id.into(), batch_id.to_owned().into()],
        )
        .await?;
        pause_running_job(tx, org_id, job_id, reason, now).await?;
        super::log::append(
            tx,
            org_id,
            None,
            if batch_state == "discarded" { "batch_discarded" } else { "batch_failed" },
            None,
            Some(job_id),
            &format!("reason={}", reason.as_str()),
            now,
        )
        .await?;
        Ok(AcceptOutcome::Discarded { reason })
    }

    // A stopped job stores nothing, even if its run finished first.
    if job_state == "stopped" {
        let outcome = refuse(&tx, org_id, batch_id, &job_id, "failed", PauseReason::Stopped, now).await?;
        tx.commit().await.map_err(db_error)?;
        return Ok(outcome);
    }
    let unit_rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT u.label,u.snapshot_id,s.source_id,s.cite_key FROM neev_knowledge_batch_units u JOIN neev_knowledge_snapshots s ON s.org_id=u.org_id AND s.id=u.snapshot_id WHERE u.org_id=$1 AND u.batch_id=$2 ORDER BY u.label",
            vec![org_id.into(), batch_id.to_owned().into()],
        ))
        .await
        .map_err(db_error)?;
    let mut units = Vec::with_capacity(unit_rows.len());
    for row in &unit_rows {
        units.push(UnitRow {
            label: row.try_get("", "label").map_err(db_error)?,
            snapshot_id: row.try_get("", "snapshot_id").map_err(db_error)?,
            source_id: row.try_get("", "source_id").map_err(db_error)?,
            cite_key: row.try_get("", "cite_key").map_err(db_error)?,
        });
    }
    // 1. Unit sources still eligible (FOR SHARE on PostgreSQL).
    let sources: BTreeSet<&str> = units.iter().map(|u| u.source_id.as_str()).collect();
    for source in &sources {
        if tx.get_database_backend() == sea_orm::DbBackend::Postgres {
            db::one(
                &tx,
                "SELECT id FROM neev_resources WHERE org_id=$1 AND id=$2 FOR SHARE",
                vec![org_id.into(), (*source).to_owned().into()],
            )
            .await?;
        }
        if is_eligible(&tx, org_id, source).await?.is_none() {
            let outcome = refuse(&tx, org_id, batch_id, &job_id, "discarded", PauseReason::SourceRevoked, now).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(outcome);
        }
    }
    // 2. Staged and indexed revisions still readable and still heads.
    let page_rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT bp.page_id,bp.revision,bp.role,p.head_revision,p.status FROM neev_knowledge_batch_pages bp JOIN neev_knowledge_pages p ON p.org_id=bp.org_id AND p.id=bp.page_id WHERE bp.org_id=$1 AND bp.batch_id=$2",
            vec![org_id.into(), batch_id.to_owned().into()],
        ))
        .await
        .map_err(db_error)?;
    let mut staged_pages: BTreeMap<String, i32> = BTreeMap::new();
    let mut batch_revisions: Vec<(String, i32)> = Vec::new();
    for row in &page_rows {
        let page_id: String = row.try_get("", "page_id").map_err(db_error)?;
        let revision: i32 = row.try_get("", "revision").map_err(db_error)?;
        let role: String = row.try_get("", "role").map_err(db_error)?;
        let head: Option<i32> = row.try_get("", "head_revision").map_err(db_error)?;
        let status: String = row.try_get("", "status").map_err(db_error)?;
        if head != Some(revision) || status != "active" {
            let outcome = refuse(&tx, org_id, batch_id, &job_id, "discarded", PauseReason::PageMoved, now).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(outcome);
        }
        if revision_readable(&tx, org_id, &page_id, revision).await? != Readable::Readable {
            let outcome = refuse(&tx, org_id, batch_id, &job_id, "discarded", PauseReason::SourceRevoked, now).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(outcome);
        }
        if role == "staged" {
            staged_pages.insert(page_id.clone(), revision);
        }
        batch_revisions.push((page_id, revision));
    }
    // 3. dep_set = every unit ∪ the deps of every staged/indexed revision.
    let mut dep_set: BTreeSet<String> = units.iter().map(|u| u.snapshot_id.clone()).collect();
    for (page_id, revision) in &batch_revisions {
        let rows = tx
            .query_all(db::statement(
                &tx,
                "SELECT snapshot_id FROM neev_knowledge_revision_deps WHERE org_id=$1 AND page_id=$2 AND revision=$3",
                vec![org_id.into(), page_id.clone().into(), (*revision).into()],
            ))
            .await
            .map_err(db_error)?;
        for row in &rows {
            dep_set.insert(row.try_get("", "snapshot_id").map_err(db_error)?);
        }
    }
    // 4. Decide what is stored, then prove every stored body fits.
    let keys: BTreeMap<String, String> = units.iter().map(|u| (u.label.clone(), u.cite_key.clone())).collect();
    let mut ignored = parsed.ignored.len();
    let mut writes: Vec<(OutPage, String, Option<(String, Option<i32>)>)> = Vec::new();
    // Cite keys the replaced head revisions carried, per written page (B2).
    let mut replaced_keys: BTreeSet<String> = BTreeSet::new();
    let mut replaced_pages: BTreeSet<String> = BTreeSet::new();
    for page in parsed.pages {
        let existing = db::one(
            &tx,
            "SELECT id,head_revision,status FROM neev_knowledge_pages WHERE org_id=$1 AND slug=$2",
            vec![org_id.into(), page.slug.clone().into()],
        )
        .await?;
        let existing = match existing {
            Some(row) => {
                let id: String = row.try_get("", "id").map_err(db_error)?;
                let head: Option<i32> = row.try_get("", "head_revision").map_err(db_error)?;
                let status: String = row.try_get("", "status").map_err(db_error)?;
                if mode == JobMode::Incremental && status == "active" && !staged_pages.contains_key(&id) {
                    // An existing page this batch was not given: never overwritten.
                    ignored += 1;
                    continue;
                }
                Some((id, head))
            }
            None => None,
        };
        if let Some((id, Some(head))) = &existing {
            let old = db::one(
                &tx,
                "SELECT blob_key,sha256,byte_length FROM neev_knowledge_revisions WHERE org_id=$1 AND page_id=$2 AND revision=$3",
                vec![org_id.into(), id.clone().into(), (*head).into()],
            )
            .await?
            .ok_or_else(KnowledgeError::storage)?;
            let old = blobs
                .read(
                    org_id,
                    &old.try_get::<String>("", "blob_key").map_err(db_error)?,
                    old.try_get("", "byte_length").map_err(db_error)?,
                    &old.try_get::<String>("", "sha256").map_err(db_error)?,
                )
                .await?;
            let old = String::from_utf8(old).map_err(|_| KnowledgeError::storage())?;
            replaced_keys.extend(footnotes(&old).into_iter().filter(|m| is_cite_key(m)));
            replaced_pages.insert(id.clone());
        }
        let body = rewrite_labels(&page.markdown, &keys);
        if body.len() > MAX_PAGE_BYTES {
            let outcome = refuse(&tx, org_id, batch_id, &job_id, "failed", PauseReason::InvalidOutput, now).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(outcome);
        }
        writes.push((page, body, existing));
    }
    // 5. Write pages (blobs only now, after every check passed).
    let mut written_keys: Vec<String> = Vec::new();
    let result: Result<(usize, usize, usize), KnowledgeError> = async {
        let mut stored_bodies: Vec<String> = Vec::new();
        for (page, body, existing) in &writes {
            let key = store::new_key();
            let info = blobs.put(org_id, &key, body.as_bytes()).await?;
            written_keys.push(key.clone());
            let (page_id, head) = match existing {
                Some((id, head)) => (id.clone(), *head),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    db::execute(
                        &tx,
                        "INSERT INTO neev_knowledge_pages(org_id,id,slug,title,head_revision,status,created_at,updated_at) VALUES($1,$2,$3,$4,NULL,'active',$5,$5)",
                        vec![org_id.into(), id.clone().into(), page.slug.clone().into(), page.title.clone().into(), now.into()],
                    )
                    .await?;
                    (id, None)
                }
            };
            let revision = head.unwrap_or(0) + 1;
            let parent = staged_pages.get(&page_id).copied().filter(|_| head.is_some());
            db::execute(
                &tx,
                "INSERT INTO neev_knowledge_revisions(org_id,page_id,revision,blob_key,sha256,byte_length,parent_revision,batch_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                vec![
                    org_id.into(),
                    page_id.clone().into(),
                    revision.into(),
                    key.into(),
                    info.sha256.into(),
                    info.byte_length.into(),
                    parent.into(),
                    batch_id.to_owned().into(),
                    now.into(),
                ],
            )
            .await?;
            for snapshot in &dep_set {
                db::execute(
                    &tx,
                    "INSERT INTO neev_knowledge_revision_deps(org_id,page_id,revision,snapshot_id) VALUES($1,$2,$3,$4)",
                    vec![org_id.into(), page_id.clone().into(), revision.into(), snapshot.clone().into()],
                )
                .await?;
            }
            db::execute(
                &tx,
                "UPDATE neev_knowledge_pages SET head_revision=$1,title=$2,status='active',updated_at=$3 WHERE org_id=$4 AND id=$5",
                vec![revision.into(), page.title.clone().into(), now.into(), org_id.into(), page_id.clone().into()],
            )
            .await?;
            super::log::append(&tx, org_id, None, "page_revision", Some(&page_id), Some(&job_id), &format!("revision={revision}"), now).await?;
            stored_bodies.push(body.clone());
        }
        // 6. Coverage, from STORED pages only.
        let mut cited_markers: BTreeSet<String> = BTreeSet::new();
        for body in &stored_bodies {
            cited_markers.extend(footnotes(body));
        }
        // 6a. A citation the rewrite dropped: if no stored body and no other
        // active head still carries it, its snapshot is no longer represented
        // anywhere and goes back to pending (B2: no false coverage).
        let dropped: BTreeSet<String> = replaced_keys.difference(&cited_markers).cloned().collect();
        if !dropped.is_empty() {
            let others = tx
                .query_all(db::statement(
                    &tx,
                    "SELECT p.id,v.blob_key,v.sha256,v.byte_length FROM neev_knowledge_pages p JOIN neev_knowledge_revisions v ON v.org_id=p.org_id AND v.page_id=p.id AND v.revision=p.head_revision WHERE p.org_id=$1 AND p.status='active'",
                    vec![org_id.into()],
                ))
                .await
                .map_err(db_error)?;
            let mut still: BTreeSet<String> = BTreeSet::new();
            for row in &others {
                let id: String = row.try_get("", "id").map_err(db_error)?;
                if replaced_pages.contains(&id) {
                    continue;
                }
                let body = blobs
                    .read(
                        org_id,
                        &row.try_get::<String>("", "blob_key").map_err(db_error)?,
                        row.try_get("", "byte_length").map_err(db_error)?,
                        &row.try_get::<String>("", "sha256").map_err(db_error)?,
                    )
                    .await?;
                still.extend(footnotes(&String::from_utf8_lossy(&body)));
            }
            for key in dropped.difference(&still) {
                db::execute(
                    &tx,
                    "UPDATE neev_knowledge_coverage SET state='pending',attempts=0,updated_at=$1 WHERE org_id=$2 AND state='cited' AND snapshot_id IN (SELECT id FROM neev_knowledge_snapshots WHERE org_id=$2 AND cite_key=$3)",
                    vec![now.into(), org_id.into(), key[2..].to_owned().into()],
                )
                .await?;
            }
        }
        let skips: BTreeMap<&str, &str> = parsed.skipped.iter().map(|(u, r)| (u.as_str(), r.as_str())).collect();
        let (mut cited, mut skipped) = (0usize, 0usize);
        for unit in &units {
            let attempts: i32 = db::one(
                &tx,
                "SELECT attempts FROM neev_knowledge_coverage WHERE org_id=$1 AND snapshot_id=$2",
                vec![org_id.into(), unit.snapshot_id.clone().into()],
            )
            .await?
            .map(|row| row.try_get("", "attempts"))
            .transpose()
            .map_err(db_error)?
            .unwrap_or(0);
            let (coverage, attempts) = if cited_markers.contains(&format!("k-{}", unit.cite_key)) {
                cited += 1;
                (Coverage::Cited, attempts)
            } else if let Some(reason) = skips.get(unit.label.as_str()) {
                skipped += 1;
                db::execute(
                    &tx,
                    "UPDATE neev_knowledge_batch_units SET skip_reason=$1 WHERE org_id=$2 AND batch_id=$3 AND label=$4",
                    vec![(*reason).to_owned().into(), org_id.into(), batch_id.to_owned().into(), unit.label.clone().into()],
                )
                .await?;
                (Coverage::SkippedByAgent, attempts)
            } else {
                let next = attempts.saturating_add(1);
                if next >= UNCOVERED_AFTER_ATTEMPTS {
                    (Coverage::Uncovered, next)
                } else {
                    (Coverage::Pending, next)
                }
            };
            db::execute(
                &tx,
                "UPDATE neev_knowledge_coverage SET state=$1,attempts=$2,updated_at=$3 WHERE org_id=$4 AND snapshot_id=$5",
                vec![coverage.as_str().into(), attempts.into(), now.into(), org_id.into(), unit.snapshot_id.clone().into()],
            )
            .await?;
        }
        // 7. Accepted — only if the batch is still running under our lock.
        let accepted = db::execute(
            &tx,
            "UPDATE neev_knowledge_batches SET state='accepted',settled_at=$1 WHERE org_id=$2 AND id=$3 AND state='running'",
            vec![now.into(), org_id.into(), batch_id.to_owned().into()],
        )
        .await?
        .rows_affected();
        if accepted != 1 {
            return Err(KnowledgeError::conflict());
        }
        // A settled batch changes what the job reports (batchesDone): bump
        // the job's updatedAt so the UI's freshness rule sees it (contract D4).
        db::execute(
            &tx,
            "UPDATE neev_knowledge_jobs SET updated_at=$1 WHERE org_id=$2 AND id=$3",
            vec![now.into(), org_id.into(), job_id.clone().into()],
        )
        .await?;
        super::log::append(
            &tx,
            org_id,
            None,
            "batch_accepted",
            None,
            Some(&job_id),
            &format!("pages={} cited={cited} skipped={skipped} ignored={ignored}", stored_bodies.len()),
            now,
        )
        .await?;
        Ok((stored_bodies.len(), cited, skipped))
    }
    .await;
    let committed = match result {
        Ok(counts) => tx.commit().await.map_err(db_error).map(|_| counts),
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    };
    match committed {
        Ok((pages, cited, skipped)) => Ok(AcceptOutcome::Accepted { pages, cited, skipped, ignored }),
        Err(error) => {
            for key in &written_keys {
                let _ = blobs.remove_document(org_id, key).await;
            }
            Err(error)
        }
    }
}

/// Settle a knowledge run's outcome from its spool files. Used by the run
/// driver; returns the outcome for the status line.
pub(crate) async fn settle_run(
    state: &ServerState,
    org_id: i32,
    batch_id: &str,
    completed: bool,
    canceled: bool,
    files: Result<Vec<GeneratedFile>, ()>,
    now: i64,
) -> Result<AcceptOutcome, PauseReason> {
    if !completed {
        // A canceled run was stopped or timed out; those paths settle the
        // batch themselves, and this call is then a no-op.
        let reason = if canceled { PauseReason::Stopped } else { PauseReason::RunFailed };
        let _ = settle_failed(state, org_id, batch_id, reason, now).await;
        return Err(reason);
    }
    let Ok(files) = files else {
        let _ = settle_failed(state, org_id, batch_id, PauseReason::InvalidOutput, now).await;
        return Err(PauseReason::InvalidOutput);
    };
    let labels = batch_labels(&state.database, org_id, batch_id).await;
    let keys = allowed_keys(&state.database, org_id, batch_id).await;
    let (Ok(labels), Ok(keys)) = (labels, keys) else {
        let _ = settle_failed(state, org_id, batch_id, PauseReason::RunFailed, now).await;
        return Err(PauseReason::RunFailed);
    };
    let parsed = match parse_outputs(&files, &labels, &keys) {
        Ok(parsed) => parsed,
        Err(_) => {
            let _ = settle_failed(state, org_id, batch_id, PauseReason::InvalidOutput, now).await;
            return Err(PauseReason::InvalidOutput);
        }
    };
    match accept(state, org_id, batch_id, parsed, now).await {
        Ok(outcome) => Ok(outcome),
        Err(_) => {
            let _ = settle_failed(state, org_id, batch_id, PauseReason::RunFailed, now).await;
            Err(PauseReason::RunFailed)
        }
    }
}
