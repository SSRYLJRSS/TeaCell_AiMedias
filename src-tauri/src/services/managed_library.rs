//! Startup/root inspection, controlled import evidence, and background
//! classification for the single managed library.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::db::libraries::{
    self, LibraryHealth, LibrarySnapshot, MarkerState, PendingClassification,
};
use crate::error::{AppError, AppResult};
use crate::state::Database;
use crate::utils::file_identity::file_identity;
use crate::utils::hash::sha256_full;

pub const ROOT_MARKER: &str = ".teacell-library-id";
const RESERVED_TOP_LEVEL: [&str; 5] = [
    ROOT_MARKER,
    ".migration-stage",
    ".migration-source",
    ".legacy-stage",
    ".import-stage",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFileEvidence {
    pub relative_path: String,
    pub file_identity: String,
    pub content_sha256: String,
    pub ownership_receipt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInitialRoot {
    pub canonical_root: String,
    pub root_identity: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub inspected: usize,
    pub classified: usize,
}

enum RootInspection {
    Unconfigured,
    Offline,
    WrongRoot,
    Ready {
        canonical_root: PathBuf,
        root_identity: String,
    },
}

/// Inspect and open the configured root before ordinary commands are exposed.
/// This reads/writes only the root marker; asset verification stays background.
pub fn initialize_runtime(conn: &rusqlite::Connection) -> AppResult<LibrarySnapshot> {
    let snapshot =
        libraries::current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"))?;
    // Only the one-time legacy candidate may create a missing marker. A root that
    // was previously ready or restored must not silently adopt a replacement
    // directory at the same path.
    let may_create_marker = snapshot.health == LibraryHealth::Unverified
        && snapshot.marker_state == MarkerState::InitialCandidate;
    let inspection = match inspect_root(&snapshot, may_create_marker) {
        Ok(inspection) => inspection,
        Err(error) => {
            // Any inspection IO/error path must fail closed. Leaving the
            // previous `ready` projection in place would keep verified assets
            // visible even though startup could not re-establish root health.
            let _ = libraries::set_health(conn, &snapshot, LibraryHealth::Offline);
            return Err(error);
        }
    };
    match inspection {
        RootInspection::Unconfigured => {
            if snapshot.health == LibraryHealth::Unconfigured {
                Ok(snapshot)
            } else {
                libraries::set_health(conn, &snapshot, LibraryHealth::Unconfigured)
            }
        }
        RootInspection::Offline => {
            if snapshot.health == LibraryHealth::Offline {
                Ok(snapshot)
            } else {
                libraries::set_health(conn, &snapshot, LibraryHealth::Offline)
            }
        }
        RootInspection::WrongRoot => {
            if snapshot.health == LibraryHealth::WrongRoot {
                Ok(snapshot)
            } else {
                libraries::set_health(conn, &snapshot, LibraryHealth::WrongRoot)
            }
        }
        RootInspection::Ready {
            canonical_root,
            root_identity,
        } => {
            let canonical_root = crate::utils::path::encode_native_path(&canonical_root)?;
            let unchanged = snapshot.health == LibraryHealth::Ready
                && snapshot.root_path == canonical_root
                && snapshot.root_identity.as_deref() == Some(root_identity.as_str());
            if unchanged {
                Ok(snapshot)
            } else {
                libraries::mark_root_ready(conn, &snapshot, &canonical_root, &root_identity)
            }
        }
    }
}

/// Inspect and mark an initial root without holding a database connection.
/// The caller must commit the returned evidence with the same snapshot and a
/// new short database lock; a stale preparation fails its compare-and-swap.
pub fn prepare_initial_root(
    snapshot: &LibrarySnapshot,
    requested_root: &Path,
) -> AppResult<PreparedInitialRoot> {
    if snapshot.health != LibraryHealth::Unconfigured || !snapshot.root_path.trim().is_empty() {
        return Err(AppError::conflict(
            "总仓已配置，请在受控的总仓迁移或重新定位流程中更换位置",
        ));
    }
    if !requested_root.is_absolute() {
        return Err(AppError::invalid_arg("总仓位置必须是绝对路径"));
    }
    let candidate = LibrarySnapshot {
        root_path: crate::utils::path::encode_native_path(requested_root)?,
        ..snapshot.clone()
    };
    match inspect_root(&candidate, true)? {
        RootInspection::Ready {
            canonical_root,
            root_identity,
        } => Ok(PreparedInitialRoot {
            canonical_root: crate::utils::path::encode_native_path(&canonical_root)?,
            root_identity,
        }),
        RootInspection::Unconfigured => Err(AppError::invalid_arg("请选择总仓目录")),
        RootInspection::Offline => Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总仓目录不存在或无法访问",
        )),
        RootInspection::WrongRoot => {
            Err(AppError::conflict("所选目录已有其他茶馆总仓标记，不能覆盖"))
        }
    }
}

