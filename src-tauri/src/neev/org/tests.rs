use sea_orm::{ConnectionTrait, Statement};
use std::sync::Arc;

use crate::neev::{
    auth::{oauth, secrets, session},
    config::ProviderPair,
    db,
    error::AuthError,
    org::{self, invites, BrandingLogo},
    test_support::{self, OrgHarness},
    types::{Provider, Role, SignInIntent},
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
async fn outstanding_invitation_cap_counts_only_live_pending_receipts() {
    let h = OrgHarness::new().await;
    let actor = h.owner();
    let now = 1_700_000_000_i64;
    for index in 0..100_i64 {
        let expires_at = if index == 98 { now } else { now + 3_600 };
        let revoked_at = (index == 99).then_some(now);
        test_support::execute(
            &h.database,
            "INSERT INTO neev_invitations(id,org_id,inviter_id,recipient_email,role,token_hash,expires_at,revoked_at) VALUES($1,$2,$3,'seed@example.test','member',$4,$5,$6)",
            vec![
                uuid::Uuid::new_v4().to_string().into(),
                h.org_id().into(),
                actor.identity.user_id.into(),
                format!("{index:064x}").into(),
                expires_at.into(),
                revoked_at.into(),
            ],
        )
        .await;
    }

    for _ in 0..2 {
        invites::send(
            &h.state,
            &actor,
            "boundary@example.test",
            Role::Member,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    }
    let active = db::one(
        &h.database,
        "SELECT COUNT(*) AS n FROM neev_invitations WHERE org_id=$1 AND revoked_at IS NULL AND accepted_at IS NULL AND expires_at>$2",
        vec![h.org_id().into(), now.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(active.try_get::<i64>("", "n").unwrap(), 100);
    assert_eq!(h.capture.len().await, 2);
    assert_eq!(
        invites::send(
            &h.state,
            &actor,
            "boundary@example.test",
            Role::Member,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await,
        Err(AuthError::Limited)
    );
    assert_eq!(h.capture.len().await, 2);
}

#[tokio::test]
async fn selection_rotation_denies_scoped_switch_and_removal_is_organization_local() {
    let database = test_support::database().await;
    let state = crate::neev::startup::ServerState::new(
        database.clone(),
        test_support::preview_config(),
        Arc::new(test_support::TestClock::new(1_700_000_000)),
    )
    .unwrap();
    let owner_one = test_support::native_account(
        &database,
        "selection-owner-one",
        "selection-owner-one@example.test",
    )
    .await;
    let owner_two = test_support::native_account(
        &database,
        "selection-owner-two",
        "selection-owner-two@example.test",
    )
    .await;
    let user = test_support::native_account(
        &database,
        "selection-member",
        "selection-member@example.test",
    )
    .await;
    let (org_one, _) = test_support::organization(&database, owner_one, "admin").await;
    let (org_two, _) = test_support::organization(&database, owner_two, "admin").await;
    let member_one = test_support::add_membership(&database, org_one, user, "member").await;
    test_support::add_membership(&database, org_two, user, "member").await;
    let owner_context =
        test_support::scoped_context(&database, owner_one, org_one, 1_700_000_000).await;

    let first_identity = session::issue_identity(&database, user, 1_700_000_000)
        .await
        .unwrap();
    let first_context = session::identity(&database, &first_identity.session, 1_700_000_000)
        .await
        .unwrap();
    let first_selected = org::select(&state, &first_context, org_one).await.unwrap();
    assert_eq!(
        session::identity(&database, &first_identity.session, 1_700_000_000)
            .await
            .err(),
        Some(AuthError::Unauthorized)
    );
    let scoped_identity = session::identity(&database, &first_selected.session, 1_700_000_000)
        .await
        .unwrap();
    assert_eq!(
        org::select(&state, &scoped_identity, org_two).await.err(),
        Some(AuthError::Unauthorized)
    );

    let second_identity = session::issue_identity(&database, user, 1_700_000_000)
        .await
        .unwrap();
    let second_context = session::identity(&database, &second_identity.session, 1_700_000_000)
        .await
        .unwrap();
    let second_selected = org::select(&state, &second_context, org_two).await.unwrap();
    assert_ne!(first_selected.session, second_selected.session);
    assert_eq!(
        session::identity(&database, &second_identity.session, 1_700_000_000)
            .await
            .err(),
        Some(AuthError::Unauthorized)
    );

    org::remove(&state, &owner_context, member_one)
        .await
        .unwrap();
    assert_eq!(
        session::organization(&database, &first_selected.session, 1_700_000_000)
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
    let first_hash = crate::neev::auth::secrets::digest(
        crate::neev::auth::secrets::TokenPurpose::Session,
        &first_selected.session,
    )
    .unwrap();
    let first_row = db::one(
        &database,
        "SELECT revoked_at FROM neev_sessions WHERE token_hash=$1",
        vec![first_hash.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first_row.try_get::<Option<i64>>("", "revoked_at").unwrap(),
        Some(1_700_000_000)
    );
    assert_eq!(
        session::organization(&database, &second_selected.session, 1_700_000_000)
            .await
            .unwrap()
            .org_id,
        org_two
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_admission_guard_precedes_task5_account_and_session_locks() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let user = test_support::native_account(&database, "guard-user", "guard@example.test").await;
    let identity = test_support::identity_context(&database, user, 1_700_000_000).await;
    let clock = Arc::new(test_support::TestClock::new(1_700_000_000));
    let state = crate::neev::startup::ServerState::new(
        database.clone(),
        test_support::preview_config(),
        clock.clone(),
    )
    .unwrap();
    let entered = Arc::new(tokio::sync::Barrier::new(2));
    let release = Arc::new(tokio::sync::Barrier::new(2));
    let mut oauth_config = test_support::preview_config();
    oauth_config.providers.google = Some(ProviderPair {
        id: "guard-google-client".into(),
        secret: "guard-google-secret".into(),
    });
    let oauth = Arc::new(
        oauth::Service::new(database.clone(), &oauth_config, clock)
            .unwrap()
            .with_admission_pause(entered.clone(), release.clone()),
    );
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let link_intent = SignInIntent::Link {
        user_id: identity.user_id,
        session_id: identity.session_id.clone(),
        auth_revision: identity.auth_revision,
    };
    let oauth_task =
        tokio::spawn(async move { oauth.begin(Provider::Google, link_intent, &browser).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.wait())
        .await
        .expect("actual OAuth admission should pause after acquiring the guard");
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
    release.wait().await;
    let authorization_url = oauth_task.await.unwrap().unwrap();
    assert!(authorization_url.contains("response_type=code"));
    let created = create.await.unwrap().unwrap();
    assert!(created.id > 0);
    let attempt = db::one(
        &database,
        "SELECT intent,link_user_id,link_session_id,consumed_at FROM neev_oauth_attempts",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(attempt.try_get::<String>("", "intent").unwrap(), "link");
    assert_eq!(attempt.try_get::<i32>("", "link_user_id").unwrap(), user);
    assert_eq!(
        attempt.try_get::<String>("", "link_session_id").unwrap(),
        identity.session_id
    );
    assert_eq!(
        attempt.try_get::<Option<i64>>("", "consumed_at").unwrap(),
        None
    );
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

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_authority_interleavings_preserve_owner_and_deny_removed_actor_writes() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let clock = Arc::new(test_support::TestClock::new(1_700_000_000));
    let capture = Arc::new(test_support::MailCapture::new());
    let mail = crate::neev::mail::Service::new(database.clone(), capture.clone());
    let state = Arc::new(
        crate::neev::startup::ServerState::with_mail(
            database.clone(),
            test_support::preview_config(),
            clock,
            mail,
        )
        .unwrap(),
    );
    let owner_user =
        test_support::native_account(&database, "race-owner", "race-owner@example.test").await;
    let (org_id, owner_member_id) =
        test_support::organization(&database, owner_user, "admin").await;
    let owner = test_support::scoped_context(&database, owner_user, org_id, 1_700_000_000).await;

    let account_blocker = db::begin(&database).await.unwrap();
    account_blocker
        .query_one(Statement::from_sql_and_values(
            account_blocker.get_database_backend(),
            "SELECT id FROM neev_accounts WHERE id=$1 FOR UPDATE",
            vec![owner_user.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let remove_state = state.clone();
    let remove_owner = owner.clone();
    let remove =
        tokio::spawn(
            async move { org::remove(&remove_state, &remove_owner, owner_member_id).await },
        );
    let demote_state = state.clone();
    let demote_owner = owner.clone();
    let demote = tokio::spawn(async move {
        org::set_role(&demote_state, &demote_owner, owner_member_id, Role::Member).await
    });
    test_support::wait_for_postgres_locks(&admin, "%neev_accounts%FOR UPDATE%", 2).await;
    account_blocker.commit().await.unwrap();
    assert_eq!(remove.await.unwrap(), Err(AuthError::Forbidden));
    assert_eq!(demote.await.unwrap(), Err(AuthError::Forbidden));
    let owner_row = db::one(
        &database,
        "SELECT role,revision,active FROM neev_memberships WHERE id=$1",
        vec![owner_member_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(owner_row.try_get::<String>("", "role").unwrap(), "admin");
    assert_eq!(owner_row.try_get::<i64>("", "revision").unwrap(), 1);
    assert!(owner_row.try_get::<bool>("", "active").unwrap());

    let editor_user =
        test_support::native_account(&database, "race-editor", "race-editor@example.test").await;
    let editor_member = test_support::add_membership(&database, org_id, editor_user, "admin").await;
    let editor = test_support::scoped_context(&database, editor_user, org_id, 1_700_000_000).await;
    let org_blocker = lock_postgres_organization(&database, org_id).await;
    let remove_state = state.clone();
    let remove_owner = owner.clone();
    let removal =
        tokio::spawn(async move { org::remove(&remove_state, &remove_owner, editor_member).await });
    test_support::wait_for_postgres_lock(&admin, "%neev_organizations%FOR UPDATE%").await;
    let edit_state = state.clone();
    let edit = tokio::spawn(async move {
        org::update_branding(&edit_state, &editor, 1, "Stale editor", BrandingLogo::Keep).await
    });
    test_support::wait_for_postgres_lock(&admin, "%neev_accounts%FOR UPDATE%").await;
    org_blocker.commit().await.unwrap();
    assert_eq!(removal.await.unwrap(), Ok(()));
    assert_eq!(edit.await.unwrap().err(), Some(AuthError::Forbidden));
    assert_eq!(org::details(&state, &owner).await.unwrap().name, "Test org");

    let inviter_user =
        test_support::native_account(&database, "race-inviter", "race-inviter@example.test").await;
    let invitee_user =
        test_support::native_account(&database, "race-invitee", "race-invitee@example.test").await;
    let inviter_member =
        test_support::add_membership(&database, org_id, inviter_user, "admin").await;
    let inviter =
        test_support::scoped_context(&database, inviter_user, org_id, 1_700_000_000).await;
    let invitee = test_support::identity_context(&database, invitee_user, 1_700_000_000).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let invitation_id = invites::send(
        &state,
        &inviter,
        "race-invitee@example.test",
        Role::Member,
        &request_id,
    )
    .await
    .unwrap();
    let token = capture.latest_fragment().await;
    let org_blocker = lock_postgres_organization(&database, org_id).await;
    let remove_state = state.clone();
    let remove_owner = owner.clone();
    let removal =
        tokio::spawn(
            async move { org::remove(&remove_state, &remove_owner, inviter_member).await },
        );
    test_support::wait_for_postgres_lock(&admin, "%neev_organizations%FOR UPDATE%").await;
    let accept_state = state.clone();
    let acceptance =
        tokio::spawn(async move { invites::accept(&accept_state, &invitee, &token, org_id).await });
    test_support::wait_for_postgres_lock(&admin, "%neev_accounts%FOR UPDATE%").await;
    org_blocker.commit().await.unwrap();
    assert_eq!(removal.await.unwrap(), Ok(()));
    assert_eq!(acceptance.await.unwrap(), Err(AuthError::Forbidden));
    let admitted = db::one(
        &database,
        "SELECT COUNT(*) AS n FROM neev_memberships WHERE org_id=$1 AND user_id=$2 AND active=true",
        vec![org_id.into(), invitee_user.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(admitted.try_get::<i64>("", "n").unwrap(), 0);
    let invitation = db::one(
        &database,
        "SELECT accepted_by,accepted_at,revoked_at FROM neev_invitations WHERE id=$1",
        vec![invitation_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        invitation
            .try_get::<Option<i32>>("", "accepted_by")
            .unwrap(),
        None
    );
    assert_eq!(
        invitation
            .try_get::<Option<i64>>("", "accepted_at")
            .unwrap(),
        None
    );
    assert_eq!(
        invitation.try_get::<Option<i64>>("", "revoked_at").unwrap(),
        None
    );
    let receipt = db::one(
        &database,
        "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action='invitation.send' AND request_id=$2",
        vec![inviter_user.into(), request_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        receipt.try_get::<String>("", "resource_id").unwrap(),
        invitation_id
    );
    let quota = database
        .query_all(Statement::from_string(
            database.get_database_backend(),
            "SELECT kind,used FROM neev_quota_buckets WHERE kind IN ('invitation_actor_day','invitation_org_day') ORDER BY kind",
        ))
        .await
        .unwrap();
    assert_eq!(quota.len(), 2);
    assert!(quota
        .iter()
        .all(|row| row.try_get::<i64>("", "used").unwrap() == 1));

    test_support::cleanup_postgres(database, admin, schema).await;
}

async fn lock_postgres_organization(
    database: &sea_orm::DatabaseConnection,
    org_id: i32,
) -> sea_orm::DatabaseTransaction {
    let tx = db::begin(database).await.unwrap();
    tx.query_one(Statement::from_sql_and_values(
        tx.get_database_backend(),
        "SELECT id FROM neev_organizations WHERE id=$1 FOR UPDATE",
        vec![org_id.into()],
    ))
    .await
    .unwrap()
    .unwrap();
    tx
}
