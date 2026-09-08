use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use axum::http::HeaderMap;
use futures_util::future::{BoxFuture, FutureExt, Shared};
use sea_orm::{DatabaseConnection, DatabaseTransaction};
#[cfg(test)]
use tokio::sync::Semaphore;

use super::{
    core::{TeamAuthError, TeamClock},
    credentials::CredentialService,
    github::{GithubEvidenceError, GithubMembershipEvidence, GithubMembershipSource},
    session::{
        resolve_local_session, LocalPrincipal, LocalSessionError, MembershipFreshness, TeamRole,
    },
};

#[path = "principal_store.rs"]
mod principal_store;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProofRequirement {
    Cached,
    Fresh,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthContext {
    pub(crate) session_id: String,
    pub(crate) csrf_hash: String,
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) membership_id: i32,
    pub(crate) role: TeamRole,
    pub(crate) authorization_revision: i64,
    pub(crate) credential_generation: i64,
    pub(crate) verified_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Copy, Hash, Eq, PartialEq)]
struct ProofKey {
    github_user_id: i64,
    github_org_id: i64,
    membership_id: i32,
    authorization_revision: i64,
    credential_generation: i64,
}

type ProofFuture = Shared<BoxFuture<'static, Result<GithubMembershipEvidence, TeamAuthError>>>;

struct ProofFlight {
    future: ProofFuture,
}

pub(crate) struct PrincipalService {
    db: DatabaseConnection,
    credentials: Arc<CredentialService>,
    github: Arc<dyn GithubMembershipSource>,
    clock: Arc<dyn TeamClock>,
    flights: Mutex<HashMap<ProofKey, Weak<ProofFlight>>>,
    #[cfg(test)]
    proof_flight_observer: Option<Arc<Semaphore>>,
}

impl PrincipalService {
    pub(crate) fn new(
        db: DatabaseConnection,
        credentials: Arc<CredentialService>,
        github: Arc<dyn GithubMembershipSource>,
        clock: Arc<dyn TeamClock>,
    ) -> Self {
        Self {
            db,
            credentials,
            github,
            clock,
            flights: Mutex::new(HashMap::new()),
            #[cfg(test)]
            proof_flight_observer: None,
        }
    }

    #[cfg(test)]
    pub(super) fn observe_proof_flight(&mut self, observer: Arc<Semaphore>) {
        self.proof_flight_observer = Some(observer);
    }

    pub(crate) async fn resolve(
        &self,
        headers: &HeaderMap,
        requirement: ProofRequirement,
    ) -> Result<AuthContext, TeamAuthError> {
        let initial_local = self.resolve_local(headers).await?;
        let initial = principal_store::load_for_local(
            &self.db,
            &initial_local,
            principal_store::CredentialRequirement::ActiveOrRefreshing,
            self.clock.as_ref(),
        )
        .await?;

        let grant = self
            .credentials
            .access(initial.user_id, initial.github_user_id)
            .await?;

        let current_local = self.resolve_local(headers).await?;
        require_same_session_authority(&initial_local, &current_local)?;
        let current = principal_store::load_for_local(
            &self.db,
            &current_local,
            principal_store::CredentialRequirement::Active,
            self.clock.as_ref(),
        )
        .await?;
        if grant.user_id != current.user_id
            || grant.github_user_id != current.github_user_id
            || grant.generation != current.credential_generation
        {
            return Err(TeamAuthError::Unauthorized);
        }

        if requirement == ProofRequirement::Cached
            && current.freshness(self.clock.now()) == MembershipFreshness::Fresh
        {
            return current.into_context();
        }

        let key = current.proof_key();
        let flight = self.proof_flight(key, current.clone(), grant.access_token)?;
        #[cfg(test)]
        if let Some(observer) = &self.proof_flight_observer {
            observer.add_permits(1);
        }
        flight.future.clone().await?;

        let final_local = self.resolve_local(headers).await?;
        require_same_session_authority(&current_local, &final_local)?;
        let final_authority = principal_store::load_for_local(
            &self.db,
            &final_local,
            principal_store::CredentialRequirement::Active,
            self.clock.as_ref(),
        )
        .await?;
        if final_authority.proof_key() != key
            || final_authority.freshness(self.clock.now()) != MembershipFreshness::Fresh
        {
            return Err(TeamAuthError::Unauthorized);
        }
        final_authority.into_context()
    }

