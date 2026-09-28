//! Allow generated output references in addition to the five input uploads.
//! Idempotent CHECK relaxation; rows, indexes and foreign keys are preserved.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

/// The bound schema 15 baked, as SQLite stores it verbatim.
const OLD_BOUND_SQLITE: &str = "position BETWEEN 0 AND 4";
/// What replaces it: a lower bound, and no ceiling.
const NEW_BOUND: &str = "position>=0";

/// A POSIX regex (Postgres `~`) for the NORMALISED ceiling. `pg_get_constraintdef`
/// rewrites `BETWEEN a AND b` into `(x >= a) AND (x <= b)`, so the ceiling is
/// found as `position <= …`. Matching ANY upper bound (not just 10485760) means
/// a database relaxed to some other number is still recognised and flattened.
/// Double-escaped for Rust so the SQL literal carries `\y`.
const PG_CEILING_REGEX: &str = "position\"? <= [0-9]+\\y";

fn relid() -> &'static str {
    "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
      WHERE c.relname='neev_attachment_refs' AND n.nspname=current_schema())"
}

/// Drop the `position` ceiling. Idempotent, row/blob/FK preserving, runs in the
/// caller's transaction.
pub(super) async fn relax_attachment_position_cap(
    tx: &DatabaseTransaction,
) -> Result<(), AuthError> {
    match tx.get_database_backend() {
        DbBackend::Postgres => relax_postgres(tx).await,
        DbBackend::Sqlite => relax_sqlite(tx).await,
        _ => Err(AuthError::Unavailable),
    }
}

/// Constraint replacement. The old CHECK carried both bounds, so dropping it also
/// drops `position >= 1`; the lower bound is re-added under a stable name of
/// its own. Constraints are found by their NORMALISED definition, never by a
/// fragile auto-generated name.
///
/// Loops: schema 15 baked every column CHECK as its own constraint, but a
/// hand-patched database could carry more than one ceiling, and leaving a second
/// one behind would keep the limit alive under a different name.
async fn relax_postgres(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let relid = relid();
    loop {
        let Some(row) = super::one(
            tx,
            &format!(
                "SELECT conname FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
                 AND pg_get_constraintdef(oid) ~ '{PG_CEILING_REGEX}' LIMIT 1"
            ),
            vec![],
        )
        .await?
        else {
            break;
        };
        let name: String = row.try_get("", "conname").map_err(database_error)?;
        // A catalog identifier, not caller input, but quoted as one regardless.
        let quoted = name.replace('"', "\"\"");
        super::execute(
            tx,
            &format!("ALTER TABLE neev_attachment_refs DROP CONSTRAINT \"{quoted}\""),
            vec![],
        )
        .await?;
    }
    // Re-assert the lower bound the dropped range CHECK used to carry — unless one
    // is already there, which is the case on a fresh install (schema 15 now bakes
    // `position>=0`) and on any replay. Detected by definition, so the baked
    // constraint's auto-generated name counts.
    if super::one(
        tx,
        &format!(
            "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ~ 'position\"? >= [0-9]+\\y' LIMIT 1"
        ),
        vec![],
    )
    .await?
    .is_none()
    {
        super::execute(
            tx,
            "ALTER TABLE neev_attachment_refs \
             ADD CONSTRAINT neev_attachment_refs_position_nonnegative CHECK (\"position\">=0)",
            vec![],
        )
        .await?;
    }
    Ok(())
}

