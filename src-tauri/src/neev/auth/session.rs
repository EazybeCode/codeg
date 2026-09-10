use crate::neev::{
    db::{
        self,
        entities::{Account, Membership, OrganizationRow, Session},
    },
    error::{database_error, AuthError},
    types::{IdentityContext, OrgContext, OrgId, Role, SessionSecrets, UserId},
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, FromQueryResult,
};

use super::secrets::{self, TokenPurpose};

const IDENTITY_TTL: i64 = 30 * 60;
const ORGANIZATION_TTL: i64 = 12 * 60 * 60;

pub(crate) async fn issue_identity(
    database: &DatabaseConnection,
    user_id: UserId,
    now: i64,
) -> Result<SessionSecrets, AuthError> {
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let expires_at = now.checked_add(IDENTITY_TTL).ok_or(AuthError::Invalid)?;
    let session = secrets::random_token(TokenPurpose::Session)?;
    let csrf = secrets::random_token(TokenPurpose::Csrf)?;
    let session_hash = secrets::digest(TokenPurpose::Session, &session)?;
    let csrf_hash = secrets::digest(TokenPurpose::Csrf, &csrf)?;
    let id = uuid::Uuid::new_v4().to_string();
    let tx = db::begin(database).await?;
    let account = lock_account(&tx, user_id).await?;
    db::execute(
        &tx,
        "INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,authenticated_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",
        vec![
            id.into(), session_hash.into(), csrf_hash.into(), user_id.into(),
            account.auth_revision.into(), now.into(), expires_at.into(),
        ],
    ).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(SessionSecrets {
        session,
        csrf,
        expires_at,
    })
}

pub(crate) async fn identity(
    database: &DatabaseConnection,
    token: &str,
    now: i64,
) -> Result<IdentityContext, AuthError> {
    let token_hash =
        secrets::digest(TokenPurpose::Session, token).map_err(|_| AuthError::Unauthorized)?;
    let row = db::one(
        database,
        "SELECT s.id,s.user_id,s.account_revision,s.org_id,s.member_id,s.member_revision,s.org_revision,s.authenticated_at,s.expires_at,s.revoked_at,a.auth_revision AS current_revision,a.disabled AS account_disabled FROM neev_sessions s JOIN neev_accounts a ON a.id=s.user_id WHERE s.token_hash=$1",
        vec![token_hash.into()],
    ).await?.ok_or(AuthError::Unauthorized)?;
    let session = Session::from_query_result(&row, "").map_err(database_error)?;
    let current_revision: i64 = row
        .try_get("", "current_revision")
        .map_err(database_error)?;
    let disabled: bool = row
        .try_get("", "account_disabled")
        .map_err(database_error)?;
    validate_session(&session, current_revision, disabled, now)?;
    Ok(IdentityContext {
        user_id: session.user_id,
        session_id: session.id,
        auth_revision: session.account_revision,
        authenticated_at: session.authenticated_at,
    })
}

pub(crate) async fn organization(
    database: &DatabaseConnection,
    token: &str,
    now: i64,
) -> Result<OrgContext, AuthError> {
    organization_after_lookup(database, token, now, || async {}).await
}

async fn organization_after_lookup<F, Future>(
    database: &DatabaseConnection,
    token: &str,
    now: i64,
    after_lookup: F,
) -> Result<OrgContext, AuthError>
where
    F: FnOnce() -> Future,
    Future: std::future::Future<Output = ()>,
{
    let token_hash =
        secrets::digest(TokenPurpose::Session, token).map_err(|_| AuthError::Unauthorized)?;
    let tx = db::begin(database).await?;
    let row = db::one(
        &tx,
        "SELECT s.id,s.user_id,s.account_revision,s.org_id,s.member_id,s.member_revision,s.org_revision,s.authenticated_at,s.expires_at,s.revoked_at,m.role FROM neev_sessions s LEFT JOIN neev_memberships m ON m.id=s.member_id AND m.org_id=s.org_id AND m.user_id=s.user_id WHERE s.token_hash=$1",
        vec![token_hash.into()],
    ).await?.ok_or(AuthError::Unauthorized)?;
    let session = Session::from_query_result(&row, "").map_err(database_error)?;
    let role_name: Option<String> = row.try_get("", "role").map_err(database_error)?;
    let context = OrgContext {
        identity: IdentityContext {
            user_id: session.user_id,
            session_id: session.id,
            auth_revision: session.account_revision,
            authenticated_at: session.authenticated_at,
        },
        org_id: session.org_id.ok_or(AuthError::Forbidden)?,
        member_id: session.member_id.ok_or(AuthError::Forbidden)?,
        member_revision: session.member_revision.ok_or(AuthError::Forbidden)?,
        org_revision: session.org_revision.ok_or(AuthError::Forbidden)?,
        role: role(&role_name.ok_or(AuthError::Forbidden)?)?,
    };
    after_lookup().await;
    recheck(&tx, &context, now).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(context)
}

