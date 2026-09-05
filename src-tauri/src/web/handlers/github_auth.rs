//! GitHub login (GitHub App "EazyWorkOS") — the multi-tenant identity flow.
//!
//! `/auth/github/login`    → redirect to GitHub's authorize page.
//! `/auth/github/callback` → exchange code → user token, fetch the GitHub user
//!   and their primary org, upsert `orgs`/`users` (tokens encrypted at rest),
//!   create a `sessions` row, set a signed httpOnly cookie, redirect to `/`.
//!
//! Config comes from env (loaded from secrets/workos.env): GITHUB_CLIENT_ID,
//! GITHUB_CLIENT_SECRET, GITHUB_CALLBACK_URL, WORKOS_ENC_KEY (hex-32-byte AEAD key).
//! See decision 0006 and migration m20260905_000001_multitenancy.

use std::sync::Arc;

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use axum::{
    extract::{Extension, Query},
    http::{header, HeaderMap, StatusCode},
    response::{AppendHeaders, Html, IntoResponse, Redirect, Response},
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{Duration as ChronoDuration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::db::entities::{org, session, user};

const SESSION_COOKIE: &str = "workos_session";
const SESSION_DAYS: i64 = 30;
const SCOPES: &str = "read:user user:email read:org repo";

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

// ─────────────────────────────── crypto ───────────────────────────────
fn enc_key() -> Result<[u8; 32], String> {
    let hex = env("WORKOS_ENC_KEY").ok_or("WORKOS_ENC_KEY not set")?;
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("zz"), 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| "WORKOS_ENC_KEY must be hex".to_string())?;
    if bytes.len() != 32 {
        return Err("WORKOS_ENC_KEY must be 32 bytes (64 hex chars)".into());
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&bytes);
    Ok(k)
}

/// Encrypt a secret → base64(nonce ‖ ciphertext). Returns None on any failure
/// (a missing token simply isn't stored, rather than blocking login).
fn encrypt(plaintext: &str) -> Option<String> {
    let key = enc_key().ok()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plaintext.as_bytes()).ok()?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Some(B64.encode(blob))
}

/// Reverse of `encrypt`. Returns None on any failure (bad key, tampered blob).
pub fn decrypt(blob: &str) -> Option<String> {
    let key = enc_key().ok()?;
    let raw = B64.decode(blob).ok()?;
    if raw.len() < 12 {
        return None;
    }
    let (nonce_bytes, ct) = raw.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let pt = cipher.decrypt(Nonce::from_slice(nonce_bytes), ct).ok()?;
    String::from_utf8(pt).ok()
}

/// Resolve the logged-in user from the session cookie. The API is still token-
/// gated (v1 bridge), but the browser also carries the `workos_session` cookie,
/// so per-user endpoints (GitHub repo ops) can identify who is acting.
pub async fn current_user(
    headers: &HeaderMap,
    db: &sea_orm::DatabaseConnection,
) -> Option<user::Model> {
    let sid = cookie_value(headers, SESSION_COOKIE)?;
    let sess = session::Entity::find_by_id(sid).one(db).await.ok().flatten()?;
    if sess.expires_at < chrono::Utc::now() {
        return None;
    }
    user::Entity::find_by_id(sess.user_id)
        .one(db)
        .await
        .ok()
        .flatten()
}

fn random_id() -> String {
    // 24 random bytes → url-safe-ish base64; used for session ids and OAuth state.
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 12 bytes
    let nonce2 = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut b = nonce.to_vec();
    b.extend_from_slice(&nonce2);
    B64.encode(b).replace(['+', '/', '='], "")
}

fn html_error(msg: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Html(format!(
            "<html><body style='font:15px -apple-system;padding:40px'>\
             <h2>Sign-in failed</h2><p>{}</p><p><a href='/auth/github/login'>Try again</a></p>\
             </body></html>",
            askama_escape(msg)
        )),
    )
        .into_response()
}

fn askama_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ─────────────────────────────── /auth/github/login ───────────────────────────────
pub async fn github_login() -> Response {
    let (client_id, callback) = match (env("GITHUB_CLIENT_ID"), env("GITHUB_CALLBACK_URL")) {
        (Some(c), Some(cb)) => (c, cb),
        _ => return html_error("Server missing GITHUB_CLIENT_ID / GITHUB_CALLBACK_URL"),
    };
    let state = random_id();
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope={}&state={}",
        urlencoding::encode(&client_id),
        urlencoding::encode(&callback),
        urlencoding::encode(SCOPES),
        urlencoding::encode(&state),
    );
    let cookie = format!(
        "workos_oauth_state={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600",
        state
    );
    (
        AppendHeaders([
            (header::SET_COOKIE, cookie),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ]),
        Redirect::to(&url),
    )
        .into_response()
}

