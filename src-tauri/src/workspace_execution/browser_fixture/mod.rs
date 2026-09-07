//! Opt-in browser acceptance server. Never present in a production binary.
//! Only external GitHub, local-Git transport and provider/status are fixtures.
mod external;
mod server;

use std::{path::PathBuf, sync::Arc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use crate::{app_state::AppState, db::entities::{org, user, session}};

pub const TOKEN: &str = "workspace-e2e-installation";
pub const COOKIE: &str = "workspace-e2e-session";

#[tokio::test]
#[ignore = "requires exported app and browser driver; see scripts/tests/workspace-e2e.cjs"]
async fn workspace_browser_fixture_server() {
    let root = PathBuf::from(std::env::var("WORKOS_E2E_ROOT").expect("explicit fixture root"));
    assert!(root.is_absolute() && root.join("fixture-owned").is_file(), "driver-owned isolated root required");
    assert_eq!(std::env::var_os("CLAUDE_CONFIG_DIR"), Some(root.join("provider").into_os_string()));
    let db = crate::db::test_helpers::fresh_disk_db(&root).await;
    if org::Entity::find_by_id(1).one(&db.conn).await.unwrap().is_none() {
        let now = chrono::Utc::now();
        org::ActiveModel { id: Set(1), github_org_id: Set(910001), login: Set("fixture-org".into()), name: Set(Some("Fixture Organization".into())), avatar_url: Set(None), created_at: Set(now), updated_at: Set(now) }.insert(&db.conn).await.unwrap();
        user::ActiveModel { id: Set(1), github_user_id: Set(910002), login: Set("fixture-person".into()), name: Set(Some("Fixture Person".into())), email: Set(None), avatar_url: Set(None), org_id: Set(1), role: Set("owner".into()), gh_token_enc: Set(None), gh_refresh_token_enc: Set(None), gh_token_expires_at: Set(None), created_at: Set(now), updated_at: Set(now) }.insert(&db.conn).await.unwrap();
        session::ActiveModel { id: Set(COOKIE.into()), user_id: Set(1), created_at: Set(now), expires_at: Set(now + chrono::Duration::days(1)) }.insert(&db.conn).await.unwrap();
    }
    let state = Arc::new(AppState::new_for_test(db, root.join("managed")));
    std::fs::create_dir_all(&state.data_dir).unwrap();
    // This function subscribes synchronously before returning its future.
    let lifecycle = tokio::spawn(crate::acp::lifecycle::lifecycle_subscriber_task(
        state.db.conn.clone(), state.connection_manager.clone_ref(), state.acp_event_bus.clone(), None,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3097").await.unwrap();
    let router = server::router(state, &root).await;
    println!("WORKSPACE_E2E_READY http://localhost:3097");
    axum::serve(listener, router).await.unwrap();
    lifecycle.abort();
}
