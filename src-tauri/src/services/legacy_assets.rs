//! Trusted maintenance for historical asset rows outside the current library.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contracts::agent::{CallerKind, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::libraries::{self, LibraryHealth, LibrarySnapshot, PendingClassification};
use crate::db::{self, legacy_assets as legacy_db};
use crate::error::{AppError, AppResult};
use crate::services::{authorization, managed_library};
use crate::state::Database;
use crate::utils::file_identity::{file_identity, file_object_identity};
use crate::utils::path::{encode_native_path, is_link_or_reparse, normalize_path};

const RESOLUTION_TTL_SECONDS: i64 = 15 * 60;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingLegacyAssetPage {
    pub library_health: String,
    pub can_resolve: bool,
    pub root_path: Option<String>,
    pub items: Vec<legacy_db::PendingLegacyAsset>,
    pub next_after_asset_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionPreview {
    pub resolution_id: ResourceRef,
    pub asset: legacy_db::PendingLegacyAsset,
    pub action: String,
    pub matching_assets: Vec<legacy_db::HashMatchCandidate>,
    pub keep_separate_required: bool,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionResult {
    pub resolution_id: ResourceRef,
    pub asset_id: i64,
    pub action: String,
    pub state: String,
    pub target_path: Option<String>,
    pub matching_assets: Vec<legacy_db::HashMatchCandidate>,
    pub error_code: Option<String>,
    pub expires_at: i64,
}

struct SourceFileSnapshot {
    file_identity: String,
    file_size: i64,
    file_modified_at: i64,
    file_modified_at_ns: i64,
    sha256: String,
}

pub fn list_pending(
    db: &Database,
    caller: &CallerContext,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<PendingLegacyAssetPage> {
    if caller.kind != CallerKind::Gui {
        return Err(AppError::unauthorized("该维护功能仅供茶馆界面使用"));
    }
    let (library, items) = {
        let conn = db.lock()?;
        authorization::validate_caller_session(&conn, caller)?;
        (
            libraries::current(&conn)?,
            legacy_db::pending_page(&conn, after_asset_id, limit)?,
        )
    };
    let (mut health, root_path, mut can_resolve) = match &library {
        Some(library) => (
            library.health.as_str().to_string(),
            Some(library.root_path.clone()),
            library.health == LibraryHealth::Ready,
        ),
        None => ("unconfigured".to_string(), None, false),
    };
    if let Some(library) = library.as_ref().filter(|_| can_resolve) {
        if let Err(error) = managed_library::verified_root_path(library) {
            health = if error.code() == "CONFLICT" {
                "wrong_root".to_string()
            } else {
                "offline".to_string()
            };
            can_resolve = false;
        }
    }
    let next_after_asset_id = (items.len() == limit)
        .then(|| items.last().map(|item| item.asset_id))
        .flatten();
    Ok(PendingLegacyAssetPage {
        library_health: health,
        can_resolve,
        root_path,
        items,
        next_after_asset_id,
    })
}

pub fn list_unlinked(
    db: &Database,
    caller: &CallerContext,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<Vec<legacy_db::PendingLegacyAsset>> {
    require_gui(caller)?;
    let conn = db.lock()?;
    authorization::validate_caller_session(&conn, caller)?;
    legacy_db::unlinked_page(&conn, after_asset_id, limit)
}

pub fn prepare_unlink(
    db: &Database,
    caller: &CallerContext,
    asset_id: i64,
) -> AppResult<ResolutionPreview> {
    require_gui(caller)?;
    let (library, asset) = {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
        let library = ready_library(&conn)?;
        let asset = legacy_db::pending_asset(&conn, asset_id)?
            .ok_or_else(|| AppError::conflict("旧记录范围已变化，请刷新后重试"))?;
        if asset.status != "external_pending" {
            return Err(AppError::conflict("只有已确认位于总仓外的记录可解除关联"));
        }
        (library, asset)
    };
    managed_library::verified_root_path(&library)?;
    let (_, source_identity, source_file_size, source_file_modified_at, source_file_modified_at_ns) =
        match inspect_unlink_source(&asset, &library, None) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                classify_asset_unknown_if_root_ready(db, caller, &library, asset_id)?;
                return Err(error);
            }
        };
    let now = now_unix_seconds()?;
    let id = ResourceRef::generated("legacy");
    let source_path = asset.file_path.clone();
    let conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    ensure_same_library(&conn, &library)?;
    legacy_db::expire_prepared(&conn, now)?;
    legacy_db::insert_prepared(
        &conn,
        &legacy_db::NewResolution {
            resolution_id: id.as_str(),
            asset_id,
            action: "unlink",
            library_id: &library.library_id,
            library_epoch: &library.library_epoch,
            source_path: &source_path,
            source_size: asset.file_size,
            source_modified_at: asset.modified_at,
            source_file_size,
            source_file_modified_at,
            source_file_modified_at_ns,
            source_file_identity: Some(&source_identity),
            source_sha256: None,
            target_path: None,
            matching_assets_json: "[]",
            now,
            expires_at: now + RESOLUTION_TTL_SECONDS,
        },
    )?;
    Ok(ResolutionPreview {
        resolution_id: id,
        asset,
        action: "unlink".to_string(),
        matching_assets: Vec::new(),
        keep_separate_required: false,
        expires_at: now + RESOLUTION_TTL_SECONDS,
    })
}

/// The source path comes from a native file picker, never from IPC input.
pub fn prepare_copy_back(
    db: &Database,
    caller: &CallerContext,
    asset_id: i64,
    selected_source: &Path,
) -> AppResult<ResolutionPreview> {
    require_gui(caller)?;
    let (library, asset) = {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
        let library = ready_library(&conn)?;
        let asset = legacy_db::pending_asset(&conn, asset_id)?
            .ok_or_else(|| AppError::conflict("旧记录范围已变化，请刷新后重试"))?;
        if asset.status != "external_pending" {
            return Err(AppError::conflict("只有已确认位于总仓外的记录可复制回仓"));
        }
        (library, asset)
    };
    let root = managed_library::verified_root_path(&library)?;
    let source = match inspect_selected_source(&asset.file_path, selected_source, &root) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if inspect_unlink_source(&asset, &library, None).is_err() {
                classify_asset_unknown_if_root_ready(db, caller, &library, asset_id)?;
            }
            return Err(error);
        }
    };
    if source.file_size != asset.file_size || source.file_modified_at != asset.modified_at {
        classify_asset_unknown_if_root_ready(db, caller, &library, asset_id)?;
        return Err(AppError::conflict("原文件已变化，不能复制回仓"));
    }

    let candidate_rows = {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
        ensure_same_library(&conn, &library)?;
        legacy_db::exact_hash_candidates(&conn, &source.sha256)?
    };
    let mut matching_assets = Vec::new();
    for candidate in candidate_rows {
        let asset_ref = authorization::issue_asset_handle(db, caller, candidate.asset_id)?;
        let opened = authorization::open_authorized_asset(db, caller, &asset_ref)?;
        let (digest, size, modified_at) = digest_open_file(opened.file())?;
        if digest != source.sha256 || size != opened.size() || modified_at != opened.modified_at() {
            return Err(AppError::conflict(
                "已有托管素材无法通过当前文件摘要复核，请先刷新素材库",
            ));
        }
        if digest == candidate.content_sha256 {
            matching_assets.push(candidate);
        }
    }

    let id = ResourceRef::generated("legacy");
    let target = choose_target(db, &library, selected_source)?;
    let target_path = encode_native_path(&target)?;
    let matching_json = serde_json::to_string(&matching_assets)
        .map_err(|error| AppError::internal(format!("冲突预览编码失败: {error}")))?;
    let now = now_unix_seconds()?;
    let expires_at = now + RESOLUTION_TTL_SECONDS;
    {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
        ensure_same_library(&conn, &library)?;
        let current = legacy_db::pending_asset(&conn, asset_id)?
            .ok_or_else(|| AppError::conflict("旧记录范围已变化，请刷新后重试"))?;
        if current.status != "external_pending"
            || current.file_path != asset.file_path
            || current.file_size != asset.file_size
            || current.modified_at != asset.modified_at
        {
            return Err(AppError::conflict("原文件记录已变化，请重新准备"));
        }
        legacy_db::expire_prepared(&conn, now)?;
        legacy_db::insert_prepared(
            &conn,
            &legacy_db::NewResolution {
                resolution_id: id.as_str(),
                asset_id,
                action: "copy_back",
                library_id: &library.library_id,
                library_epoch: &library.library_epoch,
                source_path: &asset.file_path,
                source_size: asset.file_size,
                source_modified_at: asset.modified_at,
                source_file_size: source.file_size,
                source_file_modified_at: source.file_modified_at,
                source_file_modified_at_ns: source.file_modified_at_ns,
                source_file_identity: Some(&source.file_identity),
                source_sha256: Some(&source.sha256),
                target_path: Some(&target_path),
                matching_assets_json: &matching_json,
                now,
                expires_at,
            },
        )?;
    }
    Ok(ResolutionPreview {
        resolution_id: id,
        asset,
        action: "copy_back".to_string(),
        keep_separate_required: !matching_assets.is_empty(),
        matching_assets,
        expires_at,
    })
}

/// 读一条待处理记录（供批量处置按记录定位用户选中的文件）。
///
/// 只做 GUI 会话校验 + 同一次读；真正的所有权复核仍由 `prepare_copy_back`
/// 重做一遍——批量入口不允许跳过任何一项检查。
pub fn pending_record(
    db: &Database,
    caller: &CallerContext,
    asset_id: i64,
) -> AppResult<Option<legacy_db::PendingLegacyAsset>> {
    require_gui(caller)?;
    let conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    legacy_db::pending_asset(&conn, asset_id)
}

/// 读一条处置收据对应的素材 id。批量确认要先用它给失败项归位，因为确认成功后
/// 该记录会从待处理投影里消失，回头再查就拿不到 id 了。
pub fn resolution_asset_id(
    db: &Database,
    caller: &CallerContext,
    id: &ResourceRef,
) -> AppResult<Option<i64>> {
    require_gui(caller)?;
    let conn = db.lock()?;
    authorization::validate_caller_grant(&conn, caller)?;
    Ok(legacy_db::get_resolution(&conn, id.as_str())?.map(|record| record.asset_id))
}

pub fn cancel_prepared(db: &Database, caller: &CallerContext, id: &ResourceRef) -> AppResult<()> {
    require_gui(caller)?;
    let conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    let record = legacy_db::get_resolution(&conn, id.as_str())?
        .ok_or_else(|| AppError::not_found("旧记录处置预览已失效"))?;
    if record.state != "prepared" {
        return Err(AppError::conflict("处置已开始，不能撤销预览"));
    }
    legacy_db::cancel_prepared(&conn, id.as_str(), now_unix_seconds()?)?;
    Ok(())
}

