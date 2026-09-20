//! Additive schema8 gateway storage plus a clock-independent, bounded-memory
//! read-only validator. Stores historical issuer facts only; introduces no
//! issuer, revocation, cleanup or request-admission runtime. The initializer
//! owns the transaction and publishes marker8 after the whole chain.
use super::{execute, one, statement, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 8;
pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_gateway_audience",
    "neev_gateway_relying_bindings",
    "neev_gateway_exchanges",
    "neev_gateway_relying_sessions",
    "neev_gateway_redemptions",
    "neev_gateway_revocations",
    "neev_gateway_revoke_intents",
];

const AUDIENCE: &str = "neev-collaboration-v1";
const ISSUER: &str = "neev-native-v1";
const NATIVE_MAX: i64 = 2_147_483_647;
const BIGINT_MAX: i64 = 9_223_372_036_854_775_807;
const TIME_MAX: i64 = 253_402_300_799;
const ISSUED_MAX: i64 = 253_402_300_439;
const REDEEMED_MAX: i64 = 253_402_300_499;
const PAGE: usize = 256;
const MAX_EXCHANGES: i64 = 10_000;
const MAX_RECEIPTS: i64 = 10_000;
const MAX_LIVE_AUTHORITY: i64 = 100_000;

// Backend-specific fixed column-domain fragments; identifiers are literal.
fn hex64(backend: DbBackend, col: &str) -> String {
    if backend == DbBackend::Postgres {
        format!("{col} ~ '^[0-9a-f]{{64}}$'")
    } else {
        format!("length({col})=64 AND {col} NOT GLOB '*[^0-9a-f]*'")
    }
}
fn octet(backend: DbBackend, col: &str, lo: i64, hi: i64) -> String {
    if backend == DbBackend::Postgres {
        format!("octet_length({col}) BETWEEN {lo} AND {hi}")
    } else {
        format!("length(CAST({col} AS BLOB)) BETWEEN {lo} AND {hi}")
    }
}

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let backend = tx.get_database_backend();
    let sqlite = backend == DbBackend::Sqlite;
    // Storage-type guards fold into each column's named CHECK; PostgreSQL relies
    // on its column types for the same enforcement.
    let ti = |c: &str| {
        if sqlite {
            format!("typeof({c})='integer' AND ")
        } else {
            String::new()
        }
    };
    let tt = |c: &str| {
        if sqlite {
            format!("typeof({c})='text' AND ")
        } else {
            String::new()
        }
    };
    let tni = |c: &str| {
        if sqlite {
            format!("({c} IS NULL OR typeof({c})='integer') AND ")
        } else {
            String::new()
        }
    };
    let tnt = |c: &str| {
        if sqlite {
            format!("({c} IS NULL OR typeof({c})='text') AND ")
        } else {
            String::new()
        }
    };

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_audience (
                audience TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_gateway_audience_audience CHECK ({audience_t}audience='{AUDIENCE}'),
                issuer TEXT NOT NULL CONSTRAINT ck_neev_gateway_audience_issuer CHECK ({issuer_t}issuer='{ISSUER}'),
                last_cursor BIGINT NOT NULL CONSTRAINT ck_neev_gateway_audience_last_cursor CHECK ({last_t}last_cursor BETWEEN 0 AND {BIGINT_MAX}),
                fetched_through BIGINT NOT NULL CONSTRAINT ck_neev_gateway_audience_fetched_through CHECK ({fetched_t}fetched_through BETWEEN 0 AND {BIGINT_MAX}),
                acknowledged_cursor BIGINT NOT NULL CONSTRAINT ck_neev_gateway_audience_acknowledged_cursor CHECK ({ack_t}acknowledged_cursor BETWEEN 0 AND {BIGINT_MAX}),
                writer_epoch BIGINT NOT NULL CONSTRAINT ck_neev_gateway_audience_writer_epoch CHECK ({epoch_t}writer_epoch BETWEEN 0 AND {BIGINT_MAX}),
                CONSTRAINT ck_neev_gateway_audience_cursor_order CHECK (acknowledged_cursor <= fetched_through AND fetched_through <= last_cursor),
                CONSTRAINT ck_neev_gateway_audience_epoch_ack CHECK (writer_epoch > 0 OR acknowledged_cursor = 0)
            )",
            audience_t = tt("audience"),
            issuer_t = tt("issuer"),
            last_t = ti("last_cursor"),
            fetched_t = ti("fetched_through"),
            ack_t = ti("acknowledged_cursor"),
            epoch_t = ti("writer_epoch"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_relying_bindings (
                relying_ref TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_gateway_relying_bindings_relying_ref CHECK ({ref_t}length(relying_ref)=36),
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_bindings_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                source_session_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_bindings_source_session_id CHECK ({src_t}length(source_session_id)=36),
                user_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_relying_bindings_user_id CHECK ({user_t}user_id BETWEEN 1 AND {NATIVE_MAX}),
                org_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_relying_bindings_org_id CHECK ({org_t}org_id BETWEEN 1 AND {NATIVE_MAX}),
                created_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_bindings_created_at CHECK ({created_t}created_at BETWEEN 0 AND {TIME_MAX}),
                CONSTRAINT ck_neev_gateway_relying_bindings_distinct CHECK (relying_ref <> source_session_id),
                CONSTRAINT uq_neev_gateway_relying_bindings_ref_audience UNIQUE (relying_ref,audience),
                CONSTRAINT fk_neev_gateway_relying_bindings_audience FOREIGN KEY (audience) REFERENCES neev_gateway_audience(audience) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_relying_bindings_session FOREIGN KEY (source_session_id) REFERENCES neev_sessions(id) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            ref_t = tt("relying_ref"),
            aud_t = tt("audience"),
            src_t = tt("source_session_id"),
            user_t = ti("user_id"),
            org_t = ti("org_id"),
            created_t = ti("created_at"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_exchanges (
                code_hash TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_gateway_exchanges_code_hash CHECK ({hash_t}{hash}),
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                attempt_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_attempt_id CHECK ({attempt_t}length(attempt_id)=43),
                challenge TEXT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_challenge CHECK ({challenge_t}length(challenge)=43),
                source_session_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_source_session_id CHECK ({src_t}length(source_session_id)=36),
                user_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_exchanges_user_id CHECK ({user_t}user_id BETWEEN 1 AND {NATIVE_MAX}),
                org_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_exchanges_org_id CHECK ({org_t}org_id BETWEEN 1 AND {NATIVE_MAX}),
                member_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_exchanges_member_id CHECK ({member_t}member_id BETWEEN 1 AND {NATIVE_MAX}),
                account_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_account_revision CHECK ({acct_t}account_revision BETWEEN 1 AND {BIGINT_MAX}),
                org_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_org_revision CHECK ({orgr_t}org_revision BETWEEN 1 AND {BIGINT_MAX}),
                member_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_member_revision CHECK ({memr_t}member_revision BETWEEN 1 AND {BIGINT_MAX}),
                role TEXT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_role CHECK ({role_t}role IN ('admin','member')),
                authenticated_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_authenticated_at CHECK ({authn_t}authenticated_at BETWEEN 0 AND {TIME_MAX}),
                issued_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_issued_at CHECK ({issued_t}issued_at BETWEEN 0 AND {ISSUED_MAX}),
                code_expires_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_code_expires_at CHECK ({expires_t}code_expires_at BETWEEN 0 AND {TIME_MAX}),
                retain_until BIGINT NOT NULL CONSTRAINT ck_neev_gateway_exchanges_retain_until CHECK ({retain_t}retain_until = issued_at+360),
                consumed_at BIGINT CONSTRAINT ck_neev_gateway_exchanges_consumed_at CHECK ({consumed_t}(consumed_at IS NULL OR (issued_at <= consumed_at AND consumed_at < code_expires_at))),
                CONSTRAINT ck_neev_gateway_exchanges_authn_order CHECK (authenticated_at <= issued_at),
                CONSTRAINT ck_neev_gateway_exchanges_expiry_window CHECK (issued_at < code_expires_at AND code_expires_at <= issued_at+60),
                CONSTRAINT uq_neev_gateway_exchanges_audience_attempt UNIQUE (audience,attempt_id),
                CONSTRAINT uq_neev_gateway_exchanges_code_audience UNIQUE (code_hash,audience),
                CONSTRAINT fk_neev_gateway_exchanges_audience FOREIGN KEY (audience) REFERENCES neev_gateway_audience(audience) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_exchanges_session FOREIGN KEY (source_session_id) REFERENCES neev_sessions(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_exchanges_membership FOREIGN KEY (member_id,org_id,user_id) REFERENCES neev_memberships(id,org_id,user_id) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            hash = hex64(backend, "code_hash"),
            hash_t = tt("code_hash"),
            aud_t = tt("audience"),
            attempt_t = tt("attempt_id"),
            challenge_t = tt("challenge"),
            src_t = tt("source_session_id"),
            user_t = ti("user_id"),
            org_t = ti("org_id"),
            member_t = ti("member_id"),
            acct_t = ti("account_revision"),
            orgr_t = ti("org_revision"),
            memr_t = ti("member_revision"),
            role_t = tt("role"),
            authn_t = ti("authenticated_at"),
            issued_t = ti("issued_at"),
            expires_t = ti("code_expires_at"),
            retain_t = ti("retain_until"),
            consumed_t = tni("consumed_at"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_relying_sessions (
                relying_ref TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_gateway_relying_sessions_relying_ref CHECK ({ref_t}length(relying_ref)=36),
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                issuer TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_issuer CHECK ({iss_t}issuer='{ISSUER}'),
                source_session_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_source_session_id CHECK ({src_t}length(source_session_id)=36),
                user_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_user_id CHECK ({user_t}user_id BETWEEN 1 AND {NATIVE_MAX}),
                org_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_org_id CHECK ({org_t}org_id BETWEEN 1 AND {NATIVE_MAX}),
                member_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_member_id CHECK ({member_t}member_id BETWEEN 1 AND {NATIVE_MAX}),
                account_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_account_revision CHECK ({acct_t}account_revision BETWEEN 1 AND {BIGINT_MAX}),
                org_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_org_revision CHECK ({orgr_t}org_revision BETWEEN 1 AND {BIGINT_MAX}),
                member_revision BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_member_revision CHECK ({memr_t}member_revision BETWEEN 1 AND {BIGINT_MAX}),
                role TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_role CHECK ({role_t}role IN ('admin','member')),
                authenticated_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_authenticated_at CHECK ({authn_t}authenticated_at BETWEEN 0 AND {TIME_MAX}),
                display_name TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_display_name CHECK ({dn_t}{display}),
                org_name TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_org_name CHECK ({on_t}{orgname}),
                verified_email TEXT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_verified_email CHECK ({ve_t}{email}),
                created_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_created_at CHECK ({created_t}created_at BETWEEN 0 AND {TIME_MAX}),
                expires_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_expires_at CHECK ({exp_t}expires_at BETWEEN 0 AND {TIME_MAX}),
                issued_after_cursor BIGINT NOT NULL CONSTRAINT ck_neev_gateway_relying_sessions_issued_after_cursor CHECK ({cur_t}issued_after_cursor BETWEEN 0 AND {BIGINT_MAX}),
                CONSTRAINT ck_neev_gateway_relying_sessions_distinct CHECK (relying_ref <> source_session_id),
                CONSTRAINT ck_neev_gateway_relying_sessions_lifetime CHECK (authenticated_at <= created_at AND created_at < expires_at),
                CONSTRAINT uq_neev_gateway_relying_sessions_ref_audience UNIQUE (relying_ref,audience),
                CONSTRAINT fk_neev_gateway_relying_sessions_audience FOREIGN KEY (audience) REFERENCES neev_gateway_audience(audience) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_relying_sessions_session FOREIGN KEY (source_session_id) REFERENCES neev_sessions(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_relying_sessions_membership FOREIGN KEY (member_id,org_id,user_id) REFERENCES neev_memberships(id,org_id,user_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_relying_sessions_binding FOREIGN KEY (relying_ref,audience) REFERENCES neev_gateway_relying_bindings(relying_ref,audience) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            ref_t = tt("relying_ref"),
            aud_t = tt("audience"),
            iss_t = tt("issuer"),
            src_t = tt("source_session_id"),
            user_t = ti("user_id"),
            org_t = ti("org_id"),
            member_t = ti("member_id"),
            acct_t = ti("account_revision"),
            orgr_t = ti("org_revision"),
            memr_t = ti("member_revision"),
            role_t = tt("role"),
            authn_t = ti("authenticated_at"),
            dn_t = tt("display_name"),
            display = octet(backend, "display_name", 1, 256),
            on_t = tt("org_name"),
            orgname = octet(backend, "org_name", 1, 256),
            ve_t = tt("verified_email"),
            email = octet(backend, "verified_email", 1, 254),
            created_t = ti("created_at"),
            exp_t = ti("expires_at"),
            cur_t = ti("issued_after_cursor"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_redemptions (
                receipt_id TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_gateway_redemptions_receipt_id CHECK ({rid_t}length(receipt_id)=36),
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_redemptions_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                code_hash TEXT NOT NULL CONSTRAINT ck_neev_gateway_redemptions_code_hash CHECK ({hash_t}{hash}),
                relying_ref TEXT NOT NULL CONSTRAINT ck_neev_gateway_redemptions_relying_ref CHECK ({ref_t}length(relying_ref)=36),
                redeemed_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_redemptions_redeemed_at CHECK ({red_t}redeemed_at BETWEEN 0 AND {REDEEMED_MAX}),
                retry_until BIGINT NOT NULL CONSTRAINT ck_neev_gateway_redemptions_retry_until CHECK ({retry_t}retry_until = redeemed_at+300),
                CONSTRAINT uq_neev_gateway_redemptions_code_hash UNIQUE (code_hash),
                CONSTRAINT uq_neev_gateway_redemptions_relying_ref UNIQUE (relying_ref),
                CONSTRAINT fk_neev_gateway_redemptions_exchange FOREIGN KEY (code_hash,audience) REFERENCES neev_gateway_exchanges(code_hash,audience) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_redemptions_relying FOREIGN KEY (relying_ref,audience) REFERENCES neev_gateway_relying_sessions(relying_ref,audience) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            rid_t = tt("receipt_id"),
            aud_t = tt("audience"),
            hash = hex64(backend, "code_hash"),
            hash_t = tt("code_hash"),
            ref_t = tt("relying_ref"),
            red_t = ti("redeemed_at"),
            retry_t = ti("retry_until"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_revocations (
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_revocations_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                cursor BIGINT NOT NULL CONSTRAINT ck_neev_gateway_revocations_cursor CHECK ({cur_t}cursor > 0 AND cursor <= {BIGINT_MAX}),
                event_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_revocations_event_id CHECK ({eid_t}length(event_id)=36),
                scope TEXT NOT NULL CONSTRAINT ck_neev_gateway_revocations_scope CHECK ({scope_t}scope IN ('session','account','org','membership')),
                user_id INTEGER CONSTRAINT ck_neev_gateway_revocations_user_id CHECK ({user_t}(user_id IS NULL OR user_id BETWEEN 1 AND {NATIVE_MAX})),
                org_id INTEGER CONSTRAINT ck_neev_gateway_revocations_org_id CHECK ({org_t}(org_id IS NULL OR org_id BETWEEN 1 AND {NATIVE_MAX})),
                source_session_id TEXT CONSTRAINT ck_neev_gateway_revocations_source_session_id CHECK ({src_t}(source_session_id IS NULL OR length(source_session_id)=36)),
                created_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_revocations_created_at CHECK ({created_t}created_at BETWEEN 0 AND {TIME_MAX}),
                CONSTRAINT ck_neev_gateway_revocations_scope_shape CHECK (
                    (scope='session' AND user_id IS NOT NULL AND org_id IS NOT NULL AND source_session_id IS NOT NULL)
                    OR (scope='account' AND user_id IS NOT NULL AND org_id IS NULL AND source_session_id IS NULL)
                    OR (scope='org' AND user_id IS NULL AND org_id IS NOT NULL AND source_session_id IS NULL)
                    OR (scope='membership' AND user_id IS NOT NULL AND org_id IS NOT NULL AND source_session_id IS NULL)
                ),
                CONSTRAINT pk_neev_gateway_revocations PRIMARY KEY (audience,cursor),
                CONSTRAINT uq_neev_gateway_revocations_event UNIQUE (event_id),
                CONSTRAINT uq_neev_gateway_revocations_audience_event_cursor UNIQUE (audience,event_id,cursor),
                CONSTRAINT fk_neev_gateway_revocations_audience FOREIGN KEY (audience) REFERENCES neev_gateway_audience(audience) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            aud_t = tt("audience"),
            cur_t = ti("cursor"),
            eid_t = tt("event_id"),
            scope_t = tt("scope"),
            user_t = tni("user_id"),
            org_t = tni("org_id"),
            src_t = tnt("source_session_id"),
            created_t = ti("created_at"),
        ),
        vec![],
    )
    .await?;

    execute(
        tx,
        &format!(
            "CREATE TABLE neev_gateway_revoke_intents (
                audience TEXT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_audience CHECK ({aud_t}audience='{AUDIENCE}'),
                intent_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_intent_id CHECK ({iid_t}length(intent_id)=36),
                relying_ref TEXT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_relying_ref CHECK ({ref_t}length(relying_ref)=36),
                source_session_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_source_session_id CHECK ({src_t}length(source_session_id)=36),
                user_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_user_id CHECK ({user_t}user_id BETWEEN 1 AND {NATIVE_MAX}),
                org_id INTEGER NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_org_id CHECK ({org_t}org_id BETWEEN 1 AND {NATIVE_MAX}),
                event_id TEXT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_event_id CHECK ({eid_t}length(event_id)=36),
                cursor BIGINT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_cursor CHECK ({cur_t}cursor BETWEEN 0 AND {BIGINT_MAX}),
                created_at BIGINT NOT NULL CONSTRAINT ck_neev_gateway_revoke_intents_created_at CHECK ({created_t}created_at BETWEEN 0 AND {TIME_MAX}),
                CONSTRAINT ck_neev_gateway_revoke_intents_shape CHECK (relying_ref <> source_session_id AND cursor > 0),
                CONSTRAINT pk_neev_gateway_revoke_intents PRIMARY KEY (audience,intent_id),
                CONSTRAINT fk_neev_gateway_revoke_intents_binding FOREIGN KEY (relying_ref,audience) REFERENCES neev_gateway_relying_bindings(relying_ref,audience) ON DELETE RESTRICT ON UPDATE RESTRICT,
                CONSTRAINT fk_neev_gateway_revoke_intents_event FOREIGN KEY (audience,event_id,cursor) REFERENCES neev_gateway_revocations(audience,event_id,cursor) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            aud_t = tt("audience"),
            iid_t = tt("intent_id"),
            ref_t = tt("relying_ref"),
            src_t = tt("source_session_id"),
            user_t = ti("user_id"),
            org_t = ti("org_id"),
            eid_t = tt("event_id"),
            cur_t = ti("cursor"),
            created_t = ti("created_at"),
        ),
        vec![],
    )
    .await?;

    for index in [
        "CREATE UNIQUE INDEX uq_neev_gateway_revocations_session ON neev_gateway_revocations(audience,source_session_id) WHERE scope='session'",
        "CREATE INDEX ix_neev_gateway_exchanges_source_expiry ON neev_gateway_exchanges(source_session_id,code_expires_at)",
        "CREATE INDEX ix_neev_gateway_exchanges_retention ON neev_gateway_exchanges(retain_until)",
        "CREATE INDEX ix_neev_gateway_relying_sessions_source_expiry ON neev_gateway_relying_sessions(source_session_id,expires_at)",
        "CREATE INDEX ix_neev_gateway_relying_sessions_expiry ON neev_gateway_relying_sessions(expires_at)",
        "CREATE INDEX ix_neev_gateway_redemptions_retry ON neev_gateway_redemptions(retry_until)",
        "CREATE INDEX ix_neev_gateway_revocations_scope ON neev_gateway_revocations(audience,scope,user_id,org_id,source_session_id)",
    ] {
        execute(tx, index, vec![]).await?;
    }

    execute(
        tx,
        "INSERT INTO neev_gateway_audience (audience,issuer,last_cursor,fetched_through,acknowledged_cursor,writer_epoch) VALUES ('neev-collaboration-v1','neev-native-v1',0,0,0,0)",
        vec![],
    )
    .await?;

    validate(tx).await
}

// ---- Private scalar predicates (display syntax only; mint no authority) ----

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version_num() == 4
            && id.get_variant() == uuid::Variant::RFC4122
            && id.to_string() == value
    })
}
fn canonical_token(value: &str) -> bool {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == value)
}
fn canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_display(name: &str, org: &str, email: &str) -> bool {
    use crate::neev::gateway::{GatewayEmail, GatewayName};
    name.len() <= 256
        && org.len() <= 256
        && email.len() <= 254
        && GatewayName::new(name.to_owned()).is_ok()
        && GatewayName::new(org.to_owned()).is_ok()
        && GatewayEmail::new(email.to_owned()).is_ok()
        && crate::neev::auth::email::canonical_email(email).as_deref() == Some(email)
}

// ---- Bounded page fetch (never accumulates the whole table) ----

#[cfg(test)]
pub(super) static MAX_PAGE_LEN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

async fn fetch_page<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    args: Vec<Value>,
) -> Result<Vec<QueryResult>, AuthError> {
    let rows = db
        .query_all(statement(db, sql, args))
        .await
        .map_err(super::database_error)?;
    if rows.len() > PAGE {
        return Err(AuthError::Unavailable);
    }
    #[cfg(test)]
    MAX_PAGE_LEN.fetch_max(rows.len(), std::sync::atomic::Ordering::SeqCst);
    Ok(rows)
}

async fn any<C: ConnectionTrait>(db: &C, sql: &str) -> Result<bool, AuthError> {
    Ok(one(db, sql, vec![]).await?.is_some())
}

// SQLite reports storage class per value; PostgreSQL enforces it via column type.
fn typed_i64(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    if sqlite
        && row
            .try_get::<String>("", &format!("{col}__t"))
            .map_err(super::database_error)?
            != "integer"
    {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<i64>("", col).map_err(super::database_error)
}
fn typed_str(row: &QueryResult, col: &str, sqlite: bool) -> Result<String, AuthError> {
    if sqlite
        && row
            .try_get::<String>("", &format!("{col}__t"))
            .map_err(super::database_error)?
            != "text"
    {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<String>("", col).map_err(super::database_error)
}
fn typed_opt_i64(row: &QueryResult, col: &str, sqlite: bool) -> Result<Option<i64>, AuthError> {
    if sqlite
        && !matches!(
            row.try_get::<String>("", &format!("{col}__t"))
                .map_err(super::database_error)?
                .as_str(),
            "null" | "integer"
        )
    {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<Option<i64>>("", col)
        .map_err(super::database_error)
}
// Native id columns are INTEGER: i32 on PostgreSQL, dynamically-typed on SQLite.
fn typed_native_id(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    if sqlite {
        typed_i64(row, col, true)
    } else {
        row.try_get::<i32>("", col)
            .map(i64::from)
            .map_err(super::database_error)
    }
}
fn typed_opt_native_id(row: &QueryResult, col: &str, sqlite: bool) -> Result<Option<i64>, AuthError> {
    if sqlite {
        typed_opt_i64(row, col, true)
    } else {
        Ok(row
            .try_get::<Option<i32>>("", col)
            .map_err(super::database_error)?
            .map(i64::from))
    }
}
fn typed_opt_str(row: &QueryResult, col: &str, sqlite: bool) -> Result<Option<String>, AuthError> {
    if sqlite
        && !matches!(
            row.try_get::<String>("", &format!("{col}__t"))
                .map_err(super::database_error)?
                .as_str(),
            "null" | "text"
        )
    {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<Option<String>>("", col)
        .map_err(super::database_error)
}

fn type_cols(sqlite: bool, cols: &[&str]) -> String {
    cols.iter()
        .map(|col| {
            if sqlite {
                format!("{col},typeof({col}) AS {col}__t")
            } else {
                (*col).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let sqlite = match db.get_database_backend() {
        DbBackend::Sqlite => true,
        DbBackend::Postgres => false,
        _ => return Err(AuthError::Unavailable),
    };

    // Phase 1: audience identity, at most two rows, cursor/epoch bounds.
    let last_cursor = validate_audience(db, sqlite).await?;

    // Phase 1 + 3: bounded scalar/type/domain scan of every table.
    validate_bindings_rows(db, sqlite).await?;
    validate_exchanges_rows(db, sqlite, last_cursor).await?;
    validate_relying_rows(db, sqlite, last_cursor).await?;
    validate_redemptions_rows(db, sqlite).await?;
    validate_revocation_rows(db, sqlite, last_cursor).await?;
    validate_intent_rows(db, sqlite).await?;

    // Phase 2: independent uniqueness of every declared primary/unique key.
    for query in [
        "SELECT 1 AS invalid FROM neev_gateway_relying_bindings GROUP BY relying_ref HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_relying_bindings GROUP BY relying_ref,audience HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_exchanges GROUP BY code_hash HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_exchanges GROUP BY audience,attempt_id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_exchanges GROUP BY code_hash,audience HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions GROUP BY relying_ref HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions GROUP BY relying_ref,audience HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_redemptions GROUP BY receipt_id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_redemptions GROUP BY code_hash HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_redemptions GROUP BY relying_ref HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_revocations GROUP BY audience,cursor HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_revocations GROUP BY event_id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_revocations WHERE scope='session' GROUP BY audience,source_session_id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_gateway_revoke_intents GROUP BY audience,intent_id HAVING COUNT(*)<>1 LIMIT 1",
    ] {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }

    // Phase 5: exhaustive joined invariants and bounded count caps.
    validate_joins(db).await?;
    validate_counts(db).await?;
    Ok(())
}

async fn validate_audience<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<i64, AuthError> {
    let cols = [
        "audience",
        "issuer",
        "last_cursor",
        "fetched_through",
        "acknowledged_cursor",
        "writer_epoch",
    ];
    let rows = db
        .query_all(statement(
            db,
            &format!(
                "SELECT {} FROM neev_gateway_audience LIMIT 2",
                type_cols(sqlite, &cols)
            ),
            vec![],
        ))
        .await
        .map_err(super::database_error)?;
    if rows.len() != 1 {
        return Err(AuthError::Unavailable);
    }
    let row = &rows[0];
    let audience = typed_str(row, "audience", sqlite)?;
    let issuer = typed_str(row, "issuer", sqlite)?;
    let last_cursor = typed_i64(row, "last_cursor", sqlite)?;
    let fetched_through = typed_i64(row, "fetched_through", sqlite)?;
    let acknowledged_cursor = typed_i64(row, "acknowledged_cursor", sqlite)?;
    let writer_epoch = typed_i64(row, "writer_epoch", sqlite)?;
    if audience != AUDIENCE
        || issuer != ISSUER
        || !(0..=BIGINT_MAX).contains(&last_cursor)
        || !(0..=BIGINT_MAX).contains(&fetched_through)
        || !(0..=BIGINT_MAX).contains(&acknowledged_cursor)
        || !(0..=BIGINT_MAX).contains(&writer_epoch)
        || acknowledged_cursor > fetched_through
        || fetched_through > last_cursor
        || (writer_epoch == 0 && acknowledged_cursor != 0)
    {
        return Err(AuthError::Unavailable);
    }
    Ok(last_cursor)
}

async fn validate_bindings_rows<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let cols = ["relying_ref", "audience", "source_session_id", "user_id", "org_id", "created_at"];
    let select = type_cols(sqlite, &cols);
    let mut cursor: Option<String> = None;
    loop {
        let rows = match &cursor {
            None => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_relying_bindings ORDER BY relying_ref LIMIT {PAGE}"),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_relying_bindings WHERE relying_ref > $1 ORDER BY relying_ref LIMIT {PAGE}"),
                    vec![key.clone().into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let relying_ref = typed_str(row, "relying_ref", sqlite)?;
            let audience = typed_str(row, "audience", sqlite)?;
            let source = typed_str(row, "source_session_id", sqlite)?;
            let user_id = typed_native_id(row, "user_id", sqlite)?;
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            if !canonical_uuid(&relying_ref)
                || audience != AUDIENCE
                || !canonical_uuid(&source)
                || relying_ref == source
                || !(1..=NATIVE_MAX).contains(&user_id)
                || !(1..=NATIVE_MAX).contains(&org_id)
                || !(0..=TIME_MAX).contains(&created_at)
            {
                return Err(AuthError::Unavailable);
            }
        }
        cursor = Some(typed_str(rows.last().unwrap(), "relying_ref", sqlite)?);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

async fn validate_exchanges_rows<C: ConnectionTrait>(
    db: &C,
    sqlite: bool,
    _last_cursor: i64,
) -> Result<(), AuthError> {
    let cols = [
        "code_hash", "audience", "attempt_id", "challenge", "source_session_id", "user_id",
        "org_id", "member_id", "account_revision", "org_revision", "member_revision", "role",
        "authenticated_at", "issued_at", "code_expires_at", "retain_until", "consumed_at",
    ];
    let select = type_cols(sqlite, &cols);
    let mut cursor: Option<String> = None;
    loop {
        let rows = match &cursor {
            None => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_exchanges ORDER BY code_hash LIMIT {PAGE}"),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_exchanges WHERE code_hash > $1 ORDER BY code_hash LIMIT {PAGE}"),
                    vec![key.clone().into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let code_hash = typed_str(row, "code_hash", sqlite)?;
            let audience = typed_str(row, "audience", sqlite)?;
            let attempt_id = typed_str(row, "attempt_id", sqlite)?;
            let challenge = typed_str(row, "challenge", sqlite)?;
            let source = typed_str(row, "source_session_id", sqlite)?;
            let user_id = typed_native_id(row, "user_id", sqlite)?;
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let member_id = typed_native_id(row, "member_id", sqlite)?;
            let account_revision = typed_i64(row, "account_revision", sqlite)?;
            let org_revision = typed_i64(row, "org_revision", sqlite)?;
            let member_revision = typed_i64(row, "member_revision", sqlite)?;
            let role = typed_str(row, "role", sqlite)?;
            let authenticated_at = typed_i64(row, "authenticated_at", sqlite)?;
            let issued_at = typed_i64(row, "issued_at", sqlite)?;
            let code_expires_at = typed_i64(row, "code_expires_at", sqlite)?;
            let retain_until = typed_i64(row, "retain_until", sqlite)?;
            let consumed_at = typed_opt_i64(row, "consumed_at", sqlite)?;
            let times_ok = (0..=ISSUED_MAX).contains(&issued_at)
                && (0..=TIME_MAX).contains(&authenticated_at)
                && (0..=TIME_MAX).contains(&code_expires_at)
                && authenticated_at <= issued_at
                && issued_at < code_expires_at
                && code_expires_at <= issued_at + 60
                && retain_until == issued_at + 360
                && consumed_at.is_none_or(|value| issued_at <= value && value < code_expires_at);
            if !canonical_hash(&code_hash)
                || audience != AUDIENCE
                || !canonical_token(&attempt_id)
                || !canonical_token(&challenge)
                || !canonical_uuid(&source)
                || !(1..=NATIVE_MAX).contains(&user_id)
                || !(1..=NATIVE_MAX).contains(&org_id)
                || !(1..=NATIVE_MAX).contains(&member_id)
                || !(1..=BIGINT_MAX).contains(&account_revision)
                || !(1..=BIGINT_MAX).contains(&org_revision)
                || !(1..=BIGINT_MAX).contains(&member_revision)
                || !matches!(role.as_str(), "admin" | "member")
                || !times_ok
            {
                return Err(AuthError::Unavailable);
            }
        }
        cursor = Some(typed_str(rows.last().unwrap(), "code_hash", sqlite)?);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

async fn validate_relying_rows<C: ConnectionTrait>(
    db: &C,
    sqlite: bool,
    last_cursor: i64,
) -> Result<(), AuthError> {
    let cols = [
        "relying_ref", "audience", "issuer", "source_session_id", "user_id", "org_id", "member_id",
        "account_revision", "org_revision", "member_revision", "role", "authenticated_at",
        "display_name", "org_name", "verified_email", "created_at", "expires_at",
        "issued_after_cursor",
    ];
    let select = type_cols(sqlite, &cols);
    let mut cursor: Option<String> = None;
    loop {
        let rows = match &cursor {
            None => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_relying_sessions ORDER BY relying_ref LIMIT {PAGE}"),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_relying_sessions WHERE relying_ref > $1 ORDER BY relying_ref LIMIT {PAGE}"),
                    vec![key.clone().into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let relying_ref = typed_str(row, "relying_ref", sqlite)?;
            let audience = typed_str(row, "audience", sqlite)?;
            let issuer = typed_str(row, "issuer", sqlite)?;
            let source = typed_str(row, "source_session_id", sqlite)?;
            let user_id = typed_native_id(row, "user_id", sqlite)?;
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let member_id = typed_native_id(row, "member_id", sqlite)?;
            let account_revision = typed_i64(row, "account_revision", sqlite)?;
            let org_revision = typed_i64(row, "org_revision", sqlite)?;
            let member_revision = typed_i64(row, "member_revision", sqlite)?;
            let role = typed_str(row, "role", sqlite)?;
            let authenticated_at = typed_i64(row, "authenticated_at", sqlite)?;
            let display_name = typed_str(row, "display_name", sqlite)?;
            let org_name = typed_str(row, "org_name", sqlite)?;
            let verified_email = typed_str(row, "verified_email", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            let expires_at = typed_i64(row, "expires_at", sqlite)?;
            let issued_after_cursor = typed_i64(row, "issued_after_cursor", sqlite)?;
            if !canonical_uuid(&relying_ref)
                || audience != AUDIENCE
                || issuer != ISSUER
                || !canonical_uuid(&source)
                || relying_ref == source
                || !(1..=NATIVE_MAX).contains(&user_id)
                || !(1..=NATIVE_MAX).contains(&org_id)
                || !(1..=NATIVE_MAX).contains(&member_id)
                || !(1..=BIGINT_MAX).contains(&account_revision)
                || !(1..=BIGINT_MAX).contains(&org_revision)
                || !(1..=BIGINT_MAX).contains(&member_revision)
                || !matches!(role.as_str(), "admin" | "member")
                || !(0..=TIME_MAX).contains(&authenticated_at)
                || !(0..=TIME_MAX).contains(&created_at)
                || !(0..=TIME_MAX).contains(&expires_at)
                || authenticated_at > created_at
                || created_at >= expires_at
                || !(0..=BIGINT_MAX).contains(&issued_after_cursor)
                || issued_after_cursor > last_cursor
                || !valid_display(&display_name, &org_name, &verified_email)
            {
                return Err(AuthError::Unavailable);
            }
        }
        cursor = Some(typed_str(rows.last().unwrap(), "relying_ref", sqlite)?);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

async fn validate_redemptions_rows<C: ConnectionTrait>(
    db: &C,
    sqlite: bool,
) -> Result<(), AuthError> {
    let cols = ["receipt_id", "audience", "code_hash", "relying_ref", "redeemed_at", "retry_until"];
    let select = type_cols(sqlite, &cols);
    let mut cursor: Option<String> = None;
    loop {
        let rows = match &cursor {
            None => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_redemptions ORDER BY receipt_id LIMIT {PAGE}"),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_redemptions WHERE receipt_id > $1 ORDER BY receipt_id LIMIT {PAGE}"),
                    vec![key.clone().into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let receipt_id = typed_str(row, "receipt_id", sqlite)?;
            let audience = typed_str(row, "audience", sqlite)?;
            let code_hash = typed_str(row, "code_hash", sqlite)?;
            let relying_ref = typed_str(row, "relying_ref", sqlite)?;
            let redeemed_at = typed_i64(row, "redeemed_at", sqlite)?;
            let retry_until = typed_i64(row, "retry_until", sqlite)?;
            if !canonical_uuid(&receipt_id)
                || audience != AUDIENCE
                || !canonical_hash(&code_hash)
                || !canonical_uuid(&relying_ref)
                || !(0..=REDEEMED_MAX).contains(&redeemed_at)
                || retry_until != redeemed_at + 300
            {
                return Err(AuthError::Unavailable);
            }
        }
        cursor = Some(typed_str(rows.last().unwrap(), "receipt_id", sqlite)?);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

// Bounded-memory: retains only the scalar `previous` cursor across pages; never
// accumulates a per-event collection regardless of retained history size.
async fn validate_revocation_rows<C: ConnectionTrait>(
    db: &C,
    sqlite: bool,
    last_cursor: i64,
) -> Result<(), AuthError> {
    let cols = ["audience", "cursor", "event_id", "scope", "user_id", "org_id", "source_session_id", "created_at"];
    let select = type_cols(sqlite, &cols);
    // Event cursor contiguity from 1..=last_cursor with fixed-scope shape.
    let mut previous: i64 = 0;
    loop {
        let rows = fetch_page(
            db,
            &format!(
                "SELECT {select} FROM neev_gateway_revocations WHERE cursor > $1 ORDER BY cursor LIMIT {PAGE}"
            ),
            vec![previous.into()],
        )
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let audience = typed_str(row, "audience", sqlite)?;
            let cursor = typed_i64(row, "cursor", sqlite)?;
            let event_id = typed_str(row, "event_id", sqlite)?;
            let scope = typed_str(row, "scope", sqlite)?;
            let user_id = typed_opt_native_id(row, "user_id", sqlite)?;
            let org_id = typed_opt_native_id(row, "org_id", sqlite)?;
            let source = typed_opt_str(row, "source_session_id", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            let scope_shape = match scope.as_str() {
                "session" => user_id.is_some() && org_id.is_some() && source.is_some(),
                "account" => user_id.is_some() && org_id.is_none() && source.is_none(),
                "org" => user_id.is_none() && org_id.is_some() && source.is_none(),
                "membership" => user_id.is_some() && org_id.is_some() && source.is_none(),
                _ => false,
            };
            let scoped_ids_ok = user_id.is_none_or(|value| (1..=NATIVE_MAX).contains(&value))
                && org_id.is_none_or(|value| (1..=NATIVE_MAX).contains(&value))
                && source.as_deref().is_none_or(canonical_uuid);
            if audience != AUDIENCE
                || cursor != previous.checked_add(1).ok_or(AuthError::Unavailable)?
                || !canonical_uuid(&event_id)
                || !scope_shape
                || !scoped_ids_ok
                || !(0..=TIME_MAX).contains(&created_at)
            {
                return Err(AuthError::Unavailable);
            }
            previous = cursor;
        }
        if rows.len() < PAGE {
            break;
        }
    }
    if previous != last_cursor {
        return Err(AuthError::Unavailable);
    }
    // Guard against zero/negative/non-integer cursors excluded by the keyset WHERE.
    if any(
        db,
        "SELECT 1 AS invalid FROM neev_gateway_revocations WHERE cursor IS NULL OR cursor <= 0 LIMIT 1",
    )
    .await?
    {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

async fn validate_intent_rows<C: ConnectionTrait>(
    db: &C,
    sqlite: bool,
) -> Result<(), AuthError> {
    let cols = ["audience", "intent_id", "relying_ref", "source_session_id", "user_id", "org_id", "event_id", "cursor", "created_at"];
    let select = type_cols(sqlite, &cols);
    let mut cursor: Option<String> = None;
    loop {
        let rows = match &cursor {
            None => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_revoke_intents ORDER BY intent_id LIMIT {PAGE}"),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_gateway_revoke_intents WHERE intent_id > $1 ORDER BY intent_id LIMIT {PAGE}"),
                    vec![key.clone().into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let audience = typed_str(row, "audience", sqlite)?;
            let intent_id = typed_str(row, "intent_id", sqlite)?;
            let relying_ref = typed_str(row, "relying_ref", sqlite)?;
            let source = typed_str(row, "source_session_id", sqlite)?;
            let user_id = typed_native_id(row, "user_id", sqlite)?;
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let event_id = typed_str(row, "event_id", sqlite)?;
            let event_cursor = typed_i64(row, "cursor", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            if audience != AUDIENCE
                || !canonical_uuid(&intent_id)
                || !canonical_uuid(&relying_ref)
                || !canonical_uuid(&source)
                || relying_ref == source
                || !(1..=NATIVE_MAX).contains(&user_id)
                || !(1..=NATIVE_MAX).contains(&org_id)
                || !canonical_uuid(&event_id)
                || event_cursor <= 0
                || !(0..=TIME_MAX).contains(&created_at)
            {
                return Err(AuthError::Unavailable);
            }
        }
        cursor = Some(typed_str(rows.last().unwrap(), "intent_id", sqlite)?);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

async fn validate_joins<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    // Each query returns a row only when an intrinsic invariant is violated.
    let invariants = [
        // Bindings: native source-session must exist with matching user/org.
        "SELECT 1 AS invalid FROM neev_gateway_relying_bindings b
         LEFT JOIN neev_sessions s ON s.id=b.source_session_id
         WHERE s.id IS NULL OR s.user_id<>b.user_id OR s.org_id IS NULL OR s.org_id<>b.org_id LIMIT 1",
        // Bindings: exactly one native source parent, including binding-only
        // history after expirable rows are pruned (R11).
        "SELECT 1 AS invalid FROM neev_gateway_relying_bindings b
         WHERE (SELECT COUNT(*) FROM neev_sessions s WHERE s.id=b.source_session_id)<>1 LIMIT 1",
        // Exchanges: matching native source-session (ids/revisions/authenticated_at) and code lifetime.
        "SELECT 1 AS invalid FROM neev_gateway_exchanges x
         LEFT JOIN neev_sessions s ON s.id=x.source_session_id
         WHERE s.id IS NULL OR s.user_id<>x.user_id OR s.org_id IS NULL OR s.org_id<>x.org_id
           OR s.member_id IS NULL OR s.member_id<>x.member_id
           OR s.account_revision<>x.account_revision OR s.org_revision IS NULL OR s.org_revision<>x.org_revision
           OR s.member_revision IS NULL OR s.member_revision<>x.member_revision
           OR s.authenticated_at<>x.authenticated_at OR x.code_expires_at>s.expires_at LIMIT 1",
        // Exchanges: composite membership parent must exist.
        "SELECT 1 AS invalid FROM neev_gateway_exchanges x
         LEFT JOIN neev_memberships m ON m.id=x.member_id AND m.org_id=x.org_id AND m.user_id=x.user_id
         WHERE m.id IS NULL LIMIT 1",
        // Exchanges: exactly one native source and composite membership parent
        // even when an imported snapshot lacks native uniqueness constraints (R11).
        "SELECT 1 AS invalid FROM neev_gateway_exchanges x
         WHERE (SELECT COUNT(*) FROM neev_sessions s WHERE s.id=x.source_session_id)<>1
            OR (SELECT COUNT(*) FROM neev_memberships m WHERE m.id=x.member_id AND m.org_id=x.org_id AND m.user_id=x.user_id)<>1 LIMIT 1",
        // Relying: matching native source-session, captured expiry equals source expiry.
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions r
         LEFT JOIN neev_sessions s ON s.id=r.source_session_id
         WHERE s.id IS NULL OR s.user_id<>r.user_id OR s.org_id IS NULL OR s.org_id<>r.org_id
           OR s.member_id IS NULL OR s.member_id<>r.member_id
           OR s.account_revision<>r.account_revision OR s.org_revision IS NULL OR s.org_revision<>r.org_revision
           OR s.member_revision IS NULL OR s.member_revision<>r.member_revision
           OR s.authenticated_at<>r.authenticated_at OR s.expires_at<>r.expires_at LIMIT 1",
        // Relying: composite membership parent must exist.
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions r
         LEFT JOIN neev_memberships m ON m.id=r.member_id AND m.org_id=r.org_id AND m.user_id=r.user_id
         WHERE m.id IS NULL LIMIT 1",
        // Relying: exactly one native source and composite membership parent (R11).
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions r
         WHERE (SELECT COUNT(*) FROM neev_sessions s WHERE s.id=r.source_session_id)<>1
            OR (SELECT COUNT(*) FROM neev_memberships m WHERE m.id=r.member_id AND m.org_id=r.org_id AND m.user_id=r.user_id)<>1 LIMIT 1",
        // Relying: its permanent binding must exist with identical source/user/org/created_at.
        "SELECT 1 AS invalid FROM neev_gateway_relying_sessions r
         LEFT JOIN neev_gateway_relying_bindings b ON b.relying_ref=r.relying_ref AND b.audience=r.audience
         WHERE b.relying_ref IS NULL OR b.source_session_id<>r.source_session_id
           OR b.user_id<>r.user_id OR b.org_id<>r.org_id OR b.created_at<>r.created_at LIMIT 1",
        // Redemption: matching consumed code + live relying, shared identity, exact receipt timings.
        "SELECT 1 AS invalid FROM neev_gateway_redemptions x
         LEFT JOIN neev_gateway_exchanges e ON e.code_hash=x.code_hash AND e.audience=x.audience
         LEFT JOIN neev_gateway_relying_sessions r ON r.relying_ref=x.relying_ref AND r.audience=x.audience
         LEFT JOIN neev_gateway_relying_bindings b ON b.relying_ref=x.relying_ref AND b.audience=x.audience
         WHERE e.code_hash IS NULL OR r.relying_ref IS NULL OR b.relying_ref IS NULL
           OR e.consumed_at IS NULL OR x.redeemed_at<>e.consumed_at
           OR x.redeemed_at<>r.created_at OR r.created_at<>b.created_at
           OR e.issued_at>x.redeemed_at OR x.redeemed_at>=e.code_expires_at
           OR x.retry_until>e.retain_until
           OR e.source_session_id<>r.source_session_id OR e.user_id<>r.user_id OR e.org_id<>r.org_id
           OR e.member_id<>r.member_id OR e.account_revision<>r.account_revision
           OR e.org_revision<>r.org_revision OR e.member_revision<>r.member_revision
           OR e.role<>r.role OR e.authenticated_at<>r.authenticated_at LIMIT 1",
        // Intent: matching permanent binding + exact session-scope event under the audience.
        "SELECT 1 AS invalid FROM neev_gateway_revoke_intents i
         LEFT JOIN neev_gateway_relying_bindings b ON b.relying_ref=i.relying_ref AND b.audience=i.audience
         LEFT JOIN neev_gateway_revocations e ON e.audience=i.audience AND e.event_id=i.event_id AND e.cursor=i.cursor
         WHERE b.relying_ref IS NULL OR e.event_id IS NULL
           OR b.source_session_id<>i.source_session_id OR b.user_id<>i.user_id OR b.org_id<>i.org_id
           OR e.scope<>'session' OR e.source_session_id IS NULL OR e.source_session_id<>i.source_session_id
           OR e.user_id IS NULL OR e.user_id<>i.user_id OR e.org_id IS NULL OR e.org_id<>i.org_id LIMIT 1",
    ];
    for query in invariants {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

async fn validate_counts<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for (query, cap) in [
        ("SELECT COUNT(*) AS n FROM neev_gateway_exchanges", MAX_EXCHANGES),
        ("SELECT COUNT(*) AS n FROM neev_gateway_redemptions", MAX_RECEIPTS),
        (
            "SELECT COUNT(*) AS n FROM neev_gateway_relying_sessions",
            MAX_LIVE_AUTHORITY,
        ),
    ] {
        let count = one(db, query, vec![])
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get::<i64>("", "n")
            .map_err(super::database_error)?;
        if count > cap {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}
