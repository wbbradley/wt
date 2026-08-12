use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CATALOG_VERSION: u32 = 1;
pub const DEFAULT_GITHUB_REFRESH_INTERVAL_SECS: u64 = 300;
pub const DEFAULT_REPOSITORY_ROOT_EXPRESSION: &str = "~/src";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(default = "default_github_refresh_interval_secs")]
    pub github_refresh_interval_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub github_hosts: Vec<String>,
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            github_refresh_interval_secs: DEFAULT_GITHUB_REFRESH_INTERVAL_SECS,
            repository_root: None,
            github_hosts: Vec::new(),
            repositories: Vec::new(),
        }
    }
}

impl Catalog {
    pub fn repository_root_expression(&self) -> &str {
        self.repository_root
            .as_deref()
            .unwrap_or(DEFAULT_REPOSITORY_ROOT_EXPRESSION)
    }

    pub fn effective_github_hosts<'a>(
        &self,
        inferred_hosts: impl IntoIterator<Item = &'a str>,
    ) -> BTreeSet<String> {
        let mut hosts = BTreeSet::from(["github.com".to_owned()]);
        for host in self.github_hosts.iter().map(String::as_str) {
            let host = host.trim();
            if !host.is_empty() {
                hosts.insert(host.to_ascii_lowercase());
            }
        }
        for host in inferred_hosts {
            let host = host.trim();
            if !host.is_empty() {
                hosts.insert(host.to_ascii_lowercase());
            }
        }
        hosts
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub github_remotes: BTreeMap<String, GitHubRepositoryIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_preferred_remote: Option<String>,
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

#[derive(Clone, Debug, Deserialize, Hash, Ord, PartialEq, PartialOrd, Eq, Serialize)]
pub struct GitHubRepositoryIdentity {
    pub host: String,
    pub owner: String,
    pub repository: String,
}

impl GitHubRepositoryIdentity {
    pub fn canonical(host: &str, owner: &str, repository: &str) -> Self {
        Self {
            host: host.to_ascii_lowercase(),
            owner: owner.to_ascii_lowercase(),
            repository: repository.to_ascii_lowercase(),
        }
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repository)
    }
}

#[derive(Clone, Debug, Deserialize, Hash, Ord, PartialEq, PartialOrd, Eq, Serialize)]
pub struct CanonicalPullRequestId {
    pub repository: GitHubRepositoryIdentity,
    pub number: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Success,
    Failure,
    Pending,
    Expected,
    Error,
    Neutral,
    Skipped,
    Unknown,
}

