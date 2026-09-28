//! Additive archived column for attachment soft-delete (schema17).
//!
//! Adds an `archived` column to `neev_attachments` that defaults to FALSE.
//! Archived attachments are soft-deleted: they cannot be downloaded or included
//! in message/turn listings, but can be restored. Blobs and refs are retained.
//!
//! The column defaults to FALSE so existing attachments are active and new
//! uploads behave as before. An index on (org_id, chat_id, archived) and
//! (org_id, session_id, archived) supports efficient trash listing.
//!
//! Additive, forward-only, IDEMPOTENT: a database already migrated is left
//! untouched. Runs INSIDE the migration transaction.

use super::{database_error, execute, one, statement, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Value};

pub(super) const SCHEMA_VERSION: i64 = 17;

/// Add the `archived` column if it does not exist. Idempotent.
pub(super) async fn add_archived_column(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    match tx.get_database_backend() {
        DbBackend::Postgres => add_archived_postgres(tx).await,
        DbBackend::Sqlite => add_archived_sqlite(tx).await,
        _ => Err(AuthError::Unavailable),
    }
}

async fn add_archived_postgres(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    // Check if column already exists.
    let exists = one(
        tx,
        "SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='neev_attachments' AND column_name='archived'",
        vec![],
    )
    .await?
    .is_some();

    if !exists {
        // Add the column with a default value.
        execute(
            tx,
            "ALTER TABLE neev_attachments ADD COLUMN archived BOOLEAN NOT NULL DEFAULT FALSE",
            vec![],
        )
        .await?;

        // Add index for trash listing by chat.
        execute(
            tx,
            "CREATE INDEX neev_attachments_chat_archived ON neev_attachments(org_id, chat_id, archived) WHERE chat_id IS NOT NULL",
            vec![],
        )
        .await?;

        // Add index for trash listing by session.
        execute(
            tx,
            "CREATE INDEX neev_attachments_session_archived ON neev_attachments(org_id, session_id, archived) WHERE session_id IS NOT NULL",
            vec![],
        )
        .await?;
    }

    Ok(())
}

async fn add_archived_sqlite(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    // SQLite: check pragma table_info for the column.
    let columns = tx
        .query_all(statement(tx, "PRAGMA table_info(neev_attachments)", vec![]))
        .await
        .map_err(database_error)?;

    let has_archived = columns.iter().any(|row| {
        row.try_get::<String>("", "name")
            .map(|name| name == "archived")
            .unwrap_or(false)
    });

    if !has_archived {
        // Add the column with a default value.
        execute(
            tx,
            "ALTER TABLE neev_attachments ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0,1))",
            vec![],
        )
        .await?;

        // Add index for trash listing by chat.
        execute(
            tx,
            "CREATE INDEX neev_attachments_chat_archived ON neev_attachments(org_id, chat_id, archived)",
            vec![],
        )
        .await?;

        // Add index for trash listing by session.
        execute(
            tx,
            "CREATE INDEX neev_attachments_session_archived ON neev_attachments(org_id, session_id, archived)",
            vec![],
        )
        .await?;
    }

    Ok(())
}

/// Validate the archived column exists and has correct domain.
pub(super) async fn validate_archived_column<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    match db.get_database_backend() {
        DbBackend::Postgres => {
            // Check the column exists and is boolean.
            let row = one(
                db,
                "SELECT data_type FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='neev_attachments' AND column_name='archived'",
                vec![],
            )
            .await?
            .ok_or(AuthError::Unavailable)?;
            let data_type: String = row.try_get("", "data_type").map_err(database_error)?;
            if data_type != "boolean" {
                return Err(AuthError::Unavailable);
            }
        }
        DbBackend::Sqlite => {
            // Check the column exists.
            let columns = db
                .query_all(statement(db, "PRAGMA table_info(neev_attachments)", vec![]))
                .await
                .map_err(database_error)?;

            let has_archived = columns.iter().any(|row| {
                row.try_get::<String>("", "name")
                    .map(|name| name == "archived")
                    .unwrap_or(false)
            });

            if !has_archived {
                return Err(AuthError::Unavailable);
            }
        }
        _ => return Err(AuthError::Unavailable),
    }

    // Check that all archived values are valid (0 or 1 / false or true).
    let invalid = one(
        db,
        "SELECT COUNT(*) AS c FROM neev_attachments WHERE archived IS NULL OR archived NOT IN (FALSE, TRUE)",
        vec![],
    )
    .await?;

    if let Some(row) = invalid {
        let count: i64 = row.try_get("", "c").map_err(database_error)?;
        if count != 0 {
            return Err(AuthError::Unavailable);
        }
    }

    Ok(())
}
