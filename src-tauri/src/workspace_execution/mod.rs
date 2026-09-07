mod types;
mod state;
pub mod legacy_bridge;
pub mod git_allocator;
pub mod provisioning;
pub mod repository_access;
mod managed_paths;
pub mod writer;
pub mod acp_guard;
pub(crate) mod acp_runner;
pub use types::{ExecutionError, WorkspaceChatRuntime, WorkspaceExecutionState};
pub use state::{get_runtime, get_runtime_for_conversation, reserve};
pub use repository_access::{ExecutionRepositoryAccess, GitHubExecutionRepositoryAccess, VerifiedExecutionIdentity, VerifiedExecutionRepository};
pub use git_allocator::{AllocatedWorktree, GitAllocator, ManagedGitAllocator};
pub use provisioning::{provision, verify_draft_access};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod provisioning_tests;
#[cfg(test)]
mod writer_tests;
#[cfg(test)]
mod browser_fixture;
