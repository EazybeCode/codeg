//! The coordinator half of provider sign-in: turning a stored login intent into
//! a real effect on the local supervisor, and a supervisor's answer back into
//! stored connection state.
//!
//! `intents::begin` only records that a user *wants* to sign in; without this
//! module the attempt sits `pending` until it expires. Here the recorded ticket
//! is handed to the supervisor (`start_login`), its progress is polled
//! (`observe_login`), the user's pasted authorization code is delivered
//! (`submit_code`), and a prepared login is committed (`settle_login`) through
//! `completion::complete`, which is what finally stores the user's own
//! credential handle and flips the connection to `connected`.
//!
//! Two deliberate properties:
//! - **The supervisor is a port, not a hard dependency.** Production talks to
//!   it over the authenticated unix transport; tests drive the same dispatch
//!   with a scripted channel. Nothing here needs a real provider login to be
//!   exercised.
//! - **Nothing is claimed that was not done.** With no supervisor configured
//!   every effect returns `Unavailable` and the connection stays honestly
//!   `connecting`; no code path invents a `connected` state.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::control_dto::{
    AuthorizationCode, Coordinator, Counter, Id, LoginBinding, LoginChallenge, LoginSnapshot,
    ObserveLoginBody, Request, RequestBody, ResultBody, Scope, SessionId, StartLoginBody,
    SubmitCodeBody,
};
use super::control_dto::{LoginTicketDto, Provider as WireProvider};
use super::types::{AgentError, LoginTicket, Provider};
use crate::neev::startup::ServerState;
use sea_orm::ConnectionTrait;

/// Fixed, sanitized supervisor failure categories. No path, key or peer detail
/// ever reaches a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisorError {
    /// No supervisor is configured/reachable, or it refused the exchange.
    Unavailable,
    /// The supervisor answered with something outside the agreed contract.
    Invalid,
    /// The login this effect names is not known to the supervisor.
    NotFound,
    /// A relayed direct-sign-in callback did not match its attempt (state or
    /// shape). The attempt is still alive; the CLI was never touched.
    Rejected,
    /// The attempt is not in a state that accepts this (e.g. not awaiting a
    /// browser callback, or already answered).
    Conflict,
}

impl From<SupervisorError> for AgentError {
    fn from(value: SupervisorError) -> Self {
        match value {
            SupervisorError::NotFound => Self::NotFound,
            SupervisorError::Rejected => Self::InvalidInput,
            SupervisorError::Conflict => Self::Conflict,
            SupervisorError::Invalid | SupervisorError::Unavailable => Self::Unavailable,
        }
    }
}

/// One exchange with the supervisor.
///
/// A concrete enum rather than a trait object: the transport's `exchange` is an
/// async method, and this mirrors how `AgentRuntime` keeps the production and
/// test implementations apart without boxing.
pub(crate) enum Supervisor {
    /// Production default. Every effect is refused, so a deployment that has
    /// not provisioned a supervisor cannot silently appear to sign users in.
    Unavailable,
    /// The authenticated local transport.
    Live(LiveSupervisor),
    #[cfg(test)]
    Scripted(ScriptedSupervisor),
}

impl Supervisor {
    pub(crate) fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    async fn exchange(&self, request: Request) -> Result<super::control_dto::Response, SupervisorError> {
        match self {
            Self::Unavailable => Err(SupervisorError::Unavailable),
            Self::Live(live) => live.exchange(request).await,
            #[cfg(test)]
            Self::Scripted(scripted) => scripted.exchange(request).await,
        }
    }
}

/// Everything needed to reach and authenticate to the local supervisor.
pub(crate) struct LiveSupervisor {
    deployment: super::control_transport::ControlDeployment,
    coordinator: Mutex<super::control_coordinator::CoordinatorLease>,
    /// THIS coordinator's own identity, minted once per process and stable for
    /// its lifetime.
    ///
    /// Deliberately NOT the supervisor's boot id. The channel correlates every
    /// response against the coordinator it was opened with, so overwriting a
    /// request's coordinator with the peer's identity would make every live
    /// exchange fail that check. Two identities are in play: ours, which we
    /// assert, and the supervisor's, which we only ever *learn* from a verified
    /// server hello.
    boot_id: Id,
}

impl LiveSupervisor {
    pub(crate) fn new(
        deployment: super::control_transport::ControlDeployment,
        coordinator: super::control_coordinator::CoordinatorLease,
    ) -> Result<Self, SupervisorError> {
        Ok(Self {
            deployment,
            coordinator: Mutex::new(coordinator),
            boot_id: id(&uuid::Uuid::new_v4().simple().to_string())?,
        })
    }

    pub(crate) fn boot_id(&self) -> &Id {
        &self.boot_id
    }

