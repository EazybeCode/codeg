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
    /// Absolute path of the supervisor socket. Must be a direct child of
    /// `socket_parent` — the protected directory is what makes the path safe,
    /// so the file may not live anywhere else.
    pub(crate) socket_path: PathBuf,
    /// Absolute path of the shared MAC key. Direct child of `key_parent`.
    pub(crate) key_path: PathBuf,
    pub(crate) socket_parent: ProtectedParent,
    pub(crate) key_parent: ProtectedParent,
    pub(crate) socket_identity: FileIdentity,
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
    /// Production connect. Every check is on metadata the deployment declared
    /// in advance: nothing here is discovered from, or negotiated with, the
    /// peer. Non-Linux has no `SO_PEERCRED` equivalent we are willing to trust,
    /// so it stays [`TransportError::Unsupported`] rather than degrading to a
    /// weaker gate.
    #[cfg(not(target_os = "linux"))]
    pub(crate) async fn connect(
        _spec: &ControlDeployment,
        _candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        Err(TransportError::Unsupported)
    }

    /// Production connect (Linux): prove the socket and key are the declared
    /// files inside root-owned, non-tenant-writable directories reached without
    /// traversing a symlink; read the key; connect; prove the peer's credentials
    /// are the declared supervisor's; only then handshake.
    ///
    /// Ordering matters. The key is read from a verified path BEFORE the socket
    /// is touched, and the peer credentials are checked BEFORE a single byte of
    /// the handshake is written, so an impostor that managed to bind the path
    /// never sees a client nonce.
    ///
    /// The residual path TOCTOU — between `fstatat` and `connect` — is closed by
    /// the parent directory, not by the file check: only root (or the control
    /// group, when the directory grants it) can replace an entry there, and
    /// `SO_PEERCRED` re-proves who actually answered.
    #[cfg(target_os = "linux")]
    pub(crate) async fn connect(
        spec: &ControlDeployment,
        candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        let verified = spec.clone();
        // Metadata reads and the key read are blocking file I/O; keep them off
        // the async worker even though they are small and one-off.
        let key = tokio::task::spawn_blocking(move || verify_deployment(&verified))
            .await
            .map_err(|_| TransportError::Io)??;
        let stream = timeout(HANDSHAKE_DEADLINE, UnixStream::connect(&spec.socket_path))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::Io)?;
        let peer = stream.peer_cred().map_err(|_| TransportError::Peer)?;
        if peer.uid() != spec.peer_uid || peer.gid() != spec.peer_gid {
            return Err(TransportError::Peer);
        }
        Self::handshake(stream, key, candidate).await
    }

    /// The nonce/hello/verify exchange, shared by the production gate and the
    /// test seam so neither can drift from the other.
    async fn handshake(
        mut stream: UnixStream,
        key: [u8; KEY_SIZE],
        candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        let mut nonce = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| TransportError::Closed)?;
        let hello = control_wire::encode_client_hello(&nonce).map_err(|_| TransportError::Invalid)?;
        write_all(&mut stream, &hello, HANDSHAKE_DEADLINE).await?;
        let server_hello = read_exact_n(&mut stream, SERVER_HELLO_SIZE, HANDSHAKE_DEADLINE).await?;
        let context =
            control_wire::decode_server_hello(&key, &nonce, &server_hello).map_err(wire_category)?;
        let boot =
            Id::new(hex16(&context.supervisor_boot_id())).map_err(|_| TransportError::Invalid)?;
        let channel = ClientChannel::new(candidate, boot.clone());
        Ok(Self {
            stream: Some(stream),
            key,
            context,
            channel,
            boot,
        })
    }

    /// Test-only connect to an already-verified socket path. Physically behind
    /// `cfg(test)`; no production caller and no environment switch selects it.
    #[cfg(test)]
    pub(crate) async fn connect_unchecked(
        path: &std::path::Path,
        key: [u8; KEY_SIZE],
        candidate: Coordinator,
    ) -> Result<Self, TransportError> {
        let stream = timeout(HANDSHAKE_DEADLINE, UnixStream::connect(path))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::Io)?;
        Self::handshake(stream, key, candidate).await
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


