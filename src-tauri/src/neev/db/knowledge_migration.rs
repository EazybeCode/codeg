//! Schema 22 — org knowledge (plan Task 1 with Amendment 1 §A1.2).
//!
//! Twelve tables holding captured source snapshots, compiled wiki pages and
//! their revisions, compile jobs/batches, per-snapshot coverage and an
//! append-only log. Additive and table-guarded: it creates tables and indexes
//! and UPDATEs no existing row.
//!
//! The DDL strings below are the single source of truth. SQLite stores them
//! verbatim, which is what `validate` compares; on PostgreSQL the columns,
//! named constraints and indexes are derived from the same strings.
//!
//! Delete semantics (deliberately mixed, review M2): `settings`, `pages` and
//! `log` cascade from their organization; every other table references its
//! parent without a cascade, so deleting a resource with snapshots, or an org
//! with knowledge history, is refused rather than silently destroying it. No
//! org or resource delete path exists today; revisit if one is added.
//!
//! The guarded downgrade refuses while ANY knowledge row exists (that is user
//! data a schema-21 binary cannot represent; the answer is to keep 22), and
//! otherwise drops exactly these tables, restoring the schema-21 catalog.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(crate) const SCHEMA_VERSION: i64 = 22;

pub(crate) const TABLE_NAMES: &[&str] = &[
    "neev_knowledge_settings",
    "neev_knowledge_sources",
    "neev_knowledge_snapshots",
    "neev_knowledge_coverage",
    "neev_knowledge_pages",
    "neev_knowledge_revisions",
    "neev_knowledge_revision_deps",
    "neev_knowledge_jobs",
    "neev_knowledge_batches",
    "neev_knowledge_batch_units",
    "neev_knowledge_batch_pages",
    "neev_knowledge_log",
];

/// Children before parents (Amendment 1 §A1.2). Also used by test fixtures
/// that rewind a fresh database to an older inventory.
pub(crate) const DROP_ORDER: [&str; 12] = [
    "neev_knowledge_log",
    "neev_knowledge_batch_pages",
    "neev_knowledge_batch_units",
    "neev_knowledge_revision_deps",
    "neev_knowledge_revisions",
    "neev_knowledge_batches",
    "neev_knowledge_jobs",
    "neev_knowledge_pages",
    "neev_knowledge_coverage",
    "neev_knowledge_snapshots",
    "neev_knowledge_sources",
    "neev_knowledge_settings",
];

