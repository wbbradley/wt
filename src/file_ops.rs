use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn resolve_destination(
    source: &Path,
    target: &Path,
    directory_hint: bool,
) -> io::Result<PathBuf> {
    let is_directory = match fs::metadata(target) {
        Ok(metadata) => metadata.is_dir(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => directory_hint,
        Err(error) => return Err(error),
    };
    let destination = if is_directory {
        let filename = source
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no filename"))?;
        target.join(filename)
    } else {
        target.to_owned()
    };
    validate_move(source, &destination)?;
    Ok(destination)
}

fn validate_move(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "select a file to move",
        ));
    }
    let directory = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    match fs::metadata(directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "destination folder is not a directory: {}",
                    directory.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("destination already exists: {}", destination.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Move to the exact path shown in the confirmation, without resolving it again.
pub fn move_file(source: &Path, destination: &Path) -> io::Result<()> {
    validate_move(source, destination)?;
    let directory = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        move_symlink(source, destination)?;
    } else {
        // Linking keeps permissions and contents intact and refuses an existing
        // destination, including one created after the confirmation.
        match fs::hard_link(source, destination) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                // Stage a copy in the destination filesystem, publish it without
                // overwriting anything, then remove the original below.
                copy_file(source, directory, destination)?;
            }
            Err(error) => return Err(error),
        }
    }
    fs::remove_file(source).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "created {}, but could not remove {}: {error}",
                destination.display(),
                source.display()
            ),
        )
    })
}

fn copy_file(source: &Path, directory: &Path, destination: &Path) -> io::Result<()> {
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    fs::copy(source, temporary.path())?;
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(unix)]
fn move_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_moves_resolve_folders_and_create_parents_for_new_filenames() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("file with spaces");
        let directory = temporary.path().join("new/nested");
        fs::write(&source, "contents").unwrap();
        let renamed = directory.join("renamed file");
        let destination = resolve_destination(&source, &renamed, false).unwrap();
        assert_eq!(destination, renamed);
        assert!(!directory.exists());
        assert!(source.exists());
        move_file(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "contents");

        fs::write(&source, "second file").unwrap();
        let destination = resolve_destination(&source, &directory, false).unwrap();
        assert_eq!(destination, directory.join(source.file_name().unwrap()));
        move_file(&source, &destination).unwrap();
        fs::write(&source, "third file").unwrap();
        assert_eq!(
            resolve_destination(&source, &directory, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(move_file(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "third file");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "second file");
        assert!(resolve_destination(&source, &renamed, false).is_err());
        assert!(move_file(&directory, &temporary.path().join("invalid")).is_err());
    }

    #[test]
    fn explicit_folders_create_parents_and_confirmed_paths_never_change_meaning() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::write(&source, "contents").unwrap();
        let folder = temporary.path().join("new/folder");
        let destination = resolve_destination(&source, &folder, true).unwrap();
        assert_eq!(destination, folder.join("source"));
        assert!(!folder.exists());
        // A folder appearing at the confirmed file path must not redirect the move.
        fs::create_dir_all(&destination).unwrap();
        assert!(move_file(&source, &destination).is_err());
        assert!(source.exists());
        assert!(!destination.join("source").exists());
        fs::remove_dir(&destination).unwrap();
        move_file(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "contents");
    }

    #[test]
    fn cross_device_copy_preserves_content_and_never_replaces_a_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::write(&source, "contents").unwrap();
        copy_file(&source, temporary.path(), &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "contents");
        fs::write(&source, "new contents").unwrap();
        assert!(copy_file(&source, temporary.path(), &destination).is_err());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "contents");
        assert_eq!(fs::read_to_string(&source).unwrap(), "new contents");
    }

    #[cfg(unix)]
    #[test]
    fn file_moves_preserve_links_and_permissions_and_reject_dangling_collisions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("destination");
        fs::create_dir(&directory).unwrap();
        let target = temporary.path().join("target");
        fs::write(&target, "keep").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
        let source = temporary.path().join("link");
        symlink(&target, &source).unwrap();
        move_file(&source, &directory.join("link")).unwrap();
        assert_eq!(fs::read_link(directory.join("link")).unwrap(), target);
        assert!(fs::symlink_metadata(&source).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");

        symlink("missing", &source).unwrap();
        assert!(move_file(&source, &directory.join("link")).is_err());
        fs::remove_file(directory.join("link")).unwrap();
        move_file(&source, &directory.join("link")).unwrap();
        assert_eq!(
            fs::read_link(directory.join("link")).unwrap(),
            Path::new("missing")
        );
        assert!(fs::symlink_metadata(&source).is_err());

        symlink("missing", directory.join("target")).unwrap();
        assert!(resolve_destination(&target, &directory, false).is_err());
        assert!(move_file(&target, &directory.join("target")).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        fs::remove_file(directory.join("target")).unwrap();
        move_file(&target, &directory.join("target")).unwrap();
        assert_eq!(
            fs::metadata(directory.join("target"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }
}

#[cfg(windows)]
fn move_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    let target = fs::read_link(source)?;
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}
