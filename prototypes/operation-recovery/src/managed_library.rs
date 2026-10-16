use crate::{
    digest, fail,
    library_db::{AssetRecord, LibraryStore},
    Result,
};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MARKER: &str = ".teacell-library-id";

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Classification {
    Verified,
    ExternalPending,
    Unknown,
    Offline,
    WrongLibrary,
}

pub fn mark_fixture_root(root: &Path, id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 || id.contains(['\n', '\r', '/', '\\']) {
        return fail("INVALID_LIBRARY_ID");
    }
    fs::create_dir_all(root)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(MARKER))?;
    file.write_all(id.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn regular_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return fail("INVALID_RELATIVE_PATH");
    }
    let mut path = root.to_path_buf();
    if is_link(&fs::symlink_metadata(root)?) {
        return fail("LINK_REJECTED");
    }
    for component in relative.components() {
        path.push(component);
        if is_link(&fs::symlink_metadata(&path)?) {
            return fail("LINK_REJECTED");
        }
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root.canonicalize()?) || !canonical.is_file() {
        return fail("PATH_ESCAPED");
    }
    Ok(path)
}

fn verify_root(root: &Path, id: &str) -> Result<()> {
    if !root.is_dir() {
        return fail("ROOT_OFFLINE");
    }
    let marker = regular_path(root, Path::new(MARKER))?;
    if fs::metadata(&marker)?.len() > 128 || fs::read_to_string(marker)? != id {
        return fail("WRONG_LIBRARY");
    }
    Ok(())
}

pub fn classify(store: &LibraryStore, record: &AssetRecord) -> Result<Classification> {
    let (id, root, _) = store.locator()?;
    if !root.is_dir() {
        return Ok(Classification::Offline);
    }
    if verify_root(&root, &id).is_err() {
        return Ok(Classification::WrongLibrary);
    }
    if !record.trusted_receipt {
        if record.location.is_absolute() {
            if let Ok(path) = record.location.canonicalize() {
                if !path.starts_with(root.canonicalize()?) {
                    return Ok(Classification::ExternalPending);
                }
            }
        }
        return Ok(Classification::Unknown);
    }
    let Ok(path) = asset_path(&root, &record.location) else {
        return Ok(Classification::Unknown);
    };
    if digest(&path)? != record.digest {
        return Ok(Classification::Unknown);
    }
    Ok(Classification::Verified)
}

// Trusted legacy maintenance fixture only; no filesystem deletion or Agent tool.
pub fn prepare_external_unlink(store: &LibraryStore, asset: i64) -> Result<i64> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let record = store
        .records()?
        .into_iter()
        .find(|record| record.id == asset)
        .ok_or("ASSET_NOT_FOUND")?;
    if classify(store, &record)? != Classification::ExternalPending {
        return fail("LEGACY_SCOPE_UNVERIFIED");
    }
    store.save_external_unlink(&record)
}

pub fn submit_external_unlink(
    store: &mut LibraryStore,
    plan_id: i64,
    expected_epoch: i64,
    fault: &str,
) -> Result<()> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let plan = store.external_unlink_fixture(plan_id)?;
    if plan.epoch != expected_epoch {
        return fail("LIBRARY_EPOCH_CHANGED");
    }
    if plan.state == "complete" {
        return Ok(());
    }
    let (id, root, epoch) = store.locator()?;
    if id != plan.library_id || root != plan.root || epoch != plan.epoch {
        return fail("LIBRARY_EPOCH_CHANGED");
    }
    if classify(store, &plan.record)? != Classification::ExternalPending {
        return fail("LEGACY_SCOPE_UNVERIFIED");
    }
    if fault == "unlink-before-commit" {
        std::process::exit(77);
    }
    store.finish_external_unlink(plan_id, expected_epoch)?;
    if fault == "unlink-after-commit" {
        std::process::exit(77);
    }
    Ok(())
}

fn verify_records(store: &LibraryStore, root: &Path, id: &str) -> Result<()> {
    verify_root(root, id)?;
    for record in store.records()?.iter().filter(|r| r.trusted_receipt) {
        if digest(&asset_path(root, &record.location)?)? != record.digest {
            return fail("ASSET_CHANGED");
        }
    }
    Ok(())
}

fn asset_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    if matches!(relative.components().next(),Some(Component::Normal(name)) if name==MARKER || name==".migration-stage" || name==".migration-source" || name==".legacy-stage")
    {
        return fail("RESERVED_PATH");
    }
    regular_path(root, relative)
}

// Reject already-present links before creating directories or publishing. This
// fixture check does not claim to close hostile check-to-use races; production
// must use the platform handle strategy frozen in A1.
fn destination_parent(root: &Path, relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return fail("INVALID_RELATIVE_PATH");
    }
    let canonical_root = root.canonicalize()?;
    let mut path = root.to_path_buf();
    if is_link(&fs::symlink_metadata(&path)?) {
        return fail("LINK_REJECTED");
    }
    for component in relative.parent().ok_or("INVALID_DESTINATION")?.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&path)?;
        if is_link(&metadata) {
            return fail("LINK_REJECTED");
        }
        if !metadata.is_dir() || !path.canonicalize()?.starts_with(&canonical_root) {
            return fail("PATH_ESCAPED");
        }
    }
    Ok(())
}

