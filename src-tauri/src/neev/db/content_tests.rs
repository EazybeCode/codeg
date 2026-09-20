use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait, Value};

const OWNER_ID: i32 = 101;
const OTHER_OWNER_ID: i32 = 102;
const ORG_ID: i32 = 201;
const OTHER_ORG_ID: i32 = 202;
const NODE_ID: i32 = 301;
const CHAT_ID: &str = "10000000-0000-4000-8000-000000000001";

async fn initialize_v2(db: &DatabaseConnection) {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await.unwrap();
    }
    let tx = db.begin().await.unwrap();
    for table in migration::tables() {
        tx.execute(backend.build(&table)).await.unwrap();
    }
    for index in migration::indexes() {
        tx.execute(backend.build(&index)).await.unwrap();
    }
    execute(
        &tx,
        "INSERT INTO neev_schema_version(id,version) VALUES(1,2)",
        vec![],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn seed_v2(db: &DatabaseConnection) -> i32 {
    for (id, name) in [(OWNER_ID, "Owner"), (OTHER_OWNER_ID, "Other owner")] {
        execute(
            db,
            "INSERT INTO neev_accounts(id,display_name,auth_revision,disabled,created_at) VALUES($1,$2,1,false,1000)",
            vec![id.into(), name.into()],
        )
        .await
        .unwrap();
    }
    for (id, owner, name) in [
        (ORG_ID, OWNER_ID, "Primary"),
        (OTHER_ORG_ID, OTHER_OWNER_ID, "Other"),
    ] {
        execute(
            db,
            "INSERT INTO neev_organizations(id,owner_user_id,name,revision,disabled,created_at) VALUES($1,$2,$3,1,false,1000)",
            vec![id.into(), owner.into(), name.into()],
        )
        .await
        .unwrap();
        execute(
            db,
            "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,'admin',1,true)",
            vec![id.into(), owner.into()],
        )
        .await
        .unwrap();
    }
    execute(
        db,
        "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,NULL,'project','Marketing',1,$3)",
        vec![NODE_ID.into(), ORG_ID.into(), OWNER_ID.into()],
    )
    .await
    .unwrap();
    NODE_ID
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> QueryResult {
    one(db, sql, vec![]).await.unwrap().unwrap()
}

async fn schema_version(db: &DatabaseConnection) -> i64 {
    scalar(db, "SELECT version FROM neev_schema_version WHERE id=1")
        .await
        .try_get("", "version")
        .unwrap()
}

async fn node_name(db: &DatabaseConnection, id: i32) -> String {
    one(
        db,
        "SELECT name FROM neev_nodes WHERE id=$1",
        vec![id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "name")
    .unwrap()
}

async fn insert_cross_org_resource(db: &DatabaseConnection) -> Result<(), AuthError> {
    execute(
        db,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'chat',$3,$4,'Cross tenant','private',NULL,NULL,1,0,false,1000,1000)",
        vec![
            OTHER_ORG_ID.into(),
            "10000000-0000-4000-8000-000000000099".into(),
            NODE_ID.into(),
            OTHER_OWNER_ID.into(),
        ],
    )
    .await
    .map(|_| ())
}

async fn insert_document_revision_for_chat(db: &DatabaseConnection) -> Result<(), AuthError> {
    execute(
        db,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'chat',$3,$4,'Chat','private',NULL,NULL,1,0,false,1000,1000)",
        vec![ORG_ID.into(), CHAT_ID.into(), NODE_ID.into(), OWNER_ID.into()],
    )
    .await?;
    execute(
        db,
        "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at,parent_revision,restored_from) VALUES($1,$2,'document',1,'blob-1',$3,1,$4,1000,NULL,NULL)",
        vec![ORG_ID.into(), CHAT_ID.into(), "a".repeat(64).into(), OWNER_ID.into()],
    )
    .await
    .map(|_| ())
}

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres {
        (
            "SELECT 1 AS present FROM pg_tables WHERE schemaname=current_schema() AND tablename=$1",
            vec![table.into()],
        )
    } else {
        (
            "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
            vec![table.into()],
        )
    };
    one(db, sql, values).await.unwrap().is_some()
}

async fn assert_v2_upgrades(db: &DatabaseConnection) {
    let original_node_id = seed_v2(db).await;

    assert_eq!(initialize(db).await, Ok(()));
    assert_eq!(schema_version(db).await, 10);
    assert_eq!(node_name(db, original_node_id).await, "Marketing");
    assert!(insert_cross_org_resource(db).await.is_err());
    assert!(insert_document_revision_for_chat(db).await.is_err());
    assert_eq!(initialize(db).await, Ok(()));
}