/// Inspect a *new* root for a controlled whole-library move (A4-05).
///
/// This is deliberately different from [`prepare_initial_root`], which only
/// accepts an unconfigured library. Two behaviours are required by the card:
///
/// - `create_marker = true` (software migration): the target is expected to be a
///   fresh directory, so writing our own marker there is how we claim it. A
///   directory that already carries a *different* library's marker is refused —
///   the user pointed at somebody else's library.
/// - `create_marker = false` (relocation of a hand-moved root): the marker must
///   already be present and equal to our `library_id`. This is what stops an
///   arbitrary empty folder from being adopted as "the original library".
///
/// Nothing is copied here, and the returned evidence is only committed by the
/// caller through [`commit_initial_root`] (same compare-and-swap).
pub fn prepare_root_for_migration(
    snapshot: &LibrarySnapshot,
    requested_root: &Path,
    create_marker: bool,
) -> AppResult<PreparedInitialRoot> {
    if !requested_root.is_absolute() {
        return Err(AppError::invalid_arg("总仓位置必须是绝对路径"));
    }
    let candidate = LibrarySnapshot {
        root_path: crate::utils::path::encode_native_path(requested_root)?,
        ..snapshot.clone()
    };
    match inspect_root(&candidate, create_marker)? {
        RootInspection::Ready {
            canonical_root,
            root_identity,
        } => Ok(PreparedInitialRoot {
            canonical_root: crate::utils::path::encode_native_path(&canonical_root)?,
            root_identity,
        }),
        RootInspection::Unconfigured => Err(AppError::invalid_arg("请选择目标目录")),
        RootInspection::Offline => Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "目标目录不存在或无法访问",
        )),
        RootInspection::WrongRoot if create_marker => {
            Err(AppError::conflict("目标目录已有其他茶馆总仓标记，不能覆盖"))
        }
        RootInspection::WrongRoot => Err(AppError::conflict(
            "目标目录不是当前茶馆总仓：缺少或不匹配的库身份标记，无法把它识别为搬走的原库",
        )),
    }
}

/// Commit a root inspection prepared outside the database lock. The snapshot
/// compare-and-swap rejects a concurrent root/locator transition.
pub fn commit_initial_root(
    conn: &rusqlite::Connection,
    expected: &LibrarySnapshot,
    prepared: &PreparedInitialRoot,
) -> AppResult<LibrarySnapshot> {
    libraries::mark_root_ready(
        conn,
        expected,
        &prepared.canonical_root,
        &prepared.root_identity,
    )
}

/// Initial root configuration may adopt an empty/unconfigured locator. Changing
/// an existing managed root is a maintenance operation, never a settings write.
pub fn adopt_initial_root(
    conn: &rusqlite::Connection,
    requested_root: &Path,
) -> AppResult<LibrarySnapshot> {
    let snapshot =
        libraries::current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"))?;
    let prepared = prepare_initial_root(&snapshot, requested_root)?;
    commit_initial_root(conn, &snapshot, &prepared)
}

