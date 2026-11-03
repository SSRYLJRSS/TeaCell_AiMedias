//! Import preparation: turn a scan/check into a fixed, inspectable plan.
//!
//! Preparing an import never copies a file, never writes an asset row and never
//! sends bytes anywhere. It enumerates candidates through directory
//! authorization (or explicit drag/drop paths), records per-item fingerprints,
//! preview classification and the backend-computed rename preview, and stores
//! the closed candidate set so a later submit can detect changes.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::authorization;
use crate::db::import_plans::{self, NewImportPlan, NewImportPlanItem};
use crate::db::libraries;
use crate::error::{AppError, AppResult};
use crate::services::authorization as auth_service;
use crate::services::importer;
use crate::state::Database;
use crate::utils::file_identity::file_identity;
use crate::utils::path::{encode_native_path, is_link_or_reparse};

const PLAN_TTL_SECONDS: i64 = 30 * 60;
const DIRECTORY_PAGE_LIMIT: u32 = 100;
const MAX_PLAN_ITEMS: usize = 100_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareImportRequest {
    /// Authorized `import_source` directory. When present, explicit paths are ignored.
    pub source_ref: Option<ResourceRef>,
    /// Explicit drag/drop file paths (legacy trusted input, no directory grant).
    #[serde(default)]
    pub paths: Vec<String>,
    pub collection: Option<String>,
    pub rename_pattern: Option<String>,
    /// Re-preparation: exclude the items the user removed from this base plan.
    pub base_plan_ref: Option<ResourceRef>,
    #[serde(default)]
    pub excluded_item_refs: Vec<ResourceRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlanItemView {
    pub item_ref: ResourceRef,
    pub display_name: String,
    pub kind: String,
    pub size: i64,
    pub modified_at: i64,
    pub preview_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_message: Option<String>,
    pub rename_preview: String,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlanView {
    pub plan_ref: ResourceRef,
    pub source_ref: Option<ResourceRef>,
    pub target_collection: Option<String>,
    pub rename_pattern: Option<String>,
    pub item_count: u32,
    pub ready_count: u32,
    pub limited_count: u32,
    pub unsupported_count: u32,
    pub total_size: i64,
    pub expires_at_unix_seconds: i64,
    pub items: Vec<ImportPlanItemView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlanVerification {
    pub plan_ref: ResourceRef,
    pub stale_item_refs: Vec<ResourceRef>,
    pub library_changed: bool,
}

struct Candidate {
    file_ref: Option<String>,
    path: String,
    scan_path: PathBuf,
    display_name: String,
    kind: String,
    size: i64,
    modified_at: i64,
    identity: String,
}

/// Prepare an import checklist for the trusted GUI workbench.
///
/// This returns [`ImportPlanView`], the GUI checklist — **not** the `PlanPreview`
/// that `imports.prepare` declares. The reviewable preview is the
/// `imports.submit` draft built by
/// `src-tauri/src/services/import_submission.rs::submit_draft`; see
/// `docs/contracts/agent-v1.md` §「导入入口的设计结论」.
///
/// Do not call this from a Pi/MCP adapter. Two reasons, both contractual:
/// a bare `paths` request is not a directory grant (a model-supplied path string
/// must never become authorization, `tests/agent-evals` case E32), and a Pi/MCP
/// caller cannot hold an `import_source` handle because handles are bound to the
/// issuing session (`authorization::validate_handle_mask`). The remaining open
/// item is that handle handoff design, tracked with A5-03.
pub fn prepare_import(
    db: &Database,
    caller: &CallerContext,
    request: PrepareImportRequest,
) -> AppResult<ImportPlanView> {
    prepare_import_with_cancel(db, caller, request, &AtomicBool::new(false))
}

pub fn prepare_import_with_cancel(
    db: &Database,
    caller: &CallerContext,
    request: PrepareImportRequest,
    cancel: &AtomicBool,
) -> AppResult<ImportPlanView> {
    check_cancel(cancel)?;
    let collection = match request.collection.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            importer::validate_collection(value)?;
            Some(value.trim().to_string())
        }
        _ => None,
    };
    let rename_pattern = request
        .rename_pattern
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let inherited_plan = if request.source_ref.is_none()
        && request.paths.is_empty()
        && request.base_plan_ref.is_some()
    {
        let base_plan_ref = request
            .base_plan_ref
            .as_ref()
            .ok_or_else(|| AppError::from(AgentErrorCode::InvalidInput))?;
        Some(reprepare_from_plan(
            db,
            caller,
            base_plan_ref,
            &request.excluded_item_refs,
            cancel,
        )?)
    } else {
        None
    };
    let (candidates, inherited_source_ref) = match inherited_plan {
        Some((candidates, source_ref)) => (candidates, source_ref),
        None => {
            let excluded = resolve_exclusions(
                db,
                caller,
                request.base_plan_ref.as_ref(),
                &request.excluded_item_refs,
            )?;
            let mut candidates = if let Some(source_ref) = request.source_ref.as_ref() {
                enumerate_authorized_directory(db, caller, source_ref, cancel)?
            } else {
                enumerate_explicit_paths(&request.paths, cancel)?
            };
            if !excluded.is_empty() {
                candidates.retain(|candidate| !excluded.contains(&candidate.path));
            }
            (candidates, None)
        }
    };
    let plan_source_ref = request
        .source_ref
        .as_ref()
        .map(|value| value.as_str().to_string())
        .or(inherited_source_ref);
    if candidates.is_empty() {
        return Err(AppError::invalid_arg("没有可入库的候选文件"));
    }
    if candidates.len() > MAX_PLAN_ITEMS {
        return Err(AppError::invalid_arg("候选文件数量超过单次准备上限"));
    }

    let now = now_unix_seconds()?;
    let (library_id, library_epoch) = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        (
            caller.library.library_id.as_str().to_string(),
            caller.library.library_epoch.as_str().to_string(),
        )
    };
    let plan_ref = ResourceRef::generated("importplan");
    let template = rename_pattern.as_deref().unwrap_or_default();
    let collection_for_name = collection.as_deref().unwrap_or_default();

    let mut items = Vec::with_capacity(candidates.len());
    let mut ready = 0i64;
    let mut limited = 0i64;
    let mut unsupported = 0i64;
    let mut total_size = 0i64;
    for (ordinal, candidate) in candidates.iter().enumerate() {
        check_cancel(cancel)?;
        let content_sha256 =
            crate::utils::hash::sha256_full_with_checkpoint(&candidate.scan_path, || {
                check_cancel(cancel)?;
                validate_import_read(db, caller, candidate.file_ref.as_deref())
            })?;
        if candidate.kind == "image" {
            let ext = candidate
                .scan_path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let (status, message) = importer::classify_import_preview(&candidate.scan_path, ext);
            match status {
                importer::ImportPreviewStatus::Ready => ready += 1,
                importer::ImportPreviewStatus::Limited => limited += 1,
                importer::ImportPreviewStatus::Unsupported => unsupported += 1,
            }
            let stem = candidate
                .scan_path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(&candidate.display_name);
            let rename_preview = if rename_pattern.is_some() {
                let rendered = importer::plan_rename_preview(
                    template,
                    collection_for_name,
                    stem,
                    candidate.modified_at,
                    ordinal + 1,
                );
                format!("{rendered}.{}", extension_of(&candidate.display_name))
            } else {
                candidate.display_name.clone()
            };
            items.push(NewImportPlanItem {
                item_id: ResourceRef::generated("item").as_str().to_string(),
                plan_id: plan_ref.as_str().to_string(),
                ordinal: ordinal as i64,
                file_ref: candidate.file_ref.clone(),
                path: candidate.path.clone(),
                display_name: candidate.display_name.clone(),
                kind: candidate.kind.clone(),
                size: candidate.size,
                modified_at: candidate.modified_at,
                file_identity: candidate.identity.clone(),
                content_sha256: Some(content_sha256.clone()),
                preview_status: status_to_string(status).to_string(),
                preview_message: message,
                rename_preview,
                excluded: false,
            });
        } else {
            ready += 1;
            items.push(NewImportPlanItem {
                item_id: ResourceRef::generated("item").as_str().to_string(),
                plan_id: plan_ref.as_str().to_string(),
                ordinal: ordinal as i64,
                file_ref: candidate.file_ref.clone(),
                path: candidate.path.clone(),
                display_name: candidate.display_name.clone(),
                kind: candidate.kind.clone(),
                size: candidate.size,
                modified_at: candidate.modified_at,
                file_identity: candidate.identity.clone(),
                content_sha256: Some(content_sha256.clone()),
                preview_status: "ready".to_string(),
                preview_message: None,
                rename_preview: if rename_pattern.is_some() {
                    let rendered = importer::plan_rename_preview(
                        template,
                        collection_for_name,
                        stem_of(&candidate.display_name),
                        candidate.modified_at,
                        ordinal + 1,
                    );
                    format!("{rendered}.{}", extension_of(&candidate.display_name))
                } else {
                    candidate.display_name.clone()
                },
                excluded: false,
            });
        }
        check_cancel(cancel)?;
        total_size += candidate.size;
    }

    let new_plan = NewImportPlan {
        plan_id: plan_ref.as_str().to_string(),
        principal_id: caller.principal_id.as_str().to_string(),
        source_ref: plan_source_ref,
        target_collection: collection.clone(),
        rename_pattern: rename_pattern.clone(),
        library_id,
        library_epoch,
        item_count: items.len() as i64,
        ready_count: ready,
        limited_count: limited,
        unsupported_count: unsupported,
        total_size,
        created_at: now,
        expires_at: now + PLAN_TTL_SECONDS,
    };
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        check_cancel(cancel)?;
        import_plans::insert_plan_with_cancel(&conn, &new_plan, &items, cancel)?;
    }
    get_import_plan(db, caller, &plan_ref)
}

