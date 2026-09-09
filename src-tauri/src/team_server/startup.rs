use std::{fmt, process::ExitCode, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{
    config::{TeamConfig, TeamConfigError},
    database::initialize_database,
    router::build_router,
    state::TeamState,
};
use crate::team_access::{
    core::{SystemClock, TeamClock},
    credentials::CredentialService,
    github::GithubMembershipClient,
    oauth::GithubOAuthClient,
    onboarding::OnboardingService,
    principal::PrincipalService,
    secrets::SecretVault,
};

pub fn run_team_server() -> ExitCode {
    let _log_guard = crate::logging::init::init_stderr_only();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("[TEAM] startup failed");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[TEAM] startup failed: {error}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<(), StartupError> {
    let config = TeamConfig::from_environment().map_err(StartupError::Config)?;
    let db = initialize_database(&config).await.map_err(|_| StartupError::Database)?;
    let vault = Arc::new(
        SecretVault::from_hex_key(&config.encryption_key).map_err(|_| StartupError::Services)?,
    );
    let oauth = Arc::new(
        GithubOAuthClient::new(
            config.github_client_id.clone(),
            config.github_client_secret.clone(),
            config.callback.clone(),
        )
        .map_err(|_| StartupError::Services)?,
    );
    let github = Arc::new(GithubMembershipClient::new().map_err(|_| StartupError::Services)?);
    let clock: Arc<dyn TeamClock> = Arc::new(SystemClock);
    let credentials = Arc::new(CredentialService::new(
        db.clone(),
        vault.clone(),
        oauth.clone(),
        clock.clone(),
    ));
    let onboarding = Arc::new(OnboardingService::new(
        db.clone(),
        vault.clone(),
        oauth.clone(),
        github.clone(),
        credentials.clone(),
        clock.clone(),
    ));
    let principals = Arc::new(PrincipalService::new(
        db.clone(),
        credentials.clone(),
        github,
        clock.clone(),
    ));
    let bind = config.bind;
    let state = Arc::new(TeamState::new(
        db,
        config,
        vault,
        oauth,
        onboarding.clone(),
        credentials,
        principals,
        clock,
    ));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| StartupError::Bind)?;
    let cancellation = CancellationToken::new();
    let cleanup = tokio::spawn(cleanup_loop(onboarding, cancellation.clone()));
    let shutdown = cancellation.clone();
    let server = axum::serve(listener, build_router(state)).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown.cancel();
    });
    let result = server.await.map_err(|_| StartupError::Server);
    cancellation.cancel();
    cleanup.await.map_err(|_| StartupError::Server)?;
    result
}

enum StartupError {
    Config(TeamConfigError),
    Database,
    Services,
    Bind,
    Server,
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "{error}"),
            Self::Database => formatter.write_str("database unavailable"),
            Self::Services => formatter.write_str("authentication services unavailable"),
            Self::Bind => formatter.write_str("listener unavailable"),
            Self::Server => formatter.write_str("server unavailable"),
        }
    }
}

async fn cleanup_loop(service: Arc<OnboardingService>, cancellation: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                if service.cleanup_expired().await.is_err() {
                    tracing::warn!(outcome = "unavailable", "team authentication cleanup failed");
                }
            }
        }
    }
}
