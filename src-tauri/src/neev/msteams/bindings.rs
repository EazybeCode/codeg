//! Persisted Microsoft Teams bindings (schema 25), managed by Neev admins.
//!
//! - **Installation**: a Microsoft 365 tenant -> exactly one Neev org, with the
//!   agent a new Teams conversation starts on (Auto may still re-route a turn).
//! - **Project link**: a Team (the bot-reported `team.id`) -> a Neev project.
//!   Created only by a Neev admin who Graph confirms is an OWNER of that Team;
//!   the Team's Entra group id comes from the Bot Connector, never from input.
//! - **Channel link**: a channel of a linked Team that Neev answers in. Enabled
//!   only by a verified Team owner (and Neev admin) after Graph confirms a live
//!   standard channel with no guests or members from other tenants. Disabling
//!   needs no verification: turning Neev off must always work.
//! - **Principal**: an Entra object id -> a Neev member. Created only by
//!   [`link_principal`], whose caller has proved BOTH identities (a Neev
//!   session for the member and a verified Teams SSO token for the object id).
//!
//! Every write is admin-only (except a member linking or unlinking THEMSELF),
//! runs in one transaction under `resources::begin` (session recheck + global
//! admission guard), and is idempotent: repeating a connect returns what is
//! stored. A row owned by another org answers exactly like a row that does not
//! exist for this org's purposes (`Conflict` on create, `NotFound` on update),
//! so nothing about another org's binding is disclosed.

use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};

