use std::{collections::BTreeMap, net::IpAddr};

use hmac::{Hmac, Mac};
use sea_orm::{DatabaseConnection, DatabaseTransaction};
use sha2::Sha256;

use crate::neev::{
    db,
    error::{database_error, AuthError},
};

const ACTIVE_BUCKET_LIMIT: i64 = 10_000;
const CLEANUP_LIMIT: i64 = 256;

#[derive(Clone)]
pub(crate) enum Bucket {
    MagicAddress(String),
    MagicSourceIp(IpAddr),
    DeliveryMinute,
    DeliveryDay,
    InvitationActor(i32),
    InvitationOrganization(i32),
    OrganizationCreation(i32),
}

pub(crate) async fn reserve(
    database: &DatabaseConnection,
    key: &[u8; 32],
    buckets: &[Bucket],
    now: i64,
) -> Result<(), AuthError> {
    let tx = db::begin(database).await?;
    lock_guard(&tx).await?;
    reserve_in_transaction(&tx, key, buckets, now).await?;
    tx.commit().await.map_err(database_error)
}

pub(crate) async fn lock_guard(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    db::lock_admission_guard(tx).await
}

pub(crate) async fn reserve_in_transaction(
    tx: &DatabaseTransaction,
    key: &[u8; 32],
    buckets: &[Bucket],
    now: i64,
) -> Result<(), AuthError> {
    if now < 0 || buckets.is_empty() {
        return Err(AuthError::Invalid);
    }
    let mut requested = BTreeMap::new();
    for bucket in buckets {
        let descriptor = Descriptor::from_bucket(key, bucket, now)?;
        requested.insert(
            (
                descriptor.kind,
                descriptor.key_hash.clone(),
                descriptor.window_start,
            ),
            descriptor,
        );
    }

    db::execute(
        tx,
        "DELETE FROM neev_quota_buckets WHERE (kind,key_hash,window_start) IN (SELECT kind,key_hash,window_start FROM neev_quota_buckets WHERE expires_at<=$1 ORDER BY expires_at LIMIT $2)",
        vec![now.into(), CLEANUP_LIMIT.into()],
    )
    .await?;
    let active: i64 = db::one(
        tx,
        "SELECT COUNT(*) AS n FROM neev_quota_buckets WHERE expires_at>$1",
        vec![now.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "n")
    .map_err(database_error)?;
    if active >= ACTIVE_BUCKET_LIMIT {
        return Err(AuthError::Limited);
    }

    let mut new_rows = 0i64;
    for descriptor in requested.values() {
        let used = db::one(
            tx,
            "SELECT used FROM neev_quota_buckets WHERE kind=$1 AND key_hash=$2 AND window_start=$3",
            vec![
                descriptor.kind.into(),
                descriptor.key_hash.clone().into(),
                descriptor.window_start.into(),
            ],
        )
        .await?
        .map(|row| row.try_get::<i64>("", "used").map_err(database_error))
        .transpose()?;
        match used {
            Some(used) if used >= descriptor.limit => return Err(AuthError::Limited),
            Some(_) => {}
            None => new_rows += 1,
        }
    }
    if active.saturating_add(new_rows) > ACTIVE_BUCKET_LIMIT {
        return Err(AuthError::Limited);
    }
    for descriptor in requested.values() {
        let updated = db::execute(
            tx,
            "UPDATE neev_quota_buckets SET used=used+1 WHERE kind=$1 AND key_hash=$2 AND window_start=$3",
            vec![
                descriptor.kind.into(),
                descriptor.key_hash.clone().into(),
                descriptor.window_start.into(),
            ],
        )
        .await?;
        if updated.rows_affected() == 0 {
            db::execute(
                tx,
                "INSERT INTO neev_quota_buckets(kind,key_hash,window_start,used,expires_at) VALUES($1,$2,$3,1,$4)",
                vec![
                    descriptor.kind.into(),
                    descriptor.key_hash.clone().into(),
                    descriptor.window_start.into(),
                    descriptor.expires_at.into(),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

struct Descriptor {
    kind: &'static str,
    key_hash: String,
    window_start: i64,
    expires_at: i64,
    limit: i64,
}

impl Descriptor {
    fn from_bucket(key: &[u8; 32], bucket: &Bucket, now: i64) -> Result<Self, AuthError> {
        let (kind, value, window, limit) = match bucket {
            Bucket::MagicAddress(email) if !email.is_empty() && email.len() <= 254 => {
                ("magic_address_15m", email.clone(), 15 * 60, 3)
            }
            Bucket::MagicSourceIp(ip) => ("magic_ip_15m", ip.to_string(), 15 * 60, 20),
            Bucket::DeliveryMinute => ("delivery_minute", "global".into(), 60, 10),
            Bucket::DeliveryDay => ("delivery_day", "global".into(), 24 * 60 * 60, 80),
            Bucket::InvitationActor(id) if *id > 0 => {
                ("invitation_actor_day", id.to_string(), 24 * 60 * 60, 20)
            }
            Bucket::InvitationOrganization(id) if *id > 0 => {
                ("invitation_org_day", id.to_string(), 24 * 60 * 60, 80)
            }
            Bucket::OrganizationCreation(id) if *id > 0 => {
                ("organization_creation_day", id.to_string(), 24 * 60 * 60, 3)
            }
            _ => return Err(AuthError::Invalid),
        };
        let window_start = now - now.rem_euclid(window);
        let expires_at = window_start.checked_add(window).ok_or(AuthError::Invalid)?;
        Ok(Self {
            kind,
            key_hash: keyed_hash(key, kind, &value)?,
            window_start,
            expires_at,
            limit,
        })
    }
}

fn keyed_hash(key: &[u8; 32], kind: &str, value: &str) -> Result<String, AuthError> {
    let mut hmac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| AuthError::Unavailable)?;
    hmac.update(b"neev-quota-key-v1\0");
    hmac.update(kind.as_bytes());
    hmac.update(b"\0");
    hmac.update(value.as_bytes());
    Ok(format!("{:x}", hmac.finalize().into_bytes()))
}
