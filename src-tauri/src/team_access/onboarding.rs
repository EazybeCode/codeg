use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::{
    core::{lock_user_identity, TeamAuthError, TeamClock},
    credentials::{CredentialService, StoredTokens},
    github::{
        GithubEvidenceError, GithubIdentity, GithubMembershipSource, GithubOrganizationChoice,
    },
    oauth::{GithubOAuthSource, GithubTokenSet, OAuthError},
    secrets::{hash_browser_secret, random_browser_secret, SecretBinding, SecretVault},
    session::TeamRole,
};

#[path = "onboarding_store.rs"]
mod onboarding_store;

const LIFETIME_MINUTES: i64 = 10;
const SESSION_LIFETIME_DAYS: i64 = 30;
const PROOF_LIFETIME_SECONDS: i64 = 60;
const PENDING_PAYLOAD_VERSION: i64 = 1;
const MAX_NAME_CHARS: usize = 256;
const MAX_AVATAR_BYTES: usize = 2_048;

pub(crate) struct OAuthAttemptStart {
    pub(crate) state: String,
    pub(crate) browser_secret: String,
    pub(crate) verifier: String,
    pub(crate) expires_at: DateTime<Utc>,
}

pub(crate) struct PendingStart {
    pub(crate) browser_secret: String,
    pub(crate) csrf_secret: String,
    pub(crate) expires_at: DateTime<Utc>,
}

pub(crate) struct SessionStart {
    pub(crate) session_secret: String,
    pub(crate) csrf_secret: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
}

pub(crate) struct OnboardingChoices {
    pub(crate) identity: GithubIdentity,
    pub(crate) organizations: Vec<GithubOrganizationChoice>,
}

#[derive(Serialize, Deserialize)]
struct PendingPayload {
    version: i64,
    github_user_id: i64,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
    tokens: StoredTokens,
}

impl PendingPayload {
    fn new(identity: GithubIdentity, tokens: &GithubTokenSet) -> Result<Self, TeamAuthError> {
        validate_identity(&identity)?;
        Ok(Self {
            version: PENDING_PAYLOAD_VERSION,
            github_user_id: identity.github_user_id,
            login: identity.login,
            name: identity.name,
            avatar_url: bounded_avatar(identity.avatar_url),
            tokens: StoredTokens::from_tokens(tokens)?,
        })
    }

    fn validate(&self, expected_github_user_id: i64) -> Result<(), TeamAuthError> {
        if self.version != PENDING_PAYLOAD_VERSION
            || self.github_user_id != expected_github_user_id
        {
            return Err(TeamAuthError::Unavailable);
        }
        validate_identity(&self.identity())?;
        self.tokens.to_tokens()?;
        Ok(())
    }

    fn identity(&self) -> GithubIdentity {
        GithubIdentity {
            github_user_id: self.github_user_id,
            login: self.login.clone(),
            name: self.name.clone(),
            avatar_url: bounded_avatar(self.avatar_url.clone()),
        }
    }

    fn tokens(&self, now: DateTime<Utc>) -> Result<GithubTokenSet, TeamAuthError> {
        let tokens = self.tokens.to_tokens()?;
        validate_token_time(&tokens, now)?;
        Ok(tokens)
    }
}

pub(crate) struct OnboardingService {
    db: DatabaseConnection,
    vault: Arc<SecretVault>,
    oauth: Arc<dyn GithubOAuthSource>,
    github: Arc<dyn GithubMembershipSource>,
    credentials: Arc<CredentialService>,
    clock: Arc<dyn TeamClock>,
}

impl OnboardingService {
    pub(crate) fn new(
        db: DatabaseConnection,
        vault: Arc<SecretVault>,
        oauth: Arc<dyn GithubOAuthSource>,
        github: Arc<dyn GithubMembershipSource>,
        credentials: Arc<CredentialService>,
        clock: Arc<dyn TeamClock>,
    ) -> Self {
        Self {
            db,
            vault,
            oauth,
            github,
            credentials,
            clock,
        }
    }

