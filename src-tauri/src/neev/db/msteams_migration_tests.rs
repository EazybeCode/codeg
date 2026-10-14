//! Schema 25: Microsoft Teams bindings and thread ledgers. Fresh install,
//! 24 -> 25 changing no existing row, idempotent restart, the constraints that
//! make duplicate or cross-org bindings unrepresentable, refusal of incoherent
//! rows and look-alike tables, and the guarded 25 -> 24 downgrade.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const TENANT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER_TENANT: &str = "bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee";
const OID: &str = "0f0f0f0f-1111-2222-3333-444444444444";
const TEAM: &str = "19:team@thread.tacv2";
const CHANNEL: &str = "19:abc@thread.tacv2";
const ROOT: &str = "1700000000001";

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
    for table in msteams_migration::TABLE_NAMES {
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

/// An org with an admin, a member and a project; a second org with a project.
struct Seed {
    org: i32,
    admin: i32,
    member: i32,
    project: i32,
    other_org: i32,
    other_project: i32,
}

async fn seed(db: &DatabaseConnection) -> Seed {
    let admin = h::native_account(db, "teams-admin", "admin@example.test").await;
    let (org, _) = h::organization(db, admin, "admin").await;
    let member = h::native_account(db, "teams-member", "member@example.test").await;
    h::add_membership(db, org, member, "member").await;
    let project: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Sales',1,$2) RETURNING id", vec![org.into(), admin.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    let outsider = h::native_account(db, "teams-outsider", "outsider@example.test").await;
    let (other_org, _) = h::organization(db, outsider, "admin").await;
    let other_project: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Other',1,$2) RETURNING id", vec![other_org.into(), outsider.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    Seed {
        org,
        admin,
        member,
        project,
        other_org,
        other_project,
    }
}