pub(crate) async fn select_organization(
    database: &DatabaseConnection,
    identity: &IdentityContext,
    org_id: OrgId,
    now: i64,
) -> Result<SessionSecrets, AuthError> {
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let expires_at = now
        .checked_add(ORGANIZATION_TTL)
        .ok_or(AuthError::Invalid)?;
    let session = secrets::random_token(TokenPurpose::Session)?;
    let csrf = secrets::random_token(TokenPurpose::Csrf)?;
    let session_hash = secrets::digest(TokenPurpose::Session, &session)?;
    let csrf_hash = secrets::digest(TokenPurpose::Csrf, &csrf)?;
    let new_id = uuid::Uuid::new_v4().to_string();
    let tx = db::begin(database).await?;

    let account = lock_account(&tx, identity.user_id).await?;
    if account.auth_revision != identity.auth_revision {
        return Err(AuthError::Unauthorized);
    }
    let organization = lock_organization(&tx, org_id).await?;
    let membership = lock_membership(&tx, org_id, identity.user_id).await?;
    let previous = lock_session(&tx, &identity.session_id).await?;
    validate_session(&previous, account.auth_revision, account.disabled, now)?;
    if previous.user_id != identity.user_id
        || previous.account_revision != identity.auth_revision
        || previous.authenticated_at != identity.authenticated_at
        || previous.org_id.is_some()
        || previous.member_id.is_some()
        || previous.member_revision.is_some()
        || previous.org_revision.is_some()
    {
        return Err(AuthError::Unauthorized);
    }

    db::execute(
        &tx,
        "INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        vec![
            new_id.into(), session_hash.into(), csrf_hash.into(), identity.user_id.into(),
            account.auth_revision.into(), org_id.into(), membership.id.into(),
            membership.revision.into(), organization.revision.into(),
            identity.authenticated_at.into(), expires_at.into(),
        ],
    ).await?;
    let result = db::execute(
        &tx,
        "UPDATE neev_sessions SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL",
        vec![now.into(), identity.session_id.clone().into()],
    )
    .await?;
    if result.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    tx.commit().await.map_err(database_error)?;
    Ok(SessionSecrets {
        session,
        csrf,
        expires_at,
    })
}

pub(crate) async fn validate_csrf(
    database: &DatabaseConnection,
    identity: &IdentityContext,
    raw: &str,
    now: i64,
) -> Result<(), AuthError> {
    let hash = secrets::digest(TokenPurpose::Csrf, raw).map_err(|_| AuthError::Unauthorized)?;
    let row = db::one(
        database,
        "SELECT csrf_hash FROM neev_sessions WHERE id=$1 AND user_id=$2 AND account_revision=$3 AND revoked_at IS NULL AND expires_at>$4",
        vec![
            identity.session_id.clone().into(), identity.user_id.into(),
            identity.auth_revision.into(), now.into(),
        ],
    ).await?.ok_or(AuthError::Unauthorized)?;
    let expected: String = row.try_get("", "csrf_hash").map_err(database_error)?;
    if expected == hash {
        Ok(())
    } else {
        Err(AuthError::Unauthorized)
    }
}

pub(crate) async fn revoke(
    database: &DatabaseConnection,
    identity: &IdentityContext,
    now: i64,
) -> Result<(), AuthError> {
    let tx = db::begin(database).await?;
    let account = lock_account(&tx, identity.user_id).await?;
    if account.auth_revision != identity.auth_revision {
        return Err(AuthError::Unauthorized);
    }
    let current = lock_session(&tx, &identity.session_id).await?;
    validate_session(&current, account.auth_revision, account.disabled, now)?;
    if current.user_id != identity.user_id || current.authenticated_at != identity.authenticated_at
    {
        return Err(AuthError::Unauthorized);
    }
    let result = db::execute(
        &tx,
        "UPDATE neev_sessions SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL",
        vec![now.into(), identity.session_id.clone().into()],
    )
    .await?;
    if result.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    tx.commit().await.map_err(database_error)
}

pub(crate) async fn recheck(
    tx: &DatabaseTransaction,
    context: &OrgContext,
    now: i64,
) -> Result<(), AuthError> {
    let account = lock_account(tx, context.identity.user_id).await?;
    let organization = lock_organization(tx, context.org_id).await?;
    let membership = lock_membership(tx, context.org_id, context.identity.user_id).await?;
    let session = lock_session(tx, &context.identity.session_id).await?;
    validate_session(&session, account.auth_revision, account.disabled, now)?;
    if account.auth_revision != context.identity.auth_revision
        || organization.revision != context.org_revision
        || membership.id != context.member_id
        || membership.revision != context.member_revision
        || role(&membership.role)? != context.role
        || session.user_id != context.identity.user_id
        || session.org_id != Some(context.org_id)
        || session.member_id != Some(context.member_id)
        || session.member_revision != Some(context.member_revision)
        || session.org_revision != Some(context.org_revision)
        || session.authenticated_at != context.identity.authenticated_at
    {
        return Err(AuthError::Forbidden);
    }
    Ok(())
}

