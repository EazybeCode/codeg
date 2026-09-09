use chrono::{TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, IntoActiveModel, PaginatorTrait, Set, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

use super::{
    m20260211_000001_init::Migration as InitialMigration,
    m20260907_000001_workspace_catalog::Migration as WorkspaceCatalogMigration,
    m20260909_000001_personal_agent_connections::Migration as PersonalAgentMigration,
    Migrator,
};
use crate::db::entities::{
    org, team_agent_attempt, team_agent_connection, team_agent_operation, user,
};

const ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const DATABASE_PREFIX: &str = "workos_personal_agents_test_";
const C1: &str = "10000000-0000-4000-8000-000000000001";
const C2: &str = "10000000-0000-4000-8000-000000000002";
const C3: &str = "10000000-0000-4000-8000-000000000003";
const A1: &str = "20000000-0000-4000-8000-000000000001";
const O1: &str = "30000000-0000-4000-8000-000000000001";
const S1: &str = "40000000-0000-4000-8000-000000000001";
const CATALOG_TABLES: [&str; 3] = [
    "workspace_chat_draft",
    "workspace_node",
    "workspace_repository",
];
const PERSONAL_TABLES: [&str; 3] = [
    "team_agent_connection",
    "team_agent_attempt",
    "team_agent_operation",
];
type TestResult<T> = Result<T, String>;

fn ts(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 9, 12, 0, second)
        .single()
        .unwrap()
}

async fn sqlite_fks(db: &DatabaseConnection, enabled: bool) {
    let value = if enabled { "ON" } else { "OFF" };
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        format!("PRAGMA foreign_keys={value}"),
    ))
    .await
    .unwrap();
}

async fn before_personal() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    sqlite_fks(&db, true).await;
    let position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260909_000001_personal_agent_connections")
        .unwrap();
    Migrator::up(&db, Some(position as u32)).await.unwrap();
    db
}

async fn seed_org(db: &DatabaseConnection, id: i32) -> org::Model {
    org::ActiveModel {
        id: Set(id),
        github_org_id: Set(100_000 + i64::from(id)),
        login: Set(format!("invented-org-{id}")),
        name: Set(Some(format!("Invented Organization {id}"))),
        avatar_url: Set(None),
        created_at: Set(ts(1)),
        updated_at: Set(ts(2)),
    }
    .insert(db)
    .await
    .unwrap()
}

async fn seed_user(db: &DatabaseConnection, id: i32, org_id: i32) -> user::Model {
    user::ActiveModel {
        id: Set(id),
        github_user_id: Set(200_000 + i64::from(id)),
        login: Set(format!("invented-member-{id}")),
        name: Set(Some(format!("Invented Member {id}"))),
        email: Set(None),
        avatar_url: Set(None),
        org_id: Set(org_id),
        role: Set("member".into()),
        gh_token_enc: Set(None),
        gh_refresh_token_enc: Set(None),
        gh_token_expires_at: Set(None),
        created_at: Set(ts(1)),
        updated_at: Set(ts(2)),
    }
    .insert(db)
    .await
    .unwrap()
}

async fn seed_identity(db: &DatabaseConnection, org_id: i32, user_id: i32) {
    seed_org(db, org_id).await;
    seed_user(db, user_id, org_id).await;
}

fn connection(
    id: &str,
    org_id: i32,
    user_id: i32,
    provider: &str,
    generation: i64,
    state: &str,
) -> team_agent_connection::ActiveModel {
    let connected = state == "connected";
    team_agent_connection::ActiveModel {
        id: Set(id.into()),
        org_id: Set(org_id),
        user_id: Set(user_id),
        provider: Set(provider.into()),
        generation: Set(generation),
        state: Set(state.into()),
        account_label: Set(connected.then(|| "Invented Account".into())),
        checked_at: Set(connected.then(|| ts(4))),
        error_code: Set(None),
        storage_handle: Set(connected.then(|| "invented-storage-handle".into())),
        created_at: Set(ts(3)),
        updated_at: Set(ts(5)),
    }
}

fn attempt(id: &str, connection_id: &str, generation: i64, state: &str) -> team_agent_attempt::ActiveModel {
    team_agent_attempt::ActiveModel {
        id: Set(id.into()),
        connection_id: Set(connection_id.into()),
        generation: Set(generation),
        session_id: Set(S1.into()),
        state: Set(state.into()),
        created_at: Set(ts(6)),
        expires_at: Set(ts(9)),
        updated_at: Set(ts(7)),
    }
}

fn operation(id: &str, connection_id: &str, kind: &str, request_key: &str, state: &str) -> team_agent_operation::ActiveModel {
    let leased = state == "leased";
    team_agent_operation::ActiveModel {
        id: Set(id.into()),
        connection_id: Set(connection_id.into()),
        kind: Set(kind.into()),
        request_key: Set(request_key.into()),
        target_attempt_id: Set((kind != "disconnect").then(|| A1.into())),
        target_generation: Set(1),
        result_generation: Set(1),
        state: Set(state.into()),
        lease_owner: Set(leased.then(|| "invented-worker".into())),
        lease_expires_at: Set(leased.then(|| ts(8))),
        error_code: Set(None),
        created_at: Set(ts(6)),
        expires_at: Set(ts(9)),
        updated_at: Set(ts(7)),
    }
}

fn named(error: impl ToString, expected: &str) {
    let rendered = error.to_string();
    assert!(
        rendered.contains(expected),
        "expected {expected} in {rendered}"
    );
}

