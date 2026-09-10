use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, Set,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::entities::{workspace_chat_draft, workspace_node, workspace_repository};

use super::repository_access::validate_repository_full_name;
use super::service::{begin_scoped_write, to_workspace_node, verify_repository, verify_scope};
use super::{
    resolve_default, validate_branch, CatalogError, CreateDraftInput, RepositoryAccess,
    RepositoryDefault, Scope, VerifiedRepository, WorkspaceChatDraft, WorkspaceNode,
};

struct NormalizedDraftInput {
    node_id: i32,
    repository_override: Option<RepositoryDefault>,
    title: Option<String>,
    request_id: String,
    request_fingerprint: String,
}

fn normalize_input(input: CreateDraftInput) -> Result<NormalizedDraftInput, CatalogError> {
    let request_id = Uuid::parse_str(&input.request_id)
        .map_err(|_| CatalogError::InvalidInput("request id must be a UUID".into()))?
        .to_string();
    let title = input.title.and_then(|title| {
        let trimmed = title.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    });
    if title
        .as_ref()
        .is_some_and(|title| title.chars().count() > 200)
    {
        return Err(CatalogError::InvalidInput(
            "title cannot exceed 200 characters".into(),
        ));
    }
    if let Some(repository_override) = &input.repository_override {
        validate_branch(&repository_override.starting_branch)?;
    }
    let request_fingerprint = request_fingerprint(
        input.node_id,
        title.as_deref(),
        input.repository_override.as_ref(),
    );
    Ok(NormalizedDraftInput {
        node_id: input.node_id,
        repository_override: input.repository_override,
        title,
        request_id,
        request_fingerprint,
    })
}

