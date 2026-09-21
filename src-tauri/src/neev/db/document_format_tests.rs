use super::*;
use crate::neev::test_support as h;

// Catches missing backfill, changed immutable revision metadata, and incomplete schema6 inventories.
async fn populated_five_upgrades(database: DatabaseConnection) {
    let db = &database;
    // Reconstruct the exact v5 inventory, with its populated native revision and receipts.
    let exists = one(db, "SELECT version FROM neev_schema_version", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "version")
        .unwrap();
    assert_eq!(exists, 13);
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
    for table in agent_status_migration::TABLE_NAMES.iter().rev() {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(db, "DROP TABLE neev_document_formats", vec![])
        .await
        .unwrap();
    execute(db, "UPDATE neev_schema_version SET version=5", vec![])
        .await
        .unwrap();
    execute(db, "INSERT INTO neev_accounts(id,display_name,auth_revision,disabled,created_at) VALUES(101,'Preserved owner',1,false,1000)", vec![]).await.unwrap();
    execute(db, "INSERT INTO neev_organizations(id,owner_user_id,name,revision,disabled,created_at) VALUES(201,101,'Preserved org',1,false,1000)", vec![]).await.unwrap();
    execute(db, "INSERT INTO neev_memberships(id,org_id,user_id,role,revision,active) VALUES(301,201,101,'admin',1,true)", vec![]).await.unwrap();
    execute(db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(401,201,NULL,'project','Preserved project',1,101)", vec![]).await.unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,auth_revision,content_revision,archived,created_at,updated_at) VALUES(201,'11111111-1111-4111-8111-111111111111','document',401,101,'History','private',1,1,false,1000,1001)", vec![]).await.unwrap();
    execute(db, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at) VALUES(201,'11111111-1111-4111-8111-111111111111','document',1,'preserved-blob',$1,17,101,1001)", vec!["a".repeat(64).into()]).await.unwrap();
    initialize(db).await.unwrap();
    assert_eq!(validate_content_snapshot(db).await.unwrap(), 13);
    let format = one(
        db,
        "SELECT format,format_version FROM neev_document_formats",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(format.try_get::<String>("", "format").unwrap(), "markdown");
    assert_eq!(format.try_get::<i32>("", "format_version").unwrap(), 1);
    let row = one(
        db,
        "SELECT blob_key,sha256,byte_length FROM neev_document_revisions",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.try_get::<String>("", "blob_key").unwrap(),
        "preserved-blob"
    );
    assert_eq!(row.try_get::<String>("", "sha256").unwrap(), "a".repeat(64));
    assert_eq!(row.try_get::<i64>("", "byte_length").unwrap(), 17);
    initialize(db).await.unwrap();
    for sql in [
        "UPDATE neev_document_formats SET format='unknown'",
        "UPDATE neev_document_formats SET format_version=2",
        "UPDATE neev_document_formats SET format=NULL",
        "UPDATE neev_document_formats SET org_id=0",
        "UPDATE neev_document_formats SET revision=0",
        "INSERT INTO neev_document_formats SELECT * FROM neev_document_formats",
        "DELETE FROM neev_document_revisions",
        "UPDATE neev_document_revisions SET revision=2",
        "INSERT INTO neev_document_formats(org_id,document_id,revision,format,format_version) VALUES(201,'11111111-1111-4111-8111-111111111111',2,'blocks',1)",
    ] {assert!(execute(db,sql,vec![]).await.is_err(),"{sql}");}
    execute(db, "DELETE FROM neev_document_formats", vec![])
        .await
        .unwrap();
    assert!(initialize(db).await.is_err());
    assert!(validate_content_snapshot(db).await.is_err());
}

#[tokio::test]
async fn sqlite_document_format_populated_migration() {
    populated_five_upgrades(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_document_format_populated_migration() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("document format migration owned schema: {schema}");
    populated_five_upgrades(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
async fn sqlite_document_format_rejects_mixed_and_orphan_metadata() {
    let db = h::database().await;
    execute(&db, "UPDATE neev_schema_version SET version=5", vec![])
        .await
        .unwrap();
    assert!(initialize(&db).await.is_err());
    assert!(validate_content_snapshot(&db).await.is_err());
    execute(&db, "UPDATE neev_schema_version SET version=6", vec![])
        .await
        .unwrap();
    execute(&db, "DROP TABLE neev_document_formats", vec![])
        .await
        .unwrap();
    assert!(initialize(&db).await.is_err());
    assert!(validate_content_snapshot(&db).await.is_err());
    let db = h::database().await;
    execute(&db, "PRAGMA foreign_keys=OFF", vec![])
        .await
        .unwrap();
    execute(&db,"INSERT INTO neev_document_formats VALUES(1,'11111111-1111-4111-8111-111111111111',1,'blocks',1)",vec![]).await.unwrap();
    assert!(validate_content_snapshot(&db).await.is_err());
    assert!(initialize(&db).await.is_err());
}