use super::access::graph_refusal;
use super::codes::Refusal;
use super::config::is_guid;
use super::directory::Directory;
use super::graph::Membership;
use crate::neev::content::{resources, types::ContentError};
use crate::neev::db;
use crate::neev::error::{database_error, AuthError};
use crate::neev::startup::ServerState;
use crate::neev::types::{OrgContext, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Installation {
    pub(crate) tenant_id: String,
    pub(crate) org_id: i32,
    pub(crate) enabled: bool,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectLink {
    pub(crate) tenant_id: String,
    pub(crate) team_id: String,
    pub(crate) graph_group_id: String,
    pub(crate) node_id: i32,
    pub(crate) enabled: bool,
    pub(crate) revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelLink {
    pub(crate) tenant_id: String,
    pub(crate) team_id: String,
    pub(crate) channel_id: String,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Principal {
    pub(crate) tenant_id: String,
    pub(crate) aad_object_id: String,
    pub(crate) user_id: i32,
}

/// Everything bound for the actor's org.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bindings {
    pub(crate) installations: Vec<Installation>,
    pub(crate) projects: Vec<ProjectLink>,
    pub(crate) channels: Vec<ChannelLink>,
    pub(crate) principals: Vec<Principal>,
}

/// A Teams conversation id (`19:…@thread.tacv2`, `19:…@thread.skype`): short,
/// no whitespace or control characters. Teams ids are opaque; nothing else is
/// assumed about their shape.
pub(crate) fn teams_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// A binding write that needed a Teams-side fact: either Neev refused it
/// (`Content`), or the Teams verification did (`Refused`, a stable code).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BindingError {
    Content(ContentError),
    Refused(Refusal),
}

impl From<ContentError> for BindingError {
    fn from(error: ContentError) -> Self {
        Self::Content(error)
    }
}

impl From<AuthError> for BindingError {
    fn from(error: AuthError) -> Self {
        Self::Content(error.into())
    }
}

fn admin(actor: &OrgContext) -> Result<(), ContentError> {
    if actor.role == Role::Admin {
        Ok(())
    } else {
        Err(ContentError::Auth(AuthError::Forbidden))
    }
}

fn tenant(value: &str) -> Result<String, ContentError> {
    let value = value.to_ascii_lowercase();
    if is_guid(&value) {
        Ok(value)
    } else {
        Err(ContentError::Invalid)
    }
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContentError> {
    row.try_get("", column).map_err(|error| database_error(error).into())
}

fn installation_row(row: &QueryResult) -> Result<Installation, ContentError> {
    Ok(Installation {
        tenant_id: get(row, "tenant_id")?,
        org_id: get(row, "org_id")?,
        enabled: get(row, "enabled")?,
        provider: get(row, "provider")?,
        model: get(row, "model")?,
        revision: get(row, "revision")?,
    })
}

const INSTALLATION_COLUMNS: &str = "tenant_id,org_id,enabled,provider,model,revision";

async fn installation_of<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
) -> Result<Option<Installation>, ContentError> {
    db::one(
        db,
        &format!("SELECT {INSTALLATION_COLUMNS} FROM neev_msteams_installations WHERE tenant_id=$1"),
        vec![tenant_id.into()],
    )
    .await?
    .map(|row| installation_row(&row))
    .transpose()
}

/// The actor org's installation for `tenant_id`, or `NotFound` (absent, or
/// another org's).
async fn own_installation(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    tenant_id: &str,
) -> Result<Installation, ContentError> {
    installation_of(tx, tenant_id)
        .await?
        .filter(|found| found.org_id == actor.org_id)
        .ok_or(ContentError::NotFound)
}

async fn commit(
    state: &ServerState,
    tx: DatabaseTransaction,
    actor: &OrgContext,
) -> Result<(), ContentError> {
    crate::neev::auth::session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(|error| database_error(error).into())
}

/// Connect a tenant to the actor's org (admin). Idempotent: connecting again
/// with the same agent returns the stored row unchanged; a different agent
/// updates it (revision + 1). A tenant already connected to ANOTHER org is a
/// `Conflict` — one tenant, one org.
pub(crate) async fn connect_tenant(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
    provider: &str,
    model: &str,
) -> Result<Installation, ContentError> {
    admin(actor)?;
    let tenant_id = tenant(tenant_id)?;
    if crate::neev::agents::types::Provider::parse_offered(provider).is_none()
        || !(model == "default" || crate::neev::agents::execution::safe_model(model))
    {
        return Err(ContentError::Invalid);
    }
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    let found = match installation_of(&tx, &tenant_id).await? {
        Some(existing) if existing.org_id != actor.org_id => return Err(ContentError::Conflict),
        Some(existing) if existing.provider == provider && existing.model == model => existing,
        Some(existing) => {
            db::execute(
                &tx,
                "UPDATE neev_msteams_installations SET provider=$1,model=$2,revision=revision+1,updated_at=$3 WHERE tenant_id=$4 AND org_id=$5 AND revision=$6",
                vec![
                    provider.into(),
                    model.into(),
                    now.into(),
                    tenant_id.clone().into(),
                    actor.org_id.into(),
                    existing.revision.into(),
                ],
            )
            .await?;
            own_installation(&tx, actor, &tenant_id).await?
        }
        None => {
            db::execute(
                &tx,
                "INSERT INTO neev_msteams_installations(tenant_id,org_id,enabled,provider,model,revision,created_by,created_at,updated_at) VALUES($1,$2,true,$3,$4,1,$5,$6,$6)",
                vec![
                    tenant_id.clone().into(),
                    actor.org_id.into(),
                    provider.into(),
                    model.into(),
                    actor.identity.user_id.into(),
                    now.into(),
                ],
            )
            .await?;
            own_installation(&tx, actor, &tenant_id).await?
        }
    };
    commit(state, tx, actor).await?;
    Ok(found)
}

/// Suspend or resume an installation (admin). Suspended: Neev answers nothing
/// from the tenant and sends nothing to it; every row and thread is kept.
pub(crate) async fn set_tenant_enabled(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
    enabled: bool,
) -> Result<Installation, ContentError> {
    admin(actor)?;
    let tenant_id = tenant(tenant_id)?;
    let tx = resources::begin(state, actor).await?;
    let existing = own_installation(&tx, actor, &tenant_id).await?;
    if existing.enabled != enabled {
        db::execute(
            &tx,
            "UPDATE neev_msteams_installations SET enabled=$1,revision=revision+1,updated_at=$2 WHERE tenant_id=$3 AND org_id=$4",
            vec![enabled.into(), state.clock.now_unix().into(), tenant_id.clone().into(), actor.org_id.into()],
        )
        .await?;
    }
    let found = own_installation(&tx, actor, &tenant_id).await?;
    commit(state, tx, actor).await?;
    Ok(found)
}

const PROJECT_COLUMNS: &str = "tenant_id,team_id,graph_group_id,node_id,enabled,revision";

fn project_row(row: &QueryResult) -> Result<ProjectLink, ContentError> {
    Ok(ProjectLink {
        tenant_id: get(row, "tenant_id")?,
        team_id: get(row, "team_id")?,
        graph_group_id: get(row, "graph_group_id")?,
        node_id: get(row, "node_id")?,
        enabled: get(row, "enabled")?,
        revision: get(row, "revision")?,
    })
}

async fn own_project(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    tenant_id: &str,
    team_id: &str,
) -> Result<Option<ProjectLink>, ContentError> {
    db::one(
        tx,
        &format!("SELECT {PROJECT_COLUMNS} FROM neev_msteams_project_links WHERE tenant_id=$1 AND team_id=$2 AND org_id=$3"),
        vec![tenant_id.into(), team_id.into(), actor.org_id.into()],
    )
    .await?
    .map(|row| project_row(&row))
    .transpose()
}

/// The Teams facts an admin binding write depends on, read BEFORE any
/// transaction (no admission guard is held across a network call): the
/// tenant's Bot Connector service URL and the ACTOR's own linked Entra id.
async fn admin_teams_identity(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
) -> Result<(String, String), BindingError> {
    let installation = installation_of(&state.database, tenant_id)
        .await?
        .filter(|found| found.org_id == actor.org_id)
        .ok_or(ContentError::NotFound)?;
    if !installation.enabled {
        return Err(ContentError::Archived.into());
    }
    let service_url: Option<String> = db::one(
        &state.database,
        "SELECT service_url FROM neev_msteams_installations WHERE tenant_id=$1 AND org_id=$2",
        vec![tenant_id.into(), actor.org_id.into()],
    )
    .await?
    .map(|row| get(&row, "service_url"))
    .transpose()?
    .flatten();
    // Neev learns the service URL only from a signed Bot Connector activity;
    // until the app has been installed and heard from, nothing can be verified.
    let service_url = service_url.ok_or(BindingError::Refused(Refusal::TeamsUnverifiable))?;
    let object_id: String = db::one(
        &state.database,
        "SELECT aad_object_id FROM neev_msteams_principals WHERE tenant_id=$1 AND user_id=$2 AND org_id=$3",
        vec![tenant_id.into(), actor.identity.user_id.into(), actor.org_id.into()],
    )
    .await?
    .map(|row| get(&row, "aad_object_id"))
    .transpose()?
    .ok_or(BindingError::Refused(Refusal::UnmappedUser))?;
    Ok((service_url, object_id))
}

/// The actor's linked Entra id is an OWNER of the Team (Graph, fresh).
async fn require_owner(
    directory: &Directory,
    tenant_id: &str,
    group_id: &str,
    object_id: &str,
) -> Result<(), BindingError> {
    match directory.membership(tenant_id, group_id, object_id).await {
        Ok(Membership::Owner) => Ok(()),
        Ok(_) => Err(BindingError::Refused(Refusal::NotTeamOwner)),
        Err(error) => Err(BindingError::Refused(graph_refusal(error))),
    }
}

/// Link a Team of a connected tenant to a project of the actor's org. The
/// actor must be a Neev admin AND, per Graph, an owner of that Team through
/// their own linked Teams identity; Neev admin alone is not proof of Team
/// management. The Team's Entra group id is resolved from the Bot Connector.
/// The node must be a live (not trashed) PROJECT of the org. Idempotent for the
/// same project; naming a different project relinks (revision + 1) — later
/// threads file there, earlier threads keep their sessions where they are.
pub(crate) async fn link_team(
    state: &ServerState,
    directory: &Directory,
    actor: &OrgContext,
    tenant_id: &str,
    team_id: &str,
    node_id: i32,
) -> Result<ProjectLink, BindingError> {
    admin(actor)?;
    let tenant_id = tenant(tenant_id)?;
    if !teams_id(team_id) || node_id <= 0 {
        return Err(ContentError::Invalid.into());
    }
    let (service_url, owner) = admin_teams_identity(state, actor, &tenant_id).await?;
    let group_id = directory
        .team_group_id(&service_url, team_id)
        .await
        .map_err(|error| BindingError::Refused(graph_refusal(error)))?;
    require_owner(directory, &tenant_id, &group_id, &owner).await?;
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    own_installation(&tx, actor, &tenant_id).await?;
    // The identity Graph verified must still be the actor's own link.
    let still_linked = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_principals WHERE tenant_id=$1 AND user_id=$2 AND org_id=$3 AND aad_object_id=$4",
        vec![tenant_id.clone().into(), actor.identity.user_id.into(), actor.org_id.into(), owner.clone().into()],
    )
    .await?;
    if still_linked.is_none() {
        return Err(ContentError::Conflict.into());
    }
    let kind: Option<String> = db::one(
        &tx,
        "SELECT kind FROM neev_nodes WHERE org_id=$1 AND id=$2",
        vec![actor.org_id.into(), node_id.into()],
    )
    .await?
    .map(|row| get(&row, "kind"))
    .transpose()?;
    if kind.as_deref() != Some("project")
        || crate::neev::catalog::trash::node_trashed(&tx, actor.org_id, node_id).await?
    {
        return Err(ContentError::NotFound.into());
    }
    // Another Team row already claiming this group (any org) means the ids do
    // not describe one Team: refuse rather than guess.
    let group_taken = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_project_links WHERE tenant_id=$1 AND graph_group_id=$2 AND team_id<>$3",
        vec![tenant_id.clone().into(), group_id.clone().into(), team_id.into()],
    )
    .await?;
    if group_taken.is_some() {
        return Err(ContentError::Conflict.into());
    }
    let other_org = db::one(
        &tx,
        "SELECT 1 AS present FROM neev_msteams_project_links WHERE tenant_id=$1 AND team_id=$2 AND org_id<>$3",
        vec![tenant_id.clone().into(), team_id.into(), actor.org_id.into()],
    )
    .await?;
    if other_org.is_some() {
        return Err(ContentError::Conflict.into());
    }
    match own_project(&tx, actor, &tenant_id, team_id).await? {
        Some(existing) if existing.node_id == node_id && existing.graph_group_id == group_id => {
            db::execute(
                &tx,
                "UPDATE neev_msteams_project_links SET verified_owner=$1,verified_at=$2 WHERE tenant_id=$3 AND team_id=$4 AND org_id=$5",
                vec![owner.clone().into(), now.into(), tenant_id.clone().into(), team_id.into(), actor.org_id.into()],
            )
            .await?;
        }
        Some(existing) => {
            db::execute(
                &tx,
                "UPDATE neev_msteams_project_links SET node_id=$1,graph_group_id=$2,verified_owner=$3,verified_at=$4,revision=revision+1,updated_at=$4 WHERE tenant_id=$5 AND team_id=$6 AND org_id=$7 AND revision=$8",
                vec![node_id.into(), group_id.clone().into(), owner.clone().into(), now.into(), tenant_id.clone().into(), team_id.into(), actor.org_id.into(), existing.revision.into()],
            )
            .await?;
        }
        None => {
            db::execute(
                &tx,
                "INSERT INTO neev_msteams_project_links(org_id,tenant_id,team_id,graph_group_id,node_id,enabled,verified_owner,verified_at,revision,created_by,created_at,updated_at) VALUES($1,$2,$3,$4,$5,true,$6,$7,1,$8,$7,$7)",
                vec![actor.org_id.into(), tenant_id.clone().into(), team_id.into(), group_id.clone().into(), node_id.into(), owner.clone().into(), now.into(), actor.identity.user_id.into()],
            )
            .await?;
        }
    }
    let found = own_project(&tx, actor, &tenant_id, team_id)
        .await?
        .ok_or(ContentError::Storage)?;
    commit(state, tx, actor).await?;
    Ok(found)
}

