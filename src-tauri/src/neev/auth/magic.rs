use std::{net::IpAddr, sync::Arc};

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend};
use url::Url;

use crate::neev::{
    auth::{email::canonical_email, identity, secrets, secrets::TokenPurpose, session},
    clock::Clock,
    db,
    error::database_error,
    error::AuthError,
    mail::{self, quota, MailMessage},
    types::{SessionSecrets, SignInIntent},
};

const ATTEMPT_TTL: i64 = 10 * 60;
const CLEANUP_LIMIT: i64 = 256;

pub(crate) enum Completion {
    LoggedIn(SessionSecrets),
    Linked,
}

#[derive(Clone)]
pub(crate) struct Service {
    database: DatabaseConnection,
    key: [u8; 32],
    origin: Url,
    clock: Arc<dyn Clock + Send + Sync>,
    mail: mail::Service,
}

impl Service {
    pub(crate) fn new(
        database: DatabaseConnection,
        key: [u8; 32],
        origin: Url,
        clock: Arc<dyn Clock + Send + Sync>,
        mail: mail::Service,
    ) -> Self {
        Self {
            database,
            key,
            origin,
            clock,
            mail,
        }
    }
}

pub(crate) async fn request(
    service: &Service,
    email: &str,
    browser: &str,
    source_ip: IpAddr,
    intent: SignInIntent,
) -> Result<(), AuthError> {
    let canonical = canonical_email(email).ok_or(AuthError::Invalid)?;
    let browser_hash = secrets::digest(TokenPurpose::Browser, browser)?;
    let token = secrets::random_token(TokenPurpose::Magic)?;
    let token_hash = secrets::digest(TokenPurpose::Magic, &token)?;
    let now = service.clock.now_unix();
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let (intent_name, link_user_id, link_session_id, link_revision) = intent_columns(&intent);
    let tx = db::begin(&service.database).await?;
    quota::lock_guard(&tx).await?;
    db::execute(
        &tx,
        "DELETE FROM neev_magic_attempts WHERE token_hash IN (SELECT token_hash FROM neev_magic_attempts WHERE expires_at<=$1 ORDER BY expires_at LIMIT $2)",
        vec![now.into(), CLEANUP_LIMIT.into()],
    )
    .await?;
    quota::reserve_in_transaction(
        &tx,
        &service.key,
        &[
            quota::Bucket::MagicAddress(canonical.clone()),
            quota::Bucket::MagicSourceIp(source_ip),
            quota::Bucket::DeliveryMinute,
            quota::Bucket::DeliveryDay,
        ],
        now,
    )
    .await?;
    tx.commit().await.map_err(database_error)?;

    let mut confirmation = service
        .origin
        .join("auth/email/confirm")
        .map_err(|_| AuthError::Unavailable)?;
    confirmation.set_fragment(Some(&format!("token={token}")));
    let message = MailMessage {
        recipient: canonical.clone(),
        subject: "Sign in to Neev".to_owned(),
        text: format!(
            "Confirm your Neev sign-in in the browser that requested it:\n\n{}\n\nThis one-use link expires in 10 minutes.",
            confirmation.as_str()
        ),
    };
    if let Err(error) = service.mail.send(message).await {
        tracing::warn!(
            reason = mail_failure_reason(error),
            "Neev mail request was not delivered"
        );
        return Err(error);
    }
    let delivered_at = service.clock.now_unix();
    let expires_at = delivered_at
        .checked_add(ATTEMPT_TTL)
        .filter(|_| delivered_at >= 0)
        .ok_or(AuthError::Invalid)?;
    db::execute(
        &service.database,
        "INSERT INTO neev_magic_attempts(token_hash,browser_hash,canonical_email,intent,link_user_id,link_session_id,link_revision,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        vec![
            token_hash.into(),
            browser_hash.into(),
            canonical.into(),
            intent_name.into(),
            link_user_id.into(),
            link_session_id.into(),
            link_revision.into(),
            expires_at.into(),
        ],
    )
    .await?;
    Ok(())
}

pub(crate) async fn confirm(
    service: &Service,
    token: &str,
    browser: &str,
) -> Result<Completion, AuthError> {
    let token_hash =
        secrets::digest(TokenPurpose::Magic, token).map_err(|_| AuthError::Unauthorized)?;
    let browser_hash =
        secrets::digest(TokenPurpose::Browser, browser).map_err(|_| AuthError::Unauthorized)?;
    let now = service.clock.now_unix();
    if now < 0 {
        return Err(AuthError::Unauthorized);
    }
    let tx = db::begin(&service.database).await?;
    quota::lock_guard(&tx).await?;
    let lock = if tx.get_database_backend() == DbBackend::Postgres {
        " FOR UPDATE"
    } else {
        ""
    };
    let row = db::one(
        &tx,
        &format!("SELECT browser_hash,canonical_email,intent,link_user_id,link_session_id,link_revision,expires_at,consumed_at FROM neev_magic_attempts WHERE token_hash=$1{lock}"),
        vec![token_hash.clone().into()],
    )
    .await?
    .ok_or(AuthError::Unauthorized)?;
    let stored_browser: String = row.try_get("", "browser_hash").map_err(database_error)?;
    let canonical: String = row.try_get("", "canonical_email").map_err(database_error)?;
    let intent_name: String = row.try_get("", "intent").map_err(database_error)?;
    let link_user_id: Option<i32> = row.try_get("", "link_user_id").map_err(database_error)?;
    let link_session_id: Option<String> =
        row.try_get("", "link_session_id").map_err(database_error)?;
    let link_revision: Option<i64> = row.try_get("", "link_revision").map_err(database_error)?;
    let expires_at: i64 = row.try_get("", "expires_at").map_err(database_error)?;
    let consumed_at: Option<i64> = row.try_get("", "consumed_at").map_err(database_error)?;
    if stored_browser != browser_hash || expires_at <= now || consumed_at.is_some() {
        return Err(AuthError::Unauthorized);
    }
    let intent = stored_intent(intent_name, link_user_id, link_session_id, link_revision)?;
    let updated = db::execute(
        &tx,
        "UPDATE neev_magic_attempts SET consumed_at=$1 WHERE token_hash=$2 AND browser_hash=$3 AND consumed_at IS NULL AND expires_at>$1",
        vec![now.into(), token_hash.into(), browser_hash.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Unauthorized);
    }
    tx.commit().await.map_err(database_error)?;
    let is_login = matches!(intent, SignInIntent::Login);
    let display_name = canonical
        .split_once('@')
        .map(|(local, _)| local.to_owned())
        .ok_or(AuthError::Unauthorized)?;
    let proof = crate::neev::types::VerifiedIdentity {
        provider: crate::neev::types::Provider::Email,
        issuer: "https://app.neev.so".to_owned(),
        subject: canonical.clone(),
        verified_email: Some(canonical),
        display_name,
        avatar_url: None,
    };
    let user_id = identity::resolve(&service.database, proof, intent, now).await?;
    if is_login {
        Ok(Completion::LoggedIn(
            session::issue_identity(&service.database, user_id, now).await?,
        ))
    } else {
        Ok(Completion::Linked)
    }
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

fn mail_failure_reason(error: AuthError) -> &'static str {
    match error {
        AuthError::Limited => "limited",
        _ => "provider",
    }
}
