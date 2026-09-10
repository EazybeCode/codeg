use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

use super::{
    m20260910_000001_chat_privacy::Migration as ChatPrivacyMigration, Migrator,
};

const ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const DATABASE_PREFIX: &str = "workos_chat_privacy_test_";
const PRIVACY_INDEX: &str = "idx_workspace_chat_draft_org_visibility_creator_id";
type TestResult<T> = Result<T, String>;

async fn before_privacy_sqlite() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260910_000001_chat_privacy")
        .unwrap();
    Migrator::up(&db, Some(position as u32)).await.unwrap();
    db
}

async fn execute(db: &DatabaseConnection, sql: impl Into<String>) {
    db.execute(Statement::from_string(db.get_database_backend(), sql.into()))
        .await
        .unwrap();
}

async fn seed_legacy_catalog(db: &DatabaseConnection) {
    execute(
        db,
        r#"INSERT INTO workspace_repository
            (id, org_id, github_repository_id, full_name, verified_names, created_at, updated_at)
            VALUES
            (101, 1, '9001', 'invented-one/repo', '["invented-one/repo"]', '2026-09-10T01:00:00Z', '2026-09-10T01:01:00Z'),
            (202, 2, '9002', 'invented-two/repo', '["invented-two/repo"]', '2026-09-10T01:00:00Z', '2026-09-10T01:01:00Z')"#,
    )
    .await;
    execute(
        db,
        r#"INSERT INTO workspace_node
            (id, org_id, parent_id, kind, name, repository_id, starting_branch, revision, created_at, updated_at)
            VALUES
            (1001, 1, 1002, 'project', 'Invented Project', 101, 'main', 4, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z'),
            (1002, 1, 1001, 'folder', 'Invented Cycle', NULL, NULL, 5, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z'),
            (2001, 2, NULL, 'working_directory', 'Invented Other', 202, 'release', 6, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z')"#,
    )
    .await;
    execute(
        db,
        r#"INSERT INTO workspace_chat_draft
            (id, org_id, node_id, creator_user_id, repository_id, github_repository_id, full_name, starting_branch, title, request_id, request_fingerprint, execution_state, created_at, updated_at)
            VALUES
            (5001, 1, 1001, 11, 101, '9001', 'invented-one/repo', 'main', 'Invented private history', 'request-5001', 'legacy-fingerprint-5001', 'ready', '2026-09-10T01:04:00Z', '2026-09-10T01:05:00Z'),
            (5002, 2, 1001, 9999, 101, '9001', 'invented-one/repo', 'foreign', 'Invented quarantined history', 'request-5002', 'legacy-fingerprint-5002', 'failed', '2026-09-10T01:04:00Z', '2026-09-10T01:05:00Z')"#,
    )
    .await;
    execute(
        db,
        r#"INSERT INTO workspace_chat_execution
            (draft_id, org_id, agent_type, state, allocation_id, starting_commit, branch_name, worktree_relpath, legacy_folder_id, legacy_conversation_id, revision, started_at, updated_at, last_error_code, provisioning_owner_token, provisioning_lease_expires_at)
            VALUES
            (5001, 1, 'codex', 'ready', 'allocation-5001', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'workos/chat/5001', 'orgs/1/workspace-chats/5001/worktree', 71, 81, 7, '2026-09-10T01:06:00Z', '2026-09-10T01:07:00Z', NULL, NULL, NULL),
            (5002, 2, 'claude', 'failed', 'allocation-5002', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'workos/chat/5002', 'orgs/2/workspace-chats/5002/worktree', 72, 82, 8, '2026-09-10T01:06:00Z', '2026-09-10T01:07:00Z', 'invented_failure', NULL, NULL)"#,
    )
    .await;
}

async fn scalar_snapshot(
    db: &DatabaseConnection,
) -> Vec<(i32, i32, i32, i32, String, String, String, String, i32, i32)> {
    db.query_all(Statement::from_string(
        db.get_database_backend(),
        "SELECT d.id, d.org_id, d.node_id, d.creator_user_id, d.full_name, d.starting_branch, d.request_fingerprint, e.branch_name, e.legacy_conversation_id, n.parent_id FROM workspace_chat_draft d JOIN workspace_chat_execution e ON e.draft_id=d.id JOIN workspace_node n ON n.id=d.node_id ORDER BY d.id".to_owned(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get("", "id").unwrap(),
            row.try_get("", "org_id").unwrap(),
            row.try_get("", "node_id").unwrap(),
            row.try_get("", "creator_user_id").unwrap(),
            row.try_get("", "full_name").unwrap(),
            row.try_get("", "starting_branch").unwrap(),
            row.try_get("", "request_fingerprint").unwrap(),
            row.try_get("", "branch_name").unwrap(),
            row.try_get("", "legacy_conversation_id").unwrap(),
            row.try_get("", "parent_id").unwrap(),
        )
    })
    .collect()
}

async fn audience_metadata(db: &DatabaseConnection, id: i32) -> (String, i64, String, i32) {
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT visibility, access_revision, creation_visibility, creation_fingerprint_version FROM workspace_chat_draft WHERE id = $1",
            [id.into()],
        ))
        .await
        .unwrap()
        .expect("existing draft");
    (
        row.try_get("", "visibility").unwrap(),
        row.try_get("", "access_revision").unwrap(),
        row.try_get("", "creation_visibility").unwrap(),
        row.try_get("", "creation_fingerprint_version").unwrap(),
    )
}

fn draft_insert_sql(
    id: i32,
    visibility: &str,
    access_revision: &str,
    creation_visibility: &str,
    version: &str,
) -> String {
    format!(
        "INSERT INTO workspace_chat_draft (id, org_id, node_id, creator_user_id, repository_id, github_repository_id, full_name, starting_branch, title, request_id, request_fingerprint, execution_state, created_at, updated_at, visibility, access_revision, creation_visibility, creation_fingerprint_version) VALUES ({id}, 1, 1001, 11, 101, '9001', 'invented-one/repo', 'main', NULL, 'request-{id}', 'fingerprint-{id}', 'unprovisioned', '2026-09-10T02:00:00Z', '2026-09-10T02:00:00Z', {visibility}, {access_revision}, {creation_visibility}, {version})"
    )
}

async fn reject(db: &DatabaseConnection, sql: String, expected: &str) {
    let error = db
        .execute(Statement::from_string(db.get_database_backend(), sql))
        .await
        .expect_err("invalid privacy metadata must be rejected");
    assert!(
        error.to_string().contains(expected),
        "expected {expected} in {error}"
    );
}

async fn privacy_columns(db: &DatabaseConnection) -> Vec<String> {
    db.query_all(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA table_info('workspace_chat_draft')".to_owned(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get("", "name").unwrap())
    .filter(|name: &String| {
        matches!(
            name.as_str(),
            "visibility"
                | "access_revision"
                | "creation_visibility"
                | "creation_fingerprint_version"
        )
    })
    .collect()
}

async fn sqlite_index_columns(db: &DatabaseConnection) -> Vec<String> {
    db.query_all(Statement::from_string(
        DbBackend::Sqlite,
        format!("PRAGMA index_info('{PRIVACY_INDEX}')"),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get("", "name").unwrap())
    .collect()
}

#[tokio::test]
async fn migration_backfills_private_without_repairing_legacy_identity_or_links() {
    let db = before_privacy_sqlite().await;
    seed_legacy_catalog(&db).await;
    let before = scalar_snapshot(&db).await;

    Migrator::up(&db, None).await.unwrap();

    assert_eq!(scalar_snapshot(&db).await, before);
    for id in [5001, 5002] {
        assert_eq!(
            audience_metadata(&db, id).await,
            ("private".into(), 0, "private".into(), 1)
        );
    }
    assert_eq!(
        sqlite_index_columns(&db).await,
        ["org_id", "visibility", "creator_user_id", "id"]
    );
}

#[tokio::test]
async fn sqlite_rejects_invalid_or_null_privacy_metadata_and_non_integer_revisions() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    for (id, visibility, revision, creation, version, expected) in [
        (5101, "'public'", "0", "'private'", "1", "ck_workspace_chat_draft_visibility"),
        (5102, "NULL", "0", "'private'", "1", "visibility"),
        (5103, "'private'", "-1", "'private'", "1", "ck_workspace_chat_draft_access_revision"),
        (5104, "'private'", "0.5", "'private'", "1", "ck_workspace_chat_draft_access_revision"),
        (5105, "'private'", "'not-an-integer'", "'private'", "1", "ck_workspace_chat_draft_access_revision"),
        (5106, "'private'", "9223372036854775808", "'private'", "1", "ck_workspace_chat_draft_access_revision"),
        (5107, "'private'", "NULL", "'private'", "1", "access_revision"),
        (5108, "'private'", "0", "'public'", "2", "ck_workspace_chat_draft_creation_visibility"),
        (5109, "'private'", "0", "NULL", "2", "creation_visibility"),
        (5110, "'private'", "0", "'private'", "0", "ck_workspace_chat_draft_creation_fingerprint_version"),
        (5111, "'private'", "0", "'private'", "3", "ck_workspace_chat_draft_creation_fingerprint_version"),
        (5112, "'private'", "0", "'private'", "NULL", "creation_fingerprint_version"),
        (5113, "'private'", "0", "'organization'", "1", "ck_workspace_chat_draft_creation_contract"),
    ] {
        reject(
            &db,
            draft_insert_sql(id, visibility, revision, creation, version),
            expected,
        )
        .await;
    }
}

#[tokio::test]
async fn valid_creation_metadata_and_current_visibility_changes_preserve_creation_audience() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    for (id, visibility, creation, version) in [
        (5201, "private", "private", 2),
        (5202, "organization", "organization", 2),
        (5203, "organization", "private", 1),
    ] {
        execute(
            &db,
            draft_insert_sql(
                id,
                &format!("'{visibility}'"),
                "0",
                &format!("'{creation}'"),
                &version.to_string(),
            ),
        )
        .await;
    }
    execute(
        &db,
        "UPDATE workspace_chat_draft SET visibility='private', access_revision=1 WHERE id=5202",
    )
    .await;
    assert_eq!(
        audience_metadata(&db, 5202).await,
        ("private".into(), 1, "organization".into(), 2)
    );
    assert_eq!(
        audience_metadata(&db, 5203).await,
        ("organization".into(), 0, "private".into(), 1)
    );
}

#[tokio::test]
async fn populated_down_refuses_before_removing_any_privacy_evidence() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    execute(&db, draft_insert_sql(5301, "'private'", "0", "'private'", "1")).await;

    let error = ChatPrivacyMigration
        .down(&SchemaManager::new(&db))
        .await
        .expect_err("populated rollback must fail");
    assert_eq!(
        error.to_string(),
        "Custom Error: chat privacy migration cannot be reverted while draft rows exist"
    );
    assert_eq!(privacy_columns(&db).await.len(), 4);
    assert_eq!(sqlite_index_columns(&db).await.len(), 4);
    assert_eq!(audience_metadata(&db, 5301).await.0, "private");
}

