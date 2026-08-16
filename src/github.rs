use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::git::{GitRunner, SystemGit};
use crate::model::{
    AuthoredPullRequest, CanonicalPullRequestId, Catalog, CheckRollup, CheckState, FeedbackKind,
    GitHubBranchData, GitHubRepositoryIdentity, MergeConflictState, PullRequest, PullRequestCheck,
    PullRequestDetails, PullRequestFeedback, PullRequestIdentity, PullRequestState, RateLimit,
    RepositoryConfig, ReviewRequest, ReviewerKind, ReviewerReview, SubmittedReviewState, Worktree,
};

pub const MAX_BRANCHES_PER_BATCH: usize = 20;
pub const AUTHORED_PULL_REQUESTS_PER_PAGE: usize = 100;
pub const MAX_AUTHORED_PULL_REQUEST_PAGES: usize = 10;
pub const CHECK_CONTEXTS_PER_PAGE: usize = 100;
pub const MAX_CHECK_CONTEXT_PAGES: usize = 10;

const VIEWER_PULL_REQUEST_SEARCHES: [(&str, &str); 2] = [
    ("authored", "is:pr is:open author:@me"),
    ("assigned", "is:pr is:open assignee:@me"),
];

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum WebScheme {
    Http,
    Https,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct RemoteRepository {
    pub host: String,
    pub owner: String,
    pub name: String,
    scheme: WebScheme,
}

impl RemoteRepository {
    #[cfg(test)]
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    #[cfg(test)]
    pub fn rest_base(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com".to_owned()
        } else {
            format!("{}://{}/api/v3", self.scheme.as_str(), self.host)
        }
    }

    pub fn graphql_url(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("{}://{}/api/graphql", self.scheme.as_str(), self.host)
        }
    }

    pub fn identity(&self) -> GitHubRepositoryIdentity {
        GitHubRepositoryIdentity::canonical(&self.host, &self.owner, &self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteIdentityRefresh {
    pub changed: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestMapping {
    pub identity: CanonicalPullRequestId,
    pub mapped_repository: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredHost {
    pub host: String,
    pub graphql_url: String,
    pub credential_anchor: PathBuf,
}

impl AuthoredHost {
    pub fn inferred(host: &str, credential_anchor: PathBuf) -> Self {
        let graphql_url = if host == "github.com" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("https://{host}/api/graphql")
        };
        Self {
            host: host.to_owned(),
            graphql_url,
            credential_anchor,
        }
    }
}

#[derive(Clone, Debug)]
pub enum AuthoredRefreshEvent {
    Page {
        host: String,
        page: usize,
        pull_requests: Vec<AuthoredPullRequest>,
        warnings: Vec<String>,
    },
    Finished {
        complete: bool,
        warnings: Vec<String>,
        error: Option<String>,
    },
}

pub fn refresh_catalog_remote_identities(
    runner: &dyn GitRunner,
    catalog: &mut Catalog,
    refreshable_paths: &HashSet<PathBuf>,
) -> RemoteIdentityRefresh {
    let mut refresh = RemoteIdentityRefresh::default();
    for repository in catalog
        .repositories
        .iter_mut()
        .filter(|repository| refreshable_paths.contains(&repository.path))
    {
        match refresh_repository_remote_identities(runner, repository) {
            Ok(repository_refresh) => {
                refresh.changed |= repository_refresh.changed;
                refresh.warnings.extend(repository_refresh.warnings);
            }
            Err(error) => refresh.warnings.push(format!(
                "{}: unable to refresh GitHub remotes: {error}",
                repository.display_label()
            )),
        }
    }
    refresh
}

pub fn refresh_repository_remote_identities(
    runner: &dyn GitRunner,
    repository: &mut RepositoryConfig,
) -> Result<RemoteIdentityRefresh, GitHubError> {
    let names = required_git_value(runner, &repository.path, &["remote"])?;
    let remote_names: Vec<String> = names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let mut identities = BTreeMap::new();
    let mut warnings = Vec::new();
    for name in remote_names {
        let Some(url) =
            optional_git_value(runner, &repository.path, &["remote", "get-url", &name])?
        else {
            warnings.push(format!(
                "{}: remote {name:?} has no fetch URL",
                repository.display_label()
            ));
            continue;
        };
        let Ok(observed) = parse_remote_url(&url).map(|remote| remote.identity()) else {
            continue;
        };
        match repository.github_remotes.get(&name) {
            Some(existing) if existing != &observed => {
                warnings.push(format!(
                    "{}: remote {name:?} now resolves to {}/{} but the catalog retains {}/{}",
                    repository.display_label(),
                    observed.host,
                    observed.full_name(),
                    existing.host,
                    existing.full_name()
                ));
                identities.insert(name, existing.clone());
            }
            _ => {
                identities.insert(name, observed);
            }
        }
    }
    let preferred = repository
        .github_remote
        .as_ref()
        .filter(|name| identities.contains_key(*name))
        .cloned()
        .or_else(|| {
            identities
                .contains_key("origin")
                .then(|| "origin".to_owned())
        })
        .or_else(|| identities.keys().next().cloned());
    let changed =
        identities != repository.github_remotes || preferred != repository.github_preferred_remote;
    repository.github_remotes = identities;
    repository.github_preferred_remote = preferred;
    Ok(RemoteIdentityRefresh { changed, warnings })
}

pub fn inferred_github_hosts(catalog: &Catalog) -> BTreeSet<String> {
    catalog.effective_github_hosts(
        catalog
            .repositories
            .iter()
            .flat_map(|repository| repository.github_remotes.values())
            .map(|identity| identity.host.as_str()),
    )
}

pub fn canonical_pull_request_id(
    host: &str,
    pull_request: &PullRequest,
) -> Option<CanonicalPullRequestId> {
    let full_name = pull_request.base.repository.as_deref()?;
    let (owner, repository) = full_name.split_once('/')?;
    Some(CanonicalPullRequestId {
        repository: GitHubRepositoryIdentity::canonical(host, owner, repository),
        number: pull_request.number,
    })
}

pub fn map_pull_request_identities(
    catalog: &Catalog,
    pull_requests: impl IntoIterator<Item = CanonicalPullRequestId>,
    active: &HashSet<CanonicalPullRequestId>,
    usable: impl Fn(&RepositoryConfig) -> bool,
) -> Vec<PullRequestMapping> {
    let unique: BTreeSet<CanonicalPullRequestId> = pull_requests.into_iter().collect();
    unique
        .into_iter()
        .filter(|identity| !active.contains(identity))
        .map(|identity| {
            let mut configured = None;
            let mut origin = None;
            let mut earliest = None;
            for (index, repository) in catalog.repositories.iter().enumerate() {
                if !usable(repository) {
                    continue;
                }
                let matches = |remote: &str| {
                    repository
                        .github_remotes
                        .get(remote)
                        .is_some_and(|candidate| candidate == &identity.repository)
                };
                if !repository
                    .github_remotes
                    .values()
                    .any(|candidate| candidate == &identity.repository)
                {
                    continue;
                }
                earliest.get_or_insert(index);
                if repository.github_remote.as_deref().is_some_and(matches) {
                    configured.get_or_insert(index);
                }
                if matches("origin") {
                    origin.get_or_insert(index);
                }
            }
            PullRequestMapping {
                identity,
                mapped_repository: configured
                    .or(origin)
                    .or(earliest)
                    .map(|index| catalog.repositories[index].path.clone()),
            }
        })
        .collect()
}

impl WebScheme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSource {
    Environment,
    RepositoryGitConfig,
    GhCli,
}

#[derive(Clone)]
pub struct SecretToken(String);

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedToken {
    token: SecretToken,
    pub source: AuthSource,
}

impl ResolvedToken {
    pub(crate) fn expose(&self) -> &str {
        &self.token.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: &str) -> Self {
        Self {
            token: SecretToken(value.to_owned()),
            source: AuthSource::Environment,
        }
    }
}

pub trait CredentialProvider: Send + Sync {
    fn environment(&self, key: &str) -> Option<String>;
    fn repository_git_config(&self, anchor: &Path, key: &str) -> Option<String>;
    fn gh_token(&self, host: &str) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentials;

impl CredentialProvider for SystemCredentials {
    fn environment(&self, key: &str) -> Option<String> {
        env::var(key).ok().filter(|value| !value.trim().is_empty())
    }

    fn repository_git_config(&self, anchor: &Path, key: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(anchor)
            .args(["config", "--local", "--get", key])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        nonempty_lossy(&output.stdout)
    }

    fn gh_token(&self, host: &str) -> Option<String> {
        let output = Command::new("gh")
            .args(["auth", "token", "--hostname", host])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        nonempty_lossy(&output.stdout)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GitHubError {
    #[error("remote is not a supported GitHub SSH/HTTPS remote")]
    UnsupportedRemote,
    #[error("no GitHub token is configured for {host}")]
    NoToken { host: String },
    #[error("GitHub authentication is missing, invalid, or expired")]
    Unauthorized,
    #[error("GitHub permission denied: {0}")]
    Permission(String),
    #[error("GitHub SSO/SAML authorization required: {0}")]
    Sso(String),
    #[error("GitHub organization forbids classic personal access tokens: {0}")]
    ClassicPat(String),
    #[error("GitHub rate limit exhausted until {reset_at}")]
    RateLimited { reset_at: String },
    #[error("GitHub network failure: {0}")]
    Network(String),
    #[error("GitHub returned malformed data: {0}")]
    Malformed(String),
    #[error("GitHub API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("local Git metadata error: {0}")]
    LocalGit(String),
    #[error("branch {0:?} does not exist on its selected GitHub remote")]
    BranchNotFound(String),
    #[error("pull request {repository}#{number} is gone or inaccessible")]
    PullRequestUnavailable { repository: String, number: u64 },
}

#[derive(Clone, Debug)]
pub struct RepositoryGitHubInput {
    pub repository: RepositoryConfig,
    pub worktrees: Vec<Worktree>,
    pub trunk_branch: Option<String>,
}

impl RepositoryGitHubInput {
    pub fn refreshes_worktree(&self, worktree: &Worktree) -> bool {
        !worktree.bare
            && !worktree.detached
            && worktree
                .branch
                .as_deref()
                .and_then(|branch| branch.strip_prefix("refs/heads/"))
                .is_some_and(|branch| {
                    !branch.is_empty() && self.trunk_branch.as_deref() != Some(branch)
                })
    }
}

#[derive(Clone, Debug, Default)]
pub struct GitHubRefresh {
    pub branches: HashMap<PathBuf, Result<GitHubBranchData, GitHubError>>,
    pub active_pull_requests: HashSet<CanonicalPullRequestId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssociatedPullRequest {
    pub identity: CanonicalPullRequestId,
    pub pull_request: PullRequest,
}

#[derive(Clone)]
pub struct GitHubService {
    agent: ureq::Agent,
    suppressed_until: Arc<Mutex<HashMap<String, Suppression>>>,
}

#[derive(Clone, Debug)]
struct Suppression {
    epoch_seconds: u64,
    reset_at: String,
}

#[derive(Clone, Debug)]
struct BranchTarget {
    worktree: PathBuf,
    branch: String,
    head: Option<String>,
}

struct BranchBatchRefresh {
    branches: Vec<Result<GitHubBranchData, GitHubError>>,
    associations: Vec<Result<Vec<AssociatedPullRequest>, GitHubError>>,
    active_pull_requests: HashSet<CanonicalPullRequestId>,
}

type ParsedBatchData = (
    Vec<Result<GitHubBranchData, GitHubError>>,
    Vec<Result<Vec<AssociatedPullRequest>, GitHubError>>,
);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FetchGroupKey {
    remote: RemoteRepository,
    anchor: PathBuf,
}

impl GitHubService {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .build()
            .new_agent();
        Self {
            agent,
            suppressed_until: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn fetch_catalog(&self, inputs: &[RepositoryGitHubInput]) -> GitHubRefresh {
        self.fetch_catalog_with(&SystemGit, &SystemCredentials, inputs)
    }

    pub fn fetch_catalog_with(
        &self,
        runner: &dyn GitRunner,
        credentials: &dyn CredentialProvider,
        inputs: &[RepositoryGitHubInput],
    ) -> GitHubRefresh {
        let mut refresh = GitHubRefresh::default();
        let mut groups: HashMap<FetchGroupKey, Vec<BranchTarget>> = HashMap::new();
        for input in inputs {
            for worktree in input
                .worktrees
                .iter()
                .filter(|worktree| input.refreshes_worktree(worktree))
            {
                let Some(branch) = worktree
                    .branch
                    .as_deref()
                    .and_then(|branch| branch.strip_prefix("refs/heads/"))
                else {
                    continue;
                };
                match resolve_branch_remote(runner, &input.repository, branch) {
                    Ok(remote) => groups
                        .entry(FetchGroupKey {
                            remote,
                            anchor: input.repository.path.clone(),
                        })
                        .or_default()
                        .push(BranchTarget {
                            worktree: worktree.path.clone(),
                            branch: branch.to_owned(),
                            head: worktree.head.clone(),
                        }),
                    Err(error) => {
                        refresh.branches.insert(worktree.path.clone(), Err(error));
                    }
                }
            }
        }

        for (key, targets) in groups {
            let token = match resolve_token(credentials, &key.remote.host, &key.anchor) {
                Ok(token) => token,
                Err(error) => {
                    apply_group_error(&mut refresh, &targets, error);
                    continue;
                }
            };
            let _ = token.source;
            if let Some(error) = self.suppressed_error(&key.remote.host) {
                apply_group_error(&mut refresh, &targets, error);
                continue;
            }
            for chunk in targets.chunks(MAX_BRANCHES_PER_BATCH) {
                match self.fetch_batch(&key.remote, &token, chunk) {
                    Ok(batch) => {
                        refresh
                            .active_pull_requests
                            .extend(batch.active_pull_requests);
                        for (target, outcome) in chunk.iter().zip(batch.branches) {
                            refresh.branches.insert(target.worktree.clone(), outcome);
                        }
                    }
                    Err(error) => {
                        if let GitHubError::RateLimited { reset_at } = &error {
                            self.suppress(&key.remote.host, reset_at, None);
                        }
                        apply_group_error(&mut refresh, chunk, error);
                    }
                }
            }
        }
        refresh
    }

    pub fn fetch_associated_pull_requests_with(
        &self,
        runner: &dyn GitRunner,
        credentials: &dyn CredentialProvider,
        repository: &RepositoryConfig,
        worktree: &Worktree,
    ) -> Result<Vec<AssociatedPullRequest>, GitHubError> {
        let branch = worktree
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("refs/heads/"))
            .ok_or_else(|| GitHubError::Malformed("worktree has no local branch".to_owned()))?;
        let head = worktree
            .head
            .clone()
            .ok_or_else(|| GitHubError::Malformed("worktree head commit is missing".to_owned()))?;
        let remote = resolve_branch_remote(runner, repository, branch)?;
        let token = resolve_token(credentials, &remote.host, &repository.path)?;
        if let Some(error) = self.suppressed_error(&remote.host) {
            return Err(error);
        }
        let target = BranchTarget {
            worktree: worktree.path.clone(),
            branch: branch.to_owned(),
            head: Some(head),
        };
        let mut batch = self.fetch_batch(&remote, &token, &[target])?;
        batch
            .associations
            .pop()
            .ok_or_else(|| GitHubError::Malformed("association response is missing".to_owned()))?
    }

    pub fn fetch_authored_with(
        &self,
        credentials: &dyn CredentialProvider,
        hosts: &[AuthoredHost],
        mut publish: impl FnMut(AuthoredRefreshEvent),
    ) {
        let mut complete = true;
        let mut warnings = Vec::new();
        let mut errors = Vec::new();
        for host in hosts {
            let token = match resolve_token(credentials, &host.host, &host.credential_anchor) {
                Ok(token) => token,
                Err(error) => {
                    complete = false;
                    errors.push(format!("{}: {error}", host.host));
                    continue;
                }
            };
            if let Some(error) = self.suppressed_error(&host.host) {
                complete = false;
                errors.push(format!("{}: {error}", host.host));
                continue;
            }
            let mut published_page = 0;
            std::thread::scope(|scope| {
                let (sender, receiver) = std::sync::mpsc::channel();
                let token = &token;
                let handles = VIEWER_PULL_REQUEST_SEARCHES.map(|(search_kind, search_query)| {
                    let sender = sender.clone();
                    scope.spawn(move || {
                        self.fetch_viewer_search(host, &token, search_kind, search_query, sender);
                    })
                });
                drop(sender);

                for message in receiver {
                    match message {
                        ViewerSearchMessage::Page(result) => {
                            warnings.extend(result.warnings.clone());
                            published_page += 1;
                            publish(AuthoredRefreshEvent::Page {
                                host: host.host.clone(),
                                page: published_page,
                                pull_requests: result.pull_requests,
                                warnings: result.warnings,
                            });
                        }
                        ViewerSearchMessage::Failed {
                            search_kind,
                            page,
                            error,
                        } => {
                            if let GitHubError::RateLimited { reset_at } = &error {
                                self.suppress(&host.host, reset_at, None);
                            }
                            complete = false;
                            errors
                                .push(format!("{} {search_kind} page {page}: {error}", host.host));
                        }
                        ViewerSearchMessage::Incomplete(message) => {
                            complete = false;
                            warnings.push(message.clone());
                            errors.push(message);
                        }
                    }
                }

                for handle in handles {
                    if handle.join().is_err() {
                        complete = false;
                        errors.push(format!(
                            "{}: pull request search worker panicked",
                            host.host
                        ));
                    }
                }
            });
        }
        publish(AuthoredRefreshEvent::Finished {
            complete,
            warnings: deduplicate(warnings),
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        });
    }

    pub fn hydrate_pull_requests_with(
        &self,
        credentials: &dyn CredentialProvider,
        hosts: &[AuthoredHost],
        identities: impl IntoIterator<Item = CanonicalPullRequestId>,
    ) -> BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>> {
        let unique: BTreeSet<_> = identities.into_iter().collect();
        let mut hydrated = BTreeMap::new();
        let mut by_host = BTreeMap::<String, Vec<CanonicalPullRequestId>>::new();
        for identity in unique {
            by_host
                .entry(identity.repository.host.clone())
                .or_default()
                .push(identity);
        }
        for (host_name, identities) in by_host {
            let Some(host) = hosts.iter().find(|host| host.host == host_name) else {
                let error = GitHubError::Malformed(format!(
                    "no API endpoint is configured for {host_name}"
                ));
                for identity in identities {
                    hydrated.insert(identity, Err(error.clone()));
                }
                continue;
            };
            let token = match resolve_token(credentials, &host.host, &host.credential_anchor) {
                Ok(token) => token,
                Err(error) => {
                    for identity in identities {
                        hydrated.insert(identity, Err(error.clone()));
                    }
                    continue;
                }
            };
            if let Some(error) = self.suppressed_error(&host.host) {
                for identity in identities {
                    hydrated.insert(identity, Err(error.clone()));
                }
                continue;
            }
            for identity in identities {
                let result = self.fetch_pull_request_details(host, &token, &identity);
                if let Err(GitHubError::RateLimited { reset_at }) = &result {
                    self.suppress(&host.host, reset_at, None);
                }
                hydrated.insert(identity, result);
            }
        }
        hydrated
    }

    pub fn fetch_pull_request_with(
        &self,
        credentials: &dyn CredentialProvider,
        host: &AuthoredHost,
        identity: &CanonicalPullRequestId,
    ) -> Result<AuthoredPullRequest, GitHubError> {
        if identity.repository.host != host.host {
            return Err(GitHubError::Malformed(
                "pull request host does not match request host".to_owned(),
            ));
        }
        let token = resolve_token(credentials, &host.host, &host.credential_anchor)?;
        if let Some(error) = self.suppressed_error(&host.host) {
            return Err(error);
        }
        let mut variables = serde_json::Map::new();
        variables.insert(
            "owner".to_owned(),
            Value::String(identity.repository.owner.clone()),
        );
        variables.insert(
            "repository".to_owned(),
            Value::String(identity.repository.repository.clone()),
        );
        variables.insert("number".to_owned(), Value::from(identity.number));
        let body = GraphQlRequest {
            query: pull_request_query().to_owned(),
            variables,
        };
        let mut response = self
            .agent
            .post(&host.graphql_url)
            .header("Authorization", &format!("Bearer {}", token.expose()))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "wt")
            .send_json(&body)
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let header_rate = rate_from_headers(response.headers());
        let response_body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(classify_http_error(
                status,
                &response_body,
                header_rate.as_ref(),
            ));
        }
        let envelope: GraphQlEnvelope = serde_json::from_str(&response_body)
            .map_err(|error| GitHubError::Malformed(error.to_string()))?;
        let warnings = deduplicate(envelope.errors.iter().map(|error| error.message.clone()));
        let data = envelope
            .data
            .ok_or_else(|| classify_graphql_errors(&warnings))?;
        if let Some(rate) = parse_graphql_rate(&data).or(header_rate)
            && rate.remaining == 0
        {
            self.suppress(
                &host.host,
                &rate.reset_at,
                header_reset_epoch(response.headers()),
            );
        }
        let node = data.pointer("/repository/pullRequest").ok_or_else(|| {
            GitHubError::PullRequestUnavailable {
                repository: identity.repository.full_name(),
                number: identity.number,
            }
        })?;
        if node.is_null() {
            return Err(GitHubError::PullRequestUnavailable {
                repository: identity.repository.full_name(),
                number: identity.number,
            });
        }
        let author = node
            .pointer("/author/login")
            .and_then(Value::as_str)
            .ok_or_else(|| GitHubError::Malformed("pull request author is missing".to_owned()))?;
        let pull_request = normalize_pull_request(node)?;
        let refreshed_identity =
            canonical_pull_request_id(&host.host, &pull_request).ok_or_else(|| {
                GitHubError::Malformed("pull request base repository is missing".to_owned())
            })?;
        if &refreshed_identity != identity {
            return Err(GitHubError::Malformed(
                "pull request canonical identity changed".to_owned(),
            ));
        }
        Ok(AuthoredPullRequest {
            identity: refreshed_identity,
            author: author.to_owned(),
            pull_request,
        })
    }

    fn fetch_authored_page(
        &self,
        host: &AuthoredHost,
        token: &ResolvedToken,
        search_query: &str,
        cursor: Option<&str>,
    ) -> Result<AuthoredPage, GitHubError> {
        let mut variables = serde_json::Map::new();
        variables.insert("query".to_owned(), Value::String(search_query.to_owned()));
        variables.insert(
            "cursor".to_owned(),
            cursor.map_or(Value::Null, |cursor| Value::String(cursor.to_owned())),
        );
        let body = GraphQlRequest {
            query: authored_pull_request_query(),
            variables,
        };
        let mut response = self
            .agent
            .post(&host.graphql_url)
            .header("Authorization", &format!("Bearer {}", token.expose()))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "wt")
            .send_json(&body)
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let header_rate = rate_from_headers(response.headers());
        let response_body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(classify_http_error(
                status,
                &response_body,
                header_rate.as_ref(),
            ));
        }
        let envelope: GraphQlEnvelope = serde_json::from_str(&response_body)
            .map_err(|error| GitHubError::Malformed(error.to_string()))?;
        let response_warnings =
            deduplicate(envelope.errors.iter().map(|error| error.message.clone()));
        let data = envelope
            .data
            .ok_or_else(|| classify_graphql_errors(&response_warnings))?;
        let rate = parse_graphql_rate(&data).or(header_rate);
        if let Some(rate) = &rate
            && rate.remaining == 0
        {
            self.suppress(
                &host.host,
                &rate.reset_at,
                header_reset_epoch(response.headers()),
            );
        }
        parse_authored_page(&host.host, &data, response_warnings)
    }

    fn fetch_viewer_search(
        &self,
        host: &AuthoredHost,
        token: &ResolvedToken,
        search_kind: &'static str,
        search_query: &'static str,
        sender: std::sync::mpsc::Sender<ViewerSearchMessage>,
    ) {
        let mut cursor = None;
        for page in 1..=MAX_AUTHORED_PULL_REQUEST_PAGES {
            match self.fetch_authored_page(host, token, search_query, cursor.as_deref()) {
                Ok(result) => {
                    let has_next_page = result.has_next_page;
                    let end_cursor = result.end_cursor.clone();
                    if sender.send(ViewerSearchMessage::Page(result)).is_err() {
                        return;
                    }
                    if !has_next_page {
                        return;
                    }
                    if page == MAX_AUTHORED_PULL_REQUEST_PAGES {
                        let _ = sender.send(ViewerSearchMessage::Incomplete(format!(
                            "{}: {search_kind} pull request search was truncated at 1,000 results",
                            host.host,
                        )));
                        return;
                    }
                    let Some(next_cursor) = end_cursor else {
                        let _ = sender.send(ViewerSearchMessage::Incomplete(format!(
                            "{}: {search_kind} pull request page lacks a cursor",
                            host.host,
                        )));
                        return;
                    };
                    cursor = Some(next_cursor);
                }
                Err(error) => {
                    let _ = sender.send(ViewerSearchMessage::Failed {
                        search_kind,
                        page,
                        error,
                    });
                    return;
                }
            }
        }
    }

    fn fetch_pull_request_details(
        &self,
        host: &AuthoredHost,
        token: &ResolvedToken,
        identity: &CanonicalPullRequestId,
    ) -> Result<PullRequestDetails, GitHubError> {
        let mut details = PullRequestDetails::default();
        let mut cursor = None;
        let mut requiredness_complete = true;
        for page in 1..=MAX_CHECK_CONTEXT_PAGES {
            let response =
                self.fetch_pull_request_detail_page(host, token, identity, cursor.as_deref())?;
            let pull_request = response.data.pointer("/repository/pr0").ok_or_else(|| {
                GitHubError::PullRequestUnavailable {
                    repository: identity.repository.full_name(),
                    number: identity.number,
                }
            })?;
            if pull_request.is_null() {
                return Err(GitHubError::PullRequestUnavailable {
                    repository: identity.repository.full_name(),
                    number: identity.number,
                });
            }
            if page == 1 {
                parse_pull_request_attention(pull_request, &mut details);
                details.warnings.extend(response.warnings.clone());
            }
            let contexts =
                pull_request.pointer("/commits/nodes/0/commit/statusCheckRollup/contexts");
            let Some(contexts) = contexts.filter(|contexts| !contexts.is_null()) else {
                details.check_contexts_complete = false;
                details
                    .warnings
                    .push("check contexts are missing or still computing".to_owned());
                break;
            };
            requiredness_complete &= parse_check_contexts(contexts, &mut details.checks);
            let has_next = contexts
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool);
            match has_next {
                Some(false) => {
                    details.check_contexts_complete = requiredness_complete;
                    if !requiredness_complete {
                        details.warnings.push(
                            "one or more check contexts lack required-check metadata".to_owned(),
                        );
                    }
                    break;
                }
                Some(true) if page == MAX_CHECK_CONTEXT_PAGES => {
                    details.check_contexts_complete = false;
                    details.warnings.push(format!(
                        "check contexts were truncated after {} entries",
                        CHECK_CONTEXTS_PER_PAGE * MAX_CHECK_CONTEXT_PAGES
                    ));
                    break;
                }
                Some(true) => {
                    let Some(end_cursor) = contexts
                        .pointer("/pageInfo/endCursor")
                        .and_then(Value::as_str)
                    else {
                        details.check_contexts_complete = false;
                        details
                            .warnings
                            .push("check contexts page lacks a cursor".to_owned());
                        break;
                    };
                    cursor = Some(end_cursor.to_owned());
                }
                None => {
                    details.check_contexts_complete = false;
                    details
                        .warnings
                        .push("check contexts pageInfo is missing".to_owned());
                    break;
                }
            }
        }
        details.normalize_checks();
        details.fold_latest_reviews();
        details.warnings = deduplicate(details.warnings);
        Ok(details)
    }

    fn fetch_pull_request_detail_page(
        &self,
        host: &AuthoredHost,
        token: &ResolvedToken,
        identity: &CanonicalPullRequestId,
        contexts_cursor: Option<&str>,
    ) -> Result<DetailPage, GitHubError> {
        let mut variables = serde_json::Map::new();
        variables.insert(
            "owner".to_owned(),
            Value::String(identity.repository.owner.clone()),
        );
        variables.insert(
            "repository".to_owned(),
            Value::String(identity.repository.repository.clone()),
        );
        variables.insert("number".to_owned(), Value::from(identity.number));
        variables.insert(
            "contextsCursor".to_owned(),
            contexts_cursor.map_or(Value::Null, |cursor| Value::String(cursor.to_owned())),
        );
        let body = GraphQlRequest {
            query: pull_request_detail_query(identity.number),
            variables,
        };
        let mut response = self
            .agent
            .post(&host.graphql_url)
            .header("Authorization", &format!("Bearer {}", token.expose()))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "wt")
            .send_json(&body)
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let header_rate = rate_from_headers(response.headers());
        let response_body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(classify_http_error(
                status,
                &response_body,
                header_rate.as_ref(),
            ));
        }
        let envelope: GraphQlEnvelope = serde_json::from_str(&response_body)
            .map_err(|error| GitHubError::Malformed(error.to_string()))?;
        let warnings = deduplicate(envelope.errors.iter().map(|error| error.message.clone()));
        let data = envelope
            .data
            .ok_or_else(|| classify_graphql_errors(&warnings))?;
        if let Some(rate) = parse_graphql_rate(&data).or(header_rate)
            && rate.remaining == 0
        {
            self.suppress(
                &host.host,
                &rate.reset_at,
                header_reset_epoch(response.headers()),
            );
        }
        Ok(DetailPage { data, warnings })
    }

    fn fetch_batch(
        &self,
        remote: &RemoteRepository,
        token: &ResolvedToken,
        targets: &[BranchTarget],
    ) -> Result<BranchBatchRefresh, GitHubError> {
        let (query, variables) = build_query(remote, targets);
        let body = GraphQlRequest { query, variables };
        let mut response = self
            .agent
            .post(&remote.graphql_url())
            .header("Authorization", &format!("Bearer {}", token.expose()))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "wt")
            .send_json(&body)
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let header_rate = rate_from_headers(response.headers());
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| GitHubError::Network(error.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(classify_http_error(status, &body, header_rate.as_ref()));
        }
        let envelope: GraphQlEnvelope = serde_json::from_str(&body)
            .map_err(|error| GitHubError::Malformed(error.to_string()))?;
        let warnings = deduplicate(envelope.errors.iter().map(|error| error.message.clone()));
        let Some(data) = envelope.data else {
            return Err(classify_graphql_errors(&warnings));
        };
        let rate = parse_graphql_rate(&data).or(header_rate);
        if let Some(rate) = &rate
            && rate.remaining == 0
        {
            self.suppress(
                &remote.host,
                &rate.reset_at,
                header_reset_epoch(response.headers()),
            );
        }
        let (outcomes, associations) = parse_batch_data(
            &data,
            targets,
            &envelope.errors,
            &warnings,
            rate,
            &remote.host,
        )?;
        let active_pull_requests = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().ok())
            .filter_map(|data| data.pull_request.as_ref())
            .filter_map(|pull_request| canonical_pull_request_id(&remote.host, pull_request))
            .collect();
        Ok(BranchBatchRefresh {
            branches: outcomes,
            associations,
            active_pull_requests,
        })
    }

    fn suppressed_error(&self, host: &str) -> Option<GitHubError> {
        let now = epoch_seconds();
        let mut suppressions = self.suppressed_until.lock().expect("rate gate poisoned");
        let suppression = suppressions.get(host)?;
        if now < suppression.epoch_seconds {
            Some(GitHubError::RateLimited {
                reset_at: suppression.reset_at.clone(),
            })
        } else {
            suppressions.remove(host);
            None
        }
    }

    fn suppress(&self, host: &str, reset_at: &str, epoch: Option<u64>) {
        let epoch_seconds = epoch
            .or_else(|| reset_epoch(reset_at))
            .unwrap_or_else(|| epoch_seconds().saturating_add(60));
        self.suppressed_until
            .lock()
            .expect("rate gate poisoned")
            .insert(
                host.to_owned(),
                Suppression {
                    epoch_seconds,
                    reset_at: reset_at.to_owned(),
                },
            );
    }
}