/// Re-prepare exactly the stored candidate set for an explicit user edit. This
/// lets the GUI remove an item or change rename settings without resubmitting
/// arbitrary paths, expanding a directory, or copying the whole file list into
/// a persistent front-end draft. Every surviving candidate is reopened and
/// fingerprinted again under its original authorization context.
fn reprepare_from_plan(
    db: &Database,
    caller: &CallerContext,
    base_plan_ref: &ResourceRef,
    excluded_item_refs: &[ResourceRef],
    cancel: &AtomicBool,
) -> AppResult<(Vec<Candidate>, Option<String>)> {
    let (plan, items) = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let plan = load_owned(&conn, caller, base_plan_ref)?;
        if plan.library_id != caller.library.library_id.as_str()
            || plan.library_epoch != caller.library.library_epoch.as_str()
        {
            return Err(AppError::from(AgentErrorCode::LibraryChanged));
        }
        let items = import_plans::items(&conn, plan.plan_id.as_str())?;
        (plan, items)
    };
    let known: HashSet<String> = items.iter().map(|item| item.item_id.clone()).collect();
    let excluded: HashSet<String> = excluded_item_refs
        .iter()
        .map(|reference| reference.as_str().to_string())
        .collect();
    if excluded.iter().any(|item_ref| !known.contains(item_ref)) {
        return Err(AppError::invalid_arg("待剔除项目不属于当前导入计划"));
    }

    let mut candidates = Vec::with_capacity(items.len().saturating_sub(excluded.len()));
    for item in items {
        check_cancel(cancel)?;
        if excluded.contains(item.item_id.as_str()) {
            continue;
        }
        let candidate = if let Some(file_ref) = item.file_ref.as_deref() {
            let file_ref = ResourceRef::from_trusted(file_ref.to_string())?;
            let location = auth_service::locate_authorized_file(db, caller, &file_ref)?;
            Candidate {
                file_ref: Some(file_ref.as_str().to_string()),
                path: item.path,
                scan_path: location.path,
                display_name: item.display_name,
                kind: item.kind,
                size: location.size,
                modified_at: location.modified_at,
                identity: location.identity,
            }
        } else {
            enumerate_explicit_paths(&[item.path], cancel)?
                .into_iter()
                .next()
                .ok_or_else(|| AppError::conflict("导入计划中的所选文件已不可用"))?
        };
        candidates.push(candidate);
    }
    if candidates.is_empty() {
        return Err(AppError::invalid_arg("重新准备后没有可入库的候选文件"));
    }
    Ok((candidates, plan.source_ref))
}

