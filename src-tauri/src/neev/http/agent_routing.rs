//! Auto model routing over HTTP: whether it is available, the decision for a
//! new chat, the decision `start_turn` makes for an Auto turn, and whether a
//! complex Auto turn is supervised.
//!
//! Every decision is made over a list built here from the caller's OWN
//! catalogue and connections, ordered and filtered by what the caller's OWN
//! provider streams reported about their usage. A routed selection is
//! re-checked by `validated_for` exactly as a manual one is before anything
//! runs. Nothing here executes, creates a session or a turn, or touches a
//! credential beyond looking up the caller's own handle for a supervisor.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::*;
use crate::neev::agents::acp_providers;
use crate::neev::agents::capabilities::{self, ProviderCapabilities};
use crate::neev::agents::routing::{
    self, Ask, FallbackReason, Outcome, Previous, Route, Selection,
};
use crate::neev::agents::routing_usage::{self, AccountKey, ProviderUsage, Withheld};
use crate::neev::agents::runs::supervision::{Limits, SupervisionPlan, Supervisor};
use crate::neev::agents::types::ConnectionState;
use crate::neev::config::SupervisionMode;

/// The same bound a turn's prompt has.
const MAX_ROUTE_PROMPT_CHARS: usize = 32_768;
/// More files than one turn can carry is not a real ask.
const MAX_ROUTE_FILES: usize = 64;
/// How long a new chat's decision waits for its first turn.
const TICKET_TTL_SECS: i64 = 600;
const MAX_TICKETS: usize = 1_024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RouteDto {
    provider: String,
    model: String,
    /// The chosen model's catalogue label, when routed.
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    effort: Option<String>,
    fast: bool,
    /// `routed`, `kept` (a brief follow-up stayed on its selection) or
    /// `fallback` (the manual selection, for `reason`).
    outcome: &'static str,
    reason: Option<&'static str>,
    /// The decision provider consulted, if one was.
    router: Option<&'static str>,
    /// A new chat's decision, for its first turn to present exactly once.
    #[serde(skip_serializing_if = "Option::is_none")]
    route_id: Option<String>,
    /// Providers Auto left out because the caller's own usage there is spent
    /// or cooling down, as the provider reported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    withheld: Vec<WithheldDto>,
    /// Whether this turn is supervised, or why it could not be. Absent when
    /// the work did not call for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    supervision: Option<SupervisionDto>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WithheldDto {
    provider: &'static str,
    availability: &'static str,
    until: Option<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SupervisionDto {
    /// `active` or `unavailable`.
    state: &'static str,
    /// `review` or `plan_review`, when active.
    phases: Option<&'static str>,
    /// The supervisor model, when active.
    model: Option<String>,
    /// Why it is unavailable.
    reason: Option<&'static str>,
}

pub(super) fn route_dto(route: &Route) -> RouteDto {
    let (outcome, reason) = match route.outcome {
        Outcome::Routed => ("routed", None),
        Outcome::Kept => ("kept", None),
        Outcome::Fallback(reason) => ("fallback", Some(reason.code())),
    };
    RouteDto {
        provider: route.selection.provider.clone(),
        model: route.selection.model.clone(),
        label: route.label.clone(),
        effort: route.selection.effort.clone(),
        fast: route.selection.fast,
        outcome,
        reason,
        router: route.router,
        route_id: None,
        withheld: Vec::new(),
        supervision: None,
    }
}

impl RouteDto {
    pub(super) fn with_supervision(mut self, supervision: Option<SupervisionDto>) -> Self {
        self.supervision = supervision;
        self
    }
}

fn withheld_dto(withheld: &[Withheld]) -> Vec<WithheldDto> {
    withheld
        .iter()
        .map(|entry| WithheldDto {
            provider: entry.provider,
            availability: entry.availability.code(),
            until: entry.availability.until(),
        })
        .collect()
}

// ---- Usage ----

fn usage_key(actor: &OrgContext, provider: &str) -> AccountKey {
    AccountKey {
        org_id: actor.org_id,
        user_id: actor.identity.user_id,
        provider: provider.to_owned(),
        // Matches the run driver: connection changes forget the account.
        generation: 0,
    }
}

/// The caller's OWN usage for one provider. Never anyone else's.
fn usage_of(state: &ServerState, actor: &OrgContext, provider: &str) -> ProviderUsage {
    state
        .agent_usage
        .view(&usage_key(actor, provider), state.clock.now_unix())
}

fn label_usage(state: &ServerState, actor: &OrgContext, candidates: &mut [routing::Candidate]) {
    for candidate in candidates {
        candidate.remaining_usage =
            routing_usage::decision_hint(&usage_of(state, actor, candidate.provider));
    }
}

// ---- Status ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutingStatusDto {
    /// `off` (Auto not offered), `missing_credential` (configured, unusable,
    /// shown as such) or `ready`.
    state: &'static str,
    router: Option<&'static str>,
    /// How much of a prompt the router is sent, so the UI can say so.
    max_task_chars: usize,
    /// How much of the previous exchange (request plus reply) may also be
    /// sent with a follow-up.
    max_previous_chars: usize,
    /// The operator's supervision setting: `off`, `review` or `plan_review`.
    supervision: &'static str,
    /// The caller's own usage per connected provider, as far as known.
    usage: Vec<routing_usage::UsageSummary>,
}

