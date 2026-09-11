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
    org::{self, invites, BrandingLogo, OrganizationDetails},
    startup::ServerState,
    types::{IdentityContext, OrgContext, Role},
};

use super::{auth, cookies, private_json, ApiError};

const BRANDING_BODY_LIMIT: usize = 1_500_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateInput {
    request_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectInput {
    organization_id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoleInput {
    role: RoleInputValue,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RoleInputValue {
    Admin,
    Member,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrandingInput {
    expected_branding_revision: String,
    name: String,
    logo: LogoInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LogoInput {
    action: LogoAction,
    #[serde(default, deserialize_with = "deserialize_logo_data")]
    data: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogoAction {
    Keep,
    Remove,
    Upload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendInvitationInput {
    email: String,
    role: RoleInputValue,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenInput {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptInvitationInput {
    token: String,
    confirmed_org_id: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pagination {
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationDto {
    id: i32,
    name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationWithRoleDto {
    id: i32,
    name: String,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatedDto {
    organization: OrganizationDto,
    role: &'static str,
    branding_revision: String,
}

#[derive(Serialize)]
struct OrganizationsDto {
    organizations: Vec<OrganizationWithRoleDto>,
    #[serde(rename = "nextOffset")]
    next_offset: Option<u32>,
}

#[derive(Serialize)]
struct SelectedDto {
    organization: OrganizationDto,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DetailsDto {
    id: i32,
    name: String,
    avatar_url: Option<String>,
    owner_user_id: i32,
    branding_revision: String,
}

#[derive(Serialize)]
struct MembersDto {
    members: Vec<MemberDto>,
}

#[derive(Serialize)]
struct MemberDto {
    id: i32,
    user: MemberUserDto,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberUserDto {
    id: i32,
    display_name: String,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvitationSentDto {
    invitation_id: String,
}

#[derive(Serialize)]
struct InvitationPreviewDto {
    organization: InvitationOrganizationDto,
}

#[derive(Serialize)]
struct InvitationOrganizationDto {
    id: i32,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvitationAcceptedDto {
    organization_id: i32,
}

pub(super) async fn create(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, identity, _) = identity_mutation::<CreateInput>(&state, request, 4096).await?;
    let created = org::create(&state, &identity, &input.request_id, &input.name).await?;
    let mut response = private_json(CreatedDto {
        organization: OrganizationDto {
            id: created.id,
            name: created.name,
            avatar_url: None,
        },
        role: "admin",
        branding_revision: "1".to_owned(),
    });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn list(
    State(state): State<Arc<ServerState>>,
    Query(page): Query<Pagination>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let identity = identity_get(&state, &headers).await?;
    let (organizations, next_offset) =
        org::list_page(&state, &identity, page.limit, page.offset).await?;
    let organizations = organizations
        .into_iter()
        .map(|organization| OrganizationWithRoleDto {
            id: organization.id,
            name: organization.name,
            role: role_name(organization.role),
        })
        .collect();
    Ok(private_json(OrganizationsDto {
        organizations,
        next_offset,
    }))
}

pub(super) async fn select(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, identity, _) = identity_mutation::<SelectInput>(&state, request, 4096).await?;
    let secrets = org::select(&state, &identity, input.organization_id).await?;
    let view = session::view(
        &state.database,
        &secrets.session,
        Some(&secrets.csrf),
        state.clock.now_unix(),
    )
    .await?;
    let organization = view.organization.ok_or(ApiError::Forbidden)?;
    let role = view.role.ok_or(ApiError::Forbidden)?;
    let mut response = private_json(SelectedDto {
        organization: OrganizationDto {
            id: organization.id,
            name: organization.name,
            avatar_url: organization.logo_data,
        },
        role: role_name(role),
    });
    let names = cookies::CookieNames::for_preview(state.config.preview);
    cookies::set_session(
        response.headers_mut(),
        names,
        &secrets.session,
        &secrets.csrf,
        secrets.expires_at.saturating_sub(state.clock.now_unix()),
    );
    Ok(response)
}

pub(super) async fn details(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    Ok(private_json(details_dto(
        org::details(&state, &actor).await?,
    )))
}

pub(super) async fn update_branding(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, _, token) =
        identity_mutation::<BrandingInput>(&state, request, BRANDING_BODY_LIMIT).await?;
    let actor = session::organization(&state.database, &token, state.clock.now_unix()).await?;
    let revision = canonical_revision(&input.expected_branding_revision)?;
    let logo = match (input.logo.action, input.logo.data) {
        (LogoAction::Keep, None) => BrandingLogo::Keep,
        (LogoAction::Remove, None) => BrandingLogo::Remove,
        (LogoAction::Upload, Some(data)) => BrandingLogo::Upload(data),
        _ => return Err(ApiError::Invalid),
    };
    Ok(private_json(details_dto(
        org::update_branding(&state, &actor, revision, &input.name, logo).await?,
    )))
}

pub(super) async fn members(
    State(state): State<Arc<ServerState>>,
    Query(page): Query<Pagination>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let actor = organization_get(&state, &headers).await?;
    let members = org::members(&state, &actor, page.limit, page.offset)
        .await?
        .into_iter()
        .map(|member| MemberDto {
            id: member.id,
            user: MemberUserDto {
                id: member.user_id,
                display_name: member.display_name,
                avatar_url: member.avatar_url,
            },
            role: role_name(member.role),
        })
        .collect();
    Ok(private_json(MembersDto { members }))
}

pub(super) async fn set_role(
    State(state): State<Arc<ServerState>>,
    Path(member_id): Path<i32>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, _, token) = identity_mutation::<RoleInput>(&state, request, 1024).await?;
    let actor = session::organization(&state.database, &token, state.clock.now_unix()).await?;
    org::set_role(&state, &actor, member_id, input.role.into()).await?;
    Ok(no_content())
}

pub(super) async fn remove(
    State(state): State<Arc<ServerState>>,
    Path(member_id): Path<i32>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (_input, _, token) = identity_mutation::<EmptyInput>(&state, request, 1024).await?;
    let actor = session::organization(&state.database, &token, state.clock.now_unix()).await?;
    org::remove(&state, &actor, member_id).await?;
    Ok(no_content())
}

pub(super) async fn send_invitation(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, _, token) = identity_mutation::<SendInvitationInput>(&state, request, 4096).await?;
    let actor = session::organization(&state.database, &token, state.clock.now_unix()).await?;
    let invitation_id = invites::send(
        &state,
        &actor,
        &input.email,
        input.role.into(),
        &input.request_id,
    )
    .await?;
    let mut response = private_json(InvitationSentDto { invitation_id });
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

pub(super) async fn revoke_invitation(
    State(state): State<Arc<ServerState>>,
    Path(invite_id): Path<String>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (_input, _, token) = identity_mutation::<EmptyInput>(&state, request, 1024).await?;
    let actor = session::organization(&state.database, &token, state.clock.now_unix()).await?;
    invites::revoke(&state, &actor, &invite_id).await?;
    Ok(no_content())
}

pub(super) async fn preview_invitation(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, identity, _) = identity_mutation::<TokenInput>(&state, request, 1024).await?;
    let preview = invites::preview(&state, &identity, &input.token).await?;
    Ok(private_json(InvitationPreviewDto {
        organization: InvitationOrganizationDto {
            id: preview.org_id,
            name: preview.organization_name,
        },
    }))
}

pub(super) async fn accept_invitation(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = permit(&state)?;
    let (input, identity, _) =
        identity_mutation::<AcceptInvitationInput>(&state, request, 1024).await?;
    let organization_id =
        invites::accept(&state, &identity, &input.token, input.confirmed_org_id).await?;
    Ok(private_json(InvitationAcceptedDto { organization_id }))
}

async fn identity_get(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<IdentityContext, ApiError> {
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(headers, names.session)?;
    Ok(
        session::view(&state.database, token, None, state.clock.now_unix())
            .await?
            .identity,
    )
}

async fn organization_get(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<OrgContext, ApiError> {
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(headers, names.session)?;
    Ok(session::organization(&state.database, token, state.clock.now_unix()).await?)
}

async fn identity_mutation<T: for<'de> Deserialize<'de>>(
    state: &ServerState,
    request: Request<Body>,
    limit: usize,
) -> Result<(T, IdentityContext, String), ApiError> {
    let (headers, input) =
        auth::json_request_limited::<T>(request, &state.config.origin_header, limit).await?;
    let names = cookies::CookieNames::for_preview(state.config.preview);
    let token = cookies::required(&headers, names.session)?.to_owned();
    let cookie_csrf = cookies::required(&headers, names.csrf)?;
    if cookie_csrf != cookies::csrf_header(&headers)? {
        return Err(ApiError::Unauthorized);
    }
    let identity = session::view(
        &state.database,
        &token,
        Some(cookie_csrf),
        state.clock.now_unix(),
    )
    .await?
    .identity;
    Ok((input, identity, token))
}

fn details_dto(details: OrganizationDetails) -> DetailsDto {
    DetailsDto {
        id: details.id,
        name: details.name,
        avatar_url: details.logo_data,
        owner_user_id: details.owner_user_id,
        branding_revision: details.branding_revision.to_string(),
    }
}

fn canonical_revision(value: &str) -> Result<i64, ApiError> {
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

fn deserialize_logo_data<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn default_limit() -> u32 {
    50
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
    }
}

impl From<RoleInputValue> for Role {
    fn from(value: RoleInputValue) -> Self {
        match value {
            RoleInputValue::Admin => Self::Admin,
            RoleInputValue::Member => Self::Member,
        }
    }
}

fn permit(state: &ServerState) -> Result<tokio::sync::SemaphorePermit<'_>, ApiError> {
    state
        .auth_limit
        .try_acquire()
        .map_err(|_| ApiError::Limited)
}

fn no_content() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    super::add_private_headers(&mut response);
    response
}

use axum::response::IntoResponse;
