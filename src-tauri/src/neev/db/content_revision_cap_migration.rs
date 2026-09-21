//! Forward step for schema 13: raise the per-document revision ceiling.
//!
//! `neev_document_revisions` is created (schema 3, `content_migration`) with a hard
//! CHECK `revision BETWEEN 1 AND 1000`. That upper bound IS the per-document revision
//! quota, and 1000 makes AUTOSAVE unusable — a busy document exhausts it in a single
//! sitting and can then never be saved again. The application ceiling now lives in
//! `content::documents::MAX_DOCUMENT_REVISIONS`; this raises the database's
//! defence-in-depth CHECK to match, WITHOUT deleting any history, dropping any
//! revision, or changing a single revision/blob reference. Aggregate storage stays
//! protected by the per-org byte budget (`neev_content_usage.charged_bytes <= 1 GiB`).
//!
//! Additive, forward-only, and IDEMPOTENT: a database already at the raised bound is
//! left untouched, so a replayed run is a no-op. Runs INSIDE the migration
//! transaction (no dedicated connection, no foreign-key enforcement disabled).
//!
//! WIRING (owned by the schema-13 integration): call [`raise_document_revision_cap`]
//! on the init `&tx`, right after `session_migration::record_activity`, on both the
//! fresh-install path and the upgrade tail; and [`validate_document_revision_cap`]
//! from the schema-13 validation arm (and `validate_content_snapshot` for 12|13).

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

/// Mirrors `content::documents::MAX_DOCUMENT_REVISIONS`; the two MUST stay equal.
/// The application refuses a save at the ceiling and this is the database's
/// defence-in-depth copy of the same bound. Duplicated (not imported) on purpose,
/// to keep the `db` layer from depending on `content` — the same mirror convention
/// schema 13 uses between `activity::MAX_ACTIVITY_CHARS` and the `activity` CHECK.
const MAX_DOCUMENT_REVISIONS: i64 = 100_000;

/// The literal bound as SQLite stores it (SQLite keeps `BETWEEN` verbatim; only
/// PostgreSQL's `pg_get_constraintdef` normalises `BETWEEN a AND b` into
/// `(x >= a) AND (x <= b)`, verified on PG 15: `CHECK (((revision >= 1) AND
/// (revision <= 1000)))`, which is why the two backends are detected differently).
const OLD_BOUND_SQLITE: &str = "BETWEEN 1 AND 1000";

fn new_bound_sqlite() -> String {
    format!("BETWEEN 1 AND {MAX_DOCUMENT_REVISIONS}")
}

/// A POSIX-regex (Postgres `~`) matching the NORMALISED upper bound with a trailing
/// token boundary, so `<= 1000` never also matches `<= 10000`/`<= 100000`. `\y` is a
/// word boundary; after the digits it holds only when the next character is a
/// non-digit (e.g. the closing paren), pinning the number exactly. Double-escaped for
/// Rust (`\\y`) so the SQL string literal carries `\y`.
fn pg_bound_regex(bound: i64) -> String {
    format!("revision <= {bound}\\y")
}

