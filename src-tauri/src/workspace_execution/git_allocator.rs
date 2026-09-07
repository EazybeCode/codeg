use std::path::{Path, PathBuf};
use async_trait::async_trait;
use crate::{workspace_catalog::Scope, db::entities::workspace_chat_execution};
use super::{ExecutionError, repository_access::VerifiedExecutionRepository};
#[derive(Debug)]
pub struct AllocatedWorktree { pub allocation_id: String, pub worktree_path: PathBuf, pub branch_name: String }
#[async_trait]
pub trait GitAllocator: Send + Sync {
    async fn allocate(&self, scope: &Scope, reservation: &workspace_chat_execution::Model, repository: &VerifiedExecutionRepository) -> Result<AllocatedWorktree, ExecutionError>;
}
pub struct ManagedGitAllocator {
    root: PathBuf,
    credentials: crate::workspace_catalog::GitHubRepositoryAccess,
}
impl ManagedGitAllocator {
    pub fn new(root: PathBuf, db: &sea_orm::DatabaseConnection) -> Result<Self, ExecutionError> {
        Ok(Self { root, credentials: crate::workspace_catalog::GitHubRepositoryAccess::new(db)? })
    }
}
#[async_trait]
impl GitAllocator for ManagedGitAllocator {
    async fn allocate(&self, scope: &Scope, reservation: &workspace_chat_execution::Model, repository: &VerifiedExecutionRepository) -> Result<AllocatedWorktree, ExecutionError> {
        if scope.org_id != reservation.org_id { return Err(ExecutionError::NotFound); }
        crate::workspace_catalog::validate_repository_full_name(&repository.full_name)?;
        let token = self.credentials.token(scope).await?;
        let remote = format!("https://github.com/{}.git", repository.full_name);
        let root = self.root.clone();
        let reservation = reservation.clone();
        let repository = repository.clone();
        tokio::task::spawn_blocking(move || allocate_git(&root, &reservation, &repository, &remote, Some(&token)))
            .await.map_err(|_| ExecutionError::Failed)?
    }
}

#[cfg(test)]
pub(crate) fn allocate_local(root: &Path, reservation: &workspace_chat_execution::Model, repository: &VerifiedExecutionRepository, remote: &str) -> Result<AllocatedWorktree, ExecutionError> {
    allocate_git(root, reservation, repository, remote, None)
}

struct Git { home: PathBuf, askpass: Option<PathBuf>, token: Option<String> }
impl Git {
    fn run(&self, directory: &Path, args: &[&str]) -> Result<String, ExecutionError> {
        let mut command = std::process::Command::new("git");
        // Remove directory overrides, injected config, tracing and askpass from
        // the server process. Managed Git has no ambient account fallback.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("GIT_") || name.starts_with("CODEG_GIT_") || name == "SSH_ASKPASS" {
                command.env_remove(key);
            }
        }
        command.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_TERMINAL_PROMPT", "0")
            .env("HOME", &self.home).env("XDG_CONFIG_HOME", &self.home)
            .args(["-c", "core.hooksPath=/dev/null", "-c", "credential.helper=", "-c", "http.followRedirects=false", "-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=60", "-c", "submodule.recurse=false"])
            .current_dir(directory).args(args);
        if let (Some(token), Some(askpass)) = (&self.token, &self.askpass) {
            crate::git_credential::inject_credentials_std(&mut command, "x-access-token", token, askpass);
        } else {
            command.env("GIT_ASKPASS", "/usr/bin/false");
        }
        let output = command.output().map_err(|_| ExecutionError::Failed)?;
        if !output.status.success() { return Err(ExecutionError::Failed); }
        String::from_utf8(output.stdout).map(|s| s.trim().to_owned()).map_err(|_| ExecutionError::Failed)
    }
}

