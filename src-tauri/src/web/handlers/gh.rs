//! Cloud repo management for multi-tenant WorkOS. Everything is GitHub-backed
//! and server-side — no local folders. Uses the logged-in user's (encrypted)
//! GitHub token, resolved from their session cookie.
//!
//! Stage 1 (this file): list the user's GitHub repos for the "Open GitHub
//! project" picker. Clone-to-server + create-repo come next.

use std::sync::Arc;

use axum::{extract::Extension, http::HeaderMap, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

use crate::app_state::AppState;
use crate::web::handlers::github_auth;

#[derive(Serialize)]
pub struct RepoItem {
    full_name: String,
    name: String,
    owner: String,
    private: bool,
    default_branch: String,
    description: Option<String>,
    updated_at: Option<String>,
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// GET/POST /api/list_github_repos — the current user's GitHub repos, newest first.
pub async fn list_github_repos(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let db = &state.db.conn;
    let user = match github_auth::current_user(&headers, db).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "not signed in"})),
            )
                .into_response()
        }
    };
    let token = match user.gh_token_enc.as_deref().and_then(github_auth::decrypt) {
        Some(t) => t,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "no github token on record — sign in again"})),
            )
                .into_response()
        }
    };

    let http = reqwest::Client::new();
    let raw: Vec<serde_json::Value> = match http
        .get("https://api.github.com/user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member")
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "EazyWorkOS")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("github: {e}")})),
            )
                .into_response()
        }
    };

    let repos: Vec<RepoItem> = raw
        .into_iter()
        .map(|v| RepoItem {
            full_name: s(&v, "full_name"),
            name: s(&v, "name"),
            owner: v
                .get("owner")
                .map(|o| s(o, "login"))
                .unwrap_or_default(),
            private: v.get("private").and_then(|x| x.as_bool()).unwrap_or(false),
            default_branch: {
                let b = s(&v, "default_branch");
                if b.is_empty() { "main".into() } else { b }
            },
            description: v
                .get("description")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            updated_at: v
                .get("updated_at")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        })
        .collect();

    Json(repos).into_response()
}
