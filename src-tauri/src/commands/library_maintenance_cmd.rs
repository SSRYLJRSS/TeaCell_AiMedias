//! 总仓维护（A4-05）的命令层：软件迁移整个总仓 / 核验用户已搬好的总仓。
//!
//! 三个硬边界，全部来自卡片与实现契约：
//!
//! 1. **不注册成 Agent 工具**。`plans/v2/06-permissions-and-data-safety.md` §7.2 把
//!    迁仓定为可信手动维护能力；这些命令不进 `contracts::agent_enums` 的能力表，
//!    因此模型无法通过 `capability.request` 触达它们。
//! 2. **不信任前端路径**。`start` 重新跑一遍预检，用重新推导出的清单与目标；
//!    前端传回的 `target_root` 只用于对齐取消标志，不参与任何路径决定——真正
//!    写入的目标根由服务层重新 canonicalize 并与库定位交叉校验。
//! 3. **不自动清理源目录**。A0-05 冻结的协议是「核验通过后才允许处置来源」，而
//!    处置本身需要用户在可信界面明示；本命令只报告残留（见 `MigrationReport.residue`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::State;

use crate::db::library_migration;
use crate::error::{AppError, AppResult};
use crate::services::library_migration::{self as migration, MigrationPreflight, MigrationReport};
use crate::state::AppState;

/// 迁移预检视图。与 `MigrationPreflight` 同一形状，单独定义是为了让前端拿到
/// 一个稳定的 camelCase 契约，而服务层结构体可以随实现调整。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMigrationPreflightView {
    pub run_id: String,
    pub kind: String,
    pub library_id: String,
    pub source_root: String,
    pub target_root: String,
    pub total_items: u32,
    pub total_bytes: u64,
    pub target_free_bytes: Option<u64>,
    pub notes: Vec<String>,
    pub warnings: Vec<MigrationWarningView>,
    /// A4-06：卡片第 1 条要求预检区展示「任务影响」。
    pub impact: MigrationTaskImpactView,
}

/// 「开始迁移会影响到哪些任务」的投影。
///
/// 语义要点：**维护期不会静默取消任何任务**。`blocking` 为真时开始按钮被禁用，
/// 用户必须自己先结束或取消那些任务；这是卡片「先检查后允许切换」的落点之一，
/// 也让「迁移把别人的打标结果弄坏」这类事故不可能由一次点击产生。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationTaskImpactView {
    /// 当前总仓代次下没有终态的操作（排队 / 执行中 / 被中断 / 结果不明）。
    pub running_operations: i64,
    pub running_by_capability: Vec<MigrationCapabilityCountView>,
    /// 仍在排队或处理中的 AI 打标批次。
    pub pending_ai_batches: i64,
    pub import_running: bool,
    pub refill_running: bool,
    pub running_exports: i64,
    /// 任一影响项存在时为真：此时不允许开始迁移。
    pub blocking: bool,
    /// 逐条中文说明，界面直接展示，不自行拼句。
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationCapabilityCountView {
    pub capability: String,
    pub count: i64,
}

/// 维护期一定会被暂停的写入动作（展示用，不参与判断）。
const SUSPENDED_WRITE_HINT: &str =
    "迁移期间入库、复制导出、移动、删除/清空回收站、AI 打标和设置保存都会被暂停；浏览、搜索与旧会话仍然可用。";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationWarningView {
    pub code: String,
    pub message: String,
}

impl LibraryMigrationPreflightView {
    fn with_impact(preflight: MigrationPreflight, impact: MigrationTaskImpactView) -> Self {
        Self {
            run_id: preflight.run_id,
            kind: preflight.kind,
            library_id: preflight.library_id,
            source_root: preflight.source_root,
            target_root: preflight.target_root,
            total_items: preflight.total_items,
            total_bytes: preflight.total_bytes,
            target_free_bytes: preflight.target_free_bytes,
            notes: preflight.notes,
            warnings: preflight
                .warnings
                .into_iter()
                .map(|warning| MigrationWarningView {
                    code: warning.code,
                    message: warning.message,
                })
                .collect(),
            impact,
        }
    }
}

/// 维护态投影：界面据此显示「正在迁移」横幅并禁写，而不是让用户撞 CONFLICT。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMaintenanceStatusView {
    pub active: bool,
    pub reason: Option<String>,
    pub migration: Option<MigrationReport>,
}

/// 显式拒绝维护期内被调用的写命令。
///
/// 读命令不调它——卡片要求「读和旧会话继续可用，只停写入与自动清理」。
pub fn ensure_writable(state: &AppState, action: &str) -> AppResult<()> {
    state.library_maintenance.ensure_writable(action)
}

