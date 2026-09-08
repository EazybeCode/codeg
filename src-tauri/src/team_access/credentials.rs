use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration as StdDuration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use super::{
    core::{lock_user_identity, TeamAuthError, TeamClock},
    oauth::{GithubOAuthSource, GithubTokenSet},
    secrets::{SecretBinding, SecretVault},
};

#[path = "credential_store.rs"]
mod credential_store;

const STORED_TOKEN_VERSION: i64 = 1;
const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_ENCODED_TOKEN_BYTES: usize = 5_464;
const REFRESH_MARGIN_SECONDS: i64 = 60;
const REFRESH_LEASE_SECONDS: i64 = 45;
const OTHER_REFRESH_WAIT: StdDuration = StdDuration::from_secs(20);
const OTHER_REFRESH_POLL: StdDuration = StdDuration::from_millis(50);

#[derive(Serialize, Deserialize)]
pub(super) struct StoredTokens {
    version: i64,
    access_token_b64: String,
    refresh_token_b64: Option<String>,
    access_expires_at: Option<DateTime<Utc>>,
    refresh_expires_at: Option<DateTime<Utc>>,
}

impl StoredTokens {
    pub(super) fn from_tokens(tokens: &GithubTokenSet) -> Result<Self, TeamAuthError> {
        validate_token(&tokens.access_token)?;
        if let Some(refresh) = &tokens.refresh_token {
            validate_token(refresh)?;
        }
        validate_expiring_shape(
            tokens.refresh_token.is_some(),
            tokens.access_expires_at.is_some(),
            tokens.refresh_expires_at.is_some(),
        )?;
        Ok(Self {
            version: STORED_TOKEN_VERSION,
            access_token_b64: STANDARD.encode(tokens.access_token.as_bytes()),
            refresh_token_b64: tokens
                .refresh_token
                .as_ref()
                .map(|value| STANDARD.encode(value.as_bytes())),
            access_expires_at: tokens.access_expires_at,
            refresh_expires_at: tokens.refresh_expires_at,
        })
    }

    pub(super) fn to_tokens(&self) -> Result<GithubTokenSet, TeamAuthError> {
        if self.version != STORED_TOKEN_VERSION {
            return Err(TeamAuthError::Unavailable);
        }
        validate_expiring_shape(
            self.refresh_token_b64.is_some(),
            self.access_expires_at.is_some(),
            self.refresh_expires_at.is_some(),
        )?;
        Ok(GithubTokenSet {
            access_token: decode_token(&self.access_token_b64)?,
            refresh_token: self
                .refresh_token_b64
                .as_deref()
                .map(decode_token)
                .transpose()?,
            access_expires_at: self.access_expires_at,
            refresh_expires_at: self.refresh_expires_at,
        })
    }
}

fn validate_expiring_shape(
    has_refresh: bool,
    has_access_expiry: bool,
    has_refresh_expiry: bool,
) -> Result<(), TeamAuthError> {
    if (has_refresh && has_access_expiry && has_refresh_expiry)
        || (!has_refresh && !has_access_expiry && !has_refresh_expiry)
    {
        Ok(())
    } else {
        Err(TeamAuthError::Unavailable)
    }
}

fn validate_token(value: &str) -> Result<(), TeamAuthError> {
    if (1..=MAX_TOKEN_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(TeamAuthError::Unavailable)
    }
}

fn decode_token(encoded: &str) -> Result<String, TeamAuthError> {
    if encoded.is_empty() || encoded.len() > MAX_ENCODED_TOKEN_BYTES {
        return Err(TeamAuthError::Unavailable);
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| TeamAuthError::Unavailable)?;
    if decoded.len() > MAX_TOKEN_BYTES {
        return Err(TeamAuthError::Unavailable);
    }
    let value = String::from_utf8(decoded).map_err(|_| TeamAuthError::Unavailable)?;
    validate_token(&value)?;
    Ok(value)
}

pub(crate) struct AccessGrant {
    pub(crate) user_id: i32,
    pub(crate) github_user_id: i64,
    pub(crate) generation: i64,
    pub(crate) access_token: String,
}

pub(crate) struct CredentialService {
    db: DatabaseConnection,
    vault: Arc<SecretVault>,
    oauth: Arc<dyn GithubOAuthSource>,
    clock: Arc<dyn TeamClock>,
    locks: Mutex<HashMap<i32, Weak<AsyncMutex<()>>>>,
}

