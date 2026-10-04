use super::*;
use crate::neev::test_support as h;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, TransactionTrait, Value};

// Invented fixture identity (never minted through runtime APIs).
const USER: i64 = 101;
const ORG: i64 = 201;
const MEMBER: i64 = 301;
const SOURCE: &str = "11111111-1111-4111-8111-111111111111";
const RELYING: &str = "22222222-2222-4222-8222-222222222222";
const RECEIPT: &str = "33333333-3333-4333-8333-333333333333";
const EVENT: &str = "44444444-4444-4444-8444-444444444444";
const INTENT: &str = "55555555-5555-4555-8555-555555555555";

// Exact ordered columns per section 1; reused for assertions and the
// physically test-only unconstrained fixture.
const AUDIENCE_COLS: &[(&str, &str)] = &[
    ("audience", "TEXT"),
    ("issuer", "TEXT"),
    ("last_cursor", "BIGINT"),
    ("fetched_through", "BIGINT"),
    ("acknowledged_cursor", "BIGINT"),
    ("writer_epoch", "BIGINT"),
];
const BINDING_COLS: &[(&str, &str)] = &[
    ("relying_ref", "TEXT"),
    ("audience", "TEXT"),
    ("source_session_id", "TEXT"),
    ("user_id", "INTEGER"),
    ("org_id", "INTEGER"),
    ("created_at", "BIGINT"),
];
const EXCHANGE_COLS: &[(&str, &str)] = &[
    ("code_hash", "TEXT"),
    ("audience", "TEXT"),
    ("attempt_id", "TEXT"),
    ("challenge", "TEXT"),
    ("source_session_id", "TEXT"),
    ("user_id", "INTEGER"),
    ("org_id", "INTEGER"),
    ("member_id", "INTEGER"),
    ("account_revision", "BIGINT"),
    ("org_revision", "BIGINT"),
    ("member_revision", "BIGINT"),
    ("role", "TEXT"),
    ("authenticated_at", "BIGINT"),
    ("issued_at", "BIGINT"),
    ("code_expires_at", "BIGINT"),
    ("retain_until", "BIGINT"),
    ("consumed_at", "BIGINT"),
];
const RELYING_COLS: &[(&str, &str)] = &[
    ("relying_ref", "TEXT"),
    ("audience", "TEXT"),
    ("issuer", "TEXT"),
    ("source_session_id", "TEXT"),
    ("user_id", "INTEGER"),
    ("org_id", "INTEGER"),
    ("member_id", "INTEGER"),
    ("account_revision", "BIGINT"),
    ("org_revision", "BIGINT"),
    ("member_revision", "BIGINT"),
    ("role", "TEXT"),
    ("authenticated_at", "BIGINT"),
    ("display_name", "TEXT"),
    ("org_name", "TEXT"),
    ("verified_email", "TEXT"),
    ("created_at", "BIGINT"),
    ("expires_at", "BIGINT"),
    ("issued_after_cursor", "BIGINT"),
];
const REDEMPTION_COLS: &[(&str, &str)] = &[
    ("receipt_id", "TEXT"),
    ("audience", "TEXT"),
    ("code_hash", "TEXT"),
    ("relying_ref", "TEXT"),
    ("redeemed_at", "BIGINT"),
    ("retry_until", "BIGINT"),
];
const REVOCATION_COLS: &[(&str, &str)] = &[
    ("audience", "TEXT"),
    ("cursor", "BIGINT"),
    ("event_id", "TEXT"),
    ("scope", "TEXT"),
    ("user_id", "INTEGER"),
    ("org_id", "INTEGER"),
    ("source_session_id", "TEXT"),
    ("created_at", "BIGINT"),
];
const INTENT_COLS: &[(&str, &str)] = &[
    ("audience", "TEXT"),
    ("intent_id", "TEXT"),
    ("relying_ref", "TEXT"),
    ("source_session_id", "TEXT"),
    ("user_id", "INTEGER"),
    ("org_id", "INTEGER"),
    ("event_id", "TEXT"),
    ("cursor", "BIGINT"),
    ("created_at", "BIGINT"),
];
// (table suffix, columns, snapshot ordering key)
const GATEWAY_TABLES: &[(&str, &[(&str, &str)], &str)] = &[
    ("neev_gateway_audience", AUDIENCE_COLS, "audience"),
    ("neev_gateway_relying_bindings", BINDING_COLS, "relying_ref"),
    ("neev_gateway_exchanges", EXCHANGE_COLS, "code_hash"),
    ("neev_gateway_relying_sessions", RELYING_COLS, "relying_ref"),
    ("neev_gateway_redemptions", REDEMPTION_COLS, "receipt_id"),
    ("neev_gateway_revocations", REVOCATION_COLS, "cursor"),
    ("neev_gateway_revoke_intents", INTENT_COLS, "intent_id"),
];
// FK-safe drop order (section 5): children before their gateway parents.
const DROP_ORDER: &[&str] = &[
    "neev_gateway_revoke_intents",
    "neev_gateway_redemptions",
    "neev_gateway_relying_sessions",
    "neev_gateway_exchanges",
    "neev_gateway_relying_bindings",
    "neev_gateway_revocations",
    "neev_gateway_audience",
];

fn token(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}
fn hash() -> String {
    "a".repeat(64)
}

async fn relation_exists(db: &DatabaseConnection, name: &str, kind: &str) -> bool {
    let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres {
        let relkind = match kind {
            "view" => "v",
            "index" => "i",
            _ => "r",
        };
        (
            "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1 AND c.relkind=$2",
            vec![name.into(), relkind.into()],
        )
    } else {
        (
            "SELECT 1 AS present FROM sqlite_master WHERE name=$1 AND type=$2",
            vec![name.into(), kind.into()],
        )
    };
    one(db, sql, values).await.unwrap().is_some()
}

