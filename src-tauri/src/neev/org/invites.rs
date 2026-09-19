use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};

use crate::neev::{
    auth::{email::canonical_email, secrets, secrets::TokenPurpose, session},
    db,
    error::{database_error, AuthError},
    mail::{quota, MailMessage},
    startup::ServerState,
    types::{IdentityContext, OrgContext, OrgId, Role, UserId},
};

use super::service::{
    digest_parts, lock_account_any, lock_organization_details, normalize_request_id, role_name,
    LockedMember, OrganizationDetails,
};

const INVITE_TTL: i64 = 7 * 24 * 60 * 60;
const REQUEST_TTL: i64 = 8 * 24 * 60 * 60;
const OUTSTANDING_LIMIT: i64 = 100;

pub(crate) struct Preview {
    pub(crate) org_id: OrgId,
    pub(crate) organization_name: String,
}

#[derive(FromQueryResult)]
struct Invitation {
    id: String,
    org_id: OrgId,
    inviter_id: UserId,
    recipient_email: String,
    role: String,
    token_hash: String,
    expires_at: i64,
    revoked_at: Option<i64>,
    accepted_by: Option<UserId>,
    accepted_at: Option<i64>,
}

pub(crate) async fn send(
    state: &ServerState,
    actor: &OrgContext,
    email: &str,
    role: Role,
    request_id: &str,
) -> Result<String, AuthError> {
    let recipient = canonical_email(email).ok_or(AuthError::Invalid)?;
    let request_id = normalize_request_id(request_id)?;
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let role_value = role_name(role);
    let org_part = actor.org_id.to_string();
    let body_hash = digest_parts(&["invitation.send", &org_part, &recipient, role_value]);
    let token = secrets::random_token(TokenPurpose::Invite)?;
    let token_hash = secrets::digest(TokenPurpose::Invite, &token)?;
    let invitation_id = uuid::Uuid::new_v4().to_string();

    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, now).await?;
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    let organization = lock_organization_details(&tx, actor.org_id).await?;
    if role == Role::Admin && organization.owner_user_id != actor.identity.user_id {
        return Err(AuthError::Forbidden);
    }
    let mail = state.mail.as_ref().ok_or(AuthError::Unavailable)?;
    if let Some(row) = db::one(
        &tx,
        "SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action='invitation.send' AND request_id=$2",
        vec![actor.identity.user_id.into(), request_id.clone().into()],
    )
    .await?
    {
        let stored_hash: String = row.try_get("", "body_hash").map_err(database_error)?;
        if stored_hash != body_hash {
            return Err(AuthError::Conflict);
        }
        let id: String = row.try_get("", "resource_id").map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        return Ok(id);
    }
    let outstanding = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_invitations WHERE org_id=$1 AND revoked_at IS NULL AND accepted_at IS NULL AND expires_at>$2",
        vec![actor.org_id.into(), now.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get::<i64>("", "n")
    .map_err(database_error)?;
    if outstanding >= OUTSTANDING_LIMIT {
        return Err(AuthError::Limited);
    }
    quota::reserve_in_transaction(
        &tx,
        &state.config.encryption_key,
        &[
            quota::Bucket::InvitationActor(actor.identity.user_id),
            quota::Bucket::InvitationOrganization(actor.org_id),
            quota::Bucket::DeliveryMinute,
            quota::Bucket::DeliveryDay,
        ],
        now,
    )
    .await?;
    let expires_at = now.checked_add(INVITE_TTL).ok_or(AuthError::Invalid)?;
    db::execute(
        &tx,
        "INSERT INTO neev_invitations(id,org_id,inviter_id,recipient_email,role,token_hash,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",
        vec![invitation_id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), recipient.clone().into(), role_value.into(), token_hash.into(), expires_at.into()],
    )
    .await?;
    db::execute(
        &tx,
        "INSERT INTO neev_requests(user_id,action,request_id,body_hash,resource_id,expires_at) VALUES($1,'invitation.send',$2,$3,$4,$5)",
        vec![actor.identity.user_id.into(), request_id.into(), body_hash.into(), invitation_id.clone().into(), now.checked_add(REQUEST_TTL).ok_or(AuthError::Invalid)?.into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;

    let mut link = state
        .config
        .origin
        .join("auth/invite")
        .map_err(|_| AuthError::Unavailable)?;
    link.set_fragment(Some(&format!("token={token}")));
    mail.send(MailMessage {
        recipient,
        subject: "You're invited to Neev".into(),
        text: format!(
            "You were invited to join {} on Neev. Sign in with this exact recipient address, then confirm the organization:\n\n{}\n\nThis invitation expires in 7 days.",
            organization.name,
            link.as_str()
        ),
    })
    .await?;
    Ok(invitation_id)
}

pub(crate) async fn preview(
    state: &ServerState,
    identity: &IdentityContext,
    token: &str,
) -> Result<Preview, AuthError> {
    let locked = lock_acceptance(state, identity, token).await?;
    if locked.invitation.revoked_at.is_some()
        || locked.invitation.expires_at <= locked.now
        || locked.invitation.accepted_at.is_some()
    {
        return Err(AuthError::Forbidden);
    }
    let result = Preview {
        org_id: locked.organization.id,
        organization_name: locked.organization.name,
    };
    locked.tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn accept(
    state: &ServerState,
    identity: &IdentityContext,
    token: &str,
    confirmed_org_id: OrgId,
) -> Result<OrgId, AuthError> {
    let locked = lock_acceptance(state, identity, token).await?;
    if locked.invitation.org_id != confirmed_org_id {
        return Err(AuthError::Forbidden);
    }
    if let Some(accepted_by) = locked.invitation.accepted_by {
        if accepted_by != identity.user_id {
            return Err(AuthError::Forbidden);
        }
        let active = locked
            .actor_membership
            .as_ref()
            .is_some_and(|member| member.active);
        if !active {
            return Err(AuthError::Forbidden);
        }
        locked.tx.commit().await.map_err(database_error)?;
        return Ok(confirmed_org_id);
    }
    if locked.invitation.revoked_at.is_some()
        || locked.invitation.accepted_at.is_some()
        || locked.invitation.expires_at <= locked.now
    {
        return Err(AuthError::Forbidden);
    }
    let invited_role = session::role(&locked.invitation.role)?;
    // Reactivating an inactive member increments the membership revision and
    // therefore changes prior authority; first-member insertion and active
    // replay do not. Only the revision-changing reactivation earns an event.
    let reactivated = match locked.actor_membership {
        Some(member) if member.active => false,
        Some(member) => {
            db::execute(
                &locked.tx,
                "UPDATE neev_memberships SET role=$1,active=true,revision=revision+1 WHERE id=$2 AND active=false",
                vec![role_name(invited_role).into(), member.id.into()],
            )
            .await?;
            true
        }
        None => {
            db::execute(
                &locked.tx,
                "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,$3,1,true)",
                vec![confirmed_org_id.into(), identity.user_id.into(), role_name(invited_role).into()],
            )
            .await?;
            false
        }
    };
    let updated = db::execute(
        &locked.tx,
        "UPDATE neev_invitations SET accepted_by=$1,accepted_at=$2 WHERE id=$3 AND accepted_by IS NULL AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>$2",
        vec![identity.user_id.into(), locked.now.into(), locked.invitation.id.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    // Deny-only gateway membership-scope event (integration-gated, counter last):
    // reactivation revokes every relying binding for (user, org). Appended after
    // the conditional acceptance succeeds and before commit.
    if reactivated && state.config.gateway_events {
        crate::neev::gateway::issuer::append_revocation(
            &locked.tx,
            "membership",
            Some(identity.user_id),
            Some(confirmed_org_id),
            None,
            locked.now,
        )
        .await?;
    }
    locked.tx.commit().await.map_err(database_error)?;
    Ok(confirmed_org_id)
}

pub(crate) async fn revoke(
    state: &ServerState,
    actor: &OrgContext,
    invite_id: &str,
) -> Result<(), AuthError> {
    let invite_id = normalize_request_id(invite_id)?;
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    let organization = lock_organization_details(&tx, actor.org_id).await?;
    let invitation = load_invitation_by_id(&tx, &invite_id, true).await?;
    if invitation.org_id != actor.org_id
        || invitation.accepted_at.is_some()
        || invitation.revoked_at.is_some()
    {
        return Err(AuthError::Forbidden);
    }
    if session::role(&invitation.role)? == Role::Admin
        && organization.owner_user_id != actor.identity.user_id
    {
        return Err(AuthError::Forbidden);
    }
    let updated = db::execute(
        &tx,
        "UPDATE neev_invitations SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL AND accepted_at IS NULL",
        vec![state.clock.now_unix().into(), invite_id.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    tx.commit().await.map_err(database_error)
}

struct AcceptanceLocks {
    tx: DatabaseTransaction,
    invitation: Invitation,
    organization: OrganizationDetails,
    actor_membership: Option<LockedMember>,
    now: i64,
}

async fn lock_acceptance(
    state: &ServerState,
    identity: &IdentityContext,
    token: &str,
) -> Result<AcceptanceLocks, AuthError> {
    let token_hash =
        secrets::digest(TokenPurpose::Invite, token).map_err(|_| AuthError::Forbidden)?;
    let hint = load_invitation_by_hash(&state.database, &token_hash, false).await?;
    let scope = session::identity_scope_hint(&state.database, identity).await?;
    let now = state.clock.now_unix();
    let tx = db::begin(&state.database).await?;
    // Admission first (adopted lock order: admission, accounts, orgs, members,
    // session/attempt/invite; gateway audience counter last).
    db::lock_admission_guard(&tx).await?;

    let mut user_ids = vec![identity.user_id, hint.inviter_id];
    user_ids.sort_unstable();
    user_ids.dedup();
    let mut accounts = Vec::new();
    for user_id in user_ids {
        accounts.push(lock_account_any(&tx, user_id).await?);
    }
    let actor_account = accounts
        .iter()
        .find(|account| account.id == identity.user_id)
        .ok_or(AuthError::Unauthorized)?;
    let inviter_disabled = accounts
        .iter()
        .find(|account| account.id == hint.inviter_id)
        .ok_or(AuthError::Forbidden)?
        .disabled;

    let mut org_ids = vec![hint.org_id];
    if let Some(scope) = scope {
        org_ids.push(scope.org_id);
    }
    org_ids.sort_unstable();
    org_ids.dedup();
    let mut organizations = Vec::new();
    for org_id in org_ids {
        organizations.push(session::lock_organization(&tx, org_id).await?);
    }

    let preliminary = membership_candidates(&tx, identity, &hint, scope).await?;
    let mut memberships = Vec::new();
    for id in preliminary {
        memberships.push(lock_member_by_id(&tx, id).await?);
    }
    if let Some(scope) = scope {
        let scope_org = organizations
            .iter()
            .find(|org| org.id == scope.org_id)
            .ok_or(AuthError::Forbidden)?;
        let scope_member = memberships
            .iter()
            .find(|member| member.id == scope.member_id && member.user_id == identity.user_id)
            .ok_or(AuthError::Forbidden)?;
        if scope_org.revision != scope.org_revision
            || scope_member.revision != scope.member_revision
            || !scope_member.active
            || session::role(&scope_member.role).is_err()
        {
            return Err(AuthError::Forbidden);
        }
    }
    session::validate_identity_session_locked(&tx, identity, actor_account, scope, now).await?;
    let invitation = load_invitation_by_hash(&tx, &token_hash, true).await?;
    if invitation.id != hint.id
        || invitation.org_id != hint.org_id
        || invitation.inviter_id != hint.inviter_id
        || invitation.token_hash != token_hash
    {
        return Err(AuthError::Forbidden);
    }
    let recipient_matches = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_verified_emails WHERE user_id=$1 AND canonical_email=$2",
        vec![
            identity.user_id.into(),
            invitation.recipient_email.clone().into(),
        ],
    )
    .await?
    .is_some();
    if !recipient_matches {
        return Err(AuthError::Forbidden);
    }
    let organization = lock_organization_details(&tx, invitation.org_id).await?;
    if invitation.accepted_by.is_none() {
        let inviter = memberships
            .iter()
            .find(|member| {
                member.org_id == invitation.org_id && member.user_id == invitation.inviter_id
            })
            .ok_or(AuthError::Forbidden)?;
        if inviter_disabled || !inviter.active || session::role(&inviter.role)? != Role::Admin {
            return Err(AuthError::Forbidden);
        }
        if session::role(&invitation.role)? == Role::Admin
            && organization.owner_user_id != invitation.inviter_id
        {
            return Err(AuthError::Forbidden);
        }
    }
    let actor_membership = memberships
        .into_iter()
        .find(|member| member.org_id == invitation.org_id && member.user_id == identity.user_id);
    Ok(AcceptanceLocks {
        tx,
        invitation,
        organization,
        actor_membership,
        now,
    })
}

async fn membership_candidates(
    database: &impl sea_orm::ConnectionTrait,
    identity: &IdentityContext,
    invite: &Invitation,
    scope: Option<session::IdentityScopeHint>,
) -> Result<Vec<i32>, AuthError> {
    let mut pairs = vec![
        (invite.org_id, invite.inviter_id),
        (invite.org_id, identity.user_id),
    ];
    if let Some(scope) = scope {
        pairs.push((scope.org_id, identity.user_id));
    }
    let mut ids = Vec::new();
    for (org_id, user_id) in pairs {
        if let Some(row) = db::one(
            database,
            "SELECT id FROM neev_memberships WHERE org_id=$1 AND user_id=$2",
            vec![org_id.into(), user_id.into()],
        )
        .await?
        {
            ids.push(row.try_get::<i32>("", "id").map_err(database_error)?);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

async fn lock_member_by_id(tx: &DatabaseTransaction, id: i32) -> Result<LockedMember, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,org_id,user_id,role,revision,active FROM neev_memberships WHERE id=$1{}",
            session::lock(tx)
        ),
        vec![id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    LockedMember::from_query_result(&row, "").map_err(database_error)
}

async fn load_invitation_by_hash<C: ConnectionTrait>(
    database: &C,
    token_hash: &str,
    locked: bool,
) -> Result<Invitation, AuthError> {
    let row = db::one(
        database,
        &format!("SELECT id,org_id,inviter_id,recipient_email,role,token_hash,expires_at,revoked_at,accepted_by,accepted_at FROM neev_invitations WHERE token_hash=$1{}", if locked && database.get_database_backend() == sea_orm::DbBackend::Postgres { " FOR UPDATE" } else { "" }),
        vec![token_hash.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    Invitation::from_query_result(&row, "").map_err(database_error)
}

async fn load_invitation_by_id(
    tx: &DatabaseTransaction,
    id: &str,
    locked: bool,
) -> Result<Invitation, AuthError> {
    let row = db::one(
        tx,
        &format!("SELECT id,org_id,inviter_id,recipient_email,role,token_hash,expires_at,revoked_at,accepted_by,accepted_at FROM neev_invitations WHERE id=$1{}", if locked { session::lock(tx) } else { "" }),
        vec![id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    Invitation::from_query_result(&row, "").map_err(database_error)
}
