use std::{collections::BTreeSet, io::Read, path::Path, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};

use crate::neev::startup::ServerState;

use super::{add_private_headers, ApiError, IMMUTABLE_ASSET};

const CONFIRM_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'";

fn read_bounded(root: &Path, file: &str, limit: u64) -> Result<Vec<u8>, ApiError> {
    let root = root.canonicalize().map_err(|_| ApiError::NotFound)?;
    let path = root
        .join(file)
        .canonicalize()
        .map_err(|_| ApiError::NotFound)?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(ApiError::NotFound);
    }
    let file = std::fs::File::open(path).map_err(|_| ApiError::NotFound)?;
    if file.metadata().map_err(|_| ApiError::NotFound)?.len() > limit {
        return Err(ApiError::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ApiError::NotFound)?;
    if bytes.len() as u64 > limit {
        return Err(ApiError::Invalid);
    }
    Ok(bytes)
}

// Next's static export embeds hydration scripts. Authorize only the exact inline
// bytes from this admitted, trusted export file; dynamic nonce injection requires
// a dynamic Next server and cannot work here. Never permit inline script/eval.
fn app_csp(bytes: &[u8]) -> Result<HeaderValue, ApiError> {
    let html = std::str::from_utf8(bytes).map_err(|_| ApiError::Invalid)?;
    let mut remaining = html;
    let mut hashes = BTreeSet::new();
    while let Some(start) = remaining.find("<script") {
        remaining = &remaining[start + 7..];
        if !remaining.starts_with(|c: char| c == '>' || c.is_ascii_whitespace()) {
            continue;
        }
        let open = remaining.find('>').ok_or(ApiError::Invalid)?;
        let attributes = &remaining[..open];
        let body = &remaining[open + 1..];
        let close = body.find("</script>").ok_or(ApiError::Invalid)?;
        if !attributes
            .split_ascii_whitespace()
            .any(|attr| attr.starts_with("src="))
        {
            hashes.insert(format!(
                "'sha256-{}'",
                STANDARD.encode(Sha256::digest(&body.as_bytes()[..close]))
            ));
            if hashes.len() > 64 {
                return Err(ApiError::Invalid);
            }
        }
        remaining = &body[close + 9..];
    }
    // `frame-src 'self'` admits the document engine's iframe, which is
    // same-origin under `/document-engine/`. It is the narrowest addition that
    // lets the editor load at all: without it the frame is blocked by this
    // policy and the document area is simply empty. Nothing else is relaxed —
    // the frame's own document carries its own policy, and cross-origin frames
    // remain impossible because only `'self'` is permitted.
    let policy = format!("default-src 'none'; script-src 'self' {}; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'", hashes.into_iter().collect::<Vec<_>>().join(" "));
    HeaderValue::from_str(&policy).map_err(|_| ApiError::Invalid)
}

pub(super) async fn page(
    State(state): State<Arc<ServerState>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    if uri.path() == "/auth/invite" && uri.query().is_some() {
        return Err(ApiError::Invalid);
    }
    let (file, confirmation) = match uri.path() {
        "/" => ("index.html", false),
        "/login" => ("login.html", false),
        "/onboarding" => ("onboarding.html", false),
        "/workspace" => ("workspace.html", false),
        "/settings" => ("settings.html", false),
        "/agents/signin" => ("agents/signin.html", false),
        "/auth/email/confirm" => ("neev-auth/confirm.html", true),
        "/auth/invite" => ("neev-auth/invite.html", true),
        _ => return Err(ApiError::NotFound),
    };
    let bytes = read_bounded(&state.config.static_root, file, 1024 * 1024)?;
    let csp = if confirmation {
        HeaderValue::from_static(CONFIRM_CSP)
    } else {
        app_csp(&bytes)?
    };
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    add_private_headers(&mut response);
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, csp);
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

