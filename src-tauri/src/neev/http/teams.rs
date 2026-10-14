//! HTTP surface for Microsoft Teams (`msteams`). Mounted only when
//! `config.teams` is set.
//!
//! - `POST /api/teams/messages` — the Azure Bot messaging endpoint. Every
//!   activity is authenticated (Bot Connector JWT bound to the activity's
//!   `serviceUrl` and channel) BEFORE its body is acted on, then normalized,
//!   resolved through the admin-managed bindings (`msteams::access`) and
//!   authorized on BOTH sides, fresh: Teams (Graph: Team membership, not a
//!   guest, same tenant, eligible standard channel — fail closed when it cannot
//!   be verified) and Neev (an active membership of the linked account).
//!   An accepted message is COMMITTED to the durable inbox (and, in a channel,
//!   the shared thread ledger) before this endpoint answers 200; if it cannot
//!   be stored the endpoint answers 503 and the Bot Connector retries. The
//!   worker (`msteams::worker`) then launches the run as the person who asked,
//!   through `launch_session_turn` — the web chat's own path, Auto routing
//!   included — and delivers the acknowledgement and the final answer from the
//!   durable outbox.
//!   - A channel mention belongs to a SHARED thread (`msteams::threads`); at
//!     most one run is live per thread and later messages run after it, in
//!     order, each as its own author.
//!   - A personal (1:1) message runs in that person's own Neev chat.
//!   - Card buttons (Status, Stop) act only on a run launched from the SAME
//!     conversation (personal chat, or channel + root thread) the card is in,
//!     after re-authorizing the clicker; their answer is shown to the clicker
//!     only.
//! - `GET /api/neev/teams/tab/status` — the tab's SSO probe: verifies the Entra
//!   token and reports whether the signed-in Teams user is linked. Grants nothing.
//! - `POST /api/neev/teams/link` — a signed-in Neev member links THEIR account
//!   to the Teams identity a verified SSO token names (both proofs required;
//!   guest and external accounts are refused).
//! - `/api/neev/teams/bindings…` — admin management of tenant, Team, channel
//!   and principal bindings (`msteams::bindings`); linking a Team and enabling
//!   a channel additionally require a Graph-verified Team owner.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{catalog, private_json, ApiError};
use crate::neev::agents::{runs, sessions};
use crate::neev::error::AuthError;
use crate::neev::msteams::access;
use crate::neev::msteams::activity::{
    self, Activity, CardAction, Envelope, Inbound, InboundMessage, Lifecycle, LifecycleEvent, Scope,
};
use crate::neev::msteams::bindings::{self, BindingError};
use crate::neev::msteams::codes::Refusal;
use crate::neev::msteams::config::TeamsConfig;
use crate::neev::msteams::directory::Directory;
use crate::neev::msteams::inbox;
use crate::neev::msteams::keys::KeySource;
use crate::neev::msteams::threads::{self, Admission};
use crate::neev::msteams::{bot, cards, jwt, TeamsRuntime, TAB_SCOPE};
use crate::neev::startup::ServerState;

/// Activities are small; a card submit with data is the largest we read.
const ACTIVITY_BODY_LIMIT: usize = 256 * 1024;
const ADMIN_BODY_LIMIT: usize = 4 * 1024;

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn jwt_error(error: jwt::JwtError) -> ApiError {
    match error {
        jwt::JwtError::KeysUnavailable => ApiError::Unavailable,
        _ => ApiError::Unauthorized,
    }
}

fn teams_config(state: &ServerState) -> Result<&TeamsConfig, ApiError> {
    state.config.teams.as_ref().ok_or(ApiError::NotFound)
}

/// The Neev half of authorizing a member, read-only: the account is enabled
/// and an active member of the org right now.
async fn neev_member(state: &ServerState, org_id: i32, user_id: i32) -> Result<(), Refusal> {
    crate::neev::auth::session::member_active(&state.database, user_id, org_id)
        .await
        .map_err(|error| match error {
            AuthError::Unavailable => Refusal::Unavailable,
            _ => Refusal::NotAMember,
        })
}

