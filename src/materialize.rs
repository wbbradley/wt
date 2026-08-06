use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

use base64::Engine;
use thiserror::Error;

use crate::git::{self, GitRunner};
use crate::github::ResolvedToken;
use crate::model::{
    AuthoredPullRequest, CanonicalPullRequestId, GitHubRepositoryIdentity, RepositoryConfig,
};
use crate::operations::{self, CreateMode};

const PULL_REQUEST_MARKER: &str = "wt-pr";

#[derive(Clone)]
pub struct FetchRequest {
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

impl std::fmt::Debug for FetchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FetchRequest")
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
pub struct FetchOutput {
    pub success: bool,
    pub stderr: String,
}

pub trait FetchRunner {
    fn run(&self, repository: &Path, request: &FetchRequest)
    -> Result<FetchOutput, std::io::Error>;
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg(test)]
pub struct SystemFetchRunner;

#[cfg(test)]
impl FetchRunner for SystemFetchRunner {
    fn run(
        &self,
        repository: &Path,
        request: &FetchRequest,
    ) -> Result<FetchOutput, std::io::Error> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(&request.arguments)
            .envs(request.environment.iter().cloned())
            .output()?;
        Ok(FetchOutput {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedPullRequest {
    pub branch: String,
    pub path: PathBuf,
    pub reused: bool,
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Operation(#[from] operations::OperationError),
    #[error("failed to launch Git fetch: {0}")]
    FetchLaunch(std::io::Error),
    #[error("Git fetch failed: {0}")]
    Fetch(String),
    #[error("pull request head repository is malformed: {0:?}")]
    MalformedHeadRepository(String),
    #[error("repository has no local remote for base repository {0}")]
    MissingBaseRemote(String),
    #[error("pull request head commit is missing")]
    MissingHeadCommit,
    #[error("fetched pull request head {actual} does not match refreshed head {expected}")]
    HeadChanged { expected: String, actual: String },
    #[error("cannot canonicalize materialized worktree {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to mark incomplete worktree under {path}: {source}")]
    IncompleteMarker {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn materialize_pull_request(
    runner: &dyn GitRunner,
    fetch_runner: &dyn FetchRunner,
    repository: &RepositoryConfig,
    repository_root: &Path,
    authored: &AuthoredPullRequest,
    https_token: Option<&ResolvedToken>,
) -> Result<MaterializedPullRequest, MaterializeError> {
    let expected_head = authored
        .pull_request
        .head
        .oid
        .as_deref()
        .ok_or(MaterializeError::MissingHeadCommit)?;
    let head_identity = authored
        .pull_request
        .head
        .repository
        .as_deref()
        .map(|name| repository_identity(&authored.identity.repository.host, name))
        .transpose()?;
    let real_remote = head_identity
        .as_ref()
        .and_then(|identity| matching_remote(repository, identity));
    let real_fetch = real_remote.map(|remote| {
        let branch = authored.pull_request.head.branch.clone();
        let target = format!("refs/remotes/{remote}/{branch}");
        let result = fetch_ref(
            fetch_runner,
            &repository.path,
            &remote,
            &format!("+refs/heads/{branch}:{target}"),
            https_token,
        );
        (
            result,
            branch,
            target,
            format!("{remote}/{}", authored.pull_request.head.branch),
        )
    });
    let (intended_branch, target, upstream) = match real_fetch {
        Some((Ok(()), branch, target, upstream)) => (branch, target, Some(upstream)),
        Some((Err(MaterializeError::Fetch(_)), _, _, _)) | None => {
            let remote =
                matching_remote(repository, &authored.identity.repository).ok_or_else(|| {
                    MaterializeError::MissingBaseRemote(authored.identity.repository.full_name())
                })?;
            let branch = format!(
                "pr/{}-{}",
                authored.identity.number,
                operations::sanitize_name(&authored.pull_request.head.branch)
            );
            let target = format!("refs/wt/pull/{}/head", authored.identity.number);
            fetch_ref(
                fetch_runner,
                &repository.path,
                &remote,
                &format!("+refs/pull/{}/head:{target}", authored.identity.number),
                https_token,
            )?;
            (branch, target, None)
        }
        Some((Err(error), _, _, _)) => return Err(error),
    };

    let fetched_head = resolve_commit(runner, &repository.path, &target)?;
    if !fetched_head.eq_ignore_ascii_case(expected_head) {
        return Err(MaterializeError::HeadChanged {
            expected: expected_head.to_owned(),
            actual: fetched_head,
        });
    }

    let marker = canonical_marker(&authored.identity);
    let worktrees = operations::list(runner, repository)?;
    let mut suffix = 0_u64;
    loop {
        let branch = branch_candidate(&intended_branch, authored.identity.number, suffix);
        let branch_ref = format!("refs/heads/{branch}");
        let branch_head = optional_commit(runner, &repository.path, &branch_ref)?;
        let checked_out = worktrees
            .iter()
            .find(|worktree| worktree.branch.as_deref() == Some(branch_ref.as_str()));
        if let Some(branch_head) = branch_head {
            let existing_marker = branch_marker(runner, &repository.path, &branch)?;
            if let Some(worktree) = checked_out {
                if existing_marker.as_deref() == Some(marker.as_str())
                    && branch_head == fetched_head
                {
                    return Ok(MaterializedPullRequest {
                        branch,
                        path: canonicalize_worktree(&worktree.path)?,
                        reused: true,
                    });
                }
                suffix += 1;
                continue;
            }
            if existing_marker
                .as_deref()
                .is_some_and(|value| value != marker)
                || (branch_head != fetched_head
                    && !is_ancestor(runner, &repository.path, &branch_head, &fetched_head)?)
            {
                suffix += 1;
                continue;
            }
            if branch_head != fetched_head {
                update_branch(runner, &repository.path, &branch, &target)?;
            }
        } else {
            create_branch(runner, &repository.path, &branch, &target)?;
        }
        set_branch_marker(runner, &repository.path, &branch, &marker)?;
        if let Some(upstream) = upstream.as_deref() {
            set_upstream(runner, &repository.path, &branch, upstream)?;
        }

        let destination = operations::pull_request_destination(
            repository,
            repository_root,
            &authored.identity,
            &branch,
        );
        operations::validate_create(
            runner,
            repository,
            &destination,
            &CreateMode::ExistingBranch(branch.clone()),
            false,
        )?;
        operations::prepare_destination_parent(repository, &destination, false)?;
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let staging_file = tempfile::Builder::new()
            .prefix(".wt-incomplete-worktree-")
            .tempfile_in(destination_parent)
            .map_err(|source| MaterializeError::IncompleteMarker {
                path: destination_parent.to_owned(),
                source,
            })?;
        let staging = staging_file.path().to_owned();
        staging_file
            .close()
            .map_err(|source| MaterializeError::IncompleteMarker {
                path: destination_parent.to_owned(),
                source,
            })?;
        if let Err(error) = operations::create(
            runner,
            repository,
            &staging,
            &CreateMode::ExistingBranch(branch.clone()),
            false,
        ) {
            cleanup_owned_incomplete_worktree(repository, &staging);
            return Err(error.into());
        }
        if let Err(error) = operations::move_worktree(
            runner,
            repository,
            &staging.to_string_lossy(),
            &destination,
            false,
        ) {
            cleanup_owned_incomplete_worktree(repository, &staging);
            return Err(error.into());
        }
        return Ok(MaterializedPullRequest {
            branch,
            path: canonicalize_worktree(&destination)?,
            reused: false,
        });
    }
}

fn cleanup_owned_incomplete_worktree(repository: &RepositoryConfig, destination: &Path) {
    let _ = git::run_git(
        &git::SystemGit,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            destination.as_os_str().to_owned(),
        ],
    );
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let _ = fs::remove_dir_all(destination);
        } else {
            let _ = fs::remove_file(destination);
        }
    }
    let _ = git::run_git(
        &git::SystemGit,
        &repository.path,
        &[
            OsString::from("worktree"),
            OsString::from("prune"),
            OsString::from("--expire=now"),
        ],
    );
}

fn repository_identity(
    host: &str,
    name_with_owner: &str,
) -> Result<GitHubRepositoryIdentity, MaterializeError> {
    let (owner, repository) = name_with_owner
        .split_once('/')
        .filter(|(owner, repository)| !owner.is_empty() && !repository.is_empty())
        .ok_or_else(|| MaterializeError::MalformedHeadRepository(name_with_owner.to_owned()))?;
    Ok(GitHubRepositoryIdentity::canonical(host, owner, repository))
}

fn matching_remote(
    repository: &RepositoryConfig,
    identity: &GitHubRepositoryIdentity,
) -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(remote) = repository.github_remote.as_ref() {
        candidates.push(remote.clone());
    }
    candidates.push("origin".to_owned());
    if let Some(remote) = repository.github_preferred_remote.as_ref() {
        candidates.push(remote.clone());
    }
    candidates.extend(repository.github_remotes.keys().cloned());
    let mut seen = BTreeSet::new();
    candidates.into_iter().find(|remote| {
        seen.insert(remote.clone()) && repository.github_remotes.get(remote) == Some(identity)
    })
}

fn fetch_ref(
    runner: &dyn FetchRunner,
    repository: &Path,
    remote: &str,
    refspec: &str,
    token: Option<&ResolvedToken>,
) -> Result<(), MaterializeError> {
    let request = FetchRequest {
        arguments: vec![
            OsString::from("fetch"),
            OsString::from("--no-tags"),
            OsString::from(remote),
            OsString::from(refspec),
        ],
        environment: git_transport_environment(token),
    };
    let output = runner
        .run(repository, &request)
        .map_err(MaterializeError::FetchLaunch)?;
    if output.success {
        Ok(())
    } else {
        Err(MaterializeError::Fetch(redact(
            &output.stderr,
            token.map(ResolvedToken::expose),
        )))
    }
}

fn git_transport_environment(token: Option<&ResolvedToken>) -> Vec<(OsString, OsString)> {
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
    environment
}

fn redact(message: &str, secret: Option<&str>) -> String {
    secret
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("x-access-token:{secret}"));
            message
                .replace(secret, "[REDACTED]")
                .replace(&encoded, "[REDACTED]")
        })
        .unwrap_or_else(|| message.to_owned())
}