struct AuthoredPage {
    pull_requests: Vec<AuthoredPullRequest>,
    has_next_page: bool,
    end_cursor: Option<String>,
    warnings: Vec<String>,
}

enum ViewerSearchMessage {
    Page(AuthoredPage),
    Failed {
        search_kind: &'static str,
        page: usize,
        error: GitHubError,
    },
    Incomplete(String),
}

struct DetailPage {
    data: Value,
    warnings: Vec<String>,
}

fn pull_request_detail_query(number: u64) -> String {
    format!(
        r#"query($owner: String!, $repository: String!, $number: Int!, $contextsCursor: String) {{
      repository(owner: $owner, name: $repository) {{
        pr0: pullRequest(number: $number) {{
          mergeable mergeStateStatus
          reviewRequests(first: 100) {{
            nodes {{ requestedReviewer {{
              __typename
              ... on User {{ id login }}
              ... on Team {{ id slug name }}
            }} }}
            pageInfo {{ hasNextPage }}
          }}
          reviews(first: 100) {{
            nodes {{ id databaseId author {{ login }} body state submittedAt url }}
            pageInfo {{ hasNextPage }}
          }}
          reviewThreads(first: 100) {{
            nodes {{
              id isResolved isOutdated path
              comments(last: 100) {{
                nodes {{ id databaseId author {{ login }} body url }}
                pageInfo {{ hasPreviousPage }}
              }}
            }}
            pageInfo {{ hasNextPage }}
          }}
          commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{
            contexts(first: {CHECK_CONTEXTS_PER_PAGE}, after: $contextsCursor) {{
              nodes {{
                __typename
                ... on CheckRun {{
                  name status conclusion detailsUrl startedAt completedAt
                  isRequired(pullRequestNumber: {number})
                }}
                ... on StatusContext {{
                  context state targetUrl createdAt
                  isRequired(pullRequestNumber: {number})
                }}
              }}
              pageInfo {{ hasNextPage endCursor }}
            }}
          }} }} }} }}
        }}
      }}
      rateLimit {{ remaining resetAt }}
    }}"#
    )
}

