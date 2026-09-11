use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::neev::{
    clock::Clock,
    config::Config,
    db,
    error::{database_error, AuthError},
    types::{Provider, SessionSecrets, SignInIntent},
};

use super::{
    google::Google,
    identity,
    secrets::{self, TokenPurpose},
    session,
};

const ATTEMPT_TTL: i64 = 10 * 60;
const BROWSER_WINDOW: i64 = 15 * 60;
const GLOBAL_WINDOW: i64 = 60;
const BROWSER_LIMIT: i64 = 5;
const GLOBAL_LIMIT: i64 = 60;
const ACTIVE_LIMIT: i64 = 1000;
const CLEANUP_LIMIT: i64 = 100;

#[derive(Serialize, Deserialize)]
struct ExchangeSecrets {
    pkce_verifier: String,
    nonce: String,
}

pub(crate) struct ClaimedAttempt {
    pub(crate) intent: SignInIntent,
    pub(crate) pkce_verifier: String,
    pub(crate) nonce: String,
}

pub(crate) enum CallbackResult {
    Code(String),
    Denied,
}

pub(crate) enum Completion {
    Login(SessionSecrets),
    Linked,
}

pub(crate) struct Service {
    database: DatabaseConnection,
    encryption_key: [u8; 32],
    clock: Arc<dyn Clock + Send + Sync>,
    google: Option<Google>,
    #[cfg(test)]
    admission_pause: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
}

