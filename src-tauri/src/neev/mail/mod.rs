use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend};

use crate::neev::error::AuthError;

pub(crate) mod quota;
pub(crate) mod resend;
#[cfg(test)]
mod tests;

pub(crate) struct MailMessage {
    pub(crate) recipient: String,
    pub(crate) subject: String,
    pub(crate) text: String,
}

#[async_trait]
pub(crate) trait MailSender: Send + Sync {
    async fn send(&self, message: MailMessage) -> Result<(), AuthError>;
}

#[derive(Clone)]
pub(crate) struct Service {
    database: DatabaseConnection,
    sender: Arc<dyn MailSender>,
}

impl Service {
    pub(crate) fn new(database: DatabaseConnection, sender: Arc<dyn MailSender>) -> Self {
        Self { database, sender }
    }

    pub(crate) async fn send(&self, message: MailMessage) -> Result<(), AuthError> {
        let lease = acquire_lease(&self.database).await?;
        let result = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.sender.send(message),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(AuthError::Unavailable),
        };
        let release = release_lease(&self.database, &lease).await;
        match (result, release) {
            (Err(error), _) => Err(error),
            (Ok(()), result) => result,
        }
    }
}

struct Lease {
    id: &'static str,
    expires_at: i64,
}

async fn acquire_lease(database: &DatabaseConnection) -> Result<Lease, AuthError> {
    let tx = crate::neev::db::begin(database).await?;
    if tx.get_database_backend() == DbBackend::Postgres {
        crate::neev::db::one(
            &tx,
            "SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
    }
    let now = database_time(&tx).await?;
    let expires_at = now.checked_add(30).ok_or(AuthError::Unavailable)?;
    for id in ["mail-0", "mail-1"] {
        let current = crate::neev::db::one(
            &tx,
            "SELECT expires_at FROM neev_mail_leases WHERE id=$1",
            vec![id.into()],
        )
        .await?
        .map(|row| {
            row.try_get::<i64>("", "expires_at")
                .map_err(crate::neev::error::database_error)
        })
        .transpose()?;
        match current {
            None => {
                crate::neev::db::execute(
                    &tx,
                    "INSERT INTO neev_mail_leases(id,expires_at) VALUES($1,$2)",
                    vec![id.into(), expires_at.into()],
                )
                .await?;
            }
            Some(current) if current <= now => {
                let updated = crate::neev::db::execute(
                    &tx,
                    "UPDATE neev_mail_leases SET expires_at=$1 WHERE id=$2 AND expires_at=$3",
                    vec![expires_at.into(), id.into(), current.into()],
                )
                .await?;
                if updated.rows_affected() != 1 {
                    continue;
                }
            }
            Some(_) => continue,
        }
        tx.commit()
            .await
            .map_err(crate::neev::error::database_error)?;
        return Ok(Lease { id, expires_at });
    }
    Err(AuthError::Limited)
}

async fn release_lease(database: &DatabaseConnection, lease: &Lease) -> Result<(), AuthError> {
    crate::neev::db::execute(
        database,
        "DELETE FROM neev_mail_leases WHERE id=$1 AND expires_at=$2",
        vec![lease.id.into(), lease.expires_at.into()],
    )
    .await?;
    Ok(())
}

async fn database_time<C: ConnectionTrait>(database: &C) -> Result<i64, AuthError> {
    let sql = match database.get_database_backend() {
        DbBackend::Postgres => {
            "SELECT CAST(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) AS BIGINT) AS now"
        }
        DbBackend::Sqlite => "SELECT CAST(strftime('%s','now') AS INTEGER) AS now",
        _ => return Err(AuthError::Unavailable),
    };
    crate::neev::db::one(database, sql, vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "now")
        .map_err(crate::neev::error::database_error)
}