/// Suspend or resume a Team link (admin).
pub(crate) async fn set_team_enabled(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
    team_id: &str,
    enabled: bool,
) -> Result<ProjectLink, ContentError> {
    admin(actor)?;
    let tenant_id = tenant(tenant_id)?;
    let tx = resources::begin(state, actor).await?;
    let existing = own_project(&tx, actor, &tenant_id, team_id)
        .await?
        .ok_or(ContentError::NotFound)?;
    if existing.enabled != enabled {
        db::execute(
            &tx,
            "UPDATE neev_msteams_project_links SET enabled=$1,revision=revision+1,updated_at=$2 WHERE tenant_id=$3 AND team_id=$4 AND org_id=$5",
            vec![enabled.into(), state.clock.now_unix().into(), tenant_id.clone().into(), team_id.into(), actor.org_id.into()],
        )
        .await?;
    }
    let found = own_project(&tx, actor, &tenant_id, team_id)
        .await?
        .ok_or(ContentError::Storage)?;
    commit(state, tx, actor).await?;
    Ok(found)
}

/// Enable or disable Neev in one channel of a linked Team. Upsert: the first
/// call creates the row. ENABLING requires a Neev admin who is a verified Team
/// owner and a channel Graph shows is live, standard and free of guests and
/// members from other tenants (Graph answers 404 for a channel that is not in
/// this Team). DISABLING needs only a Neev admin. A channel id already bound
/// under a DIFFERENT Team or org is a `Conflict`.
pub(crate) async fn set_channel(
    state: &ServerState,
    directory: &Directory,
    actor: &OrgContext,
    tenant_id: &str,
    team_id: &str,
    channel_id: &str,
    enabled: bool,
) -> Result<ChannelLink, BindingError> {
    admin(actor)?;
    let tenant_id = tenant(tenant_id)?;
    if !teams_id(team_id) || !teams_id(channel_id) {
        return Err(ContentError::Invalid.into());
    }
    let verified_group = if enabled {
        let (_, owner) = admin_teams_identity(state, actor, &tenant_id).await?;
        let group_id: String = db::one(
            &state.database,
            "SELECT graph_group_id FROM neev_msteams_project_links WHERE tenant_id=$1 AND team_id=$2 AND org_id=$3",
            vec![tenant_id.clone().into(), team_id.into(), actor.org_id.into()],
        )
        .await?
        .map(|row| get(&row, "graph_group_id"))
        .transpose()?
        .ok_or(ContentError::NotFound)?;
        require_owner(directory, &tenant_id, &group_id, &owner).await?;
        let audience = directory
            .channel_audience(&tenant_id, &group_id, channel_id, true)
            .await
            .map_err(|error| {
                BindingError::Refused(match error {
                    super::graph::GraphError::NotFound => Refusal::UnmappedChannel,
                    other => graph_refusal(other),
                })
            })?;
        if !audience.pilot_eligible() {
            return Err(BindingError::Refused(Refusal::ChannelNotEligible));
        }
        Some(group_id)
    } else {
        None
    };
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    let project = own_project(&tx, actor, &tenant_id, team_id)
        .await?
        .ok_or(ContentError::NotFound)?;
    if verified_group.as_deref().is_some_and(|group| group != project.graph_group_id) {
        // The Team was relinked while Graph was being asked: verify again.
        return Err(ContentError::Conflict.into());
    }
    let existing = db::one(
        &tx,
        "SELECT org_id,team_id,enabled FROM neev_msteams_channel_links WHERE tenant_id=$1 AND channel_id=$2",
        vec![tenant_id.clone().into(), channel_id.into()],
    )
    .await?;
    match existing {
        Some(row) => {
            let org: i32 = get(&row, "org_id")?;
            let team: String = get(&row, "team_id")?;
            if org != actor.org_id || team != team_id {
                return Err(ContentError::Conflict.into());
            }
            let was: bool = get(&row, "enabled")?;
            if was != enabled || enabled {
                db::execute(
                    &tx,
                    if enabled {
                        "UPDATE neev_msteams_channel_links SET enabled=$1,updated_at=$2,verified_at=$2 WHERE tenant_id=$3 AND channel_id=$4 AND org_id=$5"
                    } else {
                        "UPDATE neev_msteams_channel_links SET enabled=$1,updated_at=$2 WHERE tenant_id=$3 AND channel_id=$4 AND org_id=$5"
                    },
                    vec![enabled.into(), now.into(), tenant_id.clone().into(), channel_id.into(), actor.org_id.into()],
                )
                .await?;
            }
        }
        // Disabling a channel that was never enabled stores nothing.
        None if !enabled => {}
        None => {
            db::execute(
                &tx,
                "INSERT INTO neev_msteams_channel_links(org_id,tenant_id,team_id,channel_id,enabled,verified_at,created_by,created_at,updated_at) VALUES($1,$2,$3,$4,true,$5,$6,$5,$5)",
                vec![actor.org_id.into(), tenant_id.clone().into(), team_id.into(), channel_id.into(), now.into(), actor.identity.user_id.into()],
            )
            .await?;
        }
    }
    commit(state, tx, actor).await?;
    Ok(ChannelLink {
        tenant_id,
        team_id: team_id.to_owned(),
        channel_id: channel_id.to_owned(),
        enabled,
    })
}