/// GET `/api/neev/agent/routing`.
pub(in crate::neev::http) async fn routing_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let usage = intents::list(&state, &actor)
        .await
        .map_err(super::super::agent_error)?
        .into_iter()
        .filter(|view| view.state == ConnectionState::Connected)
        .map(|view| {
            let provider = view.provider.name();
            routing_usage::summary(provider, &usage_of(&state, &actor, provider))
        })
        .collect();
    Ok(private_json(RoutingStatusDto {
        state: state.agent_router.status().code(),
        router: state.agent_router.name(),
        max_task_chars: routing::MAX_TASK_CHARS,
        max_previous_chars: 2 * routing::MAX_PREVIOUS_CHARS,
        supervision: state.config.supervision.mode.code(),
        usage,
    }))
}

// ---- New chats ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RouteInput {
    prompt: String,
    /// The draft's manual selection: the fallback when routing cannot decide.
    provider: String,
    model: String,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    fast: bool,
    #[serde(default)]
    attached_files: usize,
}

/// A new chat's routed decision, held until its first turn presents it.
#[derive(Clone)]
struct Ticket {
    org_id: i32,
    user_id: i32,
    route: Route,
    expires_at: i64,
}

fn tickets() -> &'static Mutex<HashMap<String, Ticket>> {
    static TICKETS: OnceLock<Mutex<HashMap<String, Ticket>>> = OnceLock::new();
    TICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn issue_ticket(state: &ServerState, actor: &OrgContext, route: &Route) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let now = state.clock.now_unix();
    let mut held = tickets()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    held.retain(|_, ticket| ticket.expires_at > now);
    if held.len() >= MAX_TICKETS {
        // Bounded: the soonest to expire goes first.
        if let Some(oldest) = held
            .iter()
            .min_by_key(|(_, ticket)| ticket.expires_at)
            .map(|(key, _)| key.clone())
        {
            held.remove(&oldest);
        }
    }
    held.insert(
        id.clone(),
        Ticket {
            org_id: actor.org_id,
            user_id: actor.identity.user_id,
            route: route.clone(),
            expires_at: now + TICKET_TTL_SECS,
        },
    );
    id
}

