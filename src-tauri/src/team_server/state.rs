use std::sync::Arc;

use sea_orm::DatabaseConnection;

use super::{config::TeamConfig, limiter::AuthLimiter};
use crate::team_access::{
    core::TeamClock,
    credentials::CredentialService,
    oauth::GithubOAuthClient,
    onboarding::OnboardingService,
    principal::PrincipalService,
    secrets::SecretVault,
};

pub(crate) struct TeamState {
    pub(super) db: DatabaseConnection,
    pub(super) config: TeamConfig,
    #[allow(dead_code)]
    pub(super) vault: Arc<SecretVault>,
    pub(super) oauth: Arc<GithubOAuthClient>,
    pub(super) onboarding: Arc<OnboardingService>,
    pub(super) credentials: Arc<CredentialService>,
    pub(super) principals: Arc<PrincipalService>,
    pub(super) clock: Arc<dyn TeamClock>,
    pub(super) auth_limiter: AuthLimiter,
    pub(super) catalog_limiter: AuthLimiter,
    #[cfg(test)]
    pub(super) catalog_repository_origin: Option<String>,
}

impl TeamState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        db: DatabaseConnection,
        config: TeamConfig,
        vault: Arc<SecretVault>,
        oauth: Arc<GithubOAuthClient>,
        onboarding: Arc<OnboardingService>,
        credentials: Arc<CredentialService>,
        principals: Arc<PrincipalService>,
        clock: Arc<dyn TeamClock>,
    ) -> Self {
        let (auth_limiter, catalog_limiter) = AuthLimiter::pair();
        Self {
            db,
            config,
            vault,
            oauth,
            onboarding,
            credentials,
            principals,
            clock,
            auth_limiter,
            catalog_limiter,
            #[cfg(test)]
            catalog_repository_origin: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_catalog_repository_origin_for_test(mut self, origin: &str) -> Self {
        self.catalog_repository_origin = Some(origin.to_owned());
        self
    }
}