async fn reject_connection(db: &DatabaseConnection, row: team_agent_connection::ActiveModel, expected: &str) {
    named(
        row.insert(db).await.expect_err("invalid connection must fail"),
        expected,
    );
}
async fn reject_attempt(db: &DatabaseConnection, row: team_agent_attempt::ActiveModel, expected: &str) {
    named(
        row.insert(db).await.expect_err("invalid attempt must fail"),
        expected,
    );
}
async fn reject_operation(db: &DatabaseConnection, row: team_agent_operation::ActiveModel, expected: &str) {
    named(
        row.insert(db).await.expect_err("invalid operation must fail"),
        expected,
    );
}

#[tokio::test]
async fn personal_agent_metadata_tables_are_usable_after_full_migration_chain() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    Migrator::down(&db, None)
        .await
        .expect("complete empty migration chain is reversible");
    assert!(Migrator::get_applied_migrations(&db)
        .await
        .unwrap()
        .is_empty());
    let manager = SchemaManager::new(&db);
    for table in CATALOG_TABLES.into_iter().chain(PERSONAL_TABLES) {
        assert!(
            !manager.has_table(table).await.unwrap(),
            "full rollback retained {table}"
        );
    }

    Migrator::up(&db, None).await.unwrap();
    for table in PERSONAL_TABLES {
        let rows = db
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT id FROM {table}"),
            ))
            .await
            .unwrap_or_else(|e| panic!("{table} must be usable after rerun: {e}"));
        assert!(rows.is_empty());
    }
}

async fn historical_catalog_fixture() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    for table in CATALOG_TABLES {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY)"),
        ))
        .await
        .unwrap();
    }
    create_sentinel(&db).await;
    db
}

async fn create_sentinel(db: &DatabaseConnection) {
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE unrelated_sentinel (id INTEGER PRIMARY KEY, value TEXT NOT NULL)"
            .to_owned(),
    ))
    .await
    .unwrap();
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO unrelated_sentinel (id, value) VALUES (1, 'preserve-me')".to_owned(),
    ))
    .await
    .unwrap();
}

async fn assert_sentinel_preserved(db: &DatabaseConnection) {
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT id, value FROM unrelated_sentinel".to_owned(),
        ))
        .await
        .unwrap()
        .expect("sentinel row must remain");
    assert_eq!(row.try_get::<i64>("", "id").unwrap(), 1);
    assert_eq!(
        row.try_get::<String>("", "value").unwrap(),
        "preserve-me"
    );
}

#[tokio::test]
async fn historical_catalog_down_refuses_each_id_only_table_before_any_drop() {
    for populated_table in CATALOG_TABLES {
        let db = historical_catalog_fixture().await;
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            format!("INSERT INTO {populated_table} (id) VALUES (101)"),
        ))
        .await
        .unwrap();

        let error = WorkspaceCatalogMigration
            .down(&SchemaManager::new(&db))
            .await
            .expect_err("catalog rollback with rows must fail");
        assert_eq!(
            error.to_string(),
            "Custom Error: workspace catalog migration cannot be reverted while catalog rows exist"
        );

        let manager = SchemaManager::new(&db);
        for table in CATALOG_TABLES {
            assert!(
                manager.has_table(table).await.unwrap(),
                "guard dropped {table} while {populated_table} contained a row"
            );
            let row_count = db
                .query_one(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT COUNT(*) AS count FROM {table}"),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "count")
                .unwrap();
            assert_eq!(
                row_count,
                i64::from(table == populated_table),
                "catalog rows changed in {table}"
            );
        }
        assert_sentinel_preserved(&db).await;
    }
}

#[tokio::test]
async fn historical_catalog_down_drops_only_empty_id_only_catalog_tables() {
    let db = historical_catalog_fixture().await;
    WorkspaceCatalogMigration
        .down(&SchemaManager::new(&db))
        .await
        .unwrap();

    let manager = SchemaManager::new(&db);
    for table in CATALOG_TABLES {
        assert!(
            !manager.has_table(table).await.unwrap(),
            "empty rollback retained {table}"
        );
    }
    assert_sentinel_preserved(&db).await;
}

#[tokio::test]
async fn initial_down_tolerates_historically_removed_opened_conversation_table() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    InitialMigration.up(&manager).await.unwrap();
    create_sentinel(&db).await;
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "DROP TABLE folder_opened_conversation".to_owned(),
    ))
    .await
    .unwrap();

    InitialMigration.down(&manager).await.unwrap();

    for table in [
        "app_metadata",
        "folder",
        "conversation",
        "folder_opened_conversation",
    ] {
        assert!(
            !manager.has_table(table).await.unwrap(),
            "initial rollback retained {table}"
        );
    }
    assert_sentinel_preserved(&db).await;
}