async fn seed_native(db: &DatabaseConnection) {
    execute(
        db,
        "INSERT INTO neev_accounts(id,display_name,auth_revision,disabled,created_at) VALUES (101,'Fixture user',1,false,1000)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_organizations(id,owner_user_id,name,revision,disabled,created_at) VALUES (201,101,'Fixture org',1,false,1000)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_memberships(id,org_id,user_id,role,revision,active) VALUES (301,201,101,'admin',1,true)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at) VALUES ($1,$2,$3,101,1,201,301,1,1,1000,2000)",
        vec![SOURCE.into(), "b".repeat(64).into(), "c".repeat(64).into()],
    )
    .await
    .unwrap();
}

// Insert the full valid history graph (works on constrained fresh8 and on the
// unconstrained test fixture). Assumes native rows already exist.
async fn seed_history(db: &DatabaseConnection) {
    execute(db, "DELETE FROM neev_gateway_audience", vec![])
        .await
        .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_audience(audience,issuer,last_cursor,fetched_through,acknowledged_cursor,writer_epoch) VALUES ('neev-collaboration-v1','neev-native-v1',1,1,0,0)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ($1,'neev-collaboration-v1',$2,101,201,1120)",
        vec![RELYING.into(), SOURCE.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_exchanges(code_hash,audience,attempt_id,challenge,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,issued_at,code_expires_at,retain_until,consumed_at) VALUES ($1,'neev-collaboration-v1',$2,$3,$4,101,201,301,1,1,1,'admin',1000,1100,1160,1460,1120)",
        vec![hash().into(), token(0).into(), token(1).into(), SOURCE.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_relying_sessions(relying_ref,audience,issuer,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,display_name,org_name,verified_email,created_at,expires_at,issued_after_cursor) VALUES ($1,'neev-collaboration-v1','neev-native-v1',$2,101,201,301,1,1,1,'admin',1000,'Fixture user','Fixture org','fixture@example.test',1120,2000,0)",
        vec![RELYING.into(), SOURCE.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_redemptions(receipt_id,audience,code_hash,relying_ref,redeemed_at,retry_until) VALUES ($1,'neev-collaboration-v1',$2,$3,1120,1420)",
        vec![RECEIPT.into(), hash().into(), RELYING.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES ('neev-collaboration-v1',1,$1,'session',101,201,$2,1200)",
        vec![EVENT.into(), SOURCE.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_gateway_revoke_intents(audience,intent_id,relying_ref,source_session_id,user_id,org_id,event_id,cursor,created_at) VALUES ('neev-collaboration-v1',$1,$2,$3,101,201,$4,1,1201)",
        vec![INTENT.into(), RELYING.into(), SOURCE.into(), EVENT.into()],
    )
    .await
    .unwrap();
}

async fn exact_seven_fixture(db: &DatabaseConnection) {
    // Drop later additive tables to reach the exact v7 inventory. Newest first:
    // mentions16 points at messages and attachments15 at turns and messages, so
    // neither can outlive them.
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
    for table in DROP_ORDER {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(db, "UPDATE neev_schema_version SET version=7", vec![])
        .await
        .unwrap();
    assert_eq!(validate_content_snapshot(db).await, Ok(7));
}

async fn unconstrained_gateway8_fixture(db: &DatabaseConnection) {
    exact_seven_fixture(db).await;
    for (table, cols, _) in GATEWAY_TABLES {
        let columns = cols
            .iter()
            .map(|(name, kind)| format!("{name} {kind}"))
            .collect::<Vec<_>>()
            .join(",");
        execute(db, &format!("CREATE TABLE {table} ({columns})"), vec![])
            .await
            .unwrap();
    }
    execute(db, "UPDATE neev_schema_version SET version=8", vec![])
        .await
        .unwrap();
    seed_native(db).await;
    seed_history(db).await;
}

fn concat_expr(cols: &[(&str, &str)]) -> String {
    cols.iter()
        .map(|(name, _)| format!("COALESCE(CAST({name} AS TEXT),'~')"))
        .collect::<Vec<_>>()
        .join("||'|'||")
}

async fn snapshot_gateway8_rows(db: &DatabaseConnection) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (table, cols, order) in GATEWAY_TABLES {
        let rows = db
            .query_all(statement(
                db,
                &format!(
                    "SELECT {} AS r FROM {table} ORDER BY {order}",
                    concat_expr(cols)
                ),
                vec![],
            ))
            .await
            .unwrap();
        out.push(
            rows.iter()
                .map(|row| row.try_get::<String>("", "r").unwrap())
                .collect(),
        );
    }
    out
}

// ---- Task A: gate, migration and constraints ----

#[tokio::test]
async fn sqlite_gateway8_fresh_inventory_and_seed() {
    let database = h::empty_database().await;
    initialize(&database).await.unwrap();
    assert_eq!(
        validate_content_snapshot(&database).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
    let row = one(
        &database,
        "SELECT audience,issuer,last_cursor,fetched_through,acknowledged_cursor,writer_epoch FROM neev_gateway_audience",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.try_get::<String>("", "audience").unwrap(),
        "neev-collaboration-v1"
    );
    assert_eq!(
        row.try_get::<String>("", "issuer").unwrap(),
        "neev-native-v1"
    );
    for column in [
        "last_cursor",
        "fetched_through",
        "acknowledged_cursor",
        "writer_epoch",
    ] {
        assert_eq!(row.try_get::<i64>("", column).unwrap(), 0);
    }
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_fresh_inventory_and_seed() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 fresh owned schema: {schema}");
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
    let row = one(
        &db,
        "SELECT audience,issuer FROM neev_gateway_audience",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.try_get::<String>("", "audience").unwrap(),
        "neev-collaboration-v1"
    );
    assert_eq!(
        row.try_get::<String>("", "issuer").unwrap(),
        "neev-native-v1"
    );
    h::cleanup_postgres(db, admin, schema).await;
}

async fn assert_exact_columns(db: &DatabaseConnection) {
    for (table, cols, _) in GATEWAY_TABLES {
        let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres
        {
            ("SELECT column_name AS name FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY ordinal_position", vec![(*table).into()])
        } else {
            (
                "SELECT name FROM pragma_table_info($1) ORDER BY cid",
                vec![(*table).into()],
            )
        };
        let actual: Vec<String> = db
            .query_all(statement(db, sql, values))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get("", "name").unwrap())
            .collect();
        let expected: Vec<String> = cols.iter().map(|(name, _)| (*name).to_owned()).collect();
        assert_eq!(actual, expected, "unexpected columns in {table}");
        let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres
        {
            ("SELECT column_default FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1", vec![(*table).into()])
        } else {
            (
                "SELECT dflt_value AS column_default FROM pragma_table_info($1)",
                vec![(*table).into()],
            )
        };
        assert!(
            db.query_all(statement(db, sql, values))
                .await
                .unwrap()
                .iter()
                .all(|row| row
                    .try_get::<Option<String>>("", "column_default")
                    .unwrap()
                    .is_none()),
            "automatic defaults in {table}"
        );
    }
}

async fn constraints_and_keys(db: &DatabaseConnection) {
    seed_native(db).await;
    seed_history(db).await;
    // The full valid graph must pass the read-only validator.
    assert_eq!(gateway_migration::validate(db).await, Ok(()));
    // Seven declared indexes exist besides the primary/unique keys.
    for name in [
        "uq_neev_gateway_revocations_session",
        "ix_neev_gateway_exchanges_source_expiry",
        "ix_neev_gateway_exchanges_retention",
        "ix_neev_gateway_relying_sessions_source_expiry",
        "ix_neev_gateway_relying_sessions_expiry",
        "ix_neev_gateway_redemptions_retry",
        "ix_neev_gateway_revocations_scope",
    ] {
        assert!(
            relation_exists(db, name, "index").await,
            "missing index {name}"
        );
    }
    // RESTRICT foreign keys: neither the source session nor the audience can be
    // deleted while a permanent binding references them.
    assert!(
        execute(
            db,
            "DELETE FROM neev_sessions WHERE id=$1",
            vec![SOURCE.into()]
        )
        .await
        .is_err(),
        "permanent source FK must block session deletion"
    );
    assert!(
        execute(
            db,
            "DELETE FROM neev_gateway_audience WHERE audience='neev-collaboration-v1'",
            vec![]
        )
        .await
        .is_err(),
        "gateway children must block audience deletion"
    );
    // One invalid value per owned transaction; every attempt fails and rolls back.
    for sql in [
        // native id domain
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ('66666666-6666-4666-8666-666666666666','neev-collaboration-v1','11111111-1111-4111-8111-111111111111',0,201,1120)",
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ('66666666-6666-4666-8666-666666666666','neev-collaboration-v1','11111111-1111-4111-8111-111111111111',2147483648,201,1120)",
        // binding relying_ref must differ from source
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ('11111111-1111-4111-8111-111111111111','neev-collaboration-v1','11111111-1111-4111-8111-111111111111',101,201,1120)",
        // wrong audience
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ('66666666-6666-4666-8666-666666666666','other','11111111-1111-4111-8111-111111111111',101,201,1120)",
        // revision domain
        "INSERT INTO neev_gateway_relying_sessions(relying_ref,audience,issuer,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,display_name,org_name,verified_email,created_at,expires_at,issued_after_cursor) VALUES ('66666666-6666-4666-8666-666666666666','neev-collaboration-v1','neev-native-v1','11111111-1111-4111-8111-111111111111',101,201,301,0,1,1,'admin',1000,'n','o','fixture@example.test',1120,2000,0)",
        // wrong issuer
        "INSERT INTO neev_gateway_relying_sessions(relying_ref,audience,issuer,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,display_name,org_name,verified_email,created_at,expires_at,issued_after_cursor) VALUES ('66666666-6666-4666-8666-666666666666','neev-collaboration-v1','evil','11111111-1111-4111-8111-111111111111',101,201,301,1,1,1,'admin',1000,'n','o','fixture@example.test',1120,2000,0)",
        // malformed hash
        "INSERT INTO neev_gateway_exchanges(code_hash,audience,attempt_id,challenge,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,issued_at,code_expires_at,retain_until,consumed_at) VALUES ('zz','neev-collaboration-v1','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','11111111-1111-4111-8111-111111111111',101,201,301,1,1,1,'admin',1000,1100,1160,1460,NULL)",
        // TTL window overflow (code_expires_at > issued_at+60)
        "INSERT INTO neev_gateway_exchanges(code_hash,audience,attempt_id,challenge,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,issued_at,code_expires_at,retain_until,consumed_at) VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb1','neev-collaboration-v1','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','11111111-1111-4111-8111-111111111111',101,201,301,1,1,1,'admin',1000,1100,1161,1460,NULL)",
        // retain_until must equal issued_at+360
        "INSERT INTO neev_gateway_exchanges(code_hash,audience,attempt_id,challenge,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,issued_at,code_expires_at,retain_until,consumed_at) VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2','neev-collaboration-v1','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','11111111-1111-4111-8111-111111111111',101,201,301,1,1,1,'admin',1000,1100,1160,1461,NULL)",
        // event cursor 0 (must be > 0)
        "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES ('neev-collaboration-v1',0,'77777777-7777-4777-8777-777777777777','account',101,NULL,NULL,1200)",
        // nullable-scope combination: account scope with org_id present
        "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES ('neev-collaboration-v1',2,'77777777-7777-4777-8777-777777777777','account',101,201,NULL,1200)",
        // duplicate session event for the same source
        "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES ('neev-collaboration-v1',2,'88888888-8888-4888-8888-888888888888','session',101,201,'11111111-1111-4111-8111-111111111111',1200)",
        // intent cursor 0 (must be > 0)
        "INSERT INTO neev_gateway_revoke_intents(audience,intent_id,relying_ref,source_session_id,user_id,org_id,event_id,cursor,created_at) VALUES ('neev-collaboration-v1','99999999-9999-4999-8999-999999999999','22222222-2222-4222-8222-222222222222','11111111-1111-4111-8111-111111111111',101,201,'44444444-4444-4444-8444-444444444444',0,1201)",
    ] {
        let tx = db.begin().await.unwrap();
        assert!(
            execute(&tx, sql, vec![]).await.is_err(),
            "constraint accepted invalid row: {sql}"
        );
        tx.rollback().await.unwrap();
    }
    // The valid graph and its seed row are untouched by the rejected attempts.
    assert_eq!(gateway_migration::validate(db).await, Ok(()));
}

#[tokio::test]
async fn sqlite_gateway8_columns_keys_and_constraints() {
    let db = h::database().await;
    assert_exact_columns(&db).await;
    constraints_and_keys(&db).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_columns_keys_and_constraints() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 constraints owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(async {
        assert_exact_columns(&db).await;
        constraints_and_keys(&db).await;
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

// ---- Task B: imported history and bounded validator ----

async fn assert_import_rejects(corruptions: &[&str]) {
    for sql in corruptions {
        let db = h::database().await;
        unconstrained_gateway8_fixture(&db).await;
        assert_eq!(
            gateway_migration::validate(&db).await,
            Ok(()),
            "seed graph must start valid before {sql}"
        );
        execute(&db, sql, vec![]).await.unwrap();
        let before = snapshot_gateway8_rows(&db).await;
        assert_eq!(
            gateway_migration::validate(&db).await,
            Err(AuthError::Unavailable),
            "validator accepted imported corruption: {sql}"
        );
        assert_eq!(
            snapshot_gateway8_rows(&db).await,
            before,
            "validator repaired corrupt data: {sql}"
        );
    }
}

#[tokio::test]
async fn sqlite_gateway8_imported_scalars_and_scopes() {
    assert_import_rejects(&[
        // UUID variant/version/case/hyphen corruption
        "UPDATE neev_gateway_relying_bindings SET relying_ref='22222222-2222-1222-8222-222222222222'",
        "UPDATE neev_gateway_relying_bindings SET relying_ref='22222222-2222-4222-c222-222222222222'",
        "UPDATE neev_gateway_relying_bindings SET relying_ref='22222222-2222-4222-8222-22222222222A'",
        "UPDATE neev_gateway_relying_bindings SET source_session_id='11111111111111114111811111111111111'",
        // token pad-bit corruption on attempt/challenge
        "UPDATE neev_gateway_exchanges SET attempt_id='AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB'",
        // hash alphabet
        "UPDATE neev_gateway_exchanges SET code_hash='g'||substr(code_hash,2)",
        // negative / future times
        "UPDATE neev_gateway_exchanges SET issued_at=-1",
        "UPDATE neev_gateway_relying_sessions SET expires_at=253402300800",
        // float / blob numeric storage in a required integer field
        "UPDATE neev_gateway_relying_bindings SET user_id=1.5",
        "UPDATE neev_gateway_relying_bindings SET org_id=X'0102'",
        // NULL in required fields
        "UPDATE neev_gateway_relying_bindings SET created_at=NULL",
        "UPDATE neev_gateway_relying_sessions SET verified_email=NULL",
        // audience / issuer identity
        "UPDATE neev_gateway_audience SET audience='wrong'",
        "UPDATE neev_gateway_audience SET issuer='wrong'",
        "UPDATE neev_gateway_relying_sessions SET issuer='wrong'",
        // audience cursor ordering / epoch rule
        "UPDATE neev_gateway_audience SET acknowledged_cursor=1,writer_epoch=0",
        "UPDATE neev_gateway_audience SET fetched_through=2",
        // event scope shape
        "UPDATE neev_gateway_revocations SET scope='account'",
        // display syntax: native-valid but gateway-invalid email/name
        "UPDATE neev_gateway_relying_sessions SET verified_email='a!b@example.test'",
        "UPDATE neev_gateway_relying_sessions SET verified_email='a@example.1'",
        "UPDATE neev_gateway_relying_sessions SET display_name=printf('%.257d',0)",
        "UPDATE neev_gateway_relying_sessions SET verified_email=printf('%.255d',0)||'@example.test'",
        "UPDATE neev_gateway_relying_sessions SET verified_email='FIXTURE@example.test'",
    ])
    .await;
}

#[tokio::test]
async fn sqlite_gateway8_imported_parent_and_receipt_mismatch() {
    assert_import_rejects(&[
        // missing native source-session
        "DELETE FROM neev_sessions WHERE id='11111111-1111-4111-8111-111111111111'",
        // binding user/org disagree with the native source
        "UPDATE neev_gateway_relying_bindings SET user_id=102",
        // captured revisions disagree with the native source-session
        "UPDATE neev_gateway_exchanges SET account_revision=2",
        "UPDATE neev_gateway_relying_sessions SET member_revision=2",
        // relying expiry must equal the captured source expiry
        "UPDATE neev_gateway_relying_sessions SET expires_at=1999",
        // cross-audience substitution of a binding referenced by relying rows
        "UPDATE neev_gateway_relying_bindings SET audience='neev-collaboration-v1 '",
        // receipt on an unconsumed code
        "UPDATE neev_gateway_exchanges SET consumed_at=NULL",
        // receipt time/cutoff mismatch
        "UPDATE neev_gateway_redemptions SET redeemed_at=1121,retry_until=1421",
        // intent points at a non-session event
        "UPDATE neev_gateway_revocations SET scope='membership',source_session_id=NULL",
        // duplicate primary/unique keys
        "INSERT INTO neev_gateway_relying_bindings SELECT * FROM neev_gateway_relying_bindings",
        "INSERT INTO neev_gateway_revocations SELECT * FROM neev_gateway_revocations",
    ])
    .await;
}

#[tokio::test]
async fn sqlite_gateway8_binding_only_history_is_valid() {
    // A permanent binding with no live relying row, receipt, event or intent.
    let db = h::database().await;
    seed_native(&db).await;
    execute(
        &db,
        "UPDATE neev_gateway_audience SET last_cursor=0,fetched_through=0",
        vec![],
    )
    .await
    .unwrap();
    execute(
        &db,
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ($1,'neev-collaboration-v1',$2,101,201,1120)",
        vec![RELYING.into(), SOURCE.into()],
    )
    .await
    .unwrap();
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));

    // Retained intent+event+binding after every expirable row is pruned in
    // FK-safe order; no clock is consulted to justify the pruning.
    let db = h::database().await;
    seed_native(&db).await;
    seed_history(&db).await;
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
    for table in [
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
    ] {
        execute(&db, &format!("DELETE FROM {table}"), vec![])
            .await
            .unwrap();
    }
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
    // A consumed exchange without a receipt is also valid.
    let db = h::database().await;
    seed_native(&db).await;
    seed_history(&db).await;
    execute(&db, "DELETE FROM neev_gateway_redemptions", vec![])
        .await
        .unwrap();
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
    // Later native revocation/role change is legitimate historical state.
    execute(
        &db,
        "UPDATE neev_accounts SET auth_revision=9 WHERE id=101",
        vec![],
    )
    .await
    .unwrap();
    execute(
        &db,
        "UPDATE neev_memberships SET active=false,role='member' WHERE id=301",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
}

async fn seed_keyset_history(db: &DatabaseConnection, count: usize) {
    // Distinct native sessions so each binding/event/intent shares one source.
    let uuid = |namespace: u8, index: usize| {
        let mut bytes = [0u8; 16];
        bytes[0] = namespace;
        bytes[12..16].copy_from_slice(&(index as u32).to_be_bytes());
        bytes[6] = 0x40 | (bytes[6] & 0x0f);
        bytes[8] = 0x80 | (bytes[8] & 0x3f);
        uuid::Uuid::from_bytes(bytes).to_string()
    };
    seed_native(db).await;
    execute(
        db,
        &format!("UPDATE neev_gateway_audience SET last_cursor={count},fetched_through={count}"),
        vec![],
    )
    .await
    .unwrap();
    for index in 0..count {
        let source = uuid(0x51, index);
        let relying = uuid(0x52, index);
        let event = uuid(0x53, index);
        let intent = uuid(0x54, index);
        let cursor = (index + 1) as i64;
        execute(
            db,
            "INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at) VALUES ($1,$2,$3,101,1,201,301,1,1,1000,2000)",
            vec![
                source.clone().into(),
                format!("{index:064x}").into(),
                format!("{:064x}", index + 1_000_000).into(),
            ],
        )
        .await
        .unwrap();
        execute(
            db,
            "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES ($1,'neev-collaboration-v1',$2,101,201,1120)",
            vec![relying.clone().into(), source.clone().into()],
        )
        .await
        .unwrap();
        execute(
            db,
            "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES ('neev-collaboration-v1',$1,$2,'session',101,201,$3,1200)",
            vec![cursor.into(), event.clone().into(), source.clone().into()],
        )
        .await
        .unwrap();
        execute(
            db,
            "INSERT INTO neev_gateway_revoke_intents(audience,intent_id,relying_ref,source_session_id,user_id,org_id,event_id,cursor,created_at) VALUES ('neev-collaboration-v1',$1,$2,$3,101,201,$4,$5,1201)",
            vec![
                intent.into(),
                relying.into(),
                source.into(),
                event.into(),
                cursor.into(),
            ],
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn sqlite_gateway8_keyset_history_crosses_pages() {
    // 513 rows each crosses the two 256-row page boundaries.
    let db = h::database().await;
    seed_keyset_history(&db, 513).await;
    gateway_migration::MAX_PAGE_LEN.store(0, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
    let max = gateway_migration::MAX_PAGE_LEN.load(std::sync::atomic::Ordering::SeqCst);
    assert!(max == 256, "expected a full 256-row page, observed {max}");

    // Independently corrupt at each page boundary; every case is rejected while
    // still crossing the 256-row pages, and no data is modified by validation.
    for position in [255usize, 256, 511, 512] {
        // Event contiguity gap: delete one cursor (and its dependent intent, to
        // respect the RESTRICT foreign key) so a later page observes the hole.
        let db = h::database().await;
        seed_keyset_history(&db, 513).await;
        let cursor = (position + 1) as i64;
        execute(
            &db,
            "DELETE FROM neev_gateway_revoke_intents WHERE cursor=$1",
            vec![cursor.into()],
        )
        .await
        .unwrap();
        execute(
            &db,
            "DELETE FROM neev_gateway_revocations WHERE cursor=$1",
            vec![cursor.into()],
        )
        .await
        .unwrap();
        let before = snapshot_gateway8_rows(&db).await;
        assert_eq!(
            gateway_migration::validate(&db).await,
            Err(AuthError::Unavailable),
            "event gap at {position} accepted"
        );
        assert_eq!(
            snapshot_gateway8_rows(&db).await,
            before,
            "validation modified data at {position}"
        );
    }
}

// ---- Task C: atomic version chain and read-only history ----

async fn downgrade_to(db: &DatabaseConnection, target: i64) {
    // Remove current additive layers before reconstructing historical v16.
    super::tests::drop_tables_after_seventeen(db).await;
    // Peel back one additive layer per version.
    // 16 and 15 are TABLES, so they come off with DROP TABLE. 14, 13 and 12 are
    // COLUMNS on the session tables rather than tables of their own, so they
    // come off with DROP COLUMN and the tables survive.
    if target < 16 {
        super::tests::drop_mention_tables(db).await;
    }
    if target < 15 {
        super::tests::drop_attachment_tables(db).await;
    }
    if target < 14 {
        for column in ["fast", "effort"] {
            execute(
                db,
                &format!("ALTER TABLE neev_agent_sessions DROP COLUMN {column}"),
                vec![],
            )
            .await
            .unwrap();
        }
    }
    if target < 13 {
        execute(db, "ALTER TABLE neev_agent_turns DROP COLUMN activity", vec![])
            .await
            .unwrap();
    }
    if target < 12 {
        execute(
            db,
            "ALTER TABLE neev_agent_sessions DROP COLUMN node_id",
            vec![],
        )
        .await
        .unwrap();
    }
    if target < 11 {
        for table in ["neev_agent_turns", "neev_agent_sessions"] {
            execute(db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if target < 10 {
        execute(db, "DROP TABLE neev_image_blobs", vec![])
            .await
            .unwrap();
    }
    if target < 9 {
        execute(db, "DROP TABLE neev_org_join_codes", vec![])
            .await
            .unwrap();
    }
    if target < 8 {
        for table in DROP_ORDER {
            execute(db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if target < 7 {
        for table in ["neev_agent_status_checks", "neev_agent_status_owner"] {
            execute(db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if target < 6 {
        execute(db, "DROP TABLE neev_document_formats", vec![])
            .await
            .unwrap();
    }
    if target < 5 {
        for table in [
            "neev_agent_operations",
            "neev_agent_attempts",
            "neev_agent_connections",
        ] {
            execute(db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    execute(
        db,
        "UPDATE neev_schema_version SET version=$1",
        vec![target.into()],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn sqlite_gateway8_populated_seven_upgrade_preserves_rows() {
    let db = h::database().await;
    // Populate native document/revision/format plus status owner/check rows.
    let user = h::native_account(&db, "gw-owner", "gw-owner@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    execute(&db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(801,$1,NULL,'project','Preserved',1,$2)", vec![org.into(), user.into()]).await.unwrap();
    let doc = "70000000-0000-4000-8000-000000000007";
    execute(&db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',801,$3,'History','private',1,1,false,1000,1001)", vec![org.into(), doc.into(), user.into()]).await.unwrap();
    execute(&db, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at) VALUES($1,$2,'document',1,'blob',$3,17,$4,1001)", vec![org.into(), doc.into(), "a".repeat(64).into(), user.into()]).await.unwrap();
    execute(&db, "INSERT INTO neev_document_formats(org_id,document_id,revision,format,format_version) VALUES($1,$2,1,'blocks',1)", vec![org.into(), doc.into()]).await.unwrap();

    exact_seven_fixture(&db).await;
    let before = one(&db, "SELECT r.blob_key,r.sha256,f.format FROM neev_document_revisions r JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE r.document_id=$1", vec![doc.into()]).await.unwrap().unwrap();
    let before: (String, String, String) = (
        before.try_get("", "blob_key").unwrap(),
        before.try_get("", "sha256").unwrap(),
        before.try_get("", "format").unwrap(),
    );

    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
    let after = one(&db, "SELECT r.blob_key,r.sha256,f.format FROM neev_document_revisions r JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE r.document_id=$1", vec![doc.into()]).await.unwrap().unwrap();
    assert_eq!(
        (
            after.try_get::<String>("", "blob_key").unwrap(),
            after.try_get::<String>("", "sha256").unwrap(),
            after.try_get::<String>("", "format").unwrap(),
        ),
        before
    );
    // One seeded audience and six initially empty gateway tables.
    assert_eq!(h::count(&db, "neev_gateway_audience").await, 1);
    for (table, _, _) in &GATEWAY_TABLES[1..] {
        assert_eq!(
            h::count(&db, table).await,
            0,
            "{table} not empty after upgrade"
        );
    }
}

#[tokio::test]
async fn sqlite_gateway8_reinitialize_preserves_history() {
    let db = h::database().await;
    seed_native(&db).await;
    seed_history(&db).await;
    // Advance the audience to nonzero cursor/fetch/ack/epoch.
    execute(
        &db,
        "UPDATE neev_gateway_audience SET acknowledged_cursor=1,writer_epoch=5",
        vec![],
    )
    .await
    .unwrap();
    let before = snapshot_gateway8_rows(&db).await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
    assert_eq!(
        snapshot_gateway8_rows(&db).await,
        before,
        "reinitialize reset gateway history"
    );
}

#[tokio::test]
async fn sqlite_gateway8_initializer_marker_failure_rolls_back() {
    let db = h::database().await;
    seed_native(&db).await;
    let account_before = h::count(&db, "neev_accounts").await;
    exact_seven_fixture(&db).await;
    execute(&db, &format!("CREATE TRIGGER fixture_reject_gateway8_marker BEFORE UPDATE OF version ON neev_schema_version WHEN NEW.version={CURRENT_SCHEMA_VERSION} BEGIN SELECT RAISE(ABORT,'fixture marker rejection'); END"), vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        one(
            &db,
            "SELECT version FROM neev_schema_version WHERE id=1",
            vec![]
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "version")
        .unwrap(),
        7
    );
    for table in DROP_ORDER {
        assert!(
            !relation_exists(&db, table, "table").await,
            "leaked {table}"
        );
    }
    assert_eq!(h::count(&db, "neev_accounts").await, account_before);
    execute(&db, "DROP TRIGGER fixture_reject_gateway8_marker", vec![])
        .await
        .unwrap();

    // Seed/DDL atomicity independent of the marker: roll back an explicit
    // transaction after gateway_migration::upgrade succeeds.
    let db = h::database().await;
    exact_seven_fixture(&db).await;
    let tx = db.begin().await.unwrap();
    gateway_migration::upgrade(&tx).await.unwrap();
    assert!(execute(&tx, "THIS IS NOT VALID SQL", vec![]).await.is_err());
    tx.rollback().await.unwrap();
    for table in DROP_ORDER {
        assert!(
            !relation_exists(&db, table, "table").await,
            "rolled-back upgrade leaked {table}"
        );
    }
}

#[tokio::test]
async fn sqlite_gateway8_exact_historical_three_through_eight_read_only() {
    for target in [16_i64, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3] {
        let db = h::database().await;
        downgrade_to(&db, target).await;
        execute(&db, "PRAGMA query_only=ON", vec![]).await.unwrap();
        assert_eq!(validate_content_snapshot(&db).await, Ok(target));
        execute(&db, "PRAGMA query_only=OFF", vec![]).await.unwrap();
        assert_eq!(
            one(
                &db,
                "SELECT version FROM neev_schema_version WHERE id=1",
                vec![]
            )
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "version")
            .unwrap(),
            target
        );
    }
}

#[tokio::test]
async fn sqlite_gateway8_partial_marker_and_non_table_rejected() {
    // Missing one gateway table.
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_gateway_revoke_intents", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable)
    );

    // Full v8 inventory with a stale marker.
    let db = h::database().await;
    execute(&db, "UPDATE neev_schema_version SET version=7", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable)
    );

    // Extra marker row: the production CHECK(id=1) forbids a second row, so an
    // imported snapshot is simulated by rebuilding the marker table unconstrained.
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_schema_version", vec![])
        .await
        .unwrap();
    execute(
        &db,
        "CREATE TABLE neev_schema_version (id INTEGER PRIMARY KEY, version BIGINT NOT NULL)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        &db,
        "INSERT INTO neev_schema_version(id,version) VALUES (1,8),(2,8)",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable)
    );
    // Initialize must also reject the extra marker without mutating (R10).
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        h::count(&db, "neev_schema_version").await,
        2,
        "extra marker rejection must not mutate the marker table"
    );

    // Unknown extra table.
    let db = h::database().await;
    execute(&db, "CREATE TABLE unknown_gateway_data(id INTEGER)", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));

    // A same-name view must not pass as native gateway storage.
    let db = h::database().await;
    execute(&db, "DROP TABLE neev_gateway_revoke_intents", vec![])
        .await
        .unwrap();
    execute(
        &db,
        "CREATE VIEW neev_gateway_revoke_intents AS SELECT 1 AS audience",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        validate_content_snapshot(&db).await,
        Err(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn sqlite_gateway8_rejects_duplicate_native_parent() {
    let db = h::database().await;
    unconstrained_gateway8_fixture(&db).await;
    assert_eq!(gateway_migration::validate(&db).await, Ok(()));
    // Simulate an imported snapshot whose native membership table lacks the
    // composite unique constraint, holding a duplicate matching parent (R11).
    execute(&db, "PRAGMA foreign_keys=OFF", vec![])
        .await
        .unwrap();
    execute(
        &db,
        "CREATE TABLE fixture_m AS SELECT * FROM neev_memberships",
        vec![],
    )
    .await
    .unwrap();
    execute(&db, "DROP TABLE neev_memberships", vec![])
        .await
        .unwrap();
    execute(
        &db,
        "CREATE TABLE neev_memberships(id,org_id,user_id,role,revision,active)",
        vec![],
    )
    .await
    .unwrap();
    execute(
        &db,
        "INSERT INTO neev_memberships SELECT * FROM fixture_m",
        vec![],
    )
    .await
    .unwrap();
    execute(
        &db,
        "INSERT INTO neev_memberships(id,org_id,user_id,role,revision,active) VALUES(301,201,101,'admin',1,true)",
        vec![],
    )
    .await
    .unwrap();
    let before = snapshot_gateway8_rows(&db).await;
    assert_eq!(
        gateway_migration::validate(&db).await,
        Err(AuthError::Unavailable),
        "duplicate composite membership parent must be rejected"
    );
    assert_eq!(snapshot_gateway8_rows(&db).await, before);
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_populated_seven_upgrade_preserves_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 populated7 upgrade owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(async {
        let user = h::native_account(&db, "gw-owner", "gw-owner@example.test").await;
        let (org, _) = h::organization(&db, user, "admin").await;
        execute(&db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(801,$1,NULL,'project','Preserved',1,$2)", vec![org.into(), user.into()]).await.unwrap();
        let doc = "70000000-0000-4000-8000-000000000007";
        execute(&db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',801,$3,'History','private',1,1,false,1000,1001)", vec![org.into(), doc.into(), user.into()]).await.unwrap();
        execute(&db, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at) VALUES($1,$2,'document',1,'blob',$3,17,$4,1001)", vec![org.into(), doc.into(), "a".repeat(64).into(), user.into()]).await.unwrap();
        execute(&db, "INSERT INTO neev_document_formats(org_id,document_id,revision,format,format_version) VALUES($1,$2,1,'blocks',1)", vec![org.into(), doc.into()]).await.unwrap();
        // Populated status owner + check (Task5A rows) must also survive.
        let conn = "10000000000000000000000000000001";
        let handle = "40000000000000000000000000000004";
        execute(&db, "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,'codex',1,'connected','acct',1700000000,NULL,$4,1000,1000)", vec![conn.into(), org.into(), user.into(), handle.into()]).await.unwrap();
        execute(&db, "UPDATE neev_agent_status_owner SET epoch=1,boot_id='30000000000000000000000000000003',phase='ready' WHERE id=1", vec![]).await.unwrap();
        execute(&db, "INSERT INTO neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at) VALUES($1,'20000000000000000000000000000002',1,'50000000-0000-4000-8000-000000000005',1,$2,0,60)", vec![conn.into(), handle.into()]).await.unwrap();

        let before = one(&db, "SELECT r.blob_key,r.sha256,f.format FROM neev_document_revisions r JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE r.document_id=$1", vec![doc.into()]).await.unwrap().unwrap();
        let before: (String, String, String) = (
            before.try_get("", "blob_key").unwrap(),
            before.try_get("", "sha256").unwrap(),
            before.try_get("", "format").unwrap(),
        );
        exact_seven_fixture(&db).await;
        assert_eq!(initialize(&db).await, Ok(()));
        assert_eq!(
            validate_content_snapshot(&db).await,
            Ok(CURRENT_SCHEMA_VERSION)
        );
        // Content revision/format preserved unchanged.
        let after = one(&db, "SELECT r.blob_key,r.sha256,f.format FROM neev_document_revisions r JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision WHERE r.document_id=$1", vec![doc.into()]).await.unwrap().unwrap();
        assert_eq!(
            (
                after.try_get::<String>("", "blob_key").unwrap(),
                after.try_get::<String>("", "sha256").unwrap(),
                after.try_get::<String>("", "format").unwrap(),
            ),
            before
        );
        // Status owner phase and check preserved.
        let owner = one(&db, "SELECT phase,epoch FROM neev_agent_status_owner WHERE id=1", vec![]).await.unwrap().unwrap();
        assert_eq!(owner.try_get::<String>("", "phase").unwrap(), "ready");
        assert_eq!(owner.try_get::<i64>("", "epoch").unwrap(), 1);
        assert_eq!(h::count(&db, "neev_agent_status_checks").await, 1);
        // One seeded audience row and six initially empty gateway tables.
        assert_eq!(h::count(&db, "neev_gateway_audience").await, 1);
        for (table, _, _) in &GATEWAY_TABLES[1..] {
            assert_eq!(h::count(&db, table).await, 0, "{table} not empty after upgrade");
        }
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_reinitialize_preserves_history() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 reinitialize owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(async {
        seed_native(&db).await;
        seed_history(&db).await;
        execute(
            &db,
            "UPDATE neev_gateway_audience SET acknowledged_cursor=1,writer_epoch=5",
            vec![],
        )
        .await
        .unwrap();
        let before = snapshot_gateway8_rows(&db).await;
        assert_eq!(initialize(&db).await, Ok(()));
        assert_eq!(
            validate_content_snapshot(&db).await,
            Ok(CURRENT_SCHEMA_VERSION)
        );
        assert_eq!(snapshot_gateway8_rows(&db).await, before);
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_initializer_marker_failure_rolls_back() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 marker-failure owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(async {
        let user = h::native_account(&db, "marker-owner", "marker-owner@example.test").await;
        let account_before = h::count(&db, "neev_accounts").await;
        exact_seven_fixture(&db).await;
        let inventory_before = h::count(&db, "neev_document_formats").await; // status7 content present
        execute(
            &db,
            &format!("ALTER TABLE neev_schema_version ADD CONSTRAINT ck_fixture_reject_gateway8_marker CHECK (version<>{CURRENT_SCHEMA_VERSION})"),
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
        // Original 7 marker, inventory and native data preserved unchanged.
        assert_eq!(
            one(&db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "version")
                .unwrap(),
            7
        );
        assert_eq!(validate_content_snapshot(&db).await, Ok(7));
        assert_eq!(h::count(&db, "neev_accounts").await, account_before);
        assert_eq!(h::count(&db, "neev_document_formats").await, inventory_before);
        assert!(
            one(&db, "SELECT id FROM neev_accounts WHERE id=$1", vec![user.into()])
                .await
                .unwrap()
                .is_some()
        );
        for table in DROP_ORDER {
            assert!(!relation_exists(&db, table, "table").await, "leaked {table}");
        }
        execute(
            &db,
            "ALTER TABLE neev_schema_version DROP CONSTRAINT ck_fixture_reject_gateway8_marker",
            vec![],
        )
        .await
        .unwrap();
        // Seed/DDL atomicity independent of the marker: roll back an explicit
        // transaction after gateway_migration::upgrade succeeds.
        let tx = db.begin().await.unwrap();
        gateway_migration::upgrade(&tx).await.unwrap();
        assert!(execute(&tx, "THIS IS NOT VALID SQL", vec![]).await.is_err());
        tx.rollback().await.unwrap();
        for table in DROP_ORDER {
            assert!(
                !relation_exists(&db, table, "table").await,
                "rolled-back upgrade leaked {table}"
            );
        }
        assert_eq!(validate_content_snapshot(&db).await, Ok(7));
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_exact_historical_three_through_eight_read_only() {
    for target in [11_i64, 10, 9, 8, 7, 6, 5, 4, 3] {
        let (db, admin, schema) = h::postgres_database().await;
        eprintln!("gateway8 read-only {target} owned schema: {schema}");
        let outcome = std::panic::AssertUnwindSafe(async {
            downgrade_to(&db, target).await;
            let tx = db.begin().await.unwrap();
            execute(&tx, "SET TRANSACTION READ ONLY", vec![])
                .await
                .unwrap();
            assert_eq!(validate_content_snapshot(&tx).await, Ok(target));
            tx.rollback().await.unwrap();
        })
        .catch_unwind()
        .await;
        finish(db, admin, schema, outcome).await;
    }
}

async fn pg_reject_case(label: &str, setup: &str) {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("gateway8 rejection [{label}] owned schema: {schema}");
    let setup = setup.to_owned();
    let outcome = std::panic::AssertUnwindSafe(async {
        for statement in setup.split('\n').filter(|s| !s.trim().is_empty()) {
            execute(&db, statement, vec![]).await.unwrap();
        }
        assert_eq!(
            initialize(&db).await,
            Err(AuthError::Unavailable),
            "initialize accepted [{label}]"
        );
        assert_eq!(
            validate_content_snapshot(&db).await,
            Err(AuthError::Unavailable),
            "snapshot accepted [{label}]"
        );
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_partial_marker_and_non_table_rejected() {
    // Missing gateway table.
    pg_reject_case("missing-table", "DROP TABLE neev_gateway_revoke_intents").await;
    // Stale marker over a full v8 inventory.
    pg_reject_case(
        "stale-marker",
        "UPDATE neev_schema_version SET version=7 WHERE id=1",
    )
    .await;
    // Unknown extra table.
    pg_reject_case(
        "extra-table",
        "CREATE TABLE unknown_gateway_data(id INTEGER)",
    )
    .await;
    // Same-name view where a gateway table is expected.
    pg_reject_case(
        "same-name-view",
        "DROP TABLE neev_gateway_revoke_intents\nCREATE VIEW neev_gateway_revoke_intents AS SELECT 1 AS audience",
    )
    .await;
    // Extra marker row (rebuild the marker table unconstrained to bypass CHECK id=1).
    pg_reject_case(
        "extra-marker",
        "DROP TABLE neev_schema_version CASCADE\nCREATE TABLE neev_schema_version(id INTEGER PRIMARY KEY, version BIGINT NOT NULL)\nINSERT INTO neev_schema_version(id,version) VALUES (1,8),(2,8)",
    )
    .await;
}

async fn finish(
    db: DatabaseConnection,
    admin: DatabaseConnection,
    schema: String,
    outcome: Result<(), Box<dyn std::any::Any + Send>>,
) {
    if let Err(panic) = outcome {
        eprintln!("retained failed gateway8 owned schema: {schema}");
        drop(db);
        drop(admin);
        std::panic::resume_unwind(panic);
    }
    h::cleanup_postgres(db, admin, schema).await;
}