pub(super) async fn messages(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let config = teams_config(&state)?;
    let runtime = crate::neev::msteams::runtime(config).ok_or(ApiError::Unavailable)?;
    let token = bearer(request.headers())
        .ok_or(ApiError::Unauthorized)?
        .to_owned();
    let unverified = jwt::decode(&token).map_err(jwt_error)?;
    let body = axum::body::to_bytes(request.into_body(), ACTIVITY_BODY_LIMIT)
        .await
        .map_err(|_| ApiError::Invalid)?;
    let activity: Activity = serde_json::from_slice(&body).map_err(|_| ApiError::Invalid)?;
    let _permit = catalog::permit(&state)?;
    let key = runtime
        .connector_keys
        .key(&unverified.kid)
        .await
        .map_err(jwt_error)?;
    jwt::verify_connector(
        unverified,
        &key,
        &config.bot_app_id,
        &activity.service_url,
        &activity.channel_id,
        state.clock.now_unix(),
    )
    .map_err(jwt_error)?;
    // Authenticated from here on. A serviceUrl we would not send our bot token
    // to cannot be answered, so it is refused rather than half-handled.
    if !bot::service_url_allowed(&activity.service_url, &config.service_url_hosts) {
        return Err(ApiError::Forbidden);
    }
    let inbound = activity::normalize(&activity).map_err(|_| ApiError::Invalid)?;
    handle(&state, &runtime, inbound).await
}

/// Everything after authentication, shared with the end-to-end tests.
async fn handle(state: &Arc<ServerState>, runtime: &Arc<TeamsRuntime>, inbound: Inbound) -> Result<Response, ApiError> {
    let now = state.clock.now_unix();
    match inbound {
        Inbound::Ignored => Ok(StatusCode::OK.into_response()),
        Inbound::Installed(envelope) => {
            remember_service_url(state, &envelope).await;
            tokio::spawn(welcome(state.clone(), runtime.clone(), envelope));
            Ok(StatusCode::OK.into_response())
        }
        Inbound::Lifecycle(event) => {
            lifecycle(state, &event, now).await?;
            Ok(StatusCode::OK.into_response())
        }
        Inbound::Message(message) => {
            remember_service_url(state, &message.envelope).await;
            match accept(state, &runtime.directory, &message).await {
                // Stored: the worker takes it from here.
                Ok(Accepted::Recorded) => runtime.wake.notify_one(),
                Ok(Accepted::Replay) | Ok(Accepted::Silent) => {}
                // A refusal carries no work; telling the person is best effort.
                Ok(Accepted::Refused(reason)) => {
                    tracing::info!(code = reason.code(), tenant = %message.envelope.tenant_id, "teams message refused");
                    tokio::spawn(send_direct(runtime.clone(), message.envelope.clone(), cards::refusal(reason)));
                }
                // Not stored: answer 503 so the Bot Connector delivers it again.
                Err(error) => return Err(error),
            }
            Ok(StatusCode::OK.into_response())
        }
        Inbound::Action(action) if action.invoke => {
            let text = match card_action(state, &runtime.directory, &action).await {
                Ok(text) => text,
                Err(reason) => reason.message().to_owned(),
            };
            Ok(private_json(cards::invoke_message(&text)))
        }
        Inbound::Action(action) => {
            let text = match card_action(state, &runtime.directory, &action).await {
                Ok(text) => text,
                Err(Refusal::Suspended) => return Ok(StatusCode::OK.into_response()),
                Err(reason) => reason.message().to_owned(),
            };
            tokio::spawn(send_direct(runtime.clone(), action.envelope.clone(), cards::notice(&text)));
            Ok(StatusCode::OK.into_response())
        }
    }
}

/// A connected tenant's Bot Connector service URL, from a signed activity.
async fn remember_service_url(state: &ServerState, envelope: &Envelope) {
    if let Err(error) = bindings::record_service_url(&state.database, &envelope.tenant_id, &envelope.service_url, state.clock.now_unix()).await {
        tracing::warn!(%error, "teams service url not recorded");
    }
}

