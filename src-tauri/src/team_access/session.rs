use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult,
    QueryFilter, Statement,
};
use sha2::{Digest, Sha256};

use crate::db::entities::team_session;

const SESSION_COOKIE: &str = "workos_team_session";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TeamRole {
    Admin,
    Member,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum LocalSessionError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("session storage unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MembershipFreshness {
    Fresh,
    ReverificationRequired,
}

/// A locally consistent session identity, not a fully authorized request principal.
/// Callers must still require the later GitHub membership verifier and apply the
/// route and resource policy before granting access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalPrincipal {
    pub session_id: String,
    pub user_id: i32,
    pub org_id: i32,
    pub membership_id: i32,
    pub role: TeamRole,
    pub authorization_revision: i64,
    pub credential_generation: i64,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct IdentityRow {
    session_id: String,
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
}

fn validated_cookie(headers: &HeaderMap) -> Result<&str, LocalSessionError> {
    let mut session_cookie = None;
    for value in headers.get_all(axum::http::header::COOKIE) {
        let cookies = value
            .to_str()
            .map_err(|_| LocalSessionError::Unauthorized)?;
        for pair in cookies.split(';').map(str::trim) {
            let Some((name, value)) = pair.split_once('=') else {
                if pair == SESSION_COOKIE {
                    return Err(LocalSessionError::Unauthorized);
                }
                continue;
            };
            if name != SESSION_COOKIE {
                continue;
            }
            if session_cookie.is_some()
                || value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err(LocalSessionError::Unauthorized);
            }
            session_cookie = Some(value);
        }
    }
    session_cookie.ok_or(LocalSessionError::Unauthorized)
}

pub(crate) async fn resolve_local_session(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    now: DateTime<Utc>,
) -> Result<LocalPrincipal, LocalSessionError> {
    let raw_cookie = validated_cookie(headers)?;
    let digest = format!("{:x}", Sha256::digest(raw_cookie.as_bytes()));
    let backend = db.get_database_backend();
    let placeholder = match backend {
        DbBackend::Postgres => "$1",
        DbBackend::MySql | DbBackend::Sqlite => "?",
    };
    let sql = format!(
        "SELECT \
            session.token_hash AS session_id, \
            session.user_id AS session_user_id, \
            session.org_id AS session_org_id, \
            session.membership_id AS session_membership_id, \
            session.authorization_revision AS session_authorization_revision, \
            session.credential_generation AS session_credential_generation, \
            session.created_at AS session_created_at, \
            session.expires_at AS session_expires_at, \
            membership.id AS membership_id, \
            membership.user_id AS membership_user_id, \
            membership.org_id AS membership_org_id, \
            membership.role AS membership_role, \
            membership.state AS membership_state, \
            membership.authorization_revision AS membership_authorization_revision, \
            membership.credential_generation AS membership_credential_generation, \
            membership.verified_at AS membership_verified_at \
         FROM team_session AS session \
         INNER JOIN organization_membership AS membership \
            ON membership.id = session.membership_id \
         INNER JOIN users AS account ON account.id = session.user_id \
         INNER JOIN orgs AS organization ON organization.id = session.org_id \
         WHERE session.token_hash = {placeholder}"
    );
    let row = db
        .query_one(Statement::from_sql_and_values(
            backend,
            sql,
            [digest.clone().into()],
        ))
        .await
        .map_err(|_| LocalSessionError::Unavailable)?
        .ok_or(LocalSessionError::Unauthorized)?;
    let row = IdentityRow::from_query_result(&row, "")
        .map_err(|_| LocalSessionError::Unavailable)?;

    let role = match row.membership_role.as_str() {
        "admin" => TeamRole::Admin,
        "member" => TeamRole::Member,
        _ => return Err(LocalSessionError::Unauthorized),
    };
    if row.session_id != digest
        || row.session_membership_id != row.membership_id
        || row.session_user_id != row.membership_user_id
        || row.session_org_id != row.membership_org_id
        || row.membership_state != "active"
        || row.session_authorization_revision < 0
        || row.membership_authorization_revision < 0
        || row.session_authorization_revision != row.membership_authorization_revision
        || row.session_credential_generation < 0
        || row.membership_credential_generation < 0
        || row.session_credential_generation != row.membership_credential_generation
        || row.session_created_at > now
        || now >= row.session_expires_at
    {
        return Err(LocalSessionError::Unauthorized);
    }

    Ok(LocalPrincipal {
        session_id: row.session_id,
        user_id: row.session_user_id,
        org_id: row.session_org_id,
        membership_id: row.session_membership_id,
        role,
        authorization_revision: row.session_authorization_revision,
        credential_generation: row.session_credential_generation,
        verified_at: row.membership_verified_at,
    })
}

pub(crate) fn membership_freshness(
    principal: &LocalPrincipal,
    now: DateTime<Utc>,
) -> MembershipFreshness {
    match principal.verified_at {
        Some(at) if at <= now && now.signed_duration_since(at) < chrono::Duration::seconds(60) => {
            MembershipFreshness::Fresh
        }
        _ => MembershipFreshness::ReverificationRequired,
    }
}

/// Deletes only the checked local session. A route exposing this operation must add
/// CSRF validation and external verification/revocation notifications first.
pub(crate) async fn revoke_local_session(
    db: &DatabaseConnection,
    principal: &LocalPrincipal,
) -> Result<(), LocalSessionError> {
    team_session::Entity::delete_many()
        .filter(team_session::Column::TokenHash.eq(principal.session_id.clone()))
        .filter(team_session::Column::UserId.eq(principal.user_id))
        .filter(team_session::Column::OrgId.eq(principal.org_id))
        .filter(team_session::Column::MembershipId.eq(principal.membership_id))
        .exec(db)
        .await
        .map_err(|_| LocalSessionError::Unavailable)?;
    Ok(())
}
