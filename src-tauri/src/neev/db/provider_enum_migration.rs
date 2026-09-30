//! Schema 20 — widen the agent provider enum to the ACP providers.
//!
//! Schemas 5 (`agent_migration`) and 11 (`session_migration`) bake
//! `CHECK (provider IN ('claude','codex'))` into `neev_agent_connections`,
//! `neev_agent_sessions` and `neev_agent_turns`. Cursor, Gemini CLI and OpenCode
//! (`agents::acp_providers`) need their own connections, sessions and turns, so
//! the CHECK becomes the five-provider list. Whether the product OFFERS a
//! provider stays a runtime decision (`NEEV_AGENT_ACP_PROVIDERS`); the database
//! only stops making it impossible.
//!
//! Forward-only, IDEMPOTENT, adds no table and changes no row: widening an IN
//! list can only admit more rows, so every existing row stays valid.
//!
//! - **PostgreSQL**: the old CHECK is found by its NORMALISED definition
//!   (`provider = ANY (ARRAY[...])` without `'opencode'`), dropped, and the
//!   widened one added under a stable name unless one already exists.
//! - **SQLite**: a CHECK cannot be altered, and a rebuild is unsafe here —
//!   `neev_agent_turns` references sessions and several tables reference
//!   connections with `ON DELETE RESTRICT`, which the implicit delete of a
//!   `DROP TABLE` trips immediately even with deferred keys. So this uses the
//!   procedure SQLite documents for relaxing a CHECK ("Making Other Kinds Of
//!   Table Schema Changes", sqlite.org/lang_altertable.html): rewrite the stored
//!   `CREATE TABLE` text under `writable_schema` and bump `schema_version`, all
//!   inside the migration transaction. Only the exact old CHECK text is replaced;
//!   an unrecognised DDL shape is refused, never edited blindly. The on-disk
//!   record format is unchanged, and `integrity_check` is run afterwards.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(super) const SCHEMA_VERSION: i64 = 20;

/// The tables whose `provider` column carries the enum CHECK.
pub(super) const TABLES: [&str; 3] = [
    "neev_agent_connections",
    "neev_agent_sessions",
    "neev_agent_turns",
];

/// Every provider a row may name. Mirrors `agents::types::Provider`.
pub(crate) const PROVIDERS: [&str; 5] = ["claude", "codex", "cursor", "gemini", "opencode"];

const OLD_CHECK: &str = "provider IN ('claude','codex')";
const NEW_CHECK: &str = "provider IN ('claude','codex','cursor','gemini','opencode')";

fn relid(table: &str) -> String {
    format!(
        "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
          WHERE c.relname='{table}' AND n.nspname=current_schema())"
    )
}

/// Widen every provider CHECK. Idempotent; runs in the caller's transaction.
pub(super) async fn widen(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    match tx.get_database_backend() {
        DbBackend::Postgres => {
            for table in TABLES {
                widen_postgres(tx, table).await?;
            }
            Ok(())
        }
        DbBackend::Sqlite => widen_sqlite(tx).await,
        _ => Err(AuthError::Unavailable),
    }
}

async fn widen_postgres(tx: &DatabaseTransaction, table: &str) -> Result<(), AuthError> {
    let relid = relid(table);
    // Any provider IN-list CHECK that does not yet admit the ACP providers.
    loop {
        let Some(row) = super::one(
            tx,
            &format!(
                "SELECT conname FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
                 AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
                 AND pg_get_constraintdef(oid) !~ '''opencode''' LIMIT 1"
            ),
            vec![],
        )
        .await?
        else {
            break;
        };
        let name: String = row.try_get("", "conname").map_err(database_error)?;
        let quoted = name.replace('"', "\"\"");
        super::execute(
            tx,
            &format!("ALTER TABLE {table} DROP CONSTRAINT \"{quoted}\""),
            vec![],
        )
        .await?;
    }
    if !postgres_widened(tx, table).await? {
        super::execute(
            tx,
            &format!("ALTER TABLE {table} ADD CONSTRAINT {table}_provider_allowed CHECK ({NEW_CHECK})"),
            vec![],
        )
        .await?;
    }
    Ok(())
}