/// Removal, deletion and archive events suspend the binding they name.
async fn lifecycle(state: &ServerState, event: &Lifecycle, now: i64) -> Result<(), ApiError> {
    let changed = match &event.event {
        LifecycleEvent::TeamSuspended { team_id } => bindings::suspend_team(&state.database, &event.tenant_id, team_id, now).await,
        LifecycleEvent::ChannelDeleted { channel_id } => bindings::suspend_channel(&state.database, &event.tenant_id, channel_id, now).await,
    }
    .map_err(|_| ApiError::Unavailable)?;
    if changed > 0 {
        tracing::info!(tenant = %event.tenant_id, ?event.event, "teams binding suspended by a Teams event");
    }
    Ok(())
}

/// Best-effort reply that carries no work (refusals, button answers).
async fn send_direct(runtime: Arc<TeamsRuntime>, envelope: Envelope, card: Value) {
    let activity = cards::message(card, Some(&envelope.activity_id));
    if let Err(error) = runtime
        .connector
        .reply(&envelope.service_url, &envelope.conversation_id, &envelope.activity_id, &activity)
        .await
    {
        tracing::warn!(%error, tenant = %envelope.tenant_id, "teams reply not delivered");
    }
}

async fn welcome(state: Arc<ServerState>, runtime: Arc<TeamsRuntime>, envelope: Envelope) {
    let Some(config) = state.config.teams.as_ref() else {
        return;
    };
    if bot::check_recipient(&envelope, &config.bot_app_id).is_err() {
        return;
    }
    // Suspended or unconnected tenants get no message at all: Neev sends
    // nothing to a tenant whose binding does not allow it.
    match access::tenant(&state.database, &envelope.tenant_id).await {
        Ok(_) => {}
        Err(Refusal::UnmappedTenant) => {
            send_direct(runtime, envelope.clone(), cards::welcome(&envelope.from_name, false)).await;
            return;
        }
        Err(_) => return,
    }
    send_direct(runtime, envelope.clone(), cards::welcome(&envelope.from_name, true)).await;
}

/// What happened to an addressed message before it was acknowledged.
#[derive(Debug, PartialEq, Eq)]
enum Accepted {
    /// Committed to the inbox; the worker launches it.
    Recorded,
    /// An earlier delivery of this activity was already recorded.
    Replay,
    /// A suspended binding: no run, no reply.
    Silent,
    /// Refused; the person is told why.
    Refused(Refusal),
}

/// Authorize and record an addressed message. `Err` only when it could not be
/// STORED (the Bot Connector must retry); every authorization outcome is `Ok`.
async fn accept(state: &Arc<ServerState>, directory: &Directory, message: &InboundMessage) -> Result<Accepted, ApiError> {
    match authorize_and_record(state, directory, message).await {
        Ok(accepted) => Ok(accepted),
        Err(Stop::Refused(Refusal::Suspended)) => Ok(Accepted::Silent),
        Err(Stop::Refused(reason)) => Ok(Accepted::Refused(reason)),
        Err(Stop::Storage) => Err(ApiError::Unavailable),
    }
}

enum Stop {
    Refused(Refusal),
    Storage,
}

impl From<Refusal> for Stop {
    fn from(reason: Refusal) -> Self {
        Self::Refused(reason)
    }
}