// Trusted maintenance entry, never an Agent tool. No source deletion is implemented here.
pub fn relocate_already_moved(store: &mut LibraryStore, target: &Path) -> Result<()> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let (id, _, epoch) = store.locator()?;
    let target = target.canonicalize()?;
    verify_records(store, &target, &id)?;
    if store.has_copy()? {
        return fail("MAINTENANCE_CONFLICT");
    }
    store.switch(&target, epoch)
}

pub fn migrate_copy(store: &mut LibraryStore, target: &Path, stop_after: usize) -> Result<bool> {
    migrate_copy_fault(store, target, stop_after, "")
}

pub fn migrate_copy_fault(
    store: &mut LibraryStore,
    target: &Path,
    stop_after: usize,
    fault: &str,
) -> Result<bool> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let (id, source, epoch) = store.locator()?;
    verify_records(store, &source, &id)?;
    let target = target.canonicalize()?;
    let source = source.canonicalize()?;
    if source == target && store.copy_switched_to(&target, epoch)? {
        return Ok(true);
    }
    if source.starts_with(&target) || target.starts_with(&source) {
        return fail("OVERLAPPING_ROOTS");
    }
    if !store.has_copy()? {
        if fs::read_dir(&target)?.next().is_some() {
            return fail("DESTINATION_NOT_EMPTY");
        }
        store.start_copy(&source, &target, epoch)?;
    } else {
        store.start_copy(&source, &target, epoch)?;
    }
    if !target.join(MARKER).exists() {
        mark_fixture_root(&target, &id)?;
    }
    verify_root(&target, &id)?;
    let stage_dir = target.join(".migration-stage");
    fs::create_dir_all(&stage_dir)?;
    if is_link(&fs::symlink_metadata(&stage_dir)?) {
        return fail("LINK_REJECTED");
    }
    let records: Vec<_> = store
        .records()?
        .into_iter()
        .filter(|r| r.trusted_receipt)
        .collect();
    for (index, record) in records.iter().enumerate() {
        if index >= stop_after {
            return Ok(false);
        }
        let from = asset_path(&source, &record.location)?;
        let source_stage = source.join(".migration-source");
        fs::create_dir_all(&source_stage)?;
        if is_link(&fs::symlink_metadata(&source_stage)?) {
            return fail("LINK_REJECTED");
        }
        let source_witness = source_stage.join(format!("{}.identity", record.id));
        if !source_witness.exists() {
            fs::hard_link(&from, &source_witness)?;
        }
        if is_link(&fs::symlink_metadata(&source_witness)?)
            || !same_file::is_same_file(&from, &source_witness)?
        {
            return fail("SOURCE_IDENTITY_CHANGED");
        }
        let destination = target.join(&record.location);
        destination_parent(&target, &record.location)?;
        let stage = stage_dir.join(record.id.to_string());
        let witness = stage_dir.join(format!("{}.identity", record.id));
        if stage.exists()
            && (!witness.exists()
                || is_link(&fs::symlink_metadata(&stage)?)
                || is_link(&fs::symlink_metadata(&witness)?)
                || !same_file::is_same_file(&stage, &witness)?)
        {
            return fail("STAGING_IDENTITY_UNKNOWN");
        }
        if destination.exists() {
            if is_link(&fs::symlink_metadata(&destination)?)
                || !stage.exists()
                || !same_file::is_same_file(&stage, &destination)?
                || digest(&destination)? != record.digest
            {
                return fail("DESTINATION_CONFLICT");
            }
            continue;
        }
        if !stage.exists() || digest(&stage)? != record.digest {
            let mut output = if !stage.exists() {
                let output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stage)?;
                fs::hard_link(&stage, &witness)?;
                output
            } else {
                if !witness.exists()
                    || is_link(&fs::symlink_metadata(&stage)?)
                    || !same_file::is_same_file(&stage, &witness)?
                {
                    return fail("STAGING_IDENTITY_UNKNOWN");
                }
                fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&stage)?
            };
            let mut input = fs::File::open(from)?;
            let mut buffer = [0u8; 8];
            let read = input.read(&mut buffer)?;
            output.write_all(&buffer[..read])?;
            output.sync_all()?;
            if fault == "migration-partial" {
                std::process::exit(77);
            }
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        if digest(&stage)? != record.digest {
            return fail("COPY_INCOMPLETE");
        }
        verify_root(&target, &id)?;
        destination_parent(&target, &record.location)?;
        fs::hard_link(&stage, &destination)?;
        if fault == "migration-published" {
            std::process::exit(77);
        }
    }
    verify_records(store, &target, &id)?;
    if fault == "migration-before-switch" {
        std::process::exit(77);
    }
    store.switch(&target, epoch)?;
    if fault == "migration-switched" {
        std::process::exit(77);
    }
    Ok(true)
}