/// 统计「开始迁移会影响到哪些任务」。
///
/// 必须同时看两个来源，任一都不能省：
///
/// * 进程内标志（`import_running`、`refill_running`、`export_cancel`、
///   `ai_cancel`）——覆盖那些还没在数据库里留下可查询行的短任务；
/// * 数据库里的未终态 `operations` 与 AI 批次——覆盖 WebView 重载后进程内标志
///   已丢失、但任务仍在库里的情形（这正是「重启后不能新建重复迁移」依赖的部分）。
pub fn task_impact(state: &AppState) -> AppResult<MigrationTaskImpactView> {
    let library = {
        let conn = state.db.lock()?;
        crate::db::libraries::current(&conn)?
    };
    let (running_operations, running_by_capability, pending_ai_batches) = match library.as_ref() {
        Some(library) => {
            let conn = state.db.lock()?;
            let in_flight = crate::db::plans::in_flight_operations(
                &conn,
                &library.library_id,
                &library.library_epoch,
            )?;
            let batches = crate::db::ai::count_pending_or_processing(&conn)?;
            (
                in_flight.total,
                in_flight
                    .by_capability
                    .into_iter()
                    .map(|(capability, count)| MigrationCapabilityCountView { capability, count })
                    .collect(),
                batches,
            )
        }
        // 没有库定位时不可能开始迁移，影响面在别处报错；这里不假装有任务。
        None => (0, Vec::new(), 0),
    };
    let import_running = state.import_running.load(Ordering::Relaxed);
    let refill_running = state.refill_running.load(Ordering::Relaxed);
    let running_exports = state
        .export_cancel
        .lock()
        .map_err(|_| AppError::msg("导出取消标志表锁中毒"))?
        .len() as i64;

    let mut messages = Vec::new();
    if running_operations > 0 {
        let detail = running_by_capability
            .iter()
            .map(|row| format!("{} × {}", capability_label(&row.capability), row.count))
            .collect::<Vec<_>>()
            .join("、");
        messages.push(format!(
            "还有 {running_operations} 个未结束的任务（{detail}）。请等它们结束，或先取消后再开始迁移。"
        ));
    }
    if pending_ai_batches > 0 {
        messages.push(format!(
            "有 {pending_ai_batches} 个待处理或进行中的 AI 打标批次。请先取消批次后再开始迁移。"
        ));
    }
    if import_running {
        messages.push("文件入库进行中，请等它结束或取消后再开始迁移。".to_string());
    }
    if refill_running {
        messages.push("回填/色板任务进行中，请等它结束或取消后再开始迁移。".to_string());
    }
    if running_exports > 0 {
        messages.push(format!(
            "有 {running_exports} 个复制导出任务进行中，请等它结束或取消后再开始迁移。"
        ));
    }
    let blocking = running_operations > 0
        || pending_ai_batches > 0
        || import_running
        || refill_running
        || running_exports > 0;
    messages.push(SUSPENDED_WRITE_HINT.to_string());
    Ok(MigrationTaskImpactView {
        running_operations,
        running_by_capability,
        pending_ai_batches,
        import_running,
        refill_running,
        running_exports,
        blocking,
        messages,
    })
}

/// 能力 id → 用户可读名称。未知 id 原样显示，不猜。
fn capability_label(capability: &str) -> &str {
    match capability {
        "imports.submit" => "入库",
        "tagging.submit" => "AI 打标",
        "exports.submit" => "复制导出",
        "assets.move.submit" => "素材移动",
        "assets.trash.submit" => "移入回收站",
        "assets.restore.submit" => "恢复素材",
        "trash.empty.submit" => "永久清空回收站",
        other => other,
    }
}

/// 预检：检查目标、容量、嵌套与清单完整性，返回给用户确认的计划。
///
/// 只读，不写任何东西——`kind = relocate` 也会在预检阶段核验目标根是否带着
/// 我们自己的库身份标记（`prepare_root_for_migration(..., false)`），但**只核验、
/// 不创建**：现场补标记等于把任意空目录认领成「用户搬走的原库」。标记只由
/// 迁移本身在切根前的 `ensure_target_marker` 用 `create_new` 写下。
#[tauri::command]
pub fn preflight_library_migration(
    state: State<AppState>,
    target_root: String,
    kind: String,
) -> AppResult<LibraryMigrationPreflightView> {
    let kind = normalize_kind(&kind)?;
    ensure_writable(&state, "开始总仓迁移")?;
    if state.library_maintenance.is_active() {
        // ensure_writable 已经拒绝过；这里再取一次是给「无活动迁移但门被占」留余地。
        return Err(AppError::conflict(
            "总仓维护正在进行，请等它结束或收尾后再试",
        ));
    }
    let preflight =
        migration::preflight_migration(&state.db, std::path::Path::new(&target_root), &kind)?;
    // 影响面单独算：它是 AppState 与数据库的联合投影，不属于路径预检。
    let impact = task_impact(&state)?;
    Ok(LibraryMigrationPreflightView::with_impact(
        preflight, impact,
    ))
}