/// (table, DDL) in creation order: every referenced table precedes its referrer.
const TABLES: [(&str, &str); 12] = [
    ("neev_knowledge_settings", "CREATE TABLE neev_knowledge_settings (org_id integer NOT NULL, capture_enabled boolean NOT NULL, capture_blocked text, revision bigint NOT NULL, updated_by integer NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_knowledge_settings_pk PRIMARY KEY (org_id), CONSTRAINT neev_knowledge_settings_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE, CONSTRAINT neev_knowledge_settings_by FOREIGN KEY (updated_by) REFERENCES neev_accounts (id), CONSTRAINT neev_knowledge_settings_rev CHECK (revision>0), CONSTRAINT neev_knowledge_settings_blocked CHECK (capture_blocked IS NULL OR capture_blocked IN ('quota_exceeded')))"),
    ("neev_knowledge_sources", "CREATE TABLE neev_knowledge_sources (org_id integer NOT NULL, source_id text NOT NULL, state text NOT NULL, detail text NOT NULL, marker text, pending_marker text, pending_seen integer NOT NULL DEFAULT 0, next_check_at bigint NOT NULL DEFAULT 0, failures integer NOT NULL DEFAULT 0, checked_at bigint NOT NULL, CONSTRAINT neev_knowledge_sources_pk PRIMARY KEY (org_id, source_id), CONSTRAINT neev_knowledge_sources_state CHECK (state IN ('ok','too_large','engine_unavailable','failed')), CONSTRAINT neev_knowledge_sources_detail CHECK (length(detail)<=200), CONSTRAINT neev_knowledge_sources_res FOREIGN KEY (org_id, source_id) REFERENCES neev_resources (org_id, id))"),
    ("neev_knowledge_snapshots", "CREATE TABLE neev_knowledge_snapshots (org_id integer NOT NULL, id text NOT NULL, source_id text NOT NULL, source_kind text NOT NULL, engine boolean NOT NULL, segment integer NOT NULL, from_sequence bigint, to_sequence bigint, marker text NOT NULL, blob_key text NOT NULL, sha256 text NOT NULL, byte_length bigint NOT NULL, state text NOT NULL, captured_at bigint NOT NULL, cite_key text NOT NULL, CONSTRAINT neev_knowledge_snapshots_pk PRIMARY KEY (org_id, id), CONSTRAINT neev_knowledge_snapshots_kind CHECK (source_kind IN ('document','chat')), CONSTRAINT neev_knowledge_snapshots_state CHECK (state IN ('current','superseded')), CONSTRAINT neev_knowledge_snapshots_len CHECK (byte_length>=0 AND byte_length<=65536), CONSTRAINT neev_knowledge_snapshots_seg CHECK (segment>=0), CONSTRAINT neev_knowledge_snapshots_cite UNIQUE (org_id, cite_key), CONSTRAINT neev_knowledge_snapshots_cite_len CHECK (length(cite_key)=12), CONSTRAINT neev_knowledge_snapshots_res FOREIGN KEY (org_id, source_id) REFERENCES neev_resources (org_id, id))"),
    ("neev_knowledge_coverage", "CREATE TABLE neev_knowledge_coverage (org_id integer NOT NULL, snapshot_id text NOT NULL, state text NOT NULL, attempts integer NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_knowledge_coverage_pk PRIMARY KEY (org_id, snapshot_id), CONSTRAINT neev_knowledge_coverage_state CHECK (state IN ('pending','cited','skipped_by_agent','uncovered')), CONSTRAINT neev_knowledge_coverage_snap FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots (org_id, id))"),
    ("neev_knowledge_pages", "CREATE TABLE neev_knowledge_pages (org_id integer NOT NULL, id text NOT NULL, slug text NOT NULL, title text NOT NULL, head_revision integer, status text NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_knowledge_pages_pk PRIMARY KEY (org_id, id), CONSTRAINT neev_knowledge_pages_slug UNIQUE (org_id, slug), CONSTRAINT neev_knowledge_pages_status CHECK (status IN ('active','archived')), CONSTRAINT neev_knowledge_pages_title CHECK (length(title)>=1 AND length(title)<=120), CONSTRAINT neev_knowledge_pages_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE)"),
    ("neev_knowledge_jobs", "CREATE TABLE neev_knowledge_jobs (org_id integer NOT NULL, id text NOT NULL, mode text NOT NULL, state text NOT NULL, started_by integer NOT NULL, provider text NOT NULL, model text NOT NULL, pause_reason text, session_id text, wait_reason text, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_knowledge_jobs_pk PRIMARY KEY (org_id, id), CONSTRAINT neev_knowledge_jobs_mode CHECK (mode IN ('incremental','rebuild')), CONSTRAINT neev_knowledge_jobs_state CHECK (state IN ('running','paused','completed','completed_with_gaps','stopped')), CONSTRAINT neev_knowledge_jobs_wait CHECK (wait_reason IS NULL OR wait_reason IN ('slot')), CONSTRAINT neev_knowledge_jobs_by FOREIGN KEY (started_by) REFERENCES neev_accounts (id))"),
    ("neev_knowledge_batches", "CREATE TABLE neev_knowledge_batches (org_id integer NOT NULL, id text NOT NULL, job_id text NOT NULL, seq integer NOT NULL, state text NOT NULL, turn_id text, reason text, provider text NOT NULL, model text NOT NULL, instructions_sha256 text NOT NULL, created_at bigint NOT NULL, started_at bigint, settled_at bigint, CONSTRAINT neev_knowledge_batches_pk PRIMARY KEY (org_id, id), CONSTRAINT neev_knowledge_batches_state CHECK (state IN ('running','accepted','discarded','failed')), CONSTRAINT neev_knowledge_batches_job FOREIGN KEY (org_id, job_id) REFERENCES neev_knowledge_jobs (org_id, id))"),
    ("neev_knowledge_revisions", "CREATE TABLE neev_knowledge_revisions (org_id integer NOT NULL, page_id text NOT NULL, revision integer NOT NULL, blob_key text NOT NULL, sha256 text NOT NULL, byte_length bigint NOT NULL, parent_revision integer, batch_id text NOT NULL, created_at bigint NOT NULL, CONSTRAINT neev_knowledge_revisions_pk PRIMARY KEY (org_id, page_id, revision), CONSTRAINT neev_knowledge_revisions_len CHECK (byte_length>0 AND byte_length<=65536), CONSTRAINT neev_knowledge_revisions_page FOREIGN KEY (org_id, page_id) REFERENCES neev_knowledge_pages (org_id, id), CONSTRAINT neev_knowledge_revisions_batch FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches (org_id, id))"),
    ("neev_knowledge_revision_deps", "CREATE TABLE neev_knowledge_revision_deps (org_id integer NOT NULL, page_id text NOT NULL, revision integer NOT NULL, snapshot_id text NOT NULL, CONSTRAINT neev_knowledge_revision_deps_pk PRIMARY KEY (org_id, page_id, revision, snapshot_id), CONSTRAINT neev_knowledge_revision_deps_rev FOREIGN KEY (org_id, page_id, revision) REFERENCES neev_knowledge_revisions (org_id, page_id, revision), CONSTRAINT neev_knowledge_revision_deps_snap FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots (org_id, id))"),
    ("neev_knowledge_batch_units", "CREATE TABLE neev_knowledge_batch_units (org_id integer NOT NULL, batch_id text NOT NULL, label text NOT NULL, snapshot_id text NOT NULL, skip_reason text, CONSTRAINT neev_knowledge_batch_units_pk PRIMARY KEY (org_id, batch_id, label), CONSTRAINT neev_knowledge_batch_units_batch FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches (org_id, id), CONSTRAINT neev_knowledge_batch_units_snap FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots (org_id, id))"),
    ("neev_knowledge_batch_pages", "CREATE TABLE neev_knowledge_batch_pages (org_id integer NOT NULL, batch_id text NOT NULL, page_id text NOT NULL, revision integer NOT NULL, role text NOT NULL, CONSTRAINT neev_knowledge_batch_pages_pk PRIMARY KEY (org_id, batch_id, page_id), CONSTRAINT neev_knowledge_batch_pages_role CHECK (role IN ('staged','indexed')), CONSTRAINT neev_knowledge_batch_pages_batch FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches (org_id, id), CONSTRAINT neev_knowledge_batch_pages_rev FOREIGN KEY (org_id, page_id, revision) REFERENCES neev_knowledge_revisions (org_id, page_id, revision))"),
    ("neev_knowledge_log", "CREATE TABLE neev_knowledge_log (org_id integer NOT NULL, seq bigint NOT NULL, at bigint NOT NULL, actor_id integer, event text NOT NULL, page_id text, job_id text, detail text NOT NULL, CONSTRAINT neev_knowledge_log_pk PRIMARY KEY (org_id, seq), CONSTRAINT neev_knowledge_log_detail CHECK (length(detail)<=500), CONSTRAINT neev_knowledge_log_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE)"),
];

