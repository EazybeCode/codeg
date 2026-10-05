//! Schema 18: Shared library for organization skills and MCP configurations.
//!
//! Adds `neev_library_items` and `neev_library_files`, enabling org admins to
//! upload SKILL.md files and MCP JSON configurations that are shared with all
//! current members and staged into each isolated agent run.
//!
//! ACL: org admins write, all members read. No secret exposure in listings.

use sea_orm::{ConnectionTrait, DbBackend};

use super::execute;
use crate::neev::error::AuthError;

pub(crate) const SCHEMA_VERSION: i64 = 18;

pub(crate) const TABLE_NAMES: &[&str] = &["neev_library_items", "neev_library_files"];

/// Upgrade from schema 17 → 18: add library tables.
pub(crate) async fn upgrade<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let backend = db.get_database_backend();
    // neev_library_items: each row is a skill or mcp configuration
    // Uses canonical 36-char hyphenated UUIDs
    let items_ddl = match backend {
        DbBackend::Postgres => {
            r#"
CREATE TABLE IF NOT EXISTS neev_library_items (
    id TEXT PRIMARY KEY CHECK (length(id)=36),
    org_id INTEGER NOT NULL REFERENCES neev_organizations(id),
    kind TEXT NOT NULL CHECK (kind IN ('skill', 'mcp')),
    name TEXT NOT NULL CHECK (length(name) >= 1 AND length(name) <= 200),
    description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    enabled BOOLEAN NOT NULL DEFAULT false,
    creator_id INTEGER NOT NULL REFERENCES neev_accounts(id),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    setup_note TEXT CHECK (setup_note IS NULL OR length(setup_note) <= 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#
        }
        DbBackend::Sqlite => {
            r#"
CREATE TABLE IF NOT EXISTS neev_library_items (
    id TEXT PRIMARY KEY CHECK (length(id)=36),
    org_id INTEGER NOT NULL REFERENCES neev_organizations(id),
    kind TEXT NOT NULL CHECK (kind IN ('skill', 'mcp')),
    name TEXT NOT NULL CHECK (length(name) >= 1 AND length(name) <= 200),
    description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    creator_id INTEGER NOT NULL REFERENCES neev_accounts(id),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    setup_note TEXT CHECK (setup_note IS NULL OR length(setup_note) <= 2000),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
)
"#
        }
        _ => return Err(AuthError::Unavailable),
    };
    execute(db, items_ddl, vec![]).await?;

    // Index for listing items by organization
    execute(
        db,
        "CREATE INDEX IF NOT EXISTS idx_library_items_org ON neev_library_items(org_id)",
        vec![],
    )
    .await?;

    // neev_library_files: files belonging to a library item (skill supporting files)
    // Uses canonical 36-char hyphenated UUIDs
    let files_ddl = match backend {
        DbBackend::Postgres => {
            r#"
CREATE TABLE IF NOT EXISTS neev_library_files (
    id TEXT PRIMARY KEY CHECK (length(id)=36),
    item_id TEXT NOT NULL REFERENCES neev_library_items(id) ON DELETE CASCADE,
    org_id INTEGER NOT NULL REFERENCES neev_organizations(id),
    path TEXT NOT NULL CHECK (length(path) >= 1 AND length(path) <= 500),
    content_hash TEXT NOT NULL CHECK (length(content_hash)=64),
    content BYTEA NOT NULL CHECK (octet_length(content) <= 1048576),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#
        }
        DbBackend::Sqlite => {
            r#"
CREATE TABLE IF NOT EXISTS neev_library_files (
    id TEXT PRIMARY KEY CHECK (length(id)=36),
    item_id TEXT NOT NULL REFERENCES neev_library_items(id) ON DELETE CASCADE,
    org_id INTEGER NOT NULL REFERENCES neev_organizations(id),
    path TEXT NOT NULL CHECK (length(path) >= 1 AND length(path) <= 500),
    content_hash TEXT NOT NULL CHECK (length(content_hash)=64),
    content BLOB NOT NULL CHECK (length(content) <= 1048576),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
)
"#
        }
        _ => return Err(AuthError::Unavailable),
    };
    execute(db, files_ddl, vec![]).await?;

    // Index for listing files by item
    execute(
        db,
        "CREATE INDEX IF NOT EXISTS idx_library_files_item ON neev_library_files(item_id)",
        vec![],
    )
    .await?;

    execute(
        db,
        "CREATE INDEX IF NOT EXISTS idx_library_files_org ON neev_library_files(org_id)",
        vec![],
    )
    .await?;

    // Unique constraint: one file per path per item
    execute(
        db,
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_library_files_item_path ON neev_library_files(item_id, path)",
        vec![],
    )
    .await?;

    Ok(())
}

/// Validate schema 18 structure without mutation.
pub(crate) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    use crate::neev::error::database_error;

    // Validate neev_library_items table structure
    let items_check = db
        .query_one(super::statement(
            db,
            "SELECT id, org_id, kind, name, description, enabled, creator_id, revision, setup_note, created_at, updated_at FROM neev_library_items LIMIT 0",
            vec![],
        ))
        .await
        .map_err(database_error);
    if items_check.is_err() {
        return Err(AuthError::Unavailable);
    }

    // Validate neev_library_files table structure
    let files_check = db
        .query_one(super::statement(
            db,
            "SELECT id, item_id, org_id, path, content_hash, content, created_at FROM neev_library_files LIMIT 0",
            vec![],
        ))
        .await
        .map_err(database_error);
    if files_check.is_err() {
        return Err(AuthError::Unavailable);
    }

    // Referential integrity is rechecked even when restoring a database whose
    // original constraints may have been absent or disabled.
    for sql in [
        "SELECT COUNT(*) AS broken FROM neev_library_items i LEFT JOIN neev_organizations o ON o.id=i.org_id LEFT JOIN neev_accounts a ON a.id=i.creator_id WHERE o.id IS NULL OR a.id IS NULL",
        "SELECT COUNT(*) AS broken FROM neev_library_files f LEFT JOIN neev_library_items i ON i.id=f.item_id AND i.org_id=f.org_id WHERE i.id IS NULL",
        "SELECT COUNT(*) AS broken FROM neev_library_items WHERE id IS NULL OR length(id)<>36 OR kind NOT IN ('skill','mcp') OR kind IS NULL OR name IS NULL OR length(name)<1 OR length(name)>200 OR description IS NULL OR length(description)>2000 OR revision IS NULL OR revision<1 OR enabled IS NULL OR enabled NOT IN (false,true) OR length(setup_note)>2000",
        "SELECT COUNT(*) AS broken FROM neev_library_files WHERE id IS NULL OR length(id)<>36 OR path IS NULL OR length(path)<1 OR length(path)>500 OR content IS NULL OR length(content)>1048576 OR content_hash IS NULL OR length(content_hash)<>64",
        "SELECT COUNT(*) AS broken FROM (SELECT id FROM neev_library_items GROUP BY id HAVING COUNT(*)>1) d",
        "SELECT COUNT(*) AS broken FROM (SELECT id FROM neev_library_files GROUP BY id HAVING COUNT(*)>1) d",
        "SELECT COUNT(*) AS broken FROM (SELECT item_id,path FROM neev_library_files GROUP BY item_id,path HAVING COUNT(*)>1) d",
    ] {
        let row = super::one(db, sql, vec![]).await?.ok_or(AuthError::Unavailable)?;
        if row.try_get::<i64>("", "broken").map_err(database_error)? != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    // Check actual PostgreSQL types; SQLite storage classes are verified by
    // decoding each row below as the exact Rust type it represents.
    if db.get_database_backend() == DbBackend::Postgres {
        let row = super::one(db, "SELECT COUNT(*) AS broken FROM information_schema.columns WHERE table_schema=current_schema() AND table_name IN ('neev_library_items','neev_library_files') AND ((column_name IN ('org_id','creator_id') AND data_type<>'integer') OR (column_name='revision' AND data_type<>'bigint') OR (column_name='enabled' AND data_type<>'boolean') OR (column_name='content' AND data_type<>'bytea') OR (column_name IN ('created_at','updated_at') AND data_type<>'timestamp with time zone') OR (column_name IN ('id','item_id','kind','name','description','setup_note','path','content_hash') AND data_type<>'text'))", vec![]).await?.ok_or(AuthError::Unavailable)?;
        if row.try_get::<i64>("", "broken").map_err(database_error)? != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    let mut after = String::new();
    loop {
        let rows = db.query_all(super::statement(db, "SELECT id,org_id,creator_id,revision,enabled FROM neev_library_items WHERE id>$1 ORDER BY id LIMIT 100", vec![after.clone().into()])).await.map_err(database_error)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let id: String = row.try_get("", "id").map_err(database_error)?;
            if !uuid::Uuid::parse_str(&id).is_ok_and(|v| v.to_string() == id)
                || row.try_get::<i32>("", "org_id").map_err(database_error)? <= 0
                || row
                    .try_get::<i32>("", "creator_id")
                    .map_err(database_error)?
                    <= 0
                || row.try_get::<i64>("", "revision").map_err(database_error)? <= 0
            {
                return Err(AuthError::Unavailable);
            }
            row.try_get::<bool>("", "enabled").map_err(database_error)?;
            after = id;
        }
    }
    let mut after = String::new();
    loop {
        let rows = db.query_all(super::statement(db, "SELECT id,path,content_hash FROM neev_library_files WHERE id>$1 ORDER BY id LIMIT 100", vec![after.clone().into()])).await.map_err(database_error)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let id: String = row.try_get("", "id").map_err(database_error)?;
            let path: String = row.try_get("", "path").map_err(database_error)?;
            let hash: String = row.try_get("", "content_hash").map_err(database_error)?;
            if !uuid::Uuid::parse_str(&id).is_ok_and(|v| v.to_string() == id)
                || path.contains(['\\', ':'])
                || path.chars().any(char::is_control)
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(AuthError::Unavailable);
            }
            after = id;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;

    #[tokio::test]
    async fn upgrade_creates_tables() {
        let db = h::database().await;
        // Upgrade should succeed on fresh database
        upgrade(&db).await.expect("upgrade failed");
        // Validation should pass
        validate(&db).await.expect("validation failed");
    }

    #[tokio::test]
    async fn upgrade_is_idempotent() {
        let db = h::database().await;
        upgrade(&db).await.expect("first upgrade failed");
        // Second upgrade should also succeed (IF NOT EXISTS)
        upgrade(&db).await.expect("second upgrade failed");
        validate(&db).await.expect("validation failed");
    }
    async fn assert_upgrade_preserves_organization(db: sea_orm::DatabaseConnection) {
        let user = h::native_account(&db, "library-upgrade", "upgrade@example.test").await;
        let (org, _) = h::organization(&db, user, "admin").await;
        // Schema 19's steer table and schema 21's sharing defaults are newer
        // than 17: remove them too, or the inventory matches no known version.
        // Schema 24's sharing tables are newest, then schema 23's trash ledger.
        for table in ["neev_restricted_nodes", "neev_shared_marks", "neev_grants", "neev_team_members", "neev_teams"] {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        execute(&db, "DROP TABLE neev_node_trash", vec![])
            .await
            .unwrap();
        for table in super::super::knowledge_migration::DROP_ORDER {
            execute(&db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
        execute(&db, "DROP TABLE neev_node_sharing_defaults", vec![])
            .await
            .unwrap();
        execute(&db, "DROP TABLE neev_steer_inputs", vec![])
            .await
            .unwrap();
        execute(&db, "DROP TABLE neev_library_files", vec![])
            .await
            .unwrap();
        execute(&db, "DROP TABLE neev_library_items", vec![])
            .await
            .unwrap();
        execute(
            &db,
            "UPDATE neev_schema_version SET version=17 WHERE id=1",
            vec![],
        )
        .await
        .unwrap();
        super::super::initialize(&db).await.unwrap();
        super::super::initialize(&db).await.unwrap();
        validate(&db).await.unwrap();
        let row = super::super::one(
            &db,
            "SELECT version FROM neev_schema_version WHERE id=1",
            vec![],
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "version").unwrap(),
            super::super::CURRENT_SCHEMA_VERSION
        );
        assert!(super::super::one(
            &db,
            "SELECT id FROM neev_organizations WHERE id=$1",
            vec![org.into()]
        )
        .await
        .unwrap()
        .is_some());
    }

    #[tokio::test]
    async fn schema17_upgrade_preserves_organization() {
        assert_upgrade_preserves_organization(h::database().await).await;
    }

    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL"]
    async fn postgres_schema17_upgrade_preserves_organization() {
        let (db, admin, schema) = h::postgres_database().await;
        assert_upgrade_preserves_organization(db.clone()).await;
        h::cleanup_postgres(db, admin, schema).await;
    }

    #[tokio::test]
    async fn validator_rejects_cross_org_file_reference() {
        use base64::Engine;
        let harness = h::OrgHarness::new().await;
        crate::neev::library::import(
            harness.state(),
            harness.owner_context(),
            crate::neev::library::ImportRequest {
                file_name: "SKILL.md".into(),
                data: base64::engine::general_purpose::STANDARD.encode(b"# Team skill"),
            },
        )
        .await
        .unwrap();
        execute(
            &harness.database,
            "UPDATE neev_library_files SET org_id=$1",
            vec![harness.other_org_context().org_id.into()],
        )
        .await
        .unwrap();
        assert_eq!(
            validate(&harness.database).await,
            Err(AuthError::Unavailable)
        );
    }
}
