//! Teams-side authorization facts, behind one interface so the HTTP flow can
//! be tested end to end with a recording fake and run in production against
//! the real services:
//!
//! - the Team's Entra group id, from the Bot Connector's team details
//!   (`GET {serviceUrl}/v3/teams/{teamId}`, the bot's own token) — the bot
//!   `team.id` and the Graph group id are different identifiers, and only this
//!   answer (or Graph) ties them together;
//! - a person's standing in the Team and the channel's audience, from the
//!   separate Graph authority (`graph`).
//!
//! Every error is a refusal for the caller. Nothing here is cached except a
//! channel's audience, briefly, because membership changes must take effect on
//! the next message while a channel's type cannot change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::bot::Connector;
use super::graph::{ChannelAudience, GraphAuthority, GraphError, Membership};

/// How long a channel's audience answer is reused. Guests added to a channel
/// are noticed within this window; type changes cannot happen at all.
const AUDIENCE_TTL: Duration = Duration::from_secs(120);

pub(crate) struct LiveDirectory {
    connector: Arc<Connector>,
    graph: Option<Arc<GraphAuthority>>,
    audiences: Mutex<HashMap<(String, String, String), (ChannelAudience, Instant)>>,
}

impl LiveDirectory {
    pub(crate) fn new(connector: Arc<Connector>, graph: Option<Arc<GraphAuthority>>) -> Self {
        Self {
            connector,
            graph,
            audiences: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Directory {
    Live(Arc<LiveDirectory>),
    #[cfg(test)]
    Fake(Arc<FakeDirectory>),
}

impl Directory {
    /// Whether Graph facts can be verified at all (the ops/status surfaces say
    /// so; every Graph-dependent call still fails closed on its own).
    pub(crate) fn graph_configured(&self) -> bool {
        match self {
            Self::Live(live) => live.graph.is_some(),
            #[cfg(test)]
            Self::Fake(fake) => fake.graph_configured,
        }
    }

    /// The Entra group id of the Team whose bot id is `team_id`.
    pub(crate) async fn team_group_id(&self, service_url: &str, team_id: &str) -> Result<String, GraphError> {
        match self {
            Self::Live(live) => live
                .connector
                .team_group_id(service_url, team_id)
                .await
                .map_err(|_| GraphError::Unavailable),
            #[cfg(test)]
            Self::Fake(fake) => {
                fake.calls.lock().await.push(format!("team:{team_id}"));
                fake.groups.lock().await.get(team_id).cloned().ok_or(GraphError::NotFound)
            }
        }
    }

    pub(crate) async fn membership(&self, tenant_id: &str, group_id: &str, object_id: &str) -> Result<Membership, GraphError> {
        match self {
            Self::Live(live) => {
                live.graph
                    .as_ref()
                    .ok_or(GraphError::Disabled)?
                    .membership(tenant_id, group_id, object_id)
                    .await
            }
            #[cfg(test)]
            Self::Fake(fake) => {
                fake.calls.lock().await.push(format!("member:{object_id}"));
                if !fake.graph_configured {
                    return Err(GraphError::Disabled);
                }
                if let Some(error) = *fake.fail.lock().await {
                    return Err(error);
                }
                Ok(fake
                    .members
                    .lock()
                    .await
                    .get(&(group_id.to_owned(), object_id.to_owned()))
                    .copied()
                    .unwrap_or(Membership::Absent))
            }
        }
    }

    /// `fresh` bypasses the short cache: used right before anything is
    /// PUBLISHED, so a guest added a moment ago is seen.
    pub(crate) async fn channel_audience(&self, tenant_id: &str, group_id: &str, channel_id: &str, fresh: bool) -> Result<ChannelAudience, GraphError> {
        match self {
            Self::Live(live) => {
                let graph = live.graph.as_ref().ok_or(GraphError::Disabled)?;
                let key = (tenant_id.to_owned(), group_id.to_owned(), channel_id.to_owned());
                if !fresh {
                    if let Some((audience, at)) = live.audiences.lock().await.get(&key) {
                        if at.elapsed() < AUDIENCE_TTL {
                            return Ok(audience.clone());
                        }
                    }
                }
                let audience = graph.channel_audience(tenant_id, group_id, channel_id).await?;
                live.audiences.lock().await.insert(key, (audience.clone(), Instant::now()));
                Ok(audience)
            }
            #[cfg(test)]
            Self::Fake(fake) => {
                fake.calls.lock().await.push(format!("channel:{channel_id}:{fresh}"));
                if !fake.graph_configured {
                    return Err(GraphError::Disabled);
                }
                if let Some(error) = *fake.fail.lock().await {
                    return Err(error);
                }
                fake.channels.lock().await.get(channel_id).cloned().ok_or(GraphError::NotFound)
            }
        }
    }
}

/// A recording, programmable directory for tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeDirectory {
    pub(crate) graph_configured: bool,
    pub(crate) groups: Mutex<HashMap<String, String>>,
    pub(crate) members: Mutex<HashMap<(String, String), Membership>>,
    pub(crate) channels: Mutex<HashMap<String, ChannelAudience>>,
    pub(crate) fail: Mutex<Option<GraphError>>,
    pub(crate) calls: Mutex<Vec<String>>,
}

#[cfg(test)]
impl FakeDirectory {
    pub(crate) fn standard() -> ChannelAudience {
        ChannelAudience {
            membership_type: "standard".to_owned(),
            archived: false,
            guests: Some(0),
            external_members: Some(false),
        }
    }
}
