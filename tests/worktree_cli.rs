use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

#[test]
fn creates_all_modes_reports_status_and_validates_conflicts() {
    let fixture = Fixture::normal("creation");
    let existing = fixture.root.join("existing tree");
    let new_tree = fixture.root.join("nested/new tree");
    let detached = fixture.root.join("detached tree");
    git(&fixture.anchor, &["branch", "existing"]);

    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        existing.to_str().unwrap(),
        "--branch",
        "existing",
        "--yes",
    ]));
    let checked_out = fixture.wt(&[
        "worktree",
        "create",
        "project",
        fixture.root.join("duplicate").to_str().unwrap(),
        "--branch",
        "existing",
        "--yes",
    ]);
    assert_failure_contains(&checked_out, "already checked out");

    let missing_parent = fixture.wt(&[
        "worktree",
        "create",
        "project",
        new_tree.to_str().unwrap(),
        "--new-branch",
        "new/topic",
        "--start-point",
        "HEAD",
        "--yes",
    ]);
    assert_failure_contains(&missing_parent, "destination parent does not exist");
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        new_tree.to_str().unwrap(),
        "--new-branch",
        "new/topic",
        "--start-point",
        "HEAD",
        "--create-parents",
        "--yes",
    ]));
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        detached.to_str().unwrap(),
        "--detach",
        "HEAD",
        "--yes",
    ]));

    fs::write(new_tree.join("untracked file"), "change").unwrap();
    let inspect = fixture.wt(&["worktree", "inspect", "project", "new/topic"]);
    assert_success(&inspect);
    assert!(stdout(&inspect).contains("upstream\t-"));
    assert!(stdout(&inspect).contains("status\t0 staged, 0 unstaged, 1 untracked"));
    let list = fixture.wt(&["worktree", "list", "project"]);
    assert_success(&list);
    assert!(stdout(&list).contains(detached.to_str().unwrap()));
    assert!(stdout(&list).contains("detached:"));

    let invalid = fixture.wt(&[
        "worktree",
        "create",
        "project",
        fixture.root.join("bad").to_str().unwrap(),
        "--detach",
        "not-a-commit",
        "--yes",
    ]);
    assert_failure_contains(&invalid, "does not resolve to a commit");
    let missing_branch = fixture.wt(&[
        "worktree",
        "create",
        "project",
        fixture.root.join("missing").to_str().unwrap(),
        "--branch",
        "does-not-exist",
        "--yes",
    ]);
    assert_failure_contains(&missing_branch, "does not exist");
    let existing_new_branch = fixture.wt(&[
        "worktree",
        "create",
        "project",
        fixture.root.join("already").to_str().unwrap(),
        "--new-branch",
        "existing",
        "--yes",
    ]);
    assert_failure_contains(&existing_new_branch, "already exists");
    let collision = fixture.wt(&[
        "worktree",
        "create",
        "project",
        existing.to_str().unwrap(),
        "--detach",
        "HEAD",
        "--yes",
    ]);
    assert_failure_contains(&collision, "destination already exists");
}

#[test]
fn suggests_moves_locks_unlocks_and_repairs_worktrees() {
    let fixture = Fixture::normal_with_worktree_root("updates");
    git(&fixture.anchor, &["branch", "suggested/topic"]);
    assert!(!fixture.worktree_root.exists());
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        "--branch",
        "suggested/topic",
        "--yes",
    ]));
    let suggested = fixture.worktree_root.join("suggested-topic");
    assert!(suggested.exists());

    let moved = fixture.root.join("moved tree");
    assert_success(&fixture.wt(&[
        "worktree",
        "move",
        "project",
        "suggested/topic",
        moved.to_str().unwrap(),
        "--yes",
    ]));
    assert!(!suggested.exists());
    assert!(moved.exists());

    assert_success(&fixture.wt(&[
        "worktree",
        "lock",
        "project",
        "suggested/topic",
        "--reason",
        "do not remove",
        "--yes",
    ]));
    let list = fixture.wt(&["worktree", "list", "project"]);
    assert!(stdout(&list).contains("locked=do not remove"));
    let removal = fixture.wt(&["worktree", "remove", "project", "suggested/topic", "--yes"]);
    assert_failure_contains(&removal, "worktree is locked: do not remove");
    assert_success(&fixture.wt(&["worktree", "unlock", "project", "suggested/topic", "--yes"]));

    let relocated = fixture.root.join("relocated outside git");
    fs::rename(&moved, &relocated).unwrap();
    assert_success(&fixture.wt(&[
        "worktree",
        "repair",
        "project",
        relocated.to_str().unwrap(),
        "--yes",
    ]));
    assert_success(&git_output(&relocated, &["status", "--short"]));
}