async fn authorize_and_record(state: &Arc<ServerState>, directory: &Directory, message: &InboundMessage) -> Result<Accepted, Stop> {
    let config = state.config.teams.as_ref().ok_or(Refusal::Unavailable)?;
    let envelope = &message.envelope;
    bot::check_recipient(envelope, &config.bot_app_id)?;
    // Checked before anything is written: without a runtime the turn could
    // only ever be settled as refused, which is worse than saying so.
    if !state.agents.is_available() {
        return Err(Refusal::RuntimeUnavailable.into());
    }
    let object_id = envelope.from_aad.as_deref().ok_or(Refusal::GuestSender)?;
    let now = state.clock.now_unix();
    match &envelope.scope {
        Scope::GroupChat => Err(Refusal::GroupChat.into()),
        Scope::Personal => {
            let tenant = access::tenant(&state.database, &envelope.tenant_id).await?;
            let user_id = access::principal(&state.database, &envelope.tenant_id, tenant.org_id, Some(object_id)).await?;
            neev_member(state, tenant.org_id, user_id).await?;
            let recorded = inbox::admit_personal(
                &state.database,
                &inbox::Personal {
                    org_id: tenant.org_id,
                    tenant_id: &envelope.tenant_id,
                    conversation_id: &envelope.conversation_id,
                    activity_id: &envelope.activity_id,
                    service_url: &envelope.service_url,
                    user_id,
                    aad_object_id: object_id,
                    author_name: &envelope.from_name,
                    body: &message.text,
                },
                now,
            )
            .await
            .map_err(|_| Stop::Storage)?;
            Ok(if recorded { Accepted::Recorded } else { Accepted::Replay })
        }
        Scope::Channel { team_id, channel_id } => {
            // The Team id Teams reported must be the Team the channel is bound
            // under; an activity without one cannot be placed and is refused.
            let team_id = team_id.as_deref().ok_or(Refusal::UnmappedChannel)?;
            let access = access::channel(&state.database, &envelope.tenant_id, Some(team_id), channel_id).await?;
            let key = threads::thread_key(envelope).ok_or(Refusal::UnknownThread)?;
            let user_id = access::principal(&state.database, &envelope.tenant_id, access.tenant.org_id, Some(object_id)).await?;
            // BOTH sides, fresh, BEFORE the message enters the shared ledger:
            // a removed Team member, a guest, an ineligible channel or a removed
            // Neev member contributes nothing to the thread.
            access::teams_member(directory, &access, Some(object_id), false).await?;
            neev_member(state, access.tenant.org_id, user_id).await?;
            let admission = threads::admit(
                &state.database,
                &key,
                &threads::Inbound {
                    org_id: access.tenant.org_id,
                    team_id,
                    conversation_id: &envelope.conversation_id,
                    activity_id: &envelope.activity_id,
                    service_url: &envelope.service_url,
                    author_user_id: user_id,
                    author_object_id: object_id,
                    author_name: &envelope.from_name,
                    body: &message.text,
                },
                now,
            )
            .await
            .map_err(|_| Stop::Storage)?;
            match admission {
                Admission::Replay => Ok(Accepted::Replay),
                Admission::Start { .. } => Ok(Accepted::Recorded),
                Admission::Queued { .. } => {
                    // Durable like any other notice; a lost note is not lost work.
                    if let Ok(Some(item)) = inbox::by_activity(&state.database, &envelope.tenant_id, &envelope.conversation_id, &envelope.activity_id).await {
                        let cause = activity::digest(&[&item.tenant_id, &item.conversation_id, &item.activity_id]);
                        if let Err(error) = crate::neev::msteams::outbox::enqueue(
                            &state.database,
                            &crate::neev::msteams::outbox::id(&cause, "queued"),
                            &item,
                            crate::neev::msteams::outbox::Kind::Notice,
                            None,
                            Some(&cards::queued()),
                            now,
                        )
                        .await
                        {
                            tracing::warn!(%error, "teams queued notice not stored");
                        }
                    }
                    Ok(Accepted::Recorded)
                }
            }
        }
    }
}