fn parse_pull_request_attention(node: &Value, details: &mut PullRequestDetails) {
    details.merge_conflict = match node.get("mergeable").and_then(Value::as_str) {
        Some("MERGEABLE") => MergeConflictState::Clean,
        Some("CONFLICTING") => MergeConflictState::Conflicting,
        _ => MergeConflictState::Unknown,
    };

    details.review_requests = node
        .pointer("/reviewRequests/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|request| parse_review_request(request.get("requestedReviewer")?))
        .collect();
    details
        .review_requests
        .sort_by(|left, right| (&left.name, &left.id).cmp(&(&right.name, &right.id)));

    let reviews = node.pointer("/reviews/nodes").and_then(Value::as_array);
    for review in reviews.into_iter().flatten() {
        if let Some(parsed) = parse_reviewer_review(review) {
            details.reviewer_reviews.push(parsed);
        }
        if let Some(summary) = parse_review_summary(review) {
            details.feedback.push(summary);
        }
    }
    details.reviews_complete =
        connection_complete(node.get("reviewRequests")) && connection_complete(node.get("reviews"));
    if !details.reviews_complete {
        details
            .warnings
            .push("review requests or reviews are incomplete".to_owned());
    }

    if let Some(threads) = node
        .pointer("/reviewThreads/nodes")
        .and_then(Value::as_array)
    {
        for thread in threads
            .iter()
            .filter(|thread| thread.get("isResolved").and_then(Value::as_bool) == Some(false))
        {
            let thread_id = thread.get("id").and_then(Value::as_str).map(str::to_owned);
            let path = thread
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let outdated = thread
                .get("isOutdated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(comments) = thread.pointer("/comments/nodes").and_then(Value::as_array) {
                for comment in comments {
                    let body = comment
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim();
                    let Some(id) = comment.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    details.feedback.push(PullRequestFeedback {
                        id: id.to_owned(),
                        database_id: comment.get("databaseId").and_then(Value::as_u64),
                        thread_id: thread_id.clone(),
                        kind: FeedbackKind::InlineThread,
                        author: actor_name(comment.get("author")),
                        body: body.to_owned(),
                        path: path.clone(),
                        permalink: comment
                            .get("url")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        outdated,
                    });
                }
            }
        }
    }
    details.feedback_complete = connection_complete(node.get("reviewThreads"))
        && node
            .pointer("/reviewThreads/nodes")
            .and_then(Value::as_array)
            .is_some_and(|threads| {
                threads.iter().all(|thread| {
                    thread
                        .pointer("/comments/pageInfo/hasPreviousPage")
                        .and_then(Value::as_bool)
                        == Some(false)
                })
            });
    if !details.feedback_complete {
        details
            .warnings
            .push("review feedback is incomplete".to_owned());
    }
}

fn connection_complete(connection: Option<&Value>) -> bool {
    connection
        .and_then(|connection| connection.pointer("/pageInfo/hasNextPage"))
        .and_then(Value::as_bool)
        == Some(false)
}

fn parse_review_request(node: &Value) -> Option<ReviewRequest> {
    let id = node.get("id")?.as_str()?.to_owned();
    let kind = match node.get("__typename").and_then(Value::as_str) {
        Some("User") => ReviewerKind::User,
        Some("Team") => ReviewerKind::Team,
        _ => ReviewerKind::Unknown,
    };
    let name = node
        .get("login")
        .or_else(|| node.get("slug"))
        .or_else(|| node.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown reviewer")
        .to_owned();
    Some(ReviewRequest { id, name, kind })
}

fn parse_reviewer_review(node: &Value) -> Option<ReviewerReview> {
    Some(ReviewerReview {
        id: node.get("id")?.as_str()?.to_owned(),
        database_id: node.get("databaseId").and_then(Value::as_u64),
        reviewer: actor_name(node.get("author")),
        state: submitted_review_state(node.get("state").and_then(Value::as_str)),
        submitted_at: node
            .get("submittedAt")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn parse_review_summary(node: &Value) -> Option<PullRequestFeedback> {
    let body = node.get("body").and_then(Value::as_str)?.trim();
    if body.is_empty() {
        return None;
    }
    Some(PullRequestFeedback {
        id: node.get("id")?.as_str()?.to_owned(),
        database_id: node.get("databaseId").and_then(Value::as_u64),
        thread_id: None,
        kind: FeedbackKind::ReviewSummary,
        author: actor_name(node.get("author")),
        body: body.to_owned(),
        path: None,
        permalink: node.get("url").and_then(Value::as_str).map(str::to_owned),
        outdated: false,
    })
}

fn actor_name(actor: Option<&Value>) -> String {
    actor
        .and_then(|actor| actor.get("login"))
        .and_then(Value::as_str)
        .unwrap_or("deleted user")
        .to_owned()
}

fn submitted_review_state(state: Option<&str>) -> SubmittedReviewState {
    match state {
        Some("APPROVED") => SubmittedReviewState::Approved,
        Some("CHANGES_REQUESTED") => SubmittedReviewState::ChangesRequested,
        Some("COMMENTED") => SubmittedReviewState::Commented,
        Some("DISMISSED") => SubmittedReviewState::Dismissed,
        Some("PENDING") => SubmittedReviewState::Pending,
        _ => SubmittedReviewState::Unknown,
    }
}

fn parse_check_contexts(connection: &Value, checks: &mut Vec<PullRequestCheck>) -> bool {
    let source_offset = checks.len();
    let mut complete = connection.get("nodes").and_then(Value::as_array).is_some();
    for (index, node) in connection
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let typename = node.get("__typename").and_then(Value::as_str);
        complete &= node.get("isRequired").and_then(Value::as_bool).is_some();
        let parsed = match typename {
            Some("CheckRun") => {
                node.get("name")
                    .and_then(Value::as_str)
                    .map(|name| PullRequestCheck {
                        name: name.to_owned(),
                        state: check_run_state(
                            node.get("status").and_then(Value::as_str),
                            node.get("conclusion").and_then(Value::as_str),
                        ),
                        target_url: node
                            .get("detailsUrl")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        required: node
                            .get("isRequired")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        source_order: source_offset + index,
                        completed_at: node
                            .get("completedAt")
                            .and_then(Value::as_str)
                            .or_else(|| node.get("startedAt").and_then(Value::as_str))
                            .map(str::to_owned),
                    })
            }
            Some("StatusContext") => {
                node.get("context")
                    .and_then(Value::as_str)
                    .map(|name| PullRequestCheck {
                        name: name.to_owned(),
                        state: status_context_state(node.get("state").and_then(Value::as_str)),
                        target_url: node
                            .get("targetUrl")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        required: node
                            .get("isRequired")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        source_order: source_offset + index,
                        completed_at: node
                            .get("createdAt")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
            }
            _ => None,
        };
        if let Some(check) = parsed {
            checks.push(check);
        } else {
            complete = false;
        }
    }
    complete
}

fn check_run_state(status: Option<&str>, conclusion: Option<&str>) -> CheckState {
    if status != Some("COMPLETED") {
        return match status {
            Some("QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" | "PENDING") => {
                CheckState::Pending
            }
            _ => CheckState::Unknown,
        };
    }
    match conclusion {
        Some("SUCCESS") => CheckState::Success,
        Some("FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE") => CheckState::Failure,
        Some("ACTION_REQUIRED" | "STALE") => CheckState::Error,
        Some("NEUTRAL") => CheckState::Neutral,
        Some("SKIPPED") => CheckState::Skipped,
        _ => CheckState::Unknown,
    }
}

fn status_context_state(state: Option<&str>) -> CheckState {
    match state {
        Some("SUCCESS") => CheckState::Success,
        Some("FAILURE") => CheckState::Failure,
        Some("ERROR") => CheckState::Error,
        Some("PENDING") => CheckState::Pending,
        Some("EXPECTED") => CheckState::Expected,
        _ => CheckState::Unknown,
    }
}

fn authored_pull_request_query() -> String {
    format!(
        r#"query($query: String!, $cursor: String) {{
      viewer {{ login }}
      search(query: $query, type: ISSUE, first: {AUTHORED_PULL_REQUESTS_PER_PAGE}, after: $cursor) {{
        issueCount
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          ... on PullRequest {{
            number title url state isDraft mergedAt updatedAt reviewDecision
            autoMergeRequest {{ enabledAt }}
            author {{ login }}
            assignees(first: 10) {{ nodes {{ login }} }}
            baseRefName baseRefOid baseRepository {{ nameWithOwner }}
            headRefName headRefOid headRepository {{ nameWithOwner }}
            commits(last: 1) {{ nodes {{ commit {{ oid statusCheckRollup {{ state }} }} }} }}
          }}
        }}
      }}
      rateLimit {{ remaining resetAt }}
    }}"#
    )
}

fn pull_request_query() -> &'static str {
    r#"query($owner: String!, $repository: String!, $number: Int!) {
      repository(owner: $owner, name: $repository) {
        pullRequest(number: $number) {
          number title url state isDraft mergedAt updatedAt reviewDecision
          autoMergeRequest { enabledAt }
          author { login }
          baseRefName baseRefOid baseRepository { nameWithOwner }
          headRefName headRefOid headRepository { nameWithOwner }
          commits(last: 1) { nodes { commit { oid statusCheckRollup { state } } } }
        }
      }
      rateLimit { remaining resetAt }
    }"#
}