    /// The epoch this coordinator currently owns. Read per exchange (not cached)
    /// so a lease that has been poisoned or moved on stops us immediately — the
    /// supervisor fences on exactly this value.
    pub(crate) fn epoch(&self) -> Result<i64, SupervisorError> {
        let mut lease = match self.coordinator.lock() {
            Ok(lease) => lease,
            Err(poisoned) => poisoned.into_inner(),
        };
        lease.checked_epoch().map_err(|_| SupervisorError::Unavailable)
    }

    async fn exchange(
        &self,
        mut request: Request,
    ) -> Result<super::control_dto::Response, SupervisorError> {
        let epoch = self.epoch()?;
        let epoch = Counter::new(epoch.to_string()).map_err(|_| SupervisorError::Invalid)?;
        // A fresh connection per exchange: the client carries one outstanding
        // request, and logins are rare enough that pooling would add failure
        // modes (stale boot id, half-closed socket) for no real gain.
        let candidate = Coordinator::new(epoch, self.boot_id.clone());
        let mut client = super::control_transport::SupervisorClient::connect(
            &self.deployment,
            candidate.clone(),
        )
        .await
        .map_err(|_| SupervisorError::Unavailable)?;
        // The request asserts OUR coordinator — the very one the channel was
        // opened with, so the response correlates. The supervisor's own boot id
        // is learned from the verified hello and is the channel's business, not
        // something we may put in our own coordinator field.
        request.coordinator = candidate;
        let response = client
            .exchange(&request)
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        client.close();
        Ok(response)
    }
}

/// Until the handshake runs we have no authenticated supervisor boot id; the
/// transport replaces it from the verified server hello before the request is
/// sent, so this value never reaches the wire.
fn placeholder_boot_id() -> Result<Id, SupervisorError> {
    Id::new("0".repeat(32)).map_err(|_| SupervisorError::Invalid)
}

/// A live sign-in this process started.
///
/// Held in memory rather than in a new table: an attempt is valid for ten
/// minutes, and a restart re-queues every pending attempt through
/// `recovery::recover_startup` anyway, so persisting it would add a migration
/// without adding a guarantee.
#[derive(Clone)]
pub(crate) struct LoginSession {
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) provider: Provider,
    pub(crate) binding: LoginBinding,
    pub(crate) expires_at: i64,
    /// The connect operation's lease owner, exactly as `intents::begin` minted
    /// and PERSISTED it.
    ///
    /// Kept rather than reconstructed because it is unguessable by design:
    /// `completion::lock_ticket` refuses any ticket whose `lease_owner` is not
    /// the one stored on the operation row, which is what stops a second
    /// coordinator from committing a login it does not hold the lease for. A
    /// synthesised value here does not weaken that gate — it fails it, so the
    /// credential the supervisor already stored would never be published.
    pub(crate) lease_owner: String,
    /// What the user must do, once the supervisor has told us.
    pub(crate) approval: Option<LoginChallenge>,
    /// The invoker's authority, captured when they asked to sign in.
    ///
    /// Carried so the background sweep can COMMIT a login the user approved
    /// after closing their tab. Without it a completed approval would sit
    /// unclaimed until the attempt expired, which is the whole point of doing
    /// this off the request path. It is still authority that can go stale:
    /// `completion::complete` rechecks it, so a revoked session cannot commit.
    pub(crate) actor: crate::neev::types::OrgContext,
    /// How many authorization codes have been submitted for this attempt.
    /// Bounded so a pasted-code field cannot be used to grind against the
    /// provider.
    pub(crate) code_attempts: u8,
    /// Direct sign-in: the `requestKey` of the ONE browser callback delivered
    /// (or being delivered) for this attempt. The same key replays; any other
    /// key is a conflict the client resolves by re-reading the attempt.
    pub(crate) callback_key: Option<String>,
}

/// What `reserve_callback` decided for one `/callback` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallbackReservation {
    /// No callback yet: this key now holds the attempt's single delivery.
    Fresh,
    /// This exact key already delivered (or is delivering): replay, send nothing.
    Replay,
    /// A different key holds it: 409, the client re-reads the attempt.
    Conflict,
    /// Not this caller's live login.
    Missing,
}

/// Reserve the attempt's single callback delivery for `key`, atomically, scoped
/// to the caller's own `(org, user)` and provider (review B2).
pub(crate) fn reserve_callback(
    registry: &LoginRegistry,
    attempt_id: &str,
    org_id: i32,
    user_id: i32,
    provider: Provider,
    key: &str,
) -> CallbackReservation {
    let mut guard = lock(registry);
    let Some(session) = guard.get_mut(attempt_id) else {
        return CallbackReservation::Missing;
    };
    if session.org_id != org_id || session.user_id != user_id || session.provider != provider {
        return CallbackReservation::Missing;
    }
    match session.callback_key.as_deref() {
        Some(held) if held == key => CallbackReservation::Replay,
        Some(_) => CallbackReservation::Conflict,
        None => {
            session.callback_key = Some(key.to_owned());
            CallbackReservation::Fresh
        }
    }
}

