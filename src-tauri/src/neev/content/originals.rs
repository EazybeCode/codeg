//! Read-through for originals evicted from the local document root.
//!
//! The server never holds Storage Box credentials. When a blob its database row
//! proves should exist is missing locally, it asks the root-side broker
//! (`scripts/operations/neev_originals_broker.py`) over a Unix socket to put it
//! back. The request names only `(org_id, uuid key, kind)` plus the row's
//! length and SHA-256; the broker derives both paths itself, fetches only what
//! replication verified, checks the bytes and publishes without replacing. The
//! caller then reads the file through the normal no-follow path.
//!
//! Every failure is `ContentError::Storage` (HTTP 503, retryable): the row
//! exists, so a missing blob during an outage must never look like `NotFound`.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::types::ContentError;

/// Absent or empty: read-through is off and a missing blob stays `NotFound`.
pub(crate) const BROKER_SOCKET_ENV: &str = "NEEV_ORIGINALS_BROKER_SOCKET";
/// Longer than any single broker fetch is allowed to take, so the broker's own
/// timeout, not this one, decides a slow transfer.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(660);
const REPLY_LIMIT: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlobKind {
    Document,
    Image,
    Attachment,
}

impl BlobKind {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Document => "md",
            Self::Image => "img",
            Self::Attachment => "att",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OriginalsBroker {
    socket: PathBuf,
    timeout: Duration,
}

impl OriginalsBroker {
    pub(crate) fn new(socket: PathBuf, timeout: Duration) -> Result<Self, ContentError> {
        if !socket.is_absolute() {
            return Err(ContentError::Invalid);
        }
        Ok(Self { socket, timeout })
    }

    pub(crate) fn from_env() -> Result<Option<Self>, ContentError> {
        match std::env::var_os(BROKER_SOCKET_ENV) {
            Some(value) if !value.is_empty() => {
                Self::new(PathBuf::from(value), DEFAULT_TIMEOUT).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Ask the broker to publish `{org_id}/{key}.{kind}` with exactly these
    /// bytes. `Ok` means a verified final file is now in place.
    pub(crate) async fn hydrate(
        &self,
        org_id: i32,
        key: &str,
        kind: BlobKind,
        byte_length: i64,
        sha256: &str,
    ) -> Result<(), ContentError> {
        let reply = self
            .exchange(serde_json::json!({
                "v": 1,
                "op": "hydrate",
                "org_id": org_id,
                "key": key,
                "kind": kind.extension(),
                "sha256": sha256,
                "byte_length": byte_length,
            }))
            .await;
        Self::expect(reply, &["present", "fetched"], org_id, kind)
    }

    /// Tombstone a blob whose row the app has deleted, so no later read-through,
    /// backup stream or replication can bring it back. Sent before the local
    /// unlink; the broker keeps the tombstone even after the remote is purged.
    pub(crate) async fn forget(
        &self,
        org_id: i32,
        key: &str,
        kind: BlobKind,
    ) -> Result<(), ContentError> {
        let reply = self
            .exchange(serde_json::json!({
                "v": 1,
                "op": "forget",
                "org_id": org_id,
                "key": key,
                "kind": kind.extension(),
            }))
            .await;
        Self::expect(reply, &["forgotten"], org_id, kind)
    }

    async fn exchange(&self, request: serde_json::Value) -> Option<serde_json::Value> {
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket).await.ok()?;
            let mut line = request.to_string().into_bytes();
            line.push(b'\n');
            stream.write_all(&line).await.ok()?;
            let mut reply = Vec::new();
            BufReader::new(stream.take(REPLY_LIMIT))
                .read_until(b'\n', &mut reply)
                .await
                .ok()?;
            serde_json::from_slice::<serde_json::Value>(&reply).ok()
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .ok()
            .flatten()
    }

    fn expect(
        reply: Option<serde_json::Value>,
        results: &[&str],
        org_id: i32,
        kind: BlobKind,
    ) -> Result<(), ContentError> {
        let accepted = reply.as_ref().is_some_and(|reply| {
            reply["ok"] == true
                && reply["result"]
                    .as_str()
                    .is_some_and(|result| results.contains(&result))
        });
        if accepted {
            return Ok(());
        }
        // Codes only (never paths or content): unavailable, not_verified, ...
        let code = reply
            .as_ref()
            .and_then(|reply| reply["error"].as_str())
            .unwrap_or("no_reply");
        tracing::warn!(
            org_id,
            kind = kind.extension(),
            code,
            "originals broker request failed"
        );
        Err(ContentError::Storage)
    }
}
