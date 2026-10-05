//! Member directory, teams, and sharing with members and teams
//! (`.context/sharing-api-contract.md`, v1). Thin: every rule lives in
//! `content::grants` and `org::teams`.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{rejection::PathRejection, rejection::QueryRejection, Path, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::neev::{
    agents::sessions,
    content::{
        access,
        grants::{
            self, Grant, InheritedGrant, Level, NodeAccess, Principal, PrincipalView,
            ResourceAccess, SessionAccess, SharingError,
        },
    },
    org::teams,
    startup::ServerState,
    types::Role,
};

use super::{
    add_private_headers,
    agent_sessions::{session_dto, SessionDto},
    catalog::{canonical_revision, organization_get, organization_mutation, permit},
    content::{canonical_uuid, safe_path, safe_query},
    private_json, ApiError,
};

const BODY_LIMIT: usize = 1024;

/// Refusals with a stable code the client names (`neev-client`); everything
/// else is the shared taxonomy.
fn refusal(error: SharingError) -> Response {
    let code = match error {
        SharingError::Content(error) => return ApiError::from(error).into_response(),
        SharingError::NotShared => "resource_not_shared",
        SharingError::NameTaken => "team_name_taken",
    };
    let mut response =
        (StatusCode::CONFLICT, Json(serde_json::json!({ "code": code }))).into_response();
    add_private_headers(&mut response);
    response
}

// ------------------------------------------------------------------ inputs

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum PrincipalInput {
    #[serde(rename_all = "camelCase")]
    User { user_id: i32 },
    #[serde(rename_all = "camelCase")]
    Team { team_id: String },
}