#[test]
fn missing_worktree_root_is_created_for_creates_and_moves() {
    let fixture = Fixture::normal_with_worktree_root("root creation");
    git(&fixture.anchor, &["branch", "nested/topic"]);
    assert!(!fixture.worktree_root.exists());

    let nested = fixture.worktree_root.join("team/nested-topic");
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        nested.to_str().unwrap(),
        "--branch",
        "nested/topic",
        "--yes",
    ]));
    assert!(nested.join(".git").exists());

    let unmanaged = fixture.root.join("unmanaged/topic");
    let refusal = fixture.wt(&[
        "worktree",
        "create",
        "project",
        unmanaged.to_str().unwrap(),
        "--detach",
        "HEAD",
        "--yes",
    ]);
    assert_failure_contains(&refusal, "destination parent does not exist");
    assert!(!fixture.root.join("unmanaged").exists());

    let relocated = fixture.worktree_root.join("moved/nested-topic");
    assert_success(&fixture.wt(&[
        "worktree",
        "move",
        "project",
        "nested/topic",
        relocated.to_str().unwrap(),
        "--yes",
    ]));
    assert!(relocated.join(".git").exists());
}

#[test]
fn removal_safeguards_force_confirmation_and_branch_preservation() {
    let fixture = Fixture::normal("removal");
    let clean = fixture.root.join("clean");
    let dirty = fixture.root.join("dirty");
    let locked = fixture.root.join("locked");
    git(&fixture.anchor, &["branch", "clean-branch"]);
    git(&fixture.anchor, &["branch", "dirty-branch"]);
    git(&fixture.anchor, &["branch", "locked-branch"]);
    create_existing(&fixture, &clean, "clean-branch");
    create_existing(&fixture, &dirty, "dirty-branch");
    create_existing(&fixture, &locked, "locked-branch");

    let main_refusal = fixture.wt(&[
        "worktree",
        "remove",
        "project",
        fixture.anchor.to_str().unwrap(),
        "--yes",
    ]);
    assert_failure_contains(&main_refusal, "cannot remove the main worktree");

    let from_inside = fixture.wt_in(
        &dirty,
        &["worktree", "remove", "project", "dirty-branch", "--yes"],
    );
    assert_failure_contains(&from_inside, "containing the current directory");

    fs::write(dirty.join("untracked"), "local work").unwrap();
    let safe = fixture.wt(&["worktree", "remove", "project", "dirty-branch", "--yes"]);
    assert_failure_contains(&safe, "local changes");
    let wrong = fixture.wt(&[
        "worktree",
        "force-remove",
        "project",
        "dirty-branch",
        "--confirm",
        "wrong",
    ]);
    assert_failure_contains(&wrong, "typed confirmation must equal");
    let forced = fixture.wt(&[
        "worktree",
        "force-remove",
        "project",
        "dirty-branch",
        "--confirm",
        "dirty-branch",
    ]);
    assert_success(&forced);
    assert!(stderr(&forced).contains("0 staged, 0 unstaged, 1 untracked"));
    assert!(branch_exists(&fixture.anchor, "dirty-branch"));

    assert_success(&fixture.wt(&[
        "worktree",
        "lock",
        "project",
        "locked-branch",
        "--reason",
        "protected",
        "--yes",
    ]));
    assert_success(&fixture.wt(&[
        "worktree",
        "force-remove",
        "project",
        "locked-branch",
        "--confirm",
        "locked-branch",
    ]));
    assert!(branch_exists(&fixture.anchor, "locked-branch"));

    assert_success(&fixture.wt(&["worktree", "remove", "project", "clean-branch", "--yes"]));
    assert!(!clean.exists());
    assert!(branch_exists(&fixture.anchor, "clean-branch"));
}

