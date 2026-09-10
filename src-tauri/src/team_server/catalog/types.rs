use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::team_server) enum Visibility {
    #[default]
    Private,
    Organization,
}

impl Visibility {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Organization => "organization",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "private" => Some(Self::Private),
            "organization" => Some(Self::Organization),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::team_server) enum WorkspaceNodeKind {
    Project,
    Folder,
    WorkingDirectory,
}

impl WorkspaceNodeKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Folder => "folder",
            Self::WorkingDirectory => "working_directory",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "project" => Some(Self::Project),
            "folder" => Some(Self::Folder),
            "working_directory" => Some(Self::WorkingDirectory),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct RepositoryDefaultInput {
    pub(in crate::team_server) repository_id: i32,
    pub(in crate::team_server) starting_branch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct ConnectRepositoryInput {
    pub(in crate::team_server) full_name: String,
    pub(in crate::team_server) starting_branch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct CreateNodeInput {
    pub(in crate::team_server) parent_id: Option<i32>,
    pub(in crate::team_server) kind: WorkspaceNodeKind,
    pub(in crate::team_server) name: String,
    pub(in crate::team_server) repository_default: Option<RepositoryDefaultInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct UpdateNodeInput {
    pub(in crate::team_server) expected_revision: String,
    pub(in crate::team_server) parent_id: Option<i32>,
    pub(in crate::team_server) name: String,
    pub(in crate::team_server) repository_default: Option<RepositoryDefaultInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct CreateChatInput {
    pub(in crate::team_server) node_id: i32,
    pub(in crate::team_server) repository_override: Option<RepositoryDefaultInput>,
    pub(in crate::team_server) title: Option<String>,
    pub(in crate::team_server) request_id: String,
    #[serde(default)]
    pub(in crate::team_server) visibility: Visibility,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::team_server) struct ChangeVisibilityInput {
    pub(in crate::team_server) expected_revision: String,
    pub(in crate::team_server) visibility: Visibility,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::team_server) struct NodeDto {
    pub(in crate::team_server) id: i32,
    pub(in crate::team_server) parent_id: Option<i32>,
    pub(in crate::team_server) kind: WorkspaceNodeKind,
    pub(in crate::team_server) name: String,
    pub(in crate::team_server) repository_default: Option<RepositoryDefaultInput>,
    pub(in crate::team_server) revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::team_server) struct RepositoryDto {
    pub(in crate::team_server) id: i32,
    pub(in crate::team_server) github_repository_id: String,
    pub(in crate::team_server) full_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::team_server) struct ChatDto {
    pub(in crate::team_server) id: i32,
    pub(in crate::team_server) node_id: i32,
    pub(in crate::team_server) title: Option<String>,
    pub(in crate::team_server) repository_id: i32,
    pub(in crate::team_server) github_repository_id: String,
    pub(in crate::team_server) full_name: String,
    pub(in crate::team_server) starting_branch: String,
    pub(in crate::team_server) visibility: Visibility,
    pub(in crate::team_server) access_revision: String,
    pub(in crate::team_server) can_share: bool,
    pub(in crate::team_server) execution_state: String,
}

pub(super) fn parse_revision(value: &str) -> Option<i64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse::<i64>().ok().filter(|revision| *revision >= 0)
}

pub(super) fn valid_positive_id(value: i32) -> bool {
    value > 0
}