fn parse_authored_page(
    host: &str,
    data: &Value,
    mut warnings: Vec<String>,
) -> Result<AuthoredPage, GitHubError> {
    let viewer = data
        .pointer("/viewer/login")
        .and_then(Value::as_str)
        .ok_or_else(|| GitHubError::Malformed("viewer login is missing".to_owned()))?;
    let search = data
        .get("search")
        .ok_or_else(|| GitHubError::Malformed("authored search data is missing".to_owned()))?;
    let nodes = search
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| GitHubError::Malformed("authored search nodes are missing".to_owned()))?;
    let mut pull_requests = Vec::new();
    for node in nodes {
        let author = node
            .pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let assigned_to_viewer = node
            .pointer("/assignees/nodes")
            .and_then(Value::as_array)
            .is_some_and(|assignees| {
                assignees.iter().any(|assignee| {
                    assignee
                        .get("login")
                        .and_then(Value::as_str)
                        .is_some_and(|login| login.eq_ignore_ascii_case(viewer))
                })
            });
        if !author.eq_ignore_ascii_case(viewer) && !assigned_to_viewer {
            warnings.push(format!(
                "{host}: ignored search result neither authored by nor assigned to viewer {viewer}"
            ));
            continue;
        }
        match normalize_pull_request(node) {
            Ok(pull_request) => {
                let Some(identity) = canonical_pull_request_id(host, &pull_request) else {
                    warnings.push(format!(
                        "{host}: ignored pull request {} without a base repository",
                        pull_request.number
                    ));
                    continue;
                };
                pull_requests.push(AuthoredPullRequest {
                    identity,
                    author: author.to_owned(),
                    pull_request,
                });
            }
            Err(error) => warnings.push(error.to_string()),
        }
    }
    let has_next_page = search
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .ok_or_else(|| GitHubError::Malformed("authored pageInfo is missing".to_owned()))?;
    let end_cursor = search
        .pointer("/pageInfo/endCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(AuthoredPage {
        pull_requests,
        has_next_page,
        end_cursor,
        warnings: deduplicate(warnings),
    })
}

impl Default for GitHubService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
struct GraphQlRequest {
    query: String,
    variables: serde_json::Map<String, Value>,
}

#[derive(serde::Deserialize)]
struct GraphQlEnvelope {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<GraphQlResponseError>,
}

#[derive(serde::Deserialize)]
struct GraphQlResponseError {
    message: String,
    #[serde(default)]
    path: Vec<Value>,
}

pub fn parse_remote_url(url: &str) -> Result<RemoteRepository, GitHubError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(GitHubError::UnsupportedRemote);
    }
    if !trimmed.contains("://")
        && let Some((authority, path)) = trimmed.split_once(':')
        && authority.contains('@')
    {
        let host = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        return remote_from_parts(host, path, WebScheme::Https);
    }

    let (scheme, remainder) = trimmed
        .split_once("://")
        .ok_or(GitHubError::UnsupportedRemote)?;
    let normalized_scheme = scheme.to_ascii_lowercase();
    let web_scheme = match normalized_scheme.as_str() {
        "http" => WebScheme::Http,
        "https" => WebScheme::Https,
        "ssh" | "git" => WebScheme::Https,
        _ => return Err(GitHubError::UnsupportedRemote),
    };
    let (authority, path) = remainder
        .split_once('/')
        .ok_or(GitHubError::UnsupportedRemote)?;
    let authority = authority
        .rsplit_once('@')
        .map(|(_, authority)| authority)
        .unwrap_or(authority);
    let host = if matches!(normalized_scheme.as_str(), "ssh" | "git") {
        authority.split(':').next().unwrap_or(authority)
    } else {
        authority
    };
    remote_from_parts(host, path, web_scheme)
}

fn remote_from_parts(
    host: &str,
    path: &str,
    scheme: WebScheme,
) -> Result<RemoteRepository, GitHubError> {
    let clean_path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_matches('/');
    let path = clean_path.strip_suffix(".git").unwrap_or(clean_path);
    let mut parts = path.split('/');
    let owner = parts.next().filter(|part| !part.is_empty());
    let name = parts.next().filter(|part| !part.is_empty());
    if owner.is_none() || name.is_none() || parts.next().is_some() || host.is_empty() {
        return Err(GitHubError::UnsupportedRemote);
    }
    Ok(RemoteRepository {
        host: host.to_ascii_lowercase(),
        owner: owner.unwrap().to_owned(),
        name: name.unwrap().to_owned(),
        scheme,
    })
}

pub fn resolve_token(
    credentials: &dyn CredentialProvider,
    host: &str,
    anchor: &Path,
) -> Result<ResolvedToken, GitHubError> {
    let environment_keys: &[&str] = if host == "github.com" {
        &["GITHUB_TOKEN", "GH_TOKEN"]
    } else {
        &["GITHUB_ENTERPRISE_TOKEN", "GH_ENTERPRISE_TOKEN"]
    };
    for key in environment_keys {
        if let Some(token) = credentials
            .environment(key)
            .filter(|token| !token.trim().is_empty())
        {
            return Ok(ResolvedToken {
                token: SecretToken(token),
                source: AuthSource::Environment,
            });
        }
    }
    let normalized_host: String = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    for key in [
        format!("github.{normalized_host}.token"),
        "github.token".to_owned(),
    ] {
        if let Some(token) = credentials
            .repository_git_config(anchor, &key)
            .filter(|token| !token.trim().is_empty())
        {
            return Ok(ResolvedToken {
                token: SecretToken(token),
                source: AuthSource::RepositoryGitConfig,
            });
        }
    }
    if let Some(token) = credentials
        .gh_token(host)
        .filter(|token| !token.trim().is_empty())
    {
        return Ok(ResolvedToken {
            token: SecretToken(token),
            source: AuthSource::GhCli,
        });
    }
    Err(GitHubError::NoToken {
        host: host.to_owned(),
    })
}

fn resolve_branch_remote(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
    branch: &str,
) -> Result<RemoteRepository, GitHubError> {
    let upstream_remote = optional_git_value(
        runner,
        &repository.path,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )?;
    let remote_name = upstream_remote
        .filter(|remote| remote != ".")
        .or_else(|| repository.github_remote.clone())
        .unwrap_or_else(|| "origin".to_owned());
    let url = optional_git_value(
        runner,
        &repository.path,
        &["remote", "get-url", &remote_name],
    )?
    .ok_or(GitHubError::UnsupportedRemote)?;
    parse_remote_url(&url)
}