#[test]
fn bare_repository_supports_crud_and_prune_preview_parity() {
    let fixture = Fixture::bare("bare");
    let bare_removal = fixture.wt(&["worktree", "remove", "project", "project.git", "--yes"]);
    assert_failure_contains(&bare_removal, "bare repository anchor");
    let tree = fixture.root.join("bare tree");
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        tree.to_str().unwrap(),
        "--new-branch",
        "bare-topic",
        "--start-point",
        "main",
        "--yes",
    ]));
    assert_success(&fixture.wt(&["worktree", "lock", "project", "bare-topic", "--yes"]));
    assert_success(&fixture.wt(&["worktree", "unlock", "project", "bare-topic", "--yes"]));
    let moved = fixture.root.join("bare moved");
    assert_success(&fixture.wt(&[
        "worktree",
        "move",
        "project",
        "bare-topic",
        moved.to_str().unwrap(),
        "--yes",
    ]));
    let repaired = fixture.root.join("bare repaired");
    fs::rename(&moved, &repaired).unwrap();
    assert_success(&fixture.wt(&[
        "worktree",
        "repair",
        "project",
        repaired.to_str().unwrap(),
        "--yes",
    ]));
    assert_success(&git_output(&repaired, &["status", "--short"]));
    assert_success(&fixture.wt(&["worktree", "remove", "project", "bare-topic", "--yes"]));
    assert!(branch_exists(&fixture.anchor, "bare-topic"));

    let stale = fixture.root.join("stale tree");
    git(
        &fixture.anchor,
        &[
            "worktree",
            "add",
            "-b",
            "stale-topic",
            stale.to_str().unwrap(),
            "main",
        ],
    );
    let displaced = fixture.root.join("displaced tree");
    fs::rename(&stale, &displaced).unwrap();
    let preview = fixture.wt(&["worktree", "prune-preview", "project"]);
    assert_success(&preview);
    assert!(stdout(&preview).contains("gitdir file points to non-existent location"));
    let detail = fixture.wt(&["worktree", "inspect", "project", "stale-topic"]);
    assert_success(&detail);
    assert!(stdout(&detail).contains("prunable\tgitdir file points"));
    let prune = fixture.wt(&["worktree", "prune", "project", "--yes"]);
    assert_success(&prune);
    assert!(stderr(&prune).contains(&stdout(&preview)));
    let after = fixture.wt(&["worktree", "prune-preview", "project"]);
    assert_success(&after);
    assert!(stdout(&after).is_empty());
}

#[test]
fn remove_merged_previews_confirms_revalidates_and_preserves_branches() {
    let fixture = Fixture::normal("remove merged");
    let worktree = fixture.root.join("merged topic");
    git(&fixture.anchor, &["branch", "topic"]);
    create_existing(&fixture, &worktree, "topic");

    let (base, preview_server) = fake_github_server(1);
    git(
        &fixture.anchor,
        &[
            "remote",
            "add",
            "origin",
            &format!("{base}/base/project.git"),
        ],
    );
    git(&fixture.anchor, &["config", "github.token", "test-token"]);
    let cancelled = fixture.wt(&["worktree", "remove-merged", "project"]);
    preview_server.join().unwrap();
    assert_failure_contains(&cancelled, "operation cancelled");
    assert!(stderr(&cancelled).contains("eligible\trepository=project\tbranch=topic"));
    assert!(worktree.exists());

    let (base, removal_server) = fake_github_server(2);
    git(
        &fixture.anchor,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("{base}/base/project.git"),
        ],
    );
    let removed = fixture.wt(&["worktree", "remove-merged", "project", "--yes"]);
    removal_server.join().unwrap();
    assert_success(&removed);
    assert!(stdout(&removed).contains("removed\t"));
    assert!(stderr(&removed).contains("result\tremoved=1\tskipped=1"));
    assert!(!worktree.exists());
    assert!(branch_exists(&fixture.anchor, "topic"));
}

