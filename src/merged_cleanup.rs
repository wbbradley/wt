use std::fs;
use std::path::{Path, PathBuf};

use crate::git::{self, GitRunner, SystemGit};
use crate::github::{AssociatedPullRequest, AuthoredHost, GitHubService, SystemCredentials};
use crate::materialize;
use crate::model::{CanonicalPullRequestId, PullRequestState, RepositoryConfig, Worktree};
use crate::operations;

pub trait PullRequestLookup {
    fn associated(
        &self,
        repository: &RepositoryConfig,
        worktree: &Worktree,
    ) -> Result<Vec<AssociatedPullRequest>, String>;

    fn exact(
        &self,
        repository: &RepositoryConfig,
        identity: &CanonicalPullRequestId,
    ) -> Result<AssociatedPullRequest, String>;
}

pub struct LivePullRequestLookup {
    service: GitHubService,
}

impl LivePullRequestLookup {
    pub fn new() -> Self {
        Self {
            service: GitHubService::new(),
        }
    }
}

impl PullRequestLookup for LivePullRequestLookup {
    fn associated(
        &self,
        repository: &RepositoryConfig,
        worktree: &Worktree,
    ) -> Result<Vec<AssociatedPullRequest>, String> {
        self.service
            .fetch_associated_pull_requests_with(
                &SystemGit,
                &SystemCredentials,
                repository,
                worktree,
            )
            .map_err(|error| error.to_string())
    }

    fn exact(
        &self,
        repository: &RepositoryConfig,
        identity: &CanonicalPullRequestId,
    ) -> Result<AssociatedPullRequest, String> {
        let host = AuthoredHost::inferred(&identity.repository.host, repository.path.clone());
        self.service
            .fetch_pull_request_with(&SystemCredentials, &host, identity)
            .map(|authored| AssociatedPullRequest {
                identity: authored.identity,
                pull_request: authored.pull_request,
            })
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct CleanupRecord {
    pub repository: RepositoryConfig,
    pub worktree: Option<Worktree>,
    pub identity: Option<CanonicalPullRequestId>,
    pub disposition: CleanupDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupDisposition {
    Eligible,
    Skipped(String),
}

impl CleanupRecord {
    pub fn eligible(&self) -> bool {
        self.disposition == CleanupDisposition::Eligible
    }

    pub fn path(&self) -> &Path {
        self.worktree
            .as_ref()
            .map(|worktree| worktree.path.as_path())
            .unwrap_or(&self.repository.path)
    }

    pub fn branch(&self) -> &str {
        self.worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref())
            .and_then(|branch| branch.strip_prefix("refs/heads/"))
            .unwrap_or("-")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupOutcome {
    Removed(PathBuf),
    Skipped { path: PathBuf, reason: String },
}

pub fn plan(
    runner: &dyn GitRunner,
    lookup: &dyn PullRequestLookup,
    repositories: &[RepositoryConfig],
    current_directory: &Path,
) -> Vec<CleanupRecord> {
    let mut records = Vec::new();
    for repository in repositories {
        match git::discover_worktrees(runner, &repository.path) {
            Ok(worktrees) => {
                for (index, worktree) in worktrees.iter().enumerate() {
                    records.push(plan_worktree(
                        runner,
                        lookup,
                        repository,
                        worktree,
                        index,
                        current_directory,
                    ));
                }
            }
            Err(error) => records.push(CleanupRecord {
                repository: repository.clone(),
                worktree: None,
                identity: None,
                disposition: CleanupDisposition::Skipped(format!("cannot list worktrees: {error}")),
            }),
        }
    }
    records.sort_by(|left, right| {
        (left.repository.display_label(), left.path())
            .cmp(&(right.repository.display_label(), right.path()))
    });
    records
}

pub fn execute(
    runner: &dyn GitRunner,
    lookup: &dyn PullRequestLookup,
    records: &[CleanupRecord],
    current_directory: &Path,
) -> Vec<CleanupOutcome> {
    records
        .iter()
        .filter(|record| record.eligible())
        .map(|record| execute_record(runner, lookup, record, current_directory))
        .collect()
}

fn execute_record(
    runner: &dyn GitRunner,
    lookup: &dyn PullRequestLookup,
    record: &CleanupRecord,
    current_directory: &Path,
) -> CleanupOutcome {
    let path = record.path().to_owned();
    let worktrees = match git::discover_worktrees(runner, &record.repository.path) {
        Ok(worktrees) => worktrees,
        Err(error) => {
            return CleanupOutcome::Skipped {
                path,
                reason: format!("revalidation failed: cannot list worktrees: {error}"),
            };
        }
    };
    let Some((index, worktree)) = worktrees
        .iter()
        .enumerate()
        .find(|(_, worktree)| same_path(&worktree.path, record.path()))
    else {
        return CleanupOutcome::Skipped {
            path,
            reason: "revalidation failed: worktree is no longer registered".to_owned(),
        };
    };
    let refreshed = plan_worktree(
        runner,
        lookup,
        &record.repository,
        worktree,
        index,
        current_directory,
    );
    if let CleanupDisposition::Skipped(reason) = refreshed.disposition {
        return CleanupOutcome::Skipped {
            path,
            reason: format!("revalidation failed: {reason}"),
        };
    }
    if refreshed.identity != record.identity
        || refreshed
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.head.as_ref())
            != record
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.head.as_ref())
        || refreshed
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_ref())
            != record
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.branch.as_ref())
    {
        return CleanupOutcome::Skipped {
            path,
            reason: "revalidation failed: worktree or pull request identity changed".to_owned(),
        };
    }
    match operations::remove(
        runner,
        &record.repository,
        &path.to_string_lossy(),
        current_directory,
    ) {
        Ok(_) => CleanupOutcome::Removed(path),
        Err(error) => CleanupOutcome::Skipped {
            path,
            reason: format!("removal failed after revalidation: {error}"),
        },
    }
}

