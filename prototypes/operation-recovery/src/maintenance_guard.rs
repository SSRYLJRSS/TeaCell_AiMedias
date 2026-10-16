//! Cooperative maintenance guard for a canonical fixture DB path. Never delete
//! the lock file: replacement would create a different lock object on Unix.
use crate::{fail, Result};
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

pub struct MaintenanceGuard {
    _file: File,
}
pub fn acquire(database: &Path) -> Result<MaintenanceGuard> {
    let mut name = database.as_os_str().to_os_string();
    name.push(".maintenance.lock");
    let path = std::path::PathBuf::from(name);
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return fail("MAINTENANCE_LOCK_INVALID");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return fail("MAINTENANCE_LOCK_INVALID");
            }
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Cooperating holders can open the same file, but cannot unlink it.
        options.share_mode(
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
        );
    }
    let file = options.open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(MaintenanceGuard { _file: file }),
        Err(TryLockError::WouldBlock) => fail("MAINTENANCE_BUSY"),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}
