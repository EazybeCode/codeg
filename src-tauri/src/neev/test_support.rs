use super::types::{Provider, VerifiedIdentity};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

pub(super) struct TestClock(pub(super) AtomicI64);

impl TestClock {
    pub(super) fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }
}

impl super::clock::Clock for TestClock {
    fn now_unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn isolated_postgres_database_name(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") || parsed.fragment().is_some() {
        return None;
    }
    let encoded = parsed.path().strip_prefix('/')?;
    if encoded.is_empty() || encoded.contains('/') {
        return None;
    }
    let decoded = urlencoding::decode(encoded).ok()?.into_owned();
    (decoded == "neev_identity_test").then_some(decoded)
}

pub(super) async fn empty_database() -> DatabaseConnection {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.max_connections(1).sqlx_logging(false);
    Database::connect(options).await.unwrap()
}
pub(super) async fn database() -> DatabaseConnection {
    let db = empty_database().await;
    super::db::initialize(&db).await.unwrap();
    db
}
pub(super) fn google(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        provider: Provider::Google,
        issuer: "https://accounts.google.com".into(),
        subject: subject.into(),
        verified_email: Some(email.into()),
        display_name: "Test supporter".into(),
        avatar_url: None,
    }
}
pub(super) async fn execute(db: &DatabaseConnection, sql: &str, values: Vec<Value>) {
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        sql,
        values,
    ))
    .await
    .unwrap();
}
pub(super) async fn count(db: &DatabaseConnection, table: &str) -> i64 {
    db.query_one(Statement::from_string(
        db.get_database_backend(),
        format!("SELECT COUNT(*) AS n FROM {table}"),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}

pub(super) async fn native_account(db: &DatabaseConnection, subject: &str, email: &str) -> i32 {
    super::auth::identity::resolve(
        db,
        google(subject, email),
        super::types::SignInIntent::Login,
        1000,
    )
    .await
    .unwrap()
}

pub(super) async fn organization(db: &DatabaseConnection, user: i32, role: &str) -> (i32, i32) {
    let row = db.query_one(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO neev_organizations(owner_user_id,name,revision,disabled,created_at) VALUES($1,'Test org',1,false,1000) RETURNING id",
        vec![user.into()],
    )).await.unwrap().unwrap();
    let org: i32 = row.try_get("", "id").unwrap();
    let row = db.query_one(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,$3,1,true) RETURNING id",
        vec![org.into(), user.into(), role.into()],
    )).await.unwrap().unwrap();
    let member: i32 = row.try_get("", "id").unwrap();
    (org, member)
}

pub(super) fn preview_config() -> super::config::Config {
    let origin = url::Url::parse("http://127.0.0.1:3100/").unwrap();
    super::config::Config {
        origin: origin.clone(),
        origin_header: "http://127.0.0.1:3100".into(),
        google_callback: origin.join("auth/google/callback").unwrap(),
        github_callback: origin.join("auth/github/callback").unwrap(),
        database_url: "postgresql:///unused_test_database".into(),
        encryption_key: [7; 32],
        bind: "127.0.0.1:0".parse().unwrap(),
        static_root: std::env::current_dir().unwrap(),
        preview: true,
        providers: super::config::ProviderConfig {
            google: None,
            github: None,
            email: None,
        },
    }
}

pub(super) async fn http_fixture() -> (axum_test::TestServer, DatabaseConnection, Arc<TestClock>) {
    let database = database().await;
    let clock = Arc::new(TestClock::new(1000));
    let state = Arc::new(super::startup::ServerState::new(
        database.clone(),
        preview_config(),
        clock.clone(),
    ));
    let server = axum_test::TestServer::new(super::http::router(state)).unwrap();
    (server, database, clock)
}

pub(super) async fn http_server() -> axum_test::TestServer {
    http_fixture().await.0
}
pub(super) async fn session(
    db: &DatabaseConnection,
    user: i32,
    name: &str,
    authenticated: i64,
    revision: i64,
) {
    execute(db,"INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,authenticated_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",vec![name.into(),"1".repeat(64).into(),"2".repeat(64).into(),user.into(),revision.into(),authenticated.into(),(authenticated+3600).into()]).await;
}

/// Explicitly isolated PostgreSQL test schema; caller must close before cleanup.
pub(super) async fn postgres_database() -> (DatabaseConnection, DatabaseConnection, String) {
    let url =
        std::env::var("NEEV_TEST_DATABASE_URL").expect("isolated NEEV_TEST_DATABASE_URL required");
    assert_eq!(
        isolated_postgres_database_name(&url).as_deref(),
        Some("neev_identity_test"),
        "refuse non-test PostgreSQL database"
    );
    let mut opts = ConnectOptions::new(url.clone());
    opts.max_connections(1).sqlx_logging(false);
    let admin = Database::connect(opts).await.unwrap();
    let schema = format!("neev_test_{}", uuid::Uuid::new_v4().simple());
    execute(&admin, &format!("CREATE SCHEMA {schema}"), vec![]).await;
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(8)
        .sqlx_logging(false)
        .set_schema_search_path(&schema);
    let db = Database::connect(opts).await.unwrap();
    super::db::initialize(&db).await.unwrap();
    (db, admin, schema)
}
pub(super) async fn cleanup_postgres(
    db: DatabaseConnection,
    admin: DatabaseConnection,
    schema: String,
) {
    db.close().await.unwrap();
    assert!(
        schema.starts_with("neev_test_")
            && schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    execute(&admin, &format!("DROP SCHEMA {schema} CASCADE"), vec![]).await;
    admin.close().await.unwrap();
}

pub(super) async fn wait_for_postgres_lock(admin: &DatabaseConnection, query_pattern: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let row = admin
                .query_one(Statement::from_sql_and_values(
                    admin.get_database_backend(),
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1) AS waiting",
                    vec![query_pattern.into()],
                ))
                .await
                .unwrap()
                .unwrap();
            if row.try_get::<bool>("", "waiting").unwrap() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected PostgreSQL query to become observably lock-waiting");
}

#[cfg(test)]
mod tests {
    use super::isolated_postgres_database_name;

    #[test]
    fn postgres_guard_requires_the_exact_decoded_database_name() {
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///neev_identity_test?host=/tmp/neev-pg&user=air1"
            ),
            Some("neev_identity_test".into())
        );
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///%6Eeev_identity_test?host=/tmp/neev-pg&user=air1"
            ),
            Some("neev_identity_test".into())
        );
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///wrong_database?note=/neev_identity_test?host=/tmp/neev-pg"
            ),
            None
        );
    }
}