/// A card button, answered to the clicker. The run must have been launched
/// from THIS conversation (personal chat, or this channel's thread), and the
/// clicker must pass both the Teams and the Neev checks now. Status is open to
/// any such member and never shows answer content (the answer is delivered to
/// the thread itself); Stop only to the member the run executes as.
async fn card_action(state: &Arc<ServerState>, directory: &Directory, action: &CardAction) -> Result<String, Refusal> {
    let config = state.config.teams.as_ref().ok_or(Refusal::Unavailable)?;
    let envelope = &action.envelope;
    bot::check_recipient(envelope, &config.bot_app_id)?;
    if action.verb != cards::STATUS_VERB && action.verb != cards::STOP_VERB {
        return Err(Refusal::UnknownAction);
    }
    let run_id = action
        .data
        .get("runId")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or(Refusal::UnknownAction)?;
    let object_id = envelope.from_aad.as_deref().ok_or(Refusal::GuestSender)?;
    let tenant = access::tenant(&state.database, &envelope.tenant_id).await?;
    let user_id = access::principal(&state.database, &envelope.tenant_id, tenant.org_id, Some(object_id)).await?;
    let item = inbox::by_run(&state.database, tenant.org_id, run_id)
        .await
        .map_err(|_| Refusal::Unavailable)?
        .ok_or(Refusal::RunNotFound)?;
    // Provenance: the run belongs to exactly the conversation this card is in.
    match (&item.channel, &envelope.scope) {
        (None, Scope::Personal) if item.tenant_id == envelope.tenant_id && item.conversation_id == envelope.conversation_id => {}
        (Some(origin), Scope::Channel { team_id, channel_id }) => {
            let clicked = threads::thread_key(envelope).ok_or(Refusal::RunNotFound)?;
            if clicked != origin.key || team_id.as_deref() != Some(origin.team_id.as_str()) {
                return Err(Refusal::RunNotFound);
            }
            let access = access::channel(&state.database, &envelope.tenant_id, Some(&origin.team_id), channel_id).await?;
            access::teams_member(directory, &access, Some(object_id), false).await?;
        }
        _ => return Err(Refusal::RunNotFound),
    }
    neev_member(state, tenant.org_id, user_id).await?;
    if item.channel.is_none() && item.user_id != user_id {
        return Err(Refusal::RunNotFound);
    }
    let turn = sessions::turn_by_run_id(&state.database, run_id)
        .await
        .map_err(|_| Refusal::Unavailable)?
        .ok_or(Refusal::RunNotFound)?;
    if action.verb == cards::STOP_VERB {
        if item.user_id != user_id {
            return Err(Refusal::NotRunOwner);
        }
        if turn.status == sessions::TurnStatus::Running {
            return Ok(match runs::find(&state.agent_runs, run_id) {
                Some(run) if run.org_id == tenant.org_id && run.owner_id == user_id => {
                    run.cancel();
                    "Stopping. The outcome will be posted here.".to_owned()
                }
                // Not live in this process: say so rather than claim a stop.
                _ => "This request could not be stopped from here. Try again shortly.".to_owned(),
            });
        }
    }
    Ok(cards::status_text(turn.status.name()).to_owned())
}

/// Verify a Teams tab SSO token for this app's delegated scope.
async fn verified_entra(
    state: &ServerState,
    config: &TeamsConfig,
    token: &str,
) -> Result<jwt::EntraUser, ApiError> {
    let app_id_uri = config.tab_app_id_uri.as_deref().ok_or(ApiError::NotFound)?;
    let runtime = crate::neev::msteams::runtime(config).ok_or(ApiError::Unavailable)?;
    let unverified = jwt::decode(token).map_err(jwt_error)?;
    let key = runtime
        .entra_keys
        .key(&unverified.kid)
        .await
        .map_err(jwt_error)?;
    jwt::verify_entra(
        unverified,
        &key,
        &[app_id_uri, config.bot_app_id.as_str()],
        TAB_SCOPE,
        state.clock.now_unix(),
    )
    .map_err(jwt_error)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TabStatusDto {
    display_name: String,
    tenant_connected: bool,
    linked: bool,
    /// Why the user is not linked, as a stable `codes::Refusal` code.
    code: Option<&'static str>,
    /// Whether Teams membership/channel checks can be verified at all (the
    /// Graph authority is configured). When false, channels refuse every
    /// request; 1:1 chats still work.
    channel_checks_available: bool,
}

pub(super) async fn tab_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let config = teams_config(&state)?;
    let token = bearer(&headers).ok_or(ApiError::Unauthorized)?;
    let _permit = catalog::permit(&state)?;
    let user = verified_entra(&state, config, token).await?;
    let (tenant_connected, linked, code) = match access::tenant(&state.database, &user.tenant_id).await {
        Ok(tenant) => match access::principal(&state.database, &user.tenant_id, tenant.org_id, Some(&user.object_id)).await {
            Ok(_) => (true, true, None),
            Err(reason) => (true, false, Some(reason.code())),
        },
        Err(Refusal::Unavailable) => return Err(ApiError::Unavailable),
        Err(reason) => (false, false, Some(reason.code())),
    };
    let channel_checks_available = crate::neev::msteams::runtime(config)
        .is_some_and(|runtime| runtime.directory.graph_configured());
    Ok(private_json(TabStatusDto {
        display_name: user.name,
        tenant_connected,
        linked,
        code,
        channel_checks_available,
    }))
}