/// Give the reservation back after a delivery that did not reach the CLI or
/// was refused, so the user can retry (same or new key) or use the fallback.
pub(crate) fn release_callback(registry: &LoginRegistry, attempt_id: &str, key: &str) {
    let mut guard = lock(registry);
    if let Some(session) = guard.get_mut(attempt_id) {
        if session.callback_key.as_deref() == Some(key) {
            session.callback_key = None;
        }
    }
}

/// Authorization codes accepted per attempt before it must be restarted.
pub(crate) const MAX_CODE_SUBMISSIONS: u8 = 5;

/// Live sign-ins by attempt id.
pub(crate) type LoginRegistry = Arc<Mutex<HashMap<String, LoginSession>>>;

pub(crate) fn new_login_registry() -> LoginRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

fn lock(registry: &LoginRegistry) -> std::sync::MutexGuard<'_, HashMap<String, LoginSession>> {
    match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The caller's own login session, or `None`. Scoped by `(org, user)` from the
/// server-authoritative actor, so an attempt id alone is never authority.
pub(crate) fn find_login(
    registry: &LoginRegistry,
    attempt_id: &str,
    org_id: i32,
    user_id: i32,
) -> Option<LoginSession> {
    lock(registry)
        .get(attempt_id)
        .filter(|session| session.org_id == org_id && session.user_id == user_id)
        .cloned()
}

pub(crate) fn forget_login(registry: &LoginRegistry, attempt_id: &str) {
    lock(registry).remove(attempt_id);
}

/// Bound so a stream of abandoned sign-ins cannot grow the process; the oldest
/// entries are dropped, and their attempts still expire in the database.
const MAX_LIVE_LOGINS: usize = 512;

fn remember_login(registry: &LoginRegistry, attempt_id: String, session: LoginSession) {
    let mut guard = lock(registry);
    if guard.len() >= MAX_LIVE_LOGINS {
        // Drop whatever expires soonest rather than refusing a real sign-in.
        if let Some(stale) = guard
            .iter()
            .min_by_key(|(_, entry)| entry.expires_at)
            .map(|(key, _)| key.clone())
        {
            guard.remove(&stale);
        }
    }
    guard.insert(attempt_id, session);
}

fn wire_provider(provider: Provider) -> WireProvider {
    match provider {
        Provider::Claude => WireProvider::Claude,
        Provider::Codex => WireProvider::Codex,
        Provider::Cursor => WireProvider::Cursor,
        Provider::Gemini => WireProvider::Gemini,
        Provider::OpenCode => WireProvider::Opencode,
    }
}

fn counter(value: i64) -> Result<Counter, SupervisorError> {
    Counter::new(value.to_string()).map_err(|_| SupervisorError::Invalid)
}

fn owner_id(value: i32) -> Result<super::control_dto::OwnerId, SupervisorError> {
    super::control_dto::OwnerId::new(i64::from(value)).map_err(|_| SupervisorError::Invalid)
}

fn unix_sec(value: i64) -> Result<super::control_dto::UnixSec, SupervisorError> {
    super::control_dto::UnixSec::new(value).map_err(|_| SupervisorError::Invalid)
}

fn id(value: &str) -> Result<Id, SupervisorError> {
    Id::new(value.to_owned()).map_err(|_| SupervisorError::Invalid)
}

/// Build the wire binding for a login ticket. Every field comes from the
/// server's own stored ticket, never from a request.
fn binding_of(ticket: &LoginTicket) -> Result<LoginBinding, SupervisorError> {
    Ok(LoginBinding {
        scope: Scope {
            org_id: owner_id(ticket.org_id)?,
            user_id: owner_id(ticket.user_id)?,
            provider: wire_provider(ticket.provider),
        },
        connection_id: id(&ticket.connection_id)?,
        generation: counter(ticket.generation)?,
        session_id: SessionId::new(ticket.session_id.clone())
            .map_err(|_| SupervisorError::Invalid)?,
        attempt_id: id(&ticket.attempt_id)?,
        operation_id: id(&ticket.operation_id)?,
    })
}

fn request_of(body: RequestBody) -> Result<Request, SupervisorError> {
    Ok(Request {
        // Replaced by the live transport with the authenticated coordinator;
        // the scripted channel ignores it.
        coordinator: Coordinator::new(counter(1)?, placeholder_boot_id()?),
        sequence: counter(1)?,
        request_id: id(&uuid::Uuid::new_v4().simple().to_string())?,
        body,
    })
}