async fn postgres_widened<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, AuthError> {
    let relid = relid(table);
    let narrow = super::one(
        db,
        &format!(
            "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
             AND pg_get_constraintdef(oid) !~ '''opencode''' LIMIT 1"
        ),
        vec![],
    )
    .await?
    .is_some();
    let wide = super::one(
        db,
        &format!(
            "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
             AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
             AND pg_get_constraintdef(oid) ~ '''cursor''' \
             AND pg_get_constraintdef(oid) ~ '''gemini''' \
             AND pg_get_constraintdef(oid) ~ '''opencode''' LIMIT 1"
        ),
        vec![],
    )
    .await?
    .is_some();
    Ok(wide && !narrow)
}

async fn sqlite_ddl<C: ConnectionTrait>(db: &C, table: &str) -> Result<String, AuthError> {
    super::one(
        db,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
        vec![table.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "sql")
    .map_err(database_error)
}

/// Pure: `ddl` with the provider CHECK rewritten `from` -> `to`, `None` when it
/// already reads `to`, or an error for a shape this migration does not
/// recognise (exactly one provider CHECK is expected).
fn rewritten_ddl(ddl: &str, from: &str, to: &str) -> Result<Option<String>, AuthError> {
    match (ddl.matches(from).count(), ddl.matches(to).count()) {
        (1, 0) => Ok(Some(ddl.replacen(from, to, 1))),
        (0, 1) => Ok(None),
        _ => Err(AuthError::Unavailable),
    }
}

fn widened_ddl(ddl: &str) -> Result<Option<String>, AuthError> {
    rewritten_ddl(ddl, OLD_CHECK, NEW_CHECK)
}

async fn widen_sqlite(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    rewrite_sqlite(tx, OLD_CHECK, NEW_CHECK).await
}

/// SQLite's documented CHECK change (module docs), `from` -> `to` on every
/// provider table, inside the caller's transaction, then proven.
async fn rewrite_sqlite(
    tx: &DatabaseTransaction,
    from: &str,
    to: &str,
) -> Result<(), AuthError> {
    let mut changes = Vec::new();
    for table in TABLES {
        let ddl = sqlite_ddl(tx, table).await?;
        if let Some(rewritten) = rewritten_ddl(&ddl, from, to)? {
            changes.push((table, rewritten));
        }
    }
    if changes.is_empty() {
        return Ok(());
    }
    let version: i64 = super::one(tx, "PRAGMA schema_version", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "schema_version")
        .map_err(database_error)?;
    super::execute(tx, "PRAGMA writable_schema=ON", vec![]).await?;
    let mut result = Ok(());
    for (table, widened) in &changes {
        if let Err(error) = super::execute(
            tx,
            "UPDATE sqlite_master SET sql=$1 WHERE type='table' AND name=$2",
            vec![widened.clone().into(), (*table).into()],
        )
        .await
        {
            result = Err(error);
            break;
        }
    }
    if result.is_ok() {
        // Forces every connection to re-read the schema (documented step).
        result = super::execute(tx, &format!("PRAGMA schema_version={}", version + 1), vec![])
            .await
            .map(|_| ());
    }
    // Always leave writable_schema off, whatever happened above.
    super::execute(tx, "PRAGMA writable_schema=OFF", vec![]).await?;
    result?;
    let integrity: String = super::one(tx, "PRAGMA integrity_check", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "integrity_check")
        .map_err(database_error)?;
    if integrity != "ok" {
        return Err(AuthError::Unavailable);
    }
    for table in TABLES {
        if rewritten_ddl(&sqlite_ddl(tx, table).await?, from, to)?.is_some() {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

/// What a 20 -> 19 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// Marker 19 and the narrow CHECKs; every row preserved.
    Done,
    /// The marker is not 20.
    NotAtTwenty(i64),
    /// Rows name an ACP provider: narrowing would invalidate them and deleting
    /// them would destroy user data.
    AcpRowsExist { table: &'static str, rows: i64 },
}

/// Take a schema-20 database back to 19: the narrow `provider IN
/// ('claude','codex')` CHECKs and marker 19, nothing else.
///
/// The only data-preserving inverse, and GUARDED: it refuses when the marker is
/// not exactly 20, or when any connection, session or turn names cursor, gemini
/// or opencode. Those rows are user data a schema-19 binary cannot represent;
/// the answer then is to keep schema 20 — never to delete them, never to
/// restore a backup over newer writes. Runs in the caller's transaction; the
/// caller commits only on `Done`.
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
        return Ok(Downgrade::NotAtTwenty(version));
    }
    for table in TABLES {
        let rows: i64 = super::one(
            tx,
            &format!("SELECT COUNT(*) AS n FROM {table} WHERE provider NOT IN ('claude','codex')"),
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
        if rows > 0 {
            return Ok(Downgrade::AcpRowsExist { table, rows });
        }
    }
    match tx.get_database_backend() {
        DbBackend::Postgres => {
            for table in TABLES {
                let relid = relid(table);
                // Every provider IN-list CHECK goes (the widened one and any
                // stray); the schema-19 one is re-added under a stable name.
                loop {
                    let Some(row) = super::one(
                        tx,
                        &format!(
                            "SELECT conname FROM pg_constraint WHERE conrelid={relid} \
                             AND contype='c' AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
                             LIMIT 1"
                        ),
                        vec![],
                    )
                    .await?
                    else {
                        break;
                    };
                    let name: String = row.try_get("", "conname").map_err(database_error)?;
                    let quoted = name.replace('"', "\"\"");
                    super::execute(
                        tx,
                        &format!("ALTER TABLE {table} DROP CONSTRAINT \"{quoted}\""),
                        vec![],
                    )
                    .await?;
                }
                super::execute(
                    tx,
                    &format!(
                        "ALTER TABLE {table} ADD CONSTRAINT {table}_provider_v19 CHECK ({OLD_CHECK})"
                    ),
                    vec![],
                )
                .await?;
            }
        }
        DbBackend::Sqlite => rewrite_sqlite(tx, NEW_CHECK, OLD_CHECK).await?,
        _ => return Err(AuthError::Unavailable),
    }
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::steer_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    validate_narrow(tx).await?;
    Ok(Downgrade::Done)
}

/// Prove every provider CHECK is the schema-19 two-provider list again.
pub(super) async fn validate_narrow<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for table in TABLES {
        let narrow = match db.get_database_backend() {
            DbBackend::Postgres => {
                let relid = relid(table);
                let wide = super::one(
                    db,
                    &format!(
                        "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
                         AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
                         AND pg_get_constraintdef(oid) ~ '''(cursor|gemini|opencode)''' LIMIT 1"
                    ),
                    vec![],
                )
                .await?
                .is_some();
                let two = super::one(
                    db,
                    &format!(
                        "SELECT 1 FROM pg_constraint WHERE conrelid={relid} AND contype='c' \
                         AND pg_get_constraintdef(oid) ~ '\\yprovider = ANY' \
                         AND pg_get_constraintdef(oid) ~ '''claude''' \
                         AND pg_get_constraintdef(oid) ~ '''codex''' LIMIT 1"
                    ),
                    vec![],
                )
                .await?
                .is_some();
                two && !wide
            }
            DbBackend::Sqlite => {
                let ddl = sqlite_ddl(db, table).await?;
                ddl.matches(OLD_CHECK).count() == 1 && !ddl.contains(NEW_CHECK)
            }
            _ => return Err(AuthError::Unavailable),
        };
        if !narrow {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

/// Prove every provider CHECK admits exactly the five providers and no narrow
/// one remains. Safe on a transaction or a pooled connection.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for table in TABLES {
        let widened = match db.get_database_backend() {
            DbBackend::Postgres => postgres_widened(db, table).await?,
            DbBackend::Sqlite => {
                let ddl = sqlite_ddl(db, table).await?;
                ddl.matches(NEW_CHECK).count() == 1 && !ddl.contains(OLD_CHECK)
            }
            _ => return Err(AuthError::Unavailable),
        };
        if !widened {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sqlite_rewrite_is_exact_and_idempotent() {
        let ddl = "CREATE TABLE \"t\" ( \"provider\" text NOT NULL CHECK (provider IN ('claude','codex')), x int )";
        let widened = widened_ddl(ddl).unwrap().unwrap();
        assert!(widened.contains(NEW_CHECK));
        assert!(!widened.contains(OLD_CHECK));
        assert_eq!(widened_ddl(&widened).unwrap(), None);
        // Unknown shapes are refused, never edited.
        assert!(widened_ddl("CREATE TABLE t (provider text)").is_err());
        assert!(widened_ddl(&format!("{ddl} {OLD_CHECK}")).is_err());
    }

    #[test]
    fn the_list_matches_the_provider_enum() {
        for provider in PROVIDERS {
            assert!(NEW_CHECK.contains(&format!("'{provider}'")));
            assert!(crate::neev::agents::types::Provider::parse(provider).is_some());
        }
        assert_eq!(
            crate::neev::agents::types::Provider::ALL.len(),
            PROVIDERS.len()
        );
    }
}
