//! Auto model routing: which of the caller's OWN connected agents, models and
//! effort levels should take the next turn.
//!
//! A decision provider (TypeSafe Jev today) only CHOOSES. It never executes,
//! never sees a credential, and can only pick from a list built here from the
//! caller's connected providers and their catalogues. Its answer is checked
//! against that list, and anything wrong with it — no credential, a timeout, an
//! error, a malformed or off-list answer, low confidence — falls back to the
//! selection the person already made by hand. Routing happens before a turn
//! exists, so it cannot change a model under a running tool loop, race
//! steering or stop, or start a second run.
//!
//! Replaceable on purpose: [`DecisionProvider`] is the whole seam. Another
//! decision API can be added as another implementation without touching the
//! allowlist, the validation or the fallback.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::{redirect::Policy, StatusCode};
use url::Url;

use super::capabilities::ProviderCapabilities;

/// The most options one decision is asked to pick from. Well under Jev's 255
/// Choice options, and a catalogue bigger than this is not a menu anyone reads.
pub(crate) const MAX_CANDIDATES: usize = 32;
/// The most options from any one provider, so a large first catalogue cannot
/// crowd every other connected provider out of the list.
pub(crate) const MAX_PER_PROVIDER: usize = 16;
/// The most characters of each side of the previous exchange sent as context.
pub(crate) const MAX_PREVIOUS_CHARS: usize = 1_000;
/// A follow-up at most this long, with no files, is treated as a continuation
/// of the work already under way ("yes", "continue", "fix it", "ok, ship it")
/// rather than a new task. Judged by length, not by a phrase list, so it holds
/// in every language. Such a turn keeps the selection the conversation is
/// already running on instead of being re-decided from a word or two.
pub(crate) const BRIEF_FOLLOW_UP_CHARS: usize = 48;
/// Below this complexity confidence the effort is rounded UP: an unsure
/// "probably easy" must not under-provision the run.
const UNSURE_COMPLEXITY: f64 = 0.5;
/// The most prompt text sent to the router. The ask is classified, not
/// answered, so its opening is what matters; the bound also keeps the request
/// well inside the router's context window and its per-token price negligible.
pub(crate) const MAX_TASK_CHARS: usize = 6_000;
/// The largest router response read. A real answer is well under 2 KiB.
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// A hard ceiling on one routing decision, independent of the HTTP client's
/// own timeout, so a provider that never answers cannot hold a send open.
const DEADLINE: Duration = Duration::from_secs(11);

/// The complexity rubric, least to most. Its index drives the effort level.
pub(crate) const COMPLEXITY_LEVELS: [&str; 4] = [
    "Trivial: a quick answer, lookup or explanation with no code changes",
    "Simple: a small, well-scoped change or a focused question about code",
    "Substantial: multi-step or multi-file work that needs care and verification",
    "Hard: complex, ambiguous, architectural or high-risk engineering work",
];

/// The levels Auto may choose, least to most. `max` is deliberately absent: it
/// is the costliest level and stays an explicit, manual choice.
const AUTO_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];

/// One option a router may pick: a real (provider, model) the caller can run,
/// with the effort levels that model accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) provider: &'static str,
    pub(crate) provider_label: &'static str,
    pub(crate) model: String,
    pub(crate) label: String,
    /// Allowed levels, least to most. Empty = this model takes none.
    pub(crate) efforts: Vec<String>,
    /// How much of the caller's plan is left for this provider, as one word
    /// (`routing_usage::decision_hint`). `unknown` until the caller sets it.
    pub(crate) remaining_usage: &'static str,
}

/// The allowlist: every catalogued model of every CONNECTED provider.
///
/// A provider's unnamed `default` entry is left out when it has real models,
/// because "whatever the CLI picks" is not something a router can reason about;
/// a provider whose only entry is its default still offers it.
pub(crate) fn candidates(
    catalogues: &[ProviderCapabilities],
    connected: &[&str],
) -> Vec<Candidate> {
    let mut list = Vec::new();
    for catalogue in catalogues {
        if !connected.contains(&catalogue.provider) {
            continue;
        }
        let named = catalogue.models.iter().any(|model| model.id != "default");
        let mut taken = 0;
        for model in &catalogue.models {
            if named && model.id == "default" {
                continue;
            }
            if list.len() >= MAX_CANDIDATES {
                return list;
            }
            if taken >= MAX_PER_PROVIDER {
                break;
            }
            taken += 1;
            list.push(Candidate {
                provider: catalogue.provider,
                provider_label: catalogue.label,
                model: model.id.clone(),
                label: model.label.clone(),
                efforts: model
                    .efforts
                    .clone()
                    .unwrap_or_else(|| catalogue.efforts.clone()),
                remaining_usage: "unknown",
            });
        }
    }
    list
}

