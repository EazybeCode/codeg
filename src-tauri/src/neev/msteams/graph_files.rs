//! Native channel-file transport. No token acquisition or authorization is implied.
//! Call only after checking tenant, user, channel, and artifact audience access.
use super::files_policy::{safe_name, segment, FileError};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum GraphError {
    Policy(FileError),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Retryable {
        status: u16,
        retry_after_seconds: Option<u64>,
    },
    Http(u16),
    UncertainTransport,
    InvalidResponse,
}
impl From<FileError> for GraphError {
    fn from(e: FileError) -> Self {
        Self::Policy(e)
    }
}
#[derive(Debug, Clone)]
pub struct Item {
    pub drive_id: String,
    pub item_id: String,
    pub web_url: String,
    pub etag: String,
}

/// A file found at a destination path: what reconciliation compares.
#[derive(Debug, Clone)]
pub struct Remote {
    pub item: Item,
    pub size: u64,
}

pub struct GraphFiles {
    client: Client,
    pub(crate) base: String,
    /// Plain-HTTP loopback upload capabilities are accepted (tests only).
    #[cfg(test)]
    pub(crate) allow_loopback: bool,
}
impl GraphFiles {
    pub fn new() -> Result<Self, GraphError> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| GraphError::UncertainTransport)?,
            base: "https://graph.microsoft.com/v1.0".into(),
            #[cfg(test)]
            allow_loopback: false,
        })
    }

    /// The named child of `parent`, with its size, or `None`
    /// when absent — the reconciliation read after an upload whose completion
    /// is uncertain.
    pub async fn child(&self, token: &str, parent: &Item, name: &str) -> Result<Option<Remote>, GraphError> {
        safe_name(name)?;
        let path = format!(
            "/drives/{}/items/{}:/{}",
            segment(&parent.drive_id)?,
            segment(&parent.item_id)?,
            segment(name)?
        );
        let value = match self.get(token, &path).await {
            Err(GraphError::NotFound) => return Ok(None),
            other => other?,
        };
        if value["folder"].is_object() {
            return Err(GraphError::Conflict);
        }
        let item = parse_item(&value)?;
        // Exactly this folder's child, in this drive.
        if item.drive_id != parent.drive_id || value["parentReference"]["id"].as_str() != Some(parent.item_id.as_str()) {
            return Err(GraphError::InvalidResponse);
        }
        let size = value["size"].as_u64().ok_or(GraphError::InvalidResponse)?;
        Ok(Some(Remote { item, size }))
    }

    /// SHA-256 and length of a file's CURRENT content, for proving it is the
    /// artifact. Read through Graph's `/content` with the Graph bearer; when
    /// Graph redirects, the pre-authenticated download URL is fetched WITHOUT
    /// that bearer, only on an https `*.sharepoint.com` host, and never
    /// redirected further. `Ok(None)` once more than `limit` bytes arrive:
    /// such a file cannot be the artifact, and is not read further.
    pub async fn content_sha256(&self, token: &str, item: &Item, limit: u64) -> Result<Option<(String, u64)>, GraphError> {
        use sha2::{Digest, Sha256};
        let path = format!("/drives/{}/items/{}/content", segment(&item.drive_id)?, segment(&item.item_id)?);
        let first = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(|_| GraphError::UncertainTransport)?;
        let mut res = match first.status().as_u16() {
            200 => first,
            301 | 302 | 303 | 307 | 308 => {
                let location = first
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(GraphError::InvalidResponse)?;
                let url = self.trusted_download(location)?;
                let res = self.client.get(url).timeout(DOWNLOAD_TIMEOUT).send().await.map_err(|_| GraphError::UncertainTransport)?;
                if res.status().as_u16() != 200 {
                    return Err(status_error(&res));
                }
                res
            }
            _ => return Err(status_error(&first)),
        };
        let mut hasher = Sha256::new();
        let mut length = 0u64;
        while let Some(chunk) = res.chunk().await.map_err(|_| GraphError::UncertainTransport)? {
            length += chunk.len() as u64;
            if length > limit {
                return Ok(None);
            }
            hasher.update(&chunk);
        }
        Ok(Some((format!("{:x}", hasher.finalize()), length)))
    }

    /// A download URL Graph redirected to: https SharePoint only, no
    /// credentials or port in it (loopback only in tests).
    fn trusted_download(&self, location: &str) -> Result<reqwest::Url, GraphError> {
        let url = reqwest::Url::parse(location).map_err(|_| GraphError::InvalidResponse)?;
        #[cfg(test)]
        if self.allow_loopback && url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
            return Ok(url);
        }
        let host = url.host_str().ok_or(GraphError::InvalidResponse)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || !host.ends_with(".sharepoint.com") {
            return Err(GraphError::InvalidResponse);
        }
        Ok(url)
    }
    pub async fn channel_folder(
        &self,
        token: &str,
        team: &str,
        channel: &str,
    ) -> Result<Item, GraphError> {
        let path = format!(
            "/teams/{}/channels/{}/filesFolder",
            segment(team)?,
            segment(channel)?
        );
        let value = self.get(token, &path).await?;
        if !value["folder"].is_object() {
            return Err(GraphError::InvalidResponse);
        }
        parse_item(&value)
    }

    async fn get(&self, token: &str, path: &str) -> Result<Value, GraphError> {
        response(
            self.client
                .get(format!("{}{path}", self.base))
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| GraphError::UncertainTransport)?,
        )
        .await
    }

    /// Creates an inherited-permission child folder. A collision is resolved only
    /// by reading the same named folder, never by choosing another destination.
    pub async fn folder(&self, token: &str, parent: &Item, name: &str) -> Result<Item, GraphError> {
        safe_name(name)?;
        let path = format!(
            "/drives/{}/items/{}",
            segment(&parent.drive_id)?,
            segment(&parent.item_id)?
        );
        let result = response(
            self.client
                .post(format!("{}{path}/children", self.base))
                .bearer_auth(token)
                .json(&json!({"name":name,"folder":{},"@microsoft.graph.conflictBehavior":"fail"}))
                .send()
                .await
                .map_err(|_| GraphError::UncertainTransport)?,
        )
        .await;
        let value = match result {
            Err(GraphError::Conflict) => {
                self.get(token, &format!("{path}:/{}", segment(name)?))
                    .await?
            }
            other => other?,
        };
        if !value["folder"].is_object() {
            return Err(GraphError::Conflict);
        }
        let item = parse_item(&value)?;
        if item.drive_id != parent.drive_id {
            return Err(GraphError::InvalidResponse);
        }
        Ok(item)
    }

    /// Fetch current metadata before generating a proposed revision. This API
    /// deliberately has no overwrite operation: human edits remain authoritative.
    pub async fn current(&self, token: &str, item: &Item) -> Result<Item, GraphError> {
        let value = self
            .get(
                token,
                &format!(
                    "/drives/{}/items/{}",
                    segment(&item.drive_id)?,
                    segment(&item.item_id)?
                ),
            )
            .await?;
        let current = parse_item(&value)?;
        if current.drive_id != item.drive_id || current.item_id != item.item_id {
            return Err(GraphError::InvalidResponse);
        }
        Ok(current)
    }

    /// Begin create-only publication. Persist the returned session securely before
    /// sending bytes. A revision must have a different proposed-copy filename.
    pub async fn begin(
        &self,
        token: &str,
        parent: &Item,
        name: &str,
    ) -> Result<UploadSession, GraphError> {
        safe_name(name)?;
        let path = format!(
            "/drives/{}/items/{}:/{}:/createUploadSession",
            segment(&parent.drive_id)?,
            segment(&parent.item_id)?,
            segment(name)?
        );
        let value = response(
            self.client
                .post(format!("{}{path}", self.base))
                .bearer_auth(token)
                .json(&json!({"item":{"name":name,"@microsoft.graph.conflictBehavior":"fail"}}))
                .send()
                .await
                .map_err(|_| GraphError::UncertainTransport)?,
        )
        .await?;
        #[cfg(test)]
        if self.allow_loopback {
            return UploadSession::restore(required(&value, "uploadUrl")?, required(&value, "expirationDateTime")?, &parent.drive_id, true);
        }
        UploadSession::from_response(&value, parent.drive_id.clone())
    }

    /// Compatibility wrapper; no arbitrary file-size ceiling. Caller retains its
    /// durable artifact independently of this owned byte buffer.
    pub async fn complete_small(&self, session: &UploadSession, bytes: Vec<u8>) -> Result<Item, GraphError> {
        self.complete(session, &bytes).await
    }

    /// Sequential 5 MiB fragments (16 * 320 KiB), with a short final fragment.
    /// Capability requests never carry Graph bearer credentials or follow redirects.
    pub async fn complete(&self, session: &UploadSession, bytes: &[u8]) -> Result<Item, GraphError> {
        self.transfer(session, bytes, 0).await
    }

    /// Resume the same securely persisted capability; never create a new session
    /// after ambiguity. A missing completed session requires publisher reconciliation.
    pub async fn resume(&self, session: &UploadSession, bytes: &[u8]) -> Result<Item, GraphError> {
        let status = match self.upload_status(session).await {
            // A committed upload and an expired session both disappear. The
            // durable owner must reconcile artifact metadata before retrying.
            Err(GraphError::NotFound) => return Err(GraphError::UncertainTransport),
            other => other?,
        };
        if status["id"].is_string() { return self.completed_item(session, &status); }
        let start = next_offset(&status, bytes.len())?;
        self.transfer(session, bytes, start).await
    }

    fn completed_item(&self, session: &UploadSession, value: &Value) -> Result<Item, GraphError> {
        let item = parse_item(value)?;
        if item.drive_id != session.drive_id { return Err(GraphError::InvalidResponse); }
        Ok(item)
    }

    async fn transfer(&self, session: &UploadSession, bytes: &[u8], mut start: usize) -> Result<Item, GraphError> {
        if bytes.is_empty() { return Err(GraphError::InvalidResponse); }
        let mut recoveries = 0;
        loop {
            let end = start.saturating_add(UPLOAD_CHUNK_BYTES).min(bytes.len());
            if start >= end || start % UPLOAD_ALIGNMENT != 0 { return Err(GraphError::InvalidResponse); }
            let sent = self.client.put(&session.upload_url)
                .header("Content-Range", format!("bytes {start}-{}/{total}", end - 1, total=bytes.len()))
                .header("Content-Type", "application/octet-stream")
                .body(bytes[start..end].to_vec()).send().await;
            let result = match sent { Ok(res) => response(res).await, Err(_) => Err(GraphError::UncertainTransport) };
            match result {
                Ok(value) if value["id"].is_string() => return self.completed_item(session, &value),
                Ok(value) => {
                    let next = next_offset(&value, bytes.len())?;
                    if next != end { return Err(GraphError::InvalidResponse); }
                    start = next;
                    recoveries = 0;
                }
                Err(error @ GraphError::Retryable { .. }) => return Err(error),
                Err(GraphError::UncertainTransport) | Err(GraphError::Http(416)) => {
                    recoveries += 1;
                    if recoveries > 3 { return Err(GraphError::UncertainTransport); }
                    let status = match self.upload_status(session).await {
                        Err(GraphError::NotFound) => return Err(GraphError::UncertainTransport),
                        other => other?,
                    };
                    if status["id"].is_string() { return self.completed_item(session, &status); }
                    let next = next_offset(&status, bytes.len())?;
                    if next > end { return Err(GraphError::InvalidResponse); }
                    start = next;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// After an uncertain transfer, return Graph's nextExpectedRanges for the
    /// durable publisher to reconcile; never create another agent run.
    pub async fn upload_status(&self, session: &UploadSession) -> Result<Value, GraphError> {
        response(
            self.client
                .get(&session.upload_url)
                .send()
                .await
                .map_err(|_| GraphError::UncertainTransport)?,
        )
        .await
    }
}

/// Graph requires non-final fragments aligned to 320 KiB and requests under 60 MiB.
pub const UPLOAD_ALIGNMENT: usize = 320 * 1024;
pub const UPLOAD_CHUNK_BYTES: usize = 16 * UPLOAD_ALIGNMENT;

fn next_offset(value: &Value, total: usize) -> Result<usize, GraphError> {
    let ranges = value["nextExpectedRanges"].as_array().filter(|r| !r.is_empty()).ok_or(GraphError::InvalidResponse)?;
    let mut first = total;
    for range in ranges {
        let (start, end) = range.as_str().and_then(|r|r.split_once('-')).ok_or(GraphError::InvalidResponse)?;
        let start: usize = start.parse().map_err(|_|GraphError::InvalidResponse)?;
        if start >= total || start % UPLOAD_ALIGNMENT != 0 { return Err(GraphError::InvalidResponse); }
        if !end.is_empty() {
            let end: usize = end.parse().map_err(|_|GraphError::InvalidResponse)?;
            // Only sequential uploads are supported: never transmit through an
            // already received range reported by a different uploader.
            if end != total - 1 { return Err(GraphError::InvalidResponse); }
        }
        first = first.min(start);
    }
    Ok(first)
}

// Intentionally no Debug/Serialize: the URL is a preauthorized secret.
pub struct UploadSession {
    upload_url: String,
    pub expires_at: String,
    drive_id: String,
}
impl UploadSession {
    /// The capability's parts, ONLY for sealing into encrypted storage
    /// (`documents::seal`); never logged, displayed or serialized in clear.
    pub(crate) fn export_for_sealing(&self) -> (String, String, String) {
        (self.upload_url.clone(), self.expires_at.clone(), self.drive_id.clone())
    }

    /// Rebuild a session from sealed storage, re-applying the same host rules
    /// as when Graph first returned it.
    pub(crate) fn restore(upload_url: &str, expires_at: &str, drive_id: &str, loopback: bool) -> Result<Self, GraphError> {
        if loopback && cfg!(test) && upload_url.starts_with("http://127.0.0.1:") {
            return Ok(Self {
                upload_url: upload_url.into(),
                expires_at: expires_at.into(),
                drive_id: drive_id.into(),
            });
        }
        Self::from_response(&json!({"uploadUrl": upload_url, "expirationDateTime": expires_at}), drive_id.to_owned())
    }

    fn from_response(value: &Value, drive_id: String) -> Result<Self, GraphError> {
        let raw = required(value, "uploadUrl")?;
        let url = reqwest::Url::parse(raw).map_err(|_| GraphError::InvalidResponse)?;
        let host = url.host_str().ok_or(GraphError::InvalidResponse)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || !(host.ends_with(".sharepoint.com") || host.ends_with(".up.1drv.com"))
        {
            return Err(GraphError::InvalidResponse);
        }
        Ok(Self {
            upload_url: raw.into(),
            expires_at: required(value, "expirationDateTime")?.into(),
            drive_id,
        })
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, GraphError> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or(GraphError::InvalidResponse)
}
fn parse_item(value: &Value) -> Result<Item, GraphError> {
    let web_url = required(value, "webUrl")?;
    let url = reqwest::Url::parse(web_url).map_err(|_| GraphError::InvalidResponse)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !url
            .host_str()
            .is_some_and(|h| h.ends_with(".sharepoint.com"))
    {
        return Err(GraphError::InvalidResponse);
    }
    Ok(Item {
        drive_id: required(&value["parentReference"], "driveId")?.into(),
        item_id: required(value, "id")?.into(),
        web_url: web_url.into(),
        // folders sometimes omit eTag; revision callers must check_version.
        etag: value["eTag"].as_str().unwrap_or("").into(),
    })
}
/// A content download may take longer than one metadata call.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

fn status_error(res: &Response) -> GraphError {
    let status = res.status().as_u16();
    match status {
        401 => GraphError::Unauthorized,
        403 => GraphError::Forbidden,
        404 => GraphError::NotFound,
        409 | 412 => GraphError::Conflict,
        429 | 500..=599 => GraphError::Retryable {
            status,
            retry_after_seconds: res
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse().ok()),
        },
        _ => GraphError::Http(status),
    }
}

async fn response(res: Response) -> Result<Value, GraphError> {
    if !res.status().is_success() {
        return Err(status_error(&res));
    }
    // Receiving headers does not prove the response body arrived. A failed
    // body read can hide a successful final commit and must retain ambiguity.
    let bytes = res.bytes().await.map_err(|_| GraphError::UncertainTransport)?;
    serde_json::from_slice(&bytes).map_err(|_| GraphError::InvalidResponse)
}

#[cfg(test)]
#[path = "graph_files_tests.rs"]
mod tests;
