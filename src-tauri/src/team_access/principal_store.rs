use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::{Expr, LockType},
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait,
    FromQueryResult, QueryFilter, QuerySelect, Statement, TransactionTrait,
};

use super::AuthContext;
use crate::db::entities::{organization_membership, team_session};
use crate::team_access::{
    core::{database_error, TeamAuthError, TeamClock},
    github::{valid_login, GithubMembershipEvidence},
    session::{LocalPrincipal, TeamRole},
};

#[derive(Clone, Copy)]
pub(super) enum CredentialRequirement {
    ActiveOrRefreshing,
    Active,
}

#[derive(Clone)]
pub(super) struct Authority {
    pub(super) session_id: String,
    pub(super) csrf_hash: String,
    pub(super) user_id: i32,
    pub(super) github_user_id: i64,
    pub(super) org_id: i32,
    pub(super) github_org_id: i64,
    pub(super) org_login: String,
    pub(super) membership_id: i32,
    pub(super) role: TeamRole,
    pub(super) authorization_revision: i64,
    pub(super) credential_generation: i64,
    pub(super) verified_at: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct AuthorityRow {
    session_id: String,
    csrf_hash: String,
    session_user_id: i32,
    session_org_id: i32,
    session_membership_id: i32,
    session_authorization_revision: i64,
    session_credential_generation: i64,
    session_created_at: DateTime<Utc>,
    session_expires_at: DateTime<Utc>,
    membership_id: i32,
    membership_user_id: i32,
    membership_org_id: i32,
    membership_role: String,
    membership_state: String,
    membership_authorization_revision: i64,
    membership_credential_generation: i64,
    membership_verified_at: Option<DateTime<Utc>>,
    account_id: i32,
    github_user_id: i64,
    organization_id: i32,
    github_org_id: i64,
    organization_login: String,
    credential_user_id: i32,
    credential_github_user_id: i64,
    credential_generation: i64,
    credential_state: String,
    credential_payload: Option<String>,
    credential_refresh_owner: Option<String>,
    credential_refresh_expires_at: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct MembershipAuthorityRow {
    membership_id: i32,
    membership_user_id: i32,
    membership_org_id: i32,
    membership_role: String,
    membership_state: String,
    membership_authorization_revision: i64,
    membership_credential_generation: i64,
    account_id: i32,
    github_user_id: i64,
    organization_id: i32,
    github_org_id: i64,
    credential_user_id: i32,
    credential_github_user_id: i64,
    credential_generation: i64,
    credential_state: String,
    credential_payload: Option<String>,
    credential_refresh_owner: Option<String>,
    credential_refresh_expires_at: Option<DateTime<Utc>>,
}

pub(super) async fn load_for_local<C: ConnectionTrait>(
    db: &C,
    local: &LocalPrincipal,
    credential_requirement: CredentialRequirement,
    clock: &dyn TeamClock,
) -> Result<Authority, TeamAuthError> {
    let backend = db.get_database_backend();
    let placeholder = placeholder(backend);
    let sql = format!(
        "SELECT \
            session.token_hash AS session_id, session.csrf_hash AS csrf_hash, \
            session.user_id AS session_user_id, session.org_id AS session_org_id, \
            session.membership_id AS session_membership_id, \
            session.authorization_revision AS session_authorization_revision, \
            session.credential_generation AS session_credential_generation, \
            session.created_at AS session_created_at, session.expires_at AS session_expires_at, \
            membership.id AS membership_id, membership.user_id AS membership_user_id, \
            membership.org_id AS membership_org_id, membership.role AS membership_role, \
            membership.state AS membership_state, \
            membership.authorization_revision AS membership_authorization_revision, \
            membership.credential_generation AS membership_credential_generation, \
            membership.verified_at AS membership_verified_at, \
            account.id AS account_id, account.github_user_id AS github_user_id, \
            organization.id AS organization_id, organization.github_org_id AS github_org_id, \
            organization.login AS organization_login, \
            credential.user_id AS credential_user_id, \
            credential.github_user_id AS credential_github_user_id, \
            credential.generation AS credential_generation, credential.state AS credential_state, \
            credential.encrypted_payload AS credential_payload, \
            credential.refresh_owner AS credential_refresh_owner, \
            credential.refresh_expires_at AS credential_refresh_expires_at \
         FROM team_session AS session \
         INNER JOIN organization_membership AS membership ON membership.id = session.membership_id \
         INNER JOIN users AS account ON account.id = session.user_id \
         INNER JOIN orgs AS organization ON organization.id = session.org_id \
         INNER JOIN team_github_credential AS credential ON credential.user_id = session.user_id \
         WHERE session.token_hash = {placeholder}"
    );
    let row = db
        .query_one(Statement::from_sql_and_values(
            backend,
            sql,
            [local.session_id.clone().into()],
        ))
        .await
        .map_err(|error| database_error(&error))?
        .ok_or(TeamAuthError::Unauthorized)?;
    let row = AuthorityRow::from_query_result(&row, "").map_err(|_| TeamAuthError::Unavailable)?;
    validate_authority_row(row, local, credential_requirement, clock.now())
}

pub(super) async fn persist_evidence(
    db: &DatabaseConnection,
    authority: &Authority,
    evidence: &GithubMembershipEvidence,
    proof_started_at: DateTime<Utc>,
    clock: &dyn TeamClock,
) -> Result<(), TeamAuthError> {
    if evidence.github_user_id != authority.github_user_id
        || evidence.organization.github_org_id != authority.github_org_id
    {
        return Err(TeamAuthError::Unauthorized);
    }
    let transaction = db.begin().await.map_err(|error| database_error(&error))?;
    lock_membership(&transaction, authority.membership_id).await?;
    let current = load_membership_authority(&transaction, authority.membership_id).await?;
    validate_membership_authority(&current, authority)?;
    ensure_fresh_proof(proof_started_at, clock.now())?;

    let next_revision = if current.membership_role != role_name(evidence.organization.role) {
        Some(
            current
                .membership_authorization_revision
                .checked_add(1)
                .ok_or(TeamAuthError::Unavailable)?,
        )
    } else {
        None
    };
    let mut update = organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::VerifiedAt,
            Expr::value(Some(proof_started_at)),
        )
        .col_expr(
            organization_membership::Column::UpdatedAt,
            Expr::value(clock.now()),
        )
        .filter(organization_membership::Column::Id.eq(authority.membership_id))
        .filter(organization_membership::Column::UserId.eq(authority.user_id))
        .filter(organization_membership::Column::OrgId.eq(authority.org_id))
        .filter(organization_membership::Column::State.eq("active"))
        .filter(
            organization_membership::Column::AuthorizationRevision
                .eq(authority.authorization_revision),
        )
        .filter(
            organization_membership::Column::CredentialGeneration
                .eq(authority.credential_generation),
        );
    if let Some(next_revision) = next_revision {
        update = update
            .col_expr(
                organization_membership::Column::Role,
                Expr::value(role_name(evidence.organization.role)),
            )
            .col_expr(
                organization_membership::Column::AuthorizationRevision,
                Expr::value(next_revision),
            )
            .filter(
                organization_membership::Column::Role.eq(current.membership_role.clone()),
            );
    } else {
        update = update.filter(
            organization_membership::Column::Role.eq(current.membership_role.clone()),
        );
    }
    let changed = update
        .exec(&transaction)
        .await
        .map_err(|error| database_error(&error))?;
    if changed.rows_affected != 1 {
        return cas_failure(&transaction, authority).await;
    }
    ensure_fresh_proof(proof_started_at, clock.now())?;
    transaction.commit().await.map_err(|error| database_error(&error))
}

pub(super) async fn revoke_not_member(
    db: &DatabaseConnection,
    authority: &Authority,
    _proof_started_at: DateTime<Utc>,
    clock: &dyn TeamClock,
) -> Result<(), TeamAuthError> {
    let transaction = db.begin().await.map_err(|error| database_error(&error))?;
    lock_membership(&transaction, authority.membership_id).await?;
    let current = load_membership_authority(&transaction, authority.membership_id).await?;
    validate_membership_authority(&current, authority)?;
    let next_revision = authority
        .authorization_revision
        .checked_add(1)
        .ok_or(TeamAuthError::Unavailable)?;
    let changed = organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::State,
            Expr::value("revoked"),
        )
        .col_expr(
            organization_membership::Column::AuthorizationRevision,
            Expr::value(next_revision),
        )
        .col_expr(
            organization_membership::Column::VerifiedAt,
            Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            organization_membership::Column::UpdatedAt,
            Expr::value(clock.now()),
        )
        .filter(organization_membership::Column::Id.eq(authority.membership_id))
        .filter(organization_membership::Column::UserId.eq(authority.user_id))
        .filter(organization_membership::Column::OrgId.eq(authority.org_id))
        .filter(organization_membership::Column::State.eq("active"))
        .filter(
            organization_membership::Column::AuthorizationRevision
                .eq(authority.authorization_revision),
        )
        .filter(
            organization_membership::Column::CredentialGeneration
                .eq(authority.credential_generation),
        )
        .exec(&transaction)
        .await
        .map_err(|error| database_error(&error))?;
    if changed.rows_affected != 1 {
        return cas_failure(&transaction, authority).await;
    }
    transaction.commit().await.map_err(|error| database_error(&error))
}

