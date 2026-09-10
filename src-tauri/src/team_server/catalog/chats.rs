use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait,
    DatabaseTransaction, EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, Set,
    Statement,
};
use uuid::Uuid;

use super::{
    authority::{admit, begin_authorized, commit_rechecked, require_admission, CHAT, REPOSITORY},
    nodes::load_valid_node,
    repository::{validate_verified_names, TeamRepositoryAccess},
    types::{
        parse_revision, valid_positive_id, ChangeVisibilityInput, ChatDto, CreateChatInput,
        RepositoryDefaultInput, Visibility,
    },
};
use crate::{
    db::entities::{
        team_catalog_admission, workspace_chat_draft, workspace_chat_execution,
        workspace_repository, workspace_writer_lease,
    },
    team_access::principal::AuthContext,
    team_server::{handlers::ApiError, state::TeamState},
    workspace_catalog::{
        request_fingerprint::{fingerprint, CreationAudience, FingerprintVersion},
        validate_branch, RepositoryDefault,
    },
};

const PAGE_SIZE: usize = 200;
const CREATOR_CHAT_CAP: u64 = 1000;

struct NormalizedInput {
    node_id: i32,
    repository_override: Option<RepositoryDefaultInput>,
    title: Option<String>,
    request_id: String,
    visibility: Visibility,
    fingerprint: String,
}

fn normalize(input: CreateChatInput) -> Result<NormalizedInput, ApiError> {
    if !valid_positive_id(input.node_id)
        || input
            .repository_override
            .as_ref()
            .is_some_and(|value| !valid_positive_id(value.repository_id))
    {
        return Err(ApiError::InvalidRequest);
    }
    let request_id = Uuid::parse_str(&input.request_id)
        .map_err(|_| ApiError::InvalidRequest)?
        .to_string();
    if request_id != input.request_id {
        return Err(ApiError::InvalidRequest);
    }
    let title = input.title.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    });
    if title.as_ref().is_some_and(|value| value.chars().count() > 200) {
        return Err(ApiError::InvalidRequest);
    }
    if let Some(default) = &input.repository_override {
        validate_branch(&default.starting_branch).map_err(|_| ApiError::InvalidRequest)?;
    }
    let legacy_default = input
        .repository_override
        .as_ref()
        .map(|value| RepositoryDefault {
            repository_id: value.repository_id,
            starting_branch: value.starting_branch.clone(),
        });
    let fingerprint = fingerprint(
        FingerprintVersion::AudienceV2,
        input.node_id,
        title.as_deref(),
        legacy_default.as_ref(),
        match input.visibility {
            Visibility::Private => CreationAudience::Private,
            Visibility::Organization => CreationAudience::Organization,
        },
    )
    .map_err(|_| ApiError::Conflict)?;
    Ok(NormalizedInput {
        node_id: input.node_id,
        repository_override: input.repository_override,
        title,
        request_id,
        visibility: input.visibility,
        fingerprint,
    })
}

