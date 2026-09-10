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

#[derive(Debug, Eq, PartialEq)]
struct LegacySnapshot {
    repositories: Vec<(i32, i32, String, String, String)>,
    nodes: Vec<(
        i32,
        i32,
        Option<i32>,
        String,
        String,
        Option<i32>,
        Option<String>,
        i64,
    )>,
    drafts: Vec<(
        i32,
        i32,
        i32,
        i32,
        i32,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        String,
    )>,
    executions: Vec<(
        i32,
        i32,
        String,
        String,
        String,
        String,
        String,
        String,
        (
            Option<i32>,
            Option<i32>,
            i64,
            Option<String>,
            Option<String>,
        ),
    )>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PostgresPrivacyCatalog {
    columns: Vec<(String, String, String, Option<String>)>,
    checks: Vec<(String, String)>,
    index_columns: Vec<String>,
}

async fn before_privacy_sqlite() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260910_000001_chat_privacy")
        .unwrap();
    Migrator::up(&db, Some(position as u32)).await.unwrap();
    db
}

async fn execute(db: &DatabaseConnection, sql: impl Into<String>) -> TestResult<()> {
    db.execute(Statement::from_string(db.get_database_backend(), sql.into()))
        .await
        .map_err(|error| format!("execute synthetic fixture statement: {error}"))?;
    Ok(())
}

