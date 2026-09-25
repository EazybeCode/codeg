use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Value};

const CONNECTION_ID: &str = "10000000000000000000000000000001";
const CHECK_ID: &str = "20000000000000000000000000000002";
const BOOT_ID: &str = "30000000000000000000000000000003";
const STORAGE_HANDLE: &str = "40000000000000000000000000000004";
const SESSION_ID: &str = "50000000-0000-4000-8000-000000000005";
const DOCUMENT_ID: &str = "60000000-0000-4000-8000-000000000006";

async fn schema_version(db: &DatabaseConnection) -> i64 {
    one(
        db,
        "SELECT version FROM neev_schema_version WHERE id=1",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "version")
    .unwrap()
}

async fn relation_exists(db: &DatabaseConnection, name: &str) -> bool {
    let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres {
        (
            "SELECT 1 AS present FROM pg_tables WHERE schemaname=current_schema() AND tablename=$1",
            vec![name.into()],
        )
    } else {
        (
            "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
            vec![name.into()],
        )
    };
    one(db, sql, values).await.unwrap().is_some()
}

async fn owner(db: &DatabaseConnection) -> Option<(i32, i64, Option<String>, String)> {
    one(
        db,
        "SELECT id,epoch,boot_id,phase FROM neev_agent_status_owner LIMIT 2",
        vec![],
    )
    .await
    .ok()
    .flatten()
    .map(|row| {
        (
            row.try_get("", "id").unwrap(),
            row.try_get("", "epoch").unwrap(),
            row.try_get("", "boot_id").unwrap(),
            row.try_get("", "phase").unwrap(),
        )
    })
}

async fn drop_status(db: &DatabaseConnection) {
    execute(db, "DROP TABLE neev_agent_status_checks", vec![])
        .await
        .unwrap();
    execute(db, "DROP TABLE neev_agent_status_owner", vec![])
        .await
        .unwrap();
}

async fn seed_connection(db: &DatabaseConnection, state: &str) -> (i32, i32) {
    let user = h::native_account(db, "status-owner", "status-owner@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let (label, checked_at, handle): (Option<String>, Option<i64>, Option<String>) =
        if state == "connected" {
            (
                Some("Status account".into()),
                Some(1_700_000_000),
                Some(STORAGE_HANDLE.into()),
            )
        } else {
            (None, None, None)
        };
    execute(db, "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,'codex',1,$4,$5,$6,NULL,$7,1000,1000)", vec![CONNECTION_ID.into(), org.into(), user.into(), state.into(), label.into(), checked_at.into(), handle.into()]).await.unwrap();
    (org, user)
}

async fn insert_valid_check(db: &DatabaseConnection) {
    execute(db, "INSERT INTO neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at) VALUES($1,$2,1,$3,1,$4,0,60)", vec![CONNECTION_ID.into(), CHECK_ID.into(), SESSION_ID.into(), STORAGE_HANDLE.into()]).await.unwrap();
}

#[tokio::test]
async fn sqlite_status_schema_fresh_snapshot_is_eleven() {
    let db = h::database().await;
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(mention_migration::SCHEMA_VERSION)
    );
}

#[tokio::test]
async fn sqlite_status_schema_seeds_closed_owner() {
    let db = h::database().await;
    assert_eq!(owner(&db).await, Some((1, 0, None, "closed".to_owned())));
}

