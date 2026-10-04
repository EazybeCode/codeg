//! Schema 23: the project/folder trash ledger. Fresh install, 22 -> 23
//! preserving every row, idempotent restart, refusal of incoherent ledger rows,
//! and the guarded 23 -> 22 downgrade that refuses while anything is in the trash.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const DOC: &str = "33333333-3333-4333-8333-333333333333";
const SESSION: &str = "55555555-5555-4555-8555-555555555555";

async fn version(db: &DatabaseConnection) -> i64 {
    one(db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "version")
        .unwrap()
}

async fn has_table(db: &DatabaseConnection) -> bool {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS x FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname='neev_node_trash'",
        _ => "SELECT 1 AS x FROM sqlite_master WHERE type='table' AND name='neev_node_trash'",
    };
    one(db, sql, vec![]).await.unwrap().is_some()
}

async fn catalog(db: &DatabaseConnection) -> Vec<String> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT c.relname || ':' || c.relkind::text AS item FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() UNION ALL SELECT t.relname || ':' || k.conname || ':' || pg_get_constraintdef(k.oid) AS item FROM pg_constraint k JOIN pg_class t ON t.oid=k.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() ORDER BY item",
        _ => "SELECT type || ':' || name || ':' || tbl_name || ':' || coalesce(sql,'') AS item FROM sqlite_master ORDER BY name",
    };
    db.query_all(statement(db, sql, vec![]))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "item").unwrap())
        .collect()
}

async fn rows(db: &DatabaseConnection) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT CAST(id AS text) || '|' || name || '|' || CAST(revision AS text) AS line FROM neev_nodes ORDER BY id",
        "SELECT id || '|' || visibility || '|' || CAST(archived AS text) || '|' || CAST(auth_revision AS text) AS line FROM neev_resources ORDER BY id",
    ] {
        for row in db.query_all(statement(db, sql, vec![])).await.unwrap() {
            out.push(row.try_get::<String>("", "line").unwrap());
        }
    }
    out
}

/// An org with a project, a folder and an archived document in the folder.
async fn seed(db: &DatabaseConnection) -> (i32, i32, i32, i32) {
    let user = h::native_account(db, "trash-owner", "trash@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let project: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','P',1,$2) RETURNING id", vec![org.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    let folder: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'folder','F',1,$3) RETURNING id", vec![org.into(), project.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',$3,$4,'Doc','organization',NULL,NULL,2,0,true,1000,1000)", vec![org.into(), DOC.into(), folder.into(), user.into()]).await.unwrap();
    (org, user, project, folder)
}

