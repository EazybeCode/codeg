use std::{process::ExitCode, sync::Arc};

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use tokio::sync::Semaphore;

use super::{
    auth::{magic, oauth},
    clock::{Clock, SystemClock},
    config::Config,
    content::{realtime::MessageHub, types::ContentError, BlobStore},
    db, http,
    mail::{self, resend::Resend},
};

/// How often expired provider-login attempts are swept. Well under the 600s
/// attempt deadline, so an abandoned login frees its connection promptly.
const PENDING_SWEEP_SECS: u64 = 60;

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
    /// Content-free fan-out for team-message push delivery. In-memory only: a
    /// subscriber never holds a database connection.
    pub(crate) realtime: Arc<MessageHub>,
    /// Server-owned live runs by durable run id. A session run is registered
    /// here for its lifetime so a dropped stream can REATTACH to it and replay
    /// what was already produced, instead of re-executing the prompt (§5.2).
    pub(crate) agent_runs: super::agents::runs::RunRegistry,
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
            realtime: Arc::new(MessageHub::default()),
            agent_runs: super::agents::runs::new_registry(),
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
            realtime: Arc::new(MessageHub::default()),
            agent_runs: super::agents::runs::new_registry(),
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
            realtime: Arc::new(MessageHub::default()),
            agent_runs: super::agents::runs::new_registry(),
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
            realtime: Arc::new(MessageHub::default()),
            agent_runs: super::agents::runs::new_registry(),
            oauth,
            magic: None,
            mail: Some(mail),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_realtime_policy(&mut self, policy: super::content::realtime::StreamPolicy) {
        self.realtime = Arc::new(MessageHub::new(policy));
    }

    #[cfg(any(test, feature = "dev-agent"))]
    pub(crate) fn set_agent_runtime(&mut self, runtime: super::agents::execution::AgentRuntime) {
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

/// True if the path is writable by group or other (or cannot be stat'd) — the
/// launcher/helper must be root-owned and not tenant-writable, or a tenant could
/// swap what the privileged helper executes.
fn is_group_or_other_writable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o022 != 0)
        .unwrap_or(true)
}

