use std::sync::Arc;

use axum::{body::Bytes, http::{header, Method, StatusCode}};
use axum_test::TestServer;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};

use super::{
    router::build_router,
    test_server::{begin_pending, select_pending, session_browser, team_state, SessionBrowser},
};
use crate::{
    db::entities::{org, user},
    team_access::{
        github::{GithubIdentity, GithubMembershipEvidence, GithubOrganizationChoice},
        session::TeamRole,
        test_auth::tokens,
    },
};

const USER_ID: i64 = 30_301;
const ORG_ID: i64 = 10_201;
const ORG_B_ID: i64 = 10_202;

fn identity() -> GithubIdentity {
    GithubIdentity {
        github_user_id: USER_ID,
        login: "alice".into(),
        name: Some("Alice Atlas".into()),
        avatar_url: None,
    }
}

fn organization(role: TeamRole) -> GithubOrganizationChoice {
    GithubOrganizationChoice {
        github_org_id: ORG_ID,
        login: "alpha-company".into(),
        avatar_url: None,
        role,
    }
}

fn evidence(role: TeamRole) -> GithubMembershipEvidence {
    GithubMembershipEvidence {
        github_user_id: USER_ID,
        organization: organization(role),
    }
}

fn persona_identity(github_user_id: i64, login: &str) -> GithubIdentity {
    GithubIdentity {
        github_user_id,
        login: login.into(),
        name: None,
        avatar_url: None,
    }
}

fn persona_organization(
    github_org_id: i64,
    login: &str,
    role: TeamRole,
) -> GithubOrganizationChoice {
    GithubOrganizationChoice {
        github_org_id,
        login: login.into(),
        avatar_url: None,
        role,
    }
}

fn persona_evidence(
    github_user_id: i64,
    organization: GithubOrganizationChoice,
) -> GithubMembershipEvidence {
    GithubMembershipEvidence {
        github_user_id,
        organization,
    }
}

async fn onboard(server: &TestServer, code: &str, github_org_id: i64) -> SessionBrowser {
    let (_, pending) = begin_pending(server, code, None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &pending.cookies)
        .await
        .assert_status_ok();
    let selected = select_pending(server, &pending, &github_org_id.to_string(), None).await;
    selected.assert_status_ok();
    session_browser(&selected)
}

async fn selected_server(role: TeamRole, proof_count: usize) -> (TestServer, sea_orm::DatabaseConnection, SessionBrowser) {
    let selected = organization(role);
    let fixture = team_state(
        vec![Ok(tokens("organization-access", None, None, None))],
        vec![Ok(identity())],
        vec![Ok(vec![selected.clone()]), Ok(vec![selected])],
        (0..=proof_count).map(|_| Ok(evidence(role))).collect(),
    )
    .await;
    let db = fixture.state.db.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).unwrap();
    let (_, pending) = begin_pending(&server, "organization-code", None).await;
    server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &pending.cookies)
        .await
        .assert_status_ok();
    let selected = select_pending(&server, &pending, &ORG_ID.to_string(), None).await;
    selected.assert_status_ok();
    (server, db, session_browser(&selected))
}