fn allocate_git(root: &Path, row: &workspace_chat_execution::Model, repo: &VerifiedExecutionRepository, remote: &str, token: Option<&str>) -> Result<AllocatedWorktree, ExecutionError> {
    use super::managed_paths::{checked, cache_lock};
    use std::fs;
    if row.org_id <= 0 || row.draft_id <= 0 || repo.github_repository_id.is_empty()
        || !repo.github_repository_id.bytes().all(|b| b.is_ascii_digit())
        || row.branch_name != format!("workos/chat/{}", row.draft_id)
        || row.worktree_relpath != format!("orgs/{}/workspace-chats/{}/worktree", row.org_id, row.draft_id)
        || row.starting_commit != repo.starting_commit
        || !matches!(row.starting_commit.len(), 40 | 64)
        || !row.starting_commit.bytes().all(|b| b.is_ascii_hexdigit())
    { return Err(ExecutionError::AllocationMismatch); }
    let parent = PathBuf::from(format!("orgs/{}/repositories/{}", row.org_id, repo.github_repository_id));
    let cache = checked(root, &parent.join("repo.git"))?;
    let worktree = checked(root, Path::new(&row.worktree_relpath))?;
    // Reject unrelated existing directories before any Git metadata mutation.
    if worktree.exists() && (!worktree.is_dir() || !worktree.join(".git").is_file()) {
        return Err(ExecutionError::AllocationMismatch);
    }
    if worktree.exists() { checked(root, &PathBuf::from(&row.worktree_relpath).join(".git"))?; }
    let _lock = cache_lock(root, &parent)?;
    let home = checked(root, &parent.join("git-home"))?;
    fs::create_dir_all(&home).map_err(|_| ExecutionError::AllocationMismatch)?;
    #[cfg(unix)] let helper = "git-askpass.sh";
    #[cfg(windows)] let helper = "git-askpass.bat";
    for entry in fs::read_dir(&home).map_err(|_| ExecutionError::AllocationMismatch)? {
        let entry = entry.map_err(|_| ExecutionError::AllocationMismatch)?;
        // This HOME must not contain .netrc or other ambient credential files.
        if entry.file_name() != helper { return Err(ExecutionError::AllocationMismatch); }
        let path = checked(root, &parent.join("git-home").join(helper))?;
        if fs::read_to_string(path).map_err(|_| ExecutionError::AllocationMismatch)? != crate::git_credential::askpass_script_content() {
            return Err(ExecutionError::AllocationMismatch);
        }
    }
    let askpass = if token.is_some() {
        // A dedicated cache-local helper, validated before the shared primitive
        // can create it. No secret is written to the helper or remote URL.
        checked(root, &parent.join("git-home").join(helper))?;
        Some(crate::git_credential::ensure_askpass_script(&home).map_err(|_| ExecutionError::Failed)?)
    } else { None };
    let git = Git { home, askpass, token: token.map(Into::into) };
    if cache.exists() {
        if !cache.is_dir() { return Err(ExecutionError::AllocationMismatch); }
        super::managed_paths::reject_metadata_symlinks(&cache)?;
        for name in ["config", "HEAD", "objects", "refs"] { checked(root, &parent.join("repo.git").join(name))?; }
        if git.run(&cache, &["rev-parse", "--is-bare-repository"]).ok().as_deref() != Some("true")
        { return Err(ExecutionError::AllocationMismatch); }
        reconcile_remote(&git, &cache, remote, &repo.verified_names)?;
    } else {
        if worktree.exists() { return Err(ExecutionError::AllocationMismatch); }
        // Rename a complete clone while holding the filesystem cache lock. A
        // killed clone leaves a separate staging directory, never a fake cache.
        let staging = checked(root, &parent.join(format!("clone-{}", uuid::Uuid::new_v4())))?;
        git.run(cache.parent().unwrap(), &["clone", "--bare", "--no-local", "--", remote, path_str(&staging)?])?;
        fs::rename(&staging, &cache).map_err(|_| ExecutionError::AllocationMismatch)?;
    }
    if worktree.exists() {
        verify_checkout(&git, &cache, &worktree, row)?;
    } else {
        if git.run(&cache, &["cat-file", "-e", &format!("{}^{{commit}}", row.starting_commit)]).is_err() {
            git.run(&cache, &["fetch", "--no-tags", "--no-recurse-submodules", "origin", &row.starting_commit])?;
            git.run(&cache, &["cat-file", "-e", &format!("{}^{{commit}}", row.starting_commit)])?;
        }
        let branch_ref = format!("refs/heads/{}", row.branch_name);
        match git.run(&cache, &["rev-parse", "--verify", &branch_ref]) {
            Ok(commit) if commit != row.starting_commit => return Err(ExecutionError::AllocationMismatch),
            Ok(_) => {},
            Err(_) => { git.run(&cache, &["branch", "--", &row.branch_name, &row.starting_commit])?; },
        }
        fs::create_dir_all(worktree.parent().unwrap()).map_err(|_| ExecutionError::AllocationMismatch)?;
        git.run(&cache, &["worktree", "add", "--", path_str(&worktree)?, &row.branch_name])?;
        verify_checkout(&git, &cache, &worktree, row)?;
    }
    Ok(AllocatedWorktree { allocation_id: row.allocation_id.clone(), worktree_path: worktree, branch_name: row.branch_name.clone() })
}
fn path_str(path: &Path) -> Result<&str, ExecutionError> { path.to_str().ok_or(ExecutionError::AllocationMismatch) }
/// Authorized writer-start validation may reconcile verified remote metadata
/// under the cache lock. It never changes a ready checkout's branch or HEAD.
pub(crate) fn validate_ready_checkout(root: &Path, row: &workspace_chat_execution::Model, repo: &super::VerifiedExecutionIdentity) -> Result<PathBuf, ExecutionError> {
    use super::managed_paths::{checked, reject_metadata_symlinks};
    if row.state != "ready" || row.org_id <= 0 || row.draft_id <= 0
        || repo.github_repository_id.is_empty() || !repo.github_repository_id.bytes().all(|b| b.is_ascii_digit())
        || row.branch_name != format!("workos/chat/{}", row.draft_id)
        || row.worktree_relpath != format!("orgs/{}/workspace-chats/{}/worktree", row.org_id, row.draft_id)
    { return Err(ExecutionError::AllocationMismatch); }
    crate::workspace_catalog::validate_repository_full_name(&repo.full_name)?;
    let parent = PathBuf::from(format!("orgs/{}/repositories/{}", row.org_id, repo.github_repository_id));
    let _lock = super::managed_paths::cache_lock(root, &parent)?;
    let cache = checked(root, &parent.join("repo.git"))?;
    let worktree = checked(root, Path::new(&row.worktree_relpath))?;
    checked(root, &PathBuf::from(&row.worktree_relpath).join(".git"))?;
    if !worktree.is_dir() || !worktree.join(".git").is_file() || !cache.is_dir() { return Err(ExecutionError::AllocationMismatch); }
    reject_metadata_symlinks(&cache)?;
    let git = Git { home: checked(root, &parent.join("git-home"))?, askpass: None, token: None };
    let remote = format!("https://github.com/{}.git", repo.full_name);
    if git.run(&cache, &["rev-parse", "--is-bare-repository"]).ok().as_deref() != Some("true")
    { return Err(ExecutionError::AllocationMismatch); }
    reconcile_remote(&git, &cache, &remote, &repo.verified_names)?;
    verify_checkout(&git, &cache, &worktree, row)?;
    Ok(worktree)
}
fn reconcile_remote(git: &Git, cache: &Path, remote: &str, verified_names: &[String]) -> Result<(), ExecutionError> {
    if verified_names.is_empty() || verified_names.iter().any(|name| crate::workspace_catalog::validate_repository_full_name(name).is_err()) {
        return Err(ExecutionError::AllocationMismatch);
    }
    let origin = git.run(cache, &["config", "--get", "remote.origin.url"]).map_err(|_| ExecutionError::AllocationMismatch)?;
    if origin == remote { return Ok(()); }
    if !verified_names.iter().any(|name| origin == format!("https://github.com/{name}.git")) {
        return Err(ExecutionError::AllocationMismatch);
    }
    git.run(cache, &["remote", "set-url", "origin", remote])?;
    Ok(())
}
fn verify_checkout(git: &Git, cache: &Path, worktree: &Path, row: &workspace_chat_execution::Model) -> Result<(), ExecutionError> {
    let common = git.run(worktree, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).map_err(|_| ExecutionError::AllocationMismatch)?;
    let common = Path::new(&common).canonicalize().map_err(|_| ExecutionError::AllocationMismatch)?;
    let branch = git.run(worktree, &["symbolic-ref", "HEAD"]).map_err(|_| ExecutionError::AllocationMismatch)?;
    let head = git.run(worktree, &["rev-parse", "HEAD"]).map_err(|_| ExecutionError::AllocationMismatch)?;
    if common != cache || branch != format!("refs/heads/{}", row.branch_name)
        || (row.state != "ready" && head != row.starting_commit)
    { return Err(ExecutionError::AllocationMismatch); }
    Ok(())
}
