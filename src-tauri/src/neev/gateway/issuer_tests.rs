use super::issuer::{self, IssuerError};
use crate::neev::types::{OrgContext, Role};
use crate::neev::{db, test_support as h};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sea_orm::DatabaseConnection;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn tok(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn clock(now: i64) -> h::TestClock {
    h::TestClock::new(now)
}

async fn setup(now: i64) -> (DatabaseConnection, i32, i32, OrgContext) {
    let db = h::database().await;
    let user = h::native_account(&db, "issuer-sub", "issuer@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let context = h::scoped_context(&db, user, org, now).await;
    (db, user, org, context)
}

async fn issue_redeem(
    db: &DatabaseConnection,
    context: &OrgContext,
    attempt: u8,
    verifier: u8,
    now: i64,
) -> Result<issuer::Receipt, IssuerError> {
    let verifier = tok(verifier);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(db, context, &tok(attempt), &challenge, &clock(now))
        .await
        .unwrap();
    issuer::redeem(db, &tok(attempt), &issued.code, &verifier, &clock(now)).await
}

#[test]
fn code_hash_is_domain_separated_pinned_vector() {
    // Pinned vector: code = base64url(32 zero bytes); SHA256("neev-gateway-code-v1"|NUL|code).
    let code = tok(0);
    assert_eq!(
        issuer::hash_code(&code),
        "26868d039be671f30f28a02a473d443f40d2d655412298114dfff6eda0e18200"
    );
    assert_ne!(
        issuer::hash_code(&code),
        format!("{:x}", Sha256::digest(code.as_bytes())),
        "code hash must be domain-separated, not a plain SHA256"
    );
}

#[tokio::test]
async fn sqlite_issuer_redeem_validate_happy_path() {
    let (db, user, org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    assert_eq!(issued.expires_at, 2060);

    let receipt = issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    assert_eq!(receipt.subject.user_id, user);
    assert_eq!(receipt.subject.org_id, org);
    assert_eq!(receipt.subject.role, Role::Admin);
    assert_eq!(receipt.subject.verified_email, "issuer@example.test");
    assert_eq!(receipt.issued_after_cursor, 0);
    assert_eq!(receipt.revocation_cursor, 0);

    let retry = issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2002))
        .await
        .unwrap();
    assert_eq!(retry.receipt_id, receipt.receipt_id);
    assert_eq!(retry.relying_ref, receipt.relying_ref);

    let validated = issuer::validate(&db, &receipt.relying_ref, &clock(2003))
        .await
        .unwrap();
    assert_eq!(validated.subject.user_id, user);
    assert_eq!(validated.issued_after_cursor, 0);
    assert_eq!(validated.expires_at, receipt.expires_at);
}

#[tokio::test]
async fn sqlite_issuer_authority_change_is_distinct_from_conflict() {
    let (db, user, org, context) = setup(1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    h::execute(
        &db,
        "UPDATE neev_memberships SET revision=revision+1 WHERE org_id=$1 AND user_id=$2",
        vec![org.into(), user.into()],
    )
    .await;
    // Distinct typed outcome, not ordinary Conflict (R3).
    assert_eq!(
        issuer::validate(&db, &receipt.relying_ref, &clock(2005)).await.err(),
        Some(IssuerError::AuthorityChanged)
    );
    assert_eq!(
        issuer::issue(&db, &context, &tok(2), &challenge_for(&tok(9)), &clock(2006))
            .await
            .err(),
        Some(IssuerError::Forbidden)
    );
}

#[tokio::test]
async fn sqlite_issuer_rejects_wrong_and_expired_inputs() {
    let (db, _user, _org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &tok(8), &clock(2001)).await.err(),
        Some(IssuerError::Unauthorized)
    );
    assert_eq!(
        issuer::redeem(&db, &tok(2), &issued.code, &verifier, &clock(2001)).await.err(),
        Some(IssuerError::Unauthorized)
    );
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2060)).await.err(),
        Some(IssuerError::Unauthorized)
    );
    assert_eq!(
        issuer::redeem(&db, "short", &issued.code, &verifier, &clock(2001)).await.err(),
        Some(IssuerError::Invalid)
    );
}