#[test]
fn remove_merged_requires_exactly_one_scope_and_completes_all() {
    let fixture = Fixture::normal("remove merged scope");
    let missing = fixture.wt(&["worktree", "remove-merged", "--yes"]);
    assert_failure_contains(&missing, "required arguments were not provided");
    let conflicting = fixture.wt(&["worktree", "remove-merged", "project", "--all", "--yes"]);
    assert_failure_contains(&conflicting, "cannot be used with");
    let completion = fixture.wt(&["__complete", "worktree", "remove-merged", ""]);
    assert_success(&completion);
    assert!(stdout(&completion).lines().any(|line| line == "--all"));
    assert!(stdout(&completion).lines().any(|line| line == "project"));
}

#[test]
fn remove_merged_all_cleans_every_registered_repository() {
    let fixture = Fixture::normal("remove merged all");
    let first_worktree = fixture.root.join("first topic");
    git(&fixture.anchor, &["branch", "topic"]);
    create_existing(&fixture, &first_worktree, "topic");

    let second_anchor = fixture.root.join("second main");
    let second_worktree = fixture.root.join("second topic");
    git(
        &fixture.root,
        &["init", "-b", "main", second_anchor.to_str().unwrap()],
    );
    configure_identity(&second_anchor);
    git(
        &second_anchor,
        &["commit", "--allow-empty", "-m", "initial"],
    );
    git(&second_anchor, &["branch", "topic-two"]);
    assert_success(&fixture.wt(&[
        "repo",
        "add",
        second_anchor.to_str().unwrap(),
        "--label",
        "second",
    ]));
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "second",
        second_worktree.to_str().unwrap(),
        "--branch",
        "topic-two",
        "--yes",
    ]));

    let (base, server) = fake_github_server(4);
    for anchor in [&fixture.anchor, &second_anchor] {
        git(
            anchor,
            &[
                "remote",
                "add",
                "origin",
                &format!("{base}/base/project.git"),
            ],
        );
        git(anchor, &["config", "github.token", "test-token"]);
    }
    let removed = fixture.wt(&["worktree", "remove-merged", "--all", "--yes"]);
    server.join().unwrap();
    assert_success(&removed);
    assert!(stderr(&removed).contains("result\tremoved=2\tskipped=2"));
    assert!(!first_worktree.exists());
    assert!(!second_worktree.exists());
    assert!(branch_exists(&fixture.anchor, "topic"));
    assert!(branch_exists(&second_anchor, "topic-two"));
}