/// A complete binding chain and one thread with one input.
async fn bind(db: &DatabaseConnection, s: &Seed) {
    execute(db, "INSERT INTO neev_msteams_installations(tenant_id,org_id,enabled,provider,model,revision,created_by,created_at,updated_at) VALUES($1,$2,true,'claude','default',1,$3,1000,1000)", vec![TENANT.into(), s.org.into(), s.admin.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_project_links(org_id,tenant_id,team_id,graph_group_id,node_id,enabled,verified_owner,verified_at,revision,created_by,created_at,updated_at) VALUES($1,$2,$3,'12345678-1234-1234-1234-123456789abc',$4,true,'0a0a0a0a-1111-2222-3333-444444444444',1000,1,$5,1000,1000)", vec![s.org.into(), TENANT.into(), TEAM.into(), s.project.into(), s.admin.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_channel_links(org_id,tenant_id,team_id,channel_id,enabled,verified_at,created_by,created_at,updated_at) VALUES($1,$2,$3,$4,true,1000,$5,1000,1000)", vec![s.org.into(), TENANT.into(), TEAM.into(), CHANNEL.into(), s.admin.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_principals(org_id,tenant_id,aad_object_id,user_id,created_by,created_at) VALUES($1,$2,$3,$4,$5,1000)", vec![s.org.into(), TENANT.into(), OID.into(), s.member.into(), s.admin.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_threads(org_id,tenant_id,team_id,channel_id,root_id,service_url,started_by,live_sequence,next_sequence,created_at,updated_at) VALUES($1,$2,'19:team@thread.tacv2',$3,$4,'https://smba.trafficmanager.net/amer/',$5,NULL,2,1000,1000)", vec![s.org.into(), TENANT.into(), CHANNEL.into(), ROOT.into(), s.member.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,$3,$4,$5,$6,1,'input',$7,'Asha','draft a proposal','queued',NULL,NULL,1000)", vec![s.org.into(), TENANT.into(), format!("{CHANNEL};messageid={ROOT}").into(), ROOT.into(), CHANNEL.into(), ROOT.into(), s.member.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_inbox(org_id,tenant_id,conversation_id,activity_id,scope,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,refusal,attempts,next_attempt_at,created_at,updated_at) VALUES($1,$2,$3,$4,'channel','https://smba.trafficmanager.net/amer/',$5,$6,'Asha','draft a proposal','19:team@thread.tacv2',$7,$4,1,'queued',NULL,NULL,0,1000,1000,1000)", vec![s.org.into(), TENANT.into(), format!("{CHANNEL};messageid={ROOT}").into(), ROOT.into(), s.member.into(), OID.into(), CHANNEL.into()]).await.unwrap();
    execute(db, "INSERT INTO neev_msteams_outbox(id,org_id,tenant_id,service_url,conversation_id,reply_to_id,scope,channel_id,root_id,kind,run_id,user_id,card,state,attempts,next_attempt_at,message_id,error_code,created_at,updated_at) VALUES($1,$2,$3,'https://smba.trafficmanager.net/amer/',$4,$5,'channel',$6,$5,'notice',NULL,$7,'{}','pending',0,1000,NULL,NULL,1000,1000)", vec!["a".repeat(64).into(), s.org.into(), TENANT.into(), format!("{CHANNEL};messageid={ROOT}").into(), ROOT.into(), CHANNEL.into(), s.member.into()]).await.unwrap();
}

async fn fresh_is_twenty_five(db: &DatabaseConnection) {
    assert_eq!(CURRENT_SCHEMA_VERSION, 25);
    assert_eq!(version(db).await, msteams_migration::SCHEMA_VERSION);
    assert_eq!(tables(db).await, msteams_migration::TABLE_NAMES.len());
    assert_eq!(msteams_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(25));
    // The older operator step refuses a schema-25 database.
    assert_eq!(
        downgrade_member_sharing(db).await,
        Ok(member_sharing_migration::Downgrade::NotAtTwentyFour(25))
    );
    let s = seed(db).await;
    bind(db, &s).await;
    assert_eq!(msteams_migration::validate(db).await, Ok(()));
    assert_eq!(validate_content_snapshot(db).await, Ok(25));
    for (sql, values) in [
        // A tenant binds to exactly one org: a second installation row for the
        // same tenant (any org) is refused by the primary key.
        ("INSERT INTO neev_msteams_installations(tenant_id,org_id,enabled,provider,model,revision,created_by,created_at,updated_at) VALUES($1,$2,true,'claude','default',1,$3,1,1)", vec![TENANT.into(), s.other_org.into(), s.admin.into()]),
        // A Team cannot be linked twice in a tenant.
        ("INSERT INTO neev_msteams_project_links(org_id,tenant_id,team_id,graph_group_id,node_id,enabled,verified_owner,verified_at,revision,created_by,created_at,updated_at) VALUES($1,$2,$3,'2'||substr('12345678-1234-1234-1234-123456789abc',2),$4,true,'0a0a0a0a-1111-2222-3333-444444444444',1,1,$5,1,1)", vec![s.org.into(), TENANT.into(), TEAM.into(), s.project.into(), s.admin.into()]),
        // A Team cannot be linked to another org's project ...
        ("INSERT INTO neev_msteams_project_links(org_id,tenant_id,team_id,graph_group_id,node_id,enabled,verified_owner,verified_at,revision,created_by,created_at,updated_at) VALUES($1,$2,'19:t2@thread.tacv2','2'||substr('12345678-1234-1234-1234-123456789abc',2),$3,true,'0a0a0a0a-1111-2222-3333-444444444444',1,1,$4,1,1)", vec![s.org.into(), TENANT.into(), s.other_project.into(), s.admin.into()]),
        // ... nor claim another org for an installed tenant.
        ("INSERT INTO neev_msteams_project_links(org_id,tenant_id,team_id,graph_group_id,node_id,enabled,verified_owner,verified_at,revision,created_by,created_at,updated_at) VALUES($1,$2,'19:t3@thread.tacv2','2'||substr('12345678-1234-1234-1234-123456789abc',2),$3,true,'0a0a0a0a-1111-2222-3333-444444444444',1,1,$4,1,1)", vec![s.other_org.into(), TENANT.into(), s.other_project.into(), s.admin.into()]),
        // A channel of a Team that is not linked.
        ("INSERT INTO neev_msteams_channel_links(org_id,tenant_id,team_id,channel_id,enabled,verified_at,created_by,created_at,updated_at) VALUES($1,$2,'19:nolink@thread.tacv2','19:c2@thread.tacv2',true,1,$3,1,1)", vec![s.org.into(), TENANT.into(), s.admin.into()]),
        // One Entra object id per tenant, and one Neev account per tenant.
        ("INSERT INTO neev_msteams_principals(org_id,tenant_id,aad_object_id,user_id,created_by,created_at) VALUES($1,$2,$3,$4,$5,1)", vec![s.org.into(), TENANT.into(), OID.into(), s.admin.into(), s.admin.into()]),
        ("INSERT INTO neev_msteams_principals(org_id,tenant_id,aad_object_id,user_id,created_by,created_at) VALUES($1,$2,'0e0e0e0e-1111-2222-3333-444444444444',$3,$4,1)", vec![s.org.into(), TENANT.into(), s.member.into(), s.admin.into()]),
        // A principal must be a member of the tenant's org.
        ("INSERT INTO neev_msteams_principals(org_id,tenant_id,aad_object_id,user_id,created_by,created_at) VALUES($1,$2,'0d0d0d0d-1111-2222-3333-444444444444',$3,$4,1)", vec![s.other_org.into(), TENANT.into(), s.admin.into(), s.admin.into()]),
        // A thread in a channel that is not linked.
        ("INSERT INTO neev_msteams_threads(org_id,tenant_id,team_id,channel_id,root_id,service_url,started_by,live_sequence,next_sequence,created_at,updated_at) VALUES($1,$2,'19:team@thread.tacv2','19:none@thread.tacv2','r','https://smba.trafficmanager.net/amer/',$3,NULL,1,1,1)", vec![s.org.into(), TENANT.into(), s.member.into()]),
        // The inbox key: a retried activity cannot be recorded twice.
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,$3,$4,$5,$4,2,'input',$6,'Asha','again','queued',NULL,NULL,1)", vec![s.org.into(), TENANT.into(), format!("{CHANNEL};messageid={ROOT}").into(), ROOT.into(), CHANNEL.into(), s.member.into()]),
        // Ordering is unique per thread.
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,'c','a2',$3,$4,1,'input',$5,'Asha','x','queued',NULL,NULL,1)", vec![s.org.into(), TENANT.into(), CHANNEL.into(), ROOT.into(), s.member.into()]),
        // An input always has an author; an answer never does, and names its run.
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,'c','a3',$3,$4,3,'input',NULL,'?','x','queued',NULL,NULL,1)", vec![s.org.into(), TENANT.into(), CHANNEL.into(), ROOT.into()]),
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,'c','a4',$3,$4,4,'answer',NULL,'Neev','x','answered',NULL,NULL,1)", vec![s.org.into(), TENANT.into(), CHANNEL.into(), ROOT.into()]),
        // Unknown kinds/outcomes.
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,'c','a5',$3,$4,5,'input',$5,'Asha','x','executed',NULL,NULL,1)", vec![s.org.into(), TENANT.into(), CHANNEL.into(), ROOT.into(), s.member.into()]),
        // A ledger row claiming ANOTHER org for an existing thread (composite key).
        ("INSERT INTO neev_msteams_thread_messages(org_id,tenant_id,conversation_id,activity_id,channel_id,root_id,sequence,kind,author_user_id,author_name,body,outcome,executed_as,run_id,created_at) VALUES($1,$2,'c','x1',$3,$4,9,'input',$5,'Eve','x','queued',NULL,NULL,1)", vec![s.other_org.into(), TENANT.into(), CHANNEL.into(), ROOT.into(), s.admin.into()]),
        // An inbox row in a state that contradicts its run.
        ("INSERT INTO neev_msteams_inbox(org_id,tenant_id,conversation_id,activity_id,scope,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,refusal,attempts,next_attempt_at,created_at,updated_at) VALUES($1,$2,'p','x2','personal','https://smba.trafficmanager.net/amer/',$3,$4,'Asha','x',NULL,NULL,NULL,NULL,'launched',NULL,NULL,0,1,1,1)", vec![s.org.into(), TENANT.into(), s.member.into(), OID.into()]),
        // A personal inbox row naming a channel.
        ("INSERT INTO neev_msteams_inbox(org_id,tenant_id,conversation_id,activity_id,scope,service_url,user_id,aad_object_id,author_name,body,team_id,channel_id,root_id,sequence,state,run_id,refusal,attempts,next_attempt_at,created_at,updated_at) VALUES($1,$2,'p','x3','personal','https://smba.trafficmanager.net/amer/',$3,$4,'Asha','x','t',$5,'r',1,'pending',NULL,NULL,0,1,1,1)", vec![s.org.into(), TENANT.into(), s.member.into(), OID.into(), CHANNEL.into()]),
        // A final delivery must name its run and store no text.
        ("INSERT INTO neev_msteams_outbox(id,org_id,tenant_id,service_url,conversation_id,reply_to_id,scope,channel_id,root_id,kind,run_id,user_id,card,state,attempts,next_attempt_at,message_id,error_code,created_at,updated_at) VALUES($1,$2,$3,'https://smba.trafficmanager.net/amer/','p','r','personal',NULL,NULL,'final',NULL,$4,'{\"answer\":1}','pending',0,1,NULL,NULL,1,1)", vec!["b".repeat(64).into(), s.org.into(), TENANT.into(), s.member.into()]),
        // A malformed tenant id.
        ("INSERT INTO neev_msteams_installations(tenant_id,org_id,enabled,provider,model,revision,created_by,created_at,updated_at) VALUES('short',$1,true,'claude','default',1,$2,1,1)", vec![s.org.into(), s.admin.into()]),
    ] {
        assert!(execute(db, sql, values).await.is_err(), "{sql}");
    }
    // The second tenant can bind (to its own org); the bound rows are coherent.
    execute(db, "INSERT INTO neev_msteams_installations(tenant_id,org_id,enabled,provider,model,revision,created_by,created_at,updated_at) VALUES($1,$2,true,'claude','default',1,$3,1,1)", vec![OTHER_TENANT.into(), s.other_org.into(), s.admin.into()]).await.unwrap();
    assert_eq!(msteams_migration::validate(db).await, Ok(()));
}