async fn assert_refused_without_content_tables(db: &DatabaseConnection) {
    assert_eq!(initialize(db).await, Err(AuthError::Unavailable));
    for table in content_migration::TABLE_NAMES {
        assert!(!table_exists(db, table).await, "unexpected table {table}");
    }
}

#[tokio::test]
async fn sqlite_v2_upgrades_transactionally_and_is_idempotent() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    assert_v2_upgrades(&db).await;
}

#[tokio::test]
async fn fresh_database_initializes_at_version_eight() {
    let db = h::empty_database().await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(schema_version(&db).await, 10);
    assert!(table_exists(&db, "neev_resources").await);
    assert!(table_exists(&db, "neev_content_leases").await);
}

#[tokio::test]
async fn invalid_statement_rolls_back_the_entire_v2_upgrade() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    let original_node_id = seed_v2(&db).await;

    let tx = db.begin().await.unwrap();
    content_migration::upgrade(&tx).await.unwrap();
    assert!(execute(&tx, "THIS IS NOT VALID SQL", vec![]).await.is_err());
    tx.rollback().await.unwrap();

    assert_eq!(schema_version(&db).await, 2);
    assert_eq!(node_name(&db, original_node_id).await, "Marketing");
    for table in content_migration::TABLE_NAMES {
        assert!(
            !table_exists(&db, table).await,
            "upgrade leaked table {table}"
        );
    }
}

#[tokio::test]
async fn v2_schema_with_unknown_version_is_refused_without_writes() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    execute(
        &db,
        "UPDATE neev_schema_version SET version=99 WHERE id=1",
        vec![],
    )
    .await
    .unwrap();
    assert_refused_without_content_tables(&db).await;
}

#[tokio::test]
async fn version_one_schema_is_refused_without_writes() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    execute(
        &db,
        "UPDATE neev_schema_version SET version=1 WHERE id=1",
        vec![],
    )
    .await
    .unwrap();
    assert_refused_without_content_tables(&db).await;
}

#[tokio::test]
async fn v2_schema_with_extra_table_is_refused_without_writes() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    execute(&db, "CREATE TABLE unknown_native_data(id INTEGER)", vec![])
        .await
        .unwrap();
    assert_refused_without_content_tables(&db).await;
}

#[tokio::test]
async fn partial_v2_schema_is_refused_without_writes() {
    let db = h::empty_database().await;
    initialize_v2(&db).await;
    execute(&db, "DROP TABLE neev_mail_leases", vec![])
        .await
        .unwrap();
    assert_refused_without_content_tables(&db).await;
}