    pub(crate) async fn begin(&self) -> Result<OAuthAttemptStart, TeamAuthError> {
        let state = random_secret()?;
        let browser_secret = random_secret()?;
        let verifier = random_secret()?;
        let state_hash = secret_hash(&state)?;
        let browser_hash = secret_hash(&browser_secret)?;
        let created_at = self.clock.now();
        let expires_at = checked_add(created_at, Duration::minutes(LIFETIME_MINUTES))?;
        let encrypted_verifier = self
            .vault
            .seal(
                SecretBinding::OAuthAttempt {
                    state_hash: &state_hash,
                    browser_hash: &browser_hash,
                },
                verifier.as_bytes(),
            )
            .map_err(|_| TeamAuthError::Unavailable)?;
        onboarding_store::insert_attempt(
            &self.db,
            state_hash,
            browser_hash,
            encrypted_verifier,
            created_at,
            expires_at,
        )
        .await?;
        Ok(OAuthAttemptStart {
            state,
            browser_secret,
            verifier,
            expires_at,
        })
    }

    pub(crate) async fn complete(
        &self,
        state: &str,
        browser_secret: &str,
        code: &str,
    ) -> Result<PendingStart, TeamAuthError> {
        let state_hash = authenticated_hash(state)?;
        let browser_hash = authenticated_hash(browser_secret)?;
        let now = self.clock.now();
        let attempt = onboarding_store::live_attempt(&self.db, &state_hash, &browser_hash, now)
            .await?
            .ok_or(TeamAuthError::Unauthorized)?;
        let verifier = self
            .vault
            .open(
                SecretBinding::OAuthAttempt {
                    state_hash: &attempt.state_hash,
                    browser_hash: &attempt.browser_hash,
                },
                &attempt.encrypted_verifier,
            )
            .map_err(|_| TeamAuthError::Unavailable)?;
        let verifier = String::from_utf8(verifier).map_err(|_| TeamAuthError::Unavailable)?;
        if !onboarding_store::consume_attempt(&self.db, &attempt, now).await? {
            return Err(TeamAuthError::Unauthorized);
        }
        let after_consume = self.clock.now();
        if attempt.created_at > after_consume || after_consume >= attempt.expires_at {
            return Err(TeamAuthError::Unauthorized);
        }
        let tokens = self
            .oauth
            .exchange_code(code, &verifier, after_consume)
            .await
            .map_err(oauth_error)?;
        let identity = self
            .github
            .identity(&tokens.access_token)
            .await
            .map_err(github_error)?;
        let payload = PendingPayload::new(identity, &tokens)?;
        let created_at = self.clock.now();
        validate_token_time(&tokens, created_at)?;
        let expires_at = checked_add(created_at, Duration::minutes(LIFETIME_MINUTES))?;
        let pending_secret = random_secret()?;
        let csrf_secret = random_secret()?;
        let token_hash = secret_hash(&pending_secret)?;
        let csrf_hash = secret_hash(&csrf_secret)?;
        let plaintext = serde_json::to_vec(&payload).map_err(|_| TeamAuthError::Unavailable)?;
        let encrypted_payload = self
            .vault
            .seal(
                SecretBinding::PendingOnboarding {
                    token_hash: &token_hash,
                    github_user_id: payload.github_user_id,
                },
                &plaintext,
            )
            .map_err(|_| TeamAuthError::Unavailable)?;
        onboarding_store::insert_pending(
            &self.db,
            token_hash,
            csrf_hash,
            payload.github_user_id,
            encrypted_payload,
            created_at,
            expires_at,
        )
        .await?;
        Ok(PendingStart {
            browser_secret: pending_secret,
            csrf_secret,
            expires_at,
        })
    }

