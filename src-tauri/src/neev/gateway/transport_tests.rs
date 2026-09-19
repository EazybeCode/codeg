use super::*;
use crate::neev::startup::ServerState;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::RootCertStore;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

const CA: &[u8] = include_bytes!("fixtures/private/ca.crt");
const SERVER_CRT: &[u8] = include_bytes!("fixtures/private/server.crt");
const SERVER_KEY: &[u8] = include_bytes!("fixtures/private/server.key");
const SERVICE_A: &[u8] = include_bytes!("fixtures/private/service-a.crt");
const SERVICE_A_KEY: &[u8] = include_bytes!("fixtures/private/service-a.key");
const SERVICE_B: &[u8] = include_bytes!("fixtures/private/service-b.crt");
const SERVICE_WRONG: &[u8] = include_bytes!("fixtures/private/service-wrong.crt");
const SERVICE_WRONG_KEY: &[u8] = include_bytes!("fixtures/private/service-wrong.key");
const SHORTLIVED: &[u8] = include_bytes!("fixtures/private/service-shortlived.crt");
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/neev/gateway/fixtures/private");

fn der(pem: &[u8]) -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(pem).unwrap().into_owned()
}
fn fp(pem: &[u8]) -> String {
    fingerprint_hex(&der(pem))
}
fn now() -> UnixTime {
    UnixTime::since_unix_epoch(SystemTime::now().duration_since(UNIX_EPOCH).unwrap())
}

fn make_verifier(entries: &[(&str, &[u8])]) -> (Arc<AllowlistClientVerifier>, Arc<Mutex<ServiceAllowlist>>) {
    let list = Arc::new(Mutex::new(
        ServiceAllowlist::new(entries.iter().map(|(s, pem)| (s.to_string(), fp(pem)))).unwrap(),
    ));
    let verifier = AllowlistClientVerifier::new(&der(CA), list.clone()).unwrap();
    (verifier, list)
}

// ---- configuration (fail-closed) ----

#[test]
fn config_from_env_is_all_or_nothing() {
    let full: HashMap<&str, String> = [
        (ENV_BIND, "127.0.0.1:8443".to_string()),
        (ENV_CERT, "/run/secrets/s.crt".to_string()),
        (ENV_KEY, "/run/secrets/s.key".to_string()),
        (ENV_CLIENT_CA, "/run/secrets/ca.crt".to_string()),
        (ENV_SERVICES, format!("svc-a={}", fp(SERVICE_A))),
    ]
    .into_iter()
    .collect();
    fn parse(
        m: &HashMap<&'static str, String>,
    ) -> Result<Option<PrivateTransportConfig>, TransportError> {
        let owned: HashMap<String, String> =
            m.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        PrivateTransportConfig::from_env(move |k: &str| owned.get(k).cloned())
    }

    // Nothing set → disabled.
    assert!(parse(&HashMap::new()).unwrap().is_none());
    // Full, valid → configured.
    let cfg = parse(&full).unwrap().unwrap();
    assert_eq!(cfg.bind.port(), 8443);
    assert_eq!(cfg.services.len(), 1);
    // Partial → fail-closed.
    let mut partial = full.clone();
    partial.remove(ENV_SERVICES);
    assert_eq!(parse(&partial), Err(TransportError::Incomplete));
    // Bad bind → invalid.
    let mut bad_bind = full.clone();
    bad_bind.insert(ENV_BIND, "not-an-addr".to_string());
    assert_eq!(parse(&bad_bind), Err(TransportError::Invalid));
    // Empty services value → invalid.
    let mut empty_services = full.clone();
    empty_services.insert(ENV_SERVICES, "   ".to_string());
    assert_eq!(parse(&empty_services), Err(TransportError::Invalid));
}

#[test]
fn allowlist_rejects_empty_oversized_and_malformed() {
    assert!(ServiceAllowlist::new(Vec::<(String, String)>::new()).is_err());
    let three = vec![
        ("a".into(), fp(SERVICE_A)),
        ("b".into(), fp(SERVICE_B)),
        ("c".into(), fp(SERVICE_WRONG)),
    ];
    assert!(ServiceAllowlist::new(three).is_err());
    assert!(ServiceAllowlist::new(vec![("a".into(), "xyz".into())]).is_err());
    assert!(ServiceAllowlist::new(vec![("".into(), fp(SERVICE_A))]).is_err());
    assert!(ServiceAllowlist::new(vec![("a".into(), fp(SERVICE_A))]).is_ok());
}

#[test]
fn allowlist_identifies_and_revokes() {
    let mut list = ServiceAllowlist::new(vec![
        ("svc-a".into(), fp(SERVICE_A)),
        ("svc-b".into(), fp(SERVICE_B)),
    ])
    .unwrap();
    assert_eq!(list.identify(&der(SERVICE_A)), Some("svc-a"));
    assert_eq!(list.identify(&der(SERVICE_B)), Some("svc-b"));
    assert_eq!(list.identify(&der(SERVICE_WRONG)), None);
    list.remove("svc-a");
    assert_eq!(list.identify(&der(SERVICE_A)), None);
    assert_eq!(list.identify(&der(SERVICE_B)), Some("svc-b"));
    assert_eq!(list.len(), 1);
}

// ---- verifier: CA pin + identity allowlist + expiry + fail-closed ----

#[test]
fn verifier_accepts_allowlisted_ca_signed_identity() {
    let (v, _) = make_verifier(&[("svc-a", SERVICE_A)]);
    assert!(v.verify_client_cert(&der(SERVICE_A), &[], now()).is_ok());
    assert!(v.client_auth_mandatory());
    assert!(v.offer_client_auth());
}

