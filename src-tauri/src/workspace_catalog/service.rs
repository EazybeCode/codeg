use std::collections::HashSet;

use chrono::Utc;
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, TransactionTrait,
};

use crate::db::entities::{org, user, workspace_node, workspace_repository};

use super::{
    repository_access::validate_repository_full_name, validate_branch, validate_label,
    validate_node_shape, CatalogError, CreateNodeInput, RepositoryAccess, RepositoryDefault, Scope,
    UpdateNodeInput, WorkspaceNode, WorkspaceNodeKind, WorkspaceRepository,
};

fn kind_as_str(kind: &WorkspaceNodeKind) -> &'static str {
    match kind {
        WorkspaceNodeKind::Project => "project",
        WorkspaceNodeKind::Folder => "folder",
        WorkspaceNodeKind::WorkingDirectory => "working_directory",
    }
}

fn parse_kind(value: &str) -> Result<WorkspaceNodeKind, CatalogError> {
    match value {
        "project" => Ok(WorkspaceNodeKind::Project),
        "folder" => Ok(WorkspaceNodeKind::Folder),
        "working_directory" => Ok(WorkspaceNodeKind::WorkingDirectory),
        other => {
            tracing::error!(kind = other, "workspace catalog row has an invalid node kind");
            Err(CatalogError::Database)
        }
    }
}

pub(crate) fn to_workspace_node(
    model: workspace_node::Model,
) -> Result<WorkspaceNode, CatalogError> {
    let repository_default = match (model.repository_id, model.starting_branch) {
        (Some(repository_id), Some(starting_branch)) => Some(RepositoryDefault {
            repository_id,
            starting_branch,
        }),
        (None, None) => None,
        _ => {
            tracing::error!(
                node_id = model.id,
                "workspace catalog row has a partial repository default"
            );
            return Err(CatalogError::Database);
        }
    };
    Ok(WorkspaceNode {
        id: model.id,
        org_id: model.org_id,
        parent_id: model.parent_id,
        kind: parse_kind(&model.kind)?,
        name: model.name,
        repository_default,
        revision: model.revision,
    })
}

/// Verify that both scope principals exist and that the user belongs to the
/// organization. This is reusable by draft mutations that already own a
/// transaction.
pub(crate) async fn verify_scope<C>(db: &C, scope: &Scope) -> Result<(), CatalogError>
where
    C: ConnectionTrait,
{
    let org_exists = org::Entity::find_by_id(scope.org_id).one(db).await?.is_some();
    let user_exists = user::Entity::find_by_id(scope.user_id)
        .filter(user::Column::OrgId.eq(scope.org_id))
        .one(db)
        .await?
        .is_some();
    if !org_exists || !user_exists {
        return Err(CatalogError::Unauthorized);
    }
    Ok(())
}

/// Begin a tenant mutation and serialize it against other mutations for the
/// same organization before callers read hierarchy or defaults.
pub(crate) async fn begin_scoped_write(
    db: &DatabaseConnection,
    scope: &Scope,
) -> Result<DatabaseTransaction, CatalogError> {
    let txn = db.begin().await?;
    let org_exists = match db.get_database_backend() {
        DbBackend::Postgres => org::Entity::find_by_id(scope.org_id)
            .lock(LockType::Update)
            .one(&txn)
            .await?
            .is_some(),
        DbBackend::Sqlite => {
            let touched = org::Entity::update_many()
                .col_expr(
                    org::Column::UpdatedAt,
                    Expr::col(org::Column::UpdatedAt).into(),
                )
                .filter(org::Column::Id.eq(scope.org_id))
                .exec(&txn)
                .await?;
            touched.rows_affected == 1
        }
        _ => org::Entity::find_by_id(scope.org_id)
            .one(&txn)
            .await?
            .is_some(),
    };
    if !org_exists {
        return Err(CatalogError::Unauthorized);
    }
    let user_exists = user::Entity::find_by_id(scope.user_id)
        .filter(user::Column::OrgId.eq(scope.org_id))
        .one(&txn)
        .await?
        .is_some();
    if !user_exists {
        return Err(CatalogError::Unauthorized);
    }
    Ok(txn)
}

/// Require a catalog repository owned by the scoped organization. Draft
/// creation reuses this check before persisting a selected repository.
pub(crate) async fn verify_repository(
    txn: &DatabaseTransaction,
    scope: &Scope,
    repository_id: i32,
) -> Result<workspace_repository::Model, CatalogError> {
    workspace_repository::Entity::find_by_id(repository_id)
        .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
        .one(txn)
        .await?
        .ok_or(CatalogError::NotFound)
}

pub async fn connect_repository(
    db: &DatabaseConnection,
    scope: &Scope,
    access: &dyn RepositoryAccess,
    full_name: &str,
    branch: &str,
) -> Result<WorkspaceRepository, CatalogError> {
    validate_repository_full_name(full_name)?;
    validate_branch(branch)?;
    verify_scope(db, scope).await?;

    let verified = access.verify(scope, full_name, branch).await?;
    if verified.github_repository_id.is_empty()
        || !verified
            .github_repository_id
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(CatalogError::UpstreamUnavailable);
    }
    validate_repository_full_name(&verified.full_name)
        .map_err(|_| CatalogError::UpstreamUnavailable)?;
    validate_branch(&verified.starting_branch).map_err(|_| CatalogError::UpstreamUnavailable)?;
    if verified.starting_branch != branch {
        return Err(CatalogError::RepositoryUnavailable);
    }

    let txn = begin_scoped_write(db, scope).await?;
    let now = Utc::now();
    let model = match workspace_repository::Entity::find()
        .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
        .filter(
            workspace_repository::Column::GithubRepositoryId
                .eq(verified.github_repository_id.as_str()),
        )
        .one(&txn)
        .await?
    {
        Some(existing) => {
            let mut active: workspace_repository::ActiveModel = existing.into();
            active.full_name = Set(verified.full_name);
            active.updated_at = Set(now);
            active.update(&txn).await?
        }
        None => workspace_repository::ActiveModel {
            id: NotSet,
            org_id: Set(scope.org_id),
            github_repository_id: Set(verified.github_repository_id),
            full_name: Set(verified.full_name),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?,
    };
    txn.commit().await?;
    Ok(WorkspaceRepository {
        id: model.id,
        org_id: model.org_id,
        github_repository_id: model.github_repository_id,
        full_name: model.full_name,
    })
}

