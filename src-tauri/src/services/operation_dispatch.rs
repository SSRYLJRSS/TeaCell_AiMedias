//! A4-02：把「已批准并已提交」的操作真正执行掉。
//!
//! ## 为什么需要这一层
//!
//! 提交一个写能力的计划（`agent_tools::submit_*`）只做三件事：把冻结的输入写进
//! `operation_steps` / `operation_items`、把 operation 置为 `queued/ready`、返回快照。
//! **它从不触碰文件系统**——这是刻意的：数据库事务无法回滚已经落盘的文件。
//!
//! 在 2.0 的每一条 GUI 命令里，文件效果由命令自己在 `spawn_blocking` 里完成
//! （`move_assets` / `export_local_files` / `restore_assets` / `empty_trash` /
//! `run_workflow_instance`）。但 Agent（Pi）只走到 `submit_*`，没有任何一层替它
//! 继续——结果是模型提交的每一个写入操作都永远停在 `queued`。本模块补上这一层，
//! 并保持既定的三段式语义：
//!
//! 1. 准备（Pi 或 GUI 生成待审计划）；
//! 2. 批准（`plans::approve_plan`，`PlansApprove` 是 GUI 独占能力）；
//! 3. **显式开始**（本模块，由 GUI 点击「开始执行」触发）。
//!
//! ## 三条不可动摇的约束
//!
//! - **执行身份来自持久行，不来自 IPC 参数**。`require_owner`（`services::operations`）
//!   要求 principal/session/instance 与本操作的持久记录逐字相等，所以 GUI 无法
//!   替 Pi 的操作写收据。这里按 `plan.origin_kind` 重建创建者的 `CallerContext`，
//!   与原先只服务导入的 `import_submission::execution_context` 同一手法（后者已删）——不新增任何权限，也不
//!   放大授权：请求仍然只能作用于「已经批准、已经提交、且排到队首」的那一个操作。
//! - **不静默后台排空**。只有用户点「开始执行」才会跑，模型无法自行产生文件效果；
//!   后台排空器还会与 GUI 内联执行抢同一个单槽队列。
//! - **终态必须如实**。部分失败落 `partially_succeeded`，永久清空出现 `unknown`
//!   项落 `outcome_unknown`，绝不把「没法证明成功」写成成功。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::contracts::agent::{
    AgentErrorCode, CallerKind, CapabilityId, ExportLayout, OperationState, ResourceRef,
};
use crate::contracts::caller::CallerContext;
use crate::db::export;
use crate::db::export_items::{self, NewExportItem};
use crate::db::libraries::{self, LibraryHealth, LibrarySnapshot};
use crate::db::operations as operations_db;
use crate::db::plans as plans_db;
use crate::error::{AppError, AppResult};
use crate::services::export_local::{self, ExportProgress, ExportRun};
use crate::services::plans as plan_service;
use crate::services::workflow_run::{self, SubmittedInstance, WorkflowRunSnapshot};
use crate::services::{agent_tools, library_move, operations, trash};
use crate::state::Database;

/// 取消注册表：与 `AppState::export_cancel` 是同一份结构。
///
/// 导出是唯一需要进程内原子标志的领域（`export_local` 只读 `AtomicBool`），
/// 其余领域在执行前每项都会查一次数据库里的 `cancelling`。
pub type ExportCancelRegistry = Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>;

/// 一次「开始执行」请求，在持有队列槽位后执行。
///
/// 冻结输入全部来自已批准计划的 `details_json`；IPC 参数只提供 operation 引用。
pub enum DispatchKind {
    Move(Vec<library_move::MovePlanItem>),
    Restore(Vec<i64>),
    Empty(Vec<i64>),
    Export {
        destination_ref: ResourceRef,
        layout: ExportLayout,
        asset_ids: Vec<i64>,
    },
    Workflow(Box<SubmittedInstance>),
}

pub struct DispatchPlan {
    operation_id: String,
    operation_ref: ResourceRef,
    step_ref: Option<ResourceRef>,
    capability_key: &'static str,
    kind: DispatchKind,
}

impl DispatchPlan {
    pub fn capability_str(&self) -> &'static str {
        self.capability_key
    }
}

fn library_snapshot(conn: &rusqlite::Connection) -> AppResult<LibrarySnapshot> {
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    Ok(library)
}

