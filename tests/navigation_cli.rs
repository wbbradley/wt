use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[test]
fn qualified_selectors_return_exact_worktree_roots_and_complete_locally() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("main repository");
    let linked = directory.path().join("topic tree");
    let config = directory.path().join("config/wt.json");
    git(
        directory.path(),
        &["init", "-b", "main", repository.to_str().unwrap()],
    );
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Test User"]);
    git(&repository, &["commit", "--allow-empty", "-m", "initial"]);
    git(
        &repository,
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    assert_success(&wt(
        &config,
        &[
            "repo",
            "add",
            repository.to_str().unwrap(),
            "--label",
            "project",
        ],
    ));

    let selected = wt(&config, &["project:topic"]);
    assert_success(&selected);
    assert_eq!(
        selected.stdout,
        format!("{}\n", fs::canonicalize(&linked).unwrap().display()).as_bytes()
    );
    let by_basename = wt(&config, &["project:topic tree"]);
    assert_success(&by_basename);
    assert_eq!(by_basename.stdout, selected.stdout);

    let completion = wt(&config, &["__complete", "project:to"]);
    assert_success(&completion);
    let candidates = String::from_utf8(completion.stdout).unwrap();
    assert!(
        candidates
            .lines()
            .any(|candidate| candidate == "project:topic")
    );
    assert!(
        candidates
            .lines()
            .any(|candidate| candidate == "project:topic tree")
    );
}

fn wt(config: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", config)
        .args(arguments)
        .output()
        .unwrap()
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert_success(&output);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
