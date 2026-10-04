//! Schema 20: the agent provider enum admits the ACP providers, on SQLite and
//! PostgreSQL, fresh and upgraded from a real schema-19 shape, preserving rows.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const SESSION: &str = "22222222-2222-4222-8222-222222222222";
const OLD: &str = "provider IN ('claude','codex')";
const NEW: &str = "provider IN ('claude','codex','cursor','gemini','opencode')";

async fn version(db: &DatabaseConnection) -> i64 {
    one(db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "version")
        .unwrap()
}

/// A fresh install is schema 23; the 20 -> 19 step starts from 20, so take the
/// (table-only, empty) schema-23, schema-22 and schema-21 layers off first.
async fn at_twenty(db: &DatabaseConnection) {
    assert_eq!(downgrade_node_trash(db).await, Ok(node_trash_migration::Downgrade::Done));
    assert_eq!(
        downgrade_knowledge(db).await,
        Ok(knowledge_migration::Downgrade::Downgraded)
    );
    assert_eq!(
        downgrade_node_sharing(db).await,
        Ok(node_sharing_migration::Downgrade::Done)
    );
    assert_eq!(version(db).await, provider_enum_migration::SCHEMA_VERSION);
}

async fn seed(db: &DatabaseConnection) -> (i32, i32) {
    let user = h::native_account(db, "provider-owner", "provider@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    (org, user)
}

async fn connection(
    db: &DatabaseConnection,
    org: i32,
    user: i32,
    id_byte: char,
    provider: &str,
) -> Result<(), AuthError> {
    execute(
        db,
        "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,1,'disconnected',NULL,NULL,NULL,NULL,1000,1000)",
        vec![
            std::iter::repeat_n(id_byte, 32).collect::<String>().into(),
            org.into(),
            user.into(),
            provider.into(),
        ],
    )
    .await
    .map(|_| ())
}

async fn session_and_turn(db: &DatabaseConnection, org: i32, user: i32, provider: &str) -> Result<(), AuthError> {
    execute(
        db,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Plan','$P','default','private','chat',NULL,NULL,false,1,1000,1000)"
            .replace("$P", provider)
            .as_str(),
        vec![org.into(), SESSION.into(), user.into()],
    )
    .await?;
    execute(
        db,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,'33333333-3333-4333-8333-333333333333','run-1','ask','ok','completed',NULL,'$P','default',1000,1050,1100)"
            .replace("$P", provider)
            .as_str(),
        vec![org.into(), SESSION.into()],
    )
    .await
    .map(|_| ())
}

/// Put a current database back into the exact schema-19 provider shape: the
/// narrow CHECK text schema 5/11 baked, and marker 19.
async fn narrow_back_to_nineteen(db: &DatabaseConnection) {
    at_twenty(db).await;
    match db.get_database_backend() {
        DbBackend::Sqlite => {
            let version: i64 = one(db, "PRAGMA schema_version", vec![])
                .await
                .unwrap()
                .unwrap()
                .try_get("", "schema_version")
                .unwrap();
            execute(db, "PRAGMA writable_schema=ON", vec![]).await.unwrap();
            for table in provider_enum_migration::TABLES {
                execute(
                    db,
                    "UPDATE sqlite_master SET sql=replace(sql,$1,$2) WHERE type='table' AND name=$3",
                    vec![NEW.into(), OLD.into(), table.into()],
                )
                .await
                .unwrap();
            }
            execute(db, &format!("PRAGMA schema_version={}", version + 1), vec![])
                .await
                .unwrap();
            execute(db, "PRAGMA writable_schema=OFF", vec![]).await.unwrap();
        }
        _ => {
            for table in provider_enum_migration::TABLES {
                execute(
                    db,
                    &format!("ALTER TABLE {table} DROP CONSTRAINT {table}_provider_allowed"),
                    vec![],
                )
                .await
                .unwrap();
                execute(
                    db,
                    &format!("ALTER TABLE {table} ADD CONSTRAINT ck_{table}_provider_v19 CHECK ({OLD})"),
                    vec![],
                )
                .await
                .unwrap();
            }
        }
    }
    execute(db, "UPDATE neev_schema_version SET version=19 WHERE id=1", vec![])
        .await
        .unwrap();
    assert_eq!(
        provider_enum_migration::validate(db).await,
        Err(AuthError::Unavailable),
        "the fixture really is narrow"
    );
}

