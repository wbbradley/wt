use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(test))]
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::github::{GitHubRefresh, RepositoryGitHubInput};
use crate::model::{
    AuthoredPullRequest, CanonicalPullRequestId, GitHubBranchData, GitHubRepositoryIdentity,
    PullRequestDetails, RepositoryConfig,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CachedRepositoryBinding {
    pub repository_path: PathBuf,
    pub github_remote: Option<String>,
    pub github_preferred_remote: Option<String>,
    pub github_remotes: BTreeMap<String, GitHubRepositoryIdentity>,
}

impl From<&RepositoryConfig> for CachedRepositoryBinding {
    fn from(repository: &RepositoryConfig) -> Self {
        Self {
            repository_path: repository.path.clone(),
            github_remote: repository.github_remote.clone(),
            github_preferred_remote: repository.github_preferred_remote.clone(),
            github_remotes: repository.github_remotes.clone(),
        }
    }
}

#[cfg(not(test))]
pub const CACHE_PATH_ENV: &str = "WT_CACHE_PATH";
const CACHE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CachedBranch {
    pub worktree: PathBuf,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_binding: Option<CachedRepositoryBinding>,
    pub data: GitHubBranchData,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CachedPullRequestDetails {
    pub identity: CanonicalPullRequestId,
    pub details: PullRequestDetails,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RemoteCache {
    pub version: u32,
    #[serde(default)]
    pub updated_at_epoch_seconds: u64,
    #[serde(default)]
    pub branches: Vec<CachedBranch>,
    #[serde(default)]
    pub authored_pull_requests: Vec<AuthoredPullRequest>,
    #[serde(default)]
    pub active_pull_requests: Vec<CanonicalPullRequestId>,
    #[serde(default)]
    pub pull_request_details: Vec<CachedPullRequestDetails>,
}

impl Default for RemoteCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            updated_at_epoch_seconds: 0,
            branches: Vec::new(),
            authored_pull_requests: Vec::new(),
            active_pull_requests: Vec::new(),
            pull_request_details: Vec::new(),
        }
    }
}

impl RemoteCache {
    fn reconcile_active_pull_requests(
        &mut self,
        refreshed: impl IntoIterator<Item = CanonicalPullRequestId>,
    ) {
        let candidates = self.active_pull_requests.drain(..).chain(refreshed);
        self.active_pull_requests = represented_pull_request_identities(&self.branches, candidates)
            .into_iter()
            .collect();
    }

    pub fn merge_branch_refresh(
        &mut self,
        inputs: &[RepositoryGitHubInput],
        refresh: &GitHubRefresh,
    ) {
        let current_branches: BTreeMap<PathBuf, (String, CachedRepositoryBinding)> = inputs
            .iter()
            .flat_map(|input| {
                input
                    .worktrees
                    .iter()
                    .filter(|worktree| input.refreshes_worktree(worktree))
                    .filter_map(|worktree| {
                        let branch = worktree
                            .branch
                            .as_ref()
                            .filter(|branch| branch.starts_with("refs/heads/"))?;
                        Some((
                            worktree.path.clone(),
                            (
                                branch.clone(),
                                CachedRepositoryBinding::from(&input.repository),
                            ),
                        ))
                    })
            })
            .collect();
        self.branches.retain(|cached| {
            current_branches
                .get(&cached.worktree)
                .is_some_and(|(branch, binding)| {
                    branch == &cached.branch && cached.repository_binding.as_ref() == Some(binding)
                })
        });

        for (worktree, result) in &refresh.branches {
            let Some((branch, binding)) = current_branches.get(worktree) else {
                continue;
            };
            let Ok(data) = result else {
                continue;
            };
            self.branches
                .retain(|cached| cached.worktree != *worktree || cached.branch != *branch);
            self.branches.push(CachedBranch {
                worktree: worktree.clone(),
                branch: branch.clone(),
                repository_binding: Some(binding.clone()),
                data: data.clone(),
            });
        }
        self.branches.sort_by(|left, right| {
            (&left.worktree, &left.branch).cmp(&(&right.worktree, &right.branch))
        });

        self.reconcile_active_pull_requests(refresh.active_pull_requests.iter().cloned());
        self.updated_at_epoch_seconds = epoch_seconds();
    }