/// Validate the lexical publication path before resolving it, so links inside
/// the root cannot disguise a journal target as another managed file.
pub(crate) fn validate_import_target(library: &LibrarySnapshot, file: &Path) -> AppResult<()> {
    let root = verified_root_path(library)?;
    let relative = file
        .strip_prefix(&root)
        .map_err(|_| AppError::conflict("托管目标不在当前总仓内"))?;
    validate_relative_path(relative)?;
    reject_links_between(&root, relative)
}

/// Create or verify the private same-volume directory used for staged imports.
/// It is reserved from normal managed-asset paths and directory import scans.
pub(crate) fn prepare_import_staging_directory(library: &LibrarySnapshot) -> AppResult<PathBuf> {
    let root = verified_root_path(library)?;
    let staging = root.join(".import-stage");
    match fs::symlink_metadata(&staging) {
        Ok(metadata) if !is_link(&metadata) && metadata.is_dir() => {}
        Ok(_) => return Err(AppError::conflict("导入暂存目录身份不可信")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&staging) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&staging)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(AppError::conflict("导入暂存目录身份不可信"));
    }
    let canonical = staging.canonicalize()?;
    if canonical != staging || !canonical.starts_with(&root) {
        return Err(AppError::conflict("导入暂存目录越出当前总仓"));
    }
    Ok(canonical)
}

pub(crate) fn is_reserved_managed_path(relative: &Path) -> bool {
    let Some(Component::Normal(first)) = relative.components().next() else {
        return false;
    };
    first.to_str().is_some_and(|name| {
        RESERVED_TOP_LEVEL
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
    })
}

/// Produce evidence only after confirming the path is a regular, non-link file
/// inside the currently ready managed root.
pub fn prepare_import_evidence(
    library: &LibrarySnapshot,
    file: &Path,
    content_sha256: &str,
) -> AppResult<ImportFileEvidence> {
    let canonical_root = verified_root_path(library)?;

    let canonical_file = file
        .canonicalize()
        .map_err(|_| AppError::conflict("托管文件在登记前不可访问"))?;
    if !canonical_file.starts_with(&canonical_root) || !canonical_file.is_file() {
        return Err(AppError::conflict("托管文件不在当前总仓内"));
    }
    let relative = canonical_file
        .strip_prefix(&canonical_root)
        .map_err(|_| AppError::conflict("无法计算托管相对路径"))?;
    validate_relative_path(relative)?;
    reject_links_between(&canonical_root, relative)?;

    let opened = File::open(&canonical_file)?;
    if !opened.metadata()?.is_file() {
        return Err(AppError::conflict("托管目标不是普通文件"));
    }
    let identity = file_identity(&opened)?;
    let digest = if content_sha256.is_empty() {
        sha256_full(&canonical_file)?
    } else {
        validate_sha256(content_sha256)?;
        content_sha256.to_ascii_lowercase()
    };
    let relative_path = path_to_slash(relative)?;
    let receipt = ownership_receipt(library, &relative_path, &identity, &digest);
    Ok(ImportFileEvidence {
        relative_path,
        file_identity: identity,
        content_sha256: digest,
        ownership_receipt: receipt,
    })
}

/// Revalidate the ready managed root immediately before a managed-file IO.
/// This is deliberately separate from the database snapshot: a root path alone
/// is not ownership evidence.
pub fn verified_root_path(library: &LibrarySnapshot) -> AppResult<PathBuf> {
    if library.health != LibraryHealth::Ready || library.root_identity.is_none() {
        return Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总仓尚未通过身份核验",
        ));
    }
    let root = Path::new(&library.root_path);
    let root_metadata = fs::symlink_metadata(root).map_err(|_| {
        AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总仓目录不可访问，无法登记托管素材",
        )
    })?;
    if is_link(&root_metadata) || !root_metadata.is_dir() {
        return Err(AppError::conflict("总仓目录不能是链接或非目录"));
    }
    let canonical_root = root.canonicalize().map_err(|_| {
        AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总仓目录不可访问，无法登记托管素材",
        )
    })?;
    reject_links(&canonical_root)?;
    verify_marker(&canonical_root, &library.library_id)?;
    Ok(canonical_root)
}

