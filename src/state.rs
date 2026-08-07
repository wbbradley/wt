use std::collections::BTreeSet;
#[cfg(not(test))]
use std::env;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::model::CanonicalPullRequestId;

#[cfg(not(test))]
pub const STATE_PATH_ENV: &str = "WT_STATE_PATH";
const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PersistentState {
    pub version: u32,
    #[serde(default)]
    pub backburner: BTreeSet<CanonicalPullRequestId>,
}

impl Default for PersistentState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            backburner: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("cannot read state {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot parse state {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("state schema version {found} is newer than supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    #[error("cannot write state {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("cannot encode state: {0}")]
    Encode(#[from] serde_json::Error),
}

pub fn path(catalog_path: &Path) -> PathBuf {
    #[cfg(test)]
    {
        catalog_path.with_extension("state.json")
    }
    #[cfg(not(test))]
    {
        if let Some(path) = env::var_os(STATE_PATH_ENV) {
            return PathBuf::from(path);
        }
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .map(|root| root.join("wt/state.json"))
            .unwrap_or_else(|| catalog_path.with_extension("state.json"))
    }
}

pub fn load(path: &Path) -> Result<PersistentState, StateError> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PersistentState::default());
        }
        Err(source) => {
            return Err(StateError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let state = serde_json::from_slice::<PersistentState>(&contents).map_err(|source| {
        StateError::Parse {
            path: path.to_owned(),
            source,
        }
    })?;
    if state.version > STATE_VERSION {
        return Err(StateError::FutureVersion {
            found: state.version,
            supported: STATE_VERSION,
        });
    }
    Ok(state)
}

pub fn save(path: &Path, state: &PersistentState) -> Result<(), StateError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| StateError::Write {
        path: path.to_owned(),
        source,
    })?;
    let temporary = NamedTempFile::new_in(parent).map_err(|source| StateError::Write {
        path: path.to_owned(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| StateError::Write {
                path: path.to_owned(),
                source,
            })?;
    }
    let mut writer = BufWriter::new(temporary.as_file());
    serde_json::to_writer_pretty(&mut writer, state)?;
    writer.flush().map_err(|source| StateError::Write {
        path: path.to_owned(),
        source,
    })?;
    drop(writer);
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| StateError::Write {
            path: path.to_owned(),
            source,
        })?;
    temporary.persist(path).map_err(|error| StateError::Write {
        path: path.to_owned(),
        source: error.error,
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| StateError::Write {
            path: path.to_owned(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GitHubRepositoryIdentity;

    fn identity(host: &str) -> CanonicalPullRequestId {
        CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical(host, "team", "project"),
            number: 7,
        }
    }

    #[test]
    fn round_trip_is_atomic_host_aware_and_creates_parents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/state.json");
        let state = PersistentState {
            version: STATE_VERSION,
            backburner: BTreeSet::from([identity("github.com"), identity("github.example.com")]),
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);
        let replacement = PersistentState::default();
        save(&path, &replacement).unwrap();
        assert_eq!(load(&path).unwrap(), replacement);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn missing_is_empty_and_corrupt_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        assert_eq!(load(&path).unwrap(), PersistentState::default());
        fs::write(&path, b"not json").unwrap();
        assert!(matches!(load(&path), Err(StateError::Parse { .. })));
    }
}