// Trusted maintenance only. Preparing never removes source files; approval is a
// separate fixture call. Production authorization/path locking is not implemented.
pub fn prepare_source_cleanup(store: &LibraryStore) -> Result<usize> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let (source, target, epoch) = store.switched_source()?;
    let (id, current, current_epoch) = store.locator()?;
    if current != target || current_epoch != epoch {
        return fail("LIBRARY_EPOCH_CHANGED");
    }
    verify_records(store, &target, &id)?;
    verify_root(&source, &id)?;
    let records: Vec<_> = store
        .records()?
        .into_iter()
        .filter(|record| record.trusted_receipt)
        .collect();
    let mut locations = std::collections::HashSet::new();
    if records
        .iter()
        .any(|record| !locations.insert(&record.location))
    {
        return fail("CLEANUP_SCOPE_AMBIGUOUS");
    }
    for record in &records {
        verify_copy_target(&target, record)?;
        let path = asset_path(&source, &record.location)?;
        let witness = regular_path(
            &source,
            Path::new(&format!(".migration-source/{}.identity", record.id)),
        )?;
        if !same_file::is_same_file(&path, &witness)? || digest(&path)? != record.digest {
            return fail("SOURCE_IDENTITY_CHANGED");
        }
    }
    store.create_cleanup(&source, &target, epoch, &records)?;
    Ok(records.len())
}

pub fn cleanup_source_fault(
    store: &mut LibraryStore,
    expected_epoch: i64,
    fault: &str,
) -> Result<()> {
    let _maintenance = store.maintenance_guard_fixture()?;
    let plan = store.cleanup_plan()?;
    let (id, current, epoch) = store.locator()?;
    if current != plan.target || epoch != plan.epoch || epoch != expected_epoch {
        return fail("LIBRARY_EPOCH_CHANGED");
    }
    verify_root(&plan.target, &id)?;
    verify_root(&plan.source, &id)?;
    if store
        .records()?
        .into_iter()
        .filter(|record| record.trusted_receipt)
        .collect::<Vec<_>>()
        != plan.records
    {
        return fail("CLEANUP_SCOPE_CHANGED");
    }
    for record in &plan.records {
        verify_copy_target(&plan.target, record)?;
    }
    if plan.state == "complete" {
        return Ok(());
    }
    store.begin_cleanup()?;
    for record in &plan.records {
        let state = store.cleanup_item(record.id)?;
        if state == "complete" {
            continue;
        }
        // A target that goes offline between items must stop the next unlink.
        verify_root(&plan.target, &id)?;
        verify_copy_target(&plan.target, record)?;
        verify_root(&plan.source, &id)?;
        let raw_witness = plan
            .source
            .join(format!(".migration-source/{}.identity", record.id));
        let raw_source = plan.source.join(&record.location);
        if state == "removed"
            && matches!(fs::symlink_metadata(&raw_witness), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            && matches!(fs::symlink_metadata(&raw_source), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            // The durable removed receipt predates witness removal; absence now
            // completes that step without touching any newly appeared path.
            store.cleanup_item_state(record.id, "complete")?;
            continue;
        }
        let witness = regular_path(
            &plan.source,
            Path::new(&format!(".migration-source/{}.identity", record.id)),
        )?;
        if digest(&witness)? != record.digest {
            return fail("SOURCE_IDENTITY_CHANGED");
        }
        let path = plan.source.join(&record.location);
        if path.try_exists()? {
            let path = asset_path(&plan.source, &record.location)?;
            if !same_file::is_same_file(&path, &witness)? {
                return fail("SOURCE_IDENTITY_CHANGED");
            }
            store.cleanup_item_state(record.id, "intent")?;
            if fault == "cleanup-intent" {
                std::process::exit(77);
            }
            fs::remove_file(path)?;
            if fault == "cleanup-unlinked" {
                std::process::exit(77);
            }
        } else if state != "intent" && state != "removed" {
            return fail("SOURCE_OUTCOME_UNKNOWN");
        }
        store.cleanup_item_state(record.id, "removed")?;
        if fault == "cleanup-receipt" {
            std::process::exit(77);
        }
        // Keep the identity witness until after the removed receipt. If an unknown
        // actor removes this witness before its durable final receipt, fail closed.
        fs::remove_file(witness)?;
        if fault == "cleanup-witness" {
            std::process::exit(77);
        }
        store.cleanup_item_state(record.id, "complete")?;
    }
    store.finish_cleanup()
}

fn verify_copy_target(target: &Path, record: &AssetRecord) -> Result<()> {
    let destination = asset_path(target, &record.location)?;
    let stage = regular_path(
        target,
        Path::new(&format!(".migration-stage/{}", record.id)),
    )?;
    let witness = regular_path(
        target,
        Path::new(&format!(".migration-stage/{}.identity", record.id)),
    )?;
    if !same_file::is_same_file(&destination, &stage)?
        || !same_file::is_same_file(&stage, &witness)?
        || digest(&destination)? != record.digest
    {
        return fail("DESTINATION_CHANGED");
    }
    Ok(())
}
