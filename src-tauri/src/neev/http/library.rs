//! HTTP handlers for organization library (skills and MCP configurations).
//!
//! GET /api/neev/library - List items (any org member)
//! POST /api/neev/library/import - Import skill ZIP/MD or MCP JSON (admin only)
//! PUT /api/neev/library/{id}/secrets - Replace an integration's credentials (admin only)
//! POST /api/neev/library/{id}/test - Test an integration (admin only)
//! PATCH /api/neev/library/{id} - Enable/disable item (admin only)
//! DELETE /api/neev/library/{id} - Delete item (admin only)

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{rejection::PathRejection, Path, State},
    http::{HeaderMap, Request, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};

use crate::neev::{library, startup::ServerState};

use super::{catalog, content::safe_path, private_json, ApiError};

/// Body limit for import: base64-encoded ZIP up to 10MB -> ~14MB base64 + envelope.
const IMPORT_BODY_LIMIT: usize = 15 * 1024 * 1024;
/// Small body for patch/delete.
const SMALL_BODY_LIMIT: usize = 4096;
/// Credentials body: a handful of short names and tokens.
const SECRETS_BODY_LIMIT: usize = 64 * 1024;

/// Request to import a skill or MCP config.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportInput {
    file_name: String,
    data: String, // base64
}

// The payload carries plaintext tokens: Debug prints the file name only.
impl std::fmt::Debug for ImportInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportInput")
            .field("file_name", &self.file_name)
            .field("data", &format_args!("<{} base64 bytes>", self.data.len()))
            .finish()
    }
}

/// Request to update a library item (enable/disable).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchInput {
    expected_revision: String,
    enabled: bool,
}

/// Request to delete a library item.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteInput {
    expected_revision: String,
}

/// Parse revision string to i64.
fn parse_revision(s: &str) -> Result<i64, ApiError> {
    let value = s.parse::<i64>().map_err(|_| ApiError::Invalid)?;
    if value < 1 || value.to_string() != s {
        return Err(ApiError::Invalid);
    }
    Ok(value)
}

/// Response with library items.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryListDto {
    items: Vec<library::LibraryItem>,
}

/// Response to an import: stored items plus the servers that were not.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResultDto {
    items: Vec<library::LibraryItem>,
    skipped: Vec<library::SkippedServer>,
}

/// Request to replace an integration's credentials. Values are never echoed
/// (the type has no `Debug`, so a stray `{:?}` cannot leak them either).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretsInput {
    expected_revision: String,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

/// Request to test an integration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestInput {
    expected_revision: String,
}

/// Response with successful deletion.
#[derive(Debug, Serialize)]
struct DeleteResultDto {
    ok: bool,
}

/// GET /api/neev/library - List all library items for the organization.
///
/// Any org member can read. Session cookie required.
pub(super) async fn list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let items = library::list(&state, &actor).await?;
    Ok(private_json(LibraryListDto { items }))
}

/// POST /api/neev/library - Import a skill ZIP/MD or MCP JSON.
///
/// Admin only. CSRF + origin required.
pub(super) async fn import(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<ImportInput>(&state, request, IMPORT_BODY_LIMIT).await?;

    // Validate filename is present and reasonable
    if input.file_name.is_empty() || input.file_name.len() > 255 {
        return Err(ApiError::Invalid);
    }

    let import_request = library::ImportRequest {
        file_name: input.file_name,
        data: input.data,
    };
    let outcome = library::import(&state, &actor, import_request).await?;

    // Nothing stored but something skipped: 422 with the reasons, so the UI can
    // explain instead of showing a generic failure.
    let status = if outcome.items.is_empty() && !outcome.skipped.is_empty() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::CREATED
    };
    let mut response = private_json(ImportResultDto {
        items: outcome.items,
        skipped: outcome.skipped,
    });
    *response.status_mut() = status;
    Ok(response)
}

/// PUT /api/neev/library/{id}/secrets - Replace an integration's credentials.
///
/// Admin only. CSRF + origin required. Values are write-only.
pub(super) async fn update_secrets(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    super::content::canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<SecretsInput>(&state, request, SECRETS_BODY_LIMIT)
            .await?;
    let secrets_request = library::SecretsRequest {
        expected_revision: parse_revision(&input.expected_revision)?,
        env: input.env,
        headers: input.headers,
    };
    let item = library::update_secrets(&state, &actor, &id, secrets_request).await?;
    Ok(private_json(item))
}