    pub(crate) async fn recheck_in_transaction(
        &self,
        transaction: &DatabaseTransaction,
        context: &AuthContext,
    ) -> Result<(), TeamAuthError> {
        principal_store::recheck_context(transaction, context, self.clock.as_ref()).await
    }

    async fn resolve_local(&self, headers: &HeaderMap) -> Result<LocalPrincipal, TeamAuthError> {
        resolve_local_session(&self.db, headers, self.clock.now())
            .await
            .map_err(|error| match error {
                LocalSessionError::Unauthorized => TeamAuthError::Unauthorized,
                LocalSessionError::Unavailable => TeamAuthError::Unavailable,
            })
    }

    fn proof_flight(
        &self,
        key: ProofKey,
        authority: principal_store::Authority,
        access_token: String,
    ) -> Result<Arc<ProofFlight>, TeamAuthError> {
        let mut flights = self.flights.lock().map_err(|_| TeamAuthError::Unavailable)?;
        flights.retain(|_, flight| flight.strong_count() > 0);
        if let Some(flight) = flights.get(&key).and_then(Weak::upgrade) {
            if flight.future.peek().is_none() {
                return Ok(flight);
            }
        }

        let db = self.db.clone();
        let credentials = self.credentials.clone();
        let github = self.github.clone();
        let clock = self.clock.clone();
        let future = async move {
            let proof_started_at = clock.now();
            let result = github
                .verify_membership(
                    &access_token,
                    authority.github_user_id,
                    authority.github_org_id,
                    &authority.org_login,
                )
                .await;
            match result {
                Ok(evidence) => {
                    principal_store::persist_evidence(
                        &db,
                        &authority,
                        &evidence,
                        proof_started_at,
                        clock.as_ref(),
                    )
                    .await?;
                    Ok(evidence)
                }
                Err(GithubEvidenceError::ReauthenticationRequired) => {
                    credentials
                        .invalidate_if_generation(
                            authority.user_id,
                            authority.github_user_id,
                            authority.credential_generation,
                        )
                        .await?;
                    Err(TeamAuthError::Unauthorized)
                }
                Err(GithubEvidenceError::NotMember) => {
                    principal_store::revoke_not_member(
                        &db,
                        &authority,
                        proof_started_at,
                        clock.as_ref(),
                    )
                    .await?;
                    Err(TeamAuthError::Unauthorized)
                }
                Err(GithubEvidenceError::Unavailable) => Err(TeamAuthError::Unavailable),
            }
        }
        .boxed()
        .shared();
        let flight = Arc::new(ProofFlight { future });
        flights.insert(key, Arc::downgrade(&flight));
        Ok(flight)
    }
}

fn require_same_session_authority(
    before: &LocalPrincipal,
    after: &LocalPrincipal,
) -> Result<(), TeamAuthError> {
    if before.session_id != after.session_id
        || before.user_id != after.user_id
        || before.org_id != after.org_id
        || before.membership_id != after.membership_id
        || before.role != after.role
        || before.authorization_revision != after.authorization_revision
    {
        return Err(TeamAuthError::Unauthorized);
    }
    Ok(())
}

impl principal_store::Authority {
    fn proof_key(&self) -> ProofKey {
        ProofKey {
            github_user_id: self.github_user_id,
            github_org_id: self.github_org_id,
            membership_id: self.membership_id,
            authorization_revision: self.authorization_revision,
            credential_generation: self.credential_generation,
        }
    }

    fn freshness(&self, now: chrono::DateTime<chrono::Utc>) -> MembershipFreshness {
        match self.verified_at {
            Some(verified_at)
                if verified_at <= now
                    && now.signed_duration_since(verified_at) < chrono::Duration::seconds(60) =>
            {
                MembershipFreshness::Fresh
            }
            _ => MembershipFreshness::ReverificationRequired,
        }
    }

    fn into_context(self) -> Result<AuthContext, TeamAuthError> {
        Ok(AuthContext {
            session_id: self.session_id,
            csrf_hash: self.csrf_hash,
            user_id: self.user_id,
            org_id: self.org_id,
            membership_id: self.membership_id,
            role: self.role,
            authorization_revision: self.authorization_revision,
            credential_generation: self.credential_generation,
            verified_at: self.verified_at.ok_or(TeamAuthError::Unauthorized)?,
        })
    }
}
