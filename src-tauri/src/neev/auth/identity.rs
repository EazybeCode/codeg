use super::email::canonical_email;
use crate::neev::{
    db,
    db::entities::Account,
    error::{database_error, AuthError},
    types::{Provider, SignInIntent, UserId, VerifiedIdentity},
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, FromQueryResult,
};

pub(crate) async fn resolve(
    db: &DatabaseConnection,
    mut proof: VerifiedIdentity,
    intent: SignInIntent,
    now: i64,
) -> Result<UserId, AuthError> {
    validate(&mut proof, now)?;
    let tx = db::begin(db).await?;
    let user_id = match intent {
        SignInIntent::Login => {
            if let Some(user_id) = subject_owner(&tx, &proof).await? {
                lock_account(&tx, user_id).await?;
                user_id
            } else {
                // Do not create an orphan account when another identity owns email.
                if let Some(email) = &proof.verified_email {
                    if email_owner(&tx, email).await?.is_some() {
                        return Err(AuthError::Conflict);
                    }
                }
                let row = db::one(&tx,
                    "INSERT INTO neev_accounts(display_name,avatar_url,auth_revision,disabled,created_at) VALUES($1,$2,1,false,$3) RETURNING id",
                    vec![proof.display_name.clone().into(), proof.avatar_url.clone().into(), now.into()]).await?.ok_or(AuthError::Unavailable)?;
                let id = row.try_get::<i32>("", "id").map_err(database_error)?;
                add_subject(&tx, &proof, id, now).await?;
                id
            }
        }
        SignInIntent::Link {
            user_id,
            session_id,
            auth_revision,
        } => {
            super::session::recheck_link(&tx, user_id, &session_id, auth_revision, now).await?;
            match subject_owner(&tx, &proof).await? {
                Some(other) if other != user_id => return Err(AuthError::Conflict),
                Some(_) => {}
                None => add_subject(&tx, &proof, user_id, now).await?,
            }
            user_id
        }
    };
    if let Some(email) = &proof.verified_email {
        match email_owner(&tx, email).await? {
            Some(other) if other != user_id => return Err(AuthError::Conflict),
            Some(_) => {}
            None => {
                db::execute(&tx, "INSERT INTO neev_verified_emails(user_id,canonical_email,verified_at) VALUES($1,$2,$3)", vec![user_id.into(),email.clone().into(),now.into()]).await?;
            }
        }
    }
    tx.commit().await.map_err(database_error)?;
    Ok(user_id)
}

async fn lock_account(tx: &DatabaseTransaction, id: i32) -> Result<Account, AuthError> {
    if id <= 0 {
        return Err(AuthError::Unauthorized);
    }
    let lock = if tx.get_database_backend() == DbBackend::Postgres {
        " FOR UPDATE"
    } else {
        ""
    };
    let row = db::one(
        tx,
        &format!("SELECT id,auth_revision,disabled FROM neev_accounts WHERE id=$1{lock}"),
        vec![id.into()],
    )
    .await?
    .ok_or(AuthError::Unauthorized)?;
    let account = Account::from_query_result(&row, "").map_err(database_error)?;
    if account.disabled || account.id <= 0 || account.auth_revision < 1 {
        return Err(AuthError::Unauthorized);
    }
    Ok(account)
}

async fn subject_owner(
    tx: &DatabaseTransaction,
    proof: &VerifiedIdentity,
) -> Result<Option<i32>, AuthError> {
    db::one(
        tx,
        "SELECT user_id FROM neev_identities WHERE provider=$1 AND issuer=$2 AND subject=$3",
        vec![
            provider_name(proof.provider).into(),
            proof.issuer.clone().into(),
            proof.subject.clone().into(),
        ],
    )
    .await?
    .map(|r| r.try_get("", "user_id").map_err(database_error))
    .transpose()
}
async fn email_owner(tx: &DatabaseTransaction, email: &str) -> Result<Option<i32>, AuthError> {
    db::one(
        tx,
        "SELECT user_id FROM neev_verified_emails WHERE canonical_email=$1",
        vec![email.into()],
    )
    .await?
    .map(|r| r.try_get("", "user_id").map_err(database_error))
    .transpose()
}
async fn add_subject(
    tx: &DatabaseTransaction,
    proof: &VerifiedIdentity,
    id: i32,
    now: i64,
) -> Result<(), AuthError> {
    db::execute(tx, "INSERT INTO neev_identities(user_id,provider,issuer,subject,created_at) VALUES($1,$2,$3,$4,$5)",vec![id.into(),provider_name(proof.provider).into(),proof.issuer.clone().into(),proof.subject.clone().into(),now.into()]).await?;
    Ok(())
}
fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "google",
        Provider::Github => "github",
        Provider::Email => "email",
    }
}
fn validate(proof: &mut VerifiedIdentity, now: i64) -> Result<(), AuthError> {
    if now < 0
        || proof.subject.is_empty()
        || proof.subject.len() > 255
        || proof.subject.chars().any(char::is_control)
    {
        return Err(AuthError::Invalid);
    }
    proof.display_name = proof.display_name.trim().to_owned();
    if proof.display_name.is_empty()
        || proof.display_name.chars().count() > 200
        || proof.display_name.chars().any(char::is_control)
    {
        return Err(AuthError::Invalid);
    }
    proof.verified_email = proof
        .verified_email
        .as_deref()
        .map(|s| canonical_email(s).ok_or(AuthError::Invalid))
        .transpose()?;
    let issuer = match proof.provider {
        Provider::Google => "https://accounts.google.com",
        Provider::Github => "https://github.com",
        Provider::Email => "https://app.neev.so",
    };
    if proof.issuer != issuer {
        return Err(AuthError::Invalid);
    }
    if proof.provider == Provider::Github
        && (proof.subject.starts_with('0')
            || !proof.subject.bytes().all(|b| b.is_ascii_digit())
            || proof
                .subject
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .is_none())
    {
        return Err(AuthError::Invalid);
    }
    if proof.provider == Provider::Email
        && proof.verified_email.as_deref() != Some(proof.subject.as_str())
    {
        return Err(AuthError::Invalid);
    }
    if let Some(avatar) = &proof.avatar_url {
        if avatar.len() > 2048
            || !avatar.starts_with("https://")
            || avatar.chars().any(char::is_control)
        {
            return Err(AuthError::Invalid);
        }
    }
    Ok(())
}
