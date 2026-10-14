//! Schema 22: org knowledge tables. Fresh install, 21 -> 22 preserving every
//! row, idempotent restart, the Amendment-1 constraints, refusal of partial or
//! tampered shapes, and the guarded 22 -> 21 downgrade that refuses while ANY
//! knowledge row exists and otherwise restores the exact schema-21 catalog.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const RESOURCE: &str = "11111111-1111-4111-8111-111111111111";

async fn version(db: &DatabaseConnection) -> i64 {
    one(db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "version")
        .unwrap()
}

async fn table_count(db: &DatabaseConnection) -> usize {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS x FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1",
        _ => "SELECT 1 AS x FROM sqlite_master WHERE type='table' AND name=$1",
    };
    let mut n = 0;
    for table in knowledge_migration::TABLE_NAMES {
        if one(db, sql, vec![(*table).into()]).await.unwrap().is_some() {
            n += 1;
        }
    }
    n
}

/// Every object of the catalog as text (constraint names and definitions included).
async fn catalog(db: &DatabaseConnection) -> Vec<String> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT c.relname || ':' || c.relkind::text AS item FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() UNION ALL SELECT t.relname || ':' || k.conname || ':' || pg_get_constraintdef(k.oid) AS item FROM pg_constraint k JOIN pg_class t ON t.oid=k.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() UNION ALL SELECT i.tablename || ':' || i.indexdef AS item FROM pg_indexes i WHERE i.schemaname=current_schema() ORDER BY item",
        _ => "SELECT type || ':' || name || ':' || tbl_name || ':' || coalesce(sql,'') AS item FROM sqlite_master ORDER BY name",
    };
    db.query_all(statement(db, sql, vec![]))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "item").unwrap())
        .collect()
}

/// Every existing (pre-22) row of the tables a fixture touches, as text.
async fn rows(db: &DatabaseConnection) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT CAST(id AS text) || '|' || display_name AS line FROM neev_accounts ORDER BY id",
        "SELECT CAST(id AS text) || '|' || name || '|' || CAST(revision AS text) AS line FROM neev_organizations ORDER BY id",
        "SELECT CAST(id AS text) || '|' || CAST(org_id AS text) || '|' || name || '|' || CAST(revision AS text) AS line FROM neev_nodes ORDER BY id",
        "SELECT id || '|' || visibility || '|' || CAST(auth_revision AS text) || '|' || CAST(updated_at AS text) AS line FROM neev_resources ORDER BY id",
        "SELECT CAST(node_id AS text) || '|' || default_visibility AS line FROM neev_node_sharing_defaults ORDER BY node_id",
    ] {
        for row in db.query_all(statement(db, sql, vec![])).await.unwrap() {
            out.push(row.try_get::<String>("", "line").unwrap());
        }
    }
    out
}

