//! Private gateway transport: an in-process mutual-TLS listener that terminates
//! client certificates, pins a private CA, enforces an exact-identity service
//! allowlist, and dispatches the private issuer operations
//! (`redeem/validate/revoke/revocations/ack`). `issue` is browser-bound and is
//! never served here.
//!
//! Security posture (per NEEV-GATEWAY-CONTRACT-v1 "Private transport"):
//! - Chain validity is pinned to a private CA (`WebPkiClientVerifier`).
//! - Client identity is pinned to an explicit allowlist of end-entity
//!   certificate SHA-256 fingerprints, independent of the CA — a CA-signed but
//!   unlisted certificate is denied (fail-closed). Up to two identities may be
//!   trusted at once for bounded rotation overlap; removing one denies new
//!   requests from it.
//! - Client auth is mandatory; an absent, expired, wrong-CA or wrong-service
//!   certificate aborts the handshake. Timeouts/unavailability are surfaced as
//!   `Unavailable`, never as permission granted.
//! - Private keys are read from mounted files, never source or browser config.
//! - Only the request ID and a fixed error category are ever logged.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::WebPkiClientVerifier;
use rustls::{DigitallySignedStruct, DistinguishedName, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use axum::extract::{DefaultBodyLimit, State};
use axum::Extension;
use tokio::sync::Semaphore;

use super::routes::{self, Encoded};
use crate::neev::startup::ServerState;

/// Maximum concurrent private connections; excess is shed (unavailable, not a
/// grant). Bounded to keep the private listener's footprint predictable.
const MAX_CONNECTIONS: usize = 64;
/// Largest request body accepted on the private path (contract: 16 KiB).
const MAX_BODY: usize = 16 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Fixed environment keys for the private listener. All-or-nothing: a partial
/// set is a hard configuration error (fail-closed), an empty set disables it.
pub(crate) const ENV_BIND: &str = "NEEV_GATEWAY_PRIVATE_BIND";
pub(crate) const ENV_CERT: &str = "NEEV_GATEWAY_PRIVATE_CERT";
pub(crate) const ENV_KEY: &str = "NEEV_GATEWAY_PRIVATE_KEY";
pub(crate) const ENV_CLIENT_CA: &str = "NEEV_GATEWAY_PRIVATE_CLIENT_CA";
pub(crate) const ENV_SERVICES: &str = "NEEV_GATEWAY_PRIVATE_SERVICES";

/// A configuration error. Deliberately opaque: never echoes paths or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransportError {
    /// A required key was present but another was missing (partial config).
    Incomplete,
    /// A value failed to parse (bind address, fingerprint, empty allowlist,
    /// more than two identities).
    Invalid,
    /// A certificate/key file could not be read or parsed.
    Material,
}

/// The allowlist of trusted client identities, keyed by lowercase hex SHA-256 of
/// the end-entity certificate DER. At most two entries (rotation overlap).
#[derive(Debug, Clone, Default)]
pub(crate) struct ServiceAllowlist {
    by_fingerprint: BTreeMap<String, String>,
}

impl ServiceAllowlist {
    /// Build from `(service, fingerprint)` pairs. Rejects an empty set, more
    /// than two identities, or a malformed/duplicate fingerprint.
    pub(crate) fn new(
        entries: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, TransportError> {
        let mut by_fingerprint = BTreeMap::new();
        for (service, fingerprint) in entries {
            let fingerprint = normalize_fingerprint(&fingerprint)?;
            if service.trim().is_empty() {
                return Err(TransportError::Invalid);
            }
            if by_fingerprint.insert(fingerprint, service).is_some() {
                return Err(TransportError::Invalid);
            }
        }
        if by_fingerprint.is_empty() || by_fingerprint.len() > 2 {
            return Err(TransportError::Invalid);
        }
        Ok(Self { by_fingerprint })
    }

    /// Return the trusted service name for a certificate, or `None` if its
    /// fingerprint is not (or no longer) allowlisted.
    pub(crate) fn identify(&self, end_entity: &CertificateDer<'_>) -> Option<&str> {
        self.by_fingerprint
            .get(&fingerprint_hex(end_entity))
            .map(String::as_str)
    }

    /// Revoke an identity by service name (rotation removal / connection
    /// revocation). New handshakes presenting it are denied thereafter.
    pub(crate) fn remove(&mut self, service: &str) {
        self.by_fingerprint.retain(|_, s| s != service);
    }

    pub(crate) fn len(&self) -> usize {
        self.by_fingerprint.len()
    }
}

fn normalize_fingerprint(raw: &str) -> Result<String, TransportError> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.len() != 64 || !cleaned.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(TransportError::Invalid);
    }
    Ok(cleaned)
}

fn fingerprint_hex(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A client-certificate verifier that pins the CA (chain validity) and then the
/// exact end-entity fingerprint (identity). Fails closed on either check.
#[derive(Debug)]
pub(crate) struct AllowlistClientVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    allowlist: Arc<std::sync::Mutex<ServiceAllowlist>>,
}

