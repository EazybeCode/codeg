//! Logical workspace ancestry is distinct from a physical Git checkout.
use crate::{app_error::AppCommandError, db::AppDatabase, models::FolderDetail};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct FolderRepoContext {
    pub folder_id: i32,
    pub project_folder_id: i32,
    pub repo_folder_id: i32,
    pub repo_full_name: Option<String>,
    pub branch: Option<String>,
    pub relative_path: String,
    pub is_inherited: bool,
    pub git_root: Option<String>,
}

pub async fn get_folder_repo_context(db: &AppDatabase, folder_id: i32) -> Result<FolderRepoContext, AppCommandError> {
    let folders = crate::commands::folders::list_all_folder_details_core(db).await?;
    resolve_context(folder_id, &folders).await
}

async fn git_value(path: &std::path::Path, args: &[&str]) -> Result<Option<String>, AppCommandError> {
    let output = crate::process::tokio_command("git")
        .current_dir(path).args(args).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR").output().await.map_err(AppCommandError::io)?;
    Ok(output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()).filter(|value| !value.is_empty()))
}

/// Return only the public owner/repository identity, never URL credentials.
pub fn github_repo_name(remote: &str) -> Option<String> {
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path.to_string()
    } else {
        let url = reqwest::Url::parse(remote).ok()?;
        if url.host_str()? != "github.com" || !matches!(url.scheme(), "https" | "ssh") { return None; }
        url.path().trim_start_matches('/').to_string()
    };
    let name = path.strip_suffix(".git").unwrap_or(&path);
    let (owner, repo) = name.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo == "." || repo == ".."
        || !owner.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
        || !repo.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"-_.".contains(&ch)) { return None; }
    Some(name.to_string())
}

