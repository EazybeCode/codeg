//! Microsoft Teams as a Neev client: a single-tenant bot (personal chat and
//! channel mentions) and a tab SSO status probe.
//!
//! Off unless `NEEV_TEAMS_ENABLED=true` with a complete configuration
//! (`config`); when off, no Teams route is mounted and no worker runs. Teams
//! never gains an access path of its own: every request is verified (`jwt`,
//! `keys`), normalized (`activity`), resolved through admin-managed database
//! bindings (`bindings`, `access`), authorized on the Teams side through a
//! separate Graph authority (`graph`, `directory`; fail closed when it is not
//! configured) AND on the Neev side as the linked member, committed to a
//! durable inbox before it is acknowledged (`inbox`, `threads`), and then run by
//! the worker (`worker`) through the same saved-session turn launcher the web
//! chat uses (`http::launch_session_turn`). Answers are delivered from a
//! durable outbox (`outbox`). Channel threads are shared between members
//! (`threads`). Setup: `docs/operations/microsoft-teams.md`.

use std::sync::{Arc, OnceLock};

pub(crate) mod access;
pub(crate) mod activity;
pub(crate) mod bindings;
pub(crate) mod bot;
pub(crate) mod cards;
pub(crate) mod codes;
pub(crate) mod config;
pub(crate) mod directory;
pub(crate) mod documents;
// Release 4 transport for channel SharePoint files (filesFolder discovery,
// inherited-permission folders, create-only multipart upload sessions,
// reconciliation reads). Not an authorization boundary: `documents` decides
// what may be published, as whom, and where.
pub(crate) mod files_policy;
pub(crate) mod graph;
pub(crate) mod graph_files;
pub(crate) mod inbox;
pub(crate) mod jwt;
pub(crate) mod keys;
pub(crate) mod outbox;
pub(crate) mod sources;
pub(crate) mod threads;
pub(crate) mod worker;
#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod tests;

/// The delegated scope the Teams tab requests (`api://…/access_as_user`).
pub(crate) const TAB_SCOPE: &str = "access_as_user";

/// Process-wide clients: the two signing-key caches and the outbound
/// connector. Built on first use from the (immutable) startup config, so the
/// key caches and the bot token are shared by every request.
pub(crate) struct TeamsRuntime {
    pub(crate) connector_keys: keys::JwksCache,
    pub(crate) entra_keys: keys::JwksCache,
    pub(crate) connector: Arc<bot::Connector>,
    pub(crate) directory: directory::Directory,
    /// Wakes the worker after a message is committed to the inbox.
    pub(crate) wake: tokio::sync::Notify,
    /// Channel file publication, only when explicitly authorized
    /// (`NEEV_TEAMS_GRAPH_FILES=true`); `None` keeps documents in Neev.
    pub(crate) files: Option<documents::FilesAuthority>,
}

static RUNTIME: OnceLock<Arc<TeamsRuntime>> = OnceLock::new();

/// `None` only when an HTTP client cannot be built; callers answer 503.
pub(crate) fn runtime(config: &config::TeamsConfig) -> Option<Arc<TeamsRuntime>> {
    if let Some(runtime) = RUNTIME.get() {
        return Some(runtime.clone());
    }
    let connector = Arc::new(bot::Connector::new(config).ok()?);
    let graph = match &config.graph {
        Some(graph) => Some(Arc::new(graph::GraphAuthority::new(graph).ok()?)),
        None => None,
    };
    let files = match (&config.graph, &graph) {
        (Some(settings), Some(authority)) if settings.files => Some(documents::FilesAuthority {
            graph: authority.clone(),
            files: graph_files::GraphFiles::new().ok()?,
        }),
        _ => None,
    };
    let built = TeamsRuntime {
        connector_keys: keys::JwksCache::bot_framework().ok()?,
        entra_keys: keys::JwksCache::entra().ok()?,
        directory: directory::Directory::Live(Arc::new(directory::LiveDirectory::new(connector.clone(), graph))),
        connector,
        wake: tokio::sync::Notify::new(),
        files,
    };
    // A concurrent first request may have won the race; either copy is
    // equivalent, and the stored one is the one everybody shares from now on.
    let _ = RUNTIME.set(Arc::new(built));
    RUNTIME.get().cloned()
}