#[tokio::test]
async fn personal_agent_migration_preserves_legacy_identity_and_starts_empty() {
    let db = before_personal().await;
    let expected_org = seed_org(&db, 9101).await;
    let expected_user = seed_user(&db, 9201, expected_org.id).await;
    PersonalAgentMigration.up(&SchemaManager::new(&db)).await.unwrap();
    assert_eq!(org::Entity::find_by_id(expected_org.id).one(&db).await.unwrap(), Some(expected_org));
    assert_eq!(user::Entity::find_by_id(expected_user.id).one(&db).await.unwrap(), Some(expected_user));
    assert_eq!(team_agent_connection::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(team_agent_attempt::Entity::find().count(&db).await.unwrap(), 0);
    assert_eq!(team_agent_operation::Entity::find().count(&db).await.unwrap(), 0);
}

#[tokio::test]
async fn personal_agent_migration_accepts_all_states_and_adds_named_indexes() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    for (offset, state) in ["disconnected", "connecting", "connected", "reconnect_required", "disconnecting", "unavailable"].into_iter().enumerate() {
        let org_id = 9300 + offset as i32;
        let user_id = 9400 + offset as i32;
        seed_identity(&db.conn, org_id, user_id).await;
        let id = format!("10000000-0000-4000-8000-{:012}", offset + 10);
        let mut row = connection(&id, org_id, user_id, "claude", 1, state);
        if state == "connecting" { row.account_label = Set(Some("Retained Account".into()));
    row.checked_at = Set(Some(ts(4)));
    row.error_code = Set(Some("interrupted".into()));
    row.storage_handle = Set(Some("retained-handle".into())); }
        row.insert(&db.conn).await.unwrap();
    }
    for (offset, state) in ["pending", "completed", "canceled", "expired", "failed"].into_iter().enumerate() {
        let id = format!("20000000-0000-4000-8000-{:012}", offset + 10);
        attempt(&id, "10000000-0000-4000-8000-000000000010", offset as i64 + 1, state).insert(&db.conn).await.unwrap();
    }
    for (offset, (kind, state)) in [("connect", "queued"), ("cancel", "leased"), ("disconnect", "completed"), ("connect", "abandoned")].into_iter().enumerate() {
        let id = format!("30000000-0000-4000-8000-{:012}", offset + 10);
        operation(&id, "10000000-0000-4000-8000-000000000010", kind, &format!("request-{offset}"), state).insert(&db.conn).await.unwrap();
    }
    assert_eq!(team_agent_connection::Entity::find().count(&db.conn).await.unwrap(), 6);
    assert_eq!(team_agent_attempt::Entity::find().count(&db.conn).await.unwrap(), 5);
    assert_eq!(team_agent_operation::Entity::find().count(&db.conn).await.unwrap(), 4);
    let indexes = db.conn.query_all(Statement::from_string(DbBackend::Sqlite, "SELECT name FROM sqlite_master WHERE type = 'index'".to_owned())).await.unwrap().into_iter().map(|r| r.try_get::<String>("", "name").unwrap()).collect::<Vec<_>>();
    for expected in ["idx_team_agent_connection_org_user_provider", "idx_team_agent_attempt_connection_generation", "idx_team_agent_attempt_state_expires_at", "idx_team_agent_operation_connection_kind_request_key", "idx_team_agent_operation_state_lease_expires_at"] {
        assert!(indexes.iter().any(|name| name == expected), "missing {expected}");
    }
}

#[tokio::test]
async fn connection_constraints_enforce_ownership_bounds_shapes_and_atomic_updates() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    seed_identity(&db.conn, 9501, 9601).await;
    seed_org(&db.conn, 9502).await;
    seed_user(&db.conn, 9602, 9501).await;
    assert!(db.conn.execute(Statement::from_string(DbBackend::Sqlite, "INSERT INTO team_agent_connection (id, org_id, user_id, created_at, updated_at) VALUES ('10000000-0000-4000-8000-000000000099', 9501, 9601, '2026-09-09T12:00:03Z', '2026-09-09T12:00:05Z')".to_owned())).await.is_err());
    for (row, check) in [
        (connection("", 9501, 9601, "claude", 1, "disconnected"), "ck_team_agent_connection_id_length"),
        (connection(&"x".repeat(65), 9501, 9601, "claude", 1, "disconnected"), "ck_team_agent_connection_id_length"),
        (connection(C1, 0, 9601, "claude", 1, "disconnected"), "ck_team_agent_connection_org_id_positive"),
        (connection(C1, 9501, 0, "claude", 1, "disconnected"), "ck_team_agent_connection_user_id_positive"),
        (connection(C1, 9501, 9601, "gemini", 1, "disconnected"), "ck_team_agent_connection_provider"),
        (connection(C1, 9501, 9601, "claude", 0, "disconnected"), "ck_team_agent_connection_generation_positive"),
        (connection(C1, 9501, 9601, "claude", 1, "pending"), "ck_team_agent_connection_state"),
    ] { reject_connection(&db.conn, row, check).await; }
    let mut row = connection(C1, 9501, 9601, "claude", 1, "connecting");
    row.account_label=Set(Some(String::new()));
    reject_connection(&db.conn, row, "ck_team_agent_connection_account_label_length").await;
    let mut row = connection(C1, 9501, 9601, "claude", 1, "connecting");
    row.account_label=Set(Some("a".repeat(161)));
    reject_connection(&db.conn, row, "ck_team_agent_connection_account_label_length").await;
    let mut row = connection(C1, 9501, 9601, "claude", 1, "connecting");
    row.storage_handle=Set(Some(String::new()));
    reject_connection(&db.conn, row, "ck_team_agent_connection_storage_handle_length").await;
    let mut row = connection(C1, 9501, 9601, "claude", 1, "connecting");
    row.storage_handle=Set(Some("h".repeat(65)));
    reject_connection(&db.conn, row, "ck_team_agent_connection_storage_handle_length").await;
    let mut row = connection(C1, 9501, 9601, "claude", 1, "connecting");
    row.error_code=Set(Some("raw-error".into()));
    reject_connection(&db.conn, row, "ck_team_agent_connection_error_code").await;
    for field in 0..4 { let mut row=connection(C1, 9501, 9601, "claude", 1, "connected"); match field {0=>row.account_label=Set(None), 1=>row.checked_at=Set(None), 2=>row.storage_handle=Set(None), _=>row.error_code=Set(Some("interrupted".into()))};
    reject_connection(&db.conn, row, "ck_team_agent_connection_state_shape").await; }
    for field in 0..4 { let mut row=connection(C1, 9501, 9601, "claude", 1, "disconnected"); match field {0=>row.account_label=Set(Some("x".into())), 1=>row.checked_at=Set(Some(ts(4))), 2=>row.storage_handle=Set(Some("x".into())), _=>row.error_code=Set(Some("interrupted".into()))};
    reject_connection(&db.conn, row, "ck_team_agent_connection_state_shape").await; }
    let mut row=connection(C1, 9501, 9601, "claude", 1, "disconnected");
    row.updated_at=Set(ts(2));
    reject_connection(&db.conn, row, "ck_team_agent_connection_updated_at_order").await;
    named(connection(C1, 9599, 9601, "claude", 1, "disconnected").insert(&db.conn).await.unwrap_err(), "FOREIGN KEY constraint failed");
    named(connection(C1, 9501, 9699, "claude", 1, "disconnected").insert(&db.conn).await.unwrap_err(), "FOREIGN KEY constraint failed");
    let expected = connection(C1, 9501, 9601, "claude", 1, "disconnected").insert(&db.conn).await.unwrap();
    assert!(connection(C2, 9501, 9601, "claude", 2, "connecting").insert(&db.conn).await.is_err());
    connection(C2, 9502, 9601, "claude", 1, "disconnected").insert(&db.conn).await.unwrap();
    connection(C3, 9501, 9602, "claude", 1, "disconnected").insert(&db.conn).await.unwrap();
    let mut invalid=expected.clone().into_active_model(); invalid.state=Set("connected".into());
    assert!(invalid.update(&db.conn).await.is_err());
    assert_eq!(team_agent_connection::Entity::find_by_id(C1).one(&db.conn).await.unwrap(), Some(expected));
}

