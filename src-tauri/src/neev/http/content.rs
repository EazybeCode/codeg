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
        attachment,
        documents, image, mentions, messages, resources, rich,
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
// An attachment has NO per-file size limit. This bounds one request body at the
// org's whole attachment storage budget — the largest body that could ever be
// stored — so nothing unbounded is read into memory. Exceeding it is a quota
// answer, not a bad request: see `upload_attachment`.
const ATTACHMENT_BODY_LIMIT: usize = crate::neev::content::ATTACHMENT_STORAGE_CEILING;

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
    /// Keys of files already uploaded to THIS chat, in send order. Defaulted so
    /// a client that predates attachments posts exactly the body it always did.
    #[serde(default)]
    attachment_keys: Vec<String>,
    /// The coworkers this message names, in the order the body says them.
    /// Defaulted for the same reason the keys are.
    #[serde(default)]
    mentions: Vec<MentionInputDto>,
}

/// One person a send names: who, and which characters of the body say so.
///
/// Offsets are Unicode code points, so neither side has to agree about bytes or
/// UTF-16. The span is proved against the stored body before anything is
/// written, so a client cannot claim a run of somebody else's words.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct MentionInputDto {
    user_id: i32,
    start: i64,
    length: i64,
}

pub(super) fn requested_mentions(
    input: &[MentionInputDto],
) -> Result<Vec<mentions::MentionInput>, ApiError> {
    if input.len() > mentions::MAX_MENTIONS_PER_MESSAGE {
        return Err(ApiError::Invalid);
    }
    input
        .iter()
        .map(|mention| {
            if mention.user_id <= 0 || mention.start < 0 || mention.length < 2 {
                return Err(ApiError::Invalid);
            }
            Ok(mentions::MentionInput {
                user_id: mention.user_id,
                start: mention.start,
                length: mention.length,
            })
        })
        .collect()
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
    /// The files this message was sent with, in the order the sender arranged
    /// them. Always present, `[]` when none — a client seeding a thread must
    /// not have to tell "no files" apart from "field missing".
    attachments: Vec<AttachmentDto>,
    /// Omit empty mentions so already-open clients can still read ordinary
    /// messages. Agent-authored messages never carry mentions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mentions: Vec<MentionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MentionDto {
    user_id: i32,
    start: i64,
    length: i64,
}