#[tokio::test]
async fn sqlite_issuer_replay_after_retry_window_is_denied() {
    let (db, _user, _org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2301)).await.err(),
        Some(IssuerError::Unauthorized)
    );
}

#[tokio::test]
async fn sqlite_issuer_revoke_denies_source_and_same_source_reissue() {
    let (db, user, org, context) = setup(1000).await;
    let receipt_a = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    let intent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let outcome = issuer::revoke(&db, &receipt_a.relying_ref, intent, &clock(2100))
        .await
        .unwrap();
    assert_eq!(outcome.cursor, 1);
    // Idempotent by intent id.
    let again = issuer::revoke(&db, &receipt_a.relying_ref, intent, &clock(2101))
        .await
        .unwrap();
    assert_eq!((again.event_id, again.cursor), (outcome.event_id, outcome.cursor));
    // A is denied and its native source is now revoked.
    assert_eq!(
        issuer::validate(&db, &receipt_a.relying_ref, &clock(2102)).await.err(),
        Some(IssuerError::Unauthorized)
    );
    // R5 counterexample fix: a reissue on the SAME (now revoked) source fails.
    assert_eq!(
        issuer::issue(&db, &context, &tok(2), &challenge_for(&tok(9)), &clock(2103))
            .await
            .err(),
        Some(IssuerError::Unauthorized)
    );
    // A fresh login on a NEW source session after the event is admitted (cutoff 1).
    let context_b = h::scoped_context(&db, user, org, 3000).await;
    let receipt_b = issue_redeem(&db, &context_b, 3, 7, 3001).await.unwrap();
    assert_eq!(receipt_b.issued_after_cursor, 1);
    assert_eq!(
        issuer::validate(&db, &receipt_b.relying_ref, &clock(3002))
            .await
            .unwrap()
            .subject
            .user_id,
        user
    );
}

#[tokio::test]
async fn sqlite_issuer_revocations_page_ack_and_replay() {
    let (db, _user, _org, context) = setup(1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    issuer::revoke(&db, &receipt.relying_ref, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &clock(2100))
        .await
        .unwrap();
    let page = issuer::revocations(&db, 0, 50).await.unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.next_cursor, 1);
    assert_eq!(page.high_watermark, 1);
    assert_eq!(page.events[0].scope, "session");
    // Intentional replay: re-fetch after_cursor <= fetched_through returns events.
    assert_eq!(issuer::revocations(&db, 0, 50).await.unwrap().events.len(), 1);

    assert_eq!(issuer::ack(&db, 1, 1).await, Ok(1));
    assert_eq!(issuer::ack(&db, 1, 0).await, Err(IssuerError::Invalid));
    assert_eq!(issuer::ack(&db, 2, 1).await, Err(IssuerError::Conflict));
    // Skipping unseen events is denied; past the watermark is a conflict.
    assert_eq!(issuer::revocations(&db, 5, 50).await.err(), Some(IssuerError::Conflict));
}

#[tokio::test]
async fn sqlite_issuer_quota_and_duplicate_attempt_priority() {
    let (db, _user, _org, context) = setup(1000).await;
    let challenge = challenge_for(&tok(9));
    for attempt in 0..5u8 {
        issuer::issue(&db, &context, &tok(attempt), &challenge, &clock(2000))
            .await
            .unwrap();
    }
    // A duplicate retained attempt is Conflict even at quota (R14).
    assert_eq!(
        issuer::issue(&db, &context, &tok(0), &challenge, &clock(2000)).await.err(),
        Some(IssuerError::Conflict)
    );
    // A new attempt at capacity is rate limited.
    assert_eq!(
        issuer::issue(&db, &context, &tok(5), &challenge, &clock(2000)).await.err(),
        Some(IssuerError::Limited)
    );
}

