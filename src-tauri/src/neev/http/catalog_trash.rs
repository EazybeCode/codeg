//! Move to Trash / restore for projects and folders (`catalog::trash`).

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::neev::{
    catalog::trash::{self, Blocker, TrashError},
    startup::ServerState,
    types::NodeKind,
};

use super::{
    add_private_headers,
    catalog::{canonical_revision, organization_get, organization_mutation, permit},
    private_json, ApiError,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrashInput {
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreInput {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockedDto {
    agent_active: bool,
    in_use: bool,
    private_others: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewDto {
    folders: i64,
    documents: i64,
    chats: i64,
    agent_chats: i64,
    blocked: BlockedDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashedDto {
    batch_id: String,
    nodes: i64,
    resources: i64,
    sessions: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoredDto {
    nodes: i64,
    resources: i64,
    sessions: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashItemDto {
    node_id: i32,
    kind: &'static str,
    name: String,
    parent_id: Option<i32>,
    parent_name: Option<String>,
    parent_trashed: bool,
    trashed_by: i32,
    trashed_by_name: String,
    trashed_at: i64,
    folders: i64,
    documents: i64,
    chats: i64,
    agent_chats: i64,
}

#[derive(Serialize)]
struct TrashListDto {
    items: Vec<TrashItemDto>,
}

/// Domain refusals carry a stable code (and reason) the client can explain;
/// everything else is the shared taxonomy.
fn refusal(error: TrashError) -> Response {
    let body = match error {
        TrashError::Auth(error) => return ApiError::from(error).into_response(),
        TrashError::NotFound => return ApiError::NotFound.into_response(),
        TrashError::Blocked { blocker } => serde_json::json!({
            "code": "trash_blocked",
            "reason": match blocker {
                Blocker::AgentActive => "agent_active",
                Blocker::InUse => "in_use",
                Blocker::PrivateContent => "private_content",
            },
        }),
        TrashError::ParentTrashed => serde_json::json!({"code": "parent_trashed"}),
    };
    let mut response = (StatusCode::CONFLICT, Json(body)).into_response();
    add_private_headers(&mut response);
    response
}

pub(super) async fn preview(
    State(state): State<Arc<ServerState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(match trash::preview(&state, &actor, node_id).await {
        Ok(preview) => private_json(PreviewDto {
            folders: preview.folders,
            documents: preview.documents,
            chats: preview.chats,
            agent_chats: preview.agent_chats,
            blocked: BlockedDto {
                agent_active: preview.agent_active,
                in_use: preview.in_use,
                private_others: preview.private_others,
            },
        }),
        Err(error) => refusal(error),
    })
}

pub(super) async fn trash(
    State(state): State<Arc<ServerState>>,
    Path(node_id): Path<i32>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, actor) = organization_mutation::<TrashInput>(&state, request, 1024).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    Ok(
        match trash::trash(&state, &actor, node_id, revision).await {
            Ok(done) => private_json(TrashedDto {
                batch_id: done.batch_id,
                nodes: done.nodes,
                resources: done.resources,
                sessions: done.sessions,
            }),
            Err(error) => refusal(error),
        },
    )
}

pub(super) async fn restore(
    State(state): State<Arc<ServerState>>,
    Path(node_id): Path<i32>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (RestoreInput {}, actor) =
        organization_mutation::<RestoreInput>(&state, request, 1024).await?;
    Ok(match trash::restore(&state, &actor, node_id).await {
        Ok(done) => private_json(RestoredDto {
            nodes: done.nodes,
            resources: done.resources,
            sessions: done.sessions,
        }),
        Err(error) => refusal(error),
    })
}

pub(super) async fn list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(match trash::list(&state, &actor).await {
        Ok(items) => private_json(TrashListDto {
            items: items
                .into_iter()
                .map(|item| TrashItemDto {
                    node_id: item.node_id,
                    kind: match item.kind {
                        NodeKind::Project => "project",
                        NodeKind::Folder => "folder",
                        NodeKind::WorkingDirectory => "working_directory",
                    },
                    name: item.name,
                    parent_id: item.parent_id,
                    parent_name: item.parent_name,
                    parent_trashed: item.parent_trashed,
                    trashed_by: item.trashed_by,
                    trashed_by_name: item.trashed_by_name,
                    trashed_at: item.trashed_at,
                    folders: item.folders,
                    documents: item.documents,
                    chats: item.chats,
                    agent_chats: item.agent_chats,
                })
                .collect(),
        }),
        Err(error) => refusal(error),
    })
}

#[cfg(test)]
#[path = "catalog_trash_tests.rs"]
mod tests;