/// Classify legacy/unverified records in bounded pages. No file content is
/// hashed here: legacy paths without ownership receipts stay Unknown.
pub fn reconcile_pending(db: &Database) -> AppResult<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let mut after_id = 0_i64;
    loop {
        let (library, page) = {
            let conn = db.lock()?;
            let Some(library) = libraries::current(&conn)? else {
                return Ok(report);
            };
            let page = libraries::pending_page(&conn, after_id, 100)?;
            (library, page)
        };
        if page.is_empty() {
            return Ok(report);
        }
        after_id = page
            .last()
            .map(|candidate| candidate.asset_id)
            .unwrap_or(after_id);
        report.inspected += page.len();
        if library.health != LibraryHealth::Ready {
            return Ok(report);
        }
        let root = Path::new(&library.root_path).canonicalize().map_err(|_| {
            AppError::coded(
                crate::error::AppErrorCode::InvalidLibraryRoot,
                "总仓目录不可访问",
            )
        })?;
        for candidate in page {
            let classification = classify_path(&root, Path::new(&candidate.observed_path));
            let conn = db.lock()?;
            if libraries::classify_pending(&conn, &library, &candidate, classification)? {
                report.classified += 1;
            }
            std::thread::yield_now();
        }
    }
}

fn inspect_root(snapshot: &LibrarySnapshot, create_marker: bool) -> AppResult<RootInspection> {
    let raw = snapshot.root_path.trim();
    if raw.is_empty() {
        return Ok(RootInspection::Unconfigured);
    }
    let root = PathBuf::from(raw);
    let metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RootInspection::Offline);
        }
        Err(_) => return Ok(RootInspection::Offline),
    };
    if is_link(&metadata) || !metadata.is_dir() {
        return Ok(RootInspection::WrongRoot);
    }
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(RootInspection::Offline),
    };
    let marker = canonical_root.join(ROOT_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            if is_link(&metadata) || !metadata.is_file() || metadata.len() > 128 {
                return Ok(RootInspection::WrongRoot);
            }
            match fs::read_to_string(&marker) {
                Ok(marker_library_id) if marker_library_id == snapshot.library_id => {}
                Ok(_) => return Ok(RootInspection::WrongRoot),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData
                    ) =>
                {
                    return Ok(RootInspection::WrongRoot);
                }
                Err(_) => return Ok(RootInspection::Offline),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_marker => {
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return inspect_root(snapshot, false);
                }
                Err(_) => return Ok(RootInspection::Offline),
            };
            file.write_all(snapshot.library_id.as_bytes())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RootInspection::WrongRoot);
        }
        Err(_) => return Ok(RootInspection::Offline),
    }
    if verify_marker(&canonical_root, &snapshot.library_id).is_err() {
        return Ok(RootInspection::WrongRoot);
    }
    Ok(RootInspection::Ready {
        canonical_root,
        root_identity: marker_identity(&snapshot.library_id),
    })
}

fn verify_marker(root: &Path, library_id: &str) -> AppResult<()> {
    let marker = root.join(ROOT_MARKER);
    let metadata = fs::symlink_metadata(&marker).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::conflict("总仓身份标记缺失")
        } else {
            AppError::from(error)
        }
    })?;
    if is_link(&metadata) || !metadata.is_file() || metadata.len() > 128 {
        return Err(AppError::conflict("总仓标记与当前库身份不一致"));
    }
    let marker_library_id = fs::read_to_string(&marker).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => AppError::conflict("总仓身份标记缺失"),
        std::io::ErrorKind::InvalidData => AppError::conflict("总仓身份标记内容无效"),
        _ => AppError::from(error),
    })?;
    if marker_library_id != library_id {
        return Err(AppError::conflict("总仓标记与当前库身份不一致"));
    }
    Ok(())
}