async fn fresh_admits_acp_and_nothing_else(db: &DatabaseConnection) {
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(provider_enum_migration::validate(db).await, Ok(()));
    let (org, user) = seed(db).await;
    for (byte, provider) in [('a', "claude"), ('b', "codex"), ('c', "cursor"), ('d', "gemini"), ('e', "opencode")] {
        connection(db, org, user, byte, provider).await.unwrap();
    }
    for bad in ["grok", "Gemini", ""] {
        assert!(connection(db, org, user, 'f', bad).await.is_err(), "{bad}");
    }
    session_and_turn(db, org, user, "opencode").await.unwrap();
    assert_eq!(session_migration::validate(db).await, Ok(()));
}

async fn nineteen_upgrades_in_place(db: &DatabaseConnection) {
    let (org, user) = seed(db).await;
    connection(db, org, user, 'a', "claude").await.unwrap();
    session_and_turn(db, org, user, "codex").await.unwrap();
    narrow_back_to_nineteen(db).await;
    // At 19 the narrow CHECK really refuses an ACP provider.
    assert!(connection(db, org, user, 'b', "gemini").await.is_err());

    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(provider_enum_migration::validate(db).await, Ok(()));
    // Every row survived, FKs intact.
    assert_eq!(h::count(db, "neev_agent_connections").await, 1);
    assert_eq!(h::count(db, "neev_agent_sessions").await, 1);
    assert_eq!(h::count(db, "neev_agent_turns").await, 1);
    connection(db, org, user, 'b', "gemini").await.unwrap();
    assert!(connection(db, org, user, 'c', "grok").await.is_err());
    // Restart at the current schema validates in place and changes nothing.
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(h::count(db, "neev_agent_connections").await, 2);
}

#[tokio::test]
async fn sqlite_fresh_schema_admits_the_acp_providers() {
    let db = h::database().await;
    fresh_admits_acp_and_nothing_else(&db).await;
}