impl CredentialService {
    pub(crate) fn new(
        db: DatabaseConnection,
        vault: Arc<SecretVault>,
        oauth: Arc<dyn GithubOAuthSource>,
        clock: Arc<dyn TeamClock>,
    ) -> Self {
        Self {
            db,
            vault,
            oauth,
            clock,
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn access(
        &self,
        user_id: i32,
        github_user_id: i64,
    ) -> Result<AccessGrant, TeamAuthError> {
        let lock = self.user_lock(user_id)?;
        let _guard = lock.lock().await;
        self.access_serialized(user_id, github_user_id).await
    }

    pub(crate) async fn install_in_transaction(
        &self,
        transaction: &DatabaseTransaction,
        user_id: i32,
        github_user_id: i64,
        tokens: &GithubTokenSet,
        now: DateTime<Utc>,
    ) -> Result<i64, TeamAuthError> {
        validate_installable(tokens, now)?;
        lock_user_identity(transaction, user_id, github_user_id).await?;
        let existing = credential_store::load(transaction, user_id).await?;
        if existing
            .as_ref()
            .is_some_and(|row| row.github_user_id != github_user_id)
        {
            return Err(TeamAuthError::Unavailable);
        }
        let previous_generation = existing.as_ref().map(|row| row.generation);
        let generation = match previous_generation {
            None => 1,
            Some(previous) if previous > 0 => previous
                .checked_add(1)
                .ok_or(TeamAuthError::Unavailable)?,
            Some(_) => return Err(TeamAuthError::Unavailable),
        };
        let encrypted = self.seal(user_id, github_user_id, generation, tokens)?;
        credential_store::replace_active(
            transaction,
            existing,
            user_id,
            github_user_id,
            generation,
            encrypted,
            now,
        )
        .await?;
        if let Some(previous_generation) = previous_generation {
            credential_store::advance_bindings(
                transaction,
                user_id,
                previous_generation,
                generation,
                now,
            )
            .await?;
        }
        Ok(generation)
    }

    pub(crate) async fn invalidate_if_generation(
        &self,
        user_id: i32,
        github_user_id: i64,
        generation: i64,
    ) -> Result<bool, TeamAuthError> {
        let transaction = self.db.begin().await.map_err(|_| TeamAuthError::Unavailable)?;
        lock_user_identity(&transaction, user_id, github_user_id).await?;
        let Some(row) = credential_store::load(&transaction, user_id).await? else {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(false);
        };
        if row.github_user_id != github_user_id || row.generation != generation {
            if row.github_user_id != github_user_id {
                transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
                return Err(TeamAuthError::Unavailable);
            }
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(false);
        }
        let changed = credential_store::invalidate(
            &transaction,
            user_id,
            github_user_id,
            generation,
            &row.state,
            row.refresh_owner.as_deref(),
            row.refresh_expires_at,
            self.clock.now(),
        )
        .await?;
        transaction.commit().await.map_err(|_| TeamAuthError::Unavailable)?;
        Ok(changed)
    }

    fn user_lock(&self, user_id: i32) -> Result<Arc<AsyncMutex<()>>, TeamAuthError> {
        let mut locks = self.locks.lock().map_err(|_| TeamAuthError::Unavailable)?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&user_id).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(user_id, Arc::downgrade(&lock));
        Ok(lock)
    }

    async fn access_serialized(
        &self,
        user_id: i32,
        github_user_id: i64,
    ) -> Result<AccessGrant, TeamAuthError> {
        let wait_started = Instant::now();
        loop {
            let row = credential_store::load(&self.db, user_id)
                .await?
                .ok_or(TeamAuthError::Unauthorized)?;
            if row.github_user_id != github_user_id {
                return Err(TeamAuthError::Unavailable);
            }
            let now = self.clock.now();
            match row.state.as_str() {
                "active" => {
                    let tokens = self.open(&row)?;
                    let refresh_at = now
                        .checked_add_signed(Duration::seconds(REFRESH_MARGIN_SECONDS))
                        .ok_or(TeamAuthError::Unavailable)?;
                    if tokens
                        .access_expires_at
                        .is_none_or(|expires_at| expires_at > refresh_at)
                    {
                        return Ok(grant(user_id, github_user_id, row.generation, tokens));
                    }
                    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
                        if self.invalidate_snapshot(&row).await? {
                            return Err(TeamAuthError::Unauthorized);
                        }
                        continue;
                    };
                    if tokens.refresh_expires_at.is_none_or(|expires_at| expires_at <= now) {
                        if self.invalidate_snapshot(&row).await? {
                            return Err(TeamAuthError::Unauthorized);
                        }
                        continue;
                    }
                    let owner = random_owner()?;
                    let Some(deadline) = self.claim(&row, &owner, now).await? else {
                        continue;
                    };
                    let refreshed = self.oauth.refresh(refresh_token, now).await;
                    let tokens = match refreshed {
                        Ok(tokens) if validate_installable(&tokens, self.clock.now()).is_ok() => {
                            tokens
                        }
                        _ => {
                            if self
                                .invalidate_claim(&row, &owner, deadline, self.clock.now())
                                .await?
                            {
                                return Err(TeamAuthError::Unauthorized);
                            }
                            continue;
                        }
                    };
                    if let Some(grant) = self.finish(&row, &owner, deadline, &tokens).await? {
                        return Ok(grant);
                    }
                }
                "refreshing" => {
                    let owner = row.refresh_owner.as_deref().ok_or(TeamAuthError::Unavailable)?;
                    let deadline = row.refresh_expires_at.ok_or(TeamAuthError::Unavailable)?;
                    if deadline <= now {
                        if self.invalidate_claim(&row, owner, deadline, now).await? {
                            return Err(TeamAuthError::Unauthorized);
                        }
                        continue;
                    }
                    if wait_started.elapsed() >= OTHER_REFRESH_WAIT {
                        return Err(TeamAuthError::Unavailable);
                    }
                    tokio::time::sleep(OTHER_REFRESH_POLL).await;
                }
                "reauth_required" => return Err(TeamAuthError::Unauthorized),
                _ => return Err(TeamAuthError::Unavailable),
            }
        }
    }

