use chrono::{TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, PaginatorTrait, Set, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

use super::{m20260908_000001_team_access::Migration as TeamAccessMigration, Migrator};
use crate::db::entities::{organization_membership, org, session, team_session, user};

const ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const DATABASE_PREFIX: &str = "workos_team_access_test_";

type TestResult<T> = Result<T, String>;

fn timestamp(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 10, 0, second)
        .single()
        .expect("valid fixture timestamp")
}

async fn connect(url: &str, max_connections: u32) -> TestResult<DatabaseConnection> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(options)
        .await
        .map_err(|error| format!("connect to {url}: {error}"))
}

async fn database_before_team_access() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("open isolated SQLite database");
    let prior_migration_count = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260908_000001_team_access")
        .expect("team-access migration must be registered");
    Migrator::up(&db, Some(prior_migration_count as u32))
        .await
        .expect("apply migrations preceding team access");
    db
}

async fn seed_legacy_identity(
    db: &DatabaseConnection,
) -> (org::Model, user::Model, session::Model) {
    let organization = org::ActiveModel {
        id: Set(7101),
        github_org_id: Set(97101),
        login: Set("legacy-org".into()),
        name: Set(Some("Legacy Organization".into())),
        avatar_url: Set(Some("https://fixtures.invalid/org.png".into())),
        created_at: Set(timestamp(1)),
        updated_at: Set(timestamp(2)),
    }
    .insert(db)
    .await
    .expect("seed legacy organization");
    let owner = user::ActiveModel {
        id: Set(7201),
        github_user_id: Set(97201),
        login: Set("legacy-owner".into()),
        name: Set(Some("Legacy Owner".into())),
        email: Set(Some("legacy-owner@example.invalid".into())),
        avatar_url: Set(Some("https://fixtures.invalid/user.png".into())),
        org_id: Set(organization.id),
        role: Set("owner".into()),
        gh_token_enc: Set(Some("encrypted-access-fixture".into())),
        gh_refresh_token_enc: Set(Some("encrypted-refresh-fixture".into())),
        gh_token_expires_at: Set(Some(timestamp(3))),
        created_at: Set(timestamp(4)),
        updated_at: Set(timestamp(5)),
    }
    .insert(db)
    .await
    .expect("seed legacy owner");
    let login_session = session::ActiveModel {
        id: Set("legacy-session-id".into()),
        user_id: Set(owner.id),
        created_at: Set(timestamp(6)),
        expires_at: Set(timestamp(7)),
    }
    .insert(db)
    .await
    .expect("seed legacy session");
    (organization, owner, login_session)
}

fn membership(
    id: i32,
    org_id: i32,
    user_id: i32,
    role: &str,
    state: &str,
) -> organization_membership::ActiveModel {
    organization_membership::ActiveModel {
        id: Set(id),
        org_id: Set(org_id),
        user_id: Set(user_id),
        role: Set(role.into()),
        state: Set(state.into()),
        authorization_revision: Set(11),
        credential_generation: Set(13),
        verified_at: Set(Some(timestamp(10))),
        created_at: Set(timestamp(11)),
        updated_at: Set(timestamp(12)),
    }
}

fn bound_session(
    token_hash: &str,
    user_id: i32,
    org_id: i32,
    membership_id: i32,
) -> team_session::ActiveModel {
    team_session::ActiveModel {
        token_hash: Set(token_hash.into()),
        user_id: Set(user_id),
        org_id: Set(org_id),
        membership_id: Set(membership_id),
        authorization_revision: Set(11),
        credential_generation: Set(13),
        csrf_hash: Set(format!("csrf-{token_hash}")),
        created_at: Set(timestamp(13)),
        expires_at: Set(timestamp(14)),
    }
}

