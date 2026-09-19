use std::{process::ExitCode, sync::Arc};

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use tokio::sync::Semaphore;

use super::{
    auth::{magic, oauth},
    clock::{Clock, SystemClock},
    config::Config,
    content::{types::ContentError, BlobStore},
    db, http,
    mail::{self, resend::Resend},
};

pub(crate) struct ServerState {
    pub(crate) database: DatabaseConnection,
    pub(crate) config: Config,
    #[allow(dead_code)] // Populated before HTTP starts; later content handlers consume it.
    pub(crate) blobs: Arc<BlobStore>,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
    pub(crate) auth_limit: Arc<Semaphore>,
    pub(crate) oauth: oauth::Service,
    pub(crate) magic: Option<magic::Service>,
    pub(crate) mail: Option<mail::Service>,
    /// Personal-agent execution runtime. Production is always `Disabled` (the
    /// config layer refuses `NEEV_RUNTIME_ENABLED`); tests inject a runtime.
    pub(crate) agents: Arc<super::agents::execution::AgentRuntime>,
    /// Cancel handles for in-flight runs, so a separate request can cancel/restart.
    pub(crate) agent_jobs: super::agents::execution::JobRegistry,
}

impl ServerState {
    pub(crate) fn new(
        database: DatabaseConnection,
        config: Config,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, ContentError> {
        let blobs = Arc::new(BlobStore::open(&config.document_root)?);
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
            blobs,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            agents: super::agents::execution::disabled_runtime(),
            agent_jobs: super::agents::execution::new_job_registry(),
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
        let blobs = Arc::new(BlobStore::open(&config.document_root).expect("test document root"));
        Self {
            database,
            config,
            blobs,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            agents: super::agents::execution::disabled_runtime(),
            agent_jobs: super::agents::execution::new_job_registry(),
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
    ) -> Result<Self, ContentError> {
        let blobs = Arc::new(BlobStore::open(&config.document_root)?);
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        Ok(Self {
            database,
            config,
            blobs,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            agents: super::agents::execution::disabled_runtime(),
            agent_jobs: super::agents::execution::new_job_registry(),
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
    ) -> Result<Self, ContentError> {
        let blobs = Arc::new(BlobStore::open(&config.document_root)?);
        let oauth = oauth::Service::new(database.clone(), &config, clock.clone())?;
        Ok(Self {
            database,
            config,
            blobs,
            clock,
            auth_limit: Arc::new(Semaphore::new(32)),
            agents: super::agents::execution::disabled_runtime(),
            agent_jobs: super::agents::execution::new_job_registry(),
            oauth,
            magic: None,
            mail: Some(mail),
        })
    }

    #[cfg(any(test, feature = "dev-agent"))]
    pub(crate) fn set_agent_runtime(
        &mut self,
        runtime: super::agents::execution::AgentRuntime,
    ) {
        self.agents = Arc::new(runtime);
    }
}

/// Reject any path component that is a symlink: the root process later
/// creates/chowns/removes per-run workdirs under this root, and a symlinked
/// component would let it act through a tenant-swappable link.
fn no_symlink_component(path: &std::path::Path) -> Result<(), ()> {
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component);
        if let Ok(meta) = std::fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                return Err(());
            }
        }
    }
    Ok(())
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
    // Fail-closed: a partial or malformed private-transport configuration aborts
    // startup rather than running the browser surface without the private path.
    let private = crate::neev::gateway::transport::PrivateTransportConfig::from_env(|k| {
        std::env::var(k).ok()
    })
    .map_err(|_| ())?;
    let mut options = ConnectOptions::new(config.database_url.clone());
    options.max_connections(32).sqlx_logging(false);
    let database = Database::connect(options).await.map_err(|_| ())?;
    db::initialize(&database).await.map_err(|_| ())?;
    let bind = config.bind;
    let mut state = ServerState::new(database, config, Arc::new(SystemClock)).map_err(|_| ())?;
    // Production agent-runtime enablement: only when the config layer accepted
    // `NEEV_RUNTIME_ENABLED=true` do we bind the SANDBOXED runtime, which spawns
    // every agent through the per-tenant isolation + provider-egress launcher
    // (never a bare provider). Default stays `Disabled` (fail-closed).
    if state.config.runtime_enabled {
        // Validate operational paths and fail closed: an enabled runtime with a
        // missing/relative launcher or a non-absolute/symlinked workspace root
        // aborts startup rather than running agents through an unexpected path.
        let launcher = std::env::var("NEEV_AGENT_LAUNCHER").map_err(|_| ())?;
        let workspace_root = std::env::var("NEEV_AGENT_WORKSPACE_ROOT")
            .unwrap_or_else(|_| "/var/lib/neev-native/agents".to_string());
        let launcher_path = std::path::Path::new(&launcher);
        let workspace_path = std::path::Path::new(&workspace_root);
        if !launcher_path.is_absolute()
            || !launcher_path.is_file()
            || !workspace_path.is_absolute()
            || !workspace_path.is_dir()
            || no_symlink_component(workspace_path).is_err()
        {
            return Err(());
        }
        let python = std::env::var("NEEV_AGENT_PYTHON").unwrap_or_else(|_| "python3".to_string());
        let binary = std::env::var("NEEV_AGENT_BINARY").unwrap_or_else(|_| "claude".to_string());
        let provider_ipv4: Vec<String> = std::env::var("NEEV_AGENT_PROVIDER_IPV4")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        state.agents = super::agents::execution::sandboxed_runtime(
            python,
            launcher,
            binary,
            workspace_root,
            provider_ipv4,
        );
    }
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|_| ())?;
    let public = axum::serve(
        listener,
        http::router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    match private {
        // The private mTLS listener runs alongside the public browser surface;
        // if either exits (bind failure, invalid TLS material) the server stops.
        Some(cfg) => {
            let private = crate::neev::gateway::transport::serve(cfg, state.clone());
            tokio::try_join!(
                async { public.await.map_err(|_| ()) },
                async { private.await.map_err(|_| ()) },
            )
            .map(|_| ())
        }
        None => public.await.map_err(|_| ()),
    }
}
