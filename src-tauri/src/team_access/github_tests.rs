use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use super::{
    github::{
        GithubEvidenceError, GithubMembershipClient, GithubMembershipSource,
    },
    session::TeamRole,
    test_http::{FixtureBody, FixtureResponse, FixtureServer},
};

const TOKEN: &str = "team-github-fixture-token";

impl FixtureServer {
    fn client(&self) -> GithubMembershipClient {
        GithubMembershipClient::new_for_test(self.origin(), Duration::from_secs(2)).unwrap()
    }

    fn client_with_timeout(&self, timeout: Duration) -> GithubMembershipClient {
        GithubMembershipClient::new_for_test(self.origin(), timeout).unwrap()
    }
}

fn user(id: i64, login: &str) -> Value {
    json!({
        "id": id,
        "login": login,
        "name": "Alice Example",
        "avatar_url": "https://avatars.githubusercontent.com/u/301",
        "type": "User"
    })
}

fn membership(user_id: i64, org_id: i64, org_login: &str, state: &str, role: &str) -> Value {
    json!({
        "state": state,
        "role": role,
        "user": {"id": user_id, "login": "alice", "type": "User"},
        "organization": {
            "id": org_id,
            "login": org_login,
            "avatar_url": format!("https://avatars.githubusercontent.com/u/{org_id}")
        }
    })
}

#[tokio::test]
async fn identity_returns_validated_metadata_and_required_headers() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        user(301, "alice"),
    )])
    .await;
    let identity = server.client().identity(TOKEN).await.unwrap();

    assert_eq!(identity.github_user_id, 301);
    assert_eq!(identity.login, "alice");
    assert_eq!(identity.name.as_deref(), Some("Alice Example"));
    assert_eq!(
        identity.avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/u/301")
    );
    let requests = server.requests();
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].path, "/user");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer team-github-fixture-token")
    );
    assert_eq!(requests[0].user_agent.as_deref(), Some("EazyWorkOS"));
    assert_eq!(
        requests[0].accept.as_deref(),
        Some("application/vnd.github+json")
    );
    assert_eq!(requests[0].api_version.as_deref(), Some("2022-11-28"));
}

#[tokio::test]
async fn identity_accepts_missing_optional_metadata_and_discards_unsafe_values() {
    let mut unsafe_metadata = user(301, "alice");
    unsafe_metadata["name"] = json!(format!("{}\n", "x".repeat(256)));
    unsafe_metadata["avatar_url"] = json!("https://evil.example/avatar.png");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(
            StatusCode::OK,
            json!({"id": 301, "login": "alice", "type": "User"}),
        ),
        FixtureResponse::json(
            StatusCode::OK,
            json!({"id": 301, "login": "alice", "name": null, "avatar_url": null, "type": "User"}),
        ),
        FixtureResponse::json(StatusCode::OK, unsafe_metadata),
    ])
    .await;

    for _ in 0..3 {
        let identity = server.client().identity(TOKEN).await.unwrap();
        assert_eq!(
            (identity.github_user_id, identity.login.as_str()),
            (301, "alice")
        );
        assert_eq!(identity.name, None);
        assert_eq!(identity.avatar_url, None);
    }
}

#[tokio::test]
async fn identity_rejects_invalid_authority_shapes() {
    let cases = vec![
        json!({"id": 0, "login": "alice", "type": "User"}),
        json!({"id": -1, "login": "alice", "type": "User"}),
        json!({"login": "alice", "type": "User"}),
        json!({"id": 301, "type": "User"}),
        json!({"id": 301, "login": "alice", "type": "Bot"}),
        json!({"id": 301, "login": "-alice", "type": "User"}),
        json!({"id": 301, "login": "alice--ops", "type": "User"}),
    ];
    let mut responses: Vec<_> = cases
        .into_iter()
        .map(|body| FixtureResponse::json(StatusCode::OK, body))
        .collect();
    responses.push(FixtureResponse::raw(StatusCode::OK, b"not json".to_vec()));
    let server = FixtureServer::start(responses).await;

    for _ in 0..8 {
        assert_eq!(
            server.client().identity(TOKEN).await,
            Err(GithubEvidenceError::Unavailable)
        );
    }
}

#[tokio::test]
async fn individual_membership_maps_member_and_admin_roles() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(
            StatusCode::OK,
            membership(301, 202, "beta-company", "active", "member"),
        ),
        FixtureResponse::json(
            StatusCode::OK,
            membership(301, 202, "Beta-Company", "active", "admin"),
        ),
    ])
    .await;

    let member = server
        .client()
        .verify_membership(TOKEN, 301, 202, "beta-company")
        .await
        .unwrap();
    assert_eq!(member.github_user_id, 301);
    assert_eq!(member.organization.github_org_id, 202);
    assert_eq!(member.organization.role, TeamRole::Member);
    let admin = server
        .client()
        .verify_membership(TOKEN, 301, 202, "beta-company")
        .await
        .unwrap();
    assert_eq!(admin.organization.role, TeamRole::Admin);
    assert_eq!(server.requests()[0].path, "/user/memberships/orgs/beta-company");
}