fn request_fingerprint(
    node_id: i32,
    title: Option<&str>,
    repository_override: Option<&RepositoryDefault>,
) -> String {
    fn add_bytes(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let mut hasher = Sha256::new();
    hasher.update(node_id.to_be_bytes());
    match title {
        Some(title) => {
            hasher.update([1]);
            add_bytes(&mut hasher, title.as_bytes());
        }
        None => hasher.update([0]),
    }
    match repository_override {
        Some(repository_override) => {
            hasher.update([1]);
            hasher.update(repository_override.repository_id.to_be_bytes());
            add_bytes(&mut hasher, repository_override.starting_branch.as_bytes());
        }
        None => hasher.update([0]),
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn to_workspace_chat_draft(model: workspace_chat_draft::Model) -> WorkspaceChatDraft {
    WorkspaceChatDraft {
        id: model.id,
        org_id: model.org_id,
        node_id: model.node_id,
        creator_user_id: model.creator_user_id,
        repository_id: model.repository_id,
        github_repository_id: model.github_repository_id,
        full_name: model.full_name,
        starting_branch: model.starting_branch,
        title: model.title,
        execution_state: model.execution_state,
    }
}

async fn load_nodes<C>(db: &C, org_id: i32) -> Result<Vec<WorkspaceNode>, CatalogError>
where
    C: ConnectionTrait,
{
    workspace_node::Entity::find()
        .filter(workspace_node::Column::OrgId.eq(org_id))
        .order_by_asc(workspace_node::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(to_workspace_node)
        .collect()
}

async fn find_existing<C>(
    db: &C,
    scope: &Scope,
    request_id: &str,
) -> Result<Option<workspace_chat_draft::Model>, CatalogError>
where
    C: ConnectionTrait,
{
    Ok(workspace_chat_draft::Entity::find()
        .filter(workspace_chat_draft::Column::OrgId.eq(scope.org_id))
        .filter(workspace_chat_draft::Column::CreatorUserId.eq(scope.user_id))
        .filter(workspace_chat_draft::Column::RequestId.eq(request_id))
        .one(db)
        .await?)
}

async fn verify_identity(
    access: &dyn RepositoryAccess,
    scope: &Scope,
    github_repository_id: &str,
    full_name: &str,
    starting_branch: &str,
) -> Result<VerifiedRepository, CatalogError> {
    let verified = access.verify(scope, full_name, starting_branch).await?;
    if verified.github_repository_id != github_repository_id {
        return Err(CatalogError::RepositoryUnavailable);
    }
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
    if verified.starting_branch != starting_branch {
        return Err(CatalogError::RepositoryUnavailable);
    }
    Ok(verified)
}

async fn retry_existing(
    db: &DatabaseConnection,
    scope: &Scope,
    access: &dyn RepositoryAccess,
    input: &NormalizedDraftInput,
    existing: workspace_chat_draft::Model,
) -> Result<WorkspaceChatDraft, CatalogError> {
    if existing.request_fingerprint != input.request_fingerprint {
        return Err(CatalogError::Conflict);
    }
    let repository = workspace_repository::Entity::find_by_id(existing.repository_id)
        .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
        .one(db)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if repository.github_repository_id != existing.github_repository_id {
        return Err(CatalogError::RepositoryUnavailable);
    }
    verify_identity(
        access,
        scope,
        &existing.github_repository_id,
        &repository.full_name,
        &existing.starting_branch,
    )
    .await?;
    Ok(to_workspace_chat_draft(existing))
}

pub async fn create_draft(
    db: &DatabaseConnection,
    scope: &Scope,
    access: &dyn RepositoryAccess,
    input: CreateDraftInput,
) -> Result<WorkspaceChatDraft, CatalogError> {
    let input = normalize_input(input)?;
    verify_scope(db, scope).await?;

    if let Some(existing) = find_existing(db, scope, &input.request_id).await? {
        return retry_existing(db, scope, access, &input, existing).await;
    }

    let nodes = load_nodes(db, scope.org_id).await?;
    let inherited = resolve_default(&nodes, scope, input.node_id)?;
    let selected = input
        .repository_override
        .clone()
        .or(inherited)
        .ok_or(CatalogError::RepositoryRequired)?;
    let repository = workspace_repository::Entity::find_by_id(selected.repository_id)
        .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
        .one(db)
        .await?
        .ok_or(CatalogError::NotFound)?;
    let verified = verify_identity(
        access,
        scope,
        &repository.github_repository_id,
        &repository.full_name,
        &selected.starting_branch,
    )
    .await?;

    let txn = begin_scoped_write(db, scope).await?;
    if let Some(existing) = find_existing(&txn, scope, &input.request_id).await? {
        if existing.request_fingerprint != input.request_fingerprint {
            return Err(CatalogError::Conflict);
        }
        if existing.repository_id == selected.repository_id
            && existing.github_repository_id == verified.github_repository_id
            && existing.full_name == verified.full_name
            && existing.starting_branch == verified.starting_branch
        {
            txn.commit().await?;
            return Ok(to_workspace_chat_draft(existing));
        }
        txn.commit().await?;
        return retry_existing(db, scope, access, &input, existing).await;
    }

    let current_nodes = load_nodes(&txn, scope.org_id).await?;
    let current_inherited = resolve_default(&current_nodes, scope, input.node_id)
        .map_err(|error| match error {
            CatalogError::NotFound => CatalogError::Conflict,
            other => other,
        })?;
    if input.repository_override.is_none() && current_inherited.as_ref() != Some(&selected) {
        return Err(CatalogError::Conflict);
    }
    let current_repository = verify_repository(&txn, scope, selected.repository_id).await?;
    if current_repository.github_repository_id != verified.github_repository_id {
        return Err(CatalogError::Conflict);
    }

    let now = Utc::now();
    let model = workspace_chat_draft::ActiveModel {
        id: NotSet,
        org_id: Set(scope.org_id),
        node_id: Set(input.node_id),
        creator_user_id: Set(scope.user_id),
        repository_id: Set(selected.repository_id),
        github_repository_id: Set(verified.github_repository_id),
        full_name: Set(verified.full_name),
        starting_branch: Set(verified.starting_branch),
        title: Set(input.title),
        request_id: Set(input.request_id),
        request_fingerprint: Set(input.request_fingerprint),
        execution_state: Set("unprovisioned".into()),
        visibility: Set("private".into()),
        access_revision: Set(0),
        creation_visibility: Set("private".into()),
        creation_fingerprint_version: Set(1),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok(to_workspace_chat_draft(model))
}

pub async fn list_drafts(
    db: &DatabaseConnection,
    scope: &Scope,
    node_id: i32,
) -> Result<Vec<WorkspaceChatDraft>, CatalogError> {
    verify_scope(db, scope).await?;
    workspace_node::Entity::find_by_id(node_id)
        .filter(workspace_node::Column::OrgId.eq(scope.org_id))
        .one(db)
        .await?
        .ok_or(CatalogError::NotFound)?;
    Ok(workspace_chat_draft::Entity::find()
        .filter(workspace_chat_draft::Column::OrgId.eq(scope.org_id))
        .filter(workspace_chat_draft::Column::CreatorUserId.eq(scope.user_id))
        .filter(workspace_chat_draft::Column::NodeId.eq(node_id))
        .order_by_asc(workspace_chat_draft::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(to_workspace_chat_draft)
        .collect())
}
