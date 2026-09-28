//! HTTP handlers for organization library (skills and MCP configurations).
//!
//! GET /api/neev/library - List items (any org member)
//! POST /api/neev/library/import - Import skill ZIP/MD or MCP JSON (admin only)
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

/// Request to import a skill or MCP config.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportInput {
    file_name: String,
    data: String, // base64
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
    let items = library::import(&state, &actor, import_request).await?;

    let mut response = private_json(LibraryListDto { items });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
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
}