#[tokio::test]
async fn http_organization_manifest_denies_head_and_requires_a_team_session() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let server = TestServer::new(build_router(Arc::new(fixture.state))).unwrap();

    server
        .method(Method::HEAD, "/api/team/organization")
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    let response = server.get("/api/team/organization").await;
    response.assert_status_unauthorized();
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    for cookies in [
        format!("workos_session={}", "a".repeat(64)),
        format!(
            "workos_team_pending={}; workos_team_pending_csrf={}",
            "b".repeat(64),
            "c".repeat(64)
        ),
    ] {
        server
            .get("/api/team/organization")
            .add_header(header::COOKIE, cookies)
            .await
            .assert_status_unauthorized();
    }
    server
        .patch("/api/team/organization")
        .add_header(header::AUTHORIZATION, "Bearer invented-operator")
        .add_header(header::ORIGIN, "https://workos.example")
        .json(&serde_json::json!({"name":"Atlas","logo":{"action":"keep"}}))
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn http_organization_actual_onboarding_reads_and_admin_patch_updates_only_branding() {
    let (server, db, alice) = selected_server(TeamRole::Admin, 1).await;
    let before = org::Entity::find().one(&db).await.unwrap().unwrap();

    let read = server
        .get("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .await;
    read.assert_status_ok();
    let initial: serde_json::Value = read.json();
    assert_eq!(initial["login"], "alpha-company");
    assert_eq!(initial["name"], "alpha-company");
    assert_eq!(initial["canEdit"], true);
    assert_eq!(initial["avatarUrl"], serde_json::Value::Null);
    assert_eq!(initial.as_object().unwrap().len(), 5);
    assert!(initial.get("githubOrgId").is_none());

    let response = server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&serde_json::json!({"name":"Atlas Team","logo":{"action":"keep"}}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<serde_json::Value>()["name"], "Atlas Team");
    let after = org::Entity::find_by_id(before.id).one(&db).await.unwrap().unwrap();
    assert_eq!(after.github_org_id, before.github_org_id);
    assert_eq!(after.login, before.login);
    assert_eq!(after.name.as_deref(), Some("Atlas Team"));
}

#[tokio::test]
async fn http_organization_unseeded_personas_keep_independent_selected_org_authority() {
    const ALICE: i64 = 30_401;
    const BOB: i64 = 30_402;
    const CAROL: i64 = 30_403;
    let alpha_admin = persona_organization(ORG_ID, "alpha-company", TeamRole::Admin);
    let alpha_member = persona_organization(ORG_ID, "alpha-company", TeamRole::Member);
    let beta_admin = persona_organization(ORG_B_ID, "beta-company", TeamRole::Admin);
    let alice_choices = vec![alpha_admin.clone(), beta_admin.clone()];
    let fixture = team_state(
        vec![
            Ok(tokens("alice-a-access", None, None, None)),
            Ok(tokens("alice-b-access", None, None, None)),
            Ok(tokens("bob-a-access", None, None, None)),
            Ok(tokens("carol-b-access", None, None, None)),
        ],
        vec![
            Ok(persona_identity(ALICE, "alice")),
            Ok(persona_identity(ALICE, "alice")),
            Ok(persona_identity(BOB, "bob")),
            Ok(persona_identity(CAROL, "carol")),
        ],
        vec![
            Ok(alice_choices.clone()),
            Ok(alice_choices.clone()),
            Ok(alice_choices.clone()),
            Ok(alice_choices),
            Ok(vec![alpha_member.clone()]),
            Ok(vec![alpha_member.clone()]),
            Ok(vec![beta_admin.clone()]),
            Ok(vec![beta_admin.clone()]),
        ],
        vec![
            Ok(persona_evidence(ALICE, alpha_admin.clone())),
            Ok(persona_evidence(ALICE, beta_admin.clone())),
            Ok(persona_evidence(BOB, alpha_member.clone())),
            Ok(persona_evidence(CAROL, beta_admin)),
            Ok(persona_evidence(ALICE, alpha_admin.clone())),
            Ok(persona_evidence(ALICE, alpha_admin)),
            Ok(persona_evidence(BOB, alpha_member)),
        ],
    )
    .await;
    let db = fixture.state.db.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).unwrap();

    let alice_a = onboard(&server, "alice-a-code", ORG_ID).await;
    let alice_b = onboard(&server, "alice-b-code", ORG_B_ID).await;
    let bob_a = onboard(&server, "bob-a-code", ORG_ID).await;
    let carol_b = onboard(&server, "carol-b-code", ORG_B_ID).await;
    assert_ne!(alice_a.session, alice_b.session);

    for (browser, expected_login, can_edit) in [
        (&alice_a, "alpha-company", true),
        (&alice_b, "beta-company", true),
        (&bob_a, "alpha-company", false),
        (&carol_b, "beta-company", true),
    ] {
        let response = server
            .get("/api/team/organization")
            .add_header(header::COOKIE, &browser.cookies)
            .await;
        response.assert_status_ok();
        let body: serde_json::Value = response.json();
        assert_eq!(body["login"], expected_login);
        assert_eq!(body["canEdit"], can_edit);
    }

    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice_a.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice_a.csrf)
        .json(&serde_json::json!({"name":"Atlas Team","logo":{"action":"keep"}}))
        .await
        .assert_status_ok();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &bob_a.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &bob_a.csrf)
        .json(&serde_json::json!({
            "name":"Forged Beta",
            "logo":{"action":"remove"},
            "orgId": ORG_B_ID,
            "role":"admin"
        }))
        .await
        .assert_status_forbidden();

    let alpha = org::Entity::find()
        .filter(org::Column::GithubOrgId.eq(ORG_ID))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let beta = org::Entity::find()
        .filter(org::Column::GithubOrgId.eq(ORG_B_ID))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alpha.name.as_deref(), Some("Atlas Team"));
    assert!(beta.name.is_none());
    for browser in [&alice_b, &carol_b] {
        let response = server
            .get("/api/team/organization")
            .add_header(header::COOKIE, &browser.cookies)
            .await;
        response.assert_status_ok();
        let body: serde_json::Value = response.json();
        assert_eq!(body["login"], "beta-company");
        assert_eq!(body["name"], "beta-company");
    }
    let bob_view = server
        .get("/api/team/organization")
        .add_header(header::COOKIE, &bob_a.cookies)
        .await;
    bob_view.assert_status_ok();
    assert_eq!(bob_view.json::<serde_json::Value>()["name"], "Atlas Team");
}