/// The decision a new chat's first turn presents: consumed on first use, only
/// by the caller it was made for, and only if the session runs exactly what
/// was decided. Anything else is no decision at all — never an error.
pub(super) fn take_ticket(
    state: &ServerState,
    actor: &OrgContext,
    route_id: &str,
    session: &AgentSession,
) -> Option<Route> {
    let ticket = tickets()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(route_id)?;
    let selection = &ticket.route.selection;
    (ticket.org_id == actor.org_id
        && ticket.user_id == actor.identity.user_id
        && ticket.expires_at > state.clock.now_unix()
        && selection.provider == session.provider
        && selection.model == session.model
        && selection.effort == session.effort
        && selection.fast == session.fast)
        .then_some(ticket.route)
}

/// POST `/api/neev/agent/route` — decide a NEW chat's agent, model and effort
/// across every provider the caller has connected.
///
/// Creates nothing. The client creates the session with the answer and starts
/// its first turn with the returned `routeId`, so the decision is made exactly
/// once and a cancelled draft leaves nothing behind. `409` when no provider is
/// connected: the composer's existing Connect control is the remedy.
pub(in crate::neev::http) async fn route_new_chat(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) =
        catalog::organization_mutation::<RouteInput>(&state, request, TURN_BODY_LIMIT).await?;
    if input.prompt.chars().count() > MAX_ROUTE_PROMPT_CHARS
        || input.attached_files > MAX_ROUTE_FILES
        || (input.prompt.trim().is_empty() && input.attached_files == 0)
    {
        return Err(ApiError::Invalid);
    }
    let (route, withheld) = route_across(&state, &actor, &input).await?;
    let mut dto = route_dto(&route);
    dto.withheld = withheld_dto(&withheld);
    if route.outcome == Outcome::Routed {
        dto.route_id = Some(issue_ticket(&state, &actor, &route));
    }
    Ok(private_json(dto))
}

/// The cross-provider decision for a NEW conversation (a new Agent Chat, or a
/// first Auto ask in a resource chat): every provider the caller has
/// connected, most headroom first, usage-filtered, admitted, confirmed, with
/// the caller's manual selection as the fallback. `Conflict` when nothing is
/// connected or no runtime is bound.
async fn route_across(
    state: &ServerState,
    actor: &OrgContext,
    input: &RouteInput,
) -> Result<(Route, Vec<Withheld>), ApiError> {
    if !state.agents.is_available() {
        return Err(ApiError::Conflict);
    }
    let connected: Vec<&'static str> = intents::list(state, actor)
        .await
        .map_err(super::super::agent_error)?
        .into_iter()
        .filter(|view| view.state == ConnectionState::Connected)
        .map(|view| view.provider.name())
        .collect();
    if connected.is_empty() {
        return Err(ApiError::Conflict);
    }
    // Most headroom first, so the per-decision caps never drop the provider
    // that still has room.
    let connected =
        routing_usage::order_providers(&connected, |provider| usage_of(state, actor, provider));
    let mut catalogues = Vec::with_capacity(connected.len());
    for provider in &connected {
        catalogues.push(catalogue(state, actor, provider).await);
    }
    let fallback = manual_fallback(state, actor, input, &catalogues).await;
    let mut candidates = routing::candidates(&catalogues, &connected);
    label_usage(state, actor, &mut candidates);
    let filtered =
        routing_usage::filter_for_auto(&candidates, |provider| usage_of(state, actor, provider));
    let ready = state.agent_router.status() == routing::RouterStatus::Ready;
    let route = if filtered.eligible.is_empty() && !candidates.is_empty() && ready {
        Route {
            selection: fallback,
            outcome: Outcome::Fallback(FallbackReason::UsageLimited),
            router: None,
            label: None,
            complexity: None,
        }
    } else if ready && !filtered.eligible.is_empty() && !admit(state, actor) {
        Route {
            selection: fallback,
            outcome: Outcome::Fallback(FallbackReason::Throttled),
            router: None,
            label: None,
            complexity: None,
        }
    } else {
        let route = state
            .agent_router
            .route(
                Ask {
                    prompt: &input.prompt,
                    attached_files: input.attached_files,
                    earlier_turns: 0,
                    previous: None,
                },
                &filtered.eligible,
                fallback.clone(),
            )
            .await;
        confirm(state, actor, route, fallback).await
    };
    Ok((route, filtered.withheld))
}

