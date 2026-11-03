//! A3-03: capability execution shared by the Pi adapter, MCP adapter and tests.
//!
//! Every entry validates the caller's capability permission, reads through the
//! existing domain services/SQL, and returns the frozen contract records. No
//! model-supplied principal, approval or path is accepted here: the caller is
//! always built by the host adapter from authenticated state.

use crate::contracts::agent::{
    AgentErrorCode, AgentOutput, AssetSummary, BudgetSummary, CapabilityId, ExportLayout,
    Instructions, LoadWorkflowInput, OperationSnapshot, PlanPreview, PlanScopeSummary,
    PrepareExportInput, PrepareMoveInput, PrepareSelectionInput, PrepareTaggingInput,
    PrepareWorkflowInput, ProtocolVersion, ResourceRef, SearchAssetsInput, SearchPage, ShortText,
    SubmissionResult, SubmitPlanInput, WorkflowPlanPreview, WorkflowStepSummary, WorkflowSummary,
    MAX_PAGE_ITEMS, SHORT_TEXT_MAX_BYTES,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::assets::{self, AssetFilter};
use crate::db::libraries::{self, LibraryHealth};
use crate::db::operations::{self as operations_db, NewOperationItem, NewOperationStep};
use crate::db::plans as plans_db;
use crate::db::selections;
use crate::error::{AppError, AppResult};
use crate::services::authorization;
use crate::services::library_move;
use crate::services::operations;
use crate::services::plans;
use crate::services::selections as selection_service;
use crate::services::workflow_run;
use crate::state::Database;

/// Stable reference for one asset row. Opaque to the model and re-checked on use.
pub fn asset_ref(asset_id: i64) -> AppResult<ResourceRef> {
    ResourceRef::from_trusted(format!("asset_{asset_id:016}")).map_err(AppError::from)
}

/// `assets.search`: run the normal library search and persist the exact result set.
pub fn search_assets(
    db: &Database,
    caller: &CallerContext,
    input: &SearchAssetsInput,
) -> AppResult<SearchPage> {
    check_capability_entry(CapabilityId::AssetsSearch, caller).map_err(AppError::from)?;
    if input.cursor.is_some() {
        // Pagination cursors are not implemented yet; fail closed instead of
        // silently returning the first page again.
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    let text = input.text.as_str();
    let filter = AssetFilter {
        search: (!text.is_empty()).then(|| text.to_string()),
        limit: MAX_PAGE_ITEMS as i64,
        ..AssetFilter::default()
    };
    let (page, all_ids) = {
        let conn = db.lock()?;
        (
            assets::list(&conn, &filter)?,
            assets::list_ids(&conn, &filter)?,
        )
    };
    let stored_ids = &all_ids[..all_ids.len().min(selections::MAX_SELECTION_ASSETS)];
    let selection = selection_service::store(db, caller, text, stored_ids, page.total)?;
    let items = page
        .items
        .iter()
        .map(|asset| {
            Ok(AssetSummary {
                asset_ref: asset_ref(asset.id)?,
                display_name: short_text(&asset.file_name)?,
                preview_ref: None,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let total = u32::try_from(page.total).map_err(|_| AppError::from(AgentErrorCode::Internal))?;
    let result = SearchPage {
        schema_version: ProtocolVersion::V1,
        library: caller.library.clone(),
        selection_ref: selection.selection_ref.clone(),
        total,
        truncated: selection.truncated || all_ids.len() > page.items.len(),
        items,
        cursor: None,
    };
    result.validate_output().map_err(AppError::from)?;
    Ok(result)
}

/// `operations.get`: current state of an operation owned by this caller.
///
/// Ownership is re-checked in `services::plans`; a model can only observe the
/// operations created from its own approved plans.
pub fn get_operation(
    db: &Database,
    caller: &CallerContext,
    operation_id: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsGet, caller).map_err(AppError::from)?;
    plans::get_operation(db, caller, operation_id)
}

/// `operations.cancel`: request cancellation of the remaining items.
///
/// This asks the domain service to cancel; it never deletes or rolls back work
/// that already happened.
pub fn cancel_operation(
    db: &Database,
    caller: &CallerContext,
    operation_id: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsCancel, caller).map_err(AppError::from)?;
    operations::cancel_operation(db, caller, operation_id)
}

/// Immutable details of one trash plan, persisted in the plan row.
///
/// The asset-id list is the exact set the caller's selection resolved to; it is
/// compared again at submit time so an approved plan cannot be widened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrashPlanSnapshot {
    schema_version: ProtocolVersion,
    selection_ref: ResourceRef,
    asset_ids: Vec<i64>,
}

/// List a few concrete names so the review never says only "N 项".
/// Bounded because `Instructions` itself is capped at 4096 bytes.
fn name_preview(names: &[String], limit: usize, max_chars: usize) -> String {
    let mut preview = String::new();
    for name in names.iter().take(limit) {
        if !preview.is_empty() {
            preview.push('、');
        }
        let remaining = max_chars.saturating_sub(preview.chars().count());
        if remaining == 0 {
            break;
        }
        let short: String = name.chars().take(remaining).collect();
        let truncated = short.chars().count() < name.chars().count();
        preview.push_str(&short);
        if truncated {
            preview.push('…');
            break;
        }
    }
    if names.len() > limit {
        preview.push_str(&format!(" 等 {} 项", names.len()));
    }
    preview
}

fn trash_summary(names: &[String], expires_at_ms: i64) -> AppResult<Instructions> {
    serde_json::from_value(serde_json::Value::String(format!(
        "将选中的 {} 项素材移入回收站：{}。预计 {} 自动清理，期间可恢复",
        names.len(),
        name_preview(names, 5, 120),
        crate::services::trash::format_day(expires_at_ms)
    )))
    .map_err(|_| AppError::internal("计划摘要无法生成"))
}

/// Rebuild the same draft `prepare_trash` hashed so `plan_hash` matches.
fn trash_draft(
    snapshot: &TrashPlanSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plans::PlanDraft> {
    let item_count = u32::try_from(snapshot.asset_ids.len())
        .map_err(|_| AppError::from(AgentErrorCode::InvalidInput))?;
    Ok(plans::PlanDraft {
        capability: CapabilityId::AssetsTrashSubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

fn trash_scope(selection_ref: ResourceRef) -> PlanScopeSummary {
    PlanScopeSummary {
        scope_ref: selection_ref.clone(),
        source_ref: None,
        selection_ref: Some(selection_ref),
        target_ref: None,
        service_ref: None,
        service_revision: None,
        model: None,
    }
}

/// `assets.trash.prepare`: turn the caller's selection into a reviewable plan.
///
/// Nothing is deleted here. The caller must have resolved the selection through
/// its own `assets.search`, so the target set is never model-supplied.
pub fn prepare_trash(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareSelectionInput,
) -> AppResult<PlanPreview> {
    check_capability_entry(CapabilityId::AssetsTrashPrepare, caller).map_err(AppError::from)?;
    let selection = selection_service::resolve(db, caller, &input.selection_ref)?;
    if selection.asset_ids.is_empty() || selection.truncated {
        // 回收站删除禁止作用于截断集合（docs/contracts/search-plan-v3.md §8）：
        // 模型必须收窄条件后重新搜索，而不是把不完整集合当成全部。
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let active = {
        let conn = db.lock()?;
        assets::count_active(&conn, &selection.asset_ids)?
    };
    if active as usize != selection.asset_ids.len() {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let (names, retention_days, now_ms) = {
        let conn = db.lock()?;
        let names = assets::active_names(&conn, &selection.asset_ids)?
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>();
        let settings = crate::db::settings::get_settings(&conn)?;
        (
            names,
            settings.trash_retention_days,
            chrono::Utc::now().timestamp_millis(),
        )
    };
    // 摘要列出具体素材与预计清理时间，且参与 `plan_hash`：提交时从持久计划复读，
    // 不重新计算（重新算会因时间前进而改变，把合法提交判成「计划已改变」）。
    let summary = trash_summary(
        &names,
        crate::services::trash::expires_at_ms(now_ms, retention_days),
    )?;
    let snapshot = TrashPlanSnapshot {
        schema_version: ProtocolVersion::V1,
        selection_ref: selection.selection_ref.clone(),
        asset_ids: selection.asset_ids.clone(),
    };
    let details_json =
        serde_json::to_string(&snapshot).map_err(|_| AppError::internal("计划明细序列化失败"))?;
    let draft = trash_draft(
        &snapshot,
        trash_scope(snapshot.selection_ref.clone()),
        Some(details_json),
        summary,
    )?;
    plans::prepare_plan(db, caller, draft)
}

/// `assets.trash.submit`: consume the GUI approval and soft-delete exactly the
/// approved selection in one transaction with its operation record.
pub fn submit_trash(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::AssetsTrashSubmit, caller).map_err(AppError::from)?;
    let (snapshot, record, summary) = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, input.plan_id.as_str())?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.capability != "assets.trash.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        let details_json = record
            .details_json
            .as_deref()
            .ok_or(AppError::from(AgentErrorCode::PlanChanged))?;
        let snapshot: TrashPlanSnapshot = serde_json::from_str(details_json)
            .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
        if record.selection_ref.as_deref() != Some(snapshot.selection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
            .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
        (snapshot, record, summary)
    };
    // Re-check principal, expiry and library epoch outside any lock, and make
    // sure the stored selection still resolves to the same asset ids.
    let selection = selection_service::resolve(db, caller, &snapshot.selection_ref)?;
    if selection.asset_ids != snapshot.asset_ids {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let current = {
        let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
            value
                .as_ref()
                .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
                .transpose()
        };
        let scope = PlanScopeSummary {
            scope_ref: ResourceRef::from_trusted(record.scope_ref.clone())
                .map_err(AppError::from)?,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value))
                        .map_err(|_| AppError::internal("持久计划模型名损坏"))
                })
                .transpose()?,
        };
        trash_draft(&snapshot, scope, record.details_json.clone(), summary)?
    };
    plans::submit_plan_with_effect(
        db,
        caller,
        &input.plan_id,
        &input.idempotency_key,
        &current,
        |transaction, operation| {
            let deleted = assets::soft_delete(transaction, &snapshot.asset_ids)?;
            if deleted as usize != snapshot.asset_ids.len() {
                // Some rows left the active set after approval: abort the whole
                // effect instead of trashing a partial set.
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
            operations_db::insert_step(
                transaction,
                &NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: "assets.trash.submit".to_string(),
                    total: Some(
                        i64::try_from(deleted)
                            .map_err(|_| AppError::internal("回收站操作条目数超出范围"))?,
                    ),
                    created_at: operation.created_at,
                },
            )?;
            operations_db::settle_operation(
                transaction,
                &operation.operation_id,
                "succeeded",
                operation.created_at,
            )?;
            Ok(())
        },
    )
}

/// Frozen facts of one copy-export plan. The destination is referenced, never
/// a free-form path, so an approved plan cannot be redirected after review.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportPlanSnapshot {
    schema_version: ProtocolVersion,
    selection_ref: ResourceRef,
    destination_ref: ResourceRef,
    layout: ExportLayout,
    asset_ids: Vec<i64>,
}

/// Stable per-asset item id so a retried export maps onto the same items.
///
/// `pub(crate)` because the GUI export command must address the very same
/// operation items when it writes one receipt per copied file.
pub(crate) fn export_item_id(asset_id: i64) -> String {
    format!("export_{asset_id:016}")
}

/// 已批准复制导出计划的冻结输入。
///
/// 目标以引用形式返回，执行器必须再用授权服务解析一次句柄：冻结的裸路径在
/// 用户搬动目标目录后会指向错误位置，而句柄会校验文件系统身份。
pub(crate) struct ExportPlanItems {
    pub(crate) destination_ref: ResourceRef,
    pub(crate) layout: ExportLayout,
    pub(crate) asset_ids: Vec<i64>,
}

/// 读取已批准复制导出计划的冻结集合。
///
/// 与 `move_plan_items` 同理：执行期绝不重新推导目标，否则批准时看到的与真正
/// 写入的可能是两个不同的目录。
pub(crate) fn export_plan_items(
    db: &Database,
    plan_ref: &ResourceRef,
) -> AppResult<ExportPlanItems> {
    let conn = db.lock()?;
    let record = plans_db::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let details_json = record
        .details_json
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let snapshot: ExportPlanSnapshot = serde_json::from_str(details_json)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    Ok(ExportPlanItems {
        destination_ref: snapshot.destination_ref,
        layout: snapshot.layout,
        asset_ids: snapshot.asset_ids,
    })
}

fn export_layout_label(layout: ExportLayout) -> &'static str {
    match layout {
        ExportLayout::Flat => "平铺",
        ExportLayout::ByTag => "按标签分目录",
        ExportLayout::ByDate => "按拍摄月份分目录",
    }
}

/// The review summary must name the operation and where the files land.
///
/// A4-01 asked for "复制导出" to be explicit in the assistant plan, and an
/// opaque "授权目录" does not let a reviewer check the destination: the only
/// place that knows the real path is `resolve_export_directory`, so the display
/// path is captured while the grant is still resolved and frozen into the plan.
fn export_summary(
    count: usize,
    layout: ExportLayout,
    destination_display: &str,
) -> AppResult<Instructions> {
    // A very deep path could otherwise blow the Instructions byte budget and
    // turn "prepare" into an opaque INTERNAL failure; keep the tail (the part
    // that identifies the folder) and say it was shortened. Measured in chars
    // so a Chinese path is not cut mid-glyph.
    const MAX_PATH_CHARS: usize = 240;
    let destination = if destination_display.chars().count() > MAX_PATH_CHARS {
        let tail: String = destination_display
            .chars()
            .rev()
            .take(MAX_PATH_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}（路径过长，仅显示末尾）")
    } else {
        destination_display.to_string()
    };
    serde_json::from_value(serde_json::Value::String(format!(
        "复制导出：将选中的 {count} 项素材复制到 {destination}（{}），库内原文件与记录保留、不移动",
        export_layout_label(layout)
    )))
    .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

fn export_scope(selection_ref: ResourceRef, destination_ref: ResourceRef) -> PlanScopeSummary {
    PlanScopeSummary {
        scope_ref: selection_ref.clone(),
        source_ref: None,
        selection_ref: Some(selection_ref),
        target_ref: Some(destination_ref),
        service_ref: None,
        service_revision: None,
        model: None,
    }
}

fn export_draft(
    snapshot: &ExportPlanSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plans::PlanDraft> {
    let item_count = u32::try_from(snapshot.asset_ids.len())
        .map_err(|_| AppError::invalid_arg("导出条目数超出范围"))?;
    Ok(plans::PlanDraft {
        capability: CapabilityId::ExportsSubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

/// `exports.prepare`: freeze the selection, the authorised destination and the
/// layout into a reviewable plan. Nothing is copied here.
pub fn prepare_export(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareExportInput,
) -> AppResult<PlanPreview> {
    check_capability_entry(CapabilityId::ExportsPrepare, caller).map_err(AppError::from)?;
    let selection = selection_service::resolve(db, caller, &input.selection_ref)?;
    if selection.asset_ids.is_empty() || selection.truncated {
        // Export must never silently cover only the first page of a truncated
        // selection (docs/contracts/search-plan-v3.md §8).
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    {
        let conn = db.lock()?;
        if assets::count_active(&conn, &selection.asset_ids)? != selection.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
    }
    // Fail fast when the destination grant is missing, revoked or read-only, so
    // the user learns about it at review time instead of mid-copy. The resolved
    // path is also the only honest source for "where does this copy to", so it
    // is written into the reviewable summary instead of being dropped.
    let destination = authorization::resolve_export_directory(db, caller, &input.destination_ref)?;
    // Display form only: `canonicalize` yields `\\?\C:\...` on Windows and a
    // reviewer comparing against Explorer would think the wrong folder was
    // picked. Execution still uses the native path.
    let destination_display = crate::utils::path::display_native_path(&destination);
    let snapshot = ExportPlanSnapshot {
        schema_version: input.schema_version,
        selection_ref: input.selection_ref.clone(),
        destination_ref: input.destination_ref.clone(),
        layout: input.layout,
        asset_ids: selection.asset_ids,
    };
    let details_json = serde_json::to_string(&snapshot)
        .map_err(|_| AppError::internal("导出计划快照序列化失败"))?;
    let draft = export_draft(
        &snapshot,
        export_scope(
            snapshot.selection_ref.clone(),
            snapshot.destination_ref.clone(),
        ),
        Some(details_json),
        export_summary(
            snapshot.asset_ids.len(),
            snapshot.layout,
            &destination_display,
        )?,
    )?;
    plans::prepare_plan(db, caller, draft)
}

/// `exports.submit`: consume the approval and register one operation item per
/// asset. The copy happens afterwards and reports per-file receipts, because a
/// database transaction cannot roll back files that already landed on disk.
pub fn submit_export(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::ExportsSubmit, caller).map_err(AppError::from)?;
    let (snapshot, record, summary) = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, input.plan_id.as_str())?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.capability != "exports.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        let details_json = record
            .details_json
            .as_deref()
            .ok_or(AppError::from(AgentErrorCode::PlanChanged))?;
        let snapshot: ExportPlanSnapshot = serde_json::from_str(details_json)
            .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
        if record.selection_ref.as_deref() != Some(snapshot.selection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        if record.target_ref.as_deref() != Some(snapshot.destination_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
            .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
        (snapshot, record, summary)
    };
    // Re-check principal, expiry, library epoch and both handles outside any lock.
    let selection = selection_service::resolve(db, caller, &snapshot.selection_ref)?;
    if selection.asset_ids != snapshot.asset_ids {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    // The selection snapshot is frozen at prepare time, so it cannot notice an
    // asset that was trashed while the user was reviewing. Re-check the active
    // set here: copying a row that is already gone from the library would make
    // the approved item count a lie.
    {
        let conn = db.lock()?;
        if assets::count_active(&conn, &snapshot.asset_ids)? != snapshot.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
    }
    authorization::resolve_export_directory(db, caller, &snapshot.destination_ref)?;
    let current = {
        let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
            value
                .as_ref()
                .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
                .transpose()
        };
        let scope = PlanScopeSummary {
            scope_ref: ResourceRef::from_trusted(record.scope_ref.clone())
                .map_err(AppError::from)?,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value))
                        .map_err(|_| AppError::internal("持久计划模型名损坏"))
                })
                .transpose()?,
        };
        export_draft(&snapshot, scope, record.details_json.clone(), summary)?
    };
    plans::submit_plan_with_effect(
        db,
        caller,
        &input.plan_id,
        &input.idempotency_key,
        &current,
        |transaction, operation| {
            let total = i64::try_from(snapshot.asset_ids.len())
                .map_err(|_| AppError::internal("导出操作条目数超出范围"))?;
            operations_db::insert_step(
                transaction,
                &NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: "exports.submit".to_string(),
                    total: Some(total),
                    created_at: operation.created_at,
                },
            )?;
            for (ordinal, asset_id) in snapshot.asset_ids.iter().enumerate() {
                operations_db::insert_item(
                    transaction,
                    &NewOperationItem {
                        operation_id: operation.operation_id.clone(),
                        step_id: operation.step_id.clone(),
                        item_id: export_item_id(*asset_id),
                        ordinal: i64::try_from(ordinal)
                            .map_err(|_| AppError::internal("导出条目序号超出范围"))?,
                        input_fingerprint: None,
                        target_ref: Some(snapshot.destination_ref.as_str().to_string()),
                        created_at: operation.created_at,
                    },
                )?;
            }
            Ok(())
        },
    )
}

/// Frozen facts of one in-library move plan.
///
/// The target is a collection reference, never a path: a collection belongs to
/// one `library_id + library_epoch`, so a plan approved before a root switch
/// cannot be replayed onto a different root. The per-file target names are
/// frozen here as well, so the executor cannot silently pick a different
/// collision suffix than the one the user reviewed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MovePlanSnapshot {
    schema_version: ProtocolVersion,
    selection_ref: ResourceRef,
    collection_ref: ResourceRef,
    asset_ids: Vec<i64>,
    targets: Vec<MoveTargetSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveTargetSnapshot {
    asset_id: i64,
    source_path: String,
    target_path: String,
    /// 批准时冻结的源文件身份。旧计划快照没有这个字段，`default` 让它们仍可解析
    /// （执行阶段回退到「执行开始时采集 + 立刻复验」），而不是整体判成 `PlanChanged`。
    #[serde(default)]
    source_identity: String,
}

fn move_summary(count: usize, collection_relative_path: &str) -> AppResult<Instructions> {
    let label = if collection_relative_path.is_empty() {
        "总仓根目录".to_string()
    } else {
        format!("分库「{collection_relative_path}」")
    };
    serde_json::from_value(serde_json::Value::String(format!(
        "将选中的 {count} 项素材移动到总仓内{label}，素材 ID、标签与历史保留"
    )))
    .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

fn move_scope(selection_ref: ResourceRef, collection_ref: ResourceRef) -> PlanScopeSummary {
    PlanScopeSummary {
        scope_ref: selection_ref.clone(),
        source_ref: None,
        selection_ref: Some(selection_ref),
        target_ref: Some(collection_ref),
        service_ref: None,
        service_revision: None,
        model: None,
    }
}

fn move_draft(
    snapshot: &MovePlanSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plans::PlanDraft> {
    let item_count = u32::try_from(snapshot.targets.len())
        .map_err(|_| AppError::invalid_arg("移动条目数超出范围"))?;
    Ok(plans::PlanDraft {
        capability: CapabilityId::AssetsMoveSubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

/// `assets.move.prepare`: freeze the movable files and the target collection.
/// Nothing on disk changes here.
pub fn prepare_move(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareMoveInput,
) -> AppResult<PlanPreview> {
    check_capability_entry(CapabilityId::AssetsMovePrepare, caller).map_err(AppError::from)?;
    let selection = selection_service::resolve(db, caller, &input.selection_ref)?;
    if selection.asset_ids.is_empty() || selection.truncated {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let (library, target) = {
        let conn = db.lock()?;
        if assets::count_active(&conn, &selection.asset_ids)? != selection.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let library =
            libraries::current(&conn)?.ok_or(AppError::from(AgentErrorCode::LibraryOffline))?;
        if library.health != LibraryHealth::Ready {
            return Err(AppError::from(AgentErrorCode::LibraryOffline));
        }
        let target = library_move::resolve_target(&conn, &library, &input.collection_ref)?;
        (library, target)
    };
    let items = library_move::plan_items(db, &library, &selection.asset_ids, &target)?;
    if items.is_empty() {
        return Err(AppError::invalid_arg("所选素材都已在目标分库，无需移动"));
    }
    let snapshot = MovePlanSnapshot {
        schema_version: input.schema_version,
        selection_ref: input.selection_ref.clone(),
        collection_ref: input.collection_ref.clone(),
        asset_ids: selection.asset_ids,
        targets: items
            .into_iter()
            .map(|item| MoveTargetSnapshot {
                asset_id: item.asset_id,
                source_path: item.source_path,
                target_path: item.target_path,
                source_identity: item.source_identity,
            })
            .collect(),
    };
    let details_json = serde_json::to_string(&snapshot)
        .map_err(|_| AppError::internal("移动计划快照序列化失败"))?;
    let draft = move_draft(
        &snapshot,
        move_scope(
            snapshot.selection_ref.clone(),
            snapshot.collection_ref.clone(),
        ),
        Some(details_json),
        move_summary(snapshot.targets.len(), &target.relative_path)?,
    )?;
    plans::prepare_plan(db, caller, draft)
}

/// `assets.move.submit`: consume the approval and register one operation item
/// per moved file. The file system operation happens afterwards and reports one
/// receipt per file, because a database transaction cannot roll back renames.
pub fn submit_move(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::AssetsMoveSubmit, caller).map_err(AppError::from)?;
    let (snapshot, record, summary) = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, input.plan_id.as_str())?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.capability != "assets.move.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        let details_json = record
            .details_json
            .as_deref()
            .ok_or(AppError::from(AgentErrorCode::PlanChanged))?;
        let snapshot: MovePlanSnapshot = serde_json::from_str(details_json)
            .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
        if record.selection_ref.as_deref() != Some(snapshot.selection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        if record.target_ref.as_deref() != Some(snapshot.collection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
            .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
        (snapshot, record, summary)
    };
    // Re-check principal, selection contents and the current library epoch.
    let selection = selection_service::resolve(db, caller, &snapshot.selection_ref)?;
    if selection.asset_ids != snapshot.asset_ids {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    {
        let conn = db.lock()?;
        if assets::count_active(&conn, &snapshot.asset_ids)? != snapshot.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let library =
            libraries::current(&conn)?.ok_or(AppError::from(AgentErrorCode::LibraryOffline))?;
        library_move::resolve_target(&conn, &library, &snapshot.collection_ref)?;
    }
    let current = {
        let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
            value
                .as_ref()
                .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
                .transpose()
        };
        let scope = PlanScopeSummary {
            scope_ref: ResourceRef::from_trusted(record.scope_ref.clone())
                .map_err(AppError::from)?,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value))
                        .map_err(|_| AppError::internal("持久计划模型名损坏"))
                })
                .transpose()?,
        };
        move_draft(&snapshot, scope, record.details_json.clone(), summary)?
    };
    plans::submit_plan_with_effect(
        db,
        caller,
        &input.plan_id,
        &input.idempotency_key,
        &current,
        |transaction, operation| {
            let total = i64::try_from(snapshot.targets.len())
                .map_err(|_| AppError::internal("移动操作条目数超出范围"))?;
            operations_db::insert_step(
                transaction,
                &NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: "assets.move.submit".to_string(),
                    total: Some(total),
                    created_at: operation.created_at,
                },
            )?;
            for (ordinal, target) in snapshot.targets.iter().enumerate() {
                operations_db::insert_item(
                    transaction,
                    &NewOperationItem {
                        operation_id: operation.operation_id.clone(),
                        step_id: operation.step_id.clone(),
                        item_id: library_move::move_item_id(target.asset_id),
                        ordinal: i64::try_from(ordinal)
                            .map_err(|_| AppError::internal("移动条目序号超出范围"))?,
                        input_fingerprint: None,
                        target_ref: Some(target.target_path.clone()),
                        created_at: operation.created_at,
                    },
                )?;
            }
            Ok(())
        },
    )
}

/// Load the frozen per-file targets of an approved move plan.
///
/// The GUI move command must rename exactly these paths; re-deriving them would
/// let a different collision suffix slip past what the user approved.
pub(crate) fn move_plan_items(
    db: &Database,
    plan_ref: &ResourceRef,
) -> AppResult<Vec<library_move::MovePlanItem>> {
    let conn = db.lock()?;
    let record = plans_db::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let details_json = record
        .details_json
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let snapshot: MovePlanSnapshot = serde_json::from_str(details_json)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    Ok(snapshot
        .targets
        .into_iter()
        .map(|target| library_move::MovePlanItem {
            asset_id: target.asset_id,
            source_path: target.source_path,
            target_path: target.target_path,
            source_identity: target.source_identity,
        })
        .collect())
}

/// Frozen facts of one restore plan.
///
/// Restoring is the inverse of trashing, but the plan must not merely say "N 项":
/// A4-03 要求恢复核对文件实际存在与归属，所以摘要列出具体素材名，
/// 让确认者看到的是「会把哪几项放回素材库」。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestorePlanSnapshot {
    schema_version: ProtocolVersion,
    selection_ref: ResourceRef,
    asset_ids: Vec<i64>,
}