async fn resolve_context(folder_id: i32, folders: &[FolderDetail]) -> Result<FolderRepoContext, AppCommandError> {
    let by_id: std::collections::HashMap<_, _> = folders.iter().map(|folder| (folder.id, folder)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut chain = Vec::new();
    let mut current = Some(folder_id);
    while let Some(id) = current {
        if !seen.insert(id) || chain.len() >= 256 {
            return Err(AppCommandError::invalid_input("Invalid folder ancestry"));
        }
        let folder = by_id.get(&id).ok_or_else(|| AppCommandError::not_found("Folder or ancestor not found"))?;
        chain.push(*folder);
        current = folder.parent_id;
    }
    let folder = chain[0];
    let path = std::fs::canonicalize(&folder.path).map_err(AppCommandError::io)?;
    let mut context = FolderRepoContext {
        folder_id, project_folder_id: chain.last().unwrap().id, repo_folder_id: folder_id,
        repo_full_name: None, branch: None, relative_path: String::new(), is_inherited: false, git_root: None,
    };
    let Some(root) = git_value(&path, &["rev-parse", "--show-toplevel"]).await? else { return Ok(context); };
    let root = std::fs::canonicalize(root).map_err(AppCommandError::io)?;
    // Never hand Git controls an unrelated folder ID, even if Git discovers a
    // checkout above an unregistered legacy folder or across a symlink.
    let repo_folder = chain.iter().find(|candidate| std::fs::canonicalize(&candidate.path).ok().as_ref() == Some(&root))
        .ok_or_else(|| AppCommandError::invalid_input("The Git root is not a registered ancestor"))?;
    let relative = path.strip_prefix(&root).map_err(|_| AppCommandError::invalid_input("Folder is outside its repository"))?;
    context.repo_folder_id = repo_folder.id;
    context.is_inherited = repo_folder.id != folder_id;
    context.relative_path = relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    context.repo_full_name = git_value(&root, &["remote", "get-url", "origin"]).await?.and_then(|remote| github_repo_name(&remote));
    if let Some(expected) = &repo_folder.repo_full_name {
        if !context.repo_full_name.as_deref().is_some_and(|actual| actual.eq_ignore_ascii_case(expected)) {
            return Err(AppCommandError::invalid_input("Repository binding does not match its checkout"));
        }
    }
    context.branch = git_value(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    context.git_root = Some(root.to_string_lossy().into_owned());
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::folder::FolderKind;
    use std::path::Path;

    fn folder(id: i32, parent_id: Option<i32>, path: &Path) -> FolderDetail {
        FolderDetail { id, parent_id, name: format!("folder-{id}"), path: path.to_string_lossy().into_owned(),
            git_branch: None, default_agent_type: None, last_opened_at: chrono::Utc::now(),
            sort_order: 0, color: "blue".into(), kind: if parent_id.is_some() { FolderKind::Subfolder } else { FolderKind::Regular },
            alias: None, group_id: None, repo_full_name: None }
    }

    fn git(path: &Path, args: &[&str]) {
        assert!(std::process::Command::new("git").current_dir(path).args(args).output().unwrap().status.success());
    }

    fn repo(path: &Path, remote: &str) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
        git(path, &["remote", "add", "origin", remote]);
    }

    #[tokio::test]
    async fn nested_directory_inherits_actual_repo_and_unborn_branch() {
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path(), "https://github.com/acme/docs.git");
        let nested = dir.path().join("Analytics/Test1");
        std::fs::create_dir_all(&nested).unwrap();
        let folders = vec![folder(1, None, dir.path()), folder(2, Some(1), &dir.path().join("Analytics")), folder(3, Some(2), &nested)];
        let context = resolve_context(3, &folders).await.unwrap();
        assert_eq!((context.folder_id, context.project_folder_id, context.repo_folder_id), (3, 1, 1));
        assert_eq!(context.repo_full_name.as_deref(), Some("acme/docs"));
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(context.relative_path, "Analytics/Test1");
        assert!(context.is_inherited);
    }

    #[tokio::test]
    async fn nested_override_keeps_project_but_uses_own_checkout() {
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path(), "git@github.com:acme/docs.git");
        let child = dir.path().join("wiki");
        repo(&child, "https://user:secret@github.com/acme/wiki.git");
        let mut override_folder = folder(2, Some(1), &child);
        override_folder.repo_full_name = Some("acme/wiki".into());
        let context = resolve_context(2, &[folder(1, None, dir.path()), override_folder]).await.unwrap();
        assert_eq!((context.project_folder_id, context.repo_folder_id), (1, 2));
        assert_eq!(context.repo_full_name.as_deref(), Some("acme/wiki"));
        assert_eq!(context.relative_path, "");
        assert!(!context.is_inherited);
        assert!(!serde_json::to_string(&context).unwrap().contains("secret"));
    }

    #[tokio::test]
    async fn worktree_uses_own_runtime_and_parent_project() {
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path(), "git@github.com:acme/docs.git");
        git(dir.path(), &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-m", "init"]);
        let worktree = dir.path().join("checkout");
        git(dir.path(), &["worktree", "add", "-b", "feature", worktree.to_str().unwrap()]);
        let mut runtime = folder(2, Some(1), &worktree);
        runtime.kind = FolderKind::Regular;
        let context = resolve_context(2, &[folder(1, None, dir.path()), runtime]).await.unwrap();
        assert_eq!((context.project_folder_id, context.repo_folder_id), (1, 2));
        assert_eq!(context.branch.as_deref(), Some("feature"));
    }

    #[tokio::test]
    async fn non_git_legacy_folder_has_no_repo() {
        let dir = tempfile::tempdir().unwrap();
        let context = resolve_context(1, &[folder(1, None, dir.path())]).await.unwrap();
        assert!(context.git_root.is_none());
        assert!(context.repo_full_name.is_none());
    }

    #[tokio::test]
    async fn missing_and_cyclic_ancestry_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_context(99, &[]).await.is_err());
        assert!(resolve_context(1, &[folder(1, Some(2), dir.path())]).await.is_err());
        assert!(resolve_context(1, &[folder(1, Some(2), dir.path()), folder(2, Some(1), dir.path())]).await.is_err());
    }
}
