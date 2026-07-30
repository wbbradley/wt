use std::collections::{HashMap, HashSet};
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
    CheckRollup, GitHubBranchData, PullRequest, PullRequestIdentity, PullRequestState, RateLimit,
    RepositoryConfig, Worktree,
};

pub const MAX_BRANCHES_PER_BATCH: usize = 20;

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
    fn expose(&self) -> &str {
        &self.token.0
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
}

#[derive(Clone, Debug)]
pub struct RepositoryGitHubInput {
    pub repository: RepositoryConfig,
    pub worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug, Default)]
pub struct GitHubRefresh {
    pub branches: HashMap<PathBuf, Result<GitHubBranchData, GitHubError>>,
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
}

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
            for worktree in &input.worktrees {
                if worktree.bare || worktree.detached {
                    continue;
                }
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
                    Ok(outcomes) => {
                        for (target, outcome) in chunk.iter().zip(outcomes) {
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

    fn fetch_batch(
        &self,
        remote: &RemoteRepository,
        token: &ResolvedToken,
        targets: &[BranchTarget],
    ) -> Result<Vec<Result<GitHubBranchData, GitHubError>>, GitHubError> {
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
        parse_batch_data(&data, targets, &envelope.errors, &warnings, rate)
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
            Value::String(format!("refs/heads/{}", target.branch)),
        );
        aliases.push_str(&format!(
            r#"
            branch{index}: ref(qualifiedName: $branch{index}) {{
              target {{
                ... on Commit {{
                  associatedPullRequests(first: 20) {{
                    nodes {{
                      number title url state isDraft mergedAt updatedAt reviewDecision
                      baseRefName baseRefOid baseRepository {{ nameWithOwner }}
                      headRefName headRefOid headRepository {{ nameWithOwner }}
                      commits(last: 1) {{ nodes {{ commit {{ oid statusCheckRollup {{ state }} }} }} }}
                    }}
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
) -> Result<Vec<Result<GitHubBranchData, GitHubError>>, GitHubError> {
    let repository = match data.get("repository").and_then(Value::as_object) {
        Some(repository) => repository,
        None if !warnings.is_empty() => return Err(classify_graphql_errors(warnings)),
        None => return Err(GitHubError::Malformed("missing repository data".to_owned())),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let alias = format!("branch{index}");
        let alias_errors: Vec<String> = errors
            .iter()
            .filter(|error| error.path.iter().any(|part| part.as_str() == Some(&alias)))
            .map(|error| error.message.clone())
            .collect();
        let Some(reference) = repository.get(&alias) else {
            outcomes.push(Err(classify_graphql_errors(if alias_errors.is_empty() {
                warnings
            } else {
                &alias_errors
            })));
            continue;
        };
        if reference.is_null() {
            outcomes.push(Err(if alias_errors.is_empty() {
                GitHubError::BranchNotFound(target.branch.clone())
            } else {
                classify_graphql_errors(&alias_errors)
            }));
            continue;
        }
        let nodes = reference
            .pointer("/target/associatedPullRequests/nodes")
            .and_then(Value::as_array);
        let Some(nodes) = nodes else {
            outcomes.push(Err(if alias_errors.is_empty() {
                GitHubError::Malformed(format!(
                    "missing associated pull requests for {}",
                    target.branch
                ))
            } else {
                classify_graphql_errors(&alias_errors)
            }));
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
            outcomes.push(Err(GitHubError::Malformed(malformed.join("; "))));
            continue;
        }
        let pull_request = prefer_pull_request(pull_requests);
        let mut branch_warnings = warnings.to_vec();
        branch_warnings.extend(malformed);
        outcomes.push(Ok(GitHubBranchData {
            pull_request,
            warnings: deduplicate(branch_warnings),
            rate_limit: rate.clone(),
        }));
    }
    Ok(outcomes)
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

fn prefer_pull_request(mut pull_requests: Vec<PullRequest>) -> Option<PullRequest> {
    pull_requests.sort_by(|left, right| {
        let priority = |pull_request: &PullRequest| {
            if matches!(
                pull_request.state,
                PullRequestState::Open | PullRequestState::Draft
            ) {
                0
            } else {
                1
            }
        };
        priority(left)
            .cmp(&priority(right))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
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
            let value = if arguments
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
                        "target": {"associatedPullRequests": {"nodes": []}}
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

    fn input(branches: usize) -> RepositoryGitHubInput {
        RepositoryGitHubInput {
            repository: RepositoryConfig {
                path: PathBuf::from("/repo"),
                label: None,
                worktree_root: None,
                github_remote: None,
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
        }
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
            assert!(!body["query"].as_str().unwrap().contains("topic-0"));
        }
        batch_sizes.sort_unstable();
        assert_eq!(batch_sizes, vec![1, MAX_BRANCHES_PER_BATCH]);
    }

    #[test]
    fn partial_data_keeps_authoritative_aliases_and_classifies_failed_ones() {
        let body = serde_json::json!({
            "data": {
                "repository": {
                    "branch0": {"target": {"associatedPullRequests": {"nodes": []}}}
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
            parse_batch_data(&data, &[], &[], &errors, None),
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
        assert_eq!(prefer_pull_request(vec![merged, draft]).unwrap().number, 4);
    }

    #[test]
    fn detached_and_bare_worktrees_never_request_github_data() {
        let mut catalog_input = input(2);
        catalog_input.worktrees[0].bare = true;
        catalog_input.worktrees[1].detached = true;
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
