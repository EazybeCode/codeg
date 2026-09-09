pub(crate) mod config;
pub(crate) mod database;

pub use config::{
    select_server_mode, select_server_mode_from_os, ServerMode, TeamConfigError,
};

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod database_tests;