fn snapshot_of(response: super::control_dto::Response) -> Result<LoginSnapshot, SupervisorError> {
    match response.body {
        Some(ResultBody::ObserveLogin(snapshot))
        | Some(ResultBody::SubmitCode(snapshot))
        | Some(ResultBody::DeliverCallback(snapshot)) => Ok(snapshot),
        _ => Err(SupervisorError::Invalid),
    }
}

/// Hand a recorded login intent to the supervisor, which performs the actual
/// provider sign-in inside the user's own isolation profile.
///
/// On success the sign-in is remembered so later polls can address it. On
/// failure NOTHING is remembered — the caller settles the attempt rather than
/// leaving a login the user can never finish.
pub(crate) async fn start_login(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    ticket: &LoginTicket,
    method: Option<super::control_dto::LoginMethod>,
) -> Result<(), SupervisorError> {
    let binding = binding_of(ticket)?;
    let request = request_of(RequestBody::StartLogin(StartLoginBody {
        method,
        ticket: LoginTicketDto {
            binding: binding.clone(),
            lease_owner: id(&ticket.lease_owner)?,
            expires_at: unix_sec(ticket.expires_at)?,
        },
    }))?;
    let response = state.supervisor.exchange(request).await?;
    if !matches!(response.body, Some(ResultBody::StartLogin(_))) {
        return Err(SupervisorError::Invalid);
    }
    remember_login(
        &state.agent_logins,
        ticket.attempt_id.clone(),
        LoginSession {
            org_id: ticket.org_id,
            user_id: ticket.user_id,
            provider: ticket.provider,
            binding,
            expires_at: ticket.expires_at,
            lease_owner: ticket.lease_owner.clone(),
            approval: None,
            actor: actor.clone(),
            code_attempts: 0,
            callback_key: None,
        },
    );
    Ok(())
}

/// Record one authorization-code submission, refusing once the budget is spent.
///
/// The cap is per ATTEMPT and separate from the connect quota: five bad pastes
/// end this sign-in and the user starts a new one, rather than being able to
/// drive an unbounded number of provider exchanges from a single attempt.
pub(crate) fn charge_code_attempt(registry: &LoginRegistry, attempt_id: &str) -> Result<(), ()> {
    let mut guard = lock(registry);
    let session = guard.get_mut(attempt_id).ok_or(())?;
    if session.code_attempts >= MAX_CODE_SUBMISSIONS {
        return Err(());
    }
    session.code_attempts += 1;
    Ok(())
}

/// Advance every live login and drain every queued cleanup.
///
/// This is what makes the lifecycle independent of whoever happens to be
/// polling. A user who approves a sign-in and closes the tab still gets
/// connected; a cancelled or disconnected connection still gets torn down and
/// returns to `disconnected` instead of being stuck `disconnecting` forever.
pub(crate) async fn sweep(state: &ServerState) {
    if !state.supervisor.is_available() {
        return;
    }
    advance_live_logins(state).await;
    drain_queued_cleanups(state).await;
}

async fn advance_live_logins(state: &ServerState) {
    let sessions: Vec<LoginSession> = lock(&state.agent_logins).values().cloned().collect();
    let now = state.clock.now_unix();
    for session in sessions {
        if session.expires_at <= now {
            // The attempt's own deadline has passed; `expire_pending` settles
            // the stored row, so just stop tracking it here.
            forget_login(&state.agent_logins, binding_attempt(&session.binding));
            continue;
        }
        let actor = session.actor.clone();
        let _ = advance_login(state, &actor, &session).await;
    }
}

/// Cleanups queued but not yet performed, oldest first. Bounded per sweep so a
/// backlog cannot monopolise a tick.
const CLEANUP_BATCH: usize = 16;

async fn drain_queued_cleanups(state: &ServerState) {
    // Queued cleanups AND ones whose lease has run out.
    //
    // `claim_cleanup` will re-claim an expired lease, which is exactly the case
    // a supervisor timeout leaves behind: it was claimed, the effect never
    // finished, and the lease lapsed. Selecting only `queued` would exclude
    // those from every future sweep, so a single timeout would wedge that
    // connection permanently.
    let now = state.clock.now_unix();
    let sql = format!(
        "SELECT id FROM neev_agent_operations WHERE kind IN ('cancel','disconnect') AND (state='queued' OR (state='leased' AND lease_expires_at<=$1)) ORDER BY created_at LIMIT {CLEANUP_BATCH}"
    );
    let rows = match state
        .database
        .query_all(crate::neev::db::statement(
            &state.database,
            &sql,
            vec![now.into()],
        ))
        .await
    {
        Ok(rows) => rows,
        Err(_) => return,
    };
    let ids: Vec<String> = rows
        .iter()
        .filter_map(|row| row.try_get::<String>("", "id").ok())
        .collect();
    if !ids.is_empty() {
        drain_cleanups(state, &ids).await;
    }
}