    fn seal(
        &self,
        user_id: i32,
        github_user_id: i64,
        generation: i64,
        tokens: &GithubTokenSet,
    ) -> Result<String, TeamAuthError> {
        let stored = StoredTokens::from_tokens(tokens)?;
        let plaintext = serde_json::to_vec(&stored).map_err(|_| TeamAuthError::Unavailable)?;
        self.vault
            .seal(
                SecretBinding::GithubCredential {
                    user_id,
                    github_user_id,
                    generation,
                },
                &plaintext,
            )
            .map_err(|_| TeamAuthError::Unavailable)
    }

    fn open(
        &self,
        row: &crate::db::entities::team_github_credential::Model,
    ) -> Result<GithubTokenSet, TeamAuthError> {
        let encrypted = row
            .encrypted_payload
            .as_deref()
            .ok_or(TeamAuthError::Unavailable)?;
        let plaintext = self
            .vault
            .open(
                SecretBinding::GithubCredential {
                    user_id: row.user_id,
                    github_user_id: row.github_user_id,
                    generation: row.generation,
                },
                encrypted,
            )
            .map_err(|_| TeamAuthError::Unavailable)?;
        let stored: StoredTokens =
            serde_json::from_slice(&plaintext).map_err(|_| TeamAuthError::Unavailable)?;
        stored.to_tokens()
    }

