use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::app::StatusUpdate;
use crate::bootstrap::{CloneOutput, CloneRequest, CloneRunner};
use crate::git::{self, SystemGit};
use crate::github::ResolvedToken;
use crate::materialize::{FetchOutput, FetchRequest, FetchRunner};
use crate::model::WorktreeStatus;

#[derive(Clone, Debug)]
pub struct StatusTask {
    pub generation: u64,
    pub path: PathBuf,
    pub ignored_files: Vec<PathBuf>,
}

type StatusLoader =
    dyn Fn(&Path, &[PathBuf]) -> Result<WorktreeStatus, String> + Send + Sync + 'static;

pub struct StatusPool {
    sender: Option<SyncSender<StatusTask>>,
    receiver: Receiver<StatusUpdate>,
    workers: Vec<JoinHandle<()>>,
}

impl StatusPool {
    pub fn with_git(worker_count: usize) -> Self {
        Self::new(
            worker_count,
            Arc::new(|path, configured| {
                git::status_with_ignored(&SystemGit, path, configured)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    pub fn new(worker_count: usize, loader: Arc<StatusLoader>) -> Self {
        let worker_count = worker_count.clamp(1, 8);
        let (task_sender, task_receiver) = mpsc::sync_channel::<StatusTask>(worker_count * 2);
        let (result_sender, result_receiver) = mpsc::channel();
        let shared_receiver = Arc::new(Mutex::new(task_receiver));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let receiver = Arc::clone(&shared_receiver);
            let sender = result_sender.clone();
            let loader = Arc::clone(&loader);
            workers.push(
                thread::Builder::new()
                    .name(format!("wt-status-worker-{index}"))
                    .spawn(move || {
                        loop {
                            let task = {
                                let receiver = receiver.lock().expect("status queue lock poisoned");
                                receiver.recv()
                            };
                            let Ok(task) = task else {
                                break;
                            };
                            let result = loader(&task.path, &task.ignored_files);
                            if sender
                                .send(StatusUpdate {
                                    generation: task.generation,
                                    path: task.path,
                                    result,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .expect("failed to spawn status worker"),
            );
        }
        drop(result_sender);
        Self {
            sender: Some(task_sender),
            receiver: result_receiver,
            workers,
        }
    }

    pub fn try_submit(&self, task: StatusTask) -> Result<(), StatusTask> {
        match self
            .sender
            .as_ref()
            .expect("status pool is active")
            .try_send(task)
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(task) | TrySendError::Disconnected(task)) => Err(task),
        }
    }

    pub fn try_recv(&self) -> Option<StatusUpdate> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for StatusPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    Cancelled,
    Failed(String),
}

/// A phase names the coarse step a job is in; a detail is one line of the
/// underlying command's own progress output, reported inside that phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgressUpdate {
    Phase(String),
    Detail(String),
}

pub enum JobMessage<T> {
    Progress(ProgressUpdate),
    Finished(Result<T, JobError>),
}

pub struct BackgroundJob<T> {
    cancelled: Arc<AtomicBool>,
    progress_receiver: Receiver<ProgressUpdate>,
    result_receiver: Receiver<Result<T, JobError>>,
    worker: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> BackgroundJob<T> {
    pub fn spawn(
        name: &str,
        task: impl FnOnce(JobContext) -> Result<T, String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (progress_sender, progress_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let context = JobContext {
            cancelled: Arc::clone(&cancelled),
            progress_sender,
        };
        let worker = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let result = task(context.clone());
                let result = if context.is_cancelled() {
                    Err(JobError::Cancelled)
                } else {
                    result.map_err(JobError::Failed)
                };
                let _ = result_sender.send(result);
            })?;
        Ok(Self {
            cancelled,
            progress_receiver,
            result_receiver,
            worker: Some(worker),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn try_recv(&self) -> Option<JobMessage<T>> {
        self.progress_receiver
            .try_recv()
            .map(JobMessage::Progress)
            .ok()
            .or_else(|| {
                self.result_receiver
                    .try_recv()
                    .map(JobMessage::Finished)
                    .ok()
            })
    }

    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl<T> Drop for BackgroundJob<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct JobContext {
    cancelled: Arc<AtomicBool>,
    progress_sender: mpsc::Sender<ProgressUpdate>,
}

impl JobContext {
    pub fn progress(&self, message: impl Into<String>) {
        let _ = self
            .progress_sender
            .send(ProgressUpdate::Phase(message.into()));
    }

    /// Reports one line of the running command's own progress output. Details
    /// refine the current phase rather than replacing it.
    pub fn detail(&self, message: impl Into<String>) {
        let _ = self
            .progress_sender
            .send(ProgressUpdate::Detail(message.into()));
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Builds a runner that scrubs `token` out of every stderr line before it
    /// is published as progress. Fetch and clone stderr can echo the URL or the
    /// credential itself, so a runner that streams progress must know the
    /// secret in order to hide it.
    pub fn git_runner(&self, token: Option<&ResolvedToken>) -> CancellableGitRunner {
        CancellableGitRunner {
            executable: OsString::from("git"),
            context: self.clone(),
            secret: token.map(|token| token.expose().to_owned()),
        }
    }
}

#[derive(Clone)]
pub struct CancellableGitRunner {
    executable: OsString,
    context: JobContext,
    secret: Option<String>,
}

impl CancellableGitRunner {
    #[cfg(test)]
    fn with_executable(context: JobContext, executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            context,
            secret: None,
        }
    }

    #[cfg(test)]
    fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
        self
    }

    fn execute(
        &self,
        directory: Option<&Path>,
        arguments: &[OsString],
        environment: &[(OsString, OsString)],
    ) -> Result<ProcessOutput, std::io::Error> {
        let mut command = Command::new(&self.executable);
        if let Some(directory) = directory {
            command.arg("-C").arg(directory);
        }
        command
            .args(arguments)
            .envs(environment.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout is available");
        let stderr = child.stderr.take().expect("piped stderr is available");
        let stdout_reader = thread::spawn(move || read_pipe(stdout));
        // Only stderr is streamed. Stdout is parsed by callers (porcelain
        // listings, rev-parse output) and must stay whole and untouched.
        let stderr_reader = {
            let context = self.context.clone();
            let secret = self.secret.clone();
            thread::spawn(move || {
                read_pipe_lines(stderr, |line| {
                    context.detail(git::redact_secret(line, secret.as_deref()));
                })
            })
        };
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if self.context.is_cancelled() {
                terminate_child(&mut child);
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Git operation cancelled",
                ));
            }
            thread::sleep(Duration::from_millis(20));
        };
        let stdout = join_reader(stdout_reader)?;
        let stderr = join_reader(stderr_reader)?;
        Ok(ProcessOutput {
            stdout,
            stderr,
            success: status.success(),
        })
    }

    fn progress_for_git(&self, arguments: &[OsString]) {
        let argument = |index| arguments.get(index).map(OsString::as_os_str);
        if argument(0) == Some(OsStr::new("branch"))
            || (argument(0) == Some(OsStr::new("config"))
                && arguments
                    .iter()
                    .any(|value| value.to_string_lossy().contains(".wt-pr")))
        {
            self.context.progress("preparing pull request branch");
        } else if argument(0) == Some(OsStr::new("checkout")) {
            self.context.progress("checking out files");
        } else if argument(0) == Some(OsStr::new("worktree")) {
            match argument(1) {
                Some(value) if value == OsStr::new("add") => {
                    self.context.progress("creating linked worktree")
                }
                Some(value) if value == OsStr::new("move") => {
                    self.context.progress("moving worktree into place")
                }
                Some(value)
                    if value == OsStr::new("unlock")
                        || value == OsStr::new("remove")
                        || value == OsStr::new("prune") =>
                {
                    self.context.progress("clearing interrupted worktrees")
                }
                _ => {}
            }
        }
    }
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child starts its own process group, so this also terminates Git's
        // SSH/credential helpers and closes every inherited output pipe.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    success: bool,
}

fn read_pipe(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads `pipe` incrementally, handing each completed line to `sink` as it
/// arrives while still accumulating the complete byte stream for the caller.
///
/// Lines end at either `\n` or `\r`: Git redraws progress in place with
/// carriage returns, so a reader that only honors `\n` would see one enormous
/// line and emit nothing until the phase ended.
fn read_pipe_lines(mut pipe: impl Read, mut sink: impl FnMut(&str)) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    let flush = |line: &mut Vec<u8>, sink: &mut dyn FnMut(&str)| {
        if line.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if !text.is_empty() {
            sink(text);
        }
        line.clear();
    };
    loop {
        let read = pipe.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        for &byte in &chunk[..read] {
            if byte == b'\n' || byte == b'\r' {
                flush(&mut line, &mut sink);
            } else {
                line.push(byte);
            }
        }
    }
    flush(&mut line, &mut sink);
    Ok(bytes)
}

fn join_reader(reader: JoinHandle<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>, std::io::Error> {
    reader
        .join()
        .map_err(|_| std::io::Error::other("Git output reader panicked"))?
}

impl git::GitRunner for CancellableGitRunner {
    fn run(
        &self,
        directory: &Path,
        arguments: &[OsString],
    ) -> Result<git::CommandOutput, git::GitError> {
        self.progress_for_git(arguments);
        let output = self
            .execute(Some(directory), arguments, &[])
            .map_err(|source| git::GitError::Launch { source })?;
        Ok(git::CommandOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            success: output.success,
            exit_code: None,
        })
    }
}

impl CloneRunner for CancellableGitRunner {
    fn run(&self, request: &CloneRequest) -> Result<CloneOutput, std::io::Error> {
        self.context.progress("cloning repository");
        let output = self.execute(None, &request.arguments, &request.environment)?;
        Ok(CloneOutput {
            success: output.success,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl FetchRunner for CancellableGitRunner {
    fn run(
        &self,
        repository: &Path,
        request: &FetchRequest,
    ) -> Result<FetchOutput, std::io::Error> {
        self.context.progress("fetching pull request head");
        let output = self.execute(Some(repository), &request.arguments, &request.environment)?;
        Ok(FetchOutput {
            success: output.success,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn worker_pool_is_bounded_and_preserves_generation_ids() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let loader = {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            Arc::new(move |_path: &Path, _configured: &[PathBuf]| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(WorktreeStatus::default())
            })
        };
        let pool = StatusPool::new(2, loader);
        let mut pending: Vec<StatusTask> = (0..8)
            .map(|index| StatusTask {
                generation: 42,
                ignored_files: Vec::new(),
                path: PathBuf::from(format!("/{index}")),
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut results = Vec::new();
        while results.len() < 8 && Instant::now() < deadline {
            let mut remaining = Vec::new();
            for task in pending.drain(..) {
                if let Err(task) = pool.try_submit(task) {
                    remaining.push(task);
                }
            }
            pending = remaining;
            while let Some(result) = pool.try_recv() {
                results.push(result);
            }
            thread::yield_now();
        }
        assert_eq!(results.len(), 8);
        assert!(results.iter().all(|result| result.generation == 42));
        assert!(maximum.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn background_job_starts_without_blocking_and_cancels_lock_wait() {
        let directory = tempfile::tempdir().unwrap();
        let catalog_path = directory.path().join("wt.json");
        let _held_lock = crate::config::acquire_catalog_lock(&catalog_path).unwrap();
        let started = Instant::now();
        let mut job = BackgroundJob::<()>::spawn("lock-wait-test", move |context| {
            let _lock = crate::config::acquire_catalog_lock_with(
                &catalog_path,
                || context.is_cancelled(),
                || context.progress("waiting for catalog lock"),
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_waiting = false;
        while Instant::now() < deadline && !saw_waiting {
            saw_waiting = matches!(
                job.try_recv(),
                Some(JobMessage::Progress(ProgressUpdate::Phase(message)))
                    if message == "waiting for catalog lock"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_waiting);
        job.cancel();
        let result = loop {
            assert!(Instant::now() < deadline, "job did not cancel lock wait");
            if let Some(JobMessage::Finished(result)) = job.try_recv() {
                break result;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(result, Err(JobError::Cancelled));
        job.join();
    }

    #[cfg(unix)]
    fn executable_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, body).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    /// Collects every published detail line, reporting whether the job also
    /// finished; the result message shares the queue and must not be dropped.
    fn drain_details(job: &BackgroundJob<()>, details: &mut Vec<String>) -> bool {
        let mut finished = false;
        while let Some(message) = job.try_recv() {
            match message {
                JobMessage::Progress(ProgressUpdate::Detail(detail)) => details.push(detail),
                JobMessage::Progress(ProgressUpdate::Phase(_)) => {}
                JobMessage::Finished(_) => finished = true,
            }
        }
        finished
    }

    /// Git redraws progress with carriage returns and only flushes stderr as it
    /// goes, so the runner has to publish lines while the child is still
    /// running rather than after it exits.
    #[cfg(unix)]
    #[test]
    fn stderr_progress_lines_are_published_before_the_child_exits() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("progress-git");
        executable_script(
            &script,
            "#!/bin/sh\nprintf 'Updating files:  20%%\\rUpdating files:  61%%\\r' >&2\nsleep 30\n",
        );
        let work = directory.path().to_owned();
        let executable = script.into_os_string();
        let mut job = BackgroundJob::<()>::spawn("stream-progress-test", move |context| {
            let runner = CancellableGitRunner::with_executable(context, executable);
            crate::git::GitRunner::run(&runner, &work, &[OsString::from("checkout")])
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut details = Vec::new();
        while !details
            .iter()
            .any(|detail| detail == "Updating files:  61%")
        {
            assert!(
                Instant::now() < deadline,
                "no progress arrived while the child was running: {details:?}"
            );
            drain_details(&job, &mut details);
            thread::sleep(Duration::from_millis(10));
        }
        // Both carriage-return-separated redraws surfaced as their own lines.
        assert!(
            details
                .iter()
                .any(|detail| detail == "Updating files:  20%")
        );
        job.cancel();
        job.join();
    }

    /// Streamed stderr can echo the HTTPS credential, so every published line
    /// goes through redaction while the error path keeps the complete text.
    #[cfg(unix)]
    #[test]
    fn published_progress_lines_are_redacted_but_stderr_stays_complete() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("leaky-git");
        executable_script(
            &script,
            "#!/bin/sh\nprintf 'remote: using recognizable-secret\\n' >&2\nprintf 'header eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA==\\n' >&2\nprintf 'fatal: no\\n' >&2\nexit 1\n",
        );
        let work = directory.path().to_owned();
        let executable = script.into_os_string();
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&captured);
        let mut job = BackgroundJob::<()>::spawn("redaction-test", move |context| {
            let runner = CancellableGitRunner::with_executable(context, executable)
                .with_secret("recognizable-secret");
            let output = crate::git::GitRunner::run(&runner, &work, &[OsString::from("fetch")])
                .map_err(|error| error.to_string())?;
            *sink.lock().unwrap() = Some(String::from_utf8_lossy(&output.stderr).into_owned());
            Ok(())
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut details = Vec::new();
        while !drain_details(&job, &mut details) {
            assert!(Instant::now() < deadline, "job never finished");
            thread::sleep(Duration::from_millis(10));
        }
        job.join();
        assert!(!details.is_empty());
        for detail in &details {
            assert!(!detail.contains("recognizable-secret"), "{detail}");
            assert!(
                !detail.contains("eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA=="),
                "{detail}"
            );
        }
        assert!(
            details
                .iter()
                .any(|detail| detail == "remote: using [REDACTED]"),
            "{details:?}"
        );
        assert!(
            details.iter().any(|detail| detail == "header [REDACTED]"),
            "{details:?}"
        );
        // The accumulated stderr callers build error messages from keeps every
        // line, not just the last one.
        let stderr = captured.lock().unwrap().clone().unwrap();
        assert_eq!(
            stderr,
            "remote: using recognizable-secret\nheader eC1hY2Nlc3MtdG9rZW46cmVjb2duaXphYmxlLXNlY3JldA==\nfatal: no\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_and_reaps_the_active_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("blocking-git");
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$2/child.pid\"\nsleep 30\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let work = directory.path().to_owned();
        let executable = script.into_os_string();
        let mut job = BackgroundJob::<()>::spawn("child-cancel-test", move |context| {
            let runner = CancellableGitRunner::with_executable(context, executable);
            crate::git::GitRunner::run(&runner, &work, &[OsString::from("status")])
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
        let pid_path = directory.path().join("child.pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(fs::read_to_string(&pid_path), Ok(pid) if !pid.trim().is_empty()) {
            assert!(Instant::now() < deadline, "child did not start");
            thread::sleep(Duration::from_millis(10));
        }
        let pid: i32 = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        job.cancel();
        let result = loop {
            assert!(Instant::now() < deadline, "active child was not cancelled");
            if let Some(JobMessage::Finished(result)) = job.try_recv() {
                break result;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(result, Err(JobError::Cancelled));
        job.join();
        let still_alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!still_alive, "cancelled child is still alive");
    }
}
