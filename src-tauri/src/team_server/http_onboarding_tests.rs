use std::sync::Arc;

use axum::http::header;
use axum_test::TestServer;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, Set,
};

use super::{
    router::build_router,
    test_server::{
        begin_pending, select_pending, session_browser, set_cookies, team_state, SessionBrowser,
    },
};
use crate::{
    db::entities::{org, team_session},
    team_access::{
        github::{GithubIdentity, GithubMembershipEvidence, GithubOrganizationChoice},
        session::TeamRole,
        test_auth::{fixed_now, token_hash, tokens},
    },
};

const GITHUB_USER_ID: i64 = 30_301;
const GITHUB_ORG_A: i64 = 10_201;
const GITHUB_ORG_B: i64 = 10_202;

fn identity() -> GithubIdentity {
    GithubIdentity {
        github_user_id: GITHUB_USER_ID,
        login: "team-user".into(),
        name: Some("Team User".into()),
        avatar_url: Some("https://avatars.example/user".into()),
    }
}

fn organization(
    github_org_id: i64,
    login: &str,
    avatar_url: &str,
    role: TeamRole,
) -> GithubOrganizationChoice {
    GithubOrganizationChoice {
        github_org_id,
        login: login.into(),
        avatar_url: Some(avatar_url.into()),
        role,
    }
}

fn evidence(organization: GithubOrganizationChoice) -> GithubMembershipEvidence {
    GithubMembershipEvidence {
        github_user_id: GITHUB_USER_ID,
        organization,
    }
}

async fn bootstrap(server: &TestServer, browser: &SessionBrowser) -> serde_json::Value {
    let response = server
        .get("/api/team/session")
        .add_header(header::COOKIE, &browser.cookies)
        .await;
    response.assert_status_ok();
    response.json()
}

#[tokio::test]
async fn zero_and_multiple_choices_never_auto_issue_authority_and_multiple_selects_explicitly() {
    let zero_fixture = team_state(
        vec![Ok(tokens("zero-access", None, None, None))],
        vec![Ok(identity())],
        vec![Ok(Vec::new())],
        Vec::new(),
    )
    .await;
    let zero_db = zero_fixture.state.db.clone();
    let zero_server =
        TestServer::new(build_router(Arc::new(zero_fixture.state))).expect("zero-choice server");
    let (_, zero_pending) = begin_pending(&zero_server, "zero-code", None).await;
    let zero_choices = zero_server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &zero_pending.cookies)
        .await;
    zero_choices.assert_status_ok();
    assert_eq!(zero_choices.json::<serde_json::Value>()["organizations"], serde_json::json!([]));
    assert!(set_cookies(&zero_choices)
        .iter()
        .all(|cookie| !cookie.starts_with("workos_team_session=")));
    assert_eq!(team_session::Entity::find().count(&zero_db).await.unwrap(), 0);

    let alpha = organization(
        GITHUB_ORG_A,
        "alpha-company",
        "https://avatars.example/alpha",
        TeamRole::Member,
    );
    let beta = organization(
        GITHUB_ORG_B,
        "beta-company",
        "https://avatars.example/beta",
        TeamRole::Admin,
    );
    let options = vec![alpha, beta.clone()];
    let multiple_fixture = team_state(
        vec![Ok(tokens("multiple-access", None, None, None))],
        vec![Ok(identity())],
        vec![Ok(options.clone()), Ok(options)],
        vec![Ok(evidence(beta))],
    )
    .await;
    let multiple_db = multiple_fixture.state.db.clone();
    let multiple_server = TestServer::new(build_router(Arc::new(multiple_fixture.state)))
        .expect("multiple-choice server");
    let (_, multiple_pending) = begin_pending(&multiple_server, "multiple-code", None).await;
    let multiple_choices = multiple_server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &multiple_pending.cookies)
        .await;
    multiple_choices.assert_status_ok();
    let choices: serde_json::Value = multiple_choices.json();
    assert_eq!(choices["organizations"].as_array().unwrap().len(), 2);
    assert_eq!(team_session::Entity::find().count(&multiple_db).await.unwrap(), 0);
    assert!(set_cookies(&multiple_choices)
        .iter()
        .all(|cookie| !cookie.starts_with("workos_team_session=")));

    let selected = select_pending(&multiple_server, &multiple_pending, "10202", None).await;
    selected.assert_status_ok();
    let session = session_browser(&selected);
    let session_json = bootstrap(&multiple_server, &session).await;
    assert_eq!(session_json["organization"]["login"], "beta-company");
    assert_eq!(session_json["role"], "admin");
}