#[tokio::test]
async fn partial_v3_schema_is_refused_without_repair() {
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_content_leases", vec![])
        .await
        .unwrap();

    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert!(!table_exists(&db, "neev_content_leases").await);
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_v2_upgrade_preserves_data_and_enforces_content_foreign_keys() {
    let (db, admin, schema) = h::postgres_database().await;
    for table in [
        "neev_gateway_revoke_intents",
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
        "neev_gateway_relying_bindings",
        "neev_gateway_revocations",
        "neev_gateway_audience",
    ] {
        execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    for table in agent_status_migration::TABLE_NAMES.iter().rev() {
        execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(&db, "DROP TABLE neev_document_formats", vec![])
        .await
        .unwrap();
    for table in agent_migration::TABLE_NAMES.iter().rev() {
        execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    for table in content_migration::TABLE_NAMES.iter().rev() {
        execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(
        &db,
        "UPDATE neev_schema_version SET version=2 WHERE id=1",
        vec![],
    )
    .await
    .unwrap();

    assert_v2_upgrades(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

// Reconstruct the previously committed v3 CHECK, including SQL NULL semantics, without
// weakening the production DDL or depending on whichever guards v4 adds.
async fn historical_v3(db: &DatabaseConnection) {
    let backend = db.get_database_backend();
    let tx = db.begin().await.unwrap();
    for table in content_migration::tables() {
        let sql = backend.build(&table).to_string().replace(
            "origin_chat_id IS NOT NULL AND origin_kind IS NOT NULL AND",
            "origin_chat_id IS NOT NULL AND",
        );
        execute(&tx, &sql, vec![]).await.unwrap();
    }
    for index in content_migration::indexes() {
        tx.execute(backend.build(&index)).await.unwrap();
    }
    execute(&tx, "UPDATE neev_schema_version SET version=3", vec![])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn origin_resource(
    db: &DatabaseConnection,
    id: &str,
    kind: &str,
    origin: Option<&str>,
    origin_kind: Option<&str>,
) -> Result<(), AuthError> {
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'Retained','private',$6,$7,1,0,false,1000,1000)", vec![ORG_ID.into(),id.into(),kind.into(),NODE_ID.into(),OWNER_ID.into(),origin.map(str::to_owned).into(),origin_kind.map(str::to_owned).into()]).await.map(|_| ())
}

async fn origin_constraints(db: &DatabaseConnection) {
    origin_resource(db, "source", "chat", None, None)
        .await
        .unwrap();
    origin_resource(db, "document", "document", None, None)
        .await
        .unwrap();
    // A real foreign-organization source with otherwise valid placement.
    execute(db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(302,$1,NULL,'project','Other',1,$2)", vec![OTHER_ORG_ID.into(),OTHER_OWNER_ID.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,'foreign','chat',302,$2,'Foreign','private',1,0,false,1000,1000)", vec![OTHER_ORG_ID.into(),OTHER_OWNER_ID.into()]).await.unwrap();
    for (origin, kind) in [
        (Some("source"), None),
        (None, Some("chat")),
        (Some("missing"), Some("chat")),
        (Some("document"), Some("chat")),
        (Some("foreign"), Some("chat")),
    ] {
        assert!(
            origin_resource(db, "invalid", "document", origin, kind)
                .await
                .is_err(),
            "insert {origin:?}/{kind:?}"
        );
        assert!(
            execute(
                db,
                "UPDATE neev_resources SET origin_chat_id=$1,origin_kind=$2 WHERE id='document'",
                vec![
                    origin.map(str::to_owned).into(),
                    kind.map(str::to_owned).into()
                ]
            )
            .await
            .is_err(),
            "update {origin:?}/{kind:?}"
        );
    }
    origin_resource(db, "linked", "document", Some("source"), Some("chat"))
        .await
        .unwrap();
}

async fn v3_forward_hardening(db: &DatabaseConnection, invalid: bool) {
    seed_v2(db).await;
    historical_v3(db).await;
    origin_resource(
        db,
        "retained",
        "document",
        if invalid { Some("missing") } else { None },
        None,
    )
    .await
    .unwrap();
    if invalid {
        assert_eq!(initialize(db).await, Err(AuthError::Unavailable));
        assert_eq!(schema_version(db).await, 3);
        for table in agent_migration::TABLE_NAMES {
            assert!(
                !table_exists(db, table).await,
                "failed hardening leaked agent table {table}"
            );
        }
        assert_eq!(
            scalar(
                db,
                "SELECT origin_chat_id FROM neev_resources WHERE id='retained'"
            )
            .await
            .try_get::<String>("", "origin_chat_id")
            .unwrap(),
            "missing"
        );
    } else {
        assert_eq!(validate_content_snapshot(db).await, Ok(3));
        assert_eq!(initialize(db).await, Ok(()));
        assert_eq!(schema_version(db).await, 10);
        assert_eq!(validate_content_snapshot(db).await, Ok(10));
        assert_eq!(node_name(db, NODE_ID).await, "Marketing");
        assert_eq!(
            scalar(db, "SELECT title FROM neev_resources WHERE id='retained'")
                .await
                .try_get::<String>("", "title")
                .unwrap(),
            "Retained"
        );
        assert_eq!(initialize(db).await, Ok(()));
        origin_constraints(db).await;
    }
}

#[tokio::test]
async fn sqlite_v3_forward_origin_hardening() {
    for invalid in [false, true] {
        let db = h::empty_database().await;
        initialize_v2(&db).await;
        v3_forward_hardening(&db, invalid).await;
    }
    let db = h::database().await;
    seed_v2(&db).await;
    origin_constraints(&db).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_v3_forward_origin_hardening() {
    use futures::FutureExt;
    for invalid in [false, true] {
        let (db, admin, schema) = h::postgres_database().await;
        for table in [
            "neev_gateway_revoke_intents",
            "neev_gateway_redemptions",
            "neev_gateway_relying_sessions",
            "neev_gateway_exchanges",
            "neev_gateway_relying_bindings",
            "neev_gateway_revocations",
            "neev_gateway_audience",
        ] {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        for table in agent_status_migration::TABLE_NAMES.iter().rev() {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        execute(&db, "DROP TABLE neev_document_formats", vec![])
            .await
            .unwrap();
        for table in agent_migration::TABLE_NAMES.iter().rev() {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        for table in content_migration::TABLE_NAMES.iter().rev() {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        execute(&db, "UPDATE neev_schema_version SET version=2", vec![])
            .await
            .unwrap();
        let outcome = std::panic::AssertUnwindSafe(v3_forward_hardening(&db, invalid))
            .catch_unwind()
            .await;
        if let Err(panic) = outcome {
            eprintln!("retained failed origin migration owned schema: {schema}");
            std::panic::resume_unwind(panic);
        }
        h::cleanup_postgres(db, admin, schema).await;
    }
    let (db, admin, schema) = h::postgres_database().await;
    seed_v2(&db).await;
    origin_constraints(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}