/// Remember the Bot Connector service URL a SIGNED activity from this
/// connected tenant named (the caller verified the token and the host). The
/// admin binding checks need it to ask the Bot Connector about a Team.
pub(crate) async fn record_service_url<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
    service_url: &str,
    now: i64,
) -> Result<(), ContentError> {
    if service_url.len() < 9 || service_url.len() > 512 {
        return Err(ContentError::Invalid);
    }
    db::execute(
        db,
        "UPDATE neev_msteams_installations SET service_url=$1,updated_at=$2 WHERE tenant_id=$3 AND (service_url IS NULL OR service_url<>$1)",
        vec![service_url.into(), now.into(), tenant_id.to_ascii_lowercase().into()],
    )
    .await?;
    Ok(())
}

/// Teams said (signed activity) the app was removed from a Team, or the Team
/// was deleted or archived: suspend the Team link. Nothing is deleted; an
/// admin re-enables it deliberately.
pub(crate) async fn suspend_team<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
    team_id: &str,
    now: i64,
) -> Result<u64, ContentError> {
    Ok(db::execute(
        db,
        "UPDATE neev_msteams_project_links SET enabled=false,revision=revision+1,updated_at=$1 WHERE tenant_id=$2 AND team_id=$3 AND enabled=true",
        vec![now.into(), tenant_id.to_ascii_lowercase().into(), team_id.into()],
    )
    .await?
    .rows_affected())
}

