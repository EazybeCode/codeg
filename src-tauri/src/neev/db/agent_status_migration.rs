use super::{execute, one, statement, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};
use std::collections::BTreeSet;

pub(super) const SCHEMA_VERSION: i64 = 7;
pub(super) const TABLE_NAMES: &[&str] = &["neev_agent_status_owner", "neev_agent_status_checks"];

fn lowercase_hex(backend: DbBackend, value: &str) -> String {
    if backend == DbBackend::Postgres {
        format!("{value} ~ '^[0-9a-f]{{32}}$'")
    } else {
        format!("length({value})=32 AND {value} NOT GLOB '*[^0-9a-f]*'")
    }
}

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let backend = tx.get_database_backend();
    execute(
        tx,
        &format!(
            "CREATE TABLE neev_agent_status_owner (
                id INTEGER PRIMARY KEY CONSTRAINT ck_neev_agent_status_owner_id CHECK (id=1),
                epoch BIGINT NOT NULL CONSTRAINT ck_neev_agent_status_owner_epoch CHECK (epoch>=0),
                boot_id TEXT,
                phase TEXT NOT NULL CONSTRAINT ck_neev_agent_status_owner_phase CHECK (phase IN ('initializing','ready','closed')),
                CONSTRAINT ck_neev_agent_status_owner_boot_id CHECK (boot_id IS NULL OR {}),
                CONSTRAINT ck_neev_agent_status_owner_shape CHECK ((epoch=0 AND boot_id IS NULL AND phase='closed') OR (epoch>0 AND boot_id IS NOT NULL))
            )",
            lowercase_hex(backend, "boot_id")
        ),
        vec![],
    )
    .await?;
    execute(
        tx,
        &format!(
            "CREATE TABLE neev_agent_status_checks (
                connection_id TEXT NOT NULL PRIMARY KEY CONSTRAINT ck_neev_agent_status_checks_connection_id CHECK ({}),
                check_id TEXT NOT NULL CONSTRAINT ck_neev_agent_status_checks_check_id CHECK ({}),
                owner_epoch BIGINT NOT NULL CONSTRAINT ck_neev_agent_status_checks_owner_epoch CHECK (owner_epoch BETWEEN 1 AND 9223372036854775807),
                session_id TEXT NOT NULL CONSTRAINT ck_neev_agent_status_checks_session_id CHECK (length(session_id)=36),
                generation BIGINT NOT NULL CONSTRAINT ck_neev_agent_status_checks_generation CHECK (generation BETWEEN 1 AND 9223372036854775807),
                storage_handle TEXT NOT NULL CONSTRAINT ck_neev_agent_status_checks_storage_handle CHECK ({}),
                issued_at BIGINT NOT NULL CONSTRAINT ck_neev_agent_status_checks_issued_at CHECK (issued_at BETWEEN 0 AND 253402300739),
                expires_at BIGINT NOT NULL CONSTRAINT ck_neev_agent_status_checks_expires_at CHECK (expires_at BETWEEN 60 AND 253402300799),
                CONSTRAINT ck_neev_agent_status_checks_times CHECK (expires_at=issued_at+60),
                CONSTRAINT uq_neev_agent_status_checks_check_id UNIQUE (check_id),
                CONSTRAINT fk_neev_agent_status_checks_connection FOREIGN KEY (connection_id) REFERENCES neev_agent_connections(id) ON DELETE RESTRICT ON UPDATE RESTRICT
            )",
            lowercase_hex(backend, "connection_id"),
            lowercase_hex(backend, "check_id"),
            lowercase_hex(backend, "storage_handle")
        ),
        vec![],
    )
    .await?;
    execute(
        tx,
        "INSERT INTO neev_agent_status_owner(id,epoch,boot_id,phase) VALUES(1,0,NULL,'closed')",
        vec![],
    )
    .await?;
    validate(tx).await
}

fn canonical_hex(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_session(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| {
        parsed.to_string() == value
            && parsed.get_version() == Some(uuid::Version::Random)
            && parsed.get_variant() == uuid::Variant::RFC4122
    })
}

pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let (owner_sql, checks_sql) = match db.get_database_backend() {
        DbBackend::Sqlite => (
            "SELECT id,epoch,boot_id,phase,typeof(id) AS id_type,typeof(epoch) AS epoch_type,typeof(boot_id) AS boot_id_type,typeof(phase) AS phase_type FROM neev_agent_status_owner LIMIT 2",
            "SELECT connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at,typeof(connection_id) AS connection_id_type,typeof(check_id) AS check_id_type,typeof(owner_epoch) AS owner_epoch_type,typeof(session_id) AS session_id_type,typeof(generation) AS generation_type,typeof(storage_handle) AS storage_handle_type,typeof(issued_at) AS issued_at_type,typeof(expires_at) AS expires_at_type FROM neev_agent_status_checks ORDER BY connection_id LIMIT 65",
        ),
        DbBackend::Postgres => (
            "SELECT id,epoch,boot_id,phase,'integer' AS id_type,'integer' AS epoch_type,CASE WHEN boot_id IS NULL THEN 'null' ELSE 'text' END AS boot_id_type,'text' AS phase_type FROM neev_agent_status_owner LIMIT 2",
            "SELECT connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at,'text' AS connection_id_type,'text' AS check_id_type,'integer' AS owner_epoch_type,'text' AS session_id_type,'integer' AS generation_type,'text' AS storage_handle_type,'integer' AS issued_at_type,'integer' AS expires_at_type FROM neev_agent_status_checks ORDER BY connection_id LIMIT 65",
        ),
        _ => return Err(AuthError::Unavailable),
    };
    let owners = db
        .query_all(statement(db, owner_sql, vec![]))
        .await
        .map_err(super::database_error)?;
    if owners.len() != 1 {
        return Err(AuthError::Unavailable);
    }
    let owner = &owners[0];
    let owner_types_valid = owner
        .try_get::<String>("", "id_type")
        .map_err(super::database_error)?
        == "integer"
        && owner
            .try_get::<String>("", "epoch_type")
            .map_err(super::database_error)?
            == "integer"
        && matches!(
            owner
                .try_get::<String>("", "boot_id_type")
                .map_err(super::database_error)?
                .as_str(),
            "null" | "text"
        )
        && owner
            .try_get::<String>("", "phase_type")
            .map_err(super::database_error)?
            == "text";
    if !owner_types_valid {
        return Err(AuthError::Unavailable);
    }
    let id = owner
        .try_get::<i32>("", "id")
        .map_err(super::database_error)?;
    let epoch = owner
        .try_get::<i64>("", "epoch")
        .map_err(super::database_error)?;
    let boot_id = owner
        .try_get::<Option<String>>("", "boot_id")
        .map_err(super::database_error)?;
    let phase = owner
        .try_get::<String>("", "phase")
        .map_err(super::database_error)?;
    let owner_valid = id == 1
        && epoch >= 0
        && matches!(phase.as_str(), "initializing" | "ready" | "closed")
        && match (epoch, boot_id.as_deref(), phase.as_str()) {
            (0, None, "closed") => true,
            (1.., Some(boot), _) => canonical_hex(boot),
            _ => false,
        };
    if !owner_valid {
        return Err(AuthError::Unavailable);
    }

    let checks = db
        .query_all(statement(db, checks_sql, vec![]))
        .await
        .map_err(super::database_error)?;
    if checks.len() > 64 || (!checks.is_empty() && phase != "ready") {
        return Err(AuthError::Unavailable);
    }
    let mut connection_ids = BTreeSet::new();
    let mut check_ids = BTreeSet::new();
    for check in checks {
        for (column, expected) in [
            ("connection_id_type", "text"),
            ("check_id_type", "text"),
            ("owner_epoch_type", "integer"),
            ("session_id_type", "text"),
            ("generation_type", "integer"),
            ("storage_handle_type", "text"),
            ("issued_at_type", "integer"),
            ("expires_at_type", "integer"),
        ] {
            if check
                .try_get::<String>("", column)
                .map_err(super::database_error)?
                != expected
            {
                return Err(AuthError::Unavailable);
            }
        }
        let connection_id = check
            .try_get::<String>("", "connection_id")
            .map_err(super::database_error)?;
        let check_id = check
            .try_get::<String>("", "check_id")
            .map_err(super::database_error)?;
        let owner_epoch = check
            .try_get::<i64>("", "owner_epoch")
            .map_err(super::database_error)?;
        let session_id = check
            .try_get::<String>("", "session_id")
            .map_err(super::database_error)?;
        let generation = check
            .try_get::<i64>("", "generation")
            .map_err(super::database_error)?;
        let storage_handle = check
            .try_get::<String>("", "storage_handle")
            .map_err(super::database_error)?;
        let issued_at = check
            .try_get::<i64>("", "issued_at")
            .map_err(super::database_error)?;
        let expires_at = check
            .try_get::<i64>("", "expires_at")
            .map_err(super::database_error)?;
        if !canonical_hex(&connection_id)
            || !canonical_hex(&check_id)
            || !canonical_session(&session_id)
            || !canonical_hex(&storage_handle)
            || owner_epoch < 1
            || owner_epoch != epoch
            || generation < 1
            || !(0..=253_402_300_739).contains(&issued_at)
            || !(60..=253_402_300_799).contains(&expires_at)
            || issued_at.checked_add(60) != Some(expires_at)
            || !connection_ids.insert(connection_id)
            || !check_ids.insert(check_id)
        {
            return Err(AuthError::Unavailable);
        }
    }
    if one(
        db,
        "SELECT 1 AS invalid FROM neev_agent_status_checks s LEFT JOIN neev_agent_connections c ON c.id=s.connection_id WHERE c.id IS NULL LIMIT 1",
        vec![],
    )
    .await?
    .is_some()
    {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}