pub fn reopen_unlinked(db: &Database, caller: &CallerContext, asset_id: i64) -> AppResult<()> {
    require_gui(caller)?;
    let library = {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
        ready_library(&conn)?
    };
    managed_library::verified_root_path(&library)?;
    let conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    ensure_same_library(&conn, &library)?;
    if !legacy_db::reopen_unlinked(&conn, asset_id, now_unix_seconds()?)? {
        return Err(AppError::conflict("没有可重新打开的解除关联记录"));
    }
    Ok(())
}

pub fn confirm(
    db: &Database,
    caller: &CallerContext,
    id: &ResourceRef,
) -> AppResult<ResolutionResult> {
    require_gui(caller)?;
    let (record, library, asset) = {
        let conn = db.lock()?;
        // A still-authorized GUI may need to quarantine a receipt after the
        // library epoch changed. Validate its grant first, then validate the
        // full caller context after stale active receipts have been handled.
        authorization::validate_caller_grant(&conn, caller)?;
        let record = legacy_db::get_resolution(&conn, id.as_str())?
            .ok_or_else(|| AppError::not_found("旧记录处置预览已失效"))?;
        let library = ready_library(&conn)?;
        let asset = legacy_db::pending_asset(&conn, record.asset_id)?;
        (record, library, asset)
    };
    if matches!(record.state.as_str(), "prepared" | "copying" | "published") {
        managed_library::verified_root_path(&library)?;
        if record.library_id != library.library_id
            || record.library_epoch != library.library_epoch
            || caller.library.library_id.as_str() != library.library_id
            || caller.library.library_epoch.as_str() != library.library_epoch
        {
            mark_unknown(db, &record, "OUTCOME_UNKNOWN")?;
            return Err(AppError::conflict("总仓身份已变化，处置收据已暂停待核实"));
        }
    }
    {
        let conn = db.lock()?;
        authorization::validate_caller(&conn, caller)?;
    }
    if let Err(error) = ensure_record_library(&record, &library) {
        if matches!(record.state.as_str(), "prepared" | "copying" | "published") {
            mark_unknown(db, &record, "OUTCOME_UNKNOWN")?;
        }
        return Err(error);
    }
    if matches!(
        record.state.as_str(),
        "completed" | "unlinked" | "outcome_unknown"
    ) {
        return result_from_record(&record);
    }
    if record.expires_at <= now_unix_seconds()? && record.state == "prepared" {
        let conn = db.lock()?;
        legacy_db::expire_resolution(&conn, id.as_str(), now_unix_seconds()?)?;
        return Err(AppError::conflict("预览已过期，请重新准备"));
    }
    if matches!(record.state.as_str(), "prepared" | "copying" | "published") {
        let Some(asset) = asset.as_ref() else {
            mark_unknown(db, &record, "SOURCE_CHANGED")?;
            return Err(AppError::conflict("旧记录已变化，请刷新后重试"));
        };
        if asset.status != "external_pending"
            || asset.file_path != record.source_path
            || asset.file_size != record.source_size
            || asset.modified_at != record.source_modified_at
        {
            mark_unknown(db, &record, "SOURCE_CHANGED")?;
            return Err(AppError::conflict("旧记录状态已变化，处置已暂停"));
        }
    }
    let matching_assets = decode_matching_assets(&record)?;
    match record.action.as_str() {
        "unlink" => {
            let Some(asset) = asset.as_ref() else {
                return Err(AppError::conflict("旧记录已变化，请刷新后重试"));
            };
            if record.state != "prepared" {
                return Err(AppError::conflict("解除关联收据状态需要人工核实"));
            }
            let Some(source_file_identity) = record.source_file_identity.as_deref() else {
                fail_prepared(db, &record, "SOURCE_IDENTITY_MISSING")?;
                return Err(AppError::conflict("原文件身份收据缺失，已安全停止解除关联"));
            };
            let (_source_file, _, _, _, modified_at_ns) =
                match inspect_unlink_source(asset, &library, Some(source_file_identity)) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        classify_asset_unknown_if_root_ready(
                            db,
                            caller,
                            &library,
                            record.asset_id,
                        )?;
                        fail_prepared(db, &record, "SOURCE_CHANGED")?;
                        return Err(error);
                    }
                };
            if modified_at_ns != record.source_file_modified_at_ns {
                classify_asset_unknown_if_root_ready(db, caller, &library, record.asset_id)?;
                fail_prepared(db, &record, "SOURCE_CHANGED")?;
                return Err(AppError::conflict("原文件已变化，请重新准备"));
            }
            let mut conn = db.lock()?;
            authorization::validate_caller(&conn, caller)?;
            ensure_same_library(&conn, &library)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !legacy_db::finish_unlink(&tx, id.as_str(), now_unix_seconds()?)? {
                legacy_db::mark_unknown(&tx, id.as_str(), "SOURCE_CHANGED", now_unix_seconds()?)?;
                tx.commit()?;
                return Err(AppError::conflict("旧记录已变化，请刷新后重试"));
            }
            tx.commit()?;
        }
        "copy_back" => copy_back(db, caller, &record, &library)?,
        _ => return Err(AppError::internal("未知旧记录处置动作")),
    }
    let updated = {
        let conn = db.lock()?;
        legacy_db::get_resolution(&conn, id.as_str())?
            .ok_or_else(|| AppError::internal("旧记录处置收据缺失"))?
    };
    let mut result = result_from_record(&updated)?;
    result.matching_assets = matching_assets;
    Ok(result)
}

fn copy_back(
    db: &Database,
    caller: &CallerContext,
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
) -> AppResult<()> {
    copy_back_with_hook(db, caller, record, library, || Ok(()))
}

fn copy_back_with_hook<F>(
    db: &Database,
    caller: &CallerContext,
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
    after_copy_before_publish: F,
) -> AppResult<()>
where
    F: FnOnce() -> AppResult<()>,
{
    copy_back_with_hooks(
        db,
        caller,
        record,
        library,
        after_copy_before_publish,
        || Ok(()),
    )
}

fn copy_back_with_hooks<F, G>(
    db: &Database,
    caller: &CallerContext,
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
    after_copy_before_publish: F,
    after_publish_before_verify: G,
) -> AppResult<()>
where
    F: FnOnce() -> AppResult<()>,
    G: FnOnce() -> AppResult<()>,
{
    let mut current = {
        let conn = db.lock()?;
        legacy_db::get_resolution(&conn, &record.resolution_id)?
            .ok_or_else(|| AppError::not_found("旧记录处置预览已失效"))?
    };
    let Some(target_text) = current.target_path.as_deref() else {
        match current.state.as_str() {
            "prepared" => fail_prepared(db, &current, "RECEIPT_INCOMPLETE")?,
            "published" => mark_unknown(db, &current, "OUTCOME_UNKNOWN")?,
            "copying" => mark_copy_unknown(db, &current, "OUTCOME_UNKNOWN")?,
            _ => {}
        }
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    };
    let target = PathBuf::from(target_text);
    let Some(digest) = current.source_sha256.as_deref() else {
        match current.state.as_str() {
            "prepared" => fail_prepared(db, &current, "RECEIPT_INCOMPLETE")?,
            "published" => mark_unknown(db, &current, "OUTCOME_UNKNOWN")?,
            "copying" => mark_copy_unknown(db, &current, "OUTCOME_UNKNOWN")?,
            _ => {}
        }
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    };
    if let Err(error) = managed_library::validate_import_target(library, &target) {
        if managed_library::verified_root_path(library).is_ok() {
            match current.state.as_str() {
                "published" => mark_unknown(db, &current, "OUTCOME_UNKNOWN")?,
                "copying" => mark_copy_unknown(db, &current, "OUTCOME_UNKNOWN")?,
                "prepared" => mark_unknown(db, &current, "OUTCOME_UNKNOWN")?,
                _ => {}
            }
        }
        return Err(error);
    }
    if current.state == "prepared" {
        let root = managed_library::verified_root_path(library)?;
        let source_path = match Path::new(&current.source_path).canonicalize() {
            Ok(path) => path,
            Err(_) => {
                classify_asset_unknown_if_root_ready(db, caller, library, current.asset_id)?;
                fail_prepared(db, &current, "SOURCE_CHANGED")?;
                return Err(AppError::conflict("原文件已变化，请重新准备"));
            }
        };
        if source_path.starts_with(&root) {
            fail_prepared(db, &current, "PLAN_CHANGED")?;
            return Err(AppError::conflict("原文件已进入总仓，请重新核验记录"));
        }
        if let Err(error) = verify_source_snapshot(&current) {
            classify_asset_unknown_if_root_ready(db, caller, library, current.asset_id)?;
            fail_prepared(db, &current, "SOURCE_CHANGED")?;
            return Err(error);
        }
        if target.exists() {
            fail_prepared(db, &current, "TARGET_EXISTS")?;
            return Err(AppError::conflict("回仓目标已被占用，请重新准备"));
        }
        let target_db_path = normalize_path(target_text);
        let target_registered = {
            let conn = db.lock()?;
            db::assets::find_by_path(&conn, &target_db_path)?.is_some()
        };
        if target_registered {
            fail_prepared(db, &current, "TARGET_EXISTS")?;
            return Err(AppError::conflict("回仓目标已登记，请重新准备"));
        }
        let now = now_unix_seconds()?;
        let persisted = {
            let conn = db.lock()?;
            ensure_same_library(&conn, library)?;
            legacy_db::mark_copy_started(&conn, &current.resolution_id, target_text, now)?
        };
        if !persisted {
            return Err(AppError::conflict("旧记录处置状态已变化，请刷新后重试"));
        }
        current.state = "copying".to_string();
    }

    if current.state == "copying" {
        let mut after_copy_before_publish = Some(after_copy_before_publish);
        let mut after_publish_before_verify = Some(after_publish_before_verify);
        let stage = copy_and_publish_staged(
            db,
            library,
            &current,
            &target,
            digest,
            &mut after_copy_before_publish,
            &mut after_publish_before_verify,
        );
        let (object_identity, published_identity) = match stage {
            Ok(identities) => identities,
            Err(error) => {
                if !copy_back_is_resumable(db, library, &current, &target).unwrap_or(false)
                    && managed_library::verified_root_path(library).is_ok()
                {
                    mark_copy_unknown(db, &current, "OUTCOME_UNKNOWN")?;
                }
                return Err(error);
            }
        };
        current.created_file_identity = Some(object_identity);
        current.published_file_identity = Some(published_identity);
        current.state = "published".to_string();
    }

    if current.state != "published" {
        if current.state == "outcome_unknown" {
            return Err(AppError::conflict("OUTCOME_UNKNOWN"));
        }
        return Err(AppError::conflict("旧记录处置状态需要人工核实"));
    }
    commit_published_copy(db, caller, &current, library, &target)
}