/// 只读地取一次任务影响。界面在向导打开时先查它，用户不必先选目标目录。
#[tauri::command]
pub fn library_task_impact(state: State<AppState>) -> AppResult<MigrationTaskImpactView> {
    task_impact(&state)
}

/// 卡片第 1 条「先检查后允许切换」的落点：预检给出的影响面不是装饰，开始前必须
/// 再查一次。用户可能在预检之后又启动了入库或打标，此时切换会让那些任务在换根后
/// 写到已失效的代次上。
fn ensure_startable(state: &AppState) -> AppResult<()> {
    let impact = task_impact(state)?;
    if !impact.blocking {
        return Ok(());
    }
    let reason = impact
        .messages
        .iter()
        .find(|message| message.as_str() != SUSPENDED_WRITE_HINT)
        .cloned()
        .unwrap_or_else(|| "还有未结束的任务，请先等它们结束或取消。".to_string());
    Err(AppError::conflict(format!("现在还不能开始迁移：{reason}")))
}

/// 开始（或继续）一次已确认的迁移。
///
/// `target_root` 与 `kind` 必须与预检一致：服务层会重建清单并与入参比对，不一致
/// 直接报 `LIBRARY_CHANGED` 而不是照着一个过期计划动文件。
#[tauri::command]
pub async fn start_library_migration(
    state: State<'_, AppState>,
    target_root: String,
    kind: String,
) -> AppResult<MigrationReport> {
    start_library_migration_with_state(&state, target_root, kind).await
}

/// 测试可直接调用的形态：`State<'_, T>` 无法在单元测试里构造。
async fn start_library_migration_with_state(
    state: &AppState,
    target_root: String,
    kind: String,
) -> AppResult<MigrationReport> {
    let kind = normalize_kind(&kind)?;
    ensure_writable(state, "开始总仓迁移")?;
    ensure_startable(state)?;
    let db = Arc::clone(&state.db);
    let gate = Arc::clone(&state.library_maintenance);
    let cancels = Arc::clone(&state.library_migration_cancel);
    let target = std::path::PathBuf::from(&target_root);
    let run_key = migration::cancel_key(&target);
    let cancelled = Arc::new(AtomicBool::new(false));
    cancels
        .lock()
        .map_err(|_| AppError::msg("迁移取消标志注册表锁中毒"))?
        .insert(run_key.clone(), Arc::clone(&cancelled));

    tauri::async_runtime::spawn_blocking(move || {
        let preflight = migration::preflight_migration(&db, &target, &kind)?;
        migration::run_migration(&db, &gate, &preflight, &cancelled)
    })
    .await
    .map_err(|error| AppError::msg(format!("总仓迁移线程异常: {error}")))?
}