impl Service {
    pub(crate) fn new(
        database: DatabaseConnection,
        config: &Config,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, AuthError> {
        let google = config
            .providers
            .google
            .as_ref()
            .map(|provider| {
                Google::new(
                    provider.id.clone(),
                    provider.secret.clone(),
                    config.google_callback.clone(),
                )
            })
            .transpose()?;
        Ok(Self {
            database,
            encryption_key: config.encryption_key,
            clock,
            google,
            #[cfg(test)]
            admission_pause: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_google(
        database: DatabaseConnection,
        encryption_key: [u8; 32],
        clock: Arc<dyn Clock + Send + Sync>,
        google: Google,
    ) -> Self {
        Self {
            database,
            encryption_key,
            clock,
            google: Some(google),
            admission_pause: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_admission_pause(
        mut self,
        entered: Arc<tokio::sync::Barrier>,
        release: Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.admission_pause = Some((entered, release));
        self
    }

    pub(crate) fn google_available(&self) -> bool {
        self.google.is_some()
    }

    pub(crate) async fn begin(
        &self,
        provider: Provider,
        intent: SignInIntent,
        browser_secret: &str,
    ) -> Result<String, AuthError> {
        if provider != Provider::Google {
            return Err(AuthError::Unavailable);
        }
        let google = self.google.as_ref().ok_or(AuthError::Unavailable)?;
        let state = secrets::random_token(TokenPurpose::OAuth)?;
        let now = self.clock.now_unix();
        let prepared = google.authorization(&state)?;
        #[cfg(test)]
        insert_attempt_after_guard(
            &self.database,
            &self.encryption_key,
            provider,
            intent,
            browser_secret,
            &state,
            &prepared.pkce_verifier,
            &prepared.nonce,
            now,
            {
                let pause = self.admission_pause.clone();
                move || async move {
                    if let Some((entered, release)) = pause {
                        entered.wait().await;
                        release.wait().await;
                    }
                }
            },
        )
        .await?;
        #[cfg(not(test))]
        insert_attempt(
            &self.database,
            &self.encryption_key,
            provider,
            intent,
            browser_secret,
            &state,
            &prepared.pkce_verifier,
            &prepared.nonce,
            now,
        )
        .await?;
        Ok(prepared.url)
    }

    pub(crate) async fn complete(
        &self,
        provider: Provider,
        state: &str,
        result: CallbackResult,
        browser_secret: &str,
    ) -> Result<Completion, AuthError> {
        if provider != Provider::Google {
            return Err(AuthError::Unavailable);
        }
        let google = self.google.as_ref().ok_or(AuthError::Unavailable)?;
        let claimed = claim_attempt(
            &self.database,
            &self.encryption_key,
            provider,
            state,
            browser_secret,
            self.clock.now_unix(),
        )
        .await?;
        let CallbackResult::Code(code) = result else {
            return Err(AuthError::Unauthorized);
        };
        if code.is_empty() || code.len() > 2048 || code.chars().any(char::is_control) {
            return Err(AuthError::Invalid);
        }
        let proof = google
            .exchange(
                &code,
                &claimed.pkce_verifier,
                &claimed.nonce,
                self.clock.now_unix(),
            )
            .await?;
        let now = self.clock.now_unix();
        match claimed.intent {
            SignInIntent::Login => {
                let user_id =
                    identity::resolve(&self.database, proof, SignInIntent::Login, now).await?;
                Ok(Completion::Login(
                    session::issue_identity(&self.database, user_id, now).await?,
                ))
            }
            intent @ SignInIntent::Link { .. } => {
                identity::resolve(&self.database, proof, intent, now).await?;
                Ok(Completion::Linked)
            }
        }
    }
}

#[cfg(test)]
pub(crate) async fn reserve_attempt(
    database: &DatabaseConnection,
    encryption_key: &[u8; 32],
    provider: Provider,
    intent: SignInIntent,
    browser_secret: &str,
    pkce_verifier: &str,
    nonce: &str,
    now: i64,
) -> Result<String, AuthError> {
    let state = secrets::random_token(TokenPurpose::OAuth)?;
    insert_attempt(
        database,
        encryption_key,
        provider,
        intent,
        browser_secret,
        &state,
        pkce_verifier,
        nonce,
        now,
    )
    .await?;
    Ok(state)
}

async fn insert_attempt(
    database: &DatabaseConnection,
    encryption_key: &[u8; 32],
    provider: Provider,
    intent: SignInIntent,
    browser_secret: &str,
    state: &str,
    pkce_verifier: &str,
    nonce: &str,
    now: i64,
) -> Result<(), AuthError> {
    insert_attempt_after_guard(
        database,
        encryption_key,
        provider,
        intent,
        browser_secret,
        state,
        pkce_verifier,
        nonce,
        now,
        || async {},
    )
    .await
}

async fn insert_attempt_after_guard<F, Future>(
    database: &DatabaseConnection,
    encryption_key: &[u8; 32],
    provider: Provider,
    intent: SignInIntent,
    browser_secret: &str,
    state: &str,
    pkce_verifier: &str,
    nonce: &str,
    now: i64,
    after_guard: F,
) -> Result<(), AuthError>
where
    F: FnOnce() -> Future,
    Future: std::future::Future<Output = ()>,
{
    if provider == Provider::Email
        || now < 0
        || pkce_verifier.is_empty()
        || pkce_verifier.len() > 128
        || nonce.is_empty()
        || nonce.len() > 255
    {
        return Err(AuthError::Invalid);
    }
    let expires_at = now.checked_add(ATTEMPT_TTL).ok_or(AuthError::Invalid)?;
    let global_after = now
        .checked_add(ATTEMPT_TTL - GLOBAL_WINDOW)
        .ok_or(AuthError::Invalid)?;
    let browser_after = now
        .checked_sub(BROWSER_WINDOW - ATTEMPT_TTL)
        .ok_or(AuthError::Invalid)?;
    let cleanup_before = now
        .checked_sub(BROWSER_WINDOW - ATTEMPT_TTL)
        .ok_or(AuthError::Invalid)?;
    let browser_hash = secrets::digest(TokenPurpose::Browser, browser_secret)?;
    let state_hash = secrets::digest(TokenPurpose::OAuth, &state)?;
    let (intent_name, link_user_id, link_session_id, link_revision) = intent_columns(&intent);
    let aad = aad(&state, browser_secret, provider, &intent);
    let exchange_enc = encrypt(
        encryption_key,
        &aad,
        &ExchangeSecrets {
            pkce_verifier: pkce_verifier.to_owned(),
            nonce: nonce.to_owned(),
        },
    )?;

    let tx = db::begin(database).await?;
    db::lock_admission_guard(&tx).await?;
    after_guard().await;
    db::execute(
        &tx,
        "DELETE FROM neev_oauth_attempts WHERE state_hash IN (SELECT state_hash FROM neev_oauth_attempts WHERE expires_at<=$1 ORDER BY expires_at LIMIT $2)",
        vec![cleanup_before.into(), CLEANUP_LIMIT.into()],
    )
    .await?;
    if count(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_oauth_attempts WHERE expires_at>$1",
        vec![global_after.into()],
    )
    .await?
        >= GLOBAL_LIMIT
        || count(
            &tx,
            "SELECT COUNT(*) AS n FROM neev_oauth_attempts WHERE browser_hash=$1 AND expires_at>$2",
            vec![browser_hash.clone().into(), browser_after.into()],
        )
        .await?
            >= BROWSER_LIMIT
        || count(
            &tx,
            "SELECT COUNT(*) AS n FROM neev_oauth_attempts WHERE consumed_at IS NULL AND expires_at>$1",
            vec![now.into()],
        )
        .await?
            >= ACTIVE_LIMIT
    {
        return Err(AuthError::Limited);
    }
    db::execute(
        &tx,
        "INSERT INTO neev_oauth_attempts(state_hash,browser_hash,provider,intent,link_user_id,link_session_id,link_revision,exchange_enc,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        vec![
            state_hash.into(),
            browser_hash.into(),
            provider_name(provider).into(),
            intent_name.into(),
            link_user_id.into(),
            link_session_id.into(),
            link_revision.into(),
            exchange_enc.into(),
            expires_at.into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

pub(crate) async fn claim_attempt(
    database: &DatabaseConnection,
    encryption_key: &[u8; 32],
    provider: Provider,
    state: &str,
    browser_secret: &str,
    now: i64,
) -> Result<ClaimedAttempt, AuthError> {
    if provider == Provider::Email || now < 0 {
        return Err(AuthError::Unauthorized);
    }
    let state_hash =
        secrets::digest(TokenPurpose::OAuth, state).map_err(|_| AuthError::Unauthorized)?;
    let browser_hash = secrets::digest(TokenPurpose::Browser, browser_secret)
        .map_err(|_| AuthError::Unauthorized)?;
    let tx = db::begin(database).await?;
    let lock = if tx.get_database_backend() == DbBackend::Postgres {
        " FOR UPDATE"
    } else {
        ""
    };
    let row = db::one(
        &tx,
        &format!("SELECT browser_hash,provider,intent,link_user_id,link_session_id,link_revision,exchange_enc,expires_at,consumed_at FROM neev_oauth_attempts WHERE state_hash=$1{lock}"),
        vec![state_hash.clone().into()],
    )
    .await?
    .ok_or(AuthError::Unauthorized)?;
    let stored_browser: String = row.try_get("", "browser_hash").map_err(database_error)?;
    let stored_provider: String = row.try_get("", "provider").map_err(database_error)?;
    let intent_name: String = row.try_get("", "intent").map_err(database_error)?;
    let link_user_id: Option<i32> = row.try_get("", "link_user_id").map_err(database_error)?;
    let link_session_id: Option<String> =
        row.try_get("", "link_session_id").map_err(database_error)?;
    let link_revision: Option<i64> = row.try_get("", "link_revision").map_err(database_error)?;
    let exchange_enc: String = row.try_get("", "exchange_enc").map_err(database_error)?;
    let expires_at: i64 = row.try_get("", "expires_at").map_err(database_error)?;
    let consumed_at: Option<i64> = row.try_get("", "consumed_at").map_err(database_error)?;
    if stored_browser != browser_hash
        || stored_provider != provider_name(provider)
        || expires_at <= now
        || consumed_at.is_some()
    {
        return Err(AuthError::Unauthorized);
    }
    let intent = stored_intent(intent_name, link_user_id, link_session_id, link_revision)?;
    let exchange: ExchangeSecrets = decrypt(
        encryption_key,
        &aad(state, browser_secret, provider, &intent),
        &exchange_enc,
    )?;
    let result = db::execute(
        &tx,
        "UPDATE neev_oauth_attempts SET consumed_at=$1,exchange_enc='' WHERE state_hash=$2 AND consumed_at IS NULL",
        vec![now.into(), state_hash.into()],
    )
    .await?;
    if result.rows_affected() != 1 {
        return Err(AuthError::Unauthorized);
    }
    tx.commit().await.map_err(database_error)?;
    Ok(ClaimedAttempt {
        intent,
        pkce_verifier: exchange.pkce_verifier,
        nonce: exchange.nonce,
    })
}

async fn count<C: ConnectionTrait>(
    database: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<i64, AuthError> {
    db::one(database, sql, values)
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)
}

fn intent_columns(
    intent: &SignInIntent,
) -> (&'static str, Option<i32>, Option<String>, Option<i64>) {
    match intent {
        SignInIntent::Login => ("login", None, None, None),
        SignInIntent::Link {
            user_id,
            session_id,
            auth_revision,
        } => (
            "link",
            Some(*user_id),
            Some(session_id.clone()),
            Some(*auth_revision),
        ),
    }
}

fn stored_intent(
    name: String,
    user_id: Option<i32>,
    session_id: Option<String>,
    revision: Option<i64>,
) -> Result<SignInIntent, AuthError> {
    match (name.as_str(), user_id, session_id, revision) {
        ("login", None, None, None) => Ok(SignInIntent::Login),
        ("link", Some(user_id), Some(session_id), Some(auth_revision)) => Ok(SignInIntent::Link {
            user_id,
            session_id,
            auth_revision,
        }),
        _ => Err(AuthError::Unauthorized),
    }
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "google",
        Provider::Github => "github",
        Provider::Email => "email",
    }
}

fn aad(state: &str, browser: &str, provider: Provider, intent: &SignInIntent) -> Vec<u8> {
    let (intent_name, user_id, session_id, revision) = intent_columns(intent);
    format!(
        "neev-oauth-v1\0{state}\0{browser}\0{}\0{intent_name}\0{}\0{}\0{}",
        provider_name(provider),
        user_id.map_or_else(String::new, |value| value.to_string()),
        session_id.unwrap_or_default(),
        revision.map_or_else(String::new, |value| value.to_string()),
    )
    .into_bytes()
}

fn encrypt(key: &[u8; 32], aad: &[u8], value: &ExchangeSecrets) -> Result<String, AuthError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AuthError::Unavailable)?;
    let mut nonce = [0u8; 12];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| AuthError::Unavailable)?;
    let plaintext = serde_json::to_vec(value).map_err(|_| AuthError::Unavailable)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad,
            },
        )
        .map_err(|_| AuthError::Unavailable)?;
    let mut output = nonce.to_vec();
    output.extend(ciphertext);
    Ok(URL_SAFE_NO_PAD.encode(output))
}

fn decrypt(key: &[u8; 32], aad: &[u8], value: &str) -> Result<ExchangeSecrets, AuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthError::Unauthorized)?;
    let (nonce, ciphertext) = bytes.split_at_checked(12).ok_or(AuthError::Unauthorized)?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AuthError::Unavailable)?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| AuthError::Unauthorized)?;
    serde_json::from_slice(&plaintext).map_err(|_| AuthError::Unauthorized)
}
