use super::error::{database_error, AuthError};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, ExecResult, QueryResult,
    Statement, TransactionTrait, Value,
};
mod agent_migration;
mod agent_status_migration;
#[cfg(test)]
mod agent_status_tests;
mod content_migration;
mod document_format_migration;
#[cfg(test)]
mod document_format_tests;
pub(crate) mod entities;
mod gateway_migration;
#[cfg(test)]
mod gateway_migration_tests;
mod image_blob_migration;
mod migration;
mod org_code_migration;
mod session_migration;
#[cfg(test)]
mod session_tests;

/// The failure codes a turn may durably record. Re-exported so the run driver
/// and its tests cannot drift from what the schema actually accepts.
pub(crate) use session_migration::TURN_ERROR_CODES;

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
            execute(
                &tx,
                "INSERT INTO neev_schema_version(id,version) VALUES(1,$1)",
                vec![session_migration::NODE_LINK_SCHEMA_VERSION.into()],
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
            // Current. Validate in place, including the node link, and preserve
            // every row.
            session_migration::NODE_LINK_SCHEMA_VERSION if names == v11_expected => {
                document_format_migration::validate(&tx).await?;
                agent_status_migration::validate(&tx).await?;
                gateway_migration::validate(&tx).await?;
                session_migration::validate(&tx).await?;
                session_migration::validate_node_link(&tx).await?;
                return Ok(());
            }
            _ => return Err(AuthError::Unavailable),
        }
        // Apply missing additive storage in order, then publish schema 12.
        if names != v8_expected && names != v9_expected && names != v10_expected && names != v11_expected {
            gateway_migration::upgrade(&tx).await?;
        }
        if names != v9_expected && names != v10_expected && names != v11_expected {
            org_code_migration::upgrade(&tx).await?;
        }
        if names != v10_expected {
            image_blob_migration::upgrade(&tx).await?;
        }
        session_migration::upgrade(&tx).await?;
        session_migration::link_nodes(&tx).await?;
        execute(
            &tx,
            "UPDATE neev_schema_version SET version=$1 WHERE id=1",
            vec![session_migration::NODE_LINK_SCHEMA_VERSION.into()],
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
        session_migration::SCHEMA_VERSION => names == session_expected,
        _ => false,
    };
    if !valid {
        return Err(AuthError::Unavailable);
    }
    if matches!(version, 6 | 7 | 8 | 9 | 10 | 11) {
        document_format_migration::validate(db).await?;
    }
    if matches!(version, 7 | 8 | 9 | 10 | 11) {
        agent_status_migration::validate(db).await?;
    }
    if matches!(version, 8 | 9 | 10 | 11) {
        gateway_migration::validate(db).await?;
    }
    if matches!(version, 9 | 10 | 11) {
        org_code_migration::validate(db).await?;
    }
    if matches!(version, 10 | 11) {
        image_blob_migration::validate(db).await?;
    }
    if version == session_migration::SCHEMA_VERSION {
        session_migration::validate(db).await?;
    }
    Ok(version)
}
#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod tests;
