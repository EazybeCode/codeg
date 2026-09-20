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

/// How often live logins are advanced and queued cleanups drained. Short enough
/// that a user who approves in their browser is connected promptly even with
/// nothing polling, and that a disconnect finishes while they watch.
const SUPERVISOR_SWEEP_SECS: u64 = 3;

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
    /// The local supervisor that performs provider sign-in effects. Production
    /// default is `Unavailable`, so a deployment without one refuses every
    /// effect instead of appearing to sign users in.
    pub(crate) supervisor: std::sync::Arc<super::agents::supervisor::Supervisor>,
    /// Sign-ins this process started, by attempt id.
    pub(crate) agent_logins: super::agents::supervisor::LoginRegistry,
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
            supervisor: std::sync::Arc::new(super::agents::supervisor::Supervisor::Unavailable),
            agent_logins: super::agents::supervisor::new_login_registry(),
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
            supervisor: std::sync::Arc::new(super::agents::supervisor::Supervisor::Unavailable),
            agent_logins: super::agents::supervisor::new_login_registry(),
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
            supervisor: std::sync::Arc::new(super::agents::supervisor::Supervisor::Unavailable),
            agent_logins: super::agents::supervisor::new_login_registry(),
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
            supervisor: std::sync::Arc::new(super::agents::supervisor::Supervisor::Unavailable),
            agent_logins: super::agents::supervisor::new_login_registry(),
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

/// Remove every abandoned per-run workdir, refusing if any survives.
///
/// A run directory is chowned to its lane uid at 0700, so a survivor means the
/// unprivileged server cannot clear it and a privileged sweep has not yet run.
/// In that state we cannot prove a lane uid is clean, and `SlotPool` starts full
/// on every boot and would hand it straight out — so the honest answer is to
/// fail closed rather than risk one tenant reading another's leftovers.
fn reclaim_agent_workdirs(root: &std::path::Path) -> Result<(), ()> {
    let entries = std::fs::read_dir(root).map_err(|_| ())?;
    let mut stranded = 0usize;
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("run-") {
            continue;
        }
        let path = entry.path();
        if std::fs::remove_dir_all(&path).is_err() && path.exists() {
            stranded += 1;
        }
    }
    if stranded == 0 {
        Ok(())
    } else {
        eprintln!(
            "Neev refuses to enable the agent runtime: {stranded} abandoned run workdir(s) under {} could not be removed; a privileged sweep must clear them before lane uids are reused",
            root.display()
        );
        Err(())
    }
}

/// The device + inode of a path, for the coordinator's own identity type.
/// The transport and the coordinator each declare their own, so the raw pair is
/// read once and mapped to whichever the caller needs.
#[cfg(unix)]
fn coordinator_identity(
    path: &std::path::Path,
) -> Result<super::agents::control_coordinator::FileIdentity, ()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|_| ())?;
    Ok(super::agents::control_coordinator::FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

/// The identity (device + inode) of a path, pinned so the file cannot be
/// swapped for another under the same name after we validated it.
#[cfg(unix)]
fn file_identity(
    path: &std::path::Path,
) -> Result<super::agents::control_transport::FileIdentity, ()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|_| ())?;
    Ok(super::agents::control_transport::FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(unix)]
fn protected_parent(
    parent: &std::path::Path,
) -> Result<super::agents::control_transport::ProtectedParent, ()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    super::agents::control_transport::validate_parent_path(parent).map_err(|_| ())?;
    if no_symlink_component(parent).is_err() {
        return Err(());
    }
    let meta = std::fs::metadata(parent).map_err(|_| ())?;
    Ok(super::agents::control_transport::ProtectedParent {
        absolute_path: parent.to_path_buf(),
        identity: file_identity(parent)?,
        gid: meta.gid(),
        mode: meta.permissions().mode() & 0o7777,
    })
}

