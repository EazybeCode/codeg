//! Organization join code: one reusable, admin-managed code per org. Admins
//! generate/rotate/disable and can re-display it (encrypted at rest). Any
//! authenticated user previews the org from a code then joins immediately as a
//! regular `member`. Unguessable, server-validated, rate-limited, never logged or
//! placed in URLs. No admin escalation; no bypass of removal (a removed member is
//! not silently reactivated by self-service); idempotent for an active member.
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use sea_orm::ConnectionTrait;
use sha2::{Digest, Sha256};
use std::net::IpAddr;

use crate::neev::{
    auth::session,
    db,
    error::{database_error, AuthError},
    mail::quota,
    startup::ServerState,
    types::{IdentityContext, OrgContext, OrgId, Role},
};

use super::service::lock_organization_details;

// Crockford-style alphabet without ambiguous 0/O/1/I/L; ~59 bits over 12 chars.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";
const CODE_LEN: usize = 12;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CodeView {
    pub(crate) code: String,
    pub(crate) disabled: bool,
    pub(crate) created_at: i64,
    pub(crate) revision: i64,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Preview {
    pub(crate) org_id: OrgId,
    pub(crate) organization_name: String,
}

fn generate_code() -> Result<String, AuthError> {
    let mut bytes = [0u8; CODE_LEN];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| AuthError::Unavailable)?;
    // Rejection-free modulo bias is negligible for a 30-symbol alphabet at this
    // entropy; the code is a shareable admission secret, not a key.
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect())
}
/// Canonical form for hashing/lookup: uppercase alphanumerics only (callers may
/// paste with spaces or dashes). Never logged.
fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}
fn code_hash(normalized: &str) -> String {
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}
fn aad(org_id: OrgId) -> Vec<u8> {
    format!("neev.org_join_code:{org_id}").into_bytes()
}
fn encrypt_code(key: &[u8; 32], org_id: OrgId, normalized: &str) -> Result<String, AuthError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AuthError::Unavailable)?;
    let mut nonce = [0u8; 12];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| AuthError::Unavailable)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: normalized.as_bytes(),
                aad: &aad(org_id),
            },
        )
        .map_err(|_| AuthError::Unavailable)?;
    let mut output = nonce.to_vec();
    output.extend(ciphertext);
    Ok(URL_SAFE_NO_PAD.encode(output))
}
fn decrypt_code(key: &[u8; 32], org_id: OrgId, value: &str) -> Result<String, AuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthError::Unavailable)?;
    let (nonce, ciphertext) = bytes.split_at_checked(12).ok_or(AuthError::Unavailable)?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AuthError::Unavailable)?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad(org_id),
            },
        )
        .map_err(|_| AuthError::Unavailable)?;
    String::from_utf8(plaintext).map_err(|_| AuthError::Unavailable)
}

