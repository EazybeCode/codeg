use std::sync::Arc;

use axum::{
    body::to_bytes,
    extract::{Path, RawQuery, Request, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

use super::{json, ApiError};
use crate::{
    team_access::{principal::AuthContext, principal::ProofRequirement},
    team_server::{
        catalog::{
            self, ChangeVisibilityInput, ConnectRepositoryInput, CreateChatInput,
            CreateNodeInput, GithubTeamRepositoryAccess, UpdateNodeInput,
        },
        cookies,
        state::TeamState,
    },
};

const MAX_BODY_BYTES: usize = 32 * 1024;

pub(in crate::team_server) async fn list_nodes(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match read_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    match catalog::list_nodes(&state, &context).await {
        Ok(items) => json(serde_json::json!({"items":items})),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn create_node(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match mutation_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let input = match parse_body::<CreateNodeInput>(request).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let access = match repository_access(&state) {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    match catalog::create_node(&state, &context, &access, input).await {
        Ok(node) => json(node),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn update_node(
    Path(id): Path<String>,
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match mutation_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let input = match parse_body::<UpdateNodeInput>(request).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let access = match repository_access(&state) {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    match catalog::update_node(&state, &context, &access, id, input).await {
        Ok(node) => json(node),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn list_repositories(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match read_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    match catalog::list_repositories(&state, &context).await {
        Ok(items) => json(serde_json::json!({"items":items})),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn connect_repository(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match mutation_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let input = match parse_body::<ConnectRepositoryInput>(request).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let access = match repository_access(&state) {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    match catalog::connect_repository(&state, &context, &access, input).await {
        Ok(repository) => json(repository),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn list_chats(
    State(state): State<Arc<TeamState>>,
    RawQuery(query): RawQuery,
    request: Request,
) -> Response {
    let context = match read_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let (node_id, after_id) = match parse_chat_query(query.as_deref()) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    match catalog::list_chats(&state, &context, node_id, after_id).await {
        Ok(items) => json(serde_json::json!({"items":items})),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn create_chat(
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match mutation_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let input = match parse_body::<CreateChatInput>(request).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let access = match repository_access(&state) {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    match catalog::create_chat(&state, &context, &access, input).await {
        Ok(chat) => json(chat),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn read_chat(
    Path(id): Path<String>,
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match read_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match catalog::read_chat(&state, &context, id).await {
        Ok(chat) => json(chat),
        Err(error) => error.into_response(),
    }
}

pub(in crate::team_server) async fn change_visibility(
    Path(id): Path<String>,
    State(state): State<Arc<TeamState>>,
    request: Request,
) -> Response {
    let context = match mutation_context(&state, request.headers()).await {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let input = match parse_body::<ChangeVisibilityInput>(request).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    match catalog::change_visibility(&state, &context, id, input).await {
        Ok(chat) => json(chat),
        Err(error) => error.into_response(),
    }
}

async fn read_context(state: &TeamState, headers: &HeaderMap) -> Result<AuthContext, ApiError> {
    state
        .principals
        .resolve(headers, ProofRequirement::Cached)
        .await
        .map_err(ApiError::from)
}

async fn mutation_context(
    state: &TeamState,
    headers: &HeaderMap,
) -> Result<AuthContext, ApiError> {
    let expected_origin = state.config.origin.origin().ascii_serialization();
    if !cookies::exact_origin(headers, &expected_origin) {
        return Err(ApiError::Forbidden);
    }
    if !cookies::is_json(headers) {
        return Err(ApiError::InvalidRequest);
    }
    let csrf_cookie = cookies::required(headers, cookies::CSRF)
        .map_err(|_| ApiError::Unauthorized)?;
    let csrf_header =
        cookies::csrf_header(headers).map_err(|_| ApiError::Unauthorized)?;
    if csrf_cookie != csrf_header {
        return Err(ApiError::Unauthorized);
    }
    let context = state
        .principals
        .resolve(headers, ProofRequirement::Fresh)
        .await
        .map_err(ApiError::from)?;
    if !cookies::csrf_matches(csrf_header, &context.csrf_hash) {
        return Err(ApiError::Unauthorized);
    }
    Ok(context)
}

async fn parse_body<T: DeserializeOwned>(request: Request) -> Result<T, ApiError> {
    let (_, body) = request.into_parts();
    let body = to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| ApiError::InvalidRequest)?;
    serde_json::from_slice(&body).map_err(|_| ApiError::InvalidRequest)
}

fn parse_id(value: &str) -> Result<i32, ApiError> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ApiError::InvalidRequest);
    }
    value
        .parse::<i32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or(ApiError::InvalidRequest)
}

fn parse_chat_query(query: Option<&str>) -> Result<(Option<i32>, Option<i32>), ApiError> {
    let Some(query) = query else {
        return Ok((None, None));
    };
    if query.is_empty() {
        return Ok((None, None));
    }
    let mut node_id = None;
    let mut after_id = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').ok_or(ApiError::InvalidRequest)?;
        if key.is_empty()
            || value.is_empty()
            || key.contains(['%', '+'])
            || value.contains(['%', '+', '='])
        {
            return Err(ApiError::InvalidRequest);
        }
        let parsed = parse_id(value)?;
        match key {
            "nodeId" if node_id.is_none() => node_id = Some(parsed),
            "afterId" if after_id.is_none() => after_id = Some(parsed),
            _ => return Err(ApiError::InvalidRequest),
        }
    }
    Ok((node_id, after_id))
}

fn repository_access(state: &TeamState) -> Result<GithubTeamRepositoryAccess, ApiError> {
    #[cfg(test)]
    if let Some(origin) = state.catalog_repository_origin.as_deref() {
        return catalog::github_repository_access_for_test(state, origin);
    }
    GithubTeamRepositoryAccess::new(state)
}
