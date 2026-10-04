//! Schema 23 — Move to Trash for projects and folders.
//!
//! Adds `neev_node_trash`: the ledger of one trash *batch* per Move to Trash.
//! One row per node the batch hid (`item_kind='node'`, decimal node id) and one
//! per resource the batch archived (`item_kind='resource'`, resource id) and per
//! Agent Chat session it archived (`item_kind='session'`, session id). The
//! batch root is the node row whose `item_id` is `root_node_id`.
//!
//! Additive and table-guarded: it creates one table and UPDATEs no existing row.
//! Nodes are never deleted and never re-parented by trash, so a node's original
//! location is its unchanged `parent_id`. Resources keep using their own
//! `archived` flag — the existing per-resource trash, which knowledge
//! eligibility already honours — and this table only remembers which of them a
//! folder trash archived, so restore never resurrects an item trashed on its own.
//!
//! The primary key `(org_id,item_kind,item_id)` puts every item in at most one
//! batch. A descendant trashed before its parent therefore stays in its own
//! batch and is not restored with the parent.
//!
//! A separate table rather than columns on `neev_nodes` so the guarded
//! downgrade is a `DROP TABLE` that restores the schema-22 catalog exactly.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(crate) const SCHEMA_VERSION: i64 = 23;
pub(super) const TABLE_NAMES: &[&str] = &["neev_node_trash"];

const TABLE: &str = "neev_node_trash";

/// Both backends accept this text verbatim, and SQLite stores it verbatim, which
/// is what `validate` compares against.
const DDL: &str = "CREATE TABLE neev_node_trash (org_id integer NOT NULL, batch_id text NOT NULL, item_kind text NOT NULL, item_id text NOT NULL, root_node_id integer NOT NULL, trashed_by integer NOT NULL, trashed_at bigint NOT NULL, CONSTRAINT neev_node_trash_pk PRIMARY KEY (org_id, item_kind, item_id), CONSTRAINT neev_node_trash_kind CHECK (item_kind IN ('node','resource','session')), CONSTRAINT neev_node_trash_time CHECK (trashed_at>=0), CONSTRAINT neev_node_trash_root FOREIGN KEY (root_node_id, org_id) REFERENCES neev_nodes (id, org_id) ON DELETE CASCADE, CONSTRAINT neev_node_trash_actor FOREIGN KEY (trashed_by) REFERENCES neev_accounts (id))";

/// (name, contype, required fragments of `pg_get_constraintdef`)
const PG_CONSTRAINTS: [(&str, &str, &[&str]); 5] = [
    ("neev_node_trash_pk", "p", &["PRIMARY KEY (org_id, item_kind, item_id)"]),
    ("neev_node_trash_kind", "c", &["item_kind", "'node'", "'resource'", "'session'"]),
    ("neev_node_trash_time", "c", &["trashed_at >= 0"]),
    (
        "neev_node_trash_root",
        "f",
        &["(root_node_id, org_id) REFERENCES neev_nodes(id, org_id) ON DELETE CASCADE"],
    ),
    ("neev_node_trash_actor", "f", &["(trashed_by) REFERENCES neev_accounts(id)"]),
];

const PG_COLUMNS: [(&str, &str); 7] = [
    ("batch_id", "text"),
    ("item_id", "text"),
    ("item_kind", "text"),
    ("org_id", "integer"),
    ("root_node_id", "integer"),
    ("trashed_at", "bigint"),
    ("trashed_by", "integer"),
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

/// Prove the exact schema-23 shape and that every stored row is coherent.
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
    // Refuse a snapshot whose ledger the API could not have written: an item in
    // another org's tree, a node row that is not a project/folder, a resource row
    // or session that does not exist, or a batch without its root row. (Whether a resource
    // is still archived is application state, never a reason to refuse a boot.)
    let invalid = super::one(
        db,
        "SELECT 1 AS invalid FROM neev_node_trash t \
         LEFT JOIN neev_nodes n ON t.item_kind='node' AND n.org_id=t.org_id AND CAST(n.id AS TEXT)=t.item_id \
         LEFT JOIN neev_resources r ON t.item_kind='resource' AND r.org_id=t.org_id AND r.id=t.item_id \
         LEFT JOIN neev_agent_sessions g ON t.item_kind='session' AND g.org_id=t.org_id AND g.id=t.item_id \
         WHERE (t.item_kind='node' AND (n.id IS NULL OR n.kind NOT IN ('project','folder'))) \
         OR (t.item_kind='resource' AND r.id IS NULL) \
         OR (t.item_kind='session' AND g.id IS NULL) \
         OR NOT EXISTS (SELECT 1 FROM neev_node_trash b WHERE b.org_id=t.org_id AND b.batch_id=t.batch_id \
           AND b.item_kind='node' AND b.item_id=CAST(t.root_node_id AS TEXT)) \
         LIMIT 1",
        vec![],
    )
    .await?;
    if invalid.is_some() {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

/// What a 23 -> 22 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// The table is gone and the marker is 22; no other row or table changed.
    Done,
    /// The marker is not 23.
    NotAtTwentyThree(i64),
    /// Something is in the trash. Dropping the ledger would surface the hidden
    /// nodes again with their resources still archived, so restore first.
    TrashExists { rows: i64 },
}

/// Take a schema-23 database back to 22. GUARDED: refuses unless the marker is
/// exactly 23 and the trash is empty. Runs in the caller's transaction; the
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
        return Ok(Downgrade::NotAtTwentyThree(version));
    }
    validate(tx).await?;
    let rows: i64 = super::one(tx, "SELECT COUNT(*) AS n FROM neev_node_trash", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
    if rows > 0 {
        return Ok(Downgrade::TrashExists { rows });
    }
    super::execute(tx, "DROP TABLE neev_node_trash", vec![]).await?;
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::knowledge_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    if exists(tx).await? {
        return Err(AuthError::Unavailable);
    }
    super::knowledge_migration::validate(tx).await?;
    Ok(Downgrade::Done)
}
