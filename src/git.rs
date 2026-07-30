use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::model::{Catalog, RepositoryConfig, RepositoryDiscovery, RepositoryIdentity, Worktree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
}

pub trait GitRunner {
    fn run(&self, directory: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn run(&self, directory: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .map_err(|source| GitError::Launch { source })?;
        Ok(CommandOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            success: output.status.success(),
        })
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to launch Git: {source}")]
    Launch { source: std::io::Error },
    #[error("Git command failed: {message}")]
    Command { message: String },
    #[error("Git returned a non-path value for {field}")]
    InvalidPath { field: &'static str },
    #[error("cannot canonicalize {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("malformed worktree porcelain: {0}")]
    MalformedPorcelain(String),
}

pub fn resolve_repository(
    runner: &dyn GitRunner,
    path: &Path,
) -> Result<RepositoryIdentity, GitError> {
    let common_output = run_checked(
        runner,
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_path = bytes_to_path(trim_ascii_line_end(&common_output), "Git common directory")?;
    let common_git_dir =
        fs::canonicalize(&common_path).map_err(|source| GitError::Canonicalize {
            path: common_path,
            source,
        })?;

    let worktrees = discover_worktrees(runner, path)?;
    let header = worktrees.first().ok_or_else(|| {
        GitError::MalformedPorcelain("Git reported no repository anchor".to_owned())
    })?;
    let anchor = fs::canonicalize(&header.path).map_err(|source| GitError::Canonicalize {
        path: header.path.clone(),
        source,
    })?;
    Ok(RepositoryIdentity {
        anchor,
        common_git_dir,
        bare: header.bare,
    })
}

pub fn discover_worktrees(
    runner: &dyn GitRunner,
    anchor: &Path,
) -> Result<Vec<Worktree>, GitError> {
    let output = run_checked(runner, anchor, &["worktree", "list", "--porcelain", "-z"])?;
    parse_worktree_porcelain(&output)
}

pub fn discover_catalog(runner: &dyn GitRunner, catalog: &Catalog) -> Vec<RepositoryDiscovery> {
    catalog
        .repositories
        .iter()
        .cloned()
        .map(|repository| {
            let result =
                discover_worktrees(runner, &repository.path).map_err(|error| error.to_string());
            RepositoryDiscovery { repository, result }
        })
        .collect()
}

pub fn canonical_common_dir(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<PathBuf, GitError> {
    resolve_repository(runner, &repository.path).map(|identity| identity.common_git_dir)
}

fn run_checked(
    runner: &dyn GitRunner,
    directory: &Path,
    arguments: &[&str],
) -> Result<Vec<u8>, GitError> {
    let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let output = runner.run(directory, &arguments)?;
    if output.success {
        return Ok(output.stdout);
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(GitError::Command {
        message: if message.is_empty() {
            "unknown Git error".to_owned()
        } else {
            message
        },
    })
}

pub fn parse_worktree_porcelain(input: &[u8]) -> Result<Vec<Worktree>, GitError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in input.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }
        let (key, value) = split_field(field);
        match key {
            b"worktree" => {
                if current.is_some() {
                    return Err(GitError::MalformedPorcelain(
                        "worktree record was not terminated".to_owned(),
                    ));
                }
                let path = bytes_to_path(value, "worktree path")?;
                current = Some(Worktree {
                    path,
                    head: None,
                    branch: None,
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                });
            }
            b"HEAD" => record_mut(&mut current, "HEAD")?.head = Some(lossy(value)),
            b"branch" => record_mut(&mut current, "branch")?.branch = Some(lossy(value)),
            b"detached" => record_mut(&mut current, "detached")?.detached = true,
            b"bare" => record_mut(&mut current, "bare")?.bare = true,
            b"locked" => record_mut(&mut current, "locked")?.locked = Some(lossy(value)),
            b"prunable" => record_mut(&mut current, "prunable")?.prunable = Some(lossy(value)),
            _ => {}
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

fn record_mut<'a>(
    current: &'a mut Option<Worktree>,
    field: &str,
) -> Result<&'a mut Worktree, GitError> {
    current.as_mut().ok_or_else(|| {
        GitError::MalformedPorcelain(format!("{field} appeared before a worktree path"))
    })
}

fn split_field(field: &[u8]) -> (&[u8], &[u8]) {
    match field.iter().position(|byte| *byte == b' ') {
        Some(index) => (&field[..index], &field[index + 1..]),
        None => (field, &[]),
    }
}

fn trim_ascii_line_end(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8], field: &'static str) -> Result<PathBuf, GitError> {
    use std::os::unix::ffi::OsStringExt;
    if bytes.is_empty() {
        return Err(GitError::InvalidPath { field });
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8], field: &'static str) -> Result<PathBuf, GitError> {
    if bytes.is_empty() {
        return Err(GitError::InvalidPath { field });
    }
    Ok(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn parses_all_worktree_states_and_spaces() {
        let input = b"worktree /tmp/main tree\0HEAD abc123\0branch refs/heads/main\0\0worktree /tmp/other\0HEAD def456\0detached\0locked maintenance window\0prunable gitdir file points to missing location\0\0";
        let parsed = parse_worktree_porcelain(input).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("/tmp/main tree"));
        assert_eq!(parsed[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(parsed[1].detached);
        assert_eq!(parsed[1].locked.as_deref(), Some("maintenance window"));
        assert!(parsed[1].prunable.is_some());
    }

    #[test]
    fn parses_bare_anchor() {
        let parsed = parse_worktree_porcelain(b"worktree /tmp/project.git\0bare\0\0").unwrap();
        assert!(parsed[0].bare);
        assert!(!parsed[0].navigable());
    }

    #[test]
    fn rejects_fields_outside_records() {
        let error = parse_worktree_porcelain(b"HEAD abc\0").unwrap_err();
        assert!(matches!(error, GitError::MalformedPorcelain(_)));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let parsed = parse_worktree_porcelain(b"worktree /tmp/bad\xffpath\0HEAD abc\0\0").unwrap();
        assert_eq!(parsed[0].path.as_os_str().as_bytes(), b"/tmp/bad\xffpath");
    }

    #[test]
    fn resolves_main_and_linked_worktrees_to_the_same_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main");
        let linked = directory.path().join("linked tree");
        git(directory.path(), &["init", main.to_str().unwrap()]);
        git(&main, &["config", "user.email", "test@example.com"]);
        git(&main, &["config", "user.name", "Test User"]);
        git(&main, &["commit", "--allow-empty", "-m", "initial"]);
        git(
            &main,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );

        let main_identity = resolve_repository(&SystemGit, &main).unwrap();
        let linked_identity = resolve_repository(&SystemGit, &linked).unwrap();
        assert_eq!(main_identity, linked_identity);
        assert_eq!(main_identity.anchor, fs::canonicalize(main).unwrap());
        assert!(!main_identity.bare);
    }

    #[test]
    fn resolves_and_discovers_a_bare_repository_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let bare = directory.path().join("project.git");
        git(
            directory.path(),
            &["init", "--bare", bare.to_str().unwrap()],
        );

        let identity = resolve_repository(&SystemGit, &bare).unwrap();
        assert!(identity.bare);
        assert_eq!(identity.anchor, fs::canonicalize(&bare).unwrap());
        let worktrees = discover_worktrees(&SystemGit, &bare).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].bare);
    }

    #[test]
    fn catalog_discovery_isolates_stale_repositories() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        git(directory.path(), &["init", repository.to_str().unwrap()]);
        let catalog = Catalog {
            repositories: vec![
                RepositoryConfig {
                    path: repository,
                    label: Some("valid".to_owned()),
                    worktree_root: None,
                    github_remote: None,
                },
                RepositoryConfig {
                    path: directory.path().join("missing"),
                    label: Some("stale".to_owned()),
                    worktree_root: None,
                    github_remote: None,
                },
            ],
            ..Catalog::default()
        };
        let discoveries = discover_catalog(&SystemGit, &catalog);
        assert!(discoveries[0].result.is_ok());
        assert!(discoveries[1].result.is_err());
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}
