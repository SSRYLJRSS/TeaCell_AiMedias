//! Reliable import execution over a prepared import plan.
//!
//! Execution runs only against an already accepted operation (created through
//! the A1-04 approval flow). Every item is registered with a fixed identity and
//! a receipt; already-finished items are skipped on resume, duplicates record a
//! real asset mapping instead of re-copying, and a source change rejects the
//! whole run instead of silently importing the wrong bytes.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::import_plans::{self, ImportPlanItemRecord, ImportPlanRecord};
use crate::db::operations::{NewOperationItem, NewOperationReceipt};
use crate::db::{assets, libraries, operations as operations_db, plans};
use crate::error::{AppError, AppResult};
use crate::services::authorization as auth_service;
#[cfg(test)]
use crate::services::import_prepare;
use crate::services::importer::{
    self, ImportOptions, ImportPlan, ImportPlanItem, ImportPreviewStatus, ImportProgress,
};
use crate::services::managed_library;
use crate::services::operations as operations_service;
use crate::services::thumbnail::ThumbnailService;
use crate::state::Database;
use crate::utils::file_identity::{file_identity, file_object_identity};
#[cfg(test)]
use crate::utils::hash::sha256_full;
use crate::utils::hash::sha256_full_with_checkpoint;
use crate::utils::path::{encode_native_path, normalize_path};