/// Poll a sign-in. Returns the supervisor's snapshot and updates the remembered
/// approval so a subsequent poll can serve it without another round trip.
pub(crate) async fn observe_login(
    state: &ServerState,
    session: &LoginSession,
) -> Result<LoginSnapshot, SupervisorError> {
    let request = request_of(RequestBody::ObserveLogin(ObserveLoginBody {
        binding: session.binding.clone(),
    }))?;
    let snapshot = snapshot_of(state.supervisor.exchange(request).await?)?;
    record_challenge(state, session, &snapshot);
    Ok(snapshot)
}

/// Deliver the authorization code the user pasted back (Claude-style logins).
///
/// The code is bound to THIS login by the binding travelling with it, so a code
/// cannot be applied to another user's attempt even if it leaked.
pub(crate) async fn submit_code(
    state: &ServerState,
    session: &LoginSession,
    code: AuthorizationCode,
) -> Result<LoginSnapshot, SupervisorError> {
    let request = request_of(RequestBody::SubmitCode(SubmitCodeBody {
        binding: session.binding.clone(),
        code,
    }))?;
    let snapshot = snapshot_of(state.supervisor.exchange(request).await?)?;
    record_challenge(state, session, &snapshot);
    Ok(snapshot)
}

/// Relay the provider redirect's query from the user's browser to THIS login's
/// CLI loopback listener (direct-signin-contract.md). The supervisor binds it
/// to the attempt's state and delivers it once; a mismatch is `Rejected` and
/// leaves the attempt alive for a retry or the fallback.
pub(crate) async fn deliver_callback(
    state: &ServerState,
    session: &LoginSession,
    query: super::control_dto::CallbackQuery,
) -> Result<LoginSnapshot, SupervisorError> {
    let request = request_of(RequestBody::DeliverCallback(
        super::control_dto::DeliverCallbackBody {
            binding: session.binding.clone(),
            query,
        },
    ))?;
    let response = state.supervisor.exchange(request).await?;
    if let Some(error) = response.error.as_ref() {
        return Err(match error.code {
            super::control_dto::ErrorCode::Rejected => SupervisorError::Rejected,
            super::control_dto::ErrorCode::Conflict => SupervisorError::Conflict,
            super::control_dto::ErrorCode::NotFound => SupervisorError::NotFound,
            _ => SupervisorError::Unavailable,
        });
    }
    let snapshot = snapshot_of(response)?;
    record_challenge(state, session, &snapshot);
    Ok(snapshot)
}

fn record_challenge(state: &ServerState, session: &LoginSession, snapshot: &LoginSnapshot) {
    if let LoginSnapshot::AwaitingUser { challenge, .. } = snapshot {
        let mut guard = lock(&state.agent_logins);
        if let Some(entry) = guard.get_mut(binding_attempt(&session.binding)) {
            entry.approval = Some(challenge.clone());
        }
    }
}

fn binding_attempt(binding: &LoginBinding) -> &str {
    binding.attempt_id.as_str()
}

/// Where a login has got to, in the terms the HTTP surface needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoginProgress {
    /// Still working; `approval` is what the user must do, if known yet.
    Pending,
    /// The connection is now `connected` and the credential handle is stored.
    Connected,
    /// Terminal, with the stored connection settled back to a usable state.
    Failed(&'static str),
}

/// Drive a login one step: poll the supervisor, and act on what it says.
///
/// This is where a sign-in actually *finishes*. A `Prepared` snapshot carries
/// the user's own credential handle, which is committed through
/// `completion::complete` — the only path that stores a handle and marks a
/// connection `connected` — and then acknowledged to the supervisor with
/// `settle_login`, so it can release its side. A terminal failure settles the
/// attempt instead of leaving it pending until it expires.
pub(crate) async fn advance_login(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    session: &LoginSession,
) -> Result<LoginProgress, SupervisorError> {
    let snapshot = observe_login(state, session).await?;
    settle_snapshot(state, actor, session, snapshot).await
}

