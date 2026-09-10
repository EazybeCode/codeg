use std::{process::ExitCode, sync::Arc};

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use tokio::sync::Semaphore;

use super::{
    auth::oauth,
    clock::{Clock, SystemClock},
    config::Config,
    db, http,
};

pub(crate) struct ServerState {
    pub(crate) database: DatabaseConnection,
    pub(crate) config: Config,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
    pub(crate) auth_limit: Arc<Semaphore>,
    pub(crate) oauth: oauth::Service,
}

impl ServerState {
    pub(crate) fn new(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, super::error::AuthError> {
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        Ok(Self {
            database,
            config,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            oauth,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_oauth(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
        oauth: oauth::Service,
    ) -> Self {
        Self {
            database,
            config,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            oauth,
        }
    }
}

pub(super) fn run_server() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::FAILURE,
    };
    match runtime.block_on(serve()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => {
            eprintln!("Neev native server failed to start");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), ()> {
    let config = Config::from_env().map_err(|_| ())?;
    let mut options = ConnectOptions::new(config.database_url.clone());
    options.max_connections(32).sqlx_logging(false);
    let database = Database::connect(options).await.map_err(|_| ())?;
    db::initialize(&database).await.map_err(|_| ())?;
    let bind = config.bind;
    let state =
        Arc::new(ServerState::new(database, config, Arc::new(SystemClock)).map_err(|_| ())?);
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|_| ())?;
    axum::serve(listener, http::router(state))
        .await
        .map_err(|_| ())
}
