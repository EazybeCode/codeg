use std::collections::HashSet;

use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseTransaction,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use super::{
    authority::{admit, begin_authorized, commit_rechecked, require_admission, NODE, REPOSITORY},
    repository::{validate_verified_names, TeamRepositoryAccess, VerifiedRepository},
    types::{
        parse_revision, valid_positive_id, CreateNodeInput, NodeDto, RepositoryDefaultInput,
        UpdateNodeInput, WorkspaceNodeKind,
    },
};
use crate::{
    db::entities::{team_catalog_admission, workspace_node, workspace_repository},
    team_access::principal::AuthContext,
    team_server::{handlers::ApiError, state::TeamState},
    workspace_catalog::{
        validate_branch, validate_label, validate_node_shape, RepositoryDefault,
        WorkspaceNodeKind as LegacyNodeKind,
    },
};

const CATALOG_CAP: u64 = 1000;

pub(super) struct ValidNode {
    pub(super) row: workspace_node::Model,
    pub(super) resolved_default: Option<RepositoryDefaultInput>,
}

fn legacy_kind(kind: WorkspaceNodeKind) -> LegacyNodeKind {
    match kind {
        WorkspaceNodeKind::Project => LegacyNodeKind::Project,
        WorkspaceNodeKind::Folder => LegacyNodeKind::Folder,
        WorkspaceNodeKind::WorkingDirectory => LegacyNodeKind::WorkingDirectory,
    }
}

fn legacy_default(value: &Option<RepositoryDefaultInput>) -> Option<RepositoryDefault> {
    value.as_ref().map(|value| RepositoryDefault {
        repository_id: value.repository_id,
        starting_branch: value.starting_branch.clone(),
    })
}

fn validate_input_shape(
    kind: WorkspaceNodeKind,
    parent_id: Option<i32>,
    default: &Option<RepositoryDefaultInput>,
) -> Result<(), ApiError> {
    if parent_id.is_some_and(|id| !valid_positive_id(id))
        || default
            .as_ref()
            .is_some_and(|default| !valid_positive_id(default.repository_id))
    {
        return Err(ApiError::InvalidRequest);
    }
    validate_node_shape(&legacy_kind(kind), parent_id, &legacy_default(default))
        .map_err(|_| ApiError::InvalidRequest)
}

fn row_default(row: &workspace_node::Model) -> Result<Option<RepositoryDefaultInput>, ApiError> {
    match (&row.repository_id, &row.starting_branch) {
        (Some(repository_id), Some(starting_branch)) if valid_positive_id(*repository_id) => {
            validate_branch(starting_branch).map_err(|_| ApiError::Unavailable)?;
            Ok(Some(RepositoryDefaultInput {
                repository_id: *repository_id,
                starting_branch: starting_branch.clone(),
            }))
        }
        (None, None) => Ok(None),
        _ => Err(ApiError::Unavailable),
    }
}

async fn require_valid_repository(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    repository_id: i32,
) -> Result<workspace_repository::Model, ApiError> {
    require_admission(transaction, REPOSITORY, repository_id, context.org_id).await?;
    let row = workspace_repository::Entity::find_by_id(repository_id)
        .one(transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .filter(|row| row.org_id == context.org_id)
        .ok_or(ApiError::NotFound)?;
    validate_verified_names(&row)?;
    Ok(row)
}

pub(super) async fn load_valid_node(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    node_id: i32,
) -> Result<ValidNode, ApiError> {
    if !valid_positive_id(node_id) {
        return Err(ApiError::NotFound);
    }
    let mut cursor = Some(node_id);
    let mut seen = HashSet::new();
    let mut leaf = None;
    let mut resolved_default = None;
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return Err(ApiError::NotFound);
        }
        require_admission(transaction, NODE, id, context.org_id).await?;
        let row = workspace_node::Entity::find_by_id(id)
            .one(transaction)
            .await
            .map_err(|_| ApiError::Unavailable)?
            .filter(|row| row.org_id == context.org_id)
            .ok_or(ApiError::NotFound)?;
        let kind = WorkspaceNodeKind::parse(&row.kind).ok_or(ApiError::Unavailable)?;
        let default = row_default(&row)?;
        validate_label(&row.name).map_err(|_| ApiError::Unavailable)?;
        validate_input_shape(kind, row.parent_id, &default).map_err(|_| ApiError::NotFound)?;
        if let Some(default) = &default {
            require_valid_repository(transaction, context, default.repository_id).await?;
            if resolved_default.is_none() {
                resolved_default = Some(default.clone());
            }
        }
        if leaf.is_none() {
            leaf = Some(row.clone());
        }
        cursor = row.parent_id;
    }
    Ok(ValidNode {
        row: leaf.ok_or(ApiError::NotFound)?,
        resolved_default,
    })
}