fn marker_identity(library_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teacell-library-root-v1\0");
    hasher.update(library_id.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ownership_receipt(
    library: &LibrarySnapshot,
    relative_path: &str,
    identity: &str,
    digest: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teacell-import-receipt-v1\0");
    hasher.update(library.library_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(library.library_epoch.as_bytes());
    hasher.update(b"\0");
    hasher.update(relative_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(identity.as_bytes());
    hasher.update(b"\0");
    hasher.update(digest.as_bytes());
    format!(
        "import-v1:{}",
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn validate_relative_path(relative: &Path) -> AppResult<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::invalid_arg("托管相对路径不合法"));
    }
    if let Some(Component::Normal(first)) = relative.components().next() {
        let first = first
            .to_str()
            .ok_or_else(|| AppError::unsupported("托管路径不是有效 UTF-8"))?;
        if RESERVED_TOP_LEVEL
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(first))
        {
            return Err(AppError::conflict("托管路径使用了保留名称"));
        }
    }
    Ok(())
}

fn path_to_slash(path: &Path) -> AppResult<String> {
    let text = crate::utils::path::encode_native_path(path)?;
    Ok(if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    })
}

fn validate_sha256(value: &str) -> AppResult<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::internal("导入文件摘要格式错误"));
    }
    Ok(())
}

fn reject_links(path: &Path) -> AppResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link(&metadata) {
        return Err(AppError::conflict("托管路径不允许使用链接或重解析点"));
    }
    Ok(())
}

fn reject_links_between(root: &Path, relative: &Path) -> AppResult<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        if !current.exists() {
            break;
        }
        let metadata = fs::symlink_metadata(&current)?;
        if is_link(&metadata) {
            return Err(AppError::conflict("托管路径不允许使用链接或重解析点"));
        }
    }
    Ok(())
}

fn classify_path(canonical_root: &Path, observed: &Path) -> PendingClassification {
    if let Ok(canonical) = observed.canonicalize() {
        if canonical.starts_with(canonical_root) && canonical.is_file() {
            // A legacy path inside the root is not ownership evidence.
            PendingClassification::Unknown
        } else {
            PendingClassification::ExternalPending
        }
    } else if lexical_inside(canonical_root, observed) {
        PendingClassification::Missing
    } else {
        PendingClassification::ExternalPending
    }
}

fn lexical_inside(root: &Path, observed: &Path) -> bool {
    let root = crate::utils::path::normalize_path(&root.to_string_lossy());
    let observed = crate::utils::path::normalize_path(&observed.to_string_lossy());
    observed == root || observed.starts_with(&format!("{root}/"))
}

#[cfg(windows)]
fn is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
        != 0
}

