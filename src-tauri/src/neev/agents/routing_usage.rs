//! What Auto knows about the caller's remaining provider usage, and nothing it
//! does not.
//!
//! Subscription plans meter usage in shared windows (Codex: a primary and a
//! secondary rolling window per metered limit; Claude: plan windows). Every
//! model of a provider draws from the same pools, so usage is tracked per
//! ACCOUNT — org, user, provider and connection generation — never per model,
//! and never converted into money.
//!
//! Only two kinds of evidence are accepted:
//!
//! - **A provider-reported snapshot** in a documented structure. Today that is
//!   Codex's app-server `account/rateLimits/read` response and its
//!   `account/rateLimits/updated` notification (`RateLimitWindow`: `usedPercent`
//!   0..=100, `windowDurationMins`, `resetsAt` unix seconds).
//! - **A structured failure hint** the provider itself attached to a failed turn
//!   (Codex `codexErrorInfo`, Claude's `SDKAssistantMessageError`). Failure TEXT
//!   is never read here: a message that merely mentions "limit" proves nothing,
//!   and auth, network, overload and timeout failures are not quota.
//!
//! Everything else is `Unknown`, and unknown stays unknown: no percentage, reset
//! time or price is ever guessed. Evidence expires — a snapshot stops informing
//! ranking after [`SNAPSHOT_TTL_SECS`], a window whose reset time has passed is
//! forgotten, and a hold without a known reset is a bounded re-probe delay.
//!
//! Held in memory only (no schema change). A restart forgets everything, which
//! errs towards trying a provider rather than wrongly withholding it.
//!
//! Auto may withhold a provider that is authoritatively exhausted or cooling
//! down, and may prefer one with more headroom among candidates that are already
//! eligible and connected. A MANUAL selection is never changed here: callers get
//! an advisory to show, nothing more.
// Wired by the final integration (`.context/auto-routing/usage-integration.md`).
#![allow(dead_code)]

use std::{collections::HashMap, sync::Mutex};

use serde::Serialize;

use super::routing::Candidate;
use crate::neev::types::{OrgId, UserId};

/// How long a provider-reported snapshot informs ranking. Codex re-reports its
/// windows during every turn, so an active account stays fresh; an idle one
/// decays to `Unknown` rather than being ranked on an old number.
pub(crate) const SNAPSHOT_TTL_SECS: i64 = 15 * 60;
/// The furthest reset accepted from a provider. Codex's longest window is
/// weekly; anything beyond this is treated as an unknown reset, not trusted.
pub(crate) const MAX_RESET_HORIZON_SECS: i64 = 8 * 24 * 60 * 60;
/// First cooldown after a verified rate-limit failure; doubles per consecutive
/// verified failure up to [`RATE_LIMIT_MAX_COOLDOWN_SECS`].
pub(crate) const RATE_LIMIT_BASE_COOLDOWN_SECS: i64 = 60;
pub(crate) const RATE_LIMIT_MAX_COOLDOWN_SECS: i64 = 15 * 60;
/// A verified quota failure with no known reset withholds the provider from
/// Auto for this long, then Auto may try it again. A re-probe delay, not a
/// claim about when the plan resets.
pub(crate) const QUOTA_UNKNOWN_RESET_SECS: i64 = 30 * 60;
/// Memory bounds.
pub(crate) const MAX_ACCOUNTS: usize = 4_096;
pub(crate) const MAX_POOLS: usize = 8;
pub(crate) const MAX_WINDOWS_PER_POOL: usize = 2;
const MAX_POOL_ID_CHARS: usize = 64;
const MAX_WINDOW_MINUTES: u32 = 31 * 24 * 60;
/// Band thresholds on the MOST-used fresh window of an account (percent used).
/// Policy, not data: they only sort what the provider reported into three
/// coarse bands so nothing finer is implied downstream.
const LIMITED_FROM_PERCENT: f64 = 70.0;
const LOW_FROM_PERCENT: f64 = 90.0;
const EXHAUSTED_PERCENT: f64 = 100.0;

/// One provider account, as the caller connected it. A reconnect bumps the
/// generation, so a different sign-in never inherits the previous one's usage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AccountKey {
    pub(crate) org_id: OrgId,
    pub(crate) user_id: UserId,
    pub(crate) provider: String,
    pub(crate) generation: i64,
}

