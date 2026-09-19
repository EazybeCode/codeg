//! Authenticated local supervisor transport — Rust client.
//!
//! Composes the accepted wire framing ([`super::control_wire`]) and the typed
//! [`super::control_dto::ClientChannel`] over one owned Tokio Unix stream. It
//! only connects/validates/authenticates/correlates; it decides no effect and
//! mints no registration authority. The authenticated supervisor boot id is
//! established solely from a verified `decode_server_hello`.
//!
//! Linux is the production target for real filesystem/`SO_PEERCRED` acceptance;
//! [`SupervisorClient::connect`] returns [`TransportError::Unsupported`] on other
//! platforms. Portable framing/handshake/exchange is exercised through the
//! `#[cfg(test)]` `connect_unchecked` seam against a pre-bound socket.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use rand::rngs::OsRng;
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;

use super::control_dto::{self, ClientChannel, Coordinator, Id, Request, Response};
use super::control_wire::{self, WireContext};

const KEY_SIZE: usize = 32;
const CLIENT_HELLO_SIZE: usize = 40;
const SERVER_HELLO_SIZE: usize = 120;
const LEN_PREFIX: usize = 4;
const MAC_SIZE: usize = 32;
const RESPONSE_PAYLOAD_MAX: usize = 16384;

const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(10);

/// Fixed, sanitized transport failure categories. No path/key/peer detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    Invalid,
    Auth,
    Peer,
    Metadata,
    Timeout,
    Closed,
    Io,
    Unsupported,
}

/// Trusted deployment inputs (namespace-visible), never discovered from the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedParent {
    pub(crate) absolute_path: PathBuf,
    pub(crate) identity: FileIdentity,
    pub(crate) gid: u32,
    pub(crate) mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlDeployment {
    pub(crate) socket_parent: ProtectedParent,
    pub(crate) key_parent: ProtectedParent,
    pub(crate) key_identity: FileIdentity,
    pub(crate) control_gid: u32,
    pub(crate) self_uid: u32,
    pub(crate) self_gid: u32,
    pub(crate) peer_uid: u32,
    pub(crate) peer_gid: u32,
    pub(crate) socket_owner_uid: u32,
}

const MAX_PATH_COMPONENTS: usize = 32;
const MAX_SOCKET_PATH_BYTES: usize = 107;

/// A protected parent path must be absolute, normal, and bounded.
pub(crate) fn validate_parent_path(path: &std::path::Path) -> Result<(), TransportError> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(TransportError::Metadata);
    }
    let bytes = path.as_os_str().len();
    if bytes == 0 || bytes > MAX_SOCKET_PATH_BYTES {
        return Err(TransportError::Metadata);
    }
    let mut components = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                if part.is_empty() || part.to_str().is_none_or(|s| s.contains('\0')) {
                    return Err(TransportError::Metadata);
                }
                components += 1;
            }
            // No `.`/`..`/prefix or repeated-separator normalization is accepted.
            _ => return Err(TransportError::Metadata),
        }
    }
    if components == 0 || components > MAX_PATH_COMPONENTS {
        return Err(TransportError::Metadata);
    }
    Ok(())
}

fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// One authenticated client connection with a single outstanding request.
pub(crate) struct SupervisorClient {
    stream: Option<UnixStream>,
    key: [u8; KEY_SIZE],
    context: WireContext,
    channel: ClientChannel,
    boot: Id,
}

impl SupervisorClient {
    /// Production connect (Linux only): validate socket/key metadata + peer
    /// credentials, then handshake. Other platforms are unsupported.
    pub(crate) async fn connect(
        _spec: &ControlDeployment,
        _candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        if !cfg!(target_os = "linux") {
            return Err(TransportError::Unsupported);
        }
        // The Linux filesystem/no-follow-walk/SO_PEERCRED acceptance is a
        // deployment gate wired by the launcher owner; it is intentionally not
        // reachable from this environment and is not faked here.
        Err(TransportError::Unsupported)
    }

