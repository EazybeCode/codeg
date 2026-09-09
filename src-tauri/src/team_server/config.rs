use std::{
    collections::HashMap,
    ffi::OsString,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

use reqwest::Url;

use crate::team_access::{oauth::GithubOAuthClient, secrets::SecretVault};

const CONFIG_KEYS: [(&str, TeamConfigError); 11] = [
    ("WORKOS_APP_ORIGIN", TeamConfigError::Origin),
    ("GITHUB_CALLBACK_URL", TeamConfigError::Callback),
    ("GITHUB_CLIENT_ID", TeamConfigError::Github),
    ("GITHUB_CLIENT_SECRET", TeamConfigError::Github),
    ("WORKOS_ENC_KEY", TeamConfigError::EncryptionKey),
    ("DATABASE_URL", TeamConfigError::Database),
    ("CODEG_HOST", TeamConfigError::Bind),
    ("CODEG_PORT", TeamConfigError::Bind),
    ("CODEG_STATIC_DIR", TeamConfigError::StaticRoot),
    ("WORKOS_ALLOW_LOOPBACK_PREVIEW", TeamConfigError::Origin),
    ("WORKOS_ENABLE_CATALOG_RUNTIME", TeamConfigError::Runtime),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerMode {
    Operator,
    Team,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TeamConfigError {
    #[error("invalid or conflicting WORKOS_SERVER_MODE")]
    Mode,
    #[error("team mode cannot run supervisor or credential-helper flags")]
    Flag,
    #[error("invalid WORKOS_APP_ORIGIN or loopback-preview configuration")]
    Origin,
    #[error("GITHUB_CALLBACK_URL must match WORKOS_APP_ORIGIN/auth/github/callback")]
    Callback,
    #[error("invalid GITHUB_CLIENT_ID or GITHUB_CLIENT_SECRET")]
    Github,
    #[error("invalid WORKOS_ENC_KEY")]
    EncryptionKey,
    #[error("DATABASE_URL must identify a PostgreSQL database")]
    Database,
    #[error("invalid CODEG_HOST or CODEG_PORT")]
    Bind,
    #[error("CODEG_STATIC_DIR must be an existing absolute directory")]
    StaticRoot,
    #[error("WORKOS_ENABLE_CATALOG_RUNTIME must remain disabled")]
    Runtime,
}

pub fn select_server_mode(
    args: &[String],
    configured: Option<&str>,
) -> Result<ServerMode, TeamConfigError> {
    let team_flag = args.iter().any(|arg| arg == "--team");
    let configured = match configured {
        None => None,
        Some("operator") => Some(ServerMode::Operator),
        Some("team") => Some(ServerMode::Team),
        Some(_) => return Err(TeamConfigError::Mode),
    };
    if team_flag && configured == Some(ServerMode::Operator) {
        return Err(TeamConfigError::Mode);
    }
    let mode = if team_flag || configured == Some(ServerMode::Team) {
        ServerMode::Team
    } else {
        ServerMode::Operator
    };
    if mode == ServerMode::Team
        && args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--supervise" | "--credential-helper"))
    {
        return Err(TeamConfigError::Flag);
    }
    Ok(mode)
}

pub fn select_server_mode_from_os(
    args: &[String],
    configured: Option<&std::ffi::OsStr>,
) -> Result<ServerMode, TeamConfigError> {
    let configured = configured
        .map(|value| value.to_str().ok_or(TeamConfigError::Mode))
        .transpose()?;
    select_server_mode(args, configured)
}

pub(crate) struct TeamConfig {
    pub(crate) origin: Url,
    pub(crate) callback: Url,
    pub(crate) github_client_id: String,
    pub(crate) github_client_secret: String,
    pub(crate) encryption_key: String,
    pub(crate) database_url: String,
    pub(crate) bind: SocketAddr,
    pub(crate) static_root: PathBuf,
}

impl TeamConfig {
    pub(crate) fn from_values(
        values: &HashMap<String, String>,
    ) -> Result<Self, TeamConfigError> {
        let allow_loopback = optional_bool(
            values.get("WORKOS_ALLOW_LOOPBACK_PREVIEW"),
            TeamConfigError::Origin,
        )?;
        let origin = required_url(values, "WORKOS_APP_ORIGIN", TeamConfigError::Origin)?;
        if !valid_origin(&origin, allow_loopback) {
            return Err(TeamConfigError::Origin);
        }

        let callback = required_url(values, "GITHUB_CALLBACK_URL", TeamConfigError::Callback)?;
        let expected_callback = origin
            .join("auth/github/callback")
            .map_err(|_| TeamConfigError::Callback)?;
        if callback != expected_callback {
            return Err(TeamConfigError::Callback);
        }

        let github_client_id = required(values, "GITHUB_CLIENT_ID", TeamConfigError::Github)?;
        let github_client_secret =
            required(values, "GITHUB_CLIENT_SECRET", TeamConfigError::Github)?;
        GithubOAuthClient::new(
            github_client_id.clone(),
            github_client_secret.clone(),
            callback.clone(),
        )
        .map_err(|_| TeamConfigError::Github)?;

        let encryption_key = required(values, "WORKOS_ENC_KEY", TeamConfigError::EncryptionKey)?;
        SecretVault::from_hex_key(&encryption_key).map_err(|_| TeamConfigError::EncryptionKey)?;

        let database_url = required(values, "DATABASE_URL", TeamConfigError::Database)?;
        if !is_postgres_database_url(&database_url) {
            return Err(TeamConfigError::Database);
        }

        let host = match values.get("CODEG_HOST") {
            Some(value) => value.parse::<IpAddr>().map_err(|_| TeamConfigError::Bind)?,
            None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        };
        let port = match values.get("CODEG_PORT") {
            Some(value) => value.parse::<u16>().map_err(|_| TeamConfigError::Bind)?,
            None => 3080,
        };
        if port == 0 {
            return Err(TeamConfigError::Bind);
        }

        let static_root = PathBuf::from(required(
            values,
            "CODEG_STATIC_DIR",
            TeamConfigError::StaticRoot,
        )?);
        if !static_root.is_absolute() || !static_root.is_dir() {
            return Err(TeamConfigError::StaticRoot);
        }

        if optional_bool(
            values.get("WORKOS_ENABLE_CATALOG_RUNTIME"),
            TeamConfigError::Runtime,
        )? {
            return Err(TeamConfigError::Runtime);
        }

        Ok(Self {
            origin,
            callback,
            github_client_id,
            github_client_secret,
            encryption_key,
            database_url,
            bind: SocketAddr::new(host, port),
            static_root,
        })
    }

    pub(crate) fn from_environment() -> Result<Self, TeamConfigError> {
        let mut values = HashMap::new();
        for (key, _) in CONFIG_KEYS {
            if let Some(value) = std::env::var_os(key) {
                values.insert(key.to_owned(), value);
            }
        }
        from_os_values(&values)
    }
}

pub(super) fn from_os_values(
    os_values: &HashMap<String, OsString>,
) -> Result<TeamConfig, TeamConfigError> {
    let mut values = HashMap::new();
    for (key, error) in CONFIG_KEYS {
        if let Some(value) = os_values.get(key) {
            values.insert(key.to_owned(), unicode_value(value.clone(), error)?);
        }
    }
    TeamConfig::from_values(&values)
}

fn unicode_value(value: OsString, error: TeamConfigError) -> Result<String, TeamConfigError> {
    value.into_string().map_err(|_| error)
}

fn required(
    values: &HashMap<String, String>,
    key: &str,
    error: TeamConfigError,
) -> Result<String, TeamConfigError> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(error)
}

fn required_url(
    values: &HashMap<String, String>,
    key: &str,
    error: TeamConfigError,
) -> Result<Url, TeamConfigError> {
    let value = required(values, key, error)?;
    Url::parse(&value).map_err(|_| error)
}

fn optional_bool(
    value: Option<&String>,
    error: TeamConfigError,
) -> Result<bool, TeamConfigError> {
    match value.map(String::as_str) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(error),
    }
}

fn valid_origin(origin: &Url, allow_loopback: bool) -> bool {
    if !origin.has_host()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || origin.path() != "/"
        || origin.host_str().is_some_and(|host| host.contains('*'))
    {
        return false;
    }
    if origin.scheme() == "https" {
        return true;
    }
    origin.scheme() == "http" && allow_loopback && explicit_loopback(origin)
}

fn explicit_loopback(url: &Url) -> bool {
    match url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
    {
        Some(IpAddr::V4(address)) => address == Ipv4Addr::new(127, 0, 0, 1),
        Some(IpAddr::V6(address)) => address == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

pub(super) fn is_postgres_database_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "postgres" | "postgresql")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.path_segments().is_some_and(|mut segments| {
            matches!((segments.next(), segments.next()), (Some(name), None) if !name.is_empty())
        })
}