async fn validate_repository_default(
    txn: &DatabaseTransaction,
    scope: &Scope,
    default: &Option<RepositoryDefault>,
) -> Result<(), CatalogError> {
    if let Some(default) = default {
        verify_repository(txn, scope, default.repository_id).await?;
    }
    Ok(())
}

async fn validate_parent_hierarchy(
    txn: &DatabaseTransaction,
    scope: &Scope,
    parent_id: Option<i32>,
    moving_node_id: Option<i32>,
) -> Result<(), CatalogError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    let mut cursor = Some(parent_id);
    let mut seen = HashSet::new();
    while let Some(id) = cursor {
        if moving_node_id == Some(id) || !seen.insert(id) {
            return Err(CatalogError::InvalidInput("hierarchy cycle".into()));
        }
        let row = workspace_node::Entity::find_by_id(id)
            .filter(workspace_node::Column::OrgId.eq(scope.org_id))
            .one(txn)
            .await?
            .ok_or(CatalogError::NotFound)?;
        cursor = row.parent_id;
    }
    Ok(())
}

pub async fn list_nodes(
    db: &DatabaseConnection,
    scope: &Scope,
) -> Result<Vec<WorkspaceNode>, CatalogError> {
    verify_scope(db, scope).await?;
    workspace_node::Entity::find()
        .filter(workspace_node::Column::OrgId.eq(scope.org_id))
        .order_by_asc(workspace_node::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(to_workspace_node)
        .collect()
}

pub async fn create_node(
    db: &DatabaseConnection,
    scope: &Scope,
    input: CreateNodeInput,
) -> Result<WorkspaceNode, CatalogError> {
    validate_label(&input.name)?;
    validate_node_shape(&input.kind, input.parent_id, &input.repository_default)?;

    let txn = begin_scoped_write(db, scope).await?;
    validate_parent_hierarchy(&txn, scope, input.parent_id, None).await?;
    validate_repository_default(&txn, scope, &input.repository_default).await?;

    let now = Utc::now();
    let (repository_id, starting_branch) = input
        .repository_default
        .map(|default| (Some(default.repository_id), Some(default.starting_branch)))
        .unwrap_or((None, None));
    let model = workspace_node::ActiveModel {
        id: NotSet,
        org_id: Set(scope.org_id),
        parent_id: Set(input.parent_id),
        kind: Set(kind_as_str(&input.kind).into()),
        name: Set(input.name.trim().into()),
        repository_id: Set(repository_id),
        starting_branch: Set(starting_branch),
        revision: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    to_workspace_node(model)
}

pub async fn update_node(
    db: &DatabaseConnection,
    scope: &Scope,
    input: UpdateNodeInput,
) -> Result<WorkspaceNode, CatalogError> {
    validate_label(&input.name)?;
    if input.expected_revision < 0 {
        return Err(CatalogError::InvalidInput(
            "expected revision cannot be negative".into(),
        ));
    }

    let txn = begin_scoped_write(db, scope).await?;
    let existing = workspace_node::Entity::find_by_id(input.node_id)
        .filter(workspace_node::Column::OrgId.eq(scope.org_id))
        .one(&txn)
        .await?
        .ok_or(CatalogError::NotFound)?;
    let kind = parse_kind(&existing.kind)?;
    validate_node_shape(&kind, input.parent_id, &input.repository_default)?;
    validate_parent_hierarchy(&txn, scope, input.parent_id, Some(input.node_id)).await?;
    validate_repository_default(&txn, scope, &input.repository_default).await?;

    let (repository_id, starting_branch) = input
        .repository_default
        .map(|default| (Some(default.repository_id), Some(default.starting_branch)))
        .unwrap_or((None, None));
    let result = workspace_node::Entity::update_many()
        .col_expr(
            workspace_node::Column::ParentId,
            Expr::value(input.parent_id),
        )
        .col_expr(
            workspace_node::Column::Name,
            Expr::value(input.name.trim()),
        )
        .col_expr(
            workspace_node::Column::RepositoryId,
            Expr::value(repository_id),
        )
        .col_expr(
            workspace_node::Column::StartingBranch,
            Expr::value(starting_branch),
        )
        .col_expr(
            workspace_node::Column::Revision,
            Expr::value(existing.revision + 1),
        )
        .col_expr(
            workspace_node::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(workspace_node::Column::Id.eq(input.node_id))
        .filter(workspace_node::Column::OrgId.eq(scope.org_id))
        .filter(workspace_node::Column::Revision.eq(input.expected_revision))
        .exec(&txn)
        .await?;
    if result.rows_affected != 1 {
        return Err(CatalogError::Conflict);
    }
    let updated = workspace_node::Entity::find_by_id(input.node_id)
        .filter(workspace_node::Column::OrgId.eq(scope.org_id))
        .one(&txn)
        .await?
        .ok_or(CatalogError::Database)?;
    txn.commit().await?;
    to_workspace_node(updated)
}