pub(super) async fn recheck_context(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    clock: &dyn TeamClock,
) -> Result<(), TeamAuthError> {
    lock_membership(transaction, context.membership_id).await?;
    lock_session(transaction, &context.session_id).await?;
    let local = LocalPrincipal {
        session_id: context.session_id.clone(),
        user_id: context.user_id,
        org_id: context.org_id,
        membership_id: context.membership_id,
        role: context.role,
        authorization_revision: context.authorization_revision,
        credential_generation: context.credential_generation,
        verified_at: Some(context.verified_at),
    };
    let authority = load_for_local(
        transaction,
        &local,
        CredentialRequirement::Active,
        clock,
    )
    .await?;
    if authority.csrf_hash != context.csrf_hash {
        return Err(TeamAuthError::Unauthorized);
    }
    ensure_fresh_proof(
        authority.verified_at.ok_or(TeamAuthError::Unauthorized)?,
        clock.now(),
    )
}

fn validate_authority_row(
    row: AuthorityRow,
    local: &LocalPrincipal,
    credential_requirement: CredentialRequirement,
    now: DateTime<Utc>,
) -> Result<Authority, TeamAuthError> {
    let role = parse_role(&row.membership_role)?;
    if row.session_id != local.session_id
        || row.session_user_id != local.user_id
        || row.session_org_id != local.org_id
        || row.session_membership_id != local.membership_id
        || row.session_authorization_revision != local.authorization_revision
        || row.session_credential_generation != local.credential_generation
        || role != local.role
        || row.session_user_id <= 0
        || row.session_org_id <= 0
        || row.session_membership_id <= 0
        || row.account_id != row.session_user_id
        || row.membership_id != row.session_membership_id
        || row.membership_user_id != row.session_user_id
        || row.membership_org_id != row.session_org_id
        || row.organization_id != row.session_org_id
        || row.credential_user_id != row.session_user_id
        || row.github_user_id <= 0
        || row.github_org_id <= 0
        || row.credential_github_user_id != row.github_user_id
        || row.membership_state != "active"
        || row.membership_authorization_revision < 0
        || row.membership_authorization_revision != row.session_authorization_revision
        || row.credential_generation <= 0
        || row.membership_credential_generation != row.session_credential_generation
        || row.credential_generation != row.session_credential_generation
        || row.session_created_at > now
        || now >= row.session_expires_at
        || !valid_hash(&row.csrf_hash)
        || !valid_login(&row.organization_login)
        || !valid_credential_shape(&row, credential_requirement)
    {
        return Err(TeamAuthError::Unauthorized);
    }
    Ok(Authority {
        session_id: row.session_id,
        csrf_hash: row.csrf_hash,
        user_id: row.session_user_id,
        github_user_id: row.github_user_id,
        org_id: row.session_org_id,
        github_org_id: row.github_org_id,
        org_login: row.organization_login,
        membership_id: row.session_membership_id,
        role,
        authorization_revision: row.session_authorization_revision,
        credential_generation: row.session_credential_generation,
        verified_at: row.membership_verified_at,
    })
}

