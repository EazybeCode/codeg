use std::sync::{Arc, Mutex};

use axum::{body::Body, extract::State, http::{HeaderMap, Response, StatusCode, Uri}, routing::any, Router};

use super::{GithubTeamRepositoryAccess, TeamRepositoryAccess};
use crate::{
    team_access::{principal::ProofRequirement, test_auth::{cookie_headers, principal_fixture, ALICE_A_TOKEN}},
    team_server::{state::TeamState, test_server::team_state},
};

async fn fixture(State(requests): State<Arc<Mutex<Vec<(String, String)>>>>, headers: HeaderMap, uri: Uri) -> Response<Body> {
    requests.lock().unwrap().push((uri.path().into(), headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().into()));
    let (status, body) = match uri.path() {
        "/repos/alice-a/repository" => (StatusCode::OK, r#"{"id":9007199254740992,"full_name":"alice-a/repository"}"#),
        "/repos/alice-a/repository/branches/feature%2Fteam" | "/repos/alice-a/repository/branches/feature/team" => (StatusCode::OK, r#"{"name":"feature/team"}"#),
        _ => (StatusCode::NOT_FOUND, r#"{"message":"not found"}"#),
    };
    Response::builder().status(status).body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn github_access_uses_personal_credential_and_fixed_metadata_endpoints() {
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
    let verified = access.verify(&context, "alice-a/repository", "feature/team").await.unwrap_or_else(|_| panic!("verify"));
    assert_eq!(verified.github_repository_id, "9007199254740992");
    assert_eq!(verified.starting_branch, "feature/team");
    assert_eq!(requests.lock().unwrap().as_slice(), [
        ("/repos/alice-a/repository".into(), "Bearer alice-access".into()),
        ("/repos/alice-a/repository/branches/feature%2Fteam".into(), "Bearer alice-access".into()),
    ]);
}
