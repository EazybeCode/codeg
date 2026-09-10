//! Opt-in browser acceptance server for team onboarding. This module is
//! compiled only into the Rust test harness; production binaries cannot mount
//! its fake provider or control routes.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
use sea_orm::{ActiveModelTrait, Set};
use tokio::io::AsyncBufReadExt;

use super::{
    config::TeamConfig,
    router::build_router,
    state::TeamState,
    test_server::{
        BrowserCanaryServer, BrowserProviderServer, SessionResponseGate, BROWSER_CLIENT_ID,
        BROWSER_CLIENT_SECRET, ORG_A_ID,
    },
};
use crate::{
    db::entities::org,
    team_access::{
        core::TeamClock,
        credentials::CredentialService,
        github::GithubMembershipClient,
        oauth::GithubOAuthClient,
        onboarding::OnboardingService,
        principal::PrincipalService,
        secrets::SecretVault,
        test_auth::{fixed_now, TestClock},
    },
};

const ENCRYPTION_KEY: &str =
    "7878787878787878787878787878787878787878787878787878787878787878";
const STORED_ORG_NAME: &str = "Atlas Operations";
const STORED_ORG_LOGO: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

#[tokio::test]
#[ignore = "requires exported app and cached Playwright; see scripts/tests/team-auth-e2e.cjs"]
async fn team_browser_fixture_server() {
    let artifact_root =
        PathBuf::from(std::env::var("WORKOS_TEAM_E2E_ROOT").expect("explicit artifact root"));
    assert!(artifact_root.is_absolute(), "artifact root must be absolute");
    assert!(
        artifact_root.join("fixture-owned").is_file(),
        "driver-owned artifact root required"
    );
    let runtime_root = artifact_root.join("synthetic-runtime");
    assert!(
        runtime_root.join("fixture-owned").is_file(),
        "driver-owned synthetic runtime required"
    );
    let static_root =
        PathBuf::from(std::env::var("CODEG_STATIC_DIR").expect("explicit static export root"));
    assert!(static_root.is_absolute() && static_root.join("workspace.html").is_file());

    let app_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("team browser listener");
    let app_address = app_listener.local_addr().expect("team browser address");
    let app_origin = format!("http://{app_address}");
    let callback = format!("{app_origin}/auth/github/callback");

    let clock = Arc::new(TestClock::new(fixed_now()));
    let mut provider = BrowserProviderServer::start(Arc::clone(&clock), callback.clone()).await;
    let mut canary = BrowserCanaryServer::start().await;

    let mut values = HashMap::from([
        ("WORKOS_APP_ORIGIN".to_owned(), app_origin.clone()),
        ("GITHUB_CALLBACK_URL".to_owned(), callback.clone()),
        ("GITHUB_CLIENT_ID".to_owned(), BROWSER_CLIENT_ID.to_owned()),
        (
            "GITHUB_CLIENT_SECRET".to_owned(),
            BROWSER_CLIENT_SECRET.to_owned(),
        ),
        ("WORKOS_ENC_KEY".to_owned(), ENCRYPTION_KEY.to_owned()),
        (
            "DATABASE_URL".to_owned(),
            "postgres://browser-fixture@127.0.0.1/team".to_owned(),
        ),
        ("CODEG_HOST".to_owned(), "127.0.0.1".to_owned()),
        ("CODEG_PORT".to_owned(), app_address.port().to_string()),
        (
            "CODEG_STATIC_DIR".to_owned(),
            static_root.display().to_string(),
        ),
        ("WORKOS_ALLOW_LOOPBACK_PREVIEW".to_owned(), "true".to_owned()),
        (
            "WORKOS_ENABLE_CATALOG_RUNTIME".to_owned(),
            "false".to_owned(),
        ),
    ]);
    // Explicit insertion keeps the test setup visibly exhaustive if config
    // gains another optional browser-mode key.
    values.insert("WORKOS_SERVER_MODE".to_owned(), "team".to_owned());
    let config = TeamConfig::from_values(&values).expect("team browser config");

    let db = crate::db::test_helpers::fresh_in_memory_db().await.conn;
    seed_organization_branding(&db).await;
    let vault = Arc::new(SecretVault::from_hex_key(ENCRYPTION_KEY).expect("browser vault"));
    let authorization = Arc::new(
        GithubOAuthClient::new_for_test(
            BROWSER_CLIENT_ID.to_owned(),
            BROWSER_CLIENT_SECRET.to_owned(),
            config.callback.clone(),
            provider.origin(),
            Duration::from_secs(2),
        )
        .expect("loopback OAuth client"),
    );
    let membership = Arc::new(
        GithubMembershipClient::new_for_test(provider.origin(), Duration::from_secs(2))
            .expect("loopback GitHub membership client"),
    );
    let team_clock: Arc<dyn TeamClock> = clock;
    let credentials = Arc::new(CredentialService::new(
        db.clone(),
        Arc::clone(&vault),
        authorization.clone(),
        Arc::clone(&team_clock),
    ));
    let onboarding = Arc::new(OnboardingService::new(
        db.clone(),
        Arc::clone(&vault),
        authorization.clone(),
        membership.clone(),
        Arc::clone(&credentials),
        Arc::clone(&team_clock),
    ));
    let principals = Arc::new(PrincipalService::new(
        db.clone(),
        Arc::clone(&credentials),
        membership,
        Arc::clone(&team_clock),
    ));
    let state = Arc::new(
        TeamState::new(
            db,
            config,
            vault,
            authorization,
            onboarding,
            credentials,
            principals,
            team_clock,
        )
        .with_catalog_repository_origin_for_test(provider.origin()),
    );
    let session_delay = provider.session_delay();
    let settings_delay = provider.settings_delay();
    let app = build_router(state)
        .layer(middleware::from_fn_with_state(
            settings_delay.clone(),
            delay_team_settings_response,
        ))
        .layer(middleware::from_fn_with_state(
            session_delay.clone(),
            delay_team_session_response,
        ));

    println!(
        "TEAM_BROWSER_E2E_READY {}",
        serde_json::json!({
            "appOrigin": app_origin,
            "providerOrigin": provider.origin(),
            "canaryOrigin": canary.origin(),
            "evidence": "fake-loopback-github"
        })
    );

    let shutdown = async move {
        let mut line = String::new();
        let mut input = tokio::io::BufReader::new(tokio::io::stdin());
        let _ = input.read_line(&mut line).await;
        session_delay.release();
        settings_delay.release();
    };
    axum::serve(app_listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("team browser server");

    provider.shutdown().await;
    canary.shutdown().await;
    cleanup_synthetic_runtime(&artifact_root, &runtime_root);
}

async fn delay_team_settings_response(
    State(gate): State<Arc<SessionResponseGate>>,
    request: Request,
    next: Next,
) -> Response {
    let delayed = request.method() == axum::http::Method::GET
        && request.uri().path() == "/api/team/organization"
        && gate.take();
    let response = next.run(request).await;
    if delayed {
        gate.mark_started();
        gate.wait_for_release().await;
    }
    response
}

async fn delay_team_session_response(
    State(gate): State<Arc<SessionResponseGate>>,
    request: Request,
    next: Next,
) -> Response {
    let delayed = request.method() == axum::http::Method::GET
        && request.uri().path() == "/api/team/session"
        && gate.take();
    let response = next.run(request).await;
    if delayed {
        gate.mark_started();
        gate.wait_for_release().await;
    }
    response
}

async fn seed_organization_branding(db: &sea_orm::DatabaseConnection) {
    let now = fixed_now();
    org::ActiveModel {
        id: Set(1),
        github_org_id: Set(ORG_A_ID),
        login: Set("atlas-before-provider-refresh".into()),
        name: Set(Some(STORED_ORG_NAME.into())),
        avatar_url: Set(Some(STORED_ORG_LOGO.into())),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .expect("stored organization branding");
}

fn cleanup_synthetic_runtime(artifact_root: &std::path::Path, runtime_root: &std::path::Path) {
    assert_eq!(runtime_root.parent(), Some(artifact_root));
    assert!(runtime_root.join("fixture-owned").is_file());
    std::fs::remove_dir_all(runtime_root).expect("remove exact synthetic runtime");
}