/// Map a complexity score (0 ..= levels-1) onto the levels THIS model allows.
///
/// Only levels the model advertises can come out of this, so the result always
/// passes the same validation a manual choice does.
pub(crate) fn effort_for(allowed: &[String], complexity: f64, unsure: bool) -> Option<String> {
    let preferred: Vec<&String> = allowed
        .iter()
        .filter(|level| AUTO_EFFORTS.contains(&level.as_str()))
        .collect();
    let scale: Vec<&String> = if preferred.is_empty() {
        allowed
            .iter()
            .filter(|level| level.as_str() != "max")
            .collect()
    } else {
        preferred
    };
    if scale.is_empty() {
        return None;
    }
    let top = (COMPLEXITY_LEVELS.len() - 1) as f64;
    let exact = complexity.clamp(0.0, top) / top * (scale.len() - 1) as f64;
    // Unsure means "do not round this down".
    let position = if unsure { exact.ceil() } else { exact.round() };
    Some(scale[(position as usize).min(scale.len() - 1)].clone())
}

/// The prompt as the router sees it: bounded, on a character boundary.
pub(crate) fn bound_task(prompt: &str) -> String {
    bound(prompt, MAX_TASK_CHARS)
}

fn bound(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(limit).collect();
    kept.push_str("\n[truncated]");
    kept
}

/// Whether this ask continues the conversation rather than starting new work.
pub(crate) fn is_brief_follow_up(ask: &Ask<'_>) -> bool {
    ask.earlier_turns > 0
        && ask.attached_files == 0
        && ask.prompt.trim().chars().count() <= BRIEF_FOLLOW_UP_CHARS
}

/// What will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) fast: bool,
}

/// Why Auto used the manual selection instead of a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FallbackReason {
    NotConfigured,
    MissingCredential,
    NoCandidates,
    Timeout,
    Unavailable,
    Malformed,
    /// The router refused the request itself (bad key, invalid request):
    /// persistent until the integration is fixed, unlike an overload.
    Rejected,
    LowConfidence,
    /// Every eligible provider account is spent or cooling down, as the
    /// provider itself reported.
    UsageLimited,
    /// This person asked for more decisions than the per-user (or the
    /// deployment-wide) limit allows; the router was not called.
    Throttled,
}

impl FallbackReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::MissingCredential => "missing_credential",
            Self::NoCandidates => "no_candidates",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Malformed => "malformed",
            Self::Rejected => "rejected",
            Self::LowConfidence => "low_confidence",
            Self::UsageLimited => "usage_limited",
            Self::Throttled => "throttled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Routed,
    /// A brief follow-up: the conversation keeps what it is running on.
    Kept,
    Fallback(FallbackReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    pub(crate) selection: Selection,
    pub(crate) outcome: Outcome,
    /// The decision provider that was consulted, if any was.
    pub(crate) router: Option<&'static str>,
    /// The chosen model's catalogue label, when routed.
    pub(crate) label: Option<String>,
    /// The task's complexity level (index into [`COMPLEXITY_LEVELS`]), when
    /// a decision was made. Rounded UP when the router was unsure, so it never
    /// understates the work.
    pub(crate) complexity: Option<u8>,
}

/// The ask, as far as routing is concerned.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ask<'a> {
    pub(crate) prompt: &'a str,
    pub(crate) attached_files: usize,
    pub(crate) earlier_turns: usize,
    /// The last completed exchange, when there is one, so a follow-up is
    /// judged as part of the work it continues.
    pub(crate) previous: Option<Previous<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Previous<'a> {
    pub(crate) request: &'a str,
    pub(crate) reply: &'a str,
}

/// One option as the router sees it: the agent and model as the catalogue
/// names them. Nothing about cost or capability is asserted here — the
/// catalogue does not say, and a guess from a name would be presented to the
/// router as fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteOption {
    pub(crate) agent: String,
    pub(crate) model: String,
    /// `plenty`, `limited`, `low` or `unknown`: the only usage detail a
    /// decision provider ever sees. No numbers, times, pools or identity.
    pub(crate) remaining_usage: &'static str,
}

/// What a decision provider is given. No identity, no credential, no history
/// text: the bounded ask and the option labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecisionInput {
    pub(crate) task: String,
    pub(crate) attached_files: usize,
    pub(crate) earlier_turns: usize,
    /// The previous exchange, each side bounded to [`MAX_PREVIOUS_CHARS`].
    pub(crate) previous: Option<(String, String)>,
    /// One per candidate, in candidate order.
    pub(crate) options: Vec<RouteOption>,
}