async fn settle_snapshot(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    session: &LoginSession,
    snapshot: LoginSnapshot,
) -> Result<LoginProgress, SupervisorError> {
    match snapshot {
        LoginSnapshot::Queued { .. }
        | LoginSnapshot::Starting { .. }
        | LoginSnapshot::AwaitingUser { .. }
        | LoginSnapshot::Verifying { .. } => Ok(LoginProgress::Pending),
        LoginSnapshot::Prepared {
            receipt_id,
            storage_handle,
            account_label,
        } => {
            commit_login(
                state,
                actor,
                session,
                receipt_id,
                storage_handle,
                account_label,
            )
            .await
        }
        // Already committed on the supervisor's side but not here: commit ours
        // too. `completion::complete` is idempotent on the stored generation,
        // so a repeat is harmless.
        LoginSnapshot::Committed {
            receipt_id,
            storage_handle,
        } => commit_login(state, actor, session, receipt_id, storage_handle, None).await,
        LoginSnapshot::Failed { reason } => {
            let code = match reason {
                super::control_dto::LoginFailure::ProviderUnavailable => "provider_unavailable",
                super::control_dto::LoginFailure::ReauthRequired => "reauth_required",
                super::control_dto::LoginFailure::UnsupportedAccount => "unsupported_account",
            };
            abandon_login(state, actor, session, code).await;
            Ok(LoginProgress::Failed(code))
        }
        LoginSnapshot::Expired => {
            abandon_login(state, actor, session, "expired").await;
            Ok(LoginProgress::Failed("expired"))
        }
        LoginSnapshot::Canceled => {
            abandon_login(state, actor, session, "canceled").await;
            Ok(LoginProgress::Failed("canceled"))
        }
        LoginSnapshot::Interrupted => {
            abandon_login(state, actor, session, "interrupted").await;
            Ok(LoginProgress::Failed("interrupted"))
        }
    }
}

/// Commit a prepared login: store the credential handle, then tell the
/// supervisor we have it.
///
/// Order matters. `completion::complete` is what makes the credential usable,
/// so it runs FIRST; only once it has committed do we acknowledge. If the
/// acknowledgement fails the user is already connected and the supervisor's
/// own reconciliation will settle its side — the reverse order could
/// acknowledge a login we never stored.
async fn commit_login(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    session: &LoginSession,
    receipt_id: Id,
    storage_handle: Id,
    account_label: Option<super::control_dto::Label>,
) -> Result<LoginProgress, SupervisorError> {
    let ticket = ticket_of(session)?;
    let label = account_label
        .map(|label| label.as_str().to_owned())
        .unwrap_or_else(|| default_account_label(session.provider));
    super::completion::complete(
        state,
        actor,
        &ticket,
        super::types::VerifiedSubscription {
            account_label: label,
            storage_handle: storage_handle.as_str().to_owned(),
        },
    )
    .await
    .map_err(|_| SupervisorError::Invalid)?;

    // Best effort: the connection is already usable, and the supervisor
    // reconciles anything we fail to acknowledge.
    let witness = super::control_dto::CommittedLoginWitnessDto {
        kind: super::control_dto::CompletedConnectKind::CompletedConnect,
        binding: session.binding.clone(),
        storage_handle,
        completed_at: unix_sec(state.clock.now_unix())?,
    };
    if let Ok(request) = request_of(RequestBody::SettleLogin(
        super::control_dto::SettleLoginBody {
            receipt_id,
            witness,
        },
    )) {
        let _ = state.supervisor.exchange(request).await;
    }
    forget_login(&state.agent_logins, binding_attempt(&session.binding));
    Ok(LoginProgress::Connected)
}

/// A login that cannot succeed. Cancel the stored attempt so the connection
/// returns to a state the user can retry from, instead of staying `connecting`
/// until the deadline.
async fn abandon_login(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
    session: &LoginSession,
    reason: &str,
) {
    let attempt_id = binding_attempt(&session.binding).to_owned();
    let _ = super::intents::cancel(
        state,
        actor,
        session.provider,
        &attempt_id,
        &format!("sv-{reason}"),
    )
    .await;
    forget_login(&state.agent_logins, &attempt_id);
}

/// A provider account label we can show when the supervisor did not give one.
/// Never invented detail — just which provider this connection is.
fn default_account_label(provider: Provider) -> String {
    match provider {
        Provider::Claude => "Claude".to_owned(),
        Provider::Codex => "Codex".to_owned(),
        Provider::Cursor => "Cursor".to_owned(),
        Provider::Gemini => "Gemini CLI".to_owned(),
        Provider::OpenCode => "OpenCode".to_owned(),
    }
}

fn ticket_of(session: &LoginSession) -> Result<LoginTicket, SupervisorError> {
    Ok(LoginTicket {
        connection_id: session.binding.connection_id.as_str().to_owned(),
        provider: session.provider,
        generation: session.binding.generation.get(),
        org_id: session.org_id,
        user_id: session.user_id,
        session_id: session.binding.session_id.as_str().to_owned(),
        attempt_id: session.binding.attempt_id.as_str().to_owned(),
        operation_id: session.binding.operation_id.as_str().to_owned(),
        lease_owner: session.lease_owner.clone(),
        expires_at: session.expires_at,
    })
}