    pub(crate) async fn choices(
        &self,
        browser_secret: &str,
    ) -> Result<OnboardingChoices, TeamAuthError> {
        let now = self.clock.now();
        let (_, payload) = self.load_pending(browser_secret, now).await?;
        let tokens = payload.tokens(now)?;
        let organizations = self
            .github
            .active_organizations(&tokens.access_token, payload.github_user_id)
            .await
            .map_err(github_error)?;
        let organizations = validated_organizations(organizations)?;
        Ok(OnboardingChoices {
            identity: payload.identity(),
            organizations,
        })
    }

    pub(crate) async fn select(
        &self,
        browser_secret: &str,
        csrf_secret: &str,
        github_org_id: i64,
        previous_session_secret: Option<&str>,
    ) -> Result<SessionStart, TeamAuthError> {
        let token_hash = authenticated_hash(browser_secret)?;
        let csrf_hash = authenticated_hash(csrf_secret)?;
        let previous_session_hash = previous_session_secret
            .map(authenticated_hash)
            .transpose()?;
        if github_org_id <= 0 {
            return Err(TeamAuthError::Forbidden);
        }
        let now = self.clock.now();
        let pending = onboarding_store::live_pending(&self.db, &token_hash, now)
            .await?
            .ok_or(TeamAuthError::Unauthorized)?;
        if pending.csrf_hash != csrf_hash {
            return Err(TeamAuthError::Unauthorized);
        }
        let payload = self.open_pending(&pending)?;
        let tokens = payload.tokens(now)?;
        let organizations = self
            .github
            .active_organizations(&tokens.access_token, payload.github_user_id)
            .await
            .map_err(github_error)?;
        let selected = validated_organizations(organizations)?
            .into_iter()
            .find(|organization| organization.github_org_id == github_org_id)
            .ok_or(TeamAuthError::Forbidden)?;
        let proof_at = self.clock.now();
        let evidence = self
            .github
            .verify_membership(
                &tokens.access_token,
                payload.github_user_id,
                selected.github_org_id,
                &selected.login,
            )
            .await
            .map_err(github_error)?;
        validate_evidence(&evidence, &selected, payload.github_user_id)?;

        let transaction = self
            .db
            .begin()
            .await
            .map_err(|_| TeamAuthError::Unavailable)?;
        let result = self
            .select_in_transaction(
                &transaction,
                &pending,
                &payload,
                &tokens,
                &selected,
                proof_at,
                previous_session_hash.as_deref(),
            )
            .await;
        match result {
            Ok(session) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| onboarding_store::selection_database_error(&error))?;
                Ok(session)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| TeamAuthError::Unavailable)?;
                Err(error)
            }
        }
    }

    pub(crate) async fn cleanup_expired(&self) -> Result<(), TeamAuthError> {
        onboarding_store::cleanup_expired(&self.db, self.clock.now()).await
    }

    async fn load_pending(
        &self,
        browser_secret: &str,
        now: DateTime<Utc>,
    ) -> Result<
        (
            crate::db::entities::team_pending_onboarding::Model,
            PendingPayload,
        ),
        TeamAuthError,
    > {
        let token_hash = authenticated_hash(browser_secret)?;
        let pending = onboarding_store::live_pending(&self.db, &token_hash, now)
            .await?
            .ok_or(TeamAuthError::Unauthorized)?;
        let payload = self.open_pending(&pending)?;
        Ok((pending, payload))
    }

    fn open_pending(
        &self,
        pending: &crate::db::entities::team_pending_onboarding::Model,
    ) -> Result<PendingPayload, TeamAuthError> {
        let plaintext = self
            .vault
            .open(
                SecretBinding::PendingOnboarding {
                    token_hash: &pending.token_hash,
                    github_user_id: pending.github_user_id,
                },
                &pending.encrypted_payload,
            )
            .map_err(|_| TeamAuthError::Unavailable)?;
        let payload: PendingPayload =
            serde_json::from_slice(&plaintext).map_err(|_| TeamAuthError::Unavailable)?;
        payload.validate(pending.github_user_id)?;
        Ok(payload)
    }

    #[allow(clippy::too_many_arguments)]
    async fn select_in_transaction(
        &self,
        transaction: &DatabaseTransaction,
        pending: &crate::db::entities::team_pending_onboarding::Model,
        payload: &PendingPayload,
        tokens: &GithubTokenSet,
        selected: &GithubOrganizationChoice,
        proof_at: DateTime<Utc>,
        previous_session_hash: Option<&str>,
    ) -> Result<SessionStart, TeamAuthError> {
        let transaction_now = self.clock.now();
        validate_pending_and_proof(pending, proof_at, transaction_now)?;
        if !onboarding_store::consume_pending(transaction, pending, transaction_now).await? {
            return Err(TeamAuthError::Conflict);
        }

        let existing_user = onboarding_store::find_user(transaction, payload.github_user_id).await?;
        let (account, organization) = if let Some(existing_user) = existing_user {
            lock_user_identity(transaction, existing_user.id, payload.github_user_id).await?;
            let account = onboarding_store::update_user(
                transaction,
                existing_user,
                payload.login.clone(),
                payload.name.clone(),
                bounded_avatar(payload.avatar_url.clone()),
                transaction_now,
            )
            .await?;
            let organization = onboarding_store::resolve_org(
                transaction,
                selected.github_org_id,
                selected.login.clone(),
                bounded_avatar(selected.avatar_url.clone()),
                transaction_now,
            )
            .await?;
            (account, organization)
        } else {
            let organization = onboarding_store::resolve_org(
                transaction,
                selected.github_org_id,
                selected.login.clone(),
                bounded_avatar(selected.avatar_url.clone()),
                transaction_now,
            )
            .await?;
            let account = onboarding_store::insert_user(
                transaction,
                payload.github_user_id,
                payload.login.clone(),
                payload.name.clone(),
                bounded_avatar(payload.avatar_url.clone()),
                organization.id,
                transaction_now,
            )
            .await?;
            (account, organization)
        };

        let role = role_name(selected.role);
        let membership = match onboarding_store::find_membership(
            transaction,
            organization.id,
            account.id,
        )
        .await?
        {
            None => {
                onboarding_store::insert_membership(
                    transaction,
                    organization.id,
                    account.id,
                    role,
                    transaction_now,
                )
                .await?
            }
            Some(existing) if existing.state == "blocked" => return Err(TeamAuthError::Forbidden),
            Some(existing) => {
                let revision = match existing.state.as_str() {
                    "revoked" => existing
                        .authorization_revision
                        .checked_add(1)
                        .ok_or(TeamAuthError::Unavailable)?,
                    "active" if existing.role != role => existing
                        .authorization_revision
                        .checked_add(1)
                        .ok_or(TeamAuthError::Unavailable)?,
                    "active" => existing.authorization_revision,
                    _ => return Err(TeamAuthError::Unavailable),
                };
                onboarding_store::update_membership_authority(
                    transaction,
                    existing,
                    role,
                    "active",
                    revision,
                    transaction_now,
                )
                .await?
            }
        };
        let generation = self
            .credentials
            .install_in_transaction(
                transaction,
                account.id,
                payload.github_user_id,
                tokens,
                transaction_now,
            )
            .await?;
        let membership = onboarding_store::bind_membership_proof(
            transaction,
            &membership,
            generation,
            proof_at,
            transaction_now,
        )
        .await?;

        let issued_at = self.clock.now();
        validate_pending_and_proof(pending, proof_at, issued_at)?;
        validate_token_time(tokens, issued_at)?;
        let expires_at = checked_add(issued_at, Duration::days(SESSION_LIFETIME_DAYS))?;
        let session_secret = random_secret()?;
        let csrf_secret = random_secret()?;
        onboarding_store::delete_previous_session(transaction, previous_session_hash).await?;
        onboarding_store::insert_session(
            transaction,
            secret_hash(&session_secret)?,
            secret_hash(&csrf_secret)?,
            account.id,
            organization.id,
            membership.id,
            membership.authorization_revision,
            generation,
            issued_at,
            expires_at,
        )
        .await?;
        Ok(SessionStart {
            session_secret,
            csrf_secret,
            expires_at,
            user_id: account.id,
            org_id: organization.id,
        })
    }
}