/// 24 -> 25 changes no existing row; restart at 25 is a no-op.
async fn twenty_four_upgrades_preserving_rows(db: &DatabaseConnection) {
    assert_eq!(downgrade_msteams(db).await, Ok(msteams_migration::Downgrade::Done));
    assert_eq!(version(db).await, 24);
    assert_eq!(tables(db).await, 0);
    assert_eq!(validate_content_snapshot(db).await, Ok(24));
    seed(db).await;
    let before = catalog(db).await;
    let members = h::count(db, "neev_memberships").await;
    let nodes = h::count(db, "neev_nodes").await;
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, 25);
    assert_eq!(tables(db).await, msteams_migration::TABLE_NAMES.len());
    assert_eq!(h::count(db, "neev_memberships").await, members);
    assert_eq!(h::count(db, "neev_nodes").await, nodes);
    for table in msteams_migration::TABLE_NAMES {
        assert_eq!(h::count(db, table).await, 0, "{table} was backfilled");
    }
    // Only the new Teams tables were added to the catalog.
    let after = catalog(db).await;
    let added: Vec<&String> = after.iter().filter(|item| !before.contains(item)).collect();
    assert!(added
        .iter()
        .all(|item| msteams_migration::TABLE_NAMES.iter().any(|table| item.contains(table))));
    initialize(db).await.unwrap();
    assert_eq!(catalog(db).await, after, "restart changed the catalog");
}