#[tokio::test]
async fn http_organization_member_is_read_only_despite_forged_legacy_owner() {
    let (server, db, bob) = selected_server(TeamRole::Member, 1).await;
    let person = user::Entity::find().one(&db).await.unwrap().unwrap();
    let mut active = person.into_active_model();
    active.role = Set("owner".into());
    active.update(&db).await.unwrap();

    let read = server
        .get("/api/team/organization")
        .add_header(header::COOKIE, &bob.cookies)
        .await;
    read.assert_status_ok();
    assert_eq!(read.json::<serde_json::Value>()["canEdit"], false);
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &bob.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &bob.csrf)
        .json(&serde_json::json!({
            "name":"Forged",
            "logo":{"action":"keep"},
            "role":"admin",
            "orgId":999,
            "actor":999
        }))
        .await
        .assert_status_forbidden();
    assert!(org::Entity::find().one(&db).await.unwrap().unwrap().name.is_none());
}

#[tokio::test]
async fn http_organization_patch_rejects_ambiguous_security_headers_and_bounded_bad_bodies() {
    let (server, db, alice) = selected_server(TeamRole::Admin, 8).await;
    let valid = serde_json::json!({"name":"Atlas","logo":{"action":"keep"}});

    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_forbidden();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://evil.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_forbidden();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_forbidden();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_unauthorized();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .json(&valid)
        .await
        .assert_status_unauthorized();
    server
        .patch("/api/team/organization")
        .add_header(
            header::COOKIE,
            format!("workos_team_session={}", alice.session),
        )
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_unauthorized();
    let wrong_csrf = "f".repeat(64);
    server
        .patch("/api/team/organization")
        .add_header(
            header::COOKIE,
            format!(
                "workos_team_session={}; workos_team_csrf={wrong_csrf}",
                alice.session
            ),
        )
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &wrong_csrf)
        .json(&valid)
        .await
        .assert_status_unauthorized();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", "f".repeat(64))
        .json(&valid)
        .await
        .assert_status_unauthorized();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .text("not-json")
        .await
        .assert_status_bad_request();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&serde_json::json!({
            "name":"Forged",
            "logo":{"action":"keep"},
            "actor":301,
            "orgId":201,
            "role":"admin"
        }))
        .await
        .assert_status_bad_request();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(Bytes::from_static(b"{"))
        .await
        .assert_status_bad_request();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(Bytes::from(vec![b' '; 2 * 1024 * 1024 + 1]))
        .await
        .assert_status_bad_request();
    server
        .patch("/api/team/organization")
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::COOKIE, &alice.cookies)
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("x-workos-csrf", &alice.csrf)
        .json(&valid)
        .await
        .assert_status_unauthorized();
    assert!(org::Entity::find().one(&db).await.unwrap().unwrap().name.is_none());
}

#[tokio::test]
async fn http_organization_patch_requires_fresh_proof_and_upstream_failure_never_writes() {
    for (reply, expected_status) in [
        (
            Err(crate::team_access::github::GithubEvidenceError::Unavailable),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (Ok(evidence(TeamRole::Member)), StatusCode::UNAUTHORIZED),
    ] {
        let selected = organization(TeamRole::Admin);
        let fixture = team_state(
            vec![Ok(tokens("fresh-proof-access", None, None, None))],
            vec![Ok(identity())],
            vec![Ok(vec![selected.clone()]), Ok(vec![selected])],
            vec![Ok(evidence(TeamRole::Admin)), reply],
        )
        .await;
        let db = fixture.state.db.clone();
        let server = TestServer::new(build_router(Arc::new(fixture.state))).unwrap();
        let (_, pending) = begin_pending(&server, "fresh-proof-code", None).await;
        server
            .get("/api/team/onboarding")
            .add_header(header::COOKIE, &pending.cookies)
            .await
            .assert_status_ok();
        let selected = select_pending(&server, &pending, &ORG_ID.to_string(), None).await;
        selected.assert_status_ok();
        let alice = session_browser(&selected);
        server
            .get("/api/team/organization")
            .add_header(header::COOKIE, &alice.cookies)
            .await
            .assert_status_ok();

        let response = server
            .patch("/api/team/organization")
            .add_header(header::COOKIE, &alice.cookies)
            .add_header(header::ORIGIN, "https://workos.example")
            .add_header("x-workos-csrf", &alice.csrf)
            .json(&serde_json::json!({"name":"Must Not Persist","logo":{"action":"remove"}}))
            .await;
        assert_eq!(response.status_code(), expected_status);
        assert!(org::Entity::find().one(&db).await.unwrap().unwrap().name.is_none());
    }
}