impl PrincipalInput {
    fn checked(self) -> Result<Principal, ApiError> {
        match self {
            Self::User { user_id } if user_id > 0 => Ok(Principal::User(user_id)),
            Self::User { .. } => Err(ApiError::Invalid),
            Self::Team { team_id } => {
                canonical_uuid(&team_id)?;
                Ok(Principal::Team(team_id))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum LevelInput {
    View,
    Edit,
}

impl From<LevelInput> for Level {
    fn from(value: LevelInput) -> Self {
        match value {
            LevelInput::View => Level::View,
            LevelInput::Edit => Level::Edit,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceGrantInput {
    expected_auth_revision: String,
    principal: PrincipalInput,
    level: LevelInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceRevokeInput {
    expected_auth_revision: String,
    principal: PrincipalInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantInput {
    expected_revision: String,
    principal: PrincipalInput,
    level: LevelInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevokeInput {
    expected_revision: String,
    principal: PrincipalInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DirectoryQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamCreateInput {
    name: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamRenameInput {
    name: String,
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionInput {
    expected_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamMemberInput {
    user_id: i32,
    expected_revision: String,
}

// -------------------------------------------------------------------- DTOs

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum PrincipalDto {
    #[serde(rename_all = "camelCase")]
    User {
        user_id: i32,
        display_name: String,
        avatar_url: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Team {
        team_id: String,
        name: String,
        member_count: i64,
    },
}

fn principal_dto(value: PrincipalView) -> PrincipalDto {
    match value {
        PrincipalView::User { user_id, display_name, avatar_url } => {
            PrincipalDto::User { user_id, display_name, avatar_url }
        }
        PrincipalView::Team { team_id, name, member_count } => {
            PrincipalDto::Team { team_id, name, member_count }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantDto {
    principal: PrincipalDto,
    level: &'static str,
}

fn grant_dtos(grants: Vec<Grant>) -> Vec<GrantDto> {
    grants
        .into_iter()
        .map(|grant| GrantDto { principal: principal_dto(grant.principal), level: grant.level.name() })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InheritedGrantDto {
    node_id: i32,
    node_name: String,
    principal: PrincipalDto,
    level: &'static str,
}

fn inherited_dtos(grants: Vec<InheritedGrant>) -> Vec<InheritedGrantDto> {
    grants
        .into_iter()
        .map(|grant| InheritedGrantDto {
            node_id: grant.node_id,
            node_name: grant.node_name,
            principal: principal_dto(grant.principal),
            level: grant.level.name(),
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceAccessDto {
    resource_id: String,
    visibility: &'static str,
    auth_revision: String,
    level: &'static str,
    can_manage: bool,
    bound_to: Option<String>,
    direct: Vec<GrantDto>,
    inherited: Vec<InheritedGrantDto>,
}

fn resource_access_dto(value: ResourceAccess) -> ResourceAccessDto {
    ResourceAccessDto {
        resource_id: value.resource_id,
        visibility: access::visibility_label(value.visibility),
        auth_revision: value.auth_revision.to_string(),
        level: value.level.name(),
        can_manage: value.can_manage,
        bound_to: value.bound_to,
        direct: grant_dtos(value.direct),
        inherited: inherited_dtos(value.inherited),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeAccessDto {
    node_id: i32,
    revision: String,
    can_manage: bool,
    can_restrict: bool,
    restricted: bool,
    level: &'static str,
    direct: Vec<GrantDto>,
    inherited: Vec<InheritedGrantDto>,
}

fn node_access_dto(value: NodeAccess) -> NodeAccessDto {
    NodeAccessDto {
        node_id: value.node_id,
        revision: value.revision.to_string(),
        can_manage: value.can_manage,
        can_restrict: value.can_restrict,
        restricted: value.restricted,
        level: value.level.name(),
        direct: grant_dtos(value.direct),
        inherited: inherited_dtos(value.inherited),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestrictionInput {
    expected_revision: String,
    restricted: bool,
}

/// PUT /api/neev/nodes/{nodeId}/restriction — the node's creator only.
pub(super) async fn put_node_restriction(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let _permit = permit(&state)?;
    positive(node_id)?;
    let (input, actor) =
        organization_mutation::<RestrictionInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let access =
        grants::set_node_restricted(&state, &actor, node_id, revision, input.restricted).await?;
    Ok(private_json(node_access_dto(access)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionAccessDto {
    session_id: String,
    revision: String,
    level: &'static str,
    can_manage: bool,
    direct: Vec<GrantDto>,
}

fn session_access_dto(value: SessionAccess) -> SessionAccessDto {
    SessionAccessDto {
        session_id: value.session_id,
        revision: value.revision.to_string(),
        level: value.level.name(),
        can_manage: value.can_manage,
        direct: grant_dtos(value.direct),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnerDto {
    user_id: i32,
    display_name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedSessionDto {
    session: SessionDto,
    owner: OwnerDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedSessionsDto {
    sessions: Vec<SharedSessionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryMemberDto {
    user_id: i32,
    display_name: String,
    avatar_url: Option<String>,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryDto {
    members: Vec<DirectoryMemberDto>,
    next_after: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamDto {
    id: String,
    name: String,
    member_count: i64,
    revision: String,
    can_manage: bool,
}

fn team_dto(team: teams::Team) -> TeamDto {
    TeamDto {
        id: team.id,
        name: team.name,
        member_count: team.member_count,
        revision: team.revision.to_string(),
        can_manage: team.can_manage,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamsDto {
    teams: Vec<TeamDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamMemberDto {
    user_id: i32,
    display_name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamMembersDto {
    team_id: String,
    revision: String,
    members: Vec<TeamMemberDto>,
}

fn team_members_dto(value: teams::TeamMembers) -> TeamMembersDto {
    TeamMembersDto {
        team_id: value.team_id,
        revision: value.revision.to_string(),
        members: value
            .members
            .into_iter()
            .map(|member| TeamMemberDto {
                user_id: member.user_id,
                display_name: member.display_name,
                avatar_url: member.avatar_url,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------- directory

/// GET /api/neev/organization/directory — any active member.
pub(super) async fn directory(
    State(state): State<Arc<ServerState>>,
    query: Result<Query<DirectoryQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let query = safe_query(query)?;
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    let page = teams::directory(
        &state,
        &actor,
        query.q.as_deref(),
        query.limit.unwrap_or(50),
        query.after.as_deref(),
    )
    .await?;
    Ok(private_json(DirectoryDto {
        members: page
            .members
            .into_iter()
            .map(|member| DirectoryMemberDto {
                user_id: member.user_id,
                display_name: member.display_name,
                avatar_url: member.avatar_url,
                role: match member.role {
                    Role::Admin => "admin",
                    Role::Member => "member",
                },
            })
            .collect(),
        next_after: page.next_after,
    }))
}

// -------------------------------------------------------------------- teams

pub(super) async fn list_teams(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    let teams = teams::list(&state, &actor).await?;
    Ok(private_json(TeamsDto { teams: teams.into_iter().map(team_dto).collect() }))
}

pub(super) async fn create_team(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Response {
    let result: Result<Response, Response> = async {
        let _permit = permit(&state).map_err(IntoResponse::into_response)?;
        let (input, actor) = organization_mutation::<TeamCreateInput>(&state, request, BODY_LIMIT)
            .await
            .map_err(IntoResponse::into_response)?;
        canonical_uuid(&input.request_id).map_err(IntoResponse::into_response)?;
        let team = teams::create(&state, &actor, &input.name, &input.request_id)
            .await
            .map_err(refusal)?;
        let mut response = private_json(team_dto(team));
        *response.status_mut() = StatusCode::CREATED;
        Ok(response)
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub(super) async fn rename_team(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let result: Result<Response, Response> = async {
        let id = safe_path(path).map_err(IntoResponse::into_response)?;
        let _permit = permit(&state).map_err(IntoResponse::into_response)?;
        canonical_uuid(&id).map_err(IntoResponse::into_response)?;
        let (input, actor) = organization_mutation::<TeamRenameInput>(&state, request, BODY_LIMIT)
            .await
            .map_err(IntoResponse::into_response)?;
        let revision =
            canonical_revision(&input.expected_revision).map_err(IntoResponse::into_response)?;
        let team = teams::rename(&state, &actor, &id, &input.name, revision)
            .await
            .map_err(refusal)?;
        Ok(private_json(team_dto(team)))
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub(super) async fn delete_team(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        organization_mutation::<RevisionInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    teams::delete(&state, &actor, &id, revision).await?;
    Ok(private_json(serde_json::json!({ "ok": true })))
}

pub(super) async fn team_members(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(private_json(team_members_dto(teams::members(&state, &actor, &id).await?)))
}

pub(super) async fn add_team_member(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        organization_mutation::<TeamMemberInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let result = teams::set_member(&state, &actor, &id, input.user_id, revision, true).await?;
    Ok(private_json(team_members_dto(result)))
}

pub(super) async fn remove_team_member(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, i32)>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let (id, user) = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) =
        organization_mutation::<RevisionInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let result = teams::set_member(&state, &actor, &id, user, revision, false).await?;
    Ok(private_json(team_members_dto(result)))
}

// ---------------------------------------------------------------- resources

pub(super) async fn resource_access(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(private_json(resource_access_dto(grants::resource_access(&state, &actor, &id).await?)))
}

pub(super) async fn put_resource_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let result: Result<Response, Response> = async {
        let id = safe_path(path).map_err(IntoResponse::into_response)?;
        let _permit = permit(&state).map_err(IntoResponse::into_response)?;
        canonical_uuid(&id).map_err(IntoResponse::into_response)?;
        let (input, actor) =
            organization_mutation::<ResourceGrantInput>(&state, request, BODY_LIMIT)
                .await
                .map_err(IntoResponse::into_response)?;
        let revision = canonical_revision(&input.expected_auth_revision)
            .map_err(IntoResponse::into_response)?;
        let principal = input.principal.checked().map_err(IntoResponse::into_response)?;
        let access = grants::set_resource_grant(
            &state,
            &actor,
            &id,
            revision,
            &principal,
            Some(input.level.into()),
        )
        .await
        .map_err(refusal)?;
        Ok(private_json(resource_access_dto(access)))
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub(super) async fn delete_resource_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Response {
    let result: Result<Response, Response> = async {
        let id = safe_path(path).map_err(IntoResponse::into_response)?;
        let _permit = permit(&state).map_err(IntoResponse::into_response)?;
        canonical_uuid(&id).map_err(IntoResponse::into_response)?;
        let (input, actor) =
            organization_mutation::<ResourceRevokeInput>(&state, request, BODY_LIMIT)
                .await
                .map_err(IntoResponse::into_response)?;
        let revision = canonical_revision(&input.expected_auth_revision)
            .map_err(IntoResponse::into_response)?;
        let principal = input.principal.checked().map_err(IntoResponse::into_response)?;
        let access = grants::set_resource_grant(&state, &actor, &id, revision, &principal, None)
            .await
            .map_err(refusal)?;
        Ok(private_json(resource_access_dto(access)))
    }
    .await;
    result.unwrap_or_else(|response| response)
}

// -------------------------------------------------------------------- nodes

fn positive(node_id: i32) -> Result<(), ApiError> {
    if node_id > 0 {
        Ok(())
    } else {
        Err(ApiError::Invalid)
    }
}

pub(super) async fn node_access(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let _permit = permit(&state)?;
    positive(node_id)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(private_json(node_access_dto(grants::node_access(&state, &actor, node_id).await?)))
}

pub(super) async fn put_node_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let _permit = permit(&state)?;
    positive(node_id)?;
    let (input, actor) = organization_mutation::<GrantInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let principal = input.principal.checked()?;
    let access = grants::set_node_grant(
        &state,
        &actor,
        node_id,
        revision,
        &principal,
        Some(input.level.into()),
    )
    .await?;
    Ok(private_json(node_access_dto(access)))
}

pub(super) async fn delete_node_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<i32>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let node_id = safe_path(path)?;
    let _permit = permit(&state)?;
    positive(node_id)?;
    let (input, actor) = organization_mutation::<RevokeInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let principal = input.principal.checked()?;
    let access =
        grants::set_node_grant(&state, &actor, node_id, revision, &principal, None).await?;
    Ok(private_json(node_access_dto(access)))
}

// ----------------------------------------------------------------- sessions

pub(super) async fn session_access(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(private_json(session_access_dto(grants::session_access(&state, &actor, &id).await?)))
}

pub(super) async fn put_session_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) = organization_mutation::<GrantInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let principal = input.principal.checked()?;
    // An Agent Chat is shared for viewing only; continuing it is a fork.
    let level: Level = input.level.into();
    if level != Level::View {
        return Err(ApiError::Invalid);
    }
    let access =
        grants::set_session_grant(&state, &actor, &id, revision, &principal, Some(level)).await?;
    Ok(private_json(session_access_dto(access)))
}

pub(super) async fn delete_session_grant(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let id = safe_path(path)?;
    let _permit = permit(&state)?;
    canonical_uuid(&id)?;
    let (input, actor) = organization_mutation::<RevokeInput>(&state, request, BODY_LIMIT).await?;
    let revision = canonical_revision(&input.expected_revision)?;
    let principal = input.principal.checked()?;
    let access = grants::set_session_grant(&state, &actor, &id, revision, &principal, None).await?;
    Ok(private_json(session_access_dto(access)))
}

pub(super) async fn shared_sessions(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    let sessions = sessions::shared_with(&state, &actor).await?;
    Ok(private_json(SharedSessionsDto {
        sessions: sessions
            .into_iter()
            .map(|(session, owner)| SharedSessionDto {
                session: session_dto(session),
                owner: OwnerDto {
                    user_id: owner.user_id,
                    display_name: owner.display_name,
                    avatar_url: owner.avatar_url,
                },
            })
            .collect(),
    }))
}