/// The decision for an Auto ask in a resource-bound chat ("Ask agent").
///
/// The existing policy, nothing new: a FOLLOW-UP continues the invoker's most
/// recently used holder for this chat and is decided within that holder's
/// provider exactly as an Agent Chat follow-up is (`route_turn`; a brief one
/// keeps its selection without a decision). A FIRST ask — or one whose last
/// holder's provider is no longer connected — is decided across every
/// connected provider exactly as a new Agent Chat is (`route_across`).
///
/// Never an error, and never a disconnected provider when a connected one
/// exists: an undecided cross-provider ask keeps `route_across`'s own fallback
/// (the composer's selection when it is connected and unblocked, otherwise a
/// connected provider's default); an undecided follow-up uses the composer's
/// selection when its provider is connected, otherwise the holder's own
/// (connected) selection. Only when NOTHING is connected is the composer's
/// selection returned as is — the caller refuses that before anything is
/// written.
pub(super) async fn route_team_ask(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    prompt: &str,
    attached_files: usize,
    composer: Selection,
) -> Route {
    let connected = connected_providers(state, actor).await;
    let holder = sessions::latest_team_session(state, actor, chat_id)
        .await
        .ok()
        .flatten();
    match holder {
        Some(holder) if connected.contains(&holder.provider.as_str()) => {
            let history = sessions::recent_turns(state, actor, &holder.id)
                .await
                .unwrap_or_default();
            let route = route_turn(state, actor, &holder, prompt, attached_files, &history).await;
            match route.outcome {
                // Undecided: the composer's choice when it can run; otherwise the
                // holder's own selection, which `route_turn` already fell back to.
                Outcome::Fallback(reason) if connected.contains(&composer.provider.as_str()) => {
                    Route {
                        selection: composer,
                        outcome: Outcome::Fallback(reason),
                        router: None,
                        label: None,
                        complexity: None,
                    }
                }
                _ => route,
            }
        }
        _ => {
            let input = RouteInput {
                prompt: prompt.to_owned(),
                provider: composer.provider.clone(),
                model: composer.model.clone(),
                effort: composer.effort.clone(),
                fast: composer.fast,
                attached_files,
            };
            match route_across(state, actor, &input).await {
                Ok((route, _withheld)) => route,
                Err(_) => Route {
                    selection: composer,
                    outcome: Outcome::Fallback(FallbackReason::NoCandidates),
                    router: None,
                    label: None,
                    complexity: None,
                },
            }
        }
    }
}

/// The providers this caller has connected (empty when the list cannot be read).
pub(super) async fn connected_providers(state: &ServerState, actor: &OrgContext) -> Vec<&'static str> {
    intents::list(state, actor)
        .await
        .map(|views| {
            views
                .into_iter()
                .filter(|view| view.state == ConnectionState::Connected)
                .map(|view| view.provider.name())
                .collect()
        })
        .unwrap_or_default()
}

// ---- Turns ----

