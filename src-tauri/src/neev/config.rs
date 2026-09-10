use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("invalid native application origin")]
    Origin,
    #[error("invalid native database URL")]
    Database,
    #[error("invalid native encryption key")]
    EncryptionKey,
    #[error("invalid native bind address")]
    Bind,
    #[error("invalid native static root")]
    StaticRoot,
    #[error("invalid native provider configuration")]
    Provider,
}

pub(crate) struct ProviderPair {
    pub(crate) id: String,
    pub(crate) secret: String,
}

pub(crate) struct MailProvider {
    pub(crate) api_key: String,
    pub(crate) from: String,
}

pub(crate) struct ProviderConfig {
    pub(crate) google: Option<ProviderPair>,
    pub(crate) github: Option<ProviderPair>,
    pub(crate) email: Option<MailProvider>,
}

pub(crate) struct Config {
    pub(crate) origin: Url,
    pub(crate) origin_header: String,
    pub(crate) google_callback: Url,
    pub(crate) github_callback: Url,
    pub(crate) database_url: String,
    pub(crate) encryption_key: [u8; 32],
    pub(crate) bind: SocketAddr,
    pub(crate) trusted_proxy_ips: Vec<IpAddr>,
    pub(crate) static_root: PathBuf,
    pub(crate) preview: bool,
    pub(crate) providers: ProviderConfig,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        const KEYS: [&str; 14] = [
            "NEEV_APP_ORIGIN",
            "NEEV_DATABASE_URL",
            "NEEV_ENC_KEY",
            "NEEV_BIND",
            "NEEV_TRUSTED_PROXY_IPS",
            "NEEV_STATIC_ROOT",
            "NEEV_ALLOW_LOOPBACK_PREVIEW",
            "NEEV_GOOGLE_CLIENT_ID",
            "NEEV_GOOGLE_CLIENT_SECRET",
            "NEEV_GITHUB_CLIENT_ID",
            "NEEV_GITHUB_CLIENT_SECRET",
            "NEEV_RESEND_API_KEY",
            "NEEV_MAIL_FROM",
            "NEEV_RUNTIME_ENABLED",
        ];
        let mut values = HashMap::new();
        for key in KEYS {
            if let Some(value) = std::env::var_os(key) {
                values.insert(
                    key.to_owned(),
                    value.into_string().map_err(|_| ConfigError::Provider)?,
                );
            }
        }
        Self::from_values(&values)
    }

    pub(crate) fn from_values(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        if values
            .get("NEEV_RUNTIME_ENABLED")
            .is_some_and(|value| value != "false")
        {
            return Err(ConfigError::Provider);
        }
        let preview = optional_bool(values.get("NEEV_ALLOW_LOOPBACK_PREVIEW"))?;
        let origin = required_url(values, "NEEV_APP_ORIGIN", ConfigError::Origin)?;
        if !valid_origin(&origin, preview) {
            return Err(ConfigError::Origin);
        }
        let origin_header = origin.as_str().trim_end_matches('/').to_owned();
        let google_callback = origin
            .join("auth/google/callback")
            .map_err(|_| ConfigError::Origin)?;
        let github_callback = origin
            .join("auth/github/callback")
            .map_err(|_| ConfigError::Origin)?;

        let database_url = required(values, "NEEV_DATABASE_URL", ConfigError::Database)?;
        let database = Url::parse(&database_url).map_err(|_| ConfigError::Database)?;
        if !matches!(database.scheme(), "postgres" | "postgresql")
            || database.path().trim_matches('/').is_empty()
            || database.fragment().is_some()
        {
            return Err(ConfigError::Database);
        }

        let encryption_key = decode_key(&required(
            values,
            "NEEV_ENC_KEY",
            ConfigError::EncryptionKey,
        )?)?;
        let bind: SocketAddr = required(values, "NEEV_BIND", ConfigError::Bind)?
            .parse()
            .map_err(|_| ConfigError::Bind)?;
        if preview && !bind.ip().is_loopback() {
            return Err(ConfigError::Bind);
        }
        let trusted_proxy_ips = trusted_proxy_ips(values.get("NEEV_TRUSTED_PROXY_IPS"))?;
        let static_root = PathBuf::from(required(
            values,
            "NEEV_STATIC_ROOT",
            ConfigError::StaticRoot,
        )?);
        if !static_root.is_absolute() || !static_root.is_dir() {
            return Err(ConfigError::StaticRoot);
        }

        let providers = ProviderConfig {
            google: pair(values, "NEEV_GOOGLE_CLIENT_ID", "NEEV_GOOGLE_CLIENT_SECRET")?,
            github: pair(values, "NEEV_GITHUB_CLIENT_ID", "NEEV_GITHUB_CLIENT_SECRET")?,
            email: mail(values)?,
        };
        Ok(Self {
            origin,
            origin_header,
            google_callback,
            github_callback,
            database_url,
            encryption_key,
            bind,
            trusted_proxy_ips,
            static_root,
            preview,
            providers,
        })
    }
}

