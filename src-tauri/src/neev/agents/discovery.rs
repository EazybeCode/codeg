//! The provider's own model catalogue, read with the caller's own credentials.
//!
//! Codex reports its models through the app-server's `model/list`, with a
//! `supportedReasoningEfforts` and `defaultReasoningEffort` per model. The
//! pinned binary's schema types a reasoning effort as *"a non-empty reasoning
//! effort value advertised by the model"* — an open string — so the catalogue
//! cannot be a constant in source without being wrong the moment the pin moves.
//!
//! Three properties matter more than the caching.
//!
//! **Never a shared catalogue.** What a provider offers depends on the account
//! asking, so discovery runs under the CALLER's credential volume inside the
//! same isolation profile a turn uses. One user's entitlements are never
//! reported to another — a shared probe would advertise models some readers
//! cannot run and, worse, disclose what another account is paying for.
//!
//! **A failure is never permanent.** Not connected, a timeout, a provider
//! outage: each yields the provider default plus a stated reason, and the next
//! request tries again. A negative result is not cached, because "your catalogue
//! is unavailable forever" is the wrong answer to a dropped connection.
//!
//! **Bounded.** One process, a hard timeout, a capped read, and teardown on
//! every path including the timeout. Metadata only — no turn is started, so
//! nothing here spends tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::neev::{startup::ServerState, types::OrgContext};

/// How long a discovered catalogue is trusted. Long enough that opening the
/// composer repeatedly costs one probe, short enough that a model added to an
/// account shows up the same session.
const TTL_SECS: i64 = 300;
/// Entries held across all users. A bound so a large org cannot grow the process
/// through a menu.
const MAX_ENTRIES: usize = 512;
/// Bounds on what a provider may tell us. This is remote input that ends up in
/// an argv, a menu and a database column, so each field is clamped and an entry
/// that cannot be made to fit is DROPPED — one bad model should cost that model,
/// not the whole catalogue.
const MAX_MODELS: usize = 50;
const MAX_LABEL_CHARS: usize = 100;
const MAX_MODEL_EFFORTS: usize = 20;

/// An effort token, in the shape both sides agree on: lowercase, starting with a
/// letter, and short. Open enough for anything the provider advertises — the
/// real ones are `low`/`medium`/`high`/`xhigh` — without admitting a token the
/// client would refuse to render or that could be read as a flag.
pub(crate) fn safe_effort_token(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
}

/// A label safe to show: bounded, and with no control characters that could
/// scramble a menu.
fn clean_label(value: &str) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// One model as the provider describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredModel {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) default: bool,
    /// Effort levels THIS model advertises.
    ///
    /// `None` means the provider said nothing about this model's efforts, so the
    /// provider-wide list applies. `Some([])` means it answered with an EMPTY
    /// list — it supports none, and no level should be offered for it. Folding
    /// those together would either hide a level the model accepts or offer one it
    /// refuses, so they are kept apart all the way to the wire.
    pub(crate) efforts: Option<Vec<String>>,
    pub(crate) default_effort: Option<String>,
    /// Whether the provider advertises a fast tier for THIS model, read from its
    /// own `serviceTiers`/`additionalSpeedTiers` rather than assumed.
    pub(crate) supports_speed: bool,
}

/// What a caller gets back: a catalogue, or the honest reason there isn't one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Discovery {
    pub(crate) models: Vec<DiscoveredModel>,
    /// `None` when the list came from the provider. `Some(reason)` when it did
    /// not, so the UI can say why instead of showing a menu of one and implying
    /// that is all there is.
    pub(crate) unavailable: Option<&'static str>,
}

impl Discovery {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            models: Vec::new(),
            unavailable: Some(reason),
        }
    }
}

/// The cache key.
///
/// The connection generation is part of it on purpose: reconnecting, replacing
/// or revoking a provider login bumps that generation, so a catalogue read under
/// the old credential can never be served after the credential changed. That is
/// what makes "invalidate on connect/disconnect" automatic rather than a
/// separate bookkeeping step somebody has to remember.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    org_id: i32,
    user_id: i32,
    provider: String,
    generation: i64,
}

pub(crate) struct Entry {
    discovery: Discovery,
    fetched_at: i64,
}

