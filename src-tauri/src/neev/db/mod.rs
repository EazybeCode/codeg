use super::error::{database_error, AuthError};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, ExecResult, QueryResult,
    Statement, TransactionTrait, Value,
};
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

/// Native transaction lock order: accounts ascending, org, memberships ascending,
/// then session/attempt/invite/resource. No network calls inside transactions.
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
    let sql = if backend == DbBackend::Postgres {
        "SELECT tablename AS name FROM pg_tables WHERE schemaname=current_schema()"
    } else {
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    };
    let rows = tx
        .query_all(statement(&tx, sql, vec![]))
        .await
        .map_err(database_error)?;
    let names: std::collections::BTreeSet<String> = rows
        .iter()
        .map(|r| r.try_get("", "name").map_err(database_error))
        .collect::<Result<_, _>>()?;
    let expected: std::collections::BTreeSet<String> = migration::TABLE_NAMES
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    // Empty or exactly our known native schema; refuse legacy and partial data.
    if !names.is_empty() {
        if names != expected {
            return Err(AuthError::Unavailable);
        }
        let version = one(
            &tx,
            "SELECT version FROM neev_schema_version WHERE id=1",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
        if version
            .try_get::<i64>("", "version")
            .map_err(database_error)?
            != 1
        {
            return Err(AuthError::Unavailable);
        }
    } else {
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
        execute(
            &tx,
            "INSERT INTO neev_schema_version(id,version) VALUES(1,1)",
            vec![],
        )
        .await?;
    }
    tx.commit().await.map_err(database_error)
}
#[cfg(test)]
mod tests;