#[tokio::test]
async fn length_constraints_accept_inclusive_minimum_and_maximum_values() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    seed_identity(&db.conn, 9651, 9661).await;

    let minimum_connection_id = "c";
    let minimum_attempt_id = "a";
    let minimum_operation_id = "o";
    let mut minimum_connection =
        connection(minimum_connection_id, 9651, 9661, "claude", 1, "connected");
    minimum_connection.account_label = Set(Some("a".into()));
    minimum_connection.storage_handle = Set(Some("h".into()));
    minimum_connection.insert(&db.conn).await.unwrap();

    let mut minimum_attempt = attempt(minimum_attempt_id, minimum_connection_id, 1, "pending");
    minimum_attempt.session_id = Set("s".into());
    minimum_attempt.insert(&db.conn).await.unwrap();

    let mut minimum_operation = operation(
        minimum_operation_id,
        minimum_connection_id,
        "connect",
        "r",
        "leased",
    );
    minimum_operation.target_attempt_id = Set(Some(minimum_attempt_id.into()));
    minimum_operation.lease_owner = Set(Some("w".into()));
    minimum_operation.insert(&db.conn).await.unwrap();

    let maximum_connection_id = "c".repeat(64);
    let maximum_attempt_id = "a".repeat(64);
    let maximum_operation_id = "o".repeat(64);
    let mut maximum_connection = connection(
        &maximum_connection_id,
        9651,
        9661,
        "codex",
        1,
        "connected",
    );
    maximum_connection.account_label = Set(Some("l".repeat(160)));
    maximum_connection.storage_handle = Set(Some("h".repeat(64)));
    maximum_connection.insert(&db.conn).await.unwrap();

    let mut maximum_attempt = attempt(&maximum_attempt_id, &maximum_connection_id, 1, "pending");
    maximum_attempt.session_id = Set("s".repeat(128));
    maximum_attempt.insert(&db.conn).await.unwrap();

    let mut maximum_operation = operation(
        &maximum_operation_id,
        &maximum_connection_id,
        "connect",
        &"r".repeat(64),
        "leased",
    );
    maximum_operation.target_attempt_id = Set(Some(maximum_attempt_id.clone()));
    maximum_operation.lease_owner = Set(Some("w".repeat(64)));
    maximum_operation.insert(&db.conn).await.unwrap();

    for connection_id in [minimum_connection_id, maximum_connection_id.as_str()] {
        assert!(
            team_agent_connection::Entity::find_by_id(connection_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_some()
        );
    }
    for attempt_id in [minimum_attempt_id, maximum_attempt_id.as_str()] {
        assert!(
            team_agent_attempt::Entity::find_by_id(attempt_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_some()
        );
    }
    for operation_id in [minimum_operation_id, maximum_operation_id.as_str()] {
        assert!(
            team_agent_operation::Entity::find_by_id(operation_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn attempt_constraints_enforce_binding_lifecycle_and_generation_uniqueness() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    seed_identity(&db.conn, 9701, 9801).await;
    connection(C1, 9701, 9801, "claude", 1, "connecting").insert(&db.conn).await.unwrap();
    assert!(db.conn.execute(Statement::from_string(DbBackend::Sqlite, format!("INSERT INTO team_agent_attempt (id, connection_id, created_at, expires_at, updated_at) VALUES ('{A1}', '{C1}', '2026-09-09T12:00:06Z', '2026-09-09T12:00:09Z', '2026-09-09T12:00:07Z')"))).await.is_err());
    let mut cases=vec![
        (attempt("", C1, 1, "pending"), "ck_team_agent_attempt_id_length"),
        (attempt(&"i".repeat(65), C1, 1, "pending"), "ck_team_agent_attempt_id_length"),
        (attempt(A1, &"c".repeat(65), 1, "pending"), "ck_team_agent_attempt_connection_id_length"),
        (attempt(A1, C1, 0, "pending"), "ck_team_agent_attempt_generation_positive"),
        (attempt(A1, C1, 1, "running"), "ck_team_agent_attempt_state"),
    ];
    let mut row=attempt(A1, C1, 1, "pending");
    row.session_id=Set(String::new()); cases.push((row, "ck_team_agent_attempt_session_id_length"));
    let mut row=attempt(A1, C1, 1, "pending");
    row.session_id=Set("s".repeat(129)); cases.push((row, "ck_team_agent_attempt_session_id_length"));
    let mut row=attempt(A1, C1, 1, "pending");
    row.expires_at=Set(ts(6)); cases.push((row, "ck_team_agent_attempt_expires_at_order"));
    let mut row=attempt(A1, C1, 1, "pending");
    row.updated_at=Set(ts(5)); cases.push((row, "ck_team_agent_attempt_updated_at_order"));
    for (row, check) in cases { reject_attempt(&db.conn, row, check).await; }
    named(attempt(A1, C2, 1, "pending").insert(&db.conn).await.unwrap_err(), "FOREIGN KEY constraint failed");
    attempt(A1, C1, 1, "pending").insert(&db.conn).await.unwrap();
    assert!(attempt("20000000-0000-4000-8000-000000000002", C1, 1, "failed").insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn operation_constraints_enforce_recovery_shape_and_idempotency_scope() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    seed_identity(&db.conn, 9901, 9911).await;
    seed_identity(&db.conn, 9902, 9912).await;
    connection(C1, 9901, 9911, "claude", 1, "connecting").insert(&db.conn).await.unwrap();
    connection(C2, 9902, 9912, "claude", 1, "connecting").insert(&db.conn).await.unwrap();
    assert!(db.conn.execute(Statement::from_string(DbBackend::Sqlite, format!("INSERT INTO team_agent_operation (id, connection_id, created_at, expires_at, updated_at) VALUES ('{O1}', '{C1}', '2026-09-09T12:00:06Z', '2026-09-09T12:00:09Z', '2026-09-09T12:00:07Z')"))).await.is_err());
    for (row, check) in [
        (operation("", C1, "connect", "request", "queued"), "ck_team_agent_operation_id_length"),
        (operation(&"i".repeat(65), C1, "connect", "request", "queued"), "ck_team_agent_operation_id_length"),
        (operation(O1, &"c".repeat(65), "connect", "request", "queued"), "ck_team_agent_operation_connection_id_length"),
        (operation(O1, C1, "refresh", "request", "queued"), "ck_team_agent_operation_kind"),
        (operation(O1, C1, "connect", "", "queued"), "ck_team_agent_operation_request_key_length"),
        (operation(O1, C1, "connect", &"r".repeat(65), "queued"), "ck_team_agent_operation_request_key_length"),
        (operation(O1, C1, "connect", "request", "running"), "ck_team_agent_operation_state"),
    ] { reject_operation(&db.conn, row, check).await; }
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.target_attempt_id=Set(Some("t".repeat(65)));
    reject_operation(&db.conn, row, "ck_team_agent_operation_target_attempt_id_length").await;
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.target_generation=Set(0);
    reject_operation(&db.conn, row, "ck_team_agent_operation_target_generation_positive").await;
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.target_generation=Set(2);
    row.result_generation=Set(1);
    reject_operation(&db.conn, row, "ck_team_agent_operation_result_generation").await;
    let mut row = operation(O1, C1, "connect", "request", "leased");
    row.lease_owner = Set(Some(String::new()));
    reject_operation(&db.conn, row, "ck_team_agent_operation_lease_owner_length").await;
    let mut row = operation(O1, C1, "connect", "request", "leased");
    row.lease_owner = Set(Some("w".repeat(65)));
    reject_operation(
        &db.conn,
        row,
        "ck_team_agent_operation_lease_owner_length",
    )
    .await;
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.error_code=Set(Some("provider-response".into()));
    reject_operation(&db.conn, row, "ck_team_agent_operation_error_code").await;
    for (owner, deadline) in [(None, None), (Some("worker".into()), None), (None, Some(ts(8)))] {
        let mut row=operation(O1, C1, "connect", "request", "leased");
    row.lease_owner=Set(owner);
    row.lease_expires_at=Set(deadline);
    reject_operation(&db.conn, row, "ck_team_agent_operation_lease_shape").await;
    }
    for state in ["queued", "completed", "abandoned"] { let mut row=operation(O1, C1, "connect", "request", state);
    row.lease_owner=Set(Some("worker".into()));
    row.lease_expires_at=Set(Some(ts(8)));
    reject_operation(&db.conn, row, "ck_team_agent_operation_lease_shape").await; }
    for kind in ["connect", "cancel"] { let mut row=operation(O1, C1, kind, "request", "queued");
    row.target_attempt_id=Set(None);
    reject_operation(&db.conn, row, "ck_team_agent_operation_target_shape").await; }
    let mut row=operation(O1, C1, "disconnect", "request", "queued");
    row.target_attempt_id=Set(Some(A1.into()));
    reject_operation(&db.conn, row, "ck_team_agent_operation_target_shape").await;
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.expires_at=Set(ts(6));
    reject_operation(&db.conn, row, "ck_team_agent_operation_expires_at_order").await;
    let mut row=operation(O1, C1, "connect", "request", "queued");
    row.updated_at=Set(ts(5));
    reject_operation(&db.conn, row, "ck_team_agent_operation_updated_at_order").await;
    named(operation(O1, C3, "connect", "request", "queued").insert(&db.conn).await.unwrap_err(), "FOREIGN KEY constraint failed");
    operation(O1, C1, "connect", "same-key", "queued").insert(&db.conn).await.expect("opaque target attempt is not a single-column FK");
    assert!(operation("30000000-0000-4000-8000-000000000002", C1, "connect", "same-key", "completed").insert(&db.conn).await.is_err());
    operation("30000000-0000-4000-8000-000000000003", C1, "cancel", "same-key", "completed").insert(&db.conn).await.unwrap();
    operation("30000000-0000-4000-8000-000000000004", C2, "connect", "same-key", "completed").insert(&db.conn).await.unwrap();
}

async fn personal_tables_exist(db: &DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for table in ["team_agent_connection", "team_agent_attempt", "team_agent_operation"] { assert!(manager.has_table(table).await.unwrap(), "missing {table}"); }
}

async fn down_refuses(db: &DatabaseConnection) {
    let error = PersonalAgentMigration.down(&SchemaManager::new(db)).await.expect_err("populated rollback must fail");
    assert_eq!(error.to_string(), "Custom Error: personal agent metadata migration cannot be reverted while rows exist");
    personal_tables_exist(db).await;
    assert_eq!(org::Entity::find().count(db).await.unwrap(), 1);
    assert_eq!(user::Entity::find().count(db).await.unwrap(), 1);
}

async fn orphan_children(db: &DatabaseConnection) {
    sqlite_fks(db, false).await;
    db.execute(Statement::from_string(DbBackend::Sqlite, format!("DELETE FROM team_agent_connection WHERE id='{C1}'"))).await.unwrap();
    sqlite_fks(db, true).await;
}

async fn assert_down_preflights(populated: &str) {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    seed_identity(&db.conn, 9921, 9931).await;
    connection(C1, 9921, 9931, "claude", 1, "connecting").insert(&db.conn).await.unwrap();
    if populated == "attempt" { attempt(A1, C1, 1, "pending").insert(&db.conn).await.unwrap(); orphan_children(&db.conn).await; }
    if populated == "operation" { operation(O1, C1, "disconnect", "guard", "queued").insert(&db.conn).await.unwrap(); orphan_children(&db.conn).await; }
    down_refuses(&db.conn).await;
    let count = db.conn.query_one(Statement::from_string(DbBackend::Sqlite, format!("SELECT COUNT(*) AS count FROM team_agent_{populated}"))).await.unwrap().unwrap().try_get::<i64>("", "count").unwrap();
    assert_eq!(count, 1, "historical {populated} row must remain");
}

#[tokio::test]
async fn down_preflights_connection_rows_before_any_drop() {
    assert_down_preflights("connection").await;
}

#[tokio::test]
async fn down_preflights_historical_attempt_rows_before_any_drop() {
    assert_down_preflights("attempt").await;
}

#[tokio::test]
async fn down_preflights_historical_operation_rows_before_any_drop() {
    assert_down_preflights("operation").await;
}

#[tokio::test]
async fn rerun_retains_rows_and_empty_rollback_is_reversible_and_complete() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let expected_org = seed_org(&db.conn, 9941).await;
    let expected_user = seed_user(&db.conn, 9951, expected_org.id).await;
    let c = connection(C1, 9941, 9951, "codex", 2, "connecting").insert(&db.conn).await.unwrap();
    let a = attempt(A1, C1, 2, "pending").insert(&db.conn).await.unwrap();
    let o = operation(O1, C1, "connect", "rerun", "queued").insert(&db.conn).await.unwrap();
    PersonalAgentMigration.up(&SchemaManager::new(&db.conn)).await.unwrap();
    assert_eq!(team_agent_connection::Entity::find_by_id(C1).one(&db.conn).await.unwrap(), Some(c));
    assert_eq!(team_agent_attempt::Entity::find_by_id(A1).one(&db.conn).await.unwrap(), Some(a));
    assert_eq!(team_agent_operation::Entity::find_by_id(O1).one(&db.conn).await.unwrap(), Some(o));
    team_agent_operation::Entity::delete_many().exec(&db.conn).await.unwrap();
    team_agent_attempt::Entity::delete_many().exec(&db.conn).await.unwrap();
    team_agent_connection::Entity::delete_many().exec(&db.conn).await.unwrap();
    let manager = SchemaManager::new(&db.conn);
    PersonalAgentMigration.down(&manager).await.unwrap();
    for table in ["team_agent_connection", "team_agent_attempt", "team_agent_operation"] { assert!(!manager.has_table(table).await.unwrap()); }
    assert!(manager.has_table("orgs").await.unwrap());
    assert!(manager.has_table("users").await.unwrap());
    assert_eq!(
        org::Entity::find_by_id(expected_org.id)
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_org.clone())
    );
    assert_eq!(
        user::Entity::find_by_id(expected_user.id)
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_user.clone())
    );
    PersonalAgentMigration.up(&manager).await.unwrap();
    personal_tables_exist(&db.conn).await;
    PersonalAgentMigration.down(&manager).await.unwrap();
    for table in ["team_agent_connection", "team_agent_attempt", "team_agent_operation"] { assert!(!manager.has_table(table).await.unwrap()); }
    assert!(manager.has_table("orgs").await.unwrap());
    assert!(manager.has_table("users").await.unwrap());
    assert_eq!(
        org::Entity::find_by_id(expected_org.id)
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_org)
    );
    assert_eq!(
        user::Entity::find_by_id(expected_user.id)
            .one(&db.conn)
            .await
            .unwrap(),
        Some(expected_user)
    );
}

fn validate_database_target(expected: &str, candidate: &str) -> TestResult<()> {
    let suffix = candidate.strip_prefix(DATABASE_PREFIX).unwrap_or_default();
    (candidate==expected && suffix.len()==32 && suffix.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))).then_some(()).ok_or_else(||format!("refusing database target {candidate}; expected exact generated name {expected}"))
}

