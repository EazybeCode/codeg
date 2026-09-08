pub(crate) mod core;
pub(crate) mod credentials;
pub(crate) mod github;
mod github_http;
pub(crate) mod oauth;
pub(crate) mod onboarding;
pub(crate) mod principal;
pub(crate) mod session;
pub(crate) mod secrets;

#[cfg(test)]
mod credentials_tests;

#[cfg(test)]
mod github_tests;

#[cfg(test)]
mod oauth_tests;

#[cfg(test)]
mod onboarding_tests;

#[cfg(test)]
mod principal_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod secrets_tests;

#[cfg(test)]
mod test_http;

#[cfg(test)]
mod test_auth;