/// (index, table, DDL). Created right after their table.
const INDEXES: [(&str, &str, &str); 3] = [
    ("neev_knowledge_sources_queue", "neev_knowledge_sources", "CREATE INDEX neev_knowledge_sources_queue ON neev_knowledge_sources (org_id, next_check_at, checked_at)"),
    ("neev_knowledge_snapshots_current", "neev_knowledge_snapshots", "CREATE UNIQUE INDEX neev_knowledge_snapshots_current ON neev_knowledge_snapshots (org_id, source_id, segment) WHERE state='current'"),
    ("neev_knowledge_jobs_active", "neev_knowledge_jobs", "CREATE UNIQUE INDEX neev_knowledge_jobs_active ON neev_knowledge_jobs (org_id) WHERE state IN ('running','paused')"),
];

/// PostgreSQL 16 (production `postgres:16`) rendering of every named table
/// constraint: `(table, name, pg_get_constraintdef)`. Captured from 16.15 and
/// compared exactly (whitespace-collapsed), so a same-named object with a
/// different target, bound, column list or predicate is refused.
const PG_CONSTRAINTS: [(&str, &str, &str); 50] = [
    ("neev_knowledge_batch_pages", "neev_knowledge_batch_pages_batch", "FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches(org_id, id)"),
    ("neev_knowledge_batch_pages", "neev_knowledge_batch_pages_pk", "PRIMARY KEY (org_id, batch_id, page_id)"),
    ("neev_knowledge_batch_pages", "neev_knowledge_batch_pages_rev", "FOREIGN KEY (org_id, page_id, revision) REFERENCES neev_knowledge_revisions(org_id, page_id, revision)"),
    ("neev_knowledge_batch_pages", "neev_knowledge_batch_pages_role", "CHECK ((role = ANY (ARRAY['staged'::text, 'indexed'::text])))"),
    ("neev_knowledge_batch_units", "neev_knowledge_batch_units_batch", "FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches(org_id, id)"),
    ("neev_knowledge_batch_units", "neev_knowledge_batch_units_pk", "PRIMARY KEY (org_id, batch_id, label)"),
    ("neev_knowledge_batch_units", "neev_knowledge_batch_units_snap", "FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots(org_id, id)"),
    ("neev_knowledge_batches", "neev_knowledge_batches_job", "FOREIGN KEY (org_id, job_id) REFERENCES neev_knowledge_jobs(org_id, id)"),
    ("neev_knowledge_batches", "neev_knowledge_batches_pk", "PRIMARY KEY (org_id, id)"),
    ("neev_knowledge_batches", "neev_knowledge_batches_state", "CHECK ((state = ANY (ARRAY['running'::text, 'accepted'::text, 'discarded'::text, 'failed'::text])))"),
    ("neev_knowledge_coverage", "neev_knowledge_coverage_pk", "PRIMARY KEY (org_id, snapshot_id)"),
    ("neev_knowledge_coverage", "neev_knowledge_coverage_snap", "FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots(org_id, id)"),
    ("neev_knowledge_coverage", "neev_knowledge_coverage_state", "CHECK ((state = ANY (ARRAY['pending'::text, 'cited'::text, 'skipped_by_agent'::text, 'uncovered'::text])))"),
    ("neev_knowledge_jobs", "neev_knowledge_jobs_by", "FOREIGN KEY (started_by) REFERENCES neev_accounts(id)"),
    ("neev_knowledge_jobs", "neev_knowledge_jobs_mode", "CHECK ((mode = ANY (ARRAY['incremental'::text, 'rebuild'::text])))"),
    ("neev_knowledge_jobs", "neev_knowledge_jobs_pk", "PRIMARY KEY (org_id, id)"),
    ("neev_knowledge_jobs", "neev_knowledge_jobs_state", "CHECK ((state = ANY (ARRAY['running'::text, 'paused'::text, 'completed'::text, 'completed_with_gaps'::text, 'stopped'::text])))"),
    ("neev_knowledge_jobs", "neev_knowledge_jobs_wait", "CHECK (((wait_reason IS NULL) OR (wait_reason = 'slot'::text)))"),
    ("neev_knowledge_log", "neev_knowledge_log_detail", "CHECK ((length(detail) <= 500))"),
    ("neev_knowledge_log", "neev_knowledge_log_org", "FOREIGN KEY (org_id) REFERENCES neev_organizations(id) ON DELETE CASCADE"),
    ("neev_knowledge_log", "neev_knowledge_log_pk", "PRIMARY KEY (org_id, seq)"),
    ("neev_knowledge_pages", "neev_knowledge_pages_org", "FOREIGN KEY (org_id) REFERENCES neev_organizations(id) ON DELETE CASCADE"),
    ("neev_knowledge_pages", "neev_knowledge_pages_pk", "PRIMARY KEY (org_id, id)"),
    ("neev_knowledge_pages", "neev_knowledge_pages_slug", "UNIQUE (org_id, slug)"),
    ("neev_knowledge_pages", "neev_knowledge_pages_status", "CHECK ((status = ANY (ARRAY['active'::text, 'archived'::text])))"),
    ("neev_knowledge_pages", "neev_knowledge_pages_title", "CHECK (((length(title) >= 1) AND (length(title) <= 120)))"),
    ("neev_knowledge_revision_deps", "neev_knowledge_revision_deps_pk", "PRIMARY KEY (org_id, page_id, revision, snapshot_id)"),
    ("neev_knowledge_revision_deps", "neev_knowledge_revision_deps_rev", "FOREIGN KEY (org_id, page_id, revision) REFERENCES neev_knowledge_revisions(org_id, page_id, revision)"),
    ("neev_knowledge_revision_deps", "neev_knowledge_revision_deps_snap", "FOREIGN KEY (org_id, snapshot_id) REFERENCES neev_knowledge_snapshots(org_id, id)"),
    ("neev_knowledge_revisions", "neev_knowledge_revisions_batch", "FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches(org_id, id)"),
    ("neev_knowledge_revisions", "neev_knowledge_revisions_len", "CHECK (((byte_length > 0) AND (byte_length <= 65536)))"),
    ("neev_knowledge_revisions", "neev_knowledge_revisions_page", "FOREIGN KEY (org_id, page_id) REFERENCES neev_knowledge_pages(org_id, id)"),
    ("neev_knowledge_revisions", "neev_knowledge_revisions_pk", "PRIMARY KEY (org_id, page_id, revision)"),
    ("neev_knowledge_settings", "neev_knowledge_settings_blocked", "CHECK (((capture_blocked IS NULL) OR (capture_blocked = 'quota_exceeded'::text)))"),
    ("neev_knowledge_settings", "neev_knowledge_settings_by", "FOREIGN KEY (updated_by) REFERENCES neev_accounts(id)"),
    ("neev_knowledge_settings", "neev_knowledge_settings_org", "FOREIGN KEY (org_id) REFERENCES neev_organizations(id) ON DELETE CASCADE"),
    ("neev_knowledge_settings", "neev_knowledge_settings_pk", "PRIMARY KEY (org_id)"),
    ("neev_knowledge_settings", "neev_knowledge_settings_rev", "CHECK ((revision > 0))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_cite", "UNIQUE (org_id, cite_key)"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_cite_len", "CHECK ((length(cite_key) = 12))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_kind", "CHECK ((source_kind = ANY (ARRAY['document'::text, 'chat'::text])))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_len", "CHECK (((byte_length >= 0) AND (byte_length <= 65536)))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_pk", "PRIMARY KEY (org_id, id)"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_res", "FOREIGN KEY (org_id, source_id) REFERENCES neev_resources(org_id, id)"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_seg", "CHECK ((segment >= 0))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_state", "CHECK ((state = ANY (ARRAY['current'::text, 'superseded'::text])))"),
    ("neev_knowledge_sources", "neev_knowledge_sources_detail", "CHECK ((length(detail) <= 200))"),
    ("neev_knowledge_sources", "neev_knowledge_sources_pk", "PRIMARY KEY (org_id, source_id)"),
    ("neev_knowledge_sources", "neev_knowledge_sources_res", "FOREIGN KEY (org_id, source_id) REFERENCES neev_resources(org_id, id)"),
    ("neev_knowledge_sources", "neev_knowledge_sources_state", "CHECK ((state = ANY (ARRAY['ok'::text, 'too_large'::text, 'engine_unavailable'::text, 'failed'::text])))"),
];

/// `pg_indexes.indexdef` of every non-constraint index, schema-unqualified.
const PG_INDEXES: [(&str, &str, &str); 3] = [
    ("neev_knowledge_jobs", "neev_knowledge_jobs_active", "CREATE UNIQUE INDEX neev_knowledge_jobs_active ON neev_knowledge_jobs USING btree (org_id) WHERE (state = ANY (ARRAY['running'::text, 'paused'::text]))"),
    ("neev_knowledge_snapshots", "neev_knowledge_snapshots_current", "CREATE UNIQUE INDEX neev_knowledge_snapshots_current ON neev_knowledge_snapshots USING btree (org_id, source_id, segment) WHERE (state = 'current'::text)"),
    ("neev_knowledge_sources", "neev_knowledge_sources_queue", "CREATE INDEX neev_knowledge_sources_queue ON neev_knowledge_sources USING btree (org_id, next_check_at, checked_at)"),
];

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ColumnSpec {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub has_default: bool,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ConstraintSpec {
    pub name: String,
    /// PostgreSQL `contype`: p, u, c or f.
    pub kind: char,
    /// Quoted literals of a CHECK (parser tests).
    pub literals: Vec<String>,
}

/// Pure: the column list of one of this module's DDL strings (everything
/// between the opening parenthesis and the first table constraint).
pub(super) fn columns(ddl: &str) -> Vec<ColumnSpec> {
    let start = ddl.find('(').map_or(ddl.len(), |i| i + 1);
    let end = ddl.find(", CONSTRAINT ").unwrap_or(ddl.len());
    ddl[start..end]
        .split(", ")
        .map(|definition| {
            let mut words = definition.split(' ');
            let name = words.next().unwrap_or_default().to_owned();
            let data_type = words.next().unwrap_or_default().to_owned();
            ColumnSpec {
                name,
                data_type,
                nullable: !definition.contains(" NOT NULL"),
                has_default: definition.contains(" DEFAULT "),
            }
        })
        .collect()
}

/// Pure: the named table constraints of one of this module's DDL strings; the
/// tests use it to prove `PG_CONSTRAINTS` names exactly the DDL's constraints.
#[cfg(test)]
pub(super) fn constraints(ddl: &str) -> Vec<ConstraintSpec> {
    ddl.split("CONSTRAINT ")
        .skip(1)
        .map(|part| {
            let mut words = part.split(' ');
            let name = words.next().unwrap_or_default().to_owned();
            let kind = match words.next().unwrap_or_default() {
                "PRIMARY" => 'p',
                "UNIQUE" => 'u',
                "CHECK" => 'c',
                "FOREIGN" => 'f',
                _ => '?',
            };
            let literals = if kind == 'c' {
                part.split('\'')
                    .skip(1)
                    .step_by(2)
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            };
            ConstraintSpec { name, kind, literals }
        })
        .collect()
}

async fn table_exists<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1 AND c.relkind IN ('r','p')",
        DbBackend::Sqlite => "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
        _ => return Err(AuthError::Unavailable),
    };
    Ok(super::one(db, sql, vec![table.into()]).await?.is_some())
}

