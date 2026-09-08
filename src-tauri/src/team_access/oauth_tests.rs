use std::{collections::BTreeMap, time::Duration};

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use reqwest::Url;
use serde_json::json;

use super::{
    oauth::{GithubOAuthClient, GithubOAuthSource, OAuthError},
    test_http::{FixtureBody, FixtureResponse, FixtureServer, RecordedRequest},
};

const CLIENT_ID: &str = "invented-client-id";
const CLIENT_SECRET: &str = "invented-client-secret";
const CALLBACK: &str = "http://127.0.0.1:3210/auth/github/callback";
const STATE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const VERIFIER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 12, 30, 0).unwrap()
}

fn client(server: &FixtureServer) -> GithubOAuthClient {
    client_with_timeout(server, Duration::from_secs(2))
}

fn client_with_timeout(server: &FixtureServer, timeout: Duration) -> GithubOAuthClient {
    GithubOAuthClient::new_for_test(
        CLIENT_ID.to_owned(),
        CLIENT_SECRET.to_owned(),
        Url::parse(CALLBACK).unwrap(),
        server.origin(),
        timeout,
    )
    .unwrap()
}

fn form(request: &RecordedRequest) -> BTreeMap<String, String> {
    let encoded = std::str::from_utf8(&request.body).unwrap();
    Url::parse(&format!("http://fixture.invalid/?{encoded}"))
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn expiring_tokens() -> serde_json::Value {
    json!({
        "access_token": "invented-access-token",
        "token_type": "bearer",
        "expires_in": 28_800,
        "refresh_token": "invented-refresh-token",
        "refresh_token_expires_in": 15_897_600,
        "scope": "read:user"
    })
}

#[test]
fn authorization_url_uses_fixed_github_route_and_pkce_without_legacy_scope() {
    let client = GithubOAuthClient::new(
        CLIENT_ID.to_owned(),
        CLIENT_SECRET.to_owned(),
        Url::parse(CALLBACK).unwrap(),
    )
    .unwrap();

    let url = client.authorization_url(STATE, VERIFIER).unwrap();
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();

    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("github.com"));
    assert_eq!(url.path(), "/login/oauth/authorize");
    assert_eq!(query.len(), 5);
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["redirect_uri"], CALLBACK);
    assert_eq!(query["state"], STATE);
    assert_eq!(
        query["code_challenge"],
        "_-BU_nrgy23GXDr5th1SCfQ5hR20PQulmXM33xVGaOs"
    );
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query.contains_key("scope"));
    assert!(!query.contains_key("login"));
}

#[test]
fn constructors_reject_invalid_credentials_callbacks_and_test_origins() {
    for invalid in ["", "has space", "has\nnewline"] {
        assert_eq!(
            GithubOAuthClient::new(
                invalid.to_owned(),
                CLIENT_SECRET.to_owned(),
                Url::parse(CALLBACK).unwrap(),
            )
            .err()
            .unwrap(),
            OAuthError::Unavailable
        );
        assert_eq!(
            GithubOAuthClient::new(
                CLIENT_ID.to_owned(),
                invalid.to_owned(),
                Url::parse(CALLBACK).unwrap(),
            )
            .err()
            .unwrap(),
            OAuthError::Unavailable
        );
    }
    assert_eq!(
        GithubOAuthClient::new(
            "x".repeat(4_097),
            CLIENT_SECRET.to_owned(),
            Url::parse(CALLBACK).unwrap(),
        )
        .err()
        .unwrap(),
        OAuthError::Unavailable
    );

    for callback in [
        "http://example.com/auth/github/callback",
        "http://localhost/auth/github/callback",
        "http://127.0.0.2/auth/github/callback",
        "ftp://127.0.0.1/auth/github/callback",
        "https://example.com/not-the-callback",
        "https://user@example.com/auth/github/callback",
        "https://example.com/auth/github/callback?next=x",
        "https://example.com/auth/github/callback#fragment",
    ] {
        assert_eq!(
            GithubOAuthClient::new(
                CLIENT_ID.to_owned(),
                CLIENT_SECRET.to_owned(),
                Url::parse(callback).unwrap(),
            )
            .err()
            .unwrap(),
            OAuthError::Unavailable
        );
    }
    for callback in [
        "https://example.com/auth/github/callback",
        "http://127.0.0.1/auth/github/callback",
        "http://[::1]/auth/github/callback",
    ] {
        assert!(GithubOAuthClient::new(
            CLIENT_ID.to_owned(),
            CLIENT_SECRET.to_owned(),
            Url::parse(callback).unwrap(),
        )
        .is_ok());
    }

    assert_eq!(
        GithubOAuthClient::new_for_test(
            CLIENT_ID.to_owned(),
            CLIENT_SECRET.to_owned(),
            Url::parse(CALLBACK).unwrap(),
            "http://example.com",
            Duration::from_secs(1),
        )
        .err()
        .unwrap(),
        OAuthError::Unavailable
    );
}

