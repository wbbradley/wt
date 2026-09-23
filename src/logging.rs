use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Mutex;

pub fn init() -> io::Result<()> {
    let path = std::env::var_os("WT_LOG_PATH")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join("wt.log")))
        .ok_or_else(|| io::Error::other("HOME is unset; set WT_LOG_PATH for logging"))?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init()
        .map_err(io::Error::other)
}

/// Never format Command itself: its environment can contain credentials.
pub fn command_span(command: &Command, secret: Option<&str>) -> tracing::Span {
    let arguments = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    tracing::info_span!("subprocess",
        program = %command.get_program().to_string_lossy(),
        arguments = %crate::git::redact_secret(&format!("{arguments:?}"), secret),
        directory = ?command.get_current_dir(),
    )
}

pub fn record_output(
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    secret: Option<&str>,
    sensitive: bool,
) {
    let redact = |bytes: &[u8]| {
        if sensitive {
            "[REDACTED]".to_owned()
        } else {
            crate::git::redact_secret(&String::from_utf8_lossy(bytes), secret)
        }
    };
    tracing::info!(success, exit_code = ?code, stdout = %redact(stdout), stderr = %redact(stderr), "subprocess completed");
}

pub fn output(command: &mut Command, sensitive: bool) -> io::Result<Output> {
    let span = command_span(command, None);
    let _entered = span.enter();
    tracing::info!("subprocess started");
    match command.output() {
        Ok(output) => {
            record_output(
                output.status.success(),
                output.status.code(),
                &output.stdout,
                &output.stderr,
                None,
                sensitive,
            );
            Ok(output)
        }
        Err(error) => {
            tracing::error!(%error, "subprocess failed to launch");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek};

    #[test]
    fn captures_output_and_redacts_credentials() {
        // Isolate tracing's callsite registry from concurrently running tests.
        if std::env::var_os("WT_LOG_TEST_CHILD").is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "logging::tests::captures_output_and_redacts_credentials",
                    "--nocapture",
                ])
                .env("WT_LOG_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        let mut file = tempfile::tempfile().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Mutex::new(file.try_clone().unwrap()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let result = output(Command::new("git").arg("--version"), false).unwrap();
            assert!(result.status.success());
            record_output(
                false,
                Some(1),
                b"private-token",
                b"private-token",
                Some("private-token"),
                false,
            );
            record_output(
                true,
                Some(0),
                b"credential-value",
                b"credential-error",
                None,
                true,
            );
            let mut command = Command::new("git");
            command.env("GH_TOKEN", "environment-secret");
            let span = command_span(&command, None);
            let _entered = span.enter();
            tracing::info!("environment omitted");
            assert!(output(&mut Command::new("/nonexistent/wt-log-test"), false).is_err());
        });
        file.rewind().unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert!(contents.contains("git version"));
        assert!(contents.contains("exit_code=Some(1)"));
        assert!(contents.contains("subprocess failed to launch"));
        assert!(contents.contains("[REDACTED]"));
        for secret in [
            "private-token",
            "credential-value",
            "credential-error",
            "environment-secret",
        ] {
            assert!(!contents.contains(secret));
        }
    }
}
