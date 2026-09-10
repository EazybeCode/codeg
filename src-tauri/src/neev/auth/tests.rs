use super::identity;
use crate::neev::{error::AuthError, test_support as h, types::SignInIntent};

#[tokio::test]
async fn repeated_subject_returns_the_same_account() {
    let db = h::database().await;
    let first = identity::resolve(
        &db,
        h::google("subject-a", "support@neev.so"),
        SignInIntent::Login,
        1000,
    )
    .await
    .unwrap();
    let again = identity::resolve(
        &db,
        h::google("subject-a", "support@neev.so"),
        SignInIntent::Login,
        1001,
    )
    .await
    .unwrap();
    assert_eq!(first, again);
    assert_eq!(h::count(&db, "neev_accounts").await, 1);
}

#[tokio::test]
async fn same_email_does_not_merge_distinct_subjects_or_leave_orphan() {
    let db = h::database().await;
    identity::resolve(
        &db,
        h::google("subject-a", "support@neev.so"),
        SignInIntent::Login,
        1000,
    )
    .await
    .unwrap();
    assert_eq!(
        identity::resolve(
            &db,
            h::google("subject-b", " SUPPORT@NEEV.SO "),
            SignInIntent::Login,
            1001
        )
        .await,
        Err(AuthError::Conflict)
    );
    assert_eq!(h::count(&db, "neev_accounts").await, 1);
    assert_eq!(h::count(&db, "neev_identities").await, 1);
}

#[tokio::test]
async fn unverified_email_cannot_become_verified_or_join_an_org() {
    let db = h::database().await;
    let mut proof = h::google("subject-a", "support@neev.so");
    proof.verified_email = None;
    identity::resolve(&db, proof, SignInIntent::Login, 1000)
        .await
        .unwrap();
    assert_eq!(h::count(&db, "neev_verified_emails").await, 0);
    assert_eq!(h::count(&db, "neev_memberships").await, 0);
}

#[tokio::test]
async fn malformed_provider_identity_is_rejected() {
    let db = h::database().await;
    let mut proof = h::google("subject", "support@neev.so");
    proof.issuer = "https://attacker.test".into();
    assert_eq!(
        identity::resolve(&db, proof, SignInIntent::Login, 1000).await,
        Err(AuthError::Invalid)
    );
    assert_eq!(h::count(&db, "neev_accounts").await, 0);
}

#[tokio::test]
async fn linking_requires_real_recent_session() {
    let db = h::database().await;
    let id = identity::resolve(
        &db,
        h::google("subject-a", "support@neev.so"),
        SignInIntent::Login,
        1000,
    )
    .await
    .unwrap();
    let intent = SignInIntent::Link {
        user_id: id,
        session_id: "invented".into(),
        auth_revision: 1,
    };
    assert_eq!(
        identity::resolve(
            &db,
            h::google("subject-b", "other@example.test"),
            intent,
            1001
        )
        .await,
        Err(AuthError::Unauthorized)
    );
    assert_eq!(h::count(&db, "neev_identities").await, 1);
}

#[tokio::test]
async fn linking_to_same_account_is_idempotent_and_expiry_is_enforced() {
    let db = h::database().await;
    let id = h::native_account(&db, "owner", "support@neev.so").await;
    h::session(&db, id, "session", 1000, 1).await;
    for _ in 0..2 {
        let intent = SignInIntent::Link {
            user_id: id,
            session_id: "session".into(),
            auth_revision: 1,
        };
        assert_eq!(
            identity::resolve(
                &db,
                h::google("second-method", "other@example.test"),
                intent,
                1001
            )
            .await,
            Ok(id)
        );
    }
    let intent = SignInIntent::Link {
        user_id: id,
        session_id: "session".into(),
        auth_revision: 1,
    };
    assert_eq!(
        identity::resolve(&db, h::google("third", "third@example.test"), intent, 1600).await,
        Err(AuthError::Unauthorized)
    );
    assert_eq!(h::count(&db, "neev_accounts").await, 1);
    assert_eq!(h::count(&db, "neev_identities").await, 2);
}

#[tokio::test]
async fn revoked_or_stale_sessions_cannot_link() {
    for (revision, revoked) in [(2, false), (1, true)] {
        let db = h::database().await;
        let id = h::native_account(&db, "owner", "support@neev.so").await;
        h::session(&db, id, "session", 1000, 1).await;
        h::execute(
            &db,
            "UPDATE neev_accounts SET auth_revision=$1 WHERE id=$2",
            vec![revision.into(), id.into()],
        )
        .await;
        if revoked {
            h::execute(&db, "UPDATE neev_sessions SET revoked_at=1001", vec![]).await;
        }
        let intent = SignInIntent::Link {
            user_id: id,
            session_id: "session".into(),
            auth_revision: 1,
        };
        assert_eq!(
            identity::resolve(&db, h::google("new", "new@example.test"), intent, 1002).await,
            Err(AuthError::Unauthorized)
        );
        assert_eq!(h::count(&db, "neev_identities").await, 1);
    }
}

#[tokio::test]
async fn existing_foreign_identity_cannot_be_linked_or_email_transferred() {
    let db = h::database().await;
    let id = h::native_account(&db, "owner", "support@neev.so").await;
    h::native_account(&db, "other", "other@example.test").await;
    h::session(&db, id, "session", 1000, 1).await;
    let intent = SignInIntent::Link {
        user_id: id,
        session_id: "session".into(),
        auth_revision: 1,
    };
    assert_eq!(
        identity::resolve(&db, h::google("other", "other@example.test"), intent, 1001).await,
        Err(AuthError::Conflict)
    );
    assert_eq!(h::count(&db, "neev_accounts").await, 2);
    assert_eq!(h::count(&db, "neev_identities").await, 2);
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_concurrent_email_collision_leaves_one_account_and_identity() {
    let (db, admin, schema) = h::postgres_database().await;
    let (a, b) = tokio::join!(
        identity::resolve(
            &db,
            h::google("a", "support@neev.so"),
            SignInIntent::Login,
            1000
        ),
        identity::resolve(
            &db,
            h::google("b", "support@neev.so"),
            SignInIntent::Login,
            1000
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(a == Err(AuthError::Conflict) || b == Err(AuthError::Conflict));
    assert_eq!(h::count(&db, "neev_accounts").await, 1);
    assert_eq!(h::count(&db, "neev_identities").await, 1);
    h::cleanup_postgres(db, admin, schema).await;
}
