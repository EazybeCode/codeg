//! What a verified Teams activity may reach, from the persisted bindings
//! (`bindings`). Fail closed: an absent row, a suspended row, a Team id that
//! does not match the channel's link, or a storage error is a refusal. A
//! refusal names what an operator must change, never a project or org.
//!
//! These resolve WHERE a request may go and WHO sent it. The Teams half of the
//! authorization is [`teams_member`] (fresh per request, through the Graph
//! authority: Team membership, not a guest, same tenant, eligible channel);
//! the Neev half is the caller running as the member through a delegated org
//! session (`auth::session::issue_delegated`), whose recheck refuses a removed
//! or disabled membership, and filing the session under the project through
//! `sessions::create`, which applies the member's own folder access. BOTH
//! must pass; neither is a fallback for the other.

use sea_orm::{ConnectionTrait, QueryResult};

use super::codes::Refusal;
use super::directory::Directory;
use super::graph::{GraphError, Membership};
use crate::neev::db;

/// A connected tenant and the agent new Teams conversations start on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TenantAccess {
    pub(crate) tenant_id: String,
    pub(crate) org_id: i32,
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// An enabled channel of an enabled Team link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelAccess {
    pub(crate) tenant: TenantAccess,
    pub(crate) team_id: String,
    /// The Team's Entra group id, verified when the Team was linked.
    pub(crate) graph_group_id: String,
    pub(crate) channel_id: String,
    pub(crate) node_id: i32,
}

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, Refusal> {
    row.try_get("", column).map_err(|_| Refusal::Unavailable)
}

/// The tenant's installation, enabled.
pub(crate) async fn tenant<C: ConnectionTrait>(db: &C, tenant_id: &str) -> Result<TenantAccess, Refusal> {
    let row = db::one(
        db,
        "SELECT org_id,enabled,provider,model FROM neev_msteams_installations WHERE tenant_id=$1",
        vec![tenant_id.to_ascii_lowercase().into()],
    )
    .await
    .map_err(|_| Refusal::Unavailable)?
    .ok_or(Refusal::UnmappedTenant)?;
    if !get::<bool>(&row, "enabled")? {
        return Err(Refusal::Suspended);
    }
    Ok(TenantAccess {
        tenant_id: tenant_id.to_ascii_lowercase(),
        org_id: get(&row, "org_id")?,
        provider: get(&row, "provider")?,
        model: get(&row, "model")?,
    })
}

/// The channel's binding. `reported_team` is the `channelData.team.id` Teams
/// sent; when present it must be the Team the channel was linked under.
pub(crate) async fn channel<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
    reported_team: Option<&str>,
    channel_id: &str,
) -> Result<ChannelAccess, Refusal> {
    let tenant = tenant(db, tenant_id).await?;
    let row = db::one(
        db,
        "SELECT c.team_id,c.enabled AS channel_enabled,p.enabled AS team_enabled,p.node_id,p.graph_group_id FROM neev_msteams_channel_links c JOIN neev_msteams_project_links p ON p.tenant_id=c.tenant_id AND p.team_id=c.team_id AND p.org_id=c.org_id WHERE c.tenant_id=$1 AND c.channel_id=$2 AND c.org_id=$3",
        vec![tenant_id.to_ascii_lowercase().into(), channel_id.into(), tenant.org_id.into()],
    )
    .await
    .map_err(|_| Refusal::Unavailable)?
    .ok_or(Refusal::UnmappedChannel)?;
    let team_id: String = get(&row, "team_id")?;
    if reported_team.is_some_and(|reported| reported != team_id) {
        return Err(Refusal::UnmappedChannel);
    }
    if !get::<bool>(&row, "team_enabled")? || !get::<bool>(&row, "channel_enabled")? {
        return Err(Refusal::Suspended);
    }
    Ok(ChannelAccess {
        tenant,
        team_id,
        graph_group_id: get(&row, "graph_group_id")?,
        channel_id: channel_id.to_owned(),
        node_id: get(&row, "node_id")?,
    })
}

/// How a Graph failure reads to the person: unverifiable is never "allowed".
pub(crate) fn graph_refusal(error: GraphError) -> Refusal {
    match error {
        GraphError::Unavailable => Refusal::Unavailable,
        GraphError::Disabled | GraphError::Denied | GraphError::NotFound => Refusal::TeamsUnverifiable,
    }
}

/// The channel may receive Neev output now: a live standard channel with no
/// guests and no members from other tenants (Graph, briefly cached).
pub(crate) async fn channel_audience(directory: &Directory, access: &ChannelAccess, fresh: bool) -> Result<(), Refusal> {
    let audience = directory
        .channel_audience(&access_tenant(access), &access.graph_group_id, &access.channel_id, fresh)
        .await
        .map_err(|error| match error {
            // The channel is gone (deleted) or no longer visible to the app.
            GraphError::NotFound => Refusal::UnmappedChannel,
            other => graph_refusal(other),
        })?;
    if audience.pilot_eligible() {
        Ok(())
    } else {
        Err(Refusal::ChannelNotEligible)
    }
}

fn access_tenant(access: &ChannelAccess) -> String {
    access.tenant.tenant_id.clone()
}

/// The Teams half of authorizing `object_id` in this channel, fresh: the
/// channel's audience is eligible, and the person is an in-tenant, non-guest
/// member of the Team (in a standard channel, Team membership IS channel
/// membership). Every unknown is a refusal.
pub(crate) async fn teams_member(
    directory: &Directory,
    access: &ChannelAccess,
    object_id: Option<&str>,
    fresh: bool,
) -> Result<Membership, Refusal> {
    let object_id = object_id.ok_or(Refusal::GuestSender)?;
    channel_audience(directory, access, fresh).await?;
    let standing = directory
        .membership(&access_tenant(access), &access.graph_group_id, object_id)
        .await
        .map_err(graph_refusal)?;
    match standing {
        Membership::Owner | Membership::Member => Ok(standing),
        Membership::Guest | Membership::External => Err(Refusal::GuestSender),
        Membership::Absent => Err(Refusal::NotATeamMember),
    }
}

/// The Neev account an Entra object id was explicitly linked to in this
/// tenant's org. Guests (no object id) are never linked.
pub(crate) async fn principal<C: ConnectionTrait>(
    db: &C,
    tenant_id: &str,
    org_id: i32,
    aad_object_id: Option<&str>,
) -> Result<i32, Refusal> {
    let object = aad_object_id.ok_or(Refusal::GuestSender)?;
    let row = db::one(
        db,
        "SELECT user_id FROM neev_msteams_principals WHERE tenant_id=$1 AND aad_object_id=$2 AND org_id=$3",
        vec![tenant_id.to_ascii_lowercase().into(), object.to_ascii_lowercase().into(), org_id.into()],
    )
    .await
    .map_err(|_| Refusal::Unavailable)?
    .ok_or(Refusal::UnmappedUser)?;
    get(&row, "user_id")
}
