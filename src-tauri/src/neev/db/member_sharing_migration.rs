//! Schema 24 — teams, and sharing with members and teams.
//!
//! Four additive tables, and NO change to any existing table or row:
//! - `neev_teams` / `neev_team_members`: admin-managed teams of org members.
//! - `neev_grants`: a View or Edit grant to a user or a team on a resource, a
//!   folder/project node, or a standalone Agent Chat session (View only).
//! - `neev_shared_marks`: which resources use member/team sharing ("shared"),
//!   and which nodes default new items to it.
//!
//! "Shared" is deliberately NOT a new `visibility` value. A shared resource keeps
//! `visibility='private'` and carries a `resource` mark; the access layer reads
//! the mark and the grants. So a binary that predates this schema, or a
//! downgrade that drops these tables, sees every shared item as private — access
//! can only ever NARROW, never widen, across a rollback. And no existing row is
//! touched: an item that was private stays private (the explicit private
//! exception) until its creator chooses otherwise.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(crate) const SCHEMA_VERSION: i64 = 24;
pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_teams",
    "neev_team_members",
    "neev_grants",
    "neev_shared_marks",
    "neev_restricted_nodes",
];

struct Table {
    name: &'static str,
    ddl: &'static str,
    /// (column, PostgreSQL data_type), every column NOT NULL.
    columns: &'static [(&'static str, &'static str)],
    /// (constraint name, contype, fragments of `pg_get_constraintdef`).
    constraints: &'static [(&'static str, &'static str, &'static [&'static str])],
}

const TABLES: [Table; 5] = [
    Table {
        name: "neev_teams",
        ddl: "CREATE TABLE neev_teams (org_id integer NOT NULL, id text NOT NULL, name text NOT NULL, name_key text NOT NULL, created_by integer NOT NULL, revision bigint NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_teams_pk PRIMARY KEY (org_id, id), CONSTRAINT neev_teams_name UNIQUE (org_id, name_key), CONSTRAINT neev_teams_name_len CHECK (length(name)>=1 AND length(name)<=80), CONSTRAINT neev_teams_revision CHECK (revision>0), CONSTRAINT neev_teams_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE, CONSTRAINT neev_teams_creator FOREIGN KEY (created_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("created_at", "bigint"),
            ("created_by", "integer"),
            ("id", "text"),
            ("name", "text"),
            ("name_key", "text"),
            ("org_id", "integer"),
            ("revision", "bigint"),
            ("updated_at", "bigint"),
        ],
        constraints: &[
            ("neev_teams_pk", "p", &["PRIMARY KEY (org_id, id)"]),
            ("neev_teams_name", "u", &["UNIQUE (org_id, name_key)"]),
            ("neev_teams_name_len", "c", &["length(name)", "80"]),
            ("neev_teams_revision", "c", &["revision > 0"]),
            ("neev_teams_org", "f", &["REFERENCES neev_organizations(id) ON DELETE CASCADE"]),
            ("neev_teams_creator", "f", &["(created_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    Table {
        name: "neev_team_members",
        ddl: "CREATE TABLE neev_team_members (org_id integer NOT NULL, team_id text NOT NULL, user_id integer NOT NULL, added_by integer NOT NULL, added_at bigint NOT NULL, CONSTRAINT neev_team_members_pk PRIMARY KEY (org_id, team_id, user_id), CONSTRAINT neev_team_members_team FOREIGN KEY (org_id, team_id) REFERENCES neev_teams (org_id, id) ON DELETE CASCADE, CONSTRAINT neev_team_members_user FOREIGN KEY (user_id) REFERENCES neev_accounts (id), CONSTRAINT neev_team_members_by FOREIGN KEY (added_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("added_at", "bigint"),
            ("added_by", "integer"),
            ("org_id", "integer"),
            ("team_id", "text"),
            ("user_id", "integer"),
        ],
        constraints: &[
            ("neev_team_members_pk", "p", &["PRIMARY KEY (org_id, team_id, user_id)"]),
            (
                "neev_team_members_team",
                "f",
                &["(org_id, team_id) REFERENCES neev_teams(org_id, id) ON DELETE CASCADE"],
            ),
            ("neev_team_members_user", "f", &["(user_id) REFERENCES neev_accounts(id)"]),
            ("neev_team_members_by", "f", &["(added_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    Table {
        name: "neev_grants",
        ddl: "CREATE TABLE neev_grants (org_id integer NOT NULL, target_kind text NOT NULL, target_id text NOT NULL, principal_kind text NOT NULL, principal_id text NOT NULL, level text NOT NULL, granted_by integer NOT NULL, created_at bigint NOT NULL, CONSTRAINT neev_grants_pk PRIMARY KEY (org_id, target_kind, target_id, principal_kind, principal_id), CONSTRAINT neev_grants_target CHECK (target_kind IN ('resource','node','session')), CONSTRAINT neev_grants_principal CHECK (principal_kind IN ('user','team')), CONSTRAINT neev_grants_level CHECK (level IN ('view','edit') AND (target_kind<>'session' OR level='view')), CONSTRAINT neev_grants_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE, CONSTRAINT neev_grants_by FOREIGN KEY (granted_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("created_at", "bigint"),
            ("granted_by", "integer"),
            ("level", "text"),
            ("org_id", "integer"),
            ("principal_id", "text"),
            ("principal_kind", "text"),
            ("target_id", "text"),
            ("target_kind", "text"),
        ],
        constraints: &[
            (
                "neev_grants_pk",
                "p",
                &["PRIMARY KEY (org_id, target_kind, target_id, principal_kind, principal_id)"],
            ),
            ("neev_grants_target", "c", &["'resource'", "'node'", "'session'"]),
            ("neev_grants_principal", "c", &["'user'", "'team'"]),
            ("neev_grants_level", "c", &["'view'", "'edit'", "'session'"]),
            ("neev_grants_org", "f", &["REFERENCES neev_organizations(id) ON DELETE CASCADE"]),
            ("neev_grants_by", "f", &["(granted_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    Table {
        name: "neev_shared_marks",
        ddl: "CREATE TABLE neev_shared_marks (org_id integer NOT NULL, mark_kind text NOT NULL, target_id text NOT NULL, set_by integer NOT NULL, created_at bigint NOT NULL, CONSTRAINT neev_shared_marks_pk PRIMARY KEY (org_id, mark_kind, target_id), CONSTRAINT neev_shared_marks_kind CHECK (mark_kind IN ('resource','node_default','node_restricted')), CONSTRAINT neev_shared_marks_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE, CONSTRAINT neev_shared_marks_by FOREIGN KEY (set_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("created_at", "bigint"),
            ("mark_kind", "text"),
            ("org_id", "integer"),
            ("set_by", "integer"),
            ("target_id", "text"),
        ],
        constraints: &[
            ("neev_shared_marks_pk", "p", &["PRIMARY KEY (org_id, mark_kind, target_id)"]),
            ("neev_shared_marks_kind", "c", &["'resource'", "'node_default'", "'node_restricted'"]),
            ("neev_shared_marks_org", "f", &["REFERENCES neev_organizations(id) ON DELETE CASCADE"]),
            ("neev_shared_marks_by", "f", &["(set_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    // Derived from the `node_restricted` marks: every node at or under a
    // restricted node, with its nearest restricted root. Rewritten in the same
    // transaction as any change to a mark or to the tree's shape, and proved
    // equal to the marks at startup. Lets organization-wide predicates (the org
    // wiki) exclude restricted regions without walking the tree in SQL.
    Table {
        name: "neev_restricted_nodes",
        ddl: "CREATE TABLE neev_restricted_nodes (org_id integer NOT NULL, node_id integer NOT NULL, root_id integer NOT NULL, CONSTRAINT neev_restricted_nodes_pk PRIMARY KEY (org_id, node_id), CONSTRAINT neev_restricted_nodes_node FOREIGN KEY (node_id, org_id) REFERENCES neev_nodes (id, org_id) ON DELETE CASCADE)",
        columns: &[("node_id", "integer"), ("org_id", "integer"), ("root_id", "integer")],
        constraints: &[
            ("neev_restricted_nodes_pk", "p", &["PRIMARY KEY (org_id, node_id)"]),
            (
                "neev_restricted_nodes_node",
                "f",
                &["(node_id, org_id) REFERENCES neev_nodes(id, org_id) ON DELETE CASCADE"],
            ),
        ],
    },
];

async fn exists<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1",
        DbBackend::Sqlite => "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
        _ => return Err(AuthError::Unavailable),
    };
    Ok(super::one(db, sql, vec![table.into()]).await?.is_some())
}

/// Tests that rewind a database to an older inventory and then exercise the
/// live (schema-24) service paths re-add the sharing tables first; the
/// historical snapshot assertions run before this.
#[cfg(test)]
pub(crate) async fn create_missing_for_tests<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for table in &TABLES {
        if !exists(db, table.name).await? {
            super::execute(db, table.ddl, vec![]).await?;
        }
    }
    Ok(())
}

/// Create each table when absent, in dependency order. Idempotent.
pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    for table in &TABLES {
        if !exists(tx, table.name).await? {
            super::execute(tx, table.ddl, vec![]).await?;
        }
    }
    validate(tx).await
}

async fn validate_table<C: ConnectionTrait>(db: &C, table: &Table) -> Result<(), AuthError> {
    match db.get_database_backend() {
        DbBackend::Sqlite => {
            let ddl: String = super::one(
                db,
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
                vec![table.name.into()],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "sql")
            .map_err(database_error)?;
            if ddl != table.ddl {
                return Err(AuthError::Unavailable);
            }
            let extra = super::one(
                db,
                "SELECT 1 AS extra FROM sqlite_master WHERE tbl_name=$1 AND type<>'table' AND name NOT LIKE 'sqlite_autoindex_%' LIMIT 1",
                vec![table.name.into()],
            )
            .await?;
            if extra.is_some() {
                return Err(AuthError::Unavailable);
            }
        }
        DbBackend::Postgres => {
            let rows = db
                .query_all(super::statement(
                    db,
                    "SELECT column_name::text AS name,data_type::text AS kind,is_nullable::text AS nullable FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY column_name",
                    vec![table.name.into()],
                ))
                .await
                .map_err(database_error)?;
            if rows.len() != table.columns.len() {
                return Err(AuthError::Unavailable);
            }
            for (row, (name, kind)) in rows.iter().zip(table.columns) {
                let actual: (String, String, String) = (
                    row.try_get("", "name").map_err(database_error)?,
                    row.try_get("", "kind").map_err(database_error)?,
                    row.try_get("", "nullable").map_err(database_error)?,
                );
                if actual != ((*name).to_owned(), (*kind).to_owned(), "NO".to_owned()) {
                    return Err(AuthError::Unavailable);
                }
            }
            let relid = format!(
                "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relname='{}' AND n.nspname=current_schema())",
                table.name
            );
            let count: i64 = super::one(
                db,
                &format!("SELECT COUNT(*) AS n FROM pg_constraint WHERE conrelid={relid}"),
                vec![],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "n")
            .map_err(database_error)?;
            if count != table.constraints.len() as i64 {
                return Err(AuthError::Unavailable);
            }
            for (name, kind, fragments) in table.constraints {
                let definition: String = super::one(
                    db,
                    &format!("SELECT pg_get_constraintdef(oid) AS def FROM pg_constraint WHERE conrelid={relid} AND conname=$1 AND contype=$2"),
                    vec![(*name).into(), (*kind).into()],
                )
                .await?
                .ok_or(AuthError::Unavailable)?
                .try_get("", "def")
                .map_err(database_error)?;
                if !fragments.iter().all(|fragment| definition.contains(fragment)) {
                    return Err(AuthError::Unavailable);
                }
            }
            // No index beyond the constraints' own.
            let extra = super::one(
                db,
                &format!("SELECT 1 AS extra FROM pg_index i WHERE i.indrelid={relid} AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid=i.indexrelid) LIMIT 1"),
                vec![],
            )
            .await?;
            if extra.is_some() {
                return Err(AuthError::Unavailable);
            }
        }
        _ => return Err(AuthError::Unavailable),
    }
    Ok(())
}

/// Prove the exact schema-24 shape, and that every stored row is one the API
/// could have written. Safe on a transaction or a pooled connection.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for table in &TABLES {
        validate_table(db, table).await?;
    }
    // A team member, and a granted user, is always a member (active or not) of
    // the team's org; a team grant names a team of the same org. A `resource`
    // mark only ever sits on a private, non-bound resource of the same org, and
    // a `node_default` mark on a node whose stored default is `private`.
    let invalid = super::one(
        db,
        "SELECT 1 AS invalid FROM neev_team_members t WHERE NOT EXISTS (SELECT 1 FROM neev_memberships m WHERE m.org_id=t.org_id AND m.user_id=t.user_id) \
         UNION ALL SELECT 1 FROM neev_grants g WHERE g.principal_kind='team' AND NOT EXISTS (SELECT 1 FROM neev_teams t WHERE t.org_id=g.org_id AND t.id=g.principal_id) \
         UNION ALL SELECT 1 FROM neev_shared_marks k WHERE k.mark_kind='resource' AND NOT EXISTS (SELECT 1 FROM neev_resources r WHERE r.org_id=k.org_id AND r.id=k.target_id AND r.visibility='private' AND r.origin_chat_id IS NULL) \
         UNION ALL SELECT 1 FROM neev_shared_marks k WHERE k.mark_kind='node_default' AND NOT EXISTS (SELECT 1 FROM neev_node_sharing_defaults d WHERE d.org_id=k.org_id AND CAST(d.node_id AS TEXT)=k.target_id AND d.default_visibility='private') \
         UNION ALL SELECT 1 FROM neev_shared_marks k WHERE k.mark_kind='node_restricted' AND NOT EXISTS (SELECT 1 FROM neev_nodes n WHERE n.org_id=k.org_id AND CAST(n.id AS TEXT)=k.target_id AND n.kind IN ('project','folder')) \
         LIMIT 1",
        vec![],
    )
    .await?;
    if invalid.is_some() {
        return Err(AuthError::Unavailable);
    }
    // The derived region is exactly what the marks say, org by org.
    let orgs = db
        .query_all(super::statement(
            db,
            "SELECT DISTINCT org_id FROM neev_shared_marks WHERE mark_kind='node_restricted' UNION SELECT DISTINCT org_id FROM neev_restricted_nodes",
            vec![],
        ))
        .await
        .map_err(database_error)?;
    for row in &orgs {
        let org: i32 = row.try_get("", "org_id").map_err(database_error)?;
        if stored_region(db, org).await? != derived_region(db, org).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

/// (node, nearest restricted root) for every node in a restricted region of
/// `org`, computed from the tree and the `node_restricted` marks.
pub(crate) async fn derived_region<C: ConnectionTrait>(
    db: &C,
    org: i32,
) -> Result<std::collections::BTreeMap<i32, i32>, AuthError> {
    let nodes = db
        .query_all(super::statement(
            db,
            "SELECT id,parent_id FROM neev_nodes WHERE org_id=$1",
            vec![org.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut parent = std::collections::HashMap::with_capacity(nodes.len());
    for row in &nodes {
        let id: i32 = row.try_get("", "id").map_err(database_error)?;
        let up: Option<i32> = row.try_get("", "parent_id").map_err(database_error)?;
        parent.insert(id, up);
    }
    let marks = db
        .query_all(super::statement(
            db,
            "SELECT target_id FROM neev_shared_marks WHERE org_id=$1 AND mark_kind='node_restricted'",
            vec![org.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut roots = std::collections::HashSet::new();
    for row in &marks {
        let id: String = row.try_get("", "target_id").map_err(database_error)?;
        roots.insert(id.parse::<i32>().map_err(|_| AuthError::Unavailable)?);
    }
    let mut out = std::collections::BTreeMap::new();
    if roots.is_empty() {
        return Ok(out);
    }
    for &id in parent.keys() {
        let mut cursor = Some(id);
        let mut depth = 0;
        while let Some(at) = cursor {
            depth += 1;
            if depth > 1000 {
                return Err(AuthError::Unavailable);
            }
            if roots.contains(&at) {
                out.insert(id, at);
                break;
            }
            cursor = parent.get(&at).copied().flatten();
        }
    }
    Ok(out)
}

async fn stored_region<C: ConnectionTrait>(
    db: &C,
    org: i32,
) -> Result<std::collections::BTreeMap<i32, i32>, AuthError> {
    let rows = db
        .query_all(super::statement(
            db,
            "SELECT node_id,root_id FROM neev_restricted_nodes WHERE org_id=$1",
            vec![org.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut out = std::collections::BTreeMap::new();
    for row in &rows {
        out.insert(
            row.try_get("", "node_id").map_err(database_error)?,
            row.try_get("", "root_id").map_err(database_error)?,
        );
    }
    Ok(out)
}

/// Rewrite `org`'s derived region from its marks and tree. Called in the same
/// transaction as every change that can move a node into or out of one.
pub(crate) async fn refresh_region(tx: &DatabaseTransaction, org: i32) -> Result<(), AuthError> {
    let region = derived_region(tx, org).await?;
    if region == stored_region(tx, org).await? {
        return Ok(());
    }
    super::execute(tx, "DELETE FROM neev_restricted_nodes WHERE org_id=$1", vec![org.into()]).await?;
    for (node, root) in region {
        super::execute(
            tx,
            "INSERT INTO neev_restricted_nodes(org_id,node_id,root_id) VALUES($1,$2,$3)",
            vec![org.into(), node.into(), root.into()],
        )
        .await?;
    }
    Ok(())
}

/// What a 24 -> 23 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// The five tables are gone and the marker is 23; nothing else changed.
    Done,
    /// The marker is not 24.
    NotAtTwentyFour(i64),
    /// Teams, grants or shared marks (including restricted folders) exist. Dropping them would silently end
    /// sharing (every shared item would become private to its creator), so the
    /// operator must remove them deliberately first.
    SharingExists { rows: i64 },
}

/// Take a schema-24 database back to 23. GUARDED: refuses unless the marker is
/// exactly 24 and no team, grant or mark exists. Runs in the caller's
/// transaction; the caller commits only on `Done`.
pub(crate) async fn downgrade(tx: &DatabaseTransaction) -> Result<Downgrade, AuthError> {
    let version: i64 = super::one(tx, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "version")
        .map_err(database_error)?;
    if version != SCHEMA_VERSION {
        return Ok(Downgrade::NotAtTwentyFour(version));
    }
    validate(tx).await?;
    let rows: i64 = super::one(
        tx,
        "SELECT (SELECT COUNT(*) FROM neev_teams)+(SELECT COUNT(*) FROM neev_grants)+(SELECT COUNT(*) FROM neev_shared_marks) AS n",
        vec![],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "n")
    .map_err(database_error)?;
    if rows > 0 {
        return Ok(Downgrade::SharingExists { rows });
    }
    // Drop children first: the derived region, then marks, grants, members, teams.
    for table in TABLES.iter().rev() {
        super::execute(tx, &format!("DROP TABLE {}", table.name), vec![]).await?;
    }
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::node_trash_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    for table in TABLES.iter() {
        if exists(tx, table.name).await? {
            return Err(AuthError::Unavailable);
        }
    }
    super::node_trash_migration::validate(tx).await?;
    Ok(Downgrade::Done)
}
