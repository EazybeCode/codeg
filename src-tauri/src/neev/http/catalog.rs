use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};

use crate::neev::{
    auth::session,
    catalog::nodes,
    startup::ServerState,
    types::{Node, NodeInput, NodeKind, OrgContext},
};

use super::{auth, cookies, private_json, ApiError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageInput {
    after: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateInput {
    parent_id: Option<i32>,
    kind: NodeKindInput,
    name: String,
    #[serde(default, deserialize_with = "present_optional")]
    repository_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_optional")]
    repository_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_optional")]
    default_branch: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateInput {
    expected_revision: String,
    parent_id: Option<i32>,
    kind: NodeKindInput,
    name: String,
    #[serde(default, deserialize_with = "present_optional")]
    repository_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_optional")]
    repository_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_optional")]
    default_branch: Option<Option<String>>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NodeKindInput {
    Project,
    Folder,
    WorkingDirectory,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDto {
    id: i32,
    org_id: i32,
    parent_id: Option<i32>,
    kind: &'static str,
    name: String,
    revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodesDto {
    nodes: Vec<NodeDto>,
    next_after: Option<i32>,
}

pub(super) async fn list(
    State(state): State<Arc<ServerState>>,
    Query(page): Query<PageInput>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    let (nodes, next_after) = nodes::list_page(&state, &actor, page.after).await?;
    Ok(private_json(NodesDto {
        nodes: nodes
            .into_iter()
            .map(node_dto)
            .collect::<Result<Vec<_>, _>>()?,
        next_after,
    }))
}

pub(super) async fn create(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, actor) = organization_mutation::<CreateInput>(&state, request, 4096).await?;
    reject_repository_fields(
        &input.repository_id,
        &input.repository_url,
        &input.default_branch,
    )?;
    let created = nodes::create(
        &state,
        &actor,
        NodeInput {
            parent_id: input.parent_id,
            kind: input.kind.into(),
            name: input.name,
        },
    )
    .await?;
    let mut response = private_json(node_dto(created)?);
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn update(
    State(state): State<Arc<ServerState>>,
    Path(node_id): Path<i32>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, actor) = organization_mutation::<UpdateInput>(&state, request, 4096).await?;
    reject_repository_fields(
        &input.repository_id,
        &input.repository_url,
        &input.default_branch,
    )?;
    let revision = canonical_revision(&input.expected_revision)?;
    let updated = nodes::update(
        &state,
        &actor,
        node_id,
        revision,
        NodeInput {
            parent_id: input.parent_id,
            kind: input.kind.into(),
            name: input.name,
        },
    )
    .await?;
    Ok(private_json(node_dto(updated)?))
}

pub(super) async fn organization_get(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<OrgContext, ApiError> {
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(headers, names.session)?;
    Ok(session::organization(&state.database, token, state.clock.now_unix()).await?)
}

pub(super) async fn organization_mutation<T: for<'de> Deserialize<'de>>(
    state: &ServerState,
    request: Request<Body>,
    limit: usize,
) -> Result<(T, OrgContext), ApiError> {
    let (headers, input) =
        auth::json_request_limited::<T>(request, &state.config.origin_header, limit).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&headers, names.session)?;
    let cookie_csrf = cookies::required(&headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let actor = session::organization(&state.database, token, state.clock.now_unix()).await?;
    session::validate_csrf(
        &state.database,
        &actor.identity,
        cookie_csrf,
        state.clock.now_unix(),
    )
    .await?;
    Ok((input, actor))
}

fn node_dto(node: Node) -> Result<NodeDto, ApiError> {
    let kind = match node.kind {
        NodeKind::Project => "project",
        NodeKind::Folder => "folder",
        NodeKind::WorkingDirectory => return Err(ApiError::Unavailable),
    };
    Ok(NodeDto {
        id: node.id,
        org_id: node.org_id,
        parent_id: node.parent_id,
        kind,
        name: node.name,
        revision: node.revision.to_string(),
    })
}

pub(super) fn canonical_revision(value: &str) -> Result<i64, ApiError> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ApiError::Invalid);
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|revision| *revision > 0 && revision.to_string() == value)
        .ok_or(ApiError::Invalid)
}

fn reject_repository_fields<T>(
    repository_id: &Option<T>,
    repository_url: &Option<T>,
    default_branch: &Option<T>,
) -> Result<(), ApiError> {
    if repository_id.is_some() || repository_url.is_some() || default_branch.is_some() {
        Err(ApiError::Unavailable)
    } else {
        Ok(())
    }
}

fn present_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub(super) fn permit(state: &ServerState) -> Result<tokio::sync::SemaphorePermit<'_>, ApiError> {
    state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)
}

impl From<NodeKindInput> for NodeKind {
    fn from(value: NodeKindInput) -> Self {
        match value {
            NodeKindInput::Project => Self::Project,
            NodeKindInput::Folder => Self::Folder,
            NodeKindInput::WorkingDirectory => Self::WorkingDirectory,
        }
    }
}