    async fn claim(
        &self,
        snapshot: &crate::db::entities::team_github_credential::Model,
        owner: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, TeamAuthError> {
        let deadline = now
            .checked_add_signed(Duration::seconds(REFRESH_LEASE_SECONDS))
            .ok_or(TeamAuthError::Unavailable)?;
        let transaction = self.db.begin().await.map_err(|_| TeamAuthError::Unavailable)?;
        lock_user_identity(&transaction, snapshot.user_id, snapshot.github_user_id).await?;
        let current = credential_store::load(&transaction, snapshot.user_id).await?;
        if !same_snapshot(current.as_ref(), snapshot, "active", None) {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(None);
        }
        let claimed = credential_store::claim_refresh(
            &transaction,
            snapshot.user_id,
            snapshot.github_user_id,
            snapshot.generation,
            owner,
            deadline,
            now,
        )
        .await?;
        if !claimed {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(None);
        }
        transaction.commit().await.map_err(|_| TeamAuthError::Unavailable)?;
        Ok(Some(deadline))
    }

    async fn finish(
        &self,
        snapshot: &crate::db::entities::team_github_credential::Model,
        owner: &str,
        deadline: DateTime<Utc>,
        tokens: &GithubTokenSet,
    ) -> Result<Option<AccessGrant>, TeamAuthError> {
        let now = self.clock.now();
        let generation = snapshot
            .generation
            .checked_add(1)
            .ok_or(TeamAuthError::Unavailable)?;
        let encrypted = self.seal(
            snapshot.user_id,
            snapshot.github_user_id,
            generation,
            tokens,
        )?;
        let transaction = self.db.begin().await.map_err(|_| TeamAuthError::Unavailable)?;
        lock_user_identity(&transaction, snapshot.user_id, snapshot.github_user_id).await?;
        let current = credential_store::load(&transaction, snapshot.user_id).await?;
        let live = current.as_ref().is_some_and(|row| {
            same_snapshot(Some(row), snapshot, "refreshing", Some(owner))
                && row.refresh_expires_at == Some(deadline)
                && deadline > now
        });
        if !live {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(None);
        }
        if !credential_store::finish_refresh(
            &transaction,
            snapshot.user_id,
            snapshot.github_user_id,
            snapshot.generation,
            generation,
            owner,
            deadline,
            encrypted,
            now,
        )
        .await?
        {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(None);
        }
        credential_store::advance_bindings(
            &transaction,
            snapshot.user_id,
            snapshot.generation,
            generation,
            now,
        )
        .await?;
        transaction.commit().await.map_err(|_| TeamAuthError::Unavailable)?;
        Ok(Some(grant(
            snapshot.user_id,
            snapshot.github_user_id,
            generation,
            GithubTokenSet {
                access_token: tokens.access_token.clone(),
                refresh_token: None,
                access_expires_at: None,
                refresh_expires_at: None,
            },
        )))
    }

    async fn invalidate_snapshot(
        &self,
        snapshot: &crate::db::entities::team_github_credential::Model,
    ) -> Result<bool, TeamAuthError> {
        let transaction = self.db.begin().await.map_err(|_| TeamAuthError::Unavailable)?;
        lock_user_identity(&transaction, snapshot.user_id, snapshot.github_user_id).await?;
        let current = credential_store::load(&transaction, snapshot.user_id).await?;
        if !same_snapshot(
            current.as_ref(),
            snapshot,
            &snapshot.state,
            snapshot.refresh_owner.as_deref(),
        ) {
            transaction.rollback().await.map_err(|_| TeamAuthError::Unavailable)?;
            return Ok(false);
        }
        let changed = credential_store::invalidate(
            &transaction,
            snapshot.user_id,
            snapshot.github_user_id,
            snapshot.generation,
            &snapshot.state,
            snapshot.refresh_owner.as_deref(),
            snapshot.refresh_expires_at,
            self.clock.now(),
        )
        .await?;
        transaction.commit().await.map_err(|_| TeamAuthError::Unavailable)?;
        Ok(changed)
    }

    async fn invalidate_claim(
        &self,
        snapshot: &crate::db::entities::team_github_credential::Model,
        owner: &str,
        deadline: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, TeamAuthError> {
        let transaction = self.db.begin().await.map_err(|_| TeamAuthError::Unavailable)?;
        lock_user_identity(&transaction, snapshot.user_id, snapshot.github_user_id).await?;
        let changed = credential_store::invalidate(
            &transaction,
            snapshot.user_id,
            snapshot.github_user_id,
            snapshot.generation,
            "refreshing",
            Some(owner),
            Some(deadline),
            now,
        )
        .await?;
        transaction.commit().await.map_err(|_| TeamAuthError::Unavailable)?;
        Ok(changed)
    }
}

fn validate_installable(tokens: &GithubTokenSet, now: DateTime<Utc>) -> Result<(), TeamAuthError> {
    StoredTokens::from_tokens(tokens)?;
    if tokens.access_expires_at.is_some_and(|expires_at| expires_at <= now)
        || tokens.refresh_expires_at.is_some_and(|expires_at| expires_at <= now)
    {
        return Err(TeamAuthError::Unauthorized);
    }
    Ok(())
}

fn same_snapshot(
    current: Option<&crate::db::entities::team_github_credential::Model>,
    snapshot: &crate::db::entities::team_github_credential::Model,
    state: &str,
    owner: Option<&str>,
) -> bool {
    current.is_some_and(|row| {
        row.user_id == snapshot.user_id
            && row.github_user_id == snapshot.github_user_id
            && row.generation == snapshot.generation
            && row.state == state
            && row.refresh_owner.as_deref() == owner
    })
}

fn grant(
    user_id: i32,
    github_user_id: i64,
    generation: i64,
    tokens: GithubTokenSet,
) -> AccessGrant {
    AccessGrant {
        user_id,
        github_user_id,
        generation,
        access_token: tokens.access_token,
    }
}

fn random_owner() -> Result<String, TeamAuthError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| TeamAuthError::Unavailable)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(output)
}