/// 请求停止正在进行的迁移。
///
/// 只置取消标志：正在写的那一个文件会写完再停，绝不留下半截文件；已切换定位的
/// 阶段不允许取消——那时唯一安全的动作是把收尾做完（见 `resume_library_migration`）。
#[tauri::command]
pub fn cancel_library_migration(state: State<AppState>, target_root: String) -> AppResult<bool> {
    let key = migration::cancel_key(std::path::Path::new(&target_root));
    let cancels = state
        .library_migration_cancel
        .lock()
        .map_err(|_| AppError::msg("迁移取消标志注册表锁中毒"))?;
    match cancels.get(&key) {
        Some(flag) => {
            flag.store(true, Ordering::Release);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// 当前维护态 + 未结束的迁移。界面用它显示可恢复阶段。
#[tauri::command]
pub fn library_maintenance_status(
    state: State<AppState>,
) -> AppResult<LibraryMaintenanceStatusView> {
    Ok(LibraryMaintenanceStatusView {
        active: state.library_maintenance.is_active(),
        reason: state.library_maintenance.reason(),
        migration: migration::active_migration(&state.db)?,
    })
}

/// 读一次迁移的完整报告（含逐文件结果与失败项）。
#[tauri::command]
pub fn library_migration_report(
    state: State<AppState>,
    run_id: String,
) -> AppResult<MigrationReport> {
    migration::migration_report(&state.db, &run_id)
}

/// 收尾一次被中断的迁移。启动对账与界面「继续」都走这里。
#[tauri::command]
pub async fn resume_library_migration(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || migration::reconcile_interrupted_migration(&db))
        .await
        .map_err(|error| AppError::msg(format!("总仓迁移收尾线程异常: {error}")))?
}

fn normalize_kind(kind: &str) -> AppResult<String> {
    match kind {
        library_migration::KIND_MIGRATE => Ok(library_migration::KIND_MIGRATE.to_string()),
        library_migration::KIND_RELOCATE => Ok(library_migration::KIND_RELOCATE.to_string()),
        other => Err(AppError::invalid_arg(format!(
            "未知的总仓维护类型：{other}；只支持「软件迁移」(migrate) 与「重新定位」(relocate)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_state() -> (tempfile::TempDir, AppState) {
        let root = tempfile::tempdir().unwrap();
        let conn = crate::db::init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, root.path()).unwrap();
        let data_dir = root.path().to_path_buf();
        (root, AppState::new(conn, data_dir))
    }

    /// 在给定代次下塞一条指定状态的操作行。
    fn enqueue_operation(state: &AppState, capability: &str, operation_state: &str) {
        let library = {
            let conn = state.db.lock().unwrap();
            crate::db::libraries::current(&conn).unwrap().unwrap()
        };
        let conn = state.db.lock().unwrap();
        crate::db::plans::insert_operation(
            &conn,
            &crate::db::plans::NewOperation {
                operation_id: format!("op_{operation_state}_{capability}"),
                step_id: "step_1".to_string(),
                plan_id: "plan_1".to_string(),
                principal_id: "principal_test".to_string(),
                capability: capability.to_string(),
                idempotency_key: format!("idem_{operation_state}_{capability}"),
                library_id: library.library_id,
                library_epoch: library.library_epoch,
                state: operation_state.to_string(),
                phase: "ready".to_string(),
                revision: 1,
                total: Some(1),
                completed: 0,
                failed: 0,
                created_at: 100,
            },
        )
        .unwrap();
    }

    #[test]
    fn idle_library_reports_no_blocking_impact_and_states_what_pauses() {
        let (_root, state) = seeded_state();
        let impact = task_impact(&state).unwrap();
        assert_eq!(impact.running_operations, 0);
        assert!(!impact.blocking);
        assert!(impact.running_by_capability.is_empty());
        // 即使没有任何任务，界面也必须说清维护期会暂停哪些动作，否则用户会以为
        // 整个应用被停用。
        assert_eq!(impact.messages, vec![SUSPENDED_WRITE_HINT.to_string()]);
    }

    #[test]
    fn unsettled_operations_block_a_migration_and_are_named_in_chinese() {
        let (_root, state) = seeded_state();
        enqueue_operation(&state, "tagging.submit", "running");
        enqueue_operation(&state, "imports.submit", "queued");
        // 已终态的操作不算影响面：它不会再写任何东西。
        enqueue_operation(&state, "exports.submit", "succeeded");

        let impact = task_impact(&state).unwrap();
        assert_eq!(impact.running_operations, 2);
        assert!(impact.blocking);
        let rendered = impact
            .messages
            .iter()
            .find(|message| message.contains("未结束的任务"))
            .expect("必须有一条说明未结束任务的消息");
        assert!(rendered.contains("AI 打标 × 1"), "{rendered}");
        assert!(rendered.contains("入库 × 1"), "{rendered}");

        let error = tauri::async_runtime::block_on(start_library_migration_with_state(
            &state,
            "/tmp/nowhere".to_string(),
            "migrate".to_string(),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("未结束的任务"), "{error}");
    }

    #[test]
    fn an_operation_from_a_previous_generation_does_not_block_forever() {
        // 换根后旧代次的行不可能再被派发。把它们算进影响面会让用户永远无法迁移。
        let (_root, state) = seeded_state();
        {
            let conn = state.db.lock().unwrap();
            crate::db::plans::insert_operation(
                &conn,
                &crate::db::plans::NewOperation {
                    operation_id: "op_stale".to_string(),
                    step_id: "step_stale".to_string(),
                    plan_id: "plan_stale".to_string(),
                    principal_id: "principal_test".to_string(),
                    capability: "tagging.submit".to_string(),
                    idempotency_key: "idem_stale".to_string(),
                    library_id: "library_old".to_string(),
                    library_epoch: "epoch_old".to_string(),
                    state: "running".to_string(),
                    phase: "ready".to_string(),
                    revision: 1,
                    total: Some(1),
                    completed: 0,
                    failed: 0,
                    created_at: 100,
                },
            )
            .unwrap();
        }
        let impact = task_impact(&state).unwrap();
        assert_eq!(impact.running_operations, 0);
        assert!(!impact.blocking);
    }
}
