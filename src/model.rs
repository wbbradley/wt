use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CATALOG_VERSION: u32 = 1;
pub const DEFAULT_GITHUB_REFRESH_INTERVAL_SECS: u64 = 300;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(default = "default_github_refresh_interval_secs")]
    pub github_refresh_interval_secs: u64,
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            github_refresh_interval_secs: DEFAULT_GITHUB_REFRESH_INTERVAL_SECS,
            repositories: Vec::new(),
        }
    }
}

fn default_github_refresh_interval_secs() -> u64 {
    DEFAULT_GITHUB_REFRESH_INTERVAL_SECS
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RepositoryConfig {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_remote: Option<String>,
}

impl RepositoryConfig {
    pub fn display_label(&self) -> String {
        self.label.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .filter(|name| !name.is_empty())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryIdentity {
    pub anchor: PathBuf,
    pub common_git_dir: PathBuf,
    pub bare: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
}

impl WorktreeStatus {
    pub fn is_dirty(&self) -> bool {
        self.staged > 0 || self.modified > 0 || self.untracked > 0
    }

    pub fn summary(&self) -> String {
        format!(
            "{} staged, {} modified, {} untracked",
            self.staged, self.modified, self.untracked
        )
    }
}

impl Worktree {
    pub fn navigable(&self) -> bool {
        !self.bare
    }
}

#[derive(Debug)]
pub struct RepositoryDiscovery {
    pub repository: RepositoryConfig,
    pub result: Result<Vec<Worktree>, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

impl std::fmt::Display for PullRequestState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckRollup {
    Success,
    Failure,
    Pending,
    Expected,
    Error,
    Unknown,
}

impl std::fmt::Display for CheckRollup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Pending => "pending",
            Self::Expected => "expected",
            Self::Error => "error",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestIdentity {
    pub repository: Option<String>,
    pub branch: String,
    pub oid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub updated_at: String,
    pub review_decision: Option<String>,
    pub base: PullRequestIdentity,
    pub head: PullRequestIdentity,
    pub checks: CheckRollup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: u64,
    pub reset_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubBranchData {
    pub pull_request: Option<PullRequest>,
    pub warnings: Vec<String>,
    pub rate_limit: Option<RateLimit>,
}
