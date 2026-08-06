use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use fs2::FileExt;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::model::{CATALOG_VERSION, Catalog};

pub const CONFIG_PATH_ENV: &str = "WT_CONFIG_PATH";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot determine the configuration directory; set {CONFIG_PATH_ENV}")]
    NoConfigDirectory,
    #[error("failed to read catalog {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("catalog {path} is not valid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("catalog schema version {found} is newer than supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    #[error("failed to write catalog {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("failed to encode catalog: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("repository root expression {expression:?} is invalid: {message}")]
    InvalidRepositoryRoot { expression: String, message: String },
    #[error("failed to prepare repository root {path}: {source}")]
    RepositoryRoot { path: PathBuf, source: io::Error },
    #[error("failed to acquire catalog lock {path}: {source}")]
    Lock { path: PathBuf, source: io::Error },
    #[error("catalog lock acquisition was cancelled")]
    LockCancelled,
}

pub struct CatalogLock {
    _file: File,
}

pub fn catalog_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os(CONFIG_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or(ConfigError::NoConfigDirectory)?;
    Ok(config_home.join("wt.json"))
}

pub fn load(path: &Path) -> Result<Catalog, ConfigError> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Catalog::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let catalog: Catalog =
        serde_json::from_slice(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
    if catalog.version > CATALOG_VERSION {
        return Err(ConfigError::FutureVersion {
            found: catalog.version,
            supported: CATALOG_VERSION,
        });
    }
    Ok(catalog)
}

pub fn save(path: &Path, catalog: &Catalog) -> Result<(), ConfigError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;

    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, catalog)?;
        writer
            .write_all(b"\n")
            .map_err(|source| ConfigError::Write {
                path: path.to_owned(),
                source,
            })?;
        writer.flush().map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| ConfigError::Write {
            path: path.to_owned(),
            source: error.error,
        })?;
    sync_directory(parent).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

pub fn repository_root(catalog: &Catalog) -> Result<PathBuf, ConfigError> {
    resolve_repository_root(catalog.repository_root_expression())
}

pub fn resolve_repository_root(expression: &str) -> Result<PathBuf, ConfigError> {
    resolve_repository_root_with(expression, |name| env::var_os(name))
}

fn resolve_repository_root_with(
    expression: &str,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let invalid = |message: String| ConfigError::InvalidRepositoryRoot {
        expression: expression.to_owned(),
        message,
    };
    if expression.is_empty() {
        return Err(invalid("value cannot be empty".to_owned()));
    }
    if expression.contains(['*', '?', '[', ']', '`', '\n', '\r'])
        || expression.contains("$(")
        || expression.contains([';', '|', '&', '<', '>'])
    {
        return Err(invalid(
            "shell syntax, command substitution, and globbing are not allowed".to_owned(),
        ));
    }

    let expanded = if expression == "~" || expression.starts_with("~/") {
        let home = environment("HOME").ok_or_else(|| invalid("HOME is not defined".to_owned()))?;
        PathBuf::from(home).join(expression.strip_prefix("~/").unwrap_or(""))
    } else if let Some(rest) = expression.strip_prefix("${") {
        let close = rest
            .find('}')
            .ok_or_else(|| invalid("leading environment variable is missing '}'".to_owned()))?;
        let name = &rest[..close];
        validate_variable_name(name).map_err(&invalid)?;
        let value = environment(name)
            .ok_or_else(|| invalid(format!("environment variable {name} is not defined")))?;
        append_expression_suffix(PathBuf::from(value), &rest[close + 1..])
    } else if let Some(rest) = expression.strip_prefix('$') {
        let name_length = rest
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        let name = &rest[..name_length];
        validate_variable_name(name).map_err(&invalid)?;
        let value = environment(name)
            .ok_or_else(|| invalid(format!("environment variable {name} is not defined")))?;
        append_expression_suffix(PathBuf::from(value), &rest[name_length..])
    } else {
        PathBuf::from(expression)
    };

    if !expanded.is_absolute() {
        return Err(invalid(format!(
            "expanded path {} is not absolute",
            expanded.display()
        )));
    }
    let normalized =
        canonicalize_existing_prefix(&expanded).map_err(|source| ConfigError::RepositoryRoot {
            path: expanded.clone(),
            source,
        })?;
    fs::create_dir_all(&normalized).map_err(|source| ConfigError::RepositoryRoot {
        path: normalized.clone(),
        source,
    })?;
    let canonical =
        fs::canonicalize(&normalized).map_err(|source| ConfigError::RepositoryRoot {
            path: normalized.clone(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(invalid(format!(
            "expanded path {} is not a directory",
            canonical.display()
        )));
    }
    NamedTempFile::new_in(&canonical).map_err(|source| ConfigError::RepositoryRoot {
        path: canonical.clone(),
        source,
    })?;
    Ok(canonical)
}

fn validate_variable_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(format!("{name:?} is not a valid environment variable name"));
    }
    Ok(())
}

