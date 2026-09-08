use chrono::{TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, PaginatorTrait, Set, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

use super::{m20260908_000002_team_credentials::Migration as CredentialsMigration, Migrator};
use crate::db::entities::{
    org, team_github_credential, team_oauth_attempt, team_pending_onboarding, user,
};

const ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const DATABASE_PREFIX: &str = "workos_team_credentials_test_";

type TestResult<T> = Result<T, String>;

fn timestamp(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 12, 0, second)
        .single()
        .expect("valid fixture timestamp")
}

fn oauth_attempt(state_hash: &str) -> team_oauth_attempt::ActiveModel {
    team_oauth_attempt::ActiveModel {
        state_hash: Set(state_hash.into()),
        browser_hash: Set("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
        encrypted_verifier: Set("v1.invented-verifier-ciphertext".into()),
        created_at: Set(timestamp(1)),
        expires_at: Set(timestamp(2)),
    }
}

fn pending(token_hash: &str, github_user_id: i64) -> team_pending_onboarding::ActiveModel {
    team_pending_onboarding::ActiveModel {
        token_hash: Set(token_hash.into()),
        csrf_hash: Set("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into()),
        github_user_id: Set(github_user_id),
        encrypted_payload: Set("v1.invented-pending-ciphertext".into()),
        created_at: Set(timestamp(3)),
        expires_at: Set(timestamp(4)),
    }
}

fn credential(
    user_id: i32,
    github_user_id: i64,
    generation: i64,
    state: &str,
) -> team_github_credential::ActiveModel {
    team_github_credential::ActiveModel {
        user_id: Set(user_id),
        github_user_id: Set(github_user_id),
        generation: Set(generation),
        state: Set(state.into()),
        encrypted_payload: Set(Some("v1.invented-credential-ciphertext".into())),
        refresh_owner: Set(None),
        refresh_expires_at: Set(None),
        created_at: Set(timestamp(5)),
        updated_at: Set(timestamp(6)),
    }
}

async fn database_before_credentials() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("open isolated SQLite database");
    let credential_migration_position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260908_000002_team_credentials")
        .expect("credential migration must be registered");
    Migrator::up(&db, Some(credential_migration_position as u32))
        .await
        .expect("apply migrations preceding team credentials");
    db
}

#[tokio::test]
async fn team_credentials_migration_preserves_legacy_rows_without_promoting_tokens() {
    let db = database_before_credentials().await;
    let organization = org::ActiveModel {
        id: Set(8101),
        github_org_id: Set(98101),
        login: Set("legacy-credential-org".into()),
        name: Set(Some("Legacy Credential Organization".into())),
        avatar_url: Set(None),
        created_at: Set(timestamp(1)),
        updated_at: Set(timestamp(2)),
    }
    .insert(&db)
    .await
    .unwrap();
    let legacy_user = user::ActiveModel {
        id: Set(8201),
        github_user_id: Set(98201),
        login: Set("legacy-credential-user".into()),
        name: Set(None),
        email: Set(None),
        avatar_url: Set(None),
        org_id: Set(organization.id),
        role: Set("owner".into()),
        gh_token_enc: Set(Some("legacy-encrypted-access".into())),
        gh_refresh_token_enc: Set(Some("legacy-encrypted-refresh".into())),
        gh_token_expires_at: Set(Some(timestamp(3))),
        created_at: Set(timestamp(4)),
        updated_at: Set(timestamp(5)),
    }
    .insert(&db)
    .await
    .unwrap();

    CredentialsMigration.up(&SchemaManager::new(&db)).await.unwrap();

    assert_eq!(team_oauth_attempt::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(team_pending_onboarding::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(team_github_credential::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(
        user::Entity::find_by_id(legacy_user.id)
            .one(&db)
            .await
            .unwrap(),
        Some(legacy_user)
    );
}

#[tokio::test]
async fn team_credentials_migration_round_trips_all_models_and_indexes_expiry_and_identity() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_oauth = oauth_attempt("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .insert(&db.conn)
        .await
        .unwrap();
    let expected_pending = pending(
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        98301,
    )
    .insert(&db.conn)
    .await
    .unwrap();
    let expected_credential = credential(8301, 98401, 1, "active")
        .insert(&db.conn)
        .await
        .unwrap();

    assert_eq!(team_oauth_attempt::Entity::find_by_id(expected_oauth.state_hash.clone()).one(&db.conn).await.unwrap(), Some(expected_oauth));
    assert_eq!(team_pending_onboarding::Entity::find_by_id(expected_pending.token_hash.clone()).one(&db.conn).await.unwrap(), Some(expected_pending));
    assert_eq!(team_github_credential::Entity::find_by_id(expected_credential.user_id).one(&db.conn).await.unwrap(), Some(expected_credential));

    let indexes = db.conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'index'".to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    for expected in [
        "idx_team_oauth_attempt_expires_at",
        "idx_team_pending_onboarding_expires_at",
        "uidx_team_github_credential_github_user_id",
    ] {
        assert!(indexes.iter().any(|name| name == expected), "missing {expected}: {indexes:?}");
    }
}

#[tokio::test]
async fn team_credentials_migration_requires_explicit_generation_state_and_valid_rows() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let missing_generation_and_state = team_github_credential::ActiveModel {
        user_id: Set(8310),
        github_user_id: Set(98410),
        encrypted_payload: Set(Some("v1.invented".into())),
        refresh_owner: Set(None),
        refresh_expires_at: Set(None),
        created_at: Set(timestamp(5)),
        updated_at: Set(timestamp(6)),
        ..Default::default()
    };
    assert!(missing_generation_and_state.insert(&db.conn).await.is_err());

    assert!(pending(
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        0,
    )
    .insert(&db.conn)
    .await
    .is_err());
    for invalid in [
        credential(0, 98411, 1, "active"),
        credential(8312, 0, 1, "active"),
        credential(8313, 98413, 0, "active"),
        credential(8314, 98414, 1, "pending"),
    ] {
        assert!(invalid.insert(&db.conn).await.is_err());
    }

    credential(8315, 98415, 1, "active")
        .insert(&db.conn)
        .await
        .unwrap();
    assert!(credential(8316, 98415, 1, "active")
        .insert(&db.conn)
        .await
        .is_err());
}

#[tokio::test]
async fn team_credentials_migration_enforces_each_credential_state_shape() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;

    let mut refreshing = credential(8321, 98521, 2, "refreshing");
    refreshing.refresh_owner = Set(Some("worker-1".into()));
    refreshing.refresh_expires_at = Set(Some(timestamp(7)));
    refreshing.insert(&db.conn).await.unwrap();

    let mut reauth = credential(8322, 98522, 3, "reauth_required");
    reauth.encrypted_payload = Set(None);
    reauth.insert(&db.conn).await.unwrap();

    let mut active_with_lease = credential(8323, 98523, 1, "active");
    active_with_lease.refresh_owner = Set(Some("worker-2".into()));
    assert!(active_with_lease.insert(&db.conn).await.is_err());

    let refreshing_without_deadline = credential(8324, 98524, 1, "refreshing");
    assert!(refreshing_without_deadline.insert(&db.conn).await.is_err());

    let reauth_with_payload = credential(8325, 98525, 1, "reauth_required");
    assert!(reauth_with_payload.insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn team_credentials_migration_rerun_preserves_data_in_all_three_tables() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_oauth = oauth_attempt("1111111111111111111111111111111111111111111111111111111111111111").insert(&db.conn).await.unwrap();
    let expected_pending = pending("2222222222222222222222222222222222222222222222222222222222222222", 98601).insert(&db.conn).await.unwrap();
    let expected_credential = credential(8331, 98602, 4, "active").insert(&db.conn).await.unwrap();

    CredentialsMigration.up(&SchemaManager::new(&db.conn)).await.unwrap();

    assert_eq!(team_oauth_attempt::Entity::find_by_id(expected_oauth.state_hash.clone()).one(&db.conn).await.unwrap(), Some(expected_oauth));
    assert_eq!(team_pending_onboarding::Entity::find_by_id(expected_pending.token_hash.clone()).one(&db.conn).await.unwrap(), Some(expected_pending));
    assert_eq!(team_github_credential::Entity::find_by_id(expected_credential.user_id).one(&db.conn).await.unwrap(), Some(expected_credential));
}

async fn assert_down_refuses_without_dropping_any_table(db: &DatabaseConnection) {
    assert!(CredentialsMigration.down(&SchemaManager::new(db)).await.is_err());
    let manager = SchemaManager::new(db);
    for table in [
        "team_oauth_attempt",
        "team_pending_onboarding",
        "team_github_credential",
    ] {
        assert!(manager.has_table(table).await.unwrap(), "down dropped {table}");
    }
}

#[tokio::test]
async fn team_credentials_migration_down_refuses_populated_oauth_attempt_without_partial_drop() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    oauth_attempt("3333333333333333333333333333333333333333333333333333333333333333").insert(&db.conn).await.unwrap();
    assert_down_refuses_without_dropping_any_table(&db.conn).await;
    assert_eq!(team_oauth_attempt::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[tokio::test]
async fn team_credentials_migration_down_refuses_populated_pending_onboarding_without_partial_drop() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    pending("4444444444444444444444444444444444444444444444444444444444444444", 98701).insert(&db.conn).await.unwrap();
    assert_down_refuses_without_dropping_any_table(&db.conn).await;
    assert_eq!(team_pending_onboarding::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[tokio::test]
async fn team_credentials_migration_down_refuses_populated_credential_without_partial_drop() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    credential(8341, 98702, 1, "active").insert(&db.conn).await.unwrap();
    assert_down_refuses_without_dropping_any_table(&db.conn).await;
    assert_eq!(team_github_credential::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[tokio::test]
async fn team_credentials_migration_down_removes_only_empty_credential_tables() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let manager = SchemaManager::new(&db.conn);
    CredentialsMigration.down(&manager).await.unwrap();
    assert!(!manager.has_table("team_oauth_attempt").await.unwrap());
    assert!(!manager.has_table("team_pending_onboarding").await.unwrap());
    assert!(!manager.has_table("team_github_credential").await.unwrap());
    assert!(manager.has_table("organization_membership").await.unwrap());
    assert!(manager.has_table("team_session").await.unwrap());
}

fn validate_database_target(expected: &str, candidate: &str) -> TestResult<()> {
    let suffix = candidate.strip_prefix(DATABASE_PREFIX).unwrap_or_default();
    (candidate == expected
        && suffix.len() == 32
        && suffix.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
        .then_some(())
        .ok_or_else(|| format!("refusing database target {candidate}; expected exact generated name {expected}"))
}

async fn connect(url: &str, max_connections: u32) -> TestResult<DatabaseConnection> {
    let mut options = ConnectOptions::new(url);
    options.max_connections(max_connections).min_connections(1).sqlx_logging(false);
    Database::connect(options).await.map_err(|error| format!("connect to {url}: {error}"))
}

async fn expect_postgres_constraint(
    db: &DatabaseConnection,
    sql: &str,
    expected_name: &str,
) -> TestResult<()> {
    let error = db
        .execute(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .expect_err("Postgres must reject invalid credential fixture");
    if !error.to_string().contains(expected_name) {
        return Err(format!("Postgres returned wrong constraint for {expected_name}: {error}"));
    }
    Ok(())
}

#[test]
fn team_credentials_migration_database_cleanup_guard_requires_exact_generated_hex_name() {
    let expected = "workos_team_credentials_test_0123456789abcdef0123456789abcdef";
    assert!(validate_database_target(expected, expected).is_ok());
    for rejected in [
        "workos_team_credentials_test_0123456789abcdef0123456789abcdee",
        "workos_team_credentials_test_0123456789abcdef0123456789abcdeG",
        "workos_team_credentials_test_0123456789abcdef",
        "workos_team_access_test_0123456789abcdef0123456789abcdef",
        "postgres",
    ] {
        assert!(validate_database_target(expected, rejected).is_err());
    }
}

#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn team_credentials_migration_postgres_round_trip() {
    let generated_database_name = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&generated_database_name, &generated_database_name).unwrap();
    let admin = connect(ADMIN_URL, 1).await.expect("connect explicit local Postgres admin database");
    admin.execute(Statement::from_string(DbBackend::Postgres, format!("CREATE DATABASE \"{generated_database_name}\""))).await.expect("create generated team-credentials test database");
    let database_url = format!("postgres://air1@localhost:5432/{generated_database_name}");
    let db = match connect(&database_url, 2).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.expect("close admin pool");
            panic!("{error}; preserved database {generated_database_name}");
        }
    };

    let outcome: TestResult<()> = async {
        Migrator::up(&db, None).await.map_err(|error| format!("run migrations: {error}"))?;
        for (sql, name) in [
            ("INSERT INTO team_pending_onboarding (token_hash, csrf_hash, github_user_id, encrypted_payload, created_at, expires_at) VALUES ('p1', 'c1', 0, 'e', NOW(), NOW())", "ck_team_pending_onboarding_github_user_id_positive"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (0, 9901, 1, 'active', 'e', NULL, NULL, NOW(), NOW())", "ck_team_github_credential_user_id_positive"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8402, 0, 1, 'active', 'e', NULL, NULL, NOW(), NOW())", "ck_team_github_credential_github_user_id_positive"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8403, 9903, 0, 'active', 'e', NULL, NULL, NOW(), NOW())", "ck_team_github_credential_generation_positive"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8404, 9904, 1, 'pending', 'e', NULL, NULL, NOW(), NOW())", "ck_team_github_credential_state"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8405, 9905, 1, 'active', NULL, NULL, NULL, NOW(), NOW())", "ck_team_github_credential_state_shape"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8406, 9906, 1, 'refreshing', 'e', 'worker', NULL, NOW(), NOW())", "ck_team_github_credential_state_shape"),
            ("INSERT INTO team_github_credential (user_id, github_user_id, generation, state, encrypted_payload, refresh_owner, refresh_expires_at, created_at, updated_at) VALUES (8407, 9907, 1, 'reauth_required', 'e', NULL, NULL, NOW(), NOW())", "ck_team_github_credential_state_shape"),
        ] {
            expect_postgres_constraint(&db, sql, name).await?;
        }
        oauth_attempt("5555555555555555555555555555555555555555555555555555555555555555").insert(&db).await.map_err(|error| error.to_string())?;
        pending("6666666666666666666666666666666666666666666666666666666666666666", 9910).insert(&db).await.map_err(|error| error.to_string())?;
        credential(8410, 9911, 1, "active").insert(&db).await.map_err(|error| error.to_string())?;
        CredentialsMigration.up(&SchemaManager::new(&db)).await.map_err(|error| format!("rerun migration: {error}"))?;
        if team_oauth_attempt::Entity::find().count(&db).await.map_err(|error| error.to_string())? != 1
            || team_pending_onboarding::Entity::find().count(&db).await.map_err(|error| error.to_string())? != 1
            || team_github_credential::Entity::find().count(&db).await.map_err(|error| error.to_string())? != 1
        {
            return Err("data-bearing Postgres rerun changed credential rows".into());
        }
        team_oauth_attempt::Entity::delete_many().exec(&db).await.map_err(|error| error.to_string())?;
        team_pending_onboarding::Entity::delete_many().exec(&db).await.map_err(|error| error.to_string())?;
        team_github_credential::Entity::delete_many().exec(&db).await.map_err(|error| error.to_string())?;
        Migrator::down(&db, Some(1)).await.map_err(|error| format!("revert credentials migration: {error}"))?;
        Ok(())
    }.await;
    if let Err(error) = outcome {
        db.close().await.expect("close disposable database pool");
        admin.close().await.expect("close admin pool after failure");
        panic!("{error}; preserved database {generated_database_name}");
    }

    let connected_database_name = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT current_database() AS database_name".to_owned())).await.expect("read current disposable database identity").expect("current_database must return one row").try_get::<String>("", "database_name").expect("decode current disposable database identity");
    validate_database_target(&generated_database_name, &connected_database_name).expect("cleanup target must equal connected generated database");
    db.close().await.expect("close disposable database pool");
    admin.execute(Statement::from_string(DbBackend::Postgres, format!("DROP DATABASE \"{connected_database_name}\""))).await.expect("drop successful team-credentials test database");
    admin.close().await.expect("close admin pool");
    eprintln!("verified and removed disposable database {connected_database_name}");
}