fn valid_credential_shape(row: &AuthorityRow, requirement: CredentialRequirement) -> bool {
    match row.credential_state.as_str() {
        "active" => {
            row.credential_payload.is_some()
                && row.credential_refresh_owner.is_none()
                && row.credential_refresh_expires_at.is_none()
        }
        "refreshing" if matches!(requirement, CredentialRequirement::ActiveOrRefreshing) => {
            row.credential_payload.is_some()
                && row
                    .credential_refresh_owner
                    .as_deref()
                    .is_some_and(|owner| !owner.is_empty())
                && row.credential_refresh_expires_at.is_some()
        }
        _ => false,
    }
}

async fn lock_membership(
    transaction: &DatabaseTransaction,
    membership_id: i32,
) -> Result<(), TeamAuthError> {
    if membership_id <= 0 {
        return Err(TeamAuthError::Unauthorized);
    }
    match transaction.get_database_backend() {
        DbBackend::Postgres => organization_membership::Entity::find_by_id(membership_id)
            .select_only()
            .column(organization_membership::Column::Id)
            .lock(LockType::Update)
            .into_tuple::<i32>()
            .one(transaction)
            .await
            .map_err(|error| database_error(&error))?
            .map(|_| ())
            .ok_or(TeamAuthError::Unauthorized),
        DbBackend::Sqlite => {
            let changed = organization_membership::Entity::update_many()
                .col_expr(
                    organization_membership::Column::UpdatedAt,
                    Expr::col(organization_membership::Column::UpdatedAt).into(),
                )
                .filter(organization_membership::Column::Id.eq(membership_id))
                .exec(transaction)
                .await
                .map_err(|error| database_error(&error))?;
            if changed.rows_affected == 1 {
                Ok(())
            } else {
                Err(TeamAuthError::Unauthorized)
            }
        }
        _ => Err(TeamAuthError::Unavailable),
    }
}