fn resolve_commit(
    runner: &dyn GitRunner,
    repository: &Path,
    reference: &str,
) -> Result<String, git::GitError> {
    let output = git::run_git(
        runner,
        repository,
        &[
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from(format!("{reference}^{{commit}}")),
        ],
    )?;
    Ok(String::from_utf8_lossy(&output).trim().to_owned())
}

fn optional_commit(
    runner: &dyn GitRunner,
    repository: &Path,
    reference: &str,
) -> Result<Option<String>, git::GitError> {
    let output = runner.run(
        repository,
        &[
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("{reference}^{{commit}}")),
        ],
    )?;
    Ok(output
        .success
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn branch_marker(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
) -> Result<Option<String>, git::GitError> {
    let output = runner.run(
        repository,
        &[
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from("--get"),
            OsString::from(branch_marker_key(branch)),
        ],
    )?;
    Ok(output
        .success
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn set_branch_marker(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
    marker: &str,
) -> Result<(), git::GitError> {
    git::run_git(
        runner,
        repository,
        &[
            OsString::from("config"),
            OsString::from("--local"),
            OsString::from(branch_marker_key(branch)),
            OsString::from(marker),
        ],
    )?;
    Ok(())
}

fn branch_marker_key(branch: &str) -> String {
    format!("branch.{branch}.{PULL_REQUEST_MARKER}")
}

fn canonical_marker(identity: &CanonicalPullRequestId) -> String {
    format!(
        "{}/{}#{}",
        identity.repository.host,
        identity.repository.full_name(),
        identity.number
    )
}

fn branch_candidate(intended: &str, number: u64, suffix: u64) -> String {
    match suffix {
        0 => intended.to_owned(),
        1 => format!("{intended}-pr-{number}"),
        suffix => format!("{intended}-pr-{number}-{suffix}"),
    }
}

fn is_ancestor(
    runner: &dyn GitRunner,
    repository: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, git::GitError> {
    git::git_succeeds(
        runner,
        repository,
        &[
            OsString::from("merge-base"),
            OsString::from("--is-ancestor"),
            OsString::from(ancestor),
            OsString::from(descendant),
        ],
    )
}

fn create_branch(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
    target: &str,
) -> Result<(), git::GitError> {
    git::run_git(
        runner,
        repository,
        &[
            OsString::from("branch"),
            OsString::from(branch),
            OsString::from(target),
        ],
    )?;
    Ok(())
}

fn update_branch(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
    target: &str,
) -> Result<(), git::GitError> {
    git::run_git(
        runner,
        repository,
        &[
            OsString::from("branch"),
            OsString::from("--force"),
            OsString::from(branch),
            OsString::from(target),
        ],
    )?;
    Ok(())
}

fn set_upstream(
    runner: &dyn GitRunner,
    repository: &Path,
    branch: &str,
    upstream: &str,
) -> Result<(), git::GitError> {
    git::run_git(
        runner,
        repository,
        &[
            OsString::from("branch"),
            OsString::from("--set-upstream-to"),
            OsString::from(upstream),
            OsString::from(branch),
        ],
    )?;
    Ok(())
}

fn canonicalize_worktree(path: &Path) -> Result<PathBuf, MaterializeError> {
    fs::canonicalize(path).map_err(|source| MaterializeError::Canonicalize {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::SystemGit;
    use crate::model::{CheckRollup, PullRequest, PullRequestIdentity, PullRequestState};
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::sync::Mutex;

    struct CapturingFetchRunner {
        request: Mutex<Option<FetchRequest>>,
        output: FetchOutput,
    }

    impl FetchRunner for CapturingFetchRunner {
        fn run(
            &self,
            _repository: &Path,
            request: &FetchRequest,
        ) -> Result<FetchOutput, std::io::Error> {
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(self.output.clone())
        }
    }

    #[test]
    fn authenticated_fetch_keeps_tokens_out_of_arguments_debug_and_errors() {
        let runner = CapturingFetchRunner {
            request: Mutex::new(None),
            output: FetchOutput {
                success: false,
                stderr: "server echoed recognizable-secret and eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA==".to_owned(),
            },
        };
        let token = ResolvedToken::for_test("recognizable-secret");
        let error = fetch_ref(
            &runner,
            Path::new("/repository"),
            "origin",
            "+refs/heads/topic:refs/remotes/origin/topic",
            Some(&token),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("recognizable-secret"));
        assert!(error.to_string().contains("[REDACTED]"));
        let request = runner.request.lock().unwrap().clone().unwrap();
        assert!(!format!("{:?}", request.arguments).contains("recognizable-secret"));
        assert!(!format!("{request:?}").contains("recognizable-secret"));
        assert!(request.environment.iter().any(|(key, value)| {
            key == "GIT_CONFIG_VALUE_0"
                && value.to_string_lossy()
                    == "Authorization: Basic eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA=="
        }));
    }

    #[test]
    fn real_head_remote_tracks_branch_marks_it_and_reuses_worktree() {
        let fixture = Fixture::new();
        let repository = fixture.local_repository(true);
        let authored = fixture.authored(42, "feature/topic", "contributor/project");

        let materialized = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &authored,
            None,
        )
        .unwrap();
        assert_eq!(materialized.branch, "feature/topic");
        assert_eq!(
            materialized.path,
            fs::canonicalize(fixture.worktree_root.join("feature-topic")).unwrap()
        );
        assert_eq!(
            git_stdout(
                &materialized.path,
                &["rev-parse", "--abbrev-ref", "@{upstream}"]
            ),
            "fork/feature/topic"
        );
        assert_eq!(
            marker(&repository.path, "feature/topic"),
            "github.com/team/project#42"
        );
        assert!(!fixture.worktree_root.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wt-incomplete-worktree-")
        }));

        let reused = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &authored,
            None,
        )
        .unwrap();
        assert!(reused.reused);
        assert_eq!(reused.path, materialized.path);
    }

    #[test]
    fn missing_head_remote_uses_synthetic_branch_without_adding_remote() {
        let fixture = Fixture::new();
        git(
            &fixture.source,
            &["update-ref", "refs/pull/42/head", &fixture.target],
        );
        let repository = fixture.base_repository(false);
        let authored = fixture.authored(42, "feature/topic", "unconfigured/project");

        let materialized = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &authored,
            None,
        )
        .unwrap();
        assert_eq!(materialized.branch, "pr/42-feature-topic");
        assert_eq!(
            materialized.path,
            fs::canonicalize(fixture.repository_root.join("project-pr-42")).unwrap()
        );
        assert_eq!(git_stdout(&repository.path, &["remote"]), "origin");
        assert_eq!(
            marker(&repository.path, "pr/42-feature-topic"),
            "github.com/team/project#42"
        );
    }

    #[test]
    fn deleted_real_head_branch_falls_back_to_the_base_pull_ref() {
        let fixture = Fixture::new();
        git(
            &fixture.source,
            &["update-ref", "refs/pull/45/head", &fixture.target],
        );
        let mut repository = fixture.base_repository(true);
        git(
            &repository.path,
            &["remote", "add", "fork", fixture.source.to_str().unwrap()],
        );
        repository.github_remotes.insert(
            "fork".to_owned(),
            GitHubRepositoryIdentity::canonical("github.com", "contributor", "project"),
        );
        let authored = fixture.authored(45, "deleted/topic", "contributor/project");

        let materialized = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &authored,
            None,
        )
        .unwrap();
        assert_eq!(materialized.branch, "pr/45-deleted-topic");
        assert_eq!(
            branch_head(&repository.path, &materialized.branch),
            fixture.target
        );
    }

    #[test]
    fn destination_collision_fails_without_a_numeric_path_fallback() {
        let fixture = Fixture::new();
        git(
            &fixture.source,
            &["update-ref", "refs/pull/42/head", &fixture.target],
        );
        let repository = fixture.base_repository(false);
        let destination = fixture.repository_root.join("project-pr-42");
        fs::write(&destination, "unrelated").unwrap();
        let result = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(42, "feature/topic", "unconfigured/project"),
            None,
        );
        assert!(matches!(
            result,
            Err(MaterializeError::Operation(
                operations::OperationError::DestinationExists(path)
            )) if path == destination
        ));
        assert_eq!(fs::read_to_string(destination).unwrap(), "unrelated");
        assert!(!fixture.repository_root.join("project-pr-42-2").exists());
        assert_eq!(
            branch_head(&repository.path, "pr/42-feature-topic"),
            fixture.target
        );
    }

    #[test]
    fn fetched_oid_mismatch_creates_neither_branch_nor_worktree() {
        let fixture = Fixture::new();
        let other = git_stdout(&fixture.source, &["rev-parse", "main"]);
        git(
            &fixture.source,
            &["update-ref", "refs/pull/46/head", &other],
        );
        let repository = fixture.base_repository(false);
        let result = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(46, "feature/topic", "unconfigured/project"),
            None,
        );
        assert!(matches!(result, Err(MaterializeError::HeadChanged { .. })));
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository.path)
                .args([
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/heads/pr/46-feature-topic"
                ])
                .status()
                .unwrap()
                .code()
                .is_some_and(|code| code != 0)
        );
        assert!(!fixture.repository_root.join("project-pr-46").exists());
    }

    #[test]
    fn incomplete_worktree_cleanup_removes_only_the_owned_destination() {
        let directory = tempfile::tempdir().unwrap();
        let repository_path = directory.path().join("project.git");
        git(
            directory.path(),
            &["init", "--bare", repository_path.to_str().unwrap()],
        );
        let destination = directory.path().join(".wt-incomplete-worktree-fixture");
        let unrelated = directory.path().join("unrelated");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("partial"), "partial").unwrap();
        fs::write(&unrelated, "safe").unwrap();
        let repository = RepositoryConfig {
            path: repository_path,
            label: None,
            worktree_root: None,
            github_remote: None,
            github_remotes: Default::default(),
            github_preferred_remote: None,
        };
        cleanup_owned_incomplete_worktree(&repository, &destination);
        assert!(!destination.exists());
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "safe");
    }

    #[test]
    fn fast_forwards_safe_branch_and_preserves_ahead_diverged_and_other_pr_branches() {
        let fixture = Fixture::new();
        let repository = fixture.local_repository(true);
        git(
            &repository.path,
            &["fetch", "fork", "+refs/heads/*:refs/remotes/fork/*"],
        );
        git(
            &repository.path,
            &["branch", "safe", "refs/remotes/fork/main"],
        );
        git(
            &repository.path,
            &["branch", "feature/topic", "refs/remotes/fork/ahead"],
        );
        git(
            &repository.path,
            &["branch", "diverged", "refs/remotes/fork/divergent"],
        );
        git(
            &repository.path,
            &["branch", "claimed", "refs/remotes/fork/claimed"],
        );
        git(
            &repository.path,
            &["config", "branch.claimed.wt-pr", "github.com/other/repo#7"],
        );

        let safe = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(41, "safe", "contributor/project"),
            None,
        )
        .unwrap();
        assert_eq!(safe.branch, "safe");
        assert_eq!(branch_head(&repository.path, "safe"), fixture.target);

        let ahead_head = branch_head(&repository.path, "feature/topic");
        let ahead = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(42, "feature/topic", "contributor/project"),
            None,
        )
        .unwrap();
        assert_eq!(ahead.branch, "feature/topic-pr-42");
        assert_eq!(branch_head(&repository.path, "feature/topic"), ahead_head);

        let diverged_head = branch_head(&repository.path, "diverged");
        let diverged = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(43, "diverged", "contributor/project"),
            None,
        )
        .unwrap();
        assert_eq!(diverged.branch, "diverged-pr-43");
        assert_eq!(branch_head(&repository.path, "diverged"), diverged_head);

        let claimed = materialize_pull_request(
            &SystemGit,
            &SystemFetchRunner,
            &repository,
            &fixture.repository_root,
            &fixture.authored(44, "claimed", "contributor/project"),
            None,
        )
        .unwrap();
        assert_eq!(claimed.branch, "claimed-pr-44");
        assert_eq!(
            marker(&repository.path, "claimed"),
            "github.com/other/repo#7"
        );
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        source: PathBuf,
        repository_root: PathBuf,
        worktree_root: PathBuf,
        target: String,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let repository_root = directory.path().join("repositories");
            let worktree_root = directory.path().join("worktrees");
            fs::create_dir_all(&repository_root).unwrap();
            git(directory.path(), &["init", source.to_str().unwrap()]);
            git(&source, &["config", "user.email", "test@example.com"]);
            git(&source, &["config", "user.name", "Test User"]);
            fs::write(source.join("file"), "base\n").unwrap();
            git(&source, &["add", "file"]);
            git(&source, &["commit", "-m", "base"]);
            git(&source, &["branch", "-M", "main"]);
            git(&source, &["checkout", "-b", "feature/topic"]);
            fs::write(source.join("file"), "target\n").unwrap();
            git(&source, &["commit", "-am", "target"]);
            let target = git_stdout(&source, &["rev-parse", "HEAD"]);
            for branch in ["safe", "diverged", "claimed"] {
                git(&source, &["branch", branch, &target]);
            }
            git(&source, &["checkout", "-b", "ahead"]);
            fs::write(source.join("ahead"), "ahead\n").unwrap();
            git(&source, &["add", "ahead"]);
            git(&source, &["commit", "-m", "ahead"]);
            git(&source, &["checkout", "-b", "divergent", "main"]);
            fs::write(source.join("diverged"), "diverged\n").unwrap();
            git(&source, &["add", "diverged"]);
            git(&source, &["commit", "-m", "diverged"]);
            Self {
                _directory: directory,
                source,
                repository_root,
                worktree_root,
                target,
            }
        }

        fn local_repository(&self, configured_root: bool) -> RepositoryConfig {
            self.repository("local.git", "fork", "contributor", configured_root)
        }

        fn base_repository(&self, configured_root: bool) -> RepositoryConfig {
            self.repository("base.git", "origin", "team", configured_root)
        }

        fn repository(
            &self,
            name: &str,
            remote: &str,
            owner: &str,
            configured_root: bool,
        ) -> RepositoryConfig {
            let path = self.repository_root.join(name);
            git(
                &self.repository_root,
                &["init", "--bare", path.to_str().unwrap()],
            );
            git(
                &path,
                &["remote", "add", remote, self.source.to_str().unwrap()],
            );
            let mut github_remotes = BTreeMap::new();
            github_remotes.insert(
                remote.to_owned(),
                GitHubRepositoryIdentity::canonical("github.com", owner, "project"),
            );
            RepositoryConfig {
                path: fs::canonicalize(path).unwrap(),
                label: None,
                worktree_root: configured_root.then(|| self.worktree_root.clone()),
                github_remote: Some(remote.to_owned()),
                github_remotes,
                github_preferred_remote: Some(remote.to_owned()),
            }
        }

        fn authored(
            &self,
            number: u64,
            branch: &str,
            head_repository: &str,
        ) -> AuthoredPullRequest {
            AuthoredPullRequest {
                identity: CanonicalPullRequestId {
                    repository: GitHubRepositoryIdentity::canonical(
                        "github.com",
                        "team",
                        "project",
                    ),
                    number,
                },
                author: "viewer".to_owned(),
                pull_request: PullRequest {
                    number,
                    title: "Test".to_owned(),
                    url: format!("https://github.com/team/project/pull/{number}"),
                    state: PullRequestState::Open,
                    updated_at: "2026-08-06T00:00:00Z".to_owned(),
                    review_decision: None,
                    auto_merge: false,
                    base: PullRequestIdentity {
                        repository: Some("team/project".to_owned()),
                        branch: "main".to_owned(),
                        oid: None,
                    },
                    head: PullRequestIdentity {
                        repository: Some(head_repository.to_owned()),
                        branch: branch.to_owned(),
                        oid: Some(self.target.clone()),
                    },
                    checks: CheckRollup::Success,
                },
            }
        }
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn marker(repository: &Path, branch: &str) -> String {
        git_stdout(
            repository,
            &["config", "--local", "--get", &branch_marker_key(branch)],
        )
    }

    fn branch_head(repository: &Path, branch: &str) -> String {
        git_stdout(repository, &["rev-parse", &format!("refs/heads/{branch}")])
    }
}
