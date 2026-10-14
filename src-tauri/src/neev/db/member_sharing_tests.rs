//! Schema 24: teams and member/team sharing. Fresh install, 23 -> 24 changing
//! no existing row (and so widening no audience), idempotent restart, refusal of
//! incoherent rows, and the guarded 24 -> 23 downgrade that refuses while any
//! team, grant or shared mark exists.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const DOC: &str = "33333333-3333-4333-8333-333333333333";
const TEAM: &str = "66666666-6666-4666-8666-666666666666";

async fn version(db: &DatabaseConnection) -> i64 {
    one(db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "version")
        .unwrap()
}

async fn tables(db: &DatabaseConnection) -> usize {
    let mut n = 0;
    for table in member_sharing_migration::TABLE_NAMES {
        let sql = match db.get_database_backend() {
            DbBackend::Postgres => "SELECT 1 AS x FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1",
            _ => "SELECT 1 AS x FROM sqlite_master WHERE type='table' AND name=$1",
        };
        if one(db, sql, vec![(*table).into()]).await.unwrap().is_some() {
            n += 1;
        }
    }
    n
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

/// Every audience-bearing value an upgrade could conceivably rewrite.
async fn rows(db: &DatabaseConnection) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT id || '|' || visibility || '|' || CAST(auth_revision AS text) || '|' || coalesce(origin_chat_id,'') AS line FROM neev_resources ORDER BY id",
        "SELECT CAST(org_id AS text) || '|' || CAST(user_id AS text) || '|' || role || '|' || CAST(active AS text) AS line FROM neev_memberships ORDER BY org_id,user_id",
        "SELECT CAST(node_id AS text) || '|' || default_visibility AS line FROM neev_node_sharing_defaults ORDER BY node_id",
    ] {
        for row in db.query_all(statement(db, sql, vec![])).await.unwrap() {
            out.push(row.try_get::<String>("", "line").unwrap());
        }
    }
    out
}