#[cfg(not(windows))]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn initial_root_is_marked_once_and_same_root_reopens_without_epoch_rotation() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let adopted = adopt_initial_root(&conn, temp.path()).unwrap();
        assert_eq!(adopted.health, LibraryHealth::Ready);
        assert_eq!(
            adopted.root_path,
            temp.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(temp.path().join(ROOT_MARKER).is_file());
        let reopened = initialize_runtime(&conn).unwrap();
        assert_eq!(reopened, adopted);
    }

    #[test]
    fn restoring_the_same_root_does_not_invalidate_verified_generation() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let ready = adopt_initial_root(&conn, temp.path()).unwrap();
        let offline = libraries::set_health(&conn, &ready, LibraryHealth::Offline).unwrap();
        assert_eq!(offline.library_epoch, ready.library_epoch);

        let restored = initialize_runtime(&conn).unwrap();
        assert_eq!(restored.health, LibraryHealth::Ready);
        assert_eq!(restored.library_epoch, ready.library_epoch);
    }

    #[test]
    fn missing_marker_on_a_ready_root_does_not_adopt_a_replacement_directory() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let ready = adopt_initial_root(&conn, temp.path()).unwrap();
        let asset_id = crate::db::assets::insert(
            &conn,
            temp.path().join("managed.jpg").to_string_lossy().as_ref(),
            "managed.jpg",
            "jpg",
            4,
            "image/jpeg",
            42,
        )
        .unwrap();
        let candidate = libraries::pending_page(&conn, 0, 100)
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.asset_id == asset_id)
            .unwrap();
        assert!(libraries::record_verified(
            &conn,
            &ready,
            &candidate,
            "managed.jpg",
            "fixture-file",
            &"a".repeat(64),
            "fixture-receipt",
        )
        .unwrap());
        assert!(crate::db::assets::get_managed(&conn, asset_id).is_ok());

        fs::remove_file(temp.path().join(ROOT_MARKER)).unwrap();
        let inspected = initialize_runtime(&conn).unwrap();
        assert_eq!(inspected.health, LibraryHealth::WrongRoot);
        assert!(!temp.path().join(ROOT_MARKER).exists());
        assert!(crate::db::assets::get_managed(&conn, asset_id).is_err());
    }

    #[test]
    fn malformed_marker_content_is_reported_as_wrong_root() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let ready = adopt_initial_root(&conn, temp.path()).unwrap();
        fs::write(temp.path().join(ROOT_MARKER), [0xff]).unwrap();

        let inspected = initialize_runtime(&conn).unwrap();
        assert_eq!(inspected.health, LibraryHealth::WrongRoot);
        assert_eq!(
            verify_marker(temp.path(), &ready.library_id)
                .unwrap_err()
                .code(),
            "CONFLICT"
        );
    }

    #[cfg(unix)]
    #[test]
    fn verified_root_rejects_a_root_path_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        let moved_root = parent.path().join("moved-root");
        fs::create_dir(&root).unwrap();
        let conn = init_memory().unwrap();
        let ready = adopt_initial_root(&conn, &root).unwrap();

        fs::rename(&root, &moved_root).unwrap();
        symlink(&moved_root, &root).unwrap();

        let error = verified_root_path(&ready).unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
    }

    #[test]
    fn legacy_candidate_can_create_its_marker_once() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        conn.execute("DELETE FROM libraries", []).unwrap();
        let candidate =
            libraries::initialize_candidate(&conn, temp.path().to_str().unwrap()).unwrap();
        assert_eq!(candidate.marker_state, MarkerState::InitialCandidate);

        let adopted = initialize_runtime(&conn).unwrap();
        assert_eq!(adopted.health, LibraryHealth::Ready);
        assert_eq!(adopted.marker_state, MarkerState::Present);
        assert!(temp.path().join(ROOT_MARKER).is_file());
    }

    #[test]
    fn wrong_marker_or_missing_root_cannot_become_ready() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(ROOT_MARKER), "another-library").unwrap();
        let conn = init_memory().unwrap();
        let error = adopt_initial_root(&conn, temp.path()).unwrap_err();
        assert!(error.to_string().contains("其他茶馆总仓标记"));

        let missing = temp.path().join("missing");
        let conn = init_memory().unwrap();
        let error = adopt_initial_root(&conn, &missing).unwrap_err();
        assert!(error.to_string().contains("不存在"));
    }

    #[test]
    fn prepared_initial_root_commit_rejects_a_changed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let snapshot = libraries::current(&conn).unwrap().unwrap();
        let prepared = prepare_initial_root(&snapshot, temp.path()).unwrap();

        conn.execute("UPDATE libraries SET library_epoch='changed-epoch'", [])
            .unwrap();
        let error = commit_initial_root(&conn, &snapshot, &prepared).unwrap_err();
        assert!(error.to_string().contains("发生变化"));
        assert_eq!(
            libraries::current(&conn).unwrap().unwrap().health,
            LibraryHealth::Unconfigured
        );

        let changed = libraries::current(&conn).unwrap().unwrap();
        let committed = commit_initial_root(&conn, &changed, &prepared).unwrap();
        assert_eq!(committed.health, LibraryHealth::Ready);
        assert!(temp.path().join(ROOT_MARKER).is_file());
    }

    #[test]
    fn import_evidence_rejects_escape_and_reserved_paths() {
        let temp = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let library = adopt_initial_root(&conn, temp.path()).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let error = prepare_import_evidence(&library, outside.path(), "").unwrap_err();
        assert!(error.to_string().contains("不在当前总仓内"));

        let reserved = temp.path().join(ROOT_MARKER);
        let error = prepare_import_evidence(&library, &reserved, "").unwrap_err();
        assert!(error.to_string().contains("保留名称"));
    }
}
