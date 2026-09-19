use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};
use sha2::{Digest, Sha256};

use crate::{
    neev::{
        auth::session,
        db,
        db::entities::{Account, Membership, OrganizationRow},
        error::{database_error, AuthError},
        mail::quota,
        startup::ServerState,
        types::{
            IdentityContext, MemberId, OrgContext, OrgId, Organization, Role, SessionSecrets,
            UserId,
        },
    },
    organization_branding::normalize_logo_data_uri,
};

const REQUEST_TTL: i64 = 8 * 24 * 60 * 60;

pub(crate) struct OrganizationSummary {
    pub(crate) id: OrgId,
    pub(crate) name: String,
    pub(crate) role: Role,
}

pub(crate) struct OrganizationDetails {
    pub(crate) id: OrgId,
    pub(crate) name: String,
    pub(crate) logo_data: Option<String>,
    pub(crate) owner_user_id: UserId,
    pub(crate) branding_revision: i64,
}

pub(crate) struct MemberView {
    pub(crate) id: MemberId,
    pub(crate) user_id: UserId,
    pub(crate) display_name: String,
    pub(crate) avatar_url: Option<String>,
    pub(crate) role: Role,
}

pub(crate) enum BrandingLogo {
    Keep,
    Remove,
    Upload(String),
}

#[derive(FromQueryResult)]
pub(super) struct LockedMember {
    pub(super) id: MemberId,
    pub(super) org_id: OrgId,
    pub(super) user_id: UserId,
    pub(super) role: String,
    pub(super) revision: i64,
    pub(super) active: bool,
}

