use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use tokio::sync::Semaphore;

use crate::neev::{error::AuthError, test_support as h};

use super::{
    quota::{self, Bucket},
    MailMessage, MailSender, Service,
};

#[tokio::test]
async fn quota_boundary_refuses_without_partially_charging_other_buckets() {
    let db = h::database().await;
    let key = [23; 32];
    let ip: IpAddr = "203.0.113.8".parse().unwrap();
    let buckets = || {
        vec![
            Bucket::MagicAddress("person@example.test".into()),
            Bucket::MagicSourceIp(ip),
            Bucket::DeliveryMinute,
            Bucket::DeliveryDay,
        ]
    };
    for _ in 0..3 {
        quota::reserve(&db, &key, &buckets(), 1_700_000_000)
            .await
            .unwrap();
    }
    assert_eq!(
        quota::reserve(&db, &key, &buckets(), 1_700_000_000).await,
        Err(AuthError::Limited)
    );
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT kind,used FROM neev_quota_buckets ORDER BY kind".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        let used: i64 = row.try_get("", "used").unwrap();
        assert_eq!(used, 3, "a rejected reservation charged a sibling bucket");
    }
}

#[tokio::test]
async fn fixed_windows_reset_at_the_utc_boundary() {
    let db = h::database().await;
    let key = [24; 32];
    let bucket = Bucket::MagicAddress("boundary@example.test".into());
    for _ in 0..3 {
        quota::reserve(&db, &key, std::slice::from_ref(&bucket), 899)
            .await
            .unwrap();
    }
    assert_eq!(
        quota::reserve(&db, &key, std::slice::from_ref(&bucket), 899).await,
        Err(AuthError::Limited)
    );
    quota::reserve(&db, &key, &[bucket], 900).await.unwrap();
}

#[tokio::test]
async fn active_bucket_cap_refuses_without_growing_the_table() {
    let db = h::database().await;
    db.execute(Statement::from_string(
        db.get_database_backend(),
        "WITH RECURSIVE values_(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM values_ WHERE n<10000) INSERT INTO neev_quota_buckets(kind,key_hash,window_start,used,expires_at) SELECT 'bounded',printf('%064x',n),0,1,2000 FROM values_".to_owned(),
    ))
    .await
    .unwrap();
    assert_eq!(
        quota::reserve(&db, &[25; 32], &[Bucket::DeliveryMinute], 1000).await,
        Err(AuthError::Limited)
    );
    assert_eq!(h::count(&db, "neev_quota_buckets").await, 10_000);
}

#[tokio::test]
async fn quota_keys_do_not_store_plain_addresses_or_ips() {
    let db = h::database().await;
    quota::reserve(
        &db,
        &[26; 32],
        &[
            Bucket::MagicAddress("private@example.test".into()),
            Bucket::MagicSourceIp("203.0.113.99".parse().unwrap()),
        ],
        1000,
    )
    .await
    .unwrap();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT key_hash FROM neev_quota_buckets".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let key: String = row.try_get("", "key_hash").unwrap();
        assert_eq!(key.len(), 64);
        assert!(!key.contains("private"));
        assert!(!key.contains("203.0.113.99"));
    }
}

#[tokio::test]
async fn invitation_quota_reservations_enforce_actor_boundary_atomically() {
    let db = h::database().await;
    let buckets = [
        Bucket::InvitationActor(7),
        Bucket::InvitationOrganization(11),
    ];
    for _ in 0..20 {
        quota::reserve(&db, &[27; 32], &buckets, 1_700_000_000)
            .await
            .unwrap();
    }
    assert_eq!(
        quota::reserve(&db, &[27; 32], &buckets, 1_700_000_000).await,
        Err(AuthError::Limited)
    );
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT kind,used FROM neev_quota_buckets ORDER BY kind".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.try_get::<i64>("", "used").unwrap(), 20);
    }
}

struct GateSender {
    entered: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
}

#[async_trait]
impl MailSender for GateSender {
    async fn send(&self, _message: MailMessage) -> Result<(), AuthError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        permit.forget();
        Ok(())
    }
}

