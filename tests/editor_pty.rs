#![cfg(unix)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn editor_subprocess_returns_to_tree_and_keeps_shell_capture_empty() {
    run_editor(Some("valid"), false, false, false);
}

#[test]
fn editor_failures_restore_terminal_without_directory_selection() {
    for editor in [
        None,
        Some("'unmatched"),
        Some("/no/such/editor"),
        Some("failure"),
    ] {
        run_editor(editor, false, false, false);
    }
}

#[test]
fn zsh_editor_handoff_keeps_directory_unchanged() {
    run_editor(Some("valid"), true, false, false);
}

#[test]
fn ignored_files_share_editor_handoff_and_handle_disappearance() {
    for zsh in [false, true] {
        run_editor(Some("valid"), zsh, true, false);
        run_editor(Some("valid"), zsh, true, true);
    }
}

#[test]
fn file_viewer_returns_to_tree_without_editor_or_shell_navigation() {
    for zsh in [false, true] {
        for ignored in [false, true] {
            run_session(Some("valid"), zsh, ignored, false, true);
        }
    }
}

fn run_editor(editor: Option<&str>, zsh: bool, ignored: bool, disappear: bool) {
    run_session(editor, zsh, ignored, disappear, false);
}

#[test]
fn picker_recovers_from_a_directory_deleted_before_startup() {
    for zsh in [false, true] {
        run_session_with_deleted_cwd(Some("valid"), zsh, false, false, false, true);
    }
}

fn run_session(editor: Option<&str>, zsh: bool, ignored: bool, disappear: bool, viewer: bool) {
    run_session_with_deleted_cwd(editor, zsh, ignored, disappear, viewer, false);
}

fn run_session_with_deleted_cwd(
    editor: Option<&str>,
    zsh: bool,
    ignored: bool,
    disappear: bool,
    viewer: bool,
    deleted_cwd: bool,
) {
    let temp = tempfile::tempdir().unwrap();
    // macOS exposes the temporary directory through a /var -> /private/var symlink.
    let root = fs::canonicalize(temp.path()).unwrap();
    let root = root.as_path();
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
    fs::write(
        repo.join(name),
        if viewer { "VIEWER_CONTENT" } else { "hello" },
    )
    .unwrap();
    if ignored {
        fs::write(repo.join(".git/info/exclude"), format!("{name}\n")).unwrap();
    }
    let config = root.join("wt.json");
    fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "repositories": [{"path": repo}], "repository_root": root,
            "ignored_files": if ignored { vec![name] } else { vec![] }
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
exit "$EDITOR_EXIT"
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
    let mut size = libc::winsize {
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
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    unsafe {
        assert_ne!(
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK),
            -1
        );
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            assert_ne!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), -1);
        }
    }
    let shell = if zsh { "zsh" } else { "bash" };
    let wrapper = format!("{}/shell/wt.{shell}", env!("CARGO_MANIFEST_DIR"));
    let removed = root.join("deleted directory");
    fs::create_dir(&removed).unwrap();
    let mut command = Command::new(shell);
    command.args([
        "-c",
        r#"source "$WRAPPER"; if [ "$DELETE_CWD" = 1 ]; then cd "$REMOVED" && rmdir "$REMOVED" || exit 1; fi; wt; result=$?; printf 'RESULT=%s\n' "$result"; pwd; printf 'WT_TEST_DONE\n' >&2; read -r acknowledgement"#,
    ]);
    command
        .current_dir(root)
        .env("WRAPPER", wrapper)
        .env("DELETE_CWD", if deleted_cwd { "1" } else { "0" })
        .env("REMOVED", removed)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("WT_CONFIG_PATH", config)
        .env("WT_STATE_PATH", root.join("state.json"))
        .env("WT_LOG_PATH", root.join("wt.log"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env(
            "EDITOR_EXIT",
            if editor == Some("failure") { "7" } else { "0" },
        )
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
            if matches!(value, "valid" | "failure") {
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
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    let mut sent = false;
    let mut viewed = false;
    let mut returned = false;
    let mut after_close = 0;
    let mut restored_termios = None;
    loop {
        let mut buffer = [0; 65536];
        while let Ok(n) = master.read(&mut buffer) {
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        if !sent && String::from_utf8_lossy(&output).contains("quote'.txt") {
            if disappear {
                fs::remove_file(repo.join(name)).unwrap();
            }
            master
                .write_all(if viewer { b"G\r" } else { b"Ge" })
                .unwrap();
            sent = true;
        }
        if viewer && sent && !viewed && String::from_utf8_lossy(&output).contains("VIEWER_CONTENT")
        {
            assert!(!root.join("result").exists());
            master.write_all(b"\x1b").unwrap();
            viewed = true;
            after_close = output.len();
        }
        if viewed
            && !returned
            && String::from_utf8_lossy(&output[after_close..]).contains("Enter views")
        {
            master.write_all(b"q").unwrap();
            returned = true;
        }
        if !viewer && sent && !returned {
            let terminal_output = String::from_utf8_lossy(&output);
            if let Some((_, after_restore)) = terminal_output.split_once("\x1b[?1049l")
                && let Some((_, resumed)) = after_restore.split_once("\x1b[?1049h")
                && resumed.contains("Enter views")
            {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "wt must remain running"
                );
                master.write_all(b"q").unwrap();
                returned = true;
            }
        }
        if restored_termios.is_none() && String::from_utf8_lossy(&output).contains("WT_TEST_DONE") {
            // Inspect restoration while the session leader still owns the PTY.
            // macOS rejects tcgetattr on the slave after that process exits.
            let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) },
                0,
                "cannot inspect restored terminal: {}",
                std::io::Error::last_os_error()
            );
            restored_termios = Some(unsafe { termios.assume_init() });
            master.write_all(b"\n").unwrap();
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            child.wait().unwrap();
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
    let success = viewer || (editor == Some("valid") && !disappear);
    assert!(returned, "wt must return to the tree before quitting");
    assert_eq!(stdout, format!("RESULT=0\n{}\n", root.display()));
    let terminal_output = String::from_utf8_lossy(&output);
    assert!(terminal_output.contains("\x1b[?1049l"));
    assert!(terminal_output.contains("\x1b[?25h"));
    let termios = restored_termios.expect("shell must wait for terminal inspection");
    assert_ne!(termios.c_lflag & libc::ICANON, 0);
    assert_ne!(termios.c_lflag & libc::ECHO, 0);
    if viewer {
        assert!(
            viewed && returned,
            "viewer must return to the selected file in the tree"
        );
        assert!(!root.join("result").exists());
        assert!(!terminal_output.contains("EDITOR_OUTPUT"));
    } else if success {
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
    } else if editor == Some("failure") {
        assert!(terminal_output.contains("EDITOR exited with"));
    } else if disappear {
        assert!(terminal_output.contains("cannot open"));
    } else {
        assert!(terminal_output.contains("EDITOR"));
    }
    assert!(!root.join("BAD").exists());
}