pub(crate) async fn create(
    state: &ServerState,
    identity: &IdentityContext,
    request_id: &str,
    name: &str,
) -> Result<Organization, AuthError> {
    let name = company_name(name)?;
    let request_id = normalize_request_id(request_id)?;
    let body_hash = digest_parts(&["organization.create", &name]);
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let scope_hint = session::identity_scope_hint(&state.database, identity).await?;
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    let account = session::lock_account(&tx, identity.user_id).await?;
    let scope = lock_identity_scope(&tx, identity.user_id, scope_hint).await?;
    session::validate_identity_locked(
        &tx,
        identity,
        &account,
        scope.as_ref().map(|(org, member)| (org, member)),
        now,
    )
    .await?;

    if let Some(row) = db::one(
        &tx,
        "SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action='organization.create' AND request_id=$2",
        vec![identity.user_id.into(), request_id.clone().into()],
    )
    .await?
    {
        let stored_hash: String = row.try_get("", "body_hash").map_err(database_error)?;
        if stored_hash != body_hash {
            return Err(AuthError::Conflict);
        }
        let resource: String = row.try_get("", "resource_id").map_err(database_error)?;
        let org_id = resource.parse::<i32>().map_err(|_| AuthError::Unavailable)?;
        let organization = load_organization(&tx, org_id).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(organization);
    }

    quota::reserve_in_transaction(
        &tx,
        &state.config.encryption_key,
        &[quota::Bucket::OrganizationCreation(identity.user_id)],
        now,
    )
    .await?;
    let row = db::one(
        &tx,
        "INSERT INTO neev_organizations(owner_user_id,name,revision,branding_revision,disabled,created_at) VALUES($1,$2,1,1,false,$3) RETURNING id",
        vec![identity.user_id.into(), name.clone().into(), now.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?;
    let org_id = row.try_get::<i32>("", "id").map_err(database_error)?;
    db::execute(
        &tx,
        "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,'admin',1,true)",
        vec![org_id.into(), identity.user_id.into()],
    )
    .await?;
    let expires_at = now.checked_add(REQUEST_TTL).ok_or(AuthError::Invalid)?;
    db::execute(
        &tx,
        "INSERT INTO neev_requests(user_id,action,request_id,body_hash,resource_id,expires_at) VALUES($1,'organization.create',$2,$3,$4,$5)",
        vec![identity.user_id.into(), request_id.into(), body_hash.into(), org_id.to_string().into(), expires_at.into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Organization {
        id: org_id,
        name,
        owner_user_id: identity.user_id,
        revision: 1,
    })
}

pub(crate) async fn list(
    state: &ServerState,
    identity: &IdentityContext,
) -> Result<Vec<OrganizationSummary>, AuthError> {
    Ok(list_page(state, identity, 100, 0).await?.0)
}

pub(crate) async fn list_page(
    state: &ServerState,
    identity: &IdentityContext,
    limit: u32,
    offset: u32,
) -> Result<(Vec<OrganizationSummary>, Option<u32>), AuthError> {
    if !(1..=100).contains(&limit) || offset > 100_000 {
        return Err(AuthError::Invalid);
    }
    let now = state.clock.now_unix();
    let scope_hint = session::identity_scope_hint(&state.database, identity).await?;
    let tx = db::begin(&state.database).await?;
    let account = session::lock_account(&tx, identity.user_id).await?;
    let scope = lock_identity_scope(&tx, identity.user_id, scope_hint).await?;
    session::validate_identity_locked(
        &tx,
        identity,
        &account,
        scope.as_ref().map(|(org, member)| (org, member)),
        now,
    )
    .await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT o.id,o.name,m.role FROM neev_memberships m JOIN neev_organizations o ON o.id=m.org_id WHERE m.user_id=$1 AND m.active=true AND o.disabled=false ORDER BY o.id LIMIT $2 OFFSET $3",
            vec![identity.user_id.into(), i64::from(limit + 1).into(), i64::from(offset).into()],
        ))
        .await
        .map_err(database_error)?;
    let mut result = rows
        .into_iter()
        .map(|row| {
            Ok(OrganizationSummary {
                id: row.try_get("", "id").map_err(database_error)?,
                name: row.try_get("", "name").map_err(database_error)?,
                role: session::role(&row.try_get::<String>("", "role").map_err(database_error)?)?,
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    let next_offset = if result.len() > limit as usize {
        result.truncate(limit as usize);
        Some(offset.checked_add(limit).ok_or(AuthError::Invalid)?)
    } else {
        None
    };
    tx.commit().await.map_err(database_error)?;
    Ok((result, next_offset))
}

pub(crate) async fn details(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<OrganizationDetails, AuthError> {
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let details = load_details(&tx, actor.org_id).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(details)
}

pub(crate) async fn update_branding(
    state: &ServerState,
    actor: &OrgContext,
    expected_branding_revision: i64,
    name: &str,
    logo: BrandingLogo,
) -> Result<OrganizationDetails, AuthError> {
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    if expected_branding_revision < 1 {
        return Err(AuthError::Invalid);
    }
    let name = company_name(name)?;
    let logo = match logo {
        BrandingLogo::Keep => None,
        BrandingLogo::Remove => Some(None),
        BrandingLogo::Upload(data) => Some(Some(
            normalize_logo_data_uri(&data).map_err(|_| AuthError::Invalid)?,
        )),
    };
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let current = load_details_locked(&tx, actor.org_id).await?;
    if current.branding_revision != expected_branding_revision {
        return Err(AuthError::Conflict);
    }
    let logo = logo.unwrap_or(current.logo_data);
    let updated = db::execute(
        &tx,
        "UPDATE neev_organizations SET name=$1,logo_data=$2,branding_revision=branding_revision+1 WHERE id=$3 AND branding_revision=$4",
        vec![name.into(), logo.into(), actor.org_id.into(), expected_branding_revision.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    let result = load_details(&tx, actor.org_id).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn members(
    state: &ServerState,
    actor: &OrgContext,
    limit: u32,
    offset: u32,
) -> Result<Vec<MemberView>, AuthError> {
    if !(1..=100).contains(&limit) || offset > 100_000 {
        return Err(AuthError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT m.id,m.user_id,m.role,a.display_name,a.avatar_url FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.active=true ORDER BY m.id LIMIT $2 OFFSET $3",
            vec![actor.org_id.into(), i64::from(limit).into(), i64::from(offset).into()],
        ))
        .await
        .map_err(database_error)?;
    let result = rows
        .into_iter()
        .map(|row| {
            Ok(MemberView {
                id: row.try_get("", "id").map_err(database_error)?,
                user_id: row.try_get("", "user_id").map_err(database_error)?,
                display_name: row.try_get("", "display_name").map_err(database_error)?,
                avatar_url: row.try_get("", "avatar_url").map_err(database_error)?,
                role: session::role(&row.try_get::<String>("", "role").map_err(database_error)?)?,
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn select(
    state: &ServerState,
    identity: &IdentityContext,
    org_id: OrgId,
) -> Result<SessionSecrets, AuthError> {
    session::select_organization(&state.database, identity, org_id, state.clock.now_unix()).await
}

pub(crate) async fn set_role(
    state: &ServerState,
    actor: &OrgContext,
    member_id: MemberId,
    role: Role,
) -> Result<(), AuthError> {
    mutate_member(state, actor, member_id, Some(role)).await
}

pub(crate) async fn remove(
    state: &ServerState,
    actor: &OrgContext,
    member_id: MemberId,
) -> Result<(), AuthError> {
    mutate_member(state, actor, member_id, None).await
}

async fn mutate_member(
    state: &ServerState,
    actor: &OrgContext,
    member_id: MemberId,
    new_role: Option<Role>,
) -> Result<(), AuthError> {
    if member_id <= 0 {
        return Err(AuthError::Invalid);
    }
    let candidate = db::one(
        &state.database,
        "SELECT org_id,user_id FROM neev_memberships WHERE id=$1",
        vec![member_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    let candidate_org: i32 = candidate.try_get("", "org_id").map_err(database_error)?;
    let target_user: i32 = candidate.try_get("", "user_id").map_err(database_error)?;
    if candidate_org != actor.org_id {
        return Err(AuthError::Forbidden);
    }
    let tx = db::begin(&state.database).await?;
    // Admission first (adopted lock order: admission, accounts, orgs, members,
    // session; gateway audience counter last). Serializes member mutations with
    // every other write-intent, exactly as logout/create/issuer already do.
    db::lock_admission_guard(&tx).await?;
    let mut users = vec![actor.identity.user_id, target_user];
    users.sort_unstable();
    users.dedup();
    for user in users {
        lock_account_any(&tx, user).await?;
    }
    let organization = lock_organization_details(&tx, actor.org_id).await?;
    let memberships = lock_all_memberships(&tx, actor.org_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let target = memberships
        .iter()
        .find(|member| member.id == member_id && member.user_id == target_user && member.active)
        .ok_or(AuthError::Forbidden)?;
    let actor_member = memberships
        .iter()
        .find(|member| member.id == actor.member_id && member.user_id == actor.identity.user_id)
        .ok_or(AuthError::Forbidden)?;
    let target_role = session::role(&target.role)?;
    if target.user_id == organization.owner_user_id {
        return Err(AuthError::Forbidden);
    }
    let self_removal = target.id == actor_member.id && new_role.is_none();
    if !self_removal && actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    if (target_role == Role::Admin || new_role == Some(Role::Admin))
        && actor.identity.user_id != organization.owner_user_id
    {
        return Err(AuthError::Forbidden);
    }
    if new_role.is_some_and(|role| role == target_role) {
        tx.commit().await.map_err(database_error)?;
        return Ok(());
    }
    if target_role == Role::Admin {
        let admins = memberships
            .iter()
            .filter(|member| member.active && member.role == "admin")
            .count();
        if admins <= 1 {
            return Err(AuthError::Forbidden);
        }
    }
    match new_role {
        Some(role) => {
            db::execute(
                &tx,
                "UPDATE neev_memberships SET role=$1,revision=revision+1 WHERE id=$2 AND active=true",
                vec![role_name(role).into(), target.id.into()],
            )
            .await?;
        }
        None => {
            db::execute(
                &tx,
                "UPDATE neev_memberships SET active=false,revision=revision+1 WHERE id=$1 AND active=true",
                vec![target.id.into()],
            )
            .await?;
        }
    }
    let now = state.clock.now_unix();
    revoke_scoped_state(&tx, actor.org_id, target.user_id, now).await?;
    // Deny-only gateway membership-scope event (integration-gated, counter last):
    // a removal or role change revokes every relying binding for (user, org).
    if state.config.gateway_events {
        crate::neev::gateway::issuer::append_revocation(
            &tx,
            "membership",
            Some(target.user_id),
            Some(actor.org_id),
            None,
            now,
        )
        .await?;
    }
    tx.commit().await.map_err(database_error)
}

pub(super) async fn revoke_scoped_state(
    tx: &DatabaseTransaction,
    org_id: OrgId,
    user_id: UserId,
    now: i64,
) -> Result<(), AuthError> {
    db::execute(
        tx,
        "UPDATE neev_sessions SET revoked_at=$1 WHERE org_id=$2 AND user_id=$3 AND revoked_at IS NULL",
        vec![now.into(), org_id.into(), user_id.into()],
    )
    .await?;
    for table in ["neev_oauth_attempts", "neev_magic_attempts"] {
        db::execute(
            tx,
            &format!("UPDATE {table} SET consumed_at=$1 WHERE link_user_id=$2 AND link_session_id IN (SELECT id FROM neev_sessions WHERE org_id=$3 AND user_id=$2) AND consumed_at IS NULL"),
            vec![now.into(), user_id.into(), org_id.into()],
        )
        .await?;
    }
    Ok(())
}

pub(super) fn company_name(input: &str) -> Result<String, AuthError> {
    let value = input.trim();
    if !(1..=100).contains(&value.chars().count()) || value.chars().any(char::is_control) {
        return Err(AuthError::Invalid);
    }
    Ok(value.to_owned())
}

pub(crate) fn normalize_request_id(input: &str) -> Result<String, AuthError> {
    uuid::Uuid::parse_str(input)
        .map(|id| id.to_string())
        .map_err(|_| AuthError::Invalid)
}

pub(crate) fn digest_parts(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"neev-request-body-v1\0");
    for part in parts {
        digest.update(part.len().to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
    }
}

pub(super) async fn lock_account_any(
    tx: &DatabaseTransaction,
    user_id: UserId,
) -> Result<Account, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,auth_revision,disabled FROM neev_accounts WHERE id=$1{}",
            session::lock(tx)
        ),
        vec![user_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    Account::from_query_result(&row, "").map_err(database_error)
}

pub(super) async fn lock_organization_details(
    tx: &DatabaseTransaction,
    org_id: OrgId,
) -> Result<OrganizationDetails, AuthError> {
    let details = load_details_query(tx, org_id, true).await?;
    let authority = session::lock_organization(tx, org_id).await?;
    if authority.id != details.id {
        return Err(AuthError::Unavailable);
    }
    Ok(details)
}

pub(super) async fn lock_identity_scope(
    tx: &DatabaseTransaction,
    user_id: UserId,
    hint: Option<session::IdentityScopeHint>,
) -> Result<Option<(OrganizationRow, Membership)>, AuthError> {
    let Some(hint) = hint else {
        return Ok(None);
    };
    let organization = session::lock_organization(tx, hint.org_id).await?;
    let membership = session::lock_membership(tx, hint.org_id, user_id).await?;
    if membership.id != hint.member_id
        || membership.revision != hint.member_revision
        || organization.revision != hint.org_revision
    {
        return Err(AuthError::Forbidden);
    }
    Ok(Some((organization, membership)))
}

async fn lock_all_memberships(
    tx: &DatabaseTransaction,
    org_id: OrgId,
) -> Result<Vec<LockedMember>, AuthError> {
    tx.query_all(db::statement(
        tx,
        &format!("SELECT id,org_id,user_id,role,revision,active FROM neev_memberships WHERE org_id=$1 ORDER BY id{}", session::lock(tx)),
        vec![org_id.into()],
    ))
    .await
    .map_err(database_error)?
    .iter()
    .map(|row| LockedMember::from_query_result(row, "").map_err(database_error))
    .collect()
}

async fn load_organization(
    tx: &DatabaseTransaction,
    org_id: OrgId,
) -> Result<Organization, AuthError> {
    let row = db::one(
        tx,
        "SELECT id,name,owner_user_id,revision,disabled FROM neev_organizations WHERE id=$1",
        vec![org_id.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?;
    if row
        .try_get::<bool>("", "disabled")
        .map_err(database_error)?
    {
        return Err(AuthError::Forbidden);
    }
    Ok(Organization {
        id: row.try_get("", "id").map_err(database_error)?,
        name: row.try_get("", "name").map_err(database_error)?,
        owner_user_id: row.try_get("", "owner_user_id").map_err(database_error)?,
        revision: row.try_get("", "revision").map_err(database_error)?,
    })
}

async fn load_details(
    tx: &DatabaseTransaction,
    org_id: OrgId,
) -> Result<OrganizationDetails, AuthError> {
    load_details_query(tx, org_id, false).await
}

async fn load_details_locked(
    tx: &DatabaseTransaction,
    org_id: OrgId,
) -> Result<OrganizationDetails, AuthError> {
    load_details_query(tx, org_id, true).await
}

async fn load_details_query(
    tx: &DatabaseTransaction,
    org_id: OrgId,
    locked: bool,
) -> Result<OrganizationDetails, AuthError> {
    let row = db::one(
        tx,
        &format!("SELECT id,name,logo_data,owner_user_id,branding_revision,disabled FROM neev_organizations WHERE id=$1{}", if locked { session::lock(tx) } else { "" }),
        vec![org_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    if row
        .try_get::<bool>("", "disabled")
        .map_err(database_error)?
    {
        return Err(AuthError::Forbidden);
    }
    Ok(OrganizationDetails {
        id: row.try_get("", "id").map_err(database_error)?,
        name: row.try_get("", "name").map_err(database_error)?,
        logo_data: row.try_get("", "logo_data").map_err(database_error)?,
        owner_user_id: row.try_get("", "owner_user_id").map_err(database_error)?,
        branding_revision: row
            .try_get("", "branding_revision")
            .map_err(database_error)?,
    })
}