/// Drain queued connection cleanups: claim one, ask the supervisor to tear the
/// credential volume down, and acknowledge the outcome.
///
/// Without this a cancelled or disconnected connection stays `disconnecting`
/// forever and `intents::begin` refuses a fresh sign-in — the exact wedge
/// `recovery` alone cannot clear.
pub(crate) async fn drain_cleanups(state: &ServerState, operation_ids: &[String]) -> u64 {
    let mut settled = 0;
    for operation_id in operation_ids {
        let ticket = match super::recovery::claim_cleanup(state, operation_id).await {
            Ok(Some(ticket)) => ticket,
            // Already claimed, already done, or not ours: nothing to do.
            _ => continue,
        };
        let succeeded = dispatch_cleanup(state, &ticket).await.is_ok();
        if super::recovery::finish_cleanup(state, &ticket, succeeded)
            .await
            .is_ok()
            && succeeded
        {
            settled += 1;
        }
    }
    settled
}

async fn dispatch_cleanup(
    state: &ServerState,
    ticket: &super::types::CleanupTicket,
) -> Result<(), SupervisorError> {
    let target = match &ticket.target {
        super::types::CleanupTarget::Disconnect => super::control_dto::CleanupTarget::Disconnect,
        super::types::CleanupTarget::Cancel { attempt_id } => {
            super::control_dto::CleanupTarget::Cancel {
                attempt_id: id(attempt_id)?,
            }
        }
    };
    let request = request_of(RequestBody::CleanupConnection(
        super::control_dto::CleanupConnectionBody {
            ticket: super::control_dto::CleanupTicketDto {
                scope: Scope {
                    org_id: owner_id(ticket.org_id)?,
                    user_id: owner_id(ticket.user_id)?,
                    provider: wire_provider(ticket.provider),
                },
                connection_id: id(&ticket.connection_id)?,
                operation_id: id(&ticket.operation_id)?,
                target,
                target_generation: counter(ticket.target_generation)?,
                result_generation: counter(ticket.result_generation)?,
                lease_owner: id(&ticket.lease_owner)?,
                lease_expires_at: unix_sec(ticket.lease_expires_at)?,
            },
        },
    ))?;
    let response = state.supervisor.exchange(request).await?;
    match response.body {
        Some(ResultBody::CleanupConnection(super::control_dto::CleanupResult::Completed {
            ..
        })) => Ok(()),
        Some(ResultBody::CleanupConnection(_)) => Err(SupervisorError::Unavailable),
        _ => Err(SupervisorError::Invalid),
    }
}

/// Entries requested per reconcile page, and the page ceiling. Both bounded so
/// a supervisor cannot make startup walk forever.
const RECONCILE_LIMIT: u8 = 32;
const RECONCILE_MAX_PAGES: usize = 64;

/// Re-learn what the supervisor still holds, page by page.
///
/// Taking the status epoch clears the previous owner's check records, so this
/// runs between activation and enablement: until we know which connections the
/// supervisor still has state for, admitting new checks would be guessing. It
/// reports how many entries it saw; the caller's readiness depends on it
/// returning `Ok` at all.
pub(crate) async fn reconcile(state: &ServerState) -> Result<usize, SupervisorError> {
    let mut snapshot: Option<Counter> = None;
    let mut after: Option<Id> = None;
    let mut seen = 0usize;
    for _ in 0..RECONCILE_MAX_PAGES {
        let request = request_of(RequestBody::ReconcilePage(
            super::control_dto::ReconcilePageRequest {
                // Both cursor fields travel together or not at all.
                snapshot_revision: snapshot.clone(),
                after_entry_id: after.clone(),
                limit: RECONCILE_LIMIT,
            },
        ))?;
        let page = match state.supervisor.exchange(request).await?.body {
            Some(ResultBody::ReconcilePage(page)) => page,
            _ => return Err(SupervisorError::Invalid),
        };
        seen += page.entries.len();
        // The page states its own continuation; absent means this was the last.
        match page.next_after_entry_id.clone() {
            None => return Ok(seen),
            Some(cursor) => {
                after = Some(cursor);
                snapshot = Some(page.snapshot_revision.clone());
            }
        }
    }
    // The ceiling is a guard, not an expected outcome: a supervisor that never
    // ends its pages has not reconciled, so readiness must not follow.
    Err(SupervisorError::Invalid)
}

/// What a bounded connection check concluded, in the terms `status` stores.
pub(crate) enum CheckProgress {
    /// The supervisor is still working; the caller should poll again.
    Pending,
    /// A terminal observation, ready to be finalized.
    Observed(super::status::CheckedAccountOutcome),
}

