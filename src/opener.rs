use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::background::{JobContext, terminate_child};

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub trait UrlOpener: Send + Sync {
    fn open(&self, url: &str, context: &JobContext) -> Result<(), String>;
}

pub struct SystemUrlOpener {
    open_command: Option<Vec<String>>,
}

impl SystemUrlOpener {
    pub fn new(open_command: Option<Vec<String>>) -> Self {
        Self { open_command }
    }

    fn command(&self, url: &str) -> Result<Command, String> {
        let mut command = if let Some(arguments) = &self.open_command {
            let (program, arguments) = arguments
                .split_first()
                .ok_or("open_command must name an executable")?;
            let mut command = Command::new(program);
            command.args(arguments);
            command
        } else {
            #[cfg(target_os = "macos")]
            let command = Command::new("open");
            #[cfg(target_os = "windows")]
            let command = {
                let mut command = Command::new("cmd");
                command.args(["/C", "start", ""]);
                command
            };
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let command = Command::new("xdg-open");
            command
        };
        command.arg(url);
        Ok(command)
    }
}

impl UrlOpener for SystemUrlOpener {
    fn open(&self, url: &str, context: &JobContext) -> Result<(), String> {
        run_command(&mut self.command(url)?, context, OPEN_TIMEOUT)
    }
}

fn run_command(
    command: &mut Command,
    context: &JobContext,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let check = || {
        if context.is_cancelled() {
            Err("URL opening cancelled".to_owned())
        } else if started.elapsed() >= timeout {
            Err("URL opener timed out".to_owned())
        } else {
            Ok(())
        }
    };
    check()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    // Browser descendants must not inherit the TUI or shell selection channel.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot launch URL opener: {error}"))?;
    let result = loop {
        if let Err(error) = check() {
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("URL opener exited with {status}"))
                };
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(error) => break Err(format!("cannot wait for URL opener: {error}")),
        }
    };
    terminate_child(&mut child);
    let _ = child.wait();
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::background::{BackgroundJob, JobError, JobMessage};

    fn finish(job: &BackgroundJob<()>) -> Result<(), JobError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(JobMessage::Finished(result)) = job.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "URL opener stuck");
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn configured_opener_receives_url_as_one_literal_argument() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opened URL");
        let url = "https://example.org/a'b?x=$(touch BAD)&quoted=\"two words\"";
        let opener = SystemUrlOpener::new(Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "test \"$1\" = 'two words' && printf '%s' \"$3\" > \"$2\"".to_owned(),
            "test".to_owned(),
            "two words".to_owned(),
            path.to_str().unwrap().to_owned(),
        ]));
        let job =
            BackgroundJob::spawn("url-opener-test", move |context| opener.open(url, &context))
                .unwrap();
        assert!(finish(&job).is_ok());
        assert_eq!(std::fs::read_to_string(path).unwrap(), url);
    }

    #[test]
    fn default_opener_and_command_failures() {
        let command = SystemUrlOpener::new(None)
            .command("https://example.org/")
            .unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(command.get_program(), "open");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(command.get_program(), "xdg-open");
        for arguments in [vec!["/nonexistent/wt-opener"], vec!["sh", "-c", "exit 7"]] {
            let opener =
                SystemUrlOpener::new(Some(arguments.into_iter().map(str::to_owned).collect()));
            let job = BackgroundJob::spawn("url-opener-failure", move |context| {
                opener.open("https://example.org/", &context)
            })
            .unwrap();
            assert!(matches!(finish(&job), Err(JobError::Failed(_))));
        }
    }

    #[test]
    fn timeout_terminates_opener_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("survived");
        let output = marker.clone();
        let job = BackgroundJob::spawn("url-opener-timeout", move |context| {
            run_command(
                Command::new("sh")
                    .args(["-c", "(sleep 0.3; echo survived > \"$1\") & wait", "test"])
                    .arg(output),
                &context,
                Duration::from_millis(100),
            )
        })
        .unwrap();
        assert!(
            matches!(finish(&job), Err(JobError::Failed(error)) if error.contains("timed out"))
        );
        std::thread::sleep(Duration::from_millis(400));
        assert!(!marker.exists());
    }
}