fn fake_github_server(request_count: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for stream in listener.incoming().take(request_count) {
            let mut stream = stream.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap();
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap();
            let head = request["variables"]["branch0"].as_str().unwrap();
            let body = serde_json::json!({
                "data": {
                    "repository": {
                        "branch0": {
                            "associatedPullRequests": {
                                "nodes": [{
                                    "number": 42,
                                    "title": "merged change",
                                    "url": "https://example.test/base/project/pull/42",
                                    "state": "MERGED",
                                    "isDraft": false,
                                    "mergedAt": "2026-08-01T00:00:00Z",
                                    "updatedAt": "2026-08-01T00:00:00Z",
                                    "reviewDecision": "APPROVED",
                                    "autoMergeRequest": null,
                                    "baseRefName": "main",
                                    "baseRefOid": "base",
                                    "baseRepository": {"nameWithOwner": "base/project"},
                                    "headRefName": "topic",
                                    "headRefOid": head,
                                    "headRepository": {"nameWithOwner": "base/project"},
                                    "commits": {"nodes": []}
                                }]
                            }
                        }
                    },
                    "rateLimit": {"remaining": 100, "resetAt": "2026-08-11T12:00:00Z"}
                }
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    (format!("http://{address}"), server)
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    anchor: PathBuf,
    config: PathBuf,
    worktree_root: PathBuf,
}

impl Fixture {
    fn normal(name: &str) -> Self {
        Self::normal_inner(name, false)
    }

    fn normal_with_worktree_root(name: &str) -> Self {
        Self::normal_inner(name, true)
    }

    fn normal_inner(name: &str, configure_root: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(name);
        fs::create_dir(&root).unwrap();
        let anchor = root.join("main");
        let config = root.join("config/wt.json");
        let worktree_root = root.join("managed trees");
        git(
            temporary.path(),
            &["init", "-b", "main", anchor.to_str().unwrap()],
        );
        configure_identity(&anchor);
        git(&anchor, &["commit", "--allow-empty", "-m", "initial"]);
        let fixture = Self {
            _temporary: temporary,
            root,
            anchor,
            config,
            worktree_root,
        };
        let mut arguments = vec![
            "repo",
            "add",
            fixture.anchor.to_str().unwrap(),
            "--label",
            "project",
        ];
        if configure_root {
            arguments.extend(["--worktree-root", fixture.worktree_root.to_str().unwrap()]);
        }
        assert_success(&fixture.wt(&arguments));
        fixture
    }

    fn bare(name: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(name);
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let anchor = root.join("project.git");
        let config = root.join("wt.json");
        let worktree_root = root.join("trees");
        git(
            temporary.path(),
            &["init", "-b", "main", source.to_str().unwrap()],
        );
        configure_identity(&source);
        git(&source, &["commit", "--allow-empty", "-m", "initial"]);
        git(
            temporary.path(),
            &["init", "--bare", "-b", "main", anchor.to_str().unwrap()],
        );
        git(
            &source,
            &["remote", "add", "origin", anchor.to_str().unwrap()],
        );
        git(&source, &["push", "origin", "main"]);
        let fixture = Self {
            _temporary: temporary,
            root,
            anchor,
            config,
            worktree_root,
        };
        assert_success(&fixture.wt(&[
            "repo",
            "add",
            fixture.anchor.to_str().unwrap(),
            "--label",
            "project",
            "--worktree-root",
            fixture.worktree_root.to_str().unwrap(),
        ]));
        fixture
    }

    fn wt(&self, arguments: &[&str]) -> Output {
        self.wt_in(&self.root, arguments)
    }

    fn wt_in(&self, directory: &Path, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wt"))
            .current_dir(directory)
            .env("WT_CONFIG_PATH", &self.config)
            .args(arguments)
            .output()
            .unwrap()
    }
}

fn create_existing(fixture: &Fixture, path: &Path, branch: &str) {
    assert_success(&fixture.wt(&[
        "worktree",
        "create",
        "project",
        path.to_str().unwrap(),
        "--branch",
        branch,
        "--yes",
    ]));
}

fn configure_identity(repository: &Path) {
    git(repository, &["config", "user.email", "test@example.com"]);
    git(repository, &["config", "user.name", "Test User"]);
}

fn branch_exists(repository: &Path, branch: &str) -> bool {
    git_output(
        repository,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .status
    .success()
}

fn git(directory: &Path, arguments: &[&str]) {
    assert_success(&git_output(directory, arguments));
}

fn git_output(directory: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments.iter().map(OsStr::new))
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

fn assert_failure_contains(output: &Output, expected: &str) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    assert!(
        stderr(output).contains(expected),
        "stderr did not contain {expected:?}: {}",
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