/// A schema-21-era org: account, org, project, a document, a sharing default.
async fn seed(db: &DatabaseConnection) -> (i32, i32) {
    let user = h::native_account(db, "knowledge-owner", "knowledge@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let node: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Kept',2,$2) RETURNING id", vec![org.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',$3,$4,'Doc','organization',NULL,NULL,3,0,false,1000,1000)", vec![org.into(), RESOURCE.into(), node.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_node_sharing_defaults(org_id,node_id,default_visibility,set_by,updated_at) VALUES($1,$2,'organization',$3,1000)", vec![org.into(), node.into(), user.into()]).await.unwrap();
    (org, user)
}

/// One row per knowledge table, parents first, in a valid chain.
fn chain(org: i32, user: i32) -> Vec<(&'static str, String, Vec<sea_orm::Value>)> {
    let o = || -> sea_orm::Value { org.into() };
    let u = || -> sea_orm::Value { user.into() };
    vec![
        ("neev_knowledge_settings", "INSERT INTO neev_knowledge_settings(org_id,capture_enabled,capture_blocked,revision,updated_by,updated_at) VALUES($1,true,NULL,1,$2,1)".into(), vec![o(), u()]),
        ("neev_knowledge_sources", "INSERT INTO neev_knowledge_sources(org_id,source_id,state,detail,marker,checked_at) VALUES($1,$2,'ok','',NULL,1)".into(), vec![o(), RESOURCE.into()]),
        ("neev_knowledge_snapshots", "INSERT INTO neev_knowledge_snapshots(org_id,id,source_id,source_kind,engine,segment,from_sequence,to_sequence,marker,blob_key,sha256,byte_length,state,captured_at,cite_key) VALUES($1,'s1',$2,'document',false,0,NULL,NULL,'m','k','h',10,'current',1,'0123456789ab')".into(), vec![o(), RESOURCE.into()]),
        ("neev_knowledge_coverage", "INSERT INTO neev_knowledge_coverage(org_id,snapshot_id,state,attempts,updated_at) VALUES($1,'s1','pending',0,1)".into(), vec![o()]),
        ("neev_knowledge_pages", "INSERT INTO neev_knowledge_pages(org_id,id,slug,title,head_revision,status,created_at,updated_at) VALUES($1,'p1','slug','Title',1,'active',1,1)".into(), vec![o()]),
        ("neev_knowledge_jobs", "INSERT INTO neev_knowledge_jobs(org_id,id,mode,state,started_by,provider,model,pause_reason,session_id,wait_reason,created_at,updated_at) VALUES($1,'j1','incremental','completed',$2,'claude','m',NULL,NULL,NULL,1,1)".into(), vec![o(), u()]),
        ("neev_knowledge_batches", "INSERT INTO neev_knowledge_batches(org_id,id,job_id,seq,state,turn_id,reason,provider,model,instructions_sha256,created_at,started_at,settled_at) VALUES($1,'b1','j1',1,'accepted',NULL,NULL,'claude','m','sha',1,1,2)".into(), vec![o()]),
        ("neev_knowledge_revisions", "INSERT INTO neev_knowledge_revisions(org_id,page_id,revision,blob_key,sha256,byte_length,parent_revision,batch_id,created_at) VALUES($1,'p1',1,'bk','h',5,NULL,'b1',1)".into(), vec![o()]),
        ("neev_knowledge_revision_deps", "INSERT INTO neev_knowledge_revision_deps(org_id,page_id,revision,snapshot_id) VALUES($1,'p1',1,'s1')".into(), vec![o()]),
        ("neev_knowledge_batch_units", "INSERT INTO neev_knowledge_batch_units(org_id,batch_id,label,snapshot_id,skip_reason) VALUES($1,'b1','u01','s1',NULL)".into(), vec![o()]),
        ("neev_knowledge_batch_pages", "INSERT INTO neev_knowledge_batch_pages(org_id,batch_id,page_id,revision,role) VALUES($1,'b1','p1',1,'staged')".into(), vec![o()]),
        ("neev_knowledge_log", "INSERT INTO neev_knowledge_log(org_id,seq,at,actor_id,event,page_id,job_id,detail) VALUES($1,1,1,$2,'batch_accepted','p1','j1','pages=1 cited=1 skipped=0')".into(), vec![o(), u()]),
    ]
}

async fn insert_chain(db: &DatabaseConnection, org: i32, user: i32) {
    for (table, sql, values) in chain(org, user) {
        execute(db, &sql, values).await.unwrap_or_else(|e| panic!("{table}: {e:?}"));
    }
}

/// A fresh install is schema 24; these tests are about the 22 layer, so take
/// the (empty) schema-24 sharing tables and schema-23 trash ledger off first.
async fn at_twenty_two(db: &DatabaseConnection) {
    assert_eq!(downgrade_msteams(db).await, Ok(msteams_migration::Downgrade::Done));
    assert_eq!(downgrade_member_sharing(db).await, Ok(member_sharing_migration::Downgrade::Done));
    assert_eq!(downgrade_node_trash(db).await, Ok(node_trash_migration::Downgrade::Done));
    assert_eq!(version(db).await, knowledge_migration::SCHEMA_VERSION);
}

async fn fresh_is_twenty_two(db: &DatabaseConnection) {
    assert_eq!(CURRENT_SCHEMA_VERSION, msteams_migration::SCHEMA_VERSION);
    at_twenty_two(db).await;
    assert_eq!(version(db).await, knowledge_migration::SCHEMA_VERSION);
    assert_eq!(knowledge_migration::TABLE_NAMES.len(), 12);
    assert_eq!(table_count(db).await, knowledge_migration::TABLE_NAMES.len());
    assert_eq!(knowledge_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(22));
    // The existing 21 -> 20 operator step refuses a schema-22 database.
    assert_eq!(
        downgrade_node_sharing(db).await,
        Ok(node_sharing_migration::Downgrade::NotAtTwentyOne(22))
    );
    // A full valid chain is storable.
    let (org, user) = seed(db).await;
    insert_chain(db, org, user).await;
    assert_eq!(validate_content_snapshot(db).await, Ok(22));
}

/// 21 -> 22 changes no existing row; restart at 22 is a no-op.
async fn twenty_one_upgrades_preserving_rows(db: &DatabaseConnection) {
    at_twenty_two(db).await;
    seed(db).await;
    assert_eq!(downgrade_knowledge(db).await, Ok(knowledge_migration::Downgrade::Downgraded));
    assert_eq!(version(db).await, 21);
    assert_eq!(table_count(db).await, 0);
    assert_eq!(validate_content_snapshot(db).await, Ok(21));
    let before = rows(db).await;
    assert_eq!(before.len(), 5);

    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(table_count(db).await, 12);
    assert_eq!(rows(db).await, before, "the migration rewrote an existing row");
    let once = catalog(db).await;
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(rows(db).await, before);
    assert_eq!(catalog(db).await, once, "restart changed the catalog");
}

/// Amendment-1 constraints: one active job per org, the state/role/coverage
/// enums, cite keys, the byte caps and tenant-carrying foreign keys.
async fn constraints_hold(db: &DatabaseConnection) {
    let (org, user) = seed(db).await;
    let job = "INSERT INTO neev_knowledge_jobs(org_id,id,mode,state,started_by,provider,model,pause_reason,session_id,wait_reason,created_at,updated_at) VALUES($1,$2,'incremental',$3,$4,'claude','m',NULL,NULL,$5,1,1)";
    let add_job = |id: &'static str, state: &'static str, wait: Option<&'static str>| {
        execute(db, job, vec![org.into(), id.into(), state.into(), user.into(), wait.map(str::to_owned).into()])
    };
    add_job("j-running", "running", Some("slot")).await.unwrap();
    assert!(add_job("j-paused", "paused", None).await.is_err(), "second active job");
    add_job("j-done", "completed", None).await.unwrap();
    add_job("j-gaps", "completed_with_gaps", None).await.unwrap();
    add_job("j-stopped", "stopped", None).await.unwrap();
    assert!(add_job("j-bad", "failed", None).await.is_err());
    assert!(add_job("j-wait", "completed", Some("lane")).await.is_err());

    let settings = "INSERT INTO neev_knowledge_settings(org_id,capture_enabled,capture_blocked,revision,updated_by,updated_at) VALUES($1,false,$2,$3,$4,1)";
    assert!(execute(db, settings, vec![org.into(), Some("full".to_owned()).into(), 1.into(), user.into()]).await.is_err());
    assert!(execute(db, settings, vec![org.into(), None::<String>.into(), 0.into(), user.into()]).await.is_err());
    execute(db, settings, vec![org.into(), Some("quota_exceeded".to_owned()).into(), 1.into(), user.into()]).await.unwrap();

    let source = "INSERT INTO neev_knowledge_sources(org_id,source_id,state,detail,marker,checked_at) VALUES($1,$2,$3,'',NULL,1)";
    assert!(execute(db, source, vec![org.into(), RESOURCE.into(), "stale".into()]).await.is_err());
    execute(db, source, vec![org.into(), RESOURCE.into(), "ok".into()]).await.unwrap();
    let defaults: (i32, i64, i32) = {
        let row = one(db, "SELECT pending_seen,next_check_at,failures FROM neev_knowledge_sources WHERE org_id=$1", vec![org.into()]).await.unwrap().unwrap();
        (row.try_get("", "pending_seen").unwrap(), row.try_get("", "next_check_at").unwrap(), row.try_get("", "failures").unwrap())
    };
    assert_eq!(defaults, (0, 0, 0));
    // The resource FK carries the org: another tenant cannot name our resource.
    let other = h::native_account(db, "knowledge-other", "other@example.test").await;
    let (other_org, _) = h::organization(db, other, "admin").await;
    assert!(execute(db, source, vec![other_org.into(), RESOURCE.into(), "ok".into()]).await.is_err());

    let snapshot = "INSERT INTO neev_knowledge_snapshots(org_id,id,source_id,source_kind,engine,segment,from_sequence,to_sequence,marker,blob_key,sha256,byte_length,state,captured_at,cite_key) VALUES($1,$2,$3,'document',false,$4,NULL,NULL,'m','k','h',$5,$6,1,$7)";
    let add_snapshot = |id: &'static str, segment: i32, bytes: i64, state: &'static str, cite: &'static str| {
        execute(db, snapshot, vec![org.into(), id.into(), RESOURCE.into(), segment.into(), bytes.into(), state.into(), cite.into()])
    };
    add_snapshot("s1", 0, 65_536, "current", "aaaaaaaaaaaa").await.unwrap();
    assert!(add_snapshot("s2", 0, 10, "current", "bbbbbbbbbbbb").await.is_err(), "two current snapshots of one segment");
    add_snapshot("s3", 0, 10, "superseded", "cccccccccccc").await.unwrap();
    assert!(add_snapshot("s4", 1, 65_537, "current", "dddddddddddd").await.is_err(), "over 64 KiB");
    assert!(add_snapshot("s5", 1, 10, "current", "aaaaaaaaaaaa").await.is_err(), "duplicate cite key");
    assert!(add_snapshot("s6", 1, 10, "current", "short").await.is_err(), "cite key length");

    let coverage = "INSERT INTO neev_knowledge_coverage(org_id,snapshot_id,state,attempts,updated_at) VALUES($1,$2,$3,0,1)";
    assert!(execute(db, coverage, vec![org.into(), "s1".into(), "done".into()]).await.is_err());
    assert!(execute(db, coverage, vec![org.into(), "missing".into(), "pending".into()]).await.is_err(), "coverage FK");
    for (id, state) in [("s1", "skipped_by_agent"), ("s3", "uncovered")] {
        execute(db, coverage, vec![org.into(), id.into(), state.into()]).await.unwrap();
    }

    let mut rest = chain(org, user);
    rest.retain(|(table, _, _)| {
        matches!(*table, "neev_knowledge_pages" | "neev_knowledge_batches" | "neev_knowledge_revisions")
    });
    // The chain's batch names job 'j1'.
    add_job("j1", "stopped", None).await.unwrap();
    for (table, sql, values) in rest {
        execute(db, &sql, values).await.unwrap_or_else(|e| panic!("{table}: {e:?}"));
    }
    let page = "INSERT INTO neev_knowledge_batch_pages(org_id,batch_id,page_id,revision,role) VALUES($1,'b1','p1',1,$2)";
    assert!(execute(db, page, vec![org.into(), "reviewed".into()]).await.is_err());
    // Review M1: a batch page must name an existing batch of the same org.
    assert!(
        execute(db, "INSERT INTO neev_knowledge_batch_pages(org_id,batch_id,page_id,revision,role) VALUES($1,'b-missing','p1',1,'staged')", vec![org.into()]).await.is_err(),
        "batch_pages without its batch"
    );
    execute(db, page, vec![org.into(), "indexed".into()]).await.unwrap();
    let revision = "INSERT INTO neev_knowledge_revisions(org_id,page_id,revision,blob_key,sha256,byte_length,parent_revision,batch_id,created_at) VALUES($1,'p1',2,'bk','h',$2,1,'b1',1)";
    assert!(execute(db, revision, vec![org.into(), 65_537_i64.into()]).await.is_err());
    assert!(execute(db, revision, vec![org.into(), 0_i64.into()]).await.is_err());
    let log = "INSERT INTO neev_knowledge_log(org_id,seq,at,actor_id,event,page_id,job_id,detail) VALUES($1,1,1,NULL,'e',NULL,NULL,$2)";
    assert!(execute(db, log, vec![org.into(), "x".repeat(501).into()]).await.is_err());
    execute(db, log, vec![org.into(), "x".repeat(500).into()]).await.unwrap();
}