impl CheckState {
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Failure | Self::Error)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PullRequestCheck {
    pub name: String,
    pub state: CheckState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    pub required: bool,
    #[serde(default)]
    pub source_order: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCheckReadiness {
    Ready,
    Failure,
    Pending,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerKind {
    User,
    Team,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReviewRequest {
    pub id: String,
    pub name: String,
    pub kind: ReviewerKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmittedReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReviewerReview {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_id: Option<u64>,
    pub reviewer: String,
    pub state: SubmittedReviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReadiness {
    Approved,
    ChangesRequested,
    Waiting,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    InlineThread,
    ReviewSummary,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PullRequestFeedback {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub kind: FeedbackKind,
    pub author: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
    #[serde(default)]
    pub outdated: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeConflictState {
    Clean,
    Conflicting,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct PullRequestDetails {
    #[serde(default)]
    pub checks: Vec<PullRequestCheck>,
    #[serde(default)]
    pub check_contexts_complete: bool,
    #[serde(default)]
    pub review_requests: Vec<ReviewRequest>,
    #[serde(default)]
    pub reviewer_reviews: Vec<ReviewerReview>,
    #[serde(default)]
    pub reviews_complete: bool,
    #[serde(default)]
    pub feedback: Vec<PullRequestFeedback>,
    #[serde(default)]
    pub feedback_complete: bool,
    #[serde(default)]
    pub merge_conflict: MergeConflictState,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestAttentionSummary {
    pub required_checks: RequiredCheckReadiness,
    pub review: ReviewReadiness,
    pub unresolved_feedback: usize,
    pub optional_failures: usize,
    pub merge_conflict: MergeConflictState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredCheckSummary {
    pub readiness: RequiredCheckReadiness,
    pub passed: usize,
    pub total: usize,
    pub complete: bool,
}

impl RequiredCheckSummary {
    pub fn ratio_text(self) -> String {
        if !self.complete || self.readiness == RequiredCheckReadiness::Unknown {
            "unknown".to_owned()
        } else if self.total == 0 {
            "no required checks".to_owned()
        } else {
            format!("{}/{} required", self.passed, self.total)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewerSummaryToken {
    Requested,
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Unknown,
}

impl ReviewerSummaryToken {
    pub fn label(self) -> &'static str {
        match self {
            Self::Requested => "req",
            Self::Approved => "✓ approved",
            Self::ChangesRequested => "✗ changes",
            Self::Commented => "◉ commented",
            Self::Dismissed => "⊘ dismissed",
            Self::Unknown => "○ unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewerView {
    pub identity: String,
    pub name: String,
    pub kind: ReviewerKind,
    pub requested: bool,
    pub request_id: Option<String>,
    pub review_id: Option<String>,
    pub review_database_id: Option<u64>,
    pub state: Option<SubmittedReviewState>,
}

impl PullRequestAttentionSummary {
    pub fn is_actionable(self) -> bool {
        self.required_checks == RequiredCheckReadiness::Failure
            || self.review == ReviewReadiness::ChangesRequested
            || self.unresolved_feedback > 0
            || self.optional_failures > 0
            || self.merge_conflict == MergeConflictState::Conflicting
    }
}

impl PullRequestDetails {
    pub fn unresolved_feedback(&self) -> impl Iterator<Item = &PullRequestFeedback> {
        self.feedback
            .iter()
            .filter(|feedback| feedback.kind == FeedbackKind::InlineThread)
    }

    pub fn normalize_checks(&mut self) {
        let mut checks = BTreeMap::<String, PullRequestCheck>::new();
        for check in std::mem::take(&mut self.checks) {
            let key = check.name.to_ascii_lowercase();
            match checks.get(&key) {
                Some(current) if !newer_check(&check, current) => {}
                _ => {
                    checks.insert(key, check);
                }
            }
        }
        self.checks = checks.into_values().collect();
        self.checks.sort_by_key(|check| check.source_order);
    }

    pub fn fold_latest_reviews(&mut self) {
        let mut reviews = BTreeMap::<String, ReviewerReview>::new();
        for review in std::mem::take(&mut self.reviewer_reviews) {
            let key = review.reviewer.to_ascii_lowercase();
            match reviews.get(&key) {
                Some(current) if !newer_review(&review, current) => {}
                _ => {
                    reviews.insert(key, review);
                }
            }
        }
        self.reviewer_reviews = reviews.into_values().collect();
    }

    pub fn required_check_readiness(&self) -> RequiredCheckReadiness {
        if !self.check_contexts_complete {
            return RequiredCheckReadiness::Unknown;
        }
        let required: Vec<_> = self.checks.iter().filter(|check| check.required).collect();
        if required
            .iter()
            .any(|check| matches!(check.state, CheckState::Failure | CheckState::Error))
        {
            RequiredCheckReadiness::Failure
        } else if required
            .iter()
            .any(|check| matches!(check.state, CheckState::Pending | CheckState::Expected))
        {
            RequiredCheckReadiness::Pending
        } else if required
            .iter()
            .any(|check| matches!(check.state, CheckState::Unknown))
        {
            RequiredCheckReadiness::Unknown
        } else {
            RequiredCheckReadiness::Ready
        }
    }

    pub fn review_readiness(&self) -> ReviewReadiness {
        if !self.reviews_complete {
            return ReviewReadiness::Unknown;
        }
        if self
            .reviewer_reviews
            .iter()
            .any(|review| review.state == SubmittedReviewState::ChangesRequested)
        {
            ReviewReadiness::ChangesRequested
        } else if !self.review_requests.is_empty() {
            ReviewReadiness::Waiting
        } else if self
            .reviewer_reviews
            .iter()
            .any(|review| review.state == SubmittedReviewState::Approved)
        {
            ReviewReadiness::Approved
        } else {
            ReviewReadiness::Unknown
        }
    }

    pub fn attention_summary(&self) -> PullRequestAttentionSummary {
        PullRequestAttentionSummary {
            required_checks: self.required_check_readiness(),
            review: self.review_readiness(),
            unresolved_feedback: self.unresolved_feedback().count(),
            optional_failures: if self.check_contexts_complete {
                self.checks
                    .iter()
                    .filter(|check| !check.required && check.state.is_actionable())
                    .count()
            } else {
                0
            },
            merge_conflict: self.merge_conflict,
        }
    }

    pub fn required_check_summary(&self) -> RequiredCheckSummary {
        let required = self
            .checks
            .iter()
            .filter(|check| check.required)
            .collect::<Vec<_>>();
        RequiredCheckSummary {
            readiness: self.required_check_readiness(),
            passed: required
                .iter()
                .filter(|check| {
                    matches!(
                        check.state,
                        CheckState::Success | CheckState::Neutral | CheckState::Skipped
                    )
                })
                .count(),
            total: required.len(),
            complete: self.check_contexts_complete,
        }
    }

    pub fn reviewers(&self) -> Vec<ReviewerView> {
        let mut reviewers = BTreeMap::<String, ReviewerView>::new();
        for request in &self.review_requests {
            let identity = request.name.to_ascii_lowercase();
            let reviewer = reviewers
                .entry(identity.clone())
                .or_insert_with(|| ReviewerView {
                    identity,
                    name: request.name.clone(),
                    kind: request.kind,
                    requested: false,
                    request_id: None,
                    review_id: None,
                    review_database_id: None,
                    state: None,
                });
            reviewer.name.clone_from(&request.name);
            reviewer.kind = request.kind;
            reviewer.requested = true;
            reviewer.request_id = Some(request.id.clone());
        }
        let mut latest_reviews = BTreeMap::<String, &ReviewerReview>::new();
        for review in &self.reviewer_reviews {
            let identity = review.reviewer.to_ascii_lowercase();
            match latest_reviews.get(&identity) {
                Some(current) if !newer_review(review, current) => {}
                _ => {
                    latest_reviews.insert(identity, review);
                }
            }
        }
        for (identity, review) in latest_reviews {
            let reviewer = reviewers
                .entry(identity.clone())
                .or_insert_with(|| ReviewerView {
                    identity,
                    name: review.reviewer.clone(),
                    kind: ReviewerKind::User,
                    requested: false,
                    request_id: None,
                    review_id: None,
                    review_database_id: None,
                    state: None,
                });
            reviewer.name.clone_from(&review.reviewer);
            reviewer.review_id = Some(review.id.clone());
            reviewer.review_database_id = review.database_id;
            reviewer.state = Some(review.state);
        }
        for feedback in self
            .feedback
            .iter()
            .filter(|feedback| feedback.kind == FeedbackKind::ReviewSummary)
        {
            let identity = feedback.author.to_ascii_lowercase();
            reviewers
                .entry(identity.clone())
                .or_insert_with(|| ReviewerView {
                    identity,
                    name: feedback.author.clone(),
                    kind: ReviewerKind::User,
                    requested: false,
                    request_id: None,
                    review_id: None,
                    review_database_id: None,
                    state: Some(SubmittedReviewState::Unknown),
                });
        }
        reviewers.into_values().collect()
    }

    pub fn reviewer_summary(&self) -> Vec<ReviewerSummaryToken> {
        let reviewers = self.reviewers();
        let mut tokens = Vec::new();
        if reviewers.iter().any(|reviewer| reviewer.requested) {
            tokens.push(ReviewerSummaryToken::Requested);
        }
        for (state, token) in [
            (
                SubmittedReviewState::Approved,
                ReviewerSummaryToken::Approved,
            ),
            (
                SubmittedReviewState::ChangesRequested,
                ReviewerSummaryToken::ChangesRequested,
            ),
            (
                SubmittedReviewState::Commented,
                ReviewerSummaryToken::Commented,
            ),
            (
                SubmittedReviewState::Dismissed,
                ReviewerSummaryToken::Dismissed,
            ),
            (SubmittedReviewState::Unknown, ReviewerSummaryToken::Unknown),
        ] {
            if reviewers
                .iter()
                .any(|reviewer| reviewer.state == Some(state))
            {
                tokens.push(token);
            }
        }
        tokens
    }
}

fn newer_check(candidate: &PullRequestCheck, current: &PullRequestCheck) -> bool {
    candidate.completed_at > current.completed_at
        || (candidate.completed_at == current.completed_at
            && candidate.source_order > current.source_order)
}

fn newer_review(candidate: &ReviewerReview, current: &ReviewerReview) -> bool {
    candidate.submitted_at > current.submitted_at
        || (candidate.submitted_at == current.submitted_at && candidate.id > current.id)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuthoredPullRequest {
    pub identity: CanonicalPullRequestId,
    pub author: String,
    pub pull_request: PullRequest,
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
    pub unstaged: usize,
    pub untracked: usize,
}

impl WorktreeStatus {
    pub fn is_dirty(&self) -> bool {
        self.staged > 0 || self.unstaged > 0 || self.untracked > 0
    }

    pub fn summary(&self) -> String {
        format!(
            "{} staged, {} unstaged, {} untracked",
            self.staged, self.unstaged, self.untracked
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PullRequestIdentity {
    pub repository: Option<String>,
    pub branch: String,
    pub oid: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub updated_at: String,
    pub review_decision: Option<String>,
    pub auto_merge: bool,
    pub base: PullRequestIdentity,
    pub head: PullRequestIdentity,
    pub checks: CheckRollup,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RateLimit {
    pub remaining: u64,
    pub reset_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GitHubBranchData {
    pub pull_request: Option<PullRequest>,
    pub warnings: Vec<String>,
    pub rate_limit: Option<RateLimit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_defaults_repository_root_without_serializing_it() {
        let catalog = Catalog::default();
        assert_eq!(
            catalog.repository_root_expression(),
            DEFAULT_REPOSITORY_ROOT_EXPRESSION
        );
        let encoded = serde_json::to_value(catalog).unwrap();
        assert!(encoded.get("repository_root").is_none());
        assert!(encoded.get("github_hosts").is_none());
    }

    #[test]
    fn github_hosts_are_normalized_deduplicated_and_include_github_com() {
        let catalog = Catalog {
            github_hosts: vec![" GHE.EXAMPLE ".to_owned(), "github.com".to_owned()],
            ..Catalog::default()
        };
        assert_eq!(
            catalog.effective_github_hosts(["ghe.example", "other.example"]),
            BTreeSet::from([
                "ghe.example".to_owned(),
                "github.com".to_owned(),
                "other.example".to_owned(),
            ])
        );
    }

    fn check(name: &str, state: CheckState, required: bool, order: usize) -> PullRequestCheck {
        PullRequestCheck {
            name: name.to_owned(),
            state,
            target_url: None,
            required,
            source_order: order,
            completed_at: Some(format!("2026-01-{order:02}T00:00:00Z")),
        }
    }

    #[test]
    fn check_normalization_prefers_newest_duplicate_and_required_only_rollup() {
        let mut details = PullRequestDetails {
            checks: vec![
                check("build", CheckState::Failure, true, 1),
                check("lint", CheckState::Failure, false, 2),
                check("Build", CheckState::Success, true, 3),
            ],
            check_contexts_complete: true,
            ..PullRequestDetails::default()
        };

        details.normalize_checks();

        assert_eq!(details.checks.len(), 2);
        assert_eq!(details.checks[1].name, "Build");
        let summary = details.attention_summary();
        assert_eq!(summary.required_checks, RequiredCheckReadiness::Ready);
        assert_eq!(summary.optional_failures, 1);
    }

    #[test]
    fn incomplete_pending_and_failed_required_checks_do_not_report_ready() {
        let mut details = PullRequestDetails {
            checks: vec![check("build", CheckState::Pending, true, 1)],
            ..PullRequestDetails::default()
        };
        assert_eq!(
            details.required_check_readiness(),
            RequiredCheckReadiness::Unknown
        );
        details.check_contexts_complete = true;
        assert_eq!(
            details.required_check_readiness(),
            RequiredCheckReadiness::Pending
        );
        details.checks[0].state = CheckState::Error;
        assert_eq!(
            details.required_check_readiness(),
            RequiredCheckReadiness::Failure
        );
    }

    #[test]
    fn reviewer_folding_keeps_latest_state_and_attention_counts_threads() {
        let review = |id: &str, state, submitted_at: &str| ReviewerReview {
            id: id.to_owned(),
            database_id: None,
            reviewer: "octocat".to_owned(),
            state,
            submitted_at: Some(submitted_at.to_owned()),
        };
        let mut details = PullRequestDetails {
            reviewer_reviews: vec![
                review("old", SubmittedReviewState::Approved, "2026-01-01"),
                review("new", SubmittedReviewState::ChangesRequested, "2026-01-02"),
            ],
            reviews_complete: true,
            feedback: vec![
                PullRequestFeedback {
                    id: "comment".to_owned(),
                    database_id: Some(7),
                    thread_id: Some("thread".to_owned()),
                    kind: FeedbackKind::InlineThread,
                    author: "octocat".to_owned(),
                    body: "please fix".to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: None,
                    outdated: false,
                },
                PullRequestFeedback {
                    id: "historical-summary".to_owned(),
                    database_id: Some(8),
                    thread_id: None,
                    kind: FeedbackKind::ReviewSummary,
                    author: "octocat".to_owned(),
                    body: "earlier review summary".to_owned(),
                    path: None,
                    permalink: None,
                    outdated: false,
                },
            ],
            feedback_complete: true,
            merge_conflict: MergeConflictState::Conflicting,
            ..PullRequestDetails::default()
        };

        details.fold_latest_reviews();

        assert_eq!(details.reviewer_reviews.len(), 1);
        let summary = details.attention_summary();
        assert_eq!(details.unresolved_feedback().count(), 1);
        assert_eq!(summary.review, ReviewReadiness::ChangesRequested);
        assert_eq!(summary.unresolved_feedback, 1);
        assert_eq!(summary.merge_conflict, MergeConflictState::Conflicting);
        assert!(summary.is_actionable());
    }

    #[test]
    fn waiting_checks_and_review_requests_are_not_actionable_attention() {
        let details = PullRequestDetails {
            checks: vec![check("build", CheckState::Pending, true, 1)],
            check_contexts_complete: true,
            review_requests: vec![ReviewRequest {
                id: "reviewer".to_owned(),
                name: "reviewer".to_owned(),
                kind: ReviewerKind::User,
            }],
            reviews_complete: true,
            feedback_complete: true,
            merge_conflict: MergeConflictState::Clean,
            ..PullRequestDetails::default()
        };

        let summary = details.attention_summary();

        assert_eq!(summary.required_checks, RequiredCheckReadiness::Pending);
        assert_eq!(summary.review, ReviewReadiness::Waiting);
        assert!(!summary.is_actionable());
    }

    #[test]
    fn section_views_report_incomplete_checks_and_combine_reviewer_identities() {
        let mut details = PullRequestDetails {
            checks: vec![
                check("required", CheckState::Success, true, 1),
                check("optional", CheckState::Failure, false, 2),
            ],
            review_requests: vec![ReviewRequest {
                id: "request".to_owned(),
                name: "OctoCat".to_owned(),
                kind: ReviewerKind::User,
            }],
            reviewer_reviews: vec![ReviewerReview {
                id: "review".to_owned(),
                database_id: Some(7),
                reviewer: "octocat".to_owned(),
                state: SubmittedReviewState::ChangesRequested,
                submitted_at: Some("2026-01-01".to_owned()),
            }],
            ..PullRequestDetails::default()
        };

        assert_eq!(details.required_check_summary().ratio_text(), "unknown");
        details.check_contexts_complete = true;
        assert_eq!(
            details.required_check_summary().ratio_text(),
            "1/1 required"
        );
        let reviewers = details.reviewers();
        assert_eq!(reviewers.len(), 1);
        assert!(reviewers[0].requested);
        assert_eq!(
            reviewers[0].state,
            Some(SubmittedReviewState::ChangesRequested)
        );
        assert_eq!(
            details.reviewer_summary(),
            vec![
                ReviewerSummaryToken::Requested,
                ReviewerSummaryToken::ChangesRequested,
            ]
        );
    }
}