async fn seed_legacy_catalog(db: &DatabaseConnection) -> TestResult<()> {
    execute(
        db,
        r#"INSERT INTO workspace_repository
            (id, org_id, github_repository_id, full_name, verified_names, created_at, updated_at)
            VALUES
            (101, 1, '9001', 'invented-one/repo', '["invented-one/repo"]', '2026-09-10T01:00:00Z', '2026-09-10T01:01:00Z'),
            (202, 2, '9002', 'invented-two/repo', '["invented-two/repo"]', '2026-09-10T01:00:00Z', '2026-09-10T01:01:00Z')"#,
    )
    .await?;
    execute(
        db,
        r#"INSERT INTO workspace_node
            (id, org_id, parent_id, kind, name, repository_id, starting_branch, revision, created_at, updated_at)
            VALUES
            (1001, 1, 1002, 'project', 'Invented Project', 101, 'main', 4, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z'),
            (1002, 1, 1001, 'folder', 'Invented Cycle', NULL, NULL, 5, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z'),
            (2001, 2, NULL, 'working_directory', 'Invented Other', 202, 'release', 6, '2026-09-10T01:02:00Z', '2026-09-10T01:03:00Z')"#,
    )
    .await?;
    execute(
        db,
        r#"INSERT INTO workspace_chat_draft
            (id, org_id, node_id, creator_user_id, repository_id, github_repository_id, full_name, starting_branch, title, request_id, request_fingerprint, execution_state, created_at, updated_at)
            VALUES
            (5001, 1, 1001, 11, 101, '9001', 'invented-one/repo', 'main', 'Invented private history', 'request-5001', 'legacy-fingerprint-5001', 'ready', '2026-09-10T01:04:00Z', '2026-09-10T01:05:00Z'),
            (5002, 2, 1001, 9999, 101, '9001', 'invented-one/repo', 'foreign', 'Invented quarantined history', 'request-5002', 'legacy-fingerprint-5002', 'failed', '2026-09-10T01:04:00Z', '2026-09-10T01:05:00Z')"#,
    )
    .await?;
    execute(
        db,
        r#"INSERT INTO workspace_chat_execution
            (draft_id, org_id, agent_type, state, allocation_id, starting_commit, branch_name, worktree_relpath, legacy_folder_id, legacy_conversation_id, revision, started_at, updated_at, last_error_code, provisioning_owner_token, provisioning_lease_expires_at)
            VALUES
            (5001, 1, 'codex', 'ready', 'allocation-5001', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'workos/chat/5001', 'orgs/1/workspace-chats/5001/worktree', 71, 81, 7, '2026-09-10T01:06:00Z', '2026-09-10T01:07:00Z', NULL, NULL, NULL),
            (5002, 2, 'claude', 'failed', 'allocation-5002', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'workos/chat/5002', 'orgs/2/workspace-chats/5002/worktree', 72, 82, 8, '2026-09-10T01:06:00Z', '2026-09-10T01:07:00Z', 'invented_failure', NULL, NULL)"#,
    )
    .await?;
    Ok(())
}

async fn scalar_snapshot(db: &DatabaseConnection) -> TestResult<LegacySnapshot> {
    let repository_rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, org_id, github_repository_id, full_name, verified_names FROM workspace_repository ORDER BY id".to_owned(),
        ))
        .await
        .map_err(|error| format!("query repository snapshot: {error}"))?;
    let mut repositories = Vec::with_capacity(repository_rows.len());
    for row in repository_rows {
        repositories.push((
            row.try_get("", "id").map_err(|error| error.to_string())?,
            row.try_get("", "org_id").map_err(|error| error.to_string())?,
            row.try_get("", "github_repository_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "full_name").map_err(|error| error.to_string())?,
            row.try_get("", "verified_names")
                .map_err(|error| error.to_string())?,
        ));
    }

    let node_rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, org_id, parent_id, kind, name, repository_id, starting_branch, revision FROM workspace_node ORDER BY id".to_owned(),
        ))
        .await
        .map_err(|error| format!("query node snapshot: {error}"))?;
    let mut nodes = Vec::with_capacity(node_rows.len());
    for row in node_rows {
        nodes.push((
            row.try_get("", "id").map_err(|error| error.to_string())?,
            row.try_get("", "org_id").map_err(|error| error.to_string())?,
            row.try_get("", "parent_id").map_err(|error| error.to_string())?,
            row.try_get("", "kind").map_err(|error| error.to_string())?,
            row.try_get("", "name").map_err(|error| error.to_string())?,
            row.try_get("", "repository_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "starting_branch")
                .map_err(|error| error.to_string())?,
            row.try_get("", "revision").map_err(|error| error.to_string())?,
        ));
    }

    let draft_rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, org_id, node_id, creator_user_id, repository_id, github_repository_id, full_name, starting_branch, title, request_id, request_fingerprint, execution_state FROM workspace_chat_draft ORDER BY id".to_owned(),
        ))
        .await
        .map_err(|error| format!("query draft snapshot: {error}"))?;
    let mut drafts = Vec::with_capacity(draft_rows.len());
    for row in draft_rows {
        drafts.push((
            row.try_get("", "id").map_err(|error| error.to_string())?,
            row.try_get("", "org_id").map_err(|error| error.to_string())?,
            row.try_get("", "node_id").map_err(|error| error.to_string())?,
            row.try_get("", "creator_user_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "repository_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "github_repository_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "full_name").map_err(|error| error.to_string())?,
            row.try_get("", "starting_branch")
                .map_err(|error| error.to_string())?,
            row.try_get("", "title").map_err(|error| error.to_string())?,
            row.try_get("", "request_id").map_err(|error| error.to_string())?,
            row.try_get("", "request_fingerprint")
                .map_err(|error| error.to_string())?,
            row.try_get("", "execution_state")
                .map_err(|error| error.to_string())?,
        ));
    }

    let execution_rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT draft_id, org_id, agent_type, state, allocation_id, starting_commit, branch_name, worktree_relpath, legacy_folder_id, legacy_conversation_id, revision, last_error_code, provisioning_owner_token FROM workspace_chat_execution ORDER BY draft_id".to_owned(),
        ))
        .await
        .map_err(|error| format!("query execution snapshot: {error}"))?;
    let mut executions = Vec::with_capacity(execution_rows.len());
    for row in execution_rows {
        executions.push((
            row.try_get("", "draft_id").map_err(|error| error.to_string())?,
            row.try_get("", "org_id").map_err(|error| error.to_string())?,
            row.try_get("", "agent_type").map_err(|error| error.to_string())?,
            row.try_get("", "state").map_err(|error| error.to_string())?,
            row.try_get("", "allocation_id")
                .map_err(|error| error.to_string())?,
            row.try_get("", "starting_commit")
                .map_err(|error| error.to_string())?,
            row.try_get("", "branch_name")
                .map_err(|error| error.to_string())?,
            row.try_get("", "worktree_relpath")
                .map_err(|error| error.to_string())?,
            (
                row.try_get("", "legacy_folder_id")
                    .map_err(|error| error.to_string())?,
                row.try_get("", "legacy_conversation_id")
                    .map_err(|error| error.to_string())?,
                row.try_get("", "revision")
                    .map_err(|error| error.to_string())?,
                row.try_get("", "last_error_code")
                    .map_err(|error| error.to_string())?,
                row.try_get("", "provisioning_owner_token")
                    .map_err(|error| error.to_string())?,
            ),
        ));
    }

    Ok(LegacySnapshot {
        repositories,
        nodes,
        drafts,
        executions,
    })
}