    pub fn replace_authored(&mut self, pull_requests: Vec<AuthoredPullRequest>) {
        let mut unique = BTreeMap::new();
        for pull_request in pull_requests {
            unique.insert(pull_request.identity.clone(), pull_request);
        }
        self.authored_pull_requests = unique.into_values().collect();
        self.updated_at_epoch_seconds = epoch_seconds();
    }

    pub fn merge_pull_request_details(
        &mut self,
        refreshed: &BTreeMap<
            CanonicalPullRequestId,
            Result<PullRequestDetails, crate::github::GitHubError>,
        >,
    ) {
        let mut details: BTreeMap<_, _> = self
            .pull_request_details
            .drain(..)
            .map(|cached| (cached.identity, cached.details))
            .collect();
        for (identity, result) in refreshed {
            if let Ok(result) = result {
                details.insert(identity.clone(), result.clone());
            }
        }
        self.pull_request_details = details
            .into_iter()
            .map(|(identity, details)| CachedPullRequestDetails { identity, details })
            .collect();
        self.updated_at_epoch_seconds = epoch_seconds();
    }

    pub fn record_materialized_pull_request(
        &mut self,
        repository: &RepositoryConfig,
        worktree: &Path,
        branch: &str,
        pull_request: AuthoredPullRequest,
    ) {
        let branch = format!("refs/heads/{branch}");
        self.branches.retain(|cached| cached.worktree != worktree);
        self.branches.push(CachedBranch {
            worktree: worktree.to_owned(),
            branch,
            repository_binding: Some(CachedRepositoryBinding::from(repository)),
            data: GitHubBranchData {
                pull_request: Some(pull_request.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            },
        });
        self.branches.sort_by(|left, right| {
            (&left.worktree, &left.branch).cmp(&(&right.worktree, &right.branch))
        });

        let identity = pull_request.identity.clone();
        self.authored_pull_requests
            .retain(|cached| cached.identity != identity);
        self.authored_pull_requests.push(pull_request);
        self.authored_pull_requests
            .sort_by(|left, right| left.identity.cmp(&right.identity));

        self.reconcile_active_pull_requests([identity]);
        self.updated_at_epoch_seconds = epoch_seconds();
    }

    pub fn record_created_worktree(
        &mut self,
        repository: &RepositoryConfig,
        worktree: &Path,
        branch: &str,
    ) {
        let branch = format!("refs/heads/{branch}");
        self.branches.retain(|cached| cached.worktree != worktree);
        self.branches.push(CachedBranch {
            worktree: worktree.to_owned(),
            branch,
            repository_binding: Some(CachedRepositoryBinding::from(repository)),
            data: GitHubBranchData {
                pull_request: None,
                warnings: Vec::new(),
                rate_limit: None,
            },
        });
        self.branches.sort_by(|left, right| {
            (&left.worktree, &left.branch).cmp(&(&right.worktree, &right.branch))
        });
        self.reconcile_active_pull_requests(std::iter::empty());
        self.updated_at_epoch_seconds = epoch_seconds();
    }
}

fn represented_pull_request_identities(
    branches: &[CachedBranch],
    candidates: impl IntoIterator<Item = CanonicalPullRequestId>,
) -> BTreeSet<CanonicalPullRequestId> {
    candidates
        .into_iter()
        .filter(|identity| {
            branches.iter().any(|cached| {
                cached
                    .data
                    .pull_request
                    .as_ref()
                    .is_some_and(|pull_request| {
                        pull_request.number == identity.number
                            && pull_request
                                .base
                                .repository
                                .as_deref()
                                .is_some_and(|repository| {
                                    repository
                                        .eq_ignore_ascii_case(&identity.repository.full_name())
                                })
                    })
            })
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cannot read remote cache {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("remote cache {path} is not valid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("remote cache schema version {found} is newer than supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    #[error("cannot write remote cache {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("cannot encode remote cache: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("cannot lock remote cache {path}: {source}")]
    Lock { path: PathBuf, source: io::Error },
}

pub fn path(catalog_path: &Path) -> PathBuf {
    #[cfg(test)]
    {
        catalog_path.with_extension("github-cache.json")
    }
    #[cfg(not(test))]
    {
        if let Some(path) = env::var_os(CACHE_PATH_ENV) {
            return PathBuf::from(path);
        }
        env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|root| root.join("wt/github.json"))
            .unwrap_or_else(|| catalog_path.with_extension("github-cache.json"))
    }
}

pub fn load(path: &Path) -> Result<RemoteCache, CacheError> {
    load_existing(path).map(Option::unwrap_or_default)
}

pub fn update(path: &Path, mutate: impl FnOnce(&mut RemoteCache)) -> Result<(), CacheError> {
    let lock_path = sidecar_path(path);
    if let Some(parent) = lock_path.parent() {
        prepare_directory(parent, &lock_path)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| CacheError::Lock {
            path: lock_path.clone(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CacheError::Lock {
                path: lock_path.clone(),
                source,
            })?;
    }
    lock.lock_exclusive().map_err(|source| CacheError::Lock {
        path: lock_path,
        source,
    })?;
    let mut cache = match load_existing(path) {
        Ok(cache) => cache.unwrap_or_default(),
        Err(CacheError::Parse { .. }) => RemoteCache::default(),
        Err(error) => return Err(error),
    };
    mutate(&mut cache);
    save(path, &cache)
}

fn load_existing(path: &Path) -> Result<Option<RemoteCache>, CacheError> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CacheError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let cache: RemoteCache =
        serde_json::from_slice(&contents).map_err(|source| CacheError::Parse {
            path: path.to_owned(),
            source,
        })?;
    if cache.version > CACHE_VERSION {
        return Err(CacheError::FutureVersion {
            found: cache.version,
            supported: CACHE_VERSION,
        });
    }
    Ok(Some(cache))
}

fn save(path: &Path, cache: &RemoteCache) -> Result<(), CacheError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    prepare_directory(parent, path)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| CacheError::Write {
        path: path.to_owned(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CacheError::Write {
                path: path.to_owned(),
                source,
            })?;
    }
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut writer, cache)?;
        writer.flush().map_err(|source| CacheError::Write {
            path: path.to_owned(),
            source,
        })?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| CacheError::Write {
            path: path.to_owned(),
            source,
        })?;
    temporary.persist(path).map_err(|error| CacheError::Write {
        path: path.to_owned(),
        source: error.error,
    })?;
    sync_directory(parent).map_err(|source| CacheError::Write {
        path: path.to_owned(),
        source,
    })
}

fn prepare_directory(directory: &Path, error_path: &Path) -> Result<(), CacheError> {
    fs::create_dir_all(directory).map_err(|source| CacheError::Write {
        path: error_path.to_owned(),
        source,
    })
}

fn sidecar_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CheckRollup, PullRequest, PullRequestIdentity, PullRequestState, Worktree};

    fn authored(number: u64) -> AuthoredPullRequest {
        let repository =
            crate::model::GitHubRepositoryIdentity::canonical("github.com", "team", "project");
        AuthoredPullRequest {
            identity: CanonicalPullRequestId { repository, number },
            author: "viewer".to_owned(),
            pull_request: PullRequest {
                number,
                title: format!("change {number}"),
                url: format!("https://github.com/team/project/pull/{number}"),
                state: PullRequestState::Open,
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
                review_decision: None,
                auto_merge: false,
                base: PullRequestIdentity {
                    repository: Some("team/project".to_owned()),
                    branch: "main".to_owned(),
                    oid: None,
                },
                head: PullRequestIdentity {
                    repository: Some("team/project".to_owned()),
                    branch: format!("topic-{number}"),
                    oid: Some(format!("head-{number}")),
                },
                checks: CheckRollup::Success,
            },
        }
    }

    fn input(path: &Path, branch: &str) -> RepositoryGitHubInput {
        RepositoryGitHubInput {
            repository: crate::model::RepositoryConfig {
                path: path.to_owned(),
                label: None,
                worktree_root: None,
                github_remote: None,
                github_remotes: BTreeMap::new(),
                github_preferred_remote: None,
            },
            worktrees: vec![Worktree {
                path: path.to_owned(),
                head: Some("head".to_owned()),
                branch: Some(format!("refs/heads/{branch}")),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            }],
            trunk_branch: None,
        }
    }

    #[test]
    fn cache_round_trips_atomically_and_deduplicates_authored_prs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache/github.json");
        update(&path, |cache| {
            cache.replace_authored(vec![authored(2), authored(1), authored(2)]);
        })
        .unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(
            loaded
                .authored_pull_requests
                .iter()
                .map(|pull_request| pull_request.identity.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn canonical_details_round_trip_and_failed_refresh_retains_stale_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let identity = authored(1).identity;
        let details = PullRequestDetails {
            check_contexts_complete: true,
            reviews_complete: true,
            feedback_complete: true,
            ..PullRequestDetails::default()
        };
        update(&path, |cache| {
            cache.merge_pull_request_details(&BTreeMap::from([(
                identity.clone(),
                Ok(details.clone()),
            )]));
        })
        .unwrap();
        update(&path, |cache| {
            cache.merge_pull_request_details(&BTreeMap::from([(
                identity.clone(),
                Err(crate::github::GitHubError::Network("offline".to_owned())),
            )]));
        })
        .unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.pull_request_details.len(), 1);
        assert_eq!(loaded.pull_request_details[0].identity, identity);
        assert_eq!(loaded.pull_request_details[0].details, details);
    }

    #[test]
    fn legacy_cache_without_detail_field_remains_readable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        fs::write(
            &path,
            r#"{"version":1,"branches":[],"authored_pull_requests":[],"active_pull_requests":[]}"#,
        )
        .unwrap();

        let loaded = load(&path).unwrap();

        assert!(loaded.pull_request_details.is_empty());
    }

    #[test]
    fn legacy_branch_binding_is_pruned_without_discarding_independent_remote_data() {
        let path = PathBuf::from("/repo");
        let authored = authored(1);
        let details = CachedPullRequestDetails {
            identity: authored.identity.clone(),
            details: PullRequestDetails::default(),
        };
        let mut cache = RemoteCache {
            branches: vec![CachedBranch {
                worktree: path.clone(),
                branch: "refs/heads/topic".to_owned(),
                repository_binding: None,
                data: GitHubBranchData {
                    pull_request: Some(authored.pull_request.clone()),
                    warnings: Vec::new(),
                    rate_limit: None,
                },
            }],
            authored_pull_requests: vec![authored.clone()],
            pull_request_details: vec![details.clone()],
            ..RemoteCache::default()
        };

        cache.merge_branch_refresh(&[input(&path, "topic")], &GitHubRefresh::default());

        assert!(cache.branches.is_empty());
        assert_eq!(cache.authored_pull_requests, vec![authored]);
        assert_eq!(cache.pull_request_details, vec![details]);
    }

    #[test]
    fn branch_updates_reject_changed_branches_and_retain_success_on_failure() {
        let path = PathBuf::from("/repo");
        let mut cache = RemoteCache::default();
        let first = authored(1);
        let data = GitHubBranchData {
            pull_request: Some(first.pull_request.clone()),
            warnings: Vec::new(),
            rate_limit: None,
        };
        let mut refresh = GitHubRefresh::default();
        refresh.branches.insert(path.clone(), Ok(data.clone()));
        refresh.active_pull_requests.insert(first.identity.clone());
        cache.merge_branch_refresh(&[input(&path, "topic")], &refresh);
        assert_eq!(cache.branches[0].data, data);

        let mut failed = GitHubRefresh::default();
        failed.branches.insert(
            path.clone(),
            Err(crate::github::GitHubError::Network("offline".to_owned())),
        );
        cache.merge_branch_refresh(&[input(&path, "topic")], &failed);
        assert_eq!(cache.branches.len(), 1);
        assert_eq!(cache.active_pull_requests, vec![first.identity]);

        cache.merge_branch_refresh(&[input(&path, "other")], &failed);
        assert!(cache.branches.is_empty());
        assert!(cache.active_pull_requests.is_empty());
    }

    #[test]
    fn branch_refresh_prunes_legacy_active_identities_and_pr_less_success() {
        let path = PathBuf::from("/repo");
        let selected = authored(1);
        let hidden_by_legacy_cache = authored(2);
        let mut cache = RemoteCache {
            branches: vec![CachedBranch {
                worktree: path.clone(),
                branch: "refs/heads/topic".to_owned(),
                repository_binding: Some(CachedRepositoryBinding::from(
                    &input(&path, "topic").repository,
                )),
                data: GitHubBranchData {
                    pull_request: Some(selected.pull_request.clone()),
                    warnings: Vec::new(),
                    rate_limit: None,
                },
            }],
            active_pull_requests: vec![
                selected.identity.clone(),
                hidden_by_legacy_cache.identity.clone(),
            ],
            ..RemoteCache::default()
        };
        let mut failed = GitHubRefresh::default();
        failed.branches.insert(
            path.clone(),
            Err(crate::github::GitHubError::Network("offline".to_owned())),
        );

        cache.merge_branch_refresh(&[input(&path, "topic")], &failed);

        assert_eq!(cache.active_pull_requests, vec![selected.identity]);

        let mut no_pull_request = GitHubRefresh::default();
        no_pull_request.branches.insert(
            path.clone(),
            Ok(GitHubBranchData {
                pull_request: None,
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        cache.merge_branch_refresh(&[input(&path, "topic")], &no_pull_request);

        assert!(cache.active_pull_requests.is_empty());
    }

    #[test]
    fn trunk_worktrees_are_removed_from_the_branch_cache() {
        let path = PathBuf::from("/repo");
        let mut input = input(&path, "main");
        let data = GitHubBranchData {
            pull_request: Some(authored(1).pull_request),
            warnings: Vec::new(),
            rate_limit: None,
        };
        let mut cache = RemoteCache {
            branches: vec![CachedBranch {
                worktree: path.clone(),
                branch: "refs/heads/main".to_owned(),
                repository_binding: Some(CachedRepositoryBinding::from(&input.repository)),
                data,
            }],
            ..RemoteCache::default()
        };
        input.trunk_branch = Some("main".to_owned());

        cache.merge_branch_refresh(&[input], &GitHubRefresh::default());

        assert!(cache.branches.is_empty());
    }

    #[test]
    fn materialized_pull_request_replaces_cached_branch_and_deduplicates_identity() {
        let path = PathBuf::from("/repo/topic");
        let mut cache = RemoteCache {
            branches: vec![CachedBranch {
                worktree: path.clone(),
                branch: "refs/heads/old".to_owned(),
                repository_binding: None,
                data: GitHubBranchData {
                    pull_request: None,
                    warnings: vec!["stale".to_owned()],
                    rate_limit: None,
                },
            }],
            authored_pull_requests: vec![authored(2), authored(1)],
            active_pull_requests: vec![authored(1).identity, authored(2).identity.clone()],
            ..RemoteCache::default()
        };
        let refreshed = authored(2);
        let repository = input(Path::new("/repo"), "topic-2").repository;

        cache.record_materialized_pull_request(&repository, &path, "topic-2", refreshed.clone());

        assert_eq!(cache.branches.len(), 1);
        assert_eq!(cache.branches[0].worktree, path);
        assert_eq!(cache.branches[0].branch, "refs/heads/topic-2");
        assert_eq!(
            cache.branches[0]
                .data
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number),
            Some(2)
        );
        assert_eq!(
            cache
                .authored_pull_requests
                .iter()
                .map(|pull_request| pull_request.identity.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            cache
                .active_pull_requests
                .iter()
                .map(|identity| identity.number)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(cache.updated_at_epoch_seconds > 0);
    }

    #[test]
    fn malformed_and_future_cache_files_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let malformed = directory.path().join("malformed.json");
        fs::write(&malformed, b"{").unwrap();
        assert!(matches!(load(&malformed), Err(CacheError::Parse { .. })));
        update(&malformed, |cache| {
            cache.replace_authored(vec![authored(1)])
        })
        .unwrap();
        assert_eq!(load(&malformed).unwrap().authored_pull_requests.len(), 1);

        let future = directory.path().join("future.json");
        fs::write(&future, br#"{"version":999}"#).unwrap();
        assert!(matches!(
            load(&future),
            Err(CacheError::FutureVersion { found: 999, .. })
        ));
        assert!(matches!(
            update(&future, |_| {}),
            Err(CacheError::FutureVersion { found: 999, .. })
        ));
    }
}