pub fn get_import_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<ImportPlanView> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let plan = load_owned(&conn, caller, plan_ref)?;
    let items = import_plans::items(&conn, plan.plan_id.as_str())?;
    Ok(ImportPlanView {
        plan_ref: ResourceRef::from_trusted(plan.plan_id.clone())?,
        source_ref: plan
            .source_ref
            .clone()
            .map(ResourceRef::from_trusted)
            .transpose()?,
        target_collection: plan.target_collection.clone(),
        rename_pattern: plan.rename_pattern.clone(),
        item_count: u32_of(plan.item_count)?,
        ready_count: u32_of(plan.ready_count)?,
        limited_count: u32_of(plan.limited_count)?,
        unsupported_count: u32_of(plan.unsupported_count)?,
        total_size: plan.total_size,
        expires_at_unix_seconds: plan.expires_at,
        items: items
            .iter()
            .map(|item| {
                Ok(ImportPlanItemView {
                    item_ref: ResourceRef::from_trusted(item.item_id.clone())?,
                    display_name: item.display_name.clone(),
                    kind: item.kind.clone(),
                    size: item.size,
                    modified_at: item.modified_at,
                    preview_status: item.preview_status.clone(),
                    preview_message: item.preview_message.clone(),
                    rename_preview: item.rename_preview.clone(),
                    excluded: item.excluded,
                })
            })
            .collect::<AppResult<Vec<_>>>()?,
    })
}

