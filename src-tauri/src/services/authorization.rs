//! Directory authorization and opaque resource handles.
//!
//! The trusted GUI picker calls the grant entry point; Pi/MCP adapters only
//! receive opaque references. A decoded reference never grants access on its
//! own: every resolution rechecks the subject, session, purpose, access kind,
//! grant generation, library generation, and current filesystem identity.

use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use walkdir::WalkDir;

use crate::contracts::agent::{AgentErrorCode, CallerKind, LibraryScope, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::authorization::{
    self, NewDirectoryGrant, NewResourceHandle, ACCESS_EXPORT, ACCESS_READ, ACCESS_WRITE,
};
use crate::db::import_plans::ImportPlanItemRecord;
use crate::db::{assets, libraries};
use crate::error::{AppError, AppResult};
use crate::services::managed_library;
use crate::state::Database;
use crate::utils::file_identity::file_identity;
use crate::utils::path::{encode_native_path, is_link_or_reparse};

const SESSION_GRANT_TTL_SECONDS: i64 = 12 * 60 * 60;
const DIRECTORY_PAGE_MAX_ITEMS: u32 = 100;
const DIRECTORY_PAGE_MAX_SCAN: usize = 10_000;
const DIRECTORY_MAX_DEPTH: usize = 64;
const RELATIVE_PATH_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryPurpose {
    ImportSource,
    ExportDestination,
}

impl DirectoryPurpose {
    pub fn parse(value: &str) -> AppResult<Self> {
        match value {
            "import_source" => Ok(Self::ImportSource),
            "export_destination" => Ok(Self::ExportDestination),
            _ => Err(AppError::invalid_arg("目录用途不合法")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImportSource => "import_source",
            Self::ExportDestination => "export_destination",
        }
    }

    fn access_mask(self) -> i64 {
        match self {
            Self::ImportSource => ACCESS_READ,
            Self::ExportDestination => ACCESS_WRITE | ACCESS_EXPORT,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RequiredAccess {
    Read,
    Write,
    Export,
}

impl RequiredAccess {
    fn mask(self) -> i64 {
        match self {
            Self::Read => ACCESS_READ,
            Self::Write => ACCESS_WRITE,
            Self::Export => ACCESS_EXPORT,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TrustedAdapterIdentity {
    Gui {
        session_id: String,
        instance_id: String,
    },
    Pi {
        principal_id: String,
        session_id: String,
        instance_id: String,
    },
    Mcp {
        principal_id: String,
        instance_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryGrantAccess {
    pub read: bool,
    pub write: bool,
    pub export: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryGrantView {
    pub grant_ref: ResourceRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_ref: Option<ResourceRef>,
    pub display_name: String,
    pub display_path: String,
    pub purpose: DirectoryPurpose,
    pub recursive: bool,
    pub remembered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_seconds: Option<i64>,
    pub available: bool,
    pub access: DirectoryGrantAccess,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedDirectoryEntry {
    pub name: String,
    pub kind: &'static str,
    pub size: i64,
    pub modified_at: i64,
    pub entry_ref: ResourceRef,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    pub items: Vec<AuthorizedDirectoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u32>,
    pub truncated: bool,
}

/// An open file proven to be inside the authorized root and matching the
/// identity captured when the handle was issued. The raw path is intentionally
/// not part of the public shape.
pub struct OpenedAuthorizedFile {
    file: File,
    size: i64,
    modified_at: i64,
    identity: String,
    handle_id: String,
}

/// A newly created output file under an `export_destination` grant. Existing
/// files are never overwritten and the caller must revalidate between writes.
pub struct AuthorizedOutputFile {
    file: File,
    size: i64,
    modified_at: i64,
    identity: String,
    handle_id: String,
}

impl AuthorizedOutputFile {
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn size(&self) -> i64 {
        self.size
    }

    pub fn modified_at(&self) -> i64 {
        self.modified_at
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn revalidate(&self, db: &Database, caller: &CallerContext) -> AppResult<()> {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle_mask(&conn, caller, &self.handle_id, ACCESS_WRITE | ACCESS_EXPORT)?;
        Ok(())
    }
}

impl OpenedAuthorizedFile {
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn size(&self) -> i64 {
        self.size
    }

    pub fn modified_at(&self) -> i64 {
        self.modified_at
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Long-running readers call this between bounded chunks. A revoked grant,
    /// rotated library, or replaced handle record stops the next chunk.
    pub fn revalidate(&self, db: &Database, caller: &CallerContext) -> AppResult<()> {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, &self.handle_id, RequiredAccess::Read)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AuthorizedFileLocation {
    pub path: PathBuf,
    pub size: i64,
    pub modified_at: i64,
    pub identity: String,
}

struct InspectedDirectory {
    canonical_path: PathBuf,
    path_identity: String,
    display_name: String,
    display_path: String,
}

struct ResolvedDirectory {
    grant: authorization::DirectoryGrant,
    canonical_path: PathBuf,
    access_mask: i64,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedFile<'a> {
    identity: Option<&'a str>,
    size: Option<i64>,
    modified_at: Option<i64>,
}

struct OpenedVerifiedFile {
    file: File,
    size: i64,
    modified_at: i64,
    identity: String,
}

struct CreatedOutputFile {
    file: File,
    path: PathBuf,
    size: i64,
    modified_at: i64,
    identity: String,
}

pub fn build_caller_context(
    conn: &rusqlite::Connection,
    adapter: TrustedAdapterIdentity,
    request_id: ResourceRef,
    origin_chain: Vec<ResourceRef>,
) -> AppResult<CallerContext> {
    let now = now_unix_seconds()?;
    let (kind, principal_id, session_id, instance_id) = match adapter {
        TrustedAdapterIdentity::Gui {
            session_id,
            instance_id,
        } => (
            CallerKind::Gui,
            "principal_gui_host".to_string(),
            Some(session_id),
            instance_id,
        ),
        TrustedAdapterIdentity::Pi {
            principal_id,
            session_id,
            instance_id,
        } => (CallerKind::Pi, principal_id, Some(session_id), instance_id),
        TrustedAdapterIdentity::Mcp {
            principal_id,
            instance_id,
        } => (CallerKind::Mcp, principal_id, None, instance_id),
    };
    let principal = authorization::ensure_principal(conn, &principal_id, now)?;
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    let library_scope = LibraryScope {
        library_id: ResourceRef::from_trusted(library.library_id)?,
        library_epoch: ResourceRef::from_trusted(library.library_epoch)?,
    };
    Ok(CallerContext {
        kind,
        principal_id: ResourceRef::from_trusted(principal.principal_id)?,
        session_id: session_id.map(ResourceRef::from_trusted).transpose()?,
        instance_id: ResourceRef::from_trusted(instance_id)?,
        request_id,
        grant_epoch: ResourceRef::from_trusted(principal.grant_epoch)?,
        library: library_scope,
        origin_chain,
    })
}

pub fn grant_selected_directory(
    db: &Database,
    caller: &CallerContext,
    selected_root: &Path,
    purpose: DirectoryPurpose,
    remembered: bool,
) -> AppResult<DirectoryGrantView> {
    require_caller_kind(caller, &[CallerKind::Gui])?;
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
    }
    let inspected = inspect_directory(selected_root)?;
    let now = now_unix_seconds()?;
    let grant_id = ResourceRef::generated("grant").as_str().to_string();
    let directory_id = ResourceRef::generated("directory").as_str().to_string();
    let expires_at = (!remembered).then_some(now + SESSION_GRANT_TTL_SECONDS);

    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let transaction = conn.unchecked_transaction()?;
    authorization::insert_directory_grant(
        &transaction,
        &NewDirectoryGrant {
            grant_id: grant_id.clone(),
            principal_id: caller.principal_id.as_str().to_string(),
            owner_session_id: (!remembered)
                .then(|| caller.session_id.as_ref().map(ResourceRef::as_str))
                .flatten()
                .map(str::to_string),
            owner_instance_id: caller.instance_id.as_str().to_string(),
            purpose: purpose.as_str().to_string(),
            canonical_path: encode_native_path(&inspected.canonical_path)?,
            path_identity: inspected.path_identity.clone(),
            display_name: inspected.display_name.clone(),
            recursive: true,
            remembered,
            created_at: now,
            expires_at,
            grant_epoch: caller.grant_epoch.as_str().to_string(),
            library_id: caller.library.library_id.as_str().to_string(),
            library_epoch: caller.library.library_epoch.as_str().to_string(),
        },
    )?;
    authorization::insert_handle(
        &transaction,
        &NewResourceHandle {
            handle_id: directory_id.clone(),
            principal_id: caller.principal_id.as_str().to_string(),
            session_id: caller
                .session_id
                .as_ref()
                .map(|value| value.as_str().to_string()),
            instance_id: caller.instance_id.as_str().to_string(),
            grant_id: Some(grant_id.clone()),
            resource_kind: "directory".to_string(),
            relative_path: None,
            asset_id: None,
            file_identity: None,
            file_size: None,
            modified_at: None,
            purpose: purpose.as_str().to_string(),
            access_mask: purpose.access_mask(),
            created_at: now,
            expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
            grant_epoch: caller.grant_epoch.as_str().to_string(),
            library_id: caller.library.library_id.as_str().to_string(),
            library_epoch: caller.library.library_epoch.as_str().to_string(),
        },
    )?;
    transaction.commit()?;

    Ok(DirectoryGrantView {
        grant_ref: ResourceRef::from_trusted(grant_id)?,
        directory_ref: Some(ResourceRef::from_trusted(directory_id)?),
        display_name: inspected.display_name,
        display_path: inspected.display_path,
        purpose,
        recursive: true,
        remembered,
        expires_at_unix_seconds: expires_at,
        available: true,
        access: access_view(purpose.access_mask()),
    })
}

pub fn list_directory_grants(
    db: &Database,
    caller: &CallerContext,
) -> AppResult<Vec<DirectoryGrantView>> {
    require_caller_kind(caller, &[CallerKind::Gui])?;
    let grants = {
        let conn = db.lock()?;
        validate_caller_subject(&conn, caller)?;
        authorization::list_remembered_grants(&conn, caller.principal_id.as_str())?
    };
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        let inspected = inspect_directory(Path::new(&grant.canonical_path));
        let library_matches = grant.library_id == caller.library.library_id.as_str()
            && grant.library_epoch == caller.library.library_epoch.as_str();
        let (available, display_path) = match &inspected {
            Ok(current) => (
                library_matches && current.path_identity == grant.path_identity,
                current.display_path.clone(),
            ),
            Err(_) => (false, grant.canonical_path.clone()),
        };
        let purpose = DirectoryPurpose::parse(&grant.purpose)?;
        out.push(DirectoryGrantView {
            grant_ref: ResourceRef::from_trusted(grant.grant_id)?,
            directory_ref: None,
            display_name: grant.display_name,
            display_path,
            purpose,
            recursive: grant.recursive,
            remembered: grant.remembered,
            expires_at_unix_seconds: grant.expires_at,
            available,
            access: access_view(grant_purpose_access(purpose)),
        });
    }
    Ok(out)
}

pub fn reissue_directory_handle(
    db: &Database,
    caller: &CallerContext,
    grant_ref: &ResourceRef,
) -> AppResult<DirectoryGrantView> {
    let grant = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let grant = authorization::directory_grant(&conn, grant_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let inspected = inspect_directory(Path::new(&grant.canonical_path))?;
    if inspected.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let now = now_unix_seconds()?;
    let directory_id = ResourceRef::generated("directory").as_str().to_string();
    let access_mask = grant_purpose_access(DirectoryPurpose::parse(&grant.purpose)?);
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let current = authorization::directory_grant(&conn, grant_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &current)?;
        if current.path_identity != grant.path_identity {
            return Err(AppError::from(AgentErrorCode::ReferenceExpired));
        }
        authorization::insert_handle(
            &conn,
            &NewResourceHandle {
                handle_id: directory_id.clone(),
                principal_id: caller.principal_id.as_str().to_string(),
                session_id: caller
                    .session_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
                instance_id: caller.instance_id.as_str().to_string(),
                grant_id: Some(grant.grant_id.clone()),
                resource_kind: "directory".to_string(),
                relative_path: None,
                asset_id: None,
                file_identity: None,
                file_size: None,
                modified_at: None,
                purpose: grant.purpose.clone(),
                access_mask,
                created_at: now,
                expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
                grant_epoch: caller.grant_epoch.as_str().to_string(),
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
            },
        )?;
    }
    let purpose = DirectoryPurpose::parse(&grant.purpose)?;
    Ok(DirectoryGrantView {
        grant_ref: ResourceRef::from_trusted(grant.grant_id)?,
        directory_ref: Some(ResourceRef::from_trusted(directory_id)?),
        display_name: grant.display_name,
        display_path: grant.canonical_path,
        purpose,
        recursive: grant.recursive,
        remembered: grant.remembered,
        expires_at_unix_seconds: grant.expires_at,
        available: true,
        access: access_view(access_mask),
    })
}

/// Rebind unfinished items from an old import directory handle to a fresh
/// directory authorization. The selected directory must have the same stable
/// directory identity, and each file must still match the frozen plan before a
/// new session-bound handle is staged for the caller to commit atomically.
pub(crate) fn prepare_import_file_rebinding(
    db: &Database,
    caller: &CallerContext,
    new_directory_ref: &ResourceRef,
    items: &[ImportPlanItemRecord],
) -> AppResult<Vec<(String, NewResourceHandle)>> {
    let directory = resolve_directory_handle(db, caller, new_directory_ref, RequiredAccess::Read)?;
    if directory.grant.purpose != DirectoryPurpose::ImportSource.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }

    let now = now_unix_seconds()?;
    let mut rebound = Vec::with_capacity(items.len());
    for item in items {
        let file_ref = item
            .file_ref
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let (old_handle, old_grant) = {
            let conn = db.lock()?;
            validate_caller(&conn, caller)?;
            let handle = authorization::handle(&conn, file_ref)?
                .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
            if handle.principal_id != caller.principal_id.as_str()
                || handle.resource_kind != "file"
                || handle.purpose != DirectoryPurpose::ImportSource.as_str()
            {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
            let grant_id = handle
                .grant_id
                .as_deref()
                .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
            let grant = authorization::directory_grant(&conn, grant_id)?
                .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
            if grant.principal_id != caller.principal_id.as_str()
                || grant.purpose != DirectoryPurpose::ImportSource.as_str()
            {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
            (handle, grant)
        };
        if old_grant.path_identity != directory.grant.path_identity
            || old_handle.file_identity.as_deref() != Some(item.file_identity.as_str())
            || old_handle.file_size != Some(item.size)
            || old_handle.modified_at != Some(item.modified_at)
        {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let relative = old_handle
            .relative_path
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let expected = ExpectedFile {
            identity: Some(&item.file_identity),
            size: Some(item.size),
            modified_at: Some(item.modified_at),
        };
        let opened = open_relative_file(&directory.canonical_path, Path::new(relative), expected)?;
        let mut file = opened.file;
        let digest = crate::utils::hash::sha256_full_file_with_checkpoint(&mut file, || {
            resolve_directory_handle(db, caller, new_directory_ref, RequiredAccess::Read)
                .map(|_| ())
        })?;
        if item.content_sha256.as_deref() != Some(digest.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let after_metadata = file.metadata()?;
        if after_metadata.len() as i64 != item.size
            || file_identity(&file)? != item.file_identity
            || metadata_modified_at(&after_metadata) != item.modified_at
        {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let path_check =
            open_relative_file(&directory.canonical_path, Path::new(relative), expected)?;
        if path_check.identity != item.file_identity {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }

        let handle_id = ResourceRef::generated("file").as_str().to_string();
        rebound.push((
            item.item_id.clone(),
            NewResourceHandle {
                handle_id,
                principal_id: caller.principal_id.as_str().to_string(),
                session_id: caller
                    .session_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
                instance_id: caller.instance_id.as_str().to_string(),
                grant_id: Some(directory.grant.grant_id.clone()),
                resource_kind: "file".to_string(),
                relative_path: Some(relative.to_string()),
                asset_id: None,
                file_identity: Some(item.file_identity.clone()),
                file_size: Some(item.size),
                modified_at: Some(item.modified_at),
                purpose: DirectoryPurpose::ImportSource.as_str().to_string(),
                access_mask: ACCESS_READ,
                created_at: now,
                expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
                grant_epoch: caller.grant_epoch.as_str().to_string(),
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
            },
        ));
    }

    // The handles are only committed after this final authorization check.
    let current = resolve_directory_handle(db, caller, new_directory_ref, RequiredAccess::Read)?;
    if current.grant.grant_id != directory.grant.grant_id
        || current.grant.path_identity != directory.grant.path_identity
    {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    Ok(rebound)
}

pub fn revoke_directory_grant(
    db: &Database,
    caller: &CallerContext,
    grant_ref: &ResourceRef,
) -> AppResult<()> {
    require_caller_kind(caller, &[CallerKind::Gui])?;
    let conn = db.lock()?;
    validate_caller_subject(&conn, caller)?;
    authorization::revoke_grant(
        &conn,
        caller.principal_id.as_str(),
        grant_ref.as_str(),
        now_unix_seconds()?,
    )
}

pub fn revoke_all_directory_grants(db: &Database, caller: &CallerContext) -> AppResult<()> {
    require_caller_kind(caller, &[CallerKind::Gui])?;
    let conn = db.lock()?;
    validate_caller_subject(&conn, caller)?;
    authorization::revoke_all(&conn, caller.principal_id.as_str(), now_unix_seconds()?)
}

pub fn page_authorized_directory(
    db: &Database,
    caller: &CallerContext,
    directory_ref: &ResourceRef,
    cursor: Option<u32>,
    limit: u32,
) -> AppResult<DirectoryPage> {
    if !(1..=DIRECTORY_PAGE_MAX_ITEMS).contains(&limit) {
        return Err(AppError::invalid_arg("目录分页大小不合法"));
    }
    let start = cursor.unwrap_or(0) as usize;
    if start > DIRECTORY_PAGE_MAX_SCAN {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let resolved = resolve_directory_handle(db, caller, directory_ref, RequiredAccess::Read)?;
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;
    let mut last_processed_index: Option<usize> = None;
    let walker = WalkDir::new(&resolved.canonical_path)
        .follow_links(false)
        .max_depth(if resolved.grant.recursive {
            DIRECTORY_MAX_DEPTH
        } else {
            1
        })
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry
                .path()
                .strip_prefix(&resolved.canonical_path)
                .is_ok_and(managed_library::is_reserved_managed_path)
            {
                return false;
            }
            fs::symlink_metadata(entry.path())
                .map(|metadata| !is_link_or_reparse(&metadata))
                .unwrap_or(false)
        });
    for (index, entry) in walker.enumerate() {
        if index > DIRECTORY_PAGE_MAX_SCAN {
            truncated = true;
            break;
        }
        if index < start {
            continue;
        }
        last_processed_index = Some(index);
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if entry.path() == resolved.canonical_path {
            continue;
        }
        if entry.file_type().is_dir() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            skipped += 1;
            continue;
        }
        let relative = match entry.path().strip_prefix(&resolved.canonical_path) {
            Ok(relative) => relative,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let name = match relative_to_slash(relative) {
            Ok(name) if name.len() <= RELATIVE_PATH_MAX_BYTES => name,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let expected = ExpectedFile {
            identity: None,
            size: None,
            modified_at: None,
        };
        let opened = match open_relative_file(&resolved.canonical_path, relative, expected) {
            Ok(opened) => opened,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let entry_id = ResourceRef::generated("file").as_str().to_string();
        let now = now_unix_seconds()?;
        let handle = NewResourceHandle {
            handle_id: entry_id.clone(),
            principal_id: caller.principal_id.as_str().to_string(),
            session_id: caller
                .session_id
                .as_ref()
                .map(|value| value.as_str().to_string()),
            instance_id: caller.instance_id.as_str().to_string(),
            grant_id: Some(resolved.grant.grant_id.clone()),
            resource_kind: "file".to_string(),
            relative_path: Some(name.clone()),
            asset_id: None,
            file_identity: Some(opened.identity),
            file_size: Some(opened.size),
            modified_at: Some(opened.modified_at),
            purpose: resolved.grant.purpose.clone(),
            access_mask: ACCESS_READ,
            created_at: now,
            expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
            grant_epoch: caller.grant_epoch.as_str().to_string(),
            library_id: caller.library.library_id.as_str().to_string(),
            library_epoch: caller.library.library_epoch.as_str().to_string(),
        };
        {
            let conn = db.lock()?;
            validate_caller(&conn, caller)?;
            authorization::insert_handle(&conn, &handle)?;
        }
        entries.push(AuthorizedDirectoryEntry {
            name,
            kind: "file",
            size: opened.size,
            modified_at: opened.modified_at,
            entry_ref: ResourceRef::from_trusted(entry_id)?,
        });
        if entries.len() == limit as usize {
            break;
        }
    }
    // Resume from the raw walk index so directory and skipped entries are not
    // revisited (which would duplicate or loop on later pages).
    let next_cursor = entries
        .len()
        .eq(&(limit as usize))
        .then_some((last_processed_index.map_or(start, |index| index + 1)) as u32);
    if skipped > 0 {
        tracing::debug!(
            operation = "authorization_directory_page",
            result = "partial",
            skipped,
            "目录授权分页跳过了不可安全读取的条目"
        );
    }
    Ok(DirectoryPage {
        items: entries,
        next_cursor,
        truncated,
    })
}

/// A4-01：解析一个「导出目标」授权目录的真实根路径。
///
/// 复制导出要在目录内按 `unique_dest` 语义自动改名（同名加 `(1)(2)…`），
/// 而 `create_authorized_output` 只允许新建、重名直接 `CONFLICT`。因此这里
/// 复用同一套 `resolve_directory_handle` 校验（主体、会话、用途、访问位、
/// 授权代次、当前文件系统身份）把 canonical root 交给导出服务，写入仍由
/// 导出服务以 `create_new(true)` 保证绝不覆盖。
pub fn resolve_export_directory(
    db: &Database,
    caller: &CallerContext,
    directory_ref: &ResourceRef,
) -> AppResult<PathBuf> {
    let resolved = resolve_directory_handle(db, caller, directory_ref, RequiredAccess::Export)?;
    ensure_access(resolved.access_mask, RequiredAccess::Write)?;
    ensure_access(resolved.access_mask, RequiredAccess::Export)?;
    if resolved.grant.purpose != DirectoryPurpose::ExportDestination.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(resolved.canonical_path)
}

pub fn create_authorized_output(
    db: &Database,
    caller: &CallerContext,
    directory_ref: &ResourceRef,
    relative_path: &str,
) -> AppResult<AuthorizedOutputFile> {
    let relative = Path::new(relative_path);
    validate_relative_path(relative)?;
    let resolved = resolve_directory_handle(db, caller, directory_ref, RequiredAccess::Export)?;
    ensure_access(resolved.access_mask, RequiredAccess::Write)?;
    ensure_access(resolved.access_mask, RequiredAccess::Export)?;
    if resolved.grant.purpose != DirectoryPurpose::ExportDestination.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let target = create_relative_output(&resolved.canonical_path, relative)?;
    let handle_id = ResourceRef::generated("file").as_str().to_string();
    let now = now_unix_seconds()?;
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        authorization::insert_handle(
            &conn,
            &NewResourceHandle {
                handle_id: handle_id.clone(),
                principal_id: caller.principal_id.as_str().to_string(),
                session_id: caller
                    .session_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
                instance_id: caller.instance_id.as_str().to_string(),
                grant_id: Some(resolved.grant.grant_id.clone()),
                resource_kind: "file".to_string(),
                relative_path: Some(encode_native_path(relative)?),
                asset_id: None,
                file_identity: Some(target.identity.clone()),
                file_size: Some(target.size),
                modified_at: Some(target.modified_at),
                purpose: resolved.grant.purpose.clone(),
                access_mask: ACCESS_WRITE | ACCESS_EXPORT,
                created_at: now,
                expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
                grant_epoch: caller.grant_epoch.as_str().to_string(),
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
            },
        )?;
    }
    let final_root = resolve_directory_handle(db, caller, directory_ref, RequiredAccess::Export)?;
    if final_root.canonical_path != resolved.canonical_path {
        let _ = fs::remove_file(&target.path);
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    Ok(AuthorizedOutputFile {
        file: target.file,
        size: target.size,
        modified_at: target.modified_at,
        identity: target.identity,
        handle_id,
    })
}

pub fn issue_asset_handle(
    db: &Database,
    caller: &CallerContext,
    asset_id: i64,
) -> AppResult<ResourceRef> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let asset = assets::get_managed(&conn, asset_id)?;
    let location = libraries::verified_location(&conn, asset_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::OwnershipUnverified))?;
    let handle_id = ResourceRef::generated("asset").as_str().to_string();
    let now = now_unix_seconds()?;
    authorization::insert_handle(
        &conn,
        &NewResourceHandle {
            handle_id: handle_id.clone(),
            principal_id: caller.principal_id.as_str().to_string(),
            session_id: caller
                .session_id
                .as_ref()
                .map(|value| value.as_str().to_string()),
            instance_id: caller.instance_id.as_str().to_string(),
            grant_id: None,
            resource_kind: "asset".to_string(),
            relative_path: None,
            asset_id: Some(asset_id),
            file_identity: Some(location.file_identity),
            file_size: Some(asset.file_size),
            modified_at: Some(location.observed_modified_at),
            purpose: "import_source".to_string(),
            access_mask: ACCESS_READ,
            created_at: now,
            expires_at: Some(now + SESSION_GRANT_TTL_SECONDS),
            grant_epoch: caller.grant_epoch.as_str().to_string(),
            library_id: caller.library.library_id.as_str().to_string(),
            library_epoch: caller.library.library_epoch.as_str().to_string(),
        },
    )?;
    Ok(ResourceRef::from_trusted(handle_id)?)
}

pub fn open_authorized_file(
    db: &Database,
    caller: &CallerContext,
    file_ref: &ResourceRef,
) -> AppResult<OpenedAuthorizedFile> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?
    };
    if handle.resource_kind != "file" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let grant = {
        let conn = db.lock()?;
        let grant_id = handle
            .grant_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let grant = authorization::directory_grant(&conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let root = inspect_directory(Path::new(&grant.canonical_path))?;
    if root.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let relative = handle
        .relative_path
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let expected_identity = handle
        .file_identity
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let opened = open_relative_file(
        &root.canonical_path,
        Path::new(relative),
        ExpectedFile {
            identity: Some(expected_identity),
            size: handle.file_size,
            modified_at: handle.modified_at,
        },
    )?;
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?;
    }
    Ok(OpenedAuthorizedFile {
        file: opened.file,
        size: opened.size,
        modified_at: opened.modified_at,
        identity: opened.identity,
        handle_id: file_ref.as_str().to_string(),
    })
}

/// Resolve a file handle to its verified canonical path plus the observed
/// fingerprint. Callers that only need to classify or record a candidate use
/// this instead of duplicating the subject/grant/link checks; the returned path
/// is still only valid for a short window and must be re-verified before use.
pub fn locate_authorized_file(
    db: &Database,
    caller: &CallerContext,
    file_ref: &ResourceRef,
) -> AppResult<AuthorizedFileLocation> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?
    };
    if handle.resource_kind != "file" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let grant = {
        let conn = db.lock()?;
        let grant_id = handle
            .grant_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let grant = authorization::directory_grant(&conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let root = inspect_directory(Path::new(&grant.canonical_path))?;
    if root.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let relative = handle
        .relative_path
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let expected_identity = handle
        .file_identity
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let opened = open_relative_file(
        &root.canonical_path,
        Path::new(relative),
        ExpectedFile {
            identity: Some(expected_identity),
            size: handle.file_size,
            modified_at: handle.modified_at,
        },
    )?;
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?;
    }
    let path = root
        .canonical_path
        .join(relative)
        .canonicalize()
        .map_err(|_| AppError::conflict("授权文件不可访问"))?;
    Ok(AuthorizedFileLocation {
        path,
        size: opened.size,
        modified_at: opened.modified_at,
        identity: opened.identity,
    })
}

/// Resolve a currently authorized source when it still exists. `None` means
/// the file has been removed after a durable import publication; callers may
/// then recover from the verified target receipt. Existing files still pass
/// through the normal handle fingerprint checks, so a changed source cannot
/// be silently accepted during first-run registration.
pub(crate) fn locate_authorized_file_if_present(
    db: &Database,
    caller: &CallerContext,
    file_ref: &ResourceRef,
) -> AppResult<Option<AuthorizedFileLocation>> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?
    };
    if handle.resource_kind != "file" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let grant = {
        let conn = db.lock()?;
        let grant_id = handle
            .grant_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let grant = authorization::directory_grant(&conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let root = inspect_directory(Path::new(&grant.canonical_path))?;
    if root.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let relative = handle
        .relative_path
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let relative_path = Path::new(relative);
    validate_relative_path(relative_path)?;
    let mut current = root.canonical_path.clone();
    for component in relative_path.components() {
        let Component::Normal(part) = component else {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link_or_reparse(&metadata) => {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self::validate_authorized_file_grant(db, caller, file_ref)?;
                return Ok(None);
            }
            Err(_) => return Err(AppError::conflict("授权文件不可访问")),
        }
    }
    locate_authorized_file(db, caller, file_ref).map(Some)
}

/// Revalidate the current caller's file grant without opening the file. This
/// is used after an import has durably published and verified its target: the
/// source bytes are no longer needed, while revocation and directory identity
/// still need to be observed during an active session.
pub(crate) fn validate_authorized_file_grant(
    db: &Database,
    caller: &CallerContext,
    file_ref: &ResourceRef,
) -> AppResult<()> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?
    };
    if handle.resource_kind != "file" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let grant = {
        let conn = db.lock()?;
        let grant_id = handle
            .grant_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let grant = authorization::directory_grant(&conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let root = inspect_directory(Path::new(&grant.canonical_path))?;
    if root.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    validate_handle(&conn, caller, file_ref.as_str(), RequiredAccess::Read)?;
    Ok(())
}

pub fn open_authorized_asset(
    db: &Database,
    caller: &CallerContext,
    asset_ref: &ResourceRef,
) -> AppResult<OpenedAuthorizedFile> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, asset_ref.as_str(), RequiredAccess::Read)?
    };
    if handle.resource_kind != "asset" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let asset_id = handle
        .asset_id
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let (library, location) = {
        let conn = db.lock()?;
        let library = libraries::current(&conn)?
            .ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
        let location = libraries::verified_location(&conn, asset_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::OwnershipUnverified))?;
        (library, location)
    };
    let root = managed_library::verified_root_path(&library)?;
    let opened = open_relative_file(
        &root,
        Path::new(&location.relative_path),
        ExpectedFile {
            identity: Some(&location.file_identity),
            size: None,
            modified_at: Some(location.observed_modified_at),
        },
    )?;
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, asset_ref.as_str(), RequiredAccess::Read)?;
    }
    Ok(OpenedAuthorizedFile {
        file: opened.file,
        size: opened.size,
        modified_at: opened.modified_at,
        identity: opened.identity,
        handle_id: asset_ref.as_str().to_string(),
    })
}

fn resolve_directory_handle(
    db: &Database,
    caller: &CallerContext,
    directory_ref: &ResourceRef,
    required: RequiredAccess,
) -> AppResult<ResolvedDirectory> {
    let handle = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, directory_ref.as_str(), required)?
    };
    if handle.resource_kind != "directory" {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let grant_id = handle
        .grant_id
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let grant = {
        let conn = db.lock()?;
        let grant = authorization::directory_grant(&conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
        grant
    };
    let inspected = inspect_directory(Path::new(&grant.canonical_path))?;
    if inspected.path_identity != grant.path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    // Recheck after filesystem work; a concurrent revoke or library rotation
    // must not let the caller continue with a snapshot read before the change.
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        validate_handle(&conn, caller, directory_ref.as_str(), required)?;
    }
    Ok(ResolvedDirectory {
        grant,
        canonical_path: inspected.canonical_path,
        access_mask: handle.access_mask,
    })
}

pub(crate) fn validate_caller(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
) -> AppResult<()> {
    validate_caller_subject(conn, caller)?;
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    Ok(())
}

/// Validate the persisted caller identity and library generation without
/// requiring the root itself to be online. Read-only maintenance views use
/// this to remain visible while destructive/relink operations stay disabled.
pub(crate) fn validate_caller_session(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
) -> AppResult<()> {
    validate_caller_subject(conn, caller)
}

/// Validate that the caller's GUI/agent grant is still current without
/// requiring its library generation to match. This is reserved for narrowly
/// scoped recovery actions that quarantine stale receipts after a library
/// generation changes.
pub(crate) fn validate_caller_grant(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
) -> AppResult<()> {
    let principal = authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    Ok(())
}

fn validate_caller_subject(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    validate_caller_grant(conn, caller)?;
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

fn validate_handle(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    handle_id: &str,
    required: RequiredAccess,
) -> AppResult<authorization::ResourceHandle> {
    validate_handle_mask(conn, caller, handle_id, required.mask())
}

fn validate_handle_mask(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    handle_id: &str,
    required_mask: i64,
) -> AppResult<authorization::ResourceHandle> {
    let handle = authorization::handle(conn, handle_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if handle.principal_id != caller.principal_id.as_str()
        || handle.instance_id != caller.instance_id.as_str()
        || handle.session_id.as_deref() != caller.session_id.as_ref().map(ResourceRef::as_str)
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let now = now_unix_seconds()?;
    if handle.revoked_at.is_some() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    if handle.expires_at.is_some_and(|expires| expires <= now) {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    if handle.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    if handle.library_id != caller.library.library_id.as_str()
        || handle.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if handle.access_mask & required_mask != required_mask {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if let Some(grant_id) = handle.grant_id.as_deref() {
        let grant = authorization::directory_grant(conn, grant_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        validate_grant(caller, &grant)?;
    }
    Ok(handle)
}

fn validate_grant(caller: &CallerContext, grant: &authorization::DirectoryGrant) -> AppResult<()> {
    if grant.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let now = now_unix_seconds()?;
    if grant.revoked_at.is_some() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    if grant.expires_at.is_some_and(|expires| expires <= now) {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    if grant.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    if grant.library_id != caller.library.library_id.as_str()
        || grant.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if !grant.remembered
        && (grant.owner_session_id.as_deref()
            != caller.session_id.as_ref().map(ResourceRef::as_str)
            || grant.owner_instance_id != caller.instance_id.as_str())
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(())
}

fn require_caller_kind(caller: &CallerContext, allowed: &[CallerKind]) -> AppResult<()> {
    if allowed.contains(&caller.kind) {
        Ok(())
    } else {
        Err(AppError::from(AgentErrorCode::Unauthorized))
    }
}

fn ensure_access(access_mask: i64, required: RequiredAccess) -> AppResult<()> {
    if access_mask & required.mask() == required.mask() {
        Ok(())
    } else {
        Err(AppError::from(AgentErrorCode::Unauthorized))
    }
}

fn grant_purpose_access(purpose: DirectoryPurpose) -> i64 {
    purpose.access_mask()
}

fn access_view(access_mask: i64) -> DirectoryGrantAccess {
    DirectoryGrantAccess {
        read: access_mask & ACCESS_READ != 0,
        write: access_mask & ACCESS_WRITE != 0,
        export: access_mask & ACCESS_EXPORT != 0,
    }
}

fn inspect_directory(path: &Path) -> AppResult<InspectedDirectory> {
    crate::utils::path::ensure_native_absolute(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| AppError::conflict("所选目录不可访问"))?;
    if is_link_or_reparse(&metadata) {
        return Err(AppError::unauthorized("授权目录不能是链接或重解析点"));
    }
    if !metadata.is_dir() {
        return Err(AppError::invalid_arg("所选路径不是目录"));
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|_| AppError::conflict("所选目录不可访问"))?;
    let current = fs::symlink_metadata(&canonical_path)
        .map_err(|_| AppError::conflict("所选目录不可访问"))?;
    if is_link_or_reparse(&current) || !current.is_dir() {
        return Err(AppError::unauthorized("授权目录身份不可信"));
    }
    let display_path = encode_native_path(&canonical_path)?;
    let display_name = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("所选目录")
        .to_string();
    let path_identity = directory_identity(&canonical_path)?;
    let canonical_after = path
        .canonicalize()
        .map_err(|_| AppError::conflict("所选目录不可访问"))?;
    if canonical_after != canonical_path {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let rechecked_identity = directory_identity(&canonical_after)?;
    if rechecked_identity != path_identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    Ok(InspectedDirectory {
        canonical_path,
        path_identity,
        display_name,
        display_path,
    })
}

fn metadata_modified_at(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn directory_identity(path: &Path) -> AppResult<String> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .access_mode(0)
        .custom_flags(
            windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
        )
        .open(path)
        .map_err(|_| AppError::conflict("所选目录不可访问"))?;
    file_identity(&file)
}

#[cfg(unix)]
fn directory_identity(path: &Path) -> AppResult<String> {
    use std::os::unix::fs::MetadataExt;
    let file = File::open(path).map_err(|_| AppError::conflict("所选目录不可访问"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::conflict("所选目录不可访问"))?;
    Ok(format!("unix-dir:{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(any(windows, unix)))]
fn directory_identity(path: &Path) -> AppResult<String> {
    let _ = path;
    Err(AppError::unsupported("当前平台不支持稳定的目录身份核验"))
}

fn open_relative_file(
    canonical_root: &Path,
    relative: &Path,
    expected: ExpectedFile<'_>,
) -> AppResult<OpenedVerifiedFile> {
    validate_relative_path(relative)?;
    let mut current = canonical_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        };
        current.push(part);
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| AppError::conflict("授权文件不可访问"))?;
        if is_link_or_reparse(&metadata) {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
    }
    let canonical_file = current
        .canonicalize()
        .map_err(|_| AppError::conflict("授权文件不可访问"))?;
    if !canonical_file.starts_with(canonical_root) {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let file = File::open(&canonical_file).map_err(|_| AppError::conflict("授权文件不可访问"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::conflict("授权文件不可访问"))?;
    if !metadata.is_file() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let identity = file_identity(&file)?;
    if expected.identity.is_some_and(|value| value != identity) {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let size = metadata.len() as i64;
    if expected.size.is_some_and(|value| value != size) {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    if expected
        .modified_at
        .is_some_and(|value| value != modified_at)
    {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let canonical_after = current
        .canonicalize()
        .map_err(|_| AppError::conflict("授权文件不可访问"))?;
    if canonical_after != canonical_file {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    let verification =
        File::open(&canonical_after).map_err(|_| AppError::conflict("授权文件不可访问"))?;
    let verification_identity = file_identity(&verification)?;
    if verification_identity != identity {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    Ok(OpenedVerifiedFile {
        file,
        size,
        modified_at,
        identity,
    })
}

fn create_relative_output(canonical_root: &Path, relative: &Path) -> AppResult<CreatedOutputFile> {
    validate_relative_path(relative)?;
    let file_name = relative
        .file_name()
        .ok_or_else(|| AppError::invalid_arg("输出文件名不能为空"))?;
    let mut parent = canonical_root.to_path_buf();
    if let Some(relative_parent) = relative.parent() {
        for component in relative_parent.components() {
            let Component::Normal(part) = component else {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            };
            parent.push(part);
            match fs::symlink_metadata(&parent) {
                Ok(metadata) => {
                    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                        return Err(AppError::from(AgentErrorCode::Unauthorized));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&parent)
                        .map_err(|_| AppError::conflict("授权目录无法创建输出子目录"))?;
                    let metadata = fs::symlink_metadata(&parent)
                        .map_err(|_| AppError::conflict("授权目录无法创建输出子目录"))?;
                    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                        return Err(AppError::from(AgentErrorCode::Unauthorized));
                    }
                }
                Err(_) => return Err(AppError::conflict("授权目录不可访问")),
            }
        }
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| AppError::conflict("授权目录不可访问"))?;
    if !canonical_parent.starts_with(canonical_root) || canonical_parent != parent {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let target = parent.join(file_name);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::conflict("目标文件已存在，已拒绝覆盖")
            } else {
                AppError::conflict("授权目录无法创建输出文件")
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::conflict("输出文件身份不可核实"))?;
    if !metadata.is_file() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let canonical_target = target
        .canonicalize()
        .map_err(|_| AppError::conflict("输出文件身份不可核实"))?;
    if !canonical_target.starts_with(canonical_root) || canonical_target != target {
        let _ = fs::remove_file(&target);
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let identity = file_identity(&file)?;
    let size = metadata.len() as i64;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    Ok(CreatedOutputFile {
        file,
        path: target,
        size,
        modified_at,
        identity,
    })
}

fn validate_relative_path(relative: &Path) -> AppResult<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let text = encode_native_path(relative)?;
    if text.len() > RELATIVE_PATH_MAX_BYTES || text.contains('\0') {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(())
}

fn relative_to_slash(relative: &Path) -> AppResult<String> {
    validate_relative_path(relative)?;
    let text = encode_native_path(relative)?;
    Ok(if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    })
}

fn now_unix_seconds() -> AppResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use crate::utils::file_identity::file_identity;

    fn ready_db(root: &Path) -> Database {
        let conn = init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        Database::new(conn)
    }

    fn gui_caller(db: &Database, session: &str, instance: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: format!("session_fixture_{session}"),
                instance_id: format!("instance_fixture_{instance}"),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn pi_caller(db: &Database, principal: &str, session: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        build_caller_context(
            &conn,
            TrustedAdapterIdentity::Pi {
                principal_id: principal.to_string(),
                session_id: session.to_string(),
                instance_id: "instance_pi_test".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn mcp_caller(db: &Database, principal: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        build_caller_context(
            &conn,
            TrustedAdapterIdentity::Mcp {
                principal_id: principal.to_string(),
                instance_id: "instance_mcp_test".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn grant_for(
        db: &Database,
        caller: &CallerContext,
        path: &Path,
        purpose: DirectoryPurpose,
        remembered: bool,
    ) -> DirectoryGrantView {
        grant_selected_directory(db, caller, path, purpose, remembered).unwrap()
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    fn expect_error<T>(result: AppResult<T>) -> AppError {
        match result {
            Ok(_) => panic!("expected an authorization error"),
            Err(error) => error,
        }
    }

    #[test]
    fn adapters_build_subjects_and_only_gui_can_select_a_directory() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let gui = gui_caller(&db, "session_gui_a", "instance_gui_a");
        assert_eq!(gui.kind, CallerKind::Gui);
        assert_eq!(gui.principal_id.as_str(), "principal_gui_host");
        assert!(gui.session_id.is_some());
        assert!(grant_selected_directory(
            &db,
            &gui,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false
        )
        .is_ok());

        let pi = pi_caller(&db, "principal_pi_fixture", "session_pi_fixture");
        assert_eq!(
            error_code(
                grant_selected_directory(
                    &db,
                    &pi,
                    selected.path(),
                    DirectoryPurpose::ImportSource,
                    false
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
        let mcp = mcp_caller(&db, "principal_mcp_fixture");
        assert_eq!(
            error_code(
                grant_selected_directory(
                    &db,
                    &mcp,
                    selected.path(),
                    DirectoryPurpose::ImportSource,
                    false
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn handles_bind_subject_session_access_and_purpose() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        std::fs::write(selected.path().join("one.jpg"), b"fixture").unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        assert!(view.access.read);
        assert!(!view.access.write);
        assert!(!view.access.export);
        assert!(page_authorized_directory(
            &db,
            &caller,
            view.directory_ref.as_ref().unwrap(),
            None,
            100
        )
        .is_ok());
        assert_eq!(
            error_code(
                ensure_access(view.purpose.access_mask(), RequiredAccess::Export).unwrap_err()
            ),
            "UNAUTHORIZED"
        );

        let other_session = gui_caller(&db, "session_gui_b", "instance_gui_a");
        assert_eq!(
            error_code(
                page_authorized_directory(
                    &db,
                    &other_session,
                    view.directory_ref.as_ref().unwrap(),
                    None,
                    100
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
        let other_principal = pi_caller(&db, "principal_pi_other", "session_pi_other");
        assert_eq!(
            error_code(
                page_authorized_directory(
                    &db,
                    &other_principal,
                    view.directory_ref.as_ref().unwrap(),
                    None,
                    100
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn revocation_and_forged_or_expired_handles_fail_closed() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        std::fs::write(selected.path().join("one.jpg"), b"fixture").unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        let forged = ResourceRef::from_trusted("directory_0123456789abcdef").unwrap();
        assert_eq!(
            error_code(page_authorized_directory(&db, &caller, &forged, None, 100).unwrap_err()),
            "REFERENCE_EXPIRED"
        );
        revoke_directory_grant(&db, &caller, &view.grant_ref).unwrap();
        assert_eq!(
            error_code(
                page_authorized_directory(
                    &db,
                    &caller,
                    view.directory_ref.as_ref().unwrap(),
                    None,
                    100
                )
                .unwrap_err()
            ),
            "GRANT_REVOKED"
        );
        let fresh = gui_caller(&db, "session_gui_a", "instance_gui_a");
        assert!(list_directory_grants(&db, &fresh).unwrap().is_empty());
    }

    #[test]
    fn library_generation_change_invalidates_old_handles() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        std::fs::write(selected.path().join("one.jpg"), b"fixture").unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE libraries SET library_epoch=?1 WHERE singleton=1",
                ["rotated_library_epoch_fixture"],
            )
            .unwrap();
        }
        let fresh = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let error =
            page_authorized_directory(&db, &fresh, view.directory_ref.as_ref().unwrap(), None, 100)
                .unwrap_err();
        assert_eq!(error.code(), "LIBRARY_CHANGED");
    }

    #[test]
    fn directory_identity_change_and_file_change_are_detected() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        let selected_path = selected.path().join("one.jpg");
        std::fs::write(&selected_path, b"fixture").unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            true,
        );
        // Adding an entry changes directory metadata, but not the authorized
        // root identity. Only replacing the root itself may invalidate it.
        std::fs::write(selected.path().join("two.jpg"), b"second").unwrap();
        let page = page_authorized_directory(
            &db,
            &caller,
            view.directory_ref.as_ref().unwrap(),
            None,
            100,
        )
        .unwrap();
        assert_eq!(page.items.len(), 2);
        let entry_ref = page
            .items
            .iter()
            .find(|item| item.name == "one.jpg")
            .unwrap()
            .entry_ref
            .clone();
        assert!(open_authorized_file(&db, &caller, &entry_ref).is_ok());

        std::fs::write(&selected_path, b"fixture changed to another size").unwrap();
        assert_eq!(
            error_code(expect_error(open_authorized_file(&db, &caller, &entry_ref))),
            "REFERENCE_EXPIRED"
        );

        let moved = tempfile::tempdir().unwrap();
        std::fs::rename(selected.path(), moved.path().join("old")).unwrap();
        std::fs::create_dir(selected.path()).unwrap();
        assert_eq!(
            error_code(
                page_authorized_directory(
                    &db,
                    &caller,
                    view.directory_ref.as_ref().unwrap(),
                    None,
                    100
                )
                .unwrap_err()
            ),
            "REFERENCE_EXPIRED"
        );
    }

    #[test]
    fn link_and_parent_escape_are_rejected_without_path_disclosure() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(selected.path().join("inside.jpg"), b"inside").unwrap();
        let outside_file = outside.path().join("secret.jpg");
        std::fs::write(&outside_file, b"secret").unwrap();
        let link = selected.path().join("escape.jpg");
        let link_created = {
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_file(&outside_file, &link).is_ok()
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&outside_file, &link).is_ok()
            }
            #[cfg(not(any(windows, unix)))]
            {
                false
            }
        };
        if link_created {
            assert_eq!(
                error_code(expect_error(inspect_directory(&link))),
                "UNAUTHORIZED"
            );
        }

        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        let page = page_authorized_directory(
            &db,
            &caller,
            view.directory_ref.as_ref().unwrap(),
            None,
            100,
        )
        .unwrap();
        assert!(page.items.iter().any(|item| item.name == "inside.jpg"));
        if link_created {
            assert!(!page.items.iter().any(|item| item.name == "escape.jpg"));
        }
        assert_eq!(
            error_code(expect_error(open_relative_file(
                selected.path(),
                Path::new("../secret.jpg"),
                ExpectedFile {
                    identity: None,
                    size: None,
                    modified_at: None,
                },
            ))),
            "UNAUTHORIZED"
        );

        let missing = selected.path().join("missing-parent");
        let error = grant_selected_directory(
            &db,
            &caller,
            &missing,
            DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap_err();
        assert!(!error
            .to_string()
            .contains(&missing.to_string_lossy().to_string()));
    }

    #[test]
    fn export_grants_create_new_outputs_without_overwrite_or_read_expansion() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let export = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ExportDestination,
            false,
        );
        let output = create_authorized_output(
            &db,
            &caller,
            export.directory_ref.as_ref().unwrap(),
            "album/out.jpg",
        )
        .unwrap();
        assert!(selected.path().join("album").join("out.jpg").is_file());
        output.revalidate(&db, &caller).unwrap();
        assert_eq!(
            error_code(expect_error(create_authorized_output(
                &db,
                &caller,
                export.directory_ref.as_ref().unwrap(),
                "album/out.jpg"
            ))),
            "CONFLICT"
        );
        assert_eq!(
            error_code(expect_error(create_authorized_output(
                &db,
                &caller,
                export.directory_ref.as_ref().unwrap(),
                "../escape.jpg"
            ))),
            "UNAUTHORIZED"
        );

        let source = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        assert_eq!(
            error_code(expect_error(create_authorized_output(
                &db,
                &caller,
                source.directory_ref.as_ref().unwrap(),
                "not-allowed.jpg"
            ))),
            "UNAUTHORIZED"
        );
        revoke_directory_grant(&db, &caller, &export.grant_ref).unwrap();
        let fresh = gui_caller(&db, "session_gui_a", "instance_gui_a");
        assert_eq!(
            error_code(output.revalidate(&db, &fresh).unwrap_err()),
            "GRANT_REVOKED"
        );
    }

    #[test]
    fn managed_asset_handles_recheck_library_ownership_and_file_identity() {
        let managed = tempfile::tempdir().unwrap();
        let asset_path = managed.path().join("managed.jpg");
        std::fs::write(&asset_path, b"managed fixture").unwrap();
        let conn = init_memory().unwrap();
        let library = managed_library::adopt_initial_root(&conn, managed.path()).unwrap();
        let modified_at = std::fs::metadata(&asset_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let asset_id = assets::insert(
            &conn,
            asset_path.to_string_lossy().as_ref(),
            "managed.jpg",
            "jpg",
            15,
            "image/jpeg",
            modified_at,
        )
        .unwrap();
        let candidate = libraries::pending_page(&conn, 0, 100)
            .unwrap()
            .into_iter()
            .find(|item| item.asset_id == asset_id)
            .unwrap();
        let identity = {
            let file = File::open(&asset_path).unwrap();
            file_identity(&file).unwrap()
        };
        assert!(libraries::record_verified(
            &conn,
            &library,
            &candidate,
            "managed.jpg",
            &identity,
            &"a".repeat(64),
            "fixture-receipt",
        )
        .unwrap());
        let db = Database::new(conn);
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let asset_ref = issue_asset_handle(&db, &caller, asset_id).unwrap();
        assert!(open_authorized_asset(&db, &caller, &asset_ref).is_ok());

        std::fs::write(&asset_path, b"managed fixture changed").unwrap();
        assert_eq!(
            error_code(expect_error(open_authorized_asset(
                &db, &caller, &asset_ref
            ))),
            "REFERENCE_EXPIRED"
        );
        let other = pi_caller(&db, "principal_pi_other", "session_pi_other");
        assert_eq!(
            error_code(expect_error(open_authorized_asset(&db, &other, &asset_ref))),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn directory_paging_resumes_across_nested_directories_without_duplicates() {
        let managed = tempfile::tempdir().unwrap();
        let selected = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(selected.path().join("sub_one")).unwrap();
        std::fs::create_dir_all(selected.path().join("sub_two/nested")).unwrap();
        let expected = [
            "root_one.jpg",
            "root_two.jpg",
            "sub_one/inside_one.jpg",
            "sub_one/inside_two.jpg",
            "sub_two/nested/deep.jpg",
        ];
        for name in expected {
            std::fs::write(selected.path().join(name), b"fixture").unwrap();
        }
        let db = ready_db(managed.path());
        let caller = gui_caller(&db, "session_gui_a", "instance_gui_a");
        let view = grant_for(
            &db,
            &caller,
            selected.path(),
            DirectoryPurpose::ImportSource,
            false,
        );
        let directory_ref = view.directory_ref.as_ref().unwrap();

        let mut cursor = None;
        let mut seen: Vec<String> = Vec::new();
        let mut pages = 0;
        loop {
            let page = page_authorized_directory(&db, &caller, directory_ref, cursor, 2).unwrap();
            pages += 1;
            assert!(pages <= 16, "paging did not terminate");
            for item in &page.items {
                assert!(
                    !seen.contains(&item.name),
                    "duplicate entry across pages: {}",
                    item.name
                );
                seen.push(item.name.clone());
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        seen.sort();
        let mut sorted_expected: Vec<String> =
            expected.iter().map(|name| name.to_string()).collect();
        sorted_expected.sort();
        assert_eq!(seen, sorted_expected);
    }
}
