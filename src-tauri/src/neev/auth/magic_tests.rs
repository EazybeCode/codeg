use std::{
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
};

use crate::neev::{
    auth::{magic, secrets, secrets::TokenPurpose, session},
    error::AuthError,
    mail::{self, MailMessage, MailSender},
    test_support as h,
    types::SignInIntent,
};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Statement};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{layer::SubscriberExt, Layer};

async fn service() -> (
    magic::Service,
    Arc<h::MailCapture>,
    sea_orm::DatabaseConnection,
    Arc<h::TestClock>,
) {
    let db = h::database().await;
    let capture = Arc::new(h::MailCapture::new());
    let mail = mail::Service::new(db.clone(), capture.clone());
    let clock = Arc::new(h::TestClock::new(1_700_000_000));
    let service = magic::Service::new(
        db.clone(),
        [31; 32],
        url::Url::parse("https://app.neev.so/").unwrap(),
        clock.clone(),
        mail,
    );
    (service, capture, db, clock)
}

fn fragment(text: &str) -> &str {
    text.split("#token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
}

#[tokio::test]
async fn wrong_browser_does_not_consume_then_confirmation_is_one_use() {
    let (service, capture, _, _) = service().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    let other = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        " Person@Example.test ",
        &browser,
        "203.0.113.10".parse::<IpAddr>().unwrap(),
        SignInIntent::Login,
    )
    .await
    .unwrap();
    assert_eq!(capture.len().await, 1);
    assert_eq!(capture.recipient(0).await, "person@example.test");
    let token = fragment(&capture.text(0).await).to_owned();

    assert!(matches!(
        magic::confirm(&service, &token, &other).await,
        Err(AuthError::Unauthorized)
    ));
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Ok(magic::Completion::LoggedIn(_))
    ));
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Err(AuthError::Unauthorized)
    ));
}

#[tokio::test]
async fn database_never_stores_raw_magic_or_browser_secret() {
    let (service, capture, db, _) = service().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        "private@example.test",
        &browser,
        "203.0.113.11".parse().unwrap(),
        SignInIntent::Login,
    )
    .await
    .unwrap();
    let token = fragment(&capture.text(0).await).to_owned();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT * FROM neev_magic_attempts".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let rendered = format!("{rows:?}");
    assert!(!rendered.contains(&token));
    assert!(!rendered.contains(&browser));
}

struct CaptureEvents(Arc<StdMutex<String>>);

impl<S: tracing::Subscriber> Layer<S> for CaptureEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visitor<'a>(&'a mut String);

        impl tracing::field::Visit for Visitor<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={value:?};", field.name());
            }
        }

        let mut output = self.0.lock().unwrap();
        output.push_str(event.metadata().target());
        event.record(&mut Visitor(&mut output));
    }
}

struct CaptureThenFail(Arc<tokio::sync::Mutex<Option<MailMessage>>>);

#[async_trait]
impl MailSender for CaptureThenFail {
    async fn send(&self, message: MailMessage) -> Result<(), AuthError> {
        *self.0.lock().await = Some(message);
        Err(AuthError::Unavailable)
    }
}

#[tokio::test]
async fn delivery_failure_logs_no_recipient_browser_or_magic_secret() {
    let db = h::database().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    let captured_message = Arc::new(tokio::sync::Mutex::new(None));
    let service = magic::Service::new(
        db.clone(),
        [33; 32],
        url::Url::parse("https://app.neev.so/").unwrap(),
        Arc::new(h::TestClock::new(1_700_000_000)),
        mail::Service::new(db, Arc::new(CaptureThenFail(captured_message.clone()))),
    );
    let events = Arc::new(StdMutex::new(String::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureEvents(events.clone()));

    let result = magic::request(
        &service,
        "private-log@example.test",
        &browser,
        "203.0.113.111".parse().unwrap(),
        SignInIntent::Login,
    )
    .with_subscriber(subscriber)
    .await;

    assert_eq!(result, Err(AuthError::Unavailable));
    let message = captured_message.lock().await.take().unwrap();
    let token = fragment(&message.text);
    let rendered = events.lock().unwrap();
    assert!(rendered.contains("reason=\"provider\""));
    for secret in ["private-log@example.test", browser.as_str(), token] {
        assert!(!rendered.contains(secret), "log exposed secret: {rendered}");
    }
}

#[tokio::test]
async fn malformed_high_cardinality_addresses_never_allocate_rows() {
    let (service, _, db, _) = service().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    for index in 0..250 {
        let email = format!("Name {index} <person{index}@example.test>");
        assert_eq!(
            magic::request(
                &service,
                &email,
                &browser,
                "203.0.113.17".parse().unwrap(),
                SignInIntent::Login,
            )
            .await,
            Err(AuthError::Invalid)
        );
    }
    assert_eq!(h::count(&db, "neev_magic_attempts").await, 0);
    assert_eq!(h::count(&db, "neev_quota_buckets").await, 0);
}

#[tokio::test]
async fn ten_minute_expiration_is_exclusive() {
    use std::sync::atomic::Ordering;

    let (service, capture, _, clock) = service().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        "expires@example.test",
        &browser,
        "203.0.113.13".parse().unwrap(),
        SignInIntent::Login,
    )
    .await
    .unwrap();
    let token = fragment(&capture.text(0).await).to_owned();
    clock.0.store(1_700_000_600, Ordering::SeqCst);
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Err(AuthError::Unauthorized)
    ));
}

