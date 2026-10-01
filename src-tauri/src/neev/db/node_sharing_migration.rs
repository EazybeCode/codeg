//! Schema 21 — folder sharing defaults for NEW resources.
//!
//! Adds `neev_node_sharing_defaults`: at most one explicit default per node,
//! `private` or `organization`. No row means "inherit from the parent", and a
//! chain with no explicit row falls back to private — exactly the behaviour of
//! schema 20, so the migration changes what nobody sees until a manager sets a
//! default.
//!
//! Additive and table-guarded: it creates one table and UPDATEs no existing row.
//! A default is consulted only when a resource is CREATED (`content::resources`)
//! and is materialized into that resource's own `visibility`; it is never an
//! access rule, so no existing resource's audience can change because of it.
//!
//! A separate table rather than a column on `neev_nodes` so the guarded
//! downgrade is a `DROP TABLE` that restores the schema-20 catalog exactly, with
//! no column rewrite on SQLite.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(crate) const SCHEMA_VERSION: i64 = 21;
pub(super) const TABLE_NAMES: &[&str] = &["neev_node_sharing_defaults"];

const TABLE: &str = "neev_node_sharing_defaults";

/// Both backends accept this text verbatim, and SQLite stores it verbatim, which
/// is what `validate` compares against.
const DDL: &str = "CREATE TABLE neev_node_sharing_defaults (org_id integer NOT NULL, node_id integer NOT NULL, default_visibility text NOT NULL, set_by integer NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_node_sharing_defaults_pk PRIMARY KEY (org_id, node_id), CONSTRAINT neev_node_sharing_defaults_visibility CHECK (default_visibility IN ('private','organization')), CONSTRAINT neev_node_sharing_defaults_time CHECK (updated_at>=0), CONSTRAINT neev_node_sharing_defaults_node FOREIGN KEY (node_id, org_id) REFERENCES neev_nodes (id, org_id) ON DELETE CASCADE, CONSTRAINT neev_node_sharing_defaults_setter FOREIGN KEY (set_by) REFERENCES neev_accounts (id))";

/// (name, contype, required fragments of `pg_get_constraintdef`)
const PG_CONSTRAINTS: [(&str, &str, &[&str]); 5] = [
    ("neev_node_sharing_defaults_pk", "p", &["PRIMARY KEY (org_id, node_id)"]),
    (
        "neev_node_sharing_defaults_visibility",
        "c",
        &["default_visibility", "'private'", "'organization'"],
    ),
    ("neev_node_sharing_defaults_time", "c", &["updated_at >= 0"]),
    (
        "neev_node_sharing_defaults_node",
        "f",
        &["(node_id, org_id) REFERENCES neev_nodes(id, org_id) ON DELETE CASCADE"],
    ),
    (
        "neev_node_sharing_defaults_setter",
        "f",
        &["(set_by) REFERENCES neev_accounts(id)"],
    ),
];

const PG_COLUMNS: [(&str, &str); 5] = [
    ("default_visibility", "text"),
    ("node_id", "integer"),
    ("org_id", "integer"),
    ("set_by", "integer"),
    ("updated_at", "bigint"),
];

async fn exists<C: ConnectionTrait>(db: &C) -> Result<bool, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1",
        DbBackend::Sqlite => "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
        _ => return Err(AuthError::Unavailable),
    };
    Ok(super::one(db, sql, vec![TABLE.into()]).await?.is_some())
}

/// Create the table when absent. Idempotent; runs in the caller's transaction.
pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    if !exists(tx).await? {
        super::execute(tx, DDL, vec![]).await?;
    }
    validate(tx).await
}

