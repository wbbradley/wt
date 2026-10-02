//! Coordinates whole GitHub refreshes across processes. The stable refresh
//! sidecar is held through publication; the cache write lock is held only while
//! merging and publishing. Catalog locks must be released before entering here.
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cache::{self, CacheError, CachedBranch, CachedPullRequestDetails, RemoteCache};
use crate::git::{CommandOutput, GitError, GitRunner};
use crate::github::{
    self, AuthoredHost, AuthoredRefreshEvent, CredentialProvider, GitHubError, GitHubRefresh,
    GitHubService, RepositoryGitHubInput,
};
use crate::model::{AuthoredPullRequest, CanonicalPullRequestId, Catalog, PullRequestDetails};

const REUSE_SECONDS: u64 = 10;
const RETRY_SECONDS: u64 = 10;
const LOCK_WAIT_LIMIT: Duration = Duration::from_secs(120);

// Resolve each Git metadata command once so a remote/upstream edit between
// scope calculation and fetching cannot publish data under the wrong scope.
type MetadataValues = HashMap<(PathBuf, Vec<std::ffi::OsString>), Result<CommandOutput, String>>;
struct MetadataGit<'a> {
    source: &'a dyn GitRunner,
    values: Mutex<MetadataValues>,
}

impl GitRunner for MetadataGit<'_> {
    fn run(
        &self,
        directory: &Path,
        arguments: &[std::ffi::OsString],
    ) -> Result<CommandOutput, GitError> {
        self.values
            .lock()
            .expect("Git metadata cache poisoned")
            .entry((directory.to_owned(), arguments.to_vec()))
            .or_insert_with(|| {
                self.source
                    .run(directory, arguments)
                    .map_err(|error| error.to_string())
            })
            .clone()
            .map_err(|message| GitError::Command { message })
    }
}

pub struct RefreshLock {
    _file: File,
}

pub fn acquire_lock(
    cache_path: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<RefreshLock, CacheError> {
    let mut name = cache_path.file_name().unwrap_or_default().to_os_string();
    name.push(".refresh.lock");
    let path = cache_path.with_file_name(name);
    let lock_error = |source| CacheError::Lock {
        path: path.clone(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(lock_error)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(lock_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(lock_error)?;
    }
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err(CacheError::LockCancelled);
        }
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(RefreshLock { _file: file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= LOCK_WAIT_LIMIT {
                    return Err(lock_error(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for another wt refresh",
                    )));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(error) => return Err(lock_error(error)),
        }
    }
}

/// Memoization freezes credentials for scope matching and the requests that
/// follow, without storing tokens in the cache or resolving gh tokens repeatedly.
pub struct Credentials<'a> {
    source: &'a dyn CredentialProvider,
    values: Mutex<HashMap<(String, PathBuf, String), Option<String>>>,
}

impl<'a> Credentials<'a> {
    pub fn new(source: &'a dyn CredentialProvider) -> Self {
        Self {
            source,
            values: Mutex::new(HashMap::new()),
        }
    }
    fn value(
        &self,
        kind: &str,
        anchor: &Path,
        key: &str,
        get: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        self.values
            .lock()
            .expect("credential cache poisoned")
            .entry((kind.to_owned(), anchor.to_owned(), key.to_owned()))
            .or_insert_with(get)
            .clone()
    }
}

impl CredentialProvider for Credentials<'_> {
    fn environment(&self, key: &str) -> Option<String> {
        self.value("env", Path::new(""), key, || self.source.environment(key))
    }
    fn repository_git_config(&self, anchor: &Path, key: &str) -> Option<String> {
        self.value("git", anchor, key, || {
            self.source.repository_git_config(anchor, key)
        })
    }
    fn gh_token(&self, host: &str) -> Option<String> {
        self.value("gh", Path::new(""), host, || self.source.gh_token(host))
    }
}

