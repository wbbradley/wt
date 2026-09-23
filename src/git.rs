use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use thiserror::Error;

use crate::model::{
    Catalog, RepositoryConfig, RepositoryDiscovery, RepositoryIdentity, Worktree, WorktreeStatus,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub exit_code: Option<i32>,
}

pub trait GitRunner {
    fn run(&self, directory: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError>;
    fn run_with_input(
        &self,
        _directory: &Path,
        _arguments: &[OsString],
        _input: &[u8],
    ) -> Result<CommandOutput, GitError> {
        Err(GitError::Command {
            message: "Git runner does not support stdin".to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn run(&self, directory: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError> {
        system_git(directory, arguments, None)
    }

    fn run_with_input(
        &self,
        directory: &Path,
        arguments: &[OsString],
        input: &[u8],
    ) -> Result<CommandOutput, GitError> {
        system_git(directory, arguments, Some(input))
    }
}

fn system_git(
    directory: &Path,
    arguments: &[OsString],
    input: Option<&[u8]>,
) -> Result<CommandOutput, GitError> {
    use std::io::{Seek, Write};
    let mut command = Command::new("git");
    command.arg("-C").arg(directory).args(arguments);
    if let Some(input) = input {
        // A file avoids pipe deadlocks when both the input and output are large.
        let mut file = tempfile::tempfile().map_err(|source| GitError::Launch { source })?;
        file.write_all(input)
            .and_then(|()| file.rewind())
            .map_err(|source| GitError::Launch { source })?;
        command.stdin(file);
    }
    let output = crate::logging::output(&mut command, false)
        .map_err(|source| GitError::Launch { source })?;
    Ok(CommandOutput {
        stdout: output.stdout,
        stderr: output.stderr,
        success: output.status.success(),
        exit_code: output.status.code(),
    })
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to launch Git: {source}")]
    Launch { source: std::io::Error },
    #[error("Git command failed: {message}")]
    Command { message: String },
    #[error("Git returned a non-path value for {field}")]
    InvalidPath { field: &'static str },
    #[error("cannot canonicalize {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("malformed worktree porcelain: {0}")]
    MalformedPorcelain(String),
    #[error("malformed status porcelain: {0}")]
    MalformedStatus(String),
    #[error("cannot inspect {path}: {source}")]
    FileMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn resolve_repository(
    runner: &dyn GitRunner,
    path: &Path,
) -> Result<RepositoryIdentity, GitError> {
    let common_output = run_checked(
        runner,
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_path = bytes_to_path(trim_ascii_line_end(&common_output), "Git common directory")?;
    let common_git_dir =
        fs::canonicalize(&common_path).map_err(|source| GitError::Canonicalize {
            path: common_path,
            source,
        })?;

    let worktrees = discover_worktrees(runner, path)?;
    let header = worktrees.first().ok_or_else(|| {
        GitError::MalformedPorcelain("Git reported no repository anchor".to_owned())
    })?;
    let anchor = fs::canonicalize(&header.path).map_err(|source| GitError::Canonicalize {
        path: header.path.clone(),
        source,
    })?;
    Ok(RepositoryIdentity {
        anchor,
        common_git_dir,
        bare: header.bare,
    })
}

pub fn discover_worktrees(
    runner: &dyn GitRunner,
    anchor: &Path,
) -> Result<Vec<Worktree>, GitError> {
    let output = run_checked(runner, anchor, &["worktree", "list", "--porcelain", "-z"])?;
    parse_worktree_porcelain(&output)
}

pub fn discover_catalog(runner: &dyn GitRunner, catalog: &Catalog) -> Vec<RepositoryDiscovery> {
    catalog
        .repositories
        .iter()
        .cloned()
        .map(|repository| {
            let result =
                discover_worktrees(runner, &repository.path).map_err(|error| error.to_string());
            RepositoryDiscovery { repository, result }
        })
        .collect()
}

pub fn canonical_common_dir(
    runner: &dyn GitRunner,
    repository: &RepositoryConfig,
) -> Result<PathBuf, GitError> {
    resolve_repository(runner, &repository.path).map(|identity| identity.common_git_dir)
}

fn run_checked(
    runner: &dyn GitRunner,
    directory: &Path,
    arguments: &[&str],
) -> Result<Vec<u8>, GitError> {
    let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let output = runner.run(directory, &arguments)?;
    if output.success {
        return Ok(output.stdout);
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(GitError::Command {
        message: if message.is_empty() {
            "unknown Git error".to_owned()
        } else {
            message
        },
    })
}

/// Scrubs an HTTPS credential out of Git output before it is shown, logged, or
/// published as progress. Both the raw token and the base64
/// `x-access-token:` transport form Git puts in an `Authorization` header can
/// appear in stderr, so both are replaced.
pub fn redact_secret(message: &str, secret: Option<&str>) -> String {
    secret
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("x-access-token:{secret}"));
            message
                .replace(secret, "[REDACTED]")
                .replace(&encoded, "[REDACTED]")
        })
        .unwrap_or_else(|| message.to_owned())
}

pub fn run_git(
    runner: &dyn GitRunner,
    directory: &Path,
    arguments: &[OsString],
) -> Result<Vec<u8>, GitError> {
    Ok(run_git_output(runner, directory, arguments)?.stdout)
}

pub fn run_git_output(
    runner: &dyn GitRunner,
    directory: &Path,
    arguments: &[OsString],
) -> Result<CommandOutput, GitError> {
    let output = runner.run(directory, arguments)?;
    if output.success {
        return Ok(output);
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(GitError::Command {
        message: if message.is_empty() {
            "unknown Git error".to_owned()
        } else {
            message
        },
    })
}

pub fn git_succeeds(
    runner: &dyn GitRunner,
    directory: &Path,
    arguments: &[OsString],
) -> Result<bool, GitError> {
    Ok(runner.run(directory, arguments)?.success)
}

pub fn status(runner: &dyn GitRunner, worktree: &Path) -> Result<WorktreeStatus, GitError> {
    let output = run_git(
        runner,
        worktree,
        &[
            OsString::from("status"),
            OsString::from("--porcelain=v2"),
            OsString::from("--untracked-files=all"),
            OsString::from("--branch"),
            OsString::from("-z"),
        ],
    )?;
    parse_status_porcelain(&output)
}

pub fn status_with_ignored(
    runner: &dyn GitRunner,
    worktree: &Path,
    configured: &[PathBuf],
) -> Result<WorktreeStatus, GitError> {
    let mut result = status(runner, worktree)?;
    result.ignored_paths = ignored_files(runner, worktree, configured)?;
    Ok(result)
}

fn ignored_files(
    runner: &dyn GitRunner,
    worktree: &Path,
    configured: &[PathBuf],
) -> Result<Vec<PathBuf>, GitError> {
    let mut candidates = Vec::new();
    for path in configured {
        match fs::metadata(worktree.join(path)) {
            Ok(metadata) if metadata.is_file() => {
                if !candidates.contains(path) {
                    candidates.push(path.clone());
                }
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(source) => {
                return Err(GitError::FileMetadata {
                    path: worktree.join(path),
                    source,
                });
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let arguments = ["check-ignore", "-z", "--stdin"].map(OsString::from);
    let mut input = Vec::new();
    for path in candidates {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            input.extend_from_slice(path.as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        input.extend_from_slice(path.to_string_lossy().as_bytes());
        input.push(0);
    }
    let output = runner.run_with_input(worktree, &arguments, &input)?;
    if output.exit_code == Some(1) {
        return Ok(Vec::new());
    }
    if !output.success {
        return Err(GitError::Command {
            message: format!("check-ignore failed: {}", lossy(&output.stderr).trim()),
        });
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| bytes_to_path(field, "ignored path"))
        .collect()
}

pub fn parse_status_porcelain(input: &[u8]) -> Result<WorktreeStatus, GitError> {
    let fields: Vec<&[u8]> = input.split(|byte| *byte == 0).collect();
    let mut status = WorktreeStatus::default();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        index += 1;
        if field.is_empty() {
            continue;
        }
        if let Some(value) = field.strip_prefix(b"# branch.oid ") {
            if value != b"(initial)" {
                status.head = Some(lossy(value));
            }
            continue;
        }
        if let Some(value) = field.strip_prefix(b"# branch.head ") {
            if value != b"(detached)" {
                status.branch = Some(lossy(value));
            }
            continue;
        }
        if let Some(value) = field.strip_prefix(b"# branch.upstream ") {
            status.upstream = Some(lossy(value));
            continue;
        }
        if field.starts_with(b"# ") {
            // Git emits further headers (branch.ab, stash) that carry no data we track.
            continue;
        }
        match field.first().copied() {
            Some(b'1' | b'2' | b'u') => {
                if field.len() < 4 || field[1] != b' ' {
                    return Err(GitError::MalformedStatus(lossy(field)));
                }
                let x = field[2];
                let y = field[3];
                if x != b'.' {
                    status.staged += 1;
                }
                if y != b'.' {
                    status.unstaged += 1;
                }
                if field[0] == b'2' {
                    if index >= fields.len() || fields[index].is_empty() {
                        return Err(GitError::MalformedStatus(
                            "rename record lacks its original path".to_owned(),
                        ));
                    }
                    index += 1;
                }
            }
            Some(b'?') if field.get(1) == Some(&b' ') => {
                status
                    .untracked_paths
                    .push(bytes_to_path(&field[2..], "untracked path")?);
                status.untracked += 1;
            }
            Some(b'!') if field.get(1) == Some(&b' ') => {}
            _ => return Err(GitError::MalformedStatus(lossy(field))),
        }
    }
    Ok(status)
}

pub fn parse_worktree_porcelain(input: &[u8]) -> Result<Vec<Worktree>, GitError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in input.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }
        let (key, value) = split_field(field);
        match key {
            b"worktree" => {
                if current.is_some() {
                    return Err(GitError::MalformedPorcelain(
                        "worktree record was not terminated".to_owned(),
                    ));
                }
                let path = bytes_to_path(value, "worktree path")?;
                current = Some(Worktree {
                    path,
                    head: None,
                    branch: None,
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                });
            }
            b"HEAD" => record_mut(&mut current, "HEAD")?.head = Some(lossy(value)),
            b"branch" => record_mut(&mut current, "branch")?.branch = Some(lossy(value)),
            b"detached" => record_mut(&mut current, "detached")?.detached = true,
            b"bare" => record_mut(&mut current, "bare")?.bare = true,
            b"locked" => record_mut(&mut current, "locked")?.locked = Some(lossy(value)),
            b"prunable" => record_mut(&mut current, "prunable")?.prunable = Some(lossy(value)),
            _ => {}
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

fn record_mut<'a>(
    current: &'a mut Option<Worktree>,
    field: &str,
) -> Result<&'a mut Worktree, GitError> {
    current.as_mut().ok_or_else(|| {
        GitError::MalformedPorcelain(format!("{field} appeared before a worktree path"))
    })
}

fn split_field(field: &[u8]) -> (&[u8], &[u8]) {
    match field.iter().position(|byte| *byte == b' ') {
        Some(index) => (&field[..index], &field[index + 1..]),
        None => (field, &[]),
    }
}

fn trim_ascii_line_end(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8], field: &'static str) -> Result<PathBuf, GitError> {
    use std::os::unix::ffi::OsStringExt;
    if bytes.is_empty() {
        return Err(GitError::InvalidPath { field });
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8], field: &'static str) -> Result<PathBuf, GitError> {
    if bytes.is_empty() {
        return Err(GitError::InvalidPath { field });
    }
    Ok(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn ignored_files_use_git_rules_and_refresh_without_affecting_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_checked(&SystemGit, root, &["init", "-q"]).unwrap();
        fs::write(
            root.join(".gitignore"),
            "*.secret\n!keep.secret\nignored-dir/\nlink\nraw*\n",
        )
        .unwrap();
        fs::write(root.join("tracked.secret"), "").unwrap();
        run_checked(
            &SystemGit,
            root,
            &["add", "-f", ".gitignore", "tracked.secret"],
        )
        .unwrap();
        fs::create_dir(root.join("ignored-dir")).unwrap();
        fs::write(root.join(".git/info/exclude"), "info-file\n").unwrap();
        let global = dir.path().join("global-excludes");
        fs::write(&global, "global-file\n").unwrap();
        run_git(
            &SystemGit,
            root,
            &[
                OsString::from("config"),
                OsString::from("core.excludesFile"),
                global.into_os_string(),
            ],
        )
        .unwrap();
        let names = [
            "a.secret",
            "keep.secret",
            "tracked.secret",
            "ignored-dir/nested ;$(x)",
            "info-file",
            "global-file",
            "ordinary",
            "missing",
            "ignored-dir",
        ];
        for name in names
            .iter()
            .filter(|name| !matches!(**name, "missing" | "ignored-dir"))
        {
            fs::write(root.join(name), "").unwrap();
        }
        let configured: Vec<_> = names.iter().map(PathBuf::from).collect();
        let before = status(&SystemGit, root).unwrap();
        let after = status_with_ignored(&SystemGit, root, &configured).unwrap();
        assert_eq!(
            after.ignored_paths,
            [
                "a.secret",
                "ignored-dir/nested ;$(x)",
                "info-file",
                "global-file"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            (
                before.staged,
                before.unstaged,
                before.untracked,
                before.is_dirty()
            ),
            (
                after.staged,
                after.unstaged,
                after.untracked,
                after.is_dirty()
            )
        );
        fs::remove_file(root.join("a.secret")).unwrap();
        assert!(
            !ignored_files(&SystemGit, root, &configured)
                .unwrap()
                .contains(&PathBuf::from("a.secret"))
        );
        fs::write(root.join("a.secret"), "").unwrap();
        fs::write(
            root.join(".gitignore"),
            "*.secret\n!a.secret\n!keep.secret\nignored-dir/\n",
        )
        .unwrap();
        assert!(
            !ignored_files(&SystemGit, root, &configured)
                .unwrap()
                .contains(&PathBuf::from("a.secret"))
        );
        assert!(
            ignored_files(&SystemGit, root, &[PathBuf::from("ordinary")])
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignored_files_preserve_bytes_and_classify_the_symlink_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_checked(&SystemGit, root, &["init", "-q"]).unwrap();
        fs::write(root.join(".gitignore"), "link\nraw*\n").unwrap();
        fs::write(root.join("target"), "").unwrap();
        symlink("target", root.join("link")).unwrap();
        symlink("absent", root.join("broken")).unwrap();
        symlink("loop", root.join("loop")).unwrap();
        let raw = PathBuf::from(OsString::from_vec(b"raw\xff\nname".to_vec()));
        fs::write(root.join(&raw), "").unwrap();
        let result = ignored_files(
            &SystemGit,
            root,
            &[PathBuf::from("link"), raw.clone(), PathBuf::from("broken")],
        )
        .unwrap();
        assert_eq!(result, vec![PathBuf::from("link"), raw.clone()]);
        assert_eq!(result[1].as_os_str().as_bytes(), raw.as_os_str().as_bytes());
        assert!(matches!(
            ignored_files(&SystemGit, root, &[PathBuf::from("loop")]),
            Err(GitError::FileMetadata { .. })
        ));
    }

    #[test]
    fn ignored_files_filter_before_git_and_distinguish_no_match_from_failure() {
        use std::cell::RefCell;
        struct Runner {
            calls: RefCell<Vec<Vec<OsString>>>,
            exit: i32,
        }
        impl GitRunner for Runner {
            fn run(&self, _: &Path, arguments: &[OsString]) -> Result<CommandOutput, GitError> {
                panic!("unexpected no-input call: {arguments:?}")
            }
            fn run_with_input(
                &self,
                _: &Path,
                arguments: &[OsString],
                input: &[u8],
            ) -> Result<CommandOutput, GitError> {
                assert_eq!(input, b"a ;$(x)\0");
                self.calls.borrow_mut().push(arguments.to_vec());
                Ok(CommandOutput {
                    stdout: Vec::new(),
                    stderr: b"diagnostic".to_vec(),
                    success: self.exit == 0,
                    exit_code: Some(self.exit),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("directory")).unwrap();
        let runner = Runner {
            calls: RefCell::new(Vec::new()),
            exit: 1,
        };
        assert!(ignored_files(&runner, root, &[]).unwrap().is_empty());
        assert!(
            ignored_files(&runner, root, &["missing", "directory"].map(PathBuf::from))
                .unwrap()
                .is_empty()
        );
        assert!(runner.calls.borrow().is_empty());
        fs::write(root.join("a ;$(x)"), "").unwrap();
        ignored_files(
            &runner,
            root,
            &["missing", "a ;$(x)", "directory", "a ;$(x)"].map(PathBuf::from),
        )
        .unwrap();
        assert_eq!(
            *runner.calls.borrow(),
            vec![
                vec!["check-ignore", "-z", "--stdin"]
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            ]
        );
        let runner = Runner {
            calls: RefCell::new(Vec::new()),
            exit: 128,
        };
        assert!(
            ignored_files(&runner, root, &[PathBuf::from("a ;$(x)")])
                .unwrap_err()
                .to_string()
                .contains("diagnostic")
        );
    }

    #[test]
    fn untracked_discovery_includes_nested_files_despite_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_checked(&SystemGit, root, &["init", "-q"]).unwrap();
        run_checked(
            &SystemGit,
            root,
            &["config", "status.showUntrackedFiles", "no"],
        )
        .unwrap();
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        fs::write(root.join("tracked"), "").unwrap();
        run_checked(&SystemGit, root, &["add", ".gitignore", "tracked"]).unwrap();
        fs::create_dir_all(root.join("scratch/nested")).unwrap();
        for name in ["root file", "scratch/nested/a;$(x)", "ignored"] {
            fs::write(root.join(name), "").unwrap();
        }
        let status = status(&SystemGit, root).unwrap();
        assert_eq!(status.untracked, 2);
        assert_eq!(
            status.untracked_paths,
            vec![
                PathBuf::from("root file"),
                PathBuf::from("scratch/nested/a;$(x)")
            ]
        );
        assert!(status.is_dirty());
    }

    #[cfg(unix)]
    #[test]
    fn untracked_parser_preserves_filename_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let status = parse_status_porcelain(b"? raw\xff\nname\0").unwrap();
        assert_eq!(
            status.untracked_paths[0].as_os_str().as_bytes(),
            b"raw\xff\nname"
        );
        assert!(parse_status_porcelain(b"? \0").is_err());
    }

    #[test]
    fn parses_all_worktree_states_and_spaces() {
        let input = b"worktree /tmp/main tree\0HEAD abc123\0branch refs/heads/main\0\0worktree /tmp/other\0HEAD def456\0detached\0locked maintenance window\0prunable gitdir file points to missing location\0\0";
        let parsed = parse_worktree_porcelain(input).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("/tmp/main tree"));
        assert_eq!(parsed[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(parsed[1].detached);
        assert_eq!(parsed[1].locked.as_deref(), Some("maintenance window"));
        assert!(parsed[1].prunable.is_some());
    }

    #[test]
    fn parses_bare_anchor() {
        let parsed = parse_worktree_porcelain(b"worktree /tmp/project.git\0bare\0\0").unwrap();
        assert!(parsed[0].bare);
        assert!(!parsed[0].navigable());
    }

    #[test]
    fn rejects_fields_outside_records() {
        let error = parse_worktree_porcelain(b"HEAD abc\0").unwrap_err();
        assert!(matches!(error, GitError::MalformedPorcelain(_)));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let parsed = parse_worktree_porcelain(b"worktree /tmp/bad\xffpath\0HEAD abc\0\0").unwrap();
        assert_eq!(parsed[0].path.as_os_str().as_bytes(), b"/tmp/bad\xffpath");
    }

    #[test]
    fn parses_v2_status_headers_and_counts() {
        let input = b"# branch.oid abc123\0# branch.head topic\0# branch.upstream origin/topic\0# branch.ab +0 -0\x001 M. N... 100644 100644 100644 a b file\x002 .M N... 100644 100644 100644 a b R100 new\0old\0? untracked\0";
        let status = parse_status_porcelain(input).unwrap();
        assert_eq!(status.head.as_deref(), Some("abc123"));
        assert_eq!(status.branch.as_deref(), Some("topic"));
        assert_eq!(status.upstream.as_deref(), Some("origin/topic"));
        assert_eq!(status.staged, 1);
        assert_eq!(status.unstaged, 1);
        assert_eq!(status.untracked, 1);
        assert!(status.is_dirty());
    }

    #[test]
    fn rejects_incomplete_rename_status() {
        let input = b"2 R. N... 100644 100644 100644 a b R100 new\0";
        assert!(matches!(
            parse_status_porcelain(input),
            Err(GitError::MalformedStatus(_))
        ));
    }

    #[test]
    fn resolves_main_and_linked_worktrees_to_the_same_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main");
        let linked = directory.path().join("linked tree");
        git(directory.path(), &["init", main.to_str().unwrap()]);
        git(&main, &["config", "user.email", "test@example.com"]);
        git(&main, &["config", "user.name", "Test User"]);
        git(&main, &["commit", "--allow-empty", "-m", "initial"]);
        git(
            &main,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );

        let main_identity = resolve_repository(&SystemGit, &main).unwrap();
        let linked_identity = resolve_repository(&SystemGit, &linked).unwrap();
        assert_eq!(main_identity, linked_identity);
        assert_eq!(main_identity.anchor, fs::canonicalize(main).unwrap());
        assert!(!main_identity.bare);
    }

    #[test]
    fn resolves_and_discovers_a_bare_repository_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let bare = directory.path().join("project.git");
        git(
            directory.path(),
            &["init", "--bare", bare.to_str().unwrap()],
        );

        let identity = resolve_repository(&SystemGit, &bare).unwrap();
        assert!(identity.bare);
        assert_eq!(identity.anchor, fs::canonicalize(&bare).unwrap());
        let worktrees = discover_worktrees(&SystemGit, &bare).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].bare);
    }

    #[test]
    fn catalog_discovery_isolates_stale_repositories() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        git(directory.path(), &["init", repository.to_str().unwrap()]);
        let catalog = Catalog {
            repositories: vec![
                RepositoryConfig {
                    path: repository,
                    label: Some("valid".to_owned()),
                    worktree_root: None,
                    github_remote: None,
                    github_remotes: Default::default(),
                    github_preferred_remote: None,
                },
                RepositoryConfig {
                    path: directory.path().join("missing"),
                    label: Some("stale".to_owned()),
                    worktree_root: None,
                    github_remote: None,
                    github_remotes: Default::default(),
                    github_preferred_remote: None,
                },
            ],
            ..Catalog::default()
        };
        let discoveries = discover_catalog(&SystemGit, &catalog);
        assert!(discoveries[0].result.is_ok());
        assert!(discoveries[1].result.is_err());
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}