#[tokio::test]
async fn concurrent_confirmation_has_exactly_one_winner() {
    let (service, capture, _, _) = service().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        "race@example.test",
        &browser,
        "203.0.113.14".parse().unwrap(),
        SignInIntent::Login,
    )
    .await
    .unwrap();
    let token = fragment(&capture.text(0).await).to_owned();
    let first = tokio::spawn({
        let service = service.clone();
        let browser = browser.clone();
        let token = token.clone();
        async move { magic::confirm(&service, &token, &browser).await }
    });
    let second = tokio::spawn({
        let service = service.clone();
        async move { magic::confirm(&service, &token, &browser).await }
    });
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(magic::Completion::LoggedIn(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(AuthError::Unauthorized)))
            .count(),
        1
    );
}

#[tokio::test]
async fn signed_in_link_preserves_the_existing_scoped_session() {
    let (service, capture, db, _) = service().await;
    let user = h::native_account(&db, "link-owner", "owner@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let identity_secrets = session::issue_identity(&db, user, 1_700_000_000)
        .await
        .unwrap();
    let identity = session::identity(&db, &identity_secrets.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped = session::select_organization(&db, &identity, org, 1_700_000_000)
        .await
        .unwrap();
    let scoped_identity = session::identity(&db, &scoped.session, 1_700_000_000)
        .await
        .unwrap();
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        "linked@example.test",
        &browser,
        "203.0.113.15".parse().unwrap(),
        SignInIntent::Link {
            user_id: scoped_identity.user_id,
            session_id: scoped_identity.session_id,
            auth_revision: scoped_identity.auth_revision,
        },
    )
    .await
    .unwrap();
    let token = fragment(&capture.text(0).await).to_owned();
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Ok(magic::Completion::Linked)
    ));
    let context = session::organization(&db, &scoped.session, 1_700_000_000)
        .await
        .unwrap();
    assert_eq!(context.org_id, org);
}

#[tokio::test]
async fn linking_collision_consumes_proof_without_granting_identity() {
    let (service, capture, db, _) = service().await;
    h::native_account(&db, "collision-owner", "collision@example.test").await;
    let user = h::native_account(&db, "linking-user", "linker@example.test").await;
    let issued = session::issue_identity(&db, user, 1_700_000_000)
        .await
        .unwrap();
    let identity = session::identity(&db, &issued.session, 1_700_000_000)
        .await
        .unwrap();
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    magic::request(
        &service,
        "collision@example.test",
        &browser,
        "203.0.113.16".parse().unwrap(),
        SignInIntent::Link {
            user_id: identity.user_id,
            session_id: identity.session_id,
            auth_revision: identity.auth_revision,
        },
    )
    .await
    .unwrap();
    let token = fragment(&capture.text(0).await).to_owned();
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Err(AuthError::Conflict)
    ));
    assert!(matches!(
        magic::confirm(&service, &token, &browser).await,
        Err(AuthError::Unauthorized)
    ));
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT COUNT(*) AS n FROM neev_identities WHERE user_id=$1 AND provider='email'",
            vec![user.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
}

#[tokio::test]
async fn scanner_get_does_not_consume_magic_link() {
    let mut harness = h::MagicHarness::new().await;
    let fragment = harness.request("person@example.test").await;
    harness.get_confirmation().await.assert_status_ok();
    harness
        .confirm_from_other_browser(&fragment)
        .await
        .assert_status_unauthorized();
    harness.confirm(&fragment).await.assert_status_ok();
    harness
        .confirm(&fragment)
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn email_request_http_202_is_uniform_for_hidden_outcomes() {
    let mut harness = h::MagicHarness::new().await;
    h::native_account(
        &harness.database,
        "existing-email-owner",
        "existing@example.test",
    )
    .await;
    let accepted = harness.request_response("new@example.test").await;
    let existing = harness.request_response("existing@example.test").await;
    harness.request_response("existing@example.test").await;
    harness.request_response("existing@example.test").await;
    let limited = harness.request_response("existing@example.test").await;
    harness.capture.fail(true);
    let provider_failure = harness.request_response("failure@example.test").await;

    for response in [&accepted, &existing, &limited, &provider_failure] {
        assert_eq!(response.status, reqwest::StatusCode::ACCEPTED);
        assert_eq!(response.body, accepted.body);
    }
    assert_eq!(
        accepted.body,
        serde_json::json!({
            "status":"accepted",
            "message":"If delivery is available, your sign-in email may arrive shortly. Delivery can be delayed; retry later if needed."
        })
    );
}

struct Failing;

#[async_trait]
impl MailSender for Failing {
    async fn send(&self, _message: MailMessage) -> Result<(), AuthError> {
        Err(AuthError::Unavailable)
    }
}

#[tokio::test]
async fn delivery_failure_revokes_the_pending_proof() {
    let db = h::database().await;
    let browser = secrets::random_token(TokenPurpose::Browser).unwrap();
    let service = magic::Service::new(
        db.clone(),
        [32; 32],
        url::Url::parse("https://app.neev.so/").unwrap(),
        Arc::new(h::TestClock::new(1_700_000_000)),
        mail::Service::new(db.clone(), Arc::new(Failing)),
    );
    assert_eq!(
        magic::request(
            &service,
            "person@example.test",
            &browser,
            "203.0.113.12".parse().unwrap(),
            SignInIntent::Login,
        )
        .await,
        Err(AuthError::Unavailable)
    );
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT consumed_at FROM neev_magic_attempts".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<Option<i64>>("", "consumed_at").unwrap(),
        Some(1_700_000_000)
    );
    assert_eq!(h::count(&db, "neev_quota_buckets").await, 4);
}