fn validate_session(
    session: &Session,
    current_revision: i64,
    disabled: bool,
    now: i64,
) -> Result<(), AuthError> {
    if disabled
        || session.revoked_at.is_some()
        || session.expires_at <= now
        || session.authenticated_at > now
        || session.account_revision != current_revision
    {
        return Err(AuthError::Unauthorized);
    }
    Ok(())
}

async fn lock_account(tx: &DatabaseTransaction, id: UserId) -> Result<Account, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,auth_revision,disabled FROM neev_accounts WHERE id=$1{}",
            lock(tx)
        ),
        vec![id.into()],
    )
    .await?
    .ok_or(AuthError::Unauthorized)?;
    let account = Account::from_query_result(&row, "").map_err(database_error)?;
    if account.id <= 0 || account.auth_revision < 1 || account.disabled {
        return Err(AuthError::Unauthorized);
    }
    Ok(account)
}

async fn lock_organization(
    tx: &DatabaseTransaction,
    id: OrgId,
) -> Result<OrganizationRow, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,revision,disabled FROM neev_organizations WHERE id=$1{}",
            lock(tx)
        ),
        vec![id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    let organization = OrganizationRow::from_query_result(&row, "").map_err(database_error)?;
    if organization.id <= 0 || organization.revision < 1 || organization.disabled {
        return Err(AuthError::Forbidden);
    }
    Ok(organization)
}

async fn lock_membership(
    tx: &DatabaseTransaction,
    org: OrgId,
    user: UserId,
) -> Result<Membership, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,revision,active,role FROM neev_memberships WHERE org_id=$1 AND user_id=$2{}",
            lock(tx)
        ),
        vec![org.into(), user.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    let membership = Membership::from_query_result(&row, "").map_err(database_error)?;
    if membership.id <= 0 || membership.revision < 1 || !membership.active {
        return Err(AuthError::Forbidden);
    }
    role(&membership.role)?;
    Ok(membership)
}

async fn lock_session(tx: &DatabaseTransaction, id: &str) -> Result<Session, AuthError> {
    let row = db::one(tx, &format!("SELECT id,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at,revoked_at FROM neev_sessions WHERE id=$1{}", lock(tx)), vec![id.into()]).await?.ok_or(AuthError::Unauthorized)?;
    Session::from_query_result(&row, "").map_err(database_error)
}

fn lock(tx: &DatabaseTransaction) -> &'static str {
    if tx.get_database_backend() == DbBackend::Postgres {
        " FOR UPDATE"
    } else {
        ""
    }
}

