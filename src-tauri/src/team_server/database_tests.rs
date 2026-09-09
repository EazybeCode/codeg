use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;

use super::{config::TeamConfig, database::initialize_database};
use crate::{
    acp::custom_registry::{
        self, CustomAgentDef, CustomAgentSource, CustomAgentSpec, CustomDistributionKind, NpxSpec,
    },
    db::{
        entities::{app_metadata, team_github_credential, user},
        migration::Migrator,
    },
    models::agent::AgentType,
    team_access::{core::TeamAuthError, test_auth},
};

fn direct_config(database_url: String, static_root: &std::path::Path) -> TeamConfig {
    TeamConfig {
        origin: reqwest::Url::parse("https://workos.example.com").unwrap(),
        callback: reqwest::Url::parse(
            "https://workos.example.com/auth/github/callback",
        )
        .unwrap(),
        github_client_id: "github-client-id".into(),
        github_client_secret: "github-client-secret".into(),
        encryption_key:
            "1111111111111111111111111111111111111111111111111111111111111111".into(),
        database_url,
        bind: "0.0.0.0:3080".parse().unwrap(),
        static_root: static_root.to_path_buf(),
    }
}

#[tokio::test]
async fn database_initialization_rejects_non_postgres_even_for_direct_config() {
    let static_root = tempfile::tempdir().unwrap();
    let config = direct_config("sqlite::memory:".into(), static_root.path());
    assert_eq!(
        initialize_database(&config).await.err(),
        Some(TeamAuthError::Unavailable)
    );
}

/// `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --no-default-features --lib team_database_postgres_initializes_only_isolated_runtime_state -- --ignored --test-threads=1 --nocapture`
#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn team_database_postgres_initializes_only_isolated_runtime_state() {
    let _custom_registry_guard = custom_registry::hydrate_test_guard();
    let sentinel_agent_id = "team-init-must-not-hydrate";
    let sentinel_agent = CustomAgentDef {
        registry_id: sentinel_agent_id.into(),
        name: "Team init sentinel".into(),
        description: String::new(),
        version: "1.0.0".into(),
        distribution_kind: CustomDistributionKind::Npx,
        spec: CustomAgentSpec {
            npx: Some(NpxSpec {
                package: "team-init-sentinel@1.0.0".into(),
                cmd: Some("team-init-sentinel".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        icon_url: None,
        skills_shared_store: false,
        skills_dir: None,
        source: CustomAgentSource::Manual,
        version_probe: None,
        supports_mcp: true,
    };
    assert!(custom_registry::hydrate(&[sentinel_agent]).is_empty());
    assert!(custom_registry::all().contains(&AgentType::Custom(sentinel_agent_id)));

    let linked_root = tempfile::tempdir().unwrap();
    let linked_target = tempfile::tempdir().unwrap();
    let canonical_root = linked_root.path().canonicalize().unwrap();
    let canonical_target = linked_target.path().canonicalize().unwrap();
    crate::folder_links::register(&canonical_root, &canonical_target);

    let (name, admin, seeded) = test_auth::create_postgres_fixture()
        .await
        .expect("PostgreSQL fixture");
    let database_url = format!("postgres://air1@localhost:5432/{name}");

    let legacy = user::Entity::find()
        .filter(user::Column::GhTokenEnc.is_not_null())
        .one(&seeded)
        .await
        .expect("read seeded legacy credential")
        .expect("seeded legacy credential");
    assert_eq!(legacy.gh_token_enc.as_deref(), Some("legacy-must-not-be-read"));
    assert_eq!(
        team_github_credential::Entity::find()
            .count(&seeded)
            .await
            .expect("count team credentials before initialization"),
        0
    );
    seeded.close().await.expect("close fixture runtime connection");

    let static_root = tempfile::tempdir().unwrap();
    let config = direct_config(database_url, static_root.path());
    let runtime = initialize_database(&config)
        .await
        .expect("initialize isolated team database");

    assert!(!Migrator::get_applied_migrations(&runtime)
        .await
        .expect("read applied migrations")
        .is_empty());
    assert_eq!(
        app_metadata::Entity::find()
            .count(&runtime)
            .await
            .expect("count legacy startup metadata"),
        0,
        "team initialization must not invoke legacy app metadata startup"
    );
    assert_eq!(
        team_github_credential::Entity::find()
            .count(&runtime)
            .await
            .expect("count promoted credentials"),
        0,
        "legacy user credentials must not be promoted"
    );
    let legacy_after = user::Entity::find_by_id(legacy.id)
        .one(&runtime)
        .await
        .expect("read legacy credential after initialization")
        .expect("legacy user remains");
    assert_eq!(legacy_after.gh_token_enc, legacy.gh_token_enc);
    assert_eq!(legacy_after.gh_refresh_token_enc, legacy.gh_refresh_token_enc);
    assert!(
        custom_registry::all().contains(&AgentType::Custom(sentinel_agent_id)),
        "team initialization must not hydrate the custom-agent provider registry"
    );
    assert_eq!(
        crate::folder_links::canonical_targets_for(&canonical_root),
        vec![canonical_target.clone()],
        "team initialization must not hydrate the legacy filesystem registry"
    );

    test_auth::drop_postgres_fixture(&name, admin, runtime)
        .await
        .expect("verified PostgreSQL fixture cleanup");
    custom_registry::hydrate(&[]);
    crate::folder_links::unregister(&canonical_root, &canonical_target);
}
