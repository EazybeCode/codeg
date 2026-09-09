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
    #[allow(dead_code)]
    pub(super) credentials: Arc<CredentialService>,
    pub(super) principals: Arc<PrincipalService>,
    pub(super) clock: Arc<dyn TeamClock>,
    pub(super) limiter: AuthLimiter,
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
        Self {
            db,
            config,
            vault,
            oauth,
            onboarding,
            credentials,
            principals,
            clock,
            limiter: AuthLimiter::new(),
        }
    }
}
