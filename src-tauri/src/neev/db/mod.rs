use super::error::{database_error, AuthError};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, ExecResult, QueryResult,
    Statement, TransactionTrait, Value,
};
mod agent_migration;
mod agent_status_migration;
#[cfg(test)]
mod agent_status_tests;
mod attachment_archived_migration;
mod attachment_byte_cap_migration;
mod attachment_migration;
mod attachment_position_cap_migration;
mod content_migration;
mod content_revision_cap_migration;
mod document_format_migration;
#[cfg(test)]
mod document_format_tests;
pub(crate) mod entities;
mod gateway_migration;
#[cfg(test)]
mod gateway_migration_tests;
mod image_blob_migration;
pub(crate) mod knowledge_migration;
#[cfg(test)]
mod knowledge_migration_tests;
pub(crate) mod library_migration;
mod mention_migration;
mod migration;
pub(crate) mod node_sharing_migration;
#[cfg(test)]
mod node_sharing_tests;
pub(crate) mod node_trash_migration;
pub(crate) mod member_sharing_migration;
#[cfg(test)]
mod member_sharing_tests;
pub(crate) mod msteams_migration;
#[cfg(test)]
mod msteams_migration_tests;
#[cfg(test)]
mod node_trash_tests;
mod org_code_migration;
pub(crate) mod provider_enum_migration;
/// The schema a fresh or fully initialized database is at. Tests that mean
/// "current" say this rather than naming whichever layer was newest when they
/// were written (several had drifted to 16/17 while installs moved on).
#[cfg(test)]
pub(crate) const CURRENT_SCHEMA_VERSION: i64 = msteams_migration::SCHEMA_VERSION;
#[cfg(test)]
mod provider_enum_tests;
mod session_migration;
#[cfg(test)]
mod session_tests;
mod steer_migration;

/// The failure codes a turn may durably record. Re-exported so the run driver
/// and its tests cannot drift from what the schema actually accepts.
pub(crate) use session_migration::TURN_ERROR_CODES;
pub(crate) use steer_migration::{
    accept_steer_input, get_steer_input, list_steer_inputs, record_steer_input, ReserveResult,
    StoredSteerInput,
};

pub(crate) fn statement<C: ConnectionTrait>(db: &C, sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(db.get_database_backend(), sql, values)
}
pub(crate) async fn one<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<Option<QueryResult>, AuthError> {
    db.query_one(statement(db, sql, values))
        .await
        .map_err(database_error)
}
pub(crate) async fn execute<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<ExecResult, AuthError> {
    db.execute(statement(db, sql, values))
        .await
        .map_err(database_error)
}

/// Native transaction lock order: admission guard, accounts ascending, organizations
/// ascending, memberships ascending, then session/attempt/invite/resource. No network
/// calls inside transactions.
pub(crate) async fn begin(db: &DatabaseConnection) -> Result<DatabaseTransaction, AuthError> {
    let tx = db.begin().await.map_err(database_error)?;
    if db.get_database_backend() == DbBackend::Sqlite {
        // Acquire write intent before any identity read (SQLite has one writer).
        execute(
            &tx,
            "UPDATE neev_schema_version SET version=version WHERE id=1",
            vec![],
        )
        .await?;
    }
    Ok(tx)
}

