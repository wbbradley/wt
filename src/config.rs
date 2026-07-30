use std::env;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

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
}