/// Teams said (signed activity) a channel was deleted: disable its link.
pub(crate) async fn suspend_channel<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
    channel_id: &str,
    now: i64,
) -> Result<u64, ContentError> {
    Ok(db::execute(
        db,
        "UPDATE neev_msteams_channel_links SET enabled=false,updated_at=$1 WHERE tenant_id=$2 AND channel_id=$3 AND enabled=true",
        vec![now.into(), tenant_id.to_ascii_lowercase().into(), channel_id.into()],
    )
    .await?
    .rows_affected())
}

/// Link the ACTOR's own Neev account to the Entra object id a verified Teams
/// SSO token named (`tenant_id`, `aad_object_id` come from that token, never
/// from a request body). The tenant must be connected to the actor's org.
/// Idempotent; a different pairing for either side is a `Conflict` — one
/// Teams identity per Neev member per tenant, both ways (two people acting as
/// one account is impersonation).
pub(crate) async fn link_principal(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
    aad_object_id: &str,
) -> Result<Principal, ContentError> {
    // Guests and external accounts are refused by the caller from the token
    // (`EntraUser::external`) before this is reached.
    let tenant_id = tenant(tenant_id)?;
    let aad_object_id = aad_object_id.to_ascii_lowercase();
    if !is_guid(&aad_object_id) {
        return Err(ContentError::Invalid);
    }
    let tx = resources::begin(state, actor).await?;
    let installation = own_installation(&tx, actor, &tenant_id).await?;
    if !installation.enabled {
        return Err(ContentError::Archived);
    }
    let by_object = db::one(
        &tx,
        "SELECT user_id FROM neev_msteams_principals WHERE tenant_id=$1 AND aad_object_id=$2",
        vec![tenant_id.clone().into(), aad_object_id.clone().into()],
    )
    .await?
    .map(|row| get::<i32>(&row, "user_id"))
    .transpose()?;
    let by_user = db::one(
        &tx,
        "SELECT aad_object_id FROM neev_msteams_principals WHERE tenant_id=$1 AND user_id=$2",
        vec![tenant_id.clone().into(), actor.identity.user_id.into()],
    )
    .await?
    .map(|row| get::<String>(&row, "aad_object_id"))
    .transpose()?;
    match (by_object, by_user) {
        (Some(user), Some(object)) if user == actor.identity.user_id && object == aad_object_id => {}
        (None, None) => {
            db::execute(
                &tx,
                "INSERT INTO neev_msteams_principals(org_id,tenant_id,aad_object_id,user_id,created_by,created_at) VALUES($1,$2,$3,$4,$4,$5)",
                vec![actor.org_id.into(), tenant_id.clone().into(), aad_object_id.clone().into(), actor.identity.user_id.into(), state.clock.now_unix().into()],
            )
            .await?;
        }
        _ => return Err(ContentError::Conflict),
    }
    commit(state, tx, actor).await?;
    Ok(Principal {
        tenant_id,
        aad_object_id,
        user_id: actor.identity.user_id,
    })
}