fn copy_back_is_resumable(
    db: &Database,
    library: &LibrarySnapshot,
    record: &legacy_db::ResolutionRecord,
    target: &Path,
) -> AppResult<bool> {
    let (latest, asset) = {
        let conn = db.lock()?;
        let Some(latest) = legacy_db::get_resolution(&conn, &record.resolution_id)? else {
            return Ok(false);
        };
        if latest.state != "copying"
            || latest.target_path.as_deref() != record.target_path.as_deref()
        {
            return Ok(false);
        }
        ensure_same_library(&conn, library)?;
        let asset = legacy_db::pending_asset(&conn, latest.asset_id)?;
        (latest, asset)
    };
    let Some(asset) = asset else {
        return Ok(false);
    };
    if asset.status != "external_pending"
        || asset.file_path != latest.source_path
        || asset.file_size != latest.source_size
        || asset.modified_at != latest.source_modified_at
    {
        return Ok(false);
    }
    let Ok(root) = managed_library::verified_root_path(library) else {
        return Ok(false);
    };
    let Some(parent) = target.parent() else {
        return Ok(false);
    };
    if parent.canonicalize().ok().as_deref() != Some(root.as_path())
        || managed_library::validate_import_target(library, target).is_err()
        || verify_source_snapshot(&latest).is_err()
    {
        return Ok(false);
    }
    let stage = legacy_stage_path(library, &latest.resolution_id)?;
    let expected_identity = latest.created_file_identity.as_deref();
    let target_file = match open_owned_target_file(target, &root, expected_identity) {
        Ok(Some(file)) => Some(file),
        Ok(None) => None,
        Err(_) => return Ok(false),
    };
    let stage_file = match open_owned_stage_file(&stage, &root, expected_identity) {
        Ok(Some(file)) => Some(file),
        Ok(None) => None,
        Err(_) => return Ok(false),
    };

    match (target_file, stage_file, expected_identity) {
        (Some(target_file), stage_file, Some(_)) => {
            let (digest, size, _) = match digest_open_file(&target_file) {
                Ok(snapshot) => snapshot,
                Err(_) => return Ok(false),
            };
            if Some(digest.as_str()) != latest.source_sha256.as_deref()
                || size != latest.source_file_size
            {
                return Ok(false);
            }
            if let Some(stage_file) = stage_file {
                if file_object_identity(&stage_file).ok() != latest.created_file_identity {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (None, Some(stage_file), Some(expected)) => {
            Ok(file_object_identity(&stage_file).ok().as_deref() == Some(expected))
        }
        (None, None, None) => Ok(true),
        _ => Ok(false),
    }
}

/// Copy to a receipt-owned file under the reserved staging directory, then
/// publish with a same-volume hard link. Creating the final name this way is
/// atomic and fails if another file already owns the destination.
fn copy_and_publish_staged<F, G>(
    db: &Database,
    library: &LibrarySnapshot,
    record: &legacy_db::ResolutionRecord,
    target: &Path,
    digest: &str,
    after_copy_before_publish: &mut Option<F>,
    after_publish_before_verify: &mut Option<G>,
) -> AppResult<(String, String)>
where
    F: FnOnce() -> AppResult<()>,
    G: FnOnce() -> AppResult<()>,
{
    let root = managed_library::verified_root_path(library)?;
    let parent = target
        .parent()
        .ok_or_else(|| AppError::conflict("回仓目标路径无效"))?
        .canonicalize()?;
    if parent != root {
        return Err(AppError::conflict("回仓目标已脱离当前总仓"));
    }
    managed_library::validate_import_target(library, target)?;
    let stage_path = legacy_stage_path(library, &record.resolution_id)?;
    let source = verify_source_snapshot(record)?;
    let expected_object = record.created_file_identity.as_deref();

    let target_file = open_owned_target_file(target, &root, expected_object)?;
    let stage_file = if target_file.is_some() {
        open_owned_stage_file_for_inspection(&stage_path, &root, expected_object)?
    } else {
        open_owned_stage_file(&stage_path, &root, expected_object)?
    };
    let (mut stage_file, object_identity) = match (target_file, stage_file) {
        (Some(target_file), stage_file) => {
            let expected = expected_object.ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
            if file_object_identity(&target_file)? != expected {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            if stage_file
                .as_ref()
                .is_some_and(|file| file_object_identity(file).ok().as_deref() != Some(expected))
            {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            // A final path with our persisted object identity can only result
            // from a completed hard-link publication. Never truncate it on
            // recovery; verify it and finish the journal transition instead.
            let (published_digest, published_size, _) = digest_open_file(&target_file)?;
            if published_digest != digest || published_size != record.source_file_size {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            verify_source_snapshot(record)?;
            if stage_file.is_some() {
                let stage_at_path =
                    open_owned_stage_file_for_inspection(&stage_path, &root, Some(expected))?
                        .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
                if file_object_identity(&stage_at_path)? != expected {
                    return Err(AppError::conflict("OUTCOME_UNKNOWN"));
                }
                drop(stage_at_path);
                drop(stage_file);
                fs::remove_file(&stage_path)?;
            }
            let evidence = managed_library::prepare_import_evidence(library, target, digest)?;
            let published_identity = file_identity(&target_file)?;
            if evidence.file_identity != published_identity {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            let conn = db.lock()?;
            if !legacy_db::mark_published(
                &conn,
                &record.resolution_id,
                expected,
                &published_identity,
                now_unix_seconds()?,
            )? {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            return Ok((expected.to_string(), published_identity));
        }
        (None, Some(file)) => {
            if expected_object.is_none() {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            let object_identity = file_object_identity(&file)?;
            if let Some(expected) = expected_object {
                if object_identity != expected {
                    return Err(AppError::conflict("OUTCOME_UNKNOWN"));
                }
            }
            (file, object_identity)
        }
        (None, None) => {
            if expected_object.is_some() {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            let file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&stage_path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(AppError::conflict("OUTCOME_UNKNOWN"));
                }
                Err(error) => return Err(error.into()),
            };
            let object_identity = file_object_identity(&file)?;
            let conn = db.lock()?;
            if !legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &object_identity,
                now_unix_seconds()?,
            )? {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
            (file, object_identity)
        }
    };

    stage_file.set_len(0)?;
    stage_file.seek(SeekFrom::Start(0))?;
    copy_and_verify(&source, &mut stage_file, record)?;
    stage_file.sync_all()?;
    if let Some(hook) = after_copy_before_publish.take() {
        hook()?;
    }
    let (staged_digest, staged_size, _) = digest_open_file(&stage_file)?;
    if staged_digest != digest || staged_size != record.source_file_size {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    // The open handle still refers to our receipt-owned object if the stage
    // name was replaced while copying. Re-open the name and bind publication
    // to the persisted object identity before using the path-based hard link.
    let stage_at_path = open_owned_stage_file(&stage_path, &root, Some(&object_identity))?
        .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
    if file_object_identity(&stage_at_path)? != object_identity {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    // Close the remaining source race after target verification and directly
    // before publication. The operation is not allowed to publish a stale
    // snapshot even when the source bytes stayed the same and only mtime moved.
    verify_source_snapshot(record)?;
    drop(stage_at_path);
    drop(stage_file);
    match fs::hard_link(&stage_path, target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let target_file = open_owned_target_file(target, &root, Some(&object_identity))?
                .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
            if file_object_identity(&target_file)? != object_identity {
                return Err(AppError::conflict("OUTCOME_UNKNOWN"));
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::CrossesDevices
            ) =>
        {
            return Err(AppError::unsupported(
                "当前总仓文件系统不支持安全回仓发布。收据、暂存文件和原文件均已保留；目前没有可用的安全迁仓重试流程，请勿移动或删除这些文件，待受控迁仓流程可用后再处理。",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    if let Some(hook) = after_publish_before_verify.take() {
        hook()?;
    }
    let target_file = open_owned_target_file(target, &root, None)?
        .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
    let target_object_identity = file_object_identity(&target_file)?;
    if target_object_identity != object_identity {
        drop(target_file);
        // A path observed after publication may have been replaced. Its newly
        // observed identity is not proof that the file belongs to this receipt;
        // preserve it and let recovery classify the outcome as unknown.
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let stage_at_path =
        open_owned_stage_file_for_inspection(&stage_path, &root, Some(&object_identity))?
            .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
    if file_object_identity(&stage_at_path)? != object_identity {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    drop(stage_at_path);
    fs::remove_file(&stage_path)?;
    managed_library::validate_import_target(library, target)?;
    let evidence = managed_library::prepare_import_evidence(library, target, digest)?;
    let target_file = open_owned_target_file(target, &root, Some(&object_identity))?
        .ok_or_else(|| AppError::conflict("OUTCOME_UNKNOWN"))?;
    let published_identity = file_identity(&target_file)?;
    if evidence.file_identity != published_identity {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let conn = db.lock()?;
    if !legacy_db::mark_published(
        &conn,
        &record.resolution_id,
        &object_identity,
        &published_identity,
        now_unix_seconds()?,
    )? {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok((object_identity, published_identity))
}

fn legacy_stage_path(library: &LibrarySnapshot, resolution_id: &str) -> AppResult<PathBuf> {
    if resolution_id.is_empty()
        || !resolution_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AppError::internal("旧记录处置 ID 格式无效"));
    }
    let root = managed_library::verified_root_path(library)?;
    let directory = root.join(".legacy-stage");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if !is_link_or_reparse(&metadata) && metadata.is_dir() => {}
        Ok(_) => return Err(AppError::conflict("回仓暂存目录不是安全目录")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(AppError::conflict("回仓暂存目录不是安全目录"));
    }
    let canonical_directory = directory.canonicalize()?;
    if canonical_directory.parent() != Some(root.as_path()) {
        return Err(AppError::conflict("回仓暂存目录已脱离当前总仓"));
    }
    Ok(canonical_directory.join(format!("{resolution_id}.part")))
}

fn open_owned_stage_file(
    path: &Path,
    root: &Path,
    expected_object_identity: Option<&str>,
) -> AppResult<Option<File>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root)
        || expected_object_identity
            .is_some_and(|expected| file_object_identity(&file).ok().as_deref() != Some(expected))
    {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(Some(file))
}

#[cfg(windows)]
fn open_owned_stage_file_for_inspection(
    path: &Path,
    root: &Path,
    expected_object_identity: Option<&str>,
) -> AppResult<Option<File>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path)?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root)
        || expected_object_identity
            .is_some_and(|expected| file_object_identity(&file).ok().as_deref() != Some(expected))
    {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(Some(file))
}

#[cfg(not(windows))]
fn open_owned_stage_file_for_inspection(
    path: &Path,
    root: &Path,
    expected_object_identity: Option<&str>,
) -> AppResult<Option<File>> {
    open_owned_stage_file(path, root, expected_object_identity)
}

fn open_owned_target_file(
    path: &Path,
    root: &Path,
    expected_object_identity: Option<&str>,
) -> AppResult<Option<File>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let file = open_target_read_only_for_inspection(path)?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root)
        || expected_object_identity
            .is_some_and(|expected| file_object_identity(&file).ok().as_deref() != Some(expected))
    {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(Some(file))
}

fn commit_published_copy(
    db: &Database,
    caller: &CallerContext,
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
    target: &Path,
) -> AppResult<()> {
    commit_published_copy_with_hook(db, caller, record, library, target, || Ok(()))
}

fn commit_published_copy_with_hook<F>(
    db: &Database,
    caller: &CallerContext,
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
    target: &Path,
    before_asset_registration: F,
) -> AppResult<()>
where
    F: FnOnce() -> AppResult<()>,
{
    if let Err(error) = verify_source_snapshot(record) {
        mark_unknown(db, record, "SOURCE_CHANGED")?;
        return Err(error);
    }
    let Some(expected_identity) = record.published_file_identity.as_deref() else {
        mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    };
    let opened = match open_target(target, Some(expected_identity)) {
        Ok(opened) => opened,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    let (digest, size, _) = match digest_open_file(&opened) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    let opened_identity = match file_identity(&opened) {
        Ok(identity) => identity,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    if Some(digest.as_str()) != record.source_sha256.as_deref()
        || size != record.source_file_size
        || opened_identity != expected_identity
    {
        mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    drop(opened);
    let target_path = normalize_path(&encode_native_path(target)?);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::unsupported("回仓文件名不是有效 UTF-8"))?;
    let compact_hash = digest[..32].to_string();
    before_asset_registration()?;
    if let Err(error) = verify_source_snapshot(record) {
        mark_unknown(db, record, "SOURCE_CHANGED")?;
        return Err(error);
    }
    // Reopen the published path after all callbacks and source checks. On
    // Windows this handle denies write/delete sharing, pinning the verified
    // target name through the database commit.
    let evidence = match managed_library::prepare_import_evidence(library, target, &digest) {
        Ok(evidence) => evidence,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    let pinned_target = match open_target_for_registration(target, Some(expected_identity)) {
        Ok(file) => file,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    let pinned_identity = match file_identity(&pinned_target) {
        Ok(identity) => identity,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    let (pinned_digest, pinned_size, pinned_modified_at) = match digest_open_file(&pinned_target) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
            return Err(error);
        }
    };
    if pinned_identity != expected_identity || pinned_digest != digest || pinned_size != size {
        mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    if evidence.file_identity != pinned_identity || evidence.content_sha256 != digest {
        mark_unknown(db, record, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    let mut conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    ensure_same_library(&conn, library)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = legacy_db::get_resolution(&tx, &record.resolution_id)?
        .ok_or_else(|| AppError::not_found("旧记录处置收据已失效"))?;
    if current.state != "published"
        || current.published_file_identity.as_deref() != Some(expected_identity)
    {
        return Err(AppError::conflict("回仓收据状态已变化"));
    }
    if !legacy_db::update_asset_after_copy(
        &tx,
        &current,
        &target_path,
        file_name,
        size,
        pinned_modified_at,
        &compact_hash,
    )? {
        legacy_db::mark_unknown(
            &tx,
            &current.resolution_id,
            "SOURCE_CHANGED",
            now_unix_seconds()?,
        )?;
        tx.commit()?;
        return Err(AppError::conflict("旧资产记录已变化，回仓收据保留待核实"));
    }
    let candidate = match libraries::verification_candidate(&tx, current.asset_id)? {
        Some(candidate) => candidate,
        None => {
            drop(tx);
            drop(conn);
            mark_unknown(db, &current, "OUTCOME_UNKNOWN")?;
            return Err(AppError::internal("回仓资产登记后消失"));
        }
    };
    if !libraries::record_verified(
        &tx,
        library,
        &candidate,
        &evidence.relative_path,
        &evidence.file_identity,
        &evidence.content_sha256,
        &evidence.ownership_receipt,
    )? {
        drop(tx);
        drop(conn);
        mark_unknown(db, &current, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("回仓资产登记条件已变化，收据保留待核实"));
    }
    if !legacy_db::finish_copy(&tx, &current.resolution_id, now_unix_seconds()?)? {
        drop(tx);
        drop(conn);
        mark_unknown(db, &current, "OUTCOME_UNKNOWN")?;
        return Err(AppError::conflict("回仓处置收据状态已变化"));
    }
    tx.commit()?;
    Ok(())
}

fn inspect_selected_source(
    recorded_path: &str,
    selected_source: &Path,
    root: &Path,
) -> AppResult<SourceFileSnapshot> {
    if !selected_source.is_absolute() {
        return Err(AppError::invalid_arg("请选择记录对应的本机原文件"));
    }
    let selected_metadata = fs::symlink_metadata(selected_source)?;
    if is_link_or_reparse(&selected_metadata) || !selected_metadata.is_file() {
        return Err(AppError::conflict("原文件不能是链接，或当前不可访问"));
    }
    let selected_canonical = selected_source.canonicalize()?;
    let recorded_canonical = Path::new(recorded_path)
        .canonicalize()
        .map_err(|_| AppError::conflict("原文件不可访问，请重新核对记录"))?;
    if selected_canonical != recorded_canonical || recorded_canonical.starts_with(root) {
        return Err(AppError::conflict("所选文件不是该库外记录对应的原文件"));
    }
    let source = File::open(&selected_canonical)?;
    let identity = file_identity(&source)?;
    let (digest, size, modified_at) = digest_open_file(&source)?;
    let modified_at_ns = modified_at_ns(&source.metadata()?)?;
    verify_open_path(
        &selected_canonical,
        &source,
        &identity,
        size,
        modified_at_ns,
    )?;
    Ok(SourceFileSnapshot {
        file_identity: identity,
        file_size: size,
        file_modified_at: modified_at,
        file_modified_at_ns: modified_at_ns,
        sha256: digest,
    })
}

fn inspect_unlink_source(
    asset: &legacy_db::PendingLegacyAsset,
    library: &LibrarySnapshot,
    expected_identity: Option<&str>,
) -> AppResult<(File, String, i64, i64, i64)> {
    let root = managed_library::verified_root_path(library)?;
    let recorded_path = Path::new(&asset.file_path);
    let metadata = fs::symlink_metadata(recorded_path)
        .map_err(|_| AppError::conflict("原文件不可访问，不能解除关联"))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict(
            "原文件不是可核验的普通文件，不能解除关联",
        ));
    }
    let canonical = recorded_path
        .canonicalize()
        .map_err(|_| AppError::conflict("原文件不可访问，不能解除关联"))?;
    if canonical.starts_with(&root) {
        return Err(AppError::conflict("原文件已位于当前总仓内，请刷新记录"));
    }
    let file = File::open(&canonical)?;
    let identity = file_identity(&file)?;
    let opened_metadata = file.metadata()?;
    let size = opened_metadata.len() as i64;
    let modified_at = modified_at_ms(&opened_metadata)?;
    let modified_at_ns = modified_at_ns(&opened_metadata)?;
    if size != asset.file_size
        || modified_at != asset.modified_at
        || expected_identity.is_some_and(|expected| expected != identity)
    {
        return Err(AppError::conflict("原文件已变化，不能解除关联"));
    }
    verify_open_path(&canonical, &file, &identity, size, modified_at_ns)?;
    Ok((file, identity, size, modified_at, modified_at_ns))
}

fn verify_source_snapshot(record: &legacy_db::ResolutionRecord) -> AppResult<File> {
    let path = Path::new(&record.source_path);
    let path_meta =
        fs::symlink_metadata(path).map_err(|_| AppError::conflict("原文件已变化，请重新准备"))?;
    if is_link_or_reparse(&path_meta) || !path_meta.is_file() {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::conflict("原文件已变化，请重新准备"))?;
    let mut file =
        File::open(&canonical).map_err(|_| AppError::conflict("原文件已变化，请重新准备"))?;
    let expected_identity = record
        .source_file_identity
        .as_deref()
        .ok_or_else(|| AppError::internal("处置收据缺少源文件身份"))?;
    let (digest, size, _) = digest_open_file(&file)?;
    let modified_at_ns = modified_at_ns(&file.metadata()?)?;
    verify_open_path(&canonical, &file, expected_identity, size, modified_at_ns)?;
    if digest != record.source_sha256.as_deref().unwrap_or_default()
        || size != record.source_file_size
        || modified_at_ns != record.source_file_modified_at_ns
    {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

fn copy_and_verify(
    source: &File,
    destination: &mut File,
    record: &legacy_db::ResolutionRecord,
) -> AppResult<()> {
    let identity = record
        .source_file_identity
        .as_deref()
        .ok_or_else(|| AppError::internal("处置收据缺少源文件身份"))?;
    let source_path = Path::new(&record.source_path);
    let canonical = source_path
        .canonicalize()
        .map_err(|_| AppError::conflict("原文件已变化，请重新准备"))?;
    let mut reader = source.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    destination.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0_i64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
        total = total.saturating_add(read as i64);
    }
    let digest = hex_digest(hasher.finalize());
    if digest != record.source_sha256.as_deref().unwrap_or_default()
        || total != record.source_file_size
        || file_identity(source)? != identity
    {
        return Err(AppError::conflict("原文件复制时发生变化"));
    }
    // The prepared snapshot includes mtime as well as identity, size, and
    // content. Recheck the open source and its path after copying, before the
    // staged target can be published or registered.
    verify_open_path(
        &canonical,
        source,
        identity,
        record.source_file_size,
        record.source_file_modified_at_ns,
    )?;
    destination.flush()?;
    Ok(())
}

fn digest_open_file(file: &File) -> AppResult<(String, i64, i64)> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(AppError::conflict("所选对象不是普通文件"));
    }
    let identity = file_identity(file)?;
    let modified_at_ns_before = modified_at_ns(&before)?;
    let mut hasher = Sha256::new();
    let mut size = 0_i64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as i64);
    }
    let after = file.metadata()?;
    let modified_at_ns_after = modified_at_ns(&after)?;
    if size != after.len() as i64
        || file_identity(file)? != identity
        || modified_at_ns_before != modified_at_ns_after
    {
        return Err(AppError::conflict("文件读取期间发生变化"));
    }
    Ok((hex_digest(hasher.finalize()), size, modified_at_ms(&after)?))
}

fn verify_open_path(
    path: &Path,
    file: &File,
    expected_identity: &str,
    expected_size: i64,
    expected_modified_at_ns: i64,
) -> AppResult<()> {
    if file_identity(file)? != expected_identity {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    let metadata = file.metadata()?;
    if metadata.len() as i64 != expected_size
        || modified_at_ns(&metadata)? != expected_modified_at_ns
    {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    let path_meta = fs::symlink_metadata(path)?;
    if is_link_or_reparse(&path_meta) || !path_meta.is_file() {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    let reopened = File::open(path)?;
    if file_identity(&reopened)? != expected_identity || path.canonicalize()? != path {
        return Err(AppError::conflict("原文件已变化，请重新准备"));
    }
    Ok(())
}

fn open_target(path: &Path, expected_identity: Option<&str>) -> AppResult<File> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict("回仓目标无法核实"));
    }
    let file = File::open(path)?;
    if expected_identity
        .is_some_and(|expected| file_identity(&file).ok().as_deref() != Some(expected))
    {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_target_read_only_for_inspection(path: &Path) -> AppResult<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

    Ok(OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path)?)
}

#[cfg(not(windows))]
fn open_target_read_only_for_inspection(path: &Path) -> AppResult<File> {
    Ok(File::open(path)?)
}

#[cfg(windows)]
fn open_target_for_registration(path: &Path, expected_identity: Option<&str>) -> AppResult<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let metadata = fs::symlink_metadata(path)?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppError::conflict("回仓目标无法核实"));
    }
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)?;
    if expected_identity
        .is_some_and(|expected| file_identity(&file).ok().as_deref() != Some(expected))
    {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_target_for_registration(path: &Path, expected_identity: Option<&str>) -> AppResult<File> {
    open_target(path, expected_identity)
}

fn choose_target(db: &Database, library: &LibrarySnapshot, source: &Path) -> AppResult<PathBuf> {
    let root = managed_library::verified_root_path(library)?;
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::unsupported("原文件名不是有效 UTF-8"))?;
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::unsupported("原文件名不是有效 UTF-8"))?;
    for index in 0..10_000_u32 {
        let name = if index == 0 {
            file_name.to_string()
        } else if extension.is_empty() {
            format!("{stem}({index})")
        } else {
            format!("{stem}({index}).{extension}")
        };
        let target = root.join(name);
        managed_library::validate_import_target(library, &target)?;
        if target.exists() {
            continue;
        }
        let native_path = encode_native_path(&target)?;
        let normalized = normalize_path(&native_path);
        let conn = db.lock()?;
        if db::assets::find_by_path(&conn, &normalized)?.is_none()
            && !legacy_db::active_target_exists(&conn, &native_path)?
        {
            return Ok(target);
        }
    }
    Err(AppError::conflict(
        "总仓中同名文件过多，无法安全选择回仓路径",
    ))
}

fn ready_library(conn: &rusqlite::Connection) -> AppResult<LibrarySnapshot> {
    let library = libraries::current(conn)?
        .ok_or_else(|| AppError::conflict("总仓尚未配置，不能处理旧记录"))?;
    if library.health != LibraryHealth::Ready {
        return Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总仓离线或尚未核验，旧记录处置已暂停",
        ));
    }
    Ok(library)
}

fn ensure_same_library(conn: &rusqlite::Connection, expected: &LibrarySnapshot) -> AppResult<()> {
    let current = ready_library(conn)?;
    if current.library_id != expected.library_id || current.library_epoch != expected.library_epoch
    {
        return Err(AppError::conflict("总仓身份已变化，请重新准备处置"));
    }
    Ok(())
}

fn ensure_record_library(
    record: &legacy_db::ResolutionRecord,
    library: &LibrarySnapshot,
) -> AppResult<()> {
    if record.library_id != library.library_id || record.library_epoch != library.library_epoch {
        return Err(AppError::conflict("总仓身份已变化，请重新准备处置"));
    }
    Ok(())
}

fn require_gui(caller: &CallerContext) -> AppResult<()> {
    if caller.kind != CallerKind::Gui {
        return Err(AppError::unauthorized("该维护功能仅供茶馆界面使用"));
    }
    Ok(())
}

fn fail_prepared(db: &Database, record: &legacy_db::ResolutionRecord, code: &str) -> AppResult<()> {
    let conn = db.lock()?;
    legacy_db::mark_failed(&conn, &record.resolution_id, code, now_unix_seconds()?)
}

fn classify_asset_unknown_if_root_ready(
    db: &Database,
    caller: &CallerContext,
    library: &LibrarySnapshot,
    asset_id: i64,
) -> AppResult<()> {
    if managed_library::verified_root_path(library).is_err() {
        return Ok(());
    }
    let conn = db.lock()?;
    authorization::validate_caller(&conn, caller)?;
    ensure_same_library(&conn, library)?;
    if let Some(candidate) = libraries::verification_candidate(&conn, asset_id)? {
        libraries::classify_pending(&conn, library, &candidate, PendingClassification::Unknown)?;
    }
    Ok(())
}

fn mark_unknown(db: &Database, record: &legacy_db::ResolutionRecord, code: &str) -> AppResult<()> {
    let conn = db.lock()?;
    legacy_db::mark_unknown(&conn, &record.resolution_id, code, now_unix_seconds()?)
}

fn mark_copy_unknown(
    db: &Database,
    record: &legacy_db::ResolutionRecord,
    code: &str,
) -> AppResult<()> {
    let conn = db.lock()?;
    legacy_db::mark_copy_unknown(&conn, &record.resolution_id, code, now_unix_seconds()?)
}

fn decode_matching_assets(
    record: &legacy_db::ResolutionRecord,
) -> AppResult<Vec<legacy_db::HashMatchCandidate>> {
    serde_json::from_str(&record.matching_assets_json)
        .map_err(|error| AppError::internal(format!("冲突预览读取失败: {error}")))
}

fn result_from_record(record: &legacy_db::ResolutionRecord) -> AppResult<ResolutionResult> {
    let matching_assets = decode_matching_assets(record)?;
    Ok(ResolutionResult {
        resolution_id: ResourceRef::from_trusted(record.resolution_id.clone())?,
        asset_id: record.asset_id,
        action: record.action.clone(),
        state: record.state.clone(),
        target_path: record.target_path.clone(),
        matching_assets,
        error_code: record.error_code.clone(),
        expires_at: record.expires_at,
    })
}

fn modified_at_ms(metadata: &fs::Metadata) -> AppResult<i64> {
    Ok(metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::conflict("文件修改时间早于 Unix epoch"))?
        .as_millis() as i64)
}

fn modified_at_ns(metadata: &fs::Metadata) -> AppResult<i64> {
    let nanos = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::conflict("文件修改时间早于 Unix epoch"))?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| AppError::unsupported("文件修改时间超出可核验范围"))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_unix_seconds() -> AppResult<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间异常"))?
        .as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use crate::services::authorization::TrustedAdapterIdentity;

    fn ready_db(root: &Path) -> Database {
        let conn = init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        managed_library::initialize_runtime(&conn).unwrap();
        Database::new(conn)
    }

    fn caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_legacy_assets_test".to_string(),
                instance_id: "instance_legacy_assets_test".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn insert_external(db: &Database, path: &Path, hash: Option<&str>) -> i64 {
        let metadata = fs::metadata(path).unwrap();
        let file_size = metadata.len() as i64;
        let modified_at = modified_at_ms(&metadata).unwrap();
        let path = encode_native_path(path).unwrap();
        let conn = db.lock().unwrap();
        let id = db::assets::insert(
            &conn,
            &path,
            "原图.jpg",
            ".jpg",
            file_size,
            "image/jpeg",
            modified_at,
        )
        .unwrap();
        conn.execute(
            "UPDATE managed_asset_locations SET status='external_pending',reason='outside_managed_root' WHERE asset_id=?1",
            [id],
        )
        .unwrap();
        if let Some(hash) = hash {
            db::assets::set_hash(&conn, id, hash).unwrap();
        }
        id
    }

    fn mark_test_copy_published(
        db: &Database,
        record: &legacy_db::ResolutionRecord,
        library: &LibrarySnapshot,
    ) -> legacy_db::ResolutionRecord {
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        let stage = legacy_stage_path(library, &record.resolution_id).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                record.target_path.as_deref().unwrap(),
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let mut staged = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&stage)
            .unwrap();
        let mut source = File::open(&record.source_path).unwrap();
        std::io::copy(&mut source, &mut staged).unwrap();
        staged.sync_all().unwrap();
        let object_identity = file_object_identity(&staged).unwrap();
        drop(staged);
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &object_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        fs::hard_link(&stage, &target).unwrap();
        fs::remove_file(&stage).unwrap();
        let published_identity = file_identity(&File::open(&target).unwrap()).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_published(
                &conn,
                &record.resolution_id,
                &object_identity,
                &published_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
            legacy_db::get_resolution(&conn, &record.resolution_id)
                .unwrap()
                .unwrap()
        }
    }

    #[test]
    fn unlink_is_only_a_database_disposition_and_can_be_reopened() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("旧图.jpg");
        fs::write(&source, b"old bytes").unwrap();
        let original_mtime = fs::metadata(&source).unwrap().modified().unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE assets SET favorite=1,rating=5 WHERE id=?1",
                [asset_id],
            )
            .unwrap();
            let tag = db::tags::create(&conn, "解除后保留标签", None).unwrap();
            conn.execute(
                "INSERT INTO asset_tags(asset_id,tag_id,source,created_at) VALUES (?1,?2,'manual',1)",
                rusqlite::params![asset_id, tag.id],
            )
            .unwrap();
        }
        let caller = caller(&db);

        let preview = prepare_unlink(&db, &caller, asset_id).unwrap();
        assert_eq!(preview.action, "unlink");
        assert!(list_pending(&db, &caller, 0, 100).unwrap().items.len() == 1);
        confirm(&db, &caller, &preview.resolution_id).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"old bytes");
        assert_eq!(
            fs::metadata(&source).unwrap().modified().unwrap(),
            original_mtime
        );
        assert!(list_pending(&db, &caller, 0, 100).unwrap().items.is_empty());
        let conn = db.lock().unwrap();
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        assert_eq!(asset.favorite, 1);
        assert_eq!(asset.rating, 5);
        assert!(asset.tags.iter().any(|tag| tag.name == "解除后保留标签"));
        drop(conn);
        assert_eq!(list_unlinked(&db, &caller, 0, 100).unwrap().len(), 1);
        reopen_unlinked(&db, &caller, asset_id).unwrap();
        assert_eq!(list_pending(&db, &caller, 0, 100).unwrap().items.len(), 1);
    }

    #[test]
    fn unlink_without_persisted_source_identity_fails_without_changing_asset() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("missing-unlink-identity.jpg");
        fs::write(&source, b"source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_unlink(&db, &caller, asset_id).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE legacy_asset_resolutions SET source_file_identity=NULL WHERE resolution_id=?1",
                [plan.resolution_id.as_str()],
            )
            .unwrap();
        }

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"source bytes");
        let conn = db.lock().unwrap();
        let receipt = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.state, "failed");
        assert_eq!(
            receipt.error_code.as_deref(),
            Some("SOURCE_IDENTITY_MISSING")
        );
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().file_path,
            encode_native_path(&source).unwrap()
        );
    }

    #[test]
    fn incomplete_prepared_copy_receipts_are_terminalized() {
        for missing_field in ["target_path", "source_sha256"] {
            let root = tempfile::tempdir().unwrap();
            let source_dir = tempfile::tempdir().unwrap();
            let source = source_dir
                .path()
                .join(format!("incomplete-copy-{missing_field}.jpg"));
            fs::write(&source, b"source bytes").unwrap();
            let db = ready_db(root.path());
            let asset_id = insert_external(&db, &source, None);
            let caller = caller(&db);
            let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
            {
                let conn = db.lock().unwrap();
                conn.execute(
                    &format!(
                        "UPDATE legacy_asset_resolutions SET {missing_field}=NULL WHERE resolution_id=?1"
                    ),
                    [plan.resolution_id.as_str()],
                )
                .unwrap();
            }

            assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
            let conn = db.lock().unwrap();
            let receipt = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap();
            assert_eq!(receipt.state, "failed", "{missing_field}");
            assert_eq!(
                receipt.error_code.as_deref(),
                Some("RECEIPT_INCOMPLETE"),
                "{missing_field}"
            );
            assert_eq!(
                db::assets::get(&conn, asset_id).unwrap().file_path,
                encode_native_path(&source).unwrap(),
                "{missing_field}"
            );
        }
    }

    #[test]
    fn copy_back_preserves_asset_identity_metadata_and_external_source() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("原图.jpg");
        fs::write(&source, b"legacy image bytes").unwrap();
        let original_mtime = fs::metadata(&source).unwrap().modified().unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE assets SET favorite=1,rating=4 WHERE id=?1",
                [asset_id],
            )
            .unwrap();
            let tag = db::tags::create(&conn, "历史标签保留", None).unwrap();
            conn.execute(
                "INSERT INTO asset_tags(asset_id,tag_id,source,created_at) VALUES (?1,?2,'manual',1)",
                rusqlite::params![asset_id, tag.id],
            )
            .unwrap();
        }
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();

        let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
        assert_eq!(result.state, "completed");
        assert_eq!(result.asset_id, asset_id);
        let target = PathBuf::from(result.target_path.unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"legacy image bytes");
        assert_eq!(fs::read(&source).unwrap(), b"legacy image bytes");
        assert_eq!(
            fs::metadata(&source).unwrap().modified().unwrap(),
            original_mtime
        );
        let conn = db.lock().unwrap();
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.id, asset_id);
        assert_eq!(
            asset.file_path,
            normalize_path(&encode_native_path(&target).unwrap())
        );
        assert_eq!(asset.favorite, 1);
        assert_eq!(asset.rating, 4);
        assert!(asset.tags.iter().any(|tag| tag.name == "历史标签保留"));
        assert_eq!(
            db::assets::get_managed(&conn, asset_id).unwrap().id,
            asset_id
        );
        drop(conn);
        assert!(list_pending(&db, &caller, 0, 100).unwrap().items.is_empty());
    }

    #[test]
    fn copy_back_rechecks_source_timestamp_after_copy_before_publish() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("changed-during-copy.jpg");
        fs::write(&source, b"unchanged source bytes").unwrap();
        let original_modified = fs::metadata(&source).unwrap().modified().unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };

        let error = copy_back_with_hook(&db, &caller, &record, &library, || {
            let source_file = OpenOptions::new().write(true).open(&source)?;
            source_file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(original_modified + std::time::Duration::from_secs(5)),
            )?;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.code(), "CONFLICT");

        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert!(resolution.published_file_identity.is_none());
        let target = PathBuf::from(resolution.target_path.as_deref().unwrap());
        assert!(
            !target.exists(),
            "unverified copy must not appear at final path"
        );
        let stage = legacy_stage_path(&library, &resolution.resolution_id).unwrap();
        assert!(stage.exists(), "interrupted bytes stay in reserved staging");
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        let target_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assets WHERE file_path=?1",
                [resolution.target_path.as_deref().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target_rows, 0);
    }

    #[test]
    fn replaced_stage_path_after_copy_is_not_published_or_removed() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("stage-path-replaced.jpg");
        fs::write(&source, b"intended source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();

        let error = copy_back_with_hook(&db, &caller, &record, &library, || {
            fs::remove_file(&stage)?;
            fs::write(&stage, b"foreign replacement bytes")?;
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.code(), "CONFLICT");
        assert!(
            !target.exists(),
            "foreign stage bytes must never be published"
        );
        assert_eq!(
            fs::read(&stage).unwrap(),
            b"foreign replacement bytes",
            "the replacement file is not owned by the receipt and must be preserved"
        );
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert!(resolution.published_file_identity.is_none());
        let asset = db::assets::get(&conn, asset_id).unwrap();
        let location_status: String = conn
            .query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(location_status, "external_pending");
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
    }

    #[test]
    fn target_replaced_after_publication_is_preserved_and_receipt_becomes_unknown() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("target-replaced-after-publish.jpg");
        fs::write(&source, b"intended source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());

        let error = copy_back_with_hooks(
            &db,
            &caller,
            &record,
            &library,
            || Ok(()),
            || {
                fs::remove_file(&target)?;
                fs::write(&target, b"foreign replacement")?;
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(fs::read(&target).unwrap(), b"foreign replacement");
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        let location_status: String = conn
            .query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(location_status, "external_pending");
    }

    #[test]
    fn published_copy_rechecks_source_snapshot_before_asset_registration() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("source-changed-after-publish.jpg");
        fs::write(&source, b"published source bytes").unwrap();
        let original_modified = fs::metadata(&source).unwrap().modified().unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                record.target_path.as_deref().unwrap(),
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let mut staged = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&stage)
            .unwrap();
        staged.write_all(b"published source bytes").unwrap();
        staged.sync_all().unwrap();
        let object_identity = file_object_identity(&staged).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &object_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        drop(staged);
        fs::hard_link(&stage, &target).unwrap();
        fs::remove_file(&stage).unwrap();
        let target_file = File::open(&target).unwrap();
        let published_identity = file_identity(&target_file).unwrap();
        drop(target_file);
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_published(
                &conn,
                &record.resolution_id,
                &object_identity,
                &published_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let published_record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, &record.resolution_id)
                .unwrap()
                .unwrap()
        };
        let error = commit_published_copy_with_hook(
            &db,
            &caller,
            &published_record,
            &library,
            &target,
            || {
                let source_file = OpenOptions::new().write(true).open(&source)?;
                source_file.set_times(
                    std::fs::FileTimes::new()
                        .set_modified(original_modified + std::time::Duration::from_secs(5)),
                )?;
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(fs::read(&target).unwrap(), b"published source bytes");
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        let asset = db::assets::get(&conn, asset_id).unwrap();
        let location_status: String = conn
            .query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(location_status, "external_pending");
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
    }

    #[test]
    fn published_copy_target_missing_or_replaced_before_confirm_becomes_unknown() {
        for replace_target in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let source_dir = tempfile::tempdir().unwrap();
            let source = source_dir.path().join("published-target-recovery.jpg");
            fs::write(&source, b"published source bytes").unwrap();
            let db = ready_db(root.path());
            let asset_id = insert_external(&db, &source, None);
            let caller = caller(&db);
            let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
            let (record, library) = {
                let conn = db.lock().unwrap();
                (
                    legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                        .unwrap()
                        .unwrap(),
                    libraries::current(&conn).unwrap().unwrap(),
                )
            };
            let published = mark_test_copy_published(&db, &record, &library);
            let target = PathBuf::from(published.target_path.as_deref().unwrap());
            if replace_target {
                fs::remove_file(&target).unwrap();
                fs::write(&target, b"foreign replacement bytes").unwrap();
            } else {
                fs::remove_file(&target).unwrap();
            }

            assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
            if replace_target {
                assert_eq!(fs::read(&target).unwrap(), b"foreign replacement bytes");
            } else {
                assert!(!target.exists());
            }
            let conn = db.lock().unwrap();
            let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
                .unwrap()
                .unwrap();
            assert_eq!(resolution.state, "outcome_unknown");
            assert_eq!(resolution.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
            let asset = db::assets::get(&conn, asset_id).unwrap();
            assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
            assert_eq!(
                conn.query_row(
                    "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                    [asset_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
                "external_pending"
            );
        }
    }

    #[test]
    fn published_copy_asset_snapshot_change_becomes_unknown_and_preserves_target() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("published-asset-snapshot.jpg");
        fs::write(&source, b"published source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let published = mark_test_copy_published(&db, &record, &library);
        let target = PathBuf::from(published.target_path.as_deref().unwrap());
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE assets SET file_size=file_size+1 WHERE id=?1",
                [asset_id],
            )
            .unwrap();
        }

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());

        assert_eq!(fs::read(&target).unwrap(), b"published source bytes");
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("SOURCE_CHANGED"));
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().file_size,
            b"published source bytes".len() as i64 + 1
        );
    }

    #[test]
    fn ready_library_epoch_change_quarantines_an_active_resolution() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("legacy-epoch-change.jpg");
        fs::write(&source, b"legacy source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE libraries SET library_epoch='new-ready-epoch' WHERE singleton=1",
                [],
            )
            .unwrap();
        }

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());

        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
        assert!(!PathBuf::from(resolution.target_path.unwrap()).exists());
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().file_path,
            encode_native_path(&source).unwrap()
        );
    }

    #[test]
    fn published_copy_asset_cas_failure_rolls_back_and_marks_unknown() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("published-asset-cas.jpg");
        fs::write(&source, b"published source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let published = mark_test_copy_published(&db, &record, &library);
        let target = PathBuf::from(published.target_path.as_deref().unwrap());

        let error =
            commit_published_copy_with_hook(&db, &caller, &published, &library, &target, || {
                let conn = db.lock()?;
                conn.execute(
                    "UPDATE assets SET file_size=file_size+1 WHERE id=?1",
                    [asset_id],
                )?;
                Ok(())
            })
            .unwrap_err();

        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(fs::read(&target).unwrap(), b"published source bytes");
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("SOURCE_CHANGED"));
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        assert_eq!(asset.file_size, b"published source bytes".len() as i64 + 1);
        assert_eq!(
            conn.query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [asset_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "external_pending"
        );
    }

    #[test]
    fn target_replaced_during_published_copy_registration_is_not_verified() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("published-target-race.jpg");
        fs::write(&source, b"intended published bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let published = mark_test_copy_published(&db, &record, &library);
        let target = PathBuf::from(published.target_path.as_deref().unwrap());

        let error =
            commit_published_copy_with_hook(&db, &caller, &published, &library, &target, || {
                fs::remove_file(&target)?;
                fs::write(&target, b"foreign replacement bytes")?;
                Ok(())
            })
            .unwrap_err();

        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(fs::read(&target).unwrap(), b"foreign replacement bytes");
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        assert_eq!(
            conn.query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [asset_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "external_pending"
        );
    }

    #[test]
    fn published_copy_registration_persists_the_pinned_target_timestamp() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("published-target-mtime.jpg");
        fs::write(&source, b"published source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let published = mark_test_copy_published(&db, &record, &library);
        let target = PathBuf::from(published.target_path.as_deref().unwrap());
        let target_modified = fs::metadata(&target).unwrap().modified().unwrap();
        let expected_modified = target_modified + std::time::Duration::from_secs(7);

        commit_published_copy_with_hook(&db, &caller, &published, &library, &target, || {
            OpenOptions::new()
                .write(true)
                .open(&target)?
                .set_times(std::fs::FileTimes::new().set_modified(expected_modified))?;
            Ok(())
        })
        .unwrap();

        let expected_db_modified = modified_at_ms(&fs::metadata(&target).unwrap()).unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().modified_at,
            expected_db_modified
        );
    }

    #[cfg(windows)]
    #[test]
    fn published_target_validation_opens_read_only_and_blocks_write_sharing() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("published-read-only.jpg");
        fs::write(&target, b"published bytes").unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let opened = open_owned_target_file(&target, &canonical_root, None)
            .unwrap()
            .unwrap();

        assert!(OpenOptions::new().write(true).open(&target).is_err());
        drop(opened);
        assert!(OpenOptions::new().write(true).open(&target).is_ok());
    }

    #[test]
    fn offline_root_never_turns_external_assets_into_unlink_candidates() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("outside.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        {
            let conn = db.lock().unwrap();
            let library = libraries::current(&conn).unwrap().unwrap();
            libraries::set_health(&conn, &library, LibraryHealth::Offline).unwrap();
        }
        assert!(prepare_unlink(&db, &caller, asset_id).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.library_health, "offline");
        assert!(!page.can_resolve);
    }

    #[test]
    fn unavailable_root_disables_actions_even_when_database_health_is_stale() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("outside.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::remove_dir_all(root.path()).unwrap();

        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.library_health, "offline");
        assert!(!page.can_resolve);
        assert_eq!(page.items.len(), 1);
        assert!(prepare_unlink(&db, &caller, asset_id).is_err());
    }

    #[test]
    fn mismatched_root_marker_is_reported_as_wrong_root() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("outside.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::write(
            root.path().join(managed_library::ROOT_MARKER),
            "different-library-id",
        )
        .unwrap();

        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.library_health, "wrong_root");
        assert!(!page.can_resolve);
    }

    #[test]
    fn missing_root_marker_is_reported_as_wrong_root() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("outside.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::remove_file(root.path().join(managed_library::ROOT_MARKER)).unwrap();

        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.library_health, "wrong_root");
        assert!(!page.can_resolve);
    }

    #[test]
    fn changed_external_file_is_not_accepted_as_the_historical_asset() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("changed-before-prepare.jpg");
        fs::write(&source, b"original snapshot").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::write(&source, b"replacement with a different size").unwrap();

        assert!(prepare_copy_back(&db, &caller, asset_id, &source).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, "unknown");
        let conn = db.lock().unwrap();
        let receipts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM legacy_asset_resolutions WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 0);
    }

    #[test]
    fn same_size_source_with_changed_timestamp_is_not_accepted() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("same-size-changed.jpg");
        fs::write(&source, b"original bytes").unwrap();
        let original_metadata = fs::metadata(&source).unwrap();
        let original_size = original_metadata.len() as i64;
        let original_modified_at = modified_at_ms(&original_metadata).unwrap();
        let original_modified = original_metadata.modified().unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_len(0).unwrap();
        (&file).write_all(b"replaced bytes").unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(original_modified + std::time::Duration::from_secs(5)),
        )
        .unwrap();
        let changed_metadata = file.metadata().unwrap();
        assert_eq!(changed_metadata.len() as i64, original_size);
        assert_ne!(
            modified_at_ms(&changed_metadata).unwrap(),
            original_modified_at
        );

        assert!(prepare_copy_back(&db, &caller, asset_id, &source).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items[0].status, "unknown");
    }

    #[test]
    fn same_content_submillisecond_mtime_change_after_copy_prepare_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("submillisecond-mtime.jpg");
        let bytes = b"same bytes and size";
        fs::write(&source, bytes).unwrap();
        let original_modified = UNIX_EPOCH
            + std::time::Duration::from_secs(1_700_000_000)
            + std::time::Duration::from_millis(125)
            + std::time::Duration::from_nanos(100);
        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let original_metadata = file.metadata().unwrap();
        let original_modified_ms = modified_at_ms(&original_metadata).unwrap();
        let original_modified_ns = modified_at_ns(&original_metadata).unwrap();
        drop(file);

        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };

        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(original_modified + std::time::Duration::from_nanos(100)),
        )
        .unwrap();
        let changed_metadata = file.metadata().unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert_eq!(changed_metadata.len() as i64, bytes.len() as i64);
        assert_eq!(
            modified_at_ms(&changed_metadata).unwrap(),
            original_modified_ms
        );
        assert_ne!(
            modified_at_ns(&changed_metadata).unwrap(),
            original_modified_ns
        );
        assert_ne!(
            modified_at_ns(&changed_metadata).unwrap(),
            record.source_file_modified_at_ns
        );

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        let conn = db.lock().unwrap();
        let updated = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(updated.state, "failed");
        assert_eq!(updated.error_code.as_deref(), Some("SOURCE_CHANGED"));
        assert!(!PathBuf::from(updated.target_path.unwrap()).exists());
    }

    #[test]
    fn unlink_rejects_submillisecond_mtime_change_after_prepare() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("unlink-submillisecond-mtime.jpg");
        let bytes = b"unchanged unlink bytes";
        fs::write(&source, bytes).unwrap();
        let original_modified = UNIX_EPOCH
            + std::time::Duration::from_secs(1_700_000_000)
            + std::time::Duration::from_millis(250)
            + std::time::Duration::from_nanos(100);
        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let original_metadata = file.metadata().unwrap();
        let original_modified_ms = modified_at_ms(&original_metadata).unwrap();
        let original_modified_ns = modified_at_ns(&original_metadata).unwrap();
        drop(file);

        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_unlink(&db, &caller, asset_id).unwrap();

        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(original_modified + std::time::Duration::from_nanos(100)),
        )
        .unwrap();
        let changed_metadata = file.metadata().unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert_eq!(changed_metadata.len() as i64, bytes.len() as i64);
        assert_eq!(
            modified_at_ms(&changed_metadata).unwrap(),
            original_modified_ms
        );
        assert_ne!(
            modified_at_ns(&changed_metadata).unwrap(),
            original_modified_ns
        );

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "failed");
        assert_eq!(resolution.error_code.as_deref(), Some("SOURCE_CHANGED"));
        let asset = db::assets::get(&conn, asset_id).unwrap();
        assert_eq!(asset.file_path, encode_native_path(&source).unwrap());
        assert_eq!(fs::read(&source).unwrap(), bytes);
    }

    #[test]
    fn changed_source_size_is_rejected_even_when_timestamp_is_restored() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("size-changed-same-time.jpg");
        fs::write(&source, b"old size").unwrap();
        let original_metadata = fs::metadata(&source).unwrap();
        let original_modified = original_metadata.modified().unwrap();
        let original_size = original_metadata.len() as i64;
        let original_modified_at = modified_at_ms(&original_metadata).unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        let file = OpenOptions::new().write(true).open(&source).unwrap();
        file.set_len(0).unwrap();
        (&file)
            .write_all(b"replacement has a different size")
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let metadata = file.metadata().unwrap();
        assert_ne!(metadata.len() as i64, original_size);
        assert_eq!(modified_at_ms(&metadata).unwrap(), original_modified_at);

        assert!(prepare_copy_back(&db, &caller, asset_id, &source).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items[0].status, "unknown");
    }

    #[test]
    fn missing_source_becomes_display_only_instead_of_being_unlinked() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("missing.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::remove_file(&source).unwrap();

        assert!(prepare_unlink(&db, &caller, asset_id).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, "unknown");
        let conn = db.lock().unwrap();
        let receipts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM legacy_asset_resolutions WHERE asset_id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 0);
    }

    #[test]
    fn missing_source_during_copy_preparation_becomes_display_only() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("missing-copy.jpg");
        fs::write(&source, b"fixture").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);

        fs::remove_file(&source).unwrap();

        assert!(prepare_copy_back(&db, &caller, asset_id, &source).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, "unknown");
    }

    #[test]
    fn unlink_rechecks_the_external_file_before_changing_its_projection() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("changed.jpg");
        fs::write(&source, b"original bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let preview = prepare_unlink(&db, &caller, asset_id).unwrap();

        fs::write(&source, b"changed source bytes").unwrap();

        assert!(confirm(&db, &caller, &preview.resolution_id).is_err());
        let page = list_pending(&db, &caller, 0, 100).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, "unknown");
        assert_eq!(fs::read(&source).unwrap(), b"changed source bytes");
    }

    #[test]
    fn interrupted_copy_resumes_only_the_owned_stage_object() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("resume.jpg");
        fs::write(&source, b"complete source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        let target_text = record.target_path.as_deref().unwrap();
        let target = PathBuf::from(target_text);
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                target_text,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let mut partial = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .unwrap();
        partial.write_all(b"partial copy").unwrap();
        partial.sync_all().unwrap();
        let object_identity = file_object_identity(&partial).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &object_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        drop(partial);

        let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
        assert_eq!(result.state, "completed");
        assert_eq!(fs::read(&target).unwrap(), b"complete source bytes");
        assert_eq!(fs::read(&source).unwrap(), b"complete source bytes");
    }

    #[test]
    fn transient_copy_error_keeps_an_identity_verified_stage_resumable() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("transient-error.jpg");
        fs::write(&source, b"complete source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let (record, library) = {
            let conn = db.lock().unwrap();
            (
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap(),
                libraries::current(&conn).unwrap().unwrap(),
            )
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();

        let error = copy_back_with_hook(&db, &caller, &record, &library, || {
            Err(std::io::Error::other("injected transient copy error").into())
        })
        .unwrap_err();
        assert_eq!(error.code(), "IO");
        let interrupted = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        assert_eq!(interrupted.state, "copying");
        assert!(interrupted.created_file_identity.is_some());
        assert!(!target.exists());
        assert_eq!(
            fs::metadata(&stage).unwrap().len(),
            fs::metadata(&source).unwrap().len()
        );

        let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
        assert_eq!(result.state, "completed");
        assert_eq!(fs::read(&target).unwrap(), b"complete source bytes");
    }

    #[test]
    fn published_target_without_receipt_is_reconciled_without_rewriting() {
        for leave_stage_link in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let source_dir = tempfile::tempdir().unwrap();
            let source = source_dir.path().join("published-before-receipt.jpg");
            fs::write(&source, b"published bytes").unwrap();
            let db = ready_db(root.path());
            let asset_id = insert_external(&db, &source, None);
            let caller = caller(&db);
            let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
            let (record, library) = {
                let conn = db.lock().unwrap();
                (
                    legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                        .unwrap()
                        .unwrap(),
                    libraries::current(&conn).unwrap().unwrap(),
                )
            };
            let target = PathBuf::from(record.target_path.as_deref().unwrap());
            let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();
            {
                let conn = db.lock().unwrap();
                assert!(legacy_db::mark_copy_started(
                    &conn,
                    &record.resolution_id,
                    record.target_path.as_deref().unwrap(),
                    now_unix_seconds().unwrap(),
                )
                .unwrap());
            }
            let mut staged = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&stage)
                .unwrap();
            staged.write_all(b"published bytes").unwrap();
            staged.sync_all().unwrap();
            let object_identity = file_object_identity(&staged).unwrap();
            {
                let conn = db.lock().unwrap();
                assert!(legacy_db::set_created_identity(
                    &conn,
                    &record.resolution_id,
                    &object_identity,
                    now_unix_seconds().unwrap(),
                )
                .unwrap());
            }
            drop(staged);
            fs::hard_link(&stage, &target).unwrap();
            if !leave_stage_link {
                fs::remove_file(&stage).unwrap();
            }

            let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
            assert_eq!(result.state, "completed");
            assert_eq!(fs::read(&target).unwrap(), b"published bytes");
            assert!(!stage.exists());
            assert_eq!(result.asset_id, asset_id);
        }
    }

    #[test]
    fn target_without_persisted_ownership_is_never_overwritten_on_recovery() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("foreign-target.jpg");
        fs::write(&source, b"source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                record.target_path.as_deref().unwrap(),
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        fs::write(&target, b"foreign bytes").unwrap();

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        let updated = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        assert_eq!(updated.state, "outcome_unknown");
        assert_eq!(fs::read(&target).unwrap(), b"foreign bytes");
    }

    #[test]
    fn copying_receipt_with_target_outside_ready_root_becomes_unknown() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("copying-target-escape.jpg");
        let outside_target = source_dir.path().join("escaped-target.jpg");
        fs::write(&source, b"source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                &encode_native_path(&outside_target).unwrap(),
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());

        assert!(!outside_target.exists());
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, &record.resolution_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().file_path,
            encode_native_path(&source).unwrap()
        );
    }

    #[test]
    fn prepared_receipt_with_target_outside_ready_root_becomes_unknown() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("prepared-target-escape.jpg");
        let outside_target = source_dir.path().join("prepared-escaped-target.jpg");
        fs::write(&source, b"source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE legacy_asset_resolutions SET target_path=?1 WHERE resolution_id=?2 AND state='prepared'",
                rusqlite::params![encode_native_path(&outside_target).unwrap(), plan.resolution_id.as_str()],
            )
            .unwrap();
        }

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());

        assert!(!outside_target.exists());
        let conn = db.lock().unwrap();
        let resolution = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(resolution.state, "outcome_unknown");
        assert_eq!(resolution.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
        assert_eq!(
            db::assets::get(&conn, asset_id).unwrap().file_path,
            encode_native_path(&source).unwrap()
        );
    }

    #[test]
    fn unavailable_or_wrong_root_preserves_active_receipt_for_recovery() {
        for root_problem in ["missing", "wrong_marker"] {
            let root = tempfile::tempdir().unwrap();
            let source_dir = tempfile::tempdir().unwrap();
            let source = source_dir
                .path()
                .join(format!("root-problem-{root_problem}.jpg"));
            fs::write(&source, b"source bytes").unwrap();
            let db = ready_db(root.path());
            let asset_id = insert_external(&db, &source, None);
            let caller = caller(&db);
            let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();

            match root_problem {
                "missing" => fs::remove_dir_all(root.path()).unwrap(),
                "wrong_marker" => fs::write(
                    root.path().join(managed_library::ROOT_MARKER),
                    "different-library-id",
                )
                .unwrap(),
                _ => unreachable!(),
            }

            assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
            let conn = db.lock().unwrap();
            let receipt = legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap();
            assert_eq!(receipt.state, "prepared", "{root_problem}");
            assert_eq!(receipt.error_code, None, "{root_problem}");
            assert_eq!(
                db::assets::get(&conn, asset_id).unwrap().file_path,
                encode_native_path(&source).unwrap(),
                "{root_problem}"
            );
        }
    }

    #[test]
    fn replaced_identity_verified_stage_is_never_overwritten_on_recovery() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("replaced-target.jpg");
        fs::write(&source, b"source bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        let target = PathBuf::from(record.target_path.as_deref().unwrap());
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let stage = legacy_stage_path(&library, &record.resolution_id).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                record.target_path.as_deref().unwrap(),
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let owned = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .unwrap();
        let expected_identity = file_object_identity(&owned).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &expected_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        drop(owned);
        fs::remove_file(&stage).unwrap();
        fs::write(&stage, b"replacement foreign bytes").unwrap();

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        let updated = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        assert_eq!(updated.state, "outcome_unknown");
        assert!(!target.exists());
        assert_eq!(fs::read(&stage).unwrap(), b"replacement foreign bytes");
    }

    #[test]
    fn published_copy_retries_asset_association_after_transaction_rollback() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("association.jpg");
        fs::write(&source, b"association bytes").unwrap();
        let db = ready_db(root.path());
        let asset_id = insert_external(&db, &source, None);
        let caller = caller(&db);
        let plan = prepare_copy_back(&db, &caller, asset_id, &source).unwrap();
        let record = {
            let conn = db.lock().unwrap();
            legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                .unwrap()
                .unwrap()
        };
        let target_text = record.target_path.as_deref().unwrap();
        let target = PathBuf::from(target_text);
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_copy_started(
                &conn,
                &record.resolution_id,
                target_text,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&target)
            .unwrap();
        let object_identity = file_object_identity(&output).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::set_created_identity(
                &conn,
                &record.resolution_id,
                &object_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        output.write_all(b"association bytes").unwrap();
        output.sync_all().unwrap();
        let published_identity = file_identity(&output).unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(legacy_db::mark_published(
                &conn,
                &record.resolution_id,
                &object_identity,
                &published_identity,
                now_unix_seconds().unwrap(),
            )
            .unwrap());
        }
        drop(output);
        let blocker_id = {
            let conn = db.lock().unwrap();
            db::assets::insert(
                &conn,
                &normalize_path(target_text),
                "occupied.jpg",
                ".jpg",
                1,
                "image/jpeg",
                1,
            )
            .unwrap()
        };

        assert!(confirm(&db, &caller, &plan.resolution_id).is_err());
        {
            let conn = db.lock().unwrap();
            let unchanged = db::assets::get(&conn, asset_id).unwrap();
            assert_eq!(unchanged.file_path, encode_native_path(&source).unwrap());
            assert_eq!(
                legacy_db::get_resolution(&conn, plan.resolution_id.as_str())
                    .unwrap()
                    .unwrap()
                    .state,
                "published"
            );
            conn.execute("DELETE FROM assets WHERE id=?1", [blocker_id])
                .unwrap();
        }
        let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
        assert_eq!(result.state, "completed");
        assert_eq!(fs::read(&target).unwrap(), b"association bytes");
    }

    #[test]
    fn full_hash_collision_is_shown_and_copied_as_a_distinct_asset() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let bytes = b"same bytes in two distinct historical records";
        let managed_file = root.path().join("same.jpg");
        let external_file = source_dir.path().join("same.jpg");
        fs::write(&managed_file, bytes).unwrap();
        fs::write(&external_file, bytes).unwrap();
        let db = ready_db(root.path());
        let managed_id = {
            let conn = db.lock().unwrap();
            let modified_at = modified_at_ms(&fs::metadata(&managed_file).unwrap()).unwrap();
            let id = db::assets::insert(
                &conn,
                &normalize_path(&encode_native_path(&managed_file).unwrap()),
                "same.jpg",
                ".jpg",
                bytes.len() as i64,
                "image/jpeg",
                modified_at,
            )
            .unwrap();
            conn.execute("UPDATE assets SET favorite=1,rating=5 WHERE id=?1", [id])
                .unwrap();
            let tag = db::tags::create(&conn, "現有標籤", None).unwrap();
            conn.execute(
                "INSERT INTO asset_tags(asset_id,tag_id,source,created_at) VALUES (?1,?2,'manual',1)",
                rusqlite::params![id, tag.id],
            )
            .unwrap();
            let library = libraries::current(&conn).unwrap().unwrap();
            let digest = hex_digest(Sha256::digest(bytes));
            let evidence =
                managed_library::prepare_import_evidence(&library, &managed_file, &digest).unwrap();
            let candidate = libraries::verification_candidate(&conn, id)
                .unwrap()
                .unwrap();
            assert!(libraries::record_verified(
                &conn,
                &library,
                &candidate,
                &evidence.relative_path,
                &evidence.file_identity,
                &evidence.content_sha256,
                &evidence.ownership_receipt,
            )
            .unwrap());
            id
        };
        let legacy_id = insert_external(&db, &external_file, None);
        let caller = caller(&db);

        let plan = prepare_copy_back(&db, &caller, legacy_id, &external_file).unwrap();
        assert!(plan.keep_separate_required);
        assert_eq!(plan.matching_assets.len(), 1);
        assert_eq!(plan.matching_assets[0].asset_id, managed_id);
        assert!(plan.matching_assets[0]
            .tags
            .iter()
            .any(|tag| tag == "現有標籤"));

        let result = confirm(&db, &caller, &plan.resolution_id).unwrap();
        assert_eq!(result.state, "completed");
        assert_eq!(result.asset_id, legacy_id);
        assert_eq!(result.matching_assets.len(), 1);
        assert_eq!(fs::read(&external_file).unwrap(), bytes);
        let conn = db.lock().unwrap();
        let original = db::assets::get(&conn, managed_id).unwrap();
        let restored = db::assets::get(&conn, legacy_id).unwrap();
        assert_eq!(original.favorite, 1);
        assert_eq!(original.rating, 5);
        assert!(original.tags.iter().any(|tag| tag.name == "現有標籤"));
        assert_eq!(restored.favorite, 0);
        assert_eq!(restored.rating, 0);
        assert!(!restored.tags.iter().any(|tag| tag.name == "現有標籤"));
        assert_ne!(original.file_path, restored.file_path);
        assert_eq!(
            db::assets::get_managed(&conn, legacy_id).unwrap().id,
            legacy_id
        );
    }
}
