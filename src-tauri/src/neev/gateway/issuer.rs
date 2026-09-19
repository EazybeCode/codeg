//! Native issuer storage operations over the schema8 gateway tables.
//!
//! Pure transaction-scoped operations behind the gateway contract
//! (`NEEV-GATEWAY-CONTRACT-v1.md`, storage rulings in
//! `NEEV-ISSUER-STORAGE-ADOPTION.md`). No HTTP listener, mTLS transport or route
//! is defined here; a later transport task consumes these functions behind an
//! integration setting. Every operation drives one native transaction:
//! `db::begin` → admission guard → native domain locks (accounts, orgs,
//! memberships, sessions ascending) → gateway rows → audience counter row last.
//! No path holding the counter later takes a domain lock. Time is sampled from an
//! injectable clock after blocking locks and re-checked before commit.
//!
//! Bounded (<=100 rows/call) cleanup of expired receipt/relying/exchange rows is
//! provided by [`prune_expired`]; a deployment runs it on its own cadence (no
//! hidden background task here). Permanent bindings, durable revocations and
//! revoke intents are never pruned.
#![allow(dead_code)]

use crate::neev::agents::recovery::invalidate_session_in_transaction;
use crate::neev::agents::types::AgentError;
use crate::neev::auth::email::canonical_email;
use crate::neev::auth::session::{lock, lock_account, lock_membership, lock_organization, lock_session};
use crate::neev::clock::Clock;
use crate::neev::db;
use crate::neev::error::{database_error, AuthError};
use crate::neev::gateway::{GatewayEmail, GatewayName};
use crate::neev::types::{OrgContext, Role};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend};
use sha2::{Digest, Sha256};

const AUDIENCE: &str = "neev-collaboration-v1";
const ISSUER: &str = "neev-native-v1";
const CODE_TTL: i64 = 60;
const RETAIN: i64 = 360;
const RECEIPT_RETRY: i64 = 300;
const TIME_MAX: i64 = 253_402_300_799;
const ISSUED_MAX: i64 = 253_402_300_439;
/// Domain separator for the one-use code hash (R13); distinct from native
/// purpose-prefixed browser tokens and any other hash in the system.
const CODE_HASH_DOMAIN: &[u8] = b"neev-gateway-code-v1\x00";
// Adopted pilot admission limits (NEEV-ISSUER-STORAGE-ADOPTION.md).
const MAX_UNEXPIRED_PER_SOURCE: i64 = 5;
const MAX_EXCHANGES: i64 = 10_000;
const MAX_RELYING_PER_SOURCE: i64 = 16;
const MAX_RELYING: i64 = 100_000;

/// Issuer-layer outcome. `AuthorityChanged` is preserved as a distinct typed
/// result from an ordinary `Conflict` so the transport layer maps the exact wire
/// code (`authority_changed` vs `conflict`) without re-reading mutable state (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssuerError {
    Invalid,
    Unauthorized,
    Forbidden,
    AuthorityChanged,
    Conflict,
    Limited,
    Unavailable,
}

impl From<AuthError> for IssuerError {
    fn from(error: AuthError) -> Self {
        // Database/lock errors never fabricate `AuthorityChanged`; only the
        // explicit captured-revision comparisons below produce it.
        match error {
            AuthError::Unauthorized => Self::Unauthorized,
            AuthError::Forbidden => Self::Forbidden,
            AuthError::Conflict => Self::Conflict,
            AuthError::Invalid => Self::Invalid,
            AuthError::Limited => Self::Limited,
            AuthError::Unavailable => Self::Unavailable,
        }
    }
}

impl From<AgentError> for IssuerError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::Auth(inner) => inner.into(),
            AgentError::Conflict => Self::Conflict,
            AgentError::InvalidInput => Self::Invalid,
            _ => Self::Unavailable,
        }
    }
}

pub(crate) struct Issued {
    pub(crate) code: String,
    pub(crate) expires_at: i64,
}

pub(crate) struct Subject {
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) source_session_id: String,
    pub(crate) account_revision: i64,
    pub(crate) org_revision: i64,
    pub(crate) member_revision: i64,
    pub(crate) role: Role,
    pub(crate) display_name: String,
    pub(crate) org_name: String,
    pub(crate) verified_email: String,
}

pub(crate) struct Receipt {
    pub(crate) receipt_id: String,
    pub(crate) relying_ref: String,
    pub(crate) subject: Subject,
    pub(crate) expires_at: i64,
    pub(crate) issued_after_cursor: i64,
    pub(crate) revocation_cursor: i64,
}

pub(crate) struct Validated {
    pub(crate) subject: Subject,
    pub(crate) expires_at: i64,
    pub(crate) issued_after_cursor: i64,
    pub(crate) revocation_cursor: i64,
}

pub(crate) struct RevokeOutcome {
    pub(crate) event_id: String,
    pub(crate) cursor: i64,
}

pub(crate) struct RevocationEvent {
    pub(crate) event_id: String,
    pub(crate) cursor: i64,
    pub(crate) scope: String,
    pub(crate) user_id: Option<i32>,
    pub(crate) org_id: Option<i32>,
    pub(crate) source_session_id: Option<String>,
}

pub(crate) struct RevocationsPage {
    pub(crate) events: Vec<RevocationEvent>,
    pub(crate) next_cursor: i64,
    pub(crate) high_watermark: i64,
}

// ---- scalar helpers ----

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
    }
}

fn canonical_token(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == value)
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version_num() == 4
            && id.get_variant() == uuid::Variant::RFC4122
            && id.to_string() == value
    })
}