#[tokio::test]
async fn sqlite_issuer_retry_keeps_original_names_validate_returns_fresh() {
    let (db, user, org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    let receipt = issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    let original_org = receipt.subject.org_name.clone();
    let original_display = receipt.subject.display_name.clone();
    // Rename org and account WITHOUT bumping authority revisions (branding-style).
    h::execute(&db, "UPDATE neev_organizations SET name='Renamed org' WHERE id=$1", vec![org.into()]).await;
    h::execute(&db, "UPDATE neev_accounts SET display_name='Renamed user' WHERE id=$1", vec![user.into()]).await;
    // R8: idempotent retry returns the ORIGINAL immutable receipt names.
    let retry = issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2002))
        .await
        .unwrap();
    assert_eq!(retry.subject.org_name, original_org);
    assert_eq!(retry.subject.display_name, original_display);
    // validate returns the CURRENT names.
    let validated = issuer::validate(&db, &receipt.relying_ref, &clock(2003))
        .await
        .unwrap();
    assert_eq!(validated.subject.org_name, "Renamed org");
    assert_eq!(validated.subject.display_name, "Renamed user");
}

#[tokio::test]
async fn sqlite_issuer_rejects_future_only_verified_email() {
    let (db, user, _org, context) = setup(1000).await;
    // The only verified email is dated in the future relative to redemption (R9).
    h::execute(
        &db,
        "UPDATE neev_verified_emails SET verified_at=9000 WHERE user_id=$1",
        vec![user.into()],
    )
    .await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001)).await.err(),
        Some(IssuerError::Unavailable)
    );
    // Code is not consumed on a failed provisioning.
    assert_eq!(
        h::count(&db, "neev_gateway_relying_sessions").await,
        0
    );
}

#[tokio::test]
async fn sqlite_issuer_rejects_native_valid_but_gateway_invalid_name() {
    let (db, _user, org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    // Native permits a 100-character CJK org name (300 UTF-8 bytes); GatewayName
    // permits at most 256 bytes (R12). Rename without a revision bump.
    let cjk = "\u{3042}".repeat(100);
    h::execute(
        &db,
        "UPDATE neev_organizations SET name=$1 WHERE id=$2",
        vec![cjk.into(), org.into()],
    )
    .await;
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001)).await.err(),
        Some(IssuerError::Unavailable)
    );
    // Atomic: the code is not consumed when the fresh name fails validation.
    let consumed = db::one(
        &db,
        "SELECT consumed_at FROM neev_gateway_exchanges WHERE attempt_id=$1",
        vec![tok(1).into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<Option<i64>>("", "consumed_at")
    .unwrap();
    assert_eq!(consumed, None);
}

#[tokio::test]
async fn sqlite_issuer_binding_survives_expirable_cleanup() {
    let (db, _user, _org, context) = setup(1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    for table in [
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
    ] {
        db::execute(&db, &format!("DELETE FROM {table}"), vec![])
            .await
            .unwrap();
    }
    let outcome = issuer::revoke(
        &db,
        &receipt.relying_ref,
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        &clock(2200),
    )
    .await
    .unwrap();
    assert_eq!(outcome.cursor, 1);
}

#[tokio::test]
async fn sqlite_issuer_retry_rejects_corrupted_persisted_relying() {
    // Malformed stored display scalar on replay fails closed.
    let (db, _user, _org, context) = setup(1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    h::execute(&db, "UPDATE neev_gateway_relying_sessions SET verified_email='BAD'", vec![]).await;
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2002)).await.err(),
        Some(IssuerError::Unavailable)
    );

    // A stored authority scalar disagreeing with the captured exchange fails closed.
    let (db, _user, _org, context) = setup(1000).await;
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    h::execute(&db, "UPDATE neev_gateway_relying_sessions SET org_revision=9", vec![]).await;
    assert_eq!(
        issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2002)).await.err(),
        Some(IssuerError::Unavailable)
    );
}