/// One metered window as the provider reported it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Window {
    /// 0..=100, as reported. Never derived.
    pub(crate) used_percent: f64,
    pub(crate) window_minutes: Option<u32>,
    /// Unix seconds. `None` = the provider did not say (or said something
    /// implausible).
    pub(crate) resets_at: Option<i64>,
}

/// One metered pool (Codex `limitId`), shared by every model of the provider.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pool {
    pub(crate) id: String,
    pub(crate) windows: Vec<Window>,
}

/// A parsed provider report: only the windows, no plan, credit or account
/// fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) pools: Vec<Pool>,
}

/// A failure the PROVIDER classified in a structured field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureSignal {
    /// Short-term request rate limit on this account.
    RateLimited,
    /// The plan's usage (or billing allowance) is spent.
    QuotaExhausted,
}

/// Whether Auto may use this account right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    /// Verified rate-limit failure: withheld until `until`.
    RateLimited {
        until: i64,
    },
    /// A provider-reported window is spent and resets at `until`.
    Exhausted {
        until: i64,
    },
    /// A verified quota failure with no known reset; Auto retries after
    /// `retry_after`.
    ExhaustedUnknownReset {
        retry_after: i64,
    },
}

impl Availability {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::RateLimited { .. } => "rate_limited",
            Self::Exhausted { .. } => "exhausted",
            Self::ExhaustedUnknownReset { .. } => "exhausted_unknown_reset",
        }
    }

    pub(crate) fn is_blocked(self) -> bool {
        self != Self::Available
    }

    /// When the hold ends, if there is one.
    pub(crate) fn until(self) -> Option<i64> {
        match self {
            Self::Available => None,
            Self::RateLimited { until } | Self::Exhausted { until } => Some(until),
            Self::ExhaustedUnknownReset { retry_after } => Some(retry_after),
        }
    }
}

/// Remaining headroom, coarsely. `Unknown` is a first-class answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Band {
    Unknown,
    Plenty,
    Limited,
    Low,
}

impl Band {
    /// The only usage detail a decision provider may see: a word, no number,
    /// no time, no identifier.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Plenty => "plenty",
            Self::Limited => "limited",
            Self::Low => "low",
        }
    }

    /// Ordering for Auto: more headroom first. Unknown sits with Limited, so
    /// a provider is neither rewarded nor punished for reporting nothing.
    fn rank(self) -> u8 {
        match self {
            Self::Plenty => 0,
            Self::Limited | Self::Unknown => 1,
            Self::Low => 2,
        }
    }
}

/// What routing sees for one account at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderUsage {
    pub(crate) availability: Availability,
    pub(crate) band: Band,
    /// The reset of the most-used fresh window, when the provider gave one.
    pub(crate) next_reset_at: Option<i64>,
    /// When the fresh data was reported; `None` when nothing fresh is known.
    pub(crate) observed_at: Option<i64>,
}

impl ProviderUsage {
    pub(crate) const UNKNOWN: Self = Self {
        availability: Availability::Available,
        band: Band::Unknown,
        next_reset_at: None,
        observed_at: None,
    };
}

// ---- Evidence: provider snapshots ----

/// Read Codex's documented rate-limit payload: the `account/rateLimits/read`
/// response (`{rateLimits, rateLimitsByLimitId?}`) or the
/// `account/rateLimits/updated` notification params (`{rateLimits}`).
///
/// Keeps windows only. `planType`, `credits` and `limitName` are dropped: they
/// are account details, not headroom, and nothing here maps them to money.
pub(crate) fn parse_codex_rate_limits(value: &serde_json::Value, now: i64) -> Option<Snapshot> {
    let mut pools = Vec::new();
    if let Some(by_id) = value.get("rateLimitsByLimitId").and_then(|v| v.as_object()) {
        for (key, snapshot) in by_id {
            if pools.len() >= MAX_POOLS {
                break;
            }
            if let Some(pool) = codex_pool(snapshot, Some(key), now) {
                pools.push(pool);
            }
        }
    }
    if pools.is_empty() {
        if let Some(pool) = value
            .get("rateLimits")
            .and_then(|snapshot| codex_pool(snapshot, None, now))
        {
            pools.push(pool);
        }
    }
    (!pools.is_empty()).then_some(Snapshot { pools })
}

fn codex_pool(snapshot: &serde_json::Value, key: Option<&str>, now: i64) -> Option<Pool> {
    let id = snapshot
        .get("limitId")
        .and_then(|v| v.as_str())
        .or(key)
        .unwrap_or("default");
    let id = pool_id(id)?;
    let windows: Vec<Window> = ["primary", "secondary"]
        .iter()
        .filter_map(|name| snapshot.get(*name))
        .filter_map(|window| codex_window(window, now))
        .take(MAX_WINDOWS_PER_POOL)
        .collect();
    (!windows.is_empty()).then_some(Pool { id, windows })
}

