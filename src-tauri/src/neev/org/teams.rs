//! The member directory and admin-managed teams (schema 24).
//!
//! Every active member may see who else is in the organization (name, avatar,
//! role — never an email) and which teams exist, because choosing whom to share
//! with needs both. Only an org admin creates, renames or deletes a team or
//! changes who is in it. Deleting a team ends every grant made to it.

use crate::neev::{
    auth::session,
    content::{
        grants::SharingError,
        resources,
        types::ContentError,
    },
    db,
    error::{database_error, AuthError},
    startup::ServerState,
    types::{OrgContext, Role},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(crate) const MAX_TEAMS_PER_ORG: i64 = 200;
pub(crate) const MAX_MEMBERS_PER_TEAM: i64 = 1000;
const MAX_QUERY_CHARS: usize = 100;
const CREATE_ACTION: &str = "org.team.create";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryMember {
    pub user_id: i32,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryPage {
    pub members: Vec<DirectoryMember>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Team {
    pub id: String,
    pub name: String,
    pub member_count: i64,
    pub revision: i64,
    pub can_manage: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamMember {
    pub user_id: i32,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamMembers {
    pub team_id: String,
    pub revision: i64,
    pub members: Vec<TeamMember>,
}

fn now(state: &ServerState) -> Result<i64, ContentError> {
    let now = state.clock.now_unix();
    if now < 0 {
        Err(ContentError::Invalid)
    } else {
        Ok(now)
    }
}

/// `%`, `_` and the escape itself match literally.
fn like_pattern(query: &str) -> String {
    let mut out = String::from("%");
    for character in query.to_lowercase().chars() {
        if matches!(character, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('%');
    out
}

/// Active members by display name then id, optionally filtered by a
/// case-insensitive name substring. The cursor is bound to the org and query.
pub(crate) async fn directory(
    state: &ServerState,
    actor: &OrgContext,
    query: Option<&str>,
    limit: u32,
    after: Option<&str>,
) -> Result<DirectoryPage, ContentError> {
    let query = query.map(str::trim).filter(|query| !query.is_empty());
    if !(1..=100).contains(&limit)
        || query.is_some_and(|query| {
            query.chars().count() > MAX_QUERY_CHARS || query.chars().any(char::is_control)
        })
    {
        return Err(ContentError::Invalid);
    }
    let cursor: Option<(String, i32)> = match after {
        None => None,
        Some(cursor) => {
            if cursor.len() > 1024 {
                return Err(ContentError::Invalid);
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| ContentError::Invalid)?;
            let (org, bound_query, name, user): (i32, Option<String>, String, i32) =
                serde_json::from_slice(&decoded).map_err(|_| ContentError::Invalid)?;
            if org != actor.org_id || bound_query.as_deref() != query {
                return Err(ContentError::Invalid);
            }
            Some((name, user))
        }
    };
    let tx = resources::begin(state, actor).await?;
    let mut binds: Vec<sea_orm::Value> = vec![actor.org_id.into()];
    let mut sql = String::from(
        "SELECT m.user_id,m.role,a.display_name,a.avatar_url FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.active=true AND a.disabled=false",
    );
    if let Some(query) = query {
        binds.push(like_pattern(query).into());
        sql.push_str(&format!(" AND lower(a.display_name) LIKE ${} ESCAPE '\\'", binds.len()));
    }
    if let Some((name, user)) = &cursor {
        binds.push(name.clone().into());
        let name_at = binds.len();
        binds.push((*user).into());
        sql.push_str(&format!(
            " AND (a.display_name>${name_at} OR (a.display_name=${name_at} AND m.user_id>${}))",
            binds.len()
        ));
    }
    binds.push(i64::from(limit + 1).into());
    sql.push_str(&format!(" ORDER BY a.display_name,m.user_id LIMIT ${}", binds.len()));
    let rows = tx
        .query_all(db::statement(&tx, &sql, binds))
        .await
        .map_err(database_error)?;
    let mut members = Vec::with_capacity(rows.len());
    for row in rows.iter().take(limit as usize) {
        members.push(DirectoryMember {
            user_id: row.try_get("", "user_id").map_err(database_error)?,
            display_name: row.try_get("", "display_name").map_err(database_error)?,
            avatar_url: row.try_get("", "avatar_url").map_err(database_error)?,
            role: session::role(&row.try_get::<String>("", "role").map_err(database_error)?)?,
        });
    }
    let next_after = if rows.len() > limit as usize {
        let last = members.last().ok_or(ContentError::Storage)?;
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&(actor.org_id, query, &last.display_name, last.user_id))
                    .map_err(|_| ContentError::Storage)?,
            ),
        )
    } else {
        None
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(DirectoryPage { members, next_after })
}

/// A team name: trimmed, 1–80 characters, no control characters. The key is
/// its case-folded form, which is what uniqueness is decided on.
fn team_name(value: &str) -> Result<(String, String), ContentError> {
    let value = value.trim();
    if value.chars().any(char::is_control) || !(1..=80).contains(&value.chars().count()) {
        return Err(ContentError::Invalid);
    }
    Ok((value.to_owned(), value.to_lowercase()))
}

fn admin(actor: &OrgContext) -> Result<(), ContentError> {
    if actor.role == Role::Admin {
        Ok(())
    } else {
        Err(ContentError::Auth(AuthError::Forbidden))
    }
}

async fn load_team(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    locked: bool,
) -> Result<Team, ContentError> {
    crate::neev::content::access::canonical_id(id)?;
    // The count is a scalar subquery so only the team row is locked.
    let row = db::one(
        tx,
        &format!(
            "SELECT t.id,t.name,t.revision,(SELECT COUNT(*) FROM neev_team_members tm WHERE tm.org_id=t.org_id AND tm.team_id=t.id) AS members FROM neev_teams t WHERE t.org_id=$1 AND t.id=$2{}",
            if locked { session::lock(tx) } else { "" }
        ),
        vec![actor.org_id.into(), id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    Ok(Team {
        id: row.try_get("", "id").map_err(database_error)?,
        name: row.try_get("", "name").map_err(database_error)?,
        member_count: row.try_get("", "members").map_err(database_error)?,
        revision: row.try_get("", "revision").map_err(database_error)?,
        can_manage: actor.role == Role::Admin,
    })
}

async fn name_taken(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    key: &str,
    except: Option<&str>,
) -> Result<bool, ContentError> {
    Ok(db::one(
        tx,
        "SELECT id FROM neev_teams WHERE org_id=$1 AND name_key=$2",
        vec![actor.org_id.into(), key.into()],
    )
    .await?
    .map(|row| row.try_get::<String>("", "id").map_err(database_error))
    .transpose()?
    .is_some_and(|id| Some(id.as_str()) != except))
}

/// Every team of the organization, by name. Any member.
pub(crate) async fn list(state: &ServerState, actor: &OrgContext) -> Result<Vec<Team>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            &format!("SELECT t.id,t.name,t.revision,(SELECT COUNT(*) FROM neev_team_members tm WHERE tm.org_id=t.org_id AND tm.team_id=t.id) AS members FROM neev_teams t WHERE t.org_id=$1 ORDER BY t.name_key,t.id LIMIT {MAX_TEAMS_PER_ORG}"),
            vec![actor.org_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut teams = Vec::with_capacity(rows.len());
    for row in &rows {
        teams.push(Team {
            id: row.try_get("", "id").map_err(database_error)?,
            name: row.try_get("", "name").map_err(database_error)?,
            member_count: row.try_get("", "members").map_err(database_error)?,
            revision: row.try_get("", "revision").map_err(database_error)?,
            can_manage: actor.role == Role::Admin,
        });
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(teams)
}

/// Admin only. A retried `request_id` returns the same team; the same id with
/// a different name is a conflict.
pub(crate) async fn create(
    state: &ServerState,
    actor: &OrgContext,
    name: &str,
    request_id: &str,
) -> Result<Team, SharingError> {
    let (name, key) = team_name(name)?;
    let request = super::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    admin(actor)?;
    let now = now(state)?;
    let hash = super::digest_parts(&[CREATE_ACTION, &actor.org_id.to_string(), &name]);
    let tx = resources::begin(state, actor).await?;
    // Serializes team creation in the org, so the cap and the name check hold.
    db::one(
        &tx,
        &format!("SELECT id FROM neev_organizations WHERE id=$1{}", session::lock(&tx)),
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    if let Some(id) = resources::receipt(&tx, actor, CREATE_ACTION, &request, &hash).await? {
        let team = load_team(&tx, actor, &id, false).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(team);
    }
    if name_taken(&tx, actor, &key, None).await? {
        return Err(SharingError::NameTaken);
    }
    let count: i64 = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_teams WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?
    .try_get("", "n")
    .map_err(database_error)?;
    if count >= MAX_TEAMS_PER_ORG {
        return Err(ContentError::Quota.into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_teams(org_id,id,name,name_key,created_by,revision,created_at,updated_at) VALUES($1,$2,$3,$4,$5,1,$6,$6)",
        vec![actor.org_id.into(), id.clone().into(), name.into(), key.into(), actor.identity.user_id.into(), now.into()],
    )
    .await?;
    resources::record_receipt(&tx, actor, CREATE_ACTION, &request, &hash, &id).await?;
    let team = load_team(&tx, actor, &id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(team)
}

/// Lock the team, prove the caller is an admin and the revision is current.
async fn guarded(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    expected_revision: i64,
) -> Result<Team, ContentError> {
    if expected_revision <= 0 {
        return Err(ContentError::Invalid);
    }
    let team = load_team(tx, actor, id, true).await?;
    admin(actor)?;
    if team.revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    Ok(team)
}

async fn bump(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    team: &Team,
    now: i64,
) -> Result<(), ContentError> {
    let next = team.revision.checked_add(1).ok_or(ContentError::Conflict)?;
    let bumped = db::execute(
        tx,
        "UPDATE neev_teams SET revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4 AND revision=$5",
        vec![next.into(), now.into(), actor.org_id.into(), team.id.clone().into(), team.revision.into()],
    )
    .await?;
    if bumped.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    Ok(())
}

pub(crate) async fn rename(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    name: &str,
    expected_revision: i64,
) -> Result<Team, SharingError> {
    let (name, key) = team_name(name)?;
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let team = guarded(&tx, actor, id, expected_revision).await?;
    if name_taken(&tx, actor, &key, Some(id)).await? {
        return Err(SharingError::NameTaken);
    }
    bump(&tx, actor, &team, now).await?;
    db::execute(
        &tx,
        "UPDATE neev_teams SET name=$1,name_key=$2 WHERE org_id=$3 AND id=$4",
        vec![name.into(), key.into(), actor.org_id.into(), id.into()],
    )
    .await?;
    let team = load_team(&tx, actor, id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(team)
}

/// Delete a team, every grant made to it, and its membership list.
pub(crate) async fn delete(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    expected_revision: i64,
) -> Result<(), ContentError> {
    let tx = resources::begin(state, actor).await?;
    guarded(&tx, actor, id, expected_revision).await?;
    db::execute(
        &tx,
        "DELETE FROM neev_grants WHERE org_id=$1 AND principal_kind='team' AND principal_id=$2",
        vec![actor.org_id.into(), id.into()],
    )
    .await?;
    db::execute(
        &tx,
        "DELETE FROM neev_teams WHERE org_id=$1 AND id=$2",
        vec![actor.org_id.into(), id.into()],
    )
    .await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

async fn members_view(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
) -> Result<TeamMembers, ContentError> {
    let team = load_team(tx, actor, id, false).await?;
    let rows = tx
        .query_all(db::statement(
            tx,
            &format!("SELECT tm.user_id,a.display_name,a.avatar_url FROM neev_team_members tm JOIN neev_memberships m ON m.org_id=tm.org_id AND m.user_id=tm.user_id AND m.active=true JOIN neev_accounts a ON a.id=tm.user_id WHERE tm.org_id=$1 AND tm.team_id=$2 ORDER BY a.display_name,tm.user_id LIMIT {MAX_MEMBERS_PER_TEAM}"),
            vec![actor.org_id.into(), id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut members = Vec::with_capacity(rows.len());
    for row in &rows {
        members.push(TeamMember {
            user_id: row.try_get("", "user_id").map_err(database_error)?,
            display_name: row.try_get("", "display_name").map_err(database_error)?,
            avatar_url: row.try_get("", "avatar_url").map_err(database_error)?,
        });
    }
    Ok(TeamMembers { team_id: team.id, revision: team.revision, members })
}

/// Who is in a team. Any member.
pub(crate) async fn members(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<TeamMembers, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let result = members_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Admin only: add (`true`) or remove (`false`) an active member. Adding
/// someone already in it, or removing someone who is not, changes nothing.
pub(crate) async fn set_member(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    user: i32,
    expected_revision: i64,
    present: bool,
) -> Result<TeamMembers, ContentError> {
    if user <= 0 {
        return Err(ContentError::NotFound);
    }
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let team = guarded(&tx, actor, id, expected_revision).await?;
    let already = db::one(
        &tx,
        "SELECT 1 AS hit FROM neev_team_members WHERE org_id=$1 AND team_id=$2 AND user_id=$3",
        vec![actor.org_id.into(), id.into(), user.into()],
    )
    .await?
    .is_some();
    if present && !already {
        db::one(
            &tx,
            "SELECT 1 AS hit FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.user_id=$2 AND m.active=true AND a.disabled=false",
            vec![actor.org_id.into(), user.into()],
        )
        .await?
        .ok_or(ContentError::NotFound)?;
        if team.member_count >= MAX_MEMBERS_PER_TEAM {
            return Err(ContentError::Quota);
        }
        bump(&tx, actor, &team, now).await?;
        db::execute(
            &tx,
            "INSERT INTO neev_team_members(org_id,team_id,user_id,added_by,added_at) VALUES($1,$2,$3,$4,$5)",
            vec![actor.org_id.into(), id.into(), user.into(), actor.identity.user_id.into(), now.into()],
        )
        .await?;
    } else if !present && already {
        bump(&tx, actor, &team, now).await?;
        db::execute(
            &tx,
            "DELETE FROM neev_team_members WHERE org_id=$1 AND team_id=$2 AND user_id=$3",
            vec![actor.org_id.into(), id.into(), user.into()],
        )
        .await?;
    }
    let result = members_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}