impl AllowlistClientVerifier {
    pub(crate) fn new(
        ca: &CertificateDer<'_>,
        allowlist: Arc<std::sync::Mutex<ServiceAllowlist>>,
    ) -> Result<Arc<Self>, TransportError> {
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone()).map_err(|_| TransportError::Material)?;
        let inner = WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| TransportError::Material)?;
        Ok(Arc::new(Self { inner, allowlist }))
    }

    fn is_allowlisted(&self, end_entity: &CertificateDer<'_>) -> bool {
        self.allowlist
            .lock()
            .map(|list| list.identify(end_entity).is_some())
            .unwrap_or(false)
    }
}

impl ClientCertVerifier for AllowlistClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // Chain + expiry against the pinned CA first, then the exact-identity
        // allowlist. Either failure denies the handshake.
        self.inner.verify_client_cert(end_entity, intermediates, now)?;
        if !self.is_allowlisted(end_entity) {
            return Err(rustls::Error::General(
                "client certificate is not an allowlisted service identity".into(),
            ));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

/// Validated private-listener configuration. `None` from [`from_env`] means the
/// listener is disabled; an `Err` means a partial or malformed configuration
/// (fail-closed — the server must refuse to start rather than run insecurely).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrivateTransportConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) server_cert_path: PathBuf,
    pub(crate) server_key_path: PathBuf,
    pub(crate) client_ca_path: PathBuf,
    pub(crate) services: Vec<(String, String)>,
}

impl PrivateTransportConfig {
    pub(crate) fn from_env(
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, TransportError> {
        let keys = [ENV_BIND, ENV_CERT, ENV_KEY, ENV_CLIENT_CA, ENV_SERVICES];
        let present: Vec<Option<String>> = keys.iter().map(|k| get(k)).collect();
        if present.iter().all(Option::is_none) {
            return Ok(None);
        }
        if present.iter().any(Option::is_none) {
            return Err(TransportError::Incomplete);
        }
        let bind = present[0]
            .as_ref()
            .unwrap()
            .parse::<SocketAddr>()
            .map_err(|_| TransportError::Invalid)?;
        let services = parse_services(present[4].as_ref().unwrap())?;
        // Validate the allowlist shape now so misconfiguration fails at startup.
        ServiceAllowlist::new(services.clone())?;
        Ok(Some(Self {
            bind,
            server_cert_path: PathBuf::from(present[1].as_ref().unwrap()),
            server_key_path: PathBuf::from(present[2].as_ref().unwrap()),
            client_ca_path: PathBuf::from(present[3].as_ref().unwrap()),
            services,
        }))
    }
}

/// Parse `service=fingerprint[,service=fingerprint]` (1–2 entries).
fn parse_services(raw: &str) -> Result<Vec<(String, String)>, TransportError> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (service, fingerprint) = part.split_once('=').ok_or(TransportError::Invalid)?;
        out.push((service.trim().to_string(), fingerprint.trim().to_string()));
    }
    if out.is_empty() {
        return Err(TransportError::Invalid);
    }
    Ok(out)
}

fn read_pem_certs(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let bytes = std::fs::read(path).map_err(|_| TransportError::Material)?;
    let certs = CertificateDer::pem_slice_iter(&bytes)
        .map(|c| c.map(|c| c.into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TransportError::Material)?;
    if certs.is_empty() {
        return Err(TransportError::Material);
    }
    Ok(certs)
}

fn read_pem_key(path: &std::path::Path) -> Result<PrivateKeyDer<'static>, TransportError> {
    let bytes = std::fs::read(path).map_err(|_| TransportError::Material)?;
    PrivateKeyDer::from_pem_slice(&bytes)
        .map(|k| k.clone_key())
        .map_err(|_| TransportError::Material)
}

/// Build the rustls server config: server identity from mounted files, client
/// auth pinned to the CA + fingerprint allowlist.
pub(crate) fn build_server_config(
    cfg: &PrivateTransportConfig,
    allowlist: Arc<std::sync::Mutex<ServiceAllowlist>>,
) -> Result<rustls::ServerConfig, TransportError> {
    let server_chain = read_pem_certs(&cfg.server_cert_path)?;
    let server_key = read_pem_key(&cfg.server_key_path)?;
    let ca = read_pem_certs(&cfg.client_ca_path)?
        .into_iter()
        .next()
        .ok_or(TransportError::Material)?;
    let verifier = AllowlistClientVerifier::new(&ca, allowlist)?;
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| TransportError::Material)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(server_chain, server_key)
        .map_err(|_| TransportError::Material)
}

/// Proof that a request arrived over the verified mTLS path from an allowlisted
/// service. Constructible only after a successful handshake + identity lookup,
/// so the private dispatch cannot be reached without it.
#[derive(Debug, Clone)]
pub(crate) struct PrivateWitness {
    service: String,
}