#[tokio::test]
async fn team_access_migration_does_not_promote_legacy_identity_or_sessions() {
    let db = database_before_team_access().await;
    let (original_org, original_user, original_session) = seed_legacy_identity(&db).await;

    TeamAccessMigration
        .up(&SchemaManager::new(&db))
        .await
        .expect("apply team-access migration");

    assert_eq!(
        organization_membership::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(team_session::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(
        org::Entity::find_by_id(original_org.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap(),
        original_org
    );
    assert_eq!(
        user::Entity::find_by_id(original_user.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap(),
        original_user
    );
    assert_eq!(
        session::Entity::find_by_id(original_session.id.clone())
            .one(&db)
            .await
            .unwrap()
            .unwrap(),
        original_session
    );
}

#[tokio::test]
async fn team_access_migration_rejects_duplicate_membership_for_one_organization() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    membership(7301, 7101, 7201, "admin", "active")
        .insert(&db.conn)
        .await
        .expect("insert first membership");

    let duplicate = membership(7302, 7101, 7201, "member", "revoked");
    assert!(duplicate.insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn team_access_migration_requires_explicit_valid_membership_role_and_state() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;

    assert!(membership(7311, 7101, 7201, "owner", "active")
        .insert(&db.conn)
        .await
        .is_err());
    assert!(membership(7312, 7101, 7202, "member", "pending")
        .insert(&db.conn)
        .await
        .is_err());

    let missing_role_and_state = organization_membership::ActiveModel {
        id: Set(7313),
        org_id: Set(7101),
        user_id: Set(7203),
        created_at: Set(timestamp(11)),
        updated_at: Set(timestamp(12)),
        ..Default::default()
    };
    assert!(missing_role_and_state.insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn team_access_migration_defaults_revisions_to_zero_and_rejects_negative_values() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let defaulted = organization_membership::ActiveModel {
        id: Set(7321),
        org_id: Set(7101),
        user_id: Set(7201),
        role: Set("member".into()),
        state: Set("active".into()),
        verified_at: Set(None),
        created_at: Set(timestamp(11)),
        updated_at: Set(timestamp(12)),
        ..Default::default()
    }
    .insert(&db.conn)
    .await
    .expect("database defaults membership generations");
    assert_eq!(defaulted.authorization_revision, 0);
    assert_eq!(defaulted.credential_generation, 0);

    let mut negative_authorization = membership(7322, 7101, 7202, "member", "active");
    negative_authorization.authorization_revision = Set(-1);
    assert!(negative_authorization.insert(&db.conn).await.is_err());

    let mut negative_credential = membership(7323, 7101, 7203, "member", "active");
    negative_credential.credential_generation = Set(-1);
    assert!(negative_credential.insert(&db.conn).await.is_err());

    let mut negative_session_authorization = bound_session("hash-negative-auth", 7201, 7101, 7321);
    negative_session_authorization.authorization_revision = Set(-1);
    assert!(negative_session_authorization.insert(&db.conn).await.is_err());

    let mut negative_session_credential =
        bound_session("hash-negative-credential", 7201, 7101, 7321);
    negative_session_credential.credential_generation = Set(-1);
    assert!(negative_session_credential.insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn team_access_migration_supports_one_user_in_two_organizations_with_bound_sessions() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let first = membership(7331, 7101, 7201, "admin", "active")
        .insert(&db.conn)
        .await
        .expect("insert first organization membership");
    let second = membership(7332, 7102, 7201, "member", "active")
        .insert(&db.conn)
        .await
        .expect("insert second organization membership");
    bound_session("hash-org-one", 7201, 7101, first.id)
        .insert(&db.conn)
        .await
        .expect("insert first bound session");
    bound_session("hash-org-two", 7201, 7102, second.id)
        .insert(&db.conn)
        .await
        .expect("insert second bound session");

    assert_eq!(
        organization_membership::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        organization_membership::Entity::find_by_id(7331)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .org_id,
        7101
    );
    assert_eq!(
        organization_membership::Entity::find_by_id(7332)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .org_id,
        7102
    );
    assert_eq!(team_session::Entity::find().count(&db.conn).await.unwrap(), 2);
    assert_eq!(
        team_session::Entity::find_by_id("hash-org-one")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .membership_id,
        7331
    );
    assert_eq!(
        team_session::Entity::find_by_id("hash-org-two")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .membership_id,
        7332
    );
}

#[tokio::test]
async fn team_access_migration_rerun_preserves_memberships_and_bound_sessions() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_membership = membership(7341, 7101, 7201, "admin", "blocked")
        .insert(&db.conn)
        .await
        .expect("insert blocked membership");
    let expected_session = bound_session("hash-preserved", 7201, 7101, expected_membership.id)
        .insert(&db.conn)
        .await
        .expect("insert bound session");

    TeamAccessMigration
        .up(&SchemaManager::new(&db.conn))
        .await
        .expect("rerun additive migration");

    assert_eq!(
        organization_membership::Entity::find_by_id(expected_membership.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap(),
        expected_membership
    );
    assert_eq!(
        team_session::Entity::find_by_id(expected_session.token_hash.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap(),
        expected_session
    );
}

#[tokio::test]
async fn team_access_migration_down_refuses_a_populated_membership_table_without_deleting_rows() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_membership = membership(7351, 7101, 7201, "member", "revoked")
        .insert(&db.conn)
        .await
        .expect("insert revoked membership");

    assert!(TeamAccessMigration
        .down(&SchemaManager::new(&db.conn))
        .await
        .is_err());
    assert_eq!(
        organization_membership::Entity::find_by_id(expected_membership.id)
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_membership)
    );
    assert_eq!(team_session::Entity::find().count(&db.conn).await.unwrap(), 0);
}

#[tokio::test]
async fn team_access_migration_down_refuses_a_populated_session_table_without_deleting_rows() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_session = bound_session("hash-down-refusal", 7201, 7101, 7351)
        .insert(&db.conn)
        .await
        .expect("insert bound session without app-enforced references");

    assert!(TeamAccessMigration
        .down(&SchemaManager::new(&db.conn))
        .await
        .is_err());
    assert_eq!(
        organization_membership::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        team_session::Entity::find_by_id(expected_session.token_hash.clone())
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_session)
    );
}

#[tokio::test]
async fn team_access_migration_down_removes_empty_tables_only() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let manager = SchemaManager::new(&db.conn);

    TeamAccessMigration
        .down(&manager)
        .await
        .expect("remove empty team-access tables");

    assert!(!manager
        .has_table("organization_membership")
        .await
        .unwrap());
    assert!(!manager.has_table("team_session").await.unwrap());
    assert!(manager.has_table("orgs").await.unwrap());
    assert!(manager.has_table("users").await.unwrap());
    assert!(manager.has_table("sessions").await.unwrap());
}

#[test]
fn team_access_migration_schema_builds_for_sqlite_and_postgres() {
    use sea_orm_migration::sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};

    let sqlite_tables: Vec<_> = super::m20260908_000001_team_access::table_statements()
        .into_iter()
        .map(|statement| statement.to_string(SqliteQueryBuilder))
        .collect();
    let postgres_tables: Vec<_> = super::m20260908_000001_team_access::table_statements()
        .into_iter()
        .map(|statement| statement.to_string(PostgresQueryBuilder))
        .collect();
    let sqlite_indexes: Vec<_> = super::m20260908_000001_team_access::index_statements()
        .into_iter()
        .map(|statement| statement.to_string(SqliteQueryBuilder))
        .collect();
    let postgres_indexes: Vec<_> = super::m20260908_000001_team_access::index_statements()
        .into_iter()
        .map(|statement| statement.to_string(PostgresQueryBuilder))
        .collect();

    for sql in sqlite_tables.iter().chain(postgres_tables.iter()) {
        assert!(sql.contains("authorization_revision"));
        assert!(sql.contains("credential_generation"));
        assert!(sql.contains("CHECK"));
    }
    for tables in [&sqlite_tables, &postgres_tables] {
        let sql = tables.join("\n");
        for constraint_name in [
            "ck_organization_membership_role",
            "ck_organization_membership_state",
            "ck_organization_membership_authorization_revision_nonnegative",
            "ck_organization_membership_credential_generation_nonnegative",
            "ck_team_session_authorization_revision_nonnegative",
            "ck_team_session_credential_generation_nonnegative",
        ] {
            assert!(
                sql.contains(constraint_name),
                "generated schema omitted {constraint_name}: {sql}"
            );
        }
    }
    assert!(postgres_tables.iter().all(|sql| sql.contains("timestamp with time zone")));
    for indexes in [&sqlite_indexes, &postgres_indexes] {
        assert!(indexes
            .iter()
            .any(|sql| sql.contains("idx_team_session_membership_id")));
        assert!(indexes
            .iter()
            .any(|sql| sql.contains("idx_team_session_expires_at")));
        assert!(indexes
            .iter()
            .any(|sql| sql.contains("uidx_organization_membership_org_user")));
    }
}

fn validate_database_target(expected: &str, candidate: &str) -> TestResult<()> {
    let suffix = candidate.strip_prefix(DATABASE_PREFIX).unwrap_or_default();
    (candidate == expected
        && suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(())
    .ok_or_else(|| {
        format!("refusing database target {candidate}; expected exact generated name {expected}")
    })
}

#[test]
fn team_access_migration_database_cleanup_guard_requires_exact_generated_hex_name() {
    let expected = "workos_team_access_test_0123456789abcdef0123456789abcdef";
    assert!(validate_database_target(expected, expected).is_ok());
    for rejected in [
        "workos_team_access_test_0123456789abcdef0123456789abcdee",
        "workos_team_access_test_0123456789abcdef0123456789abcdeG",
        "workos_team_access_test_0123456789abcdef0123456789abcde_",
        "workos_team_access_test_0123456789abcdef",
        "workos_local",
    ] {
        assert!(
            validate_database_target(expected, rejected).is_err(),
            "cleanup guard accepted {rejected}"
        );
    }
}

#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn team_access_migration_postgres_round_trip() {
    let generated_database_name = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&generated_database_name, &generated_database_name)
        .expect("generated database name must be safe");
    let admin = connect(ADMIN_URL, 1)
        .await
        .expect("connect explicit local Postgres admin database");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE \"{generated_database_name}\""),
        ))
        .await
        .expect("create generated team-access test database");
    let database_url = format!("postgres://air1@localhost:5432/{generated_database_name}");
    let db = match connect(&database_url, 2).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.expect("close admin pool");
            panic!("{error}; preserved database {generated_database_name}");
        }
    };

    let outcome: TestResult<()> = async {
        Migrator::up(&db, None)
            .await
            .map_err(|error| format!("run migrations: {error}"))?;
        let invalid_role = membership(7362, 7101, 7202, "owner", "active")
            .insert(&db)
            .await
            .expect_err("Postgres must reject an invalid membership role");
        if !invalid_role
            .to_string()
            .contains("ck_organization_membership_role")
        {
            return Err(format!("Postgres returned the wrong role constraint: {invalid_role}"));
        }
        let invalid_state = membership(7363, 7101, 7203, "member", "pending")
            .insert(&db)
            .await
            .expect_err("Postgres must reject an invalid membership state");
        if !invalid_state
            .to_string()
            .contains("ck_organization_membership_state")
        {
            return Err(format!("Postgres returned the wrong state constraint: {invalid_state}"));
        }
        let mut invalid_membership_authorization =
            membership(7364, 7101, 7204, "member", "active");
        invalid_membership_authorization.authorization_revision = Set(-1);
        let invalid_membership_authorization = invalid_membership_authorization
            .insert(&db)
            .await
            .expect_err("Postgres must reject a negative membership authorization revision");
        if !invalid_membership_authorization
            .to_string()
            .contains("ck_organization_membership_authorization_revision_nonnegative")
        {
            return Err(format!(
                "wrong Postgres membership authorization constraint: {error}",
                error = invalid_membership_authorization
            ));
        }
        let mut invalid_membership_credential =
            membership(7365, 7101, 7205, "member", "active");
        invalid_membership_credential.credential_generation = Set(-1);
        let invalid_membership_credential = invalid_membership_credential
            .insert(&db)
            .await
            .expect_err("Postgres must reject a negative membership credential generation");
        if !invalid_membership_credential
            .to_string()
            .contains("ck_organization_membership_credential_generation_nonnegative")
        {
            return Err(format!(
                "wrong Postgres membership credential constraint: {error}",
                error = invalid_membership_credential
            ));
        }
        let mut invalid_session_authorization =
            bound_session("hash-postgres-negative-auth", 7201, 7101, 7361);
        invalid_session_authorization.authorization_revision = Set(-1);
        let invalid_session_authorization = invalid_session_authorization
            .insert(&db)
            .await
            .expect_err("Postgres must reject a negative session authorization revision");
        if !invalid_session_authorization
            .to_string()
            .contains("ck_team_session_authorization_revision_nonnegative")
        {
            return Err(format!(
                "wrong Postgres session authorization constraint: {error}",
                error = invalid_session_authorization
            ));
        }
        let mut invalid_session_credential =
            bound_session("hash-postgres-negative-credential", 7201, 7101, 7361);
        invalid_session_credential.credential_generation = Set(-1);
        let invalid_session_credential = invalid_session_credential
            .insert(&db)
            .await
            .expect_err("Postgres must reject a negative session credential generation");
        if !invalid_session_credential
            .to_string()
            .contains("ck_team_session_credential_generation_nonnegative")
        {
            return Err(format!(
                "wrong Postgres session credential constraint: {error}",
                error = invalid_session_credential
            ));
        }
        membership(7361, 7101, 7201, "admin", "active")
            .insert(&db)
            .await
            .map_err(|error| format!("insert Postgres membership: {error}"))?;
        bound_session("hash-postgres", 7201, 7101, 7361)
            .insert(&db)
            .await
            .map_err(|error| format!("insert Postgres bound session: {error}"))?;
        team_session::Entity::delete_many()
            .exec(&db)
            .await
            .map_err(|error| format!("clear Postgres sessions: {error}"))?;
        organization_membership::Entity::delete_many()
            .exec(&db)
            .await
            .map_err(|error| format!("clear Postgres memberships: {error}"))?;
        let migrations = Migrator::migrations();
        let team_access_position = migrations
            .iter()
            .position(|migration| migration.name() == "m20260908_000001_team_access")
            .ok_or_else(|| "team-access migration is not registered".to_string())?;
        let team_access_and_later_count = migrations
            .len()
            .checked_sub(team_access_position)
            .ok_or_else(|| "team-access migration position is invalid".to_string())?;
        Migrator::down(&db, Some(team_access_and_later_count as u32))
            .await
            .map_err(|error| format!("revert team-access migration: {error}"))?;
        let manager = SchemaManager::new(&db);
        if manager
            .has_table("organization_membership")
            .await
            .map_err(|error| format!("inspect membership table: {error}"))?
            || manager
                .has_table("team_session")
                .await
                .map_err(|error| format!("inspect team session table: {error}"))?
        {
            return Err("team-access tables survived Postgres down migration".into());
        }
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        db.close().await.expect("close disposable database pool");
        admin
            .close()
            .await
            .expect("close admin pool after failure");
        panic!("{error}; preserved database {generated_database_name}");
    }

    let connected_database_name = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS database_name",
        ))
        .await
        .expect("read current disposable database identity")
        .expect("current_database must return one row")
        .try_get::<String>("", "database_name")
        .expect("decode current disposable database identity");
    validate_database_target(&generated_database_name, &connected_database_name)
        .expect("cleanup target must equal the connected generated database");
    db.close().await.expect("close disposable database pool");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("DROP DATABASE \"{connected_database_name}\""),
        ))
        .await
        .expect("drop successful team-access test database");
    admin.close().await.expect("close admin pool");
    eprintln!("verified and removed disposable database {connected_database_name}");
}