async fn lock_session(
    transaction: &DatabaseTransaction,
    session_id: &str,
) -> Result<(), TeamAuthError> {
    if !valid_hash(session_id) {
        return Err(TeamAuthError::Unauthorized);
    }
    let query = team_session::Entity::find_by_id(session_id)
        .select_only()
        .column(team_session::Column::TokenHash);
    let found = match transaction.get_database_backend() {
        DbBackend::Postgres => query
            .lock(LockType::Update)
            .into_tuple::<String>()
            .one(transaction)
            .await,
        DbBackend::Sqlite => query.into_tuple::<String>().one(transaction).await,
        _ => return Err(TeamAuthError::Unavailable),
    }
    .map_err(|error| database_error(&error))?;
    found.map(|_| ()).ok_or(TeamAuthError::Unauthorized)
}

async fn load_membership_authority(
    transaction: &DatabaseTransaction,
    membership_id: i32,
) -> Result<MembershipAuthorityRow, TeamAuthError> {
    let backend = transaction.get_database_backend();
    let sql = format!(
        "SELECT membership.id AS membership_id, membership.user_id AS membership_user_id, \
            membership.org_id AS membership_org_id, membership.role AS membership_role, \
            membership.state AS membership_state, \
            membership.authorization_revision AS membership_authorization_revision, \
            membership.credential_generation AS membership_credential_generation, \
            account.id AS account_id, account.github_user_id AS github_user_id, \
            organization.id AS organization_id, organization.github_org_id AS github_org_id, \
            credential.user_id AS credential_user_id, \
            credential.github_user_id AS credential_github_user_id, \
            credential.generation AS credential_generation, credential.state AS credential_state, \
            credential.encrypted_payload AS credential_payload, \
            credential.refresh_owner AS credential_refresh_owner, \
            credential.refresh_expires_at AS credential_refresh_expires_at \
         FROM organization_membership AS membership \
         INNER JOIN users AS account ON account.id = membership.user_id \
         INNER JOIN orgs AS organization ON organization.id = membership.org_id \
         INNER JOIN team_github_credential AS credential ON credential.user_id = membership.user_id \
         WHERE membership.id = {}",
        placeholder(backend)
    );
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            backend,
            sql,
            [membership_id.into()],
        ))
        .await
        .map_err(|error| database_error(&error))?
        .ok_or(TeamAuthError::Unauthorized)?;
    MembershipAuthorityRow::from_query_result(&row, "").map_err(|_| TeamAuthError::Unavailable)
}