fn mention_dtos(stored: Vec<mentions::StoredMention>) -> Vec<MentionDto> {
    stored
        .into_iter()
        .map(|mention| MentionDto {
            user_id: mention.user_id,
            start: mention.start,
            length: mention.length,
        })
        .collect()
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
    // Every native rename of an engine-backed document must reach the engine's
    // copy, or the two titles diverge the moment anyone renames from the
    // sidebar — which is what happened: Docs to Neev propagated, Neev to Docs
    // never did.
    //
    // Done here, in the HTTP layer, rather than inside resources::update: the
    // proxy's own title sync calls that function directly, so a hook inside it
    // would recurse. And it runs after the transaction has committed, because
    // the migrator forbids network calls inside one.
    //
    // A failed push does not fail the request. The native row is committed and
    // is the source of truth; failing here would report a rename that did
    // happen as an error. The divergence is instead repaired the next time the
    // document is opened, which pushes the native title again.
    if engine_backed {
        if let Some(engine) = state.engine.as_ref() {
            let assertion = crate::neev::engine::proxy::assertion(&state, &actor).await;
            let _ = engine
                .upstream
                .push_title(&resource.id, &assertion, &resource.title)
                .await;
        }
    }
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
    let page = messages::list(&state, &actor, &id, after).await?;
    Ok(private_json(
        message_page_dto(&state, &actor, page).await?,
    ))
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
    let keys = requested_keys(&input.attachment_keys)?;
    let named = requested_mentions(&input.mentions)?;
    let body = sent_text(input.body, keys.len());
    let message = messages::append_with_attachments(
        &state,
        &actor,
        &id,
        &input.request_id,
        catalog::canonical_revision(&input.expected_auth_revision)?,
        &body,
        &keys,
        &named,
    )
    .await?;
    // Read back from what the append committed, so the response describes what
    // is stored rather than what was asked for.
    let attachments = message_attachments(&state, &actor, &message.id).await?;
    let spans = mentions::for_messages(&state, &actor, &[message.id.clone()])
        .await?
        .remove(&message.id)
        .unwrap_or_default();
    let mut response =
        private_json(message_dto(message, attachment_dtos(attachments), spans)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// The reader's own mentions: the whole of the in-app notification.
///
/// There is no delivery queue behind this and nothing leaves the server — no
/// mail, no push, no webhook. A notice is a row that still passes the
/// conversation's CURRENT ACL when it is asked for, which is why the authority
/// is proved here on every read rather than remembered from when it was written.
pub(super) async fn list_mentions(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let (notices, unread) = mentions::inbox(&state, &actor).await?;
    Ok(private_json(MentionInboxDto {
        unread,
        items: notices
            .into_iter()
            .map(|notice| NoticeDto {
                message_id: notice.message_id,
                chat_id: notice.chat_id,
                chat_title: notice.chat_title,
                author_name: notice.author_name,
                excerpt: notice.excerpt,
                created_at: notice.created_at,
                read: notice.read,
            })
            .collect(),
    }))
}

/// Mark notices read. Idempotent, and only ever the caller's own rows.
pub(super) async fn read_mentions(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<MentionReadInput>(&state, request, SMALL_BODY_LIMIT)
            .await?;
    for id in &input.message_ids {
        canonical_uuid(id)?;
    }
    mentions::mark_read(&state, &actor, &input.message_ids).await?;
    let (notices, unread) = mentions::inbox(&state, &actor).await?;
    Ok(private_json(MentionInboxDto {
        unread,
        items: notices
            .into_iter()
            .map(|notice| NoticeDto {
                message_id: notice.message_id,
                chat_id: notice.chat_id,
                chat_title: notice.chat_title,
                author_name: notice.author_name,
                excerpt: notice.excerpt,
                created_at: notice.created_at,
                read: notice.read,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MentionReadInput {
    message_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MentionInboxDto {
    items: Vec<NoticeDto>,
    /// How many of the listed conversations still have something unread in
    /// them, for the one discreet indicator this feature gets.
    unread: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NoticeDto {
    message_id: String,
    chat_id: String,
    chat_title: String,
    author_name: String,
    excerpt: String,
    created_at: i64,
    read: bool,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttachmentDto {
    key: String,
    file_name: String,
    content_type: &'static str,
    byte_length: i64,
}

pub(super) fn attachment_dto(stored: attachment::StoredAttachment) -> AttachmentDto {
    AttachmentDto {
        key: stored.key,
        file_name: stored.file_name,
        content_type: stored.content_type,
        byte_length: stored.byte_length,
    }
}

pub(super) fn attachment_dtos(stored: Vec<attachment::StoredAttachment>) -> Vec<AttachmentDto> {
    stored.into_iter().map(attachment_dto).collect()
}

/// The attachment keys a send may carry, checked before they reach storage.
///
/// Shape only — that a key exists, belongs to this conversation and fits the
/// per-turn budget is proved against storage by `content::attachment`, which is
/// the only place that can prove it.
pub(super) fn requested_keys(keys: &[String]) -> Result<Vec<String>, ApiError> {
    if keys.len() > attachment::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ApiError::Invalid);
    }
    let mut seen = std::collections::BTreeSet::new();
    for key in keys {
        canonical_uuid(key)?;
        if !seen.insert(key.as_str()) {
            return Err(ApiError::Invalid);
        }
    }
    Ok(keys.to_vec())
}

/// What a send that carried files but no words is recorded as.
///
/// The durable row cannot be empty — `byte_length BETWEEN 1 AND 32768` on a
/// message, `length(prompt) BETWEEN 1 AND 32768` on a turn — so the sentence is
/// persisted rather than left for the runtime to substitute at launch. It is
/// the runtime's own constant, so the text a reader sees in the thread and the
/// text the provider is asked are the same sentence, written once.
pub(super) fn sent_text(body: String, attachments: usize) -> String {
    if body.trim().is_empty() && attachments > 0 {
        return crate::neev::agents::execution::ATTACHMENT_ONLY_PROMPT.to_owned();
    }
    body
}

/// The attachment's name, from `X-Neev-Filename`.
///
/// Percent-decoded ONCE and then required to be strict UTF-8. Decoding twice
/// would let `%252e%252e` arrive as `..`, and the decoded value goes on to name
/// a file inside the sandbox — so anything that is not a plain single path
/// segment is refused by `content::attachment`, not repaired here.
fn attachment_name(headers: &HeaderMap) -> Result<String, ApiError> {
    let raw = headers
        .get("x-neev-filename")
        .ok_or(ApiError::Invalid)?
        .to_str()
        .map_err(|_| ApiError::Invalid)?;
    if raw.len() > 1024 {
        return Err(ApiError::Invalid);
    }
    // Decoded here rather than pulling in a dependency for fifteen lines. ONCE:
    // a second pass would let `%252e%252e` arrive as `..`.
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ApiError::Invalid);
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| ApiError::Invalid)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| ApiError::Invalid)?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ApiError::Invalid)
}

/// Upload an attachment bound to a conversation. Raw bytes in the body; the
/// declared content type is advisory and the stored type is sniffed.
pub(super) async fn upload_chat_attachment(
    state: State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    upload_attachment(state, path, request, false).await
}

/// The same upload, for an Agent Chat. A session is private to its owner, so
/// the authorization behind it is ownership rather than a shared audience.
pub(super) async fn upload_session_attachment(
    state: State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    upload_attachment(state, path, request, true).await
}

async fn upload_attachment(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
    session: bool,
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
    let file_name = attachment_name(&parts.headers)?;
    let now = state.clock.now_unix();
    let actor = session::organization(&state.database, token, now).await?;
    session::validate_csrf(&state.database, &actor.identity, cookie_csrf, now).await?;
    // Over the frame bound is the tenant being out of storage, not a malformed or
    // unsupported upload: the bound IS the org budget. Answering `Invalid` here
    // made the client say "unsupported type" about a perfectly good file.
    let bytes = axum::body::to_bytes(body, ATTACHMENT_BODY_LIMIT)
        .await
        .map_err(|_| ApiError::ContentQuota)?;
    let owner = if session {
        attachment::Owner::Session(&id)
    } else {
        attachment::Owner::Chat(&id)
    };
    let stored = attachment::store(&state, &actor, owner, &file_name, bytes.as_ref()).await?;
    let mut response = private_json(AttachmentDto {
        key: stored.key,
        file_name: stored.file_name,
        content_type: stored.content_type,
        byte_length: stored.byte_length,
    });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// Serve an attachment, enforcing the owning conversation's CURRENT read ACL.
///
/// Always a download, never an inline document: a PDF or a text file rendered
/// in the page's own origin would be a same-origin document the uploader chose
/// the bytes of. Images are safe to point an `<img>` at, which is why the type
/// is still accurate — but nothing here invites the browser to execute it.
pub(super) async fn serve_attachment(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let key = safe_path(path)?;
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let (bytes, content_type, _name) = attachment::load(&state, &actor, &key).await?;
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // The stored name is deliberately NOT echoed into the header: it would have
    // to be escaped for a header, and the client already knows what it called
    // the file. A bare `attachment` is enough to stop inline rendering.
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment"),
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

/// A page of messages with the files each was sent with.
///
/// One lookup for the whole page: attachments are a second table, and reading
/// them per message would make a long thread a round trip per row.
async fn message_page_dto(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    page: Page<Message>,
) -> Result<PageDto<MessageDto>, ApiError> {
    let ids: Vec<String> = page.items.iter().map(|item| item.id.clone()).collect();
    let mut carried = attachment::for_messages(state, actor, &ids).await?;
    // One lookup for the whole page, as the files get: the spans live in a
    // second table and a message without any simply has none.
    let mut named = mentions::for_messages(state, actor, &ids).await?;
    let mut items = Vec::with_capacity(page.items.len());
    for message in page.items {
        let attachments = attachment_dtos(carried.remove(&message.id).unwrap_or_default());
        let spans = named.remove(&message.id).unwrap_or_default();
        items.push(message_dto(message, attachments, spans)?);
    }
    Ok(PageDto {
        items,
        next_after: page.next_after,
    })
}

/// The files one persisted message carries, for a response that names it.
pub(super) async fn message_attachments(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    message_id: &str,
) -> Result<Vec<attachment::StoredAttachment>, ApiError> {
    let ids = vec![message_id.to_owned()];
    let mut carried = attachment::for_messages(state, actor, &ids).await?;
    Ok(carried.remove(message_id).unwrap_or_default())
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

pub(super) fn message_dto(
    message: Message,
    attachments: Vec<AttachmentDto>,
    named: Vec<mentions::StoredMention>,
) -> Result<MessageDto, ApiError> {
    canonical_uuid(&message.id).map_err(|_| ApiError::StorageUnavailable)?;
    if message.sequence < 1 || message.author_id < 1 || message.created_at < 0 {
        return Err(ApiError::StorageUnavailable);
    }
    Ok(MessageDto {
        attachments,
        mentions: mention_dtos(named),
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