/// Resolve paths for the trusted desktop preview only. The durable/shared plan
/// DTO stays path-free; callers must present an owned plan and exact item refs.
pub fn preview_paths_for_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<std::collections::BTreeMap<String, String>> {
    let items = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let plan = load_owned(&conn, caller, plan_ref)?;
        import_plans::items(&conn, plan.plan_id.as_str())?
    };
    let mut paths = std::collections::BTreeMap::new();
    for item in items.into_iter().filter(|item| !item.excluded) {
        let path = if let Some(file_ref) = item.file_ref.as_deref() {
            let reference = ResourceRef::from_trusted(file_ref.to_string())?;
            auth_service::locate_authorized_file(db, caller, &reference)
                .map(|location| location.path)
                .map_err(|_| AppError::from(AgentErrorCode::ReferenceExpired))?
        } else {
            let source_path = Path::new(&item.path);
            let metadata = std::fs::symlink_metadata(source_path)
                .map_err(|_| AppError::from(AgentErrorCode::ReferenceExpired))?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
            PathBuf::from(&item.path)
        };
        paths.insert(item.item_id, path.to_string_lossy().into_owned());
    }
    Ok(paths)
}

/// Re-check the whole plan: report items whose file changed since preparation,
/// and whether the library/grant generation moved. A stale item must be
/// re-prepared; submit must not silently use the old fingerprint.
pub fn verify_import_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<ImportPlanVerification> {
    let plan = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        load_owned(&conn, caller, plan_ref)?
    };
    let items = {
        let conn = db.lock()?;
        import_plans::items(&conn, plan.plan_id.as_str())?
    };
    let library_changed = {
        let conn = db.lock()?;
        let library = libraries::current(&conn)?
            .ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
        library.library_id != plan.library_id || library.library_epoch != plan.library_epoch
    };
    let mut stale = Vec::new();
    for item in &items {
        let current = if let Some(file_ref) = item.file_ref.as_deref() {
            let reference = ResourceRef::from_trusted(file_ref.to_string())?;
            match auth_service::locate_authorized_file(db, caller, &reference) {
                Ok(location) => Some((
                    location.size,
                    location.modified_at,
                    location.identity,
                    location.path,
                )),
                Err(_) => None,
            }
        } else {
            fingerprint_explicit(Path::new(&item.path)).ok()
        };
        match current {
            Some((size, modified_at, identity, path)) => {
                if size != item.size
                    || modified_at != item.modified_at
                    || identity != item.file_identity
                    || item.content_sha256.is_none()
                    || crate::utils::hash::sha256_full_with_checkpoint(&path, || {
                        validate_import_read(db, caller, item.file_ref.as_deref())
                    })
                    .ok()
                    .as_ref()
                        != item.content_sha256.as_ref()
                {
                    stale.push(ResourceRef::from_trusted(item.item_id.clone())?);
                }
            }
            None => stale.push(ResourceRef::from_trusted(item.item_id.clone())?),
        }
    }
    Ok(ImportPlanVerification {
        plan_ref: ResourceRef::from_trusted(plan.plan_id)?,
        stale_item_refs: stale,
        library_changed,
    })
}

fn validate_import_read(
    db: &Database,
    caller: &CallerContext,
    file_ref: Option<&str>,
) -> AppResult<()> {
    if let Some(file_ref) = file_ref {
        auth_service::validate_authorized_file_grant(
            db,
            caller,
            &ResourceRef::from_trusted(file_ref.to_string())?,
        )
    } else {
        let conn = db.lock()?;
        validate_caller(&conn, caller)
    }
}

fn check_cancel(cancel: &AtomicBool) -> AppResult<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::cancelled("导入准备已取消"));
    }
    Ok(())
}

