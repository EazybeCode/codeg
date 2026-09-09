use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use super::config::{is_postgres_database_url, TeamConfig};
use crate::{db::migration::Migrator, team_access::core::TeamAuthError};

pub(crate) async fn initialize_database(
    config: &TeamConfig,
) -> Result<DatabaseConnection, TeamAuthError> {
    if !is_postgres_database_url(&config.database_url) {
        return Err(TeamAuthError::Unavailable);
    }

    let mut migration_options = ConnectOptions::new(config.database_url.clone());
    migration_options
        .max_connections(1)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .sqlx_logging(false);
    let migration = Database::connect(migration_options)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    Migrator::up(&migration, None)
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;
    migration
        .close()
        .await
        .map_err(|_| TeamAuthError::Unavailable)?;

    let mut runtime_options = ConnectOptions::new(config.database_url.clone());
    runtime_options
        .max_connections(5)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .sqlx_logging(false);
    Database::connect(runtime_options)
        .await
        .map_err(|_| TeamAuthError::Unavailable)
}