fn append_expression_suffix(mut base: PathBuf, suffix: &str) -> PathBuf {
    if let Some(suffix) = suffix.strip_prefix('/') {
        base.push(suffix);
    } else if !suffix.is_empty() {
        base.as_mut_os_string().push(suffix);
    }
    base
}

fn canonicalize_existing_prefix(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "path has no existing prefix")
        })?;
        suffix.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "path has no existing prefix")
        })?;
    }
    let mut canonical = fs::canonicalize(existing)?;
    for component in suffix.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

pub fn lock_path(catalog_path: &Path) -> PathBuf {
    let mut path = catalog_path.as_os_str().to_os_string();
    path.push(OsStr::new(".lock"));
    PathBuf::from(path)
}

pub fn acquire_catalog_lock(catalog_path: &Path) -> Result<CatalogLock, ConfigError> {
    acquire_catalog_lock_with(catalog_path, || false, || {})
}

pub fn acquire_catalog_lock_with(
    catalog_path: &Path,
    mut cancelled: impl FnMut() -> bool,
    mut waiting: impl FnMut(),
) -> Result<CatalogLock, ConfigError> {
    let path = lock_path(catalog_path);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| ConfigError::Lock {
        path: path.clone(),
        source,
    })?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| ConfigError::Lock {
            path: path.clone(),
            source,
        })?;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(CatalogLock { _file: file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if cancelled() {
                    return Err(ConfigError::LockCancelled);
                }
                waiting();
                thread::sleep(Duration::from_millis(40));
            }
            Err(source) => {
                return Err(ConfigError::Lock {
                    path: path.clone(),
                    source,
                });
            }
        }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DEFAULT_GITHUB_REFRESH_INTERVAL_SECS, RepositoryConfig};
    use std::collections::HashMap;
    use std::sync::mpsc;

    #[test]
    fn missing_catalog_loads_default() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = load(&directory.path().join("missing.json")).unwrap();
        assert_eq!(catalog, Catalog::default());
    }

    #[test]
    fn defaults_optional_catalog_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wt.json");
        fs::write(&path, r#"{"version":1}"#).unwrap();
        let catalog = load(&path).unwrap();
        assert_eq!(
            catalog.github_refresh_interval_secs,
            DEFAULT_GITHUB_REFRESH_INTERVAL_SECS
        );
        assert!(catalog.repositories.is_empty());
    }

    #[test]
    fn rejects_future_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wt.json");
        fs::write(&path, r#"{"version":2,"repositories":[]}"#).unwrap();
        assert!(matches!(
            load(&path),
            Err(ConfigError::FutureVersion {
                found: 2,
                supported: 1
            })
        ));
    }

    #[test]
    fn saves_atomically_and_creates_parents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/config/wt.json");
        let mut catalog = Catalog::default();
        catalog.repositories.push(RepositoryConfig {
            path: PathBuf::from("/tmp/example"),
            label: None,
            worktree_root: None,
            github_remote: None,
        });
        save(&path, &catalog).unwrap();
        assert_eq!(load(&path).unwrap(), catalog);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn failed_write_preserves_existing_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wt.json");
        let catalog = Catalog::default();
        save(&path, &catalog).unwrap();
        let original = fs::read(&path).unwrap();

        let impossible_path = path.join("child");
        assert!(save(&impossible_path, &catalog).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn expands_only_supported_leading_expressions_and_creates_roots() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let variable = directory.path().join("variable");
        let environment = HashMap::from([
            ("HOME", home.as_os_str().to_os_string()),
            ("REPOSITORY_BASE", variable.as_os_str().to_os_string()),
        ]);
        let lookup = |name: &str| environment.get(name).cloned();

        assert_eq!(
            resolve_repository_root_with("~/src/project", lookup).unwrap(),
            fs::canonicalize(&home).unwrap().join("src/project")
        );
        assert_eq!(
            resolve_repository_root_with("$REPOSITORY_BASE/repos", lookup).unwrap(),
            fs::canonicalize(&variable).unwrap().join("repos")
        );
        assert_eq!(
            resolve_repository_root_with("${REPOSITORY_BASE}/other", lookup).unwrap(),
            fs::canonicalize(&variable).unwrap().join("other")
        );
        assert!(home.join("src/project").is_dir());
        assert!(variable.join("repos").is_dir());
    }

    #[test]
    fn rejects_undefined_relative_and_shell_expressions() {
        let directory = tempfile::tempdir().unwrap();
        let environment = |_name: &str| None;
        for expression in [
            "$MISSING/repo",
            "relative/path",
            "/tmp/*",
            "/tmp/$(whoami)",
            "/tmp/`whoami`",
            "/tmp/repo;whoami",
            "$9INVALID/path",
            "${BROKEN/path",
        ] {
            assert!(
                matches!(
                    resolve_repository_root_with(expression, environment),
                    Err(ConfigError::InvalidRepositoryRoot { .. })
                ),
                "expression should be rejected: {expression}"
            );
        }
        assert!(directory.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_a_symlink_in_the_longest_existing_prefix() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual");
        let link = directory.path().join("link");
        fs::create_dir(&actual).unwrap();
        symlink(&actual, &link).unwrap();
        let resolved =
            resolve_repository_root_with(link.join("new/nested").to_str().unwrap(), |_name| None)
                .unwrap();
        assert_eq!(
            resolved,
            fs::canonicalize(&actual).unwrap().join("new/nested")
        );
        assert!(actual.join("new/nested").is_dir());
    }

    #[test]
    fn rejects_an_existing_non_directory_root() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"not a directory").unwrap();
        assert!(matches!(
            resolve_repository_root_with(file.to_str().unwrap(), |_name| None),
            Err(ConfigError::RepositoryRoot { .. })
                | Err(ConfigError::InvalidRepositoryRoot { .. })
        ));
    }

    #[test]
    fn sidecar_lock_serializes_reload_mutate_and_atomic_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wt.json");
        save(&path, &Catalog::default()).unwrap();
        let first_lock = acquire_catalog_lock(&path).unwrap();
        let thread_path = path.clone();
        let (waiting_sender, waiting_receiver) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _lock = acquire_catalog_lock_with(
                &thread_path,
                || false,
                || {
                    let _ = waiting_sender.send(());
                },
            )
            .unwrap();
            let mut catalog = load(&thread_path).unwrap();
            catalog.github_hosts.push("writer.example".to_owned());
            save(&thread_path, &catalog).unwrap();
        });
        waiting_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("second writer should wait on the sidecar");

        let mut catalog = load(&path).unwrap();
        catalog.repository_root = Some(directory.path().join("repos").display().to_string());
        save(&path, &catalog).unwrap();
        drop(first_lock);
        writer.join().unwrap();

        let catalog = load(&path).unwrap();
        assert!(catalog.repository_root.is_some());
        assert_eq!(catalog.github_hosts, ["writer.example"]);
        assert!(lock_path(&path).is_file());
    }

    #[test]
    fn lock_wait_can_be_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wt.json");
        let _lock = acquire_catalog_lock(&path).unwrap();
        let mut attempts = 0;
        let result = acquire_catalog_lock_with(
            &path,
            || {
                attempts += 1;
                attempts > 1
            },
            || {},
        );
        assert!(matches!(result, Err(ConfigError::LockCancelled)));
    }
}