    /// Test-only connect to an already-verified socket path. Physically behind
    /// `cfg(test)`; no production caller and no environment switch selects it.
    #[cfg(test)]
    pub(crate) async fn connect_unchecked(
        path: &std::path::Path,
        key: [u8; KEY_SIZE],
        candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        let mut stream = timeout(HANDSHAKE_DEADLINE, UnixStream::connect(path))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::Io)?;
        let mut nonce = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| TransportError::Closed)?;
        let hello = control_wire::encode_client_hello(&nonce).map_err(|_| TransportError::Invalid)?;
        write_all(&mut stream, &hello, HANDSHAKE_DEADLINE).await?;
        let server_hello = read_exact_n(&mut stream, SERVER_HELLO_SIZE, HANDSHAKE_DEADLINE).await?;
        let context = control_wire::decode_server_hello(&key, &nonce, &server_hello)
            .map_err(wire_category)?;
        let boot = Id::new(hex16(&context.supervisor_boot_id())).map_err(|_| TransportError::Invalid)?;
        let channel = ClientChannel::new(candidate, boot.clone());
        Ok(Self {
            stream: Some(stream),
            key,
            context,
            channel,
            boot,
        })
    }

    /// The authenticated supervisor boot id minted from the verified server hello.
    pub(crate) fn authenticated_supervisor_boot_id(&self) -> &Id {
        &self.boot
    }

    /// Send one typed request and return the correlated typed response.
    /// Cancellation-safe by ownership: the stream is taken before the first
    /// await and only restored after full success, so a dropped in-flight future
    /// leaves the client poisoned rather than reusable mid-exchange.
    pub(crate) async fn exchange(
        &mut self,
        request: &Request,
    ) -> Result<Response, TransportError> {
        let mut stream = self.stream.take().ok_or(TransportError::Closed)?;
        let result = self.exchange_inner(&mut stream, request).await;
        match result {
            Ok(response) => {
                self.stream = Some(stream);
                Ok(response)
            }
            Err(error) => {
                self.channel.poison();
                Err(error)
            }
        }
    }

    async fn exchange_inner(
        &mut self,
        stream: &mut UnixStream,
        request: &Request,
    ) -> Result<Response, TransportError> {
        self.channel
            .begin(request)
            .map_err(|_| TransportError::Invalid)?;
        let payload = control_dto::encode_request(request).map_err(|_| TransportError::Invalid)?;
        let frame = control_wire::encode_request(&self.key, &self.context, &payload)
            .map_err(|_| TransportError::Invalid)?;
        write_all(stream, &frame, EXCHANGE_DEADLINE).await?;
        let response_frame = read_frame(stream, EXCHANGE_DEADLINE).await?;
        let response_payload =
            control_wire::decode_response(&self.key, &self.context, &payload, &response_frame)
                .map_err(wire_category)?;
        let response =
            control_dto::decode_response(&response_payload).map_err(|_| TransportError::Invalid)?;
        self.channel
            .finish(&response)
            .map_err(|_| TransportError::Invalid)?;
        Ok(response)
    }

    /// Poison the channel and drop the owned stream. Idempotent; no network await.
    pub(crate) fn close(&mut self) {
        self.channel.poison();
        self.stream = None;
    }
}

fn wire_category(error: control_wire::WireError) -> TransportError {
    match error {
        control_wire::WireError::Auth => TransportError::Auth,
        control_wire::WireError::Invalid => TransportError::Invalid,
    }
}

async fn write_all(
    stream: &mut UnixStream,
    data: &[u8],
    deadline: Duration,
) -> Result<(), TransportError> {
    timeout(deadline, stream.write_all(data))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|_| TransportError::Io)
}

async fn read_exact_n(
    stream: &mut UnixStream,
    count: usize,
    deadline: Duration,
) -> Result<Vec<u8>, TransportError> {
    let mut buffer = vec![0u8; count];
    timeout(deadline, stream.read_exact(&mut buffer))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|_| TransportError::Io)?;
    Ok(buffer)
}

/// Read `[4-byte len][payload][32 MAC]`, rejecting a bad length before the body.
async fn read_frame(stream: &mut UnixStream, deadline: Duration) -> Result<Vec<u8>, TransportError> {
    let header = read_exact_n(stream, LEN_PREFIX, deadline).await?;
    let declared = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if declared == 0 || declared > RESPONSE_PAYLOAD_MAX {
        return Err(TransportError::Invalid);
    }
    let body = read_exact_n(stream, declared + MAC_SIZE, deadline).await?;
    let mut frame = header;
    frame.extend_from_slice(&body);
    Ok(frame)
}

#[cfg(test)]
#[path = "control_transport_tests.rs"]
mod control_transport_tests;