#[tokio::test]
async fn sqlite_nineteen_upgrades_to_twenty_preserving_rows() {
    let db = h::database().await;
    nineteen_upgrades_in_place(&db).await;
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
async fn sqlite_an_unrecognised_provider_ddl_is_refused_not_edited() {
    let db = h::database().await;
    narrow_back_to_nineteen(&db).await;
    // Tamper: the connections CHECK carries neither the old nor the new text.
    let cookie: i64 = one(&db, "PRAGMA schema_version", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "schema_version")
        .unwrap();
    execute(&db, "PRAGMA writable_schema=ON", vec![]).await.unwrap();
    execute(
        &db,
        "UPDATE sqlite_master SET sql=replace(sql,$1,'provider IN (''claude'',''codex'',''grok'')') WHERE type='table' AND name='neev_agent_connections'",
        vec![OLD.into()],
    )
    .await
    .unwrap();
    execute(&db, &format!("PRAGMA schema_version={}", cookie + 1), vec![])
        .await
        .unwrap();
    execute(&db, "PRAGMA writable_schema=OFF", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    // Rolled back: still marker 19, sessions/turns still narrow.
    assert_eq!(version(&db).await, 19);
    let ddl: String = one(
        &db,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_agent_sessions'",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "sql")
    .unwrap();
    assert!(ddl.contains(OLD) && !ddl.contains(NEW));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_schema_admits_the_acp_providers() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_admits_acp_and_nothing_else(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_nineteen_upgrades_to_twenty_preserving_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    nineteen_upgrades_in_place(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
async fn sqlite_a_twenty_snapshot_still_runs_row_validation() {
    // Marker 20 must not skip the per-version validators a 19 snapshot runs.
    let db = h::database().await;
    at_twenty(&db).await;
    assert_eq!(validate_content_snapshot(&db).await, Ok(provider_enum_migration::SCHEMA_VERSION));
    let (org, user) = seed(&db).await;
    session_and_turn(&db, org, user, "gemini").await.unwrap();
    assert_eq!(validate_content_snapshot(&db).await, Ok(provider_enum_migration::SCHEMA_VERSION));
    // A turn contradicting its session's provider is storable (both are valid
    // enum values) and only the session validator's join catches it.
    execute(&db, "UPDATE neev_agent_turns SET provider='cursor'", vec![])
        .await
        .unwrap();
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
}

async fn downgrade_round_trip(db: &DatabaseConnection) {
    at_twenty(db).await;
    let (org, user) = seed(db).await;
    connection(db, org, user, 'a', "claude").await.unwrap();
    session_and_turn(db, org, user, "codex").await.unwrap();

    assert_eq!(
        downgrade_provider_enum(db).await,
        Ok(provider_enum_migration::Downgrade::Done)
    );
    assert_eq!(version(db).await, 19);
    assert_eq!(provider_enum_migration::validate_narrow(db).await, Ok(()));
    assert_eq!(h::count(db, "neev_agent_connections").await, 1);
    assert_eq!(h::count(db, "neev_agent_sessions").await, 1);
    assert_eq!(h::count(db, "neev_agent_turns").await, 1);
    // Schema 19's own constraint is back: an ACP provider is refused again.
    assert!(connection(db, org, user, 'b', "gemini").await.is_err());
    // Guarded on the marker: a second run changes nothing.
    assert_eq!(
        downgrade_provider_enum(db).await,
        Ok(provider_enum_migration::Downgrade::NotAtTwenty(19))
    );
    // The downgraded database passes the whole schema-19 arm (every earlier
    // validator runs there) and re-upgrades in place.
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    connection(db, org, user, 'b', "gemini").await.unwrap();
    assert_eq!(h::count(db, "neev_agent_turns").await, 1);
}

async fn downgrade_refuses_over_acp_rows(db: &DatabaseConnection) {
    at_twenty(db).await;
    let (org, user) = seed(db).await;
    connection(db, org, user, 'a', "claude").await.unwrap();
    session_and_turn(db, org, user, "opencode").await.unwrap();
    assert_eq!(
        downgrade_provider_enum(db).await,
        Ok(provider_enum_migration::Downgrade::AcpRowsExist {
            table: "neev_agent_sessions",
            rows: 1
        })
    );
    // Nothing changed: still 20, still wide, every row still there.
    assert_eq!(version(db).await, provider_enum_migration::SCHEMA_VERSION);
    assert_eq!(provider_enum_migration::validate(db).await, Ok(()));
    assert_eq!(h::count(db, "neev_agent_sessions").await, 1);
    assert_eq!(h::count(db, "neev_agent_turns").await, 1);
    connection(db, org, user, 'b', "cursor").await.unwrap();
    assert_eq!(
        downgrade_provider_enum(db).await,
        Ok(provider_enum_migration::Downgrade::AcpRowsExist {
            table: "neev_agent_connections",
            rows: 1
        })
    );
}

#[tokio::test]
async fn sqlite_downgrade_to_nineteen_round_trips_preserving_rows() {
    let db = h::database().await;
    downgrade_round_trip(&db).await;
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
async fn sqlite_downgrade_refuses_while_acp_rows_exist() {
    let db = h::database().await;
    downgrade_refuses_over_acp_rows(&db).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_to_nineteen_round_trips_preserving_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_round_trip(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_acp_rows_exist() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_refuses_over_acp_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}