/// The decision for an Auto turn in an existing session: the session's own
/// provider (it is immutable), its own catalogue, and its current selection as
/// both the fallback and what a brief follow-up keeps.
pub(super) async fn route_turn(
    state: &ServerState,
    actor: &OrgContext,
    session: &AgentSession,
    prompt: &str,
    attached_files: usize,
    history: &[AgentTurn],
) -> Route {
    let current = Selection {
        provider: session.provider.clone(),
        model: session.model.clone(),
        effort: session.effort.clone(),
        fast: session.fast,
    };
    let fell_back = |reason| Route {
        selection: current.clone(),
        outcome: Outcome::Fallback(reason),
        router: None,
        label: None,
        complexity: None,
    };
    let Some(provider) = Provider::parse(&session.provider).map(Provider::name) else {
        return fell_back(FallbackReason::NoCandidates);
    };
    // A chat cannot change provider, and every model of a provider draws on the
    // same plan: a spent provider is reported, not routed around.
    if state.agent_router.status() == routing::RouterStatus::Ready
        && usage_of(state, actor, provider).availability.is_blocked()
    {
        return fell_back(FallbackReason::UsageLimited);
    }
    let catalogue = catalogue(state, actor, provider).await;
    let mut candidates = routing::candidates(&[catalogue], &[provider]);
    label_usage(state, actor, &mut candidates);
    // The newest completed exchange: what a follow-up most likely continues.
    let previous = history.last().map(|turn| Previous {
        request: &turn.prompt,
        reply: &turn.response,
    });
    let ask = Ask {
        prompt,
        attached_files,
        earlier_turns: history.len(),
        previous,
    };
    // Only a real decision counts against the limit: a brief follow-up keeps
    // its selection without asking anyone.
    if state.agent_router.status() == routing::RouterStatus::Ready
        && !routing::is_brief_follow_up(&ask)
        && !admit(state, actor)
    {
        return fell_back(FallbackReason::Throttled);
    }
    let route = state
        .agent_router
        .route(ask, &candidates, current.clone())
        .await;
    confirm(state, actor, route, current).await
}

/// One decision for this person, if the per-person and deployment-wide limits
/// allow it. Manual selections and execution are never limited here.
fn admit(state: &ServerState, actor: &OrgContext) -> bool {
    state
        .agent_decisions
        .admit(actor.org_id, actor.identity.user_id, state.clock.now_unix())
}

// ---- Supervision ----

/// Whether a routed turn is supervised, and by what.
///
/// Only a ROUTED decision whose complexity reaches the operator's threshold
/// is considered: simple work, brief follow-ups and fallbacks never pay for a
/// supervisor. The supervisor is Claude (the only provider with an enforced
/// tool-free mode) on the operator's configured model, checked against the
/// caller's OWN Claude catalogue, run on the caller's OWN connected Claude
/// credential — never anyone else's, and never the executor's credential for a
/// different provider. Anything missing is reported as unavailable, and the
/// turn runs unsupervised exactly as it would have.
pub(super) async fn supervision_for(
    state: &ServerState,
    actor: &OrgContext,
    route: Option<&Route>,
) -> (SupervisionPlan, Option<SupervisionDto>) {
    let config = &state.config.supervision;
    let none = SupervisionPlan {
        planner: None,
        reviewer: None,
        limits: Limits::default(),
    };
    let complexity = route
        .filter(|route| route.outcome == Outcome::Routed)
        .and_then(|route| route.complexity);
    let needed = config.mode != SupervisionMode::Off
        && complexity.is_some_and(|level| level >= config.min_complexity);
    if !needed {
        return (none, None);
    }
    resolve_supervision(state, actor).await
}

/// The supervisor for a turn already judged to need one: the caller's own
/// connected Claude on the operator's model, or the honest reason there is
/// none. Also used to re-supervise the retry of a supervised turn.
pub(super) async fn resolve_supervision(
    state: &ServerState,
    actor: &OrgContext,
) -> (SupervisionPlan, Option<SupervisionDto>) {
    let config = &state.config.supervision;
    let none = SupervisionPlan {
        planner: None,
        reviewer: None,
        limits: Limits::default(),
    };
    if config.mode == SupervisionMode::Off {
        return (none, None);
    }
    let unavailable = |reason| {
        Some(SupervisionDto {
            state: "unavailable",
            phases: None,
            model: None,
            reason: Some(reason),
        })
    };
    if !state.agents.is_available()
        || !crate::neev::agents::execution::tool_free_supported("claude")
    {
        return (none, unavailable("unsupported"));
    }
    let Ok(Some(handle)) = intents::credential(state, actor, Provider::Claude).await else {
        return (none, unavailable("no_claude_connection"));
    };
    if !capabilities::known_model("claude", &config.model)
        || capabilities::validated_for(
            state,
            actor,
            "claude",
            &config.model,
            config.effort.as_deref(),
            false,
        )
        .await
        .is_err()
    {
        return (none, unavailable("no_supervisor_model"));
    }
    let supervisor = Supervisor {
        provider: "claude".into(),
        model: config.model.clone(),
        effort: config.effort.clone(),
        credential_handle: Some(handle),
    };
    let plan = SupervisionPlan {
        planner: (config.mode == SupervisionMode::PlanReview).then(|| supervisor.clone()),
        reviewer: Some(supervisor),
        limits: Limits {
            plan_timeout: std::time::Duration::from_millis(config.plan_timeout_ms),
            review_timeout: std::time::Duration::from_millis(config.review_timeout_ms),
            ..Limits::default()
        },
    };
    (
        plan,
        Some(SupervisionDto {
            state: "active",
            phases: Some(config.mode.code()),
            model: Some(config.model.clone()),
            reason: None,
        }),
    )
}