fn project(row: workspace_node::Model) -> Result<NodeDto, ApiError> {
    let repository_default = row_default(&row)?;
    Ok(NodeDto {
        id: row.id,
        parent_id: row.parent_id,
        kind: WorkspaceNodeKind::parse(&row.kind).ok_or(ApiError::Unavailable)?,
        name: row.name,
        repository_default,
        revision: row.revision.to_string(),
    })
}

async fn verify_repository_access(
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    default: &RepositoryDefaultInput,
    repository: workspace_repository::Model,
) -> Result<VerifiedRepository, ApiError> {
    let verified = access
        .verify(context, &repository.full_name, &default.starting_branch)
        .await?;
    if verified.github_repository_id != repository.github_repository_id
        || verified.starting_branch != default.starting_branch
    {
        return Err(ApiError::NotFound);
    }
    Ok(verified)
}

pub(in crate::team_server) async fn list_nodes(
    state: &TeamState,
    context: &AuthContext,
) -> Result<Vec<NodeDto>, ApiError> {
    let transaction = begin_authorized(state, context).await?;
    let admissions = team_catalog_admission::Entity::find()
        .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
        .filter(team_catalog_admission::Column::ResourceKind.eq(NODE))
        .order_by_asc(team_catalog_admission::Column::ResourceId)
        .limit(CATALOG_CAP)
        .all(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let mut result = Vec::new();
    for admitted in admissions {
        if let Ok(valid) = load_valid_node(&transaction, context, admitted.resource_id).await {
            result.push(project(valid.row)?);
        }
    }
    commit_rechecked(state, context, transaction).await?;
    Ok(result)
}

pub(in crate::team_server) async fn create_node(
    state: &TeamState,
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    input: CreateNodeInput,
) -> Result<NodeDto, ApiError> {
    validate_label(&input.name).map_err(|_| ApiError::InvalidRequest)?;
    validate_input_shape(input.kind, input.parent_id, &input.repository_default)?;
    let preflight = begin_authorized(state, context).await?;
    if let Some(parent_id) = input.parent_id {
        load_valid_node(&preflight, context, parent_id).await?;
    }
    let preflight_repository = match &input.repository_default {
        Some(default) => {
            Some(require_valid_repository(&preflight, context, default.repository_id).await?)
        }
        None => None,
    };
    commit_rechecked(state, context, preflight).await?;
    let verified = match (&input.repository_default, preflight_repository) {
        (Some(default), Some(repository)) => {
            Some(verify_repository_access(context, access, default, repository).await?)
        }
        _ => None,
    };
    let transaction = begin_authorized(state, context).await?;
    if let Some(parent_id) = input.parent_id {
        load_valid_node(&transaction, context, parent_id).await?;
    }
    let count = team_catalog_admission::Entity::find()
        .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
        .filter(team_catalog_admission::Column::ResourceKind.eq(NODE))
        .count(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    if count >= CATALOG_CAP {
        return Err(ApiError::Conflict);
    }
    if let (Some(default), Some(verified)) = (&input.repository_default, &verified) {
        let repository = require_valid_repository(&transaction, context, default.repository_id).await?;
        if repository.github_repository_id != verified.github_repository_id {
            return Err(ApiError::Conflict);
        }
    }
    let (repository_id, starting_branch) = input
        .repository_default
        .map(|value| (Some(value.repository_id), Some(value.starting_branch)))
        .unwrap_or((None, None));
    let row = workspace_node::ActiveModel {
        id: NotSet,
        org_id: Set(context.org_id),
        parent_id: Set(input.parent_id),
        kind: Set(input.kind.as_str().into()),
        name: Set(input.name.trim().into()),
        repository_id: Set(repository_id),
        starting_branch: Set(starting_branch),
        revision: Set(0),
        created_at: Set(state.clock.now()),
        updated_at: Set(state.clock.now()),
    }
    .insert(&transaction)
    .await
    .map_err(|_| ApiError::Unavailable)?;
    admit(state, &transaction, context, NODE, row.id).await?;
    commit_rechecked(state, context, transaction).await?;
    project(row)
}

pub(in crate::team_server) async fn update_node(
    state: &TeamState,
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    node_id: i32,
    input: UpdateNodeInput,
) -> Result<NodeDto, ApiError> {
    let expected = parse_revision(&input.expected_revision).ok_or(ApiError::InvalidRequest)?;
    validate_label(&input.name).map_err(|_| ApiError::InvalidRequest)?;
    let preflight = begin_authorized(state, context).await?;
    let existing = load_valid_node(&preflight, context, node_id).await?.row;
    if existing.revision != expected {
        return Err(ApiError::Conflict);
    }
    let kind = WorkspaceNodeKind::parse(&existing.kind).ok_or(ApiError::Unavailable)?;
    validate_input_shape(kind, input.parent_id, &input.repository_default)?;
    if let Some(parent_id) = input.parent_id {
        if parent_id == node_id {
            return Err(ApiError::InvalidRequest);
        }
        load_valid_node(&preflight, context, parent_id).await?;
    }
    let preflight_repository = match &input.repository_default {
        Some(default) => {
            Some(require_valid_repository(&preflight, context, default.repository_id).await?)
        }
        None => None,
    };
    commit_rechecked(state, context, preflight).await?;
    let verified = match (&input.repository_default, preflight_repository) {
        (Some(default), Some(repository)) => {
            Some(verify_repository_access(context, access, default, repository).await?)
        }
        _ => None,
    };

    let transaction = begin_authorized(state, context).await?;
    let current = load_valid_node(&transaction, context, node_id).await?.row;
    if current.revision != expected {
        return Err(ApiError::Conflict);
    }
    if let Some(parent_id) = input.parent_id {
        if parent_id == node_id {
            return Err(ApiError::InvalidRequest);
        }
        let parent = load_valid_node(&transaction, context, parent_id).await?;
        let mut cursor = Some(parent.row.id);
        let mut seen = HashSet::new();
        while let Some(id) = cursor {
            if id == node_id || !seen.insert(id) {
                return Err(ApiError::InvalidRequest);
            }
            cursor = workspace_node::Entity::find_by_id(id)
                .one(&transaction)
                .await
                .map_err(|_| ApiError::Unavailable)?
                .and_then(|row| row.parent_id);
        }
    }
    if let (Some(default), Some(verified)) = (&input.repository_default, &verified) {
        let repository = require_valid_repository(&transaction, context, default.repository_id).await?;
        if repository.github_repository_id != verified.github_repository_id {
            return Err(ApiError::Conflict);
        }
    }
    let next = current.revision.checked_add(1).ok_or(ApiError::Conflict)?;
    let (repository_id, starting_branch) = input
        .repository_default
        .map(|value| (Some(value.repository_id), Some(value.starting_branch)))
        .unwrap_or((None, None));
    let result = workspace_node::Entity::update_many()
        .col_expr(workspace_node::Column::ParentId, Expr::value(input.parent_id))
        .col_expr(workspace_node::Column::Name, Expr::value(input.name.trim()))
        .col_expr(workspace_node::Column::RepositoryId, Expr::value(repository_id))
        .col_expr(workspace_node::Column::StartingBranch, Expr::value(starting_branch))
        .col_expr(workspace_node::Column::Revision, Expr::value(next))
        .col_expr(workspace_node::Column::UpdatedAt, Expr::value(state.clock.now()))
        .filter(workspace_node::Column::Id.eq(node_id))
        .filter(workspace_node::Column::OrgId.eq(context.org_id))
        .filter(workspace_node::Column::Revision.eq(expected))
        .exec(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    if result.rows_affected != 1 {
        return Err(ApiError::Conflict);
    }
    let updated = workspace_node::Entity::find_by_id(node_id)
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .ok_or(ApiError::Unavailable)?;
    commit_rechecked(state, context, transaction).await?;
    project(updated)
}