/// Prove the exact schema-21 shape and that every stored row is coherent.
/// Safe on a transaction or a pooled connection.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    match db.get_database_backend() {
        DbBackend::Sqlite => {
            let ddl: String = super::one(
                db,
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
                vec![TABLE.into()],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "sql")
            .map_err(database_error)?;
            if ddl != DDL {
                return Err(AuthError::Unavailable);
            }
            // Only the primary key's implicit index; no stray trigger or index.
            let extra = super::one(
                db,
                "SELECT 1 AS extra FROM sqlite_master WHERE tbl_name=$1 AND type<>'table' AND name NOT LIKE 'sqlite_autoindex_%' LIMIT 1",
                vec![TABLE.into()],
            )
            .await?;
            if extra.is_some() {
                return Err(AuthError::Unavailable);
            }
        }
        DbBackend::Postgres => {
            let rows = db
                .query_all(super::statement(
                    db,
                    "SELECT column_name::text AS name,data_type::text AS kind,is_nullable::text AS nullable FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY column_name",
                    vec![TABLE.into()],
                ))
                .await
                .map_err(database_error)?;
            if rows.len() != PG_COLUMNS.len() {
                return Err(AuthError::Unavailable);
            }
            for (row, (name, kind)) in rows.iter().zip(PG_COLUMNS) {
                let actual: (String, String, String) = (
                    row.try_get("", "name").map_err(database_error)?,
                    row.try_get("", "kind").map_err(database_error)?,
                    row.try_get("", "nullable").map_err(database_error)?,
                );
                if actual != (name.to_owned(), kind.to_owned(), "NO".to_owned()) {
                    return Err(AuthError::Unavailable);
                }
            }
            let relid = format!(
                "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relname='{TABLE}' AND n.nspname=current_schema())"
            );
            let count: i64 = super::one(
                db,
                &format!("SELECT COUNT(*) AS n FROM pg_constraint WHERE conrelid={relid}"),
                vec![],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "n")
            .map_err(database_error)?;
            if count != PG_CONSTRAINTS.len() as i64 {
                return Err(AuthError::Unavailable);
            }
            for (name, kind, fragments) in PG_CONSTRAINTS {
                let definition: String = super::one(
                    db,
                    &format!("SELECT pg_get_constraintdef(oid) AS def FROM pg_constraint WHERE conrelid={relid} AND conname=$1 AND contype=$2"),
                    vec![name.into(), kind.into()],
                )
                .await?
                .ok_or(AuthError::Unavailable)?
                .try_get("", "def")
                .map_err(database_error)?;
                if !fragments.iter().all(|fragment| definition.contains(fragment)) {
                    return Err(AuthError::Unavailable);
                }
            }
        }
        _ => return Err(AuthError::Unavailable),
    }
    // A default on a working directory or on an org's node from another tenant is
    // not creatable through the API; refuse one smuggled into a restored snapshot.
    let invalid = super::one(
        db,
        "SELECT 1 AS invalid FROM neev_node_sharing_defaults d LEFT JOIN neev_nodes n ON n.id=d.node_id AND n.org_id=d.org_id WHERE n.id IS NULL OR n.kind NOT IN ('project','folder') LIMIT 1",
        vec![],
    )
    .await?;
    if invalid.is_some() {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

/// What a 21 -> 20 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// The table is gone and the marker is 20; no other row or table changed.
    Done,
    /// The marker is not 21.
    NotAtTwentyOne(i64),
    /// Managers have configured defaults. Dropping them would silently discard
    /// user choices, so the answer is to keep 21 and repair forward.
    DefaultsExist { rows: i64 },
}

/// Take a schema-21 database back to 20. GUARDED: refuses unless the marker is
/// exactly 21 and no default has ever been configured (any surviving row). Runs
/// in the caller's transaction; the caller commits only on `Done`.
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
        return Ok(Downgrade::NotAtTwentyOne(version));
    }
    validate(tx).await?;
    let rows: i64 = super::one(tx, "SELECT COUNT(*) AS n FROM neev_node_sharing_defaults", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
    if rows > 0 {
        return Ok(Downgrade::DefaultsExist { rows });
    }
    super::execute(tx, "DROP TABLE neev_node_sharing_defaults", vec![]).await?;
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::provider_enum_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    if exists(tx).await? {
        return Err(AuthError::Unavailable);
    }
    super::provider_enum_migration::validate(tx).await?;
    Ok(Downgrade::Done)
}
