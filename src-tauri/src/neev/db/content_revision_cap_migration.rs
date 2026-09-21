//! Forward step, folded into schema 13: raise the per-document revision ceiling.
//!
//! `neev_document_revisions` is created (schema 3, `content_migration`) with a hard
//! CHECK `revision BETWEEN 1 AND 1000`. That upper bound IS the per-document revision
//! quota, and 1000 makes AUTOSAVE unusable — a busy document exhausts it in a single
//! sitting and can then never be saved again. The application ceiling now lives in
//! `content::documents::MAX_DOCUMENT_REVISIONS`; this raises the database's
//! defence-in-depth CHECK to match, WITHOUT deleting any history, dropping any
//! revision, or changing a single revision/blob reference.
//!
//! Additive, forward-only, and IDEMPOTENT like every native migration: a database
//! already carrying the raised bound is left untouched, so a replayed migration is a
//! no-op. Aggregate storage stays protected by the per-org byte budget
//! (`neev_content_usage.charged_bytes <= 1 GiB`); this is only the row-count ceiling.
//!
//! Wiring (owned by the schema-13 integration in `db::mod`): call
//! [`raise_document_revision_cap`] on both the fresh-install path and the upgrade
//! tail, right after `session_migration::record_activity`, and
//! [`validate_document_revision_cap`] from the schema-13 validation arm (and
//! `validate_content_snapshot` for versions 12|13).

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

/// Mirrors `content::documents::MAX_DOCUMENT_REVISIONS`; the two MUST stay equal.
/// The application refuses a save at the ceiling and this is the database's
/// defence-in-depth copy of the same bound. Duplicated (not imported) on purpose,
/// to keep the `db` layer from depending on `content` — the same mirror convention
/// schema 13 uses between `activity::MAX_ACTIVITY_CHARS` and the `activity` CHECK.
const MAX_DOCUMENT_REVISIONS: i64 = 100_000;

/// The original baked bound. Note it is a lexical PREFIX of the raised bound
/// (`...AND 1000` vs `...AND 100000`), so the "already raised?" test MUST run
/// before any "old bound present?" test.
const OLD_BOUND: &str = "BETWEEN 1 AND 1000";

fn new_bound() -> String {
    format!("BETWEEN 1 AND {MAX_DOCUMENT_REVISIONS}")
}

/// Raise the baked `revision` CHECK on `neev_document_revisions` to the current
/// ceiling. Idempotent and row/blob/FK preserving. Runs inside the migration
/// transaction.
pub(super) async fn raise_document_revision_cap(
    tx: &DatabaseTransaction,
) -> Result<(), AuthError> {
    match tx.get_database_backend() {
        DbBackend::Postgres => raise_postgres(tx).await,
        DbBackend::Sqlite => raise_sqlite(tx).await,
        _ => Err(AuthError::Unavailable),
    }
}

/// Constraint-replacement: drop the old revision-range CHECK (found by its
/// DEFINITION, never a fragile auto-generated name) and add the raised one.
async fn raise_postgres(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let target = new_bound();
    // The table's oid, resolved in the current schema (search-path independent).
    let relid = "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
                  WHERE c.relname='neev_document_revisions' AND n.nspname=current_schema())";
    // Already raised? -> no-op. (Runs first: OLD_BOUND is a substring of the target.)
    if super::one(
        tx,
        &format!(
            "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ILIKE '%{target}%' LIMIT 1"
        ),
        vec![],
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    if let Some(row) = super::one(
        tx,
        &format!(
            "SELECT conname FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ILIKE '%{OLD_BOUND}%' LIMIT 1"
        ),
        vec![],
    )
    .await?
    {
        let name: String = row.try_get("", "conname").map_err(database_error)?;
        // `conname` is a catalog identifier, not caller input; quote it as one.
        super::execute(
            tx,
            &format!("ALTER TABLE neev_document_revisions DROP CONSTRAINT \"{name}\""),
            vec![],
        )
        .await?;
    }
    super::execute(
        tx,
        &format!(
            "ALTER TABLE neev_document_revisions \
             ADD CONSTRAINT neev_document_revisions_revision_ceiling \
             CHECK (revision {target})"
        ),
        vec![],
    )
    .await?;
    Ok(())
}

