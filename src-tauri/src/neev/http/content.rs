use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        rejection::{PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::neev::{
    auth::session,
    content::{
        documents, image, messages, resources, rich,
        types::{
            ContentError, DocumentFormat, Message, Page, Resource, ResourceInput, ResourceKind,
            ResourcePatch, Revision, RichSaveInput, SaveInput, Visibility,
        },
    },
    startup::ServerState,
};

use super::{catalog, cookies, private_json, ApiError};

const SMALL_BODY_LIMIT: usize = 4096;
const MESSAGE_BODY_LIMIT: usize = 6 * 32768 + 4096;
const DOCUMENT_BODY_LIMIT: usize = 6 * 1048576 + 4096;
// Matches content::blob::MAX_IMAGE_BYTES; a larger body is rejected before storage.
const IMAGE_BODY_LIMIT: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResourcePageInput {
    after: Option<String>,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionPageInput {
    after: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionInput {
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateInput {
    title: String,
    request_id: String,
    #[serde(default, deserialize_with = "present_optional")]
    origin_chat_id: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchInput {
    expected_auth_revision: String,
    title: String,
    node_id: i32,
    visibility: VisibilityInput,
    archived: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VisibilityInput {
    Private,
    Organization,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageInput {
    request_id: String,
    expected_auth_revision: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase", deny_unknown_fields)]
enum SaveRevisionInput {
    Markdown {
        request_id: String,
        expected_content_revision: String,
        expected_auth_revision: String,
        markdown: String,
    },
    Blocks {
        request_id: String,
        expected_content_revision: String,
        expected_auth_revision: String,
        document: rich::RichDocument,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreInput {
    request_id: String,
    expected_content_revision: String,
    expected_auth_revision: String,
    source_revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceDto {
    id: String,
    org_id: i32,
    node_id: i32,
    kind: &'static str,
    creator_id: i32,
    title: String,
    visibility: &'static str,
    origin_chat_id: Option<String>,
    auth_revision: String,
    content_revision: String,
    archived: bool,
    created_at: i64,
    updated_at: i64,
    /// Which editor owns this document: `"lasuite"` for engine-backed documents,
    /// `"native"` for legacy ones. Additive — an older client that does not know
    /// the field treats its absence as native and keeps working, which is why
    /// this is a response field and not a stored column.
    engine: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MessageDto {
    id: String,
    sequence: String,
    author_id: i32,
    author_name: String,
    body: String,
    created_at: i64,
    /// `"human"` or `"agent"` (spec §5.3). An agent message was produced by a
    /// provider in answer to an explicit invocation; `authorId`/`authorName`
    /// still identify the human who invoked it, so clients can render
    /// "<provider> · via <name>" without the agent impersonating anyone.
    author_kind: &'static str,
    /// The provider that produced an agent message; `null` for a human one.
    agent_label: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionDto {
    format: &'static str,
    format_version: u8,
    revision: String,
    author_id: i32,
    author_name: String,
    byte_length: String,
    sha256: String,
    created_at: i64,
    parent_revision: Option<String>,
    restored_from: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageDto<T> {
    items: Vec<T>,
    next_after: Option<String>,
}

#[derive(Serialize)]
struct ContentDto {
    revision: RevisionDto,
    markdown: String,
    document: Option<rich::RichDocument>,
}

pub(super) async fn list_chats(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    query: Result<Query<ResourcePageInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let page = safe_query(query)?;
    list_resources(&state, node_id, ResourceKind::Chat, page, &headers).await
}

pub(super) async fn list_documents(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    query: Result<Query<ResourcePageInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let page = safe_query(query)?;
    list_resources(&state, node_id, ResourceKind::Document, page, &headers).await
}

async fn list_resources(
    state: &ServerState,
    node_id: i32,
    kind: ResourceKind,
    page: ResourcePageInput,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(state)?;
    positive_id(node_id)?;
    let actor = catalog::organization_get(state, headers).await?;
    let page = resources::list(
        state,
        &actor,
        node_id,
        kind,
        page.archived,
        page.after.as_deref(),
    )
    .await?;
    Ok(private_json(resource_page_dto(&state, page).await?))
}

pub(super) async fn create_chat(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    create_resource(&state, node_id, ResourceKind::Chat, request).await
}

pub(super) async fn create_document(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    create_resource(&state, node_id, ResourceKind::Document, request).await
}

async fn create_resource(
    state: &ServerState,
    node_id: i32,
    kind: ResourceKind,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(state)?;
    positive_id(node_id)?;
    let (input, actor) =
        catalog::organization_mutation::<CreateInput>(state, request, SMALL_BODY_LIMIT).await?;
    canonical_uuid(&input.request_id)?;
    if kind == ResourceKind::Chat && input.origin_chat_id.is_some() {
        return Err(ApiError::Invalid);
    }
    let origin_chat_id = input.origin_chat_id.flatten();
    if let Some(origin) = &origin_chat_id {
        canonical_uuid(origin)?;
    }
    // New documents go to the engine only when it is configured *and* creation
    // is enabled. A chat is never engine-backed, and a document attached to a
    // chat keeps the native path so its content stays with its conversation.
    let on_engine = kind == ResourceKind::Document
        && origin_chat_id.is_none()
        && state
            .config
            .engine
            .as_ref()
            .is_some_and(|engine| engine.create_enabled)
        && state.engine.is_some();
    let resource_input = ResourceInput {
        node_id,
        kind,
        title: input.title,
        origin_chat_id,
        request_id: input.request_id,
    };
    let resource = if on_engine {
        resources::create_engine_document(state, &actor, resource_input).await?
    } else {
        resources::create(state, &actor, resource_input).await?
    };
    if on_engine {
        // Outside the native transaction, which is closed by now: the migrator
        // forbids network calls inside one, and holding the admission guard and
        // lock set across an upstream round trip would let a slow Docs stall
        // unrelated native work. There is therefore no atomicity across the two
        // stores and none is claimed — provisioning uses this resource's own id,
        // so a failure here leaves a bound resource whose Docs document does not
        // exist yet and the retry converges rather than making a second one.
        if let Some(engine) = state.engine.as_ref() {
            engine
                .upstream
                .provision(
                    &resource.id,
                    &resource.title,
                    &crate::neev::engine::proxy::assertion(state, &actor).await,
                )
                .await
                .map_err(crate::neev::error::AuthError::from)?;
        }
    }
    let engine_backed = engine_backed(state, &resource).await?;
    let mut response = private_json(resource_dto(resource, engine_backed)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn get_resource(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let resource = resources::get(&state, &actor, &id).await?;
    let engine_backed = engine_backed(&state, &resource).await?;
    Ok(private_json(resource_dto(resource, engine_backed)?))
}

/// Label a resource with the engine that owns it.
///
/// A separate point read rather than a join, so the native content path keeps
/// its exact shape and a legacy document's read is unchanged.
async fn engine_backed(state: &ServerState, resource: &Resource) -> Result<bool, ApiError> {
    Ok(crate::neev::engine::binding::bound_now(&state.database, resource).await?)
}

pub(super) async fn update_resource(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<PatchInput>(&state, request, SMALL_BODY_LIMIT).await?;
    positive_id(input.node_id)?;
    let patch = ResourcePatch {
        expected_auth_revision: catalog::canonical_revision(&input.expected_auth_revision)?,
        title: input.title,
        node_id: input.node_id,
        visibility: input.visibility.into(),
        archived: input.archived,
    };
    let resource = resources::update(&state, &actor, &id, patch).await?;
    let engine_backed = engine_backed(&state, &resource).await?;
    Ok(private_json(resource_dto(resource, engine_backed)?))
}

pub(super) async fn list_messages(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RevisionPageInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let page = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let after = page
        .after
        .as_deref()
        .map(catalog::canonical_revision)
        .transpose()
        .map_err(|_| ApiError::NotFound)?;
    Ok(private_json(message_page_dto(
        messages::list(&state, &actor, &id, after).await?,
    )?))
}

pub(super) async fn append_message(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<MessageInput>(&state, request, MESSAGE_BODY_LIMIT).await?;
    canonical_uuid(&input.request_id)?;
    let message = messages::append(
        &state,
        &actor,
        &id,
        &input.request_id,
        catalog::canonical_revision(&input.expected_auth_revision)?,
        &input.body,
    )
    .await?;
    let mut response = private_json(message_dto(message)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn list_revisions(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RevisionPageInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let page = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let after = page
        .after
        .as_deref()
        .map(catalog::canonical_revision)
        .transpose()
        .map_err(|_| ApiError::NotFound)?;
    Ok(private_json(revision_page_dto(
        documents::history(&state, &actor, &id, after).await?,
    )?))
}

pub(super) async fn save_revision(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<SaveRevisionInput>(&state, request, DOCUMENT_BODY_LIMIT)
            .await?;
    let revision = match input {
        SaveRevisionInput::Markdown {
            request_id,
            expected_content_revision,
            expected_auth_revision,
            markdown,
        } => {
            canonical_uuid(&request_id)?;
            documents::save(
                &state,
                &actor,
                &id,
                SaveInput {
                    request_id,
                    expected_content_revision: canonical_nonnegative_revision(
                        &expected_content_revision,
                    )?,
                    expected_auth_revision: catalog::canonical_revision(&expected_auth_revision)?,
                    markdown,
                },
            )
            .await?
        }
        SaveRevisionInput::Blocks {
            request_id,
            expected_content_revision,
            expected_auth_revision,
            document,
        } => {
            canonical_uuid(&request_id)?;
            documents::save_rich(
                &state,
                &actor,
                &id,
                RichSaveInput {
                    request_id,
                    expected_content_revision: canonical_nonnegative_revision(
                        &expected_content_revision,
                    )?,
                    expected_auth_revision: catalog::canonical_revision(&expected_auth_revision)?,
                    document,
                },
            )
            .await?
        }
    };
    let mut response = private_json(revision_dto(revision)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn get_document_content(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RevisionInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let input = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let number = input
        .revision
        .as_deref()
        .map(catalog::canonical_revision)
        .transpose()?;
    let (revision, bytes) = documents::read(&state, &actor, &id, number)
        .await
        .map_err(document_read_error)?;
    let (markdown, document) = document_body(&revision, bytes)?;
    Ok(private_json(ContentDto {
        revision: revision_dto(revision)?,
        markdown,
        document,
    }))
}

pub(super) async fn download_document(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RevisionInput>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let input = safe_query(query)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let number = input
        .revision
        .as_deref()
        .map(catalog::canonical_revision)
        .transpose()?;
    let (revision, bytes) = documents::read(&state, &actor, &id, number)
        .await
        .map_err(document_read_error)?;
    let (markdown, _) = document_body(&revision, bytes)?;
    let mut response = Body::from(markdown).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown;charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"document.md\""),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    super::add_private_headers(&mut response);
    Ok(response)
}

pub(super) async fn restore_revision(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        catalog::organization_mutation::<RestoreInput>(&state, request, SMALL_BODY_LIMIT).await?;
    canonical_uuid(&input.request_id)?;
    let revision = documents::restore(
        &state,
        &actor,
        &id,
        catalog::canonical_revision(&input.source_revision)?,
        &input.request_id,
        canonical_nonnegative_revision(&input.expected_content_revision)?,
        catalog::canonical_revision(&input.expected_auth_revision)?,
    )
    .await?;
    let mut response = private_json(revision_dto(revision)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageUploadedDto {
    key: String,
    url: String,
    content_type: &'static str,
}

/// Upload an image bound to a document. Raw image bytes in the body (the declared
/// Content-Type is ignored; the server sniffs the bytes). CSRF + origin guarded.
pub(super) async fn upload_document_image(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    canonical_uuid(&id)?;
    let (parts, body) = request.into_parts();
    if !cookies::exact_origin(&parts.headers, &state.config.origin_header) {
        return Err(ApiError::Forbidden);
    }
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&parts.headers, names.session)?;
    let cookie_csrf = cookies::required(&parts.headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&parts.headers)? {
        return Err(ApiError::Unauthorized);
    }
    let now = state.clock.now_unix();
    let actor = session::organization(&state.database, token, now).await?;
    session::validate_csrf(&state.database, &actor.identity, cookie_csrf, now).await?;
    let bytes = axum::body::to_bytes(body, IMAGE_BODY_LIMIT)
        .await
        .map_err(|_| ApiError::Invalid)?;
    let stored = image::store(&state, &actor, &id, bytes.as_ref()).await?;
    let mut response = private_json(ImageUploadedDto {
        url: format!("/api/neev/images/{}", stored.key),
        key: stored.key,
        content_type: stored.content_type,
    });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// Serve an image, enforcing the bound document's CURRENT read ACL. Not cached
/// across the client so a revoked reader stops seeing it immediately.
pub(super) async fn serve_image(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let key = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let (bytes, content_type) = image::load(&state, &actor, &key).await?;
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    super::add_private_headers(&mut response);
    Ok(response)
}

fn document_body(
    revision: &Revision,
    bytes: Vec<u8>,
) -> Result<(String, Option<rich::RichDocument>), ApiError> {
    if revision.format_version != 1 {
        return Err(ApiError::StorageUnavailable);
    }
    match revision.format {
        DocumentFormat::Markdown => Ok((
            String::from_utf8(bytes).map_err(|_| ApiError::StorageUnavailable)?,
            None,
        )),
        DocumentFormat::Blocks => {
            let document = rich::decode(&bytes).map_err(document_read_error)?;
            let markdown = rich::export_markdown(&document).map_err(document_read_error)?;
            Ok((markdown, Some(document)))
        }
    }
}

async fn resource_page_dto(
    state: &ServerState,
    page: Page<Resource>,
) -> Result<PageDto<ResourceDto>, ApiError> {
    // One query for the whole page rather than a round trip per row.
    let bound = crate::neev::engine::binding::bound_set(&state.database, &page.items).await?;
    Ok(PageDto {
        items: page
            .items
            .into_iter()
            .map(|resource| {
                let engine_backed = bound.contains(&resource.id);
                resource_dto(resource, engine_backed)
            })
            .collect::<Result<Vec<_>, _>>()?,
        next_after: page.next_after,
    })
}

fn message_page_dto(page: Page<Message>) -> Result<PageDto<MessageDto>, ApiError> {
    Ok(PageDto {
        items: page
            .items
            .into_iter()
            .map(message_dto)
            .collect::<Result<Vec<_>, _>>()?,
        next_after: page.next_after,
    })
}

fn revision_page_dto(page: Page<Revision>) -> Result<PageDto<RevisionDto>, ApiError> {
    Ok(PageDto {
        items: page
            .items
            .into_iter()
            .map(revision_dto)
            .collect::<Result<Vec<_>, _>>()?,
        next_after: page.next_after,
    })
}

fn resource_dto(resource: Resource, engine_backed: bool) -> Result<ResourceDto, ApiError> {
    canonical_uuid(&resource.id).map_err(|_| ApiError::StorageUnavailable)?;
    if resource.org_id < 1
        || resource.node_id < 1
        || resource.creator_id < 1
        || resource.auth_revision < 1
        || resource.content_revision < 0
        || resource.created_at < 0
        || resource.updated_at < 0
    {
        return Err(ApiError::StorageUnavailable);
    }
    if let Some(origin) = &resource.origin_chat_id {
        canonical_uuid(origin).map_err(|_| ApiError::StorageUnavailable)?;
    }
    Ok(ResourceDto {
        id: resource.id,
        org_id: resource.org_id,
        node_id: resource.node_id,
        kind: match resource.kind {
            ResourceKind::Chat => "chat",
            ResourceKind::Document => "document",
        },
        creator_id: resource.creator_id,
        title: resource.title,
        visibility: match resource.visibility {
            Visibility::Private => "private",
            Visibility::Organization => "organization",
        },
        origin_chat_id: resource.origin_chat_id,
        auth_revision: resource.auth_revision.to_string(),
        content_revision: resource.content_revision.to_string(),
        archived: resource.archived,
        created_at: resource.created_at,
        updated_at: resource.updated_at,
        engine: crate::neev::engine::binding::name(engine_backed),
    })
}

pub(super) fn message_dto(message: Message) -> Result<MessageDto, ApiError> {
    canonical_uuid(&message.id).map_err(|_| ApiError::StorageUnavailable)?;
    if message.sequence < 1 || message.author_id < 1 || message.created_at < 0 {
        return Err(ApiError::StorageUnavailable);
    }
    Ok(MessageDto {
        id: message.id,
        sequence: message.sequence.to_string(),
        author_id: message.author_id,
        author_name: message.author_name,
        body: message.body,
        created_at: message.created_at,
        author_kind: message.author_kind.name(),
        agent_label: message.agent_label,
    })
}

fn revision_dto(revision: Revision) -> Result<RevisionDto, ApiError> {
    if revision.revision < 1
        || revision.author_id < 1
        || revision.byte_length < 0
        || revision.byte_length > 1048576
        || revision.format_version != 1
        || revision.created_at < 0
        || !valid_sha256(&revision.sha256)
        || revision.parent_revision.is_some_and(|value| value < 1)
        || revision.restored_from.is_some_and(|value| value < 1)
    {
        return Err(ApiError::StorageUnavailable);
    }
    Ok(RevisionDto {
        format: match revision.format {
            DocumentFormat::Markdown => "markdown",
            DocumentFormat::Blocks => "blocks",
        },
        format_version: 1,
        revision: revision.revision.to_string(),
        author_id: revision.author_id,
        author_name: revision.author_name,
        byte_length: revision.byte_length.to_string(),
        sha256: revision.sha256,
        created_at: revision.created_at,
        parent_revision: revision.parent_revision.map(|value| value.to_string()),
        restored_from: revision.restored_from.map(|value| value.to_string()),
    })
}

pub(super) fn canonical_uuid(value: &str) -> Result<(), ApiError> {
    if uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.to_string() == value) {
        Ok(())
    } else {
        Err(ApiError::Invalid)
    }
}

fn canonical_nonnegative_revision(value: &str) -> Result<i64, ApiError> {
    if value == "0" {
        return Ok(0);
    }
    catalog::canonical_revision(value)
}

fn positive_id(value: i32) -> Result<(), ApiError> {
    if value > 0 {
        Ok(())
    } else {
        Err(ApiError::Invalid)
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn document_read_error(error: ContentError) -> ApiError {
    match error {
        ContentError::Invalid | ContentError::Storage => ApiError::StorageUnavailable,
        error => error.into(),
    }
}

pub(super) fn safe_path<T>(value: Result<Path<T>, PathRejection>) -> Result<T, ApiError> {
    value
        .map(|Path(value)| value)
        .map_err(|_| ApiError::Invalid)
}

pub(super) fn safe_query<T>(value: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    value
        .map(|Query(value)| value)
        .map_err(|_| ApiError::Invalid)
}

fn present_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl From<VisibilityInput> for Visibility {
    fn from(value: VisibilityInput) -> Self {
        match value {
            VisibilityInput::Private => Self::Private,
            VisibilityInput::Organization => Self::Organization,
        }
    }
}

impl From<ContentError> for ApiError {
    fn from(value: ContentError) -> Self {
        match value {
            ContentError::Auth(error) => match error {
                crate::neev::error::AuthError::Unauthorized => Self::Unauthorized,
                crate::neev::error::AuthError::Forbidden => Self::Forbidden,
                crate::neev::error::AuthError::Conflict => Self::Conflict,
                crate::neev::error::AuthError::Invalid => Self::Invalid,
                crate::neev::error::AuthError::Limited => Self::Limited,
                crate::neev::error::AuthError::Unavailable => Self::StorageUnavailable,
            },
            ContentError::NotFound => Self::NotFound,
            ContentError::Invalid => Self::Invalid,
            ContentError::Conflict => Self::Conflict,
            ContentError::Quota => Self::ContentQuota,
            ContentError::Archived => Self::Archived,
            ContentError::Storage => Self::StorageUnavailable,
        }
    }
}