fn trusted_proxy_ips(value: Option<&String>) -> Result<Vec<IpAddr>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(ConfigError::Provider);
    }
    let mut parsed = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        let address = normalize_ip(item.parse::<IpAddr>().map_err(|_| ConfigError::Provider)?);
        if parsed.contains(&address) {
            return Err(ConfigError::Provider);
        }
        parsed.push(address);
    }
    Ok(parsed)
}

pub(crate) fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

fn required(
    values: &HashMap<String, String>,
    key: &str,
    error: ConfigError,
) -> Result<String, ConfigError> {
    values
        .get(key)
        .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
        .cloned()
        .ok_or(error)
}

fn required_url(
    values: &HashMap<String, String>,
    key: &str,
    error: ConfigError,
) -> Result<Url, ConfigError> {
    Url::parse(&required(values, key, error)?).map_err(|_| error)
}

fn optional_bool(value: Option<&String>) -> Result<bool, ConfigError> {
    match value.map(String::as_str) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(ConfigError::Origin),
    }
}

fn valid_origin(origin: &Url, preview: bool) -> bool {
    let root = origin.path().is_empty() || origin.path() == "/";
    let clean = root
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.query().is_none()
        && origin.fragment().is_none();
    if !clean {
        return false;
    }
    if preview {
        origin.scheme() == "http"
            && origin.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            })
    } else {
        origin.scheme() == "https" && origin.host_str().is_some()
    }
}

fn decode_key(value: &str) -> Result<[u8; 32], ConfigError> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ConfigError::EncryptionKey);
    }
    let mut key = [0; 32];
    for (index, output) in key.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| ConfigError::EncryptionKey)?;
    }
    Ok(key)
}

fn pair(
    values: &HashMap<String, String>,
    id_key: &str,
    secret_key: &str,
) -> Result<Option<ProviderPair>, ConfigError> {
    match (values.get(id_key), values.get(secret_key)) {
        (None, None) => Ok(None),
        (Some(id), Some(secret))
            if !id.trim().is_empty()
                && !secret.trim().is_empty()
                && !id.chars().any(char::is_control)
                && !secret.chars().any(char::is_control) =>
        {
            Ok(Some(ProviderPair {
                id: id.clone(),
                secret: secret.clone(),
            }))
        }
        _ => Err(ConfigError::Provider),
    }
}