// ---- Shared ----

async fn catalogue(
    state: &ServerState,
    actor: &OrgContext,
    provider: &'static str,
) -> ProviderCapabilities {
    if acp_providers::is_acp(provider) {
        capabilities::provider_capabilities(provider)
    } else {
        capabilities::provider_for(state, actor, provider).await
    }
}

/// A routed selection is held to the same check a manual one is. It came from
/// the allowlist, so this should always pass; if it ever does not, the manual
/// selection runs and the reason says why.
async fn confirm(
    state: &ServerState,
    actor: &OrgContext,
    route: Route,
    fallback: Selection,
) -> Route {
    if route.outcome != Outcome::Routed {
        return route;
    }
    let selection = &route.selection;
    match capabilities::validated_for(
        state,
        actor,
        &selection.provider,
        &selection.model,
        selection.effort.as_deref(),
        selection.fast,
    )
    .await
    {
        Ok(_) => route,
        Err(()) => Route {
            selection: fallback,
            outcome: Outcome::Fallback(FallbackReason::Malformed),
            router: route.router,
            label: None,
            complexity: None,
        },
    }
}

/// What a new chat runs when Auto cannot decide: the draft's manual selection
/// when it is runnable — connected, accepted by `validated_for`, and not spent
/// — otherwise the first connected provider with usage left, at its own
/// default. Never a selection the caller could not make by hand.
async fn manual_fallback(
    state: &ServerState,
    actor: &OrgContext,
    input: &RouteInput,
    catalogues: &[ProviderCapabilities],
) -> Selection {
    let blocked = |provider: &str| usage_of(state, actor, provider).availability.is_blocked();
    let mut chosen: Option<Selection> = None;
    if catalogues
        .iter()
        .any(|catalogue| catalogue.provider == input.provider)
    {
        if let Ok((effort, fast)) = capabilities::validated_for(
            state,
            actor,
            &input.provider,
            &input.model,
            input.effort.as_deref(),
            input.fast,
        )
        .await
        {
            chosen = Some(Selection {
                provider: input.provider.clone(),
                model: input.model.clone(),
                effort,
                fast,
            });
        }
    }
    if let Some(selection) = chosen.as_ref().filter(|s| !blocked(&s.provider)) {
        return selection.clone();
    }
    let default_of = |catalogue: &ProviderCapabilities| Selection {
        provider: catalogue.provider.to_owned(),
        model: catalogue
            .models
            .iter()
            .find(|model| model.default)
            .or_else(|| catalogue.models.first())
            .map_or_else(|| "default".to_owned(), |model| model.id.clone()),
        effort: None,
        fast: false,
    };
    if let Some(open) = catalogues
        .iter()
        .find(|catalogue| !blocked(catalogue.provider))
    {
        return default_of(open);
    }
    // Everything is spent: the person's own valid choice, or the first
    // provider, and the reason says why.
    chosen.unwrap_or_else(|| default_of(&catalogues[0]))
}
