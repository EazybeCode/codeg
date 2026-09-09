use std::{collections::HashMap, path::Path};

use super::config::{
    from_os_values, select_server_mode, select_server_mode_from_os, ServerMode, TeamConfig,
    TeamConfigError,
};

const KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn valid_https_values(static_root: &Path) -> HashMap<String, String> {
    HashMap::from([
        ("WORKOS_APP_ORIGIN".into(), "https://workos.example.com".into()),
        (
            "GITHUB_CALLBACK_URL".into(),
            "https://workos.example.com/auth/github/callback".into(),
        ),
        ("GITHUB_CLIENT_ID".into(), "github-client-id".into()),
        ("GITHUB_CLIENT_SECRET".into(), "github-client-secret".into()),
        ("WORKOS_ENC_KEY".into(), KEY.into()),
        (
            "DATABASE_URL".into(),
            "postgres://workos@db.example.com/workos".into(),
        ),
        ("CODEG_HOST".into(), "10.0.0.8".into()),
        ("CODEG_PORT".into(), "8443".into()),
        (
            "CODEG_STATIC_DIR".into(),
            static_root.to_string_lossy().into_owned(),
        ),
        ("WORKOS_ALLOW_LOOPBACK_PREVIEW".into(), "false".into()),
        ("WORKOS_ENABLE_CATALOG_RUNTIME".into(), "false".into()),
    ])
}

fn valid_loopback_values(static_root: &Path) -> HashMap<String, String> {
    let mut values = valid_https_values(static_root);
    values.insert("WORKOS_APP_ORIGIN".into(), "http://127.0.0.1:3080".into());
    values.insert(
        "GITHUB_CALLBACK_URL".into(),
        "http://127.0.0.1:3080/auth/github/callback".into(),
    );
    values.insert("WORKOS_ALLOW_LOOPBACK_PREVIEW".into(), "true".into());
    values
}

#[test]
fn server_mode_defaults_to_compatible_operator_mode() {
    assert_eq!(
        select_server_mode(&args(&["codeg-server"]), None),
        Ok(ServerMode::Operator)
    );
    assert_eq!(
        select_server_mode(&args(&["codeg-server"]), Some("operator")),
        Ok(ServerMode::Operator)
    );
}

#[test]
fn server_mode_accepts_each_explicit_team_selection() {
    assert_eq!(
        select_server_mode(&args(&["codeg-server", "--team"]), None),
        Ok(ServerMode::Team)
    );
    assert_eq!(
        select_server_mode(&args(&["codeg-server"]), Some("team")),
        Ok(ServerMode::Team)
    );
    assert_eq!(
        select_server_mode(&args(&["codeg-server", "--team"]), Some("team")),
        Ok(ServerMode::Team)
    );
}

#[test]
fn server_mode_rejects_conflicts_and_invalid_configured_values() {
    assert_eq!(
        select_server_mode(
            &args(&["codeg-server", "--team"]),
            Some("operator")
        ),
        Err(TeamConfigError::Mode)
    );
    for configured in [Some(""), Some("TEAM"), Some("unexpected")] {
        assert_eq!(
            select_server_mode(&args(&["codeg-server"]), configured),
            Err(TeamConfigError::Mode)
        );
    }
}

#[test]
fn os_server_mode_selector_preserves_absent_and_valid_values() {
    assert_eq!(
        select_server_mode_from_os(&args(&["codeg-server"]), None),
        Ok(ServerMode::Operator)
    );
    assert_eq!(
        select_server_mode_from_os(
            &args(&["codeg-server", "--team"]),
            Some(std::ffi::OsStr::new("team")),
        ),
        Ok(ServerMode::Team)
    );
    assert_eq!(
        select_server_mode_from_os(
            &args(&["codeg-server"]),
            Some(std::ffi::OsStr::new("operator")),
        ),
        Ok(ServerMode::Operator)
    );
}

