//! Trusted legacy maintenance experiment. Never a generic import/Agent tool.
use crate::{
    digest, fail,
    library_db::LibraryStore,
    managed_library::{classify, Classification},
    Result,
};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
};

fn identity(file: &fs::File) -> Result<String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(format!(
            "windows:{}:{}:{}:{}:{}",
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
            info.ftCreationTime.dwHighDateTime,
            info.ftCreationTime.dwLowDateTime
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(format!(
            "unix:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        fail("PLATFORM_UNSUPPORTED")
    }
}
fn relative_target(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return fail("INVALID_RELATIVE_PATH");
    }
    if let Some(Component::Normal(name)) = relative.components().next() {
        if matches!(
            name.to_str().ok_or("NON_UTF8_PATH")?,
            ".teacell-library-id" | ".migration-stage" | ".migration-source" | ".legacy-stage"
        ) {
            return fail("INVALID_RELATIVE_PATH");
        }
    }
    Ok(())
}
fn reject_links(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        let metadata = fs::symlink_metadata(part)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return fail("LINK_REJECTED");
            }
        }
        #[cfg(not(windows))]
        if metadata.file_type().is_symlink() {
            return fail("LINK_REJECTED");
        }
    }
    Ok(())
}

pub fn prepare_return(store: &LibraryStore, asset: i64, relative: &Path) -> Result<i64> {
    let _maintenance = store.maintenance_guard_fixture()?;
    relative_target(relative)?;
    let record = store
        .records()?
        .into_iter()
        .find(|record| record.id == asset)
        .ok_or("ASSET_NOT_FOUND")?;
    if classify(store, &record)? != Classification::ExternalPending {
        return fail("LEGACY_SCOPE_UNVERIFIED");
    }
    let (_, root, _) = store.locator()?;
    reject_links(&root)?;
    reject_links(&record.location)?;
    if root.join(relative).try_exists()? || fs::symlink_metadata(root.join(relative)).is_ok() {
        return fail("DESTINATION_CONFLICT");
    }
    let parent = root
        .join(relative)
        .parent()
        .ok_or("INVALID_RELATIVE_PATH")?
        .to_path_buf();
    reject_links(&parent)?; // fixture requires an existing destination collection.
    let source = fs::File::open(&record.location)?;
    if !source.metadata()?.is_file() || digest(&record.location)? != record.digest {
        return fail("SOURCE_CHANGED");
    }
    store.save_external_return(&record, relative, &identity(&source)?)
}

pub fn execute_return(
    store: &mut LibraryStore,
    plan_id: i64,
    expected_epoch: i64,
    fault: &str,
) -> Result<()> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let plan = store.external_return_fixture(plan_id)?;
    let (library, root, epoch) = store.locator()?;
    if epoch != expected_epoch
        || epoch != plan.epoch
        || library != plan.library_id
        || root != plan.root
    {
        return fail("LIBRARY_EPOCH_CHANGED");
    }
    relative_target(&plan.relative)?;
    reject_links(&root)?;
    let marker = root.join(".teacell-library-id");
    reject_links(&marker)?;
    if fs::metadata(&marker)?.len() > 128 || fs::read_to_string(marker)? != library {
        return fail("WRONG_LIBRARY");
    }
    let destination = root.join(&plan.relative);
    reject_links(destination.parent().ok_or("INVALID_RELATIVE_PATH")?)?;
    let stage_dir = root.join(".legacy-stage");
    let stage = stage_dir.join(format!("{plan_id}.stage"));
    let witness = stage_dir.join(format!("{plan_id}.identity"));
    if plan.state == "complete" {
        reject_links(&destination)?;
        reject_links(&stage)?;
        if !same_file::is_same_file(&stage, &destination)?
            || digest(&destination)? != plan.record.digest
        {
            return fail("DESTINATION_CHANGED");
        }
        return Ok(());
    }
    store.begin_external_return(plan_id, expected_epoch)?;
    reject_links(&plan.record.location)?;
    let mut input = fs::File::open(&plan.record.location)?;
    if identity(&input)? != plan.identity {
        return fail("SOURCE_CHANGED");
    }
    fs::create_dir_all(&stage_dir)?;
    reject_links(&stage_dir)?;
    if stage.try_exists()? {
        reject_links(&stage)?;
        reject_links(&witness)?;
        if !same_file::is_same_file(&stage, &witness)? {
            return fail("STAGING_IDENTITY_UNKNOWN");
        }
    } else {
        let stage_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
        fs::hard_link(&stage, &witness)?;
        stage_file.sync_all()?;
    }
    if digest(&stage)? != plan.record.digest {
        if fs::symlink_metadata(&destination).is_ok() {
            return fail("DESTINATION_CONFLICT");
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&stage)?;
        let mut prefix = [0u8; 4];
        let count = input.read(&mut prefix)?;
        output.write_all(&prefix[..count])?;
        output.sync_all()?;
        if fault == "return-partial" {
            std::process::exit(77);
        }
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
    }
    if digest(&stage)? != plan.record.digest {
        return fail("SOURCE_CHANGED");
    }
    if fs::symlink_metadata(&destination).is_ok() {
        reject_links(&destination)?;
        if !same_file::is_same_file(&stage, &destination)? {
            return fail("DESTINATION_CONFLICT");
        }
    } else {
        fs::hard_link(&stage, &destination)?;
    }
    if fault == "return-published" {
        std::process::exit(77);
    }
    store.finish_external_return(plan_id, expected_epoch)?;
    if fault == "return-registered" {
        std::process::exit(77);
    }
    Ok(())
}