/// SQLite cannot ALTER a baked CHECK, so this is the standard safe in-transaction
/// rebuild from the table's OWN stored DDL: every column, key and inline
/// constraint retained verbatim, only the CREATE name and the byte bound change.
///
/// The FK ordering is load-bearing and is the same as
/// `content_revision_cap_migration::raise_sqlite`: with FK enforcement ON,
/// `DROP TABLE` implicitly deletes referenced rows, which under
/// `defer_foreign_keys` leaves a non-zero deferred counter that aborts COMMIT even
/// though the database is sound. So: rebuild, GLOBAL `foreign_key_check` (a real
/// dangling reference must roll the whole migration back), then
/// `defer_foreign_keys=OFF` to clear the phantom counter. FK enforcement is never
/// disabled.
async fn relax_sqlite(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let ddl: String = super::one(
        tx,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_attachment_refs'",
        vec![],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "sql")
    .map_err(database_error)?;
    if !ddl.contains(OLD_BOUND_SQLITE) {
        // Already relaxed (fresh installs bake `position>=0`) -> no-op. An
        // unrecognised shape is refused rather than rebuilt blindly.
        if ddl.contains(NEW_BOUND) {
            return Ok(());
        }
        return Err(AuthError::Unavailable);
    }
    // Explicit (CREATE INDEX) indexes are dropped with the table and must be
    // re-created; auto-indexes from PK/UNIQUE (sql IS NULL) come back with the DDL.
    let index_rows = tx
        .query_all(super::statement(
            tx,
            "SELECT sql FROM sqlite_master WHERE type='index' \
             AND tbl_name='neev_attachment_refs' AND sql IS NOT NULL",
            vec![],
        ))
        .await
        .map_err(database_error)?;
    let mut index_ddls = Vec::with_capacity(index_rows.len());
    for row in &index_rows {
        index_ddls.push(row.try_get::<String>("", "sql").map_err(database_error)?);
    }
    // Only the FIRST name occurrence (the CREATE TABLE target) is renamed, so FKs
    // keep pointing at the canonical name, which exists again after the rename.
    let rebuilt = ddl
        .replacen("neev_attachment_refs", "neev_attachment_refs_new", 1)
        .replace(OLD_BOUND_SQLITE, NEW_BOUND);

    super::execute(tx, "PRAGMA defer_foreign_keys=ON", vec![]).await?;
    super::execute(tx, &rebuilt, vec![]).await?;
    super::execute(
        tx,
        "INSERT INTO neev_attachment_refs_new SELECT * FROM neev_attachment_refs",
        vec![],
    )
    .await?;
    super::execute(tx, "DROP TABLE neev_attachment_refs", vec![]).await?;
    super::execute(
        tx,
        "ALTER TABLE neev_attachment_refs_new RENAME TO neev_attachment_refs",
        vec![],
    )
    .await?;
    for index in index_ddls {
        super::execute(tx, &index, vec![]).await?;
    }
    // Integrity FIRST: a REAL dangling reference must roll the migration back, not
    // be bypassed by the counter reset below.
    if super::one(tx, "PRAGMA foreign_key_check", vec![])
        .await?
        .is_some()
    {
        return Err(AuthError::Unavailable);
    }
    super::execute(tx, "PRAGMA defer_foreign_keys=OFF", vec![]).await?;
    Ok(())
}

/// Prove the ceiling is gone: no `position` upper bound may remain. Cheap; safe
/// on either a transaction or a pooled connection.
pub(super) async fn validate_attachment_position_cap<C: ConnectionTrait>(
    db: &C,
) -> Result<(), AuthError> {
    let relaxed = match db.get_database_backend() {
        DbBackend::Postgres => super::one(
            db,
            &format!(
                "SELECT 1 FROM pg_constraint WHERE conrelid={} AND contype='c' \
                 AND pg_get_constraintdef(oid) ~ '{PG_CEILING_REGEX}' LIMIT 1",
                relid()
            ),
            vec![],
        )
        .await?
        .is_none(),
        DbBackend::Sqlite => super::one(
            db,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_attachment_refs'",
            vec![],
        )
        .await?
        .and_then(|row| row.try_get::<String>("", "sql").ok())
        .map(|ddl| !ddl.contains(OLD_BOUND_SQLITE) && ddl.contains(NEW_BOUND))
        .unwrap_or(false),
        _ => return Err(AuthError::Unavailable),
    };
    if relaxed {
        Ok(())
    } else {
        Err(AuthError::Unavailable)
    }
}