/// Ask the supervisor to verify one connection's credential, side-effect free.
///
/// The ticket is the one `status::prepare_connection_check` issued, so the
/// check is bound to a specific connection generation and storage handle: a
/// result cannot be applied to a connection that has since changed underneath
/// it, and `finalize_connection_check` re-proves that before storing anything.
pub(crate) async fn check_connection(
    state: &ServerState,
    ticket: &super::status::StatusTicket,
) -> Result<CheckProgress, SupervisorError> {
    use super::status::{CheckUnavailable, CheckedAccountOutcome};
    let request = request_of(RequestBody::CheckConnection(
        super::control_dto::CheckConnectionBody {
            ticket: super::control_dto::CheckTicketDto {
                binding: super::control_dto::ConnectionBinding {
                    scope: Scope {
                        org_id: owner_id(ticket.org_id())?,
                        user_id: owner_id(ticket.user_id())?,
                        provider: wire_provider(ticket.provider()),
                    },
                    connection_id: id(ticket.connection_id())?,
                    generation: counter(ticket.generation())?,
                },
                session_id: SessionId::new(ticket.session_id().to_owned())
                    .map_err(|_| SupervisorError::Invalid)?,
                storage_handle: id(ticket.storage_handle())?,
                check_id: id(ticket.check_id())?,
                expires_at: unix_sec(ticket.expires_at())?,
            },
        },
    ))?;
    let result = match state.supervisor.exchange(request).await?.body {
        Some(ResultBody::CheckConnection(result)) => result,
        _ => return Err(SupervisorError::Invalid),
    };
    Ok(match result {
        super::control_dto::CheckResult::Queued { .. }
        | super::control_dto::CheckResult::Checking { .. } => CheckProgress::Pending,
        super::control_dto::CheckResult::Finished {
            observed_at,
            account,
            ..
        } => {
            let observed_at = observed_at.get();
            CheckProgress::Observed(match account {
                super::control_dto::AccountStatus::Supported { account_label } => {
                    CheckedAccountOutcome::Supported {
                        account_label: account_label.as_str().to_owned(),
                        observed_at,
                    }
                }
                super::control_dto::AccountStatus::Disconnected => {
                    CheckedAccountOutcome::Disconnected { observed_at }
                }
                super::control_dto::AccountStatus::Unsupported => {
                    CheckedAccountOutcome::Unsupported { observed_at }
                }
            })
        }
        super::control_dto::CheckResult::Unavailable { reason, .. } => {
            CheckProgress::Observed(CheckedAccountOutcome::Unavailable(match reason {
                super::control_dto::CheckFailure::ProviderUnavailable => {
                    CheckUnavailable::ProviderUnavailable
                }
                super::control_dto::CheckFailure::Interrupted => CheckUnavailable::Interrupted,
                super::control_dto::CheckFailure::Expired => CheckUnavailable::Expired,
            }))
        }
    })
}

/// Adopt the status epoch this process's coordinator owns, reconciling with the
/// supervisor first. Returns `None` when there is no live supervisor to
/// reconcile against — checks then stay disabled, which is the honest state.
pub(crate) async fn adopt_status_epoch(
    state: &ServerState,
) -> Option<super::status::StatusEpochTicket> {
    let (epoch, boot_id) = match state.supervisor.as_ref() {
        Supervisor::Live(live) => (live.epoch().ok()?, live.boot_id().as_str().to_owned()),
        #[cfg(test)]
        Supervisor::Scripted(_) => (1, "1".repeat(32)),
        Supervisor::Unavailable => return None,
    };
    super::status::adopt_epoch(state, epoch, boot_id, |_ticket| async move {
        reconcile(state)
            .await
            .map(|_| ())
            .map_err(|_| super::status::StatusError::Unavailable)
    })
    .await
    .ok()
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;

/// A supervisor whose answers are scripted. Exercises the whole dispatch path
/// — request shape, snapshot handling, registry bookkeeping — without a socket,
/// a provider, or a credential.
#[cfg(test)]
pub(crate) struct ScriptedSupervisor {
    answers: Mutex<Vec<Result<super::control_dto::Response, SupervisorError>>>,
    seen: Mutex<Vec<Request>>,
}

#[cfg(test)]
impl ScriptedSupervisor {
    pub(crate) fn new(answers: Vec<Result<super::control_dto::Response, SupervisorError>>) -> Self {
        Self {
            answers: Mutex::new(answers),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    async fn exchange(
        &self,
        request: Request,
    ) -> Result<super::control_dto::Response, SupervisorError> {
        self.seen.lock().unwrap().push(request);
        let mut answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            return Err(SupervisorError::Unavailable);
        }
        answers.remove(0)
    }
}