pub(crate) type Cache = Arc<Mutex<HashMap<Key, Entry>>>;

pub(crate) fn new_cache() -> Cache {
    Arc::new(Mutex::new(HashMap::new()))
}

fn lock(cache: &Cache) -> std::sync::MutexGuard<'_, HashMap<Key, Entry>> {
    match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Drop every catalogue for one (user, provider), whatever generation it was
/// read under.
///
/// The generation in the key already prevents serving a stale credential's
/// catalogue, so this is belt and braces for the case where a connection is
/// replaced without the generation moving — and it keeps the map from holding
/// entries nobody can reach.
pub(crate) fn invalidate(cache: &Cache, org_id: i32, user_id: i32, provider: &str) {
    let mut guard = lock(cache);
    guard.retain(|key, _| {
        !(key.org_id == org_id && key.user_id == user_id && key.provider == provider)
    });
}

/// The caller's connected generation for this provider, or `None` when they have
/// no usable connection.
///
/// Both halves matter: the generation keys the cache, and its absence is the
/// "not connected" answer — which is a reason to show, not a failure to log.
async fn connected_generation(
    state: &ServerState,
    actor: &OrgContext,
    provider: &str,
) -> Option<i64> {
    crate::neev::db::one(
        &state.database,
        "SELECT generation FROM neev_agent_connections WHERE org_id=$1 AND user_id=$2 AND provider=$3 AND state='connected' AND storage_handle IS NOT NULL",
        vec![actor.org_id.into(), actor.identity.user_id.into(), provider.into()],
    )
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get::<i64>("", "generation").ok())
}

/// One model entry as `model/list` returns it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    is_default: bool,
    /// Absent entirely, versus present and empty — see `DiscoveredModel`.
    #[serde(default)]
    supported_reasoning_efforts: Option<Vec<WireEffort>>,
    #[serde(default)]
    default_reasoning_effort: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    service_tiers: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    additional_speed_tiers: Option<Vec<serde_json::Value>>,
}

/// `supportedReasoningEfforts` is a list of objects, not bare strings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEffort {
    reasoning_effort: String,
}

/// Faster-than-default service tier ids, as the pinned binary actually reports
/// them.
///
/// Recorded from a real `model/list`: `serviceTiers` carries OBJECTS whose ids
/// are `priority` and `ultrafast` — not `fast` — while the literal string
/// `"fast"` appears in `additionalSpeedTiers`. Matching `id == "fast"` therefore
/// finds nothing and would have quietly disabled Fast for every discovered
/// Codex model, which is exactly the shape of bug that looks like a working
/// feature.
const FAST_TIER_IDS: &[&str] = &["fast", "priority", "ultrafast"];

/// Whether a tier list offers something faster than the default.
///
/// The two fields have different shapes — bare strings in one, objects in the
/// other — so both are read for what they are.
fn names_a_fast_tier(tiers: &Option<Vec<serde_json::Value>>) -> bool {
    let Some(tiers) = tiers else {
        return false;
    };
    tiers.iter().any(|tier| {
        let named = tier
            .as_str()
            .or_else(|| tier.get("id").and_then(|id| id.as_str()));
        named.is_some_and(|name| FAST_TIER_IDS.contains(&name))
    })
}