/// A provider's raw answer. Validated by [`Router`], never trusted as given.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decision {
    /// Index into [`DecisionInput::options`].
    pub(crate) option: usize,
    /// 0..=1, how sure the provider is of `option`.
    pub(crate) confidence: f64,
    /// 0..=COMPLEXITY_LEVELS.len()-1.
    pub(crate) complexity: f64,
    /// 0..=1, how sure the provider is of `complexity`.
    pub(crate) complexity_confidence: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionError {
    Timeout,
    Unavailable,
    Malformed,
    Rejected,
}

#[async_trait]
pub(crate) trait DecisionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn decide(&self, input: &DecisionInput) -> Result<Decision, DecisionError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouterStatus {
    /// No router configured: Auto is not offered.
    Off,
    /// Configured, but its credential is absent: shown, never used.
    MissingCredential,
    Ready,
}

impl RouterStatus {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::MissingCredential => "missing_credential",
            Self::Ready => "ready",
        }
    }
}

pub(crate) enum Router {
    Off,
    MissingCredential {
        router: &'static str,
    },
    Ready {
        provider: Arc<dyn DecisionProvider>,
        min_confidence: f64,
    },
}

impl Router {
    /// The router an operator configured. A configured provider whose key is
    /// absent (or unusable) is `MissingCredential`: visible as such, and never
    /// reported or used as ready.
    pub(crate) fn from_config(config: &crate::neev::config::RoutingConfig) -> Self {
        use crate::neev::config::RoutingConfig;
        match config {
            RoutingConfig::Off => Self::Off,
            RoutingConfig::Jev {
                api_key: Some(key),
                model,
                timeout_ms,
                min_confidence,
            } => match Jev::new(key.clone(), *model, Duration::from_millis(*timeout_ms)) {
                Ok(jev) => Self::Ready {
                    provider: Arc::new(jev),
                    min_confidence: *min_confidence,
                },
                Err(()) => Self::MissingCredential { router: "jev" },
            },
            RoutingConfig::Jev { api_key: None, .. } => Self::MissingCredential { router: "jev" },
        }
    }

    pub(crate) fn status(&self) -> RouterStatus {
        match self {
            Self::Off => RouterStatus::Off,
            Self::MissingCredential { .. } => RouterStatus::MissingCredential,
            Self::Ready { .. } => RouterStatus::Ready,
        }
    }

    /// The configured decision provider's name, whether or not it can run.
    pub(crate) fn name(&self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::MissingCredential { router } => Some(router),
            Self::Ready { provider, .. } => Some(provider.name()),
        }
    }

    pub(crate) async fn route(
        &self,
        ask: Ask<'_>,
        candidates: &[Candidate],
        fallback: Selection,
    ) -> Route {
        self.route_within(ask, candidates, fallback, DEADLINE).await
    }

    pub(crate) async fn route_within(
        &self,
        ask: Ask<'_>,
        candidates: &[Candidate],
        fallback: Selection,
        deadline: Duration,
    ) -> Route {
        let (provider, min_confidence) = match self {
            Self::Off => return fell_back(fallback, FallbackReason::NotConfigured, None),
            Self::MissingCredential { .. } => {
                return fell_back(fallback, FallbackReason::MissingCredential, None)
            }
            Self::Ready {
                provider,
                min_confidence,
            } => (provider, *min_confidence),
        };
        if is_brief_follow_up(&ask) {
            return Route {
                selection: fallback,
                outcome: Outcome::Kept,
                router: None,
                label: None,
                complexity: None,
            };
        }
        if candidates.is_empty() {
            return fell_back(fallback, FallbackReason::NoCandidates, None);
        }
        let name = Some(provider.name());
        let input = DecisionInput {
            task: bound_task(ask.prompt),
            attached_files: ask.attached_files,
            earlier_turns: ask.earlier_turns,
            previous: ask.previous.map(|previous| {
                (
                    bound(previous.request, MAX_PREVIOUS_CHARS),
                    bound(previous.reply, MAX_PREVIOUS_CHARS),
                )
            }),
            options: candidates
                .iter()
                .map(|candidate| RouteOption {
                    agent: candidate.provider_label.to_owned(),
                    model: candidate.label.clone(),
                    remaining_usage: candidate.remaining_usage,
                })
                .collect(),
        };
        let decision = match tokio::time::timeout(deadline, provider.decide(&input)).await {
            Err(_) => Err(DecisionError::Timeout),
            Ok(result) => result,
        };
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                let reason = match error {
                    DecisionError::Timeout => FallbackReason::Timeout,
                    DecisionError::Unavailable => FallbackReason::Unavailable,
                    DecisionError::Malformed => FallbackReason::Malformed,
                    DecisionError::Rejected => FallbackReason::Rejected,
                };
                return fell_back(fallback, reason, name);
            }
        };
        let top = (COMPLEXITY_LEVELS.len() - 1) as f64;
        let Some(chosen) = candidates.get(decision.option) else {
            return fell_back(fallback, FallbackReason::Malformed, name);
        };
        if !decision.confidence.is_finite()
            || !(0.0..=1.0).contains(&decision.confidence)
            || !decision.complexity_confidence.is_finite()
            || !(0.0..=1.0).contains(&decision.complexity_confidence)
            || !decision.complexity.is_finite()
            || !(0.0..=top).contains(&decision.complexity)
        {
            return fell_back(fallback, FallbackReason::Malformed, name);
        }
        if decision.confidence < min_confidence {
            return fell_back(fallback, FallbackReason::LowConfidence, name);
        }
        let unsure = decision.complexity_confidence < UNSURE_COMPLEXITY;
        let level = if unsure {
            decision.complexity.ceil()
        } else {
            decision.complexity.round()
        };
        Route {
            selection: Selection {
                provider: chosen.provider.to_owned(),
                model: chosen.model.clone(),
                effort: effort_for(&chosen.efforts, decision.complexity, unsure),
                fast: false,
            },
            outcome: Outcome::Routed,
            router: name,
            label: Some(chosen.label.clone()),
            // Validated to 0..=top above, so this fits.
            complexity: Some(level as u8),
        }
    }
}