fn codex_window(window: &serde_json::Value, now: i64) -> Option<Window> {
    let used = window.get("usedPercent")?.as_f64()?;
    if !used.is_finite() || used < 0.0 {
        return None;
    }
    let window_minutes = window
        .get("windowDurationMins")
        .and_then(|v| v.as_u64())
        .filter(|minutes| (1..=u64::from(MAX_WINDOW_MINUTES)).contains(minutes))
        .map(|minutes| minutes as u32);
    let resets_at = window
        .get("resetsAt")
        .and_then(|v| v.as_i64())
        .filter(|at| (now - MAX_RESET_HORIZON_SECS..=now + MAX_RESET_HORIZON_SECS).contains(at));
    Some(Window {
        used_percent: used.min(EXHAUSTED_PERCENT),
        window_minutes,
        resets_at,
    })
}

/// A pool id is an identifier, not free text: bounded and plain, or refused.
fn pool_id(id: &str) -> Option<String> {
    let ok = !id.is_empty()
        && id.chars().count() <= MAX_POOL_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    ok.then(|| id.to_owned())
}

// ---- Evidence: structured failures ----

/// The quota meaning of a provider's STRUCTURED failure hint, scoped to the
/// provider that can emit it. `None` for everything else — including every
/// auth, network, overload, timeout and runtime failure, and every hint our own
/// adapters invent (`closed`, `timeout`, `not_started`, …).
///
/// - Codex `codexErrorInfo`: `usageLimitExceeded` (v2) / `usage_limit_exceeded`.
///   `serverOverloaded` is capacity on OpenAI's side, not this account's quota.
/// - Claude `SDKAssistantMessageError`: `rate_limit`, `billing_error`.
///   `overloaded` and `server_error` are likewise not this account's quota.
pub(crate) fn structured_failure(provider: &str, hint: Option<&str>) -> Option<FailureSignal> {
    let hint = hint?;
    match (provider, hint) {
        ("codex", "usageLimitExceeded" | "usage_limit_exceeded") => {
            Some(FailureSignal::QuotaExhausted)
        }
        ("claude", "rate_limit") => Some(FailureSignal::RateLimited),
        ("claude", "billing_error") => Some(FailureSignal::QuotaExhausted),
        _ => None,
    }
}

// ---- The book ----

#[derive(Clone, Debug)]
struct StoredPool {
    pool: Pool,
    observed_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    RateLimited,
    QuotaUnknownReset,
}

#[derive(Clone, Copy, Debug)]
struct Hold {
    kind: HoldKind,
    until: i64,
}

#[derive(Clone, Debug, Default)]
struct AccountUsage {
    pools: Vec<StoredPool>,
    holds: Vec<Hold>,
    consecutive_rate_limits: u32,
    touched: i64,
}

/// Per-account usage evidence, bounded, in memory. One per server.
#[derive(Default)]
pub(crate) struct UsageBook {
    accounts: Mutex<HashMap<AccountKey, AccountUsage>>,
}

