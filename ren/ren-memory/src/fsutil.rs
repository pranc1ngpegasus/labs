use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ulid::Ulid;

use crate::error::{MemoryError, Result};

pub fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| MemoryError::io(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| MemoryError::io(path, error))?;
    }
    Ok(())
}

pub fn write_atomic_replace(
    path: &Path,
    content: &[u8],
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::InvalidConfig("output path has no parent".into()))?;
    create_private_dir(parent)?;
    let temporary = temporary_path(parent, path);
    let result = (|| {
        let mut file = private_file(&temporary)?;
        file.write_all(content)
            .map_err(|error| MemoryError::io(&temporary, error))?;
        file.sync_all()
            .map_err(|error| MemoryError::io(&temporary, error))?;
        fs::rename(&temporary, path).map_err(|error| MemoryError::io(path, error))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn publish_new(
    path: &Path,
    content: &[u8],
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::InvalidConfig("output path has no parent".into()))?;
    create_private_dir(parent)?;
    let temporary = temporary_path(parent, path);
    let result = (|| {
        let mut file = private_file(&temporary)?;
        file.write_all(content)
            .map_err(|error| MemoryError::io(&temporary, error))?;
        file.sync_all()
            .map_err(|error| MemoryError::io(&temporary, error))?;
        fs::hard_link(&temporary, path).map_err(|error| MemoryError::io(path, error))?;
        fs::remove_file(&temporary).map_err(|error| MemoryError::io(&temporary, error))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn open_private_lock(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::InvalidConfig("lock path has no parent".into()))?;
    create_private_dir(parent)?;
    private_options()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| MemoryError::io(path, error))
}

fn private_file(path: &Path) -> Result<File> {
    private_options()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| MemoryError::io(path, error))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn temporary_path(
    parent: &Path,
    destination: &Path,
) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("memory");
    parent.join(format!(".{name}.{}.tmp", Ulid::generate()))
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory = File::open(path).map_err(|error| MemoryError::io(path, error))?;
    directory
        .sync_all()
        .map_err(|error| MemoryError::io(path, error))
}
