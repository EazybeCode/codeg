mod authority;
mod chats;
mod nodes;
mod repository;
mod types;

#[allow(unused_imports)]
pub(in crate::team_server) use chats::{change_visibility, create_chat, list_chats, read_chat};
#[allow(unused_imports)]
pub(in crate::team_server) use nodes::{create_node, list_nodes, update_node};
#[allow(unused_imports)]
pub(in crate::team_server) use repository::{
    connect_repository, list_repositories, GithubTeamRepositoryAccess, TeamRepositoryAccess,
    VerifiedRepository,
};
#[allow(unused_imports)]
pub(in crate::team_server) use types::{
    ChangeVisibilityInput, ChatDto, ConnectRepositoryInput, CreateChatInput, CreateNodeInput,
    NodeDto, RepositoryDefaultInput, RepositoryDto, UpdateNodeInput, Visibility,
    WorkspaceNodeKind,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod repository_tests;
#[cfg(test)]
mod matrix_tests;