fn plan_worktree(
    runner: &dyn GitRunner,
    lookup: &dyn PullRequestLookup,
    repository: &RepositoryConfig,
    worktree: &Worktree,
    index: usize,
    current_directory: &Path,
) -> CleanupRecord {
    let mut record = CleanupRecord {
        repository: repository.clone(),
        worktree: Some(worktree.clone()),
        identity: None,
        disposition: CleanupDisposition::Eligible,
    };
    if let Some(reason) = local_refusal(runner, worktree, index, current_directory) {
        record.disposition = CleanupDisposition::Skipped(reason);
        return record;
    }
    let branch = worktree
        .branch
        .as_deref()
        .and_then(|branch| branch.strip_prefix("refs/heads/"))
        .expect("local_refusal accepted a branchless worktree");
    let associated = match materialize::branch_pull_request_marker(runner, &repository.path, branch)
    {
        Ok(Some(identity)) => {
            record.identity = Some(identity.clone());
            match lookup.exact(repository, &identity) {
                Ok(associated) if associated.identity == identity => associated,
                Ok(_) => {
                    record.disposition = CleanupDisposition::Skipped(
                        "pull request marker resolved to a different identity".to_owned(),
                    );
                    return record;
                }
                Err(error) => {
                    record.disposition = CleanupDisposition::Skipped(format!(
                        "cannot refresh marked pull request: {error}"
                    ));
                    return record;
                }
            }
        }
        Ok(None) => match lookup.associated(repository, worktree) {
            Ok(mut associated) if associated.len() == 1 => associated.remove(0),
            Ok(associated) if associated.is_empty() => {
                record.disposition =
                    CleanupDisposition::Skipped("commit has no associated pull request".to_owned());
                return record;
            }
            Ok(_) => {
                record.disposition = CleanupDisposition::Skipped(
                    "commit has multiple associated pull requests".to_owned(),
                );
                return record;
            }
            Err(error) => {
                record.disposition = CleanupDisposition::Skipped(format!(
                    "cannot resolve associated pull request: {error}"
                ));
                return record;
            }
        },
        Err(error) => {
            record.disposition = CleanupDisposition::Skipped(error);
            return record;
        }
    };
    record.identity = Some(associated.identity);
    if associated.pull_request.state != PullRequestState::Merged {
        record.disposition = CleanupDisposition::Skipped(format!(
            "pull request is {}",
            associated.pull_request.state
        ));
        return record;
    }
    let Some(pull_request_head) = associated.pull_request.head.oid.as_deref() else {
        record.disposition =
            CleanupDisposition::Skipped("pull request head commit is missing".to_owned());
        return record;
    };
    if worktree.head.as_deref() != Some(pull_request_head) {
        record.disposition = CleanupDisposition::Skipped(format!(
            "worktree HEAD does not equal merged pull request head {pull_request_head}"
        ));
    }
    record
}