#[test]
fn authorization_input_rejects_invalid_state_and_verifier() {
    let client = GithubOAuthClient::new(
        CLIENT_ID.to_owned(),
        CLIENT_SECRET.to_owned(),
        Url::parse(CALLBACK).unwrap(),
    )
    .unwrap();
    for state in ["", "A123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"] {
        assert_eq!(
            client.authorization_url(state, VERIFIER),
            Err(OAuthError::ReauthenticationRequired)
        );
    }
    assert_eq!(
        client.authorization_url(STATE, "not-a-browser-secret"),
        Err(OAuthError::ReauthenticationRequired)
    );
}

#[tokio::test]
async fn exchange_posts_exact_form_headers_and_returns_expiring_tokens() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        expiring_tokens(),
    )])
    .await;

    let tokens = client(&server)
        .exchange_code("invented-code", VERIFIER, fixture_now())
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "invented-access-token");
    assert_eq!(
        tokens.refresh_token.as_deref(),
        Some("invented-refresh-token")
    );
    assert_eq!(
        tokens.access_expires_at,
        Some(fixture_now() + ChronoDuration::seconds(28_800))
    );
    assert_eq!(
        tokens.refresh_expires_at,
        Some(fixture_now() + ChronoDuration::seconds(15_897_600))
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path, "/login/oauth/access_token");
    assert_eq!(requests[0].query, None);
    assert_eq!(requests[0].user_agent.as_deref(), Some("EazyWorkOS"));
    assert_eq!(requests[0].accept.as_deref(), Some("application/json"));
    assert!(requests[0]
        .content_type
        .as_deref()
        .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded")));
    assert_eq!(
        form(&requests[0]),
        BTreeMap::from([
            ("client_id".into(), CLIENT_ID.into()),
            ("client_secret".into(), CLIENT_SECRET.into()),
            ("code".into(), "invented-code".into()),
            ("code_verifier".into(), VERIFIER.into()),
            ("redirect_uri".into(), CALLBACK.into()),
        ])
    );
    assert!(!requests[0].path.contains(CLIENT_SECRET));
}

#[tokio::test]
async fn exchange_accepts_a_complete_nonexpiring_token_response() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        json!({"access_token": "invented-access", "token_type": "bearer"}),
    )])
    .await;

    let tokens = client(&server)
        .exchange_code("invented-code", VERIFIER, fixture_now())
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "invented-access");
    assert_eq!(tokens.refresh_token, None);
    assert_eq!(tokens.access_expires_at, None);
    assert_eq!(tokens.refresh_expires_at, None);
}

#[tokio::test]
async fn refresh_requires_and_returns_a_complete_rotated_expiring_set() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        json!({
            "access_token": "invented-new-access",
            "token_type": "bearer",
            "expires_in": 28_800,
            "refresh_token": "invented-new-refresh",
            "refresh_token_expires_in": 15_897_600
        }),
    )])
    .await;

    let tokens = client(&server)
        .refresh("invented-refresh-token", fixture_now())
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "invented-new-access");
    assert_eq!(
        tokens.refresh_token.as_deref(),
        Some("invented-new-refresh")
    );
    assert_eq!(
        tokens.access_expires_at,
        Some(fixture_now() + ChronoDuration::seconds(28_800))
    );
    let requests = server.requests();
    assert_eq!(requests[0].query, None);
    assert_eq!(form(&requests[0])["grant_type"], "refresh_token");
    assert_eq!(
        form(&requests[0]),
        BTreeMap::from([
            ("client_id".into(), CLIENT_ID.into()),
            ("client_secret".into(), CLIENT_SECRET.into()),
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), "invented-refresh-token".into()),
        ])
    );
}

