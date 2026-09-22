use std::io;
use std::path::Path;
use std::process::Command;

/// Split editor configuration into literal arguments, with shell-style quoting but
/// no shell evaluation, variable expansion, pipelines, or redirection.
fn arguments(value: &str) -> io::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => word.push(c),
            (_, '\\') => {
                let next = chars
                    .next()
                    .ok_or_else(|| invalid("EDITOR ends with an escape"))?;
                if quote == Some('"') && !matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                    word.push('\\');
                }
                if next != '\n' {
                    word.push(next);
                }
                started = true;
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(invalid("EDITOR has an unmatched quote"));
    }
    if started {
        words.push(word);
    }
    if words.first().is_none_or(String::is_empty) {
        return Err(invalid("EDITOR must name an executable"));
    }
    Ok(words)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn command(value: &str, path: &Path) -> io::Result<Command> {
    let args = arguments(value)?;
    if !path.is_absolute() {
        return Err(invalid("editor file path must be absolute"));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot open {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(invalid("selected editor path is not a file"));
    }
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]).arg(path);
    Ok(command)
}

/// The caller must restore the terminal first: successful exec never returns.
pub fn exec(path: &Path) -> io::Result<()> {
    let value = std::env::var("EDITOR")
        .map_err(|_| invalid("set EDITOR to an editor executable and optional arguments"))?;
    exec_configured(&value, path)
}

#[cfg(unix)]
fn exec_configured(value: &str, path: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut command = command(value, path)?;
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot open editor terminal: {error}"),
            )
        })?;
    command
        .stdin(tty.try_clone()?)
        .stdout(tty.try_clone()?)
        .stderr(tty);
    let error = command.exec();
    Err(io::Error::new(
        error.kind(),
        format!("failed to execute EDITOR: {error}"),
    ))
}

#[cfg(not(unix))]
fn exec_configured(_value: &str, _path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "editor process replacement requires Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_editor_arguments_are_literal() {
        assert_eq!(
            arguments("editor --wait 'two words' \"quoted words\" a\\ b '$HOME'").unwrap(),
            [
                "editor",
                "--wait",
                "two words",
                "quoted words",
                "a b",
                "$HOME"
            ]
        );
        for invalid in ["", "  ", "'' arg", "editor 'oops", "editor \\"] {
            assert!(arguments(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn file_is_a_separate_raw_argument_and_missing_files_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a $(touch BAD); 'quoted'");
        std::fs::write(&path, "").unwrap();
        let cmd = command("editor --wait", &path).unwrap();
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("--wait"), path.as_os_str()]
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            command("editor", &path)
                .unwrap_err()
                .to_string()
                .contains("cannot open")
        );
    }
}