/// An org with a member, a folder, a private and an organization document,
/// and an organization folder default.
async fn seed(db: &DatabaseConnection) -> (i32, i32, i32, i32) {
    let user = h::native_account(db, "share-owner", "share@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let member = h::native_account(db, "share-member", "member@example.test").await;
    h::add_membership(db, org, member, "member").await;
    let folder: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','P',1,$2) RETURNING id", vec![org.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',$3,$4,'Doc','private',NULL,NULL,1,0,false,1000,1000)", vec![org.into(), DOC.into(), folder.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,'44444444-4444-4444-8444-444444444444','document',$2,$3,'Org','organization',NULL,NULL,1,0,false,1000,1000)", vec![org.into(), folder.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_node_sharing_defaults(org_id,node_id,default_visibility,set_by,updated_at) VALUES($1,$2,'organization',$3,1000)", vec![org.into(), folder.into(), user.into()]).await.unwrap();
    (org, user, member, folder)
}

/// A team with the member, a team grant and a user grant on the folder, and a
/// shared mark on the private document.
async fn share(db: &DatabaseConnection, org: i32, user: i32, member: i32, folder: i32) {
    execute(db, "INSERT INTO neev_teams(org_id,id,name,name_key,created_by,revision,created_at,updated_at) VALUES($1,$2,'Design','design',$3,1,1000,1000)", vec![org.into(), TEAM.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_team_members(org_id,team_id,user_id,added_by,added_at) VALUES($1,$2,$3,$4,1000)", vec![org.into(), TEAM.into(), member.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'node',$2,'team',$3,'edit',$4,1000)", vec![org.into(), folder.to_string().into(), TEAM.into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'resource',$2,'user',$3,'view',$4,1000)", vec![org.into(), DOC.into(), member.to_string().into(), user.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'resource',$2,$3,1000)", vec![org.into(), DOC.into(), user.into()]).await.unwrap();
}

/// A fresh install is schema 25; these tests are about the 24 layer, so take
/// the (empty) schema-25 Teams tables off first.
async fn at_twenty_four(db: &DatabaseConnection) {
    assert_eq!(downgrade_msteams(db).await, Ok(msteams_migration::Downgrade::Done));
    assert_eq!(version(db).await, member_sharing_migration::SCHEMA_VERSION);
}

async fn fresh_is_twenty_four(db: &DatabaseConnection) {
    assert_eq!(CURRENT_SCHEMA_VERSION, 25);
    at_twenty_four(db).await;
    assert_eq!(version(db).await, member_sharing_migration::SCHEMA_VERSION);
    assert_eq!(tables(db).await, 5);
    assert_eq!(member_sharing_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(24));
    // The older operator step refuses a schema-24 database.
    assert_eq!(
        downgrade_node_trash(db).await,
        Ok(node_trash_migration::Downgrade::NotAtTwentyThree(24))
    );
    let (org, user, member, folder) = seed(db).await;
    share(db, org, user, member, folder).await;
    assert_eq!(member_sharing_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(24));
    // The CHECKs hold: an edit grant on a session, an unknown kind or level,
    // a duplicate team name (case-folded key), an empty name.
    for sql in [
        "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'session','s','user','1','edit',$2,1)",
        "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'workspace','s','user','1','view',$2,1)",
        "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'node','1','group','1','view',$2,1)",
        "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'node','1','user','1','owner',$2,1)",
        "INSERT INTO neev_teams(org_id,id,name,name_key,created_by,revision,created_at,updated_at) VALUES($1,'t2','DESIGN','design',$2,1,1,1)",
        "INSERT INTO neev_teams(org_id,id,name,name_key,created_by,revision,created_at,updated_at) VALUES($1,'t3','','',$2,1,1,1)",
        "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'session','x',$2,1)",
    ] {
        assert!(execute(db, sql, vec![org.into(), user.into()]).await.is_err(), "{sql}");
    }
    // A View grant on a session is the one session shape.
    execute(db, "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'session','s','user','1','view',$2,1)", vec![org.into(), user.into()]).await.unwrap();
    // Deleting a team takes its members with it.
    execute(db, "DELETE FROM neev_grants WHERE principal_kind='team'", vec![]).await.unwrap();
    execute(db, "DELETE FROM neev_teams", vec![]).await.unwrap();
    assert_eq!(h::count(db, "neev_team_members").await, 0);
}

/// 23 -> 24 changes no existing row, so no audience widens; restart at 24 is a
/// no-op.
async fn twenty_three_upgrades_preserving_rows(db: &DatabaseConnection) {
    at_twenty_four(db).await;
    assert_eq!(downgrade_member_sharing(db).await, Ok(member_sharing_migration::Downgrade::Done));
    assert_eq!(version(db).await, 23);
    assert_eq!(tables(db).await, 0);
    assert_eq!(validate_content_snapshot(db).await, Ok(23));
    seed(db).await;
    let before = rows(db).await;
    assert_eq!(before.len(), 5);
    initialize(db).await.unwrap();
    // The upgrade runs through to the current schema (25).
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(tables(db).await, 5);
    assert_eq!(rows(db).await, before, "the migration rewrote an existing row");
    for table in member_sharing_migration::TABLE_NAMES {
        assert_eq!(h::count(db, table).await, 0, "{table} was backfilled");
    }
    let once = catalog(db).await;
    initialize(db).await.unwrap();
    assert_eq!(rows(db).await, before);
    assert_eq!(catalog(db).await, once, "restart changed the catalog");
}

/// Any team, grant or mark refuses the downgrade, which then changes nothing;
/// with none it lands on exactly the schema-23 catalog, and re-migrates.
async fn downgrade_is_guarded(db: &DatabaseConnection) {
    at_twenty_four(db).await;
    let fresh = catalog(db).await;
    let expected: Vec<String> = fresh
        .iter()
        .filter(|item| {
            !member_sharing_migration::TABLE_NAMES
                .iter()
                .any(|table| item.contains(table))
        })
        .cloned()
        .collect();
    let (org, user, member, folder) = seed(db).await;
    share(db, org, user, member, folder).await;
    let before = rows(db).await;
    assert_eq!(
        downgrade_member_sharing(db).await,
        Ok(member_sharing_migration::Downgrade::SharingExists { rows: 4 })
    );
    assert_eq!(version(db).await, 24);
    assert_eq!(rows(db).await, before);
    // Each kind alone still refuses.
    execute(db, "DELETE FROM neev_grants", vec![]).await.unwrap();
    assert_eq!(
        downgrade_member_sharing(db).await,
        Ok(member_sharing_migration::Downgrade::SharingExists { rows: 2 })
    );
    execute(db, "DELETE FROM neev_shared_marks", vec![]).await.unwrap();
    assert_eq!(
        downgrade_member_sharing(db).await,
        Ok(member_sharing_migration::Downgrade::SharingExists { rows: 1 })
    );
    execute(db, "DELETE FROM neev_teams", vec![]).await.unwrap();
    assert_eq!(downgrade_member_sharing(db).await, Ok(member_sharing_migration::Downgrade::Done));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    assert_eq!(version(db).await, 23);
    assert_eq!(
        downgrade_member_sharing(db).await,
        Ok(member_sharing_migration::Downgrade::NotAtTwentyFour(23))
    );
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    at_twenty_four(db).await;
    assert_eq!(catalog(db).await, fresh);
}

#[tokio::test]
async fn fresh_install_is_schema_24_with_the_sharing_tables() {
    let db = h::database().await;
    fresh_is_twenty_four(&db).await;
}

#[tokio::test]
async fn upgrade_from_23_keeps_rows_widens_nothing_and_restart_is_idempotent() {
    let db = h::database().await;
    twenty_three_upgrades_preserving_rows(&db).await;
}

#[tokio::test]
async fn downgrade_refuses_while_any_team_grant_or_mark_exists() {
    let db = h::database().await;
    downgrade_is_guarded(&db).await;
}

#[tokio::test]
async fn incoherent_sharing_rows_are_refused() {
    // A team member who was never a member of the org.
    let db = h::database().await;
    let (org, user, member, folder) = seed(&db).await;
    share(&db, org, user, member, folder).await;
    let stranger = h::native_account(&db, "stranger", "stranger@example.test").await;
    execute(&db, "INSERT INTO neev_team_members(org_id,team_id,user_id,added_by,added_at) VALUES($1,$2,$3,$4,1)", vec![org.into(), TEAM.into(), stranger.into(), user.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));

    // A team grant naming no team of the org.
    let db = h::database().await;
    let (org, user, member, folder) = seed(&db).await;
    share(&db, org, user, member, folder).await;
    execute(&db, "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,'resource',$2,'team','77777777-7777-4777-8777-777777777777','view',$3,1)", vec![org.into(), DOC.into(), user.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));

    // A shared mark on an organization resource (it would mean nothing) and on
    // a resource that does not exist.
    for target in ["44444444-4444-4444-8444-444444444444", "55555555-5555-4555-8555-555555555555"] {
        let db = h::database().await;
        let (org, user, _, _) = seed(&db).await;
        execute(&db, "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'resource',$2,$3,1)", vec![org.into(), target.into(), user.into()]).await.unwrap();
        assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
        assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    }

    // A derived restricted region the marks do not explain, or a mark whose
    // region was never written.
    let db = h::database().await;
    let (org, user, _, folder) = seed(&db).await;
    execute(&db, "INSERT INTO neev_restricted_nodes(org_id,node_id,root_id) VALUES($1,$2,$2)", vec![org.into(), folder.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
    execute(&db, "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'node_restricted',$2,$3,1)", vec![org.into(), folder.to_string().into(), user.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Ok(()), "mark and region agree");
    execute(&db, "DELETE FROM neev_restricted_nodes", vec![]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
    // A restriction mark on something that is not a project or folder.
    let db = h::database().await;
    let (org, user, _, _) = seed(&db).await;
    execute(&db, "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'node_restricted','999999',$2,1)", vec![org.into(), user.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));

    // A look-alike grants table without its level CHECK.
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_grants", vec![]).await.unwrap();
    execute(&db, "CREATE TABLE neev_grants (org_id integer NOT NULL, target_kind text NOT NULL, target_id text NOT NULL, principal_kind text NOT NULL, principal_id text NOT NULL, level text NOT NULL, granted_by integer NOT NULL, created_at bigint NOT NULL, CONSTRAINT neev_grants_pk PRIMARY KEY (org_id, target_kind, target_id, principal_kind, principal_id))", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    // Stepping down from 25 re-proves the 24 layer and refuses (rolled back).
    assert_eq!(downgrade_msteams(&db).await, Err(AuthError::Unavailable));
    assert_eq!(version(&db).await, CURRENT_SCHEMA_VERSION);

    // A stray index on a sharing table.
    let db = h::database().await;
    execute(&db, "CREATE INDEX neev_grants_extra ON neev_grants (principal_id)", vec![]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_install_is_schema_24() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_is_twenty_four(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_upgrade_from_23_keeps_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    twenty_three_upgrades_preserving_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_any_share_exists() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_is_guarded(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_incoherent_rows_and_look_alikes_are_refused() {
    let (db, admin, schema) = h::postgres_database().await;
    let (org, user, member, folder) = seed(&db).await;
    share(&db, org, user, member, folder).await;
    execute(&db, "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'resource','44444444-4444-4444-8444-444444444444',$2,1)", vec![org.into(), user.into()]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
    execute(&db, "DELETE FROM neev_shared_marks WHERE target_id='44444444-4444-4444-8444-444444444444'", vec![]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Ok(()));
    execute(&db, "ALTER TABLE neev_grants DROP CONSTRAINT neev_grants_level", vec![]).await.unwrap();
    assert_eq!(member_sharing_migration::validate(&db).await, Err(AuthError::Unavailable));
    h::cleanup_postgres(db, admin, schema).await;
}
