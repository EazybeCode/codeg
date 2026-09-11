use std::{process::ExitCode, sync::Arc};

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use tokio::sync::Semaphore;

use super::{
    auth::{magic, oauth},
    clock::{Clock, SystemClock},
    config::Config,
    db, http,
    mail::{self, resend::Resend},
};

pub(crate) struct ServerState {
    pub(crate) database: DatabaseConnection,
    pub(crate) config: Config,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
    pub(crate) auth_limit: Arc<Semaphore>,
    pub(crate) oauth: oauth::Service,
    pub(crate) magic: Option<magic::Service>,
    pub(crate) mail: Option<mail::Service>,
}

impl ServerState {
    pub(crate) fn new(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, super::error::AuthError> {
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        let mail = config
            .providers
            .email
            .as_ref()
            .map(|provider| {
                let sender = Arc::new(Resend::new(
                    provider.api_key.clone(),
                    provider.from.clone(),
                )?);
                Ok::<_, super::error::AuthError>(mail::Service::new(database.clone(), sender))
            })
            .transpose()?;
        let magic = mail.as_ref().map(|mail| {
            magic::Service::new(
                database.clone(),
                config.encryption_key,
                config.origin.clone(),
                clock.clone(),
                mail.clone(),
            )
        });
        Ok(Self {
            database,
            config,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            oauth,
            magic,
            mail,
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
            magic: None,
            mail: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_magic(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
        magic: magic::Service,
    ) -> Result<Self, super::error::AuthError> {
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        Ok(Self {
            database,
            config,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            oauth,
            magic: Some(magic),
            mail: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_mail(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
        mail: mail::Service,
    ) -> Result<Self, super::error::AuthError> {
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        Ok(Self {
            database,
            config,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            oauth,
            magic: None,
            mail: Some(mail),
        })
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
    axum::serve(
        listener,
        http::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|_| ())
}