fn local_refusal(
    runner: &dyn GitRunner,
    worktree: &Worktree,
    index: usize,
    current_directory: &Path,
) -> Option<String> {
    if worktree.bare {
        return Some("bare repository anchor".to_owned());
    }
    if index == 0 {
        return Some("main worktree".to_owned());
    }
    if worktree.detached {
        return Some("detached worktree".to_owned());
    }
    if worktree.prunable.is_some() || !worktree.path.exists() {
        return Some("worktree path is missing or prunable".to_owned());
    }
    if let Some(reason) = &worktree.locked {
        return Some(if reason.is_empty() {
            "worktree is locked".to_owned()
        } else {
            format!("worktree is locked: {reason}")
        });
    }
    if contains_path(&worktree.path, current_directory) {
        return Some("worktree contains the current directory".to_owned());
    }
    if worktree.branch.is_none() {
        return Some("worktree has no local branch".to_owned());
    }
    if worktree.head.is_none() {
        return Some("worktree head commit is missing".to_owned());
    }
    match git::status(runner, &worktree.path) {
        Ok(status) if status.is_dirty() => Some(format!("local changes: {}", status.summary())),
        Ok(_) => None,
        Err(error) => Some(format!("cannot verify local status: {error}")),
    }
}

fn contains_path(worktree: &Path, candidate: &Path) -> bool {
    let worktree = fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
    let candidate = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_owned());
    candidate.starts_with(worktree)
}