/// Create every absent table (and its indexes), then prove the exact shape.
/// Idempotent; runs in the caller's transaction.
pub(crate) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    for (table, ddl) in TABLES {
        if table_exists(tx, table).await? {
            continue;
        }
        super::execute(tx, ddl, vec![]).await?;
        for (_, indexed, index_ddl) in INDEXES {
            if indexed == table {
                super::execute(tx, index_ddl, vec![]).await?;
            }
        }
    }
    validate(tx).await
}

/// Prove the exact schema-22 knowledge shape. Safe on a transaction or a pooled
/// connection.
pub(crate) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    match db.get_database_backend() {
        DbBackend::Sqlite => validate_sqlite(db).await,
        DbBackend::Postgres => validate_postgres(db).await,
        _ => Err(AuthError::Unavailable),
    }
}

async fn validate_sqlite<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for (table, ddl) in TABLES {
        let stored: String = super::one(
            db,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
            vec![table.into()],
        )
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "sql")
        .map_err(database_error)?;
        if stored != ddl {
            return Err(AuthError::Unavailable);
        }
        // Exactly this table's declared indexes (plus SQLite's implicit ones);
        // no stray trigger, view or index.
        let rows = db
            .query_all(super::statement(
                db,
                "SELECT name,type,sql FROM sqlite_master WHERE tbl_name=$1 AND type<>'table' AND name NOT LIKE 'sqlite_autoindex_%' ORDER BY name",
                vec![table.into()],
            ))
            .await
            .map_err(database_error)?;
        let expected: Vec<(&str, &str)> = INDEXES
            .iter()
            .filter(|(_, indexed, _)| *indexed == table)
            .map(|(name, _, ddl)| (*name, *ddl))
            .collect();
        if rows.len() != expected.len() {
            return Err(AuthError::Unavailable);
        }
        for (row, (name, ddl)) in rows.iter().zip(expected) {
            let actual: (String, String, Option<String>) = (
                row.try_get("", "name").map_err(database_error)?,
                row.try_get("", "type").map_err(database_error)?,
                row.try_get("", "sql").map_err(database_error)?,
            );
            if actual != (name.to_owned(), "index".to_owned(), Some(ddl.to_owned())) {
                return Err(AuthError::Unavailable);
            }
        }
    }
    Ok(())
}