async fn audience_metadata(
    db: &DatabaseConnection,
    id: i32,
) -> TestResult<(String, i64, String, i32)> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT visibility, access_revision, creation_visibility, creation_fingerprint_version FROM workspace_chat_draft WHERE id = $1",
            [id.into()],
        ))
        .await
        .map_err(|error| format!("query audience metadata for draft {id}: {error}"))?
        .ok_or_else(|| format!("missing synthetic draft {id}"))?;
    Ok((
        row.try_get("", "visibility")
            .map_err(|error| error.to_string())?,
        row.try_get("", "access_revision")
            .map_err(|error| error.to_string())?,
        row.try_get("", "creation_visibility")
            .map_err(|error| error.to_string())?,
        row.try_get("", "creation_fingerprint_version")
            .map_err(|error| error.to_string())?,
    ))
}

async fn privacy_rows(
    db: &DatabaseConnection,
) -> TestResult<Vec<(i32, String, i64, String, i32)>> {
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, visibility, access_revision, creation_visibility, creation_fingerprint_version FROM workspace_chat_draft ORDER BY id".to_owned(),
        ))
        .await
        .map_err(|error| format!("query privacy row snapshot: {error}"))?;
    let mut snapshot = Vec::with_capacity(rows.len());
    for row in rows {
        snapshot.push((
            row.try_get("", "id").map_err(|error| error.to_string())?,
            row.try_get("", "visibility")
                .map_err(|error| error.to_string())?,
            row.try_get("", "access_revision")
                .map_err(|error| error.to_string())?,
            row.try_get("", "creation_visibility")
                .map_err(|error| error.to_string())?,
            row.try_get("", "creation_fingerprint_version")
                .map_err(|error| error.to_string())?,
        ));
    }
    Ok(snapshot)
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
    seed_legacy_catalog(&db).await.unwrap();
    let before = scalar_snapshot(&db).await.unwrap();

    Migrator::up(&db, None).await.unwrap();

    assert_eq!(scalar_snapshot(&db).await.unwrap(), before);
    for id in [5001, 5002] {
        assert_eq!(
            audience_metadata(&db, id).await.unwrap(),
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
        .await
        .unwrap();
    }
    execute(
        &db,
        "UPDATE workspace_chat_draft SET visibility='private', access_revision=1 WHERE id=5202",
    )
    .await
    .unwrap();
    assert_eq!(
        audience_metadata(&db, 5202).await.unwrap(),
        ("private".into(), 1, "organization".into(), 2)
    );
    assert_eq!(
        audience_metadata(&db, 5203).await.unwrap(),
        ("organization".into(), 0, "private".into(), 1)
    );
}

#[tokio::test]
async fn populated_down_refuses_before_removing_any_privacy_evidence() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    execute(&db, draft_insert_sql(5301, "'private'", "0", "'private'", "1"))
        .await
        .unwrap();

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
    assert_eq!(audience_metadata(&db, 5301).await.unwrap().0, "private");
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
    execute(&db, draft_insert_sql(5401, "'private'", "0", "'private'", "1"))
        .await
        .unwrap();
    assert_eq!(audience_metadata(&db, 5401).await.unwrap().1, 0);
}