#[tokio::test]
async fn empty_up_down_up_is_reversible_without_changing_catalog_or_execution_schema() {
    let db = before_privacy_sqlite().await;
    ChatPrivacyMigration.up(&SchemaManager::new(&db)).await.unwrap();
    assert_eq!(privacy_columns(&db).await.len(), 4);
    ChatPrivacyMigration.down(&SchemaManager::new(&db)).await.unwrap();
    assert!(privacy_columns(&db).await.is_empty());
    assert!(sqlite_index_columns(&db).await.is_empty());
    ChatPrivacyMigration.up(&SchemaManager::new(&db)).await.unwrap();
    execute(&db, draft_insert_sql(5401, "'private'", "0", "'private'", "1")).await;
    assert_eq!(audience_metadata(&db, 5401).await.1, 0);
}

#[tokio::test]
async fn normal_migrator_rerun_keeps_shared_v2_metadata_unchanged() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    execute(
        &db,
        draft_insert_sql(5501, "'organization'", "9", "'private'", "2"),
    )
    .await;
    Migrator::up(&db, None).await.unwrap();
    assert_eq!(
        audience_metadata(&db, 5501).await,
        ("organization".into(), 9, "private".into(), 2)
    );
}

#[tokio::test]
async fn fresh_and_preceding_schema_upgrade_have_identical_privacy_definitions() {
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();
    let upgraded = before_privacy_sqlite().await;
    Migrator::up(&upgraded, None).await.unwrap();
    for object in ["workspace_chat_draft", PRIVACY_INDEX] {
        let sql = |_db: &DatabaseConnection| {
            Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT sql FROM sqlite_master WHERE name=$1",
                [object.into()],
            )
        };
        let fresh_sql = fresh.query_one(sql(&fresh)).await.unwrap().unwrap().try_get::<String>("", "sql").unwrap();
        let upgraded_sql = upgraded.query_one(sql(&upgraded)).await.unwrap().unwrap().try_get::<String>("", "sql").unwrap();
        assert_eq!(fresh_sql, upgraded_sql, "schema differs for {object}");
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
    .ok_or_else(|| format!("refusing database target {candidate}; expected {expected}"))
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

#[test]
fn chat_privacy_database_cleanup_guard_requires_exact_generated_hex_name() {
    let expected = "workos_chat_privacy_test_0123456789abcdef0123456789abcdef";
    assert!(validate_database_target(expected, expected).is_ok());
    for rejected in [
        "workos_chat_privacy_test_0123456789abcdef0123456789abcdee",
        "workos_chat_privacy_test_0123456789abcdef0123456789abcdeG",
        "workos_chat_privacy_test_0123456789abcdef",
        "workos_personal_agents_test_0123456789abcdef0123456789abcdef",
        "postgres",
    ] {
        assert!(validate_database_target(expected, rejected).is_err());
    }
}

#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn chat_privacy_postgres_constraints_backfill_rerun_and_empty_rollback() {
    let generated = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&generated, &generated).unwrap();
    let admin = connect(ADMIN_URL, 1)
        .await
        .expect("connect explicit local Postgres admin database");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE \"{generated}\""),
        ))
        .await
        .expect("create generated chat-privacy test database");
    let url = format!("postgres://air1@localhost:5432/{generated}");
    let db = match connect(&url, 2).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.unwrap();
            panic!("{error}; preserved database {generated}");
        }
    };
    let outcome: TestResult<()> = async {
        let position = Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == "m20260910_000001_chat_privacy")
            .ok_or("privacy migration missing")?;
        Migrator::up(&db, Some(position as u32))
            .await
            .map_err(|error| format!("run preceding migrations: {error}"))?;
        seed_legacy_catalog(&db).await;
        let before = scalar_snapshot(&db).await;
        Migrator::up(&db, None)
            .await
            .map_err(|error| format!("run privacy migration: {error}"))?;
        if scalar_snapshot(&db).await != before {
            return Err("Postgres migration changed legacy identity or execution links".into());
        }
        if audience_metadata(&db, 5002).await
            != ("private".into(), 0, "private".into(), 1)
        {
            return Err("Postgres legacy backfill was not exact private/v1".into());
        }
        let type_row = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT data_type, is_nullable FROM information_schema.columns WHERE table_schema='public' AND table_name='workspace_chat_draft' AND column_name='access_revision'".to_owned())).await.map_err(|error| error.to_string())?.ok_or("access_revision column absent")?;
        if type_row.try_get::<String>("", "data_type").map_err(|error| error.to_string())? != "bigint"
            || type_row.try_get::<String>("", "is_nullable").map_err(|error| error.to_string())? != "NO"
        {
            return Err("Postgres access_revision is not NOT NULL bigint".into());
        }
        for (id, visibility, revision, creation, version, expected) in [
            (5601, "'public'", "0", "'private'", "1", "ck_workspace_chat_draft_visibility"),
            (5602, "NULL", "0", "'private'", "1", "visibility"),
            (5603, "'private'", "-1", "'private'", "1", "ck_workspace_chat_draft_access_revision"),
            (5604, "'private'", "'malformed'", "'private'", "1", "invalid input syntax"),
            (5605, "'private'", "9223372036854775808", "'private'", "1", "out of range"),
            (5606, "'private'", "0", "'public'", "2", "ck_workspace_chat_draft_creation_visibility"),
            (5607, "'private'", "0", "NULL", "2", "creation_visibility"),
            (5608, "'private'", "0", "'private'", "3", "ck_workspace_chat_draft_creation_fingerprint_version"),
            (5609, "'private'", "0", "'private'", "NULL", "creation_fingerprint_version"),
            (5610, "'private'", "0", "'organization'", "1", "ck_workspace_chat_draft_creation_contract"),
        ] {
            let error = db.execute(Statement::from_string(DbBackend::Postgres, draft_insert_sql(id, visibility, revision, creation, version))).await.expect_err("Postgres must reject invalid privacy metadata");
            if !error.to_string().contains(expected) {
                return Err(format!("Postgres returned wrong failure for draft {id}: {error}"));
            }
        }
        execute(&db, draft_insert_sql(5701, "'private'", "0", "'private'", "2")).await;
        execute(&db, draft_insert_sql(5702, "'organization'", "3", "'organization'", "2")).await;
        execute(&db, draft_insert_sql(5703, "'organization'", "0", "'private'", "1")).await;
        execute(&db, "UPDATE workspace_chat_draft SET visibility='private', access_revision=4 WHERE id=5702").await;
        Migrator::up(&db, None).await.map_err(|error| format!("normal rerun: {error}"))?;
        if audience_metadata(&db, 5701).await != ("private".into(), 0, "private".into(), 2)
            || audience_metadata(&db, 5702).await != ("private".into(), 4, "organization".into(), 2)
            || audience_metadata(&db, 5703).await != ("organization".into(), 0, "private".into(), 1)
        {
            return Err("Postgres normal rerun changed shared v2 metadata".into());
        }
        if ChatPrivacyMigration.down(&SchemaManager::new(&db)).await.is_ok() {
            return Err("populated Postgres rollback unexpectedly succeeded".into());
        }
        execute(&db, "DELETE FROM workspace_writer_lease").await;
        execute(&db, "DELETE FROM workspace_chat_execution").await;
        execute(&db, "DELETE FROM workspace_chat_draft").await;
        ChatPrivacyMigration.down(&SchemaManager::new(&db)).await.map_err(|error| format!("empty rollback: {error}"))?;
        ChatPrivacyMigration.up(&SchemaManager::new(&db)).await.map_err(|error| format!("empty rerun: {error}"))?;
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        db.close().await.unwrap();
        admin.close().await.unwrap();
        panic!("{error}; preserved database {generated}");
    }
    let connected = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT current_database() AS database_name".to_owned())).await.unwrap().unwrap().try_get::<String>("", "database_name").unwrap();
    validate_database_target(&generated, &connected).expect("cleanup target must be exact generated database");
    db.close().await.unwrap();
    admin.execute(Statement::from_string(DbBackend::Postgres, format!("DROP DATABASE \"{connected}\""))).await.expect("drop successful chat-privacy test database");
    admin.close().await.unwrap();
    eprintln!("verified and removed disposable database {connected}");
}