async fn require_repository(
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

async fn find_request(
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    request_id: &str,
) -> Result<Option<workspace_chat_draft::Model>, ApiError> {
    workspace_chat_draft::Entity::find()
        .filter(workspace_chat_draft::Column::OrgId.eq(context.org_id))
        .filter(workspace_chat_draft::Column::CreatorUserId.eq(context.user_id))
        .filter(workspace_chat_draft::Column::RequestId.eq(request_id))
        .one(transaction)
        .await
        .map_err(|_| ApiError::Unavailable)
}

async fn retry_existing(
    state: &TeamState,
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    input: &NormalizedInput,
    transaction: DatabaseTransaction,
    existing: workspace_chat_draft::Model,
) -> Result<ChatDto, ApiError> {
    require_admission(&transaction, CHAT, existing.id, context.org_id).await?;
    if existing.request_fingerprint != input.fingerprint {
        return Err(ApiError::Conflict);
    }
    if project_valid(state, &transaction, context, existing.clone())
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    let repository = require_repository(&transaction, context, existing.repository_id).await?;
    if repository.github_repository_id != existing.github_repository_id {
        return Err(ApiError::NotFound);
    }
    commit_rechecked(state, context, transaction).await?;
    let verified = access
        .verify(context, &repository.full_name, &existing.starting_branch)
        .await?;
    if verified.github_repository_id != existing.github_repository_id
        || verified.starting_branch != existing.starting_branch
    {
        return Err(ApiError::NotFound);
    }
    let final_transaction = begin_authorized(state, context).await?;
    let current = find_request(&final_transaction, context, &input.request_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    require_admission(&final_transaction, CHAT, current.id, context.org_id).await?;
    if current.id != existing.id || current.request_fingerprint != input.fingerprint {
        return Err(ApiError::Conflict);
    }
    let current_repository = require_repository(&final_transaction, context, current.repository_id).await?;
    if current_repository.github_repository_id != verified.github_repository_id {
        return Err(ApiError::Conflict);
    }
    let projected = project_valid(state, &final_transaction, context, current)
        .await?
        .ok_or(ApiError::NotFound)?;
    commit_rechecked(state, context, final_transaction).await?;
    Ok(projected)
}

async fn can_share(
    state: &TeamState,
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    row: &workspace_chat_draft::Model,
) -> Result<bool, ApiError> {
    if row.creator_user_id != context.user_id || row.execution_state != "unprovisioned" {
        return Ok(false);
    }
    let execution = workspace_chat_execution::Entity::find_by_id(row.id)
        .one(transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .is_some();
    let writer = workspace_writer_lease::Entity::find_by_id(row.id)
        .one(transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .is_some_and(|lease| lease.expires_at > state.clock.now());
    Ok(!execution && !writer)
}

async fn project_valid(
    state: &TeamState,
    transaction: &DatabaseTransaction,
    context: &AuthContext,
    row: workspace_chat_draft::Model,
) -> Result<Option<ChatDto>, ApiError> {
    let Some(admitted) = super::authority::admission(transaction, CHAT, row.id, context.org_id).await?
    else {
        return Ok(None);
    };
    let Some(visibility) = Visibility::parse(&row.visibility) else {
        return Ok(None);
    };
    if row.org_id != context.org_id
        || admitted.creator_user_id != row.creator_user_id
        || (row.creator_user_id != context.user_id && visibility != Visibility::Organization)
        || row.access_revision < 0
        || row.creation_fingerprint_version != 2
        || Visibility::parse(&row.creation_visibility).is_none()
        || validate_branch(&row.starting_branch).is_err()
    {
        return Ok(None);
    }
    match load_valid_node(transaction, context, row.node_id).await {
        Ok(_) => {}
        Err(ApiError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    }
    let repository = match require_repository(transaction, context, row.repository_id).await {
        Ok(repository) => repository,
        Err(ApiError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    if repository.github_repository_id != row.github_repository_id {
        return Ok(None);
    }
    let shareable = can_share(state, transaction, context, &row).await?;
    Ok(Some(ChatDto {
        id: row.id,
        node_id: row.node_id,
        title: row.title,
        repository_id: row.repository_id,
        github_repository_id: row.github_repository_id,
        full_name: row.full_name,
        starting_branch: row.starting_branch,
        visibility,
        access_revision: row.access_revision.to_string(),
        can_share: shareable,
        execution_state: row.execution_state,
    }))
}

pub(in crate::team_server) async fn read_chat(
    state: &TeamState,
    context: &AuthContext,
    chat_id: i32,
) -> Result<ChatDto, ApiError> {
    if !valid_positive_id(chat_id) {
        return Err(ApiError::NotFound);
    }
    let transaction = begin_authorized(state, context).await?;
    let row = workspace_chat_draft::Entity::find_by_id(chat_id)
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .ok_or(ApiError::NotFound)?;
    let result = project_valid(state, &transaction, context, row)
        .await?
        .ok_or(ApiError::NotFound)?;
    commit_rechecked(state, context, transaction).await?;
    Ok(result)
}

pub(in crate::team_server) async fn list_chats(
    state: &TeamState,
    context: &AuthContext,
    node_id: Option<i32>,
    after_id: Option<i32>,
) -> Result<Vec<ChatDto>, ApiError> {
    if node_id.is_some_and(|id| !valid_positive_id(id))
        || after_id.is_some_and(|id| !valid_positive_id(id))
    {
        return Err(ApiError::InvalidRequest);
    }
    let transaction = begin_authorized(state, context).await?;
    if let Some(node_id) = node_id {
        load_valid_node(&transaction, context, node_id).await?;
    }
    let mut cursor = after_id.unwrap_or(0);
    let mut result = Vec::with_capacity(PAGE_SIZE);
    while result.len() < PAGE_SIZE {
        let mut sql = String::from(
            "SELECT d.* FROM workspace_chat_draft d WHERE d.org_id = $1 AND d.id > $2 AND (d.creator_user_id = $3 OR d.visibility = 'organization') AND EXISTS (SELECT 1 FROM team_catalog_admission a WHERE a.resource_kind = 'chat' AND a.resource_id = d.id AND a.org_id = d.org_id AND a.creator_user_id = d.creator_user_id)",
        );
        let mut values = vec![context.org_id.into(), cursor.into(), context.user_id.into()];
        if let Some(node_id) = node_id {
            sql.push_str(" AND d.node_id = $4");
            values.push(node_id.into());
        }
        sql.push_str(" ORDER BY d.id ASC LIMIT 200");
        let batch = workspace_chat_draft::Model::find_by_statement(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            sql,
            values,
        ))
        .all(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len();
        for row in batch {
            cursor = row.id;
            if let Some(projected) = project_valid(state, &transaction, context, row).await? {
                result.push(projected);
                if result.len() == PAGE_SIZE {
                    break;
                }
            }
        }
        if batch_len < PAGE_SIZE {
            break;
        }
    }
    commit_rechecked(state, context, transaction).await?;
    Ok(result)
}

pub(in crate::team_server) async fn create_chat(
    state: &TeamState,
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    input: CreateChatInput,
) -> Result<ChatDto, ApiError> {
    let input = normalize(input)?;
    let preflight = begin_authorized(state, context).await?;
    if let Some(existing) = find_request(&preflight, context, &input.request_id).await? {
        return retry_existing(state, context, access, &input, preflight, existing).await;
    }
    let valid_node = load_valid_node(&preflight, context, input.node_id).await?;
    let selected = input
        .repository_override
        .clone()
        .or(valid_node.resolved_default)
        .ok_or(ApiError::InvalidRequest)?;
    let repository = require_repository(&preflight, context, selected.repository_id).await?;
    commit_rechecked(state, context, preflight).await?;

    let verified = access
        .verify(context, &repository.full_name, &selected.starting_branch)
        .await?;
    if verified.github_repository_id != repository.github_repository_id
        || verified.starting_branch != selected.starting_branch
    {
        return Err(ApiError::NotFound);
    }

    let transaction = begin_authorized(state, context).await?;
    if let Some(existing) = find_request(&transaction, context, &input.request_id).await? {
        if existing.request_fingerprint != input.fingerprint {
            return Err(ApiError::Conflict);
        }
        require_admission(&transaction, CHAT, existing.id, context.org_id).await?;
        let current_repository = require_repository(&transaction, context, existing.repository_id).await?;
        if existing.github_repository_id != verified.github_repository_id
            || existing.starting_branch != verified.starting_branch
            || current_repository.github_repository_id != verified.github_repository_id
        {
            return Err(ApiError::Conflict);
        }
        let projected = project_valid(state, &transaction, context, existing)
            .await?
            .ok_or(ApiError::NotFound)?;
        commit_rechecked(state, context, transaction).await?;
        return Ok(projected);
    }
    let current_node = load_valid_node(&transaction, context, input.node_id).await?;
    let current_selected = input
        .repository_override
        .clone()
        .or(current_node.resolved_default)
        .ok_or(ApiError::InvalidRequest)?;
    if current_selected != selected {
        return Err(ApiError::Conflict);
    }
    let current_repository = require_repository(&transaction, context, selected.repository_id).await?;
    if current_repository.github_repository_id != verified.github_repository_id {
        return Err(ApiError::Conflict);
    }
    let count = team_catalog_admission::Entity::find()
        .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
        .filter(team_catalog_admission::Column::ResourceKind.eq(CHAT))
        .filter(team_catalog_admission::Column::CreatorUserId.eq(context.user_id))
        .count(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    if count >= CREATOR_CHAT_CAP {
        return Err(ApiError::Conflict);
    }
    let row = workspace_chat_draft::ActiveModel {
        id: NotSet,
        org_id: Set(context.org_id),
        node_id: Set(input.node_id),
        creator_user_id: Set(context.user_id),
        repository_id: Set(selected.repository_id),
        github_repository_id: Set(verified.github_repository_id),
        full_name: Set(verified.full_name),
        starting_branch: Set(verified.starting_branch),
        title: Set(input.title),
        request_id: Set(input.request_id),
        request_fingerprint: Set(input.fingerprint),
        execution_state: Set("unprovisioned".into()),
        visibility: Set(input.visibility.as_str().into()),
        access_revision: Set(0),
        creation_visibility: Set(input.visibility.as_str().into()),
        creation_fingerprint_version: Set(2),
        created_at: Set(state.clock.now()),
        updated_at: Set(state.clock.now()),
    }
    .insert(&transaction)
    .await
    .map_err(|_| ApiError::Unavailable)?;
    admit(state, &transaction, context, CHAT, row.id).await?;
    let projected = project_valid(state, &transaction, context, row)
        .await?
        .ok_or(ApiError::Unavailable)?;
    commit_rechecked(state, context, transaction).await?;
    Ok(projected)
}

pub(in crate::team_server) async fn change_visibility(
    state: &TeamState,
    context: &AuthContext,
    chat_id: i32,
    input: ChangeVisibilityInput,
) -> Result<ChatDto, ApiError> {
    let expected = parse_revision(&input.expected_revision).ok_or(ApiError::InvalidRequest)?;
    if !valid_positive_id(chat_id) {
        return Err(ApiError::NotFound);
    }
    let transaction = begin_authorized(state, context).await?;
    let admitted = require_admission(&transaction, CHAT, chat_id, context.org_id).await?;
    let row = workspace_chat_draft::Entity::find_by_id(chat_id)
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .filter(|row| {
            row.org_id == context.org_id
                && row.creator_user_id == context.user_id
                && admitted.creator_user_id == context.user_id
        })
        .ok_or(ApiError::NotFound)?;
    if row.access_revision != expected {
        return Err(ApiError::Conflict);
    }
    if Visibility::parse(&row.visibility).is_none() {
        return Err(ApiError::Unavailable);
    }
    if input.visibility == Visibility::Organization
        && !can_share(state, &transaction, context, &row).await?
    {
        return Err(ApiError::Conflict);
    }
    let next = row.access_revision.checked_add(1).ok_or(ApiError::Conflict)?;
    let result = workspace_chat_draft::Entity::update_many()
        .col_expr(
            workspace_chat_draft::Column::Visibility,
            Expr::value(input.visibility.as_str()),
        )
        .col_expr(
            workspace_chat_draft::Column::AccessRevision,
            Expr::value(next),
        )
        .col_expr(
            workspace_chat_draft::Column::UpdatedAt,
            Expr::value(state.clock.now()),
        )
        .filter(workspace_chat_draft::Column::Id.eq(chat_id))
        .filter(workspace_chat_draft::Column::OrgId.eq(context.org_id))
        .filter(workspace_chat_draft::Column::CreatorUserId.eq(context.user_id))
        .filter(workspace_chat_draft::Column::AccessRevision.eq(expected))
        .exec(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    if result.rows_affected != 1 {
        return Err(ApiError::Conflict);
    }
    let updated = workspace_chat_draft::Entity::find_by_id(chat_id)
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
        .ok_or(ApiError::Unavailable)?;
    let projected = project_valid(state, &transaction, context, updated)
        .await?
        .ok_or(ApiError::Unavailable)?;
    commit_rechecked(state, context, transaction).await?;
    Ok(projected)
}