fn random_token() -> Result<String, IssuerError> {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| IssuerError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Domain-separated one-use code hash (R13): SHA256(domain || NUL || code ASCII).
pub(crate) fn hash_code(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CODE_HASH_DOMAIN);
    hasher.update(code.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Shared audience lock for reads that must exclude a concurrent counter writer
/// (validate) without serializing other readers: PostgreSQL `FOR SHARE`.
fn lock_shared(tx: &DatabaseTransaction) -> &'static str {
    if tx.get_database_backend() == DbBackend::Postgres {
        " FOR SHARE"
    } else {
        ""
    }
}

async fn lock_audience_cursor(tx: &DatabaseTransaction) -> Result<i64, AuthError> {
    db::one(
        tx,
        &format!(
            "SELECT last_cursor FROM neev_gateway_audience WHERE audience=$1{}",
            lock(tx)
        ),
        vec![AUDIENCE.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get::<i64>("", "last_cursor")
    .map_err(database_error)
}

async fn scalar_string(
    tx: &DatabaseTransaction,
    sql: &str,
    id: i32,
    column: &str,
) -> Result<String, AuthError> {
    db::one(tx, sql, vec![id.into()])
        .await?
        .ok_or(AuthError::Unauthorized)?
        .try_get::<String>("", column)
        .map_err(database_error)
}

/// Deterministic verified-email selection (R9): earliest `verified_at` (<= sampled
/// now, non-negative) then id; required native-canonical and accepted gateway
/// email syntax. Fails closed rather than choosing a later or future record.
async fn select_verified_email(
    tx: &DatabaseTransaction,
    user_id: i32,
    now: i64,
) -> Result<String, IssuerError> {
    let email = db::one(
        tx,
        "SELECT canonical_email FROM neev_verified_emails WHERE user_id=$1 AND verified_at>=0 AND verified_at<=$2 ORDER BY verified_at ASC, id ASC LIMIT 1",
        vec![user_id.into(), now.into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?
    .try_get::<String>("", "canonical_email")
    .map_err(database_error)?;
    if canonical_email(&email).as_deref() != Some(email.as_str())
        || GatewayEmail::new(email.clone()).is_err()
    {
        return Err(IssuerError::Unavailable);
    }
    Ok(email)
}

/// Validate current display names as gateway wire scalars (R12); native permits
/// longer/other-shaped values than the gateway predicate.
fn valid_display_names(display_name: &str, org_name: &str) -> Result<(), IssuerError> {
    if GatewayName::new(display_name.to_owned()).is_err()
        || GatewayName::new(org_name.to_owned()).is_err()
    {
        return Err(IssuerError::Unavailable);
    }
    Ok(())
}

/// Captured authority as stored on an exchange or relying record.
struct Captured {
    user_id: i32,
    org_id: i32,
    member_id: i32,
    account_revision: i64,
    org_revision: i64,
    member_revision: i64,
    role: Role,
    source_session_id: String,
    authenticated_at: i64,
}

/// Recheck current native authority against captured revisions under the
/// established lock order. A revision/role/session-rescope change is
/// `AuthorityChanged`; an invalid/revoked/expired/disabled session is
/// `Unauthorized`. Returns the live source-session expiry plus fresh names.
async fn recheck_captured(
    tx: &DatabaseTransaction,
    captured: &Captured,
    now: i64,
) -> Result<(i64, String, String), IssuerError> {
    let account = lock_account(tx, captured.user_id).await?;
    if account.auth_revision != captured.account_revision {
        return Err(IssuerError::AuthorityChanged);
    }
    let organization = lock_organization(tx, captured.org_id).await?;
    if organization.revision != captured.org_revision {
        return Err(IssuerError::AuthorityChanged);
    }
    let membership = lock_membership(tx, captured.org_id, captured.user_id).await?;
    if membership.id != captured.member_id
        || membership.revision != captured.member_revision
        || crate::neev::auth::session::role(&membership.role)? != captured.role
    {
        return Err(IssuerError::AuthorityChanged);
    }
    let session = lock_session(tx, &captured.source_session_id).await?;
    if session.revoked_at.is_some()
        || session.expires_at <= now
        || session.authenticated_at > now
        || session.account_revision != account.auth_revision
    {
        return Err(IssuerError::Unauthorized);
    }
    if session.user_id != captured.user_id
        || session.org_id != Some(captured.org_id)
        || session.member_id != Some(captured.member_id)
        || session.member_revision != Some(captured.member_revision)
        || session.org_revision != Some(captured.org_revision)
        || session.authenticated_at != captured.authenticated_at
    {
        return Err(IssuerError::AuthorityChanged);
    }
    let display_name = scalar_string(
        tx,
        "SELECT display_name FROM neev_accounts WHERE id=$1",
        captured.user_id,
        "display_name",
    )
    .await?;
    let org_name = scalar_string(
        tx,
        "SELECT name FROM neev_organizations WHERE id=$1",
        captured.org_id,
        "name",
    )
    .await?;
    Ok((session.expires_at, display_name, org_name))
}

/// Revalidate the persisted relying snapshot on idempotent replay: original
/// (immutable) names/email as gateway-valid scalars, and exact equality to the
/// exchange-captured authority and the permanent binding. Fails closed on any
/// malformed or inconsistent stored row. Returns
/// (display_name, org_name, verified_email, expires_at, issued_after_cursor).
async fn validate_persisted_relying(
    tx: &DatabaseTransaction,
    relying_ref: &str,
    captured: &Captured,
) -> Result<(String, String, String, i64, i64), IssuerError> {
    let row = db::one(
        tx,
        "SELECT source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,display_name,org_name,verified_email,authenticated_at,created_at,expires_at,issued_after_cursor FROM neev_gateway_relying_sessions WHERE relying_ref=$1 AND audience=$2",
        vec![relying_ref.into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    let source = row.try_get::<String>("", "source_session_id").map_err(database_error)?;
    let user_id = row.try_get::<i32>("", "user_id").map_err(database_error)?;
    let org_id = row.try_get::<i32>("", "org_id").map_err(database_error)?;
    let member_id = row.try_get::<i32>("", "member_id").map_err(database_error)?;
    let account_revision = row.try_get::<i64>("", "account_revision").map_err(database_error)?;
    let org_revision = row.try_get::<i64>("", "org_revision").map_err(database_error)?;
    let member_revision = row.try_get::<i64>("", "member_revision").map_err(database_error)?;
    let role = crate::neev::auth::session::role(
        &row.try_get::<String>("", "role").map_err(database_error)?,
    )?;
    let display_name = row.try_get::<String>("", "display_name").map_err(database_error)?;
    let org_name = row.try_get::<String>("", "org_name").map_err(database_error)?;
    let verified_email = row.try_get::<String>("", "verified_email").map_err(database_error)?;
    let authenticated_at = row.try_get::<i64>("", "authenticated_at").map_err(database_error)?;
    let created_at = row.try_get::<i64>("", "created_at").map_err(database_error)?;
    let expires_at = row.try_get::<i64>("", "expires_at").map_err(database_error)?;
    let cutoff = row.try_get::<i64>("", "issued_after_cursor").map_err(database_error)?;
    // Persisted scalar canonicality.
    if !canonical_uuid(relying_ref)
        || !canonical_uuid(&source)
        || relying_ref == source
        || canonical_email(&verified_email).as_deref() != Some(verified_email.as_str())
        || GatewayEmail::new(verified_email.clone()).is_err()
        || GatewayName::new(display_name.clone()).is_err()
        || GatewayName::new(org_name.clone()).is_err()
    {
        return Err(IssuerError::Unavailable);
    }
    // Exact equality to the exchange-captured authority.
    if source != captured.source_session_id
        || user_id != captured.user_id
        || org_id != captured.org_id
        || member_id != captured.member_id
        || account_revision != captured.account_revision
        || org_revision != captured.org_revision
        || member_revision != captured.member_revision
        || role != captured.role
        || authenticated_at != captured.authenticated_at
    {
        return Err(IssuerError::Unavailable);
    }
    // Exact equality to the permanent binding.
    let binding = db::one(
        tx,
        "SELECT source_session_id,user_id,org_id,created_at FROM neev_gateway_relying_bindings WHERE relying_ref=$1 AND audience=$2",
        vec![relying_ref.into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    if binding.try_get::<String>("", "source_session_id").map_err(database_error)? != source
        || binding.try_get::<i32>("", "user_id").map_err(database_error)? != user_id
        || binding.try_get::<i32>("", "org_id").map_err(database_error)? != org_id
        || binding.try_get::<i64>("", "created_at").map_err(database_error)? != created_at
    {
        return Err(IssuerError::Unavailable);
    }
    Ok((display_name, org_name, verified_email, expires_at, cutoff))
}

fn exchange_captured(row: &sea_orm::QueryResult) -> Result<Captured, AuthError> {
    Ok(Captured {
        user_id: row.try_get::<i32>("", "user_id").map_err(database_error)?,
        org_id: row.try_get::<i32>("", "org_id").map_err(database_error)?,
        member_id: row.try_get::<i32>("", "member_id").map_err(database_error)?,
        account_revision: row
            .try_get::<i64>("", "account_revision")
            .map_err(database_error)?,
        org_revision: row.try_get::<i64>("", "org_revision").map_err(database_error)?,
        member_revision: row
            .try_get::<i64>("", "member_revision")
            .map_err(database_error)?,
        role: crate::neev::auth::session::role(
            &row.try_get::<String>("", "role").map_err(database_error)?,
        )?,
        source_session_id: row
            .try_get::<String>("", "source_session_id")
            .map_err(database_error)?,
        authenticated_at: row
            .try_get::<i64>("", "authenticated_at")
            .map_err(database_error)?,
    })
}

/// Append (or, for a session scope, reuse) one audience-ordered revocation event
/// under the counter lock. Shared by `revoke` and the native authority hooks;
/// returns `AuthError` so native transactions in `AuthError`-space can call it.
pub(crate) async fn append_revocation(
    tx: &DatabaseTransaction,
    scope: &str,
    user_id: Option<i32>,
    org_id: Option<i32>,
    source_session_id: Option<&str>,
    now: i64,
) -> Result<(String, i64), AuthError> {
    if !(0..=TIME_MAX).contains(&now) {
        return Err(AuthError::Invalid);
    }
    if scope == "session" {
        if let Some(source) = source_session_id {
            if let Some(existing) = db::one(
                tx,
                "SELECT event_id,cursor FROM neev_gateway_revocations WHERE audience=$1 AND scope='session' AND source_session_id=$2",
                vec![AUDIENCE.into(), source.into()],
            )
            .await?
            {
                return Ok((
                    existing.try_get::<String>("", "event_id").map_err(database_error)?,
                    existing.try_get::<i64>("", "cursor").map_err(database_error)?,
                ));
            }
        }
    }
    let cursor = lock_audience_cursor(tx)
        .await?
        .checked_add(1)
        .ok_or(AuthError::Unavailable)?;
    let event_id = uuid::Uuid::new_v4().to_string();
    db::execute(
        tx,
        "INSERT INTO neev_gateway_revocations(audience,cursor,event_id,scope,user_id,org_id,source_session_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        vec![
            AUDIENCE.into(),
            cursor.into(),
            event_id.clone().into(),
            scope.into(),
            user_id.into(),
            org_id.into(),
            source_session_id.map(|s| s.to_owned()).into(),
            now.into(),
        ],
    )
    .await?;
    let updated = db::execute(
        tx,
        "UPDATE neev_gateway_audience SET last_cursor=$1 WHERE audience=$2 AND last_cursor=$3",
        vec![cursor.into(), AUDIENCE.into(), (cursor - 1).into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    Ok((event_id, cursor))
}

/// Rows removed by one bounded cleanup pass, per expirable table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CleanupCounts {
    pub(crate) redemptions: u32,
    pub(crate) relying_sessions: u32,
    pub(crate) exchanges: u32,
}

impl CleanupCounts {
    pub(crate) fn total(&self) -> u32 {
        self.redemptions + self.relying_sessions + self.exchanges
    }
}

/// Delete up to `budget` rows via a portable keyset subquery on the primary key.
async fn delete_expired(
    tx: &DatabaseTransaction,
    sql: &str,
    now: i64,
    budget: i64,
) -> Result<u32, IssuerError> {
    let deleted = db::execute(tx, sql, vec![now.into(), budget.into()]).await?;
    Ok(deleted.rows_affected() as u32)
}

/// Bounded, expirable-only cleanup (adopted proposal §1: `<=100` rows per call,
/// no hidden task). Removes expired redemption receipts, then expired relying
/// sessions and exchanges that no longer back a receipt, in FK-safe order. Never
/// touches a permanent binding, a durable revocation, or a revoke intent — a
/// binding without any live receipt/relying/intent row is retained precisely so
/// a delayed first revoke can still be delivered. The total deleted across all
/// three tables never exceeds `limit`.
pub(crate) async fn prune_expired(
    database: &DatabaseConnection,
    clock: &dyn Clock,
    limit: u32,
) -> Result<CleanupCounts, IssuerError> {
    if !(1..=100).contains(&limit) {
        return Err(IssuerError::Invalid);
    }
    let tx = db::begin(database).await?;
    db::lock_admission_guard(&tx).await?;
    // Resample after the blocking lock, per this module's clock discipline.
    let now = clock.now_unix();
    let mut budget = i64::from(limit);

    let redemptions = delete_expired(
        &tx,
        "DELETE FROM neev_gateway_redemptions WHERE receipt_id IN \
         (SELECT receipt_id FROM neev_gateway_redemptions WHERE retry_until < $1 LIMIT $2)",
        now,
        budget,
    )
    .await?;
    budget -= i64::from(redemptions);

    let relying_sessions = if budget > 0 {
        delete_expired(
            &tx,
            "DELETE FROM neev_gateway_relying_sessions WHERE relying_ref IN \
             (SELECT r.relying_ref FROM neev_gateway_relying_sessions r \
              WHERE r.expires_at < $1 \
                AND NOT EXISTS (SELECT 1 FROM neev_gateway_redemptions d \
                                WHERE d.relying_ref = r.relying_ref) \
              LIMIT $2)",
            now,
            budget,
        )
        .await?
    } else {
        0
    };
    budget -= i64::from(relying_sessions);

    let exchanges = if budget > 0 {
        delete_expired(
            &tx,
            "DELETE FROM neev_gateway_exchanges WHERE code_hash IN \
             (SELECT x.code_hash FROM neev_gateway_exchanges x \
              WHERE x.retain_until < $1 \
                AND NOT EXISTS (SELECT 1 FROM neev_gateway_redemptions d \
                                WHERE d.code_hash = x.code_hash) \
              LIMIT $2)",
            now,
            budget,
        )
        .await?
    } else {
        0
    };

    tx.commit().await.map_err(database_error)?;
    Ok(CleanupCounts {
        redemptions,
        relying_sessions,
        exchanges,
    })
}

// ---- operations ----

/// Exchange-issue: browser (native org session) presents an attempt and
/// challenge; native captures current authority and returns a one-use code.
pub(crate) async fn issue(
    database: &DatabaseConnection,
    context: &OrgContext,
    attempt_id: &str,
    challenge: &str,
    clock: &dyn Clock,
) -> Result<Issued, IssuerError> {
    if !canonical_token(attempt_id) || !canonical_token(challenge) {
        return Err(IssuerError::Invalid);
    }
    let entry_now = clock.now_unix();
    if !(0..=ISSUED_MAX).contains(&entry_now) {
        return Err(IssuerError::Invalid);
    }
    let code = random_token()?;
    let code_hash = hash_code(&code);
    let tx = db::begin(database).await?;
    db::lock_admission_guard(&tx).await?;
    crate::neev::auth::session::recheck(&tx, context, entry_now).await?;
    let session = lock_session(&tx, &context.identity.session_id).await?;
    // Audience counter lock last; resample time after this blocking lock (R7).
    lock_audience_cursor(&tx).await?;
    let now = clock.now_unix();
    if !(0..=ISSUED_MAX).contains(&now) || session.expires_at <= now {
        return Err(IssuerError::Unauthorized);
    }
    let code_expires_at = now.saturating_add(CODE_TTL).min(session.expires_at);
    let retain_until = now.checked_add(RETAIN).ok_or(IssuerError::Invalid)?;
    if code_expires_at <= now || retain_until > TIME_MAX {
        return Err(IssuerError::Invalid);
    }
    // A retained same-audience attempt is a Conflict, taking priority over quota (R14).
    if db::one(
        &tx,
        "SELECT 1 AS present FROM neev_gateway_exchanges WHERE audience=$1 AND attempt_id=$2",
        vec![AUDIENCE.into(), attempt_id.into()],
    )
    .await?
    .is_some()
    {
        return Err(IssuerError::Conflict);
    }
    let unexpired: i64 = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_gateway_exchanges WHERE audience=$1 AND source_session_id=$2 AND consumed_at IS NULL AND code_expires_at>$3",
        vec![AUDIENCE.into(), context.identity.session_id.clone().into(), now.into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?
    .try_get::<i64>("", "n")
    .map_err(database_error)?;
    let total: i64 = db::one(&tx, "SELECT COUNT(*) AS n FROM neev_gateway_exchanges", vec![])
        .await?
        .ok_or(IssuerError::Unavailable)?
        .try_get::<i64>("", "n")
        .map_err(database_error)?;
    if unexpired >= MAX_UNEXPIRED_PER_SOURCE || total >= MAX_EXCHANGES {
        return Err(IssuerError::Limited);
    }
    db::execute(
        &tx,
        "INSERT INTO neev_gateway_exchanges(code_hash,audience,attempt_id,challenge,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,issued_at,code_expires_at,retain_until,consumed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,NULL)",
        vec![
            code_hash.into(),
            AUDIENCE.into(),
            attempt_id.into(),
            challenge.into(),
            context.identity.session_id.clone().into(),
            context.identity.user_id.into(),
            context.org_id.into(),
            context.member_id.into(),
            context.identity.auth_revision.into(),
            context.org_revision.into(),
            context.member_revision.into(),
            role_str(context.role).into(),
            context.identity.authenticated_at.into(),
            now.into(),
            code_expires_at.into(),
            retain_until.into(),
        ],
    )
    .await?;
    // Resample before commit; reject if the source session expired during inserts.
    let commit_now = clock.now_unix();
    if commit_now >= code_expires_at || commit_now >= session.expires_at {
        return Err(IssuerError::Unauthorized);
    }
    tx.commit().await.map_err(database_error)?;
    Ok(Issued {
        code,
        expires_at: code_expires_at,
    })
}

/// Redeem the one-use code over the private boundary: verify challenge and
/// authority, consume the code, and atomically create the immutable binding,
/// receipt and live relying session. Idempotent within the retry window.
pub(crate) async fn redeem(
    database: &DatabaseConnection,
    attempt_id: &str,
    code: &str,
    verifier: &str,
    clock: &dyn Clock,
) -> Result<Receipt, IssuerError> {
    if !canonical_token(attempt_id) || !canonical_token(code) || !canonical_token(verifier) {
        return Err(IssuerError::Invalid);
    }
    let entry_now = clock.now_unix();
    if !(0..=ISSUED_MAX).contains(&entry_now) {
        return Err(IssuerError::Invalid);
    }
    let code_hash = hash_code(code);
    let expected_challenge = challenge_of(verifier);
    let tx = db::begin(database).await?;
    db::lock_admission_guard(&tx).await?;
    // Unlocked hint read to derive native lock targets (compared to the locked row).
    let hint = db::one(
        &tx,
        "SELECT user_id,org_id,member_id,account_revision,org_revision,member_revision,role,source_session_id,authenticated_at FROM neev_gateway_exchanges WHERE code_hash=$1 AND audience=$2",
        vec![code_hash.clone().into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    let captured = exchange_captured(&hint)?;
    // Native domains in order, then lock the gateway exchange row and compare.
    let (session_expires_at, display_name, org_name) =
        recheck_captured(&tx, &captured, entry_now).await?;
    let exchange = db::one(
        &tx,
        &format!(
            "SELECT attempt_id,challenge,issued_at,code_expires_at,retain_until,consumed_at,user_id,org_id,member_id,account_revision,org_revision,member_revision,source_session_id FROM neev_gateway_exchanges WHERE code_hash=$1 AND audience=$2{}",
            lock(&tx)
        ),
        vec![code_hash.clone().into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    if exchange.try_get::<String>("", "attempt_id").map_err(database_error)? != attempt_id
        || exchange.try_get::<String>("", "challenge").map_err(database_error)? != expected_challenge
        || exchange.try_get::<i32>("", "user_id").map_err(database_error)? != captured.user_id
        || exchange.try_get::<String>("", "source_session_id").map_err(database_error)?
            != captured.source_session_id
    {
        return Err(IssuerError::Unauthorized);
    }
    let consumed_at = exchange
        .try_get::<Option<i64>>("", "consumed_at")
        .map_err(database_error)?;
    let code_expires_at = exchange
        .try_get::<i64>("", "code_expires_at")
        .map_err(database_error)?;
    let issued_at = exchange.try_get::<i64>("", "issued_at").map_err(database_error)?;
    let retain_until = exchange
        .try_get::<i64>("", "retain_until")
        .map_err(database_error)?;

    if consumed_at.is_some() {
        // Idempotent retry: same receipt, ORIGINAL names/email (R8), only while
        // authority is unchanged and the receipt is within its retry window.
        let receipt = db::one(
            &tx,
            "SELECT receipt_id,relying_ref,retry_until FROM neev_gateway_redemptions WHERE code_hash=$1 AND audience=$2",
            vec![code_hash.into(), AUDIENCE.into()],
        )
        .await?
        .ok_or(IssuerError::Unauthorized)?;
        let retry_until = receipt.try_get::<i64>("", "retry_until").map_err(database_error)?;
        let relying_ref = receipt
            .try_get::<String>("", "relying_ref")
            .map_err(database_error)?;
        let (stored_display, stored_org, stored_email, expires_at, issued_after_cursor) =
            validate_persisted_relying(&tx, &relying_ref, &captured).await?;
        // Audience counter lock last; resample time after this blocking lock (R7)
        // and reject an expired receipt or source session before returning.
        let revocation_cursor = lock_audience_cursor(&tx).await?;
        let now = clock.now_unix();
        if now >= retry_until || now >= session_expires_at {
            return Err(IssuerError::Unauthorized);
        }
        let result = Receipt {
            receipt_id: receipt.try_get::<String>("", "receipt_id").map_err(database_error)?,
            relying_ref,
            subject: Subject {
                user_id: captured.user_id,
                org_id: captured.org_id,
                source_session_id: captured.source_session_id.clone(),
                account_revision: captured.account_revision,
                org_revision: captured.org_revision,
                member_revision: captured.member_revision,
                role: captured.role,
                display_name: stored_display,
                org_name: stored_org,
                verified_email: stored_email,
            },
            expires_at,
            issued_after_cursor,
            revocation_cursor,
        };
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }

    // First redemption. Fresh names must be valid gateway scalars (R12).
    valid_display_names(&display_name, &org_name)?;
    // Audience counter lock last; resample time after this blocking lock (R7).
    let cutoff = lock_audience_cursor(&tx).await?;
    let now = clock.now_unix();
    if !(0..=ISSUED_MAX).contains(&now) || now >= code_expires_at || now < issued_at {
        return Err(IssuerError::Unauthorized);
    }
    let verified_email = select_verified_email(&tx, captured.user_id, now).await?;
    let relying_source: i64 = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_gateway_relying_sessions WHERE audience=$1 AND source_session_id=$2",
        vec![AUDIENCE.into(), captured.source_session_id.clone().into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?
    .try_get::<i64>("", "n")
    .map_err(database_error)?;
    let relying_total: i64 = db::one(&tx, "SELECT COUNT(*) AS n FROM neev_gateway_relying_sessions", vec![])
        .await?
        .ok_or(IssuerError::Unavailable)?
        .try_get::<i64>("", "n")
        .map_err(database_error)?;
    if relying_source >= MAX_RELYING_PER_SOURCE || relying_total >= MAX_RELYING {
        return Err(IssuerError::Limited);
    }
    let relying_ref = uuid::Uuid::new_v4().to_string();
    let receipt_id = uuid::Uuid::new_v4().to_string();
    let retry_until = now.checked_add(RECEIPT_RETRY).ok_or(IssuerError::Invalid)?;
    if retry_until > retain_until {
        return Err(IssuerError::Conflict);
    }
    db::execute(
        &tx,
        "INSERT INTO neev_gateway_relying_bindings(relying_ref,audience,source_session_id,user_id,org_id,created_at) VALUES($1,$2,$3,$4,$5,$6)",
        vec![
            relying_ref.clone().into(),
            AUDIENCE.into(),
            captured.source_session_id.clone().into(),
            captured.user_id.into(),
            captured.org_id.into(),
            now.into(),
        ],
    )
    .await?;
    db::execute(
        &tx,
        "INSERT INTO neev_gateway_relying_sessions(relying_ref,audience,issuer,source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,authenticated_at,display_name,org_name,verified_email,created_at,expires_at,issued_after_cursor) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        vec![
            relying_ref.clone().into(),
            AUDIENCE.into(),
            ISSUER.into(),
            captured.source_session_id.clone().into(),
            captured.user_id.into(),
            captured.org_id.into(),
            captured.member_id.into(),
            captured.account_revision.into(),
            captured.org_revision.into(),
            captured.member_revision.into(),
            role_str(captured.role).into(),
            captured.authenticated_at.into(),
            display_name.clone().into(),
            org_name.clone().into(),
            verified_email.clone().into(),
            now.into(),
            session_expires_at.into(),
            cutoff.into(),
        ],
    )
    .await?;
    db::execute(
        &tx,
        "INSERT INTO neev_gateway_redemptions(receipt_id,audience,code_hash,relying_ref,redeemed_at,retry_until) VALUES($1,$2,$3,$4,$5,$6)",
        vec![
            receipt_id.clone().into(),
            AUDIENCE.into(),
            code_hash.clone().into(),
            relying_ref.clone().into(),
            now.into(),
            retry_until.into(),
        ],
    )
    .await?;
    // Conditional consume: exactly one unconsumed, in-window row (R: atomic).
    let consumed = db::execute(
        &tx,
        "UPDATE neev_gateway_exchanges SET consumed_at=$1 WHERE code_hash=$2 AND audience=$3 AND consumed_at IS NULL AND code_expires_at>$1",
        vec![now.into(), code_hash.into(), AUDIENCE.into()],
    )
    .await?;
    if consumed.rows_affected() != 1 {
        return Err(IssuerError::Conflict);
    }
    // Resample before commit; a code expiring during inserts rolls everything back.
    let commit_now = clock.now_unix();
    if commit_now >= code_expires_at {
        return Err(IssuerError::Unauthorized);
    }
    let result = Receipt {
        receipt_id,
        relying_ref,
        subject: Subject {
            user_id: captured.user_id,
            org_id: captured.org_id,
            source_session_id: captured.source_session_id.clone(),
            account_revision: captured.account_revision,
            org_revision: captured.org_revision,
            member_revision: captured.member_revision,
            role: captured.role,
            display_name,
            org_name,
            verified_email,
        },
        expires_at: session_expires_at,
        issued_after_cursor: cutoff,
        revocation_cursor: cutoff,
    };
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Private validate: recheck current authority against the relying record and
/// deny if any matching revocation scope exceeds its immutable cutoff. Returns
/// fresh (gateway-valid) display names without mutating the receipt.
pub(crate) async fn validate(
    database: &DatabaseConnection,
    relying_ref: &str,
    clock: &dyn Clock,
) -> Result<Validated, IssuerError> {
    if !canonical_uuid(relying_ref) {
        return Err(IssuerError::Invalid);
    }
    let now = clock.now_unix();
    let tx = db::begin(database).await?;
    let row = db::one(
        &tx,
        "SELECT source_session_id,user_id,org_id,member_id,account_revision,org_revision,member_revision,role,verified_email,authenticated_at,expires_at,issued_after_cursor FROM neev_gateway_relying_sessions WHERE relying_ref=$1 AND audience=$2",
        vec![relying_ref.into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    let captured = Captured {
        user_id: row.try_get::<i32>("", "user_id").map_err(database_error)?,
        org_id: row.try_get::<i32>("", "org_id").map_err(database_error)?,
        member_id: row.try_get::<i32>("", "member_id").map_err(database_error)?,
        account_revision: row
            .try_get::<i64>("", "account_revision")
            .map_err(database_error)?,
        org_revision: row.try_get::<i64>("", "org_revision").map_err(database_error)?,
        member_revision: row
            .try_get::<i64>("", "member_revision")
            .map_err(database_error)?,
        role: crate::neev::auth::session::role(
            &row.try_get::<String>("", "role").map_err(database_error)?,
        )?,
        source_session_id: row
            .try_get::<String>("", "source_session_id")
            .map_err(database_error)?,
        authenticated_at: row
            .try_get::<i64>("", "authenticated_at")
            .map_err(database_error)?,
    };
    let cutoff = row
        .try_get::<i64>("", "issued_after_cursor")
        .map_err(database_error)?;
    let expires_at = row.try_get::<i64>("", "expires_at").map_err(database_error)?;
    let verified_email = row
        .try_get::<String>("", "verified_email")
        .map_err(database_error)?;
    let (session_expires_at, display_name, org_name) =
        recheck_captured(&tx, &captured, now).await?;
    valid_display_names(&display_name, &org_name)?;
    // Consistent snapshot (R6): hold the audience row shared so a concurrent
    // counter writer cannot land between the watermark read and the scope MAX.
    let revocation_cursor = db::one(
        &tx,
        &format!(
            "SELECT last_cursor FROM neev_gateway_audience WHERE audience=$1{}",
            lock_shared(&tx)
        ),
        vec![AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?
    .try_get::<i64>("", "last_cursor")
    .map_err(database_error)?;
    // Fresh expiry after the blocking share lock (R7): the source may have
    // expired while waiting; fail closed rather than admit a stale session.
    if clock.now_unix() >= session_expires_at {
        return Err(IssuerError::Unauthorized);
    }
    let max_revoked = db::one(
        &tx,
        "SELECT MAX(cursor) AS c FROM neev_gateway_revocations WHERE audience=$1 AND ((scope='session' AND source_session_id=$2) OR (scope='account' AND user_id=$3) OR (scope='org' AND org_id=$4) OR (scope='membership' AND user_id=$3 AND org_id=$4))",
        vec![
            AUDIENCE.into(),
            captured.source_session_id.clone().into(),
            captured.user_id.into(),
            captured.org_id.into(),
        ],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?
    .try_get::<Option<i64>>("", "c")
    .map_err(database_error)?;
    if max_revoked.is_some_and(|value| value > cutoff) {
        return Err(IssuerError::Unauthorized);
    }
    // Final pre-commit sample after the scope MAX query (R7): reject a source that
    // expired while that query ran.
    if clock.now_unix() >= session_expires_at {
        return Err(IssuerError::Unauthorized);
    }
    let result = Validated {
        subject: Subject {
            user_id: captured.user_id,
            org_id: captured.org_id,
            source_session_id: captured.source_session_id.clone(),
            account_revision: captured.account_revision,
            org_revision: captured.org_revision,
            member_revision: captured.member_revision,
            role: captured.role,
            display_name,
            org_name,
            verified_email,
        },
        expires_at,
        issued_after_cursor: cutoff,
        revocation_cursor,
    };
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Inactive-aware existence lock: holds the row without native active/disabled
/// validation. Used only by `revoke`, which must proceed on already-inactive
/// authority (removal/disable set flags, not deletion).
async fn lock_existing(
    tx: &DatabaseTransaction,
    table: &str,
    column: &str,
    id: i32,
) -> Result<(), IssuerError> {
    db::one(
        tx,
        &format!("SELECT {column} FROM {table} WHERE {column}=$1{}", lock(tx)),
        vec![id.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    Ok(())
}

/// Private revoke: revoke the native source (invalidating pending agent work),
/// append (or reuse) the source session-scope event, and record a revoke intent
/// against the permanent binding. Idempotent by intent id; a same-source reissue
/// is thereby denied while a fresh new-session login remains admissible (R5).
pub(crate) async fn revoke(
    database: &DatabaseConnection,
    relying_ref: &str,
    intent_id: &str,
    clock: &dyn Clock,
) -> Result<RevokeOutcome, IssuerError> {
    if !canonical_uuid(relying_ref) || !canonical_uuid(intent_id) {
        return Err(IssuerError::Invalid);
    }
    let tx = db::begin(database).await?;
    db::lock_admission_guard(&tx).await?;
    // Unlocked routing hint; the authoritative values are the post-domain locked
    // reread below, which every new-intent write is compared against.
    let binding = db::one(
        &tx,
        "SELECT source_session_id,user_id,org_id FROM neev_gateway_relying_bindings WHERE relying_ref=$1 AND audience=$2",
        vec![relying_ref.into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    let source = binding
        .try_get::<String>("", "source_session_id")
        .map_err(database_error)?;
    let user_id = binding.try_get::<i32>("", "user_id").map_err(database_error)?;
    let org_id = binding.try_get::<i32>("", "org_id").map_err(database_error)?;
    // Existing intent: replay returns the same event; a changed ref is Conflict.
    // The stored intent must remain consistent with its permanent binding and its
    // session event (fail closed on a corrupt/inconsistent imported intent).
    if let Some(existing) = db::one(
        &tx,
        "SELECT relying_ref,source_session_id,user_id,org_id,event_id,cursor FROM neev_gateway_revoke_intents WHERE audience=$1 AND intent_id=$2",
        vec![AUDIENCE.into(), intent_id.into()],
    )
    .await?
    {
        if existing.try_get::<String>("", "relying_ref").map_err(database_error)? != relying_ref {
            return Err(IssuerError::Conflict);
        }
        let event_id = existing.try_get::<String>("", "event_id").map_err(database_error)?;
        let cursor = existing.try_get::<i64>("", "cursor").map_err(database_error)?;
        if existing.try_get::<String>("", "source_session_id").map_err(database_error)? != source
            || existing.try_get::<i32>("", "user_id").map_err(database_error)? != user_id
            || existing.try_get::<i32>("", "org_id").map_err(database_error)? != org_id
        {
            return Err(IssuerError::Unavailable);
        }
        let event = db::one(
            &tx,
            "SELECT scope,user_id,org_id,source_session_id FROM neev_gateway_revocations WHERE audience=$1 AND event_id=$2 AND cursor=$3",
            vec![AUDIENCE.into(), event_id.clone().into(), cursor.into()],
        )
        .await?
        .ok_or(IssuerError::Unavailable)?;
        if event.try_get::<String>("", "scope").map_err(database_error)? != "session"
            || event.try_get::<Option<String>>("", "source_session_id").map_err(database_error)? != Some(source.clone())
            || event.try_get::<Option<i32>>("", "user_id").map_err(database_error)? != Some(user_id)
            || event.try_get::<Option<i32>>("", "org_id").map_err(database_error)? != Some(org_id)
        {
            return Err(IssuerError::Unavailable);
        }
        tx.commit().await.map_err(database_error)?;
        return Ok(RevokeOutcome { event_id, cursor });
    }
    // Lock native authority in order using inactive-aware readers.
    lock_existing(&tx, "neev_accounts", "id", user_id).await?;
    lock_existing(&tx, "neev_organizations", "id", org_id).await?;
    db::one(
        &tx,
        &format!(
            "SELECT id FROM neev_memberships WHERE org_id=$1 AND user_id=$2{}",
            lock(&tx)
        ),
        vec![org_id.into(), user_id.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    let session = db::one(
        &tx,
        &format!(
            "SELECT user_id,org_id,revoked_at FROM neev_sessions WHERE id=$1{}",
            lock(&tx)
        ),
        vec![source.clone().into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    if session.try_get::<i32>("", "user_id").map_err(database_error)? != user_id
        || session.try_get::<Option<i32>>("", "org_id").map_err(database_error)? != Some(org_id)
    {
        return Err(IssuerError::Conflict);
    }
    let now = clock.now_unix();
    if !(0..=TIME_MAX).contains(&now) {
        return Err(IssuerError::Invalid);
    }
    // If the source is still live, invalidate pending native login/agent work and
    // conditionally revoke it — before the counter/event. Recovery locks agent
    // rows, which must happen before the audience lock (never after).
    if session
        .try_get::<Option<i64>>("", "revoked_at")
        .map_err(database_error)?
        .is_none()
    {
        invalidate_session_in_transaction(&tx, &source, clock).await?;
        db::execute(
            &tx,
            "UPDATE neev_sessions SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL",
            vec![now.into(), source.clone().into()],
        )
        .await?;
    }
    // Reread the permanent binding and require it unchanged before the counter.
    let reread = db::one(
        &tx,
        &format!(
            "SELECT source_session_id,user_id,org_id FROM neev_gateway_relying_bindings WHERE relying_ref=$1 AND audience=$2{}",
            lock(&tx)
        ),
        vec![relying_ref.into(), AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unauthorized)?;
    if reread.try_get::<String>("", "source_session_id").map_err(database_error)? != source
        || reread.try_get::<i32>("", "user_id").map_err(database_error)? != user_id
        || reread.try_get::<i32>("", "org_id").map_err(database_error)? != org_id
    {
        return Err(IssuerError::Conflict);
    }
    let (event_id, cursor) =
        append_revocation(&tx, "session", Some(user_id), Some(org_id), Some(&source), now).await?;
    db::execute(
        &tx,
        "INSERT INTO neev_gateway_revoke_intents(audience,intent_id,relying_ref,source_session_id,user_id,org_id,event_id,cursor,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        vec![
            AUDIENCE.into(),
            intent_id.into(),
            relying_ref.into(),
            source.into(),
            user_id.into(),
            org_id.into(),
            event_id.clone().into(),
            cursor.into(),
            now.into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(RevokeOutcome { event_id, cursor })
}

/// Private revocations poll: one contiguous page after the durable fetched
/// cursor. Advances `fetched_through`; skipping unseen events is a conflict,
/// while re-fetching `after_cursor <= fetched_through` is intentional replay.
pub(crate) async fn revocations(
    database: &DatabaseConnection,
    after_cursor: i64,
    limit: i64,
) -> Result<RevocationsPage, IssuerError> {
    if after_cursor < 0 || !(1..=50).contains(&limit) {
        return Err(IssuerError::Invalid);
    }
    let tx = db::begin(database).await?;
    let audience = db::one(
        &tx,
        &format!(
            "SELECT last_cursor,fetched_through FROM neev_gateway_audience WHERE audience=$1{}",
            lock(&tx)
        ),
        vec![AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?;
    let last_cursor = audience.try_get::<i64>("", "last_cursor").map_err(database_error)?;
    let fetched_through = audience
        .try_get::<i64>("", "fetched_through")
        .map_err(database_error)?;
    if after_cursor > last_cursor || after_cursor > fetched_through {
        return Err(IssuerError::Conflict);
    }
    let rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT event_id,cursor,scope,user_id,org_id,source_session_id FROM neev_gateway_revocations WHERE audience=$1 AND cursor>$2 ORDER BY cursor LIMIT $3",
            vec![AUDIENCE.into(), after_cursor.into(), limit.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut events = Vec::with_capacity(rows.len());
    for row in &rows {
        events.push(RevocationEvent {
            event_id: row.try_get::<String>("", "event_id").map_err(database_error)?,
            cursor: row.try_get::<i64>("", "cursor").map_err(database_error)?,
            scope: row.try_get::<String>("", "scope").map_err(database_error)?,
            user_id: row.try_get::<Option<i32>>("", "user_id").map_err(database_error)?,
            org_id: row.try_get::<Option<i32>>("", "org_id").map_err(database_error)?,
            source_session_id: row
                .try_get::<Option<String>>("", "source_session_id")
                .map_err(database_error)?,
        });
    }
    let next_cursor = events.last().map(|event| event.cursor).unwrap_or(after_cursor);
    if next_cursor > fetched_through {
        db::execute(
            &tx,
            "UPDATE neev_gateway_audience SET fetched_through=$1 WHERE audience=$2",
            vec![next_cursor.into(), AUDIENCE.into()],
        )
        .await?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(RevocationsPage {
        events,
        next_cursor,
        high_watermark: last_cursor,
    })
}

/// Private ack: monotonic acknowledgement through fetched events for a
/// non-decreasing writer epoch.
pub(crate) async fn ack(
    database: &DatabaseConnection,
    through_cursor: i64,
    writer_epoch: i64,
) -> Result<i64, IssuerError> {
    if through_cursor < 0 || writer_epoch <= 0 {
        return Err(IssuerError::Invalid);
    }
    let tx = db::begin(database).await?;
    let audience = db::one(
        &tx,
        &format!(
            "SELECT fetched_through,acknowledged_cursor,writer_epoch FROM neev_gateway_audience WHERE audience=$1{}",
            lock(&tx)
        ),
        vec![AUDIENCE.into()],
    )
    .await?
    .ok_or(IssuerError::Unavailable)?;
    let fetched_through = audience
        .try_get::<i64>("", "fetched_through")
        .map_err(database_error)?;
    let acknowledged = audience
        .try_get::<i64>("", "acknowledged_cursor")
        .map_err(database_error)?;
    let epoch = audience.try_get::<i64>("", "writer_epoch").map_err(database_error)?;
    if writer_epoch < epoch || through_cursor > fetched_through {
        return Err(IssuerError::Conflict);
    }
    let new_acknowledged = acknowledged.max(through_cursor);
    db::execute(
        &tx,
        "UPDATE neev_gateway_audience SET acknowledged_cursor=$1,writer_epoch=$2 WHERE audience=$3",
        vec![new_acknowledged.into(), writer_epoch.into(), AUDIENCE.into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(new_acknowledged)
}
