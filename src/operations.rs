use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::{self, GitRunner};
use crate::model::{RepositoryConfig, Worktree, WorktreeStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateMode {
    ExistingBranch(String),
    NewBranch { branch: String, start_point: String },
    Detached(String),
}

impl CreateMode {
    pub fn label(&self) -> &str {
        match self {
            Self::ExistingBranch(branch) | Self::Detached(branch) => branch,
            Self::NewBranch { branch, .. } => branch,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorktreeDetails {
    pub repository: RepositoryConfig,
    pub worktree: Worktree,
    pub status: Option<WorktreeStatus>,
    pub status_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error("repository {0:?} has no worktrees")]
    EmptyRepository(String),
    #[error("worktree selector {0:?} did not match")]
    WorktreeNotFound(String),
    #[error("worktree selector {0:?} is ambiguous")]
    AmbiguousWorktree(String),
    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("destination is already registered as a worktree: {0}")]
    DestinationRegistered(PathBuf),
    #[error("destination parent does not exist: {0}; pass --create-parents")]
    MissingParent(PathBuf),
    #[error("failed to create destination parent {path}: {source}")]
    CreateParent {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("branch {0:?} does not exist")]
    MissingBranch(String),
    #[error("branch {0:?} already exists")]
    ExistingBranch(String),
    #[error("branch {0:?} is already checked out")]
    BranchCheckedOut(String),
    #[error("commit-ish {0:?} does not resolve to a commit")]
    InvalidCommit(String),
    #[error("cannot remove the bare repository anchor")]
    BareAnchor,
    #[error("cannot remove the main worktree")]
    MainWorktree,
    #[error("cannot remove the worktree containing the current directory")]
    CurrentWorktree,
    #[error("worktree is locked{reason}", reason = format_reason(.0))]
    Locked(Option<String>),
    #[error("worktree has local changes: {0}")]
    Dirty(String),
    #[error("cannot verify worktree status: {0}")]
    StatusUnavailable(String),
    #[error("typed confirmation must equal {expected:?} or {path}")]
    ConfirmationMismatch { expected: String, path: PathBuf },
}

fn format_reason(reason: &Option<String>) -> String {
    reason
        .as_ref()
        .filter(|reason| !reason.is_empty())
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default()
}

pub fn list(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<Vec<Worktree>, OperationError> {
    Ok(git::discover_worktrees(runner, &repository.path)?)
}

pub fn inspect(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
) -> Result<WorktreeDetails, OperationError> {
    let worktrees = list(runner, repository)?;
    let (_, worktree) = select_worktree(&worktrees, selector)?;
    let (status, status_error) = if worktree.navigable() && worktree.path.exists() {
        match git::status(runner, &worktree.path) {
            Ok(status) => (Some(status), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    Ok(WorktreeDetails {
        repository: repository.clone(),
        worktree: worktree.clone(),
        status,
        status_error,
    })
}

pub fn suggested_destination(repository: &RepositoryConfig, mode: &CreateMode) -> PathBuf {
    let parent = repository
        .worktree_root
        .as_deref()
        .unwrap_or_else(|| repository.path.parent().unwrap_or_else(|| Path::new(".")));
    parent.join(sanitize_name(mode.label()))
}

pub fn create(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    destination: &Path,
    mode: &CreateMode,
    create_parents: bool,
) -> Result<(), OperationError> {
    validate_create(runner, repository, destination, mode, create_parents)?;
    ensure_destination_parent(destination, create_parents)?;
    match mode {
        CreateMode::ExistingBranch(branch) => {
            git::run_git(
                runner,
                &repository.path,
                &[
                    OsString::from("worktree"),
                    OsString::from("add"),
                    destination.as_os_str().to_owned(),
                    OsString::from(branch),
                ],
            )?;
        }
        CreateMode::NewBranch {
            branch,
            start_point,
        } => {
            git::run_git(
                runner,
                &repository.path,
                &[
                    OsString::from("worktree"),
                    OsString::from("add"),
                    OsString::from("-b"),
                    OsString::from(branch),
                    destination.as_os_str().to_owned(),
                    OsString::from(start_point),
                ],
            )?;
        }
        CreateMode::Detached(commit) => {
            git::run_git(
                runner,
                &repository.path,
                &[
                    OsString::from("worktree"),
                    OsString::from("add"),
                    OsString::from("--detach"),
                    destination.as_os_str().to_owned(),
                    OsString::from(commit),
                ],
            )?;
        }
    }
    Ok(())
}

pub fn validate_create(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    destination: &Path,
    mode: &CreateMode,
    create_parents: bool,
) -> Result<(), OperationError> {
    let worktrees = list(runner, repository)?;
    validate_destination(&worktrees, destination)?;
    validate_destination_parent(destination, create_parents)?;
    match mode {
        CreateMode::ExistingBranch(branch) => {
            if !branch_exists(runner, &repository.path, branch)? {
                return Err(OperationError::MissingBranch(branch.clone()));
            }
            if branch_checked_out(&worktrees, branch) {
                return Err(OperationError::BranchCheckedOut(branch.clone()));
            }
        }
        CreateMode::NewBranch {
            branch,
            start_point,
        } => {
            if branch_exists(runner, &repository.path, branch)? {
                return Err(OperationError::ExistingBranch(branch.clone()));
            }
            validate_commit(runner, &repository.path, start_point)?;
        }
        CreateMode::Detached(commit) => {
            validate_commit(runner, &repository.path, commit)?;
        }
    }
    Ok(())
}

pub fn move_worktree(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    destination: &Path,
    create_parents: bool,
) -> Result<Worktree, OperationError> {
    let worktree = validate_move(runner, repository, selector, destination, create_parents)?;
    ensure_destination_parent(destination, create_parents)?;
    git::run_git(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("move"),
            worktree.path.as_os_str().to_owned(),
            destination.as_os_str().to_owned(),
        ],
    )?;
    Ok(worktree)
}

pub fn validate_move(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    destination: &Path,
    create_parents: bool,
) -> Result<Worktree, OperationError> {
    let worktrees = list(runner, repository)?;
    let (index, worktree) = select_worktree(&worktrees, selector)?;
    if index == 0 && !worktree.bare {
        return Err(OperationError::MainWorktree);
    }
    validate_destination(&worktrees, destination)?;
    validate_destination_parent(destination, create_parents)?;
    Ok(worktree.clone())
}

pub fn lock(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    reason: Option<&str>,
) -> Result<Worktree, OperationError> {
    let worktrees = list(runner, repository)?;
    let (_, worktree) = select_worktree(&worktrees, selector)?;
    let mut arguments = vec![OsString::from("worktree"), OsString::from("lock")];
    if let Some(reason) = reason {
        arguments.push(OsString::from("--reason"));
        arguments.push(OsString::from(reason));
    }
    arguments.push(worktree.path.as_os_str().to_owned());
    git::run_git(runner, &repository.path, &arguments)?;
    Ok(worktree.clone())
}

pub fn unlock(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
) -> Result<Worktree, OperationError> {
    let worktrees = list(runner, repository)?;
    let (_, worktree) = select_worktree(&worktrees, selector)?;
    git::run_git(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("unlock"),
            worktree.path.as_os_str().to_owned(),
        ],
    )?;
    Ok(worktree.clone())
}

pub fn repair(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    path: &Path,
) -> Result<(), OperationError> {
    git::run_git(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("repair"),
            path.as_os_str().to_owned(),
        ],
    )?;
    Ok(())
}

pub fn remove(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    current_directory: &Path,
) -> Result<WorktreeDetails, OperationError> {
    let details = removal_preview(runner, repository, selector, current_directory, false)?;
    git::run_git(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("remove"),
            details.worktree.path.as_os_str().to_owned(),
        ],
    )?;
    Ok(details)
}

pub fn force_remove(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    current_directory: &Path,
    confirmation: &str,
) -> Result<WorktreeDetails, OperationError> {
    let details = removal_preview(runner, repository, selector, current_directory, true)?;
    let expected = short_branch(&details.worktree)
        .unwrap_or_else(|| details.worktree.path.to_string_lossy().into_owned());
    if confirmation != expected && Path::new(confirmation) != details.worktree.path {
        return Err(OperationError::ConfirmationMismatch {
            expected,
            path: details.worktree.path.clone(),
        });
    }
    let mut arguments = vec![
        OsString::from("worktree"),
        OsString::from("remove"),
        OsString::from("--force"),
    ];
    if details.worktree.locked.is_some() {
        arguments.push(OsString::from("--force"));
    }
    arguments.push(details.worktree.path.as_os_str().to_owned());
    git::run_git(runner, &repository.path, &arguments)?;
    Ok(details)
}

pub fn removal_preview(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    current_directory: &Path,
    allow_dirty_or_locked: bool,
) -> Result<WorktreeDetails, OperationError> {
    let details = removal_details(runner, repository, selector, current_directory)?;
    if let Some(error) = &details.status_error {
        return Err(OperationError::StatusUnavailable(error.clone()));
    }
    if details.status.is_none() {
        return Err(OperationError::StatusUnavailable(
            "worktree path is unavailable; use prune for stale records".to_owned(),
        ));
    }
    if !allow_dirty_or_locked {
        if let Some(reason) = &details.worktree.locked {
            return Err(OperationError::Locked(Some(reason.clone())));
        }
        if let Some(status) = &details.status
            && status.is_dirty()
        {
            return Err(OperationError::Dirty(status.summary()));
        }
    }
    Ok(details)
}

pub fn preview_prune(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<String, OperationError> {
    let output = git::run_git_output(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("prune"),
            OsString::from("--dry-run"),
            OsString::from("--verbose"),
        ],
    )?;
    Ok(combined_output(output))
}

pub fn prune(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<String, OperationError> {
    let output = git::run_git_output(
        runner,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("prune"),
            OsString::from("--verbose"),
        ],
    )?;
    Ok(combined_output(output))
}

fn combined_output(output: git::CommandOutput) -> String {
    let mut bytes = output.stdout;
    bytes.extend_from_slice(&output.stderr);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn removal_details(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    selector: &str,
    current_directory: &Path,
) -> Result<WorktreeDetails, OperationError> {
    let worktrees = list(runner, repository)?;
    let (index, worktree) = select_worktree(&worktrees, selector)?;
    if worktree.bare {
        return Err(OperationError::BareAnchor);
    }
    if index == 0 {
        return Err(OperationError::MainWorktree);
    }
    if contains_path(&worktree.path, current_directory) {
        return Err(OperationError::CurrentWorktree);
    }
    inspect(runner, repository, &worktree.path.to_string_lossy())
}

fn contains_path(worktree: &Path, candidate: &Path) -> bool {
    let worktree = fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
    let candidate = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_owned());
    candidate.starts_with(worktree)
}

fn validate_destination(worktrees: &[Worktree], destination: &Path) -> Result<(), OperationError> {
    if destination.exists() {
        return Err(OperationError::DestinationExists(destination.to_owned()));
    }
    if worktrees
        .iter()
        .any(|worktree| worktree.path == destination)
    {
        return Err(OperationError::DestinationRegistered(
            destination.to_owned(),
        ));
    }
    Ok(())
}

fn ensure_destination_parent(
    destination: &Path,
    create_parents: bool,
) -> Result<(), OperationError> {
    validate_destination_parent(destination, create_parents)?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|source| OperationError::CreateParent {
            path: parent.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn validate_destination_parent(
    destination: &Path,
    create_parents: bool,
) -> Result<(), OperationError> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() && !create_parents {
        return Err(OperationError::MissingParent(parent.to_owned()));
    }
    Ok(())
}

fn validate_commit(
    runner: &dyn GitRunner,
    repository: &Path,
    commit: &str,
) -> Result<(), OperationError> {
    let expression = format!("{commit}^{{commit}}");
    let exists = git::git_succeeds(
        runner,
        repository,
        &[
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(expression),
        ],
    )?;
    if !exists {
        return Err(OperationError::InvalidCommit(commit.to_owned()));
    }
    Ok(())
}

fn branch_exists(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
) -> Result<bool, OperationError> {
    Ok(git::git_succeeds(
        runner,
        repository,
        &[
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("refs/heads/{branch}")),
        ],
    )?)
}

fn branch_checked_out(worktrees: &[Worktree], branch: &str) -> bool {
    let full = format!("refs/heads/{branch}");
    worktrees
        .iter()
        .any(|worktree| worktree.branch.as_deref() == Some(&full))
}

fn select_worktree<'a>(
    worktrees: &'a [Worktree],
    selector: &str,
) -> Result<(usize, &'a Worktree), OperationError> {
    if worktrees.is_empty() {
        return Err(OperationError::EmptyRepository(selector.to_owned()));
    }
    let matches: Vec<(usize, &Worktree)> = worktrees
        .iter()
        .enumerate()
        .filter(|(_, worktree)| worktree_matches(worktree, selector))
        .collect();
    match matches.as_slice() {
        [] => Err(OperationError::WorktreeNotFound(selector.to_owned())),
        [matched] => Ok(*matched),
        _ => Err(OperationError::AmbiguousWorktree(selector.to_owned())),
    }
}

fn worktree_matches(worktree: &Worktree, selector: &str) -> bool {
    let selector_path = Path::new(selector);
    worktree.path == selector_path
        || (selector_path.exists()
            && fs::canonicalize(selector_path).is_ok_and(|path| {
                fs::canonicalize(&worktree.path).is_ok_and(|other| other == path)
            }))
        || worktree
            .path
            .file_name()
            .is_some_and(|name| name == selector)
        || worktree.branch.as_deref() == Some(selector)
        || short_branch(worktree).as_deref() == Some(selector)
}

fn short_branch(worktree: &Worktree) -> Option<String> {
    worktree
        .branch
        .as_deref()
        .and_then(|branch| branch.strip_prefix("refs/heads/"))
        .map(str::to_owned)
}

fn sanitize_name(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches(['-', '.']);
    if sanitized.is_empty() {
        "worktree".to_owned()
    } else {
        sanitized.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_uses_configured_root_and_sanitizes_branch() {
        let repository = RepositoryConfig {
            path: PathBuf::from("/repos/project"),
            label: None,
            worktree_root: Some(PathBuf::from("/trees")),
            github_remote: None,
        };
        assert_eq!(
            suggested_destination(
                &repository,
                &CreateMode::ExistingBranch("feature/a thing".to_owned())
            ),
            PathBuf::from("/trees/feature-a-thing")
        );
    }
}
