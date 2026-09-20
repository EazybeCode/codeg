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
    #[error("invalid native document root")]
    DocumentRoot,
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
    pub(crate) document_root: PathBuf,
    pub(crate) preview: bool,
    pub(crate) providers: ProviderConfig,
    /// Integration setting (default false): when enabled, native authority
    /// revocations (logout, member removal/role change) append a deny-only
    /// gateway revocation event in the same transaction. Requires schema8.
    pub(crate) gateway_events: bool,
    /// Personal-agent execution runtime toggle (default false). When true,
    /// startup binds the sandboxed provider runtime; the config layer accepts
    /// only "true"/"false"/absent so agents can never be enabled by a typo.
    pub(crate) runtime_enabled: bool,
    /// Team-message push delivery (default ON). `NEEV_REALTIME_DISABLED=true` is
    /// the phase-2 rollback switch: the stream route is not mounted and the
    /// capability reports false, so clients fall back to polling. No data change.
    pub(crate) realtime_enabled: bool,
    #[cfg(test)]
    pub(crate) _document_root_guard: Option<tempfile::TempDir>,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        const KEYS: [&str; 17] = [
            "NEEV_APP_ORIGIN",
            "NEEV_DATABASE_URL",
            "NEEV_ENC_KEY",
            "NEEV_BIND",
            "NEEV_TRUSTED_PROXY_IPS",
            "NEEV_STATIC_ROOT",
            "NEEV_DOCUMENT_ROOT",
            "NEEV_ALLOW_LOOPBACK_PREVIEW",
            "NEEV_GOOGLE_CLIENT_ID",
            "NEEV_GOOGLE_CLIENT_SECRET",
            "NEEV_GITHUB_CLIENT_ID",
            "NEEV_GITHUB_CLIENT_SECRET",
            "NEEV_RESEND_API_KEY",
            "NEEV_MAIL_FROM",
            "NEEV_RUNTIME_ENABLED",
            "NEEV_GATEWAY_EVENTS_ENABLED",
            "NEEV_REALTIME_DISABLED",
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
        // Agent runtime enablement is fail-closed: absent or "false" => off;
        // "true" => on. When on, startup binds the SANDBOXED provider runtime
        // (per-tenant namespaces/uid/rlimits + provider-only egress via
        // `neev_agent_launch`), never a bare provider spawn. Any other value is
        // rejected so a typo can never silently enable an unsandboxed runtime.
        let runtime_enabled = match values.get("NEEV_RUNTIME_ENABLED").map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(ConfigError::Provider),
        };
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
        let document_root = PathBuf::from(required(
            values,
            "NEEV_DOCUMENT_ROOT",
            ConfigError::DocumentRoot,
        )?);
        if !document_root.is_absolute()
            || !document_root.is_dir()
            || has_symlink_component(&document_root)
        {
            return Err(ConfigError::DocumentRoot);
        }
        let canonical_static = static_root
            .canonicalize()
            .map_err(|_| ConfigError::StaticRoot)?;
        let canonical_documents = document_root
            .canonicalize()
            .map_err(|_| ConfigError::DocumentRoot)?;
        if canonical_static == canonical_documents
            || canonical_static.starts_with(&canonical_documents)
            || canonical_documents.starts_with(&canonical_static)
        {
            return Err(ConfigError::DocumentRoot);
        }

        let providers = ProviderConfig {
            google: pair(values, "NEEV_GOOGLE_CLIENT_ID", "NEEV_GOOGLE_CLIENT_SECRET")?,
            github: pair(values, "NEEV_GITHUB_CLIENT_ID", "NEEV_GITHUB_CLIENT_SECRET")?,
            email: mail(values)?,
        };
        let gateway_events = optional_bool(values.get("NEEV_GATEWAY_EVENTS_ENABLED"))
            .map_err(|_| ConfigError::Provider)?;
        // Push delivery is on unless explicitly disabled; a typo is rejected
        // rather than silently leaving the rollback switch half-applied.
        let realtime_enabled = !optional_bool(values.get("NEEV_REALTIME_DISABLED"))
            .map_err(|_| ConfigError::Provider)?;
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
            document_root,
            preview,
            providers,
            gateway_events,
            runtime_enabled,
            realtime_enabled,
            #[cfg(test)]
            _document_root_guard: None,
        })
    }
}

