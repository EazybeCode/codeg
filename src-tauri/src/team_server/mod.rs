pub(crate) mod config;
pub(crate) mod database;
mod cookies;
mod handlers;
mod limiter;
mod projection;
mod router;
mod startup;
mod state;
mod static_files;

pub use config::{
    select_server_mode, select_server_mode_from_os, ServerMode, TeamConfigError,
};
pub use startup::run_team_server;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod database_tests;

#[cfg(test)]
mod http_tests;

#[cfg(test)]
mod http_onboarding_tests;

#[cfg(test)]
mod static_files_tests;

#[cfg(test)]
mod test_server;

#[cfg(test)]
mod browser_fixture;