/// SQLite cannot ALTER a baked CHECK, so this is the standard safe table rebuild:
/// recreate the table from its OWN stored DDL (so every column/PK/FK/inline
/// constraint is retained verbatim — only the CREATE name and the revision bound
/// change), copy every row, re-create the explicit indexes, and prove referential
/// integrity with `foreign_key_check` before the transaction may commit.
///
/// `foreign_keys` cannot be toggled inside a transaction, so `defer_foreign_keys`
/// is used instead: it holds all FK checks until COMMIT (legal mid-transaction),
/// which lets the old table be dropped while `neev_document_formats` still
/// references it — the references resolve against the renamed table before commit.
async fn raise_sqlite(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let ddl: String = super::one(
        tx,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_document_revisions'",
        vec![],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "sql")
    .map_err(database_error)?;
    let target = new_bound();
    if ddl.contains(&target) {
        return Ok(()); // already raised -> no-op
    }
    if !ddl.contains(OLD_BOUND) {
        // Unexpected shape: refuse rather than rebuild something we do not recognise.
        return Err(AuthError::Unavailable);
    }
    // Explicit (CREATE INDEX) indexes are dropped with the table and must be
    // re-created; auto-indexes from PK/UNIQUE (sql IS NULL) come back with the DDL.
    let index_rows = tx
        .query_all(super::statement(
            tx,
            "SELECT sql FROM sqlite_master WHERE type='index' \
             AND tbl_name='neev_document_revisions' AND sql IS NOT NULL",
            vec![],
        ))
        .await
        .map_err(database_error)?;
    let mut index_ddls = Vec::with_capacity(index_rows.len());
    for row in &index_rows {
        index_ddls.push(row.try_get::<String>("", "sql").map_err(database_error)?);
    }
    // Only the FIRST name occurrence (the CREATE TABLE target) is renamed, so the
    // self-referential FKs keep pointing at the canonical name, which exists again
    // after the rename. Only the revision bound in the CHECK changes.
    let rebuilt = ddl
        .replacen(
            "neev_document_revisions",
            "neev_document_revisions_new",
            1,
        )
        .replace(OLD_BOUND, &target);
    super::execute(tx, "PRAGMA defer_foreign_keys=ON", vec![]).await?;
    super::execute(tx, &rebuilt, vec![]).await?;
    super::execute(
        tx,
        "INSERT INTO neev_document_revisions_new SELECT * FROM neev_document_revisions",
        vec![],
    )
    .await?;
    super::execute(tx, "DROP TABLE neev_document_revisions", vec![]).await?;
    super::execute(
        tx,
        "ALTER TABLE neev_document_revisions_new RENAME TO neev_document_revisions",
        vec![],
    )
    .await?;
    for index in index_ddls {
        super::execute(tx, &index, vec![]).await?;
    }
    // Fail closed: any dangling reference rolls the whole rebuild back.
    if super::one(tx, "PRAGMA foreign_key_check", vec![])
        .await?
        .is_some()
    {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

/// Prove the ceiling was raised: the table's revision CHECK must carry the current
/// bound. Cheap; safe on either a transaction or a pooled connection.
pub(super) async fn validate_document_revision_cap<C: ConnectionTrait>(
    db: &C,
) -> Result<(), AuthError> {
    let target = new_bound();
    let raised = match db.get_database_backend() {
        DbBackend::Postgres => super::one(
            db,
            &format!(
                "SELECT 1 FROM pg_constraint WHERE conrelid=(SELECT c.oid FROM pg_class c \
                 JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relname='neev_document_revisions' \
                 AND n.nspname=current_schema()) AND contype='c' \
                 AND pg_get_constraintdef(oid) ILIKE '%{target}%' LIMIT 1"
            ),
            vec![],
        )
        .await?
        .is_some(),
        DbBackend::Sqlite => super::one(
            db,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_document_revisions'",
            vec![],
        )
        .await?
        .and_then(|row| row.try_get::<String>("", "sql").ok())
        .map(|ddl| ddl.contains(&target))
        .unwrap_or(false),
        _ => return Err(AuthError::Unavailable),
    };
    if raised {
        Ok(())
    } else {
        Err(AuthError::Unavailable)
    }
}