#[tokio::test]
async fn normal_migrator_rerun_keeps_shared_v2_metadata_unchanged() {
    let db = before_privacy_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    execute(
        &db,
        draft_insert_sql(5501, "'organization'", "9", "'private'", "2"),
    )
    .await
    .unwrap();
    Migrator::up(&db, None).await.unwrap();
    assert_eq!(
        audience_metadata(&db, 5501).await.unwrap(),
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

async fn postgres_privacy_catalog(
    db: &DatabaseConnection,
) -> TestResult<PostgresPrivacyCatalog> {
    let column_rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema='public' AND table_name='workspace_chat_draft' AND column_name IN ('visibility', 'access_revision', 'creation_visibility', 'creation_fingerprint_version') ORDER BY ordinal_position".to_owned(),
        ))
        .await
        .map_err(|error| format!("query Postgres privacy columns: {error}"))?;
    let mut columns = Vec::with_capacity(column_rows.len());
    for row in column_rows {
        columns.push((
            row.try_get("", "column_name")
                .map_err(|error| error.to_string())?,
            row.try_get("", "data_type")
                .map_err(|error| error.to_string())?,
            row.try_get("", "is_nullable")
                .map_err(|error| error.to_string())?,
            row.try_get("", "column_default")
                .map_err(|error| error.to_string())?,
        ));
    }

    let check_rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT constraint_row.conname::text AS constraint_name, pg_get_constraintdef(constraint_row.oid) AS definition FROM pg_constraint constraint_row JOIN pg_class table_row ON table_row.oid=constraint_row.conrelid JOIN pg_namespace schema_row ON schema_row.oid=table_row.relnamespace WHERE schema_row.nspname='public' AND table_row.relname='workspace_chat_draft' AND constraint_row.contype='c' AND constraint_row.conname LIKE 'ck_workspace_chat_draft_%' ORDER BY constraint_row.conname".to_owned(),
        ))
        .await
        .map_err(|error| format!("query Postgres privacy checks: {error}"))?;
    let mut checks = Vec::with_capacity(check_rows.len());
    for row in check_rows {
        checks.push((
            row.try_get("", "constraint_name")
                .map_err(|error| error.to_string())?,
            row.try_get("", "definition")
                .map_err(|error| error.to_string())?,
        ));
    }

    let index_rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT attribute.attname::text AS column_name FROM pg_class index_row JOIN pg_namespace schema_row ON schema_row.oid=index_row.relnamespace JOIN pg_index index_metadata ON index_metadata.indexrelid=index_row.oid CROSS JOIN LATERAL unnest(index_metadata.indkey) WITH ORDINALITY AS key(attnum, position) JOIN pg_attribute attribute ON attribute.attrelid=index_metadata.indrelid AND attribute.attnum=key.attnum WHERE schema_row.nspname='public' AND index_row.relname=$1 ORDER BY key.position",
            [PRIVACY_INDEX.into()],
        ))
        .await
        .map_err(|error| format!("query Postgres privacy index: {error}"))?;
    let mut index_columns = Vec::with_capacity(index_rows.len());
    for row in index_rows {
        index_columns.push(
            row.try_get("", "column_name")
                .map_err(|error| error.to_string())?,
        );
    }

    Ok(PostgresPrivacyCatalog {
        columns,
        checks,
        index_columns,
    })
}