/// The downgrade restores exactly the fresh catalog minus the twelve tables,
/// preserves every row, is guarded on the marker, and round-trips.
async fn downgrade_restores_schema_twenty_one(db: &DatabaseConnection) {
    at_twenty_two(db).await;
    let fresh = catalog(db).await;
    let expected: Vec<String> = fresh
        .iter()
        .filter(|item| !item.contains("neev_knowledge_"))
        .cloned()
        .collect();
    assert!(expected.len() + 12 <= fresh.len());
    seed(db).await;
    let before = rows(db).await;
    assert_eq!(downgrade_knowledge(db).await, Ok(knowledge_migration::Downgrade::Downgraded));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    assert_eq!(version(db).await, 21);
    assert_eq!(
        downgrade_knowledge(db).await,
        Ok(knowledge_migration::Downgrade::NotAtTwentyTwo(21))
    );
    // The schema-21 database is valid as itself and re-migrates; a second round
    // trip lands on the same catalogs.
    initialize(db).await.unwrap();
    at_twenty_two(db).await;
    assert_eq!(catalog(db).await, fresh);
    assert_eq!(downgrade_knowledge(db).await, Ok(knowledge_migration::Downgrade::Downgraded));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
}

/// ANY knowledge row refuses the downgrade, which then changes nothing. Rows
/// are removed child-first; every table is in turn the only populated leaf,
/// and only the empty set downgrades.
async fn downgrade_refuses_with_any_row(db: &DatabaseConnection) {
    at_twenty_two(db).await;
    let (org, user) = seed(db).await;
    insert_chain(db, org, user).await;
    let catalog_before = catalog(db).await;
    let mut populated: Vec<&str> = chain(org, user).iter().map(|(table, _, _)| *table).collect();
    while let Some(leaf) = populated.pop() {
        assert_eq!(
            downgrade_knowledge(db).await,
            Ok(knowledge_migration::Downgrade::Refused),
            "refused while {leaf} still has a row"
        );
        assert_eq!(version(db).await, 22);
        assert_eq!(catalog(db).await, catalog_before);
        execute(db, &format!("DELETE FROM {leaf}"), vec![]).await.unwrap();
    }
    assert_eq!(downgrade_knowledge(db).await, Ok(knowledge_migration::Downgrade::Downgraded));
}

