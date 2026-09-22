//! Serving the frontend's static export from the artifact directory.
//!
//! Neev serves these bytes itself rather than running an nginx beside Docs. That
//! keeps one public entry and one place where framing headers are set, and it
//! means a path that is not in the artifact is simply absent instead of being
//! something that reaches Docs.
//!
//! The export is Next.js with `trailingSlash: true` and `basePath:
//! '/document-engine'`, so the editor page for a document is emitted as a file
//! whose name comes from the route, not from the document id — the dynamic
//! route's single HTML shell is reused for every document, and the id is read
//! client-side. Resolution therefore tries the directory form, then the file
//! form, then the dynamic shell.

use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode},
};

use crate::neev::http::ApiError;

/// Largest single asset served. The export's biggest chunks are well under this;
/// the bound exists so a mistaken artifact root cannot stream something huge.
const MAX_ASSET: u64 = 32 * 1024 * 1024;

/// The shell Next.js emits for the dynamic document route.
///
/// With `trailingSlash: true` the export writes the directory form —
/// `docs/[id]/index.html`, with `[id]` literal — not `docs/[id].html`. Verified
/// against the staged artifact; getting this wrong makes every document 404
/// while every asset still loads, which reads as a routing bug rather than a
/// missing file.
const DOCUMENT_SHELL: &str = "docs/[id]/index.html";

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        // Never guessed as HTML: a mislabelled asset served as HTML is how a
        // static directory turns into a scripting surface.
        _ => "application/octet-stream",
    }
}

/// Resolve a request path to a file inside the artifact root.
///
/// The root is canonicalized and every candidate is checked to still be inside
/// it after canonicalization, so a symlink in the artifact cannot walk out of
/// the directory — the path text having no `..` in it is not sufficient, since
/// the link is resolved by the filesystem, not by the string.
fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let trimmed = relative.trim_start_matches('/');
    let mut candidates = Vec::new();
    if trimmed.is_empty() {
        candidates.push(root.join("index.html"));
    } else {
        candidates.push(root.join(trimmed));
        candidates.push(root.join(format!("{}/index.html", trimmed.trim_end_matches('/'))));
        candidates.push(root.join(format!("{}.html", trimmed.trim_end_matches('/'))));
        if trimmed.starts_with("docs/") {
            candidates.push(root.join(DOCUMENT_SHELL));
        }
    }
    for candidate in candidates {
        let Ok(resolved) = candidate.canonicalize() else {
            continue;
        };
        if resolved.starts_with(&root) && resolved.is_file() {
            return Some(resolved);
        }
    }
    None
}

pub(crate) fn serve(root: &Path, relative: &str) -> Result<Response<Body>, ApiError> {
    let path = resolve(root, relative).ok_or(ApiError::NotFound)?;
    let metadata = std::fs::metadata(&path).map_err(|_| ApiError::NotFound)?;
    if metadata.len() > MAX_ASSET {
        return Err(ApiError::Invalid);
    }
    let bytes = std::fs::read(&path).map_err(|_| ApiError::NotFound)?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&path)),
    );
    // The export's asset names are content-addressed by build id, but the HTML
    // shells are not, so nothing here is declared immutable.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/[id]")).unwrap();
        std::fs::create_dir_all(root.path().join("_next/static")).unwrap();
        std::fs::write(root.path().join("index.html"), b"home").unwrap();
        std::fs::write(root.path().join(DOCUMENT_SHELL), b"shell").unwrap();
        std::fs::write(root.path().join("_next/static/chunk.js"), b"chunk").unwrap();
        root
    }

    #[test]
    fn a_document_route_resolves_to_the_dynamic_shell() {
        // Every document shares one HTML shell; the id is read client-side. If
        // this resolved per-id we would be looking for files that the export
        // never emits.
        let root = artifact();
        let resolved = resolve(
            root.path(),
            "docs/b3b1f0d2-8c4a-4f5e-9a1b-2c3d4e5f6a7b/",
        )
        .unwrap();
        assert!(resolved.ends_with(DOCUMENT_SHELL));
    }

    #[test]
    fn assets_resolve_and_absent_names_do_not() {
        let root = artifact();
        assert!(resolve(root.path(), "_next/static/chunk.js").is_some());
        assert!(resolve(root.path(), "").is_some());
        assert!(resolve(root.path(), "_next/static/missing.js").is_none());
    }

    #[test]
    fn a_symlink_cannot_walk_out_of_the_artifact_root() {
        // The path text having no `..` is not enough: the link is resolved by
        // the filesystem, so the check has to be made after canonicalization.
        let root = artifact();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("escape"))
                .unwrap();
            assert!(
                resolve(root.path(), "escape").is_none(),
                "a symlink escaped the artifact root"
            );
        }
    }

    #[test]
    fn an_unknown_extension_is_never_served_as_html() {
        assert_eq!(content_type(Path::new("a.js")), "text/javascript; charset=utf-8");
        assert_eq!(content_type(Path::new("a.html")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("a.weird")), "application/octet-stream");
        assert_eq!(content_type(Path::new("noext")), "application/octet-stream");
    }
}