pub fn hosts(catalog: &Catalog, catalog_path: &Path) -> Vec<AuthoredHost> {
    github::inferred_github_hosts(catalog)
        .into_iter()
        .map(|host| {
            let anchor = catalog
                .repositories
                .iter()
                .find(|repository| {
                    repository
                        .github_remotes
                        .values()
                        .any(|identity| identity.host == host)
                })
                .map(|repository| repository.path.clone())
                .unwrap_or_else(|| catalog_path.parent().unwrap_or(Path::new(".")).to_owned());
            AuthoredHost::inferred(&host, anchor)
        })
        .collect()
}

pub struct Scope {
    key: String,
    credentials: Vec<(String, String)>,
}

fn fingerprint(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

pub fn scope(
    runner: &dyn GitRunner,
    credentials: &dyn CredentialProvider,
    inputs: &[RepositoryGitHubInput],
    hosts: &[AuthoredHost],
    discover: bool,
) -> Result<Scope, CacheError> {
    let mut authentication = Vec::new();
    let mut credential = |host: &str, anchor: &Path| {
        let key = match github::resolve_token(credentials, host, anchor) {
            Ok(token) => fingerprint(token.expose().as_bytes()),
            Err(error) => format!("missing:{error}"),
        };
        authentication.push((host.to_owned(), key.clone()));
        key
    };
    let mut branches = Vec::new();
    for input in inputs {
        for worktree in input
            .worktrees
            .iter()
            .filter(|tree| input.refreshes_worktree(tree))
        {
            let branch = worktree.branch.as_deref().unwrap_or_default();
            let remote = github::resolve_branch_remote(
                runner,
                &input.repository,
                branch.strip_prefix("refs/heads/").unwrap_or(branch),
            );
            let remote_key = match remote {
                Ok(remote) => serde_json::to_string(&(
                    remote.identity(),
                    remote.graphql_url(),
                    credential(&remote.host, &input.repository.path),
                ))?,
                Err(error) => format!("unresolved:{error}"),
            };
            branches.push(serde_json::to_string(&(
                cache::CachedRepositoryBinding::from(&input.repository),
                &worktree.path,
                branch,
                &worktree.head,
                remote_key,
            ))?);
        }
    }
    let mut host_keys = Vec::new();
    for host in hosts {
        host_keys.push(serde_json::to_string(&(
            &host.host,
            &host.graphql_url,
            credential(&host.host, &host.credential_anchor),
        ))?);
    }
    branches.sort();
    host_keys.sort();
    authentication.sort();
    authentication.dedup();
    let key = fingerprint(&serde_json::to_vec(&(1, branches, host_keys, discover))?);
    Ok(Scope {
        key,
        credentials: authentication,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Snapshot {
    branches: Vec<CachedBranch>,
    authored_pull_requests: Vec<AuthoredPullRequest>,
    active_pull_requests: Vec<CanonicalPullRequestId>,
    pull_request_details: Vec<CachedPullRequestDetails>,
}

impl Snapshot {
    pub fn cache(&self) -> RemoteCache {
        RemoteCache {
            branches: self.branches.clone(),
            authored_pull_requests: self.authored_pull_requests.clone(),
            active_pull_requests: self.active_pull_requests.clone(),
            pull_request_details: self.pull_request_details.clone(),
            ..RemoteCache::default()
        }
    }
    pub(crate) fn from_cache(cache: RemoteCache) -> Self {
        Self {
            branches: cache.branches,
            authored_pull_requests: cache.authored_pull_requests,
            active_pull_requests: cache.active_pull_requests,
            pull_request_details: cache.pull_request_details,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Outcome {
    pub branches: GitHubRefresh,
    authored: Vec<AuthoredPullRequest>,
    pub authored_complete: bool,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    #[serde(with = "detail_results")]
    pub details: BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>>,
}

// Canonical PR identities are structured keys; JSON object keys must be strings.
mod detail_results {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        details: &BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        details.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>>, D::Error>
    {
        Ok(Vec::<(
            CanonicalPullRequestId,
            Result<PullRequestDetails, GitHubError>,
        )>::deserialize(deserializer)?
        .into_iter()
        .collect())
    }
}

impl Outcome {
    fn successful(&self) -> bool {
        self.authored_complete
            && self.branches.branches.values().all(Result::is_ok)
            && self.details.values().all(|result| {
                result.as_ref().is_ok_and(|details| {
                    details.check_contexts_complete
                        && details.reviews_complete
                        && details.feedback_complete
                })
            })
    }
    fn replay(&self, publish: &mut impl FnMut(Event)) {
        publish(Event::Branches(self.branches.clone()));
        let mut by_host = BTreeMap::<String, Vec<AuthoredPullRequest>>::new();
        for pr in &self.authored {
            by_host
                .entry(pr.identity.repository.host.clone())
                .or_default()
                .push(pr.clone());
        }
        for (host, pull_requests) in by_host {
            publish(Event::Authored(AuthoredRefreshEvent::Page {
                host,
                page: 1,
                pull_requests,
                warnings: Vec::new(),
            }));
        }
        publish(Event::Details(self.details.clone()));
        publish(Event::Authored(AuthoredRefreshEvent::Finished {
            complete: self.authored_complete,
            warnings: self.warnings.clone(),
            error: self.error.clone(),
        }));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CachedRefresh {
    scope: String,
    attempted_at: u64,
    pub successful_at: Option<u64>,
    retry_after: u64,
    pub snapshot: Snapshot,
    pub outcome: Outcome,
}

impl CachedRefresh {
    pub fn scope_id(&self) -> &str {
        &self.scope
    }

    fn reusable(&self, now: u64) -> bool {
        // Future timestamps must not make old data fresh after a clock rollback.
        self.attempted_at <= now
            && (self
                .successful_at
                .is_some_and(|at| at <= now && now - at < REUSE_SECONDS)
                || now < self.retry_after)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CachedRateLimit {
    host: String,
    credential: String,
    until: u64,
    reset_at: String,
}

pub enum Event {
    Baseline(RemoteCache),
    Branches(GitHubRefresh),
    Authored(AuthoredRefreshEvent),
    Details(BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>>),
}

pub fn select(cache: &RemoteCache, scope: &Scope) -> Option<RemoteCache> {
    cache
        .refreshes
        .iter()
        .find(|cached| cached.scope == scope.key)
        .map(|cached| {
            let mut selected = cached.snapshot.cache();
            selected.refreshes.push(cached.clone());
            selected
        })
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    path: &Path,
    service: &GitHubService,
    runner: &dyn GitRunner,
    credentials: &dyn CredentialProvider,
    inputs: &[RepositoryGitHubInput],
    hosts: &[AuthoredHost],
    discover: bool,
    cancelled: impl Fn() -> bool,
    mut publish: impl FnMut(Event),
) -> Result<(), CacheError> {
    if cancelled() {
        return Err(CacheError::LockCancelled);
    }
    // Atomic snapshots can be reused without waiting for an unrelated scope's
    // network request. A miss is always checked again under refresh ownership.
    {
        let runner = MetadataGit {
            source: runner,
            values: Mutex::new(HashMap::new()),
        };
        let credentials = Credentials::new(credentials);
        let scope = scope(&runner, &credentials, inputs, hosts, discover)?;
        let cache = read_cache(path);
        if let Some(previous) = cache
            .refreshes
            .iter()
            .find(|cached| cached.scope == scope.key)
            && previous.reusable(epoch_seconds())
        {
            publish(Event::Baseline(previous.snapshot.cache()));
            previous.outcome.replay(&mut publish);
            return Ok(());
        }
    }
    let _lock = acquire_lock(path, &cancelled)?;
    let runner = MetadataGit {
        source: runner,
        values: Mutex::new(HashMap::new()),
    };
    let credentials = Credentials::new(credentials);
    let scope = scope(&runner, &credentials, inputs, hosts, discover)?;
    // Check after acquiring the lock, so waiting panes consume the winner's publication.
    let cache = read_cache(path);
    let previous = cache
        .refreshes
        .iter()
        .find(|cached| cached.scope == scope.key);
    if let Some(previous) = previous {
        publish(Event::Baseline(previous.snapshot.cache()));
        if previous.reusable(epoch_seconds()) {
            previous.outcome.replay(&mut publish);
            return Ok(());
        }
    } else {
        publish(Event::Baseline(RemoteCache::default()));
    }
    service.clear_rate_limits();
    for limit in &cache.rate_limits {
        if limit.until > epoch_seconds()
            && scope
                .credentials
                .contains(&(limit.host.clone(), limit.credential.clone()))
        {
            service.import_rate_limit(&limit.host, limit.until, &limit.reset_at);
        }
    }
    if cancelled() {
        return Err(CacheError::LockCancelled);
    }
    let mut outcome = Outcome {
        authored_complete: !discover,
        ..Outcome::default()
    };
    outcome.branches = service.fetch_catalog_with(&runner, &credentials, inputs);
    publish(Event::Branches(outcome.branches.clone()));
    let mut identities = outcome.branches.active_pull_requests.clone();
    if cancelled() {
        return Err(CacheError::LockCancelled);
    }
    if discover {
        service.fetch_authored_with(&credentials, hosts, |event| match event {
            AuthoredRefreshEvent::Page {
                ref pull_requests, ..
            } => {
                identities.extend(pull_requests.iter().map(|pr| pr.identity.clone()));
                outcome.authored.extend(pull_requests.clone());
                publish(Event::Authored(event));
            }
            AuthoredRefreshEvent::Finished {
                complete,
                warnings,
                error,
            } => {
                outcome.authored_complete = complete;
                outcome.warnings = warnings;
                outcome.error = error;
            }
        });
    }
    if cancelled() {
        return Err(CacheError::LockCancelled);
    }
    outcome.details = service.hydrate_pull_requests_with(&credentials, hosts, identities);
    let mut retained = previous
        .map(|previous| previous.snapshot.cache())
        .unwrap_or_default();
    retained.merge_branch_refresh(inputs, &outcome.branches);
    if discover && outcome.authored_complete {
        retained.replace_authored(outcome.authored.clone());
    }
    retained.merge_pull_request_details(&outcome.details);
    let now = epoch_seconds();
    let cached = CachedRefresh {
        scope: scope.key.clone(),
        attempted_at: now,
        successful_at: outcome.successful().then_some(now),
        retry_after: if outcome.successful() {
            0
        } else {
            now.saturating_add(RETRY_SECONDS)
        },
        snapshot: Snapshot::from_cache(retained),
        outcome: outcome.clone(),
    };
    let limits = service.rate_limits();
    // Hold refresh.lock until this atomic publication finishes. Never hold the
    // cache write lock during HTTP requests or while acquiring refresh.lock.
    if let Err(error) = cache::update(path, |cache| {
        cache.merge_branch_refresh(inputs, &outcome.branches);
        if discover && outcome.authored_complete {
            cache.replace_authored(outcome.authored.clone());
        }
        cache.merge_pull_request_details(&outcome.details);
        cache.refreshes.retain(|entry| entry.scope != scope.key);
        cache.refreshes.push(cached);
        // Bound snapshot growth for changing HEADs, scopes, and accounts.
        if cache.refreshes.len() > 32 {
            cache.refreshes.remove(0);
        }
        cache.rate_limits.retain(|limit| limit.until > now);
        for (host, until, reset_at) in limits {
            for (_, credential) in scope.credentials.iter().filter(|(name, _)| name == &host) {
                cache
                    .rate_limits
                    .retain(|limit| limit.host != host || limit.credential != *credential);
                cache.rate_limits.push(CachedRateLimit {
                    host: host.clone(),
                    credential: credential.clone(),
                    until,
                    reset_at: reset_at.clone(),
                });
            }
        }
    }) {
        outcome
            .warnings
            .push(format!("unable to persist remote cache: {error}"));
    }
    publish(Event::Details(outcome.details));
    publish(Event::Authored(AuthoredRefreshEvent::Finished {
        complete: outcome.authored_complete,
        warnings: outcome.warnings,
        error: outcome.error,
    }));
    Ok(())
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_cache(path: &Path) -> RemoteCache {
    cache::load(path).unwrap_or_else(|error| {
        // Keep fetching when the cache is unusable. update() still refuses to
        // overwrite a future schema, and its error is reported to the UI.
        tracing::warn!(%error, "remote cache ignored before refresh");
        RemoteCache::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{CommandOutput, GitError};
    use crate::model::{RepositoryConfig, Worktree};
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Git(String);
    impl GitRunner for Git {
        fn run(&self, _: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError> {
            let success = arguments.first().is_some_and(|arg| arg == "remote");
            Ok(CommandOutput {
                stdout: if success {
                    format!("{}/team/project.git\n", self.0).into_bytes()
                } else {
                    Vec::new()
                },
                stderr: Vec::new(),
                success,
                exit_code: None,
            })
        }
    }
    struct Token(&'static str);
    impl CredentialProvider for Token {
        fn environment(&self, _: &str) -> Option<String> {
            Some(self.0.to_owned())
        }
        fn repository_git_config(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
        fn gh_token(&self, _: &str) -> Option<String> {
            None
        }
    }
    fn input(path: &Path) -> RepositoryGitHubInput {
        RepositoryGitHubInput {
            repository: RepositoryConfig {
                path: path.to_owned(),
                label: None,
                worktree_root: None,
                github_remote: Some("origin".to_owned()),
                github_remotes: BTreeMap::new(),
                github_preferred_remote: None,
            },
            worktrees: vec![Worktree {
                path: path.join("topic"),
                head: Some("head-1".to_owned()),
                branch: Some("refs/heads/topic".to_owned()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            }],
            trunk_branch: None,
        }
    }
    fn host(base: &str, path: &Path) -> AuthoredHost {
        AuthoredHost {
            host: base.strip_prefix("http://").unwrap().to_owned(),
            graphql_url: format!("{base}/api/graphql"),
            credential_anchor: path.to_owned(),
        }
    }
    struct Server {
        base: String,
        count: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Server {
        fn new(status: &'static str, headers: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let count = Arc::new(AtomicUsize::new(0));
            let stopped = Arc::new(AtomicBool::new(false));
            let (count_worker, stop_worker) = (count.clone(), stopped.clone());
            let thread = std::thread::spawn(move || {
                while !stop_worker.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            let request = read_request(&mut stream);
                            count_worker.fetch_add(1, Ordering::SeqCst);
                            let body = if status != "200 OK" {
                                r#"{"message":"API rate limit exceeded"}"#.to_owned()
                            } else if request.contains("search(") {
                                serde_json::json!({"data": {"viewer": {"login": "viewer"}, "search": {
                                        "issueCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []
                                    }}}).to_string()
                            } else {
                                serde_json::json!({"data": {"repository": {"branch0": {
                                    "associatedPullRequests": {"nodes": []}
                                }}}})
                                .to_string()
                            };
                            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).unwrap();
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("test server: {error}"),
                    }
                }
            });
            Self {
                base,
                count,
                stopped,
                thread: Some(thread),
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            data.extend_from_slice(&buffer[..count]);
            if let Some(end) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&data[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if data.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8(data).unwrap()
    }
    fn fetch(
        path: &Path,
        base: &str,
        inputs: &[RepositoryGitHubInput],
        token: &'static str,
        discover: bool,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        run(
            path,
            &GitHubService::new(),
            &Git(base.to_owned()),
            &Token(token),
            inputs,
            &[host(base, path.parent().unwrap())],
            discover,
            || false,
            |event| events.push(event),
        )
        .unwrap();
        events
    }

    #[test]
    fn subprocess_refresh_worker() {
        let Ok(path) = std::env::var("WT_TEST_SHARED_REFRESH_CACHE") else {
            return;
        };
        let path = PathBuf::from(path);
        let base = std::env::var("WT_TEST_SHARED_REFRESH_SERVER").unwrap();
        if let Ok(ready) = std::env::var("WT_TEST_SHARED_REFRESH_READY") {
            fs::write(ready, "ready").unwrap();
        }
        if std::env::var_os("WT_TEST_SHARED_REFRESH_HOLD").is_some() {
            let _lock = acquire_lock(&path, || false).unwrap();
            fs::write(
                std::env::var("WT_TEST_SHARED_REFRESH_LOCKED").unwrap(),
                "locked",
            )
            .unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let inputs = [input(path.parent().unwrap())];
        let events = fetch(&path, &base, &inputs, "test-secret", true);
        assert!(events.iter().any(|event| matches!(event, Event::Branches(refresh) if refresh.branches.values().all(Result::is_ok))));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Authored(AuthoredRefreshEvent::Finished { complete: true, .. })
        )));
    }

    fn child(path: &Path, base: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "refresh::tests::subprocess_refresh_worker",
                "--nocapture",
            ])
            .env("WT_TEST_SHARED_REFRESH_CACHE", path)
            .env("WT_TEST_SHARED_REFRESH_SERVER", base)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for test worker"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn simultaneous_processes_share_branch_and_discovery_requests() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        let lock = acquire_lock(&path, || false).unwrap();
        let mut children = Vec::new();
        for number in 0..3 {
            let ready = directory.path().join(format!("ready-{number}"));
            let mut command = child(&path, &server.base);
            command.env("WT_TEST_SHARED_REFRESH_READY", &ready);
            children.push(command.spawn().unwrap());
            wait_until(|| ready.exists());
        }
        // Refresh ownership must not block unrelated cache writes.
        cache::update(&path, |cache| {
            cache.updated_at_epoch_seconds = 123;
        })
        .unwrap();
        drop(lock);
        for mut process in children {
            let mut status = None;
            wait_until(|| {
                status = process.try_wait().unwrap();
                status.is_some()
            });
            assert!(status.unwrap().success());
        }
        assert_eq!(
            server.count.load(Ordering::SeqCst),
            3,
            "one branch batch plus authored and assigned searches"
        );
        let cache = cache::load(&path).unwrap();
        assert_eq!(cache.refreshes.len(), 1);
        assert!(cache.refreshes[0].successful_at.is_some());
        assert!(!fs::read_to_string(&path).unwrap().contains("test-secret"));
    }

    #[test]
    fn fresh_snapshot_does_not_wait_for_an_unrelated_refresh_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        fetch(
            &path,
            &server.base,
            &[input(directory.path())],
            "test-secret",
            true,
        );
        let _lock = acquire_lock(&path, || false).unwrap();
        let mut process = child(&path, &server.base).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = process.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                process.kill().unwrap();
                process.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.is_some_and(|status| status.success()),
            "fresh cache should bypass network ownership"
        );
        assert_eq!(server.count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn unusable_cache_still_fetches_and_preserves_a_future_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        let inputs = [input(directory.path())];
        let future = r#"{"version":999}"#;
        fs::write(&path, future).unwrap();
        let events = fetch(&path, &server.base, &inputs, "test-secret", false);
        assert!(events.iter().any(|event| matches!(event, Event::Branches(refresh) if refresh.branches.values().all(Result::is_ok))));
        assert_eq!(fs::read_to_string(&path).unwrap(), future);
        fs::write(&path, "invalid json").unwrap();
        fetch(&path, &server.base, &inputs, "test-secret", false);
        assert_eq!(server.count.load(Ordering::SeqCst), 2);
        assert_eq!(cache::load(&path).unwrap().refreshes.len(), 1);
    }

    #[test]
    fn stale_scope_fetches_but_local_writes_cannot_extend_freshness() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        let inputs = [input(directory.path())];
        fetch(&path, &server.base, &inputs, "test-secret", false);
        cache::update(&path, |cache| {
            let old = epoch_seconds() - REUSE_SECONDS - 1;
            cache.refreshes[0].successful_at = Some(old);
            cache.refreshes[0].attempted_at = old;
            cache.record_created_worktree(
                &inputs[0].repository,
                &directory.path().join("new"),
                "new",
            );
        })
        .unwrap();
        fetch(&path, &server.base, &inputs, "test-secret", false);
        assert_eq!(server.count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_requests_share_backoff_without_claiming_success() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("500 Internal Server Error", String::new());
        let inputs = [input(directory.path())];
        for _ in 0..3 {
            let events = fetch(&path, &server.base, &inputs, "test-secret", false);
            assert!(events.iter().any(|event| matches!(event, Event::Branches(refresh) if refresh.branches.values().all(Result::is_err))));
        }
        assert_eq!(server.count.load(Ordering::SeqCst), 1);
        let cache = cache::load(&path).unwrap();
        assert!(cache.refreshes[0].successful_at.is_none());
        assert!(cache.refreshes[0].retry_after > epoch_seconds());
    }

    #[test]
    fn rate_limit_is_shared_across_scopes_and_process_local_services() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new(
            "403 Forbidden",
            format!(
                "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}\r\n",
                epoch_seconds() + 600
            ),
        );
        let mut inputs = [input(directory.path())];
        fetch(&path, &server.base, &inputs, "test-secret", false);
        inputs[0].worktrees[0].head = Some("head-2".to_owned());
        fetch(&path, &server.base, &inputs, "test-secret", false);
        assert_eq!(server.count.load(Ordering::SeqCst), 1);
        assert_eq!(cache::load(&path).unwrap().refreshes.len(), 2);
        // A different token has its own rate-limit budget.
        fetch(&path, &server.base, &inputs, "different-token", false);
        assert_eq!(server.count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn scope_matches_head_hosts_credentials_and_requested_discovery() {
        let inputs = [input(Path::new("/repo"))];
        let git = Git("http://localhost:9999".to_owned());
        let hosts = [host(&git.0, Path::new("/repo"))];
        let original = scope(&git, &Token("one"), &inputs, &hosts, true)
            .unwrap()
            .key;
        assert_ne!(
            original,
            scope(&git, &Token("two"), &inputs, &hosts, true)
                .unwrap()
                .key
        );
        assert_ne!(
            original,
            scope(&git, &Token("one"), &inputs, &hosts, false)
                .unwrap()
                .key
        );
        let mut changed = inputs.clone();
        changed[0].worktrees[0].head = Some("other".to_owned());
        assert_ne!(
            original,
            scope(&git, &Token("one"), &changed, &hosts, true)
                .unwrap()
                .key
        );
        let mut changed_hosts = hosts.clone();
        changed_hosts[0].graphql_url = "http://localhost:9998/api/graphql".to_owned();
        assert_ne!(
            original,
            scope(&git, &Token("one"), &inputs, &changed_hosts, true)
                .unwrap()
                .key
        );
    }

    #[test]
    fn lock_wait_is_cancellable_and_dead_owner_releases_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let locked = directory.path().join("locked");
        let mut command = child(&path, "http://localhost:9999");
        let mut process = command
            .env("WT_TEST_SHARED_REFRESH_HOLD", "1")
            .env("WT_TEST_SHARED_REFRESH_LOCKED", &locked)
            .spawn()
            .unwrap();
        wait_until(|| locked.exists());
        assert!(matches!(
            acquire_lock(&path, || true),
            Err(CacheError::LockCancelled)
        ));
        process.kill().unwrap();
        process.wait().unwrap();
        assert!(acquire_lock(&path, || false).is_ok());
    }

    #[test]
    fn failed_refresh_retains_last_good_data_and_replays_stale_result() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        let base = server.base.clone();
        let inputs = [input(directory.path())];
        fetch(&path, &base, &inputs, "test-secret", false);
        cache::update(&path, |cache| {
            let old = epoch_seconds() - REUSE_SECONDS - 1;
            cache.refreshes[0].successful_at = Some(old);
            cache.refreshes[0].attempted_at = old;
        })
        .unwrap();
        drop(server);
        fetch(&path, &base, &inputs, "test-secret", false);
        let cache = cache::load(&path).unwrap();
        let cached = &cache.refreshes[0];
        assert!(cached.successful_at.is_none());
        assert_eq!(cached.snapshot.branches.len(), 1);
        assert!(
            cached
                .outcome
                .branches
                .branches
                .values()
                .all(Result::is_err)
        );
        let events = fetch(&path, &base, &inputs, "test-secret", false);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Baseline(cache) if cache.branches.len() == 1))
        );
        assert!(events.iter().any(|event| matches!(event, Event::Branches(refresh) if refresh.branches.values().all(Result::is_err))));
    }

    #[test]
    fn detail_results_round_trip_and_replay_without_a_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("github.json");
        let server = Server::new("200 OK", String::new());
        let inputs = [input(directory.path())];
        fetch(&path, &server.base, &inputs, "test-secret", false);
        let identity = CanonicalPullRequestId {
            repository: crate::model::GitHubRepositoryIdentity::canonical(
                "github.com",
                "team",
                "project",
            ),
            number: 1,
        };
        let details = PullRequestDetails {
            check_contexts_complete: true,
            reviews_complete: true,
            feedback_complete: true,
            ..PullRequestDetails::default()
        };
        cache::update(&path, |cache| {
            let cached = &mut cache.refreshes[0];
            cached
                .snapshot
                .pull_request_details
                .push(CachedPullRequestDetails {
                    identity: identity.clone(),
                    details: details.clone(),
                });
            cached
                .outcome
                .details
                .insert(identity.clone(), Ok(details.clone()));
        })
        .unwrap();
        let events = fetch(&path, &server.base, &inputs, "test-secret", false);
        assert!(events.iter().any(|event| matches!(event, Event::Details(results) if results.get(&identity) == Some(&Ok(details.clone())))));
        assert_eq!(server.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn upstream_metadata_is_frozen_between_scope_matching_and_fetch() {
        struct ChangingGit {
            first: String,
            second: String,
            calls: AtomicUsize,
        }
        impl GitRunner for ChangingGit {
            fn run(&self, _: &Path, args: &[OsString]) -> Result<CommandOutput, GitError> {
                let success = args.first().is_some_and(|arg| arg == "remote");
                // The optimistic scope check and locked scope check see the
                // original remote; a third resolution would see an edit.
                let base = if success && self.calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    &self.first
                } else {
                    &self.second
                };
                Ok(CommandOutput {
                    stdout: if success {
                        format!("{base}/team/project.git\n").into_bytes()
                    } else {
                        Vec::new()
                    },
                    stderr: Vec::new(),
                    success,
                    exit_code: None,
                })
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let first = Server::new("200 OK", String::new());
        let second = Server::new("200 OK", String::new());
        let git = ChangingGit {
            first: first.base.clone(),
            second: second.base.clone(),
            calls: AtomicUsize::new(0),
        };
        run(
            &directory.path().join("github.json"),
            &GitHubService::new(),
            &git,
            &Token("test-secret"),
            &[input(directory.path())],
            &[],
            false,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(first.count.load(Ordering::SeqCst), 1);
        assert_eq!(second.count.load(Ordering::SeqCst), 0);
        assert_eq!(git.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clock_rollback_and_partial_details_never_count_as_fresh_success() {
        let now = epoch_seconds();
        let future = CachedRefresh {
            scope: String::new(),
            attempted_at: now + 20,
            successful_at: Some(now + 20),
            retry_after: 0,
            snapshot: Snapshot::default(),
            outcome: Outcome::default(),
        };
        assert!(!future.reusable(now));
        let mut partial = Outcome {
            authored_complete: true,
            ..Outcome::default()
        };
        partial.details.insert(
            CanonicalPullRequestId {
                repository: crate::model::GitHubRepositoryIdentity::canonical(
                    "github.com",
                    "team",
                    "project",
                ),
                number: 1,
            },
            Ok(PullRequestDetails::default()),
        );
        assert!(!partial.successful());
    }
}