async fn trash_folder(db: &DatabaseConnection, org: i32, user: i32, folder: i32) -> Result<(), AuthError> {
    for (kind, id) in [("node", folder.to_string()), ("resource", DOC.to_string())] {
        execute(db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b1',$2,$3,$4,$5,1000)", vec![org.into(), kind.into(), id.into(), folder.into(), user.into()]).await?;
    }
    Ok(())
}

async fn fresh_is_twenty_three(db: &DatabaseConnection) {
    assert_eq!(CURRENT_SCHEMA_VERSION, 23);
    assert_eq!(version(db).await, node_trash_migration::SCHEMA_VERSION);
    assert!(has_table(db).await);
    assert_eq!(node_trash_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(23));
    // The older operator step refuses a schema-23 database.
    assert_eq!(
        downgrade_knowledge(db).await,
        Ok(knowledge_migration::Downgrade::NotAtTwentyTwo(23))
    );
    let (org, user, _, folder) = seed(db).await;
    trash_folder(db, org, user, folder).await.unwrap();
    assert_eq!(validate_content_snapshot(db).await, Ok(23));
    // One batch per item: the same node cannot be in two batches.
    assert!(execute(db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b2','node',$2,$3,$4,1000)", vec![org.into(), folder.to_string().into(), folder.into(), user.into()]).await.is_err());
    // The kind and time checks hold.
    assert!(execute(db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b3','attachment','x',$2,$3,1000)", vec![org.into(), folder.into(), user.into()]).await.is_err());
    assert!(execute(db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b4','resource','y',$2,$3,-1)", vec![org.into(), folder.into(), user.into()]).await.is_err());
    // An Agent Chat session the batch archived is a valid ledger item.
    execute(db, "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'S','claude','default','private','chat',NULL,NULL,$4,true,2,0,0)", vec![org.into(), SESSION.into(), user.into(), folder.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b1','session',$2,$3,$4,1000)", vec![org.into(), SESSION.into(), folder.into(), user.into()]).await.unwrap();
    assert_eq!(node_trash_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(23));
}

/// 22 -> 23 changes no existing row; restart at 23 is a no-op.
async fn twenty_two_upgrades_preserving_rows(db: &DatabaseConnection) {
    assert_eq!(downgrade_node_trash(db).await, Ok(node_trash_migration::Downgrade::Done));
    assert_eq!(version(db).await, 22);
    assert!(!has_table(db).await);
    assert_eq!(validate_content_snapshot(db).await, Ok(22));
    seed(db).await;
    let before = rows(db).await;
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, 23);
    assert!(has_table(db).await);
    assert_eq!(rows(db).await, before, "the migration rewrote an existing row");
    let once = catalog(db).await;
    initialize(db).await.unwrap();
    assert_eq!(rows(db).await, before);
    assert_eq!(catalog(db).await, once, "restart changed the catalog");
}

/// Anything in the trash refuses the downgrade, which then changes nothing;
/// an empty trash downgrades to exactly the schema-22 catalog.
async fn downgrade_is_guarded(db: &DatabaseConnection) {
    let fresh = catalog(db).await;
    let expected: Vec<String> = fresh
        .iter()
        .filter(|item| !item.contains("neev_node_trash"))
        .cloned()
        .collect();
    let (org, user, _, folder) = seed(db).await;
    trash_folder(db, org, user, folder).await.unwrap();
    let before = rows(db).await;
    assert_eq!(
        downgrade_node_trash(db).await,
        Ok(node_trash_migration::Downgrade::TrashExists { rows: 2 })
    );
    assert_eq!(version(db).await, 23);
    assert_eq!(h::count(db, "neev_node_trash").await, 2);
    assert_eq!(rows(db).await, before);
    execute(db, "DELETE FROM neev_node_trash", vec![]).await.unwrap();
    assert_eq!(downgrade_node_trash(db).await, Ok(node_trash_migration::Downgrade::Done));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    assert_eq!(
        downgrade_node_trash(db).await,
        Ok(node_trash_migration::Downgrade::NotAtTwentyThree(22))
    );
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, 23);
    assert_eq!(catalog(db).await, fresh);
}

#[tokio::test]
async fn fresh_install_is_schema_23_with_the_trash_ledger() {
    let db = h::database().await;
    fresh_is_twenty_three(&db).await;
}

#[tokio::test]
async fn upgrade_from_22_keeps_rows_and_restart_is_idempotent() {
    let db = h::database().await;
    twenty_two_upgrades_preserving_rows(&db).await;
}

#[tokio::test]
async fn downgrade_refuses_while_anything_is_in_the_trash() {
    let db = h::database().await;
    downgrade_is_guarded(&db).await;
}

#[tokio::test]
async fn an_incoherent_ledger_is_refused() {
    // A resource row naming a resource that does not exist in that org.
    let db = h::database().await;
    let (org, user, _, folder) = seed(&db).await;
    trash_folder(&db, org, user, folder).await.unwrap();
    execute(&db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b1','resource','44444444-4444-4444-8444-444444444444',$2,$3,1000)", vec![org.into(), folder.into(), user.into()]).await.unwrap();
    assert_eq!(node_trash_migration::validate(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));

    // A session row naming a session that does not exist in that org.
    let db = h::database().await;
    let (org, user, _, folder) = seed(&db).await;
    trash_folder(&db, org, user, folder).await.unwrap();
    execute(&db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b1','session',$2,$3,$4,1000)", vec![org.into(), SESSION.into(), folder.into(), user.into()]).await.unwrap();
    assert_eq!(node_trash_migration::validate(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));

    // A batch without its root row.
    let db = h::database().await;
    let (org, user, project, _) = seed(&db).await;
    execute(&db, "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'b9','resource',$2,$3,$4,1000)", vec![org.into(), DOC.into(), project.into(), user.into()]).await.unwrap();
    assert_eq!(node_trash_migration::validate(&db).await, Err(AuthError::Unavailable));

    // A look-alike table without its kind CHECK.
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_node_trash", vec![]).await.unwrap();
    execute(&db, "CREATE TABLE neev_node_trash (org_id integer NOT NULL, batch_id text NOT NULL, item_kind text NOT NULL, item_id text NOT NULL, root_node_id integer NOT NULL, trashed_by integer NOT NULL, trashed_at bigint NOT NULL, CONSTRAINT neev_node_trash_pk PRIMARY KEY (org_id, item_kind, item_id))", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    assert_eq!(downgrade_node_trash(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_install_is_schema_23() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_is_twenty_three(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_upgrade_from_22_keeps_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    twenty_two_upgrades_preserving_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_anything_is_in_the_trash() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_is_guarded(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}