fn fell_back(selection: Selection, reason: FallbackReason, router: Option<&'static str>) -> Route {
    if router.is_some() {
        // The reason only: never the prompt, the answer or the key.
        tracing::warn!(
            reason = reason.code(),
            "agent auto routing used the manual selection"
        );
    }
    Route {
        selection,
        outcome: Outcome::Fallback(reason),
        router,
        label: None,
        complexity: None,
    }
}

// ---- TypeSafe Jev ----

const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The Jev models this deployment may send. Pinned is the default: the docs
/// advise pinning the versioned id once confidence thresholds are tuned
/// against it, because the alias moves with each release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JevModel {
    Pinned,
    Latest,
}

impl JevModel {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "jev-1.13.0" => Some(Self::Pinned),
            "jev-latest" => Some(Self::Latest),
            _ => None,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Pinned => "jev-1.13.0",
            Self::Latest => "jev-latest",
        }
    }
}

/// The Jev decision provider. No `Debug`: it holds the API key.
pub(crate) struct Jev {
    client: reqwest::Client,
    api_key: String,
    model: JevModel,
    endpoint: Url,
}

impl Jev {
    pub(crate) fn new(api_key: String, model: JevModel, timeout: Duration) -> Result<Self, ()> {
        Self::with_endpoint(
            api_key,
            model,
            timeout,
            Url::parse(JEV_ENDPOINT).map_err(|_| ())?,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        api_key: String,
        model: JevModel,
        timeout: Duration,
        endpoint: Url,
    ) -> Result<Self, ()> {
        if endpoint.scheme() != "http"
            || !endpoint
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
        {
            return Err(());
        }
        Self::with_endpoint(api_key, model, timeout, endpoint)
    }

    fn with_endpoint(
        api_key: String,
        model: JevModel,
        timeout: Duration,
        endpoint: Url,
    ) -> Result<Self, ()> {
        if !valid_key(&api_key) {
            return Err(());
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| ())?;
        Ok(Self {
            client,
            api_key,
            model,
            endpoint,
        })
    }

    fn body(&self, input: &DecisionInput) -> serde_json::Value {
        let criteria: serde_json::Map<String, serde_json::Value> = input
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                (
                    option_key(index),
                    serde_json::json!({
                        "agent": option.agent,
                        "model": option.model,
                        "remaining_usage": option.remaining_usage,
                    }),
                )
            })
            .collect();
        let mut state = serde_json::json!({
            "task": input.task,
            "attached_files": input.attached_files,
            "earlier_turns": input.earlier_turns,
        });
        if let Some((request, reply)) = &input.previous {
            state["previous"] = serde_json::json!({"request": request, "reply": reply});
        }
        serde_json::json!({
            "model": self.model.id(),
            "state": state,
            "questions": {
                "route": {
                    "type": "choice",
                    "instructions": "Which coding agent and model is best suited to handle `task`? When `previous` is present, `task` continues that conversation. Choose a more capable model only when the work needs it. `remaining_usage` is how much of the person's plan is left for that agent (plenty, limited, low, unknown); when options are equally suitable, prefer more remaining usage. unknown is not low.",
                    "criteria": criteria,
                },
                "complexity": {
                    "type": "score",
                    "instructions": "How demanding is `task` for a coding agent? When `previous` is present, judge `task` as a continuation of it.",
                    "criteria": COMPLEXITY_LEVELS,
                },
            },
        })
    }
}