/// Collapse whitespace and drop this schema's qualifier, so a definition reads
/// the same in the production `public` schema and an isolated test schema.
fn normalize(definition: &str, schema: &str) -> String {
    definition
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(&format!(" {schema}."), " ")
        .replace(" public.", " ")
}

async fn validate_postgres<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let schema: String = super::one(db, "SELECT current_schema()::text AS schema", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "schema")
        .map_err(database_error)?;
    for (table, ddl) in TABLES {
        if !table_exists(db, table).await? {
            return Err(AuthError::Unavailable);
        }
        let mut expected = columns(ddl);
        expected.sort_by(|a, b| a.name.cmp(&b.name));
        let rows = db
            .query_all(super::statement(
                db,
                "SELECT column_name::text AS name,data_type::text AS kind,is_nullable::text AS nullable,(column_default IS NOT NULL) AS has_default FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY column_name",
                vec![table.into()],
            ))
            .await
            .map_err(database_error)?;
        if rows.len() != expected.len() {
            return Err(AuthError::Unavailable);
        }
        for (row, column) in rows.iter().zip(&expected) {
            let actual = ColumnSpec {
                name: row.try_get("", "name").map_err(database_error)?,
                data_type: row.try_get("", "kind").map_err(database_error)?,
                nullable: row.try_get::<String>("", "nullable").map_err(database_error)? == "YES",
                has_default: row.try_get("", "has_default").map_err(database_error)?,
            };
            if &actual != column {
                return Err(AuthError::Unavailable);
            }
        }
        // Every named table constraint, exactly: name AND full definition.
        // (NOT NULL constraints are catalogued from PostgreSQL 18 on; only the
        // p/u/c/f table constraints are compared.)
        let rows = db
            .query_all(super::statement(
                db,
                "SELECT k.conname::text AS name,pg_get_constraintdef(k.oid) AS def FROM pg_constraint k JOIN pg_class t ON t.oid=k.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() AND t.relname=$1 AND k.contype IN ('p','u','c','f') ORDER BY k.conname",
                vec![table.into()],
            ))
            .await
            .map_err(database_error)?;
        let actual = rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get::<String>("", "name").map_err(database_error)?,
                    normalize(&row.try_get::<String>("", "def").map_err(database_error)?, &schema),
                ))
            })
            .collect::<Result<Vec<_>, AuthError>>()?;
        let mut expected: Vec<(String, String)> = PG_CONSTRAINTS
            .iter()
            .filter(|(owner, _, _)| *owner == table)
            .map(|(_, name, def)| ((*name).to_owned(), normalize(def, &schema)))
            .collect();
        expected.sort();
        if actual != expected {
            return Err(AuthError::Unavailable);
        }
        // Indexes that do not back a constraint: exactly the declared ones, with
        // their columns, uniqueness and predicate.
        let rows = db
            .query_all(super::statement(
                db,
                "SELECT i.indexname::text AS name,i.indexdef::text AS def FROM pg_indexes i WHERE i.schemaname=current_schema() AND i.tablename=$1 AND NOT EXISTS (SELECT 1 FROM pg_constraint k JOIN pg_class t ON t.oid=k.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() AND t.relname=$1 AND k.conname=i.indexname) ORDER BY i.indexname",
                vec![table.into()],
            ))
            .await
            .map_err(database_error)?;
        let actual = rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get::<String>("", "name").map_err(database_error)?,
                    normalize(&row.try_get::<String>("", "def").map_err(database_error)?, &schema),
                ))
            })
            .collect::<Result<Vec<_>, AuthError>>()?;
        let mut expected: Vec<(String, String)> = PG_INDEXES
            .iter()
            .filter(|(owner, _, _)| *owner == table)
            .map(|(_, name, def)| ((*name).to_owned(), normalize(def, &schema)))
            .collect();
        expected.sort();
        if actual != expected {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

/// What a 22 -> 21 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// The knowledge tables are gone and the marker is 21; nothing else changed.
    Downgraded,
    /// At least one knowledge row exists: user data a schema-21 binary cannot
    /// represent. Keep 22 and repair forward; never delete or restore over it.
    Refused,
    /// The marker is not 22.
    NotAtTwentyTwo(i64),
}