#[tokio::test]
async fn individual_membership_rejects_stable_id_and_login_mismatches() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, membership(999, 202, "beta-company", "active", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 999, "beta-company", "active", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "other-company", "active", "member")),
    ]).await;

    for _ in 0..3 {
        assert_eq!(
            server.client().verify_membership(TOKEN, 301, 202, "beta-company").await,
            Err(GithubEvidenceError::NotMember)
        );
    }
}

#[tokio::test]
async fn individual_membership_rejects_pending_and_invalid_wire_values() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "pending", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "active", "owner")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "mystery", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "active", "admin")).with_header("content-type", "application/json"),
    ]).await;

    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    for _ in 0..2 {
        assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::Unavailable));
    }
    // The final valid fixture proves the preceding failures were validation,
    // rather than an accidentally non-functional fixture server.
    assert!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await.is_ok());
}

#[tokio::test]
async fn active_organizations_returns_empty_without_personal_fallback() {
    let server = FixtureServer::start(vec![FixtureResponse::json(StatusCode::OK, json!([]))]).await;
    assert_eq!(server.client().active_organizations(TOKEN, 301).await.unwrap(), vec![]);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn active_organizations_paginates_locally_sorts_and_excludes_pending() {
    let page_one: Vec<_> = (0..100)
        .map(|offset| membership(301, 1_000 + offset, &format!("org-{offset}"), "active", "member"))
        .collect();
    let page_two = json!([
        membership(301, 202, "beta-company", "pending", "member"),
        membership(301, 102, "alpha-company", "active", "admin")
    ]);
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!(page_one)).with_header(
            "link",
            "<http://127.0.0.1:9/steal-token>; rel=\"next\"",
        ),
        FixtureResponse::json(StatusCode::OK, page_two),
    ]).await;

    let organizations = server.client().active_organizations(TOKEN, 301).await.unwrap();
    assert_eq!(organizations.len(), 101);
    assert_eq!(organizations[0].github_org_id, 102);
    assert_eq!(organizations[0].role, TeamRole::Admin);
    assert_eq!(organizations[1].github_org_id, 1_000);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/user/memberships/orgs");
    assert_eq!(requests[0].query.as_deref(), Some("state=active&per_page=100&page=1"));
    assert_eq!(requests[1].query.as_deref(), Some("state=active&per_page=100&page=2"));
}

#[tokio::test]
async fn active_organizations_rejects_duplicates_and_contradictory_rows() {
    let exact_duplicate = membership(301, 202, "beta-company", "active", "member");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!([exact_duplicate.clone(), exact_duplicate])),
        FixtureResponse::json(StatusCode::OK, json!([
            membership(301, 202, "beta-company", "active", "member"),
            membership(301, 202, "other-company", "active", "admin")
        ])),
    ]).await;

    for _ in 0..2 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn active_organizations_distinguishes_wrong_user_from_invalid_rows() {
    let mut invalid_user = membership(301, 202, "beta-company", "active", "member");
    invalid_user["user"]["type"] = json!("Bot");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!([membership(999, 202, "beta-company", "active", "member")])),
        FixtureResponse::json(StatusCode::OK, json!([invalid_user])),
        FixtureResponse::json(StatusCode::OK, json!([membership(301, 202, "beta-company", "active", "owner")])),
        FixtureResponse::json(StatusCode::OK, json!([membership(301, 202, "beta-company", "mystery", "member")])),
    ]).await;

    assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::NotMember));
    for _ in 0..3 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn one_hundred_full_pages_fail_instead_of_returning_truncated_success() {
    let responses = (0..100)
        .map(|page| {
            let rows: Vec<_> = (0..100)
                .map(|offset| {
                    let id = 10_000 + page * 100 + offset;
                    membership(301, id, &format!("org-{id}"), "active", "member")
                })
                .collect();
            FixtureResponse::json(StatusCode::OK, json!(rows))
        })
        .collect();
    let server = FixtureServer::start(responses).await;

    assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    assert_eq!(server.requests().len(), 100);
}

