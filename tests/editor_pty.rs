#![cfg(unix)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn editor_exec_restores_terminal_and_keeps_shell_capture_empty() {
    run_editor(Some("valid"), false);
}

#[test]
fn editor_failures_restore_terminal_without_directory_selection() {
    for editor in [None, Some("'unmatched"), Some("/no/such/editor")] {
        run_editor(editor, false);
    }
}

#[test]
fn zsh_editor_handoff_keeps_directory_unchanged() {
    if Command::new("zsh").arg("--version").output().is_ok() {
        run_editor(Some("valid"), true);
    }
}

fn run_editor(editor: Option<&str>, zsh: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    let name = "file ;$(touch BAD) 'quote'.txt";
    fs::write(repo.join(name), "hello").unwrap();
    let config = root.join("wt.json");
    fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "repositories": [{"path": repo}], "repository_root": root
        }))
        .unwrap(),
    )
    .unwrap();
    let editor_path = root.join("test editor");
    fs::write(
        &editor_path,
        r#"#!/bin/sh
test -t 0 && test -t 1 && test -t 2 || exit 41
test "$1" = "--wait" && test "$2" = "two words" || exit 42
printf '%s' "$3" > "$EDITOR_RESULT"
stty -a > "$EDITOR_TERMIOS"
printf 'EDITOR_OUTPUT'
printf 'EDITOR_ERROR' >&2
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&editor_path, fs::Permissions::from_mode(0o755)).unwrap();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("gh"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_wt"), bin.join("wt")).unwrap();

    let mut master_fd = -1;
    let mut slave_fd = -1;
    let size = libc::winsize {
        ws_row: 40,
        ws_col: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Own both returned descriptors, and give the child a controlling terminal.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let shell = if zsh { "zsh" } else { "bash" };
    let wrapper = format!("{}/shell/wt.{shell}", env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new(shell);
    command.args([
        "-c",
        r#"source "$WRAPPER"; wt; result=$?; printf 'RESULT=%s\n' "$result"; pwd"#,
    ]);
    command
        .current_dir(root)
        .env("WRAPPER", wrapper)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("WT_CONFIG_PATH", config)
        .env("WT_STATE_PATH", root.join("state.json"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("EDITOR_RESULT", root.join("result"))
        .env("EDITOR_TERMIOS", root.join("termios"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::piped());
    if let Some(value) = editor {
        command.env(
            "EDITOR",
            if value == "valid" {
                format!("'{}' --wait 'two words'", editor_path.display())
            } else {
                value.to_owned()
            },
        );
    } else {
        command.env_remove("EDITOR");
    }
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    let mut sent = false;
    loop {
        let mut buffer = [0; 65536];
        while let Ok(n) = master.read(&mut buffer) {
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        if !sent && String::from_utf8_lossy(&output).contains("quote'.txt") {
            master.write_all(b"G\r").unwrap();
            sent = true;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!(
                "editor handoff timed out: {}",
                String::from_utf8_lossy(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(sent);
    let success = editor == Some("valid");
    assert_eq!(
        stdout,
        format!(
            "RESULT={}\n{}\n",
            if success { 0 } else { 1 },
            root.display()
        )
    );
    let terminal_output = String::from_utf8_lossy(&output);
    assert!(terminal_output.contains("\x1b[?1049l"));
    assert!(terminal_output.contains("\x1b[?25h"));
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) },
        0
    );
    let termios = unsafe { termios.assume_init() };
    assert_ne!(termios.c_lflag & libc::ICANON, 0);
    assert_ne!(termios.c_lflag & libc::ECHO, 0);
    if success {
        assert_eq!(
            fs::read(root.join("result")).unwrap(),
            repo.join(name).as_os_str().as_encoded_bytes()
        );
        assert!(terminal_output.contains("EDITOR_OUTPUT"));
        assert!(terminal_output.contains("EDITOR_ERROR"));
        let before_editor = terminal_output.find("\x1b[?1049l").unwrap();
        assert!(before_editor < terminal_output.find("EDITOR_OUTPUT").unwrap());
        let settings = fs::read_to_string(root.join("termios")).unwrap();
        assert!(
            !settings
                .split_whitespace()
                .any(|word| word == "-icanon" || word == "-echo")
        );
    } else {
        assert!(terminal_output.contains("EDITOR"));
    }
    assert!(!root.join("BAD").exists());
}
