use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[test]
fn catalog_commands_register_deduplicate_edit_list_and_remove() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main repository");
    let linked = directory.path().join("linked repository");
    let config = directory.path().join("config/wt.json");
    git(directory.path(), &["init", main.to_str().unwrap()]);
    git(&main, &["config", "user.email", "test@example.com"]);
    git(&main, &["config", "user.name", "Test User"]);
    git(&main, &["commit", "--allow-empty", "-m", "initial"]);
    git(
        &main,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );

    let add = wt(
        &config,
        &[
            "repo",
            "add",
            linked.to_str().unwrap(),
            "--label",
            "project",
            "--worktree-root",
            "trees",
            "--github-remote",
            "upstream",
        ],
    );
    assert_success(&add);
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(stored["version"], 1);
    assert_eq!(
        stored["repositories"][0]["path"],
        fs::canonicalize(&main).unwrap().to_str().unwrap()
    );
    assert_eq!(stored["repositories"][0]["label"], "project");
    assert!(Path::new(stored["repositories"][0]["worktree_root"].as_str().unwrap()).is_absolute());

    let duplicate = wt(&config, &["repo", "add", main.to_str().unwrap()]);
    assert!(!duplicate.status.success());
    assert!(stderr(&duplicate).contains("already registered"));

    let edit = wt(
        &config,
        &[
            "repo",
            "edit",
            "project",
            "--label",
            "renamed",
            "--clear-worktree-root",
            "--clear-github-remote",
        ],
    );
    assert_success(&edit);
    let list = wt(&config, &["repo", "list"]);
    assert_success(&list);
    assert!(stdout(&list).contains("renamed"));
    assert!(stdout(&list).contains("normal"));

    let remove = wt(&config, &["repo", "remove", "renamed"]);
    assert_success(&remove);
    assert!(
        main.exists(),
        "unregistering must not delete the repository"
    );
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(stored["repositories"].as_array().unwrap().len(), 0);
}

#[test]
fn list_retains_and_marks_a_missing_repository_stale() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("wt.json");
    fs::write(
        &config,
        format!(
            "{{\"version\":1,\"repositories\":[{{\"path\":{path:?},\"label\":\"missing\"}}]}}",
            path = directory.path().join("gone").to_str().unwrap()
        ),
    )
    .unwrap();
    let list = wt(&config, &["repo", "list"]);
    assert_success(&list);
    assert!(stdout(&list).contains("stale:"));
    assert!(fs::read_to_string(config).unwrap().contains("missing"));
}

#[test]
fn edit_relinks_a_stale_repository() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("new location");
    let config = directory.path().join("wt.json");
    fs::write(
        &config,
        format!(
            "{{\"version\":1,\"repositories\":[{{\"path\":{path:?},\"label\":\"moved\"}}]}}",
            path = directory.path().join("old location").to_str().unwrap()
        ),
    )
    .unwrap();
    git(directory.path(), &["init", repository.to_str().unwrap()]);

    let edit = wt(
        &config,
        &[
            "repo",
            "edit",
            "moved",
            "--path",
            repository.to_str().unwrap(),
            "--clear-label",
        ],
    );
    assert_success(&edit);
    let list = wt(&config, &["repo", "list"]);
    assert_success(&list);
    assert!(stdout(&list).contains("new location"));
    assert!(!stdout(&list).contains("stale:"));
}

#[test]
fn bare_repository_is_registered_and_classified() {
    let directory = tempfile::tempdir().unwrap();
    let bare = directory.path().join("project.git");
    let config = directory.path().join("wt.json");
    git(
        directory.path(),
        &["init", "--bare", bare.to_str().unwrap()],
    );
    assert_success(&wt(&config, &["repo", "add", bare.to_str().unwrap()]));
    let list = wt(&config, &["repo", "list"]);
    assert_success(&list);
    assert!(stdout(&list).contains("bare"));
}

#[test]
fn config_repository_root_preserves_expression_and_shows_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config/wt.json");
    let repository_base = directory.path().join("repository base");
    let expression = "${WT_TEST_REPOSITORY_BASE}/authored";
    let set = Command::new(env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", &config)
        .env("WT_TEST_REPOSITORY_BASE", &repository_base)
        .args(["config", "set", "repository-root", expression])
        .output()
        .unwrap();
    assert_success(&set);
    assert!(repository_base.join("authored").is_dir());
    assert!(stdout(&set).contains(&format!("configured={expression}")));
    assert!(stdout(&set).contains(&format!(
        "resolved={}",
        fs::canonicalize(repository_base.join("authored"))
            .unwrap()
            .display()
    )));

    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(stored["repository_root"], expression);

    let show = Command::new(env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", &config)
        .env("WT_TEST_REPOSITORY_BASE", &repository_base)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert_success(&show);
    assert!(stdout(&show).contains(&format!("configured={expression}")));
    assert!(stdout(&show).contains("github-host\tgithub.com"));
}

#[test]
fn config_rejects_undefined_variables_without_changing_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("wt.json");
    let output = Command::new(env!("CARGO_BIN_EXE_wt"))
        .env("WT_CONFIG_PATH", &config)
        .env_remove("WT_TEST_UNDEFINED_ROOT")
        .args([
            "config",
            "set",
            "repository-root",
            "$WT_TEST_UNDEFINED_ROOT/repos",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("is not defined"));
    assert!(!config.exists());
}

#[test]
fn config_commands_are_completed() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("wt.json");
    let top = wt(&config, &["__complete", "c"]);
    assert_success(&top);
    assert!(stdout(&top).lines().any(|line| line == "config"));

    let setting = wt(&config, &["__complete", "config", "set", "r"]);
    assert_success(&setting);
    assert!(
        stdout(&setting)
            .lines()
            .any(|line| line == "repository-root")
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
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