fn has_symlink_component(path: &std::path::Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::Normal(_) => {
                current.push(component.as_os_str());
                match std::fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => return true,
                    Ok(_) => {}
                    Err(_) => return true,
                }
            }
            _ => return true,
        }
    }
    false
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
            && match origin.host() {
                Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            }
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
    use std::ops::{Deref, DerefMut};
    #[cfg(unix)]
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    struct Values {
        entries: HashMap<String, String>,
        _document_root: tempfile::TempDir,
    }

    impl Deref for Values {
        type Target = HashMap<String, String>;

        fn deref(&self) -> &Self::Target {
            &self.entries
        }
    }

    impl DerefMut for Values {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.entries
        }
    }

    fn values() -> Values {
        let document_root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(document_root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let entries = HashMap::from([
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
            (
                "NEEV_DOCUMENT_ROOT".into(),
                document_root
                    .path()
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string(),
            ),
        ]);
        Values {
            entries,
            _document_root: document_root,
        }
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
    fn ipv6_preview_accepts_only_literal_loopback() {
        let mut candidate = values();
        candidate.insert("NEEV_ALLOW_LOOPBACK_PREVIEW".into(), "true".into());
        candidate.insert("NEEV_BIND".into(), "[::1]:3100".into());
        candidate.insert("NEEV_APP_ORIGIN".into(), "http://[::1]:3100".into());
        assert!(Config::from_values(&candidate).is_ok());
        for origin in [
            "http://[::]:3100",
            "http://[2001:db8::1]:3100",
            "http://example.test:3100",
            "http://[::1]:3100/path",
        ] {
            candidate.insert("NEEV_APP_ORIGIN".into(), origin.into());
            assert_eq!(
                Config::from_values(&candidate).err(),
                Some(ConfigError::Origin)
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
        // Runtime enablement is fail-closed: absent/"false" => off, "true" => on
        // (binds the sandboxed runtime in startup), any other value rejected so a
        // typo can never silently enable it.
        assert!(!Config::from_values(&values()).unwrap().runtime_enabled);
        let mut off = values();
        off.insert("NEEV_RUNTIME_ENABLED".into(), "false".into());
        assert!(!Config::from_values(&off).unwrap().runtime_enabled);
        let mut on = values();
        on.insert("NEEV_RUNTIME_ENABLED".into(), "true".into());
        assert!(Config::from_values(&on).unwrap().runtime_enabled);
        let mut typo = values();
        typo.insert("NEEV_RUNTIME_ENABLED".into(), "1".into());
        assert_eq!(Config::from_values(&typo).err(), Some(ConfigError::Provider));
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

    #[cfg(unix)]
    #[test]
    fn requires_a_dedicated_absolute_nonsymlink_document_root() {
        let mut missing = values();
        missing.remove("NEEV_DOCUMENT_ROOT");
        assert_eq!(
            Config::from_values(&missing).err(),
            Some(ConfigError::DocumentRoot)
        );

        missing.insert("NEEV_DOCUMENT_ROOT".into(), "documents".into());
        assert_eq!(
            Config::from_values(&missing).err(),
            Some(ConfigError::DocumentRoot)
        );

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let document_root = base.join("documents");
        fs::create_dir(&document_root).unwrap();
        let linked_root = base.join("linked-documents");
        symlink(&document_root, &linked_root).unwrap();
        missing.insert(
            "NEEV_DOCUMENT_ROOT".into(),
            linked_root.display().to_string(),
        );
        assert_eq!(
            Config::from_values(&missing).err(),
            Some(ConfigError::DocumentRoot)
        );

        let real_parent = base.join("real-parent");
        let child = real_parent.join("documents");
        fs::create_dir(&real_parent).unwrap();
        fs::create_dir(&child).unwrap();
        let linked_parent = base.join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        missing.insert(
            "NEEV_DOCUMENT_ROOT".into(),
            linked_parent.join("documents").display().to_string(),
        );
        assert_eq!(
            Config::from_values(&missing).err(),
            Some(ConfigError::DocumentRoot)
        );

        let static_root = base.join("static");
        fs::create_dir(&static_root).unwrap();
        missing.insert("NEEV_STATIC_ROOT".into(), static_root.display().to_string());
        for root in [&static_root, &base] {
            missing.insert("NEEV_DOCUMENT_ROOT".into(), root.display().to_string());
            assert_eq!(
                Config::from_values(&missing).err(),
                Some(ConfigError::DocumentRoot),
                "accepted overlapping document root {root:?}"
            );
        }
        let nested = static_root.join("documents");
        fs::create_dir(&nested).unwrap();
        missing.insert("NEEV_DOCUMENT_ROOT".into(), nested.display().to_string());
        assert_eq!(
            Config::from_values(&missing).err(),
            Some(ConfigError::DocumentRoot)
        );

        missing.insert(
            "NEEV_DOCUMENT_ROOT".into(),
            document_root.display().to_string(),
        );
        assert!(Config::from_values(&missing).is_ok());
    }

    #[test]
    fn gateway_events_flag_defaults_false_and_parses_explicit_bool() {
        assert!(!Config::from_values(&values()).unwrap().gateway_events);
        let mut enabled = values();
        enabled.insert("NEEV_GATEWAY_EVENTS_ENABLED".into(), "true".into());
        assert!(Config::from_values(&enabled).unwrap().gateway_events);
        let mut invalid = values();
        invalid.insert("NEEV_GATEWAY_EVENTS_ENABLED".into(), "maybe".into());
        assert_eq!(
            Config::from_values(&invalid).err(),
            Some(ConfigError::Provider)
        );
    }

    #[test]
    fn realtime_defaults_on_and_only_an_explicit_disable_rolls_it_back() {
        assert!(Config::from_values(&values()).unwrap().realtime_enabled);
        let mut kept = values();
        kept.insert("NEEV_REALTIME_DISABLED".into(), "false".into());
        assert!(Config::from_values(&kept).unwrap().realtime_enabled);
        let mut disabled = values();
        disabled.insert("NEEV_REALTIME_DISABLED".into(), "true".into());
        assert!(!Config::from_values(&disabled).unwrap().realtime_enabled);
        let mut invalid = values();
        invalid.insert("NEEV_REALTIME_DISABLED".into(), "yes".into());
        assert_eq!(
            Config::from_values(&invalid).err(),
            Some(ConfigError::Provider)
        );
    }

    #[test]
    fn config_value_fixtures_own_distinct_document_roots() {
        let first = values();
        let first_root = PathBuf::from(first.get("NEEV_DOCUMENT_ROOT").unwrap());
        let second = values();
        let second_root = PathBuf::from(second.get("NEEV_DOCUMENT_ROOT").unwrap());
        assert_ne!(first_root, second_root);
        assert!(first_root.is_dir());
        assert!(second_root.is_dir());
        drop(first);
        assert!(!first_root.exists());
        assert!(second_root.is_dir());
        drop(second);
        assert!(!second_root.exists());
    }
}