/// Map the provider's answer, dropping anything unusable rather than failing the
/// whole catalogue: one malformed entry should cost that entry, not the menu.
pub(crate) fn parse_models(payload: &str) -> Vec<DiscoveredModel> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };
    // The list may sit at the top level or under a JSON-RPC `result`.
    // `result.data`, confirmed against the pinned binary — not `models`, which
    // is what the schema's type name suggested and what a mock would have
    // happily accepted.
    let items = value
        .get("result")
        .and_then(|result| result.get("data"))
        .or_else(|| value.get("data"))
        .and_then(|data| data.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    let mut models: Vec<DiscoveredModel> = Vec::new();
    for item in items {
        if models.len() >= MAX_MODELS {
            break;
        }
        let Ok(wire) = serde_json::from_value::<WireModel>(item.clone()) else {
            continue;
        };
        if wire.hidden {
            // The provider marks these as not for display; showing them would
            // offer something it has decided not to.
            continue;
        }
        if wire.id.is_empty() || !crate::neev::agents::execution::safe_model(&wire.id) {
            // An id we could not put in an argv safely is not a model we can
            // offer, whatever the provider called it.
            continue;
        }
        if models.iter().any(|existing| existing.id == wire.id) {
            // A duplicate id would give the menu two entries that send the same
            // thing, and the second could disagree about what it supports.
            continue;
        }
        let Some(label) = clean_label(wire.display_name.as_deref().unwrap_or(&wire.id)) else {
            continue;
        };
        models.push(DiscoveredModel {
            label,
            id: wire.id,
            default: wire.is_default,
            efforts: wire.supported_reasoning_efforts.map(|list| {
                let mut kept: Vec<String> = Vec::new();
                for effort in list {
                    let token = effort.reasoning_effort;
                    // Unusable tokens are dropped individually; the model keeps
                    // the levels that ARE usable rather than losing all of them.
                    if safe_effort_token(&token)
                        && !kept.contains(&token)
                        && kept.len() < MAX_MODEL_EFFORTS
                    {
                        kept.push(token);
                    }
                }
                kept
            }),
            default_effort: wire
                .default_reasoning_effort
                .filter(|value| safe_effort_token(value)),
            supports_speed: names_a_fast_tier(&wire.service_tiers)
                || names_a_fast_tier(&wire.additional_speed_tiers),
        });
    }
    models
}

/// The caller's catalogue for one provider: cached, discovered, or explained.
pub(crate) async fn catalogue(
    state: &ServerState,
    actor: &OrgContext,
    provider: &str,
) -> Discovery {
    // Only Codex advertises a catalogue. Claude's ids come from its published
    // model configuration, so there is nothing to probe for and saying
    // "unavailable" would be misleading.
    if provider != "codex" {
        return Discovery {
            models: Vec::new(),
            unavailable: None,
        };
    }
    let Some(generation) = connected_generation(state, actor, provider).await else {
        return Discovery::unavailable(
            "Connect this provider to see the models your account can use.",
        );
    };
    let key = Key {
        org_id: actor.org_id,
        user_id: actor.identity.user_id,
        provider: provider.to_owned(),
        generation,
    };
    let now = state.clock.now_unix();
    {
        let guard = lock(&state.agent_catalogues);
        if let Some(entry) = guard.get(&key) {
            if now.saturating_sub(entry.fetched_at) < TTL_SECS {
                return entry.discovery.clone();
            }
        }
    }
    // The caller's OWN credential, resolved through the same lookup a run uses,
    // so a probe cannot be pointed at somebody else's volume.
    let handle = match crate::neev::agents::intents::credential(
        state,
        actor,
        match provider {
            "codex" => crate::neev::agents::types::Provider::Codex,
            _ => crate::neev::agents::types::Provider::Claude,
        },
    )
    .await
    {
        Ok(handle) => handle,
        Err(_) => {
            return Discovery::unavailable(
                "Could not read the model list from the provider. Showing the default.",
            )
        }
    };
    let discovered = state.agents.discover_models(provider, actor, handle).await;
    // A probe takes seconds, and a credential can be revoked or replaced inside
    // them. Re-read the generation before trusting or storing the answer: a
    // catalogue read under a credential that is no longer current belongs to
    // nobody.
    if connected_generation(state, actor, provider).await != Some(generation) {
        return Discovery::unavailable(
            "The provider connection changed while reading the model list.",
        );
    }
    match discovered {
        Ok(models) if !models.is_empty() => {
            let discovery = Discovery {
                models,
                unavailable: None,
            };
            let mut guard = lock(&state.agent_catalogues);
            // A crude bound: past the cap, start over rather than grow. The
            // entries are a cache, so losing them costs one probe each.
            if guard.len() >= MAX_ENTRIES {
                guard.clear();
            }
            guard.insert(
                key,
                Entry {
                    discovery: discovery.clone(),
                    fetched_at: now,
                },
            );
            discovery
        }
        // Reached the provider and got nothing usable: not an error, but not a
        // catalogue either. NOT cached, so the next request tries again.
        Ok(_) => Discovery::unavailable("The provider returned no models for this account."),
        Err(_) => Discovery::unavailable(
            "Could not read the model list from the provider. Showing the default.",
        ),
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;