/// Deployment acceptance. Returns the verified MAC key, or the first
/// reason the deployment is not trustworthy. Every rejection is
/// [`TransportError::Metadata`] so a caller learns nothing about which check
/// failed or what the filesystem looks like.
#[cfg(unix)]
fn verify_deployment(spec: &ControlDeployment) -> Result<[u8; KEY_SIZE], TransportError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    // The process must be one that could legitimately read a root:control key.
    // A spec describing some other process is a misconfiguration, not a
    // permission problem to discover later.
    // SAFETY: `geteuid`/`getegid` are always-succeeding POSIX reads of the
    // calling process's own ids; they touch no shared state.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    if uid != spec.self_uid || gid != spec.self_gid {
        return Err(TransportError::Metadata);
    }
    if spec.self_uid != 0 && spec.self_gid != spec.control_gid {
        return Err(TransportError::Metadata);
    }

    verify_parent(&spec.socket_parent, spec.control_gid)?;
    verify_parent(&spec.key_parent, spec.control_gid)?;
    verify_child_of(&spec.socket_path, &spec.socket_parent)?;
    verify_child_of(&spec.key_path, &spec.key_parent)?;

    // No-follow throughout: a symlink anywhere in the socket's or key's own
    // name would let whoever can create it redirect us outside the protected
    // directory we just proved.
    let socket = std::fs::symlink_metadata(&spec.socket_path).map_err(|_| TransportError::Metadata)?;
    if !socket.file_type().is_socket()
        || socket.dev() != spec.socket_identity.device
        || socket.ino() != spec.socket_identity.inode
        || socket.uid() != spec.socket_owner_uid
        || socket.gid() != spec.control_gid
        // World-connectable is world-writable: any local account could speak to
        // the supervisor, or bind over it.
        || socket.mode() & 0o007 != 0
    {
        return Err(TransportError::Metadata);
    }

    let key_file = std::fs::symlink_metadata(&spec.key_path).map_err(|_| TransportError::Metadata)?;
    if !key_file.file_type().is_file()
        || key_file.dev() != spec.key_identity.device
        || key_file.ino() != spec.key_identity.inode
        || key_file.uid() != 0
        || key_file.gid() != spec.control_gid
        // The key authenticates the supervisor: no access outside root+control,
        // and the control group reads it — never writes it.
        || key_file.mode() & 0o007 != 0
        || key_file.mode() & 0o030 != 0
        || key_file.nlink() != 1
        || key_file.len() != KEY_SIZE as u64
    {
        return Err(TransportError::Metadata);
    }

    let bytes = std::fs::read(&spec.key_path).map_err(|_| TransportError::Metadata)?;
    // Re-check the length after the read: the stat above described a file that
    // could, in principle, have been rewritten in between.
    let key: [u8; KEY_SIZE] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::Metadata)?;
    // An all-zero key is what an uninitialised or truncated-then-extended file
    // looks like, and it would still MAC perfectly well.
    if key.iter().all(|byte| *byte == 0) {
        return Err(TransportError::Metadata);
    }
    Ok(key)
}

/// A protected directory: the declared inode, root-owned, reached without
/// traversing a symlink, and writable by nobody outside root and the control
/// group.
#[cfg(unix)]
fn verify_parent(parent: &ProtectedParent, control_gid: u32) -> Result<(), TransportError> {
    use std::os::unix::fs::MetadataExt;

    validate_parent_path(&parent.absolute_path)?;
    no_symlink_ancestry(&parent.absolute_path)?;
    let meta =
        std::fs::symlink_metadata(&parent.absolute_path).map_err(|_| TransportError::Metadata)?;
    let mode = meta.mode() & 0o7777;
    if !meta.is_dir()
        || meta.dev() != parent.identity.device
        || meta.ino() != parent.identity.inode
        || meta.uid() != 0
        || meta.gid() != parent.gid
        || mode != parent.mode & 0o7777
        // Other-writable, or group-writable by anyone but the control group,
        // means a tenant could swap the socket or the key underneath us.
        || mode & 0o002 != 0
        || (mode & 0o020 != 0 && parent.gid != control_gid)
        // A sticky-bit-less shared directory is not what this is; and setgid/
        // setuid on a control directory is never intended.
        || mode & 0o6000 != 0
    {
        return Err(TransportError::Metadata);
    }
    Ok(())
}

/// The file must sit directly in the directory we proved, under a plain name.
#[cfg(unix)]
fn verify_child_of(path: &std::path::Path, parent: &ProtectedParent) -> Result<(), TransportError> {
    use std::path::Component;
    if !path.is_absolute() || path.parent() != Some(parent.absolute_path.as_path()) {
        return Err(TransportError::Metadata);
    }
    match path.components().next_back() {
        Some(Component::Normal(name))
            if !name.is_empty() && name.to_str().is_some_and(|n| !n.contains('\0')) =>
        {
            Ok(())
        }
        _ => Err(TransportError::Metadata),
    }
}

/// Refuse if any component of `path` is itself a symlink. Walking with
/// `symlink_metadata` never follows, so a swapped component is caught here
/// rather than silently resolved.
#[cfg(unix)]
fn no_symlink_ancestry(path: &std::path::Path) -> Result<(), TransportError> {
    let mut walked = std::path::PathBuf::new();
    for component in path.components() {
        walked.push(component);
        let meta = std::fs::symlink_metadata(&walked).map_err(|_| TransportError::Metadata)?;
        if meta.file_type().is_symlink() {
            return Err(TransportError::Metadata);
        }
    }
    Ok(())
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