#[tokio::test]
async fn fresh_install_is_schema_22_with_all_knowledge_tables() {
    let db = h::database().await;
    fresh_is_twenty_two(&db).await;
}

#[tokio::test]
async fn upgrade_from_21_keeps_rows_and_restart_is_idempotent() {
    let db = h::database().await;
    twenty_one_upgrades_preserving_rows(&db).await;
}

#[tokio::test]
async fn amendment_constraints_are_enforced() {
    let db = h::database().await;
    constraints_hold(&db).await;
}

#[tokio::test]
async fn downgrade_restores_the_exact_schema_21_catalog() {
    let db = h::database().await;
    downgrade_restores_schema_twenty_one(&db).await;
    let integrity: String = one(&db, "PRAGMA integrity_check", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "integrity_check")
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(one(&db, "PRAGMA foreign_key_check", vec![]).await.unwrap().is_none());
}

#[tokio::test]
async fn downgrade_refuses_while_any_knowledge_row_exists() {
    let db = h::database().await;
    downgrade_refuses_with_any_row(&db).await;
}

#[tokio::test]
async fn a_lone_row_in_any_single_table_refuses_the_downgrade() {
    // SQLite only: foreign keys off so each table can hold a row on its own.
    for index in 0..knowledge_migration::TABLE_NAMES.len() {
        let db = h::database().await;
        at_twenty_two(&db).await;
        let (org, user) = seed(&db).await;
        let (table, sql, values) = chain(org, user).remove(index);
        execute(&db, "PRAGMA foreign_keys=OFF", vec![]).await.unwrap();
        execute(&db, &sql, values).await.unwrap();
        execute(&db, "PRAGMA foreign_keys=ON", vec![]).await.unwrap();
        assert_eq!(
            downgrade_knowledge(&db).await,
            Ok(knowledge_migration::Downgrade::Refused),
            "{table}"
        );
        assert_eq!(table_count(&db).await, 12, "{table}");
    }
}