#[tokio::test]
async fn prior_session_rotation_is_exact_other_devices_survive_and_malformed_restart_recovers() {
    let selected_org = organization(
        GITHUB_ORG_A,
        "alpha-company",
        "https://avatars.example/alpha",
        TeamRole::Member,
    );
    let fixture = team_state(
        vec![
            Ok(tokens("first-access", None, None, None)),
            Ok(tokens("rotated-access", None, None, None)),
            Ok(tokens("malformed-access", None, None, None)),
            Ok(tokens("recovery-access", None, None, None)),
        ],
        vec![Ok(identity()), Ok(identity()), Ok(identity()), Ok(identity())],
        vec![
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
            Ok(vec![selected_org.clone()]),
        ],
        vec![
            Ok(evidence(selected_org.clone())),
            Ok(evidence(selected_org.clone())),
            Ok(evidence(selected_org)),
        ],
    )
    .await;
    let db = fixture.state.db.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("rotation server");

    let (_, first_pending) = begin_pending(&server, "first-code", None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &first_pending.cookies)
        .await
        .assert_status_ok();
    let first_selected = select_pending(&server, &first_pending, "10201", None).await;
    first_selected.assert_status_ok();
    let first = session_browser(&first_selected);
    bootstrap(&server, &first).await;

    let first_row = team_session::Entity::find_by_id(token_hash(&first.session))
        .one(&db)
        .await
        .unwrap()
        .expect("first HTTP session row");
    let other_secret = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    let mut other_row = first_row.into_active_model();
    other_row.token_hash = Set(token_hash(other_secret));
    other_row.insert(&db).await.expect("other device session");
    let other = SessionBrowser {
        cookies: format!(
            "workos_team_session={other_secret}; workos_team_csrf={}",
            first.csrf
        ),
        session: other_secret.into(),
        csrf: first.csrf.clone(),
    };
    bootstrap(&server, &other).await;

    let (_, rotated_pending) = begin_pending(&server, "rotation-code", Some(&first.cookies)).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &rotated_pending.cookies)
        .await
        .assert_status_ok();
    let rotated_selected =
        select_pending(&server, &rotated_pending, "10201", Some(&first)).await;
    rotated_selected.assert_status_ok();
    let rotated = session_browser(&rotated_selected);
    server
        .get("/api/team/session")
        .add_header(header::COOKIE, &first.cookies)
        .await
        .assert_status_unauthorized();
    bootstrap(&server, &other).await;
    bootstrap(&server, &rotated).await;
    assert!(team_session::Entity::find_by_id(token_hash(&first.session))
        .one(&db)
        .await
        .unwrap()
        .is_none());
    assert!(team_session::Entity::find_by_id(token_hash(other_secret))
        .one(&db)
        .await
        .unwrap()
        .is_some());

    let malformed = "workos_team_session=short; workos_team_csrf=short";
    let (_, malformed_pending) = begin_pending(&server, "malformed-code", None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &malformed_pending.cookies)
        .await
        .assert_status_ok();
    let malformed_selection = select_pending(
        &server,
        &malformed_pending,
        "10201",
        Some(&SessionBrowser {
            cookies: malformed.into(),
            session: "short".into(),
            csrf: "short".into(),
        }),
    )
    .await;
    malformed_selection.assert_status_bad_request();
    assert!(set_cookies(&malformed_selection)
        .iter()
        .all(|cookie| !cookie.starts_with("workos_team_session=")));

    let (restart, recovery_pending) =
        begin_pending(&server, "recovery-code", Some(malformed)).await;
    assert!(set_cookies(&restart)
        .iter()
        .any(|cookie| cookie.starts_with("workos_team_session=") && cookie.contains("Max-Age=0")));
    assert!(set_cookies(&restart)
        .iter()
        .any(|cookie| cookie.starts_with("workos_team_csrf=") && cookie.contains("Max-Age=0")));
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &recovery_pending.cookies)
        .await
        .assert_status_ok();
    let recovered = select_pending(&server, &recovery_pending, "10201", None).await;
    recovered.assert_status_ok();
    bootstrap(&server, &session_browser(&recovered)).await;
}

#[tokio::test]
async fn stored_organization_branding_survives_provider_metadata_refresh_in_session_json() {
    let initial = organization(
        GITHUB_ORG_A,
        "alpha-company",
        "https://avatars.example/provider-initial",
        TeamRole::Admin,
    );
    let refreshed = organization(
        GITHUB_ORG_A,
        "alpha-renamed",
        "https://avatars.example/provider-refreshed",
        TeamRole::Admin,
    );
    let fixture = team_state(
        vec![
            Ok(tokens("branding-one", None, None, None)),
            Ok(tokens("branding-two", None, None, None)),
        ],
        vec![Ok(identity()), Ok(identity())],
        vec![
            Ok(vec![initial.clone()]),
            Ok(vec![initial.clone()]),
            Ok(vec![refreshed.clone()]),
            Ok(vec![refreshed.clone()]),
        ],
        vec![Ok(evidence(initial)), Ok(evidence(refreshed))],
    )
    .await;
    let db = fixture.state.db.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("branding server");

    let (_, initial_pending) = begin_pending(&server, "branding-one-code", None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &initial_pending.cookies)
        .await
        .assert_status_ok();
    let initial_selected = select_pending(&server, &initial_pending, "10201", None).await;
    initial_selected.assert_status_ok();

    let stored = org::Entity::find()
        .filter(org::Column::GithubOrgId.eq(GITHUB_ORG_A))
        .one(&db)
        .await
        .unwrap()
        .expect("created organization");
    let mut branded = stored.into_active_model();
    branded.name = Set(Some("Acme Operations".into()));
    branded.avatar_url = Set(Some("https://cdn.example/acme-logo.png".into()));
    branded.updated_at = Set(fixed_now());
    branded.update(&db).await.expect("stored organization branding");

    let (_, refreshed_pending) = begin_pending(&server, "branding-two-code", None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &refreshed_pending.cookies)
        .await
        .assert_status_ok();
    let refreshed_selected = select_pending(&server, &refreshed_pending, "10201", None).await;
    refreshed_selected.assert_status_ok();
    let refreshed_session = session_browser(&refreshed_selected);
    let session_json = bootstrap(&server, &refreshed_session).await;
    assert_eq!(session_json["organization"]["login"], "alpha-renamed");
    assert_eq!(session_json["organization"]["name"], "Acme Operations");
    assert_eq!(
        session_json["organization"]["avatarUrl"],
        "https://cdn.example/acme-logo.png"
    );
    assert_ne!(
        session_json["organization"]["avatarUrl"],
        "https://avatars.example/provider-refreshed"
    );
    assert_eq!(session_json["role"], "admin");
    assert_eq!(team_session::Entity::find().count(&db).await.unwrap(), 2);
}