/// POST /api/neev/library/{id}/test - Test an integration through the runtime.
///
/// Admin only. CSRF + origin required.
pub(super) async fn test_integration(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    super::content::canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<TestInput>(&state, request, SMALL_BODY_LIMIT).await?;
    let item = library::test_integration(
        &state,
        &actor,
        &id,
        parse_revision(&input.expected_revision)?,
    )
    .await?;
    Ok(private_json(item))
}

/// PATCH /api/neev/library/{id} - Enable/disable a library item.
///
/// Admin only. CSRF + origin required.
pub(super) async fn update(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;

    // Validate UUID format
    super::content::canonical_uuid(&id)?;

    let (input, actor) =
        catalog::organization_mutation::<PatchInput>(&state, request, SMALL_BODY_LIMIT).await?;

    let patch_request = library::PatchRequest {
        expected_revision: parse_revision(&input.expected_revision)?,
        enabled: input.enabled,
    };
    let item = library::patch(&state, &actor, &id, patch_request).await?;

    Ok(private_json(item))
}

/// DELETE /api/neev/library/{id} - Delete a library item.
///
/// Admin only. CSRF + origin required.
pub(super) async fn delete(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;

    // Validate UUID format
    super::content::canonical_uuid(&id)?;

    let (input, actor) =
        catalog::organization_mutation::<DeleteInput>(&state, request, SMALL_BODY_LIMIT).await?;

    let delete_request = library::DeleteRequest {
        expected_revision: parse_revision(&input.expected_revision)?,
    };
    let _result = library::delete(&state, &actor, &id, delete_request).await?;

    Ok(private_json(DeleteResultDto { ok: true }))
}