#[tokio::test]
async fn sqlite_issuer_revoke_replay_rejects_inconsistent_intent() {
    let (db, _user, _org, context) = setup(1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    let intent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    issuer::revoke(&db, &receipt.relying_ref, intent, &clock(2100))
        .await
        .unwrap();
    // Corrupt the stored intent's source so it no longer matches its binding.
    h::execute(
        &db,
        "UPDATE neev_gateway_revoke_intents SET source_session_id='99999999-9999-4999-8999-999999999999' WHERE intent_id=$1",
        vec![intent.into()],
    )
    .await;
    assert_eq!(
        issuer::revoke(&db, &receipt.relying_ref, intent, &clock(2101)).await.err(),
        Some(IssuerError::Unavailable)
    );
}

// ---- N2: transaction-coupled revocation events (integration-gated) ----

fn enabled_state(db: &DatabaseConnection, now: i64) -> Arc<crate::neev::startup::ServerState> {
    let mut config = h::preview_config();
    config.gateway_events = true;
    Arc::new(
        crate::neev::startup::ServerState::new(db.clone(), config, Arc::new(clock(now))).unwrap(),
    )
}

async fn scope_event_count(db: &DatabaseConnection, sql: &str, args: Vec<sea_orm::Value>) -> i64 {
    db::one(db, sql, args)
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

#[tokio::test]
async fn sqlite_gateway_logout_appends_session_event_when_enabled() {
    let (db, user, org, ctx) = setup(1000).await;
    let receipt = issue_redeem(&db, &ctx, 1, 9, 2000).await.unwrap();
    crate::neev::auth::session::revoke_with_gateway(&db, &ctx.identity, &clock(2100), true)
        .await
        .unwrap();
    assert_eq!(
        scope_event_count(
            &db,
            "SELECT COUNT(*) AS n FROM neev_gateway_revocations WHERE scope='session' AND source_session_id=$1 AND user_id=$2 AND org_id=$3",
            vec![ctx.identity.session_id.clone().into(), user.into(), org.into()],
        )
        .await,
        1
    );
    assert_eq!(
        issuer::validate(&db, &receipt.relying_ref, &clock(2101)).await.err(),
        Some(IssuerError::Unauthorized)
    );

    // Disabled: native logout still revokes the session, but appends no event.
    let (db, _u, _o, ctx) = setup(1000).await;
    issue_redeem(&db, &ctx, 1, 9, 2000).await.unwrap();
    crate::neev::auth::session::revoke_with_gateway(&db, &ctx.identity, &clock(2100), false)
        .await
        .unwrap();
    assert_eq!(h::count(&db, "neev_gateway_revocations").await, 0);
}

#[tokio::test]
async fn sqlite_gateway_member_removal_appends_membership_event_when_enabled() {
    let db = h::database().await;
    let owner = h::native_account(&db, "gw-owner", "gw-owner@example.test").await;
    let member = h::native_account(&db, "gw-member", "gw-member@example.test").await;
    let (org, _owner_member) = h::organization(&db, owner, "admin").await;
    let member_id = h::add_membership(&db, org, member, "member").await;
    let member_ctx = h::scoped_context(&db, member, org, 1000).await;
    let member_receipt = issue_redeem(&db, &member_ctx, 1, 9, 2000).await.unwrap();
    let owner_ctx = h::scoped_context(&db, owner, org, 1000).await;
    let state = enabled_state(&db, 3000);
    crate::neev::org::remove(&state, &owner_ctx, member_id)
        .await
        .unwrap();
    assert_eq!(
        scope_event_count(
            &db,
            "SELECT COUNT(*) AS n FROM neev_gateway_revocations WHERE scope='membership' AND user_id=$1 AND org_id=$2",
            vec![member.into(), org.into()],
        )
        .await,
        1
    );
    // The removed member's relying binding is denied (native authority recheck
    // denies an inactive membership before the event-deny path also would).
    assert!(
        issuer::validate(&db, &member_receipt.relying_ref, &clock(3001))
            .await
            .is_err(),
        "removed member's relying binding must be denied"
    );
}

#[tokio::test]
async fn sqlite_gateway_account_org_membership_scope_events_deny() {
    use sea_orm::TransactionTrait as _;
    for scope in ["account", "org", "membership"] {
        let (db, user, org, ctx) = setup(1000).await;
        let receipt = issue_redeem(&db, &ctx, 1, 9, 2000).await.unwrap();
        let (u, o) = match scope {
            "account" => (Some(user), None),
            "org" => (None, Some(org)),
            _ => (Some(user), Some(org)),
        };
        let tx = db::begin(&db).await.unwrap();
        issuer::append_revocation(&tx, scope, u, o, None, 2100).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            issuer::validate(&db, &receipt.relying_ref, &clock(2101)).await.err(),
            Some(IssuerError::Unauthorized),
            "scope {scope} did not deny the binding"
        );
    }
}

// ---- Guarded PostgreSQL peers (R2/R6/R7) ----

async fn pg_setup(
    db: &DatabaseConnection,
    now: i64,
) -> (i32, i32, OrgContext) {
    let user = h::native_account(db, "issuer-sub", "issuer@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let context = h::scoped_context(db, user, org, now).await;
    (user, org, context)
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_redeem_validate_happy_path() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer happy path owned schema: {schema}");
    let (user, org, context) = pg_setup(&db, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    let receipt = issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    assert_eq!(receipt.subject.user_id, user);
    let validated = issuer::validate(&db, &receipt.relying_ref, &clock(2002))
        .await
        .unwrap();
    assert_eq!(validated.subject.org_id, org);
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_concurrent_redeem_is_idempotent() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer concurrent redeem owned schema: {schema}");
    let (_user, _org, context) = pg_setup(&db, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    let code = issued.code.clone();
    let outcome = std::panic::AssertUnwindSafe(async {
        let first = {
            let db = db.clone();
            let code = code.clone();
            let verifier = verifier.clone();
            BoundedWorker::spawn(async move {
                issuer::redeem(&db, &tok(1), &code, &verifier, &clock(2001)).await
            })
        };
        let second = {
            let db = db.clone();
            let code = code.clone();
            let verifier = verifier.clone();
            BoundedWorker::spawn(async move {
                issuer::redeem(&db, &tok(1), &code, &verifier, &clock(2001)).await
            })
        };
        let a = first.join().await.unwrap();
        let b = second.join().await.unwrap();
        // Both observe the same single receipt/binding; exactly one is consumed.
        assert_eq!(a.receipt_id, b.receipt_id);
        assert_eq!(a.relying_ref, b.relying_ref);
        assert_eq!(h::count(&db, "neev_gateway_redemptions").await, 1);
        assert_eq!(h::count(&db, "neev_gateway_relying_sessions").await, 1);
        assert_eq!(h::count(&db, "neev_gateway_relying_bindings").await, 1);
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_first_redeem_rolls_back_without_partial_receipt() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer rollback owned schema: {schema}");
    let (_user, _org, context) = pg_setup(&db, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    // Force the receipt insert to fail; the whole first redemption must roll back.
    db::execute(
        &db,
        "CREATE FUNCTION fixture_reject_receipt() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'fixture reject'; END; $$ LANGUAGE plpgsql",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        &db,
        "CREATE TRIGGER fixture_reject_receipt BEFORE INSERT ON neev_gateway_redemptions FOR EACH ROW EXECUTE FUNCTION fixture_reject_receipt()",
        vec![],
    )
    .await
    .unwrap();
    let outcome = std::panic::AssertUnwindSafe(async {
        assert!(issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
            .await
            .is_err());
        // No partial receipt/binding/relying and the code stays unconsumed.
        assert_eq!(h::count(&db, "neev_gateway_redemptions").await, 0);
        assert_eq!(h::count(&db, "neev_gateway_relying_bindings").await, 0);
        assert_eq!(h::count(&db, "neev_gateway_relying_sessions").await, 0);
        let consumed = db::one(
            &db,
            "SELECT consumed_at FROM neev_gateway_exchanges WHERE attempt_id=$1",
            vec![tok(1).into()],
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<Option<i64>>("", "consumed_at")
        .unwrap();
        assert_eq!(consumed, None);
        db::execute(&db, "DROP TRIGGER fixture_reject_receipt ON neev_gateway_redemptions", vec![])
            .await
            .unwrap();
        db::execute(&db, "DROP FUNCTION fixture_reject_receipt()", vec![])
            .await
            .unwrap();
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_validate_revoke_race_is_consistent() {
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait as _};
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer validate/revoke race owned schema: {schema}");
    let (user, org, context) = pg_setup(&db, 1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    let source = receipt.subject.source_session_id.clone();
    let outcome = std::panic::AssertUnwindSafe(async {
        // Direction 1: validate that completes before any revocation is admitted.
        let admitted = issuer::validate(&db, &receipt.relying_ref, &clock(2001))
            .await
            .unwrap();
        assert_eq!(admitted.revocation_cursor, 0);
        // Direction 2: a revocation commits during validate's audience wait. Hold
        // the counter row exclusively, start validate (blocks on FOR SHARE), then
        // append the session event and commit; validate must observe a consistent
        // (watermark, MAX) pair and deny.
        let blocker = db.begin_with_config(None, None).await.unwrap();
        blocker
            .execute(Statement::from_string(
                blocker.get_database_backend(),
                "SELECT last_cursor FROM neev_gateway_audience WHERE audience='neev-collaboration-v1' FOR UPDATE",
            ))
            .await
            .unwrap();
        let worker = {
            let db = db.clone();
            let relying_ref = receipt.relying_ref.clone();
            BoundedWorker::spawn(async move { issuer::validate(&db, &relying_ref, &clock(2002)).await })
        };
        h::wait_for_postgres_lock(&admin, "%neev_gateway_audience%FOR SHARE%").await;
        db::execute(
            &blocker,
            "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES('neev-collaboration-v1',1,'44444444-4444-4444-8444-444444444444','session',$1,$2,$3,1200)",
            vec![user.into(), org.into(), source.clone().into()],
        )
        .await
        .unwrap();
        db::execute(
            &blocker,
            "UPDATE neev_gateway_audience SET last_cursor=1 WHERE audience='neev-collaboration-v1' AND last_cursor=0",
            vec![],
        )
        .await
        .unwrap();
        blocker.commit().await.unwrap();
        assert_eq!(worker.join().await.err(), Some(IssuerError::Unauthorized));
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_validate_rejects_expiry_during_lock_wait() {
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait as _};
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer validate expiry-wait owned schema: {schema}");
    let (_user, _org, context) = pg_setup(&db, 1000).await;
    let receipt = issue_redeem(&db, &context, 1, 9, 2000).await.unwrap();
    let shared = Arc::new(clock(2001));
    let outcome = std::panic::AssertUnwindSafe(async {
        let blocker = db.begin_with_config(None, None).await.unwrap();
        blocker
            .execute(Statement::from_string(
                blocker.get_database_backend(),
                "SELECT last_cursor FROM neev_gateway_audience WHERE audience='neev-collaboration-v1' FOR UPDATE",
            ))
            .await
            .unwrap();
        let worker = {
            let db = db.clone();
            let relying_ref = receipt.relying_ref.clone();
            let shared = shared.clone();
            BoundedWorker::spawn(async move { issuer::validate(&db, &relying_ref, shared.as_ref()).await })
        };
        h::wait_for_postgres_lock(&admin, "%neev_gateway_audience%FOR SHARE%").await;
        // Advance past the source-session expiry, then release the counter row.
        shared.0.store(9_000_000, Ordering::SeqCst);
        blocker.rollback().await.unwrap();
        assert_eq!(worker.join().await.err(), Some(IssuerError::Unauthorized));
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_retry_rejects_expiry_during_lock_wait() {
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait as _};
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer retry expiry-wait owned schema: {schema}");
    let (_user, _org, context) = pg_setup(&db, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    issuer::redeem(&db, &tok(1), &issued.code, &verifier, &clock(2001))
        .await
        .unwrap();
    let shared = Arc::new(clock(2002));
    let outcome = std::panic::AssertUnwindSafe(async {
        let blocker = db.begin_with_config(None, None).await.unwrap();
        blocker
            .execute(Statement::from_string(
                blocker.get_database_backend(),
                "SELECT last_cursor FROM neev_gateway_audience WHERE audience='neev-collaboration-v1' FOR UPDATE",
            ))
            .await
            .unwrap();
        let worker = {
            let db = db.clone();
            let code = issued.code.clone();
            let verifier = verifier.clone();
            let shared = shared.clone();
            BoundedWorker::spawn(async move { issuer::redeem(&db, &tok(1), &code, &verifier, shared.as_ref()).await })
        };
        // The consumed-retry path blocks on the audience counter lock.
        h::wait_for_postgres_lock(&admin, "%neev_gateway_audience%FOR UPDATE%").await;
        // Advance past retry_until (2001+300) and the source expiry, then release.
        shared.0.store(9_000_000, Ordering::SeqCst);
        blocker.rollback().await.unwrap();
        assert_eq!(worker.join().await.err(), Some(IssuerError::Unauthorized));
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_issuer_redeem_rejects_source_expiry_during_lock_wait() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("issuer expiry-during-wait owned schema: {schema}");
    // Short-lived source session so it can expire during a blocking lock wait.
    let user = h::native_account(&db, "issuer-sub", "issuer@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let context = h::scoped_context(&db, user, org, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let issued = issuer::issue(&db, &context, &tok(1), &challenge, &clock(2000))
        .await
        .unwrap();
    let shared = Arc::new(clock(2001));
    let outcome = std::panic::AssertUnwindSafe(async {
        // Hold the audience counter row so redeem blocks on it.
        let blocker = db.begin_with_config(None, None).await.unwrap();
        use sea_orm::{ConnectionTrait, Statement};
        blocker
            .execute(Statement::from_string(
                blocker.get_database_backend(),
                "SELECT last_cursor FROM neev_gateway_audience WHERE audience='neev-collaboration-v1' FOR UPDATE",
            ))
            .await
            .unwrap();
        let worker = {
            let db = db.clone();
            let code = issued.code.clone();
            let verifier = verifier.clone();
            let shared = shared.clone();
            BoundedWorker::spawn(async move {
                issuer::redeem(&db, &tok(1), &code, &verifier, shared.as_ref()).await
            })
        };
        // Wait until redeem is blocked on the audience row, then advance the clock
        // past the code/session expiry and release the lock.
        h::wait_for_postgres_lock(&admin, "%neev_gateway_audience%FOR UPDATE%").await;
        shared.0.store(9_000_000, Ordering::SeqCst);
        use sea_orm::TransactionTrait as _;
        blocker.rollback().await.unwrap();
        // The resampled clock is past code_expires_at, so redemption is denied.
        assert_eq!(worker.join().await.err(), Some(IssuerError::Unauthorized));
        assert_eq!(h::count(&db, "neev_gateway_relying_sessions").await, 0);
    })
    .catch_unwind()
    .await;
    finish(db, admin, schema, outcome).await;
}

use futures::FutureExt as _;

/// Spawned test worker with abort-on-drop ownership and a bounded, cancellation-
/// awaiting join, so a hung or panicking test never leaks a detached task (R2).
struct BoundedWorker<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}
impl<T: Send + 'static> BoundedWorker<T> {
    fn spawn<F: std::future::Future<Output = T> + Send + 'static>(future: F) -> Self {
        Self {
            handle: Some(tokio::spawn(future)),
        }
    }
    async fn join(mut self) -> T {
        let mut handle = self.handle.take().expect("worker joined once");
        match tokio::time::timeout(std::time::Duration::from_secs(15), &mut handle).await {
            Ok(joined) => joined.expect("worker task panicked"),
            Err(_) => {
                handle.abort();
                let _ = handle.await;
                panic!("bounded worker task timed out");
            }
        }
    }
}
impl<T> Drop for BoundedWorker<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn finish(
    db: DatabaseConnection,
    admin: DatabaseConnection,
    schema: String,
    outcome: Result<(), Box<dyn std::any::Any + Send>>,
) {
    if let Err(panic) = outcome {
        eprintln!("retained failed issuer owned schema: {schema}");
        drop(db);
        drop(admin);
        std::panic::resume_unwind(panic);
    }
    h::cleanup_postgres(db, admin, schema).await;
}
