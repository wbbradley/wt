#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn shell_init_emits_the_bash_script_without_loading_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_config = directory.path().join("invalid.json");
    fs::write(&invalid_config, "not json").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["shell-init", "bash"])
        .env("WT_CONFIG_PATH", invalid_config)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/shell/wt.bash")).unwrap()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn shell_init_emits_the_zsh_script_without_loading_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_config = directory.path().join("invalid.json");
    fs::write(&invalid_config, "not json").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["shell-init", "zsh"])
        .env("WT_CONFIG_PATH", invalid_config)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/shell/wt.zsh")).unwrap()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn shell_init_rejects_missing_and_unsupported_shells() {
    let missing = Command::new(env!("CARGO_BIN_EXE_wt"))
        .arg("shell-init")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let missing_error = String::from_utf8_lossy(&missing.stderr);
    assert!(
        missing_error.contains("Usage: wt shell-init <SHELL>"),
        "{missing_error}"
    );
    assert!(
        missing_error.contains("required arguments were not provided"),
        "{missing_error}"
    );

    let unsupported = Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["shell-init", "fish"])
        .output()
        .unwrap();
    assert!(!unsupported.status.success());
    let unsupported_error = String::from_utf8_lossy(&unsupported.stderr);
    assert!(
        unsupported_error.contains("invalid value 'fish'"),
        "{unsupported_error}"
    );
    assert!(
        unsupported_error.contains("possible values: bash, zsh"),
        "{unsupported_error}"
    );
}

#[test]
fn shell_init_is_in_local_completion_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("missing.json");
    let top_level = Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["__complete", ""])
        .env("WT_CONFIG_PATH", &config)
        .output()
        .unwrap();
    assert!(top_level.status.success());
    assert!(
        String::from_utf8_lossy(&top_level.stdout)
            .lines()
            .any(|candidate| candidate == "shell-init")
    );
    assert!(
        String::from_utf8_lossy(&top_level.stdout)
            .lines()
            .any(|candidate| candidate == "-x")
    );

    let shell = Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["__complete", "shell-init", ""])
        .env("WT_CONFIG_PATH", config)
        .output()
        .unwrap();
    assert!(shell.status.success());
    assert!(
        String::from_utf8_lossy(&shell.stdout)
            .lines()
            .any(|candidate| candidate == "bash")
    );
    assert!(
        String::from_utf8_lossy(&shell.stdout)
            .lines()
            .any(|candidate| candidate == "zsh")
    );
}

#[test]
fn initialized_bash_wrapper_navigates_safely_passes_commands_through_and_preserves_spaces() {
    let directory = tempfile::tempdir().unwrap();
    let binary_directory = directory.path().join("bin");
    let destination = directory.path().join("destination with spaces");
    fs::create_dir_all(&binary_directory).unwrap();
    fs::create_dir(&destination).unwrap();
    let fake_wt = binary_directory.join("wt");
    fs::write(
        &fake_wt,
        r#"#!/bin/bash
case "${1-}" in
    shell-init) exec "$WT_REAL_BINARY" "$@" ;;
    success) printf '%s\n' "$WT_TEST_DESTINATION" ;;
    cancel) exit 0 ;;
    fail) exit 7 ;;
    repo|worktree|help|--help|--version) printf 'passthrough:%s\n' "$1" ;;
    __complete) printf 'candidate with spaces\nplain\n' ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_wt).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_wt, permissions).unwrap();

    let script = r#"
eval "$(wt shell-init bash)"
wt cancel
printf 'cancel:%s\n' "$PWD"
wt fail
wt_status=$?
printf 'failure:%s:%s\n' "$wt_status" "$PWD"
wt success
printf 'success:%s\n' "$PWD"
wt repo list
wt help
wt --version
wt_reloaded="$(wt shell-init bash)"
case "$wt_reloaded" in
    *'wt() {'*) printf 'shell-init:reloaded\n' ;;
esac
COMP_WORDS=(wt anything)
COMP_CWORD=1
_wt_complete
printf 'completion:%s|%s\n' "${COMPREPLY[0]}" "${COMPREPLY[1]}"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(directory.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                binary_directory.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("WT_TEST_DESTINATION", &destination)
        .env("WT_REAL_BINARY", env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", directory.path().join("invalid.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let start = fs::canonicalize(directory.path())
        .unwrap()
        .display()
        .to_string();
    assert!(stdout.contains(&format!("cancel:{start}")), "{stdout:?}");
    assert!(stdout.contains(&format!("failure:7:{start}")), "{stdout:?}");
    assert!(stdout.contains(&format!("success:{}", destination.display())));
    assert!(stdout.contains("passthrough:repo"));
    assert!(stdout.contains("passthrough:help"));
    assert!(stdout.contains("passthrough:--version"));
    assert!(stdout.contains("shell-init:reloaded"));
    assert!(stdout.contains("completion:candidate with spaces|plain"));
}

#[test]
fn initialized_zsh_wrapper_navigates_safely_passes_commands_through_and_preserves_spaces() {
    let directory = tempfile::tempdir().unwrap();
    let binary_directory = directory.path().join("bin");
    let destination = directory.path().join("destination with spaces");
    fs::create_dir_all(&binary_directory).unwrap();
    fs::create_dir(&destination).unwrap();
    let fake_wt = binary_directory.join("wt");
    fs::write(
        &fake_wt,
        r#"#!/bin/sh
case "${1-}" in
    shell-init) exec "$WT_REAL_BINARY" "$@" ;;
    success) printf '%s\n' "$WT_TEST_DESTINATION" ;;
    cancel) exit 0 ;;
    fail) exit 7 ;;
    repo|worktree|help|--help|--version) printf 'passthrough:%s\n' "$1" ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_wt).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_wt, permissions).unwrap();

    let script = r#"
eval "$(wt shell-init zsh)"
printf 'completion:%s\n' "${_comps[wt]}"
wt cancel
printf 'cancel:%s\n' "$PWD"
wt fail
wt_status=$?
printf 'failure:%s:%s\n' "$wt_status" "$PWD"
wt success
printf 'success:%s\n' "$PWD"
wt repo list
wt help
wt --version
wt_reloaded="$(wt shell-init zsh)"
case "$wt_reloaded" in
    *'wt() {'*) printf 'shell-init:reloaded\n' ;;
esac
"#;
    let output = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(script)
        .current_dir(directory.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                binary_directory.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("WT_TEST_DESTINATION", &destination)
        .env("WT_REAL_BINARY", env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", directory.path().join("invalid.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let start = fs::canonicalize(directory.path())
        .unwrap()
        .display()
        .to_string();
    assert!(stdout.contains("completion:_wt_complete"), "{stdout:?}");
    assert!(stdout.contains(&format!("cancel:{start}")), "{stdout:?}");
    assert!(stdout.contains(&format!("failure:7:{start}")), "{stdout:?}");
    assert!(stdout.contains(&format!("success:{}", destination.display())));
    assert!(stdout.contains("passthrough:repo"));
    assert!(stdout.contains("passthrough:help"));
    assert!(stdout.contains("passthrough:--version"));
    assert!(stdout.contains("shell-init:reloaded"));
}

#[test]
fn bash_script_uses_bash_32_compatible_syntax() {
    let status = Command::new("bash")
        .args(["-n", concat!(env!("CARGO_MANIFEST_DIR"), "/shell/wt.bash")])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn zsh_script_has_valid_syntax() {
    let status = Command::new("zsh")
        .args(["-n", concat!(env!("CARGO_MANIFEST_DIR"), "/shell/wt.zsh")])
        .status()
        .unwrap();
    assert!(status.success());
}
