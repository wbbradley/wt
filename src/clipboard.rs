use std::io::{Seek, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::background::JobContext;

const COPY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub trait Clipboard: Send + Sync {
    fn copy(&self, contents: &str, context: &JobContext) -> Result<(), String>;
}

pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&self, contents: &str, context: &JobContext) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let mut command = Command::new("pbcopy");
        #[cfg(target_os = "windows")]
        let mut command = Command::new("clip");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut command = Command::new("wl-copy");
        copy_with_command(&mut command, contents, context, COPY_TIMEOUT)
    }
}

fn copy_with_command(
    command: &mut Command,
    contents: &str,
    context: &JobContext,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let check = || {
        if context.is_cancelled() {
            Err("clipboard copy cancelled".to_owned())
        } else if started.elapsed() >= timeout {
            Err("clipboard command timed out".to_owned())
        } else {
            Ok(())
        }
    };
    check()?;
    // A regular file supplies EOF without a pipe writer that can block forever
    // when the backend stops reading. tempfile removes the private file on close.
    let mut input = tempfile::tempfile()
        .map_err(|error| format!("cannot prepare clipboard contents: {error}"))?;
    for chunk in contents.as_bytes().chunks(64 * 1024) {
        check()?;
        input
            .write_all(chunk)
            .map_err(|error| format!("cannot write clipboard contents: {error}"))?;
    }
    input
        .rewind()
        .map_err(|error| format!("cannot rewind clipboard contents: {error}"))?;
    check()?;
    let mut child = command
        .stdin(Stdio::from(input))
        // stdout is the shell integration's directory-selection channel.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot launch clipboard command: {error}"))?;
    let result = loop {
        if let Err(error) = check() {
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // wl-copy may have forked a clipboard owner. Leave that owner
                // alive after success so pasting still works after wt exits.
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("clipboard command exited with {status}"))
                };
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(error) => break Err(format!("cannot wait for clipboard command: {error}")),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::background::{BackgroundJob, JobError, JobMessage};

    fn run(script: &str, contents: String, timeout: Duration) -> Result<(), JobError> {
        let script = script.to_owned();
        let job = BackgroundJob::spawn("clipboard-test", move |context| {
            copy_with_command(
                Command::new("sh").arg("-c").arg(script),
                &contents,
                &context,
                timeout,
            )
        })
        .unwrap();
        finish(&job)
    }

    fn finish(job: &BackgroundJob<()>) -> Result<(), JobError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(JobMessage::Finished(result)) = job.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "clipboard worker stuck");
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn copies_exact_contents_and_reports_command_failures() {
        assert!(
            run(
                "test \"$(cat)\" = 'hello clipboard'",
                "hello clipboard".into(),
                Duration::from_secs(2)
            )
            .is_ok()
        );
        assert!(
            matches!(run("exit 7", String::new(), Duration::from_secs(2)), Err(JobError::Failed(error)) if error.contains("exited with"))
        );
        let job = BackgroundJob::spawn("clipboard-missing", |context| {
            copy_with_command(
                &mut Command::new("/nonexistent/wt-clipboard-command"),
                "text",
                &context,
                Duration::from_secs(2),
            )
        })
        .unwrap();
        assert!(
            matches!(finish(&job), Err(JobError::Failed(error)) if error.contains("cannot launch"))
        );
    }

    #[test]
    fn nonreading_command_with_large_input_times_out() {
        assert!(
            matches!(run("exec sleep 30", "x".repeat(4 * 1024 * 1024), Duration::from_millis(100)), Err(JobError::Failed(error)) if error.contains("timed out"))
        );
    }

    #[test]
    fn command_that_reads_then_hangs_times_out() {
        assert!(
            matches!(run("cat >/dev/null; exec sleep 30", "contents".into(), Duration::from_millis(100)), Err(JobError::Failed(error)) if error.contains("timed out"))
        );
    }

    #[test]
    fn cancellation_kills_and_reaps_child() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("pid");
        let child_pid_path = pid_path.clone();
        let job = BackgroundJob::spawn("clipboard-cancel", move |context| {
            copy_with_command(
                Command::new("sh")
                    .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "clipboard-test"])
                    .arg(child_pid_path),
                "text",
                &context,
                Duration::from_secs(30),
            )
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = text.trim().parse() {
                    break pid;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        };
        let started = Instant::now();
        drop(job); // Drop requests cancellation and joins the worker.
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