#[test]
fn verifier_rejects_wrong_ca() {
    let (v, _) = make_verifier(&[("svc-a", SERVICE_A)]);
    // service-wrong is signed by a different CA: chain validation fails.
    assert!(v.verify_client_cert(&der(SERVICE_WRONG), &[], now()).is_err());
}

#[test]
fn verifier_rejects_ca_signed_but_unlisted_identity() {
    let (v, _) = make_verifier(&[("svc-a", SERVICE_A)]);
    // service-b is validly CA-signed but not allowlisted → fail-closed.
    assert!(v.verify_client_cert(&der(SERVICE_B), &[], now()).is_err());
}

#[test]
fn verifier_rejects_expired_certificate() {
    let (v, _) = make_verifier(&[("svc-short", SHORTLIVED)]);
    // The fixture cert is intentionally short-lived (valid 2026-09-15..09-16), so
    // anchor to FIXED times inside/after its window rather than the wall clock —
    // otherwise this test silently expires with the calendar (it did on 09-19).
    let within = UnixTime::since_unix_epoch(Duration::from_secs(1_789_516_800)); // 2026-09-16 00:00Z (in window)
    let after = UnixTime::since_unix_epoch(Duration::from_secs(1_789_862_400)); // 2026-09-20 00:00Z (past notAfter)
    // Verified after notAfter → rejected.
    assert!(v.verify_client_cert(&der(SHORTLIVED), &[], after).is_err());
    // Sanity: accepted within its validity window.
    assert!(v.verify_client_cert(&der(SHORTLIVED), &[], within).is_ok());
}

#[test]
fn verifier_rotation_overlap_then_revocation() {
    let (v, list) = make_verifier(&[("svc-a", SERVICE_A), ("svc-b", SERVICE_B)]);
    assert!(v.verify_client_cert(&der(SERVICE_A), &[], now()).is_ok());
    assert!(v.verify_client_cert(&der(SERVICE_B), &[], now()).is_ok());
    // Revoke A: new requests presenting A are denied; B still admitted.
    list.lock().unwrap().remove("svc-a");
    assert!(v.verify_client_cert(&der(SERVICE_A), &[], now()).is_err());
    assert!(v.verify_client_cert(&der(SERVICE_B), &[], now()).is_ok());
}

// ---- live mTLS listener ----

async fn test_state() -> Arc<ServerState> {
    let db = crate::neev::test_support::database().await;
    Arc::new(
        crate::neev::startup::ServerState::new(
            db,
            crate::neev::test_support::preview_config(),
            Arc::new(crate::neev::test_support::TestClock::new(1_000)),
        )
        .unwrap(),
    )
}

fn listener_config(bind: SocketAddr, services: Vec<(String, String)>) -> PrivateTransportConfig {
    PrivateTransportConfig {
        bind,
        server_cert_path: format!("{FIXTURE_DIR}/server.crt").into(),
        server_key_path: format!("{FIXTURE_DIR}/server.key").into(),
        client_ca_path: format!("{FIXTURE_DIR}/ca.crt").into(),
        services,
    }
}

fn client_config(client_crt: &[u8], client_key: &[u8]) -> rustls::ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.add(der(CA)).unwrap();
    let chain = vec![der(client_crt)];
    let key = PrivateKeyDer::from_pem_slice(client_key).unwrap().clone_key();
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .unwrap()
}

async fn request(addr: SocketAddr, crt: &[u8], key: &[u8], raw: &[u8]) -> std::io::Result<String> {
    let connector = TlsConnector::from(Arc::new(client_config(crt, key)));
    let tcp = TcpStream::connect(addr).await?;
    let mut tls = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await?;
    tls.write_all(raw).await?;
    let mut resp = Vec::new();
    tls.read_to_end(&mut resp).await?;
    Ok(String::from_utf8_lossy(&resp).into_owned())
}

#[tokio::test]
async fn live_listener_serves_allowlisted_and_excludes_public_and_rejects_untrusted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = listener_config(addr, vec![("svc-a".into(), fp(SERVICE_A))]);
    tokio::spawn(serve_on(listener, cfg, test_state().await));

    // Allowlisted client reaches a private route. An empty body fails wire
    // decoding, which the handler reports as 400 — proving the full path
    // (mTLS → route → issuer handler → response) without minting anything.
    let validate = request(
        addr,
        SERVICE_A,
        SERVICE_A_KEY,
        b"POST /validate HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(validate.starts_with("HTTP/1.1 400"), "validate path: {validate}");

    // The browser-bound `issue` op is not routed on the private listener.
    let issue = request(
        addr,
        SERVICE_A,
        SERVICE_A_KEY,
        b"POST /issue HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(issue.starts_with("HTTP/1.1 404"), "public exclusion: {issue}");

    // Malformed framing is handled by the mature HTTP stack (400 / closed).
    let malformed = request(addr, SERVICE_A, SERVICE_A_KEY, b"GARBAGE\r\n\r\n")
        .await
        .unwrap_or_default();
    assert!(
        malformed.is_empty() || malformed.contains("400"),
        "malformed framing: {malformed}"
    );

    // Wrong-CA client cannot complete the handshake.
    let wrong = request(
        addr,
        SERVICE_WRONG,
        SERVICE_WRONG_KEY,
        b"POST /validate HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(wrong.is_err(), "wrong-CA client must fail the handshake");
}

#[tokio::test]
async fn live_listener_denies_ca_signed_but_unlisted_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Allowlist only svc-a; present service-b (validly CA-signed, unlisted).
    let cfg = listener_config(addr, vec![("svc-a".into(), fp(SERVICE_A))]);
    tokio::spawn(serve_on(listener, cfg, test_state().await));

    let denied = request(
        addr,
        SERVICE_B,
        include_bytes!("fixtures/private/service-b.key"),
        b"POST /validate HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(denied.is_err(), "unlisted identity must be denied at handshake");
}