#[tokio::test]
async fn refresh_never_falls_back_to_the_submitted_refresh_token() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        json!({
            "access_token": "invented-new-access",
            "token_type": "bearer",
            "expires_in": 28_800,
            "refresh_token_expires_in": 15_897_600
        }),
    )])
    .await;

    assert_eq!(
        client(&server)
            .refresh("invented-old-refresh", fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn token_lifetime_sets_reject_partial_nonpositive_and_unrepresentable_values() {
    let valid_base = json!({
        "access_token": "invented-access",
        "token_type": "bearer",
        "expires_in": 60,
        "refresh_token": "invented-refresh",
        "refresh_token_expires_in": 120
    });
    let mut partial = valid_base.clone();
    partial.as_object_mut().unwrap().remove("refresh_token");
    let mut zero = valid_base.clone();
    zero["expires_in"] = json!(0);
    let mut negative = valid_base.clone();
    negative["refresh_token_expires_in"] = json!(-1);
    let mut wrong_type = valid_base;
    wrong_type["expires_in"] = json!(18_446_744_073_709_551_615_u64);
    let server = FixtureServer::start(
        [partial, zero, negative, wrong_type]
            .into_iter()
            .map(|value| FixtureResponse::json(StatusCode::OK, value))
            .collect(),
    )
    .await;

    for _ in 0..4 {
        assert_eq!(
            client(&server)
                .exchange_code("invented-code", VERIFIER, fixture_now())
                .await
                .err(),
            Some(OAuthError::Unavailable)
        );
    }
}

#[tokio::test]
async fn token_lifetimes_are_capped_and_checked_before_time_addition() {
    let capped = json!({
        "access_token": "invented-access",
        "token_type": "bearer",
        "expires_in": 999_999_999,
        "refresh_token": "invented-refresh",
        "refresh_token_expires_in": 999_999_999
    });
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, capped.clone()),
        FixtureResponse::json(StatusCode::OK, capped),
    ])
    .await;

    let tokens = client(&server)
        .exchange_code("invented-code", VERIFIER, fixture_now())
        .await
        .unwrap();
    let cap = ChronoDuration::days(366);
    assert_eq!(tokens.access_expires_at, Some(fixture_now() + cap));
    assert_eq!(tokens.refresh_expires_at, Some(fixture_now() + cap));

    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, DateTime::<Utc>::MAX_UTC)
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn known_token_fields_are_strictly_validated_while_unknown_fields_are_ignored() {
    let cases = [
        json!({"access_token": "invented", "token_type": "Bearer"}),
        json!({"access_token": "", "token_type": "bearer"}),
        json!({"access_token": "has space", "token_type": "bearer"}),
        json!({"access_token": "x".repeat(4_097), "token_type": "bearer"}),
        json!({"access_token": 42, "token_type": "bearer"}),
        json!({"access_token": "invented", "token_type": "bearer", "expires_in": "60"}),
        json!({
            "access_token": "invented",
            "token_type": "bearer",
            "expires_in": 60,
            "refresh_token": "bad refresh",
            "refresh_token_expires_in": 120
        }),
    ];
    let mut responses: Vec<_> = cases
        .into_iter()
        .map(|value| FixtureResponse::json(StatusCode::OK, value))
        .collect();
    responses.push(FixtureResponse::json(
        StatusCode::OK,
        json!({
            "access_token": "invented-valid",
            "token_type": "bearer",
            "future_field": {"ignored": true}
        }),
    ));
    let server = FixtureServer::start(responses).await;

    for _ in 0..7 {
        assert_eq!(
            client(&server)
                .exchange_code("invented-code", VERIFIER, fixture_now())
                .await
                .err(),
            Some(OAuthError::Unavailable)
        );
    }
    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .unwrap()
            .access_token,
        "invented-valid"
    );
}

#[tokio::test]
async fn malformed_success_body_is_unavailable() {
    let server =
        FixtureServer::start(vec![FixtureResponse::raw(StatusCode::OK, "not json")]).await;

    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn oauth_error_payloads_map_known_rejections_without_leaking_descriptions() {
    let cases = [
        ("bad_verification_code", OAuthError::ReauthenticationRequired),
        ("bad_refresh_token", OAuthError::ReauthenticationRequired),
        ("access_denied", OAuthError::ReauthenticationRequired),
        ("expired_token", OAuthError::ReauthenticationRequired),
        ("incorrect_client_credentials", OAuthError::Unavailable),
        ("redirect_uri_mismatch", OAuthError::Unavailable),
        ("future_error", OAuthError::Unavailable),
    ];
    let server = FixtureServer::start(
        cases
            .iter()
            .map(|(error, _)| {
                FixtureResponse::json(
                    StatusCode::OK,
                    json!({"error": error, "error_description": "invented-sensitive-detail"}),
                )
            })
            .collect(),
    )
    .await;

    for (_, expected) in cases {
        let error = client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err()
            .unwrap();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("invented-sensitive-detail"));
    }
}