/// Any Teams row refuses the downgrade, which then changes nothing; with none
/// it lands on exactly the schema-24 catalog, and re-migrates.
async fn downgrade_is_guarded(db: &DatabaseConnection) {
    let fresh = catalog(db).await;
    let expected: Vec<String> = fresh
        .iter()
        .filter(|item| {
            !msteams_migration::TABLE_NAMES
                .iter()
                .any(|table| item.contains(table))
        })
        .cloned()
        .collect();
    let s = seed(db).await;
    bind(db, &s).await;
    assert_eq!(
        downgrade_msteams(db).await,
        Ok(msteams_migration::Downgrade::TeamsDataExists { rows: 8 })
    );
    assert_eq!(version(db).await, 25);
    assert_eq!(h::count(db, "neev_msteams_thread_messages").await, 1);
    for table in msteams_migration::DROP_ORDER {
        execute(db, &format!("DELETE FROM {table}"), vec![]).await.unwrap();
    }
    assert_eq!(downgrade_msteams(db).await, Ok(msteams_migration::Downgrade::Done));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(version(db).await, 24);
    assert_eq!(
        downgrade_msteams(db).await,
        Ok(msteams_migration::Downgrade::NotAtTwentyFive(24))
    );
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, 25);
    assert_eq!(catalog(db).await, fresh);
}

