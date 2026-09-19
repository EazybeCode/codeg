//! Loopback, test-only dev-agent harness — compiled ONLY with
//! `--features dev-agent` and never in a production build, so the production
//! fail-closed default (config refuses `NEEV_RUNTIME_ENABLED`, agents `Disabled`)
//! is unchanged. It exists solely so a human/browser can exercise the enabled
//! agent journey (send → stream → rendered reply → cancel) against a real
//! provider on `localhost`.
//!
//! Guards: requires explicit `NEEV_DEV_AGENT=1`; refuses a non-loopback bind;
//! seeds disposable data in the configured (throwaway) database; runs the normal
//! session/Origin/CSRF/per-chat authorization on every request; never prints
//! secrets beyond the disposable dev session cookie values it just minted.

use std::process::ExitCode;
use std::sync::Arc;

use sea_orm::{ConnectOptions, Database};

use crate::neev::agents::execution::{AgentRuntime, ProviderCliRuntime};
use crate::neev::auth::{identity, session};
use crate::neev::catalog::nodes;
use crate::neev::clock::{Clock, SystemClock};
use crate::neev::config::Config;
use crate::neev::content::resources;
use crate::neev::content::types::{ResourceInput, ResourceKind};
use crate::neev::db;
use crate::neev::http;
use crate::neev::startup::ServerState;
use crate::neev::types::{NodeInput, NodeKind, Provider, SignInIntent, VerifiedIdentity};

pub fn run() -> ExitCode {
    if std::env::var("NEEV_DEV_AGENT").ok().as_deref() != Some("1") {
        eprintln!("dev-agent: refusing; set NEEV_DEV_AGENT=1 to opt in (test-only)");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::FAILURE,
    };
    match runtime.block_on(serve()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("dev-agent: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), String> {
    let config = Config::from_env().map_err(|_| "invalid config".to_string())?;
    if !config.bind.ip().is_loopback() {
        return Err("dev-agent binds loopback only".into());
    }
    let origin = config.origin_header.clone();
    let preview = config.preview;
    let bind = config.bind;

    let mut options = ConnectOptions::new(config.database_url.clone());
    options.max_connections(8).sqlx_logging(false);
    let database = Database::connect(options)
        .await
        .map_err(|_| "database connect".to_string())?;
    db::initialize(&database)
        .await
        .map_err(|_| "database initialize".to_string())?;

    let now = SystemClock.now_unix();
    // Disposable seed: one account, one org, an authenticated org session.
    let user = identity::resolve(
        &database,
        VerifiedIdentity {
            provider: Provider::Google,
            issuer: "https://accounts.google.com".into(),
            subject: "dev-agent-subject".into(),
            verified_email: Some("dev-agent@example.test".into()),
            display_name: "Dev Agent".into(),
            avatar_url: None,
        },
        SignInIntent::Login,
        now,
    )
    .await
    .map_err(|_| "seed account".to_string())?;
    let org = db::one(
        &database,
        "INSERT INTO neev_organizations(owner_user_id,name,revision,disabled,created_at) VALUES($1,'Dev Org',1,false,$2) RETURNING id",
        vec![user.into(), now.into()],
    )
    .await
    .map_err(|_| "seed org".to_string())?
    .ok_or("seed org row")?
    .try_get::<i32>("", "id")
    .map_err(|_| "seed org id".to_string())?;
    db::execute(
        &database,
        "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,'admin',1,true)",
        vec![org.into(), user.into()],
    )
    .await
    .map_err(|_| "seed membership".to_string())?;

    let issued = session::issue_identity(&database, user, now)
        .await
        .map_err(|_| "issue session".to_string())?;
    let ident = session::identity(&database, &issued.session, now)
        .await
        .map_err(|_| "identity".to_string())?;
    let scoped = session::select_organization(&database, &ident, org, now)
        .await
        .map_err(|_| "scope org".to_string())?;
    let actor = session::organization(&database, &scoped.session, now)
        .await
        .map_err(|_| "org context".to_string())?;

    // Build the server with a REAL provider runtime bound (test-only injection).
    let mut server_state =
        ServerState::new(database, config, Arc::new(SystemClock)).map_err(|_| "server state")?;
    server_state.set_agent_runtime(AgentRuntime::Provider(ProviderCliRuntime::new(
        "claude",
        std::env::temp_dir(),
    )));
    let state = Arc::new(server_state);

    // Seed a project + chat the dev session owns, so the browser has a target.
    let node = nodes::create(
        &state,
        &actor,
        NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Dev Project".into(),
        },
    )
    .await
    .map_err(|_| "seed node".to_string())?
    .id;
    let chat = resources::create(
        &state,
        &actor,
        ResourceInput {
            node_id: node,
            kind: ResourceKind::Chat,
            title: "Dev Chat".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .map_err(|_| "seed chat".to_string())?
    .id;

    let names = http::cookies::CookieNames::for_preview(preview);
    eprintln!("dev-agent ready (LOOPBACK, test-only, runtime bound):");
    eprintln!("  url:        {origin}/workspace");
    eprintln!("  cookie:     {}={}", names.session, scoped.session);
    eprintln!("  cookie:     {}={}", names.csrf, scoped.csrf);
    eprintln!("  csrf-header: x-neev-csrf: {}", scoped.csrf);
    eprintln!("  chat_id:    {chat}");

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| "bind".to_string())?;
    axum::serve(
        listener,
        http::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|_| "serve".to_string())
}
