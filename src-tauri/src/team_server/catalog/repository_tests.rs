use std::sync::{Arc, Mutex};

use axum::{body::Body, extract::State, http::{HeaderMap, Response, StatusCode, Uri}, routing::any, Router};
use sea_orm::{EntityTrait, TransactionTrait};

use super::{GithubTeamRepositoryAccess, TeamRepositoryAccess};
use crate::{
    db::entities::user,
    team_access::{principal::ProofRequirement, test_auth::{cookie_headers, principal_fixture, tokens, ALICE_A_TOKEN}},
    team_server::{handlers::ApiError, state::TeamState, test_server::team_state},
};

async fn fixture(State(requests): State<Arc<Mutex<Vec<(String, String)>>>>, headers: HeaderMap, uri: Uri) -> Response<Body> {
    requests.lock().unwrap().push((uri.path().into(), headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().into()));
    match uri.path() {
        "/repos/alice-a/repository" => Response::builder().status(StatusCode::OK).body(Body::from(r#"{"id":9007199254740992,"full_name":"alice-a/repository"}"#)).unwrap(),
        "/repos/alice-a/repository/branches/feature%2Fteam" | "/repos/alice-a/repository/branches/feature/team" => Response::builder().status(StatusCode::OK).body(Body::from(r#"{"name":"feature/team"}"#)).unwrap(),
        "/repos/redirect/repo" => Response::builder().status(StatusCode::FOUND).header("location", "/repos/alice-a/repository").body(Body::empty()).unwrap(),
        "/repos/oversized/repo" => Response::builder().status(StatusCode::OK).body(Body::from(vec![b'x'; 1024 * 1024 + 1])).unwrap(),
        "/repos/malformed/repo" => Response::builder().status(StatusCode::OK).body(Body::from("not-json")).unwrap(),
        "/repos/unauthorized/repo" => Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::empty()).unwrap(),
        "/repos/forbidden/repo" => Response::builder().status(StatusCode::FORBIDDEN).body(Body::empty()).unwrap(),
        "/repos/missing/repo" => Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap(),
        "/repos/rate-limit/repo" => Response::builder().status(StatusCode::FORBIDDEN).header("x-ratelimit-remaining", "0").body(Body::empty()).unwrap(),
        "/repos/too-many/repo" => Response::builder().status(StatusCode::TOO_MANY_REQUESTS).header("retry-after", "1").body(Body::empty()).unwrap(),
        _ => Response::builder().status(StatusCode::NOT_FOUND).body(Body::from(r#"{"message":"not found"}"#)).unwrap(),
    }
}

async fn access_fixture() -> (TeamState, crate::team_access::principal::AuthContext, GithubTeamRepositoryAccess, Arc<Mutex<Vec<(String, String)>>>) {
    let principal = principal_fixture(Vec::new()).await;
    let principals = principal.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let state = TeamState::new(principal.db, shell.state.config, principal.vault, shell.state.oauth, shell.state.onboarding, principal.credentials, principals, principal.clock);
    let context = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(fixture)).with_state(requests.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let access = GithubTeamRepositoryAccess::new_for_test(&state, &origin).unwrap_or_else(|_| panic!("test access"));
    (state, context, access, requests)
}

#[tokio::test]
async fn github_access_uses_personal_credential_and_fixed_metadata_endpoints() {
    let (_, context, access, requests) = access_fixture().await;
    let verified = access.verify(&context, "alice-a/repository", "feature/team").await.unwrap_or_else(|_| panic!("verify"));
    assert_eq!(verified.github_repository_id, "9007199254740992");
    assert_eq!(verified.starting_branch, "feature/team");
    assert_eq!(requests.lock().unwrap().as_slice(), [
        ("/repos/alice-a/repository".into(), "Bearer alice-access".into()),
        ("/repos/alice-a/repository/branches/feature%2Fteam".into(), "Bearer alice-access".into()),
    ]);
}

#[tokio::test]
async fn github_access_rejects_redirects_oversize_malformed_and_http_failures_safely() {
    let (_, context, access, requests) = access_fixture().await;
    for repository in ["redirect/repo", "oversized/repo", "malformed/repo"] {
        assert!(matches!(
            access.verify(&context, repository, "main").await,
            Err(ApiError::Unavailable)
        ));
    }
    assert!(matches!(
        access.verify(&context, "unauthorized/repo", "main").await,
        Err(ApiError::Unauthorized)
    ));
    for repository in ["forbidden/repo", "missing/repo"] {
        assert!(matches!(
            access.verify(&context, repository, "main").await,
            Err(ApiError::NotFound)
        ));
    }
    for repository in ["rate-limit/repo", "too-many/repo"] {
        assert!(matches!(
            access.verify(&context, repository, "main").await,
            Err(ApiError::Unavailable)
        ));
    }
    let paths = requests.lock().unwrap().iter().map(|request| request.0.clone()).collect::<Vec<_>>();
    assert!(!paths.iter().any(|path| path == "/repos/alice-a/repository"), "redirect was followed");
}

#[tokio::test]
async fn github_access_rejects_a_credential_generation_advance_before_network() {
    let (state, context, access, requests) = access_fixture().await;
    let github_user_id = user::Entity::find_by_id(context.user_id).one(&state.db).await.unwrap().unwrap().github_user_id;
    let transaction = state.db.begin().await.unwrap();
    state.credentials.install_in_transaction(
        &transaction,
        context.user_id,
        github_user_id,
        &tokens("rotated-access", None, None, None),
        state.clock.now(),
    ).await.unwrap();
    transaction.commit().await.unwrap();
    assert!(matches!(
        access.verify(&context, "alice-a/repository", "main").await,
        Err(ApiError::Unauthorized)
    ));
    assert!(requests.lock().unwrap().is_empty());
}
