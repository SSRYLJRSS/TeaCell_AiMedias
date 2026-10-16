pub mod db;
pub mod legacy_return;
pub mod library_db;
pub mod maintenance_guard;
pub mod managed_library;
pub mod workflow;
pub mod workflow_db;

use db::{Input, Store};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(crate) fn fail<T>(message: &str) -> Result<T> {
    Err(message.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    TrustedGui,
    Agent,
}

pub fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn prepare(store: &Store, sources: &[PathBuf], granted_directory: &Path) -> Result<i64> {
    let directory = granted_directory.canonicalize()?;
    let mut inputs = Vec::new();
    for path in sources {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return fail("LINK_REJECTED");
        }
        let source = path.canonicalize()?;
        if !source.starts_with(&directory) || !source.is_file() {
            return fail("DIRECTORY_NOT_GRANTED");
        }
        inputs.push(Input {
            digest: digest(&source)?,
            source,
        });
    }
    if inputs.is_empty() {
        return fail("EMPTY_PLAN");
    }
    store.prepare(&inputs)
}

// Test-only crash hook. A CLI subprocess exits abruptly; no unwinding or SQLite cleanup.
fn checkpoint(fault: &str, point: &str) {
    if fault == point {
        std::process::exit(77);
    }
}

pub fn execute(
    store: &mut Store,
    operation: i64,
    root: &Path,
    fault: &str,
    limit: usize,
) -> Result<()> {
    let directory = root.join(format!("op-{operation}"));
    fs::create_dir_all(&directory)?;
    let inputs = store.inputs(operation)?;
    for (step, input) in inputs.iter().enumerate().take(limit) {
        if store.cancelled(operation)? {
            return fail("CANCELLED");
        }
        let state = store.state(operation, step)?;
        let stage = directory.join(format!("{step}.stage"));
        let target = directory.join(format!("{step}.asset"));
        if state == "registered" {
            if digest(&target)? != input.digest {
                return fail("REGISTERED_FILE_CHANGED");
            }
            continue;
        }
        // Filesystem work occurs outside every SQLite transaction.
        if state == "pending" {
            if target.exists() {
                return fail("UNOWNED_DESTINATION");
            }
            if digest(&input.source)? != input.digest {
                return fail("PLAN_STALE");
            }
            let mut source = fs::File::open(&input.source)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&stage)?;
            let mut buffer = [0u8; 8192];
            let read = source.read(&mut buffer)?;
            output.write_all(&buffer[..read])?;
            output.sync_all()?;
            checkpoint(fault, "partial-copy");
            std::io::copy(&mut source, &mut output)?;
            output.sync_all()?;
            drop(output);
            if digest(&stage)? != input.digest {
                return fail("PLAN_STALE");
            }
            store.staged(operation, step)?;
        }
        if digest(&stage)? != input.digest {
            return fail("STAGING_CHANGED");
        }
        if target.exists() {
            // Equal content alone cannot establish ownership: require the retained hard-link identity.
            if !same_file::is_same_file(&stage, &target)? {
                return fail("UNOWNED_DESTINATION");
            }
        } else {
            fs::hard_link(&stage, &target)?; // Atomic create, never replaces an existing file.
        }
        checkpoint(fault, "published");
        store.register(operation, step, &input.digest)?;
        checkpoint(fault, "registered");
    }
    if store.completed_count(operation)? > 0 {
        store.batch(operation)?;
        checkpoint(fault, "batch-created");
    }
    Ok(())
}

// No upstream idempotency/reconciliation: retain uncertainty rather than sending twice.
pub fn request_external_once<F: FnOnce() -> Result<()>>(
    store: &Store,
    operation: i64,
    fault: &str,
    request: F,
) -> Result<bool> {
    if !store.begin_external_once(operation)? {
        return Ok(false);
    }
    request()?; // Outside SQLite transactions; failure remains outcome_unknown.
    checkpoint(fault, "external-replied");
    store.confirm_external(operation)?;
    Ok(true)
}