fn restore_summary(names: &[String]) -> AppResult<Instructions> {
    serde_json::from_value(serde_json::Value::String(format!(
        "将回收站中选中的 {} 项素材恢复到原位置：{}。文件缺失或已被替换的项会单独报告，不会假装恢复成功",
        names.len(),
        name_preview(names, 5, 120)
    )))
    .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

fn restore_scope(selection_ref: ResourceRef) -> PlanScopeSummary {
    PlanScopeSummary {
        scope_ref: selection_ref.clone(),
        source_ref: None,
        selection_ref: Some(selection_ref),
        target_ref: None,
        service_ref: None,
        service_revision: None,
        model: None,
    }
}

fn restore_draft(
    snapshot: &RestorePlanSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plans::PlanDraft> {
    let item_count = u32::try_from(snapshot.asset_ids.len())
        .map_err(|_| AppError::from(AgentErrorCode::InvalidInput))?;
    Ok(plans::PlanDraft {
        capability: CapabilityId::AssetsRestoreSubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

/// `assets.restore.prepare`: freeze the trashed set that a review would restore.
///
/// Nothing changes here, and the set must still be in the trash: an id that a
/// concurrent purge already removed makes the approved count a lie.
pub fn prepare_restore(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareSelectionInput,
) -> AppResult<PlanPreview> {
    check_capability_entry(CapabilityId::AssetsRestorePrepare, caller).map_err(AppError::from)?;
    let selection = selection_service::resolve(db, caller, &input.selection_ref)?;
    if selection.asset_ids.is_empty() || selection.truncated {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let names = {
        let conn = db.lock()?;
        if assets::count_trashed(&conn, &selection.asset_ids)? != selection.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        assets::trash_records(&conn, &selection.asset_ids)?
            .into_iter()
            .map(|record| record.file_name)
            .collect::<Vec<_>>()
    };
    let snapshot = RestorePlanSnapshot {
        schema_version: ProtocolVersion::V1,
        selection_ref: selection.selection_ref.clone(),
        asset_ids: selection.asset_ids.clone(),
    };
    let details_json =
        serde_json::to_string(&snapshot).map_err(|_| AppError::internal("计划明细序列化失败"))?;
    let draft = restore_draft(
        &snapshot,
        restore_scope(snapshot.selection_ref.clone()),
        Some(details_json),
        restore_summary(&names)?,
    )?;
    plans::prepare_plan(db, caller, draft)
}

/// `assets.restore.submit`: consume the approval and register one operation item
/// per restored asset. `deleted_at` is cleared later, one verified item at a time,
/// because restoring must prove the file is still where the record says it is.
pub fn submit_restore(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::AssetsRestoreSubmit, caller).map_err(AppError::from)?;
    let (snapshot, record, summary) = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, input.plan_id.as_str())?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.capability != "assets.restore.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        let details_json = record
            .details_json
            .as_deref()
            .ok_or(AppError::from(AgentErrorCode::PlanChanged))?;
        let snapshot: RestorePlanSnapshot = serde_json::from_str(details_json)
            .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
        if record.selection_ref.as_deref() != Some(snapshot.selection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
            .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
        (snapshot, record, summary)
    };
    let selection = selection_service::resolve(db, caller, &snapshot.selection_ref)?;
    if selection.asset_ids != snapshot.asset_ids {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    // 「集合仍全部在回收站」的校验放进 effect 而不是提交前：幂等重放要在
    // `submit_plan_with_effect` 里先被识别出来，否则一次成功的恢复之后重放
    // 会先撞上集合变化被误判成「计划已改变」。effect 只在真正创建新操作时执行。
    let current = {
        let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
            value
                .as_ref()
                .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
                .transpose()
        };
        let scope = PlanScopeSummary {
            scope_ref: ResourceRef::from_trusted(record.scope_ref.clone())
                .map_err(AppError::from)?,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value))
                        .map_err(|_| AppError::internal("持久计划模型名损坏"))
                })
                .transpose()?,
        };
        restore_draft(&snapshot, scope, record.details_json.clone(), summary)?
    };
    plans::submit_plan_with_effect(
        db,
        caller,
        &input.plan_id,
        &input.idempotency_key,
        &current,
        |transaction, operation| {
            // 恢复前集合必须仍完整留在回收站：已被其他入口恢复或清理的项
            // 会让批准的条目数变成谎话，所以整批失败而不是恢复一个子集。
            if assets::count_trashed(transaction, &snapshot.asset_ids)?
                != snapshot.asset_ids.len() as u64
            {
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
            let total = i64::try_from(snapshot.asset_ids.len())
                .map_err(|_| AppError::internal("恢复操作条目数超出范围"))?;
            operations_db::insert_step(
                transaction,
                &NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: "assets.restore.submit".to_string(),
                    total: Some(total),
                    created_at: operation.created_at,
                },
            )?;
            for (ordinal, asset_id) in snapshot.asset_ids.iter().enumerate() {
                operations_db::insert_item(
                    transaction,
                    &NewOperationItem {
                        operation_id: operation.operation_id.clone(),
                        step_id: operation.step_id.clone(),
                        item_id: crate::services::trash::restore_item_id(*asset_id),
                        ordinal: i64::try_from(ordinal)
                            .map_err(|_| AppError::internal("恢复条目序号超出范围"))?,
                        input_fingerprint: None,
                        target_ref: None,
                        created_at: operation.created_at,
                    },
                )?;
            }
            Ok(())
        },
    )
}