/// 读取并校验「这个操作现在可以开始执行」。
///
/// 校验刻意偏保守：状态必须是 `queued/ready`、必须还有 queued 步骤、库与代次必须
/// 仍是当前值。任何一项不符都直接失败，而不是「尽量执行」——一个 `succeeded` 的
/// 操作被重复执行会真的再搬一次文件。
pub fn plan_for_operation(db: &Database, operation_ref: &ResourceRef) -> AppResult<DispatchPlan> {
    let (record, plan, step_ref) = {
        let conn = db.lock()?;
        let record = plans_db::operation(&conn, operation_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.state != "queued" || record.phase != "ready" {
            return Err(AppError::invalid_arg(
                "这个任务不在待执行状态：可能已经开始、已经结束，或被取消。",
            ));
        }
        let plan = plans_db::plan(&conn, &record.plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let library = library_snapshot(&conn)?;
        if record.library_id != library.library_id || record.library_epoch != library.library_epoch
        {
            return Err(AppError::from(AgentErrorCode::LibraryChanged));
        }
        let steps = operations_db::steps(&conn, &record.operation_id)?;
        let first = steps
            .iter()
            .find(|step| step.state == "queued")
            .ok_or_else(|| AppError::invalid_arg("这个任务没有待执行的步骤。"))?;
        let step_ref = ResourceRef::from_trusted(first.step_id.clone())?;
        (record, plan, Some(step_ref))
    };

    let capability = serde_json::from_value::<CapabilityId>(serde_json::Value::String(
        record.capability.clone(),
    ))
    .map_err(|_| AppError::from(AgentErrorCode::UnsupportedCapability))?;
    let plan_ref = ResourceRef::from_trusted(plan.plan_id.clone())?;
    let capability_key = capability_str(capability);

    let kind = match capability {
        CapabilityId::AssetsMoveSubmit => {
            DispatchKind::Move(agent_tools::move_plan_items(db, &plan_ref)?)
        }
        CapabilityId::AssetsRestoreSubmit => {
            DispatchKind::Restore(agent_tools::restore_plan_items(db, &plan_ref)?)
        }
        CapabilityId::TrashEmptySubmit => {
            DispatchKind::Empty(agent_tools::empty_plan_items(db, &plan_ref)?)
        }
        CapabilityId::ExportsSubmit => {
            let frozen = agent_tools::export_plan_items(db, &plan_ref)?;
            DispatchKind::Export {
                destination_ref: frozen.destination_ref,
                layout: frozen.layout,
                asset_ids: frozen.asset_ids,
            }
        }
        CapabilityId::WorkflowsSubmit => {
            let snapshot: WorkflowRunSnapshot = workflow_run::frozen_snapshot(db, &plan_ref)?;
            DispatchKind::Workflow(Box::new(SubmittedInstance {
                plan_id: plan.plan_id.clone(),
                operation_id: record.operation_id.clone(),
                operation_ref: ResourceRef::from_trusted(record.operation_id.clone())?,
                snapshot,
                // 走到这里说明 operation 已经在库里、这次只是把它跑起来，
                // 不属于「刚受理」，因此按重放看待。
                replayed: true,
            }))
        }
        // 移入回收站是提交即完成（`submit_trash` 在 effect 里就软删并结算），导入由
        // `import_dispatch` 排空，打标由 AI 批处理驱动：它们都不该出现在这里。
        _ => return Err(AppError::from(AgentErrorCode::UnsupportedCapability)),
    };

    Ok(DispatchPlan {
        operation_id: record.operation_id.clone(),
        operation_ref: ResourceRef::from_trusted(record.operation_id)?,
        step_ref,
        capability_key,
        kind,
    })
}

fn capability_str(capability: CapabilityId) -> &'static str {
    // 契约里的稳定字符串，用于队列队首判定（`is_queued_for_dispatch` 按它比对）。
    capability.as_str()
}

/// 按操作创建者的身份重建执行上下文，绝不接受调用方传入的身份。
///
/// 这是原先 `import_submission::execution_context` 的泛化版本：那边只服务导入并硬编码
/// `principal_gui_host`（该函数已删除，导入排空也改用本函数），这里按计划的
/// `origin_kind` 还原 GUI / Pi / MCP。
/// 关键点是 principal/session/instance/grant_epoch 全部取自持久行——GUI 打不开
/// 也不会获得 Pi 的权限，重建出来的上下文只能通过 `require_owner` 那一个操作。
pub fn execution_caller(db: &Database, operation_ref: &ResourceRef) -> AppResult<CallerContext> {
    let conn = db.lock()?;
    let op = plans_db::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let plan = plans_db::plan(&conn, &op.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let kind = match plan.origin_kind.as_deref() {
        Some("pi") => CallerKind::Pi,
        Some("mcp") => CallerKind::Mcp,
        Some("gui") | None => CallerKind::Gui,
        Some(_) => return Err(AppError::internal("持久计划来源损坏")),
    };
    // 库仍必须是持久行记录的那一代：执行期间换库会让冻结的路径全部失效。
    let library = library_snapshot(&conn)?;
    if op.library_id != library.library_id || op.library_epoch != library.library_epoch {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(CallerContext {
        kind,
        principal_id: ResourceRef::from_trusted(op.principal_id)?,
        session_id: plan.session_id.map(ResourceRef::from_trusted).transpose()?,
        instance_id: ResourceRef::from_trusted(plan.instance_id)?,
        request_id: ResourceRef::generated("request"),
        grant_epoch: ResourceRef::from_trusted(plan.grant_epoch)?,
        library: crate::contracts::agent::LibraryScope {
            library_id: ResourceRef::from_trusted(op.library_id)?,
            library_epoch: ResourceRef::from_trusted(op.library_epoch)?,
        },
        origin_chain: Vec::new(),
    })
}

/// 执行一个待开始的操作，返回结算后的快照。
///
/// 调用方（`commands::operations_cmd::start_operation`）负责把它放进
/// `spawn_blocking`，本函数内部是同步阻塞的文件 IO。
pub fn start(
    db: &Arc<Database>,
    operation_ref: &ResourceRef,
    queue: &Arc<operations::OperationQueue>,
    export_cancel: &ExportCancelRegistry,
    on_progress: &dyn Fn(ExportProgress),
) -> AppResult<crate::contracts::agent::OperationSnapshot> {
    let plan = plan_for_operation(db, operation_ref)?;
    let caller = execution_caller(db, operation_ref)?;
    // 队首判定与单槽占用：另一个长任务正在跑时直接告诉用户，而不是排在后面装作
    // 已经开始。`try_reserve_operation` 同时校验「本操作是该能力的队首」。
    let lease = queue
        .try_reserve_operation(db, &plan.operation_id, plan.capability_str())?
        .ok_or_else(|| {
            AppError::conflict(
                "队列里还有更早的任务没有开始，或另一个长任务正在执行：请先在任务卡里开始（或取消）前面的任务，再回来执行这一个。",
            )
        })?;
    let _lease = lease;

    let outcome = run_kind(db, &caller, &plan, export_cancel, on_progress);
    match outcome {
        Ok(()) => {}
        Err(error) => {
            // 中途硬失败不能把 operation 留在 `running`，否则恢复链会一直等一个
            // 已经不存在的执行者。执行器自己抛错时状态可能还没被改过，这里先读
            // 一次再决定是否补结算。
            settle_failed(db, &caller, &plan, &error)?;
            return Err(error);
        }
    }
    read_snapshot(db, &caller, &plan.operation_id)
}

fn read_snapshot(
    db: &Database,
    caller: &CallerContext,
    operation_id: &str,
) -> AppResult<crate::contracts::agent::OperationSnapshot> {
    let conn = db.lock()?;
    let record = plans_db::operation(&conn, operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    plan_service::operation_snapshot(&conn, caller, &record)
}

fn still_open(db: &Database, operation_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    let record = plans_db::operation(&conn, operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    Ok(record.state == "queued" || record.state == "running")
}

/// 硬失败补齐结算：只有在操作仍未结束时才落 `failed`。
///
/// 某些失败路径（例如 `workflow_run::execute_instance`）已经自己结算过，重复结算
/// 会把 `partially_succeeded` 覆盖成 `failed`，反而抹掉真实结果。
fn settle_failed(
    db: &Database,
    caller: &CallerContext,
    plan: &DispatchPlan,
    error: &AppError,
) -> AppResult<()> {
    if !still_open(db, &plan.operation_id)? {
        return Ok(());
    }
    match operations::finish_operation(db, caller, &plan.operation_ref, OperationState::Failed) {
        Ok(_) => Ok(()),
        Err(settle) => {
            tracing::error!(
                operation_id = %plan.operation_id,
                settle_error = %settle,
                original_error = %error,
                "失败后结算操作状态失败"
            );
            Ok(())
        }
    }
}

fn run_kind(
    db: &Arc<Database>,
    caller: &CallerContext,
    plan: &DispatchPlan,
    export_cancel: &ExportCancelRegistry,
    on_progress: &dyn Fn(ExportProgress),
) -> AppResult<()> {
    match &plan.kind {
        DispatchKind::Move(items) => run_move(db, caller, plan, items),
        DispatchKind::Restore(asset_ids) => run_restore(db, caller, plan, asset_ids),
        DispatchKind::Empty(asset_ids) => run_empty(db, caller, plan, asset_ids),
        DispatchKind::Export {
            destination_ref,
            layout,
            asset_ids,
        } => run_export(
            db,
            caller,
            plan,
            destination_ref,
            *layout,
            asset_ids,
            export_cancel,
            on_progress,
        ),
        DispatchKind::Workflow(submitted) => {
            // `execute_instance` 自行收尾（含 cancelled / overall_state），
            // 这里绝不能再结算一次。
            workflow_run::execute_instance(Arc::clone(db), caller.clone(), (**submitted).clone())?;
            Ok(())
        }
    }
}

fn step_ref_of(plan: &DispatchPlan) -> AppResult<&ResourceRef> {
    plan.step_ref
        .as_ref()
        .ok_or_else(|| AppError::internal("操作缺少步骤"))
}

fn run_move(
    db: &Arc<Database>,
    caller: &CallerContext,
    plan: &DispatchPlan,
    items: &[library_move::MovePlanItem],
) -> AppResult<()> {
    let library = {
        let conn = db.lock()?;
        library_snapshot(&conn)?
    };
    let step_ref = step_ref_of(plan)?;
    let cancelled = AtomicBool::new(false);
    let run = library_move::MoveExecution {
        operation_id: &plan.operation_id,
        operation_ref: &plan.operation_ref,
        step_ref,
        items,
        cancelled: &cancelled,
    };
    let summary = library_move::execute(db, caller, &library, run)?;
    if summary.cancelled {
        operations::finish_cancelled(db, caller, &plan.operation_ref)?;
        return Ok(());
    }
    let state = if summary.unknown > 0 {
        // 未知项意味着文件可能已经搬走而记录没跟上：宣称 partially_succeeded 会误导
        // 用户以为「重试剩下的就能恢复」，所以必须先落 outcome_unknown（同 run_empty）。
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.moved == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, &plan.operation_ref, state)?;
    Ok(())
}

fn run_restore(
    db: &Arc<Database>,
    caller: &CallerContext,
    plan: &DispatchPlan,
    asset_ids: &[i64],
) -> AppResult<()> {
    let library = {
        let conn = db.lock()?;
        library_snapshot(&conn)?
    };
    let step_ref = step_ref_of(plan)?;
    let cancelled = AtomicBool::new(false);
    let run = trash::RestoreExecution {
        operation_id: &plan.operation_id,
        operation_ref: &plan.operation_ref,
        step_ref,
        asset_ids,
        cancelled: &cancelled,
    };
    let summary = trash::execute(db, caller, &library, run)?;
    if summary.cancelled {
        operations::finish_cancelled(db, caller, &plan.operation_ref)?;
        return Ok(());
    }
    let state = if summary.unknown > 0 {
        // 不可证实的效果优先：可能已恢复的项不能被压成「失败」。
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.restored == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, &plan.operation_ref, state)?;
    Ok(())
}

fn run_empty(
    db: &Arc<Database>,
    caller: &CallerContext,
    plan: &DispatchPlan,
    asset_ids: &[i64],
) -> AppResult<()> {
    let library = {
        let conn = db.lock()?;
        library_snapshot(&conn)?
    };
    let step_ref = step_ref_of(plan)?;
    let cancelled = AtomicBool::new(false);
    let run = trash::EmptyExecution {
        operation_id: &plan.operation_id,
        operation_ref: &plan.operation_ref,
        step_ref,
        asset_ids,
        cancelled: &cancelled,
    };
    let summary = trash::empty(db, caller, &library, run)?;
    if summary.cancelled {
        operations::finish_cancelled(db, caller, &plan.operation_ref)?;
        return Ok(());
    }
    // `unknown` 项意味着文件可能已删而记录未同步，宣称部分成功会误导用户以为
    // 剩下的重试就能恢复，所以必须落 `outcome_unknown`。
    let state = if summary.unknown > 0 {
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.missing == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.removed == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, &plan.operation_ref, state)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_export(
    db: &Arc<Database>,
    caller: &CallerContext,
    plan: &DispatchPlan,
    destination_ref: &ResourceRef,
    layout: ExportLayout,
    asset_ids: &[i64],
    export_cancel: &ExportCancelRegistry,
    on_progress: &dyn Fn(ExportProgress),
) -> AppResult<()> {
    let step_ref = step_ref_of(plan)?;
    // 目标根目录从授权句柄重新解析：不信任任何冻结的裸路径，也顺带在复制前再校验
    // 一次授权代次与文件系统身份。
    let path =
        crate::services::authorization::resolve_export_directory(db, caller, destination_ref)?;
    let dest_dir = crate::utils::path::encode_native_path(&path)?;
    let task = {
        let conn = db.lock()?;
        export::create_task(
            &conn,
            "local",
            asset_ids.len() as i64,
            Some(&dest_dir),
            None,
        )?
    };
    let cancel = Arc::new(AtomicBool::new(false));
    export_cancel
        .lock()
        .map_err(|_| AppError::msg("取消注册表锁中毒"))?
        .insert(task.id, Arc::clone(&cancel));

    let result = {
        let mut on_item = |row: &NewExportItem| -> AppResult<()> {
            // 数据库侧的 `cancelling` 只能靠逐项轮询转达给 `export_local`
            // （它只读进程内原子标志）。取消后剩余项会由 finish_cancelled 统一结算。
            if !cancel.load(Ordering::Relaxed) {
                let conn = db.lock()?;
                if let Some(record) = plans_db::operation(&conn, &plan.operation_id)? {
                    if record.phase == "cancelling" || record.state == "cancelled" {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
            let Some(asset_id) = row.asset_id else {
                return Ok(());
            };
            let (item_state, effect_state, error_code, event) = match row.state.as_str() {
                export_items::STATE_COPIED => ("succeeded", "published", None, "copied"),
                export_items::STATE_FAILED => {
                    ("failed", "none", row.error_code.as_deref(), "copy_failed")
                }
                // 取消后补齐的 skipped 项交给收尾统一结算，避免同一 item 出现
                // 两条互相矛盾的终态收据。
                _ => return Ok(()),
            };
            let item_ref = ResourceRef::from_trusted(agent_tools::export_item_id(asset_id))?;
            operations::record_item_state(
                db,
                caller,
                &plan.operation_ref,
                step_ref,
                &item_ref,
                item_state,
                effect_state,
                error_code,
                &ResourceRef::generated("receipt"),
                event,
                row.dest_path.as_deref(),
            )
        };
        let run = ExportRun {
            task_id: task.id,
            asset_ids,
            dest_dir: &dest_dir,
            layout: layout_key(layout),
            cancel: &cancel,
        };
        export_local::export_local(db, run, on_progress, &mut on_item)
    };

    match export_cancel.lock() {
        Ok(mut registry) => {
            registry.remove(&task.id);
        }
        Err(_) => tracing::error!("取消注册表锁中毒，task {} 的 flag 未清理", task.id),
    }

    let summary = result?;
    if summary.cancelled {
        operations::finish_cancelled(db, caller, &plan.operation_ref)?;
        return Ok(());
    }
    let state = if summary.failed == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.copied == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, &plan.operation_ref, state)?;
    Ok(())
}

fn layout_key(layout: ExportLayout) -> &'static str {
    match layout {
        ExportLayout::Flat => "flat",
        ExportLayout::ByTag => "by_tag",
        ExportLayout::ByDate => "by_date",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dispatched_capability_has_a_contract_id() {
        // 队列队首判定按契约字符串比对，能力与字符串一旦脱钩，`start` 会在
        // 队首检查处永远返回 None，表现为「点了开始执行但什么都没发生」。
        for capability in [
            CapabilityId::AssetsMoveSubmit,
            CapabilityId::AssetsRestoreSubmit,
            CapabilityId::TrashEmptySubmit,
            CapabilityId::ExportsSubmit,
            CapabilityId::WorkflowsSubmit,
        ] {
            let key = capability_str(capability);
            assert!(!key.is_empty());
            assert!(key.ends_with(".submit"));
        }
    }

    #[test]
    fn submit_only_capabilities_without_an_executor_are_rejected() {
        // 移入回收站是提交即完成、导入有独立排空器、打标由批处理驱动：
        // 它们出现在执行器里说明接线错了，必须显式拒绝而不是静默成功。
        for capability in [
            CapabilityId::AssetsTrashSubmit,
            CapabilityId::ImportsSubmit,
            CapabilityId::TaggingSubmit,
        ] {
            let kind = match capability {
                CapabilityId::AssetsMoveSubmit
                | CapabilityId::AssetsRestoreSubmit
                | CapabilityId::TrashEmptySubmit
                | CapabilityId::ExportsSubmit
                | CapabilityId::WorkflowsSubmit => "dispatchable",
                _ => "rejected",
            };
            assert_eq!(kind, "rejected");
        }
    }

    #[test]
    fn layout_keys_match_the_export_service_expectations() {
        assert_eq!(layout_key(ExportLayout::Flat), "flat");
        assert_eq!(layout_key(ExportLayout::ByTag), "by_tag");
        assert_eq!(layout_key(ExportLayout::ByDate), "by_date");
    }
}