/// Serializes all admission/quota decisions before any domain lock is acquired.
pub(crate) async fn lock_admission_guard(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    if tx.get_database_backend() == DbBackend::Postgres {
        one(
            tx,
            "SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
    }
    Ok(())
}

pub(crate) async fn initialize(db: &DatabaseConnection) -> Result<(), AuthError> {
    let backend = db.get_database_backend();
    if !matches!(backend, DbBackend::Sqlite | DbBackend::Postgres) {
        return Err(AuthError::Unavailable);
    }
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        // Serialize schema initialization across processes on this database.
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    let result: Result<(), AuthError> = async {
        let sql = if backend == DbBackend::Postgres {
            "SELECT c.relname AS name,(c.relkind='r' AND NOT c.relispartition) AS native_table FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relkind IN ('r','p','v','m','f')"
        } else {
            "SELECT name,type='table' AS native_table FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%'"
        };
        let rows = tx
            .query_all(statement(&tx, sql, vec![]))
            .await
            .map_err(database_error)?;
        let names: std::collections::BTreeSet<String> = rows
            .iter()
            .map(|r| r.try_get("", "name").map_err(database_error))
            .collect::<Result<_, _>>()?;
        for row in &rows {
            if !row
                .try_get::<bool>("", "native_table")
                .map_err(database_error)?
            {
                return Err(AuthError::Unavailable);
            }
        }
        let v2_expected: std::collections::BTreeSet<String> = migration::TABLE_NAMES
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        let v3_expected: std::collections::BTreeSet<String> = v2_expected
            .iter()
            .cloned()
            .chain(
                content_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();
        let v5_expected: std::collections::BTreeSet<String> = v3_expected
            .iter()
            .cloned()
            .chain(
                agent_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();
        let v6_expected: std::collections::BTreeSet<String> = v5_expected.iter().cloned()
            .chain(document_format_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();
        let v7_expected: std::collections::BTreeSet<String> = v6_expected
            .iter()
            .cloned()
            .chain(
                agent_status_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();
        let v8_expected: std::collections::BTreeSet<String> = v7_expected
            .iter()
            .cloned()
            .chain(
                gateway_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();
        let v9_expected: std::collections::BTreeSet<String> = v8_expected
            .iter()
            .cloned()
            .chain(
                org_code_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();
        let v10_expected: std::collections::BTreeSet<String> = v9_expected
            .iter()
            .cloned()
            .chain(
                image_blob_migration::TABLE_NAMES
                    .iter()
                    .map(|name| (*name).to_owned()),
            )
            .collect();

        let v11_expected: std::collections::BTreeSet<String> = v10_expected.iter().cloned()
            .chain(session_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema15 adds two tables to the v11 inventory. Every earlier marker
        // still names the v11 set, so they fall through the tail and gain them.
        let v15_expected: std::collections::BTreeSet<String> = v11_expected.iter().cloned()
            .chain(attachment_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema16 adds one more: the mentions a message carries.
        let v16_expected: std::collections::BTreeSet<String> = v15_expected.iter().cloned()
            .chain(mention_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema17 adds NO new tables, only a column for attachment soft-delete.
        // It shares v16's table inventory.
        let v17_expected = v16_expected.clone();

        // Schema18 adds library tables (skills and MCP configs).
        let v18_expected: std::collections::BTreeSet<String> = v17_expected.iter().cloned()
            .chain(library_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema19 adds steering input table for mid-run user follow-ups.
        let v19_expected: std::collections::BTreeSet<String> = v18_expected.iter().cloned()
            .chain(steer_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema20 adds no table; schema21 adds the folder sharing defaults.
        let v21_expected: std::collections::BTreeSet<String> = v19_expected.iter().cloned()
            .chain(node_sharing_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema22 adds the org knowledge tables.
        let v22_expected: std::collections::BTreeSet<String> = v21_expected.iter().cloned()
            .chain(knowledge_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema23 adds the project/folder trash ledger.
        let v23_expected: std::collections::BTreeSet<String> = v22_expected.iter().cloned()
            .chain(node_trash_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema24 adds teams, grants and shared marks.
        let v24_expected: std::collections::BTreeSet<String> = v23_expected.iter().cloned()
            .chain(member_sharing_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Schema25 adds the Microsoft Teams bindings and thread ledgers.
        let v25_expected: std::collections::BTreeSet<String> = v24_expected.iter().cloned()
            .chain(msteams_migration::TABLE_NAMES.iter().map(|name| (*name).to_owned())).collect();

        // Empty or an exact known native inventory only; refuse legacy and partial data.
        if names.is_empty() {
            for table in migration::tables() {
                tx.execute(backend.build(&table))
                    .await
                    .map_err(database_error)?;
            }
            for index in migration::indexes() {
                tx.execute(backend.build(&index))
                    .await
                    .map_err(database_error)?;
            }
            content_migration::upgrade(&tx).await?;
            content_migration::harden_origins(&tx).await?;
            agent_migration::upgrade(&tx).await?;
            document_format_migration::upgrade(&tx).await?;
            agent_status_migration::upgrade(&tx).await?;
            gateway_migration::upgrade(&tx).await?;
            org_code_migration::upgrade(&tx).await?;
            image_blob_migration::upgrade(&tx).await?;
            session_migration::upgrade(&tx).await?;
            session_migration::link_nodes(&tx).await?;
            session_migration::record_activity(&tx).await?;
            session_migration::record_effort(&tx).await?;
            content_revision_cap_migration::raise_document_revision_cap(&tx).await?;
            attachment_migration::upgrade(&tx).await?;
            attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
            attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
            attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
            mention_migration::upgrade(&tx).await?;
            attachment_archived_migration::add_archived_column(&tx).await?;
            library_migration::upgrade(&tx).await?;
            steer_migration::upgrade(&tx).await?;
            provider_enum_migration::widen(&tx).await?;
            node_sharing_migration::upgrade(&tx).await?;
            knowledge_migration::upgrade(&tx).await?;
            node_trash_migration::upgrade(&tx).await?;
            member_sharing_migration::upgrade(&tx).await?;
            msteams_migration::upgrade(&tx).await?;
            execute(
                &tx,
                "INSERT INTO neev_schema_version(id,version) VALUES(1,$1)",
                vec![msteams_migration::SCHEMA_VERSION.into()],
            )
            .await?;
            return Ok(());
        }
        if names != v2_expected
            && names != v3_expected
            && names != v5_expected
            && names != v6_expected
            && names != v7_expected
            && names != v8_expected
            && names != v9_expected
            && names != v10_expected
            && names != v11_expected
            // Schema15 adds TABLES, so it is the first version since 11 whose
            // inventory differs. Without it here every restart of a migrated
            // database was refused by this guard before the match below could
            // recognise it — the server came up once, migrated, and then never
            // started again.
            && names != v15_expected
            && names != v16_expected
            && names != v17_expected
            && names != v18_expected
            && names != v19_expected
            && names != v21_expected
            && names != v22_expected
            && names != v23_expected
            && names != v24_expected
            && names != v25_expected
        {
            return Err(AuthError::Unavailable);
        }
        // Bounded marker inventory: exactly one (id=1,version) row; reject extra
        // or non-id1 markers without mutation (R10).
        let markers = tx
            .query_all(statement(
                &tx,
                "SELECT id,version FROM neev_schema_version LIMIT 2",
                vec![],
            ))
            .await
            .map_err(database_error)?;
        if markers.len() != 1
            || markers[0]
                .try_get::<i32>("", "id")
                .map_err(database_error)?
                != 1
        {
            return Err(AuthError::Unavailable);
        }
        let version = markers[0]
            .try_get::<i64>("", "version")
            .map_err(database_error)?;

        // DOCX support widens the existing attachment content-type CHECK. It
        // has no table inventory change, so apply it before validating any
        // schema that already contains the attachment tables.
        if names == v15_expected
            || names == v16_expected
            || names == v17_expected
            || names == v18_expected
            || names == v19_expected
            || names == v21_expected
            || names == v22_expected
            || names == v23_expected
            || names == v24_expected
        {
            attachment_migration::allow_docx_content_type(&tx).await?;
        }
        // Each arm advances to the status7 state; the shared tail then performs
        // the additive gateway8 upgrade and publishes marker8 exactly once. The
        // exact8 arm validates in place and preserves every row.
        match version {
            migration::SCHEMA_VERSION if names == v2_expected => {
                content_migration::upgrade(&tx).await?;
                content_migration::harden_origins(&tx).await?;
                agent_migration::upgrade(&tx).await?;
                document_format_migration::upgrade(&tx).await?;
                agent_status_migration::upgrade(&tx).await?;
            }
            content_migration::SCHEMA_VERSION if names == v3_expected => {
                content_migration::harden_origins(&tx).await?;
                agent_migration::upgrade(&tx).await?;
                document_format_migration::upgrade(&tx).await?;
                agent_status_migration::upgrade(&tx).await?;
            }
            content_migration::HARDENED_SCHEMA_VERSION if names == v3_expected => {
                agent_migration::upgrade(&tx).await?;
                document_format_migration::upgrade(&tx).await?;
                agent_status_migration::upgrade(&tx).await?;
            }
            agent_migration::SCHEMA_VERSION if names == v5_expected => {
                document_format_migration::upgrade(&tx).await?;
                agent_status_migration::upgrade(&tx).await?;
            }
            document_format_migration::SCHEMA_VERSION if names == v6_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::upgrade(&tx).await?;
            }
            agent_status_migration::SCHEMA_VERSION if names == v7_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
            }
            gateway_migration::SCHEMA_VERSION if names == v8_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
            }
            // Validate each known inventory before applying later migrations.
            org_code_migration::SCHEMA_VERSION if names == v9_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
            }
            image_blob_migration::SCHEMA_VERSION if names == v10_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
            }
            // A v11 database has every session table but not yet the additive
            // node pointer: validate what exists, then fall through to the tail,
            // which adds the column and publishes marker12.
            session_migration::SCHEMA_VERSION if names == v11_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
            }
            // A v12 database has the node link but not the activity column:
            // validate what exists, then fall through to the tail.
            session_migration::NODE_LINK_SCHEMA_VERSION if names == v11_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
            }
            // A v13 database has the activity column but not the effort one:
            // validate what exists, then fall through to the tail.
            session_migration::ACTIVITY_SCHEMA_VERSION if names == v11_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
            }
            // A v14 database has every column but not the attachment tables:
            // validate what exists, then fall through to the tail, which adds
            // them and publishes marker15. Every row is preserved.
            session_migration::EFFORT_SCHEMA_VERSION if names == v11_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
            }
            // A v15 database has the attachment tables but not the mention one:
            // validate what exists, then fall through to the tail, which adds it
            // and publishes marker16. Every row is preserved.
            attachment_migration::SCHEMA_VERSION if names == v15_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                // Relax BEFORE validating: a database at this version still carries
                // schema 15's ranged byte CHECK, and it is this call that removes it.
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
            attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
            attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
            }
            // Schema16 has mentions but not the archived column.
            mention_migration::SCHEMA_VERSION if names == v16_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                // Relax BEFORE validating: a database at this version still carries
                // schema 15's ranged byte CHECK, and it is this call that removes it.
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
            }
            // Schema17 has all columns but no library tables.
            attachment_archived_migration::SCHEMA_VERSION if names == v17_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
            }
            // Schema18: validate what exists, then fall through to add steer table.
            library_migration::SCHEMA_VERSION if names == v18_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
            }
            // Schema19: validate what exists, then fall through to the tail, which
            // widens the provider enum and publishes marker20. Rows preserved.
            steer_migration::SCHEMA_VERSION if names == v19_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
            }
            // Schema20 (no table of its own: v19's inventory). Validate in place,
            // then fall through to the tail, which adds the schema-21 table.
            provider_enum_migration::SCHEMA_VERSION if names == v19_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
            }
            // Schema21. Validate in place, then fall through to the tail, which adds
            // the schema-22 knowledge tables.
            node_sharing_migration::SCHEMA_VERSION if names == v21_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
                node_sharing_migration::validate(&tx).await?;
            }
            // Schema22. Validate in place, then fall through to the tail, which adds
            // the schema-23 trash ledger.
            knowledge_migration::SCHEMA_VERSION if names == v22_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
                node_sharing_migration::validate(&tx).await?;
                knowledge_migration::validate(&tx).await?;
            }
            // Schema23. Validate in place, then fall through to the tail, which adds
            // the schema-24 sharing tables.
            node_trash_migration::SCHEMA_VERSION if names == v23_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
                node_sharing_migration::validate(&tx).await?;
                knowledge_migration::validate(&tx).await?;
                node_trash_migration::validate(&tx).await?;
            }
            // Schema24. Validate in place, then fall through to the tail, which adds
            // the schema-25 Microsoft Teams tables.
            member_sharing_migration::SCHEMA_VERSION if names == v24_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
                node_sharing_migration::validate(&tx).await?;
                knowledge_migration::validate(&tx).await?;
                node_trash_migration::validate(&tx).await?;
                member_sharing_migration::validate(&tx).await?;
            }
            // Current (schema 25). Validate in place and preserve every row.
            msteams_migration::SCHEMA_VERSION if names == v25_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                org_code_migration::validate(&tx).await?;
                image_blob_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                session_migration::validate_activity(&tx).await?;
                session_migration::validate_effort(&tx).await?;
                content_revision_cap_migration::validate_document_revision_cap(&tx).await?;
                attachment_migration::validate(&tx).await?;
                attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
                attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
                attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
                attachment_byte_cap_migration::validate_attachment_byte_cap(&tx).await?;
                mention_migration::validate(&tx).await?;
                attachment_archived_migration::validate_archived_column(&tx).await?;
                library_migration::validate(&tx).await?;
                steer_migration::validate(&tx).await?;
                provider_enum_migration::validate(&tx).await?;
                node_sharing_migration::validate(&tx).await?;
                knowledge_migration::validate(&tx).await?;
                node_trash_migration::validate(&tx).await?;
                member_sharing_migration::validate(&tx).await?;
                msteams_migration::validate(&tx).await?;
                return Ok(());
            }
            _ => return Err(AuthError::Unavailable),
        }
        // Apply missing additive storage in order, then publish schema 25.
        if names != v8_expected && names != v9_expected && names != v10_expected && names != v11_expected && names != v15_expected && names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            gateway_migration::upgrade(&tx).await?;
        }
        if names != v9_expected && names != v10_expected && names != v11_expected && names != v15_expected && names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            org_code_migration::upgrade(&tx).await?;
        }
        if names != v10_expected && names != v11_expected && names != v15_expected && names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            image_blob_migration::upgrade(&tx).await?;
        }
        if names != v11_expected && names != v15_expected && names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            session_migration::upgrade(&tx).await?;
        }
        // Both are column-guarded, so every path can apply them unconditionally
        // and a database that already has them is unchanged.
        session_migration::link_nodes(&tx).await?;
        session_migration::record_activity(&tx).await?;
        session_migration::record_effort(&tx).await?;
        content_revision_cap_migration::raise_document_revision_cap(&tx).await?;
        if names != v15_expected && names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            attachment_migration::upgrade(&tx).await?;
        }
        // Constraint-guarded and idempotent, so every path applies it: a database
        // that never had the ranged CHECK is unchanged, one that has it is relaxed.
        attachment_byte_cap_migration::relax_attachment_byte_cap(&tx).await?;
        attachment_position_cap_migration::relax_attachment_position_cap(&tx).await?;
        attachment_position_cap_migration::validate_attachment_position_cap(&tx).await?;
        if names != v16_expected && names != v17_expected && names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            mention_migration::upgrade(&tx).await?;
        }
        // Column-guarded and idempotent: a database that already has the column
        // is unchanged. Adds the archived column for attachment soft-delete.
        attachment_archived_migration::add_archived_column(&tx).await?;
        // Table-guarded and idempotent: adds library tables if not present.
        if names != v18_expected && names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            library_migration::upgrade(&tx).await?;
        }
        // Table-guarded and idempotent: adds steer input table if not present.
        if names != v19_expected && names != v21_expected && names != v22_expected && names != v23_expected && names != v24_expected {
            steer_migration::upgrade(&tx).await?;
        }
        // Constraint-guarded and idempotent: widens the provider enum (schema 20).
        provider_enum_migration::widen(&tx).await?;
        provider_enum_migration::validate(&tx).await?;
        // Table-guarded and idempotent: adds the folder sharing defaults (21).
        node_sharing_migration::upgrade(&tx).await?;
        // Table-guarded and idempotent: adds the org knowledge tables (22).
        knowledge_migration::upgrade(&tx).await?;
        // Table-guarded and idempotent: adds the project/folder trash ledger (23).
        node_trash_migration::upgrade(&tx).await?;
        // Table-guarded and idempotent: adds teams, grants and shared marks (24).
        member_sharing_migration::upgrade(&tx).await?;
        // Table-guarded and idempotent: adds the Microsoft Teams tables (25).
        msteams_migration::upgrade(&tx).await?;
        execute(
            &tx,
            "UPDATE neev_schema_version SET version=$1 WHERE id=1",
            vec![msteams_migration::SCHEMA_VERSION.into()],
        )
        .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => tx.commit().await.map_err(database_error),
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 20 -> 19 (see `provider_enum_migration::downgrade`). Takes
/// the same initialization lock, commits ONLY a completed downgrade, and rolls
/// back every refusal, so a refused run leaves the database byte-for-byte as it
/// was.
pub(crate) async fn downgrade_provider_enum(
    db: &DatabaseConnection,
) -> Result<provider_enum_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match provider_enum_migration::downgrade(&tx).await {
        Ok(provider_enum_migration::Downgrade::Done) => {
            tx.commit().await.map_err(database_error)?;
            Ok(provider_enum_migration::Downgrade::Done)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 21 -> 20 (see `node_sharing_migration::downgrade`). Same
/// lock and commit discipline as `downgrade_provider_enum`: only a completed
/// downgrade commits; every refusal rolls back and changes nothing.
pub(crate) async fn downgrade_node_sharing(
    db: &DatabaseConnection,
) -> Result<node_sharing_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match node_sharing_migration::downgrade(&tx).await {
        Ok(node_sharing_migration::Downgrade::Done) => {
            tx.commit().await.map_err(database_error)?;
            Ok(node_sharing_migration::Downgrade::Done)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 22 -> 21 (see `knowledge_migration::downgrade`). Same
/// lock and commit discipline as the other guarded downgrades: only a completed
/// downgrade commits; every refusal rolls back and changes nothing.
pub(crate) async fn downgrade_knowledge(
    db: &DatabaseConnection,
) -> Result<knowledge_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match knowledge_migration::downgrade(&tx).await {
        Ok(knowledge_migration::Downgrade::Downgraded) => {
            tx.commit().await.map_err(database_error)?;
            Ok(knowledge_migration::Downgrade::Downgraded)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 23 -> 22 (see `node_trash_migration::downgrade`). Same
/// lock and commit discipline as the other guarded downgrades: only a completed
/// downgrade commits; every refusal rolls back and changes nothing.
pub(crate) async fn downgrade_node_trash(
    db: &DatabaseConnection,
) -> Result<node_trash_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match node_trash_migration::downgrade(&tx).await {
        Ok(node_trash_migration::Downgrade::Done) => {
            tx.commit().await.map_err(database_error)?;
            Ok(node_trash_migration::Downgrade::Done)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 25 -> 24 (see `msteams_migration::downgrade`). Same lock
/// and commit discipline as the other guarded downgrades.
pub(crate) async fn downgrade_msteams(
    db: &DatabaseConnection,
) -> Result<msteams_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match msteams_migration::downgrade(&tx).await {
        Ok(msteams_migration::Downgrade::Done) => {
            tx.commit().await.map_err(database_error)?;
            Ok(msteams_migration::Downgrade::Done)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Operator rollback 24 -> 23 (see `member_sharing_migration::downgrade`). Same
/// lock and commit discipline as the other guarded downgrades: only a completed
/// downgrade commits; every refusal rolls back and changes nothing.
pub(crate) async fn downgrade_member_sharing(
    db: &DatabaseConnection,
) -> Result<member_sharing_migration::Downgrade, AuthError> {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await?;
    }
    let tx = db.begin().await.map_err(database_error)?;
    if backend == DbBackend::Postgres {
        one(&tx, "SELECT pg_advisory_xact_lock(1852143222)", vec![]).await?;
    }
    match member_sharing_migration::downgrade(&tx).await {
        Ok(member_sharing_migration::Downgrade::Done) => {
            tx.commit().await.map_err(database_error)?;
            Ok(member_sharing_migration::Downgrade::Done)
        }
        Ok(refused) => {
            tx.rollback().await.map_err(database_error)?;
            Ok(refused)
        }
        Err(error) => {
            tx.rollback().await.map_err(database_error)?;
            Err(error)
        }
    }
}

/// Inspect restored evidence only: exact native3/4/5/6/7 inventory and actual marker.
/// Shares the migrator's authoritative table names but never calls the migrator.
pub(crate) async fn validate_content_snapshot<C: ConnectionTrait>(
    db: &C,
) -> Result<i64, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT c.relname AS name,(c.relkind='r' AND NOT c.relispartition) AS native_table FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relkind IN ('r','p','v','m','f')",
        DbBackend::Sqlite => "SELECT name,type='table' AS native_table FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%'",
        _ => return Err(AuthError::Unavailable),
    };
    let rows = db
        .query_all(statement(db, sql, vec![]))
        .await
        .map_err(database_error)?;
    let names: std::collections::BTreeSet<String> = rows
        .iter()
        .map(|row| row.try_get("", "name").map_err(database_error))
        .collect::<Result<_, _>>()?;
    for row in &rows {
        if !row
            .try_get::<bool>("", "native_table")
            .map_err(database_error)?
        {
            return Err(AuthError::Unavailable);
        }
    }
    let content_expected: std::collections::BTreeSet<String> = migration::TABLE_NAMES
        .iter()
        .chain(content_migration::TABLE_NAMES.iter())
        .map(|name| (*name).to_owned())
        .collect();
    let agent_expected: std::collections::BTreeSet<String> = content_expected
        .iter()
        .cloned()
        .chain(
            agent_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let format_expected: std::collections::BTreeSet<String> = agent_expected
        .iter()
        .cloned()
        .chain(
            document_format_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let status_expected: std::collections::BTreeSet<String> = format_expected
        .iter()
        .cloned()
        .chain(
            agent_status_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let gateway_expected: std::collections::BTreeSet<String> = status_expected
        .iter()
        .cloned()
        .chain(
            gateway_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let org_code_expected: std::collections::BTreeSet<String> = gateway_expected
        .iter()
        .cloned()
        .chain(
            org_code_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let image_expected: std::collections::BTreeSet<String> = org_code_expected
        .iter()
        .cloned()
        .chain(
            image_blob_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let markers = db
        .query_all(statement(
            db,
            "SELECT id,version FROM neev_schema_version LIMIT 2",
            vec![],
        ))
        .await
        .map_err(database_error)?;
    if markers.len() != 1
        || markers[0]
            .try_get::<i32>("", "id")
            .map_err(database_error)?
            != 1
    {
        return Err(AuthError::Unavailable);
    }
    let version = markers[0]
        .try_get::<i64>("", "version")
        .map_err(database_error)?;
    let session_expected: std::collections::BTreeSet<String> = image_expected
        .iter()
        .cloned()
        .chain(
            session_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let attachment_expected: std::collections::BTreeSet<String> = session_expected
        .iter()
        .cloned()
        .chain(
            attachment_migration::TABLE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
        )
        .collect();
    let valid = match version {
        content_migration::SCHEMA_VERSION | content_migration::HARDENED_SCHEMA_VERSION => {
            names == content_expected
        }
        agent_migration::SCHEMA_VERSION => names == agent_expected,
        document_format_migration::SCHEMA_VERSION => names == format_expected,
        agent_status_migration::SCHEMA_VERSION => names == status_expected,
        gateway_migration::SCHEMA_VERSION => names == gateway_expected,
        org_code_migration::SCHEMA_VERSION => names == org_code_expected,
        image_blob_migration::SCHEMA_VERSION => names == image_expected,
        // 12 and 13 add COLUMNS, so they share 11's table inventory. Without
        // these arms a migrated database falls to `_ => false` and the only
        // caller, `content::backup`, refuses to export or restore it.
        session_migration::SCHEMA_VERSION
        | session_migration::NODE_LINK_SCHEMA_VERSION
        | session_migration::ACTIVITY_SCHEMA_VERSION
        | session_migration::EFFORT_SCHEMA_VERSION => names == session_expected,
        // 15 and 16 add TABLES, so each has its own inventory.
        attachment_migration::SCHEMA_VERSION => names == attachment_expected,
        mention_migration::SCHEMA_VERSION | attachment_archived_migration::SCHEMA_VERSION => {
            // Both schema 16 and 17 share the same table inventory (17 only adds a column).
            names
                == attachment_expected
                    .iter()
                    .cloned()
                    .chain(
                        mention_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .collect::<std::collections::BTreeSet<String>>()
        }
        library_migration::SCHEMA_VERSION => {
            // Schema 18 adds library tables.
            names
                == attachment_expected
                    .iter()
                    .cloned()
                    .chain(
                        mention_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .chain(
                        library_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .collect::<std::collections::BTreeSet<String>>()
        }
        steer_migration::SCHEMA_VERSION
        | provider_enum_migration::SCHEMA_VERSION
        | node_sharing_migration::SCHEMA_VERSION
        | knowledge_migration::SCHEMA_VERSION
        | node_trash_migration::SCHEMA_VERSION
        | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION => {
            // Schema 19 adds steering input table; 20 only widens a CHECK; 21
            // adds the folder sharing defaults; 22 the org knowledge tables; 23
            // the project/folder trash ledger.
            let sharing: &[&str] = if version >= node_sharing_migration::SCHEMA_VERSION {
                node_sharing_migration::TABLE_NAMES
            } else {
                &[]
            };
            let knowledge: &[&str] = if version >= knowledge_migration::SCHEMA_VERSION {
                knowledge_migration::TABLE_NAMES
            } else {
                &[]
            };
            let trash: &[&str] = if version >= node_trash_migration::SCHEMA_VERSION {
                node_trash_migration::TABLE_NAMES
            } else {
                &[]
            };
            let sharing_tables: &[&str] = if version >= member_sharing_migration::SCHEMA_VERSION {
                member_sharing_migration::TABLE_NAMES
            } else {
                &[]
            };
            let teams_tables: &[&str] = if version >= msteams_migration::SCHEMA_VERSION {
                msteams_migration::TABLE_NAMES
            } else {
                &[]
            };
            names
                == attachment_expected
                    .iter()
                    .cloned()
                    .chain(
                        mention_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .chain(
                        library_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .chain(
                        steer_migration::TABLE_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    )
                    .chain(sharing.iter().map(|name| (*name).to_owned()))
                    .chain(knowledge.iter().map(|name| (*name).to_owned()))
                    .chain(trash.iter().map(|name| (*name).to_owned()))
                    .chain(sharing_tables.iter().map(|name| (*name).to_owned()))
                    .chain(teams_tables.iter().map(|name| (*name).to_owned()))
                    .collect::<std::collections::BTreeSet<String>>()
        }
        _ => false,
    };
    if !valid {
        return Err(AuthError::Unavailable);
    }
    if matches!(
        version,
        6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25
    ) {
        document_format_migration::validate(db).await?;
    }
    if matches!(
        version,
        7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25
    ) {
        agent_status_migration::validate(db).await?;
    }
    if matches!(version, 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        gateway_migration::validate(db).await?;
    }
    if matches!(version, 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        org_code_migration::validate(db).await?;
    }
    if matches!(version, 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        image_blob_migration::validate(db).await?;
    }
    if matches!(version, 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        session_migration::validate(db).await?;
    }
    if matches!(version, 12 | 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        session_migration::validate_node_link(db).await?;
    }
    if matches!(version, 13 | 14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25) {
        session_migration::validate_activity(db).await?;
    }
    if matches!(
        version,
        session_migration::EFFORT_SCHEMA_VERSION
            | attachment_migration::SCHEMA_VERSION
            | mention_migration::SCHEMA_VERSION
            | attachment_archived_migration::SCHEMA_VERSION
            | library_migration::SCHEMA_VERSION
            | steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        session_migration::validate_effort(db).await?;
        // 13 only. A schema-12 snapshot still has the historical 1000 bound and
        // is valid as itself; requiring the raised cap before the upgrade step
        // would refuse a database that is correct for its version.
        content_revision_cap_migration::validate_document_revision_cap(db).await?;
    }
    if matches!(
        version,
        attachment_migration::SCHEMA_VERSION
            | mention_migration::SCHEMA_VERSION
            | attachment_archived_migration::SCHEMA_VERSION
            | library_migration::SCHEMA_VERSION
            | steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        attachment_migration::validate(db).await?;
    }
    if matches!(
        version,
        mention_migration::SCHEMA_VERSION
            | attachment_archived_migration::SCHEMA_VERSION
            | library_migration::SCHEMA_VERSION
            | steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        mention_migration::validate(db).await?;
    }
    if matches!(
        version,
        attachment_archived_migration::SCHEMA_VERSION
            | library_migration::SCHEMA_VERSION
            | steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        attachment_archived_migration::validate_archived_column(db).await?;
    }
    if matches!(
        version,
        library_migration::SCHEMA_VERSION | steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        library_migration::validate(db).await?;
    }
    if matches!(
        version,
        steer_migration::SCHEMA_VERSION
            | provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        steer_migration::validate(db).await?;
    }
    if matches!(
        version,
        provider_enum_migration::SCHEMA_VERSION
            | node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        provider_enum_migration::validate(db).await?;
    }
    if matches!(
        version,
        node_sharing_migration::SCHEMA_VERSION
            | knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        node_sharing_migration::validate(db).await?;
    }
    if matches!(
        version,
        knowledge_migration::SCHEMA_VERSION
            | node_trash_migration::SCHEMA_VERSION
            | member_sharing_migration::SCHEMA_VERSION
            | msteams_migration::SCHEMA_VERSION
    ) {
        knowledge_migration::validate(db).await?;
    }
    if version >= node_trash_migration::SCHEMA_VERSION {
        node_trash_migration::validate(db).await?;
    }
    if version >= member_sharing_migration::SCHEMA_VERSION {
        member_sharing_migration::validate(db).await?;
    }
    if version == msteams_migration::SCHEMA_VERSION {
        msteams_migration::validate(db).await?;
    }
    Ok(version)
}
#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod attachment_tests;
#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod mention_tests;
#[cfg(test)]
mod tests;