fn mail(values: &HashMap<String, String>) -> Result<Option<MailProvider>, ConfigError> {
    match (
        values.get("NEEV_RESEND_API_KEY"),
        values.get("NEEV_MAIL_FROM"),
    ) {
        (None, None) => Ok(None),
        (Some(api_key), Some(from))
            if !api_key.trim().is_empty()
                && !api_key.chars().any(char::is_control)
                && from == "verification@neev.so" =>
        {
            Ok(Some(MailProvider {
                api_key: api_key.clone(),
                from: from.clone(),
            }))
        }
        _ => Err(ConfigError::Provider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> HashMap<String, String> {
        HashMap::from([
            ("NEEV_APP_ORIGIN".into(), "https://app.neev.so".into()),
            (
                "NEEV_DATABASE_URL".into(),
                "postgresql://neev@db.internal/neev".into(),
            ),
            ("NEEV_ENC_KEY".into(), "01".repeat(32)),
            ("NEEV_BIND".into(), "127.0.0.1:3100".into()),
            (
                "NEEV_STATIC_ROOT".into(),
                std::env::current_dir().unwrap().display().to_string(),
            ),
        ])
    }

    #[test]
    fn derives_callbacks_and_accepts_only_explicit_loopback_preview() {
        let config = Config::from_values(&values()).unwrap();
        assert_eq!(config.origin_header, "https://app.neev.so");
        assert_eq!(config.origin.as_str(), "https://app.neev.so/");
        assert_eq!(config.encryption_key, [1; 32]);
        assert_eq!(
            config.google_callback.as_str(),
            "https://app.neev.so/auth/google/callback"
        );
        assert_eq!(
            config.github_callback.as_str(),
            "https://app.neev.so/auth/github/callback"
        );
        assert!(!config.preview);

        let mut preview = values();
        preview.insert("NEEV_APP_ORIGIN".into(), "http://127.0.0.1:3100".into());
        assert_eq!(
            Config::from_values(&preview).err(),
            Some(ConfigError::Origin)
        );
        preview.insert("NEEV_ALLOW_LOOPBACK_PREVIEW".into(), "true".into());
        assert!(Config::from_values(&preview).is_ok());

        for bind in ["0.0.0.0:3100", "192.168.1.25:3100", "[::]:3100"] {
            preview.insert("NEEV_BIND".into(), bind.into());
            assert_eq!(
                Config::from_values(&preview).err(),
                Some(ConfigError::Bind),
                "preview accepted non-loopback bind {bind}"
            );
        }
    }

    #[test]
    fn rejects_non_origin_urls_non_postgres_and_runtime_enablement() {
        for origin in [
            "https://user@app.neev.so",
            "https://app.neev.so/path",
            "https://app.neev.so?query=1",
            "https://app.neev.so#fragment",
        ] {
            let mut candidate = values();
            candidate.insert("NEEV_APP_ORIGIN".into(), origin.into());
            assert_eq!(
                Config::from_values(&candidate).err(),
                Some(ConfigError::Origin)
            );
        }
        let mut sqlite = values();
        sqlite.insert("NEEV_DATABASE_URL".into(), "sqlite::memory:".into());
        assert_eq!(
            Config::from_values(&sqlite).err(),
            Some(ConfigError::Database)
        );
        let mut legacy_only = values();
        legacy_only.remove("NEEV_DATABASE_URL");
        legacy_only.insert("DATABASE_URL".into(), "postgresql:///legacy".into());
        assert_eq!(
            Config::from_values(&legacy_only).err(),
            Some(ConfigError::Database)
        );
        let mut runtime = values();
        runtime.insert("NEEV_RUNTIME_ENABLED".into(), "true".into());
        assert!(Config::from_values(&runtime).is_err());
    }

    #[test]
    fn provider_groups_are_all_or_none_and_errors_do_not_echo_secrets() {
        let secret = "do-not-print-this-secret";
        let mut partial = values();
        partial.insert("NEEV_GOOGLE_CLIENT_SECRET".into(), secret.into());
        let error = Config::from_values(&partial).err().unwrap();
        assert_eq!(error, ConfigError::Provider);
        assert!(!format!("{error:?}").contains(secret));

        let mut complete = values();
        complete.insert("NEEV_GOOGLE_CLIENT_ID".into(), "google-id".into());
        complete.insert("NEEV_GOOGLE_CLIENT_SECRET".into(), secret.into());
        complete.insert("NEEV_RESEND_API_KEY".into(), "resend-key".into());
        complete.insert("NEEV_MAIL_FROM".into(), "verification@neev.so".into());
        let config = Config::from_values(&complete).unwrap();
        let google = config.providers.google.as_ref().unwrap();
        assert_eq!(google.id, "google-id");
        assert_eq!(google.secret, secret);
        assert!(config.providers.github.is_none());
        let email = config.providers.email.as_ref().unwrap();
        assert_eq!(email.api_key, "resend-key");
        assert_eq!(email.from, "verification@neev.so");
    }

    #[test]
    fn trusted_proxy_addresses_are_exact_ips_or_startup_fails() {
        let mut configured = values();
        configured.insert(
            "NEEV_TRUSTED_PROXY_IPS".into(),
            "127.0.0.1,2001:db8::1".into(),
        );
        let parsed = Config::from_values(&configured).unwrap();
        assert_eq!(
            parsed.trusted_proxy_ips,
            vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "2001:db8::1".parse::<IpAddr>().unwrap(),
            ]
        );

        for invalid in ["", "127.0.0.1,", "proxy.internal", "127.0.0.1,127.0.0.1"] {
            configured.insert("NEEV_TRUSTED_PROXY_IPS".into(), invalid.into());
            assert_eq!(
                Config::from_values(&configured).err(),
                Some(ConfigError::Provider),
                "accepted {invalid:?}"
            );
        }
    }
}