/// Admin: create the code if absent, or rotate it (new value, revision+1,
/// re-enabled). Returns the plaintext for one display.
pub(crate) async fn rotate(state: &ServerState, actor: &OrgContext) -> Result<CodeView, AuthError> {
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(AuthError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, now).await?;
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    lock_organization_details(&tx, actor.org_id).await?;
    let code = generate_code()?;
    let normalized = normalize(&code);
    let hash = code_hash(&normalized);
    let enc = encrypt_code(&state.config.encryption_key, actor.org_id, &normalized)?;
    db::execute(
        &tx,
        "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1) ON CONFLICT(org_id) DO UPDATE SET code_hash=$2,code_enc=$3,disabled=false,created_by=$4,created_at=$5,revision=neev_org_join_codes.revision+1",
        vec![
            actor.org_id.into(),
            hash.into(),
            enc.into(),
            actor.identity.user_id.into(),
            now.into(),
        ],
    )
    .await?;
    let row = db::one(
        &tx,
        "SELECT disabled,created_at,revision FROM neev_org_join_codes WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?;
    let view = CodeView {
        code,
        disabled: row.try_get("", "disabled").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        revision: row.try_get("", "revision").map_err(database_error)?,
    };
    tx.commit().await.map_err(database_error)?;
    Ok(view)
}

/// Admin: re-display the current code (decrypted), or None if none exists yet.
pub(crate) async fn view(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Option<CodeView>, AuthError> {
    let now = state.clock.now_unix();
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, now).await?;
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    let row = db::one(
        &tx,
        "SELECT code_enc,disabled,created_at,revision FROM neev_org_join_codes WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let enc: String = row.try_get("", "code_enc").map_err(database_error)?;
    let code = decrypt_code(&state.config.encryption_key, actor.org_id, &enc)?;
    Ok(Some(CodeView {
        code,
        disabled: row.try_get("", "disabled").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        revision: row.try_get("", "revision").map_err(database_error)?,
    }))
}

/// Admin: enable/disable joining without rotating the code.
pub(crate) async fn set_enabled(
    state: &ServerState,
    actor: &OrgContext,
    enabled: bool,
) -> Result<(), AuthError> {
    let now = state.clock.now_unix();
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, now).await?;
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    let updated = db::execute(
        &tx,
        "UPDATE neev_org_join_codes SET disabled=$1 WHERE org_id=$2",
        vec![(!enabled).into(), actor.org_id.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Unavailable);
    }
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

/// Resolve an org from a pasted code (rate-limited). Uniform Forbidden for
/// unknown/disabled so probing cannot distinguish states.
pub(crate) async fn preview(
    state: &ServerState,
    identity: &IdentityContext,
    code: &str,
    source_ip: IpAddr,
) -> Result<Preview, AuthError> {
    let now = state.clock.now_unix();
    let normalized = normalize(code);
    let hash = code_hash(&normalized);
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    quota::reserve_in_transaction(
        &tx,
        &state.config.encryption_key,
        &[
            quota::Bucket::JoinCodeSourceIp(source_ip),
            quota::Bucket::JoinCodeActor(identity.user_id),
        ],
        now,
    )
    .await?;
    let row = db::one(
        &tx,
        "SELECT j.org_id,j.disabled,o.name,o.disabled AS org_disabled FROM neev_org_join_codes j JOIN neev_organizations o ON o.id=j.org_id WHERE j.code_hash=$1",
        vec![hash.into()],
    )
    .await?;
    // Commit the rate-limit reservation even for a miss so probing is bounded.
    tx.commit().await.map_err(database_error)?;
    let Some(row) = row.filter(|_| normalized.len() >= 8) else {
        return Err(AuthError::Forbidden);
    };
    let disabled: bool = row.try_get("", "disabled").map_err(database_error)?;
    let org_disabled: bool = row.try_get("", "org_disabled").map_err(database_error)?;
    if disabled || org_disabled {
        return Err(AuthError::Forbidden);
    }
    Ok(Preview {
        org_id: row.try_get("", "org_id").map_err(database_error)?,
        organization_name: row.try_get("", "name").map_err(database_error)?,
    })
}

/// Join by code as a regular member after the caller confirmed the org.
/// Idempotent for an active member; refuses to reactivate a removed membership
/// (removal is not bypassable by self-service); never grants admin.
pub(crate) async fn join(
    state: &ServerState,
    identity: &IdentityContext,
    code: &str,
    confirmed_org_id: OrgId,
    source_ip: IpAddr,
) -> Result<OrgId, AuthError> {
    let now = state.clock.now_unix();
    let normalized = normalize(code);
    let hash = code_hash(&normalized);
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    quota::reserve_in_transaction(
        &tx,
        &state.config.encryption_key,
        &[
            quota::Bucket::JoinCodeSourceIp(source_ip),
            quota::Bucket::JoinCodeActor(identity.user_id),
        ],
        now,
    )
    .await?;
    let row = db::one(
        &tx,
        "SELECT j.org_id,j.disabled,o.disabled AS org_disabled FROM neev_org_join_codes j JOIN neev_organizations o ON o.id=j.org_id WHERE j.code_hash=$1",
        vec![hash.into()],
    )
    .await?;
    let Some(row) = row.filter(|_| normalized.len() >= 8) else {
        tx.commit().await.map_err(database_error)?;
        return Err(AuthError::Forbidden);
    };
    let org_id: OrgId = row.try_get("", "org_id").map_err(database_error)?;
    let disabled: bool = row.try_get("", "disabled").map_err(database_error)?;
    let org_disabled: bool = row.try_get("", "org_disabled").map_err(database_error)?;
    if disabled || org_disabled || org_id != confirmed_org_id {
        tx.commit().await.map_err(database_error)?;
        return Err(AuthError::Forbidden);
    }
    let existing = db::one(
        &tx,
        "SELECT active FROM neev_memberships WHERE org_id=$1 AND user_id=$2",
        vec![org_id.into(), identity.user_id.into()],
    )
    .await?;
    if let Some(member) = existing {
        let active: bool = member.try_get("", "active").map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        // Active = already a member (idempotent). Inactive = removed: self-service
        // must not resurrect a removed membership.
        return if active {
            Ok(org_id)
        } else {
            Err(AuthError::Forbidden)
        };
    }
    db::execute(
        &tx,
        "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,'member',1,true)",
        vec![org_id.into(), identity.user_id.into()],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(org_id)
}