fn same_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left).unwrap_or_else(|_| left.to_owned())
        == fs::canonicalize(right).unwrap_or_else(|_| right.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CheckRollup, GitHubRepositoryIdentity, PullRequest, PullRequestIdentity};
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeLookup {
        associated: Mutex<HashMap<PathBuf, Result<Vec<AssociatedPullRequest>, String>>>,
        exact: Mutex<HashMap<CanonicalPullRequestId, Result<AssociatedPullRequest, String>>>,
    }

    impl PullRequestLookup for FakeLookup {
        fn associated(
            &self,
            _repository: &RepositoryConfig,
            worktree: &Worktree,
        ) -> Result<Vec<AssociatedPullRequest>, String> {
            self.associated
                .lock()
                .unwrap()
                .iter()
                .find(|(path, _)| same_path(path, &worktree.path))
                .map(|(_, result)| result.clone())
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        fn exact(
            &self,
            _repository: &RepositoryConfig,
            identity: &CanonicalPullRequestId,
        ) -> Result<AssociatedPullRequest, String> {
            self.exact
                .lock()
                .unwrap()
                .get(identity)
                .cloned()
                .unwrap_or_else(|| Err("unexpected exact lookup".to_owned()))
        }
    }

    #[test]
    fn local_policy_rejects_every_unsafe_administrative_state_before_network() {
        let missing = PathBuf::from("/missing/worktree");
        let base = Worktree {
            path: missing.clone(),
            head: Some("head".to_owned()),
            branch: Some("refs/heads/topic".to_owned()),
            detached: false,
            bare: false,
            locked: None,
            prunable: None,
        };
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    bare: true,
                    ..base.clone()
                },
                1,
                Path::new("/else")
            ),
            Some("bare repository anchor".to_owned())
        );
        assert_eq!(
            local_refusal(&SystemGit, &base, 0, Path::new("/else")),
            Some("main worktree".to_owned())
        );
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    detached: true,
                    ..base.clone()
                },
                1,
                Path::new("/else")
            ),
            Some("detached worktree".to_owned())
        );
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    prunable: Some("gone".to_owned()),
                    ..base.clone()
                },
                1,
                Path::new("/else")
            ),
            Some("worktree path is missing or prunable".to_owned())
        );
        let existing = tempfile::tempdir().unwrap();
        let mut existing_worktree = base.clone();
        existing_worktree.path = existing.path().to_owned();
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    locked: Some("busy".to_owned()),
                    ..existing_worktree.clone()
                },
                1,
                Path::new("/else")
            ),
            Some("worktree is locked: busy".to_owned())
        );
        assert_eq!(
            local_refusal(&SystemGit, &existing_worktree, 1, existing.path()),
            Some("worktree contains the current directory".to_owned())
        );
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    branch: None,
                    ..existing_worktree.clone()
                },
                1,
                Path::new("/else")
            ),
            Some("worktree has no local branch".to_owned())
        );
        assert_eq!(
            local_refusal(
                &SystemGit,
                &Worktree {
                    head: None,
                    ..existing_worktree
                },
                1,
                Path::new("/else")
            ),
            Some("worktree head commit is missing".to_owned())
        );
    }

    #[test]
    fn plans_and_removes_only_an_exact_clean_merged_head_then_preserves_branch() {
        let fixture = Fixture::new();
        let lookup = FakeLookup::default();
        let associated = merged_pull_request(&fixture.head, 42);
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![associated.clone()]));
        let planned = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        let eligible = planned.iter().find(|record| record.eligible()).unwrap();
        assert_eq!(eligible.identity.as_ref(), Some(&associated.identity));
        let expected_path = std::fs::canonicalize(&fixture.worktree).unwrap();

        let outcomes = execute(&SystemGit, &lookup, &planned, fixture.root.path());
        assert!(matches!(
            &outcomes[..],
            [CleanupOutcome::Removed(path)] if path == &expected_path
        ));
        assert!(!fixture.worktree.exists());
        assert!(git_success(
            &fixture.repository.path,
            &["show-ref", "--verify", "--quiet", "refs/heads/topic"]
        ));
    }

    #[test]
    fn skips_ambiguous_nonmerged_mismatched_dirty_and_changed_after_preview() {
        let fixture = Fixture::new();
        let lookup = FakeLookup::default();
        let first = merged_pull_request(&fixture.head, 41);
        let second = merged_pull_request(&fixture.head, 42);
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![first.clone(), second]));
        let ambiguous = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(ambiguous.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason.contains("multiple associated")
        )));

        let mut open = first.clone();
        open.pull_request.state = PullRequestState::Open;
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![open]));
        let not_merged = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(not_merged.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason == "pull request is open"
        )));

        let mismatched = merged_pull_request("different", 41);
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![mismatched]));
        let mismatch = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(mismatch.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason.contains("does not equal")
        )));

        let mut missing_head = merged_pull_request(&fixture.head, 41);
        missing_head.pull_request.head.oid = None;
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![missing_head]));
        let missing_head = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(missing_head.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason == "pull request head commit is missing"
        )));

        lookup.associated.lock().unwrap().insert(
            fixture.worktree.clone(),
            Err("partial GitHub response".to_owned()),
        );
        let lookup_failure = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(lookup_failure.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason.contains("partial GitHub response")
        )));

        std::fs::write(fixture.worktree.join("untracked"), "change").unwrap();
        let dirty = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(dirty.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason.contains("1 untracked")
        )));
        std::fs::remove_file(fixture.worktree.join("untracked")).unwrap();

        let merged = merged_pull_request(&fixture.head, 41);
        lookup
            .associated
            .lock()
            .unwrap()
            .insert(fixture.worktree.clone(), Ok(vec![merged]));
        let planned = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        std::fs::write(fixture.worktree.join("late"), "change").unwrap();
        let outcomes = execute(&SystemGit, &lookup, &planned, fixture.root.path());
        assert!(matches!(
            &outcomes[0],
            CleanupOutcome::Skipped { reason, .. } if reason.contains("revalidation failed") && reason.contains("untracked")
        ));
        assert!(fixture.worktree.exists());
    }

    #[test]
    fn canonical_marker_forces_exact_lookup_and_malformed_marker_fails_closed() {
        let fixture = Fixture::new();
        let identity = merged_pull_request(&fixture.head, 42).identity;
        git(
            &fixture.repository.path,
            &["config", "branch.topic.wt-pr", "github.com/base/project#42"],
        );
        let lookup = FakeLookup::default();
        lookup
            .exact
            .lock()
            .unwrap()
            .insert(identity.clone(), Ok(merged_pull_request(&fixture.head, 42)));
        let planned = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(planned.iter().any(CleanupRecord::eligible));

        git(
            &fixture.repository.path,
            &["config", "branch.topic.wt-pr", "not-a-pr"],
        );
        let malformed = plan(
            &SystemGit,
            &lookup,
            std::slice::from_ref(&fixture.repository),
            fixture.root.path(),
        );
        assert!(malformed.iter().any(|record| matches!(
            &record.disposition,
            CleanupDisposition::Skipped(reason) if reason.contains("invalid pull request marker")
        )));
    }

    fn merged_pull_request(head: &str, number: u64) -> AssociatedPullRequest {
        let identity = CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical("github.com", "base", "project"),
            number,
        };
        AssociatedPullRequest {
            identity,
            pull_request: PullRequest {
                number,
                title: "change".to_owned(),
                url: format!("https://github.com/base/project/pull/{number}"),
                state: PullRequestState::Merged,
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
                review_decision: None,
                auto_merge: false,
                base: PullRequestIdentity {
                    repository: Some("base/project".to_owned()),
                    branch: "main".to_owned(),
                    oid: Some("base".to_owned()),
                },
                head: PullRequestIdentity {
                    repository: Some("base/project".to_owned()),
                    branch: "topic".to_owned(),
                    oid: Some(head.to_owned()),
                },
                checks: CheckRollup::Success,
            },
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        repository: RepositoryConfig,
        worktree: PathBuf,
        head: String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let anchor = root.path().join("main");
            let worktree = root.path().join("topic");
            git(
                root.path(),
                &["init", "-b", "main", anchor.to_str().unwrap()],
            );
            git(&anchor, &["config", "user.email", "test@example.com"]);
            git(&anchor, &["config", "user.name", "Test User"]);
            git(&anchor, &["commit", "--allow-empty", "-m", "initial"]);
            git(
                &anchor,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "topic",
                    worktree.to_str().unwrap(),
                    "main",
                ],
            );
            let head = git_output(&anchor, &["rev-parse", "topic"]);
            Self {
                root,
                repository: RepositoryConfig {
                    path: anchor,
                    label: Some("project".to_owned()),
                    worktree_root: None,
                    github_remote: None,
                    github_remotes: Default::default(),
                    github_preferred_remote: None,
                },
                worktree,
                head,
            }
        }
    }

    fn git(directory: &Path, arguments: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    fn git_success(directory: &Path, arguments: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap()
            .success()
    }

    fn git_output(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