impl UsageBook {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut HashMap<AccountKey, AccountUsage>) -> R) -> R {
        let mut guard = self
            .accounts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    fn entry<'a>(
        accounts: &'a mut HashMap<AccountKey, AccountUsage>,
        key: &AccountKey,
        now: i64,
    ) -> &'a mut AccountUsage {
        if !accounts.contains_key(key) && accounts.len() >= MAX_ACCOUNTS {
            if let Some(oldest) = accounts
                .iter()
                .min_by_key(|(_, usage)| usage.touched)
                .map(|(key, _)| key.clone())
            {
                accounts.remove(&oldest);
            }
        }
        let usage = accounts.entry(key.clone()).or_default();
        usage.touched = now;
        usage
    }

    /// A provider-reported snapshot. Pools it names are replaced; others are
    /// kept with their own age. A snapshot that shows no spent window clears a
    /// quota hold of unknown reset: the provider has just said there is room.
    /// A rate-limit hold stays — plan windows do not describe request rates.
    pub(crate) fn record_snapshot(&self, key: &AccountKey, snapshot: Snapshot, now: i64) {
        self.with(|accounts| {
            let usage = Self::entry(accounts, key, now);
            let spent = snapshot.pools.iter().any(|pool| {
                pool.windows
                    .iter()
                    .any(|window| window.used_percent >= EXHAUSTED_PERCENT)
            });
            for pool in snapshot.pools {
                usage.pools.retain(|stored| stored.pool.id != pool.id);
                usage.pools.push(StoredPool {
                    pool,
                    observed_at: now,
                });
            }
            if usage.pools.len() > MAX_POOLS {
                usage
                    .pools
                    .sort_by_key(|stored| std::cmp::Reverse(stored.observed_at));
                usage.pools.truncate(MAX_POOLS);
            }
            if !spent {
                usage
                    .holds
                    .retain(|hold| hold.kind != HoldKind::QuotaUnknownReset);
            }
        });
    }

    /// A verified failure (see [`structured_failure`]).
    pub(crate) fn record_failure(&self, key: &AccountKey, signal: FailureSignal, now: i64) {
        self.with(|accounts| {
            let usage = Self::entry(accounts, key, now);
            let hold = match signal {
                FailureSignal::RateLimited => {
                    usage.consecutive_rate_limits = usage.consecutive_rate_limits.saturating_add(1);
                    let doublings = usage.consecutive_rate_limits.saturating_sub(1).min(16);
                    let seconds = (RATE_LIMIT_BASE_COOLDOWN_SECS << doublings)
                        .min(RATE_LIMIT_MAX_COOLDOWN_SECS);
                    Hold {
                        kind: HoldKind::RateLimited,
                        until: now + seconds,
                    }
                }
                FailureSignal::QuotaExhausted => Hold {
                    kind: HoldKind::QuotaUnknownReset,
                    until: now + QUOTA_UNKNOWN_RESET_SECS,
                },
            };
            usage.holds.retain(|held| held.kind != hold.kind);
            usage.holds.push(hold);
        });
    }

    /// A turn on this account completed: it was not rate-limited or spent. Holds
    /// and spent windows are cleared; reported headroom is otherwise kept.
    pub(crate) fn record_success(&self, key: &AccountKey, now: i64) {
        self.with(|accounts| {
            let Some(usage) = accounts.get_mut(key) else {
                return;
            };
            usage.touched = now;
            usage.holds.clear();
            usage.consecutive_rate_limits = 0;
            for stored in &mut usage.pools {
                stored
                    .pool
                    .windows
                    .retain(|window| window.used_percent < EXHAUSTED_PERCENT);
            }
            usage.pools.retain(|stored| !stored.pool.windows.is_empty());
        });
    }

    /// Disconnect, sign-out or reconnect: drop everything known for the user's
    /// provider account(s).
    pub(crate) fn forget(&self, org_id: OrgId, user_id: UserId, provider: &str) {
        self.with(|accounts| {
            accounts.retain(|key, _| {
                !(key.org_id == org_id && key.user_id == user_id && key.provider == provider)
            });
        });
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.with(|accounts| accounts.len())
    }

    pub(crate) fn view(&self, key: &AccountKey, now: i64) -> ProviderUsage {
        self.with(|accounts| match accounts.get(key) {
            None => ProviderUsage::UNKNOWN,
            Some(usage) => evaluate(usage, now),
        })
    }
}

fn evaluate(usage: &AccountUsage, now: i64) -> ProviderUsage {
    let mut blocked: Option<Availability> = None;
    let mut block = |candidate: Availability| {
        let later = match (blocked, candidate.until()) {
            (None, _) => true,
            (Some(current), Some(until)) => until > current.until().unwrap_or(i64::MIN),
            (Some(_), None) => false,
        };
        if later {
            blocked = Some(candidate);
        }
    };
    for hold in &usage.holds {
        if hold.until > now {
            block(match hold.kind {
                HoldKind::RateLimited => Availability::RateLimited { until: hold.until },
                HoldKind::QuotaUnknownReset => Availability::ExhaustedUnknownReset {
                    retry_after: hold.until,
                },
            });
        }
    }
    let mut most_used: Option<(f64, Option<i64>, i64)> = None;
    for stored in &usage.pools {
        let fresh = now - stored.observed_at < SNAPSHOT_TTL_SECS;
        for window in &stored.pool.windows {
            match window.resets_at {
                // The window has reset since it was reported: what it said no
                // longer holds, and what holds now is unknown.
                Some(at) if at <= now => continue,
                // Spent until a reset the provider named: authoritative even
                // once the snapshot is too old to rank on.
                Some(at) if window.used_percent >= EXHAUSTED_PERCENT => {
                    block(Availability::Exhausted { until: at });
                }
                // Spent with no reset named: only while fresh, then re-probe.
                None if window.used_percent >= EXHAUSTED_PERCENT && fresh => {
                    block(Availability::ExhaustedUnknownReset {
                        retry_after: stored.observed_at + SNAPSHOT_TTL_SECS,
                    });
                }
                _ => {}
            }
            if fresh && most_used.is_none_or(|(used, _, _)| window.used_percent > used) {
                most_used = Some((window.used_percent, window.resets_at, stored.observed_at));
            }
        }
    }
    let (band, next_reset_at, observed_at) = match most_used {
        None => (Band::Unknown, None, None),
        Some((used, reset, observed)) => (
            if used >= LOW_FROM_PERCENT {
                Band::Low
            } else if used >= LIMITED_FROM_PERCENT {
                Band::Limited
            } else {
                Band::Plenty
            },
            reset,
            Some(observed),
        ),
    };
    ProviderUsage {
        availability: blocked.unwrap_or(Availability::Available),
        band,
        next_reset_at,
        observed_at,
    }
}