fn enumerate_authorized_directory(
    db: &Database,
    caller: &CallerContext,
    source_ref: &ResourceRef,
    cancel: &AtomicBool,
) -> AppResult<Vec<Candidate>> {
    // source_ref is an opaque directory handle, not a grant ID. Paging
    // validates ownership, generations and read access; export-only handles
    // cannot pass that check and must never be reissued as import authority.
    let mut cursor = None;
    let mut candidates = Vec::new();
    loop {
        check_cancel(cancel)?;
        let page = auth_service::page_authorized_directory(
            db,
            caller,
            source_ref,
            cursor,
            DIRECTORY_PAGE_LIMIT,
        )?;
        for entry in page.items {
            check_cancel(cancel)?;
            let location = auth_service::locate_authorized_file(db, caller, &entry.entry_ref)?;
            let display_name = entry
                .name
                .rsplit('/')
                .next()
                .unwrap_or(entry.name.as_str())
                .to_string();
            let kind = importer::kind_for_display_name(&display_name);
            candidates.push(Candidate {
                file_ref: Some(entry.entry_ref.as_str().to_string()),
                path: entry.name.clone(),
                scan_path: location.path,
                display_name,
                kind: kind.to_string(),
                size: location.size,
                modified_at: location.modified_at,
                identity: location.identity,
            });
            if candidates.len() > MAX_PLAN_ITEMS {
                return Err(AppError::from(AgentErrorCode::InvalidInput));
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(candidates)
}

fn enumerate_explicit_paths(paths: &[String], cancel: &AtomicBool) -> AppResult<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for raw in paths {
        check_cancel(cancel)?;
        let path = PathBuf::from(raw);
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| AppError::conflict("所选文件不可访问"))?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        let path_text = encode_native_path(&path)?;
        let display_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| AppError::from(AgentErrorCode::InvalidInput))?
            .to_string();
        let kind = importer::kind_for_display_name(&display_name);
        let (size, modified_at, identity, _canonical) = fingerprint_explicit(&path)?;
        candidates.push(Candidate {
            file_ref: None,
            path: path_text,
            scan_path: path,
            display_name,
            kind: kind.to_string(),
            size,
            modified_at,
            identity,
        });
        if candidates.len() > MAX_PLAN_ITEMS {
            return Err(AppError::from(AgentErrorCode::InvalidInput));
        }
    }
    Ok(candidates)
}

fn fingerprint_explicit(path: &Path) -> AppResult<(i64, i64, String, PathBuf)> {
    let file = File::open(path).map_err(|_| AppError::conflict("所选文件不可访问"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::conflict("所选文件不可访问"))?;
    if !metadata.is_file() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let identity = file_identity(&file)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::conflict("所选文件不可访问"))?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    Ok((metadata.len() as i64, modified_at, identity, canonical))
}

fn resolve_exclusions(
    db: &Database,
    caller: &CallerContext,
    base_plan_ref: Option<&ResourceRef>,
    excluded_item_refs: &[ResourceRef],
) -> AppResult<HashSet<String>> {
    if excluded_item_refs.is_empty() {
        return Ok(HashSet::new());
    }
    let base = base_plan_ref.ok_or_else(|| AppError::from(AgentErrorCode::InvalidInput))?;
    let item_ids: Vec<String> = excluded_item_refs
        .iter()
        .map(|value| value.as_str().to_string())
        .collect();
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let plan = load_owned(&conn, caller, base)?;
    let paths = import_plans::excluded_paths(&conn, plan.plan_id.as_str(), &item_ids)?;
    Ok(paths.into_iter().collect())
}