/// Take a schema-22 database back to 21. GUARDED: refuses unless the marker is
/// exactly 22, the shape is exactly schema 22, and every knowledge table is
/// empty. Runs in the caller's transaction; the caller commits only on
/// `Downgraded`.
pub(crate) async fn downgrade(tx: &DatabaseTransaction) -> Result<Downgrade, AuthError> {
    let version: i64 = super::one(
        tx,
        "SELECT version FROM neev_schema_version WHERE id=1",
        vec![],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "version")
    .map_err(database_error)?;
    if version != SCHEMA_VERSION {
        return Ok(Downgrade::NotAtTwentyTwo(version));
    }
    validate(tx).await?;
    // PostgreSQL: take every table's ACCESS EXCLUSIVE lock BEFORE the emptiness
    // probes. Otherwise a still-running schema-22 app could commit a row between
    // the probe and the DROP, and the DROP (which waits for the same lock) would
    // silently destroy it. With the lock held first, an in-flight writer either
    // commits before the probes (and is seen: Refused) or waits until this
    // transaction ends. The advisory migration lock does not cover app writers.
    // SQLite has a single writer: a concurrent commit fails this transaction.
    if tx.get_database_backend() == DbBackend::Postgres {
        super::execute(
            tx,
            &format!("LOCK TABLE {} IN ACCESS EXCLUSIVE MODE", TABLE_NAMES.join(", ")),
            vec![],
        )
        .await?;
    }
    for table in TABLE_NAMES {
        if super::one(tx, &format!("SELECT 1 AS present FROM {table} LIMIT 1"), vec![])
            .await?
            .is_some()
        {
            return Ok(Downgrade::Refused);
        }
    }
    for table in DROP_ORDER {
        super::execute(tx, &format!("DROP TABLE {table}"), vec![]).await?;
    }
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::node_sharing_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    for table in TABLE_NAMES {
        if table_exists(tx, table).await? {
            return Err(AuthError::Unavailable);
        }
    }
    super::node_sharing_migration::validate(tx).await?;
    Ok(Downgrade::Downgraded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_has_ddl_a_drop_slot_and_parsable_shape() {
        let created: Vec<&str> = TABLES.iter().map(|(name, _)| *name).collect();
        for list in [&created[..], &DROP_ORDER[..]] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            let mut names = TABLE_NAMES.to_vec();
            names.sort_unstable();
            assert_eq!(sorted, names);
        }
        for (table, ddl) in TABLES {
            assert!(ddl.starts_with(&format!("CREATE TABLE {table} (")), "{table}");
            let parsed = constraints(ddl);
            assert!(parsed.iter().all(|c| c.kind != '?' && c.name.starts_with(table)), "{table}");
            assert_eq!(parsed.iter().filter(|c| c.kind == 'p').count(), 1, "{table}");
            assert!(columns(ddl).iter().all(|c| matches!(c.data_type.as_str(), "integer" | "bigint" | "text" | "boolean")), "{table}");
        }
    }

    #[test]
    fn postgres_expectations_name_exactly_the_ddl_objects() {
        for (table, ddl) in TABLES {
            let mut declared: Vec<String> = constraints(ddl).into_iter().map(|c| c.name).collect();
            declared.sort();
            let mut pinned: Vec<String> = PG_CONSTRAINTS
                .iter()
                .filter(|(owner, _, _)| *owner == table)
                .map(|(_, name, _)| (*name).to_owned())
                .collect();
            pinned.sort();
            assert_eq!(declared, pinned, "{table}");
        }
        let mut declared: Vec<(&str, &str)> = INDEXES.iter().map(|(n, t, _)| (*t, *n)).collect();
        let mut pinned: Vec<(&str, &str)> = PG_INDEXES.iter().map(|(t, n, _)| (*t, *n)).collect();
        declared.sort_unstable();
        pinned.sort_unstable();
        assert_eq!(declared, pinned);
        // The tenant-carrying batch FK (review M1) is part of the schema.
        assert!(PG_CONSTRAINTS.iter().any(|(_, name, def)| *name == "neev_knowledge_batch_pages_batch"
            && *def == "FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches(org_id, id)"));
        assert_eq!(
            normalize("CREATE INDEX x ON  neev_test_ab.t USING btree (a)", "neev_test_ab"),
            "CREATE INDEX x ON t USING btree (a)"
        );
    }

    #[test]
    fn the_parser_reads_amendment_columns_and_check_literals() {
        let sources = TABLES[1].1;
        let parsed = columns(sources);
        let pending = parsed.iter().find(|c| c.name == "pending_seen").unwrap();
        assert_eq!(
            (pending.data_type.as_str(), pending.nullable, pending.has_default),
            ("integer", false, true)
        );
        assert!(parsed.iter().find(|c| c.name == "marker").unwrap().nullable);
        let jobs = constraints(TABLES[5].1);
        let state = jobs.iter().find(|c| c.name == "neev_knowledge_jobs_state").unwrap();
        assert_eq!(
            state.literals,
            ["running", "paused", "completed", "completed_with_gaps", "stopped"]
        );
    }
}