async fn status_upgrade_preserves_exact_six_and_ready_seven(db: &DatabaseConnection) {
    let (org, user) = seed_connection(db, "connected").await;
    execute(db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(901,$1,NULL,'project','Status project',1,$2)", vec![org.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',901,$3,'Status history','private',1,1,false,1000,1001)", vec![org.into(), DOCUMENT_ID.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at) VALUES($1,$2,'document',1,'status-blob',$3,17,$4,1001)", vec![org.into(), DOCUMENT_ID.into(), "a".repeat(64).into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_document_formats(org_id,document_id,revision,format,format_version) VALUES($1,$2,1,'blocks',1)", vec![org.into(), DOCUMENT_ID.into()]).await.unwrap();
    let before = one(db, "SELECT c.state,c.account_label,c.checked_at,c.storage_handle,r.blob_key,r.sha256,r.byte_length,f.format,f.format_version FROM neev_agent_connections c JOIN neev_document_revisions r ON r.org_id=$1 AND r.document_id=$2 JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE c.id=$3", vec![org.into(), DOCUMENT_ID.into(), CONNECTION_ID.into()]).await.unwrap().unwrap();
    let before: (
        String,
        String,
        i64,
        String,
        String,
        String,
        i64,
        String,
        i32,
    ) = (
        before.try_get("", "state").unwrap(),
        before.try_get("", "account_label").unwrap(),
        before.try_get("", "checked_at").unwrap(),
        before.try_get("", "storage_handle").unwrap(),
        before.try_get("", "blob_key").unwrap(),
        before.try_get("", "sha256").unwrap(),
        before.try_get("", "byte_length").unwrap(),
        before.try_get("", "format").unwrap(),
        before.try_get("", "format_version").unwrap(),
    );
    super::tests::drop_mention_tables(db).await;
    super::tests::drop_attachment_tables(db).await;
    for table in ["neev_agent_turns", "neev_agent_sessions"] {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(db, "DROP TABLE neev_image_blobs", vec![])
        .await
        .unwrap();
    execute(db, "DROP TABLE neev_org_join_codes", vec![])
        .await
        .unwrap();
    for table in [
        "neev_gateway_revoke_intents",
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
        "neev_gateway_relying_bindings",
        "neev_gateway_revocations",
        "neev_gateway_audience",
    ] {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    drop_status(db).await;
    execute(db, "UPDATE neev_schema_version SET version=6", vec![])
        .await
        .unwrap();
    assert_eq!(validate_content_snapshot(db).await, Ok(6));
    assert_eq!(initialize(db).await, Ok(()));
    assert_eq!(
        validate_content_snapshot(db).await,
        Ok(mention_migration::SCHEMA_VERSION)
    );
    let after = one(db, "SELECT c.state,c.account_label,c.checked_at,c.storage_handle,r.blob_key,r.sha256,r.byte_length,f.format,f.format_version FROM neev_agent_connections c JOIN neev_document_revisions r ON r.org_id=$1 AND r.document_id=$2 JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE c.id=$3", vec![org.into(), DOCUMENT_ID.into(), CONNECTION_ID.into()]).await.unwrap().unwrap();
    let after: (
        String,
        String,
        i64,
        String,
        String,
        String,
        i64,
        String,
        i32,
    ) = (
        after.try_get("", "state").unwrap(),
        after.try_get("", "account_label").unwrap(),
        after.try_get("", "checked_at").unwrap(),
        after.try_get("", "storage_handle").unwrap(),
        after.try_get("", "blob_key").unwrap(),
        after.try_get("", "sha256").unwrap(),
        after.try_get("", "byte_length").unwrap(),
        after.try_get("", "format").unwrap(),
        after.try_get("", "format_version").unwrap(),
    );
    assert_eq!(after, before);
    assert_eq!(owner(db).await, Some((1, 0, None, "closed".to_owned())));
    execute(
        db,
        "UPDATE neev_agent_status_owner SET epoch=1,boot_id=$1,phase='ready' WHERE id=1",
        vec![BOOT_ID.into()],
    )
    .await
    .unwrap();
    insert_valid_check(db).await;
    assert_eq!(initialize(db).await, Ok(()));
    assert_eq!(
        owner(db).await,
        Some((1, 1, Some(BOOT_ID.to_owned()), "ready".to_owned()))
    );
    assert_eq!(h::count(db, "neev_agent_status_checks").await, 1);
    assert_eq!(
        validate_content_snapshot(db).await,
        Ok(mention_migration::SCHEMA_VERSION)
    );
}

#[tokio::test]
async fn sqlite_status_schema_exact_six_upgrade_preserves_data_and_ready_seven() {
    status_upgrade_preserves_exact_six_and_ready_seven(&h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_status_schema_exact_six_upgrade_preserves_data_and_ready_seven() {
    use futures::FutureExt;
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("status migration owned schema: {schema}");
    let outcome =
        std::panic::AssertUnwindSafe(status_upgrade_preserves_exact_six_and_ready_seven(&db))
            .catch_unwind()
            .await;
    match outcome {
        Ok(()) => h::cleanup_postgres(db, admin, schema).await,
        Err(panic) => {
            eprintln!("retained failed status migration owned schema: {schema}");
            drop(db);
            drop(admin);
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn sqlite_status_schema_failed_six_upgrade_is_atomic() {
    let db = h::database().await;
    let user = h::native_account(&db, "atomic-owner", "atomic-owner@example.test").await;
    let account_before = one(
        &db,
        "SELECT display_name FROM neev_accounts WHERE id=$1",
        vec![user.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<String>("", "display_name")
    .unwrap();
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
    drop_status(&db).await;
    execute(&db, "UPDATE neev_schema_version SET version=6", vec![])
        .await
        .unwrap();
    execute(&db, "CREATE TRIGGER fail_status_version BEFORE UPDATE OF version ON neev_schema_version BEGIN SELECT RAISE(ABORT,'forced status migration failure'); END", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(schema_version(&db).await, 6);
    assert_eq!(h::count(&db, "neev_accounts").await, 1);
    assert_eq!(
        one(
            &db,
            "SELECT display_name FROM neev_accounts WHERE id=$1",
            vec![user.into()]
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "display_name")
        .unwrap(),
        account_before
    );
    for table in agent_status_migration::TABLE_NAMES {
        assert!(!relation_exists(&db, table).await, "upgrade leaked {table}");
    }
}

async fn rebuild_unconstrained_status(db: &DatabaseConnection) {
    drop_status(db).await;
    execute(
        db,
        "CREATE TABLE neev_agent_status_owner(id,epoch,boot_id,phase)",
        vec![],
    )
    .await
    .unwrap();
    execute(db, "CREATE TABLE neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at)", vec![]).await.unwrap();
    execute(
        db,
        "INSERT INTO neev_agent_status_owner VALUES(1,1,$1,'ready')",
        vec![BOOT_ID.into()],
    )
    .await
    .unwrap();
}

async fn assert_imported_corruption(sql: &str) {
    let db = h::database().await;
    seed_connection(&db, "disconnected").await;
    rebuild_unconstrained_status(&db).await;
    insert_valid_check(&db).await;
    execute(&db, sql, vec![]).await.unwrap();
    assert_eq!(
        agent_status_migration::validate(&db).await,
        Err(AuthError::Unavailable),
        "validator accepted imported corruption: {sql}"
    );
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable),
        "snapshot accepted imported corruption: {sql}"
    );
}

#[tokio::test]
async fn sqlite_status_schema_rejects_owner_inventory_and_field_corruption() {
    for sql in [
        "DELETE FROM neev_agent_status_owner",
        "INSERT INTO neev_agent_status_owner VALUES(2,1,'30000000000000000000000000000003','ready')",
        "UPDATE neev_agent_status_owner SET id=2",
        "UPDATE neev_agent_status_owner SET id='1'",
        "UPDATE neev_agent_status_owner SET epoch=-1",
        "UPDATE neev_agent_status_owner SET epoch='1'",
        "UPDATE neev_agent_status_owner SET epoch=1.5",
        "UPDATE neev_agent_status_owner SET epoch=0",
        "UPDATE neev_agent_status_owner SET boot_id=NULL",
        "UPDATE neev_agent_status_owner SET boot_id=X'3030303030303030303030303030303030303030303030303030303030303030'",
        "UPDATE neev_agent_status_owner SET boot_id='3000000000000000000000000000000G'",
        "UPDATE neev_agent_status_owner SET phase=X'7265616479'",
        "UPDATE neev_agent_status_owner SET phase='unknown'",
        "UPDATE neev_agent_status_owner SET phase='initializing'",
        "UPDATE neev_agent_status_owner SET phase='closed'",
    ] { assert_imported_corruption(sql).await; }
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_agent_status_checks", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable)
    );
    let db = h::database().await;
    execute(&db, "UPDATE neev_schema_version SET version=6", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(schema_version(&db).await, 6);
}

#[tokio::test]
async fn sqlite_status_schema_rejects_malformed_checks_duplicates_capacity_and_orphans() {
    for sql in [
        "UPDATE neev_agent_status_checks SET connection_id='1000000000000000000000000000000G'",
        "UPDATE neev_agent_status_checks SET check_id='2000000000000000000000000000000G'",
        "UPDATE neev_agent_status_checks SET owner_epoch=0",
        "UPDATE neev_agent_status_checks SET owner_epoch=2",
        "UPDATE neev_agent_status_checks SET owner_epoch='1'",
        "UPDATE neev_agent_status_checks SET session_id='50000000-0000-1000-8000-000000000005'",
        "UPDATE neev_agent_status_checks SET session_id='50000000-0000-4000-C000-000000000005'",
        "UPDATE neev_agent_status_checks SET session_id=X'35303030303030302d303030302d343030302d383030302d303030303030303030303035'",
        "UPDATE neev_agent_status_checks SET generation=0",
        "UPDATE neev_agent_status_checks SET generation=1.5",
        "UPDATE neev_agent_status_checks SET storage_handle='4000000000000000000000000000000G'",
        "UPDATE neev_agent_status_checks SET issued_at=-1,expires_at=59",
        "UPDATE neev_agent_status_checks SET issued_at='0'",
        "UPDATE neev_agent_status_checks SET issued_at=253402300740,expires_at=253402300800",
        "UPDATE neev_agent_status_checks SET expires_at=60.5",
        "UPDATE neev_agent_status_checks SET expires_at=61",
        "INSERT INTO neev_agent_status_checks SELECT connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at FROM neev_agent_status_checks",
        "INSERT INTO neev_agent_status_checks SELECT '10000000000000000000000000000002',check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at FROM neev_agent_status_checks",
        "UPDATE neev_agent_status_checks SET connection_id='90000000000000000000000000000009'",
    ] { assert_imported_corruption(sql).await; }
    let db = h::database().await;
    rebuild_unconstrained_status(&db).await;
    for index in 0..65 {
        execute(
            &db,
            "INSERT INTO neev_agent_status_checks VALUES($1,$2,1,$3,1,$4,0,60)",
            vec![
                format!("{index:032x}").into(),
                format!("{:032x}", index + 100).into(),
                SESSION_ID.into(),
                STORAGE_HANDLE.into(),
            ],
        )
        .await
        .unwrap();
    }
    assert_eq!(
        agent_status_migration::validate(&db).await,
        Err(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn sqlite_status_schema_accepts_intrinsically_valid_stale_observation() {
    let db = h::database().await;
    seed_connection(&db, "disconnected").await;
    execute(
        &db,
        "UPDATE neev_agent_status_owner SET epoch=1,boot_id=$1,phase='ready' WHERE id=1",
        vec![BOOT_ID.into()],
    )
    .await
    .unwrap();
    insert_valid_check(&db).await;
    assert_eq!(agent_status_migration::validate(&db).await, Ok(()));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(mention_migration::SCHEMA_VERSION)
    );
}

#[tokio::test]
async fn sqlite_status_schema_sql_constraints_reject_invalid_rows() {
    let db = h::database().await;
    seed_connection(&db, "disconnected").await;
    for sql in [
        "UPDATE neev_agent_status_owner SET epoch=1,boot_id='BAD',phase='ready'",
        "INSERT INTO neev_agent_status_checks VALUES('bad','bad',0,'bad',0,'bad',-1,0)",
    ] {
        assert!(execute(&db, sql, vec![]).await.is_err(), "accepted {sql}");
    }
    assert_eq!(owner(&db).await, Some((1, 0, None, "closed".to_owned())));
    assert_eq!(h::count(&db, "neev_agent_status_checks").await, 0);
}

#[tokio::test]
async fn sqlite_status_schema_connection_primary_key_rejects_null() {
    let db = h::database().await;
    execute(
        &db,
        "UPDATE neev_agent_status_owner SET epoch=1,boot_id=$1,phase='ready' WHERE id=1",
        vec![BOOT_ID.into()],
    )
    .await
    .unwrap();
    let result = execute(
        &db,
        "INSERT INTO neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at) VALUES(NULL,$1,1,$2,1,$3,0,60)",
        vec![CHECK_ID.into(), SESSION_ID.into(), STORAGE_HANDLE.into()],
    )
    .await;
    assert!(
        result.is_err(),
        "SQLite must reject a NULL text primary key"
    );
    assert_eq!(h::count(&db, "neev_agent_status_checks").await, 0);
}