async fn postgres_database_name(db: &DatabaseConnection) -> TestResult<String> {
    db.query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT current_database() AS database_name".to_owned(),
    ))
    .await
    .map_err(|error| format!("query connected Postgres database name: {error}"))?
    .ok_or_else(|| "connected Postgres database name missing".to_owned())?
    .try_get("", "database_name")
    .map_err(|error| error.to_string())
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
    let upgraded_name = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    let fresh_name = format!("{DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&upgraded_name, &upgraded_name).unwrap();
    validate_database_target(&fresh_name, &fresh_name).unwrap();
    eprintln!(
        "Postgres fixtures, preserved on any failure: upgraded={upgraded_name}, fresh={fresh_name}"
    );
    let admin = connect(ADMIN_URL, 1)
        .await
        .expect("connect explicit local Postgres admin database");
    for database_name in [&upgraded_name, &fresh_name] {
        if let Err(error) = admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!("CREATE DATABASE \"{database_name}\""),
            ))
            .await
        {
            admin.close().await.ok();
            panic!(
                "create generated chat-privacy database {database_name}: {error}; fixtures upgraded={upgraded_name}, fresh={fresh_name}"
            );
        }
    }
    let upgraded_url = format!("postgres://air1@localhost:5432/{upgraded_name}");
    let upgraded = match connect(&upgraded_url, 2).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.ok();
            panic!(
                "{error}; preserved fixtures upgraded={upgraded_name}, fresh={fresh_name}"
            );
        }
    };
    let fresh_url = format!("postgres://air1@localhost:5432/{fresh_name}");
    let fresh = match connect(&fresh_url, 2).await {
        Ok(db) => db,
        Err(error) => {
            upgraded.close().await.ok();
            admin.close().await.ok();
            panic!(
                "{error}; preserved fixtures upgraded={upgraded_name}, fresh={fresh_name}"
            );
        }
    };
    let outcome: TestResult<()> = async {
        let position = Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == "m20260910_000001_chat_privacy")
            .ok_or("privacy migration missing")?;
        Migrator::up(&upgraded, Some(position as u32))
            .await
            .map_err(|error| format!("run preceding migrations: {error}"))?;
        seed_legacy_catalog(&upgraded).await?;
        let before = scalar_snapshot(&upgraded).await?;
        Migrator::up(&upgraded, None)
            .await
            .map_err(|error| format!("run privacy migration: {error}"))?;
        Migrator::up(&fresh, None)
            .await
            .map_err(|error| format!("run fresh migration chain: {error}"))?;
        if scalar_snapshot(&upgraded).await? != before {
            return Err("Postgres migration changed legacy identity or execution links".into());
        }
        if audience_metadata(&upgraded, 5002).await?
            != ("private".into(), 0, "private".into(), 1)
        {
            return Err("Postgres legacy backfill was not exact private/v1".into());
        }
        let upgraded_catalog = postgres_privacy_catalog(&upgraded).await?;
        let fresh_catalog = postgres_privacy_catalog(&fresh).await?;
        if upgraded_catalog != fresh_catalog {
            return Err(format!(
                "Postgres fresh and upgraded privacy catalogs differ: upgraded={upgraded_catalog:?}, fresh={fresh_catalog:?}"
            ));
        }
        let expected_columns = vec![
            ("visibility".into(), "text".into(), "NO".into(), Some("'private'::text".into())),
            ("access_revision".into(), "bigint".into(), "NO".into(), Some("0".into())),
            ("creation_visibility".into(), "text".into(), "NO".into(), Some("'private'::text".into())),
            ("creation_fingerprint_version".into(), "integer".into(), "NO".into(), Some("1".into())),
        ];
        if upgraded_catalog.columns != expected_columns {
            return Err(format!(
                "Postgres privacy column types/defaults differ: {:?}",
                upgraded_catalog.columns
            ));
        }
        let expected_checks = [
            "ck_workspace_chat_draft_access_revision",
            "ck_workspace_chat_draft_creation_contract",
            "ck_workspace_chat_draft_creation_fingerprint_version",
            "ck_workspace_chat_draft_creation_visibility",
            "ck_workspace_chat_draft_visibility",
        ];
        if upgraded_catalog
            .checks
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            != expected_checks
        {
            return Err(format!(
                "Postgres privacy check catalog differs: {:?}",
                upgraded_catalog.checks
            ));
        }
        if upgraded_catalog.index_columns
            != ["org_id", "visibility", "creator_user_id", "id"]
        {
            return Err(format!(
                "Postgres privacy index order differs: {:?}",
                upgraded_catalog.index_columns
            ));
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
            let error = upgraded.execute(Statement::from_string(DbBackend::Postgres, draft_insert_sql(id, visibility, revision, creation, version))).await.expect_err("Postgres must reject invalid privacy metadata");
            if !error.to_string().contains(expected) {
                return Err(format!("Postgres returned wrong failure for draft {id}: {error}"));
            }
        }
        execute(&upgraded, draft_insert_sql(5701, "'private'", "0", "'private'", "2")).await?;
        execute(&upgraded, draft_insert_sql(5702, "'organization'", "3", "'organization'", "2")).await?;
        execute(&upgraded, draft_insert_sql(5703, "'organization'", "0", "'private'", "1")).await?;
        execute(&upgraded, "UPDATE workspace_chat_draft SET visibility='private', access_revision=4 WHERE id=5702").await?;
        Migrator::up(&upgraded, None).await.map_err(|error| format!("normal rerun: {error}"))?;
        if audience_metadata(&upgraded, 5701).await? != ("private".into(), 0, "private".into(), 2)
            || audience_metadata(&upgraded, 5702).await? != ("private".into(), 4, "organization".into(), 2)
            || audience_metadata(&upgraded, 5703).await? != ("organization".into(), 0, "private".into(), 1)
        {
            return Err("Postgres normal rerun changed shared v2 metadata".into());
        }
        let before_refusal_catalog = postgres_privacy_catalog(&upgraded).await?;
        let before_refusal_rows = privacy_rows(&upgraded).await?;
        let rollback_error = match ChatPrivacyMigration
            .down(&SchemaManager::new(&upgraded))
            .await
        {
            Ok(()) => return Err("populated Postgres rollback unexpectedly succeeded".into()),
            Err(error) => error,
        };
        if rollback_error.to_string()
            != "Custom Error: chat privacy migration cannot be reverted while draft rows exist"
        {
            return Err(format!("unexpected Postgres rollback refusal: {rollback_error}"));
        }
        if postgres_privacy_catalog(&upgraded).await? != before_refusal_catalog
            || privacy_rows(&upgraded).await? != before_refusal_rows
        {
            return Err("failed Postgres rollback changed privacy schema or rows".into());
        }
        execute(&upgraded, "DELETE FROM workspace_writer_lease").await?;
        execute(&upgraded, "DELETE FROM workspace_chat_execution").await?;
        execute(&upgraded, "DELETE FROM workspace_chat_draft").await?;
        ChatPrivacyMigration.down(&SchemaManager::new(&upgraded)).await.map_err(|error| format!("empty rollback: {error}"))?;
        ChatPrivacyMigration.up(&SchemaManager::new(&upgraded)).await.map_err(|error| format!("empty rerun: {error}"))?;
        if postgres_privacy_catalog(&upgraded).await? != fresh_catalog {
            return Err("Postgres empty up/down/up changed privacy catalog".into());
        }
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        upgraded.close().await.ok();
        fresh.close().await.ok();
        admin.close().await.ok();
        panic!(
            "{error}; preserved fixtures upgraded={upgraded_name}, fresh={fresh_name}"
        );
    }
    let connected_upgraded = postgres_database_name(&upgraded)
        .await
        .unwrap_or_else(|error| panic!("{error}; preserved upgraded={upgraded_name}, fresh={fresh_name}"));
    let connected_fresh = postgres_database_name(&fresh)
        .await
        .unwrap_or_else(|error| panic!("{error}; preserved upgraded={upgraded_name}, fresh={fresh_name}"));
    validate_database_target(&upgraded_name, &connected_upgraded)
        .unwrap_or_else(|error| panic!("{error}; preserved upgraded={upgraded_name}, fresh={fresh_name}"));
    validate_database_target(&fresh_name, &connected_fresh)
        .unwrap_or_else(|error| panic!("{error}; preserved upgraded={upgraded_name}, fresh={fresh_name}"));
    upgraded.close().await.unwrap_or_else(|error| {
        panic!("close {connected_upgraded}: {error}; preserved fresh={connected_fresh}")
    });
    fresh.close().await.unwrap_or_else(|error| {
        panic!("close {connected_fresh}: {error}; preserved upgraded={connected_upgraded}")
    });
    for database_name in [&connected_upgraded, &connected_fresh] {
        admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!("DROP DATABASE \"{database_name}\""),
            ))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "drop successful chat-privacy database {database_name}: {error}; upgraded={connected_upgraded}, fresh={connected_fresh}"
                )
            });
    }
    admin.close().await.unwrap();
    eprintln!(
        "verified and removed disposable databases {connected_upgraded}, {connected_fresh}"
    );
}