fn random_secret() -> Result<String, TeamAuthError> {
    random_browser_secret().map_err(|_| TeamAuthError::Unavailable)
}

fn secret_hash(secret: &str) -> Result<String, TeamAuthError> {
    hash_browser_secret(secret).map_err(|_| TeamAuthError::Unavailable)
}

fn authenticated_hash(secret: &str) -> Result<String, TeamAuthError> {
    hash_browser_secret(secret).map_err(|_| TeamAuthError::Unauthorized)
}

fn checked_add(at: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, TeamAuthError> {
    at.checked_add_signed(duration)
        .ok_or(TeamAuthError::Unavailable)
}

fn oauth_error(error: OAuthError) -> TeamAuthError {
    match error {
        OAuthError::ReauthenticationRequired => TeamAuthError::Unauthorized,
        OAuthError::Unavailable => TeamAuthError::Unavailable,
    }
}

fn github_error(error: GithubEvidenceError) -> TeamAuthError {
    match error {
        GithubEvidenceError::ReauthenticationRequired => TeamAuthError::Unauthorized,
        GithubEvidenceError::NotMember => TeamAuthError::Forbidden,
        GithubEvidenceError::Unavailable => TeamAuthError::Unavailable,
    }
}

fn validate_identity(identity: &GithubIdentity) -> Result<(), TeamAuthError> {
    if identity.github_user_id <= 0
        || !super::github::valid_login(&identity.login)
        || identity.name.as_ref().is_some_and(|name| {
            name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control)
        })
    {
        return Err(TeamAuthError::Unavailable);
    }
    Ok(())
}