#[tokio::test]
async fn response_with_both_error_and_tokens_is_unavailable() {
    let mut response = expiring_tokens();
    response["error"] = json!("bad_verification_code");
    let server = FixtureServer::start(vec![FixtureResponse::json(StatusCode::OK, response)]).await;

    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn statuses_distinguish_reauthentication_from_provider_unavailability() {
    let server = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::UNAUTHORIZED, "not json"),
        FixtureResponse::json(
            StatusCode::BAD_REQUEST,
            json!({"error": "bad_verification_code"}),
        ),
        FixtureResponse::json(
            StatusCode::BAD_REQUEST,
            json!({"error": "incorrect_client_credentials"}),
        ),
        FixtureResponse::json(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "bad_verification_code"}),
        ),
        FixtureResponse::json(
            StatusCode::FORBIDDEN,
            json!({"error": "bad_verification_code"}),
        )
        .with_header("x-ratelimit-remaining", "0"),
        FixtureResponse::json(
            StatusCode::FORBIDDEN,
            json!({"error": "bad_verification_code"}),
        )
        .with_header("retry-after", "1"),
        FixtureResponse::json(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "bad_verification_code"}),
        ),
        FixtureResponse::json(StatusCode::CREATED, expiring_tokens()),
    ])
    .await;

    for expected in [
        OAuthError::ReauthenticationRequired,
        OAuthError::ReauthenticationRequired,
        OAuthError::Unavailable,
        OAuthError::Unavailable,
        OAuthError::Unavailable,
        OAuthError::Unavailable,
        OAuthError::Unavailable,
        OAuthError::Unavailable,
    ] {
        assert_eq!(
            client(&server)
                .exchange_code("invented-code", VERIFIER, fixture_now())
                .await
                .err(),
            Some(expected)
        );
    }
}

#[tokio::test]
async fn redirects_are_rejected_without_contacting_the_receiver() {
    let receiver = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        expiring_tokens(),
    )])
    .await;
    let redirect = format!("{}/login/oauth/access_token", receiver.origin());
    let source = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::FOUND, "").with_header("location", redirect),
    ])
    .await;

    assert_eq!(
        client(&source)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(receiver.requests(), vec![]);
}

#[tokio::test]
async fn streaming_body_cap_accepts_valid_bounded_json_and_rejects_valid_oversized_json() {
    fn padded_chunks(padding_bytes: usize) -> Vec<Vec<u8>> {
        let mut response = expiring_tokens();
        response["ignored_padding"] = json!("x".repeat(padding_bytes));
        serde_json::to_vec(&response)
            .unwrap()
            .chunks(4_096)
            .map(<[u8]>::to_vec)
            .collect()
    }

    let server = FixtureServer::start(vec![
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Chunks(padded_chunks(64_000)),
        },
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Chunks(padded_chunks(66_000)),
        },
    ])
    .await;

    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .unwrap()
            .access_token,
        "invented-access-token"
    );
    assert_eq!(
        client(&server)
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn whole_operation_timeout_bounds_delayed_response_body() {
    let server = FixtureServer::start(vec![FixtureResponse {
        status: StatusCode::OK,
        headers: vec![("content-type".into(), "application/json".into())],
        body: FixtureBody::Delayed(
            Duration::from_millis(100),
            serde_json::to_vec(&expiring_tokens()).unwrap(),
        ),
    }])
    .await;

    assert_eq!(
        client_with_timeout(&server, Duration::from_millis(30))
            .exchange_code("invented-code", VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::Unavailable)
    );
}

#[tokio::test]
async fn invalid_exchange_and_refresh_inputs_fail_before_http_and_do_not_leak() {
    let server = FixtureServer::start(vec![]).await;
    for code in ["", "has space", "has\nnewline"] {
        let error = client(&server)
            .exchange_code(code, VERIFIER, fixture_now())
            .await
            .err()
            .unwrap();
        assert_eq!(error, OAuthError::ReauthenticationRequired);
        if !code.is_empty() {
            assert!(!format!("{error:?} {error}").contains(code));
        }
    }
    assert_eq!(
        client(&server)
            .exchange_code(&"x".repeat(4_097), VERIFIER, fixture_now())
            .await
            .err(),
        Some(OAuthError::ReauthenticationRequired)
    );
    assert_eq!(
        client(&server)
            .exchange_code("invented-code", "short", fixture_now())
            .await
            .err(),
        Some(OAuthError::ReauthenticationRequired)
    );
    for refresh_token in ["", "has space", "has\rreturn"] {
        assert_eq!(
            client(&server)
                .refresh(refresh_token, fixture_now())
                .await
                .err(),
            Some(OAuthError::ReauthenticationRequired)
        );
    }
    assert_eq!(
        client(&server)
            .refresh(&"x".repeat(4_097), fixture_now())
            .await
            .err(),
        Some(OAuthError::ReauthenticationRequired)
    );
    assert_eq!(server.requests(), vec![]);
}