pub(super) async fn asset(
    State(state): State<Arc<ServerState>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let (file, content_type) = match uri.path() {
        "/neev-auth/confirm.js" => ("neev-auth/confirm.js", "text/javascript; charset=utf-8"),
        "/neev-auth/confirm.css" => ("neev-auth/confirm.css", "text/css; charset=utf-8"),
        "/neev-auth/invite.js" => ("neev-auth/invite.js", "text/javascript; charset=utf-8"),
        "/neev-auth/invite.css" => ("neev-auth/invite.css", "text/css; charset=utf-8"),
        path => exported_asset(path).ok_or(ApiError::NotFound)?,
    };
    let limit = if file.starts_with("neev-auth/") {
        128 * 1024
    } else {
        8 * 1024 * 1024
    };
    let bytes = read_bounded(&state.config.static_root, file, limit)?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONFIRM_CSP),
    );
    add_private_headers(&mut response);
    // `add_private_headers` has just said `no-store`, which is right for every
    // response whose body depends on who asked. A build-id-named chunk is not
    // one of those: the name IS the version, so the bytes behind it can never
    // change, and re-downloading the whole bundle on every load was the cost of
    // treating it as though they could. Overridden here rather than by skipping
    // `add_private_headers`, so the `no-referrer` it also sets is kept.
    //
    // `private`, not `public`: these paths sit behind the same authorization as
    // everything else, and nothing is gained by inviting a shared cache to hold
    // them. The reader's own browser is the only cache this needs.
    //
    // Only the content-addressed prefix qualifies. The navigation payload `.txt`
    // files and the icons keep their real names across builds, so a long cache
    // on those would pin a stale app; `neev-auth/*` is unhashed too.
    if file.starts_with("_next/static/") {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(IMMUTABLE_ASSET),
        );
    }
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn exported_asset(path: &str) -> Option<(&str, &'static str)> {
    if path.len() > 256
        || !path
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_-.".contains(&c))
    {
        return None;
    }
    let file = path.strip_prefix('/')?;
    if file
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return None;
    }
    let content_type = if file.starts_with("_next/static/") {
        match file.rsplit('.').next()? {
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "woff2" => "font/woff2",
            "woff" => "font/woff",
            "ttf" => "font/ttf",
            "ico" => "image/x-icon",
            "json" => "application/json",
            _ => return None,
        }
    } else if matches!(
        file,
        "icon.svg" | "icon-32x32.png" | "icon-128x128.png" | "favicon.ico"
    ) {
        match file.rsplit('.').next()? {
            "svg" => "image/svg+xml",
            "png" => "image/png",
            _ => "image/x-icon",
        }
    } else if navigation_payload(file) {
        "text/x-component; charset=utf-8"
    } else {
        return None;
    };
    Some((file, content_type))
}

fn navigation_payload(file: &str) -> bool {
    if matches!(
        file,
        "index.txt" | "login.txt" | "onboarding.txt" | "workspace.txt" | "settings.txt"
    ) {
        return true;
    }
    // The one nested exported page (`/agents/signin`): its flight payload and
    // its own segment files, by exact name. Nothing else under `agents/`.
    if file == "agents/signin.txt" {
        return true;
    }
    if let Some(segment) = file.strip_prefix("agents/signin/") {
        return matches!(
            segment,
            "__next._tree.txt"
                | "__next._head.txt"
                | "__next._full.txt"
                | "__next._index.txt"
                | "__next.agents.txt"
                | "__next.agents.signin.txt"
                | "__next.agents.signin.__PAGE__.txt"
        );
    }
    let (page, segment) = file.split_once('/').unwrap_or(("", file));
    if !matches!(page, "" | "login" | "onboarding" | "workspace" | "settings") {
        return false;
    }
    matches!(
        segment,
        "__next._tree.txt" | "__next._head.txt" | "__next._full.txt" | "__next._index.txt"
    ) || (page.is_empty() && segment == "__next.__PAGE__.txt")
        || (!page.is_empty()
            && (segment == format!("__next.{page}.txt")
                || segment == format!("__next.{page}.__PAGE__.txt")))
}

pub(super) async fn fallback(
    state: State<Arc<ServerState>>,
    uri: Uri,
    method: Method,
) -> Result<Response, ApiError> {
    if method != Method::GET {
        return Err(ApiError::NotFound);
    }
    asset(state, uri).await
}

#[cfg(test)]
mod attachment_preview_asset_tests {
    #[test]
    fn pdf_worker_is_only_served_from_exported_build_assets() {
        assert_eq!(
            super::exported_asset("/_next/static/media/pdf.worker.min.hash.mjs"),
            Some((
                "_next/static/media/pdf.worker.min.hash.mjs",
                "text/javascript; charset=utf-8"
            ))
        );
        assert!(super::exported_asset("/uploads/file.mjs").is_none());
        assert!(super::exported_asset("/_next/static/../secret.mjs").is_none());
    }
}

#[cfg(test)]
mod signin_page_asset_tests {
    const FLIGHT: &str = "text/x-component; charset=utf-8";

    #[test]
    fn only_the_signin_pages_own_flight_files_are_served() {
        for path in [
            "/agents/signin.txt",
            "/agents/signin/__next._tree.txt",
            "/agents/signin/__next._head.txt",
            "/agents/signin/__next._full.txt",
            "/agents/signin/__next._index.txt",
            "/agents/signin/__next.agents.txt",
            "/agents/signin/__next.agents.signin.txt",
            "/agents/signin/__next.agents.signin.__PAGE__.txt",
        ] {
            assert_eq!(
                super::exported_asset(path),
                Some((&path[1..], FLIGHT)),
                "{path}"
            );
        }
        for path in [
            // The page itself is served only by `page`, with the app CSP.
            "/agents/signin.html",
            "/agents/signin",
            "/agents.txt",
            "/agents/other.txt",
            "/agents/signin/other.txt",
            "/agents/signin/__next.agents.other.__PAGE__.txt",
            "/agents/signin/nested/__next._tree.txt",
            "/agents/signin/../signin.txt",
            "/agents/signin/./__next._tree.txt",
            "/agents//signin.txt",
            "/agents/signin/%2e%2e/secret.txt",
        ] {
            assert!(super::exported_asset(path).is_none(), "{path}");
        }
    }
}