/// Raise the baked `revision` CHECK on `neev_document_revisions` to the current
/// ceiling. Idempotent, row/blob/FK preserving, runs in the caller's transaction.
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
/// NORMALISED definition, never a fragile auto-generated name) and add the raised
/// one. `pg_get_constraintdef` rewrites `BETWEEN` to `>= AND <=`, so detection
/// matches `revision <= <bound>` with a token boundary rather than the `BETWEEN` text.
async fn raise_postgres(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    // The table's oid, resolved in the current schema (search-path independent).
    let relid = "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
                  WHERE c.relname='neev_document_revisions' AND n.nspname=current_schema())";
    let raised = pg_bound_regex(MAX_DOCUMENT_REVISIONS);
    // Already raised? -> no-op. (The boundary makes `<= 1000` and `<= 100000`
    // disjoint, so ordering is not load-bearing, but this stays defensive.)
    if super::one(
        tx,
        &format!(
            "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ~ '{raised}' LIMIT 1"
        ),
        vec![],
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    let old = pg_bound_regex(1000);
    if let Some(row) = super::one(
        tx,
        &format!(
            "SELECT conname FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ~ '{old}' LIMIT 1"
        ),
        vec![],
    )
    .await?
    {
        let name: String = row.try_get("", "conname").map_err(database_error)?;
        // `conname` is a catalog identifier, not caller input, but quote it as one
        // regardless: double any embedded double-quote so the identifier is exact.
        let quoted = name.replace('"', "\"\"");
        super::execute(
            tx,
            &format!("ALTER TABLE neev_document_revisions DROP CONSTRAINT \"{quoted}\""),
            vec![],
        )
        .await?;
    }
    super::execute(
        tx,
        &format!(
            "ALTER TABLE neev_document_revisions \
             ADD CONSTRAINT neev_document_revisions_revision_ceiling \
             CHECK (revision BETWEEN 1 AND {MAX_DOCUMENT_REVISIONS})"
        ),
        vec![],
    )
    .await?;
    Ok(())
}

/// SQLite cannot ALTER a baked CHECK, so this is the standard safe in-transaction
/// table rebuild, rebuilt from the table's OWN stored DDL (every column/PK/FK/inline
/// constraint retained verbatim; only the CREATE name and the revision bound change).
///
/// The subtlety: with FK enforcement ON, `DROP TABLE` does an implicit delete of the
/// referenced rows, which — under `defer_foreign_keys` — leaves a non-zero deferred
/// violation counter that aborts COMMIT even though the database is sound. So the
/// order is: rebuild, then a GLOBAL `foreign_key_check` (a real dangling reference
/// must roll the whole migration back, never be bypassed), then
/// `defer_foreign_keys=OFF` to reconcile/clear the phantom counter before the caller
/// commits. FK enforcement is never disabled; `defer_foreign_keys` returns to its
/// default OFF.
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
    let target = new_bound_sqlite();
    if ddl.contains(&target) {
        return Ok(()); // already raised -> no-op
    }
    if !ddl.contains(OLD_BOUND_SQLITE) {
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
        .replace(OLD_BOUND_SQLITE, &target);

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
    // Global integrity check FIRST: a REAL dangling reference must roll the whole
    // migration back, not be bypassed by the counter reset below.
    if super::one(tx, "PRAGMA foreign_key_check", vec![])
        .await?
        .is_some()
    {
        return Err(AuthError::Unavailable);
    }
    // Reconcile the phantom deferred-violation counter left by DROP's implicit delete
    // (the check above proved there are no REAL violations); without this the COMMIT
    // aborts with "FOREIGN KEY constraint failed" even though the database is sound.
    super::execute(tx, "PRAGMA defer_foreign_keys=OFF", vec![]).await?;
    Ok(())
}

/// Prove the ceiling was raised: the table's revision CHECK must carry the current
/// bound. Cheap; safe on either a transaction or a pooled connection.
pub(super) async fn validate_document_revision_cap<C: ConnectionTrait>(
    db: &C,
) -> Result<(), AuthError> {
    let raised = match db.get_database_backend() {
        DbBackend::Postgres => {
            let regex = pg_bound_regex(MAX_DOCUMENT_REVISIONS);
            super::one(
                db,
                &format!(
                    "SELECT 1 FROM pg_constraint WHERE conrelid=(SELECT c.oid FROM pg_class c \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                     WHERE c.relname='neev_document_revisions' AND n.nspname=current_schema()) \
                     AND contype='c' AND pg_get_constraintdef(oid) ~ '{regex}' LIMIT 1"
                ),
                vec![],
            )
            .await?
            .is_some()
        }
        DbBackend::Sqlite => super::one(
            db,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_document_revisions'",
            vec![],
        )
        .await?
        .and_then(|row| row.try_get::<String>("", "sql").ok())
        .map(|ddl| ddl.contains(&new_bound_sqlite()))
        .unwrap_or(false),
        _ => return Err(AuthError::Unavailable),
    };
    if raised {
        Ok(())
    } else {
        Err(AuthError::Unavailable)
    }
}