/// Build the supervisor binding from the environment.
///
/// `Ok(None)` means "no supervisor deployed here" — the honest default, under
/// which every provider effect is refused. `Err(())` means one WAS configured
/// but something about it is missing, which aborts startup rather than leaving
/// an operator believing sign-in works.
///
/// Deliberately NO permission judgement here. The transport's own acceptance
/// gate (`control_transport::connect`) is the authority on ownership and modes:
/// it requires the key to be `root:control` with no world access and no group
/// *write* — i.e. exactly the `0640` the deployment provisions. Re-deciding
/// those rules here risked a stricter second opinion; an earlier draft rejected
/// any group bit at all, which would have refused the very key the gate
/// expects. This function resolves paths and identities; the gate judges them.
#[cfg(unix)]
fn bind_supervisor() -> Result<Option<super::agents::supervisor::Supervisor>, ()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let socket = match std::env::var("NEEV_SUPERVISOR_SOCKET") {
        Ok(value) => value,
        // Nothing configured at all.
        Err(_) => return Ok(None),
    };
    let key_path = std::env::var("NEEV_SUPERVISOR_KEY").map_err(|_| ())?;
    let epoch_dir = std::env::var("NEEV_SUPERVISOR_EPOCH_DIR").map_err(|_| ())?;
    // Three DISTINCT identities, and conflating them breaks the gate:
    //  - `peer_uid`/`peer_gid` are the supervisor process itself, which must be
    //    0:0 because the credential store requires real root for the volume
    //    tooling;
    //  - `control_gid` owns the socket and pre-shared key (the unprivileged web
    //    server's group, so it can reach them without being root).
    // An earlier draft used one value for both, which made every unprivileged
    // server fail the peer check against a root supervisor.
    let peer_uid: u32 = std::env::var("NEEV_SUPERVISOR_UID")
        .map_err(|_| ())?
        .parse()
        .map_err(|_| ())?;
    let peer_gid: u32 = std::env::var("NEEV_SUPERVISOR_GID")
        .map_err(|_| ())?
        .parse()
        .map_err(|_| ())?;
    let control_gid: u32 = std::env::var("NEEV_SUPERVISOR_CONTROL_GID")
        .map_err(|_| ())?
        .parse()
        .map_err(|_| ())?;

    let socket_path = std::path::Path::new(&socket).to_path_buf();
    let key = std::path::Path::new(&key_path).to_path_buf();
    // The gate requires each file to be a direct child of its protected parent.
    let socket_parent = protected_parent(socket_path.parent().ok_or(())?)?;
    let key_parent = protected_parent(key.parent().ok_or(())?)?;
    let socket_meta = std::fs::metadata(&socket_path).map_err(|_| ())?;
    let deployment = super::agents::control_transport::ControlDeployment {
        socket_identity: file_identity(&socket_path)?,
        key_identity: file_identity(&key)?,
        socket_path,
        key_path: key,
        socket_parent,
        key_parent,
        control_gid,
        self_uid: unsafe { libc::getuid() },
        self_gid: unsafe { libc::getgid() },
        peer_uid,
        peer_gid,
        socket_owner_uid: socket_meta.uid(),
    };

    // One coordinator owns the epoch at a time; the supervisor fences on it.
    let epoch_parent = std::path::Path::new(&epoch_dir);
    super::agents::control_transport::validate_parent_path(epoch_parent).map_err(|_| ())?;
    let epoch_meta = std::fs::metadata(epoch_parent).map_err(|_| ())?;
    let spec = super::agents::control_coordinator::CoordinatorFileSpec {
        parent: epoch_parent.to_path_buf(),
        parent_identity: coordinator_identity(epoch_parent)?,
        parent_gid: epoch_meta.gid(),
        parent_mode: epoch_meta.permissions().mode() & 0o7777,
        service_uid: unsafe { libc::getuid() },
        service_gid: unsafe { libc::getgid() },
        lock_identity: coordinator_identity(&epoch_parent.join("coordinator.lock"))?,
        epoch_identity: coordinator_identity(&epoch_parent.join("coordinator.epoch"))?,
    };
    let lease =
        super::agents::control_coordinator::CoordinatorLease::try_acquire(spec).map_err(|_| ())?;
    let live = super::agents::supervisor::LiveSupervisor::new(deployment, lease).map_err(|_| ())?;
    Ok(Some(super::agents::supervisor::Supervisor::Live(live)))
}

#[cfg(not(unix))]
fn bind_supervisor() -> Result<Option<super::agents::supervisor::Supervisor>, ()> {
    // The transport is Linux-only by design; a non-unix host simply has none.
    Ok(None)
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
        // Boot-safe lane reuse. The isolation lane pool starts full on every
        // restart, so a leftover `run-*` directory owned by a lane uid would be
        // handed to the NEXT tenant running under that uid — with the previous
        // tenant's private files still inside it. Refuse to enable the runtime
        // until the workspace is clean.
        reclaim_agent_workdirs(workspace_path)?;
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
    // Bind the local supervisor when one is deployed. Every part is REQUIRED
    // together and validated before use, so a half-configured host fails to
    // start rather than running with a connection surface that silently cannot
    // complete a sign-in.
    match bind_supervisor() {
        Ok(Some(supervisor)) => state.supervisor = Arc::new(supervisor),
        // Nothing configured: connections stay honestly unavailable.
        Ok(None) => {}
        Err(()) => {
            eprintln!("Neev supervisor is configured but unusable; refusing to start");
            return Err(());
        }
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
    // The supervisor lifecycle must not depend on anyone polling.
    //
    // Without this, a user who approves a sign-in and closes the tab is never
    // committed, and a cancelled or disconnected connection stays
    // `disconnecting` forever because nothing drains its queued cleanup — which
    // then blocks every future sign-in for that provider.
    let progress = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(SUPERVISOR_SWEEP_SECS)).await;
            crate::neev::agents::supervisor::sweep(&progress).await;
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

#[cfg(test)]
mod workdir_reclaim_tests {
    use super::reclaim_agent_workdirs;

    #[test]
    fn a_clean_workspace_and_a_removable_leftover_both_pass() {
        let root = tempfile::tempdir().unwrap();
        // Nothing to reclaim.
        assert!(reclaim_agent_workdirs(root.path()).is_ok());

        // A leftover the server CAN remove is reclaimed, not treated as a fault:
        // the common case is a crash before teardown, with the directory still
        // owned by the server.
        let leftover = root.path().join("run-abandoned");
        std::fs::create_dir_all(leftover.join("nested")).unwrap();
        std::fs::write(leftover.join("nested/file"), b"x").unwrap();
        assert!(reclaim_agent_workdirs(root.path()).is_ok());
        assert!(!leftover.exists());

        // Unrelated entries are left alone.
        let other = root.path().join("not-a-run");
        std::fs::create_dir_all(&other).unwrap();
        assert!(reclaim_agent_workdirs(root.path()).is_ok());
        assert!(other.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_surviving_run_workdir_refuses_to_enable_the_runtime() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let stuck = root.path().join("run-stuck");
        std::fs::create_dir_all(&stuck).unwrap();
        std::fs::write(stuck.join("credentials"), b"previous tenant").unwrap();
        // Stand in for "chowned to the lane uid at 0700": the server cannot
        // unlink the child because its parent is not writable.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let verdict = reclaim_agent_workdirs(root.path());
        let blocked = stuck.exists();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if blocked {
            // Fail closed: a lane uid whose private directory still holds
            // another tenant's files must never be handed out, and the pool
            // refills on every boot.
            assert!(
                verdict.is_err(),
                "a surviving run workdir must refuse the runtime"
            );
        }
    }
}