pub(crate) fn valid_key(key: &str) -> bool {
    !key.trim().is_empty() && key.len() <= 512 && !key.chars().any(char::is_control)
}

fn option_key(index: usize) -> String {
    format!("o{}", index + 1)
}

#[async_trait]
impl DecisionProvider for Jev {
    fn name(&self) -> &'static str {
        "jev"
    }

    async fn decide(&self, input: &DecisionInput) -> Result<Decision, DecisionError> {
        let failed = |error: reqwest::Error| {
            if error.is_timeout() {
                DecisionError::Timeout
            } else {
                DecisionError::Unavailable
            }
        };
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&self.body(input))
            .send()
            .await
            .map_err(failed)?;
        match response.status() {
            StatusCode::OK => {}
            // The request or the key is wrong: retrying will not fix it.
            StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::UNPROCESSABLE_ENTITY => return Err(DecisionError::Rejected),
            // Rate limits, overload (529) and server errors: transient.
            _ => return Err(DecisionError::Unavailable),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(DecisionError::Malformed);
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_answer(&bytes, input.options.len()).ok_or(DecisionError::Malformed)
    }
}

/// Read Jev's documented response, accepting only an option it was offered.
fn parse_answer(bytes: &[u8], offered: usize) -> Option<Decision> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let answers = value.get("answers")?;
    let route = answers.get("route")?;
    let complexity = answers.get("complexity")?;
    if route.get("type")?.as_str()? != "choice" || complexity.get("type")?.as_str()? != "score" {
        return None;
    }
    let choice = route.get("choice")?.as_str()?;
    let option = (0..offered).find(|index| option_key(*index) == choice)?;
    Some(Decision {
        option,
        confidence: route.get("confidence")?.as_f64()?,
        complexity: complexity.get("score")?.as_f64()?,
        complexity_confidence: complexity.get("confidence")?.as_f64()?,
    })
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod routing_tests;

// ---- Decision rate limit ----

/// Decisions one person may request per minute. Generous for real use (a
/// person cannot send that many messages), small enough that a client loop
/// cannot spend the operator's decision quota.
pub(crate) const DECISIONS_PER_USER_PER_MINUTE: usize = 20;
/// Decisions the whole deployment may request per minute, well under the
/// router's own account-wide limit, so one busy org cannot exhaust it for all.
pub(crate) const DECISIONS_GLOBAL_PER_MINUTE: usize = 1_200;
const DECISION_WINDOW_SECS: i64 = 60;
const MAX_TRACKED_PEOPLE: usize = 4_096;

/// A sliding one-minute limit on router calls, per person and overall. In
/// memory and bounded; only decisions are limited, never execution.
#[derive(Default)]
pub(crate) struct DecisionLimiter {
    inner: std::sync::Mutex<LimiterState>,
}

#[derive(Default)]
struct LimiterState {
    people: std::collections::HashMap<(i32, i32), std::collections::VecDeque<i64>>,
    global: std::collections::VecDeque<i64>,
}

impl DecisionLimiter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record one decision for this person at `now`, or refuse it.
    pub(crate) fn admit(&self, org_id: i32, user_id: i32, now: i64) -> bool {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let horizon = now - DECISION_WINDOW_SECS;
        while state.global.front().is_some_and(|at| *at <= horizon) {
            state.global.pop_front();
        }
        if state.global.len() >= DECISIONS_GLOBAL_PER_MINUTE {
            return false;
        }
        if !state.people.contains_key(&(org_id, user_id))
            && state.people.len() >= MAX_TRACKED_PEOPLE
        {
            // Forget people with nothing in the window; if everyone is
            // active, refuse rather than grow.
            state
                .people
                .retain(|_, times| times.back().is_some_and(|at| *at > horizon));
            if state.people.len() >= MAX_TRACKED_PEOPLE {
                return false;
            }
        }
        let times = state.people.entry((org_id, user_id)).or_default();
        while times.front().is_some_and(|at| *at <= horizon) {
            times.pop_front();
        }
        if times.len() >= DECISIONS_PER_USER_PER_MINUTE {
            return false;
        }
        times.push_back(now);
        state.global.push_back(now);
        true
    }
}