/// Remove a principal link: a member may unlink themself; an admin anyone in
/// their org. Unlinking stops every later Teams request from that identity.
pub(crate) async fn unlink_principal(
    state: &ServerState,
    actor: &OrgContext,
    tenant_id: &str,
    user_id: i32,
) -> Result<(), ContentError> {
    if user_id != actor.identity.user_id {
        admin(actor)?;
    }
    let tenant_id = tenant(tenant_id)?;
    let tx = resources::begin(state, actor).await?;
    let removed = db::execute(
        &tx,
        "DELETE FROM neev_msteams_principals WHERE tenant_id=$1 AND user_id=$2 AND org_id=$3",
        vec![tenant_id.into(), user_id.into(), actor.org_id.into()],
    )
    .await?;
    if removed.rows_affected() != 1 {
        return Err(ContentError::NotFound);
    }
    commit(state, tx, actor).await
}

/// Everything bound for the actor's org (admin).
pub(crate) async fn list(state: &ServerState, actor: &OrgContext) -> Result<Bindings, ContentError> {
    admin(actor)?;
    let tx = resources::begin(state, actor).await?;
    let org = vec![actor.org_id.into()];
    let mut out = Bindings::default();
    for row in tx
        .query_all(db::statement(&tx, &format!("SELECT {INSTALLATION_COLUMNS} FROM neev_msteams_installations WHERE org_id=$1 ORDER BY tenant_id"), org.clone()))
        .await
        .map_err(database_error)?
    {
        out.installations.push(installation_row(&row)?);
    }
    for row in tx
        .query_all(db::statement(&tx, &format!("SELECT {PROJECT_COLUMNS} FROM neev_msteams_project_links WHERE org_id=$1 ORDER BY tenant_id,team_id"), org.clone()))
        .await
        .map_err(database_error)?
    {
        out.projects.push(project_row(&row)?);
    }
    for row in tx
        .query_all(db::statement(&tx, "SELECT tenant_id,team_id,channel_id,enabled FROM neev_msteams_channel_links WHERE org_id=$1 ORDER BY tenant_id,channel_id", org.clone()))
        .await
        .map_err(database_error)?
    {
        out.channels.push(ChannelLink {
            tenant_id: get(&row, "tenant_id")?,
            team_id: get(&row, "team_id")?,
            channel_id: get(&row, "channel_id")?,
            enabled: get(&row, "enabled")?,
        });
    }
    for row in tx
        .query_all(db::statement(&tx, "SELECT tenant_id,aad_object_id,user_id FROM neev_msteams_principals WHERE org_id=$1 ORDER BY tenant_id,user_id", org))
        .await
        .map_err(database_error)?
    {
        out.principals.push(Principal {
            tenant_id: get(&row, "tenant_id")?,
            aad_object_id: get(&row, "aad_object_id")?,
            user_id: get(&row, "user_id")?,
        });
    }
    commit(state, tx, actor).await?;
    Ok(out)
}