#[tokio::test]
async fn a_partial_or_tampered_knowledge_schema_is_refused_without_change() {
    // A missing table at marker 22.
    let db = h::database().await;
    at_twenty_two(&db).await;
    execute(&db, "DROP TABLE neev_knowledge_log", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    assert_eq!(version(&db).await, 22);

    // Marker 21 with the knowledge tables still present.
    let db = h::database().await;
    at_twenty_two(&db).await;
    seed(&db).await;
    let before = rows(&db).await;
    execute(&db, "UPDATE neev_schema_version SET version=21 WHERE id=1", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(version(&db).await, 21);
    assert_eq!(rows(&db).await, before);

    // A look-alike table without its state CHECK.
    let db = h::database().await;
    at_twenty_two(&db).await;
    execute(&db, "DROP TABLE neev_knowledge_coverage", vec![]).await.unwrap();
    execute(&db, "CREATE TABLE neev_knowledge_coverage (org_id integer NOT NULL, snapshot_id text NOT NULL, state text NOT NULL, attempts integer NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_knowledge_coverage_pk PRIMARY KEY (org_id, snapshot_id))", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    // The downgrade validates before dropping anything.
    assert_eq!(downgrade_knowledge(&db).await, Err(AuthError::Unavailable));
    assert_eq!(table_count(&db).await, 12);

    // A missing partial unique index (the single-active-job guard).
    let db = h::database().await;
    execute(&db, "DROP INDEX neev_knowledge_jobs_active", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(knowledge_migration::validate(&db).await, Err(AuthError::Unavailable));

    // A stray trigger on a knowledge table.
    let db = h::database().await;
    execute(&db, "CREATE TRIGGER stray AFTER INSERT ON neev_knowledge_log BEGIN SELECT 1; END", vec![]).await.unwrap();
    assert_eq!(knowledge_migration::validate(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_install_is_schema_22() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_is_twenty_two(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_upgrade_from_21_keeps_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    twenty_one_upgrades_preserving_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_amendment_constraints_are_enforced() {
    let (db, admin, schema) = h::postgres_database().await;
    constraints_hold(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_restores_the_exact_schema_21_catalog() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_restores_schema_twenty_one(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_any_knowledge_row_exists() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_refuses_with_any_row(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_a_tampered_knowledge_table_is_refused() {
    let (db, admin, schema) = h::postgres_database().await;
    execute(&db, "ALTER TABLE neev_knowledge_jobs DROP CONSTRAINT neev_knowledge_jobs_state", vec![]).await.unwrap();
    assert_eq!(knowledge_migration::validate(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    execute(&db, "ALTER TABLE neev_knowledge_jobs ADD CONSTRAINT neev_knowledge_jobs_state CHECK (state IN ('running','paused','completed','stopped'))", vec![]).await.unwrap();
    // Same name, missing 'completed_with_gaps': still refused.
    assert_eq!(knowledge_migration::validate(&db).await, Err(AuthError::Unavailable));
    h::cleanup_postgres(db, admin, schema).await;
}

/// Review I1: same-named objects with a different bound, predicate, column list
/// or FK target are refused; the exact original is accepted again.
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_validation_compares_exact_definitions() {
    let (db, admin, schema) = h::postgres_database().await;
    let swaps: [(&str, &str, &str); 5] = [
        (
            "ALTER TABLE neev_knowledge_snapshots DROP CONSTRAINT neev_knowledge_snapshots_len",
            "ALTER TABLE neev_knowledge_snapshots ADD CONSTRAINT neev_knowledge_snapshots_len CHECK (byte_length>=0 AND byte_length<=1048576)",
            "ALTER TABLE neev_knowledge_snapshots ADD CONSTRAINT neev_knowledge_snapshots_len CHECK (byte_length>=0 AND byte_length<=65536)",
        ),
        (
            "ALTER TABLE neev_knowledge_snapshots DROP CONSTRAINT neev_knowledge_snapshots_cite_len",
            "ALTER TABLE neev_knowledge_snapshots ADD CONSTRAINT neev_knowledge_snapshots_cite_len CHECK (length(cite_key)=16)",
            "ALTER TABLE neev_knowledge_snapshots ADD CONSTRAINT neev_knowledge_snapshots_cite_len CHECK (length(cite_key)=12)",
        ),
        (
            "DROP INDEX neev_knowledge_jobs_active",
            "CREATE UNIQUE INDEX neev_knowledge_jobs_active ON neev_knowledge_jobs (org_id) WHERE state IN ('running')",
            "CREATE UNIQUE INDEX neev_knowledge_jobs_active ON neev_knowledge_jobs (org_id) WHERE state IN ('running','paused')",
        ),
        (
            "DROP INDEX neev_knowledge_sources_queue",
            "CREATE INDEX neev_knowledge_sources_queue ON neev_knowledge_sources (org_id, checked_at)",
            "CREATE INDEX neev_knowledge_sources_queue ON neev_knowledge_sources (org_id, next_check_at, checked_at)",
        ),
        (
            "ALTER TABLE neev_knowledge_batch_pages DROP CONSTRAINT neev_knowledge_batch_pages_batch",
            "ALTER TABLE neev_knowledge_batch_pages ADD CONSTRAINT neev_knowledge_batch_pages_batch FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_jobs (org_id, id)",
            "ALTER TABLE neev_knowledge_batch_pages ADD CONSTRAINT neev_knowledge_batch_pages_batch FOREIGN KEY (org_id, batch_id) REFERENCES neev_knowledge_batches (org_id, id)",
        ),
    ];
    assert_eq!(knowledge_migration::validate(&db).await, Ok(()));
    for (drop, wrong, right) in swaps {
        execute(&db, drop, vec![]).await.unwrap();
        execute(&db, wrong, vec![]).await.unwrap();
        assert_eq!(knowledge_migration::validate(&db).await, Err(AuthError::Unavailable), "{wrong}");
        assert_eq!(initialize(&db).await, Err(AuthError::Unavailable), "{wrong}");
        execute(&db, drop, vec![]).await.unwrap();
        execute(&db, right, vec![]).await.unwrap();
        assert_eq!(knowledge_migration::validate(&db).await, Ok(()), "{right}");
    }
    h::cleanup_postgres(db, admin, schema).await;
}

/// Review I2: a schema-22 writer whose row is in flight when the downgrade
/// starts must make the downgrade refuse — never let the DROP destroy the row.
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_cannot_drop_a_row_committed_during_it() {
    use sea_orm::TransactionTrait;
    let (db, admin, schema) = h::postgres_database().await;
    at_twenty_two(&db).await;
    let (org, user) = seed(&db).await;
    // Connection A: an uncommitted capture-style write.
    let writer = db.begin().await.unwrap();
    execute(&writer, "INSERT INTO neev_knowledge_settings(org_id,capture_enabled,capture_blocked,revision,updated_by,updated_at) VALUES($1,true,NULL,1,$2,1)", vec![org.into(), user.into()]).await.unwrap();
    // Connection B: the operator downgrade, which must block on the writer.
    let pool = db.clone();
    let downgrade = tokio::spawn(async move { downgrade_knowledge(&pool).await });
    h::wait_for_postgres_lock(&admin, "LOCK TABLE neev_knowledge_%").await;
    writer.commit().await.unwrap();
    assert_eq!(
        downgrade.await.unwrap(),
        Ok(knowledge_migration::Downgrade::Refused)
    );
    assert_eq!(version(&db).await, 22);
    assert_eq!(h::count(&db, "neev_knowledge_settings").await, 1);
    assert_eq!(table_count(&db).await, 12);
    h::cleanup_postgres(db, admin, schema).await;
}
