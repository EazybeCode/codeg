pub(crate) mod github;
mod github_http;
pub(crate) mod oauth;
pub(crate) mod session;
pub(crate) mod secrets;

#[cfg(test)]
mod github_tests;

#[cfg(test)]
mod oauth_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod secrets_tests;

#[cfg(test)]
mod test_http;