/// Seed the complete approved item set in the same transaction that accepts
/// an import, before any file publication can begin. Older accepted
/// operations are backfilled by the worker and startup reconciler.
pub(crate) fn seed_import_operation_items(
    transaction: &rusqlite::Transaction<'_>,
    operation_id: &str,
    step_id: &str,
    plan_items: &[ImportPlanItemRecord],
    now: i64,
) -> AppResult<()> {
    for (ordinal, item) in plan_items.iter().filter(|item| !item.excluded).enumerate() {
        let existing =
            operations_db::operation_item(transaction, operation_id, step_id, &item.item_id)?;
        operations_db::insert_item(
            transaction,
            &NewOperationItem {
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                item_id: item.item_id.clone(),
                ordinal: ordinal as i64,
                input_fingerprint: Some(item.file_identity.clone()),
                target_ref: Some(item.rename_preview.clone()),
                created_at: now,
            },
        )?;
        if existing.is_none() {
            let seeded =
                operations_db::operation_item(transaction, operation_id, step_id, &item.item_id)?
                    .ok_or_else(|| AppError::internal("入库项目登记后无法读取"))?;
            let receipt_id = format!(
                "system:item:{}:{}:{}:r{}",
                operation_id, step_id, item.item_id, seeded.revision
            );
            if !operations_db::receipt_exists(transaction, &receipt_id)? {
                operations_db::insert_receipt(
                    transaction,
                    &NewOperationReceipt {
                        receipt_id,
                        operation_id: operation_id.to_string(),
                        step_id: step_id.to_string(),
                        item_id: Some(item.item_id.clone()),
                        revision: seeded.revision,
                        event: "item_accepted".to_string(),
                        input_fingerprint: seeded.input_fingerprint,
                        target_ref: seeded.target_ref,
                        child_task_kind: None,
                        child_task_id: None,
                        db_ref: None,
                        effect_state: "none".to_string(),
                        error_code: None,
                        recorded_at: now,
                    },
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportExecutionSummary {
    pub operation_ref: ResourceRef,
    pub state: String,
    pub imported: u32,
    pub duplicates: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailureView {
    pub item_ref: ResourceRef,
    pub display_name: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// One `duplicate_mapped` plan item, listed so the trusted GUI can ask the user
/// explicitly before those assets join an AI tagging scope (A3-07 / 契约 §4.4).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDuplicateView {
    pub item_ref: ResourceRef,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResultView {
    /// The `imports.submit` step that produced these receipts. The tagging
    /// preparation path is keyed by `(operationId, stepId)`, so the GUI must be
    /// able to name the exact step instead of guessing one.
    pub source_step_ref: ResourceRef,
    pub imported: u32,
    pub duplicate_items: Vec<ImportDuplicateView>,
    pub duplicates: u32,
    pub failed: u32,
    pub failures: Vec<ImportFailureView>,
}

/// Rebuild a path-free result view from durable operation items and receipts.
/// This preserves new-import and duplicate counts after restart, when transient
/// progress events are no longer available.
pub fn import_result(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<ImportResultView> {
    crate::services::plans::get_operation(db, caller, operation_ref)?;
    let plan_ref = crate::services::import_submission::import_ref(db, operation_ref)?;
    let (source_step_ref, plan_items, items, receipts) = {
        let conn = db.lock()?;
        let step = operations_db::steps(&conn, operation_ref.as_str())?
            .into_iter()
            .find(|step| step.capability == "imports.submit")
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        (
            ResourceRef::from_trusted(step.step_id.clone())?,
            import_plans::items(&conn, plan_ref.as_str())?,
            operations_db::items(&conn, operation_ref.as_str(), &step.step_id)?,
            operations_db::receipts(&conn, operation_ref.as_str(), &step.step_id)?,
        )
    };
    let plan_item_names: HashMap<_, _> = plan_items
        .into_iter()
        .map(|item| (item.item_id, item.display_name))
        .collect();
    let mut latest_events = HashMap::new();
    for receipt in receipts {
        if let Some(item_ref) = receipt.item_id {
            latest_events.insert(item_ref, receipt.event);
        }
    }
    let mut result = ImportResultView {
        source_step_ref,
        imported: 0,
        duplicate_items: Vec::new(),
        duplicates: 0,
        failed: 0,
        failures: Vec::new(),
    };
    for item in items {
        match item.state.as_str() {
            "succeeded" => match latest_events.get(&item.item_id).map(String::as_str) {
                Some("item_imported") => result.imported += 1,
                Some("duplicate_mapped") => {
                    let display_name = plan_item_names
                        .get(&item.item_id)
                        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
                    result.duplicates += 1;
                    result.duplicate_items.push(ImportDuplicateView {
                        item_ref: ResourceRef::from_trusted(item.item_id)?,
                        display_name: display_name.clone(),
                    });
                }
                _ => return Err(AppError::internal("入库成功项缺少可识别的结果收据")),
            },
            "failed" | "outcome_unknown" => {
                let display_name = plan_item_names
                    .get(&item.item_id)
                    .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
                result.failed += 1;
                result.failures.push(ImportFailureView {
                    item_ref: ResourceRef::from_trusted(item.item_id)?,
                    display_name: display_name.clone(),
                    state: item.state,
                    error_code: item.error_code,
                });
            }
            _ => {}
        }
    }
    Ok(result)
}

/// Execute a prepared import plan against an accepted operation. Callers must
/// have already consumed the approval and created the operation.
pub fn execute_import<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    thumbs: &ThumbnailService,
    caller: &CallerContext,
    import_plan_ref: &ResourceRef,
    operation_ref: &ResourceRef,
    cancel: &AtomicBool,
    progress: F,
) -> AppResult<ImportExecutionSummary> {
    let (plan, items, step_ref) = prepare_run(db, caller, import_plan_ref, operation_ref)?;
    {
        let conn = db.lock()?;
        let transaction = conn.unchecked_transaction()?;
        seed_import_operation_items(
            &transaction,
            operation_ref.as_str(),
            step_ref.as_str(),
            &items,
            current_unix_seconds()?,
        )?;
        transaction.commit()?;
    }
    let library_root = managed_root(db)?;

    for (ordinal, item) in items.iter().filter(|item| !item.excluded).enumerate() {
        let reference = ResourceRef::from_trusted(item.item_id.clone())?;
        if !already_finished(db, operation_ref, &step_ref, &reference)? {
            if let Err(error) = execution_source(db, caller, operation_ref, item, cancel) {
                if error.code() == "CANCELLED" {
                    break;
                }
                if is_read_authorization_error(&error) {
                    return Err(error);
                }
                let unknown = error.code() == "OUTCOME_UNKNOWN";
                operations_service::register_item(
                    db,
                    caller,
                    operation_ref,
                    &step_ref,
                    reference.as_str(),
                    ordinal as u32,
                    Some(item.file_identity.as_str()),
                    Some(item.rename_preview.as_str()),
                )?;
                record_failure(
                    db,
                    caller,
                    operation_ref,
                    &step_ref,
                    &reference,
                    if unknown {
                        "OUTCOME_UNKNOWN"
                    } else {
                        "PLAN_CHANGED"
                    },
                )?;
                operations_service::finish_operation(
                    db,
                    caller,
                    operation_ref,
                    if unknown {
                        crate::contracts::agent::OperationState::OutcomeUnknown
                    } else {
                        crate::contracts::agent::OperationState::Failed
                    },
                )?;
                return Err(if unknown {
                    error
                } else {
                    AppError::from(AgentErrorCode::PlanChanged)
                });
            }
        }
    }

    let opts = ImportOptions {
        library_root: Some(library_root),
        collection: plan.target_collection.clone(),
        rename_pattern: plan.rename_pattern.clone(),
    };
    let mut imported = 0u32;
    let mut duplicates = 0u32;
    let mut failed = 0u32;
    let mut cancelled = false;
    let mut unknown = false;
    let included: Vec<ImportPlanItemRecord> =
        items.into_iter().filter(|item| !item.excluded).collect();

    for (ordinal, item) in included.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) || operation_cancelled(db, operation_ref)? {
            cancelled = true;
            break;
        }
        let item_ref = ResourceRef::from_trusted(item.item_id.clone())?;
        operations_service::register_item(
            db,
            caller,
            operation_ref,
            &step_ref,
            item_ref.as_str(),
            ordinal as u32,
            Some(item.file_identity.as_str()),
            Some(item.rename_preview.as_str()),
        )?;
        if already_finished(db, operation_ref, &step_ref, &item_ref)? {
            imported_or_duplicate(
                db,
                operation_ref,
                &step_ref,
                &item_ref,
                &mut imported,
                &mut duplicates,
            );
            continue;
        }
        let source_path = match execution_source(db, caller, operation_ref, item, cancel) {
            Ok(path) => path,
            Err(error) if error.code() == "CANCELLED" => {
                cancelled = true;
                break;
            }
            Err(error) if is_read_authorization_error(&error) => return Err(error),
            Err(error) => {
                let code = if error.code() == "OUTCOME_UNKNOWN" {
                    unknown = true;
                    "OUTCOME_UNKNOWN"
                } else {
                    "PLAN_CHANGED"
                };
                record_failure(db, caller, operation_ref, &step_ref, &item_ref, code)?;
                failed += 1;
                continue;
            }
        };
        // The library stores the 16-byte (32 hex) dedup hash; ownership evidence
        // separately uses the full digest inside the importer.
        let source_required = import_item_requires_source_rebinding(db, operation_ref, item)?;
        let full_hash = match sha256_full_with_checkpoint(&source_path, || {
            validate_hash_read(db, caller, operation_ref, item, cancel, source_required)
        }) {
            Ok(hash) => hash,
            Err(error) if error.code() == "CANCELLED" => {
                cancelled = true;
                break;
            }
            Err(error) if is_read_authorization_error(&error) => return Err(error),
            Err(_) => {
                record_failure(
                    db,
                    caller,
                    operation_ref,
                    &step_ref,
                    &item_ref,
                    "FILE_UNREADABLE",
                )?;
                failed += 1;
                continue;
            }
        };
        if item.content_sha256.as_deref() != Some(full_hash.as_str()) {
            operations_service::finish_operation(
                db,
                caller,
                operation_ref,
                crate::contracts::agent::OperationState::Failed,
            )?;
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let hash = full_hash[..32].to_owned();
        // Bind the read digest to the prepared file identity and metadata again.
        match execution_source(db, caller, operation_ref, item, cancel) {
            Err(error) if error.code() == "CANCELLED" => {
                cancelled = true;
                break;
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        if let Some(existing) = existing_asset_for(db, &source_path, &hash)? {
            operations_service::record_item_state(
                db,
                caller,
                operation_ref,
                &step_ref,
                &item_ref,
                "succeeded",
                "registered",
                None,
                &ResourceRef::generated("receipt"),
                "duplicate_mapped",
                Some(&format!("asset:{existing}")),
            )?;
            duplicates += 1;
            continue;
        }
        if cancel.load(Ordering::Relaxed) || operation_cancelled(db, operation_ref)? {
            cancelled = true;
            break;
        }
        let single = single_item_plan(item, &source_path)?;
        let task_id = item_task_id(operation_ref, item);
        let operation_id = operation_ref.as_str().to_string();
        let item_cancel = AtomicBool::new(false);
        let authorization_error = Mutex::new(None::<String>);
        let outcome = importer::import_verified_plan_with_task_id_and_check(
            db,
            thumbs,
            single,
            &opts,
            &item_cancel,
            importer::VerifiedImportExecution {
                task_id,
                content_sha256: full_hash,
                sequence: item.ordinal as usize + 1,
                progress: |mut event: ImportProgress| {
                    event.operation_id = Some(operation_id.clone());
                    progress(event);
                    if cancel.load(Ordering::Relaxed)
                        || operation_cancelled(db, operation_ref).unwrap_or(true)
                    {
                        item_cancel.store(true, Ordering::Relaxed);
                    } else if let Err(error) =
                        revalidate_source_authorization(db, caller, operation_ref, item)
                    {
                        if let Ok(mut recorded) = authorization_error.lock() {
                            recorded.get_or_insert_with(|| error.code().to_string());
                        }
                        item_cancel.store(true, Ordering::Relaxed);
                    }
                },
                checkpoint: || {
                    if cancel.load(Ordering::Relaxed) || operation_cancelled(db, operation_ref)? {
                        item_cancel.store(true, Ordering::Relaxed);
                        return Err(AppError::cancelled("导入操作已取消"));
                    }
                    if let Err(error) =
                        revalidate_source_authorization(db, caller, operation_ref, item)
                    {
                        if let Ok(mut recorded) = authorization_error.lock() {
                            recorded.get_or_insert_with(|| error.code().to_string());
                        }
                        item_cancel.store(true, Ordering::Relaxed);
                        return Err(error);
                    }
                    Ok(())
                },
            },
        );
        if let Some(code) = authorization_error
            .lock()
            .ok()
            .and_then(|recorded| recorded.clone())
        {
            if code == "PLAN_CHANGED"
                && !import_item_requires_source_rebinding(db, operation_ref, item)?
            {
                // A source snapshot changed after the import already left a
                // durable stage/publication identity. Keep that file effect
                // for explicit reconciliation instead of returning a command
                // error that would hide the uncertain outcome.
                unknown = true;
                record_failure(
                    db,
                    caller,
                    operation_ref,
                    &step_ref,
                    &item_ref,
                    "OUTCOME_UNKNOWN",
                )?;
                failed += 1;
                continue;
            }
            return Err(match code.as_str() {
                "GRANT_REVOKED" => AppError::from(AgentErrorCode::GrantRevoked),
                "REFERENCE_EXPIRED" => AppError::from(AgentErrorCode::ReferenceExpired),
                "LIBRARY_CHANGED" => AppError::from(AgentErrorCode::LibraryChanged),
                "LIBRARY_OFFLINE" => AppError::from(AgentErrorCode::LibraryOffline),
                "PLAN_CHANGED" => AppError::from(AgentErrorCode::PlanChanged),
                _ => AppError::from(AgentErrorCode::Unauthorized),
            });
        }
        match outcome {
            Ok(_) => match find_asset_by_hash(db, &hash)? {
                Some(asset_id) => {
                    operations_service::record_item_state(
                        db,
                        caller,
                        operation_ref,
                        &step_ref,
                        &item_ref,
                        "succeeded",
                        "registered",
                        None,
                        &ResourceRef::generated("receipt"),
                        "item_imported",
                        Some(&format!("asset:{asset_id}")),
                    )?;
                    imported += 1;
                }
                None if item_cancel.load(Ordering::Relaxed) => {
                    cancelled = true;
                    break;
                }
                None => {
                    unknown = true;
                    record_failure(
                        db,
                        caller,
                        operation_ref,
                        &step_ref,
                        &item_ref,
                        "OUTCOME_UNKNOWN",
                    )?;
                    failed += 1;
                }
            },
            Err(error) if error.code() == "CANCELLED" => {
                cancelled = true;
                break;
            }
            Err(_) => {
                unknown = true;
                record_failure(
                    db,
                    caller,
                    operation_ref,
                    &step_ref,
                    &item_ref,
                    "IMPORT_FAILED",
                )?;
                failed += 1;
            }
        }
    }

    if cancelled {
        // Cancellation must account for a file that was published just
        // before the import worker observed the request. The journal and
        // target identity are checked before generic cancellation closes any
        // still-open operation items.
        reconcile_import_operation(db, operation_ref.as_str())?;
    }

    let state = if unknown {
        operations_service::finish_operation(
            db,
            caller,
            operation_ref,
            crate::contracts::agent::OperationState::OutcomeUnknown,
        )?
        .state
    } else if cancelled {
        operations_service::finish_cancelled(db, caller, operation_ref)?.state
    } else if failed == 0 {
        operations_service::finish_operation(
            db,
            caller,
            operation_ref,
            crate::contracts::agent::OperationState::Succeeded,
        )?
        .state
    } else if imported + duplicates > 0 {
        operations_service::finish_operation(
            db,
            caller,
            operation_ref,
            crate::contracts::agent::OperationState::PartiallySucceeded,
        )?
        .state
    } else {
        operations_service::finish_operation(
            db,
            caller,
            operation_ref,
            crate::contracts::agent::OperationState::Failed,
        )?
        .state
    };
    Ok(ImportExecutionSummary {
        operation_ref: operation_ref.clone(),
        state: operation_state_to_string(state),
        imported,
        duplicates,
        failed,
    })
}

fn revalidate_source_authorization(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    item: &ImportPlanItemRecord,
) -> AppResult<()> {
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
    }
    let source_is_required = import_item_requires_source_rebinding(db, operation_ref, item)?;
    if let Some(file_ref) = item.file_ref.as_deref() {
        let reference = ResourceRef::from_trusted(file_ref.to_string())?;
        if source_is_required {
            auth_service::locate_authorized_file(db, caller, &reference)?;
        } else {
            let handle_matches_current_session = {
                let conn = db.lock()?;
                crate::db::authorization::handle(&conn, file_ref)?.is_some_and(|handle| {
                    handle.session_id.as_deref()
                        == caller.session_id.as_ref().map(ResourceRef::as_str)
                        && handle.instance_id == caller.instance_id.as_str()
                })
            };
            if !handle_matches_current_session {
                // A fresh session explicitly resumed this operation. The
                // durable target no longer depends on the old session-bound
                // source handle.
                return Ok(());
            }
            auth_service::validate_authorized_file_grant(db, caller, &reference)?;
            if let Some(source) =
                auth_service::locate_authorized_file_if_present(db, caller, &reference)?
            {
                if source.size != item.size
                    || source.modified_at != item.modified_at
                    || source.identity != item.file_identity
                {
                    return Err(AppError::from(AgentErrorCode::PlanChanged));
                }
            }
        }
    } else if source_is_required {
        verify_source_metadata(item)?;
    } else {
        match std::fs::symlink_metadata(&item.path) {
            Ok(_) => verify_source_metadata(item)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A durable target is sufficient to finish registration after
                // a crash that also removed the original source.
            }
            Err(_) => return Err(AppError::from(AgentErrorCode::PlanChanged)),
        }
    }
    Ok(())
}

fn prepare_run(
    db: &Database,
    caller: &CallerContext,
    import_plan_ref: &ResourceRef,
    operation_ref: &ResourceRef,
) -> AppResult<(ImportPlanRecord, Vec<ImportPlanItemRecord>, ResourceRef)> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let plan = import_plans::plan(&conn, import_plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if plan.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let operation = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if operation.principal_id != caller.principal_id.as_str()
        || operation.capability != "imports.submit"
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let domain_plan = plans::plan(&conn, &operation.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if domain_plan.scope_ref != import_plan_ref.as_str()
        || operation.library_id != plan.library_id
        || operation.library_epoch != plan.library_epoch
    {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    if !matches!(operation.state.as_str(), "queued" | "running") {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    let steps = operations_db::steps(&conn, operation_ref.as_str())?;
    let step_ref = steps
        .iter()
        .find(|step| step.capability == "imports.submit")
        .map(|step| ResourceRef::from_trusted(step.step_id.clone()))
        .transpose()?
        .ok_or_else(|| AppError::internal("操作缺少步骤"))?;
    let items = import_plans::items(&conn, plan.plan_id.as_str())?;
    Ok((plan, items, step_ref))
}

#[derive(Debug, Clone)]
struct ImportRecoveryItem {
    plan_item: ImportPlanItemRecord,
    operation_item: operations_db::OperationItemRecord,
    intent: Option<crate::db::import_execution::PublicationIntent>,
}

#[derive(Debug, Clone)]
struct ImportRecoverySnapshot {
    operation_id: String,
    step_id: String,
    library: Option<libraries::LibrarySnapshot>,
    items: Vec<ImportRecoveryItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportArtifactObservation {
    NoEffect,
    Staged,
    Published {
        object_identity: String,
        inferred_from_stage: bool,
    },
    Registered {
        asset_id: i64,
        object_identity: String,
        inferred_from_stage: bool,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecoveryFileCheck {
    Missing,
    WrongIdentity,
    SameIdentityWrongDigest,
    Verified(String),
}

/// Reconcile import publication journals after the generic operation recovery
/// pass. All path checks and hashing happen without a database lock; only the
/// resulting item/receipt transition is committed under SQLite.
pub(crate) fn reconcile_interrupted_imports(db: &Database) -> AppResult<Vec<String>> {
    let operation_ids = {
        let conn = db.lock()?;
        operations_db::interrupted_operation_ids_for_capability(&conn, "imports.submit")?
    };
    for operation_id in &operation_ids {
        reconcile_import_operation(db, operation_id)?;
        let conn = db.lock()?;
        operations_db::refresh_recovery_unknown_items(&conn, operation_id)?;
    }
    Ok(operation_ids)
}

fn reconcile_import_operation(db: &Database, operation_id: &str) -> AppResult<()> {
    let snapshot = {
        let conn = db.lock()?;
        let Some(operation) = plans::operation(&conn, operation_id)? else {
            return Ok(());
        };
        if operation.capability != "imports.submit"
            || !matches!(
                operation.state.as_str(),
                "queued" | "running" | "interrupted"
            )
        {
            return Ok(());
        }
        let domain_plan = plans::plan(&conn, &operation.plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let plan_items = import_plans::items(&conn, &domain_plan.scope_ref)?;
        let step = operations_db::steps(&conn, operation_id)?
            .into_iter()
            .find(|step| step.capability == "imports.submit")
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let transaction = conn.unchecked_transaction()?;
        seed_import_operation_items(
            &transaction,
            operation_id,
            &step.step_id,
            &plan_items,
            current_unix_seconds()?,
        )?;
        transaction.commit()?;
        let records = operations_db::items(&conn, operation_id, &step.step_id)?;
        let by_id: HashMap<_, _> = records
            .into_iter()
            .map(|record| (record.item_id.clone(), record))
            .collect();
        let operation_ref = ResourceRef::from_trusted(operation_id.to_string())?;
        let mut items = Vec::new();
        for plan_item in plan_items.into_iter().filter(|item| !item.excluded) {
            let operation_item = by_id
                .get(&plan_item.item_id)
                .cloned()
                .ok_or_else(|| AppError::internal("恢复入库项目缺失"))?;
            let intent = crate::db::import_execution::intent(
                &conn,
                &item_task_id(&operation_ref, &plan_item),
            )?;
            items.push(ImportRecoveryItem {
                plan_item,
                operation_item,
                intent,
            });
        }
        let library = libraries::current(&conn)?.filter(|library| {
            library.library_id == operation.library_id
                && library.library_epoch == operation.library_epoch
                && library.health == libraries::LibraryHealth::Ready
        });
        ImportRecoverySnapshot {
            operation_id: operation_id.to_string(),
            step_id: step.step_id,
            library,
            items,
        }
    };

    for item in &snapshot.items {
        let observation = inspect_import_artifacts(db, snapshot.library.as_ref(), item)?;
        apply_import_recovery_observation(db, &snapshot, item, observation)?;
    }
    Ok(())
}

fn inspect_import_artifacts(
    db: &Database,
    library: Option<&libraries::LibrarySnapshot>,
    item: &ImportRecoveryItem,
) -> AppResult<ImportArtifactObservation> {
    if item.operation_item.state == "succeeded" {
        return Ok(ImportArtifactObservation::NoEffect);
    }
    let Some(intent) = item.intent.as_ref() else {
        return Ok(
            if item.operation_item.state == "outcome_unknown"
                || item.operation_item.effect_state != "none"
            {
                ImportArtifactObservation::Unknown
            } else {
                ImportArtifactObservation::NoEffect
            },
        );
    };
    let has_published_marker =
        intent.published_file_identity.is_some() || intent.created_file_identity.is_some();
    let has_staging_marker = intent.staging_file_identity.is_some();
    let expected_digest = intent.content_sha256.as_str();
    let Some(library) = library else {
        return Ok(if has_published_marker || has_staging_marker {
            ImportArtifactObservation::Unknown
        } else {
            ImportArtifactObservation::NoEffect
        });
    };

    let target = PathBuf::from(&intent.target_path);
    if has_published_marker {
        let check = verify_recovery_file(
            library,
            &target,
            expected_digest,
            intent.published_file_identity.as_deref(),
            intent.created_file_identity.as_deref(),
        );
        let RecoveryFileCheck::Verified(object_identity) = check else {
            return Ok(ImportArtifactObservation::Unknown);
        };
        return registration_observation(db, &target, expected_digest, object_identity, false);
    }

    if let Some(staging_identity) = intent.staging_file_identity.as_deref() {
        if let RecoveryFileCheck::Verified(object_identity) = verify_recovery_file(
            library,
            &target,
            expected_digest,
            Some(staging_identity),
            None,
        ) {
            return registration_observation(db, &target, expected_digest, object_identity, true);
        }

        let stage_path = intent
            .staging_path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| AppError::internal("暂存身份缺少路径"))?;
        return Ok(
            match verify_recovery_file(
                library,
                &stage_path,
                expected_digest,
                Some(staging_identity),
                None,
            ) {
                RecoveryFileCheck::Verified(_) | RecoveryFileCheck::SameIdentityWrongDigest => {
                    ImportArtifactObservation::Staged
                }
                RecoveryFileCheck::Missing | RecoveryFileCheck::WrongIdentity => {
                    ImportArtifactObservation::Unknown
                }
            },
        );
    }

    Ok(ImportArtifactObservation::NoEffect)
}

fn verify_recovery_file(
    library: &libraries::LibrarySnapshot,
    path: &Path,
    expected_digest: &str,
    expected_object_identity: Option<&str>,
    expected_legacy_identity: Option<&str>,
) -> RecoveryFileCheck {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RecoveryFileCheck::Missing;
        }
        Err(_) => return RecoveryFileCheck::WrongIdentity,
    };
    if crate::utils::path::is_link_or_reparse(&metadata)
        || !metadata.is_file()
        || managed_library::validate_import_target(library, path).is_err()
    {
        return RecoveryFileCheck::WrongIdentity;
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return RecoveryFileCheck::WrongIdentity,
    };
    let object_identity = match file_object_identity(&file) {
        Ok(identity) => identity,
        Err(_) => return RecoveryFileCheck::WrongIdentity,
    };
    let legacy_identity = match file_identity(&file) {
        Ok(identity) => identity,
        Err(_) => return RecoveryFileCheck::WrongIdentity,
    };
    if expected_object_identity.is_some_and(|expected| expected != object_identity)
        || expected_legacy_identity.is_some_and(|expected| expected != legacy_identity)
    {
        return RecoveryFileCheck::WrongIdentity;
    }
    let digest = match crate::utils::hash::sha256_full_file(&mut file) {
        Ok(digest) => digest,
        Err(_) => return RecoveryFileCheck::WrongIdentity,
    };
    if digest == expected_digest {
        RecoveryFileCheck::Verified(object_identity)
    } else {
        RecoveryFileCheck::SameIdentityWrongDigest
    }
}

fn registration_observation(
    db: &Database,
    target: &Path,
    expected_digest: &str,
    object_identity: String,
    inferred_from_stage: bool,
) -> AppResult<ImportArtifactObservation> {
    let path = normalize_path(&encode_native_path(target)?);
    let registration = {
        let conn = db.lock()?;
        assets::managed_registration_by_path(&conn, &path)?
    };
    let Some((asset_id, hash, verified)) = registration else {
        return Ok(ImportArtifactObservation::Published {
            object_identity,
            inferred_from_stage,
        });
    };
    if !verified || hash.as_deref() != Some(&expected_digest[..32]) {
        return Ok(ImportArtifactObservation::Unknown);
    }
    Ok(ImportArtifactObservation::Registered {
        asset_id,
        object_identity,
        inferred_from_stage,
    })
}

fn apply_import_recovery_observation(
    db: &Database,
    snapshot: &ImportRecoverySnapshot,
    item: &ImportRecoveryItem,
    observation: ImportArtifactObservation,
) -> AppResult<()> {
    let (state, effect_state, error_code, event, db_ref, inferred_publication) = match observation {
        ImportArtifactObservation::NoEffect => {
            if item.operation_item.state == "outcome_unknown"
                || item.operation_item.state == "cancelled"
                || item.operation_item.state == "succeeded"
            {
                return Ok(());
            }
            if item.operation_item.state == "pending" && item.operation_item.effect_state == "none"
            {
                return Ok(());
            }
            (
                "pending",
                "none",
                None,
                "import_item_reset_after_restart",
                None,
                None,
            )
        }
        ImportArtifactObservation::Staged => (
            "running",
            "staged",
            None,
            "item_staged_reconciled",
            None,
            None,
        ),
        ImportArtifactObservation::Published {
            object_identity,
            inferred_from_stage,
        } => (
            "running",
            "published",
            None,
            "item_publication_reconciled",
            None,
            inferred_from_stage.then_some(object_identity),
        ),
        ImportArtifactObservation::Registered {
            asset_id,
            object_identity,
            inferred_from_stage,
        } => (
            "succeeded",
            "registered",
            None,
            "item_imported",
            Some(format!("asset:{asset_id}")),
            inferred_from_stage.then_some(object_identity),
        ),
        ImportArtifactObservation::Unknown => (
            "outcome_unknown",
            "unknown",
            Some("OUTCOME_UNKNOWN"),
            "item_recovery_outcome_unknown",
            None,
            None,
        ),
    };
    let conn = db.lock()?;
    let transaction = conn.unchecked_transaction()?;
    let Some(current) = operations_db::operation_item(
        &transaction,
        &snapshot.operation_id,
        &snapshot.step_id,
        &item.plan_item.item_id,
    )?
    else {
        return Err(AppError::internal("恢复入库项目在提交前丢失"));
    };
    if current.revision != item.operation_item.revision || current.state == "succeeded" {
        transaction.commit()?;
        return Ok(());
    }
    if let Some(published_identity) = inferred_publication.as_deref() {
        let intent = item
            .intent
            .as_ref()
            .ok_or_else(|| AppError::internal("恢复发布缺少文件意图"))?;
        let staging_identity = intent
            .staging_file_identity
            .as_deref()
            .ok_or_else(|| AppError::internal("恢复发布缺少暂存身份"))?;
        crate::db::import_execution::record_published_file(
            &transaction,
            &item_task_id(
                &ResourceRef::from_trusted(snapshot.operation_id.clone())?,
                &item.plan_item,
            ),
            staging_identity,
            published_identity,
        )?;
    }
    let latest_receipt =
        operations_db::receipts(&transaction, &snapshot.operation_id, &snapshot.step_id)?
            .into_iter()
            .rev()
            .find(|receipt| receipt.item_id.as_deref() == Some(item.plan_item.item_id.as_str()));
    let already_recorded = current.state == state
        && current.effect_state == effect_state
        && current.error_code.as_deref() == error_code
        && latest_receipt.as_ref().is_some_and(|receipt| {
            receipt.event == event && receipt.db_ref.as_deref() == db_ref.as_deref()
        });
    if already_recorded {
        transaction.commit()?;
        return Ok(());
    }
    let updated = operations_db::update_item(
        &transaction,
        &snapshot.operation_id,
        &snapshot.step_id,
        &item.plan_item.item_id,
        state,
        effect_state,
        error_code,
        current_unix_seconds()?,
    )?;
    operations_db::insert_receipt(
        &transaction,
        &NewOperationReceipt {
            receipt_id: format!(
                "system:item:{}:{}:{}:r{}",
                snapshot.operation_id, snapshot.step_id, item.plan_item.item_id, updated.revision
            ),
            operation_id: snapshot.operation_id.clone(),
            step_id: snapshot.step_id.clone(),
            item_id: Some(item.plan_item.item_id.clone()),
            revision: updated.revision,
            event: event.to_string(),
            input_fingerprint: updated.input_fingerprint,
            target_ref: updated.target_ref,
            child_task_kind: updated.child_task_kind,
            child_task_id: updated.child_task_id,
            db_ref,
            effect_state: effect_state.to_string(),
            error_code: error_code.map(str::to_string),
            recorded_at: current_unix_seconds()?,
        },
    )?;
    operations_db::refresh_progress(
        &transaction,
        &snapshot.operation_id,
        current_unix_seconds()?,
    )?;
    transaction.commit()?;
    Ok(())
}

fn current_unix_seconds() -> AppResult<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| AppError::internal("系统时间无效"))
}

fn managed_root(db: &Database) -> AppResult<String> {
    let snapshot = {
        let conn = db.lock()?;
        libraries::current(&conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?
    };
    let root = managed_library::verified_root_path(&snapshot)?;
    encode_native_path(&root)
}

fn already_finished(
    db: &Database,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_ref: &ResourceRef,
) -> AppResult<bool> {
    let conn = db.lock()?;
    Ok(operations_db::operation_item(
        &conn,
        operation_ref.as_str(),
        step_ref.as_str(),
        item_ref.as_str(),
    )?
    .is_some_and(|item| item.state == "succeeded"))
}

fn imported_or_duplicate(
    db: &Database,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_ref: &ResourceRef,
    imported: &mut u32,
    duplicates: &mut u32,
) {
    let event = match db.lock() {
        Ok(conn) => operations_db::receipts(&conn, operation_ref.as_str(), step_ref.as_str())
            .ok()
            .and_then(|receipts| {
                receipts
                    .into_iter()
                    .rfind(|receipt| receipt.item_id.as_deref() == Some(item_ref.as_str()))
                    .map(|receipt| receipt.event)
            }),
        Err(_) => None,
    };
    match event.as_deref() {
        Some("duplicate_mapped") => *duplicates += 1,
        _ => *imported += 1,
    }
}

fn record_failure(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_ref: &ResourceRef,
    code: &str,
) -> AppResult<()> {
    operations_service::record_item_state(
        db,
        caller,
        operation_ref,
        step_ref,
        item_ref,
        if matches!(code, "OUTCOME_UNKNOWN" | "IMPORT_FAILED") {
            "outcome_unknown"
        } else {
            "failed"
        },
        if matches!(code, "OUTCOME_UNKNOWN" | "IMPORT_FAILED") {
            "unknown"
        } else {
            "none"
        },
        Some(code),
        &ResourceRef::generated("receipt"),
        "item_failed",
        None,
    )
}

fn resolve_source_path(
    db: &Database,
    caller: &CallerContext,
    item: &ImportPlanItemRecord,
) -> AppResult<PathBuf> {
    if let Some(file_ref) = item.file_ref.as_deref() {
        let reference = ResourceRef::from_trusted(file_ref.to_string())?;
        Ok(auth_service::locate_authorized_file(db, caller, &reference)?.path)
    } else {
        Ok(PathBuf::from(&item.path))
    }
}

fn item_task_id(operation_ref: &ResourceRef, item: &ImportPlanItemRecord) -> String {
    format!("{}_{}", task_id_for(operation_ref), item.item_id)
}

/// Directory handles are session-bound, but a durable publication can be
/// reconciled without opening the original source again. For an interrupted
/// publish whose final identity was not yet committed, recognize the target
/// only when it is still the exact staged object with the frozen digest.
pub(crate) fn import_item_requires_source_rebinding(
    db: &Database,
    operation_ref: &ResourceRef,
    item: &ImportPlanItemRecord,
) -> AppResult<bool> {
    let (intent, library) = {
        let conn = db.lock()?;
        (
            crate::db::import_execution::intent(&conn, &item_task_id(operation_ref, item))?,
            libraries::current(&conn)?,
        )
    };
    let Some(intent) = intent else {
        return Ok(true);
    };
    if intent.published_file_identity.is_some() || intent.created_file_identity.is_some() {
        // The worker will verify the exact target and conservatively record an
        // unknown outcome if it has been replaced or removed.
        return Ok(false);
    }
    let Some(staging_identity) = intent.staging_file_identity.as_deref() else {
        return Ok(true);
    };
    let target = PathBuf::from(&intent.target_path);
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(true),
    };
    if crate::utils::path::is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Ok(true);
    }
    let Some(library) = library else {
        return Ok(true);
    };
    if managed_library::validate_import_target(&library, &target).is_err() {
        return Ok(true);
    }
    let file = match std::fs::File::open(&target) {
        Ok(file) => file,
        Err(_) => return Ok(true),
    };
    if file_object_identity(&file)? != staging_identity {
        return Ok(true);
    }
    match crate::utils::hash::sha256_full(&target) {
        Ok(digest) => Ok(digest != intent.content_sha256),
        Err(_) => Ok(true),
    }
}

fn execution_source(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    item: &ImportPlanItemRecord,
    cancel: &AtomicBool,
) -> AppResult<PathBuf> {
    let (intent, unknown) = {
        let conn = db.lock()?;
        let intent =
            crate::db::import_execution::intent(&conn, &item_task_id(operation_ref, item))?;
        let unknown = operations_db::steps(&conn, operation_ref.as_str())?
            .first()
            .map(|step| {
                operations_db::operation_item(
                    &conn,
                    operation_ref.as_str(),
                    &step.step_id,
                    &item.item_id,
                )
            })
            .transpose()?
            .flatten()
            .is_some_and(|record| record.state == "outcome_unknown");
        (intent, unknown)
    };
    if unknown && intent.is_none() {
        return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
    }
    if let Some(intent) = intent {
        let target = PathBuf::from(intent.target_path);
        let expected = intent.content_sha256;
        if std::fs::symlink_metadata(&target).is_ok() {
            let library = {
                let conn = db.lock()?;
                libraries::current(&conn)?
                    .ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?
            };
            managed_library::validate_import_target(&library, &target)?;
            let metadata = std::fs::symlink_metadata(&target)?;
            if crate::utils::path::is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
            }
            let opened = std::fs::File::open(&target)?;
            let object_identity = file_object_identity(&opened)?;
            let digest_matches = sha256_full_with_checkpoint(&target, || {
                validate_hash_read(db, caller, operation_ref, item, cancel, false)
            })? == expected;
            if let Some(published_identity) = intent.published_file_identity.as_deref() {
                if published_identity != object_identity || !digest_matches {
                    return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
                }
                return Ok(target);
            }
            if let Some(legacy_identity) = intent.created_file_identity.as_deref() {
                if legacy_identity != file_identity(&opened)? || !digest_matches {
                    return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
                }
                return Ok(target);
            }
            if let Some(staging_identity) = intent.staging_file_identity.as_deref() {
                if staging_identity == object_identity {
                    if !digest_matches {
                        return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
                    }
                    let conn = db.lock()?;
                    crate::db::import_execution::record_published_file(
                        &conn,
                        &item_task_id(operation_ref, item),
                        staging_identity,
                        staging_identity,
                    )?;
                    return Ok(target);
                }
                // The target was occupied before our staged object was linked.
                // Revalidate the source, then let the publisher choose a new
                // unused suffix while retaining its staging object.
            } else {
                return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
            }
        }
        if unknown
            || intent.published_file_identity.is_some()
            || intent.created_file_identity.is_some()
        {
            return Err(AppError::from(AgentErrorCode::OutcomeUnknown));
        }
    }
    verify_source(db, caller, operation_ref, item, cancel)
}

fn operation_cancelled(db: &Database, operation_ref: &ResourceRef) -> AppResult<bool> {
    let conn = db.lock()?;
    Ok(plans::operation(&conn, operation_ref.as_str())?.is_some_and(|op| op.phase == "cancelling"))
}

fn verify_source(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    item: &ImportPlanItemRecord,
    cancel: &AtomicBool,
) -> AppResult<PathBuf> {
    let path = resolve_source_path(db, caller, item)?;
    let file = std::fs::File::open(&path)?;
    let meta = file.metadata()?;
    if !source_metadata_matches(&meta, &file, item)?
        || item.content_sha256.as_deref()
            != Some(
                sha256_full_with_checkpoint(&path, || {
                    validate_hash_read(db, caller, operation_ref, item, cancel, true)
                })?
                .as_str(),
            )
    {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    Ok(path)
}

fn validate_hash_read(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    item: &ImportPlanItemRecord,
    cancel: &AtomicBool,
    source_required: bool,
) -> AppResult<()> {
    if cancel.load(Ordering::Relaxed) || operation_cancelled(db, operation_ref)? {
        return Err(AppError::from(AgentErrorCode::Cancelled));
    }
    {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
    }
    if let Some(file_ref) = &item.file_ref {
        if !source_required {
            let conn = db.lock()?;
            let current_session =
                crate::db::authorization::handle(&conn, file_ref)?.is_some_and(|handle| {
                    handle.session_id.as_deref()
                        == caller.session_id.as_ref().map(ResourceRef::as_str)
                        && handle.instance_id == caller.instance_id.as_str()
                });
            // A durable target does not depend on the old session's source.
            if !current_session {
                return Ok(());
            }
        }
        auth_service::validate_authorized_file_grant(
            db,
            caller,
            &ResourceRef::from_trusted(file_ref.clone())?,
        )?;
    }
    Ok(())
}

fn is_read_authorization_error(error: &AppError) -> bool {
    matches!(
        error.code(),
        "GRANT_REVOKED"
            | "UNAUTHORIZED"
            | "REFERENCE_EXPIRED"
            | "LIBRARY_CHANGED"
            | "LIBRARY_OFFLINE"
    )
}

/// Check the frozen filesystem portion of a prepared source snapshot without
/// hashing it again. The staged copy is checked against the frozen full digest
/// before publication, while this check catches metadata-only edits at each
/// copy checkpoint.
fn verify_source_metadata(item: &ImportPlanItemRecord) -> AppResult<()> {
    let path = PathBuf::from(&item.path);
    let path_metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    if crate::utils::path::is_link_or_reparse(&path_metadata) || !path_metadata.is_file() {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let file =
        std::fs::File::open(&path).map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    if source_metadata_matches(&metadata, &file, item)? {
        Ok(())
    } else {
        Err(AppError::from(AgentErrorCode::PlanChanged))
    }
}

fn source_metadata_matches(
    metadata: &std::fs::Metadata,
    file: &std::fs::File,
    item: &ImportPlanItemRecord,
) -> AppResult<bool> {
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?
        .as_millis() as i64;
    Ok(metadata.len() as i64 == item.size
        && modified == item.modified_at
        && file_identity(file)? == item.file_identity)
}

fn existing_asset_for(db: &Database, path: &Path, hash: &str) -> AppResult<Option<i64>> {
    let normalized = normalize_path(&encode_native_path(path)?);
    let conn = db.lock()?;
    if let Some(id) = assets::find_managed_by_path(&conn, &normalized)? {
        return Ok(Some(id));
    }
    assets::find_managed_by_hash(&conn, hash)
}

fn find_asset_by_hash(db: &Database, hash: &str) -> AppResult<Option<i64>> {
    let conn = db.lock()?;
    assets::find_managed_by_hash(&conn, hash)
}

fn single_item_plan(item: &ImportPlanItemRecord, source: &Path) -> AppResult<ImportPlan> {
    let status = match item.preview_status.as_str() {
        "limited" => ImportPreviewStatus::Limited,
        "unsupported" => ImportPreviewStatus::Unsupported,
        _ => ImportPreviewStatus::Ready,
    };
    let is_video = item.kind == "video";
    Ok(ImportPlan {
        items: vec![ImportPlanItem {
            path: encode_native_path(source)?,
            kind: item.kind.clone(),
            size: item.size,
            preview_status: status,
            preview_message: item.preview_message.clone(),
        }],
        images: if is_video { 0 } else { 1 },
        videos: if is_video { 1 } else { 0 },
        total_size: item.size,
        warnings: Vec::new(),
    })
}

fn task_id_for(operation_ref: &ResourceRef) -> String {
    let mut id = String::with_capacity(operation_ref.as_str().len() + 1);
    id.push('i');
    for character in operation_ref.as_str().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            id.push(character);
        } else {
            id.push('_');
        }
    }
    id
}

fn operation_state_to_string(state: crate::contracts::agent::OperationState) -> String {
    use crate::contracts::agent::OperationState::*;
    match state {
        Queued => "queued",
        Running => "running",
        Succeeded => "succeeded",
        PartiallySucceeded => "partially_succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
        Interrupted => "interrupted",
        OutcomeUnknown => "outcome_unknown",
    }
    .to_string()
}

fn validate_caller(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    let principal = crate::db::authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    let library = crate::db::libraries::current(conn)?
        .ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != crate::db::libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::agent::{BudgetSummary, CapabilityId, PlanScopeSummary};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::plans::{self as plans_service, PlanDraft};
    use crate::services::{managed_library, thumbnail::ThumbnailService};
    use std::path::{Path, PathBuf};

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
                session_id: "session_import_exec".to_string(),
                instance_id: "instance_import_exec".to_string(),
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

    fn prepare(db: &Database, caller: &CallerContext, file: &Path) -> ResourceRef {
        import_prepare::prepare_import(
            db,
            caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![encode_native_path(file).unwrap()],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref
    }

    fn accepted_operation(
        db: &Database,
        caller: &CallerContext,
        import_plan_ref: &ResourceRef,
        item_count: u32,
    ) -> ResourceRef {
        let draft = PlanDraft {
            capability: CapabilityId::ImportsSubmit,
            scope: PlanScopeSummary {
                scope_ref: import_plan_ref.clone(),
                source_ref: None,
                selection_ref: None,
                target_ref: None,
                service_ref: None,
                service_revision: None,
                model: None,
            },
            item_count,
            summary: serde_json::from_value(serde_json::json!("导入已准备清单")).unwrap(),
            details_json: None,
            budget: BudgetSummary {
                max_requests: 0,
                max_tokens: 0,
                max_seconds: 1800,
                max_items: item_count,
                monetary_limit: None,
            },
            ttl_seconds: 0,
        };
        let preview = plans_service::prepare_plan(db, caller, draft.clone()).unwrap();
        plans_service::approve_plan(db, caller, &preview.plan_id, &preview.review_ref).unwrap();
        plans_service::submit_plan(
            db,
            caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
            &draft,
        )
        .unwrap()
        .operation
        .operation_id
    }

    fn asset_count(db: &Database) -> i64 {
        let conn = db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .unwrap()
    }

    fn receipt_events(db: &Database, operation_ref: &ResourceRef) -> Vec<String> {
        let conn = db.lock().unwrap();
        let step = operations_db::steps(&conn, operation_ref.as_str())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        operations_db::receipts(&conn, operation_ref.as_str(), &step.step_id)
            .unwrap()
            .into_iter()
            .map(|receipt| receipt.event)
            .collect()
    }

    #[test]
    fn execute_imports_item_and_records_asset_receipt() {
        let managed = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 4);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data_dir.path()).unwrap();

        let plan_ref = prepare(&db, &caller, &jpg);
        let operation_ref = accepted_operation(&db, &caller, &plan_ref, 1);
        let summary = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan_ref,
            &operation_ref,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(summary.state, "succeeded");
        assert_eq!(summary.imported, 1);
        assert_eq!(asset_count(&db), 1);
        assert!(receipt_events(&db, &operation_ref)
            .iter()
            .any(|event| event == "item_imported"));
    }

    #[test]
    fn duplicate_import_maps_to_existing_asset_without_copying() {
        let managed = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 6);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data_dir.path()).unwrap();

        let plan_ref = prepare(&db, &caller, &jpg);
        let first = accepted_operation(&db, &caller, &plan_ref, 1);
        execute_import(
            &db,
            &thumbs,
            &caller,
            &plan_ref,
            &first,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(asset_count(&db), 1);

        let second = accepted_operation(&db, &caller, &plan_ref, 1);
        let summary = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan_ref,
            &second,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.state, "succeeded");
        assert_eq!(summary.duplicates, 1);
        assert_eq!(summary.imported, 0);
        assert_eq!(asset_count(&db), 1, "重复项不得再次复制入库");
        let result = import_result(&db, &caller, &second).unwrap();
        assert_eq!(
            (result.imported, result.duplicates, result.failed),
            (0, 1, 0)
        );
        // The GUI must be able to name the exact import step and each duplicate
        // item before preparing an AI tagging scope (A3-07, contract §4.4).
        assert_eq!(result.duplicate_items.len(), 1);
        assert_eq!(result.duplicate_items[0].display_name, "ok.jpg");
        assert!(!result.duplicate_items[0].item_ref.as_str().is_empty());
        assert!(!result.source_step_ref.as_str().is_empty());
        assert!(receipt_events(&db, &second)
            .iter()
            .any(|event| event == "duplicate_mapped"));
    }

    #[test]
    fn execute_rejects_source_changed_after_prepare() {
        let managed = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 7);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data_dir.path()).unwrap();

        let plan_ref = prepare(&db, &caller, &jpg);
        let operation_ref = accepted_operation(&db, &caller, &plan_ref, 1);
        write_jpg(&jpg, 201);
        let error = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan_ref,
            &operation_ref,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code(), "PLAN_CHANGED");
        assert_eq!(asset_count(&db), 0);
        let result = import_result(&db, &caller, &operation_ref).unwrap();
        assert_eq!(
            (result.imported, result.duplicates, result.failed),
            (0, 0, 1)
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].display_name, "ok.jpg");
        assert_eq!(
            result.failures[0].error_code.as_deref(),
            Some("PLAN_CHANGED")
        );
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains(&jpg.to_string_lossy().to_string()));
    }

    #[test]
    fn cancel_before_execution_marks_cancelled_without_importing() {
        let managed = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("ok.jpg");
        write_jpg(&jpg, 8);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data_dir.path()).unwrap();

        let plan_ref = prepare(&db, &caller, &jpg);
        let operation_ref = accepted_operation(&db, &caller, &plan_ref, 1);
        let cancel = AtomicBool::new(true);
        let summary = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan_ref,
            &operation_ref,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.state, "cancelled");
        assert_eq!(summary.imported, 0);
        assert_eq!(asset_count(&db), 0);
    }
    #[test]
    fn operation_cannot_execute_another_import_plan() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("scope.jpg");
        write_jpg(&jpg, 20);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let approved = prepare(&db, &caller, &jpg);
        let other = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &approved, 1);
        assert_eq!(
            execute_import(
                &db,
                &thumbs,
                &caller,
                &other,
                &operation,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .code(),
            "PLAN_CHANGED"
        );
        assert_eq!(asset_count(&db), 0);
    }

    #[test]
    fn persistent_operation_cancel_is_honoured() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("cancel.jpg");
        write_jpg(&jpg, 22);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        assert_eq!(
            operations_service::OperationQueue::default()
                .claim_next(&db)
                .unwrap(),
            Some(operation.as_str().to_owned())
        );
        operations_service::cancel_operation(&db, &caller, &operation).unwrap();
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "cancelled");
        assert_eq!(asset_count(&db), 0);
    }

    #[test]
    fn cancel_after_publication_preserves_unknown_file_effect_receipt() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("published-cancel.jpg");
        write_jpg(&jpg, 23);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        operations_service::OperationQueue::default()
            .claim_next(&db)
            .unwrap();

        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |event| {
                if event.message.as_deref() == Some("publication_verified") {
                    operations_service::cancel_operation(&db, &caller, &operation).unwrap();
                }
            },
        )
        .unwrap();

        assert_eq!(result.state, "outcome_unknown");
        assert_eq!(asset_count(&db), 0);
        assert!(managed.path().join("published-cancel.jpg").exists());
        let conn = db.lock().unwrap();
        let step = operations_db::steps(&conn, operation.as_str())
            .unwrap()
            .remove(0);
        let item_id = import_plans::items(&conn, plan.as_str())
            .unwrap()
            .remove(0)
            .item_id;
        let item =
            operations_db::operation_item(&conn, operation.as_str(), &step.step_id, &item_id)
                .unwrap()
                .unwrap();
        assert_eq!(item.state, "outcome_unknown");
        assert_eq!(item.effect_state, "unknown");
        assert!(
            operations_db::receipts(&conn, operation.as_str(), &step.step_id)
                .unwrap()
                .iter()
                .any(|receipt| {
                    receipt.item_id.as_deref() == Some(item_id.as_str())
                        && receipt.event == "item_cancelled"
                        && receipt.effect_state == "unknown"
                })
        );
    }

    #[test]
    fn publication_before_database_recovers_without_source_or_second_copy() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("recover.jpg");
        write_jpg(&jpg, 24);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![encode_native_path(&jpg).unwrap()],
                collection: None,
                rename_pattern: Some("prefix_{原名}".into()),
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |event| {
                    if event.message.as_deref() == Some("publication_verified") {
                        panic!("injected publication checkpoint");
                    }
                },
            )
        }));
        assert!(interrupted.is_err());
        assert_eq!(asset_count(&db), 0);
        let stage_path = {
            let conn = db.lock().unwrap();
            let item = crate::db::import_plans::items(&conn, plan.as_str())
                .unwrap()
                .remove(0);
            let task_id = super::item_task_id(&operation, &item);
            PathBuf::from(
                crate::db::import_execution::intent(&conn, &task_id)
                    .unwrap()
                    .unwrap()
                    .staging_path
                    .unwrap(),
            )
        };
        let published_target = managed.path().join("prefix_recover.jpg");
        std::fs::hard_link(&published_target, &stage_path).unwrap();
        std::fs::remove_file(&jpg).unwrap();
        let recoveries = operations_service::reconcile_startup(&db).unwrap();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].unknown_items, 0);
        {
            let conn = db.lock().unwrap();
            let step = operations_db::steps(&conn, operation.as_str())
                .unwrap()
                .remove(0);
            let item_id = crate::db::import_plans::items(&conn, plan.as_str())
                .unwrap()
                .remove(0)
                .item_id;
            let item =
                operations_db::operation_item(&conn, operation.as_str(), &step.step_id, &item_id)
                    .unwrap()
                    .unwrap();
            assert_eq!(item.state, "running");
            assert_eq!(item.effect_state, "published");
            assert!(
                operations_db::receipts(&conn, operation.as_str(), &step.step_id)
                    .unwrap()
                    .iter()
                    .any(|receipt| receipt.event == "item_publication_reconciled")
            );
        }
        operations_service::resume_operation(&db, &caller, &operation).unwrap();
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "succeeded");
        assert_eq!(asset_count(&db), 1);
        assert!(published_target.exists());
        assert!(!managed.path().join("prefix_prefix_recover.jpg").exists());
        assert!(!managed.path().join("prefix_recover(1).jpg").exists());
        assert_eq!(
            std::fs::read_dir(managed.path().join(".import-stage"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn directory_publication_recovery_does_not_require_deleted_source_rebinding() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("directory-recover.jpg");
        write_jpg(&jpg, 25);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let grant = authorization::grant_selected_directory(
            &db,
            &caller,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: grant.directory_ref,
                paths: Vec::new(),
                collection: None,
                rename_pattern: Some("prefix_{原名}".into()),
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |event| {
                    if event.message.as_deref() == Some("publication_verified") {
                        panic!("injected directory publication checkpoint");
                    }
                },
            )
        }));
        assert!(interrupted.is_err());
        assert_eq!(asset_count(&db), 0);
        let (task_id, stage_path, published_target) = {
            let conn = db.lock().unwrap();
            let item = crate::db::import_plans::items(&conn, plan.as_str())
                .unwrap()
                .remove(0);
            let task_id = super::item_task_id(&operation, &item);
            let intent = crate::db::import_execution::intent(&conn, &task_id)
                .unwrap()
                .unwrap();
            (
                task_id,
                PathBuf::from(intent.staging_path.unwrap()),
                PathBuf::from(intent.target_path),
            )
        };
        std::fs::hard_link(&published_target, &stage_path).unwrap();
        {
            let conn = db.lock().unwrap();
            // Simulate a crash after the target hard link is visible but before
            // the publication identity can be committed to the journal.
            conn.execute(
                "UPDATE import_execution_journal
                    SET published_file_identity=NULL WHERE task_id=?1",
                [&task_id],
            )
            .unwrap();
        }
        std::fs::remove_file(&jpg).unwrap();
        operations_service::reconcile_startup(&db).unwrap();

        let resumed_caller = {
            let conn = db.lock().unwrap();
            authorization::build_caller_context(
                &conn,
                TrustedAdapterIdentity::Gui {
                    session_id: "session_import_exec_after_restart".to_string(),
                    instance_id: "instance_import_exec_after_restart".to_string(),
                },
                ResourceRef::generated("request"),
                Vec::new(),
            )
            .unwrap()
        };
        assert!(!operations_service::import_resume_requires_source(
            &db,
            &resumed_caller,
            &operation,
        )
        .unwrap());
        operations_service::resume_operation(&db, &resumed_caller, &operation).unwrap();
        crate::services::import_dispatch::drain(
            &db,
            &operations_service::OperationQueue::default(),
            &thumbs,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        let conn = db.lock().unwrap();
        let (asset_id, asset_path): (i64, String) = conn
            .query_row("SELECT id,file_path FROM assets", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(
            PathBuf::from(asset_path).canonicalize().unwrap(),
            published_target.canonicalize().unwrap()
        );
        assert!(published_target.exists());
        assert!(!managed
            .path()
            .join("prefix_prefix_directory-recover.jpg")
            .exists());
        let step = operations_db::steps(&conn, operation.as_str())
            .unwrap()
            .into_iter()
            .find(|step| step.capability == "imports.submit")
            .unwrap();
        let item = operations_db::items(&conn, operation.as_str(), &step.step_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(item.state, "succeeded");
        let expected_asset_ref = format!("asset:{asset_id}");
        let imported_receipt = operations_db::receipts(&conn, operation.as_str(), &step.step_id)
            .unwrap()
            .into_iter()
            .find(|receipt| receipt.event == "item_imported")
            .unwrap();
        assert_eq!(
            imported_receipt.item_id.as_deref(),
            Some(item.item_id.as_str())
        );
        assert_eq!(
            imported_receipt.db_ref.as_deref(),
            Some(expected_asset_ref.as_str())
        );
    }

    #[test]
    fn directory_resume_rebinds_only_items_that_still_need_source_bytes() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_jpg(&source.path().join("first.jpg"), 41);
        write_jpg(&source.path().join("second.jpg"), 42);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let grant = authorization::grant_selected_directory(
            &db,
            &caller,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: grant.directory_ref,
                paths: Vec::new(),
                collection: None,
                rename_pattern: Some("prefix_{原名}".into()),
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 2);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |event| {
                    if event.message.as_deref() == Some("publication_verified") {
                        panic!("injected first directory publication checkpoint");
                    }
                },
            )
        }));
        assert!(interrupted.is_err());
        assert_eq!(asset_count(&db), 0);

        let (task_id, original_name, stage_path, published_target) = {
            let conn = db.lock().unwrap();
            let mut published = None;
            for item in crate::db::import_plans::items(&conn, plan.as_str()).unwrap() {
                let task_id = super::item_task_id(&operation, &item);
                let Some(intent) = crate::db::import_execution::intent(&conn, &task_id).unwrap()
                else {
                    continue;
                };
                if intent.published_file_identity.is_some() {
                    published = Some((
                        task_id,
                        item.display_name,
                        PathBuf::from(intent.staging_path.unwrap()),
                        PathBuf::from(intent.target_path),
                    ));
                    break;
                }
            }
            published.expect("one directory item must have been published")
        };
        std::fs::hard_link(&published_target, &stage_path).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE import_execution_journal
                    SET published_file_identity=NULL WHERE task_id=?1",
                [&task_id],
            )
            .unwrap();
        }
        std::fs::remove_file(source.path().join(original_name)).unwrap();
        operations_service::reconcile_startup(&db).unwrap();

        let resumed_caller = {
            let conn = db.lock().unwrap();
            authorization::build_caller_context(
                &conn,
                TrustedAdapterIdentity::Gui {
                    session_id: "session_import_exec_mixed_restart".to_string(),
                    instance_id: "instance_import_exec_mixed_restart".to_string(),
                },
                ResourceRef::generated("request"),
                Vec::new(),
            )
            .unwrap()
        };
        assert!(operations_service::import_resume_requires_source(
            &db,
            &resumed_caller,
            &operation,
        )
        .unwrap());
        let resumed_grant = authorization::grant_selected_directory(
            &db,
            &resumed_caller,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        operations_service::resume_import_operation(
            &db,
            &resumed_caller,
            &operation,
            resumed_grant.directory_ref.as_ref().unwrap(),
        )
        .unwrap();
        crate::services::import_dispatch::drain(
            &db,
            &operations_service::OperationQueue::default(),
            &thumbs,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        assert_eq!(asset_count(&db), 2);
        assert!(published_target.exists());
        assert!(!managed.path().join("prefix_prefix_first.jpg").exists());
        assert!(!managed.path().join("prefix_prefix_second.jpg").exists());
    }

    #[test]
    fn database_before_receipt_recovers_real_asset_mapping() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("receipt.jpg");
        write_jpg(&jpg, 26);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |event| {
                    if event.phase == "done" {
                        panic!("injected receipt checkpoint");
                    }
                },
            )
        }));
        assert!(interrupted.is_err());
        assert_eq!(asset_count(&db), 1);
        std::fs::remove_file(&jpg).unwrap();
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "succeeded");
        assert_eq!(result.duplicates, 1);
        assert_eq!(asset_count(&db), 1);
    }
    #[test]
    fn successful_receipt_resume_does_not_require_original_source() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("finished.jpg");
        write_jpg(&jpg, 28);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        std::fs::remove_file(&jpg).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE operations SET state='queued',phase='ready' WHERE operation_id=?1",
                [operation.as_str()],
            )
            .unwrap();
        }
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "succeeded");
        assert_eq!(result.imported, 1);
        assert_eq!(asset_count(&db), 1);
    }

    #[test]
    fn mismatched_publication_is_unknown_and_never_overwritten() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("partial.jpg");
        write_jpg(&jpg, 30);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![encode_native_path(&jpg).unwrap()],
                collection: None,
                rename_pattern: Some("prefix_{原名}".into()),
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |event| {
                    if event.message.as_deref() == Some("publication_verified") {
                        panic!("injected publication checkpoint");
                    }
                },
            )
        }));
        let target = managed.path().join("prefix_partial.jpg");
        std::fs::write(&target, b"partial bytes").unwrap();
        let error = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code(), "OUTCOME_UNKNOWN", "{error:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"partial bytes");
        assert_eq!(asset_count(&db), 0);
        let conn = db.lock().unwrap();
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "outcome_unknown"
        );
    }

    #[test]
    fn authorized_directory_import_uses_resolved_native_path() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("authorized.jpg");
        write_jpg(&jpg, 32);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let grant = authorization::grant_selected_directory(
            &db,
            &caller,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: grant.directory_ref,
                paths: vec![],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: vec![],
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "succeeded");
        assert_eq!(result.imported, 1);
        assert!(managed.path().join("authorized.jpg").exists());
    }
    #[test]
    fn source_replacement_before_staging_is_rejected_without_registration() {
        use std::time::Duration;

        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("replace.jpg");
        write_jpg(&jpg, 34);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |event| {
                if event.phase == "processing" && event.message.is_none() {
                    write_jpg(&jpg, 200);
                    let source_file = std::fs::OpenOptions::new().write(true).open(&jpg).unwrap();
                    let modified = source_file.metadata().unwrap().modified().unwrap();
                    source_file
                        .set_times(
                            std::fs::FileTimes::new()
                                .set_modified(modified + Duration::from_secs(5)),
                        )
                        .unwrap();
                }
            },
        )
        .unwrap_err();
        assert_eq!(result.code(), "PLAN_CHANGED");
        assert_eq!(asset_count(&db), 0);
        assert!(!managed.path().join("replace.jpg").exists());
    }

    #[test]
    fn cancel_running_item_before_copy_does_not_become_unknown() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("live-cancel.jpg");
        write_jpg(&jpg, 36);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        operations_service::OperationQueue::default()
            .claim_next(&db)
            .unwrap();
        let result = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |event| {
                if event.phase == "processing" && event.message.is_none() {
                    operations_service::cancel_operation(&db, &caller, &operation).unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(result.state, "cancelled");
        assert_eq!(asset_count(&db), 0);
        assert!(!managed.path().join("live-cancel.jpg").exists());
    }

    #[test]
    fn cancelled_multi_item_operation_keeps_its_frozen_total() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let first = source.path().join("first.jpg");
        let second = source.path().join("second.jpg");
        write_jpg(&first, 40);
        write_jpg(&second, 41);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![
                    encode_native_path(&first).unwrap(),
                    encode_native_path(&second).unwrap(),
                ],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap()
        .plan_ref;
        let operation = accepted_operation(&db, &caller, &plan, 2);
        operations_service::OperationQueue::default()
            .claim_next(&db)
            .unwrap();
        let cancelled = AtomicBool::new(false);

        let summary = execute_import(
            &db,
            &thumbs,
            &caller,
            &plan,
            &operation,
            &AtomicBool::new(false),
            |event| {
                if event.phase == "processing" && !cancelled.swap(true, Ordering::SeqCst) {
                    operations_service::cancel_operation(&db, &caller, &operation).unwrap();
                }
            },
        )
        .unwrap();

        assert_eq!(summary.state, "cancelled");
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .total,
            Some(2)
        );
        assert_eq!(asset_count(&db), 0);
    }
    #[test]
    fn equal_bytes_without_created_file_identity_are_not_adopted() {
        let managed = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let jpg = source.path().join("collision.jpg");
        write_jpg(&jpg, 38);
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let thumbs = ThumbnailService::new(data.path()).unwrap();
        let plan = prepare(&db, &caller, &jpg);
        let operation = accepted_operation(&db, &caller, &plan, 1);
        let item = {
            let conn = db.lock().unwrap();
            import_plans::items(&conn, plan.as_str()).unwrap().remove(0)
        };
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        let target = managed_library::verified_root_path(&library)
            .unwrap()
            .join("collision.jpg");
        {
            let conn = db.lock().unwrap();
            crate::db::import_execution::record_intent(
                &conn,
                &item_task_id(&operation, &item),
                &encode_native_path(&target).unwrap(),
                &encode_native_path(&managed.path().join(".import-stage").join("collision.part"))
                    .unwrap(),
                &sha256_full(&jpg).unwrap(),
            )
            .unwrap();
        }
        // A competing creator wins after intent but before create_new.
        std::fs::copy(&jpg, &target).unwrap();
        let source_error =
            execution_source(&db, &caller, &operation, &item, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(source_error.code(), "OUTCOME_UNKNOWN");
        assert_eq!(
            execute_import(
                &db,
                &thumbs,
                &caller,
                &plan,
                &operation,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .code(),
            "OUTCOME_UNKNOWN"
        );
        assert_eq!(asset_count(&db), 0);
        assert_eq!(sha256_full(&target).unwrap(), sha256_full(&jpg).unwrap());
        let conn = db.lock().unwrap();
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "outcome_unknown"
        );
    }
}