#[tokio::test]
async fn fresh_install_is_schema_25_with_the_teams_tables() {
    let db = h::database().await;
    fresh_is_twenty_five(&db).await;
}

#[tokio::test]
async fn upgrade_from_24_keeps_rows_and_restart_is_idempotent() {
    let db = h::database().await;
    twenty_four_upgrades_preserving_rows(&db).await;
}

#[tokio::test]
async fn downgrade_refuses_while_any_teams_row_exists() {
    let db = h::database().await;
    downgrade_is_guarded(&db).await;
}

#[tokio::test]
async fn incoherent_teams_rows_and_look_alikes_are_refused() {
    // A Team linked to a folder rather than a project.
    let db = h::database().await;
    let s = seed(&db).await;
    bind(&db, &s).await;
    let folder: i32 = one(&db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'folder','F',1,$3) RETURNING id", vec![s.org.into(), s.project.into(), s.admin.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    execute(&db, "UPDATE neev_msteams_project_links SET node_id=$1", vec![folder.into()]).await.unwrap();
    assert_eq!(msteams_migration::validate(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));

    // A live claim that names a queued input rather than a started one.
    let db = h::database().await;
    let s = seed(&db).await;
    bind(&db, &s).await;
    execute(&db, "UPDATE neev_msteams_threads SET live_sequence=1", vec![]).await.unwrap();
    assert_eq!(msteams_migration::validate(&db).await, Err(AuthError::Unavailable));
    execute(&db, "UPDATE neev_msteams_thread_messages SET outcome='started'", vec![]).await.unwrap();
    assert_eq!(msteams_migration::validate(&db).await, Ok(()), "a claim on a started input");
    // A claim beyond the thread's own sequence.
    assert!(execute(&db, "UPDATE neev_msteams_threads SET live_sequence=7", vec![]).await.is_err());

    // A look-alike inbox without its primary key (no duplicate protection).
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_msteams_thread_messages", vec![]).await.unwrap();
    execute(&db, "CREATE TABLE neev_msteams_thread_messages (org_id integer NOT NULL, tenant_id text NOT NULL, conversation_id text NOT NULL, activity_id text NOT NULL, channel_id text NOT NULL, root_id text NOT NULL, sequence bigint NOT NULL, kind text NOT NULL, author_user_id integer, author_name text NOT NULL, body text NOT NULL, outcome text NOT NULL, executed_as integer, run_id text, created_at bigint NOT NULL)", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    assert_eq!(downgrade_msteams(&db).await, Err(AuthError::Unavailable));

    // A stray index on a Teams table.
    let db = h::database().await;
    execute(&db, "CREATE INDEX neev_msteams_extra ON neev_msteams_principals (user_id)", vec![]).await.unwrap();
    assert_eq!(msteams_migration::validate(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_install_is_schema_25() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_is_twenty_five(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_upgrade_from_24_keeps_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    twenty_four_upgrades_preserving_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_any_teams_row_exists() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_is_guarded(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}