// ---------------------------------------------------------------- linking

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkInput {
    /// The Teams tab SSO access token (`access_as_user`) of the person linking.
    sso_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkedDto {
    tenant_id: String,
    aad_object_id: String,
    user_id: i32,
}

/// Link the signed-in Neev member (session cookie + CSRF) to the Teams
/// identity in a verified SSO token. Neither proof alone is enough: the cookie
/// cannot name a Teams identity, and the token cannot name a Neev account.
pub(super) async fn link(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let config = teams_config(&state)?;
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<LinkInput>(&state, request, 20 * 1024).await?;
    let user = verified_entra(&state, config, &input.sso_token).await?;
    // A B2B guest or external account never becomes a Neev principal, even
    // though it has an object id in this tenant.
    if user.external {
        return Ok(refusal_response(Refusal::GuestSender));
    }
    let linked = bindings::link_principal(&state, &actor, &user.tenant_id, &user.object_id).await?;
    Ok(private_json(LinkedDto {
        tenant_id: linked.tenant_id,
        aad_object_id: linked.aad_object_id,
        user_id: linked.user_id,
    }))
}

// ---------------------------------------------------------------- admin

/// A Teams verification refusal on an admin or link call: a stable code the
/// admin surface can explain, with the matching HTTP status.
fn refusal_response(reason: Refusal) -> Response {
    let status = match reason {
        Refusal::Unavailable | Refusal::TeamsUnverifiable => StatusCode::SERVICE_UNAVAILABLE,
        Refusal::UnmappedChannel => StatusCode::NOT_FOUND,
        _ => StatusCode::FORBIDDEN,
    };
    let mut response = private_json(serde_json::json!({"error": reason.code(), "message": reason.message()}));
    *response.status_mut() = status;
    response
}

fn binding_response<T: Serialize>(result: Result<T, BindingError>) -> Result<Response, ApiError> {
    match result {
        Ok(found) => Ok(private_json(found)),
        Err(BindingError::Content(error)) => Err(error.into()),
        Err(BindingError::Refused(reason)) => Ok(refusal_response(reason)),
    }
}

fn directory(state: &ServerState) -> Result<Directory, ApiError> {
    let config = teams_config(state)?;
    Ok(crate::neev::msteams::runtime(config)
        .ok_or(ApiError::Unavailable)?
        .directory
        .clone())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BindingsDto {
    installations: Vec<InstallationDto>,
    teams: Vec<TeamDto>,
    channels: Vec<ChannelDto>,
    principals: Vec<PrincipalDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallationDto {
    tenant_id: String,
    enabled: bool,
    provider: String,
    model: String,
    revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamDto {
    tenant_id: String,
    team_id: String,
    graph_group_id: String,
    node_id: i32,
    enabled: bool,
    revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChannelDto {
    tenant_id: String,
    team_id: String,
    channel_id: String,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrincipalDto {
    tenant_id: String,
    aad_object_id: String,
    user_id: i32,
}

fn installation_dto(found: bindings::Installation) -> InstallationDto {
    InstallationDto {
        tenant_id: found.tenant_id,
        enabled: found.enabled,
        provider: found.provider,
        model: found.model,
        revision: found.revision.to_string(),
    }
}

fn team_dto(found: bindings::ProjectLink) -> TeamDto {
    TeamDto {
        tenant_id: found.tenant_id,
        team_id: found.team_id,
        graph_group_id: found.graph_group_id,
        node_id: found.node_id,
        enabled: found.enabled,
        revision: found.revision.to_string(),
    }
}

pub(super) async fn admin_bindings(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let actor = catalog::organization_get(&state, &headers).await?;
    let found = bindings::list(&state, &actor).await?;
    Ok(private_json(BindingsDto {
        installations: found.installations.into_iter().map(installation_dto).collect(),
        teams: found.projects.into_iter().map(team_dto).collect(),
        channels: found
            .channels
            .into_iter()
            .map(|channel| ChannelDto {
                tenant_id: channel.tenant_id,
                team_id: channel.team_id,
                channel_id: channel.channel_id,
                enabled: channel.enabled,
            })
            .collect(),
        principals: found
            .principals
            .into_iter()
            .map(|principal| PrincipalDto {
                tenant_id: principal.tenant_id,
                aad_object_id: principal.aad_object_id,
                user_id: principal.user_id,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectInput {
    tenant_id: String,
    provider: String,
    model: String,
}

pub(super) async fn admin_connect(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<ConnectInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    let found = bindings::connect_tenant(&state, &actor, &input.tenant_id, &input.provider, &input.model).await?;
    Ok(private_json(installation_dto(found)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TenantStateInput {
    tenant_id: String,
    enabled: bool,
}

pub(super) async fn admin_tenant_state(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<TenantStateInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    let found = bindings::set_tenant_enabled(&state, &actor, &input.tenant_id, input.enabled).await?;
    Ok(private_json(installation_dto(found)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamInput {
    tenant_id: String,
    team_id: String,
    node_id: i32,
}

pub(super) async fn admin_link_team(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<TeamInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    let directory = directory(&state)?;
    binding_response(
        bindings::link_team(&state, &directory, &actor, &input.tenant_id, &input.team_id, input.node_id)
            .await
            .map(team_dto),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeamStateInput {
    tenant_id: String,
    team_id: String,
    enabled: bool,
}

pub(super) async fn admin_team_state(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<TeamStateInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    let found = bindings::set_team_enabled(&state, &actor, &input.tenant_id, &input.team_id, input.enabled).await?;
    Ok(private_json(team_dto(found)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelInput {
    tenant_id: String,
    team_id: String,
    channel_id: String,
    enabled: bool,
}

pub(super) async fn admin_channel(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<ChannelInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    let directory = directory(&state)?;
    binding_response(
        bindings::set_channel(&state, &directory, &actor, &input.tenant_id, &input.team_id, &input.channel_id, input.enabled)
            .await
            .map(|found| ChannelDto {
                tenant_id: found.tenant_id,
                team_id: found.team_id,
                channel_id: found.channel_id,
                enabled: found.enabled,
            }),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnlinkInput {
    tenant_id: String,
    user_id: i32,
}

pub(super) async fn unlink(
    State(state): State<Arc<ServerState>>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let _permit = catalog::permit(&state)?;
    let (input, actor) = catalog::organization_mutation::<UnlinkInput>(&state, request, ADMIN_BODY_LIMIT).await?;
    bindings::unlink_principal(&state, &actor, &input.tenant_id, input.user_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The real path end to end below the HTTP signature check: normalized
/// activities, database bindings, a programmable Graph directory, the durable
/// inbox/ledger/outbox, the worker, `launch_session_turn` on a fake runtime and
/// a REAL Bot Connector client talking to a loopback server.
#[cfg(test)]
#[path = "teams_flow_tests.rs"]
mod flow_tests;