/// The frontend sends unauthenticated users to `/login`. In server / multi-tenant
/// mode we show a branded landing page whose button starts the GitHub flow.
pub async fn login_redirect() -> Response {
    let page = r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in — Eazybe WorkOS</title>
<style>
 :root{--bg:#f7f7f5;--card:#fff;--ink:#1c1c1e;--muted:#8a8a8e;--line:#e6e6e3;--accent:#2383e2}
 @media(prefers-color-scheme:dark){:root{--bg:#111214;--card:#1b1d20;--ink:#ececed;--muted:#8a8d93;--line:#2a2d31;--accent:#4c9ffe}}
 *{box-sizing:border-box} html,body{height:100%}
 body{margin:0;display:flex;align-items:center;justify-content:center;background:
   radial-gradient(1200px 600px at 50% -10%, color-mix(in srgb,var(--accent) 12%, var(--bg)), var(--bg));
   color:var(--ink);font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
 .card{width:360px;max-width:calc(100vw - 32px);background:var(--card);border:1px solid var(--line);
   border-radius:16px;padding:36px 32px;text-align:center;box-shadow:0 10px 40px rgba(0,0,0,.08)}
 .logo{font-size:34px;line-height:1;margin-bottom:14px}
 h1{font-size:20px;margin:0 0 6px;letter-spacing:-.01em}
 p{color:var(--muted);margin:0 0 26px;font-size:13.5px}
 .btn{display:flex;align-items:center;justify-content:center;gap:10px;width:100%;
   padding:12px 16px;border-radius:10px;border:none;background:var(--ink);color:var(--card);
   font-size:14.5px;font-weight:600;cursor:pointer;text-decoration:none;transition:transform .05s}
 .btn:hover{transform:translateY(-1px)} .btn:active{transform:translateY(0)}
 .btn svg{width:18px;height:18px;fill:currentColor}
 .foot{margin-top:22px;font-size:11.5px;color:var(--muted)}
</style></head><body>
 <div class="card">
   <div class="logo">🧩</div>
   <h1>Eazybe WorkOS</h1>
   <p>Your team's shared AI coding workspace.</p>
   <a class="btn" href="/auth/github/login">
     <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>
     Sign in with GitHub
   </a>
   <div class="foot">You'll join your GitHub organization's workspace.</div>
 </div>
</body></html>"##;
    (
        AppendHeaders([(header::CACHE_CONTROL, "no-store".to_string())]),
        Html(page),
    )
        .into_response()
}

// ─────────────────────────────── /auth/github/callback ───────────────────────────────
#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct GhUser {
    id: i64,
    login: String,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct GhOrg {
    id: i64,
    login: String,
    avatar_url: Option<String>,
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix(&format!("{name}="))?.to_string().into())
}

pub async fn github_callback(
    Extension(state_app): Extension<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    // CSRF: if we set a state cookie (login-initiated), it must match. The
    // install-initiated callback carries no state of ours — tolerated.
    if let (Some(qs), Some(cs)) = (&q.state, cookie_value(&headers, "workos_oauth_state")) {
        if qs != &cs {
            return html_error("OAuth state mismatch");
        }
    }
    let code = match q.code {
        Some(c) if !c.is_empty() => c,
        _ => return html_error("No authorization code in callback"),
    };
    let (client_id, client_secret, callback) = match (
        env("GITHUB_CLIENT_ID"),
        env("GITHUB_CLIENT_SECRET"),
        env("GITHUB_CALLBACK_URL"),
    ) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return html_error("Server missing GitHub OAuth config"),
    };

    let http = reqwest::Client::new();

    // 1) code → token
    let tok: TokenResp = match http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", callback.as_str()),
        ])
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => match r.json().await {
            Ok(t) => t,
            Err(e) => return html_error(&format!("Bad token response: {e}")),
        },
        Err(e) => return html_error(&format!("Token exchange failed: {e}")),
    };
    if let Some(err) = tok.error {
        return html_error(&format!(
            "GitHub: {} — {}",
            err,
            tok.error_description.unwrap_or_default()
        ));
    }
    let access = match tok.access_token {
        Some(a) => a,
        None => return html_error("No access token returned"),
    };

    // 2) token → GitHub user
    let gh_user: GhUser = match http
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {access}"))
        .header("User-Agent", "EazyWorkOS")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => match r.json().await {
            Ok(u) => u,
            Err(e) => return html_error(&format!("Bad /user response: {e}")),
        },
        Err(e) => return html_error(&format!("Fetching user failed: {e}")),
    };

    // 3) primary org (first org, else the user's own account acts as the org)
    let orgs: Vec<GhOrg> = match http
        .get("https://api.github.com/user/orgs")
        .header("Authorization", format!("Bearer {access}"))
        .header("User-Agent", "EazyWorkOS")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r.json::<Vec<GhOrg>>().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let (org_gh_id, org_login, org_avatar) = match orgs.into_iter().next() {
        Some(o) => (o.id, o.login, o.avatar_url),
        None => (gh_user.id, gh_user.login.clone(), gh_user.avatar_url.clone()),
    };

    let db = &state_app.db.conn;
    let now = Utc::now();

    // 4) upsert org
    let org_row = match org::Entity::find()
        .filter(org::Column::GithubOrgId.eq(org_gh_id))
        .one(db)
        .await
    {
        Ok(Some(existing)) => existing,
        Ok(None) => {
            match (org::ActiveModel {
                github_org_id: Set(org_gh_id),
                login: Set(org_login.clone()),
                name: Set(Some(org_login.clone())),
                avatar_url: Set(org_avatar.clone()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            })
            .insert(db)
            .await
            {
                Ok(m) => m,
                Err(e) => return html_error(&format!("Create org failed: {e}")),
            }
        }
        Err(e) => return html_error(&format!("Org lookup failed: {e}")),
    };

    // 5) upsert user
    let enc_access = encrypt(&access);
    let enc_refresh = tok.refresh_token.as_deref().and_then(encrypt);
    let expires_at = tok.expires_in.map(|s| now + ChronoDuration::seconds(s));

    let user_row = match user::Entity::find()
        .filter(user::Column::GithubUserId.eq(gh_user.id))
        .one(db)
        .await
    {
        Ok(Some(existing)) => {
            let mut m: user::ActiveModel = existing.into();
            m.login = Set(gh_user.login.clone());
            m.name = Set(gh_user.name.clone());
            m.email = Set(gh_user.email.clone());
            m.avatar_url = Set(gh_user.avatar_url.clone());
            m.org_id = Set(org_row.id);
            m.gh_token_enc = Set(enc_access);
            m.gh_refresh_token_enc = Set(enc_refresh);
            m.gh_token_expires_at = Set(expires_at);
            m.updated_at = Set(now);
            match m.update(db).await {
                Ok(u) => u,
                Err(e) => return html_error(&format!("Update user failed: {e}")),
            }
        }
        Ok(None) => {
            match (user::ActiveModel {
                github_user_id: Set(gh_user.id),
                login: Set(gh_user.login.clone()),
                name: Set(gh_user.name.clone()),
                email: Set(gh_user.email.clone()),
                avatar_url: Set(gh_user.avatar_url.clone()),
                org_id: Set(org_row.id),
                role: Set("owner".to_string()),
                gh_token_enc: Set(enc_access),
                gh_refresh_token_enc: Set(enc_refresh),
                gh_token_expires_at: Set(expires_at),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            })
            .insert(db)
            .await
            {
                Ok(u) => u,
                Err(e) => return html_error(&format!("Create user failed: {e}")),
            }
        }
        Err(e) => return html_error(&format!("User lookup failed: {e}")),
    };

    // 6) session
    let sid = random_id();
    let expires = now + ChronoDuration::days(SESSION_DAYS);
    if let Err(e) = (session::ActiveModel {
        id: Set(sid.clone()),
        user_id: Set(user_row.id),
        created_at: Set(now),
        expires_at: Set(expires),
    })
    .insert(db)
    .await
    {
        return html_error(&format!("Create session failed: {e}"));
    }

    // 7) set the session cookie AND hand the browser the app token, so the
    // existing frontend (which reads localStorage["codeg_token"]) works with no
    // token prompt. Then land in the workspace. This token bridge is the v1
    // shim; Phase 2 switches the API to session-based per-org auth and drops it.
    let session_cookie = format!(
        "{SESSION_COOKIE}={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        SESSION_DAYS * 86400
    );
    let clear_state = "workos_oauth_state=; Path=/; HttpOnly; Max-Age=0".to_string();
    let app_token = env("CODEG_TOKEN").unwrap_or_default();
    let token_js = serde_json::to_string(&app_token).unwrap_or_else(|_| "\"\"".into());
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Signing in…</title>\
         <body style=\"font:15px -apple-system;padding:40px\">Signing you in…\
         <script>try{{localStorage.setItem('codeg_token',{token_js});}}catch(e){{}}\
         location.replace('/workspace');</script></body>"
    );
    (
        AppendHeaders([
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear_state),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ]),
        Html(body),
    )
        .into_response()
}