#[tokio::test]
async fn a_page_larger_than_the_requested_page_size_is_unavailable() {
    let oversized_page: Vec<_> = (0..101)
        .map(|offset| {
            membership(
                301,
                20_000 + offset,
                &format!("oversized-{offset}"),
                "active",
                "member",
            )
        })
        .collect();
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!(oversized_page)),
        FixtureResponse::json(StatusCode::OK, json!([])),
    ])
    .await;

    assert_eq!(
        server.client().active_organizations(TOKEN, 301).await,
        Err(GithubEvidenceError::Unavailable)
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn statuses_map_without_turning_provider_failures_into_denials() {
    let server = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::UNAUTHORIZED, ""),
        FixtureResponse::raw(StatusCode::FORBIDDEN, ""),
        FixtureResponse::raw(StatusCode::NOT_FOUND, ""),
        FixtureResponse::raw(StatusCode::FORBIDDEN, "").with_header("x-ratelimit-remaining", "0"),
        FixtureResponse::raw(StatusCode::FORBIDDEN, "").with_header("retry-after", "1"),
        FixtureResponse::raw(StatusCode::TOO_MANY_REQUESTS, ""),
        FixtureResponse::raw(StatusCode::INTERNAL_SERVER_ERROR, ""),
        FixtureResponse::raw(StatusCode::CREATED, "{}"),
    ]).await;

    assert_eq!(server.client().identity(TOKEN).await, Err(GithubEvidenceError::ReauthenticationRequired));
    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    for _ in 0..5 {
        assert_eq!(server.client().identity(TOKEN).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn listing_non_rate_limited_forbidden_and_not_found_are_unavailable() {
    let server = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::FORBIDDEN, ""),
        FixtureResponse::raw(StatusCode::NOT_FOUND, ""),
    ]).await;
    for _ in 0..2 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn redirects_are_rejected_without_contacting_the_target() {
    let target = FixtureServer::start(vec![FixtureResponse::json(StatusCode::OK, user(301, "alice"))]).await;
    let redirect = format!("{}/stolen", target.origin());
    let source = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::FOUND, "").with_header("location", redirect),
    ]).await;

    assert_eq!(source.client().identity(TOKEN).await, Err(GithubEvidenceError::Unavailable));
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(target.requests(), vec![]);
}

#[tokio::test]
async fn invalid_inputs_fail_before_http_and_errors_do_not_leak_tokens() {
    let server = FixtureServer::start(vec![]).await;
    for token in ["", "  ", "abc\rdef", "abc\ndef"] {
        let error = server.client().identity(token).await.unwrap_err();
        assert_eq!(error, GithubEvidenceError::ReauthenticationRequired);
        if !token.is_empty() {
            assert!(!format!("{error:?} {error}").contains(token));
        }
    }
    for (user_id, org_id, login) in [(0, 202, "beta-company"), (301, 0, "beta-company"), (301, 202, "bad--login")] {
        assert_eq!(server.client().verify_membership(TOKEN, user_id, org_id, login).await, Err(GithubEvidenceError::NotMember));
    }
    assert_eq!(server.client().active_organizations(TOKEN, 0).await, Err(GithubEvidenceError::NotMember));
    assert_eq!(server.requests(), vec![]);
}

#[tokio::test]
async fn decoded_body_cap_accepts_valid_bounded_json_and_rejects_valid_oversized_json() {
    fn padded_user_chunks(padding_bytes: usize) -> Vec<Vec<u8>> {
        let mut body = user(301, "alice");
        body["padding"] = json!("x".repeat(padding_bytes));
        serde_json::to_vec(&body)
            .unwrap()
            .chunks(65_536)
            .map(<[u8]>::to_vec)
            .collect()
    }

    let server = FixtureServer::start(vec![
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Chunks(padded_user_chunks(1_047_000)),
        },
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Chunks(padded_user_chunks(1_049_000)),
        },
    ])
    .await;

    let identity = server.client().identity(TOKEN).await.unwrap();
    assert_eq!(identity.github_user_id, 301);
    assert_eq!(identity.login, "alice");
    assert_eq!(
        server.client().identity(TOKEN).await,
        Err(GithubEvidenceError::Unavailable)
    );
}

#[tokio::test]
async fn pagination_is_bounded_by_complete_operation_timeout() {
    let full_page: Vec<_> = (0..100)
        .map(|offset| {
            membership(
                301,
                30_000 + offset,
                &format!("timed-org-{offset}"),
                "active",
                "member",
            )
        })
        .collect();
    let server = FixtureServer::start(vec![
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Delayed(
                Duration::from_millis(70),
                serde_json::to_vec(&full_page).unwrap(),
            ),
        },
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Delayed(
                Duration::from_millis(70),
                serde_json::to_vec(&json!([])).unwrap(),
            ),
        },
    ])
    .await;

    assert_eq!(
        server
            .client_with_timeout(Duration::from_millis(100))
            .active_organizations(TOKEN, 301)
            .await,
        Err(GithubEvidenceError::Unavailable)
    );
}
