//! A4-08：常用流程实例化命令层（薄壳）
//!
//! 只做状态编排：构建 GUI 可信 caller、检查迁仓期禁写、调用服务、抢占长任务租约、
//! 把真实文件操作丢进阻塞线程。业务规则全部在 [`crate::services::workflow_run`]。

use tauri::State;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::workflow_run::{
    self, WorkflowInstanceInputs, WorkflowInstancePreviewView, WorkflowRunReport, WorkflowRunStart,
};
use crate::state::AppState;

fn gui_caller(state: &AppState) -> AppResult<CallerContext> {
    let conn = state.db.lock()?;
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: state.authorization_session_id.clone(),
            instance_id: state.authorization_instance_id.clone(),
        },
        ResourceRef::generated("request"),
        Vec::new(),
    )
}

/// 准备：只生成本次计划与预览，不执行、不写文件。
#[tauri::command]
pub async fn prepare_workflow_instance(
    state: State<'_, AppState>,
    workflow_ref: String,
    version: i64,
    inputs: WorkflowInstanceInputs,
) -> AppResult<WorkflowInstancePreviewView> {
    let caller = gui_caller(&state)?;
    let db = state.db.clone();
    workflow_run::prepare_instance(&db, &caller, &workflow_ref, version, &inputs)
}

/// 确认后受理并返回稳定句柄。用户在对话框点了「确认并开始」，这就是 GUI 的可信确认。
///
/// 与旧实现的关键差别：**不在命令层等待整次执行**。用户点下之后立刻拿到
/// `operationId`，前端改用 [`workflow_run_instance_report`] 轮询进度，因此「运行中
/// 取消」才有可能发生 —— 旧实现在 `spawn_blocking(...).await` 里阻塞，前端在跑完
/// 之前根本拿不到 operation id，只能等出结果之后才看得到「取消任务」按钮。
#[tauri::command]
pub async fn run_workflow_instance(
    state: State<'_, AppState>,
    plan_id: String,
) -> AppResult<WorkflowRunStart> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "执行常用流程")?;
    let caller = gui_caller(&state)?;
    let db = state.db.clone();
    let queue = state.operation_queue.clone();

    let submitted = workflow_run::submit_instance(&db, &caller, &plan_id)?;
    let start = workflow_run::start_view(&db, &submitted)?;

    if submitted.replayed && start.operation_state != "queued" {
        // 同一份计划此前已经提交过、并且已经离开排队态。这时再抢队列毫无意义：
        // 已终态的 operation 永远不符合 `is_queued_for_dispatch`，抢它只会把
        // 「重试查看上一次结果」报成「队列忙」；已 running 的则说明有执行者在跑，
        // 再起一个会双重执行。
        return Ok(start);
    }

    let operation_id = submitted.operation_id.clone();
    let lease = queue
        .try_reserve_operation(&db, &operation_id, "workflows.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或流程操作尚未排到队首，请稍后重试")
        })?;

    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        // 执行线程不把结果送回 UI：真实终态一律由前端轮询数据库读回，避免线程
        // panic/超时让 UI 永远停在「正在执行」。
        match workflow_run::execute_instance(db, caller, submitted) {
            Ok(report) => tracing::info!(
                operation = "workflow_run",
                operation_id = %report.operation_id,
                state = %report.operation_state,
                "流程执行结束"
            ),
            Err(error) => tracing::warn!(
                operation = "workflow_run",
                stage = "execute_failed",
                error = %error,
                "流程执行中止"
            ),
        }
    });

    Ok(start)
}

/// 轮询一次流程实例的真实状态。运行期进度、取消后的残留项都由它读出。
#[tauri::command]
pub async fn workflow_run_instance_report(
    state: State<'_, AppState>,
    plan_id: String,
) -> AppResult<WorkflowRunReport> {
    let caller = gui_caller(&state)?;
    let db = state.db.clone();
    workflow_run::instance_report(&db, &caller, &plan_id)
}

/// 取消：只停止后续步骤，已完成步骤保持原样，不做隐式回滚。
#[tauri::command]
pub async fn cancel_workflow_instance(
    state: State<'_, AppState>,
    operation_id: String,
) -> AppResult<()> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "取消常用流程")?;
    let caller = gui_caller(&state)?;
    let db = state.db.clone();
    workflow_run::cancel_instance(&db, &caller, &operation_id)
}
