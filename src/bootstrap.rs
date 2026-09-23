use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

use base64::Engine;
use thiserror::Error;

use crate::git::{self, GitRunner};
use crate::github::{self, ResolvedToken};
use crate::model::{Catalog, GitHubRepositoryIdentity, RepositoryConfig};

#[derive(Clone)]
pub struct CloneRequest {
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

impl std::fmt::Debug for CloneRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloneRequest")
            .field("arguments", &self.arguments)
            .field(
                "environment_keys",
                &self
                    .environment
                    .iter()
                    .map(|(key, _)| key)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneOutput {
    pub success: bool,
    pub stderr: String,
}

pub trait CloneRunner {
    fn run(&self, request: &CloneRequest) -> Result<CloneOutput, std::io::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapResult {
    pub repository_index: usize,
    pub repository: RepositoryConfig,
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryCreation {
    pub path: PathBuf,
    pub bare: bool,
}

pub struct BootstrapOptions<'a> {
    pub creation: Option<&'a RepositoryCreation>,
    pub base_branch: &'a str,
    pub https_token: Option<&'a ResolvedToken>,
    pub mapped_repository_path: Option<&'a Path>,
}

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("repository creation requires an explicit destination and repository type")]
    CreationRequired,
    #[error("repository destination is occupied by unrelated files: {0}")]
    DestinationOccupied(PathBuf),
    #[error("repository destination must be an absolute path")]
    InvalidDestination,
    #[error("failed to prepare repository bootstrap directory {path}: {source}")]
    Filesystem {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to launch Git clone: {0}")]
    CloneLaunch(std::io::Error),
    #[error("SSH clone failed and no HTTPS credential is available: {0}")]
    MissingHttpsCredential(String),
    #[error("repository clone failed: {0}")]
    Clone(String),
    #[error("cloned repository did not validate as the requested GitHub repository")]
    InvalidClone,
    #[error("repository bootstrap destination appeared during clone: {0}")]
    DestinationRace(PathBuf),
}

pub fn bootstrap_repository(
    git_runner: &dyn GitRunner,
    clone_runner: &dyn CloneRunner,
    catalog: &mut Catalog,
    identity: &GitHubRepositoryIdentity,
    options: BootstrapOptions<'_>,
) -> Result<BootstrapResult, BootstrapError> {
    if let Some(path) = options.mapped_repository_path
        && let Some((index, repository)) = catalog
            .repositories
            .iter()
            .enumerate()
            .find(|(_, repository)| repository.path == path)
        && repository_matches(git_runner, &repository.path, identity)
    {
        return Ok(BootstrapResult {
            repository_index: index,
            repository: repository.clone(),
            created: false,
        });
    }

    let creation = options.creation.ok_or(BootstrapError::CreationRequired)?;
    let target = creation.path.clone();
    validate_destination(git_runner, &target, identity)?;
    let reused = path_is_occupied(&target);
    if !reused {
        let repository_root = target.parent().ok_or(BootstrapError::InvalidDestination)?;
        fs::create_dir_all(repository_root).map_err(|source| BootstrapError::Filesystem {
            path: repository_root.to_owned(),
            source,
        })?;
        clone_repository(
            clone_runner,
            git_runner,
            creation,
            identity,
            options.base_branch,
            options.https_token,
        )?;
    }
    if !repository_matches(git_runner, &target, identity) {
        return Err(BootstrapError::InvalidClone);
    }

    let mut observed = RepositoryConfig {
        path: fs::canonicalize(&target).map_err(|source| BootstrapError::Filesystem {
            path: target.clone(),
            source,
        })?,
        label: None,
        worktree_root: None,
        github_remote: Some("origin".to_owned()),
        github_remotes: Default::default(),
        github_preferred_remote: None,
    };
    github::refresh_repository_remote_identities(git_runner, &mut observed)
        .map_err(|_| BootstrapError::InvalidClone)?;

    let stale_index = catalog.repositories.iter().position(|repository| {
        git::resolve_repository(git_runner, &repository.path).is_err()
            && repository
                .github_remotes
                .values()
                .any(|cached| cached == identity)
    });
    let repository_index = if let Some(index) = stale_index {
        let stale = &catalog.repositories[index];
        let preferred_remote = stale
            .github_remote
            .as_ref()
            .or(stale.github_preferred_remote.as_ref())
            .cloned();
        if let Some(preferred_remote) = preferred_remote.as_ref()
            && !observed.github_remotes.contains_key(preferred_remote)
            && observed.github_remotes.contains_key("origin")
        {
            git::run_git(
                git_runner,
                &observed.path,
                &[
                    OsString::from("remote"),
                    OsString::from("rename"),
                    OsString::from("origin"),
                    OsString::from(preferred_remote),
                ],
            )
            .map_err(|_| BootstrapError::InvalidClone)?;
            observed.github_remotes.clear();
            observed.github_preferred_remote = None;
            github::refresh_repository_remote_identities(git_runner, &mut observed)
                .map_err(|_| BootstrapError::InvalidClone)?;
        }
        observed.label = stale.label.clone();
        observed.worktree_root = stale.worktree_root.clone();
        observed.github_remote = stale.github_remote.clone();
        observed.github_preferred_remote = stale.github_preferred_remote.clone();
        catalog.repositories[index] = observed.clone();
        index
    } else if let Some(index) = catalog
        .repositories
        .iter()
        .position(|repository| repository.path == observed.path)
    {
        let existing = &catalog.repositories[index];
        observed.label = existing.label.clone();
        observed.worktree_root = existing.worktree_root.clone();
        observed.github_remote = existing.github_remote.clone();
        observed.github_preferred_remote = existing.github_preferred_remote.clone();
        catalog.repositories[index] = observed.clone();
        index
    } else {
        let short_label = identity.repository.clone();
        let label_in_use = catalog
            .repositories
            .iter()
            .any(|repository| repository.display_label() == short_label);
        observed.label = Some(if label_in_use {
            identity.full_name()
        } else {
            short_label
        });
        catalog.repositories.push(observed.clone());
        catalog.repositories.len() - 1
    };
    Ok(BootstrapResult {
        repository_index,
        repository: catalog.repositories[repository_index].clone(),
        created: !reused,
    })
}

pub fn validate_destination(
    runner: &dyn GitRunner,
    path: &Path,
    identity: &GitHubRepositoryIdentity,
) -> Result<(), BootstrapError> {
    if !path.is_absolute() {
        return Err(BootstrapError::InvalidDestination);
    }
    if path_is_occupied(path) && !repository_matches(runner, path, identity) {
        return Err(BootstrapError::DestinationOccupied(path.to_owned()));
    }
    Ok(())
}

fn clone_repository(
    clone_runner: &dyn CloneRunner,
    git_runner: &dyn GitRunner,
    creation: &RepositoryCreation,
    identity: &GitHubRepositoryIdentity,
    base_branch: &str,
    https_token: Option<&ResolvedToken>,
) -> Result<(), BootstrapError> {
    let target = &creation.path;
    let repository_root = target.parent().ok_or(BootstrapError::InvalidDestination)?;
    let staging = tempfile::Builder::new()
        .prefix(".wt-incomplete-clone-")
        .tempdir_in(repository_root)
        .map_err(|source| BootstrapError::Filesystem {
            path: repository_root.to_owned(),
            source,
        })?;
    let clone_path = staging.path().join("repository.git");
    let ssh_url = format!(
        "git@{}:{}/{}.git",
        identity.host, identity.owner, identity.repository
    );
    let ssh_partial = clone_request(
        &ssh_url,
        &clone_path,
        base_branch,
        creation.bare,
        true,
        None,
    );
    let ssh_output = run_clone(clone_runner, &ssh_partial, https_token)?;
    let mut success = ssh_output.success;
    let mut last_error = ssh_output.stderr;
    if !success && filter_unsupported(&last_error) {
        clean_clone_path(&clone_path, staging.path())?;
        let output = run_clone(
            clone_runner,
            &clone_request(
                &ssh_url,
                &clone_path,
                base_branch,
                creation.bare,
                false,
                None,
            ),
            https_token,
        )?;
        success = output.success;
        last_error = output.stderr;
    }
    if !success {
        let token = https_token.ok_or_else(|| {
            BootstrapError::MissingHttpsCredential(git::redact_secret(&last_error, None))
        })?;
        clean_clone_path(&clone_path, staging.path())?;
        let https_url = format!(
            "https://{}/{}/{}.git",
            identity.host, identity.owner, identity.repository
        );
        let output = run_clone(
            clone_runner,
            &clone_request(
                &https_url,
                &clone_path,
                base_branch,
                creation.bare,
                true,
                Some(token),
            ),
            https_token,
        )?;
        success = output.success;
        last_error = output.stderr;
        if !success && filter_unsupported(&last_error) {
            clean_clone_path(&clone_path, staging.path())?;
            let output = run_clone(
                clone_runner,
                &clone_request(
                    &https_url,
                    &clone_path,
                    base_branch,
                    creation.bare,
                    false,
                    Some(token),
                ),
                https_token,
            )?;
            success = output.success;
            last_error = output.stderr;
        }
    }
    if !success {
        return Err(BootstrapError::Clone(git::redact_secret(
            &last_error,
            https_token.map(ResolvedToken::expose),
        )));
    }
    if !repository_matches(git_runner, &clone_path, identity) {
        return Err(BootstrapError::InvalidClone);
    }
    if path_is_occupied(target) {
        return Err(BootstrapError::DestinationRace(target.to_owned()));
    }
    fs::rename(&clone_path, target).map_err(|source| BootstrapError::Filesystem {
        path: target.to_owned(),
        source,
    })?;
    Ok(())
}

fn clone_request(
    url: &str,
    destination: &Path,
    base_branch: &str,
    bare: bool,
    partial: bool,
    token: Option<&ResolvedToken>,
) -> CloneRequest {
    let mut arguments = vec![
        OsString::from("clone"),
        // Git only draws progress on a tty unless it is asked to; the runner
        // reads stderr through a pipe.
        OsString::from("--progress"),
        OsString::from("--origin"),
        OsString::from("origin"),
        OsString::from("--single-branch"),
        OsString::from("--branch"),
        OsString::from(base_branch),
    ];
    if bare {
        // Normal clones configure their own fetch refspec. Adding another at
        // clone time would ask Git to update the same remote ref twice.
        arguments.extend([
            OsString::from("--bare"),
            OsString::from("--config"),
            OsString::from("remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*"),
        ]);
    }
    if partial {
        arguments.push(OsString::from("--filter=blob:none"));
    }
    arguments.push(OsString::from(url));
    arguments.push(destination.as_os_str().to_owned());
    let mut environment = vec![
        (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
        (OsString::from("GCM_INTERACTIVE"), OsString::from("Never")),
        (
            OsString::from("GIT_ASKPASS"),
            OsString::from("/usr/bin/false"),
        ),
        (
            OsString::from("GIT_SSH_COMMAND"),
            OsString::from("ssh -oBatchMode=yes"),
        ),
    ];
    if let Some(token) = token {
        let credential = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", token.expose()));
        environment.extend([
            (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_KEY_0"),
                OsString::from("http.extraHeader"),
            ),
            (
                OsString::from("GIT_CONFIG_VALUE_0"),
                OsString::from(format!("Authorization: Basic {credential}")),
            ),
        ]);
    }
    CloneRequest {
        arguments,
        environment,
    }
}

fn run_clone(
    runner: &dyn CloneRunner,
    request: &CloneRequest,
    token: Option<&ResolvedToken>,
) -> Result<CloneOutput, BootstrapError> {
    runner
        .run(request)
        .map(|mut output| {
            output.stderr = git::redact_secret(&output.stderr, token.map(ResolvedToken::expose));
            output
        })
        .map_err(BootstrapError::CloneLaunch)
}

pub fn repository_matches(
    runner: &dyn GitRunner,
    path: &Path,
    expected: &GitHubRepositoryIdentity,
) -> bool {
    if git::resolve_repository(runner, path).is_err() {
        return false;
    }
    let mut repository = RepositoryConfig {
        path: path.to_owned(),
        label: None,
        worktree_root: None,
        github_remote: None,
        github_remotes: Default::default(),
        github_preferred_remote: None,
    };
    github::refresh_repository_remote_identities(runner, &mut repository).is_ok()
        && repository
            .github_remotes
            .values()
            .any(|identity| identity == expected)
}

fn path_is_occupied(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn clean_clone_path(path: &Path, staging_root: &Path) -> Result<(), BootstrapError> {
    if !path.starts_with(staging_root) || !path_is_occupied(path) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| BootstrapError::Filesystem {
        path: path.to_owned(),
        source,
    })?;
    let result = if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|source| BootstrapError::Filesystem {
        path: path.to_owned(),
        source,
    })
}

fn filter_unsupported(stderr: &str) -> bool {
    let message = stderr.to_ascii_lowercase();
    message.contains("filtering not recognized")
        || message.contains("does not support filter")
        || message.contains("filter-spec")
        || message.contains("unsupported filter")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::SystemGit;
    use std::sync::Mutex;

    struct FixtureCloneRunner {
        outcomes: Mutex<Vec<CloneOutput>>,
        requests: Mutex<Vec<CloneRequest>>,
    }

    impl FixtureCloneRunner {
        fn new(outcomes: Vec<CloneOutput>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into_iter().rev().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl CloneRunner for FixtureCloneRunner {
        fn run(&self, request: &CloneRequest) -> Result<CloneOutput, std::io::Error> {
            self.requests.lock().unwrap().push(request.clone());
            let output = self.outcomes.lock().unwrap().pop().unwrap();
            if output.success {
                let destination = PathBuf::from(request.arguments.last().unwrap());
                let url = request.arguments[request.arguments.len() - 2]
                    .to_string_lossy()
                    .into_owned();
                let init = Command::new("git")
                    .args(["init", "--bare"])
                    .arg(&destination)
                    .output()
                    .unwrap();
                assert!(init.status.success());
                let remote = Command::new("git")
                    .arg("-C")
                    .arg(&destination)
                    .args(["remote", "add", "origin", &url])
                    .output()
                    .unwrap();
                assert!(remote.status.success());
            }
            Ok(output)
        }
    }

    fn success() -> CloneOutput {
        CloneOutput {
            success: true,
            stderr: String::new(),
        }
    }

    fn failure(message: &str) -> CloneOutput {
        CloneOutput {
            success: false,
            stderr: message.to_owned(),
        }
    }

    fn identity() -> GitHubRepositoryIdentity {
        GitHubRepositoryIdentity::canonical("github.com", "team", "project")
    }

    #[test]
    fn missing_or_stale_mapping_never_authorizes_creation() {
        let directory = tempfile::tempdir().unwrap();
        let stale = directory.path().join("gone");
        let mut catalog = Catalog {
            repositories: vec![RepositoryConfig {
                path: stale.clone(),
                label: None,
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            }],
            ..Catalog::default()
        };
        let runner = FixtureCloneRunner::new(Vec::new());
        for mapped_repository_path in [None, Some(stale.as_path())] {
            let result = bootstrap_repository(
                &SystemGit,
                &runner,
                &mut catalog,
                &identity(),
                BootstrapOptions {
                    creation: None,
                    base_branch: "main",
                    https_token: None,
                    mapped_repository_path,
                },
            );
            assert!(matches!(result, Err(BootstrapError::CreationRequired)));
        }
        assert!(runner.requests.lock().unwrap().is_empty());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
        assert_eq!(catalog.repositories.len(), 1);
        assert_eq!(catalog.repositories[0].path, stale);
    }

    struct LocalCloneRunner(PathBuf);

    impl CloneRunner for LocalCloneRunner {
        fn run(&self, request: &CloneRequest) -> Result<CloneOutput, std::io::Error> {
            let mut arguments = request.arguments.clone();
            let url_index = arguments.len() - 2;
            let url = arguments[url_index].clone();
            arguments[url_index] = self.0.as_os_str().to_owned();
            let output = Command::new("git")
                .args(&arguments)
                .envs(request.environment.iter().cloned())
                .output()?;
            if output.status.success() {
                assert!(
                    Command::new("git")
                        .arg("-C")
                        .arg(arguments.last().unwrap())
                        .args(["remote", "set-url", "origin"])
                        .arg(url)
                        .status()?
                        .success()
                );
            }
            Ok(CloneOutput {
                success: output.status.success(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    #[test]
    fn explicit_destination_creates_normal_or_bare_repository() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        assert!(
            Command::new("git")
                .args(["init", "-b", "main"])
                .arg(&source)
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::write(source.join("tracked"), "content").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "."])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    "initial"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        for bare in [false, true] {
            let path = directory.path().join(if bare {
                "custom/bare"
            } else {
                "custom/checkout"
            });
            let result = bootstrap_repository(
                &SystemGit,
                &LocalCloneRunner(source.clone()),
                &mut Catalog::default(),
                &identity(),
                BootstrapOptions {
                    creation: Some(&RepositoryCreation {
                        path: path.clone(),
                        bare,
                    }),
                    base_branch: "main",
                    https_token: None,
                    mapped_repository_path: None,
                },
            )
            .unwrap();
            assert!(result.created);
            assert_eq!(result.repository.path, fs::canonicalize(&path).unwrap());
            assert_eq!(
                git::resolve_repository(&SystemGit, &path).unwrap().bare,
                bare
            );
            assert_eq!(path.join("tracked").exists(), !bare);
            if !bare {
                assert_eq!(fs::read_to_string(path.join("tracked")).unwrap(), "content");
            }
            assert!(path.parent().unwrap().read_dir().unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".wt-incomplete-clone-")
            }));
        }
    }

    #[test]
    fn mapped_repository_path_is_resolved_and_validated_against_fresh_catalog() {
        assert_mapped_repository_is_reused(true);
    }

    #[test]
    fn mapped_normal_checkout_is_reused_without_creating_a_bare_clone() {
        assert_mapped_repository_is_reused(false);
    }

    fn assert_mapped_repository_is_reused(bare: bool) {
        let directory = tempfile::tempdir().unwrap();
        let unrelated = directory.path().join("unrelated.git");
        let mapped = directory.path().join("mapped.git");
        for (path, remote) in [
            (&unrelated, "git@github.com:team/unrelated.git"),
            (&mapped, "git@github.com:team/project.git"),
        ] {
            assert!(
                Command::new("git")
                    .args(if bare {
                        vec!["init", "--bare"]
                    } else {
                        vec!["init"]
                    })
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(["remote", "add", "origin", remote])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let repository = |path: PathBuf| RepositoryConfig {
            path,
            label: None,
            worktree_root: None,
            github_remote: None,
            github_remotes: Default::default(),
            github_preferred_remote: None,
        };
        let mut catalog = Catalog {
            repositories: vec![repository(unrelated.clone()), repository(mapped.clone())],
            ..Catalog::default()
        };

        let result = bootstrap_repository(
            &SystemGit,
            &FixtureCloneRunner::new(Vec::new()),
            &mut catalog,
            &identity(),
            BootstrapOptions {
                creation: None,
                base_branch: "main",
                https_token: None,
                mapped_repository_path: Some(&mapped),
            },
        )
        .unwrap();

        assert_eq!(result.repository_index, 1);
        assert_eq!(result.repository.path, mapped);
        assert!(!result.created);
        assert_eq!(catalog.repositories.len(), 2);
        assert!(!directory.path().join("repositories").exists());

        let clone_runner = FixtureCloneRunner::new(vec![success()]);
        let result = bootstrap_repository(
            &SystemGit,
            &clone_runner,
            &mut catalog,
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: directory.path().join("project.git"),
                    bare: true,
                }),
                base_branch: "main",
                https_token: None,
                mapped_repository_path: Some(&unrelated),
            },
        )
        .unwrap();

        assert_ne!(result.repository.path, unrelated);
        assert_eq!(clone_runner.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn occupied_destination_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project");
        fs::write(&path, b"unrelated").unwrap();
        let runner = FixtureCloneRunner::new(Vec::new());
        let result = bootstrap_repository(
            &SystemGit,
            &runner,
            &mut Catalog::default(),
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: path.clone(),
                    bare: false,
                }),
                base_branch: "main",
                https_token: None,
                mapped_repository_path: None,
            },
        );
        assert!(matches!(
            result,
            Err(BootstrapError::DestinationOccupied(_))
        ));
        assert_eq!(fs::read(path).unwrap(), b"unrelated");
        assert!(runner.requests.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_is_occupied_without_guessing_another_path() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project");
        symlink("missing", &path).unwrap();
        assert!(matches!(
            validate_destination(&SystemGit, &path, &identity()),
            Err(BootstrapError::DestinationOccupied(_))
        ));
        assert!(path.is_symlink());
        assert_eq!(directory.path().read_dir().unwrap().count(), 1);
    }

    #[test]
    fn filter_and_transport_fallback_keep_tokens_out_of_arguments_and_debug() {
        let directory = tempfile::tempdir().unwrap();
        let runner = FixtureCloneRunner::new(vec![
            failure("SSH denied"),
            failure("server does not support filter"),
            success(),
        ]);
        let token = crate::github::ResolvedToken::for_test("recognizable-secret");
        let result = bootstrap_repository(
            &SystemGit,
            &runner,
            &mut Catalog::default(),
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: directory.path().join("project.git"),
                    bare: true,
                }),
                base_branch: "main",
                https_token: Some(&token),
                mapped_repository_path: None,
            },
        )
        .unwrap();
        assert!(result.created);
        let requests = runner.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[0]
                .arguments
                .iter()
                .any(|value| value == "--filter=blob:none")
        );
        assert!(
            requests[1]
                .arguments
                .iter()
                .any(|value| value == "--filter=blob:none")
        );
        assert!(
            !requests[2]
                .arguments
                .iter()
                .any(|value| value == "--filter=blob:none")
        );
        for request in requests.iter() {
            // Git stays silent about progress on a pipe unless it is asked.
            assert!(request.arguments.iter().any(|value| value == "--progress"));
            assert!(
                request
                    .arguments
                    .iter()
                    .any(|value| value == "--single-branch")
            );
            assert!(
                request
                    .arguments
                    .windows(2)
                    .any(|values| values[0] == "--branch" && values[1] == "main")
            );
            assert!(request.arguments.iter().any(|value| {
                value == "remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*"
            }));
            assert!(!format!("{:?}", request.arguments).contains("recognizable-secret"));
            assert!(!format!("{request:?}").contains("recognizable-secret"));
        }
        assert!(
            requests[1]
                .environment
                .iter()
                .any(|(key, value)| key == "GIT_CONFIG_VALUE_0"
                    && value.to_string_lossy()
                        == "Authorization: Basic eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA==")
        );
        assert!(!directory.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wt-incomplete-clone-")
        }));
    }

    #[test]
    fn failed_https_clone_redacts_secret_and_cleans_owned_staging() {
        let directory = tempfile::tempdir().unwrap();
        let runner = FixtureCloneRunner::new(vec![
            failure("SSH denied"),
            failure("server echoed recognizable-secret"),
        ]);
        let token = crate::github::ResolvedToken::for_test("recognizable-secret");
        let result = bootstrap_repository(
            &SystemGit,
            &runner,
            &mut Catalog::default(),
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: directory.path().join("project.git"),
                    bare: true,
                }),
                base_branch: "main",
                https_token: Some(&token),
                mapped_repository_path: None,
            },
        );
        let message = result.unwrap_err().to_string();
        assert!(!message.contains("recognizable-secret"));
        assert!(message.contains("[REDACTED]"));
        assert!(directory.path().read_dir().unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wt-incomplete-clone-")
        }));
        assert!(!directory.path().join("project.git").exists());
    }

    #[test]
    fn invalid_entry_is_relinked_without_touching_existing_path_or_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let stale_path = directory.path().join("gone.git");
        fs::create_dir(&stale_path).unwrap();
        let marker = stale_path.join("user-file");
        fs::write(&marker, b"safe").unwrap();
        let mut stale_remotes = std::collections::BTreeMap::new();
        stale_remotes.insert("upstream".to_owned(), identity());
        let mut catalog = Catalog {
            repositories: vec![RepositoryConfig {
                path: stale_path.clone(),
                label: Some("preserved".to_owned()),
                worktree_root: Some(directory.path().join("trees")),
                github_remote: Some("upstream".to_owned()),
                github_remotes: stale_remotes,
                github_preferred_remote: Some("upstream".to_owned()),
            }],
            ..Catalog::default()
        };
        let result = bootstrap_repository(
            &SystemGit,
            &FixtureCloneRunner::new(vec![success()]),
            &mut catalog,
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: directory.path().join("project.git"),
                    bare: true,
                }),
                base_branch: "main",
                https_token: None,
                mapped_repository_path: None,
            },
        )
        .unwrap();
        assert_eq!(result.repository_index, 0);
        assert_eq!(result.repository.label.as_deref(), Some("preserved"));
        assert_eq!(
            result.repository.worktree_root,
            Some(directory.path().join("trees"))
        );
        assert_eq!(result.repository.github_remote.as_deref(), Some("upstream"));
        assert_eq!(
            result.repository.github_preferred_remote.as_deref(),
            Some("upstream")
        );
        let remotes = Command::new("git")
            .arg("-C")
            .arg(&result.repository.path)
            .arg("remote")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&remotes.stdout)
                .lines()
                .any(|remote| remote == "upstream")
        );
        assert!(stale_path.is_dir());
        assert_eq!(fs::read(marker).unwrap(), b"safe");
        assert!(result.repository.path.exists());
    }

    #[test]
    fn unique_labels_and_existing_matching_candidate_are_reused() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("project.git");
        let init = Command::new("git")
            .args(["init", "--bare"])
            .arg(&existing)
            .output()
            .unwrap();
        assert!(init.status.success());
        let remote = Command::new("git")
            .arg("-C")
            .arg(&existing)
            .args(["remote", "add", "origin", "git@github.com:team/project.git"])
            .output()
            .unwrap();
        assert!(remote.status.success());
        let mut catalog = Catalog {
            repositories: vec![RepositoryConfig {
                path: directory.path().join("other.git"),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            }],
            ..Catalog::default()
        };
        let runner = FixtureCloneRunner::new(Vec::new());
        let result = bootstrap_repository(
            &SystemGit,
            &runner,
            &mut catalog,
            &identity(),
            BootstrapOptions {
                creation: Some(&RepositoryCreation {
                    path: directory.path().join("project.git"),
                    bare: true,
                }),
                base_branch: "main",
                https_token: None,
                mapped_repository_path: None,
            },
        )
        .unwrap();
        assert!(!result.created);
        assert_eq!(result.repository.label.as_deref(), Some("team/project"));
        assert!(runner.requests.lock().unwrap().is_empty());
    }
}