pub fn remote_trunk_branch(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<Option<String>, GitHubError> {
    let remotes = required_git_value(runner, &repository.path, &["remote"])?
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(remote) = [
        repository.github_remote.as_ref(),
        repository.github_preferred_remote.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|remote| remotes.contains(remote))
    .cloned()
    .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
    .or_else(|| remotes.first().cloned()) else {
        return Ok(None);
    };
    let symbolic = format!("refs/remotes/{remote}/HEAD");
    if let Some(head) = optional_git_value(
        runner,
        &repository.path,
        &["symbolic-ref", "--quiet", "--short", &symbolic],
    )? && let Some(branch) = head.trim().strip_prefix(&format!("{remote}/"))
        && !branch.is_empty()
    {
        return Ok(Some(branch.to_owned()));
    }
    for branch in ["main", "master"] {
        let reference = format!("refs/remotes/{remote}/{branch}^{{commit}}");
        if optional_git_value(
            runner,
            &repository.path,
            &["rev-parse", "--verify", "--quiet", &reference],
        )?
        .is_some()
        {
            return Ok(Some(branch.to_owned()));
        }
    }
    Ok(None)
}

fn optional_git_value(
    runner: &dyn GitRunner,
    anchor: &Path,
    arguments: &[&str],
) -> Result<Option<String>, GitHubError> {
    let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let output = runner
        .run(anchor, &arguments)
        .map_err(|error| GitHubError::LocalGit(error.to_string()))?;
    if !output.success {
        return Ok(None);
    }
    Ok(nonempty_lossy(&output.stdout))
}

fn required_git_value(
    runner: &dyn GitRunner,
    anchor: &Path,
    arguments: &[&str],
) -> Result<String, GitHubError> {
    let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let output = runner
        .run(anchor, &arguments)
        .map_err(|error| GitHubError::LocalGit(error.to_string()))?;
    if !output.success {
        return Err(GitHubError::LocalGit(
            nonempty_lossy(&output.stderr).unwrap_or_else(|| "Git command failed".to_owned()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn nonempty_lossy(bytes: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(bytes).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn build_query(
    remote: &RemoteRepository,
    targets: &[BranchTarget],
) -> (String, serde_json::Map<String, Value>) {
    let mut declarations = vec!["$owner: String!".to_owned(), "$name: String!".to_owned()];
    let mut aliases = String::new();
    let mut variables = serde_json::Map::new();
    variables.insert("owner".to_owned(), Value::String(remote.owner.clone()));
    variables.insert("name".to_owned(), Value::String(remote.name.clone()));
    for (index, target) in targets.iter().enumerate() {
        declarations.push(format!("$branch{index}: String!"));
        variables.insert(
            format!("branch{index}"),
            Value::String(
                target
                    .head
                    .clone()
                    .unwrap_or_else(|| format!("refs/heads/{}", target.branch)),
            ),
        );
        aliases.push_str(&format!(
            r#"
            branch{index}: object(expression: $branch{index}) {{
              ... on Commit {{
                associatedPullRequests(first: 20) {{
                  nodes {{
                    number title url state isDraft mergedAt updatedAt reviewDecision
                    autoMergeRequest {{ enabledAt }}
                    baseRefName baseRefOid baseRepository {{ nameWithOwner }}
                    headRefName headRefOid headRepository {{ nameWithOwner }}
                    commits(last: 1) {{ nodes {{ commit {{ oid statusCheckRollup {{ state }} }} }} }}
                  }}
                }}
              }}
            }}
            "#
        ));
    }
    let query = format!(
        r#"query({}) {{
          repository(owner: $owner, name: $name) {{
            {aliases}
          }}
          rateLimit {{ remaining resetAt }}
        }}"#,
        declarations.join(", ")
    );
    (query, variables)
}

fn parse_batch_data(
    data: &Value,
    targets: &[BranchTarget],
    errors: &[GraphQlResponseError],
    warnings: &[String],
    rate: Option<RateLimit>,
    host: &str,
) -> Result<ParsedBatchData, GitHubError> {
    let repository = match data.get("repository").and_then(Value::as_object) {
        Some(repository) => repository,
        None if !warnings.is_empty() => return Err(classify_graphql_errors(warnings)),
        None => return Err(GitHubError::Malformed("missing repository data".to_owned())),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut associations = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let alias = format!("branch{index}");
        let alias_errors: Vec<String> = errors
            .iter()
            .filter(|error| error.path.iter().any(|part| part.as_str() == Some(&alias)))
            .map(|error| error.message.clone())
            .collect();
        let Some(reference) = repository.get(&alias) else {
            let error = classify_graphql_errors(if alias_errors.is_empty() {
                warnings
            } else {
                &alias_errors
            });
            outcomes.push(Err(error.clone()));
            associations.push(Err(error));
            continue;
        };
        if reference.is_null() {
            let error = if alias_errors.is_empty() {
                GitHubError::BranchNotFound(target.branch.clone())
            } else {
                classify_graphql_errors(&alias_errors)
            };
            outcomes.push(Err(error.clone()));
            associations.push(Err(error));
            continue;
        }
        let nodes = reference
            .pointer("/associatedPullRequests/nodes")
            .and_then(Value::as_array);
        let Some(nodes) = nodes else {
            let error = if alias_errors.is_empty() {
                GitHubError::Malformed(format!(
                    "missing associated pull requests for {}",
                    target.branch
                ))
            } else {
                classify_graphql_errors(&alias_errors)
            };
            outcomes.push(Err(error.clone()));
            associations.push(Err(error));
            continue;
        };
        let mut pull_requests = Vec::new();
        let mut malformed = Vec::new();
        for node in nodes {
            match normalize_pull_request(node) {
                Ok(pull_request) => pull_requests.push(pull_request),
                Err(error) => malformed.push(error.to_string()),
            }
        }
        if pull_requests.is_empty() && !malformed.is_empty() {
            let error = GitHubError::Malformed(malformed.join("; "));
            outcomes.push(Err(error.clone()));
            associations.push(Err(error));
            continue;
        }
        let strict_associations = if !warnings.is_empty() || !malformed.is_empty() {
            let mut errors = warnings.to_vec();
            errors.extend(malformed.clone());
            Err(GitHubError::Malformed(errors.join("; ")))
        } else {
            pull_requests
                .iter()
                .map(|pull_request| {
                    canonical_pull_request_id(host, pull_request)
                        .map(|identity| AssociatedPullRequest {
                            identity,
                            pull_request: pull_request.clone(),
                        })
                        .ok_or_else(|| {
                            GitHubError::Malformed(
                                "associated pull request base repository is missing".to_owned(),
                            )
                        })
                })
                .collect()
        };
        let pull_request = prefer_pull_request(pull_requests, target);
        let mut branch_warnings = warnings.to_vec();
        branch_warnings.extend(malformed);
        outcomes.push(Ok(GitHubBranchData {
            pull_request,
            warnings: deduplicate(branch_warnings),
            rate_limit: rate.clone(),
        }));
        associations.push(strict_associations);
    }
    Ok((outcomes, associations))
}

fn normalize_pull_request(node: &Value) -> Result<PullRequest, GitHubError> {
    let number = node
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| GitHubError::Malformed("pull request number is missing".to_owned()))?;
    let required = |field: &'static str| {
        node.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| GitHubError::Malformed(format!("pull request {field} is missing")))
    };
    let raw_state = required("state")?;
    let is_draft = node
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let state =
        if node.get("mergedAt").is_some_and(|value| !value.is_null()) || raw_state == "MERGED" {
            PullRequestState::Merged
        } else if raw_state == "OPEN" && is_draft {
            PullRequestState::Draft
        } else if raw_state == "OPEN" {
            PullRequestState::Open
        } else {
            PullRequestState::Closed
        };
    let identity = |prefix: &str| -> Result<PullRequestIdentity, GitHubError> {
        let branch_field = format!("{prefix}RefName");
        let oid_field = format!("{prefix}RefOid");
        let repository_field = format!("{prefix}Repository");
        let branch = node
            .get(&branch_field)
            .and_then(Value::as_str)
            .ok_or_else(|| GitHubError::Malformed(format!("{branch_field} is missing")))?;
        Ok(PullRequestIdentity {
            repository: node
                .get(&repository_field)
                .and_then(|repository| repository.get("nameWithOwner"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            branch: branch.to_owned(),
            oid: node
                .get(&oid_field)
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    };
    let checks = node
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(Value::as_str)
        .map(check_rollup)
        .unwrap_or(CheckRollup::Unknown);
    Ok(PullRequest {
        number,
        title: required("title")?,
        url: required("url")?,
        state,
        updated_at: required("updatedAt")?,
        review_decision: node
            .get("reviewDecision")
            .and_then(Value::as_str)
            .map(str::to_owned),
        auto_merge: node
            .get("autoMergeRequest")
            .is_some_and(|request| !request.is_null()),
        base: identity("base")?,
        head: identity("head")?,
        checks,
    })
}

fn check_rollup(state: &str) -> CheckRollup {
    match state {
        "SUCCESS" => CheckRollup::Success,
        "FAILURE" => CheckRollup::Failure,
        "PENDING" => CheckRollup::Pending,
        "EXPECTED" => CheckRollup::Expected,
        "ERROR" => CheckRollup::Error,
        _ => CheckRollup::Unknown,
    }
}

fn prefer_pull_request(
    mut pull_requests: Vec<PullRequest>,
    target: &BranchTarget,
) -> Option<PullRequest> {
    pull_requests.sort_by(|left, right| {
        let match_priority = |pull_request: &PullRequest| {
            if pull_request.head.branch == target.branch {
                0
            } else if target
                .head
                .as_deref()
                .is_some_and(|head| pull_request.head.oid.as_deref() == Some(head))
            {
                1
            } else {
                2
            }
        };
        let state_priority = |pull_request: &PullRequest| {
            if matches!(
                pull_request.state,
                PullRequestState::Open | PullRequestState::Draft
            ) {
                0
            } else {
                1
            }
        };
        match_priority(left)
            .cmp(&match_priority(right))
            .then_with(|| state_priority(left).cmp(&state_priority(right)))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.base.repository.cmp(&right.base.repository))
            .then_with(|| left.number.cmp(&right.number))
            .then_with(|| left.url.cmp(&right.url))
    });
    pull_requests.into_iter().next()
}

fn parse_graphql_rate(data: &Value) -> Option<RateLimit> {
    let rate = data.get("rateLimit")?;
    Some(RateLimit {
        remaining: rate.get("remaining")?.as_u64()?,
        reset_at: rate.get("resetAt")?.as_str()?.to_owned(),
    })
}

fn rate_from_headers(headers: &ureq::http::HeaderMap) -> Option<RateLimit> {
    let remaining = headers
        .get("x-ratelimit-remaining")?
        .to_str()
        .ok()?
        .parse()
        .ok()?;
    let reset_at = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();
    Some(RateLimit {
        remaining,
        reset_at,
    })
}

fn header_reset_epoch(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("x-ratelimit-reset")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn classify_http_error(status: u16, body: &str, rate: Option<&RateLimit>) -> GitHubError {
    let message = api_message(body, status);
    let lower = message.to_ascii_lowercase();
    match status {
        401 => GitHubError::Unauthorized,
        403 if lower.contains("saml") || lower.contains("sso") => GitHubError::Sso(message),
        403 if lower.contains("personal access token (classic)")
            || lower.contains("classic pat") =>
        {
            GitHubError::ClassicPat(message)
        }
        403 | 429
            if rate.is_some_and(|rate| rate.remaining == 0) || lower.contains("rate limit") =>
        {
            GitHubError::RateLimited {
                reset_at: rate
                    .map(|rate| rate.reset_at.clone())
                    .unwrap_or_else(|| "unknown".to_owned()),
            }
        }
        403 => GitHubError::Permission(message),
        _ => GitHubError::Api { status, message },
    }
}

fn classify_graphql_errors(errors: &[String]) -> GitHubError {
    let message = if errors.is_empty() {
        "GraphQL response contained no data".to_owned()
    } else {
        errors.join("; ")
    };
    let lower = message.to_ascii_lowercase();
    if lower.contains("bad credentials") || lower.contains("authentication") {
        GitHubError::Unauthorized
    } else if lower.contains("saml") || lower.contains("sso") {
        GitHubError::Sso(message)
    } else if lower.contains("personal access token (classic)") || lower.contains("classic pat") {
        GitHubError::ClassicPat(message)
    } else if lower.contains("rate limit") {
        GitHubError::RateLimited {
            reset_at: "unknown".to_owned(),
        }
    } else if lower.contains("permission") || lower.contains("forbidden") {
        GitHubError::Permission(message)
    } else {
        GitHubError::Api {
            status: 200,
            message,
        }
    }
}

fn api_message(body: &str, status: u16) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| {
            let body = body.trim();
            if body.is_empty() {
                format!("HTTP {status}")
            } else {
                body.to_owned()
            }
        })
}

fn deduplicate(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn apply_group_error(refresh: &mut GitHubRefresh, targets: &[BranchTarget], error: GitHubError) {
    for target in targets {
        refresh
            .branches
            .insert(target.worktree.clone(), Err(error.clone()));
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn reset_epoch(reset_at: &str) -> Option<u64> {
    reset_at.parse().ok().or_else(|| {
        time::OffsetDateTime::parse(reset_at, &time::format_description::well_known::Rfc3339)
            .ok()
            .and_then(|reset| u64::try_from(reset.unix_timestamp()).ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{CommandOutput, GitError};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::thread::JoinHandle;

    #[derive(Default)]
    struct FakeCredentials {
        environment: HashMap<String, String>,
        git_config: HashMap<String, String>,
        gh: HashMap<String, String>,
    }

    impl CredentialProvider for FakeCredentials {
        fn environment(&self, key: &str) -> Option<String> {
            self.environment.get(key).cloned()
        }

        fn repository_git_config(&self, _anchor: &Path, key: &str) -> Option<String> {
            self.git_config.get(key).cloned()
        }

        fn gh_token(&self, host: &str) -> Option<String> {
            self.gh.get(host).cloned()
        }
    }

    struct FakeGit {
        upstream: Option<String>,
        remotes: HashMap<String, String>,
    }

    impl GitRunner for FakeGit {
        fn run(
            &self,
            _directory: &Path,
            arguments: &[OsString],
        ) -> Result<CommandOutput, GitError> {
            let arguments: Vec<String> = arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            let value = if arguments.as_slice() == ["remote"] {
                let mut names: Vec<&str> = self.remotes.keys().map(String::as_str).collect();
                names.sort_unstable();
                Some(names.join("\n"))
            } else if arguments
                .first()
                .is_some_and(|argument| argument == "config")
            {
                self.upstream.clone()
            } else if arguments
                .first()
                .is_some_and(|argument| argument == "remote")
                && arguments
                    .get(1)
                    .is_some_and(|argument| argument == "get-url")
            {
                arguments
                    .get(2)
                    .and_then(|remote| self.remotes.get(remote))
                    .cloned()
            } else {
                None
            };
            Ok(CommandOutput {
                stdout: value
                    .as_ref()
                    .map(|value| format!("{value}\n").into_bytes())
                    .unwrap_or_default(),
                stderr: Vec::new(),
                success: value.is_some(),
            })
        }
    }

    struct FakeResponse {
        status: &'static str,
        headers: Vec<(String, String)>,
        body: String,
    }

    fn fake_server(responses: Vec<FakeResponse>) -> (String, Receiver<String>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                sender.send(request).unwrap();
                let headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    response.status,
                    response.body.len(),
                    headers,
                    response.body
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), receiver, handle)
    }

    fn fake_server_with(
        request_count: usize,
        responder: impl Fn(&str) -> FakeResponse + Send + Sync + 'static,
    ) -> (String, Receiver<String>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let responder = Arc::new(responder);
        let handle = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let sender = sender.clone();
                let responder = Arc::clone(&responder);
                handlers.push(std::thread::spawn(move || {
                    let request = read_request(&mut stream);
                    let response = responder(&request);
                    sender.send(request).unwrap();
                    let headers = response
                        .headers
                        .iter()
                        .map(|(name, value)| format!("{name}: {value}\r\n"))
                        .collect::<String>();
                    write!(
                        stream,
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                        response.status,
                        response.body.len(),
                        headers,
                        response.body
                    )
                    .unwrap();
                }));
            }
            drop(sender);
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        (format!("http://{address}"), receiver, handle)
    }

    fn request_body(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + content_length {
                break;
            }
        }
        String::from_utf8(request).unwrap()
    }

    fn success_body(branches: usize) -> String {
        let repository = (0..branches)
            .map(|index| {
                (
                    format!("branch{index}"),
                    serde_json::json!({
                        "associatedPullRequests": {"nodes": []}
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "data": {
                "repository": repository,
                "rateLimit": {"remaining": 4999, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string()
    }

    fn authored_node(number: u64, author: &str, draft: bool) -> Value {
        serde_json::json!({
            "number": number,
            "title": format!("change {number}"),
            "url": format!("https://example/pull/{number}"),
            "state": "OPEN",
            "isDraft": draft,
            "mergedAt": null,
            "updatedAt": "2026-01-02T00:00:00Z",
            "reviewDecision": null,
            "author": {"login": author},
            "assignees": {"nodes": []},
            "baseRefName": "main",
            "baseRefOid": "baseoid",
            "baseRepository": {"nameWithOwner": "base/project"},
            "headRefName": "topic",
            "headRefOid": "headoid",
            "headRepository": {"nameWithOwner": "fork/project"},
            "commits": {"nodes": []}
        })
    }

    fn authored_body(nodes: Vec<Value>, has_next_page: bool, cursor: Option<&str>) -> String {
        serde_json::json!({
            "data": {
                "viewer": {"login": "viewer"},
                "search": {
                    "issueCount": 1,
                    "pageInfo": {
                        "hasNextPage": has_next_page,
                        "endCursor": cursor
                    },
                    "nodes": nodes
                },
                "rateLimit": {"remaining": 4999, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string()
    }

    fn detail_body(checks: Vec<Value>, has_next_page: bool, cursor: Option<&str>) -> String {
        serde_json::json!({
            "data": {
                "repository": {
                    "pr0": {
                        "mergeable": "MERGEABLE",
                        "mergeStateStatus": "CLEAN",
                        "reviewRequests": {
                            "nodes": [{
                                "requestedReviewer": {
                                    "__typename": "Team", "id": "TEAM_1",
                                    "slug": "maintainers", "name": "Maintainers"
                                }
                            }],
                            "pageInfo": {"hasNextPage": false}
                        },
                        "reviews": {
                            "nodes": [{
                                "id": "REVIEW_1", "databaseId": 91,
                                "author": {"login": "reviewer"},
                                "body": "Please fix the race", "state": "CHANGES_REQUESTED",
                                "submittedAt": "2026-01-02T00:00:00Z",
                                "url": "https://example/review/91"
                            }],
                            "pageInfo": {"hasNextPage": false}
                        },
                        "reviewThreads": {
                            "nodes": [
                                {
                                    "id": "THREAD_1", "isResolved": false,
                                    "isOutdated": false, "path": "src/lib.rs",
                                    "comments": {
                                        "nodes": [{
                                            "id": "COMMENT_1", "databaseId": 101,
                                            "author": {"login": "reviewer"},
                                            "body": "This can deadlock",
                                            "url": "https://example/comment/101"
                                        }],
                                        "pageInfo": {"hasPreviousPage": false}
                                    }
                                },
                                {
                                    "id": "THREAD_RESOLVED", "isResolved": true,
                                    "isOutdated": false, "path": "src/old.rs",
                                    "comments": {
                                        "nodes": [{
                                            "id": "COMMENT_RESOLVED", "databaseId": 102,
                                            "author": {"login": "reviewer"},
                                            "body": "Already fixed",
                                            "url": "https://example/comment/102"
                                        }],
                                        "pageInfo": {"hasPreviousPage": false}
                                    }
                                }
                            ],
                            "pageInfo": {"hasNextPage": false}
                        },
                        "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                            "contexts": {
                                "nodes": checks,
                                "pageInfo": {
                                    "hasNextPage": has_next_page,
                                    "endCursor": cursor
                                }
                            }
                        }}}]}
                    }
                },
                "rateLimit": {"remaining": 4999, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string()
    }

    fn input(branches: usize) -> RepositoryGitHubInput {
        RepositoryGitHubInput {
            repository: RepositoryConfig {
                path: PathBuf::from("/repo"),
                label: None,
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            worktrees: (0..branches)
                .map(|index| Worktree {
                    path: PathBuf::from(format!("/tree-{index}")),
                    head: Some("abcdef".to_owned()),
                    branch: Some(format!("refs/heads/topic-{index}")),
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                })
                .collect(),
            trunk_branch: None,
        }
    }

    fn repository_with_remotes(
        path: &str,
        configured: Option<&str>,
        remotes: impl IntoIterator<Item = (&'static str, GitHubRepositoryIdentity)>,
    ) -> RepositoryConfig {
        RepositoryConfig {
            path: PathBuf::from(path),
            label: None,
            worktree_root: None,
            github_remote: configured.map(str::to_owned),
            github_remotes: remotes
                .into_iter()
                .map(|(name, identity)| (name.to_owned(), identity))
                .collect(),
            github_preferred_remote: None,
        }
    }

    fn identity(host: &str, owner: &str, repository: &str) -> GitHubRepositoryIdentity {
        GitHubRepositoryIdentity::canonical(host, owner, repository)
    }

    #[test]
    fn remote_cache_reconciles_additions_removals_conflicts_and_preference() {
        let retained = identity("github.com", "original", "project");
        let mut repository = repository_with_remotes(
            "/repo",
            Some("upstream"),
            [
                ("upstream", retained.clone()),
                ("removed", identity("github.com", "old", "gone")),
            ],
        );
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([
                (
                    "origin".to_owned(),
                    "git@github.com:team/project.git".to_owned(),
                ),
                (
                    "upstream".to_owned(),
                    "https://ghe.example/new/project.git".to_owned(),
                ),
                ("local".to_owned(), "file:///tmp/project.git".to_owned()),
            ]),
        };
        let refresh = refresh_repository_remote_identities(&git, &mut repository).unwrap();
        assert!(refresh.changed);
        assert_eq!(refresh.warnings.len(), 1);
        assert!(refresh.warnings[0].contains("retains github.com/original/project"));
        assert_eq!(repository.github_remotes["upstream"], retained);
        assert_eq!(
            repository.github_remotes["origin"],
            identity("github.com", "team", "project")
        );
        assert!(!repository.github_remotes.contains_key("removed"));
        assert!(!repository.github_remotes.contains_key("local"));
        assert_eq!(
            repository.github_preferred_remote.as_deref(),
            Some("upstream")
        );
    }

    #[test]
    fn catalog_remote_refresh_skips_paths_excluded_by_startup_discovery() {
        let retained = identity("github.com", "cached", "invalid");
        let mut catalog = Catalog {
            repositories: vec![
                repository_with_remotes("/invalid", None, [("origin", retained.clone())]),
                repository_with_remotes("/valid", None, []),
            ],
            ..Catalog::default()
        };
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([(
                "origin".to_owned(),
                "git@github.com:team/project.git".to_owned(),
            )]),
        };

        let refresh = refresh_catalog_remote_identities(
            &git,
            &mut catalog,
            &HashSet::from([PathBuf::from("/valid")]),
        );

        assert!(refresh.changed);
        assert!(refresh.warnings.is_empty());
        assert_eq!(catalog.repositories[0].github_remotes["origin"], retained);
        assert_eq!(
            catalog.repositories[1].github_remotes["origin"],
            identity("github.com", "team", "project")
        );
    }

    #[test]
    fn authored_search_includes_pull_requests_authored_by_or_assigned_to_the_viewer() {
        let initial_searches = Arc::new((std::sync::Mutex::new(0), std::sync::Condvar::new()));
        let searches_overlapped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let responder_gate = Arc::clone(&initial_searches);
        let responder_overlap = Arc::clone(&searches_overlapped);
        let (base, requests, server) = fake_server_with(3, move |request| {
            let body = request_body(request);
            let query = body["variables"]["query"].as_str().unwrap();
            let cursor = body["variables"]["cursor"].as_str();
            if cursor.is_none() {
                let (lock, condition) = &*responder_gate;
                let mut arrived = lock.lock().unwrap();
                *arrived += 1;
                if *arrived == 2 {
                    responder_overlap.store(true, std::sync::atomic::Ordering::SeqCst);
                    condition.notify_all();
                } else {
                    let (next, _) = condition
                        .wait_timeout_while(arrived, Duration::from_secs(2), |arrived| *arrived < 2)
                        .unwrap();
                    arrived = next;
                }
                drop(arrived);
            }

            let body = if query.contains("author:@me") && cursor.is_none() {
                authored_body(
                    vec![
                        authored_node(1, "viewer", true),
                        authored_node(4, "someone-else", false),
                    ],
                    true,
                    Some("cursor-1"),
                )
            } else if query.contains("author:@me") {
                authored_body(vec![authored_node(3, "VIEWER", false)], false, None)
            } else {
                authored_body(
                    vec![{
                        let mut node = authored_node(2, "someone-else", false);
                        node["assignees"] = serde_json::json!({
                            "nodes": [{"login": "VIEWER"}]
                        });
                        node
                    }],
                    false,
                    None,
                )
            };
            FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body,
            }
        });
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let host = AuthoredHost {
            host: "ghe.example".to_owned(),
            graphql_url: format!("{base}/api/graphql"),
            credential_anchor: PathBuf::from("/repo"),
        };
        let mut events = Vec::new();
        GitHubService::new().fetch_authored_with(&credentials, &[host], |event| events.push(event));
        let request_bodies = (0..3)
            .map(|_| request_body(&requests.recv().unwrap()))
            .collect::<Vec<_>>();
        server.join().unwrap();
        assert!(searches_overlapped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(request_bodies.iter().any(|body| {
            body["variables"]["query"] == "is:pr is:open author:@me"
                && body["variables"]["cursor"].is_null()
        }));
        assert!(request_bodies.iter().any(|body| {
            body["variables"]["query"] == "is:pr is:open author:@me"
                && body["variables"]["cursor"] == "cursor-1"
        }));
        assert!(request_bodies.iter().any(|body| {
            body["variables"]["query"] == "is:pr is:open assignee:@me"
                && body["variables"]["cursor"].is_null()
        }));
        assert!(
            request_bodies
                .iter()
                .all(|body| !body["query"].as_str().unwrap().contains("author:viewer"))
        );

        let pages: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AuthoredRefreshEvent::Page { pull_requests, .. } => Some(pull_requests),
                AuthoredRefreshEvent::Finished { .. } => None,
            })
            .collect();
        assert_eq!(pages.len(), 3);
        let mut pull_requests = pages
            .iter()
            .flat_map(|page| page.iter())
            .collect::<Vec<_>>();
        pull_requests.sort_by_key(|pull_request| pull_request.identity.number);
        assert_eq!(pull_requests[0].pull_request.state, PullRequestState::Draft);
        assert_eq!(pull_requests[1].identity.number, 2);
        assert_eq!(pull_requests[1].author, "someone-else");
        assert_eq!(pull_requests[2].identity.number, 3);
        assert!(matches!(
            events.last(),
            Some(AuthoredRefreshEvent::Finished {
                complete: true,
                error: None,
                ..
            })
        ));
    }

    #[test]
    fn detail_hydration_deduplicates_identity_and_paginates_check_contexts() {
        let first_check = serde_json::json!({
            "__typename": "CheckRun", "name": "build", "status": "COMPLETED",
            "conclusion": "FAILURE", "detailsUrl": "https://example/check/old",
            "completedAt": "2026-01-01T00:00:00Z", "isRequired": true
        });
        let current_check = serde_json::json!({
            "__typename": "CheckRun", "name": "Build", "status": "COMPLETED",
            "conclusion": "SUCCESS", "detailsUrl": "https://example/check/new",
            "completedAt": "2026-01-02T00:00:00Z", "isRequired": true
        });
        let optional_failure = serde_json::json!({
            "__typename": "StatusContext", "context": "lint", "state": "FAILURE",
            "targetUrl": "https://example/check/lint", "createdAt": "2026-01-02T01:00:00Z",
            "isRequired": false
        });
        let (base, requests, server) = fake_server(vec![
            FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body: detail_body(vec![first_check], true, Some("contexts-1")),
            },
            FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body: detail_body(vec![current_check, optional_failure], false, None),
            },
        ]);
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let host = AuthoredHost {
            host: "ghe.example".to_owned(),
            graphql_url: format!("{base}/api/graphql"),
            credential_anchor: PathBuf::from("/repo"),
        };
        let identity = CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical("ghe.example", "team", "project"),
            number: 42,
        };

        let hydrated = GitHubService::new().hydrate_pull_requests_with(
            &credentials,
            &[host],
            [identity.clone(), identity.clone()],
        );

        let first_request = requests.recv().unwrap();
        let second_request = requests.recv().unwrap();
        server.join().unwrap();
        let request_json = |request: &str| -> Value {
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
        };
        let first = request_json(&first_request);
        assert_eq!(first["variables"]["owner"], "team");
        assert_eq!(first["variables"]["repository"], "project");
        assert_eq!(first["variables"]["number"], 42);
        assert!(first["variables"]["contextsCursor"].is_null());
        assert!(
            first["query"]
                .as_str()
                .unwrap()
                .contains("pr0: pullRequest")
        );
        assert!(
            first["query"]
                .as_str()
                .unwrap()
                .contains("isRequired(pullRequestNumber: 42)")
        );
        let query = first["query"].as_str().unwrap();
        assert_eq!(query.matches('{').count(), query.matches('}').count());
        assert_eq!(
            request_json(&second_request)["variables"]["contextsCursor"],
            "contexts-1"
        );
        let details = hydrated[&identity].as_ref().unwrap();
        assert!(details.check_contexts_complete);
        assert_eq!(details.checks.len(), 2);
        assert_eq!(
            details.required_check_readiness(),
            crate::model::RequiredCheckReadiness::Ready
        );
        assert_eq!(details.attention_summary().optional_failures, 1);
        assert_eq!(details.review_requests[0].name, "maintainers");
        assert_eq!(details.reviewer_reviews[0].database_id, Some(91));
        assert_eq!(details.feedback.len(), 2);
        assert!(details.feedback.iter().any(|feedback| {
            feedback.kind == FeedbackKind::ReviewSummary
                && feedback.id == "REVIEW_1"
                && feedback.author == "reviewer"
                && feedback.body == "Please fix the race"
                && feedback.permalink.as_deref() == Some("https://example/review/91")
        }));
        assert!(
            details
                .feedback
                .iter()
                .any(|feedback| feedback.database_id == Some(101))
        );
        assert!(
            details
                .feedback
                .iter()
                .all(|feedback| feedback.database_id != Some(102))
        );
    }

    #[test]
    fn partial_detail_connections_remain_usable_and_unknown() {
        let body = serde_json::json!({
            "data": {
                "repository": {"pr0": {
                    "mergeable": "CONFLICTING",
                    "reviewRequests": null,
                    "reviews": null,
                    "reviewThreads": null,
                    "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
                }},
                "rateLimit": {"remaining": 10, "resetAt": "2026-07-30T12:00:00Z"}
            },
            "errors": [{
                "message": "review threads are inaccessible",
                "path": ["repository", "pr0", "reviewThreads"]
            }]
        })
        .to_string();
        let (base, _requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body,
        }]);
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let host = AuthoredHost {
            host: "ghe.example".to_owned(),
            graphql_url: format!("{base}/api/graphql"),
            credential_anchor: PathBuf::from("/repo"),
        };
        let identity = CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical("ghe.example", "team", "project"),
            number: 7,
        };

        let hydrated = GitHubService::new().hydrate_pull_requests_with(
            &credentials,
            &[host],
            [identity.clone()],
        );
        server.join().unwrap();

        let details = hydrated[&identity].as_ref().unwrap();
        assert_eq!(details.merge_conflict, MergeConflictState::Conflicting);
        assert!(!details.check_contexts_complete);
        assert!(!details.reviews_complete);
        assert!(!details.feedback_complete);
        assert_eq!(
            details.required_check_readiness(),
            crate::model::RequiredCheckReadiness::Unknown
        );
        assert!(
            details
                .warnings
                .iter()
                .any(|warning| warning.contains("review threads are inaccessible"))
        );
    }

    #[test]
    fn detail_hydration_bounds_context_pagination_and_marks_truncation_unknown() {
        let responses = (0..MAX_CHECK_CONTEXT_PAGES)
            .map(|page| FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body: detail_body(Vec::new(), true, Some(&format!("cursor-{page}"))),
            })
            .collect();
        let (base, _requests, server) = fake_server(responses);
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let host = AuthoredHost {
            host: "ghe.example".to_owned(),
            graphql_url: format!("{base}/api/graphql"),
            credential_anchor: PathBuf::from("/repo"),
        };
        let identity = CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical("ghe.example", "team", "project"),
            number: 9,
        };

        let hydrated = GitHubService::new().hydrate_pull_requests_with(
            &credentials,
            &[host],
            [identity.clone()],
        );
        server.join().unwrap();

        let details = hydrated[&identity].as_ref().unwrap();
        assert!(!details.check_contexts_complete);
        assert_eq!(
            details.required_check_readiness(),
            crate::model::RequiredCheckReadiness::Unknown
        );
        assert!(
            details
                .warnings
                .iter()
                .any(|warning| warning.contains("truncated after 1000 entries"))
        );
    }

    #[test]
    fn selected_pull_request_refetch_accepts_merged_state_and_current_head_sha() {
        let mut node = authored_node(42, "viewer", false);
        node["state"] = Value::String("CLOSED".to_owned());
        node["mergedAt"] = Value::String("2026-02-01T00:00:00Z".to_owned());
        node["headRefOid"] = Value::String("current-head".to_owned());
        let body = serde_json::json!({
            "data": {
                "repository": {"pullRequest": node},
                "rateLimit": {"remaining": 100, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string();
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body,
        }]);
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let identity = CanonicalPullRequestId {
            repository: identity("ghe.example", "base", "project"),
            number: 42,
        };
        let refreshed = GitHubService::new()
            .fetch_pull_request_with(
                &credentials,
                &AuthoredHost {
                    host: "ghe.example".to_owned(),
                    graphql_url: format!("{base}/api/graphql"),
                    credential_anchor: PathBuf::from("/repo"),
                },
                &identity,
            )
            .unwrap();
        let request = requests.recv().unwrap();
        server.join().unwrap();
        let request_body: Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(request_body["variables"]["owner"], "base");
        assert_eq!(request_body["variables"]["repository"], "project");
        assert_eq!(request_body["variables"]["number"], 42);
        assert_eq!(refreshed.pull_request.state, PullRequestState::Merged);
        assert_eq!(
            refreshed.pull_request.head.oid.as_deref(),
            Some("current-head")
        );
    }

    #[test]
    fn selected_pull_request_refetch_rejects_missing_repository_or_pr() {
        let body = serde_json::json!({
            "data": {
                "repository": null,
                "rateLimit": {"remaining": 100, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string();
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body,
        }]);
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let identity = CanonicalPullRequestId {
            repository: identity("ghe.example", "base", "project"),
            number: 42,
        };
        let result = GitHubService::new().fetch_pull_request_with(
            &credentials,
            &AuthoredHost {
                host: "ghe.example".to_owned(),
                graphql_url: format!("{base}/api/graphql"),
                credential_anchor: PathBuf::from("/repo"),
            },
            &identity,
        );
        requests.recv().unwrap();
        server.join().unwrap();
        assert!(matches!(
            result,
            Err(GitHubError::PullRequestUnavailable { number: 42, .. })
        ));
    }

    #[test]
    fn authored_search_reports_later_page_failure_after_publishing_prior_pages() {
        let (base, requests, server) = fake_server_with(3, |request| {
            let body = request_body(request);
            let query = body["variables"]["query"].as_str().unwrap();
            let cursor = body["variables"]["cursor"].as_str();
            if query.contains("assignee:@me") {
                FakeResponse {
                    status: "200 OK",
                    headers: Vec::new(),
                    body: authored_body(Vec::new(), false, None),
                }
            } else if cursor.is_none() {
                FakeResponse {
                    status: "200 OK",
                    headers: Vec::new(),
                    body: authored_body(
                        vec![authored_node(1, "viewer", false)],
                        true,
                        Some("next"),
                    ),
                }
            } else {
                FakeResponse {
                    status: "500 Internal Server Error",
                    headers: Vec::new(),
                    body: r#"{"message":"temporary failure"}"#.to_owned(),
                }
            }
        });
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let mut events = Vec::new();
        GitHubService::new().fetch_authored_with(
            &credentials,
            &[AuthoredHost {
                host: "ghe.example".to_owned(),
                graphql_url: format!("{base}/api/graphql"),
                credential_anchor: PathBuf::from("/repo"),
            }],
            |event| events.push(event),
        );
        for _ in 0..3 {
            requests.recv().unwrap();
        }
        server.join().unwrap();
        assert!(matches!(events[0], AuthoredRefreshEvent::Page { .. }));
        assert!(matches!(
            events.last(),
            Some(AuthoredRefreshEvent::Finished {
                complete: false,
                error: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn authored_search_warns_when_the_thousand_result_ceiling_truncates() {
        let request_count = MAX_AUTHORED_PULL_REQUEST_PAGES + 1;
        let (base, requests, server) = fake_server_with(request_count, |request| {
            let body = request_body(request);
            let query = body["variables"]["query"].as_str().unwrap();
            if query.contains("assignee:@me") {
                FakeResponse {
                    status: "200 OK",
                    headers: Vec::new(),
                    body: authored_body(Vec::new(), false, None),
                }
            } else {
                FakeResponse {
                    status: "200 OK",
                    headers: Vec::new(),
                    body: authored_body(Vec::new(), true, Some("next")),
                }
            }
        });
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "secret".to_owned());
        let mut events = Vec::new();
        GitHubService::new().fetch_authored_with(
            &credentials,
            &[AuthoredHost {
                host: "ghe.example".to_owned(),
                graphql_url: format!("{base}/api/graphql"),
                credential_anchor: PathBuf::from("/repo"),
            }],
            |event| events.push(event),
        );
        for _ in 0..request_count {
            requests.recv().unwrap();
        }
        server.join().unwrap();
        assert!(matches!(
            events.last(),
            Some(AuthoredRefreshEvent::Finished {
                complete: false,
                warnings,
                ..
            }) if warnings.iter().any(|warning| warning.contains("truncated at 1,000"))
        ));
    }

    #[test]
    fn inferred_hosts_union_explicit_cached_and_github_com() {
        let catalog = Catalog {
            github_hosts: vec!["Explicit.Example".to_owned()],
            repositories: vec![repository_with_remotes(
                "/repo",
                None,
                [("origin", identity("GHE.EXAMPLE", "team", "project"))],
            )],
            ..Catalog::default()
        };
        assert_eq!(
            inferred_github_hosts(&catalog),
            BTreeSet::from([
                "explicit.example".to_owned(),
                "ghe.example".to_owned(),
                "github.com".to_owned(),
            ])
        );
    }

    #[test]
    fn canonical_ids_use_the_base_repository_and_mapping_obeys_precedence() {
        let pull_request = PullRequest {
            number: 42,
            title: "fork change".to_owned(),
            url: "https://github.com/base/project/pull/42".to_owned(),
            state: PullRequestState::Open,
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            review_decision: None,
            auto_merge: false,
            base: PullRequestIdentity {
                repository: Some("Base/Project".to_owned()),
                branch: "main".to_owned(),
                oid: Some("base".to_owned()),
            },
            head: PullRequestIdentity {
                repository: Some("Contributor/Fork".to_owned()),
                branch: "topic".to_owned(),
                oid: Some("head".to_owned()),
            },
            checks: CheckRollup::Pending,
        };
        let canonical = canonical_pull_request_id("GitHub.COM", &pull_request).unwrap();
        assert_eq!(
            canonical.repository,
            identity("github.com", "base", "project")
        );

        let catalog = Catalog {
            repositories: vec![
                repository_with_remotes(
                    "/earliest",
                    None,
                    [("upstream", canonical.repository.clone())],
                ),
                repository_with_remotes(
                    "/origin",
                    None,
                    [("origin", canonical.repository.clone())],
                ),
                repository_with_remotes(
                    "/configured",
                    Some("base"),
                    [("base", canonical.repository.clone())],
                ),
            ],
            ..Catalog::default()
        };
        let mappings = map_pull_request_identities(
            &catalog,
            [canonical.clone(), canonical.clone()],
            &HashSet::new(),
            |_| true,
        );
        assert_eq!(mappings.len(), 1, "a PR is displayed only once");
        assert_eq!(
            mappings[0].mapped_repository.as_deref(),
            Some(Path::new("/configured"))
        );

        let active = HashSet::from([canonical.clone()]);
        assert!(
            map_pull_request_identities(&catalog, [canonical], &active, |_| true).is_empty(),
            "only canonical associated-PR identity suppresses a virtual PR"
        );
    }

    #[test]
    fn only_the_selected_associated_pr_marks_a_worktree_active() {
        let mut exact_branch = authored_node(1, "viewer", false);
        exact_branch["headRefName"] = Value::String("topic-0".to_owned());
        exact_branch["headRefOid"] = Value::String("branch-head".to_owned());
        exact_branch["updatedAt"] = Value::String("2026-01-01T00:00:00Z".to_owned());
        let mut newer_descendant = authored_node(2, "viewer", false);
        newer_descendant["headRefName"] = Value::String("descendant".to_owned());
        newer_descendant["headRefOid"] = Value::String("descendant-head".to_owned());
        newer_descendant["updatedAt"] = Value::String("2026-02-01T00:00:00Z".to_owned());
        let body = serde_json::json!({
            "data": {
                "repository": {
                    "branch0": {
                        "associatedPullRequests": {
                            "nodes": [exact_branch, newer_descendant]
                        }
                    }
                },
                "rateLimit": {"remaining": 10, "resetAt": "2026-07-30T12:00:00Z"}
            }
        })
        .to_string();
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body,
        }]);
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([("origin".to_owned(), format!("{base}/base/project.git"))]),
        };
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "token".to_owned());

        let refresh = GitHubService::new().fetch_catalog_with(&git, &credentials, &[input(1)]);

        requests.recv().unwrap();
        server.join().unwrap();
        assert_eq!(
            refresh.branches[Path::new("/tree-0")]
                .as_ref()
                .unwrap()
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number),
            Some(1)
        );
        assert_eq!(
            refresh
                .active_pull_requests
                .iter()
                .map(|identity| identity.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn exact_head_oid_selects_the_parent_pr_for_a_repointed_branch() {
        let target = BranchTarget {
            worktree: PathBuf::from("/tree"),
            branch: "different-local-branch".to_owned(),
            head: Some("shared-parent-head".to_owned()),
        };
        let mut parent = authored_node(33580, "viewer", false);
        parent["headRefName"] =
            Value::String("wbbradley/context-hub-fleet-path-validation".to_owned());
        parent["headRefOid"] = Value::String("shared-parent-head".to_owned());
        parent["updatedAt"] = Value::String("2026-01-01T00:00:00Z".to_owned());
        let mut child = authored_node(33902, "viewer", false);
        child["baseRefName"] =
            Value::String("wbbradley/context-hub-fleet-path-validation".to_owned());
        child["headRefName"] = Value::String("context-hub-materialization-hardening".to_owned());
        child["headRefOid"] = Value::String("child-head".to_owned());
        child["updatedAt"] = Value::String("2026-02-01T00:00:00Z".to_owned());
        let data = serde_json::json!({
            "repository": {
                "branch0": {"associatedPullRequests": {"nodes": [parent, child]}}
            }
        });

        let (display, associations) =
            parse_batch_data(&data, &[target], &[], &[], None, "github.com").unwrap();

        assert_eq!(
            display[0]
                .as_ref()
                .unwrap()
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number),
            Some(33580)
        );
        assert_eq!(associations[0].as_ref().unwrap().len(), 2);
    }

    #[test]
    fn exact_branch_match_can_select_a_merged_pr_over_an_open_sha_match() {
        let target = BranchTarget {
            worktree: PathBuf::from("/tree"),
            branch: "topic".to_owned(),
            head: Some("shared-head".to_owned()),
        };
        let mut exact_branch = authored_node(4, "viewer", false);
        exact_branch["state"] = Value::String("CLOSED".to_owned());
        exact_branch["mergedAt"] = Value::String("2026-01-03T00:00:00Z".to_owned());
        exact_branch["headRefOid"] = Value::String("old-head".to_owned());
        let mut exact_sha = authored_node(5, "viewer", false);
        exact_sha["headRefName"] = Value::String("renamed-topic".to_owned());
        exact_sha["headRefOid"] = Value::String("shared-head".to_owned());
        let data = serde_json::json!({
            "repository": {
                "branch0": {"associatedPullRequests": {"nodes": [exact_sha, exact_branch]}}
            }
        });

        let (display, _) =
            parse_batch_data(&data, &[target], &[], &[], None, "github.com").unwrap();

        assert_eq!(
            display[0]
                .as_ref()
                .unwrap()
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number),
            Some(4)
        );

        let mut higher_number = authored_node(8, "viewer", false);
        higher_number["headRefName"] = Value::String("topic".to_owned());
        let mut lower_number = higher_number.clone();
        lower_number["number"] = Value::from(7);
        lower_number["url"] = Value::String("https://example/pull/7".to_owned());
        let candidates = vec![
            normalize_pull_request(&higher_number).unwrap(),
            normalize_pull_request(&lower_number).unwrap(),
        ];
        assert_eq!(
            prefer_pull_request(
                candidates,
                &BranchTarget {
                    worktree: PathBuf::from("/tree"),
                    branch: "topic".to_owned(),
                    head: Some("unmatched".to_owned()),
                }
            )
            .unwrap()
            .number,
            7,
            "equal-priority candidates use a stable canonical ordering"
        );
    }

    #[test]
    fn strict_association_results_keep_all_prs_and_reject_partial_parsing() {
        let target = BranchTarget {
            worktree: PathBuf::from("/tree"),
            branch: "topic".to_owned(),
            head: Some("headoid".to_owned()),
        };
        let first = authored_node(1, "viewer", false);
        let second = authored_node(2, "viewer", false);
        let data = serde_json::json!({
            "repository": {
                "branch0": {
                    "associatedPullRequests": {"nodes": [first, second]}
                }
            }
        });
        let (_, associations) = parse_batch_data(
            &data,
            std::slice::from_ref(&target),
            &[],
            &[],
            None,
            "github.com",
        )
        .unwrap();
        let associations = associations.into_iter().next().unwrap().unwrap();
        assert_eq!(associations.len(), 2);
        assert_eq!(associations[0].identity.number, 1);
        assert_eq!(associations[1].identity.number, 2);

        let partial = serde_json::json!({
            "repository": {
                "branch0": {
                    "associatedPullRequests": {
                        "nodes": [authored_node(1, "viewer", false), {"number": 2}]
                    }
                }
            }
        });
        let (display, associations) =
            parse_batch_data(&partial, &[target], &[], &[], None, "github.com").unwrap();
        assert!(
            display[0].is_ok(),
            "the TUI may retain the usable PR plus a warning"
        );
        assert!(matches!(associations[0], Err(GitHubError::Malformed(_))));
    }

    #[test]
    fn mapping_skips_unusable_cached_repositories() {
        let pull_request = CanonicalPullRequestId {
            repository: identity("github.com", "base", "project"),
            number: 7,
        };
        let catalog = Catalog {
            repositories: vec![repository_with_remotes(
                "/missing",
                Some("origin"),
                [("origin", pull_request.repository.clone())],
            )],
            ..Catalog::default()
        };
        let mappings =
            map_pull_request_identities(&catalog, [pull_request], &HashSet::new(), |_| false);
        assert_eq!(mappings[0].mapped_repository, None);
    }

    #[test]
    fn parses_github_and_enterprise_remote_forms() {
        let ssh = parse_remote_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(ssh.host, "github.com");
        assert_eq!(ssh.full_name(), "owner/repo");
        assert_eq!(ssh.graphql_url(), "https://api.github.com/graphql");

        let https = parse_remote_url("https://ghe.example/team/project.git").unwrap();
        assert_eq!(https.graphql_url(), "https://ghe.example/api/graphql");
        assert_eq!(https.rest_base(), "https://ghe.example/api/v3");

        let ssh_url = parse_remote_url("ssh://git@ghe.example:2222/team/project.git").unwrap();
        assert_eq!(ssh_url.host, "ghe.example");
        assert_eq!(ssh_url.full_name(), "team/project");

        let fake = parse_remote_url("http://127.0.0.1:1234/team/project.git").unwrap();
        assert_eq!(fake.graphql_url(), "http://127.0.0.1:1234/api/graphql");
        assert!(parse_remote_url("file:///tmp/repo").is_err());

        let uppercase = parse_remote_url("SSH://git@GHE.EXAMPLE:2222/team/project.git").unwrap();
        assert_eq!(uppercase.host, "ghe.example");
    }

    #[test]
    fn credentials_follow_precedence_and_redact_tokens() {
        let anchor = Path::new("/repo");
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_TOKEN".to_owned(), "environment-secret".to_owned());
        credentials.git_config.insert(
            "github.github-com.token".to_owned(),
            "config-secret".to_owned(),
        );
        credentials
            .gh
            .insert("github.com".to_owned(), "gh-secret".to_owned());
        let token = resolve_token(&credentials, "github.com", anchor).unwrap();
        assert_eq!(token.source, AuthSource::Environment);
        assert_eq!(token.expose(), "environment-secret");
        let debug = format!("{token:?}");
        assert!(!debug.contains("environment-secret"));
        assert!(debug.contains("REDACTED"));

        credentials.environment.clear();
        assert_eq!(
            resolve_token(&credentials, "github.com", anchor)
                .unwrap()
                .source,
            AuthSource::RepositoryGitConfig
        );
        credentials.git_config.clear();
        assert_eq!(
            resolve_token(&credentials, "github.com", anchor)
                .unwrap()
                .source,
            AuthSource::GhCli
        );
    }

    #[test]
    fn branch_remote_precedence_prefers_upstream_then_configured_remote() {
        let repository = RepositoryConfig {
            path: PathBuf::from("/repo"),
            label: None,
            worktree_root: None,
            github_remote: Some("configured".to_owned()),
            github_remotes: Default::default(),
            github_preferred_remote: None,
        };
        let upstream = FakeGit {
            upstream: Some("fork".to_owned()),
            remotes: HashMap::from([
                (
                    "fork".to_owned(),
                    "git@github.com:fork/project.git".to_owned(),
                ),
                (
                    "configured".to_owned(),
                    "git@github.com:team/project.git".to_owned(),
                ),
            ]),
        };
        assert_eq!(
            resolve_branch_remote(&upstream, &repository, "topic")
                .unwrap()
                .full_name(),
            "fork/project"
        );
        let configured = FakeGit {
            upstream: None,
            remotes: upstream.remotes,
        };
        assert_eq!(
            resolve_branch_remote(&configured, &repository, "topic")
                .unwrap()
                .full_name(),
            "team/project"
        );
    }

    #[test]
    fn remote_symbolic_head_identifies_trunk_and_excludes_only_that_worktree() {
        struct TrunkGit;

        impl GitRunner for TrunkGit {
            fn run(
                &self,
                _directory: &Path,
                arguments: &[OsString],
            ) -> Result<CommandOutput, GitError> {
                let arguments = arguments
                    .iter()
                    .map(|argument| argument.to_string_lossy())
                    .collect::<Vec<_>>();
                let value = match arguments.as_slice() {
                    [remote] if remote == "remote" => Some("origin"),
                    [command, _, _, reference]
                        if command == "symbolic-ref" && reference == "refs/remotes/origin/HEAD" =>
                    {
                        Some("origin/develop")
                    }
                    _ => None,
                };
                Ok(CommandOutput {
                    stdout: value.map_or_else(Vec::new, |value| format!("{value}\n").into_bytes()),
                    stderr: Vec::new(),
                    success: value.is_some(),
                })
            }
        }

        let mut input = input(2);
        input.worktrees[0].branch = Some("refs/heads/develop".to_owned());
        input.worktrees[1].branch = Some("refs/heads/topic".to_owned());
        input.trunk_branch = remote_trunk_branch(&TrunkGit, &input.repository).unwrap();

        assert_eq!(input.trunk_branch.as_deref(), Some("develop"));
        assert!(!input.refreshes_worktree(&input.worktrees[0]));
        assert!(input.refreshes_worktree(&input.worktrees[1]));
    }

    #[test]
    fn fetch_uses_required_headers_variables_and_bounded_batches() {
        let (base, requests, server) = fake_server(vec![
            FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body: success_body(MAX_BRANCHES_PER_BATCH),
            },
            FakeResponse {
                status: "200 OK",
                headers: Vec::new(),
                body: success_body(1),
            },
        ]);
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([("origin".to_owned(), format!("{base}/team/project.git"))]),
        };
        let mut credentials = FakeCredentials::default();
        credentials.environment.insert(
            "GITHUB_ENTERPRISE_TOKEN".to_owned(),
            "top-secret".to_owned(),
        );
        let refresh = GitHubService::new().fetch_catalog_with(
            &git,
            &credentials,
            &[input(MAX_BRANCHES_PER_BATCH + 1)],
        );
        assert_eq!(refresh.branches.len(), MAX_BRANCHES_PER_BATCH + 1);
        assert!(refresh.branches.values().all(Result::is_ok));

        let captured = [requests.recv().unwrap(), requests.recv().unwrap()];
        server.join().unwrap();
        let mut batch_sizes = Vec::new();
        for request in captured {
            let lower = request.to_ascii_lowercase();
            assert!(lower.starts_with("post /api/graphql http/1.1"));
            assert!(lower.contains("authorization: bearer top-secret"));
            assert!(lower.contains("accept: application/vnd.github+json"));
            assert!(lower.contains("user-agent: wt"));
            let body: Value =
                serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            let variables = body["variables"].as_object().unwrap();
            let branches = variables
                .keys()
                .filter(|key| key.starts_with("branch"))
                .count();
            batch_sizes.push(branches);
            assert!(branches <= MAX_BRANCHES_PER_BATCH);
            assert!(
                variables
                    .iter()
                    .filter(|(key, _)| key.starts_with("branch"))
                    .all(|(_, value)| value == "abcdef")
            );
            let query = body["query"].as_str().unwrap();
            assert!(query.contains("object(expression: $branch0)"));
            assert!(!query.contains("topic-0"));
        }
        batch_sizes.sort_unstable();
        assert_eq!(batch_sizes, vec![1, MAX_BRANCHES_PER_BATCH]);
    }

    #[test]
    fn partial_data_keeps_authoritative_aliases_and_classifies_failed_ones() {
        let body = serde_json::json!({
            "data": {
                "repository": {
                    "branch0": {"associatedPullRequests": {"nodes": []}}
                },
                "rateLimit": {"remaining": 10, "resetAt": "2026-07-30T12:00:00Z"}
            },
            "errors": [
                {"message": "permission denied", "path": ["repository", "branch1"]},
                {"message": "permission denied", "path": ["repository", "branch1"]}
            ]
        })
        .to_string();
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body,
        }]);
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([("origin".to_owned(), format!("{base}/team/project.git"))]),
        };
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "token".to_owned());
        let refresh = GitHubService::new().fetch_catalog_with(&git, &credentials, &[input(2)]);
        let first = refresh.branches[Path::new("/tree-0")].as_ref().unwrap();
        assert_eq!(first.warnings, vec!["permission denied"]);
        assert!(matches!(
            refresh.branches[Path::new("/tree-1")],
            Err(GitHubError::Permission(_))
        ));
        requests.recv().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn errors_only_null_repository_and_http_failures_are_classified() {
        let errors = vec!["SAML SSO authorization required".to_owned()];
        assert!(matches!(
            classify_graphql_errors(&errors),
            GitHubError::Sso(_)
        ));
        let data = serde_json::json!({"repository": null});
        assert!(matches!(
            parse_batch_data(&data, &[], &[], &errors, None, "github.com"),
            Err(GitHubError::Sso(_))
        ));
        assert_eq!(
            classify_http_error(401, "{}", None),
            GitHubError::Unauthorized
        );
        assert!(matches!(
            classify_http_error(403, r#"{"message":"classic PAT forbidden"}"#, None),
            GitHubError::ClassicPat(_)
        ));
        assert!(matches!(
            classify_http_error(403, r#"{"message":"forbidden"}"#, None),
            GitHubError::Permission(_)
        ));
        assert!(matches!(
            classify_http_error(429, r#"{"message":"rate limit exceeded"}"#, None),
            GitHubError::RateLimited { .. }
        ));
    }

    #[test]
    fn malformed_responses_and_network_failures_do_not_escape_as_panics() {
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "200 OK",
            headers: Vec::new(),
            body: "not-json".to_owned(),
        }]);
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([("origin".to_owned(), format!("{base}/team/project.git"))]),
        };
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "token".to_owned());
        let malformed = GitHubService::new().fetch_catalog_with(&git, &credentials, &[input(1)]);
        assert!(matches!(
            malformed.branches[Path::new("/tree-0")],
            Err(GitHubError::Malformed(_))
        ));
        requests.recv().unwrap();
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let dead_git = FakeGit {
            upstream: None,
            remotes: HashMap::from([(
                "origin".to_owned(),
                format!("{dead_base}/team/project.git"),
            )]),
        };
        let network = GitHubService::new().fetch_catalog_with(&dead_git, &credentials, &[input(1)]);
        assert!(matches!(
            network.branches[Path::new("/tree-0")],
            Err(GitHubError::Network(_))
        ));
    }

    #[test]
    fn exhausted_rate_limit_suppresses_follow_up_requests_until_reset() {
        let reset = epoch_seconds() + 600;
        let (base, requests, server) = fake_server(vec![FakeResponse {
            status: "403 Forbidden",
            headers: vec![
                ("X-RateLimit-Remaining".to_owned(), "0".to_owned()),
                ("X-RateLimit-Reset".to_owned(), reset.to_string()),
            ],
            body: r#"{"message":"API rate limit exceeded"}"#.to_owned(),
        }]);
        let git = FakeGit {
            upstream: None,
            remotes: HashMap::from([("origin".to_owned(), format!("{base}/team/project.git"))]),
        };
        let mut credentials = FakeCredentials::default();
        credentials
            .environment
            .insert("GITHUB_ENTERPRISE_TOKEN".to_owned(), "token".to_owned());
        let service = GitHubService::new();
        let first = service.fetch_catalog_with(&git, &credentials, &[input(1)]);
        assert!(matches!(
            first.branches[Path::new("/tree-0")],
            Err(GitHubError::RateLimited { .. })
        ));
        requests.recv().unwrap();
        server.join().unwrap();
        let second = service.fetch_catalog_with(&git, &credentials, &[input(1)]);
        assert!(matches!(
            second.branches[Path::new("/tree-0")],
            Err(GitHubError::RateLimited { .. })
        ));
    }

    #[test]
    fn normalizes_states_forks_checks_and_preference() {
        let mut node = serde_json::json!({
            "number": 4,
            "title": "fork change",
            "url": "https://example/pr/4",
            "state": "OPEN",
            "isDraft": true,
            "mergedAt": null,
            "updatedAt": "2026-01-02T00:00:00Z",
            "reviewDecision": "CHANGES_REQUESTED",
            "autoMergeRequest": {"enabledAt": "2026-01-02T01:00:00Z"},
            "baseRefName": "main",
            "baseRefOid": "baseoid",
            "baseRepository": {"nameWithOwner": "upstream/repo"},
            "headRefName": "topic",
            "headRefOid": "headoid",
            "headRepository": {"nameWithOwner": "fork/repo"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}
        });
        let draft = normalize_pull_request(&node).unwrap();
        assert_eq!(draft.state, PullRequestState::Draft);
        assert_eq!(draft.checks, CheckRollup::Failure);
        assert_eq!(draft.head.repository.as_deref(), Some("fork/repo"));
        assert_eq!(draft.base.repository.as_deref(), Some("upstream/repo"));
        assert!(draft.auto_merge);

        node["autoMergeRequest"] = Value::Null;
        assert!(!normalize_pull_request(&node).unwrap().auto_merge);

        node["isDraft"] = Value::Bool(false);
        assert_eq!(
            normalize_pull_request(&node).unwrap().state,
            PullRequestState::Open
        );
        node["state"] = Value::String("CLOSED".to_owned());
        assert_eq!(
            normalize_pull_request(&node).unwrap().state,
            PullRequestState::Closed
        );
        node["mergedAt"] = Value::String("2026-01-03T00:00:00Z".to_owned());
        assert_eq!(
            normalize_pull_request(&node).unwrap().state,
            PullRequestState::Merged
        );

        let mut merged = draft.clone();
        merged.number = 5;
        merged.state = PullRequestState::Merged;
        merged.updated_at = "2026-12-01T00:00:00Z".to_owned();
        let target = BranchTarget {
            worktree: PathBuf::from("/tree"),
            branch: "unmatched".to_owned(),
            head: Some("unmatched".to_owned()),
        };
        assert_eq!(
            prefer_pull_request(vec![merged, draft], &target)
                .unwrap()
                .number,
            4
        );
    }

    #[test]
    fn trunk_detached_and_bare_worktrees_never_request_github_data() {
        let mut catalog_input = input(3);
        catalog_input.worktrees[0].bare = true;
        catalog_input.worktrees[1].detached = true;
        catalog_input.trunk_branch = Some("topic-2".to_owned());
        let refresh = GitHubService::new().fetch_catalog_with(
            &FakeGit {
                upstream: None,
                remotes: HashMap::new(),
            },
            &FakeCredentials::default(),
            &[catalog_input],
        );
        assert!(refresh.branches.is_empty());
    }
}
