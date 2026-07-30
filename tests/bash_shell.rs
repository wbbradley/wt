#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn bash_wrapper_navigates_safely_passes_commands_through_and_preserves_spaces() {
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

    let script = format!(
        r#"
source {shell:?}
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
COMP_WORDS=(wt anything)
COMP_CWORD=1
_wt_complete
printf 'completion:%s|%s\n' "${{COMPREPLY[0]}}" "${{COMPREPLY[1]}}"
"#,
        shell = format!("{}/shell/wt.bash", env!("CARGO_MANIFEST_DIR")),
    );
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
    assert!(stdout.contains("completion:candidate with spaces|plain"));
}

#[test]
fn bash_script_uses_bash_32_compatible_syntax() {
    let status = Command::new("bash")
        .args(["-n", concat!(env!("CARGO_MANIFEST_DIR"), "/shell/wt.bash")])
        .status()
        .unwrap();
    assert!(status.success());
}