async fn connect(url: &str, max_connections: u32) -> TestResult<DatabaseConnection> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(options).await.map_err(|e|format!("connect to {url}: {e}"))
}

async fn pg_reject(db: &DatabaseConnection, sql: String, expected: &str) -> TestResult<()> {
    let error = db.execute(Statement::from_string(DbBackend::Postgres, sql)).await.expect_err("Postgres must reject invalid personal agent metadata");
    if !error.to_string().contains(expected) { return Err(format!("Postgres returned wrong constraint for {expected}: {error}")); }
    Ok(())
}

#[test]
fn personal_agent_database_cleanup_guard_requires_exact_generated_hex_name() {
    let expected = "workos_personal_agents_test_0123456789abcdef0123456789abcdef";
    assert!(validate_database_target(expected, expected).is_ok());
    for rejected in ["workos_personal_agents_test_0123456789abcdef0123456789abcdee", "workos_personal_agents_test_0123456789abcdef0123456789abcdeG", "workos_personal_agents_test_0123456789abcdef", "workos_team_credentials_test_0123456789abcdef0123456789abcdef", "postgres"] { assert!(validate_database_target(expected, rejected).is_err()); }
}

#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn personal_agent_migration_postgres_constraints_rerun_and_empty_rollback() {
    let generated = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&generated, &generated).unwrap();
    let admin = connect(ADMIN_URL, 1).await.expect("connect explicit local Postgres admin database");
    admin.execute(Statement::from_string(DbBackend::Postgres, format!("CREATE DATABASE \"{generated}\""))).await.expect("create generated personal-agent test database");
    let url = format!("postgres://air1@localhost:5432/{generated}");
    let db = match connect(&url, 2).await { Ok(db)=>db, Err(error)=>{admin.close().await.unwrap();panic!("{error}; preserved database {generated}")} };
    let outcome: TestResult<()> = async {
        Migrator::up(&db, None).await.map_err(|e|format!("run migrations: {e}"))?;
        seed_identity(&db, 9961, 9971).await;
        for (sql, check) in [
            ("INSERT INTO team_agent_connection (id, org_id, user_id, provider, generation, state, account_label, checked_at, error_code, storage_handle, created_at, updated_at) VALUES ('', 9961, 9971, 'claude', 1, 'disconnected', NULL, NULL, NULL, NULL, NOW(), NOW())".to_owned(), "ck_team_agent_connection_id_length"),
            ("INSERT INTO team_agent_connection (id, org_id, user_id, provider, generation, state, account_label, checked_at, error_code, storage_handle, created_at, updated_at) VALUES ('10000000-0000-4000-8000-000000000091', 9961, 9971, 'gemini', 1, 'disconnected', NULL, NULL, NULL, NULL, NOW(), NOW())".to_owned(), "ck_team_agent_connection_provider"),
            ("INSERT INTO team_agent_connection (id, org_id, user_id, provider, generation, state, account_label, checked_at, error_code, storage_handle, created_at, updated_at) VALUES ('10000000-0000-4000-8000-000000000092', 9961, 9971, 'claude', 1, 'connected', NULL, NULL, NULL, NULL, NOW(), NOW())".to_owned(), "ck_team_agent_connection_state_shape"),
        ] { pg_reject(&db, sql, check).await?; }
        connection(C1, 9961, 9971, "claude", 1, "connecting").insert(&db).await.map_err(|e|e.to_string())?;
        for (sql, check) in [
            (format!("INSERT INTO team_agent_attempt (id, connection_id, generation, session_id, state, created_at, expires_at, updated_at) VALUES ('{A1}', '{C1}', 1, '{S1}', 'pending', NOW(), NOW(), NOW())"), "ck_team_agent_attempt_expires_at_order"),
            (format!("INSERT INTO team_agent_operation (id, connection_id, kind, request_key, target_attempt_id, target_generation, result_generation, state, lease_owner, lease_expires_at, error_code, created_at, expires_at, updated_at) VALUES ('{O1}', '{C1}', 'connect', 'request-1', '{A1}', 2, 1, 'queued', NULL, NULL, NULL, NOW(), NOW()+interval '1 minute', NOW())"), "ck_team_agent_operation_result_generation"),
            (format!("INSERT INTO team_agent_operation (id, connection_id, kind, request_key, target_attempt_id, target_generation, result_generation, state, lease_owner, lease_expires_at, error_code, created_at, expires_at, updated_at) VALUES ('30000000-0000-4000-8000-000000000092', '{C1}', 'disconnect', 'request-2', '{A1}', 1, 1, 'queued', NULL, NULL, NULL, NOW(), NOW()+interval '1 minute', NOW())"), "ck_team_agent_operation_target_shape"),
            (format!("INSERT INTO team_agent_operation (id, connection_id, kind, request_key, target_attempt_id, target_generation, result_generation, state, lease_owner, lease_expires_at, error_code, created_at, expires_at, updated_at) VALUES ('30000000-0000-4000-8000-000000000093', '{C1}', 'connect', 'request-3', '{A1}', 1, 1, 'leased', NULL, NULL, NULL, NOW(), NOW()+interval '1 minute', NOW())"), "ck_team_agent_operation_lease_shape"),
        ] { pg_reject(&db, sql, check).await?; }
        attempt(A1, C1, 1, "pending").insert(&db).await.map_err(|e|e.to_string())?;
        operation(O1, C1, "connect", "postgres-rerun", "queued").insert(&db).await.map_err(|e|e.to_string())?;
        PersonalAgentMigration.up(&SchemaManager::new(&db)).await.map_err(|e|format!("rerun migration: {e}"))?;
        if team_agent_connection::Entity::find().count(&db).await.map_err(|e|e.to_string())? != 1 || team_agent_attempt::Entity::find().count(&db).await.map_err(|e|e.to_string())? != 1 || team_agent_operation::Entity::find().count(&db).await.map_err(|e|e.to_string())? != 1 { return Err("data-bearing Postgres rerun changed personal agent rows".into()); }
        if PersonalAgentMigration.down(&SchemaManager::new(&db)).await.is_ok() { return Err("populated Postgres rollback unexpectedly succeeded".into()); }
        team_agent_operation::Entity::delete_many().exec(&db).await.map_err(|e|e.to_string())?;
        team_agent_attempt::Entity::delete_many().exec(&db).await.map_err(|e|e.to_string())?;
        team_agent_connection::Entity::delete_many().exec(&db).await.map_err(|e|e.to_string())?;
        PersonalAgentMigration.down(&SchemaManager::new(&db)).await.map_err(|e|format!("empty rollback: {e}"))?;
        for table in ["team_agent_connection", "team_agent_attempt", "team_agent_operation"] { if SchemaManager::new(&db).has_table(table).await.map_err(|e|e.to_string())? { return Err(format!("empty Postgres rollback retained {table}")); } }

        PersonalAgentMigration
            .up(&SchemaManager::new(&db))
            .await
            .map_err(|error| format!("recreate personal schema: {error}"))?;
        let deleted_user = user::Entity::delete_by_id(9971)
            .exec(&db)
            .await
            .map_err(|error| error.to_string())?;
        if deleted_user.rows_affected != 1 {
            return Err(format!(
                "expected to remove one invented user, removed {}",
                deleted_user.rows_affected
            ));
        }
        let deleted_org = org::Entity::delete_by_id(9961)
            .exec(&db)
            .await
            .map_err(|error| error.to_string())?;
        if deleted_org.rows_affected != 1 {
            return Err(format!(
                "expected to remove one invented organization, removed {}",
                deleted_org.rows_affected
            ));
        }

        Migrator::down(&db, None)
            .await
            .map_err(|error| format!("full empty rollback: {error}"))?;
        if !Migrator::get_applied_migrations(&db)
            .await
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err("full Postgres rollback retained applied migrations".into());
        }
        for table in CATALOG_TABLES.into_iter().chain(PERSONAL_TABLES) {
            if SchemaManager::new(&db)
                .has_table(table)
                .await
                .map_err(|error| error.to_string())?
            {
                return Err(format!("full Postgres rollback retained {table}"));
            }
        }

        Migrator::up(&db, None)
            .await
            .map_err(|error| format!("full migration rerun: {error}"))?;
        for table in PERSONAL_TABLES {
            let rows = db
                .query_all(Statement::from_string(
                    DbBackend::Postgres,
                    format!("SELECT id FROM {table}"),
                ))
                .await
                .map_err(|error| format!("query rerun {table}: {error}"))?;
            if !rows.is_empty() {
                return Err(format!("rerun personal table {table} was not empty"));
            }
        }
        Ok(())
    }.await;
    if let Err(error) = outcome { db.close().await.unwrap();
    admin.close().await.unwrap(); panic!("{error}; preserved database {generated}"); }
    let connected = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT current_database() AS database_name".to_owned())).await.unwrap().unwrap().try_get::<String>("", "database_name").unwrap();
    validate_database_target(&generated, &connected).expect("cleanup target must equal connected generated database");
    db.close().await.unwrap();
    admin.execute(Statement::from_string(DbBackend::Postgres, format!("DROP DATABASE \"{connected}\""))).await.expect("drop successful personal-agent test database");
    admin.close().await.unwrap();
    eprintln!("verified and removed disposable database {connected}");
}