impl PrivateWitness {
    pub(crate) fn service(&self) -> &str {
        &self.service
    }
}

fn encoded_response(enc: Encoded) -> axum::response::Response {
    use axum::response::IntoResponse;
    let status = axum::http::StatusCode::from_u16(enc.status)
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/octet-stream",
        )],
        enc.body,
    )
        .into_response()
}

/// Route a verified private request to its issuer operation, sealed behind the
/// witness. `issue` is browser-bound and has no route here.
pub(crate) async fn dispatch(
    _witness: &PrivateWitness,
    state: &ServerState,
    path: &str,
    body: &[u8],
) -> Encoded {
    match path {
        "/redeem" => routes::redeem(&state.database, state.clock.as_ref(), body).await,
        "/validate" => routes::validate(&state.database, state.clock.as_ref(), body).await,
        "/revoke" => routes::revoke(&state.database, state.clock.as_ref(), body).await,
        "/revocations" => routes::revocations(&state.database, body).await,
        "/ack" => routes::ack(&state.database, body).await,
        _ => Encoded {
            status: 404,
            body: Vec::new(),
        },
    }
}

macro_rules! private_route {
    ($name:ident, $path:literal) => {
        async fn $name(
            State(state): State<Arc<ServerState>>,
            Extension(witness): Extension<PrivateWitness>,
            body: axum::body::Bytes,
        ) -> axum::response::Response {
            encoded_response(dispatch(&witness, &state, $path, &body).await)
        }
    };
}
private_route!(redeem_route, "/redeem");
private_route!(validate_route, "/validate");
private_route!(revoke_route, "/revoke");
private_route!(revocations_route, "/revocations");
private_route!(ack_route, "/ack");

/// The private router: exactly the five private operations, body-bounded. It
/// contains no `issue`, no `/api` prefix and no public asset routes, so the
/// browser-bound surface can never be reached over the private listener.
pub(crate) fn private_router(state: Arc<ServerState>) -> axum::Router {
    use axum::routing::post;
    axum::Router::new()
        .route("/redeem", post(redeem_route))
        .route("/validate", post(validate_route))
        .route("/revoke", post(revoke_route))
        .route("/revocations", post(revocations_route))
        .route("/ack", post(ack_route))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

/// Bind and serve the private listener until the process ends. Runs only when a
/// validated [`PrivateTransportConfig`] is present.
pub(crate) async fn serve(
    cfg: PrivateTransportConfig,
    state: Arc<ServerState>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    serve_on(listener, cfg, state).await
}

/// Serve on an already-bound listener (so callers/tests control the socket).
pub(crate) async fn serve_on(
    listener: tokio::net::TcpListener,
    cfg: PrivateTransportConfig,
    state: Arc<ServerState>,
) -> std::io::Result<()> {
    let allowlist = Arc::new(std::sync::Mutex::new(
        ServiceAllowlist::new(cfg.services.clone())
            .map_err(|_| std::io::Error::other("invalid allowlist"))?,
    ));
    let server_config = build_server_config(&cfg, allowlist.clone())
        .map_err(|_| std::io::Error::other("invalid TLS material"))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let limiter = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let router = private_router(state);
    loop {
        let (tcp, _) = listener.accept().await?;
        let Ok(permit) = limiter.clone().try_acquire_owned() else {
            // Shed load rather than queue unboundedly: drop the connection.
            drop(tcp);
            continue;
        };
        let acceptor = acceptor.clone();
        let allowlist = allowlist.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = serve_connection(acceptor, tcp, router, allowlist).await;
        });
    }
}

/// Terminate one mTLS connection, seal the witness from the verified peer, and
/// hand the stream to the axum/hyper HTTP stack (bounds and framing owned by
/// the mature stack, not by us).
async fn serve_connection(
    acceptor: tokio_rustls::TlsAcceptor,
    tcp: tokio::net::TcpStream,
    router: axum::Router,
    allowlist: Arc<std::sync::Mutex<ServiceAllowlist>>,
) -> std::io::Result<()> {
    let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
        Ok(Ok(stream)) => stream,
        // Handshake failure (absent/expired/wrong-CA/wrong-service) closes here.
        _ => return Ok(()),
    };
    let witness = {
        let (_, conn) = stream.get_ref();
        let end_entity = conn.peer_certificates().and_then(|c| c.first()).cloned();
        match end_entity.and_then(|cert| {
            allowlist
                .lock()
                .ok()
                .and_then(|list| list.identify(&cert).map(String::from))
        }) {
            Some(service) => PrivateWitness { service },
            None => return Ok(()),
        }
    };
    let service =
        hyper_util::service::TowerToHyperService::new(router.layer(Extension(witness)));
    let io = hyper_util::rt::TokioIo::new(stream);
    let _ = tokio::time::timeout(
        IO_TIMEOUT,
        hyper::server::conn::http1::Builder::new()
            .serve_connection(io, service),
    )
    .await;
    Ok(())
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