// HTTP layer tests are minimal: domain logic is tested in library.rs.
// These tests verify DTO serialization and handler wiring.

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify ImportInput deserializes from camelCase JSON.
    #[test]
    fn import_input_parses_camel_case() {
        let json = r#"{"fileName":"skill.md","data":"SGVsbG8="}"#;
        let input: ImportInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.file_name, "skill.md");
        assert_eq!(input.data, "SGVsbG8="); // "Hello" base64
    }

    #[test]
    fn import_input_debug_prints_the_file_name_only() {
        let input: ImportInput =
            serde_json::from_str(r#"{"fileName":"mcp.json","data":"c3ludGhldGljLXRva2Vu"}"#).unwrap();
        let text = format!("{input:?}");
        assert!(text.contains("mcp.json"));
        assert!(!text.contains("c3ludGhldGljLXRva2Vu"));
    }

    /// Verify ImportInput rejects unknown fields.
    #[test]
    fn import_input_rejects_unknown_fields() {
        let json = r#"{"fileName":"skill.md","data":"SGVsbG8=","extra":"field"}"#;
        let result: Result<ImportInput, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    /// Verify PatchInput deserializes from camelCase JSON with string revision.
    #[test]
    fn patch_input_parses_camel_case() {
        let json = r#"{"expectedRevision":"42","enabled":true}"#;
        let input: PatchInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.expected_revision, "42");
        assert!(input.enabled);
    }

    /// Verify DeleteInput deserializes from camelCase JSON with string revision.
    #[test]
    fn delete_input_parses_camel_case() {
        let json = r#"{"expectedRevision":"7"}"#;
        let input: DeleteInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.expected_revision, "7");
    }

    /// Verify revision parsing.
    #[test]
    fn parse_revision_works() {
        assert_eq!(parse_revision("42").unwrap(), 42);
        assert_eq!(parse_revision("1").unwrap(), 1);
        assert!(parse_revision("").is_err());
        assert!(parse_revision("abc").is_err());
    }

    /// Verify LibraryListDto serializes with camelCase.
    #[test]
    fn list_dto_serializes_camel_case() {
        let dto = LibraryListDto {
            items: vec![library::LibraryItem {
                id: "abc".to_string(),
                name: "Test".to_string(),
                description: "Desc".to_string(),
                kind: "skill".to_string(),
                enabled: true,
                revision: 1,
                files: vec!["SKILL.md".to_string()],
                setup_note: None,
                integration: None,
            }],
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains(r#""items":"#));
        assert!(json.contains(r#""setupNote":null"#));
    }

    /// Verify DeleteResultDto serializes correctly.
    #[test]
    fn delete_result_serializes() {
        let dto = DeleteResultDto { ok: true };
        let json = serde_json::to_string(&dto).unwrap();
        assert_eq!(json, r#"{"ok":true}"#);
    }
    async fn cookies_for(
        h: &crate::neev::test_support::OrgHarness,
        actor: &crate::neev::types::OrgContext,
    ) -> (String, String) {
        use crate::neev::auth::session;
        let now = h.state().clock.now_unix();
        let issued = session::issue_identity(&h.database, actor.identity.user_id, now)
            .await
            .unwrap();
        let identity = session::identity(&h.database, &issued.session, now)
            .await
            .unwrap();
        let scoped = session::select_organization(&h.database, &identity, actor.org_id, now)
            .await
            .unwrap();
        let names = super::super::cookies::CookieNames::for_preview(true);
        (
            format!(
                "{}={}; {}={}",
                names.session, scoped.session, names.csrf, scoped.csrf
            ),
            scoped.csrf,
        )
    }

    #[tokio::test]
    async fn shared_library_http_contract_and_permissions() {
        use axum::http::header;
        use base64::Engine;
        use serde_json::{json, Value};
        let h = crate::neev::test_support::OrgHarness::new().await;
        let server = axum_test::TestServer::new(super::super::router(h.state.clone())).unwrap();
        let (owner, csrf) = cookies_for(&h, h.owner_context()).await;
        let (member, member_csrf) = cookies_for(&h, h.member_context()).await;
        let (foreign, foreign_csrf) = cookies_for(&h, h.other_org_context()).await;
        let input = json!({"fileName":"SKILL.md","data":base64::engine::general_purpose::STANDARD.encode(b"# Shared brand voice\nUse simple language.")});
        let imported = server
            .post("/api/neev/library/import")
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&input)
            .await;
        imported.assert_status(StatusCode::CREATED);
        let body = imported.json::<Value>();
        let item = &body["items"][0];
        assert_eq!(item["revision"], "1");
        assert_eq!(item["enabled"], false);
        let id = item["id"].as_str().unwrap();
        let path = format!("/api/neev/library/{id}");
        let listed = server
            .get("/api/neev/library")
            .add_header(header::COOKIE, member.clone())
            .await;
        listed.assert_status_ok();
        assert_eq!(listed.json::<Value>()["items"][0]["id"], id);
        server
            .post("/api/neev/library/import")
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, member)
            .add_header("x-neev-csrf", member_csrf)
            .json(&input)
            .await
            .assert_status(StatusCode::FORBIDDEN);
        server
            .patch(&path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .json(&json!({"expectedRevision":"1","enabled":true}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
        let foreign_list = server
            .get("/api/neev/library")
            .add_header(header::COOKIE, foreign.clone())
            .await
            .json::<Value>();
        assert_eq!(foreign_list["items"], json!([]));
        server
            .delete(&path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, foreign)
            .add_header("x-neev-csrf", foreign_csrf)
            .json(&json!({"expectedRevision":"1"}))
            .await
            .assert_status(StatusCode::FORBIDDEN);
        let changed = server
            .patch(&path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"expectedRevision":"1","enabled":true}))
            .await;
        changed.assert_status_ok();
        assert_eq!(changed.json::<Value>()["revision"], "2");
        server
            .patch(&path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"expectedRevision":"1","enabled":false}))
            .await
            .assert_status(StatusCode::CONFLICT);
        server
            .delete(&path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner)
            .add_header("x-neev-csrf", csrf)
            .json(&json!({"expectedRevision":"2"}))
            .await
            .assert_status_ok();
    }

    const SECRET_VALUES: [&str; 8] = [
        "synthetic-hubspot-token-111",
        "Bearer synthetic-fireflies-222",
        "synthetic-github-pat-333",
        "synthetic-notion-token-444",
        "synthetic-sanity-token-555",
        "synthetic-sanity-project-666",
        "synthetic-sanity-dataset-777",
        "Bearer synthetic-aimfox-888",
    ];

    fn export_json() -> serde_json::Value {
        serde_json::json!({"mcpServers":{
            "hubspot":{"command":"npx","args":["-y","@hubspot/mcp-server"],"env":{"HUBSPOT_ACCESS_TOKEN":SECRET_VALUES[0]}},
            "fireflies":{"type":"http","url":"https://api.fireflies.ai/mcp","headers":{"Authorization":SECRET_VALUES[1]}},
            "github":{"command":"npx","args":["-y","@modelcontextprotocol/server-github"],"env":{"GITHUB_PERSONAL_ACCESS_TOKEN":SECRET_VALUES[2]}},
            "notion":{"command":"npx","args":["-y","@suekou/mcp-notion-server"],"env":{"NOTION_API_TOKEN":SECRET_VALUES[3]}},
            "sanity":{"command":"npx","args":["-y","@sanity/mcp-server@latest"],"env":{
                "SANITY_API_TOKEN":SECRET_VALUES[4],"SANITY_PROJECT_ID":SECRET_VALUES[5],
                "SANITY_DATASET":SECRET_VALUES[6],"MCP_USER_ROLE":"developer"}},
            "aimfox":{"type":"http","url":"https://mcp.aimfox.com/mcp","headers":{"Authorization":SECRET_VALUES[7]}},
            "unblocked":{"type":"http","url":"https://getunblocked.com/api/mcpsse"}
        }})
    }

    fn assert_no_secrets(text: &str) {
        for value in SECRET_VALUES {
            let bare = value.trim_start_matches("Bearer ");
            assert!(!text.contains(bare), "response leaked a secret value");
        }
        assert!(!text.contains("secrets.enc"));
    }

    #[tokio::test]
    async fn shared_integrations_http_contract_and_permissions() {
        use axum::http::header;
        use base64::Engine;
        use serde_json::{json, Value};
        let h = crate::neev::test_support::OrgHarness::new().await;
        let server = axum_test::TestServer::new(super::super::router(h.state.clone())).unwrap();
        let (owner, csrf) = cookies_for(&h, h.owner_context()).await;
        let (member, member_csrf) = cookies_for(&h, h.member_context()).await;
        let (foreign, foreign_csrf) = cookies_for(&h, h.other_org_context()).await;
        let encode = |value: &Value| {
            json!({"fileName":"export.json","data":base64::engine::general_purpose::STANDARD.encode(value.to_string())})
        };

        // An import where nothing can be stored answers 422 with the reasons.
        let rejected = server
            .post("/api/neev/library/import")
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&encode(&json!({"mcpServers":{"x":{"command":"npx","args":["-y","evil-pkg"],"env":{"A":"b"}}}})))
            .await;
        rejected.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        let body = rejected.json::<Value>();
        assert_eq!(body["items"], json!([]));
        assert_eq!(body["skipped"], json!([{"name":"x","reason":"unsupported_package"}]));

        // The seven-server export is stored whole, without echoing a secret.
        let imported = server
            .post("/api/neev/library/import")
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&encode(&export_json()))
            .await;
        imported.assert_status(StatusCode::CREATED);
        assert_no_secrets(&imported.text());
        let body = imported.json::<Value>();
        assert_eq!(body["items"].as_array().unwrap().len(), 7);
        assert_eq!(body["skipped"], json!([]));
        let items = body["items"].as_array().unwrap();
        let fireflies = items
            .iter()
            .find(|i| i["integration"]["source"].as_str().unwrap_or("").contains("fireflies"))
            .expect("fireflies item");
        let id = fireflies["id"].as_str().unwrap().to_owned();
        assert_eq!(fireflies["integration"]["secretNames"], json!(["Authorization"]));

        // Members read integrations (names, never values); other orgs see nothing.
        let listed = server
            .get("/api/neev/library")
            .add_header(header::COOKIE, member.clone())
            .await;
        listed.assert_status_ok();
        assert_no_secrets(&listed.text());
        let listed = listed.json::<Value>();
        assert_eq!(listed["items"].as_array().unwrap().len(), 7);
        assert!(listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["integration"]["secretNames"] == json!(["Authorization"])));
        let foreign_list = server
            .get("/api/neev/library")
            .add_header(header::COOKIE, foreign.clone())
            .await
            .json::<Value>();
        assert_eq!(foreign_list["items"], json!([]));

        let secrets_path = format!("/api/neev/library/{id}/secrets");
        let test_path = format!("/api/neev/library/{id}/test");
        let secrets_body = json!({"expectedRevision":"1","headers":{"Authorization":"Bearer synthetic-rotated-999"}});

        // Non-admins and other orgs are refused; missing CSRF is unauthenticated.
        for (cookie, token) in [(member.clone(), member_csrf.clone()), (foreign.clone(), foreign_csrf.clone())] {
            server
                .put(&secrets_path)
                .add_header(header::ORIGIN, "http://127.0.0.1:3100")
                .add_header(header::COOKIE, cookie.clone())
                .add_header("x-neev-csrf", token.clone())
                .json(&secrets_body)
                .await
                .assert_status(StatusCode::FORBIDDEN);
            server
                .post(&test_path)
                .add_header(header::ORIGIN, "http://127.0.0.1:3100")
                .add_header(header::COOKIE, cookie)
                .add_header("x-neev-csrf", token)
                .json(&json!({"expectedRevision":"1"}))
                .await
                .assert_status(StatusCode::FORBIDDEN);
        }
        server
            .put(&secrets_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .json(&secrets_body)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
        server
            .post(&test_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .json(&json!({"expectedRevision":"1"}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);

        // A stale revision conflicts, and the error never echoes the secret.
        let stale = server
            .put(&secrets_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"expectedRevision":"99","headers":{"Authorization":"Bearer synthetic-rotated-999"}}))
            .await;
        stale.assert_status(StatusCode::CONFLICT);
        assert!(!stale.text().contains("synthetic-rotated-999"));
        server
            .post(&test_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"expectedRevision":"99"}))
            .await
            .assert_status(StatusCode::CONFLICT);

        // Rotation succeeds, bumps the revision, and echoes nothing.
        let rotated = server
            .put(&secrets_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&secrets_body)
            .await;
        rotated.assert_status_ok();
        assert!(!rotated.text().contains("synthetic-rotated-999"));
        assert_no_secrets(&rotated.text());
        let revision = rotated.json::<Value>()["revision"].as_str().unwrap().to_owned();
        assert_ne!(revision, "1");

        // Unknown fields are refused without echoing values.
        let bad = server
            .put(&secrets_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"expectedRevision":revision,"token":"synthetic-leak-000"}))
            .await;
        assert!(bad.status_code().is_client_error());
        assert!(!bad.text().contains("synthetic-leak-000"));

        // The default harness has no live runtime: the test reports that.
        let tested = server
            .post(&test_path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner)
            .add_header("x-neev-csrf", csrf)
            .json(&json!({"expectedRevision":revision}))
            .await;
        tested.assert_status_ok();
        assert_no_secrets(&tested.text());
        assert_eq!(
            tested.json::<Value>()["integration"]["status"]["state"],
            "runtime_unavailable"
        );
    }

    #[tokio::test]
    async fn shared_integrations_test_reports_scripted_runtime_states() {
        use axum::http::header;
        use base64::Engine;
        use serde_json::{json, Value};
        use crate::neev::agents::execution::{AgentRuntime, FakeRuntime};
        let mut h = crate::neev::test_support::OrgHarness::new().await;
        std::sync::Arc::get_mut(&mut h.state)
            .expect("unique harness state")
            .set_agent_runtime(AgentRuntime::Fake(
                FakeRuntime::new(vec![]).with_check_state("fireflies", "auth_failed"),
            ));
        let server = axum_test::TestServer::new(super::super::router(h.state.clone())).unwrap();
        let (owner, csrf) = cookies_for(&h, h.owner_context()).await;
        let export = json!({"mcpServers":{
            "fireflies":{"type":"http","url":"https://api.fireflies.ai/mcp","headers":{"Authorization":SECRET_VALUES[1]}},
            "unblocked":{"type":"http","url":"https://getunblocked.com/api/mcpsse"}
        }});
        let imported = server
            .post("/api/neev/library/import")
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, owner.clone())
            .add_header("x-neev-csrf", csrf.clone())
            .json(&json!({"fileName":"export.json","data":base64::engine::general_purpose::STANDARD.encode(export.to_string())}))
            .await
            .json::<Value>();
        let mut seen = std::collections::BTreeMap::new();
        for item in imported["items"].as_array().unwrap() {
            let source = item["integration"]["source"].as_str().unwrap().to_owned();
            let tested = server
                .post(&format!("/api/neev/library/{}/test", item["id"].as_str().unwrap()))
                .add_header(header::ORIGIN, "http://127.0.0.1:3100")
                .add_header(header::COOKIE, owner.clone())
                .add_header("x-neev-csrf", csrf.clone())
                .json(&json!({"expectedRevision":item["revision"]}))
                .await;
            tested.assert_status_ok();
            let status = tested.json::<Value>()["integration"]["status"].clone();
            seen.insert(source, status);
        }
        let fireflies = seen.iter().find(|(k, _)| k.contains("fireflies")).unwrap().1;
        assert_eq!(fireflies["state"], "auth_failed");
        assert_eq!(fireflies["detail"], "HTTP 401");
        let other = seen.iter().find(|(k, _)| k.contains("getunblocked")).unwrap().1;
        assert_eq!(other["state"], "ready");
    }
}