fn bounded_avatar(avatar_url: Option<String>) -> Option<String> {
    avatar_url.filter(|value| value.len() <= MAX_AVATAR_BYTES)
}

fn validated_organizations(
    organizations: Vec<GithubOrganizationChoice>,
) -> Result<Vec<GithubOrganizationChoice>, TeamAuthError> {
    let mut seen = HashSet::new();
    organizations
        .into_iter()
        .map(|mut organization| {
            if organization.github_org_id <= 0
                || !super::github::valid_login(&organization.login)
                || !seen.insert(organization.github_org_id)
            {
                return Err(TeamAuthError::Unavailable);
            }
            organization.avatar_url = bounded_avatar(organization.avatar_url);
            Ok(organization)
        })
        .collect()
}

fn validate_evidence(
    evidence: &super::github::GithubMembershipEvidence,
    selected: &GithubOrganizationChoice,
    github_user_id: i64,
) -> Result<(), TeamAuthError> {
    if evidence.github_user_id != github_user_id
        || evidence.organization.github_org_id != selected.github_org_id
        || !evidence
            .organization
            .login
            .eq_ignore_ascii_case(&selected.login)
        || evidence.organization.role != selected.role
    {
        return Err(TeamAuthError::Forbidden);
    }
    Ok(())
}

fn validate_pending_and_proof(
    pending: &crate::db::entities::team_pending_onboarding::Model,
    proof_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), TeamAuthError> {
    if pending.created_at > now
        || now >= pending.expires_at
        || proof_at > now
        || now.signed_duration_since(proof_at) >= Duration::seconds(PROOF_LIFETIME_SECONDS)
    {
        return Err(TeamAuthError::Unavailable);
    }
    Ok(())
}

fn validate_token_time(tokens: &GithubTokenSet, now: DateTime<Utc>) -> Result<(), TeamAuthError> {
    StoredTokens::from_tokens(tokens)?;
    if tokens
        .access_expires_at
        .is_some_and(|expires_at| expires_at <= now)
        || tokens
            .refresh_expires_at
            .is_some_and(|expires_at| expires_at <= now)
    {
        return Err(TeamAuthError::Unauthorized);
    }
    Ok(())
}

fn role_name(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Admin => "admin",
        TeamRole::Member => "member",
    }
}
