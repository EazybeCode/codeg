//! Durable status publication only. Host ownership and reconciled readiness remain
//! sealed until a separately reviewed child coordinator adapter exists.
#![allow(dead_code)]

use super::{
    store,
    types::{AgentError, ConnectionState, ConnectionView, Provider},
};
use crate::neev::{auth::session, db, startup::ServerState, types::OrgContext};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(crate) struct CheckedStatusEpoch {
    epoch: i64,
    boot_id: String,
}
pub(crate) struct ReconciledStatusReady {
    epoch: i64,
    boot_id: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StatusEpochTicket {
    epoch: i64,
    boot_id: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusPhase {
    Initializing,
    Ready,
    Closed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusError {
    Agent(AgentError),
    Invalid,
    Busy,
    Stale,
    Unavailable,
}
pub(crate) enum Activation {
    Activated(StatusEpochTicket),
    Existing(StatusEpochTicket, StatusPhase),
}
pub(crate) enum CheckAdmission {
    Started(StatusTicket),
    Joined(StatusTicket),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckUnavailable {
    ProviderUnavailable,
    Interrupted,
    Expired,
}
pub(crate) enum CheckedAccountOutcome {
    Supported {
        account_label: String,
        observed_at: i64,
    },
    Disconnected {
        observed_at: i64,
    },
    Unsupported {
        observed_at: i64,
    },
    Unavailable(CheckUnavailable),
}
pub(crate) enum CheckFinalized {
    Applied(ConnectionView),
    Unavailable(CheckUnavailable),
    Stale,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StatusTicket {
    epoch: i64,
    boot_id: String,
    org_id: i32,
    user_id: i32,
    provider: Provider,
    connection_id: String,
    generation: i64,
    session_id: String,
    storage_handle: String,
    check_id: String,
    issued_at: i64,
    expires_at: i64,
}

const MAX_TIME: i64 = 253_402_300_799;

fn unavailable<T>(_: T) -> StatusError {
    StatusError::Unavailable
}
fn agent(error: AgentError) -> StatusError {
    match error {
        AgentError::Unavailable => StatusError::Unavailable,
        other => StatusError::Agent(other),
    }
}
fn canonical_hex(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical_session(value: &str) -> bool {
    value.len() == 36
        && uuid::Uuid::parse_str(value).is_ok_and(|id| {
            id.to_string() == value
                && id.get_version() == Some(uuid::Version::Random)
                && id.get_variant() == uuid::Variant::RFC4122
        })
}
fn valid_epoch(epoch: i64, boot: &str) -> Result<(), StatusError> {
    if epoch > 0 && canonical_hex(boot) {
        Ok(())
    } else {
        Err(StatusError::Invalid)
    }
}
fn native_now(state: &ServerState) -> Result<i64, StatusError> {
    let now = state.clock.now_unix();
    if (0..=MAX_TIME).contains(&now) {
        Ok(now)
    } else {
        Err(StatusError::Unavailable)
    }
}
fn valid_ticket(t: &StatusTicket) -> Result<(), StatusError> {
    valid_epoch(t.epoch, &t.boot_id)?;
    if t.org_id <= 0
        || t.user_id <= 0
        || t.generation <= 0
        || !canonical_hex(&t.connection_id)
        || !canonical_hex(&t.storage_handle)
        || !canonical_hex(&t.check_id)
        || !canonical_session(&t.session_id)
        || !(0..=MAX_TIME - 60).contains(&t.issued_at)
        || t.issued_at.checked_add(60) != Some(t.expires_at)
    {
        return Err(StatusError::Invalid);
    }
    Ok(())
}
impl StatusPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Ready => "ready",
            Self::Closed => "closed",
        }
    }
}
struct Owner {
    epoch: i64,
    boot_id: Option<String>,
    phase: StatusPhase,
}
impl Owner {
    fn matches(&self, ticket: &StatusEpochTicket) -> bool {
        self.epoch == ticket.epoch && self.boot_id.as_deref() == Some(ticket.boot_id.as_str())
    }
    fn require_ready(&self, ticket: &StatusEpochTicket) -> Result<(), StatusError> {
        if self.matches(ticket) && self.phase == StatusPhase::Ready {
            Ok(())
        } else {
            Err(StatusError::Stale)
        }
    }
}

// SQLite decoding must reject REAL/BLOB values rather than let SQLx coerce them.
async fn lock_owner(tx: &DatabaseTransaction) -> Result<Owner, StatusError> {
    let sql = match tx.get_database_backend() {
        DbBackend::Sqlite => "SELECT id,epoch,boot_id,phase,(typeof(id)='integer' AND typeof(epoch)='integer' AND typeof(boot_id) IN ('null','text') AND typeof(phase)='text') AS types_ok FROM neev_agent_status_owner LIMIT 2",
        DbBackend::Postgres => "SELECT id,epoch,boot_id,phase,TRUE AS types_ok FROM neev_agent_status_owner LIMIT 2 FOR UPDATE",
        _ => return Err(StatusError::Unavailable),
    };
    let rows = tx
        .query_all(db::statement(tx, sql, vec![]))
        .await
        .map_err(unavailable)?;
    if rows.len() != 1 {
        return Err(StatusError::Unavailable);
    }
    let r = &rows[0];
    let id: i32 = r.try_get("", "id").map_err(unavailable)?;
    let epoch: i64 = r.try_get("", "epoch").map_err(unavailable)?;
    let boot_id: Option<String> = r.try_get("", "boot_id").map_err(unavailable)?;
    let phase = match r
        .try_get::<String>("", "phase")
        .map_err(unavailable)?
        .as_str()
    {
        "initializing" => StatusPhase::Initializing,
        "ready" => StatusPhase::Ready,
        "closed" => StatusPhase::Closed,
        _ => return Err(StatusError::Unavailable),
    };
    if !r.try_get::<bool>("", "types_ok").map_err(unavailable)?
        || id != 1
        || !match (epoch, boot_id.as_deref(), phase) {
            (0, None, StatusPhase::Closed) => true,
            (1.., Some(boot), _) => canonical_hex(boot),
            _ => false,
        }
    {
        return Err(StatusError::Unavailable);
    }
    Ok(Owner {
        epoch,
        boot_id,
        phase,
    })
}

struct CheckRow {
    connection_id: String,
    check_id: String,
    epoch: i64,
    session_id: String,
    generation: i64,
    storage_handle: String,
    issued_at: i64,
    expires_at: i64,
}
impl CheckRow {
    fn decode(r: &QueryResult) -> Result<Self, StatusError> {
        let row = Self {
            connection_id: r.try_get("", "connection_id").map_err(unavailable)?,
            check_id: r.try_get("", "check_id").map_err(unavailable)?,
            epoch: r.try_get("", "owner_epoch").map_err(unavailable)?,
            session_id: r.try_get("", "session_id").map_err(unavailable)?,
            generation: r.try_get("", "generation").map_err(unavailable)?,
            storage_handle: r.try_get("", "storage_handle").map_err(unavailable)?,
            issued_at: r.try_get("", "issued_at").map_err(unavailable)?,
            expires_at: r.try_get("", "expires_at").map_err(unavailable)?,
        };
        if !r.try_get::<bool>("", "types_ok").map_err(unavailable)?
            || !canonical_hex(&row.connection_id)
            || !canonical_hex(&row.check_id)
            || !canonical_hex(&row.storage_handle)
            || !canonical_session(&row.session_id)
            || row.epoch <= 0
            || row.generation <= 0
            || !(0..=MAX_TIME - 60).contains(&row.issued_at)
            || row.issued_at.checked_add(60) != Some(row.expires_at)
        {
            return Err(StatusError::Unavailable);
        }
        Ok(row)
    }
    fn matches(&self, t: &StatusTicket) -> bool {
        self.connection_id == t.connection_id
            && self.check_id == t.check_id
            && self.epoch == t.epoch
            && self.session_id == t.session_id
            && self.generation == t.generation
            && self.storage_handle == t.storage_handle
            && self.issued_at == t.issued_at
            && self.expires_at == t.expires_at
    }
    fn values(&self) -> Vec<Value> {
        vec![
            self.connection_id.clone().into(),
            self.check_id.clone().into(),
            self.epoch.into(),
            self.session_id.clone().into(),
            self.generation.into(),
            self.storage_handle.clone().into(),
            self.issued_at.into(),
            self.expires_at.into(),
        ]
    }
    fn ticket(&self, connection: &store::ConnectionRow, epoch: &StatusEpochTicket) -> StatusTicket {
        StatusTicket {
            epoch: epoch.epoch,
            boot_id: epoch.boot_id.clone(),
            org_id: connection.org_id,
            user_id: connection.user_id,
            provider: connection.provider,
            connection_id: self.connection_id.clone(),
            generation: self.generation,
            session_id: self.session_id.clone(),
            storage_handle: self.storage_handle.clone(),
            check_id: self.check_id.clone(),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        }
    }
}

async fn lock_checks(
    tx: &DatabaseTransaction,
    owner: &Owner,
) -> Result<Vec<CheckRow>, StatusError> {
    let sql=match tx.get_database_backend() {
        DbBackend::Sqlite=>"SELECT connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at,(typeof(connection_id)='text' AND typeof(check_id)='text' AND typeof(owner_epoch)='integer' AND typeof(session_id)='text' AND typeof(generation)='integer' AND typeof(storage_handle)='text' AND typeof(issued_at)='integer' AND typeof(expires_at)='integer') AS types_ok FROM neev_agent_status_checks ORDER BY connection_id LIMIT 65",
        DbBackend::Postgres=>"SELECT connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at,TRUE AS types_ok FROM neev_agent_status_checks ORDER BY connection_id LIMIT 65 FOR UPDATE",
        _=>return Err(StatusError::Unavailable),
    };
    let rows = tx
        .query_all(db::statement(tx, sql, vec![]))
        .await
        .map_err(unavailable)?;
    if rows.len() > 64 || (!rows.is_empty() && owner.phase != StatusPhase::Ready) {
        return Err(StatusError::Unavailable);
    }
    let mut connections = std::collections::BTreeSet::new();
    let mut checks = std::collections::BTreeSet::new();
    let mut result = Vec::with_capacity(rows.len());
    for row in &rows {
        let row = CheckRow::decode(row)?;
        if row.epoch != owner.epoch
            || !connections.insert(row.connection_id.clone())
            || !checks.insert(row.check_id.clone())
        {
            return Err(StatusError::Unavailable);
        }
        result.push(row);
    }
    if db::one(tx,"SELECT 1 AS invalid FROM neev_agent_status_checks s LEFT JOIN neev_agent_connections c ON c.id=s.connection_id WHERE c.id IS NULL LIMIT 1",vec![]).await.map_err(unavailable)?.is_some() {
        return Err(StatusError::Unavailable);
    }
    Ok(result)
}

async fn internal_transaction(state: &ServerState) -> Result<DatabaseTransaction, StatusError> {
    let tx = db::begin(&state.database).await.map_err(unavailable)?;
    db::lock_admission_guard(&tx).await.map_err(unavailable)?;
    Ok(tx)
}
async fn execute_exact(
    tx: &DatabaseTransaction,
    sql: &str,
    values: Vec<Value>,
    expected: u64,
) -> Result<(), StatusError> {
    if db::execute(tx, sql, values)
        .await
        .map_err(unavailable)?
        .rows_affected()
        != expected
    {
        return Err(StatusError::Unavailable);
    }
    Ok(())
}

// Each deletion retains every captured check field and the caller's original fence.
async fn delete_record(
    tx: &DatabaseTransaction,
    row: &CheckRow,
    epoch: &StatusEpochTicket,
    phase: StatusPhase,
) -> Result<(), StatusError> {
    let mut values = row.values();
    values.extend([
        epoch.epoch.into(),
        epoch.boot_id.clone().into(),
        phase.as_str().into(),
    ]);
    execute_exact(tx,"DELETE FROM neev_agent_status_checks WHERE connection_id=$1 AND check_id=$2 AND owner_epoch=$3 AND session_id=$4 AND generation=$5 AND storage_handle=$6 AND issued_at=$7 AND expires_at=$8 AND EXISTS(SELECT 1 FROM neev_agent_status_owner WHERE id=1 AND epoch=$9 AND boot_id=$10 AND phase=$11)",values,1).await
}

pub(crate) async fn activate_status_epoch(
    state: &ServerState,
    claim: &CheckedStatusEpoch,
) -> Result<Activation, StatusError> {
    valid_epoch(claim.epoch, &claim.boot_id)?;
    let tx = internal_transaction(state).await?;
    let owner = lock_owner(&tx).await?;
    let checks = lock_checks(&tx, &owner).await?;
    let epoch = StatusEpochTicket {
        epoch: claim.epoch,
        boot_id: claim.boot_id.clone(),
    };
    if owner.matches(&epoch) {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Activation::Existing(epoch, owner.phase));
    }
    if claim.epoch <= owner.epoch {
        return Err(StatusError::Stale);
    }
    execute_exact(&tx,"UPDATE neev_agent_status_owner SET epoch=$1,boot_id=$2,phase='initializing' WHERE id=1 AND epoch=$3 AND phase=$4 AND (boot_id=$5 OR (boot_id IS NULL AND $5 IS NULL))",
        vec![epoch.epoch.into(),epoch.boot_id.clone().into(),owner.epoch.into(),owner.phase.as_str().into(),owner.boot_id.into()],1).await?;
    for check in &checks {
        delete_record(&tx, check, &epoch, StatusPhase::Initializing).await?;
    }
    tx.commit().await.map_err(unavailable)?;
    Ok(Activation::Activated(epoch))
}

pub(crate) async fn enable_status_checks(
    state: &ServerState,
    epoch: &StatusEpochTicket,
    ready: &ReconciledStatusReady,
) -> Result<(), StatusError> {
    valid_epoch(epoch.epoch, &epoch.boot_id)?;
    valid_epoch(ready.epoch, &ready.boot_id)?;
    if ready.epoch != epoch.epoch || ready.boot_id != epoch.boot_id {
        return Err(StatusError::Invalid);
    }
    let tx = internal_transaction(state).await?;
    let owner = lock_owner(&tx).await?;
    lock_checks(&tx, &owner).await?;
    if !owner.matches(epoch) || owner.phase == StatusPhase::Closed {
        return Err(StatusError::Stale);
    }
    if owner.phase == StatusPhase::Initializing {
        execute_exact(&tx,"UPDATE neev_agent_status_owner SET phase='ready' WHERE id=1 AND epoch=$1 AND boot_id=$2 AND phase='initializing'",vec![epoch.epoch.into(),epoch.boot_id.clone().into()],1).await?;
    }
    tx.commit().await.map_err(unavailable)
}

pub(crate) async fn disable_status_checks(
    state: &ServerState,
    epoch: &StatusEpochTicket,
) -> Result<(), StatusError> {
    valid_epoch(epoch.epoch, &epoch.boot_id)?;
    let tx = internal_transaction(state).await?;
    let owner = lock_owner(&tx).await?;
    let checks = lock_checks(&tx, &owner).await?;
    if !owner.matches(epoch) {
        return Err(StatusError::Stale);
    }
    if owner.phase != StatusPhase::Closed {
        execute_exact(&tx,"UPDATE neev_agent_status_owner SET phase='closed' WHERE id=1 AND epoch=$1 AND boot_id=$2 AND phase=$3",vec![epoch.epoch.into(),epoch.boot_id.clone().into(),owner.phase.as_str().into()],1).await?;
        for check in &checks {
            delete_record(&tx, check, epoch, StatusPhase::Closed).await?;
        }
    }
    tx.commit().await.map_err(unavailable)
}

fn valid_connection(connection: &store::ConnectionRow) -> Result<(), StatusError> {
    if !canonical_hex(&connection.id)
        || connection.org_id <= 0
        || connection.user_id <= 0
        || connection.generation <= 0
        || connection
            .storage_handle
            .as_deref()
            .is_some_and(|h| !canonical_hex(h))
        || connection
            .checked_at
            .is_some_and(|time| !(0..=MAX_TIME).contains(&time))
    {
        return Err(StatusError::Unavailable);
    }
    Ok(())
}
fn checkable(connection: &store::ConnectionRow) -> bool {
    matches!(
        connection.state,
        ConnectionState::Connected | ConnectionState::ReconnectRequired
    ) && connection.storage_handle.is_some()
}
async fn commit_live(
    tx: DatabaseTransaction,
    state: &ServerState,
    actor: &OrgContext,
    expiry: i64,
) -> Result<(), StatusError> {
    session::recheck(&tx, actor, native_now(state)?)
        .await
        .map_err(|e| StatusError::Agent(AgentError::Auth(e)))?;
    if native_now(state)? >= expiry {
        return Err(StatusError::Unavailable);
    }
    // No awaited query may follow the final deadline sample before COMMIT.
    // A failed/unknown COMMIT never exposes a ticket or asserts rollback.
    tx.commit().await.map_err(unavailable)
}

pub(crate) async fn prepare_connection_check(
    state: &ServerState,
    actor: &OrgContext,
    epoch: &StatusEpochTicket,
    provider: Provider,
) -> Result<CheckAdmission, StatusError> {
    valid_epoch(epoch.epoch, &epoch.boot_id)?;
    if provider != Provider::Codex {
        return Err(StatusError::Agent(AgentError::Unavailable));
    }
    let tx = store::authorized_transaction(state, actor)
        .await
        .map_err(agent)?;
    let owner = lock_owner(&tx).await?;
    if let Err(stale) = owner.require_ready(epoch) {
        // A stale caller cannot repair corrupt initializing/closed marker state.
        // This branch returns before taking any connection lock.
        lock_checks(&tx, &owner).await?;
        return Err(stale);
    }
    let connection = store::lock_connection(&tx, actor, provider)
        .await
        .map_err(agent)?
        .ok_or(StatusError::Agent(AgentError::NotFound))?;
    valid_connection(&connection)?;
    if !checkable(&connection) {
        return Err(StatusError::Stale);
    }
    let checks = lock_checks(&tx, &owner).await?;
    let now = native_now(state)?;
    if let Some(row) = checks.iter().find(|row| row.connection_id == connection.id) {
        if row.generation == connection.generation
            && Some(row.storage_handle.as_str()) == connection.storage_handle.as_deref()
            && row.expires_at > now
        {
            if row.session_id != actor.identity.session_id {
                return Err(StatusError::Busy);
            }
            let ticket = row.ticket(&connection, epoch);
            commit_live(tx, state, actor, ticket.expires_at).await?;
            return Ok(CheckAdmission::Joined(ticket));
        }
    }
    let mut remaining = checks.len();
    for row in &checks {
        if row.expires_at <= now || row.connection_id == connection.id {
            delete_record(&tx, row, epoch, StatusPhase::Ready).await?;
            remaining -= 1;
        }
    }
    if remaining >= 64 {
        return Err(StatusError::Busy);
    }
    let expires_at = now
        .checked_add(60)
        .filter(|expiry| *expiry <= MAX_TIME)
        .ok_or(StatusError::Unavailable)?;
    let row = CheckRow {
        connection_id: connection.id.clone(),
        check_id: uuid::Uuid::new_v4().simple().to_string(),
        epoch: epoch.epoch,
        session_id: actor.identity.session_id.clone(),
        generation: connection.generation,
        storage_handle: connection
            .storage_handle
            .clone()
            .ok_or(StatusError::Unavailable)?,
        issued_at: now,
        expires_at,
    };
    let ticket = row.ticket(&connection, epoch);
    valid_ticket(&ticket).map_err(unavailable)?;
    let mut values = row.values();
    values.extend([
        epoch.boot_id.clone().into(),
        connection.org_id.into(),
        connection.user_id.into(),
    ]);
    execute_exact(&tx,"INSERT INTO neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at) SELECT $1,$2,$3,$4,$5,$6,$7,$8 WHERE EXISTS(SELECT 1 FROM neev_agent_status_owner WHERE id=1 AND epoch=$3 AND boot_id=$9 AND phase='ready') AND EXISTS(SELECT 1 FROM neev_agent_connections WHERE id=$1 AND org_id=$10 AND user_id=$11 AND provider='codex' AND generation=$5 AND storage_handle=$6 AND state IN ('connected','reconnect_required'))",values,1).await?;
    commit_live(tx, state, actor, expires_at).await?;
    Ok(CheckAdmission::Started(ticket))
}

async fn delete_ticket(tx: &DatabaseTransaction, t: &StatusTicket) -> Result<u64, StatusError> {
    Ok(db::execute(tx,"DELETE FROM neev_agent_status_checks WHERE connection_id=$1 AND check_id=$2 AND owner_epoch=$3 AND session_id=$4 AND generation=$5 AND storage_handle=$6 AND issued_at=$7 AND expires_at=$8 AND EXISTS(SELECT 1 FROM neev_agent_status_owner WHERE id=1 AND epoch=$3 AND boot_id=$9 AND phase='ready') AND EXISTS(SELECT 1 FROM neev_agent_connections WHERE id=$1 AND org_id=$10 AND user_id=$11 AND provider='codex')",
        vec![t.connection_id.clone().into(),t.check_id.clone().into(),t.epoch.into(),t.session_id.clone().into(),t.generation.into(),t.storage_handle.clone().into(),t.issued_at.into(),t.expires_at.into(),t.boot_id.clone().into(),t.org_id.into(),t.user_id.into()]).await.map_err(unavailable)?.rows_affected())
}

pub(crate) async fn finalize_connection_check(
    state: &ServerState,
    actor: &OrgContext,
    ticket: &StatusTicket,
    outcome: CheckedAccountOutcome,
) -> Result<CheckFinalized, StatusError> {
    // Scope is checked before any foreign row lookup, including native authorization.
    if ticket.org_id != actor.org_id
        || ticket.user_id != actor.identity.user_id
        || ticket.session_id != actor.identity.session_id
    {
        return Err(StatusError::Agent(AgentError::NotFound));
    }
    valid_ticket(ticket)?;
    if ticket.provider != Provider::Codex {
        return Err(StatusError::Agent(AgentError::Unavailable));
    }
    let tx = store::authorized_transaction(state, actor)
        .await
        .map_err(agent)?;
    let owner = lock_owner(&tx).await?;
    let epoch = StatusEpochTicket {
        epoch: ticket.epoch,
        boot_id: ticket.boot_id.clone(),
    };
    // Still validate intrinsic persisted state before returning a stale fence.
    let connection = store::lock_connection(&tx, actor, Provider::Codex)
        .await
        .map_err(agent)?;
    let checks = lock_checks(&tx, &owner).await?;
    if owner.require_ready(&epoch).is_err() {
        return Ok(CheckFinalized::Stale);
    }
    let Some(connection) = connection else {
        return Ok(CheckFinalized::Stale);
    };
    valid_connection(&connection)?;
    if connection.id != ticket.connection_id
        || connection.generation != ticket.generation
        || connection.storage_handle.as_deref() != Some(ticket.storage_handle.as_str())
        || !checkable(&connection)
        || !checks.iter().any(|row| row.matches(ticket))
    {
        return Ok(CheckFinalized::Stale);
    }
    let now = native_now(state)?;
    session::recheck(&tx, actor, now)
        .await
        .map_err(|e| StatusError::Agent(AgentError::Auth(e)))?;
    let reason = if now >= ticket.expires_at {
        Some(CheckUnavailable::Expired)
    } else {
        match &outcome {
            CheckedAccountOutcome::Unavailable(reason) => Some(*reason),
            _ => None,
        }
    };
    if let Some(reason) = reason {
        if delete_ticket(&tx, ticket).await? != 1 {
            return Err(StatusError::Unavailable);
        }
        store::commit_authorized(tx, state, actor)
            .await
            .map_err(agent)?;
        return Ok(CheckFinalized::Unavailable(reason));
    }
    let observed_at = match &outcome {
        CheckedAccountOutcome::Supported {
            account_label,
            observed_at,
        } => {
            if !store::safe_label(account_label) {
                return Err(StatusError::Invalid);
            }
            *observed_at
        }
        CheckedAccountOutcome::Disconnected { observed_at }
        | CheckedAccountOutcome::Unsupported { observed_at } => *observed_at,
        CheckedAccountOutcome::Unavailable(_) => return Err(StatusError::Unavailable),
    };
    if observed_at < ticket.issued_at || observed_at > now || observed_at >= ticket.expires_at {
        return Err(StatusError::Invalid);
    }
    // The only source of publication values is this exhaustive closed outcome match.
    let (state_name, label, checked_at, error_code) = match outcome {
        CheckedAccountOutcome::Supported { account_label, .. } => {
            ("connected", Some(account_label), Some(now), None)
        }
        CheckedAccountOutcome::Disconnected { .. } => {
            ("reconnect_required", None, None, Some("reauth_required"))
        }
        CheckedAccountOutcome::Unsupported { .. } => (
            "reconnect_required",
            None,
            None,
            Some("unsupported_account"),
        ),
        CheckedAccountOutcome::Unavailable(_) => return Err(StatusError::Unavailable),
    };
    execute_exact(&tx,
        "UPDATE neev_agent_connections
         SET state=$1,account_label=$2,checked_at=$3,error_code=$4,updated_at=$5
         WHERE id=$6 AND org_id=$7 AND user_id=$8 AND provider='codex'
           AND generation=$9 AND storage_handle=$10 AND state IN ('connected','reconnect_required')
           AND EXISTS(SELECT 1 FROM neev_agent_status_checks s,neev_agent_status_owner o
             WHERE s.connection_id=$6 AND s.check_id=$11 AND s.owner_epoch=$12 AND s.session_id=$13
               AND s.generation=$9 AND s.storage_handle=$10 AND s.issued_at=$14 AND s.expires_at=$15
               AND s.expires_at>$5 AND o.id=1 AND o.epoch=$12 AND o.boot_id=$16 AND o.phase='ready')",
        vec![state_name.into(),label.clone().into(),checked_at.into(),error_code.into(),now.into(),
            ticket.connection_id.clone().into(),ticket.org_id.into(),ticket.user_id.into(),ticket.generation.into(),
            ticket.storage_handle.clone().into(),ticket.check_id.clone().into(),ticket.epoch.into(),ticket.session_id.clone().into(),
            ticket.issued_at.into(),ticket.expires_at.into(),ticket.boot_id.clone().into()],1).await?;
    if delete_ticket(&tx, ticket).await? != 1 {
        return Err(StatusError::Unavailable);
    }
    commit_live(tx, state, actor, ticket.expires_at).await?;
    Ok(CheckFinalized::Applied(ConnectionView {
        provider: Provider::Codex,
        state: if state_name == "connected" {
            ConnectionState::Connected
        } else {
            ConnectionState::ReconnectRequired
        },
        account_label: label,
        checked_at,
        error_code: error_code.map(str::to_owned),
    }))
}

pub(crate) async fn clear_connection_check(
    state: &ServerState,
    epoch: &StatusEpochTicket,
    ticket: &StatusTicket,
) -> Result<(), StatusError> {
    valid_epoch(epoch.epoch, &epoch.boot_id)?;
    valid_ticket(ticket)?;
    if epoch.epoch != ticket.epoch || epoch.boot_id != ticket.boot_id {
        return Err(StatusError::Stale);
    }
    if ticket.provider != Provider::Codex {
        return Err(StatusError::Agent(AgentError::Unavailable));
    }
    let tx = internal_transaction(state).await?;
    let owner = lock_owner(&tx).await?;
    let connection =
        store::lock_owned_connection(&tx, &ticket.connection_id, ticket.org_id, ticket.user_id)
            .await
            .map_err(agent)?;
    lock_checks(&tx, &owner).await?;
    if let Some(connection) = connection {
        valid_connection(&connection)?;
    }
    if owner.require_ready(epoch).is_ok() && delete_ticket(&tx, ticket).await? > 1 {
        return Err(StatusError::Unavailable);
    }
    tx.commit().await.map_err(unavailable)
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