fn message(recipient: &str) -> MailMessage {
    MailMessage {
        recipient: recipient.into(),
        subject: "Sign in to Neev".into(),
        text: "synthetic body".into(),
    }
}

#[tokio::test]
async fn database_mail_leases_allow_only_two_network_sends() {
    let db = h::database().await;
    let entered = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let service = Service::new(
        db,
        Arc::new(GateSender {
            entered: entered.clone(),
            gate: gate.clone(),
        }),
    );
    let first = tokio::spawn({
        let service = service.clone();
        async move { service.send(message("one@example.test")).await }
    });
    let second = tokio::spawn({
        let service = service.clone();
        async move { service.send(message("two@example.test")).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while entered.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        service.send(message("three@example.test")).await,
        Err(AuthError::Limited)
    );
    gate.add_permits(2);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
}

struct FailingSender;

#[async_trait]
impl MailSender for FailingSender {
    async fn send(&self, _message: MailMessage) -> Result<(), AuthError> {
        Err(AuthError::Unavailable)
    }
}

#[tokio::test]
async fn provider_failure_releases_the_exact_acquired_lease() {
    let db = h::database().await;
    let service = Service::new(db.clone(), Arc::new(FailingSender));
    assert_eq!(
        service.send(message("person@example.test")).await,
        Err(AuthError::Unavailable)
    );
    assert_eq!(h::count(&db, "neev_mail_leases").await, 0);
}

#[tokio::test]
async fn stale_owner_cannot_release_a_replacement_lease() {
    let db = h::database().await;
    let lease = super::acquire_lease(&db).await.unwrap();
    h::execute(
        &db,
        "UPDATE neev_mail_leases SET expires_at=expires_at+1 WHERE id=$1",
        vec![lease.id.into()],
    )
    .await;
    super::release_lease(&db, &lease).await.unwrap();
    assert_eq!(h::count(&db, "neev_mail_leases").await, 1);
}

#[tokio::test]
#[ignore = "requires explicit isolated PostgreSQL"]
async fn postgres_mail_lease_ttl_starts_after_observed_guard_wait() {
    let (db, admin, schema) = h::postgres_database().await;
    let blocker = crate::neev::db::begin(&db).await.unwrap();
    crate::neev::db::one(
        &blocker,
        "SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE",
        vec![],
    )
    .await
    .unwrap();

    let entered = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let service = Service::new(
        db.clone(),
        Arc::new(GateSender {
            entered: entered.clone(),
            gate: gate.clone(),
        }),
    );
    let send = tokio::spawn(async move { service.send(message("delayed@example.test")).await });
    h::wait_for_postgres_lock(
        &admin,
        "%SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE%",
    )
    .await;
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    tokio::time::sleep(std::time::Duration::from_millis(2_100)).await;
    blocker.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let lease = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT expires_at,CAST(EXTRACT(EPOCH FROM clock_timestamp()) AS BIGINT) AS now FROM neev_mail_leases".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let remaining =
        lease.try_get::<i64>("", "expires_at").unwrap() - lease.try_get::<i64>("", "now").unwrap();
    gate.add_permits(1);
    send.await.unwrap().unwrap();
    h::cleanup_postgres(db, admin, schema).await;
    assert!(remaining >= 29, "new lease retained only {remaining}s");
}

struct HangingSender(Arc<tokio::sync::Notify>);

#[async_trait]
impl MailSender for HangingSender {
    async fn send(&self, _message: MailMessage) -> Result<(), AuthError> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn mail_network_timeout_is_ten_seconds_and_releases_lease() {
    let db = h::database().await;
    tokio::time::pause();
    let entered = Arc::new(tokio::sync::Notify::new());
    let service = Service::new(db.clone(), Arc::new(HangingSender(entered.clone())));
    let task = tokio::spawn(async move { service.send(message("slow@example.test")).await });
    entered.notified().await;
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    assert_eq!(task.await.unwrap(), Err(AuthError::Unavailable));
    assert_eq!(h::count(&db, "neev_mail_leases").await, 0);
}

#[tokio::test]
async fn resend_posts_one_text_only_recipient_without_tracking_fields() {
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use tokio::sync::Mutex;

    type Captured = Arc<Mutex<Option<(HeaderMap, serde_json::Value)>>>;
    async fn capture(
        State(captured): State<Captured>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> axum::http::StatusCode {
        *captured.lock().await = Some((headers, body));
        axum::http::StatusCode::ACCEPTED
    }

    let captured: Captured = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/emails", post(capture))
        .with_state(captured.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let sender = super::resend::Resend::for_test(
        "test-send-only-key".into(),
        "verification@neev.so".into(),
        url::Url::parse(&format!("http://{address}/emails")).unwrap(),
    )
    .unwrap();
    sender
        .send(MailMessage {
            recipient: "person@example.test".into(),
            subject: "Sign in to Neev".into(),
            text: "Use https://app.neev.so/auth/email/confirm#token=secret".into(),
        })
        .await
        .unwrap();
    let (headers, body) = captured.lock().await.take().unwrap();
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer test-send-only-key"
    );
    assert_eq!(
        body,
        serde_json::json!({
            "from":"Neev <verification@neev.so>",
            "to":"person@example.test",
            "subject":"Sign in to Neev",
            "text":"Use https://app.neev.so/auth/email/confirm#token=secret"
        })
    );
    assert!(body.get("html").is_none());
    assert!(body.get("attachments").is_none());
    assert!(body.get("tracking").is_none());
    server.abort();
}

#[test]
fn resend_rejects_any_unapproved_sender_address() {
    assert!(
        super::resend::Resend::new("test-send-only-key".into(), "login@mail.neev.so".into())
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires explicit isolated PostgreSQL"]
async fn postgres_quota_boundaries_serialize_across_two_service_instances() {
    let (db, admin, schema) = h::postgres_database().await;
    let peer = postgres_peer(&schema).await;
    let key = [28; 32];
    for (bucket, limit) in [
        (Bucket::MagicAddress("shared@example.test".into()), 3),
        (Bucket::MagicSourceIp("203.0.113.200".parse().unwrap()), 20),
        (Bucket::DeliveryMinute, 10),
        (Bucket::DeliveryDay, 80),
        (Bucket::InvitationActor(701), 20),
        (Bucket::InvitationOrganization(702), 80),
    ] {
        assert_two_instance_boundary(&db, &peer, key, bucket, limit).await;
    }
    peer.close().await.unwrap();
    h::cleanup_postgres(db, admin, schema).await;
}

async fn assert_two_instance_boundary(
    first: &DatabaseConnection,
    second: &DatabaseConnection,
    key: [u8; 32],
    bucket: Bucket,
    limit: usize,
) {
    for index in 0..limit - 1 {
        let instance = if index % 2 == 0 { first } else { second };
        quota::reserve(instance, &key, std::slice::from_ref(&bucket), 1_700_000_000)
            .await
            .unwrap();
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let attempt = |database: DatabaseConnection, bucket: Bucket| {
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            quota::reserve(&database, &key, &[bucket], 1_700_000_000).await
        })
    };
    let first_attempt = attempt(first.clone(), bucket.clone());
    let second_attempt = attempt(second.clone(), bucket.clone());
    barrier.wait().await;
    let results = [first_attempt.await.unwrap(), second_attempt.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().err() == Some(&AuthError::Limited))
            .count(),
        1
    );
    for instance in [first, second] {
        assert_eq!(
            quota::reserve(instance, &key, std::slice::from_ref(&bucket), 1_700_000_000,).await,
            Err(AuthError::Limited)
        );
    }
}

async fn postgres_peer(schema: &str) -> DatabaseConnection {
    let url = std::env::var("NEEV_TEST_DATABASE_URL").unwrap();
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(8)
        .sqlx_logging(false)
        .set_schema_search_path(schema);
    Database::connect(options).await.unwrap()
}