fn role(value: &str) -> Result<Role, AuthError> {
    match value {
        "admin" => Ok(Role::Admin),
        "member" => Ok(Role::Member),
        _ => Err(AuthError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;

    #[tokio::test]
    async fn identity_session_expires_and_requires_its_bound_csrf() {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "support@neev.so").await;
        let issued = issue_identity(&database, user, 1000).await.unwrap();
        assert_eq!(issued.expires_at, 2800);
        let context = identity(&database, &issued.session, 1001).await.unwrap();
        assert_eq!(context.user_id, user);
        assert_eq!(context.authenticated_at, 1000);
        assert_eq!(
            validate_csrf(&database, &context, &issued.csrf, 1001).await,
            Ok(())
        );
        assert_eq!(
            validate_csrf(
                &database,
                &context,
                &secrets::random_token(TokenPurpose::Csrf).unwrap(),
                1001
            )
            .await,
            Err(AuthError::Unauthorized)
        );
        assert_eq!(
            organization(&database, &issued.session, 1001).await.err(),
            Some(AuthError::Forbidden)
        );
        assert_eq!(
            identity(&database, &issued.session, 2800).await.err(),
            Some(AuthError::Unauthorized)
        );
    }

    #[tokio::test]
    async fn organization_selection_rotates_identity_session_and_preserves_proof_time() {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "support@neev.so").await;
        let (org, member) = h::organization(&database, user, "admin").await;
        let issued = issue_identity(&database, user, 1000).await.unwrap();
        let identity_context = identity(&database, &issued.session, 1001).await.unwrap();
        let scoped = select_organization(&database, &identity_context, org, 1100)
            .await
            .unwrap();
        assert_eq!(scoped.expires_at, 44300);
        assert_ne!(scoped.session, issued.session);
        assert_eq!(
            identity(&database, &issued.session, 1100).await.err(),
            Some(AuthError::Unauthorized)
        );
        let context = organization(&database, &scoped.session, 1100)
            .await
            .unwrap();
        assert_eq!(context.identity.authenticated_at, 1000);
        assert_eq!(context.org_id, org);
        assert_eq!(context.member_id, member);
        assert_eq!(context.role, Role::Admin);
    }

    #[tokio::test]
    async fn revoked_or_changed_authority_is_denied_and_final_recheck_catches_it() {
        for mutation in [
            "UPDATE neev_memberships SET active=false,revision=revision+1",
            "UPDATE neev_memberships SET role='member',revision=revision+1",
            "UPDATE neev_organizations SET revision=revision+1",
        ] {
            let database = h::database().await;
            let user = h::native_account(&database, "owner", "support@neev.so").await;
            let (org, _) = h::organization(&database, user, "admin").await;
            let issued = issue_identity(&database, user, 1000).await.unwrap();
            let identity_context = identity(&database, &issued.session, 1001).await.unwrap();
            let scoped = select_organization(&database, &identity_context, org, 1002)
                .await
                .unwrap();
            let context = organization(&database, &scoped.session, 1003)
                .await
                .unwrap();
            h::execute(&database, mutation, vec![]).await;
            assert_eq!(
                organization(&database, &scoped.session, 1004).await.err(),
                Some(AuthError::Forbidden)
            );
            let tx = db::begin(&database).await.unwrap();
            assert_eq!(
                recheck(&tx, &context, 1004).await,
                Err(AuthError::Forbidden)
            );
        }
    }

    #[tokio::test]
    async fn logout_revokes_server_state() {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "support@neev.so").await;
        let issued = issue_identity(&database, user, 1000).await.unwrap();
        let context = identity(&database, &issued.session, 1001).await.unwrap();
        revoke(&database, &context, 1001).await.unwrap();
        assert_eq!(
            identity(&database, &issued.session, 1001).await.err(),
            Some(AuthError::Unauthorized)
        );
    }

    #[tokio::test]
    #[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
    async fn postgres_organization_rechecks_revoke_or_disable_after_lookup() {
        for disable_account in [false, true] {
            let (database, admin, schema) = h::postgres_database().await;
            let user = h::native_account(&database, "owner", "support@neev.so").await;
            let (org, _) = h::organization(&database, user, "admin").await;
            let issued = issue_identity(&database, user, 1000).await.unwrap();
            let identity_context = identity(&database, &issued.session, 1001).await.unwrap();
            let scoped = select_organization(&database, &identity_context, org, 1002)
                .await
                .unwrap();
            let scoped_identity = identity(&database, &scoped.session, 1003).await.unwrap();
            let looked_up = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let task_database = database.clone();
            let task_token = scoped.session.clone();
            let task_looked_up = looked_up.clone();
            let task_resume = resume.clone();
            let task = tokio::spawn(async move {
                organization_after_lookup(&task_database, &task_token, 1004, move || async move {
                    task_looked_up.wait().await;
                    task_resume.wait().await;
                })
                .await
            });
            looked_up.wait().await;
            if disable_account {
                h::execute(
                    &database,
                    "UPDATE neev_accounts SET disabled=true WHERE id=$1",
                    vec![user.into()],
                )
                .await;
            } else {
                revoke(&database, &scoped_identity, 1004).await.unwrap();
            }
            resume.wait().await;
            assert_eq!(task.await.unwrap().err(), Some(AuthError::Unauthorized));
            h::cleanup_postgres(database, admin, schema).await;
        }
    }

    #[tokio::test]
    #[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
    async fn postgres_logout_waits_for_in_flight_final_recheck() {
        let (database, admin, schema) = h::postgres_database().await;
        let user = h::native_account(&database, "owner", "support@neev.so").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let issued = issue_identity(&database, user, 1000).await.unwrap();
        let identity_context = identity(&database, &issued.session, 1001).await.unwrap();
        let scoped = select_organization(&database, &identity_context, org, 1002)
            .await
            .unwrap();
        let context = organization(&database, &scoped.session, 1003)
            .await
            .unwrap();
        let tx = db::begin(&database).await.unwrap();
        recheck(&tx, &context, 1003).await.unwrap();
        let other_database = database.clone();
        let revoke_context = context.identity.clone();
        let task =
            tokio::spawn(async move { revoke(&other_database, &revoke_context, 1004).await });
        h::wait_for_postgres_lock(&admin, "%neev_accounts%FOR UPDATE%").await;
        tx.commit().await.unwrap();
        assert_eq!(task.await.unwrap(), Ok(()));
        assert_eq!(
            organization(&database, &scoped.session, 1005).await.err(),
            Some(AuthError::Unauthorized)
        );
        h::cleanup_postgres(database, admin, schema).await;
    }
}