#[cfg(unix)]
#[test]
fn os_server_mode_selector_rejects_configured_non_unicode_value() {
    use std::os::unix::ffi::OsStrExt;

    let configured = std::ffi::OsStr::from_bytes(b"team\xff");
    assert_eq!(
        select_server_mode_from_os(&args(&["codeg-server"]), Some(configured)),
        Err(TeamConfigError::Mode)
    );
}

#[cfg(unix)]
#[test]
fn configured_non_unicode_values_fail_their_static_field_group() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let static_root = tempfile::tempdir().unwrap();
    let valid: HashMap<String, OsString> = valid_https_values(static_root.path())
        .into_iter()
        .map(|(key, value)| (key, value.into()))
        .collect();
    for (key, expected) in [
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
    ] {
        let mut values = valid.clone();
        values.insert(key.into(), OsString::from_vec(b"invalid\xff".to_vec()));
        assert_eq!(from_os_values(&values).err(), Some(expected), "key {key}");
    }
}

#[test]
fn team_mode_rejects_legacy_startup_flags() {
    for flag in ["--supervise", "--credential-helper"] {
        assert_eq!(
            select_server_mode(&args(&["codeg-server", "--team", flag]), None),
            Err(TeamConfigError::Flag)
        );
        assert_eq!(
            select_server_mode(&args(&["codeg-server", flag]), Some("team")),
            Err(TeamConfigError::Flag)
        );
    }
}

#[test]
fn complete_https_and_explicit_loopback_configuration_are_accepted() {
    let static_root = tempfile::tempdir().unwrap();

    let https = TeamConfig::from_values(&valid_https_values(static_root.path())).unwrap();
    assert_eq!(https.origin.as_str(), "https://workos.example.com/");
    assert_eq!(
        https.callback.as_str(),
        "https://workos.example.com/auth/github/callback"
    );
    assert_eq!(https.github_client_id, "github-client-id");
    assert_eq!(https.github_client_secret, "github-client-secret");
    assert_eq!(https.encryption_key, KEY);
    assert_eq!(
        https.database_url,
        "postgres://workos@db.example.com/workos"
    );
    assert_eq!(https.bind.to_string(), "10.0.0.8:8443");
    assert_eq!(https.static_root, static_root.path());

    let loopback = TeamConfig::from_values(&valid_loopback_values(static_root.path())).unwrap();
    assert_eq!(loopback.origin.as_str(), "http://127.0.0.1:3080/");
}

#[test]
fn origin_rejects_public_http_aliases_and_non_origin_components() {
    let static_root = tempfile::tempdir().unwrap();
    for origin in [
        "http://192.0.2.10",
        "http://localhost:3080",
        "http://127.0.0.1:3080/path",
        "http://user@127.0.0.1:3080",
        "http://127.0.0.1:3080/?query=yes",
        "https://*.example.com",
    ] {
        let mut values = valid_loopback_values(static_root.path());
        values.insert("WORKOS_APP_ORIGIN".into(), origin.into());
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::Origin),
            "accepted invalid origin {origin}"
        );
    }

    let mut no_preview = valid_loopback_values(static_root.path());
    no_preview.insert("WORKOS_ALLOW_LOOPBACK_PREVIEW".into(), "false".into());
    assert_eq!(
        TeamConfig::from_values(&no_preview).err(),
        Some(TeamConfigError::Origin)
    );
}

#[test]
fn callback_must_be_the_exact_route_under_the_origin() {
    let static_root = tempfile::tempdir().unwrap();
    let mut values = valid_https_values(static_root.path());
    values.insert(
        "GITHUB_CALLBACK_URL".into(),
        "https://workos.example.com/auth/github/callback/extra".into(),
    );
    assert_eq!(
        TeamConfig::from_values(&values).err(),
        Some(TeamConfigError::Callback)
    );
}

