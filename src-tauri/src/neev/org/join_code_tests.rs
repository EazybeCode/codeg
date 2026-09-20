use std::net::IpAddr;
use std::sync::Arc;

use crate::neev::{
    error::AuthError,
    org::join_code,
    startup::ServerState,
    test_support,
    types::{IdentityContext, OrgContext},
};

const NOW: i64 = 1_700_000_000;
fn ip(last: u8) -> IpAddr {
    format!("203.0.113.{last}").parse().unwrap()
}

async fn setup() -> (ServerState, OrgContext, i32) {
    let database = test_support::database().await;
    let owner = test_support::native_account(&database, "code-owner", "owner@example.test").await;
    let (org_id, _) = test_support::organization(&database, owner, "admin").await;
    let admin = test_support::scoped_context(&database, owner, org_id, NOW).await;
    let state = ServerState::new(
        database,
        test_support::preview_config(),
        Arc::new(test_support::TestClock::new(NOW)),
    )
    .unwrap();
    (state, admin, org_id)
}

async fn joiner(state: &ServerState, subject: &str) -> IdentityContext {
    let user = test_support::native_account(
        &state.database,
        subject,
        &format!("{subject}@example.test"),
    )
    .await;
    test_support::identity_context(&state.database, user, NOW).await
}

#[tokio::test]
async fn admin_rotate_then_view_returns_the_same_code_and_bumps_revision() {
    let (state, admin, _) = setup().await;
    let first = join_code::rotate(&state, &admin).await.unwrap();
    assert_eq!(first.revision, 1);
    assert!(!first.disabled);
    let viewed = join_code::view(&state, &admin).await.unwrap().unwrap();
    assert_eq!(viewed.code, first.code);
    let second = join_code::rotate(&state, &admin).await.unwrap();
    assert_eq!(second.revision, 2);
    assert_ne!(second.code, first.code);
}

#[tokio::test]
async fn a_non_admin_member_cannot_manage_the_code() {
    let (state, admin, org_id) = setup().await;
    join_code::rotate(&state, &admin).await.unwrap();
    let member_user =
        test_support::native_account(&state.database, "plain-member", "member@example.test").await;
    test_support::add_membership(&state.database, org_id, member_user, "member").await;
    let member = test_support::scoped_context(&state.database, member_user, org_id, NOW).await;
    assert_eq!(join_code::view(&state, &member).await, Err(AuthError::Forbidden));
    assert_eq!(
        join_code::rotate(&state, &member).await.err(),
        Some(AuthError::Forbidden)
    );
    assert_eq!(
        join_code::set_enabled(&state, &member, false).await,
        Err(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn preview_then_join_creates_a_regular_member_and_is_idempotent() {
    let (state, admin, org_id) = setup().await;
    let code = join_code::rotate(&state, &admin).await.unwrap().code;
    let outsider = joiner(&state, "outsider").await;
    let preview = join_code::preview(&state, &outsider, &code, ip(1))
        .await
        .unwrap();
    assert_eq!(preview.org_id, org_id);
    let joined = join_code::join(&state, &outsider, &code, org_id, ip(1))
        .await
        .unwrap();
    assert_eq!(joined, org_id);
    // Regular member, not admin.
    let role: String = crate::neev::db::one(
        &state.database,
        "SELECT role FROM neev_memberships WHERE org_id=$1 AND user_id=$2",
        vec![org_id.into(), outsider.user_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "role")
    .unwrap();
    assert_eq!(role, "member");
    // Idempotent: joining again does not create a duplicate.
    assert_eq!(
        join_code::join(&state, &outsider, &code, org_id, ip(1))
            .await
            .unwrap(),
        org_id
    );
    assert_eq!(
        test_support::count(&state.database, "neev_memberships").await,
        2 // the owner + the outsider
    );
}

#[tokio::test]
async fn cross_org_confirmation_mismatch_is_denied() {
    let (state, admin, org_id) = setup().await;
    let code = join_code::rotate(&state, &admin).await.unwrap().code;
    let outsider = joiner(&state, "outsider").await;
    assert_eq!(
        join_code::join(&state, &outsider, &code, org_id + 999, ip(1)).await,
        Err(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn invalid_and_rotated_and_disabled_codes_are_denied() {
    let (state, admin, org_id) = setup().await;
    let first = join_code::rotate(&state, &admin).await.unwrap().code;
    let outsider = joiner(&state, "outsider").await;
    // Invalid code.
    assert_eq!(
        join_code::preview(&state, &outsider, "ZZZZZZZZZZZZ", ip(1)).await,
        Err(AuthError::Forbidden)
    );
    // Rotating invalidates the old code.
    let second = join_code::rotate(&state, &admin).await.unwrap().code;
    assert_eq!(
        join_code::preview(&state, &outsider, &first, ip(2)).await,
        Err(AuthError::Forbidden)
    );
    assert_eq!(
        join_code::preview(&state, &outsider, &second, ip(3))
            .await
            .unwrap()
            .org_id,
        org_id
    );
    // Disabling blocks join without rotating.
    join_code::set_enabled(&state, &admin, false).await.unwrap();
    assert_eq!(
        join_code::join(&state, &outsider, &second, org_id, ip(4)).await,
        Err(AuthError::Forbidden)
    );
    join_code::set_enabled(&state, &admin, true).await.unwrap();
    assert_eq!(
        join_code::join(&state, &outsider, &second, org_id, ip(5))
            .await
            .unwrap(),
        org_id
    );
}

#[tokio::test]
async fn a_removed_member_is_not_silently_reactivated_by_the_code() {
    let (state, admin, org_id) = setup().await;
    let code = join_code::rotate(&state, &admin).await.unwrap().code;
    let removed = joiner(&state, "removed").await;
    test_support::add_membership(&state.database, org_id, removed.user_id, "member").await;
    test_support::execute(
        &state.database,
        &format!(
            "UPDATE neev_memberships SET active=false WHERE org_id={org_id} AND user_id={}",
            removed.user_id
        ),
        vec![],
    )
    .await;
    assert_eq!(
        join_code::join(&state, &removed, &code, org_id, ip(1)).await,
        Err(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn join_code_attempts_are_rate_limited_per_actor() {
    let (state, admin, _) = setup().await;
    join_code::rotate(&state, &admin).await.unwrap();
    let prober = joiner(&state, "prober").await;
    // Actor limit is 10 per 15 min; the 11th attempt (same user, varying IPs) trips.
    for attempt in 0..10 {
        assert_eq!(
            join_code::preview(&state, &prober, "ZZZZZZZZZZZZ", ip(attempt)).await,
            Err(AuthError::Forbidden)
        );
    }
    assert_eq!(
        join_code::preview(&state, &prober, "ZZZZZZZZZZZZ", ip(200)).await,
        Err(AuthError::Limited)
    );
}