// ---- Routing helpers ----

/// A provider Auto left out, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Withheld {
    pub(crate) provider: &'static str,
    pub(crate) availability: Availability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoFilter {
    /// Candidates Auto may still pick, in their original order.
    pub(crate) eligible: Vec<Candidate>,
    /// One entry per withheld provider.
    pub(crate) withheld: Vec<Withheld>,
}

/// Remove candidates whose provider account is exhausted or cooling down.
///
/// Only for Auto. When `eligible` comes back empty, the caller falls back to
/// the manual selection exactly as it does for any other undecidable route; it
/// never invents a substitute.
pub(crate) fn filter_for_auto(
    candidates: &[Candidate],
    usage: impl Fn(&str) -> ProviderUsage,
) -> AutoFilter {
    let mut eligible = Vec::with_capacity(candidates.len());
    let mut withheld: Vec<Withheld> = Vec::new();
    let mut seen: HashMap<&'static str, Availability> = HashMap::new();
    for candidate in candidates {
        let availability = *seen
            .entry(candidate.provider)
            .or_insert_with(|| usage(candidate.provider).availability);
        if availability.is_blocked() {
            if !withheld.iter().any(|w| w.provider == candidate.provider) {
                withheld.push(Withheld {
                    provider: candidate.provider,
                    availability,
                });
            }
        } else {
            eligible.push(candidate.clone());
        }
    }
    AutoFilter { eligible, withheld }
}

/// Connected providers, most headroom first; ties keep their given order.
/// Use before building candidates so the per-decision cap never drops the
/// provider with room in favour of one that is nearly spent. Models within a
/// provider are not reordered: they share its pools.
pub(crate) fn order_providers(
    providers: &[&'static str],
    usage: impl Fn(&str) -> ProviderUsage,
) -> Vec<&'static str> {
    let mut ranked: Vec<(u8, &'static str)> = providers
        .iter()
        .map(|provider| (usage(provider).band.rank(), *provider))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, provider)| provider).collect()
}

/// The usage word attached to a decision option. See [`Band::code`].
pub(crate) fn decision_hint(usage: &ProviderUsage) -> &'static str {
    usage.band.code()
}

/// What a MANUAL selection should be told: the hold, if any. Informational —
/// the selection itself runs as chosen.
pub(crate) fn manual_advisory(usage: &ProviderUsage) -> Option<Availability> {
    usage
        .availability
        .is_blocked()
        .then_some(usage.availability)
}

/// The caller's own usage, for the caller's own UI. No pool ids, plan, credit,
/// error text or anyone else's account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageSummary {
    pub(crate) provider: String,
    pub(crate) availability: &'static str,
    /// Unix seconds the hold ends, when held.
    pub(crate) until: Option<i64>,
    pub(crate) remaining: &'static str,
    pub(crate) next_reset_at: Option<i64>,
    pub(crate) observed_at: Option<i64>,
}

pub(crate) fn summary(provider: &str, usage: &ProviderUsage) -> UsageSummary {
    UsageSummary {
        provider: provider.to_owned(),
        availability: usage.availability.code(),
        until: usage.availability.until(),
        remaining: usage.band.code(),
        next_reset_at: usage.next_reset_at,
        observed_at: usage.observed_at,
    }
}

#[cfg(test)]
#[path = "routing_usage_tests.rs"]
mod routing_usage_tests;