/// True if the path is owned by root (uid 0). A privileged, file-capped helper
/// that is not root-owned could be replaced by the tenant uid.
fn is_root_owned(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
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
    // Any turn still marked `running` belongs to a process that no longer
    // exists — its stream died with it and it can never complete. Settle those
    // before the HTTP surface opens, so no user is shown a reply that spins
    // forever (§5.2: a truly gone run is Failed, never silently re-run).
    super::agents::sessions::reconcile_interrupted(&database, SystemClock.now_unix())
        .await
        .map_err(|_| ())?;
    let bind = config.bind;
    let mut state = ServerState::new(database, config, Arc::new(SystemClock)).map_err(|_| ())?;
    // Production agent-runtime enablement: only when the config layer accepted
    // `NEEV_RUNTIME_ENABLED=true` do we bind the SANDBOXED runtime, which spawns
    // every agent through the per-tenant isolation + provider-egress launcher
    // (never a bare provider). Default stays `Disabled` (fail-closed).
    if state.config.runtime_enabled {
        // Validate operational config and FAIL CLOSED at boot: an enabled runtime
        // with a missing/relative/writable launcher, a non-absolute/symlinked
        // workspace root, a non-absolute provider binary, or a malformed provider
        // allowlist aborts startup rather than deferring the failure to first run.
        let launcher = std::env::var("NEEV_AGENT_LAUNCHER").map_err(|_| ())?;
        let workspace_root = std::env::var("NEEV_AGENT_WORKSPACE_ROOT")
            .unwrap_or_else(|_| "/data/agents".to_string()); // matches the image ENV/volume
        let launcher_path = std::path::Path::new(&launcher);
        let workspace_path = std::path::Path::new(&workspace_root);
        if !launcher_path.is_absolute()
            || !launcher_path.is_file()
            || !is_root_owned(launcher_path) // launcher must be root-owned...
            || is_group_or_other_writable(launcher_path) // ...and not tenant-writable
            || !workspace_path.is_absolute()
            || !workspace_path.is_dir()
            || no_symlink_component(workspace_path).is_err()
        {
            return Err(());
        }
        // The provider binary must be an absolute path (no PATH ambiguity for the
        // dropped uid) and exist.
        let binary = std::env::var("NEEV_AGENT_BINARY").map_err(|_| ())?;
        let binary_path = std::path::Path::new(&binary);
        if !binary_path.is_absolute() || !binary_path.is_file() {
            return Err(());
        }
        // The privileged fixed-command launch helper (`neev-agent-caplaunch`,
        // file-capped) is REQUIRED so the server itself stays unprivileged
        // (deploy review C1): it must be an absolute, existing, non-writable file.
        let cap_helper = std::env::var("NEEV_AGENT_CAP_HELPER").map_err(|_| ())?;
        let cap_helper_path = std::path::Path::new(&cap_helper);
        if !cap_helper_path.is_absolute()
            || !cap_helper_path.is_file()
            || !is_root_owned(cap_helper_path)
            || is_group_or_other_writable(cap_helper_path)
        {
            return Err(());
        }
        let python = std::env::var("NEEV_AGENT_PYTHON").unwrap_or_else(|_| "python3".to_string());
        // Parse + validate the provider allowlist at boot: each entry must be a
        // public IPv4 (mirrors the login-egress validator's intent). Empty is
        // allowed (=> deny-all egress, fail-safe); any malformed entry aborts.
        let raw_ips = std::env::var("NEEV_AGENT_PROVIDER_IPV4").unwrap_or_default();
        let mut provider_ipv4: Vec<String> = Vec::new();
        for entry in raw_ips.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match entry.parse::<std::net::Ipv4Addr>() {
                Ok(ip)
                    if !ip.is_private()
                        && !ip.is_loopback()
                        && !ip.is_link_local()
                        && !ip.is_unspecified()
                        && !ip.is_multicast()
                        && !ip.is_broadcast() =>
                {
                    provider_ipv4.push(ip.to_string());
                }
                _ => return Err(()), // malformed/non-public allowlist entry: fail closed
            }
        }
        // Only providers with a validated absolute binary are bound. A provider
        // with none is refused as `Unsupported` at run time — never served by
        // another provider's binary. `NEEV_AGENT_CODEX_BINARY` is optional and
        // held to the same absolute/existing rule as the Claude one.
        let mut binaries = super::agents::execution::ProviderBinaries::claude(binary);
        if let Ok(codex) = std::env::var("NEEV_AGENT_CODEX_BINARY") {
            let codex_path = std::path::Path::new(&codex);
            if !codex_path.is_absolute() || !codex_path.is_file() {
                return Err(());
            }
            binaries.codex = Some(codex);
        }
        state.agents = super::agents::execution::sandboxed_runtime(
            python,
            launcher,
            binaries,
            workspace_root,
            provider_ipv4,
            Some(cap_helper),
        );
    }
    let state = Arc::new(state);
    // Connection recovery, wired into the real lifecycle.
    //
    // A provider sign-in interrupted by a crash leaves its connection
    // `connecting` with a `pending` attempt, and `intents::begin` refuses a
    // fresh login while the connection is not `disconnected` — so without this
    // the user can never reconnect that provider, and therefore never obtain
    // the per-user credential a run requires (§5.4).
    //
    // `recover_startup` is safe HERE and only here: this runs once, before the
    // listener accepts anything, so no attempt owned by THIS process can be in
    // flight, and every `pending` attempt it finds belongs to a process that no
    // longer exists. It is deliberately not called again later — the periodic
    // sweep below uses `expire_pending`, which only touches attempts whose own
    // deadline has passed and therefore can never cancel a live login.
    //
    // NOTE for a future multi-instance deployment: `recover_startup` assumes a
    // single server owns this database. A second instance booting while the
    // first serves logins would reset those logins; gate it on a supervisor
    // lease before scaling out.
    if let Err(error) = crate::neev::agents::recovery::recover_startup(&state).await {
        eprintln!("Neev agent connection recovery failed at startup: {error:?}");
        return Err(());
    }
    let sweeper = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(PENDING_SWEEP_SECS)).await;
            // Only EXPIRED attempts; an in-progress login is untouched.
            let _ = crate::neev::agents::recovery::expire_pending(&sweeper).await;
        }
    });
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
            tokio::try_join!(async { public.await.map_err(|_| ()) }, async {
                private.await.map_err(|_| ())
            },)
            .map(|_| ())
        }
        None => public.await.map_err(|_| ()),
    }
}