/// Load the frozen asset ids of an approved restore plan, in approved order.
pub(crate) fn restore_plan_items(db: &Database, plan_ref: &ResourceRef) -> AppResult<Vec<i64>> {
    let conn = db.lock()?;
    let record = plans_db::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let details_json = record
        .details_json
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let snapshot: RestorePlanSnapshot = serde_json::from_str(details_json)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    Ok(snapshot.asset_ids)
}

/// `trash.empty.*` 的冻结快照。
///
/// 与恢复快照形状相同，但语义严格不同：这里的集合一旦确认就是**不可撤销的
/// 永久删除**，所以计划审核类型必须是 `PermanentTrash`，普通计划确认不能替代。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EmptyPlanSnapshot {
    schema_version: ProtocolVersion,
    selection_ref: ResourceRef,
    asset_ids: Vec<i64>,
}

fn empty_summary(names: &[String], retention_days: i64) -> AppResult<Instructions> {
    let scope_note = if retention_days > 0 {
        format!("这些素材已超出 {retention_days} 天保留期或已被手动选中")
    } else {
        "这些素材已被手动选中".to_string()
    };
    serde_json::from_value(serde_json::Value::String(format!(
        "永久删除回收站中选中的 {} 项素材（连同原始文件，不可恢复）：{}。{scope_note}；只有在本界面单独确认「永久清空」才会执行",
        names.len(),
        name_preview(names, 5, 120)
    )))
    .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

fn empty_scope(selection_ref: ResourceRef) -> PlanScopeSummary {
    PlanScopeSummary {
        scope_ref: selection_ref.clone(),
        source_ref: None,
        selection_ref: Some(selection_ref),
        target_ref: None,
        service_ref: None,
        service_revision: None,
        model: None,
    }
}

fn empty_draft(
    snapshot: &EmptyPlanSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plans::PlanDraft> {
    let item_count = u32::try_from(snapshot.asset_ids.len())
        .map_err(|_| AppError::from(AgentErrorCode::InvalidInput))?;
    Ok(plans::PlanDraft {
        capability: CapabilityId::TrashEmptySubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

/// `trash.empty.prepare`: freeze the exact trashed set a permanent empty would
/// delete. Nothing is deleted here, and the set must still be in the trash.
pub fn prepare_empty(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareSelectionInput,
) -> AppResult<PlanPreview> {
    check_capability_entry(CapabilityId::TrashEmptyPrepare, caller).map_err(AppError::from)?;
    let selection = selection_service::resolve(db, caller, &input.selection_ref)?;
    if selection.asset_ids.is_empty() || selection.truncated {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let (names, retention_days) = {
        let conn = db.lock()?;
        if assets::count_trashed(&conn, &selection.asset_ids)? != selection.asset_ids.len() as u64 {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let names = assets::trash_records(&conn, &selection.asset_ids)?
            .into_iter()
            .map(|record| record.file_name)
            .collect::<Vec<_>>();
        let retention_days = crate::db::settings::get_settings(&conn)?.trash_retention_days;
        (names, retention_days)
    };
    let snapshot = EmptyPlanSnapshot {
        schema_version: ProtocolVersion::V1,
        selection_ref: selection.selection_ref.clone(),
        asset_ids: selection.asset_ids.clone(),
    };
    let details_json =
        serde_json::to_string(&snapshot).map_err(|_| AppError::internal("计划明细序列化失败"))?;
    let draft = empty_draft(
        &snapshot,
        empty_scope(snapshot.selection_ref.clone()),
        Some(details_json),
        empty_summary(&names, retention_days)?,
    )?;
    plans::prepare_plan(db, caller, draft)
}

/// `trash.empty.submit`: consume the independent permanent-trash approval and
/// register one operation item per frozen asset. The bytes are removed later by
/// `services::trash::empty`, one fully verified item at a time.
pub fn submit_empty(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::TrashEmptySubmit, caller).map_err(AppError::from)?;
    let (snapshot, record, summary) = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, input.plan_id.as_str())?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.capability != "trash.empty.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        // 普通计划确认不能替代永久清空：审核类型必须在准备阶段就已是
        // `PermanentTrash`，这里再核一次，防止计划被替换。
        if record.review_kind != "permanent_trash" {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        let details_json = record
            .details_json
            .as_deref()
            .ok_or(AppError::from(AgentErrorCode::PlanChanged))?;
        let snapshot: EmptyPlanSnapshot = serde_json::from_str(details_json)
            .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
        if record.selection_ref.as_deref() != Some(snapshot.selection_ref.as_str()) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
            .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
        (snapshot, record, summary)
    };
    let selection = selection_service::resolve(db, caller, &snapshot.selection_ref)?;
    if selection.asset_ids != snapshot.asset_ids {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let current = {
        let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
            value
                .as_ref()
                .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
                .transpose()
        };
        let scope = PlanScopeSummary {
            scope_ref: ResourceRef::from_trusted(record.scope_ref.clone())
                .map_err(AppError::from)?,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|value| {
                    serde_json::from_value(serde_json::Value::String(value))
                        .map_err(|_| AppError::internal("持久计划模型名损坏"))
                })
                .transpose()?,
        };
        empty_draft(&snapshot, scope, record.details_json.clone(), summary)?
    };
    plans::submit_plan_with_effect(
        db,
        caller,
        &input.plan_id,
        &input.idempotency_key,
        &current,
        |transaction, operation| {
            // 冻结之后新进入回收站的项必须被排除：旧确认不能覆盖后来者。
            if assets::count_trashed(transaction, &snapshot.asset_ids)?
                != snapshot.asset_ids.len() as u64
            {
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
            let total = i64::try_from(snapshot.asset_ids.len())
                .map_err(|_| AppError::internal("清空操作条目数超出范围"))?;
            operations_db::insert_step(
                transaction,
                &NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: "trash.empty.submit".to_string(),
                    total: Some(total),
                    created_at: operation.created_at,
                },
            )?;
            for (ordinal, asset_id) in snapshot.asset_ids.iter().enumerate() {
                operations_db::insert_item(
                    transaction,
                    &NewOperationItem {
                        operation_id: operation.operation_id.clone(),
                        step_id: operation.step_id.clone(),
                        item_id: crate::services::trash::deletion_item_id(*asset_id),
                        ordinal: i64::try_from(ordinal)
                            .map_err(|_| AppError::internal("清空条目序号超出范围"))?,
                        input_fingerprint: None,
                        target_ref: None,
                        created_at: operation.created_at,
                    },
                )?;
            }
            Ok(())
        },
    )
}

/// Load the frozen asset ids of an approved empty plan, in approved order.
pub(crate) fn empty_plan_items(db: &Database, plan_ref: &ResourceRef) -> AppResult<Vec<i64>> {
    let conn = db.lock()?;
    let record = plans_db::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let details_json = record
        .details_json
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let snapshot: EmptyPlanSnapshot = serde_json::from_str(details_json)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    Ok(snapshot.asset_ids)
}

// ---------------------------------------------------------------------------
// A3-03：打标与常用流程适配
//
// 这两个域的能力在服务层是「按计划」的（打标提交后会停在 `awaiting_user_start`，
// 流程提交只写 operation，真正的执行由 GUI/命令层驱动），适配器只做契约翻译，
// 不在这里补一个执行器：模型永远拿不到「让文件动起来」的捷径。
// ---------------------------------------------------------------------------

/// `tagging.prepare`: turn the caller's own selection into a reviewable
/// tagging plan bound to the user's already-bound tagging connection.
pub fn prepare_tagging(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareTaggingInput,
) -> AppResult<PlanPreview> {
    let preparation = crate::services::tagging::prepare_from_selection(
        db,
        caller,
        &input.selection_ref,
        &input.service_ref,
        input.instructions.as_str(),
    )?;
    // 打标准备必然要审批，所以一定带回待审计划；缺失说明服务层契约被改坏了，
    // 不能静默编一个空计划交给模型。
    preparation
        .preview
        .ok_or_else(|| AppError::internal("打标准备没有返回待审计划"))
}

/// `tagging.submit`: consume the GUI approval, freeze the batch and stop at
/// 「待用户开始打标」。外发仍然只能由用户在打标界面显式开始。
pub fn submit_tagging(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::TaggingSubmit, caller).map_err(AppError::from)?;
    let replayed = replay_flag(db, caller, "tagging.submit", &input.idempotency_key)?;
    let operation =
        crate::services::tagging::submit_plan(db, caller, &input.plan_id, &input.idempotency_key)?;
    let operation_ref =
        ResourceRef::from_trusted(operation.operation_id).map_err(AppError::from)?;
    let snapshot = plans::get_operation(db, caller, &operation_ref)?;
    Ok(SubmissionResult {
        schema_version: ProtocolVersion::V1,
        operation: snapshot,
        replayed,
    })
}

/// `workflows.load`: read one stored workflow version as the contract summary.
pub fn load_workflow(
    db: &Database,
    caller: &CallerContext,
    input: &LoadWorkflowInput,
) -> AppResult<WorkflowSummary> {
    check_capability_entry(CapabilityId::WorkflowsLoad, caller).map_err(AppError::from)?;
    crate::services::workflows::contract_summary(
        db,
        input.workflow_ref.as_str(),
        i64::from(input.version),
    )
}

/// `workflows.prepare`: freeze the workflow instance into one reviewable plan.
pub fn prepare_workflow(
    db: &Database,
    caller: &CallerContext,
    input: &PrepareWorkflowInput,
) -> AppResult<WorkflowPlanPreview> {
    check_capability_entry(CapabilityId::WorkflowsPrepare, caller).map_err(AppError::from)?;
    let inputs = workflow_run::WorkflowInstanceInputs {
        source_ref: optional_resource_str(&input.inputs.source_ref),
        selection_ref: optional_resource_str(&input.inputs.selection_ref),
        collection_ref: optional_resource_str(&input.inputs.collection_ref),
        destination_ref: optional_resource_str(&input.inputs.destination_ref),
        service_ref: optional_resource_str(&input.inputs.service_ref),
        tagging_instructions: input
            .inputs
            .tagging_instructions
            .as_ref()
            .map(|value| value.as_str().to_string()),
        rename_pattern: optional_short_str(&input.inputs.rename_pattern),
    };
    let view = workflow_run::prepare_instance(
        db,
        caller,
        input.workflow_ref.as_str(),
        i64::from(input.version),
        &inputs,
    )?;
    let preview = {
        let conn = db.lock()?;
        let record = plans_db::plan(&conn, &view.plan_id)?
            .ok_or(AppError::from(AgentErrorCode::ReferenceExpired))?;
        plans::plan_preview(&conn, &record)?
    };
    let mut steps = Vec::with_capacity(view.steps.len());
    for step in &view.steps {
        let capability: CapabilityId =
            serde_json::from_value(serde_json::Value::String(step.capability.clone()))
                .map_err(|_| AppError::internal("流程步骤能力名损坏"))?;
        steps.push(WorkflowStepSummary {
            capability,
            depends_on: step.depends_on,
            review_kind: plans::review_kind_for_capability(capability)?,
        });
    }
    Ok(WorkflowPlanPreview {
        schema_version: ProtocolVersion::V1,
        workflow_ref: input.workflow_ref.clone(),
        version: input.version,
        plan: preview,
        steps,
    })
}

/// `workflows.submit`: consume the GUI approval and freeze the operation.
///
/// 执行仍由流程运行器驱动；这里绝不调用 `execute_instance`。
pub fn submit_workflow(
    db: &Database,
    caller: &CallerContext,
    input: &SubmitPlanInput,
) -> AppResult<SubmissionResult> {
    check_capability_entry(CapabilityId::WorkflowsSubmit, caller).map_err(AppError::from)?;
    let replayed = replay_flag(db, caller, "workflows.submit", &input.idempotency_key)?;
    let submitted = workflow_run::submit_instance_after_approval(
        db,
        caller,
        input.plan_id.as_str(),
        &input.idempotency_key,
    )?;
    let snapshot = plans::get_operation(db, caller, &submitted.operation_ref)?;
    Ok(SubmissionResult {
        schema_version: ProtocolVersion::V1,
        operation: snapshot,
        replayed,
    })
}

/// `Option<ResourceRef>` → the plain string the domain services expect.
fn optional_resource_str(value: &Option<ResourceRef>) -> Option<String> {
    value.as_ref().map(|value| value.as_str().to_string())
}

/// `Option<ShortText>` → plain string (workflow inputs are untyped strings).
fn optional_short_str(value: &Option<ShortText>) -> Option<String> {
    value.as_ref().map(|value| value.as_str().to_string())
}

/// Whether this idempotency key already produced an operation for the caller.
///
/// `plans::submit_plan_with_effect` decides replay/conflict under the write lock;
/// this read only tells the contract's `SubmissionResult.replayed` flag apart from
/// a first submission. A concurrent double-submit with the same key may report
/// `replayed: false` on one side; the effect itself is still applied once.
fn replay_flag(
    db: &Database,
    caller: &CallerContext,
    capability: &str,
    idempotency_key: &ResourceRef,
) -> AppResult<bool> {
    let conn = db.lock()?;
    Ok(plans_db::operation_by_idempotency(
        &conn,
        caller.principal_id.as_str(),
        caller.library.library_epoch.as_str(),
        capability,
        idempotency_key.as_str(),
    )?
    .is_some())
}

/// Truncate on a UTF-8 boundary before handing a file name to the model contract.
fn short_text(value: &str) -> AppResult<ShortText> {
    let mut end = value.len().min(SHORT_TEXT_MAX_BYTES);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::from_value(serde_json::Value::String(value[..end].to_string()))
        .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::agent::Instructions;
    use crate::db::libraries;
    use crate::db::{init_memory, selections};
    use crate::error::AppError;
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use crate::utils::file_identity::file_identity;
    use std::fs::File;
    use std::time::UNIX_EPOCH;

    fn seeded_library(names: &[&str]) -> (tempfile::TempDir, Database, Vec<i64>) {
        let root = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let library = managed_library::adopt_initial_root(&conn, root.path()).unwrap();
        let mut ids = Vec::new();
        for name in names {
            let path = root.path().join(name);
            std::fs::write(&path, format!("fixture {name}")).unwrap();
            let modified_at = std::fs::metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let id = assets::insert(
                &conn,
                path.to_string_lossy().as_ref(),
                name,
                "jpg",
                std::fs::metadata(&path).unwrap().len() as i64,
                "image/jpeg",
                modified_at,
            )
            .unwrap();
            let candidate = libraries::pending_page(&conn, 0, 100)
                .unwrap()
                .into_iter()
                .find(|item| item.asset_id == id)
                .unwrap();
            let identity = {
                let file = File::open(&path).unwrap();
                file_identity(&file).unwrap()
            };
            assert!(libraries::record_verified(
                &conn,
                &library,
                &candidate,
                name,
                &identity,
                &"a".repeat(64),
                "fixture-receipt",
            )
            .unwrap());
            ids.push(id);
        }
        (root, Database::new(conn), ids)
    }

    fn pi_caller(db: &Database, principal: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Pi {
                principal_id: principal.to_string(),
                session_id: "session_agent_tools_fixture".to_string(),
                instance_id: "instance_agent_tools_fixture".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn search_input(text: &str) -> SearchAssetsInput {
        SearchAssetsInput {
            schema_version: ProtocolVersion::V1,
            text: serde_json::from_value(serde_json::Value::String(text.to_string())).unwrap(),
            cursor: None,
        }
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    fn trashed_count(db: &Database, ids: &[i64]) -> u64 {
        let conn = db.lock().unwrap();
        assets::count_trashed(&conn, ids).unwrap()
    }

    fn active_count(db: &Database, ids: &[i64]) -> u64 {
        let conn = db.lock().unwrap();
        assets::count_active(&conn, ids).unwrap()
    }

    #[test]
    fn assets_search_returns_a_page_and_persists_the_exact_selection() {
        let (_root, db, ids) =
            seeded_library(&["photo_one.jpg", "photo_two.jpg", "fixture_note.jpg"]);
        let caller = pi_caller(&db, "principal_pi_search");
        let page = search_assets(&db, &caller, &search_input("photo")).unwrap();

        assert_eq!(page.total, 2);
        assert!(!page.truncated);
        assert_eq!(page.items.len(), 2);
        let mut names: Vec<&str> = page
            .items
            .iter()
            .map(|item| item.display_name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, vec!["photo_one.jpg", "photo_two.jpg"]);
        for item in &page.items {
            assert!(item.asset_ref.as_str().starts_with("asset_"));
        }
        assert_eq!(page.library, caller.library);

        let selection =
            crate::services::selections::resolve(&db, &caller, &page.selection_ref).unwrap();
        let mut selected = selection.asset_ids.clone();
        selected.sort_unstable();
        let mut expected = vec![ids[0], ids[1]];
        expected.sort_unstable();
        assert_eq!(selected, expected);
        assert_eq!(selection.total, 2);
        assert!(!selection.truncated);
    }

    #[test]
    fn selection_is_bound_to_its_principal() {
        let (_root, db, _ids) = seeded_library(&["photo_one.jpg"]);
        let owner = pi_caller(&db, "principal_pi_owner");
        let page = search_assets(&db, &owner, &search_input("photo")).unwrap();
        let other = pi_caller(&db, "principal_pi_other");

        let error = crate::services::selections::resolve(&db, &other, &page.selection_ref)
            .expect_err("another principal must not reuse the selection");
        assert_eq!(error_code(error), "UNAUTHORIZED");
    }

    #[test]
    fn expired_selection_is_rejected() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_expiry");
        let expired = {
            let conn = db.lock().unwrap();
            let reference = ResourceRef::generated("selection");
            selections::insert(
                &conn,
                &selections::NewSelection {
                    selection_ref: reference.as_str(),
                    principal_id: caller.principal_id.as_str(),
                    library_id: caller.library.library_id.as_str(),
                    library_epoch: caller.library.library_epoch.as_str(),
                    query: "photo",
                    asset_ids: &ids,
                    total: 1,
                    truncated: false,
                    created_at: 1,
                    expires_at: 2,
                },
            )
            .unwrap();
            reference
        };
        let error = crate::services::selections::resolve(&db, &caller, &expired)
            .expect_err("expired selections must fail closed");
        assert_eq!(error_code(error), "REFERENCE_EXPIRED");
    }

    #[test]
    fn pagination_cursor_is_explicitly_unsupported() {
        let (_root, db, _ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_cursor");
        let mut input = search_input("photo");
        input.cursor = Some(ResourceRef::generated("cursor"));
        let error = search_assets(&db, &caller, &input)
            .expect_err("unimplemented pagination must not return the first page again");
        assert_eq!(error_code(error), "UNSUPPORTED_CAPABILITY");
    }

    #[test]
    fn instructions_above_the_contract_limit_are_rejected_before_search() {
        let instructions: Result<Instructions, _> =
            serde_json::from_value(serde_json::Value::String("x".repeat(4097)));
        assert!(instructions.is_err());
    }

    fn owned_operation(db: &Database, caller: &CallerContext, state: &str) -> ResourceRef {
        let operation_id = ResourceRef::generated("operation");
        let plan_id = format!(
            "plan_{}",
            operation_id.as_str().trim_start_matches("operation_")
        );
        let conn = db.lock().unwrap();
        crate::db::plans::insert_plan(
            &conn,
            &crate::db::plans::NewPlan {
                plan_id: plan_id.clone(),
                principal_id: caller.principal_id.as_str().to_string(),
                session_id: caller.session_id.as_ref().map(|id| id.as_str().to_string()),
                instance_id: caller.instance_id.as_str().to_string(),
                capability: "imports.submit".to_string(),
                review_kind: "ordinary".to_string(),
                review_ref: format!("review_{}", operation_id.as_str()),
                plan_hash: "a".repeat(64),
                scope_ref: "scope_fixture".to_string(),
                source_ref: None,
                selection_ref: None,
                target_ref: None,
                service_ref: None,
                service_revision: None,
                model: None,
                item_count: 1,
                summary: "导入计划".to_string(),
                details_json: None,
                max_requests: 0,
                max_tokens: 0,
                max_seconds: 0,
                max_items: 1,
                currency: None,
                amount_microunits: None,
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
                grant_epoch: caller.grant_epoch.as_str().to_string(),
                created_at: 100,
                expires_at: 10_000,
                origin_kind: Some("gui".to_string()),
            },
        )
        .unwrap();
        crate::db::plans::insert_operation(
            &conn,
            &crate::db::plans::NewOperation {
                operation_id: operation_id.as_str().to_string(),
                step_id: "step_fixture".to_string(),
                plan_id,
                principal_id: caller.principal_id.as_str().to_string(),
                capability: "imports.submit".to_string(),
                idempotency_key: "request_fixture".to_string(),
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
                state: state.to_string(),
                phase: "ready".to_string(),
                revision: 1,
                total: Some(1),
                completed: 0,
                failed: 0,
                created_at: 100,
            },
        )
        .unwrap();
        operation_id
    }

    #[test]
    fn operations_get_returns_the_owners_operation_and_hides_foreign_ones() {
        let (_root, db, _ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_ops");
        let operation = owned_operation(&db, &caller, "queued");

        let snapshot = get_operation(&db, &caller, &operation).unwrap();
        assert_eq!(snapshot.operation_id, operation);
        assert_eq!(
            snapshot.state,
            crate::contracts::agent::OperationState::Queued
        );
        assert!(snapshot.can_cancel);
        assert!(!snapshot.can_resume);

        let other = pi_caller(&db, "principal_pi_ops_other");
        let error = get_operation(&db, &other, &operation).unwrap_err();
        assert_eq!(error_code(error), "UNAUTHORIZED");
    }

    #[test]
    fn operations_cancel_queued_operation_settles_as_cancelled() {
        let (_root, db, _ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_cancel");
        let operation = owned_operation(&db, &caller, "queued");

        let snapshot = cancel_operation(&db, &caller, &operation).unwrap();
        assert_eq!(
            snapshot.state,
            crate::contracts::agent::OperationState::Cancelled
        );
        assert!(!snapshot.can_cancel);
    }

    #[test]
    fn unknown_operation_references_fail_closed() {
        let (_root, db, _ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_missing_operation");
        let missing = ResourceRef::generated("operation");

        let error = get_operation(&db, &caller, &missing).unwrap_err();
        assert_eq!(error_code(error), "REFERENCE_EXPIRED");
        let error = cancel_operation(&db, &caller, &missing).unwrap_err();
        assert_eq!(error_code(error), "REFERENCE_EXPIRED");
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_agent_tools_gui".to_string(),
                instance_id: "instance_agent_tools_gui".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn prepare_input(selection_ref: &ResourceRef) -> PrepareSelectionInput {
        PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection_ref.clone(),
        }
    }

    #[test]
    fn trash_prepare_only_plans_and_submit_requires_gui_approval() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg", "note.jpg"]);
        let caller = pi_caller(&db, "principal_pi_trash");
        let gui = gui_caller(&db);
        let page = search_assets(&db, &caller, &search_input("photo")).unwrap();

        let preview = prepare_trash(&db, &caller, &prepare_input(&page.selection_ref)).unwrap();
        assert_eq!(preview.capability, CapabilityId::AssetsTrashSubmit);
        assert_eq!(preview.item_count, 2);
        assert_eq!(
            preview.scope.selection_ref.as_ref(),
            Some(&page.selection_ref)
        );
        let active = {
            let conn = db.lock().unwrap();
            assets::count_active(&conn, &ids).unwrap()
        };
        assert_eq!(active, 3, "preparing must not delete anything");

        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error =
            submit_trash(&db, &caller, &submit).expect_err("submitting without approval must fail");
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        let result = submit_trash(&db, &caller, &submit).unwrap();
        assert!(!result.replayed);
        assert_eq!(
            result.operation.state,
            crate::contracts::agent::OperationState::Succeeded
        );
        assert!(!result.operation.can_cancel);
        let active = {
            let conn = db.lock().unwrap();
            assets::count_active(&conn, &ids).unwrap()
        };
        assert_eq!(
            active, 1,
            "only the two searched assets leave the active set"
        );

        let replay = submit_trash(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id, result.operation.operation_id);
    }

    #[test]
    fn trash_submit_rejects_a_foreign_principal_and_a_changed_selection() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = pi_caller(&db, "principal_pi_trash_owner");
        let other = pi_caller(&db, "principal_pi_trash_other");
        let gui = gui_caller(&db);
        let page = search_assets(&db, &caller, &search_input("photo")).unwrap();
        let preview = prepare_trash(&db, &caller, &prepare_input(&page.selection_ref)).unwrap();
        plans::approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error = submit_trash(&db, &other, &submit).unwrap_err();
        assert_eq!(error_code(error), "UNAUTHORIZED");

        // One approved asset leaves the active set before submit: the exact set
        // changed, so the plan must fail closed instead of trashing a subset.
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &[ids[0]]).unwrap();
        }
        let error = submit_trash(&db, &caller, &submit).unwrap_err();
        assert_eq!(error_code(error), "PLAN_CHANGED");
    }

    #[test]
    fn trash_prepare_rejects_selections_with_inactive_assets() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_trash_stale");
        let page = search_assets(&db, &caller, &search_input("photo")).unwrap();
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
        }
        let error = prepare_trash(&db, &caller, &prepare_input(&page.selection_ref)).unwrap_err();
        assert_eq!(error_code(error), "PLAN_CHANGED");
    }

    #[test]
    fn trash_prepare_rejects_truncated_selections() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = pi_caller(&db, "principal_pi_trash_truncated");
        let truncated = {
            let conn = db.lock().unwrap();
            let reference = ResourceRef::generated("selection");
            selections::insert(
                &conn,
                &selections::NewSelection {
                    selection_ref: reference.as_str(),
                    principal_id: caller.principal_id.as_str(),
                    library_id: caller.library.library_id.as_str(),
                    library_epoch: caller.library.library_epoch.as_str(),
                    query: "photo",
                    asset_ids: &ids,
                    total: 100_000,
                    truncated: true,
                    created_at: 1,
                    expires_at: i64::MAX,
                },
            )
            .unwrap();
            reference
        };
        let error = prepare_trash(&db, &caller, &prepare_input(&truncated)).unwrap_err();
        assert_eq!(error_code(error), "INVALID_INPUT");
    }

    // ---- A4-01：导出走「复制 + 计划 + 收据」，不接受裸目标路径 ----

    /// 通过真实授权链给导出目标目录发一个句柄。导出命令不接受前端传来的裸路径，
    /// `prepare_export` 与 `submit_export` 都在评审期解析这个句柄。
    fn export_directory_ref(
        db: &Database,
        caller: &CallerContext,
        dir: &std::path::Path,
    ) -> ResourceRef {
        let view = authorization::grant_selected_directory(
            db,
            caller,
            dir,
            authorization::DirectoryPurpose::ExportDestination,
            false,
        )
        .unwrap();
        view.directory_ref.expect("一次授权必须返回可用的目录句柄")
    }

    fn export_input(
        selection_ref: &ResourceRef,
        destination_ref: &ResourceRef,
        layout: ExportLayout,
    ) -> PrepareExportInput {
        PrepareExportInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection_ref.clone(),
            destination_ref: destination_ref.clone(),
            layout,
        }
    }

    #[test]
    fn export_prepare_only_plans_and_submit_requires_gui_approval() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let selection =
            selection_service::store(&db, &caller, "导出选中素材", &ids, ids.len() as i64).unwrap();

        let preview = prepare_export(
            &db,
            &caller,
            &export_input(&selection.selection_ref, &destination, ExportLayout::Flat),
        )
        .unwrap();
        assert_eq!(preview.capability, CapabilityId::ExportsSubmit);
        assert_eq!(preview.item_count, 2);
        assert_eq!(
            preview.scope.selection_ref.as_ref(),
            Some(&selection.selection_ref)
        );
        assert_eq!(preview.scope.target_ref.as_ref(), Some(&destination));
        // 准备阶段绝不能写文件：目标目录仍是空的
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);

        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error = submit_export(&db, &caller, &submit)
            .expect_err("submitting without approval must fail");
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let result = submit_export(&db, &caller, &submit).unwrap();
        assert!(!result.replayed);
        // 提交只登记逐文件条目，不在这里结算——真正复制由执行侧完成
        assert_eq!(
            result.operation.state,
            crate::contracts::agent::OperationState::Queued
        );
        let operation_id = result.operation.operation_id.as_str().to_string();
        let (items, steps) = {
            let conn = db.lock().unwrap();
            let steps = operations_db::steps(&conn, &operation_id).unwrap();
            let step_id = steps[0].step_id.clone();
            (
                operations_db::items(&conn, &operation_id, &step_id).unwrap(),
                steps,
            )
        };
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].capability, "exports.submit");
        assert_eq!(steps[0].total, Some(2));
        assert_eq!(items.len(), 2);
        let mut item_ids: Vec<&str> = items.iter().map(|item| item.item_id.as_str()).collect();
        item_ids.sort_unstable();
        let mut expected: Vec<String> = ids.iter().map(|id| export_item_id(*id)).collect();
        expected.sort();
        assert_eq!(item_ids, expected);
        assert!(items
            .iter()
            .all(|item| item.target_ref.as_deref() == Some(destination.as_str())));
        assert!(items.iter().all(|item| item.state == "pending"));

        // 计划与提交阶段都不碰库内素材与磁盘
        let active = {
            let conn = db.lock().unwrap();
            assets::count_active(&conn, &ids).unwrap()
        };
        assert_eq!(active, 2);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);

        let replay = submit_export(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id, result.operation.operation_id);
    }

    #[test]
    fn export_submit_rejects_a_selection_that_left_the_library() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let selection =
            selection_service::store(&db, &caller, "导出选中素材", &ids, ids.len() as i64).unwrap();
        let preview = prepare_export(
            &db,
            &caller,
            &export_input(&selection.selection_ref, &destination, ExportLayout::Flat),
        )
        .unwrap();
        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };

        // 一个已批准素材在提交前离开活动集：集合不再精确相等，必须失败关闭
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &[ids[0]]).unwrap();
        }
        let error = submit_export(&db, &caller, &submit).unwrap_err();
        assert_eq!(error_code(error), "PLAN_CHANGED");
    }

    #[test]
    fn export_prepare_requires_an_authorized_export_destination() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let selection =
            selection_service::store(&db, &caller, "导出选中素材", &ids, ids.len() as i64).unwrap();

        // 只读的「导入来源」授权不能被当成导出目标：用途与写/导出访问位都要拦住。
        let read_only = authorization::grant_selected_directory(
            &db,
            &caller,
            &out,
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap()
        .directory_ref
        .expect("一次授权必须返回可用的目录句柄");
        let error = prepare_export(
            &db,
            &caller,
            &export_input(&selection.selection_ref, &read_only, ExportLayout::Flat),
        )
        .unwrap_err();
        assert_eq!(error_code(error), "UNAUTHORIZED");

        // 授权被撤销后，同一个目录句柄必须立刻失效（代次前移 → GRANT_REVOKED）。
        let destination = export_directory_ref(&db, &caller, &out);
        {
            let conn = db.lock().unwrap();
            crate::db::authorization::revoke_all(&conn, caller.principal_id.as_str(), 1).unwrap();
        }
        let error = prepare_export(
            &db,
            &caller,
            &export_input(&selection.selection_ref, &destination, ExportLayout::Flat),
        )
        .unwrap_err();
        assert_eq!(error_code(error), "GRANT_REVOKED");
    }

    #[test]
    fn export_plan_summary_states_copy_and_keeps_the_source() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let selection =
            selection_service::store(&db, &caller, "导出选中素材", &ids, ids.len() as i64).unwrap();
        let preview = prepare_export(
            &db,
            &caller,
            &export_input(&selection.selection_ref, &destination, ExportLayout::ByDate),
        )
        .unwrap();
        // 计划文案必须明确是「复制导出」、写明目标位置，并说明库内文件与记录保留
        let summary = preview.summary.as_str();
        assert!(summary.contains("复制导出"), "{summary}");
        assert!(summary.contains("库内原文件与记录保留"), "{summary}");
        assert!(summary.contains("按拍摄月份分目录"), "{summary}");
        // 目标目录的真实可读路径要进摘要，否则审核者无法核对「复制到哪儿」
        assert!(summary.contains("out"), "{summary}");
        // Windows canonicalize 会带 `\\?\` 前缀；给人看的形式不能出现它
        assert!(!summary.contains("\\\\?\\"), "{summary}");
    }

    // ---- A4-02：库内分库移动走同一套「计划 + 收据」，分库引用带总仓代次 ----

    fn move_collection_ref(db: &Database, relative_path: &str) -> ResourceRef {
        let conn = db.lock().unwrap();
        let library = libraries::current(&conn).unwrap().unwrap();
        let stored = crate::db::collections::ensure(
            &conn,
            &library.library_id,
            &library.library_epoch,
            relative_path,
            1,
        )
        .unwrap();
        ResourceRef::from_trusted(stored.collection_ref).unwrap()
    }

    fn move_input(selection_ref: &ResourceRef, collection_ref: &ResourceRef) -> PrepareMoveInput {
        PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection_ref.clone(),
            collection_ref: collection_ref.clone(),
        }
    }

    fn submit_input(plan_id: &ResourceRef) -> SubmitPlanInput {
        SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: plan_id.clone(),
            idempotency_key: ResourceRef::generated("idempotency"),
        }
    }

    #[test]
    fn move_prepare_freezes_targets_and_keeps_identifiers_after_execution() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        let tag = {
            let conn = db.lock().unwrap();
            let tag = crate::db::tags::create(&conn, "测试标签", None).unwrap();
            crate::db::asset_tags::assign(&conn, &ids, &[tag.id], "manual").unwrap();
            tag
        };
        let collection = move_collection_ref(&db, "2024/旅行");
        let selection =
            selection_service::store(&db, &caller, "移动到分库", &ids, ids.len() as i64).unwrap();

        let preview = prepare_move(
            &db,
            &caller,
            &move_input(&selection.selection_ref, &collection),
        )
        .unwrap();
        assert_eq!(preview.capability, CapabilityId::AssetsMoveSubmit);
        assert_eq!(preview.item_count, 2);
        assert_eq!(preview.scope.target_ref.as_ref(), Some(&collection));
        // 计划阶段绝不搬文件：目标目录都还不存在
        assert!(!root.path().join("2024").exists());
        let summary = preview.summary.as_str();
        assert!(summary.contains("分库「2024/旅行」"), "{summary}");
        assert!(summary.contains("标签与历史保留"), "{summary}");

        let submit = submit_input(&preview.plan_id);
        let error =
            submit_move(&db, &caller, &submit).expect_err("submitting without approval must fail");
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let result = submit_move(&db, &caller, &submit).unwrap();
        assert!(!result.replayed);
        let operation_id = result.operation.operation_id.as_str().to_string();
        let step_ref = {
            let conn = db.lock().unwrap();
            let steps = operations_db::steps(&conn, &operation_id).unwrap();
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].capability, "assets.move.submit");
            assert_eq!(steps[0].total, Some(2));
            ResourceRef::from_trusted(steps[0].step_id.clone()).unwrap()
        };

        // 执行阶段只搬计划里冻结的那些目标路径
        let items = move_plan_items(&db, &preview.plan_id).unwrap();
        assert_eq!(items.len(), 2);
        let operation_ref = ResourceRef::from_trusted(operation_id.clone()).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let moved = library_move::execute(
            &db,
            &caller,
            &library,
            library_move::MoveExecution {
                operation_id: &operation_id,
                operation_ref: &operation_ref,
                step_ref: &step_ref,
                items: &items,
                cancelled: &cancelled,
            },
        )
        .unwrap();
        assert_eq!(moved.moved, 2);
        assert_eq!(moved.failed, 0);

        // 同一素材 ID、同一标签：移动不是重新入库
        let (active, names, tag_ids) = {
            let conn = db.lock().unwrap();
            let assets_now = assets::get_managed(&conn, ids[0]).unwrap();
            let tags_now = crate::db::asset_tags::get_asset_tags(&conn, ids[0]).unwrap();
            (
                assets::count_active(&conn, &ids).unwrap(),
                assets_now.file_path.clone(),
                tags_now.into_iter().map(|tag| tag.id).collect::<Vec<_>>(),
            )
        };
        assert_eq!(active, 2);
        assert!(names.contains("2024/旅行"), "{names}");
        assert_eq!(tag_ids, vec![tag.id]);
        // 源位置已经空了，目标位置有真文件
        assert!(!root.path().join("photo_one.jpg").exists());
        assert!(root.path().join("2024/旅行/photo_one.jpg").exists());

        let journal = {
            let conn = db.lock().unwrap();
            crate::db::move_journal::entries(&conn, &operation_id).unwrap()
        };
        assert_eq!(journal.len(), 2);
        assert!(journal
            .iter()
            .all(|entry| entry.state == crate::db::move_journal::STATE_COMMITTED));
        // P0-2：源身份必须在执行时冻结并写入日志，不能为空串。
        assert!(
            journal
                .iter()
                .all(|entry| !entry.source_identity.is_empty()),
            "移动日志的 source_identity 不得为空"
        );
        // 目标身份必须与冻结的源身份不同（rename 不改变文件身份，但二者都是真实值）
        assert!(journal.iter().all(|entry| entry
            .target_identity
            .as_deref()
            .is_some_and(|t| !t.is_empty())));

        let replay = submit_move(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id, result.operation.operation_id);
    }

    #[test]
    fn move_targets_never_escape_the_managed_root() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = gui_caller(&db);
        let selection =
            selection_service::store(&db, &caller, "移动到分库", &ids, ids.len() as i64).unwrap();

        // 未登记的分库引用无法解析（引用由 library_id + epoch + 相对路径派生）
        let unknown = ResourceRef::generated("collection");
        let error = prepare_move(
            &db,
            &caller,
            &move_input(&selection.selection_ref, &unknown),
        )
        .unwrap_err();
        assert_eq!(error_code(error), "REFERENCE_EXPIRED");

        // 保留目录（.teacell-library-id 等）不允许作为移动目标
        let error = {
            let conn = db.lock().unwrap();
            let library = libraries::current(&conn).unwrap().unwrap();
            crate::db::collections::ensure(
                &conn,
                &library.library_id,
                &library.library_epoch,
                ".import-stage",
                1,
            )
            .unwrap();
            let stored = crate::db::collections::ensure(
                &conn,
                &library.library_id,
                &library.library_epoch,
                ".import-stage",
                1,
            )
            .unwrap();
            let reference = ResourceRef::from_trusted(stored.collection_ref).unwrap();
            drop(conn);
            prepare_move(
                &db,
                &caller,
                &move_input(&selection.selection_ref, &reference),
            )
            .unwrap_err()
        };
        assert_eq!(error_code(error), "CONFLICT");
    }

    #[test]
    fn move_submit_rejects_a_selection_that_left_the_library() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        let collection = move_collection_ref(&db, "归档");
        let selection =
            selection_service::store(&db, &caller, "移动到分库", &ids, ids.len() as i64).unwrap();
        let preview = prepare_move(
            &db,
            &caller,
            &move_input(&selection.selection_ref, &collection),
        )
        .unwrap();
        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &[ids[0]]).unwrap();
        }
        let error = submit_move(&db, &caller, &submit_input(&preview.plan_id)).unwrap_err();
        assert_eq!(error_code(error), "PLAN_CHANGED");
    }

    fn trashed_selection(db: &Database, caller: &CallerContext, ids: &[i64]) -> ResourceRef {
        selection_service::store(db, caller, "从回收站恢复", ids, ids.len() as i64)
            .unwrap()
            .selection_ref
    }

    fn restore_operation(
        db: &Database,
        caller: &CallerContext,
        submit: &SubmitPlanInput,
    ) -> (String, ResourceRef) {
        let result = submit_restore(db, caller, submit).unwrap();
        let operation_id = result.operation.operation_id.as_str().to_string();
        let step_ref = {
            let conn = db.lock().unwrap();
            let steps = operations_db::steps(&conn, &operation_id).unwrap();
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].capability, "assets.restore.submit");
            ResourceRef::from_trusted(steps[0].step_id.clone()).unwrap()
        };
        (operation_id, step_ref)
    }

    fn run_restore(
        db: &Database,
        caller: &CallerContext,
        operation_id: &str,
        step_ref: &ResourceRef,
        asset_ids: &[i64],
    ) -> crate::services::trash::RestoreSummary {
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let operation_ref = ResourceRef::from_trusted(operation_id.to_string()).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        crate::services::trash::execute(
            db,
            caller,
            &library,
            crate::services::trash::RestoreExecution {
                operation_id,
                operation_ref: &operation_ref,
                step_ref,
                asset_ids,
                cancelled: &cancelled,
            },
        )
        .unwrap()
    }

    #[test]
    fn trash_plan_summary_lists_names_and_the_cleanup_date() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        // R45：默认保留 30 天，摘要里的预计清理时间必须按这个天数真实算出。
        let retention_days = {
            let conn = db.lock().unwrap();
            crate::db::settings::get_settings(&conn)
                .unwrap()
                .trash_retention_days
        };
        assert_eq!(retention_days, 30, "回收站默认保留天数必须是 30 天");

        let selection =
            selection_service::store(&db, &caller, "移入回收站", &ids, ids.len() as i64).unwrap();
        let today_before = chrono::Local::now().date_naive();
        let preview =
            prepare_trash(&db, &caller, &prepare_input(&selection.selection_ref)).unwrap();
        let today_after = chrono::Local::now().date_naive();
        let summary = preview.summary.as_str();
        assert!(summary.contains("photo_one.jpg"), "{summary}");
        assert!(summary.contains("自动清理"), "{summary}");
        assert!(summary.contains("期间可恢复"), "{summary}");

        // 摘要必须写出具体日期（YYYY-MM-DD），而不只是「即将自动清理」。
        let date_text = summary
            .split("预计 ")
            .nth(1)
            .and_then(|rest| rest.split(" 自动清理").next())
            .unwrap_or_else(|| panic!("摘要里找不到「预计 <日期> 自动清理」：{summary}"));
        let parsed = chrono::NaiveDate::parse_from_str(date_text, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("预计清理日期不是 YYYY-MM-DD：{date_text} / {summary}"));
        // 日期 == 本地今天 + 保留天数；跨本地午夜执行时取执行前后两个允许值。
        let allowed = [
            today_before + chrono::Duration::days(retention_days),
            today_after + chrono::Duration::days(retention_days),
        ];
        assert!(
            allowed.contains(&parsed),
            "预计清理日期 {parsed} 不等于「今天 + {retention_days} 天」{allowed:?}：{summary}"
        );

        // 摘要参与 plan_hash 并被持久化：库里读回的计划摘要与预览逐字一致，
        // 证明界面显示的预计时间就是提交时复读的那一份，不会随时间前进被重算。
        let stored_summary = {
            let conn = db.lock().unwrap();
            crate::db::plans::plan(&conn, preview.plan_id.as_str())
                .unwrap()
                .unwrap()
                .summary
        };
        assert_eq!(stored_summary, summary.to_string());
    }

    #[test]
    fn restore_prepare_only_plans_and_execution_keeps_identifiers() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        let tag = {
            let conn = db.lock().unwrap();
            let tag = crate::db::tags::create(&conn, "恢复标签", None).unwrap();
            crate::db::asset_tags::assign(&conn, &ids, &[tag.id], "manual").unwrap();
            tag
        };
        {
            let conn = db.lock().unwrap();
            assert_eq!(assets::soft_delete(&conn, &ids).unwrap(), 2);
        }
        let selection = trashed_selection(&db, &caller, &ids);
        let preview = prepare_restore(&db, &caller, &prepare_input(&selection)).unwrap();
        assert_eq!(preview.capability, CapabilityId::AssetsRestoreSubmit);
        assert_eq!(preview.item_count, 2);
        assert_eq!(preview.scope.selection_ref.as_ref(), Some(&selection));
        let summary = preview.summary.as_str();
        assert!(summary.contains("photo_one.jpg"), "{summary}");
        assert!(summary.contains("不会假装恢复成功"), "{summary}");
        // 准备阶段不恢复任何东西
        assert_eq!(trashed_count(&db, &ids), 2);

        let submit = submit_input(&preview.plan_id);
        let error = submit_restore(&db, &caller, &submit)
            .expect_err("submitting without approval must fail");
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let (operation_id, step_ref) = restore_operation(&db, &caller, &submit);
        // 提交只登记逐项条目，deleted_at 还没被清
        assert_eq!(trashed_count(&db, &ids), 2);
        assert_eq!(active_count(&db, &ids), 0);
        let frozen = restore_plan_items(&db, &preview.plan_id).unwrap();
        assert_eq!(frozen, ids);

        let restored = run_restore(&db, &caller, &operation_id, &step_ref, &frozen);
        assert_eq!(restored.restored, 2);
        assert_eq!(restored.failed, 0);
        assert_eq!(trashed_count(&db, &ids), 0);
        assert_eq!(active_count(&db, &ids), 2);

        // 同一素材 ID、同一标签：恢复不是重新入库
        let tag_ids = {
            let conn = db.lock().unwrap();
            crate::db::asset_tags::get_asset_tags(&conn, ids[0])
                .unwrap()
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(tag_ids, vec![tag.id]);

        // 幂等重放：同一份已批准计划 + 同一幂等键必须返回原操作，
        // 而不是因为「集合已不在回收站」被误判成计划改变。
        let replay = submit_restore(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id.as_str(), operation_id);
    }

    #[test]
    fn restore_reports_a_missing_file_instead_of_claiming_success() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let caller = gui_caller(&db);
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
        }
        let selection = trashed_selection(&db, &caller, &ids);
        let preview = prepare_restore(&db, &caller, &prepare_input(&selection)).unwrap();
        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let submit = submit_input(&preview.plan_id);
        let (operation_id, step_ref) = restore_operation(&db, &caller, &submit);

        // 文件在计划之后消失：恢复必须报可修复异常，而不是清掉 deleted_at 假装成功
        std::fs::remove_file(root.path().join("photo_one.jpg")).unwrap();
        let restored = run_restore(&db, &caller, &operation_id, &step_ref, &ids);
        assert_eq!(restored.restored, 0);
        assert_eq!(restored.failed, 1);
        assert_eq!(trashed_count(&db, &ids), 1, "记录仍留在回收站");
        assert_eq!(active_count(&db, &ids), 0);

        let (state, error) = {
            let conn = db.lock().unwrap();
            let items = operations_db::items(&conn, &operation_id, step_ref.as_str()).unwrap();
            assert_eq!(items.len(), 1);
            (items[0].state.clone(), items[0].error_code.clone())
        };
        assert_eq!(state, "failed");
        assert_eq!(error.as_deref(), Some("FILE_MISSING"));
    }

    // ---- A4-04：独立清空与受控到期清理 -------------------------------------

    fn empty_operation(
        db: &Database,
        caller: &CallerContext,
        submit: &SubmitPlanInput,
    ) -> (String, ResourceRef) {
        let result = submit_empty(db, caller, submit).unwrap();
        let operation_id = result.operation.operation_id.as_str().to_string();
        let step_ref = {
            let conn = db.lock().unwrap();
            let steps = operations_db::steps(&conn, &operation_id).unwrap();
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].capability, "trash.empty.submit");
            ResourceRef::from_trusted(steps[0].step_id.clone()).unwrap()
        };
        (operation_id, step_ref)
    }

    fn run_empty(
        db: &Database,
        caller: &CallerContext,
        operation_id: &str,
        step_ref: &ResourceRef,
        asset_ids: &[i64],
    ) -> crate::services::trash::DeletionSummary {
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let operation_ref = ResourceRef::from_trusted(operation_id.to_string()).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        crate::services::trash::empty(
            db,
            caller,
            &library,
            crate::services::trash::EmptyExecution {
                operation_id,
                operation_ref: &operation_ref,
                step_ref,
                asset_ids,
                cancelled: &cancelled,
            },
        )
        .unwrap()
    }

    #[test]
    fn empty_needs_a_permanent_review_and_an_agent_cannot_start_it() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
        }
        let selection = trashed_selection(&db, &caller, &ids);

        // Agent 侧完全拿不到这个入口：`trash.empty.*` 的 callers 只有 gui。
        let pi = pi_caller(&db, "principal_pi_empty_probe");
        let error = prepare_empty(&db, &pi, &prepare_input(&selection))
            .expect_err("an agent must never prepare a permanent wipe");
        assert_eq!(error_code(error), "UNAUTHORIZED");
        let error = submit_empty(&db, &pi, &submit_input(&selection))
            .expect_err("an agent must never submit a permanent wipe");
        assert_eq!(error_code(error), "UNAUTHORIZED");

        let preview = prepare_empty(&db, &caller, &prepare_input(&selection)).unwrap();
        assert_eq!(preview.capability, CapabilityId::TrashEmptySubmit);
        assert_eq!(preview.item_count, 2);
        // 审核类型必须是永久删除，普通计划确认不能替代。
        assert_eq!(
            preview.review_kind,
            crate::contracts::agent::ReviewKind::PermanentTrash
        );
        let summary = preview.summary.as_str();
        assert!(summary.contains("不可恢复"), "{summary}");
        assert!(summary.contains("永久清空"), "{summary}");

        // 准备阶段一个字节都不动
        assert!(root.path().join("photo_one.jpg").exists());
        assert_eq!(trashed_count(&db, &ids), 2);

        let submit = submit_input(&preview.plan_id);
        let error =
            submit_empty(&db, &caller, &submit).expect_err("submitting without approval must fail");
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let (operation_id, step_ref) = empty_operation(&db, &caller, &submit);
        // 提交只登记逐项条目，文件与记录都还在
        assert_eq!(trashed_count(&db, &ids), 2);
        assert!(root.path().join("photo_one.jpg").exists());
        assert_eq!(empty_plan_items(&db, &preview.plan_id).unwrap(), ids);

        let summary = run_empty(&db, &caller, &operation_id, &step_ref, &ids);
        assert_eq!(summary.removed, 2);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.missing, 0);
        assert_eq!(summary.unknown, 0);
        assert!(!root.path().join("photo_one.jpg").exists());
        assert!(!root.path().join("photo_two.jpg").exists());
        // 永久删除同时清掉文件与数据库行：回收站里不应再留下任何一项。
        assert_eq!(trashed_count(&db, &ids), 0);
        let journal_states = {
            let conn = db.lock().unwrap();
            crate::db::deletion_journal::entries(&conn, &operation_id)
                .unwrap()
                .into_iter()
                .map(|entry| entry.state)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            journal_states,
            vec![
                crate::db::deletion_journal::STATE_COMMITTED.to_string(),
                crate::db::deletion_journal::STATE_COMMITTED.to_string()
            ]
        );

        // 幂等重放必须返回原操作，而不是因为集合已清空被误判成计划改变。
        let replay = submit_empty(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id.as_str(), operation_id);
    }

    #[test]
    fn empty_reports_a_missing_file_and_keeps_its_row() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let caller = gui_caller(&db);
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
        }
        let selection = trashed_selection(&db, &caller, &ids);
        let preview = prepare_empty(&db, &caller, &prepare_input(&selection)).unwrap();
        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let submit = submit_input(&preview.plan_id);
        let (operation_id, step_ref) = empty_operation(&db, &caller, &submit);

        // 计划之后有人手动删了其中一个文件：那一项只能报缺失，绝不假装删成功
        std::fs::remove_file(root.path().join("photo_one.jpg")).unwrap();
        let summary = run_empty(&db, &caller, &operation_id, &step_ref, &ids);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.missing, 1, "文件本来就不在的项必须单独计数");
        assert_eq!(summary.failed, 0);

        // 缺失项的数据库记录必须保留，用户还需要看见它
        assert_eq!(trashed_count(&db, &ids[..1]), 1, "缺失项的记录仍在回收站");
        let (state, error) = {
            let conn = db.lock().unwrap();
            let items = operations_db::items(&conn, &operation_id, step_ref.as_str()).unwrap();
            let missing = items
                .iter()
                .find(|item| item.item_id == crate::services::trash::deletion_item_id(ids[0]))
                .expect("missing item recorded");
            (missing.state.clone(), missing.error_code.clone())
        };
        assert_eq!(state, "failed");
        assert_eq!(error.as_deref(), Some("FILE_MISSING"));
    }

    #[test]
    fn expiry_only_takes_items_that_were_already_expired() {
        let (root, db, ids) = seeded_library(&["old_photo.jpg", "fresh_photo.jpg"]);
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
            // 手工把第一项推到 40 天前：到期扫描必须只带走它，
            // 不能被同一个 cutoff 顺手清掉刚进回收站的项。
            let old = chrono::Utc::now().timestamp_millis() - 40 * 86_400_000;
            conn.execute(
                "UPDATE assets SET deleted_at = ?1 WHERE id = ?2",
                rusqlite::params![old, ids[0]],
            )
            .unwrap();
        }
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let mut removed = Vec::new();
        let summary =
            crate::services::trash::purge_expired(&db, &library, 30, &mut |id| removed.push(id))
                .unwrap();
        assert_eq!(summary.removed, 1);
        // 未到期的项在 SQL 清单阶段就被排除了，所以不计入 skipped；`skipped`
        // 只在「清单取到之后该项又被改了状态」时才出现（并发保护）。
        assert_eq!(summary.skipped, 0);
        assert_eq!(removed, vec![ids[0]]);
        assert!(!root.path().join("old_photo.jpg").exists());
        assert!(root.path().join("fresh_photo.jpg").exists());
        assert_eq!(trashed_count(&db, &ids[1..]), 1, "未到期的项必须保留");

        // 到期清理没有调用者：不该生成任何操作收据。
        let (operations, items) = {
            let conn = db.lock().unwrap();
            let operations: i64 = conn
                .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
                .unwrap();
            let items: i64 = conn
                .query_row("SELECT COUNT(*) FROM operation_items", [], |row| row.get(0))
                .unwrap();
            (operations, items)
        };
        assert_eq!(operations, 0);
        assert_eq!(items, 0);

        // 但必须留下逐文件日志，供重启对账与审计。
        let journal = {
            let conn = db.lock().unwrap();
            crate::db::deletion_journal::unfinished(&conn).unwrap()
        };
        assert!(journal.is_empty(), "成功的删除不应留在未完成清单");
    }

    #[test]
    fn restart_reconciles_an_interrupted_wipe_from_the_deletion_journal() {
        let (root, db, ids) = seeded_library(&["wiped_photo.jpg", "kept_photo.jpg"]);
        let caller = gui_caller(&db);
        {
            let conn = db.lock().unwrap();
            assets::soft_delete(&conn, &ids).unwrap();
        }
        let selection = trashed_selection(&db, &caller, &ids);
        let preview = prepare_empty(&db, &caller, &prepare_input(&selection)).unwrap();
        plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let submit = submit_input(&preview.plan_id);
        let (operation_id, step_ref) = empty_operation(&db, &caller, &submit);

        // 模拟「提交后进程被杀」：操作停在 interrupted，两项都还是 pending。
        // 第一项停在 intent 且文件仍在；第二项已经 file_deleted 且文件确实不在。
        let library = {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE operations SET state='interrupted' WHERE operation_id=?1",
                rusqlite::params![operation_id],
            )
            .unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let now = chrono::Utc::now().timestamp();
        let intent_item = crate::services::trash::deletion_item_id(ids[0]);
        let deleted_item = crate::services::trash::deletion_item_id(ids[1]);
        {
            let conn = db.lock().unwrap();
            crate::db::deletion_journal::record_intent(
                &conn,
                &crate::db::deletion_journal::NewDeletionIntent {
                    scope_id: &operation_id,
                    item_id: &intent_item,
                    asset_id: ids[0],
                    library_id: &library.library_id,
                    library_epoch: &library.library_epoch,
                    purpose: crate::db::deletion_journal::PURPOSE_EMPTY,
                    source_path: root
                        .path()
                        .join("wiped_photo.jpg")
                        .to_string_lossy()
                        .as_ref(),
                    source_identity: None,
                    updated_at: now,
                },
            )
            .unwrap();
            crate::db::deletion_journal::record_intent(
                &conn,
                &crate::db::deletion_journal::NewDeletionIntent {
                    scope_id: &operation_id,
                    item_id: &deleted_item,
                    asset_id: ids[1],
                    library_id: &library.library_id,
                    library_epoch: &library.library_epoch,
                    purpose: crate::db::deletion_journal::PURPOSE_EMPTY,
                    source_path: root
                        .path()
                        .join("kept_photo.jpg")
                        .to_string_lossy()
                        .as_ref(),
                    source_identity: None,
                    updated_at: now,
                },
            )
            .unwrap();
            crate::db::deletion_journal::record_file_deleted(
                &conn,
                &operation_id,
                &deleted_item,
                now,
            )
            .unwrap();
        }
        std::fs::remove_file(root.path().join("kept_photo.jpg")).unwrap();

        let reconciled = crate::services::trash::reconcile_interrupted_deletions(&db).unwrap();
        assert_eq!(reconciled, vec![operation_id.clone()]);

        let items = {
            let conn = db.lock().unwrap();
            operations_db::items(&conn, &operation_id, step_ref.as_str()).unwrap()
        };
        let intent_row = items
            .iter()
            .find(|item| item.item_id == intent_item)
            .expect("intent row");
        assert_eq!(
            intent_row.state, "failed",
            "文件还在就只能是「未删除」，不能猜成成功"
        );
        assert_eq!(intent_row.error_code.as_deref(), Some("INTERRUPTED"));
        let deleted_row = items
            .iter()
            .find(|item| item.item_id == deleted_item)
            .expect("deleted row");
        assert_eq!(
            deleted_row.state, "succeeded",
            "有 file_deleted 证据且文件确实不在，可以向前完成"
        );

        // intent 项：文件与数据库记录都必须保留，用户可以重试。
        assert!(root.path().join("wiped_photo.jpg").exists());
        assert_eq!(trashed_count(&db, &ids[..1]), 1);
        // file_deleted 项：文件已不在，数据库行也必须跟着收掉，不留孤儿。
        assert!(!root.path().join("kept_photo.jpg").exists());
        assert_eq!(trashed_count(&db, &ids[1..]), 0);

        let (state, journal_state, events) = {
            let conn = db.lock().unwrap();
            let record = plans_db::operation(&conn, &operation_id)
                .unwrap()
                .expect("operation");
            let entry = crate::db::deletion_journal::entry(&conn, &operation_id, &deleted_item)
                .unwrap()
                .expect("journal entry");
            let receipts =
                operations_db::receipts(&conn, &operation_id, step_ref.as_str()).unwrap();
            let events = receipts
                .into_iter()
                .map(|receipt| receipt.event)
                .collect::<Vec<_>>();
            (record.state, entry.state, events)
        };
        assert_eq!(state, "outcome_unknown", "有不明项时不能报成功");
        assert_eq!(
            journal_state,
            crate::db::deletion_journal::STATE_COMMITTED,
            "完成收尾后日志必须收敛到 committed"
        );
        assert!(events.contains(&"trash_empty_committed_after_restart".to_string()));
        assert!(events.contains(&"trash_empty_interrupted".to_string()));
    }

    // -----------------------------------------------------------------
    // A3-03：打标与常用流程适配器
    //
    // 这些测试故意走完整领域链路（真实 plans / tagging / workflow_run 服务），
    // 不 mock 启动或提交函数：审计指出的「整函数 mock 掩盖断链」不能再出现。
    // -----------------------------------------------------------------

    /// Bind a tagging connection the way the user does on the tagging page.
    fn bind_tagging_connection(db: &Database) -> ResourceRef {
        let conn = db.lock().unwrap();
        crate::db::ai_connections::upsert(
            &conn,
            "tagging-adapter-test",
            "Tagging adapter test",
            "cloud",
            "openai_chat",
            "https://example.invalid/v1",
            "vision-test-model",
            None,
        )
        .unwrap();
        crate::db::ai_connections::bind_usage(&conn, "tagging", "tagging-adapter-test").unwrap();
        ResourceRef::from_trusted("tagging-adapter-test").unwrap()
    }

    fn tagging_input(
        selection_ref: &ResourceRef,
        service_ref: &ResourceRef,
        instructions: &str,
    ) -> PrepareTaggingInput {
        PrepareTaggingInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection_ref.clone(),
            service_ref: service_ref.clone(),
            instructions: serde_json::from_value(serde_json::Value::String(
                instructions.to_string(),
            ))
            .unwrap(),
        }
    }

    #[test]
    fn tagging_adapter_only_plans_and_refuses_to_swap_the_bound_service() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let bound = bind_tagging_connection(&db);
        let caller = pi_caller(&db, "principal_pi_tagging");
        let selection =
            selection_service::store(&db, &caller, "打标这批", &ids, ids.len() as i64).unwrap();

        // 模型不能用另一个服务名换掉用户绑定的连接（更贵的模型也不能被悄悄换进来）。
        let other = ResourceRef::from_trusted("some-other-service").unwrap();
        let error = prepare_tagging(
            &db,
            &caller,
            &tagging_input(&selection.selection_ref, &other, "多给点场景标签"),
        )
        .unwrap_err();
        assert_eq!(error_code(error), "INVALID_ARG");

        let preview = prepare_tagging(
            &db,
            &caller,
            &tagging_input(&selection.selection_ref, &bound, "多给点场景标签"),
        )
        .unwrap();
        assert_eq!(preview.capability, CapabilityId::TaggingSubmit);
        assert_eq!(
            preview.review_status,
            crate::contracts::agent::PlanReviewStatus::Pending
        );
        assert_eq!(preview.item_count, 2);
        // 模型的补充要求只进审批摘要，审批人能看到它，但它没有扩大外发范围。
        assert!(
            preview.summary.as_str().contains("多给点场景标签"),
            "审批摘要必须如实反映模型想做什么：{}",
            preview.summary.as_str()
        );
        // 准备阶段不得产生任何外发或批次。
        let batches = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM ai_batches", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(batches, 0, "准备阶段不能创建批次或发起外发");
    }

    #[test]
    fn tagging_adapter_submit_stops_at_awaiting_user_start() {
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let bound = bind_tagging_connection(&db);
        let caller = pi_caller(&db, "principal_pi_tagging_start");
        let gui = gui_caller(&db);
        let selection =
            selection_service::store(&db, &caller, "打标这批", &ids, ids.len() as i64).unwrap();
        let preview = prepare_tagging(
            &db,
            &caller,
            &tagging_input(&selection.selection_ref, &bound, ""),
        )
        .unwrap();

        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error = submit_tagging(&db, &caller, &submit).unwrap_err();
        assert_eq!(
            error_code(error),
            "APPROVAL_REQUIRED",
            "没有批准只能拿到待审计划"
        );

        plans::approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        let result = submit_tagging(&db, &caller, &submit).unwrap();
        assert!(!result.replayed);
        assert_eq!(
            result.operation.state,
            crate::contracts::agent::OperationState::Queued,
            "打标提交只排队，不能自己开始外发"
        );

        // 真实的 tagging 链接必须停在 awaiting_user_start，且一次请求都没发。
        let (state, attempted, batch_id) = {
            let conn = db.lock().unwrap();
            let step = operations_db::steps(&conn, result.operation.operation_id.as_str())
                .unwrap()
                .into_iter()
                .next()
                .expect("打标计划必须有一个操作步骤");
            let link = crate::db::tagging::by_operation(
                &conn,
                result.operation.operation_id.as_str(),
                &step.step_id,
            )
            .unwrap()
            .expect("打标提交必须写入批次链接");
            (link.state, link.requests_attempted, link.batch_id)
        };
        assert_eq!(
            state, "awaiting_user_start",
            "打标必须等用户在打标界面显式开始"
        );
        assert_eq!(attempted, 0, "提交阶段不能有任何请求尝试");
        assert_eq!(batch_id, 1, "应创建恰好一个批次");

        // 同一幂等键重放返回同一操作，不会开第二个批次。
        let replay = submit_tagging(&db, &caller, &submit).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id, result.operation.operation_id);
        let batches: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM ai_batches", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(batches, 1);
    }

    fn workflow_step(capability: &str) -> crate::services::workflows::WorkflowStepInput {
        crate::services::workflows::WorkflowStepInput {
            capability: capability.to_string(),
            depends_on: None,
        }
    }

    fn gui_workflow(db: &Database, caller: &CallerContext, steps: &[&str]) -> ResourceRef {
        let view = crate::services::workflows::create_workflow(
            db,
            caller,
            crate::services::workflows::WorkflowDraftInput {
                name: "适配器测试流程".to_string(),
                description: String::new(),
                steps: steps.iter().map(|step| workflow_step(step)).collect(),
                parameters: Vec::new(),
            },
        )
        .unwrap();
        ResourceRef::from_trusted(view.workflow_ref).unwrap()
    }

    fn workflow_inputs(
        selection: &ResourceRef,
        collection: Option<&ResourceRef>,
        destination: Option<&ResourceRef>,
    ) -> crate::contracts::agent::WorkflowInputs {
        crate::contracts::agent::WorkflowInputs {
            source_ref: None,
            selection_ref: Some(selection.clone()),
            collection_ref: collection.cloned(),
            destination_ref: destination.cloned(),
            service_ref: None,
            tagging_instructions: None,
            rename_pattern: None,
        }
    }

    #[test]
    fn workflow_adapter_prepares_and_submits_but_never_executes() {
        let (root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg"]);
        let gui = gui_caller(&db);
        let caller = pi_caller(&db, "principal_pi_workflow");
        let workflow_ref = gui_workflow(&db, &gui, &["assets.move.submit"]);

        // load：模型只能读流程定义，返回的是契约摘要。
        let summary = load_workflow(
            &db,
            &caller,
            &LoadWorkflowInput {
                schema_version: ProtocolVersion::V1,
                workflow_ref: workflow_ref.clone(),
                version: 1,
            },
        )
        .unwrap();
        assert_eq!(summary.version, 1);
        assert_eq!(summary.steps.len(), 1);
        assert_eq!(
            summary.steps[0].review_kind,
            crate::contracts::agent::ReviewKind::Ordinary
        );

        let selection =
            selection_service::store(&db, &caller, "流程这批", &ids, ids.len() as i64).unwrap();
        let collection = move_collection_ref(&db, "2024/流程");
        let prepared = prepare_workflow(
            &db,
            &caller,
            &PrepareWorkflowInput {
                schema_version: ProtocolVersion::V1,
                workflow_ref: workflow_ref.clone(),
                version: 1,
                inputs: workflow_inputs(&selection.selection_ref, Some(&collection), None),
            },
        )
        .unwrap();
        assert_eq!(prepared.plan.capability, CapabilityId::WorkflowsSubmit);
        assert_eq!(prepared.steps.len(), 1);
        assert_eq!(
            prepared.plan.review_status,
            crate::contracts::agent::PlanReviewStatus::Pending
        );

        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: prepared.plan.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error = submit_workflow(&db, &caller, &submit).unwrap_err();
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");

        plans::approve_plan(&db, &gui, &prepared.plan.plan_id, &prepared.plan.review_ref).unwrap();
        let result = submit_workflow(&db, &caller, &submit).unwrap();
        assert!(!result.replayed);
        assert_eq!(
            result.operation.state,
            crate::contracts::agent::OperationState::Queued,
            "适配器只提交操作，执行必须由流程运行器驱动"
        );

        // 文件必须还在原处：模型拿不到「让文件动起来」的捷径。
        for name in ["photo_one.jpg", "photo_two.jpg"] {
            assert!(
                root.path().join(name).exists(),
                "{name} 不能因为模型提交就被移动"
            );
        }
        let journal = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM move_journal", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(journal, 0, "没有执行就不该有移动日志");
    }

    #[test]
    fn workflow_export_step_is_refused_for_a_model_caller() {
        // 导出目标必须是 GUI 授权的目录句柄；Pi 没有也不该拿到这种句柄，
        // 所以这里必须明确拒绝，并给出可修复指引，而不是假装成功。
        let (root, db, ids) = seeded_library(&["photo_one.jpg"]);
        let gui = gui_caller(&db);
        let caller = pi_caller(&db, "principal_pi_workflow_export");
        let workflow_ref = gui_workflow(&db, &gui, &["exports.submit"]);

        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &gui, &out);
        let selection =
            selection_service::store(&db, &caller, "流程这批", &ids, ids.len() as i64).unwrap();

        let error = prepare_workflow(
            &db,
            &caller,
            &PrepareWorkflowInput {
                schema_version: ProtocolVersion::V1,
                workflow_ref,
                version: 1,
                inputs: workflow_inputs(&selection.selection_ref, None, Some(&destination)),
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(error),
            "UNAUTHORIZED",
            "GUI 的导出目录句柄不能被模型复用"
        );
    }

    #[test]
    fn tagging_submit_refuses_a_selection_that_changed_after_approval() {
        // 审批之后的素材集合被换掉时必须拒绝，而不是拿旧摘要去打标新的素材。
        // 这是一条真实的源复验：选择行本身是 insert-only，这里用直接改库模拟
        // 「底层素材集合与批准时不一致」的唯一可能路径。
        let (_root, db, ids) = seeded_library(&["photo_one.jpg", "photo_two.jpg", "note.jpg"]);
        let bound = bind_tagging_connection(&db);
        let caller = pi_caller(&db, "principal_pi_tagging_tamper");
        let gui = gui_caller(&db);
        let selection = selection_service::store(&db, &caller, "打标这两张", &ids[..2], 2).unwrap();
        let preview = prepare_tagging(
            &db,
            &caller,
            &tagging_input(&selection.selection_ref, &bound, ""),
        )
        .unwrap();
        plans::approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();

        // 审批后把源集合换成另一张素材（模拟选择被重绑/素材被替换）。
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE agent_selections SET asset_ids = ?1 WHERE selection_ref = ?2",
                rusqlite::params![
                    serde_json::to_string(&vec![ids[2]]).unwrap(),
                    selection.selection_ref.as_str()
                ],
            )
            .unwrap();
        }

        let submit = SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("request"),
        };
        let error = submit_tagging(&db, &caller, &submit).unwrap_err();
        assert_eq!(
            error_code(error),
            "PLAN_CHANGED",
            "素材集合与批准时不一致必须拒绝提交"
        );

        // 拒绝之后不能留下半个批次或操作。
        let (batches, operations) = {
            let conn = db.lock().unwrap();
            let batches: i64 = conn
                .query_row("SELECT COUNT(*) FROM ai_batches", [], |row| row.get(0))
                .unwrap();
            let operations: i64 = conn
                .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
                .unwrap();
            (batches, operations)
        };
        assert_eq!(batches, 0, "被拒绝的提交不能创建批次");
        assert_eq!(operations, 0, "被拒绝的提交不能创建操作");
    }
}