fn validate_membership_authority(
    row: &MembershipAuthorityRow,
    authority: &Authority,
) -> Result<(), TeamAuthError> {
    if row.membership_id != authority.membership_id
        || row.membership_user_id != authority.user_id
        || row.membership_org_id != authority.org_id
        || row.account_id != authority.user_id
        || row.organization_id != authority.org_id
        || row.github_user_id != authority.github_user_id
        || row.github_org_id != authority.github_org_id
        || row.credential_user_id != authority.user_id
        || row.credential_github_user_id != authority.github_user_id
        || row.credential_generation != authority.credential_generation
        || row.membership_credential_generation != authority.credential_generation
        || row.membership_authorization_revision != authority.authorization_revision
        || row.membership_role != role_name(authority.role)
        || row.membership_state != "active"
        || row.credential_state != "active"
        || row.credential_payload.is_none()
        || row.credential_refresh_owner.is_some()
        || row.credential_refresh_expires_at.is_some()
    {
        return Err(TeamAuthError::Unauthorized);
    }
    Ok(())
}

async fn cas_failure(
    transaction: &DatabaseTransaction,
    authority: &Authority,
) -> Result<(), TeamAuthError> {
    let current = load_membership_authority(transaction, authority.membership_id).await?;
    if validate_membership_authority(&current, authority).is_err() {
        Err(TeamAuthError::Unauthorized)
    } else {
        Err(TeamAuthError::Conflict)
    }
}

fn ensure_fresh_proof(
    proof_started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    if proof_started_at > now
        || now.signed_duration_since(proof_started_at) >= chrono::Duration::seconds(60)
    {
        Err(TeamAuthError::Unauthorized)
    } else {
        Ok(())
    }
}

fn parse_role(role: &str) -> Result<TeamRole, TeamAuthError> {
    match role {
        "admin" => Ok(TeamRole::Admin),
        "member" => Ok(TeamRole::Member),
        _ => Err(TeamAuthError::Unauthorized),
    }
}

fn role_name(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Admin => "admin",
        TeamRole::Member => "member",
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn placeholder(backend: DbBackend) -> &'static str {
    match backend {
        DbBackend::Postgres => "$1",
        DbBackend::MySql | DbBackend::Sqlite => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::{load_for_local, CredentialRequirement};
    use crate::team_access::{
        session::resolve_local_session,
        test_auth::{cookie_headers, fixed_now, principal_fixture, ALICE_A_TOKEN},
    };

    #[tokio::test]
    async fn a_newer_persisted_proof_is_not_mistaken_for_an_identity_change() {
        let fixture = principal_fixture(Vec::new()).await;
        let mut earlier_read = resolve_local_session(
            &fixture.db,
            &cookie_headers(ALICE_A_TOKEN),
            fixed_now(),
        )
        .await
        .unwrap();
        earlier_read.verified_at = Some(fixed_now() - chrono::Duration::seconds(30));

        let authority = load_for_local(
            &fixture.db,
            &earlier_read,
            CredentialRequirement::Active,
            fixture.clock.as_ref(),
        )
        .await
        .unwrap();

        assert_eq!(authority.verified_at, Some(fixed_now()));
    }
}
