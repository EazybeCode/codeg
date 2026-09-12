use super::error::{database_error, AuthError};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, ExecResult, QueryResult,
    Statement, TransactionTrait, Value,
};
mod agent_migration;
mod content_migration;
pub(crate) mod entities;
mod migration;

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
            execute(
                &tx,
                "INSERT INTO neev_schema_version(id,version) VALUES(1,$1)",
                vec![agent_migration::SCHEMA_VERSION.into()],
            )
            .await?;
            return Ok(());
        }
        if names != v2_expected && names != v3_expected && names != v5_expected {
            return Err(AuthError::Unavailable);
        }
        let version = one(
            &tx,
            "SELECT version FROM neev_schema_version WHERE id=1",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get::<i64>("", "version")
        .map_err(database_error)?;
        match version {
            migration::SCHEMA_VERSION if names == v2_expected => {
                content_migration::upgrade(&tx).await?;
                content_migration::harden_origins(&tx).await?;
                agent_migration::upgrade(&tx).await?;
            }
            content_migration::SCHEMA_VERSION if names == v3_expected => {
                content_migration::harden_origins(&tx).await?;
                agent_migration::upgrade(&tx).await?;
            }
            content_migration::HARDENED_SCHEMA_VERSION if names == v3_expected => {
                agent_migration::upgrade(&tx).await?;
            }
            agent_migration::SCHEMA_VERSION if names == v5_expected => return Ok(()),
            _ => return Err(AuthError::Unavailable),
        }
        execute(
            &tx,
            "UPDATE neev_schema_version SET version=$1 WHERE id=1",
            vec![agent_migration::SCHEMA_VERSION.into()],
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

/// Inspect restored evidence only: exact native3/4/5 inventory and actual marker.
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
    let markers = db
        .query_all(statement(
            db,
            "SELECT id,version FROM neev_schema_version",
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
    let valid = match version {
        content_migration::SCHEMA_VERSION | content_migration::HARDENED_SCHEMA_VERSION => {
            names == content_expected
        }
        agent_migration::SCHEMA_VERSION => names == agent_expected,
        _ => false,
    };
    if !valid {
        return Err(AuthError::Unavailable);
    }
    Ok(version)
}
#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod tests;
