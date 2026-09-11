use sea_orm::{ConnectionTrait, Statement};

use crate::neev::{
    auth::session,
    db,
    error::AuthError,
    org::{self, invites, BrandingLogo},
    test_support::{self, OrgHarness},
    types::Role,
};

#[tokio::test]
async fn invite_acceptance_requires_exact_verified_recipient_and_current_inviter() {
    let h = OrgHarness::new().await;
    let invite = h.invite("invitee@example.test").await;
    assert_eq!(
        h.accept(h.wrong_recipient(), &invite).await,
        Err(AuthError::Forbidden)
    );
    h.revoke_inviter_membership().await;
    assert_eq!(
        h.accept(h.invitee(), &invite).await,
        Err(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn organization_creation_is_trimmed_idempotent_bounded_and_name_nonunique() {
    let h = OrgHarness::new().await;
    let identity = h.invitee();
    let request = uuid::Uuid::new_v4().to_string();
    let first = org::create(&h.state, &identity, &request, "  Same company  ")
        .await
        .unwrap();
    let retry = org::create(&h.state, &identity, &request, "Same company")
        .await
        .unwrap();
    assert_eq!(retry.id, first.id);
    assert_eq!(retry.name, "Same company");
    assert_eq!(retry.owner_user_id, identity.user_id);
    assert_eq!(retry.revision, 1);
    assert_eq!(
        org::create(&h.state, &identity, &request, "Different body")
            .await
            .err(),
        Some(AuthError::Conflict)
    );
    for _ in 0..2 {
        org::create(
            &h.state,
            &identity,
            &uuid::Uuid::new_v4().to_string(),
            "Same company",
        )
        .await
        .unwrap();
    }
    assert_eq!(
        org::create(
            &h.state,
            &identity,
            &uuid::Uuid::new_v4().to_string(),
            "Fourth",
        )
        .await
        .err(),
        Some(AuthError::Limited)
    );
    let organizations = org::list(&h.state, &identity).await.unwrap();
    assert_eq!(organizations.len(), 3);
    assert!(organizations.iter().all(|organization| {
        organization.name == "Same company" && organization.role == Role::Admin
    }));
    for invalid in ["", " \t ", "line\nbreak"] {
        assert_eq!(
            org::create(
                &h.state,
                &identity,
                &uuid::Uuid::new_v4().to_string(),
                invalid,
            )
            .await
            .err(),
            Some(AuthError::Invalid)
        );
    }
}

#[tokio::test]
async fn owner_admin_member_matrix_protects_owner_and_revokes_only_target_org_sessions() {
    let h = OrgHarness::new().await;
    let owner = h.owner();
    let member = h.member();
    let other_owner = h.other_org_owner();
    assert_eq!(
        org::remove(&h.state, &member, owner.member_id).await,
        Err(AuthError::Forbidden)
    );
    assert_eq!(
        org::remove(&h.state, &owner, owner.member_id).await,
        Err(AuthError::Forbidden)
    );
    assert_eq!(
        org::remove(&h.state, &other_owner, member.member_id).await,
        Err(AuthError::Forbidden)
    );
    org::set_role(&h.state, &owner, member.member_id, Role::Admin)
        .await
        .unwrap();
    let row = db::one(
        &h.database,
        "SELECT role,revision FROM neev_memberships WHERE id=$1",
        vec![member.member_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "role").unwrap(), "admin");
    assert_eq!(row.try_get::<i64>("", "revision").unwrap(), 2);
    let session = db::one(
        &h.database,
        "SELECT revoked_at FROM neev_sessions WHERE id=$1",
        vec![member.identity.session_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session.try_get::<Option<i64>>("", "revoked_at").unwrap(),
        Some(1_700_000_000)
    );
}

#[tokio::test]
async fn branding_uses_cosmetic_revision_without_invalidating_member_sessions() {
    let h = OrgHarness::new().await;
    let owner = h.owner();
    let member = h.member();
    let updated =
        org::update_branding(&h.state, &owner, 1, " Renamed company ", BrandingLogo::Keep)
            .await
            .unwrap();
    assert_eq!(updated.name, "Renamed company");
    assert_eq!(updated.branding_revision, 2);
    let tx = db::begin(&h.database).await.unwrap();
    assert_eq!(session::recheck(&tx, &member, 1_700_000_000).await, Ok(()));
    tx.commit().await.unwrap();
    assert_eq!(
        org::update_branding(&h.state, &owner, 1, "Stale", BrandingLogo::Remove)
            .await
            .err(),
        Some(AuthError::Conflict)
    );
    assert_eq!(
        org::update_branding(
            &h.state,
            &owner,
            2,
            "Renamed company",
            BrandingLogo::Upload("data:image/svg+xml;base64,PHN2Zz4=".into()),
        )
        .await
        .err(),
        Some(AuthError::Invalid)
    );
}

#[tokio::test]
async fn accepted_invite_replay_cannot_reactivate_a_removed_membership() {
    let h = OrgHarness::new().await;
    let token = h.invite("invitee@example.test").await;
    let identity = h.invitee();
    assert_eq!(h.accept(identity.clone(), &token).await, Ok(h.org_id()));
    assert_eq!(h.accept(identity.clone(), &token).await, Ok(h.org_id()));
    let before = db::one(
        &h.database,
        "SELECT id,role,revision FROM neev_memberships WHERE org_id=$1 AND user_id=$2",
        vec![h.org_id().into(), identity.user_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    let member_id = before.try_get::<i32>("", "id").unwrap();
    org::remove(&h.state, &h.owner(), member_id).await.unwrap();
    let removed = db::one(
        &h.database,
        "SELECT role,revision,active FROM neev_memberships WHERE id=$1",
        vec![member_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    let role = removed.try_get::<String>("", "role").unwrap();
    let revision = removed.try_get::<i64>("", "revision").unwrap();
    assert!(!removed.try_get::<bool>("", "active").unwrap());
    assert_eq!(h.accept(identity, &token).await, Err(AuthError::Forbidden));
    let replayed = db::one(
        &h.database,
        "SELECT role,revision,active FROM neev_memberships WHERE id=$1",
        vec![member_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(replayed.try_get::<String>("", "role").unwrap(), role);
    assert_eq!(replayed.try_get::<i64>("", "revision").unwrap(), revision);
    assert!(!replayed.try_get::<bool>("", "active").unwrap());
}

#[tokio::test]
async fn accepted_invite_receipt_replays_after_inviter_loses_authority() {
    let h = OrgHarness::new().await;
    let token = h.invite("invitee@example.test").await;
    let identity = h.invitee();
    assert_eq!(h.accept(identity.clone(), &token).await, Ok(h.org_id()));

    h.revoke_inviter_membership().await;

    assert_eq!(h.accept(identity, &token).await, Ok(h.org_id()));
}

#[tokio::test]
async fn revoked_and_expired_invitations_never_admit_the_recipient() {
    let h = OrgHarness::new().await;
    let revoked_token = h.invite("invitee@example.test").await;
    let invite_id = db::one(
        &h.database,
        "SELECT id FROM neev_invitations WHERE token_hash=$1",
        vec![crate::neev::auth::secrets::digest(
            crate::neev::auth::secrets::TokenPurpose::Invite,
            &revoked_token,
        )
        .unwrap()
        .into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<String>("", "id")
    .unwrap();
    invites::revoke(&h.state, &h.owner(), &invite_id)
        .await
        .unwrap();
    assert_eq!(
        h.accept(h.invitee(), &revoked_token).await,
        Err(AuthError::Forbidden)
    );

    let expired_token = h.invite("invitee@example.test").await;
    test_support::execute(
        &h.database,
        "UPDATE neev_invitations SET expires_at=1699999999 WHERE token_hash=$1",
        vec![crate::neev::auth::secrets::digest(
            crate::neev::auth::secrets::TokenPurpose::Invite,
            &expired_token,
        )
        .unwrap()
        .into()],
    )
    .await;
    assert_eq!(
        h.accept(h.invitee(), &expired_token).await,
        Err(AuthError::Forbidden)
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_admission_guard_precedes_task5_account_and_session_locks() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let user = test_support::native_account(&database, "guard-user", "guard@example.test").await;
    let identity = test_support::identity_context(&database, user, 1_700_000_000).await;
    let clock = std::sync::Arc::new(test_support::TestClock::new(1_700_000_000));
    let state = crate::neev::startup::ServerState::new(
        database.clone(),
        test_support::preview_config(),
        clock,
    )
    .unwrap();
    let oauth_admission = db::begin(&database).await.unwrap();
    db::lock_admission_guard(&oauth_admission).await.unwrap();
    let identity_for_create = identity.clone();
    let create = tokio::spawn(async move {
        org::create(
            &state,
            &identity_for_create,
            &uuid::Uuid::new_v4().to_string(),
            "Guarded org",
        )
        .await
    });
    test_support::wait_for_postgres_lock(
        &admin,
        "%SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE%",
    )
    .await;
    oauth_admission
        .query_one(Statement::from_sql_and_values(
            oauth_admission.get_database_backend(),
            "SELECT id FROM neev_accounts WHERE id=$1 FOR UPDATE",
            vec![user.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    oauth_admission
        .query_one(Statement::from_sql_and_values(
            oauth_admission.get_database_backend(),
            "SELECT id FROM neev_sessions WHERE id=$1 FOR UPDATE",
            vec![identity.session_id.clone().into()],
        ))
        .await
        .unwrap()
        .unwrap();
    oauth_admission.commit().await.unwrap();
    assert!(create.await.unwrap().is_ok());
    let quota = db::one(
        &database,
        "SELECT used FROM neev_quota_buckets WHERE kind='organization_creation_day'",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(quota.try_get::<i64>("", "used").unwrap(), 1);
    test_support::cleanup_postgres(database, admin, schema).await;
}