#[test]
fn oauth_configuration_is_required_without_fallbacks() {
    let static_root = tempfile::tempdir().unwrap();
    for key in ["GITHUB_CLIENT_ID", "GITHUB_CLIENT_SECRET"] {
        let mut values = valid_https_values(static_root.path());
        values.remove(key);
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::Github)
        );
    }

    let mut invalid = valid_https_values(static_root.path());
    invalid.insert("GITHUB_CLIENT_SECRET".into(), "contains a space".into());
    assert_eq!(
        TeamConfig::from_values(&invalid).err(),
        Some(TeamConfigError::Github)
    );
}

#[test]
fn encryption_key_and_postgres_database_are_strictly_validated() {
    let static_root = tempfile::tempdir().unwrap();
    let mut invalid_key = valid_https_values(static_root.path());
    invalid_key.insert("WORKOS_ENC_KEY".into(), "not-a-key".into());
    assert_eq!(
        TeamConfig::from_values(&invalid_key).err(),
        Some(TeamConfigError::EncryptionKey)
    );

    for database_url in [
        "sqlite:///tmp/workos.db",
        "postgres:///workos",
        "postgres://db.example.com",
        "not a url",
    ] {
        let mut values = valid_https_values(static_root.path());
        values.insert("DATABASE_URL".into(), database_url.into());
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::Database),
            "accepted invalid database URL {database_url}"
        );
    }
}

#[test]
fn bind_address_uses_safe_defaults_and_rejects_invalid_values() {
    let static_root = tempfile::tempdir().unwrap();
    let mut defaults = valid_https_values(static_root.path());
    defaults.remove("CODEG_HOST");
    defaults.remove("CODEG_PORT");
    assert_eq!(
        TeamConfig::from_values(&defaults).unwrap().bind.to_string(),
        "0.0.0.0:3080"
    );

    for (key, value) in [
        ("CODEG_HOST", "localhost"),
        ("CODEG_HOST", "0.0.0.0/0"),
        ("CODEG_PORT", "0"),
        ("CODEG_PORT", "65536"),
    ] {
        let mut values = valid_https_values(static_root.path());
        values.insert(key.into(), value.into());
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::Bind)
        );
    }
}

#[test]
fn static_root_must_be_an_existing_absolute_directory() {
    let static_root = tempfile::tempdir().unwrap();
    for invalid in [
        "relative/static".to_owned(),
        static_root.path().join("missing").to_string_lossy().into_owned(),
    ] {
        let mut values = valid_https_values(static_root.path());
        values.insert("CODEG_STATIC_DIR".into(), invalid);
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::StaticRoot)
        );
    }

    let file = tempfile::NamedTempFile::new().unwrap();
    let mut values = valid_https_values(static_root.path());
    values.insert(
        "CODEG_STATIC_DIR".into(),
        file.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        TeamConfig::from_values(&values).err(),
        Some(TeamConfigError::StaticRoot)
    );
}

#[test]
fn catalog_runtime_must_remain_disabled() {
    let static_root = tempfile::tempdir().unwrap();
    for value in ["true", "1", "unexpected"] {
        let mut values = valid_https_values(static_root.path());
        values.insert("WORKOS_ENABLE_CATALOG_RUNTIME".into(), value.into());
        assert_eq!(
            TeamConfig::from_values(&values).err(),
            Some(TeamConfigError::Runtime)
        );
    }
}

#[test]
fn configuration_errors_never_include_configured_secrets_or_urls() {
    let static_root = tempfile::tempdir().unwrap();
    let secret = "do-not-print-this-github-secret";
    let database = "postgres://secret-user:secret-password@/missing-host";
    let mut values = valid_https_values(static_root.path());
    values.insert("GITHUB_CLIENT_SECRET".into(), secret.into());
    values.insert("DATABASE_URL".into(), database.into());

    for error in [
        TeamConfig::from_values(&values).err().unwrap(),
        TeamConfigError::Database,
    ] {
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains(secret));
        assert!(!diagnostic.contains(database));
        assert!(!diagnostic.contains("secret-password"));
    }
}