fn load_owned(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<import_plans::ImportPlanRecord> {
    let plan = import_plans::plan(conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if plan.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(plan)
}

fn validate_caller(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    let principal = authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

fn status_to_string(status: importer::ImportPreviewStatus) -> &'static str {
    match status {
        importer::ImportPreviewStatus::Ready => "ready",
        importer::ImportPreviewStatus::Limited => "limited",
        importer::ImportPreviewStatus::Unsupported => "unsupported",
    }
}

fn extension_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn stem_of(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name)
}

fn u32_of(value: i64) -> AppResult<u32> {
    u32::try_from(value).map_err(|_| AppError::internal("导入计划数值越界"))
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
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;

    fn ready_db(root: &Path) -> Database {
        let conn = crate::db::init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        Database::new(conn)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_import_prepare".to_string(),
                instance_id: "instance_import_prepare".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn write_jpg(path: &Path, seed: u8) {
        image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([
                (x as u8).wrapping_add(seed),
                (y as u8).wrapping_add(seed),
                seed,
            ])
        })
        .save(path)
        .unwrap();
    }

    fn request(paths: Vec<String>) -> PrepareImportRequest {
        PrepareImportRequest {
            source_ref: None,
            paths,
            collection: None,
            rename_pattern: None,
            base_plan_ref: None,
            excluded_item_refs: Vec::new(),
        }
    }

    fn asset_count(db: &Database) -> i64 {
        let conn = db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn hash_reads_stop_at_the_next_chunk_on_cancel_or_revocation() {
        use crate::utils::hash::with_hash_chunk_hook;
        use std::cell::Cell;
        use std::rc::Rc;

        for mode in [
            "prepare_revoke",
            "prepare_cancel",
            "verify_revoke",
            "rebind_revoke",
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            std::fs::write(source.path().join("large.mp4"), vec![7u8; 3 * 1024 * 1024]).unwrap();
            let db = Rc::new(ready_db(root.path()));
            let caller = gui_caller(&db);
            let grant = authorization::grant_selected_directory(
                &db,
                &caller,
                source.path(),
                authorization::DirectoryPurpose::ImportSource,
                false,
            )
            .unwrap();
            let mut req = request(Vec::new());
            req.source_ref = grant.directory_ref.clone();
            let existing = if mode.starts_with("prepare") {
                None
            } else {
                Some(prepare_import(&db, &caller, req.clone()).unwrap())
            };
            let cancel = Rc::new(AtomicBool::new(false));
            let count = Rc::new(Cell::new(0));
            let hook_db = Rc::clone(&db);
            let hook_caller = caller.clone();
            let hook_cancel = Rc::clone(&cancel);
            let hook_count = Rc::clone(&count);
            let grant_ref = grant.grant_ref;
            with_hash_chunk_hook(
                move || {
                    hook_count.set(hook_count.get() + 1);
                    if hook_count.get() == 1 {
                        if mode == "prepare_cancel" {
                            hook_cancel.store(true, Ordering::Relaxed);
                        } else {
                            authorization::revoke_directory_grant(
                                &hook_db,
                                &hook_caller,
                                &grant_ref,
                            )
                            .unwrap();
                        }
                    }
                },
                || {
                    if mode.starts_with("prepare") {
                        let error =
                            prepare_import_with_cancel(&db, &caller, req, &cancel).unwrap_err();
                        assert_eq!(
                            error.code(),
                            if mode == "prepare_cancel" {
                                "CANCELLED"
                            } else {
                                "GRANT_REVOKED"
                            }
                        );
                        assert_eq!(
                            db.lock()
                                .unwrap()
                                .query_row("SELECT COUNT(*) FROM import_plans", [], |r| r
                                    .get::<_, i64>(0))
                                .unwrap(),
                            0
                        );
                    } else if mode == "verify_revoke" {
                        let result =
                            verify_import_plan(&db, &caller, &existing.as_ref().unwrap().plan_ref)
                                .unwrap();
                        assert_eq!(result.stale_item_refs.len(), 1);
                    } else {
                        let items = import_plans::items(
                            &db.lock().unwrap(),
                            existing.as_ref().unwrap().plan_ref.as_str(),
                        )
                        .unwrap();
                        let error = authorization::prepare_import_file_rebinding(
                            &db,
                            &caller,
                            grant.directory_ref.as_ref().unwrap(),
                            &items,
                        )
                        .unwrap_err();
                        assert_eq!(error.code(), "GRANT_REVOKED");
                    }
                },
            );
            assert_eq!(
                count.get(),
                1,
                "{mode}: a revoked read must not consume a second chunk"
            );
            assert_eq!(asset_count(&db), 0);
            assert_eq!(
                std::fs::metadata(source.path().join("large.mp4"))
                    .unwrap()
                    .len(),
                3 * 1024 * 1024
            );
        }
    }

    #[test]
    fn prepare_binds_fingerprint_and_rename_preview_without_writing_assets() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 3);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let root_entries_before = std::fs::read_dir(managed.path()).unwrap().count();

        let mut req = request(vec![encode_native_path(&jpg).unwrap()]);
        req.collection = Some("茶具".to_string());
        req.rename_pattern = Some("{分库}_{原名}_{序号:3}".to_string());
        let plan = prepare_import(&db, &caller, req).unwrap();

        assert_eq!(plan.item_count, 1);
        assert_eq!(plan.ready_count, 1);
        assert_eq!(plan.unsupported_count, 0);
        let item = &plan.items[0];
        assert!(
            item.rename_preview.starts_with("茶具_ok_"),
            "{:?}",
            item.rename_preview
        );
        assert!(item.rename_preview.ends_with(".jpg"));
        assert_eq!(
            std::fs::read_dir(managed.path()).unwrap().count(),
            root_entries_before,
            "准备阶段不得在总库产生任何文件"
        );
        assert_eq!(asset_count(&db), 0, "准备阶段不得写入正式资产");
    }

    #[test]
    fn preview_paths_are_resolved_only_from_owned_plan_items() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let first_path = source.path().join("first.jpg");
        let second_path = source.path().join("second.jpg");
        write_jpg(&first_path, 8);
        write_jpg(&second_path, 9);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let first = prepare_import(
            &db,
            &caller,
            request(vec![
                encode_native_path(&first_path).unwrap(),
                encode_native_path(&second_path).unwrap(),
            ]),
        )
        .unwrap();
        let removed = first
            .items
            .iter()
            .find(|item| item.display_name == "first.jpg")
            .unwrap();
        let mut reprepare = request(Vec::new());
        reprepare.base_plan_ref = Some(first.plan_ref.clone());
        reprepare.excluded_item_refs = vec![removed.item_ref.clone()];
        let second = prepare_import(&db, &caller, reprepare).unwrap();
        let paths = preview_paths_for_plan(&db, &caller, &second.plan_ref).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths.values().next(),
            Some(&encode_native_path(&second_path).unwrap())
        );
        assert!(!paths
            .values()
            .any(|path| path == &encode_native_path(&first_path).unwrap()));
    }

    #[test]
    fn preview_fails_path_free_when_a_prepared_source_disappears() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("vanished.jpg");
        write_jpg(&jpg, 14);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let plan = prepare_import(
            &db,
            &caller,
            request(vec![encode_native_path(&jpg).unwrap()]),
        )
        .unwrap();
        std::fs::remove_file(&jpg).unwrap();

        let error = preview_paths_for_plan(&db, &caller, &plan.plan_ref).unwrap_err();
        assert_eq!(error.code(), "REFERENCE_EXPIRED");
        assert!(!error
            .to_string()
            .contains(source.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn unsupported_items_stay_visible_and_excluded_on_reprepare() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let good = source.path().join("good.jpg");
        let bad = source.path().join("broken.jpg");
        write_jpg(&good, 1);
        std::fs::write(&bad, b"not an image").unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);

        let first = prepare_import(
            &db,
            &caller,
            request(vec![
                encode_native_path(&good).unwrap(),
                encode_native_path(&bad).unwrap(),
            ]),
        )
        .unwrap();
        assert_eq!(first.unsupported_count, 1);
        let unsupported = first
            .items
            .iter()
            .find(|item| item.preview_status == "unsupported")
            .unwrap();

        let mut reprepare = request(vec![
            encode_native_path(&good).unwrap(),
            encode_native_path(&bad).unwrap(),
        ]);
        reprepare.base_plan_ref = Some(first.plan_ref.clone());
        reprepare.excluded_item_refs = vec![unsupported.item_ref.clone()];
        let second = prepare_import(&db, &caller, reprepare).unwrap();
        assert_eq!(second.item_count, 1);
        assert!(second
            .items
            .iter()
            .all(|item| !item.display_name.contains("broken")));
    }

    #[test]
    fn reprepare_from_stored_plan_keeps_exact_candidate_set_without_resending_paths() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let first_path = source.path().join("first.jpg");
        let second_path = source.path().join("second.jpg");
        write_jpg(&first_path, 1);
        write_jpg(&second_path, 2);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let first = prepare_import(
            &db,
            &caller,
            request(vec![
                encode_native_path(&first_path).unwrap(),
                encode_native_path(&second_path).unwrap(),
            ]),
        )
        .unwrap();
        let removed = &first.items[0];

        let mut reprepare = request(Vec::new());
        reprepare.base_plan_ref = Some(first.plan_ref.clone());
        reprepare.excluded_item_refs = vec![removed.item_ref.clone()];
        reprepare.collection = Some("回看".into());
        let second = prepare_import(&db, &caller, reprepare).unwrap();

        assert_eq!(second.item_count, 1);
        assert_eq!(second.target_collection.as_deref(), Some("回看"));
        assert_eq!(second.items[0].display_name, "second.jpg");
    }

    #[test]
    fn verify_reports_files_changed_after_preparation() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 5);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let plan = prepare_import(
            &db,
            &caller,
            request(vec![encode_native_path(&jpg).unwrap()]),
        )
        .unwrap();
        assert!(verify_import_plan(&db, &caller, &plan.plan_ref)
            .unwrap()
            .stale_item_refs
            .is_empty());

        write_jpg(&jpg, 200);
        let verification = verify_import_plan(&db, &caller, &plan.plan_ref).unwrap();
        assert_eq!(verification.stale_item_refs.len(), 1);
        assert!(!verification.library_changed);
    }

    #[test]
    fn unknown_directory_source_and_illegal_collection_are_structured_errors() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let mut req = request(Vec::new());
        req.source_ref = Some(ResourceRef::generated("directory"));
        let error = prepare_import(&db, &caller, req).unwrap_err();
        assert!(
            matches!(error.code(), "REFERENCE_EXPIRED" | "UNAUTHORIZED"),
            "unexpected code {}",
            error.code()
        );

        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 2);
        let mut req = request(vec![encode_native_path(&jpg).unwrap()]);
        req.collection = Some("../escape".to_string());
        assert_eq!(
            prepare_import(&db, &caller, req).unwrap_err().code(),
            "INVALID_ARG"
        );
    }
    #[test]
    fn cancelled_preparation_does_not_persist_a_plan_or_assets() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("cancelled.jpg");
        write_jpg(&file, 1);
        let db = ready_db(root.path());
        let caller = gui_caller(&db);
        let req = request(vec![file.to_string_lossy().to_string()]);
        let cancelled = AtomicBool::new(true);
        let error = prepare_import_with_cancel(&db, &caller, req.clone(), &cancelled).unwrap_err();
        assert_eq!(error.code(), "CANCELLED");
        {
            let conn = db.lock().unwrap();
            let count: i64 = conn
                .query_row("SELECT count(*) FROM import_plans", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        assert_eq!(asset_count(&db), 0);
        // Cancelling this task must not poison another preparation.
        let other = AtomicBool::new(false);
        let prepared = prepare_import_with_cancel(&db, &caller, req, &other).unwrap();
        assert_eq!(prepared.item_count, 1);
        assert_eq!(asset_count(&db), 0);
    }

    #[test]
    fn cancelled_enumeration_stops_before_opening_another_path() {
        let cancel = AtomicBool::new(true);
        let result = enumerate_explicit_paths(&["missing-file.jpg".into()], &cancel);
        assert_eq!(result.err().unwrap().code(), "CANCELLED");
    }
    #[test]
    fn authorized_directory_handle_prepares_candidates_without_reissuing_grant() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_jpg(&source.path().join("authorized.jpg"), 7);
        let db = ready_db(root.path());
        let caller = gui_caller(&db);
        let grant = authorization::grant_selected_directory(
            &db,
            &caller,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        let mut req = request(Vec::new());
        req.source_ref = grant.directory_ref;
        let prepared = prepare_import(&db, &caller, req).unwrap();
        assert_eq!(prepared.item_count, 1);
        assert_eq!(prepared.items[0].display_name, "authorized.jpg");
        assert_eq!(asset_count(&db), 0);
        let export = authorization::grant_selected_directory(
            &db,
            &caller,
            source.path(),
            authorization::DirectoryPurpose::ExportDestination,
            false,
        )
        .unwrap();
        let mut invalid = request(Vec::new());
        invalid.source_ref = export.directory_ref;
        assert_eq!(
            prepare_import(&db, &caller, invalid).unwrap_err().code(),
            "UNAUTHORIZED"
        );
    }
    #[test]
    fn verify_detects_same_size_rewrite_with_restored_modification_time() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("bytes.jpg");
        write_jpg(&path, 21);
        let db = ready_db(root.path());
        let caller = gui_caller(&db);
        let plan = prepare_import(
            &db,
            &caller,
            request(vec![encode_native_path(&path).unwrap()]),
        )
        .unwrap();
        let before = fingerprint_explicit(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = fingerprint_explicit(&path).unwrap();
        assert_eq!((before.0, before.1, before.2), (after.0, after.1, after.2));
        assert_eq!(
            verify_import_plan(&db, &caller, &plan.plan_ref)
                .unwrap()
                .stale_item_refs
                .len(),
            1
        );
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE import_plan_items SET content_sha256=NULL WHERE plan_id=?1",
                [plan.plan_ref.as_str()],
            )
            .unwrap();
        }
        assert_eq!(
            verify_import_plan(&db, &caller, &plan.plan_ref)
                .unwrap()
                .stale_item_refs
                .len(),
            1
        );
    }
}
