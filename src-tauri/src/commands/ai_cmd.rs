//! AI 打标命令（T05a）：批次创建/执行/取消 + 建议确认流

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, State};

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::db::ai::{AiBatch, AiSuggestion, AiSuggestionItem, CategorizedTags};
use crate::db::{ai, ai_connections, settings};
use crate::error::{AppError, AppResult};
use crate::services::ai_cloud::{self, AiProgress, RequestBudget};
use crate::services::{authorization, ollama_runtime, operations, tagging};
use crate::state::AppState;

fn lock_db(state: &AppState) -> AppResult<crate::state::DbConnectionGuard<'_>> {
    state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// 打标只认 tagging 用途绑定；旧 settings 激活档案仅作为迁移输入，不再作为运行回退。
fn require_tagging_binding(
    conn: &rusqlite::Connection,
    cfg: &mut settings::AiSettings,
) -> AppResult<()> {
    if ai_connections::apply_usage_binding(conn, "tagging", cfg)? {
        return Ok(());
    }
    Err(AppError::msg(
        "尚未绑定打标服务，请先在打标页选择在线或本地模型",
    ))
}

fn mode_for_active_profile(cfg: &settings::AiSettings) -> &'static str {
    match cfg
        .active()
        .map(crate::services::ai_cloud::is_managed_ollama_profile)
    {
        Some(true) => "local",
        _ => "cloud",
    }
}

fn gui_caller(state: &AppState) -> AppResult<CallerContext> {
    let conn = state.db.lock()?;
    authorization::build_caller_context(
        &conn,
        authorization::TrustedAdapterIdentity::Gui {
            session_id: state.authorization_session_id.clone(),
            instance_id: state.authorization_instance_id.clone(),
        },
        ResourceRef::generated("request"),
        Vec::new(),
    )
}

/// 解析本次批次的实际执行类型。
///
/// M-07（PRD R-44）：打标工作台不区分 AI/手动模式，所有批次都能直接手工填写。
/// 没有可用打标服务时不能连批次都建不出来——那会让「不配置 AI 也能用手动标签」整条路线失效。
/// 因此 auto（以及历史 cloud/local 请求）在无绑定/已停用时降级为 manual 批次：素材照常进入
/// 工作台供手工填写，之后绑定服务再点「开始打标」会经 `set_batch_mode` 改成实际部署类型。
fn resolve_batch_mode(
    conn: &rusqlite::Connection,
    cfg: &mut settings::AiSettings,
    requested: &str,
) -> String {
    if requested == "manual" {
        return requested.to_string();
    }
    if require_tagging_binding(conn, cfg).is_err() {
        return "manual".to_string();
    }
    mode_for_active_profile(cfg).to_string()
}

/// 用选中素材创建批次（pending 建议占位）
/// mode 保留为旧客户端兼容参数；除历史 manual 外，实际部署类型一律按 tagging 用途绑定解析。
/// retagMode（A3，可缺省 = append）：append / replaceAiOnly / reviewOnly
#[tauri::command]
pub fn ai_create_batch(
    state: State<AppState>,
    asset_ids: Vec<i64>,
    mode: String,
    retag_mode: Option<String>,
) -> AppResult<AiBatch> {
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    if !["cloud", "local", "manual", "auto"].contains(&mode.as_str()) {
        return Err(AppError::invalid_arg("非法打标模式"));
    }
    let retag = match retag_mode.as_deref() {
        None | Some("append") => ai::RetagMode::Append,
        Some("replaceAiOnly") => ai::RetagMode::ReplaceAiOnly,
        Some("reviewOnly") => ai::RetagMode::ReviewOnly,
        Some(other) => return Err(AppError::invalid_arg(format!("非法重跑模式: {other}"))),
    };
    let conn = lock_db(&state)?;
    let mut s = settings::get_settings(&conn)?;
    let mode = resolve_batch_mode(&conn, &mut s.ai, &mode);
    // 指导书阶段 5 §8.1/§8.3：用户选择的素材**完整**进入逻辑批次，不做静默截断。
    // 「批量上限」不再作为总批次截断——执行层按「分块大小」内存分块、限流、重试。
    // 若确需保护上限，必须在提交前明确展示与阻断，而非默认取前 N 张。
    let ids: Vec<i64> = asset_ids;
    ai::create_batch_with_retag(&conn, &ids, &mode, retag)
}

/// 准备 AI 打标计划。这里只生成供可信界面展示的审批预览，不发送网络请求。
#[tauri::command]
pub fn ai_prepare_batch(
    state: State<AppState>,
    batch_id: i64,
    limit: Option<u32>,
) -> AppResult<tagging::TaggingPreparation> {
    let caller = gui_caller(&state)?;
    tagging::prepare_existing_batch(&state.db, &caller, batch_id, limit)
}

/// 从已完成导入操作的成功收据准备打标计划；重复映射必须逐项明确纳入。
#[tauri::command]
pub fn ai_prepare_import_batch(
    state: State<AppState>,
    source_operation_id: ResourceRef,
    source_step_id: ResourceRef,
    included_duplicate_item_ids: Vec<String>,
) -> AppResult<tagging::TaggingPreparation> {
    let caller = gui_caller(&state)?;
    tagging::prepare_from_import(
        &state.db,
        &caller,
        &source_operation_id,
        &source_step_id,
        &included_duplicate_item_ids,
    )
}

/// 已批准计划只会原子创建待启动批次和操作，不会在 submit 阶段外发。
#[tauri::command]
pub fn ai_submit_tagging_plan(
    state: State<AppState>,
    plan_ref: ResourceRef,
    idempotency_key: ResourceRef,
) -> AppResult<tagging::TaggingOperation> {
    let caller = gui_caller(&state)?;
    tagging::submit_plan(&state.db, &caller, &plan_ref, &idempotency_key)
}

/// 仅可信 GUI 能显式启动已批准操作的联网阶段。
#[tauri::command]
pub async fn ai_start_tagging(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: ResourceRef,
    step_id: ResourceRef,
) -> AppResult<AiBatch> {
    // A4-05：维护期间停止写入。打标会读素材文件并回写标签，迁仓中途两者
    // 都可能指向旧根。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "开始打标")?;
    let db = Arc::clone(&state.db);
    let registry = Arc::clone(&state.ai_cancel);
    let ai_config_guard = Arc::clone(&state.ai_config_guard);
    let runtime = Arc::clone(&state.ollama_runtime);
    let caller = gui_caller(&state)?;
    let batch_id = tagging::batch_id_for_start(&db, &caller, &operation_id, &step_id)?;
    let queue_claim = state
        .operation_queue
        .try_reserve_operation(&db, operation_id.as_str(), "tagging.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或打标操作尚未排到队首，请稍后重试")
        })?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = registry
            .lock()
            .map_err(|_| AppError::msg("取消注册表锁中毒"))?;
        if active.contains_key(&batch_id) {
            return Err(AppError::conflict("批次正在执行中"));
        }
        active.insert(batch_id, Arc::clone(&cancel));
    }
    let worker_registry = Arc::clone(&registry);
    let queue_wakeup_app = app.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        let _queue_claim = queue_claim;
        let execution = (|| -> AppResult<AiBatch> {
            let config_guard = ai_config_guard
                .lock()
                .map_err(|_| AppError::msg("AI 配置锁中毒"))?;
            let started = tagging::start(&db, &caller, &operation_id, &step_id)?;
            let preparation = (|| -> AppResult<()> {
                let proxy = { tagging::model_download_proxy(&db)? };
                if started
                    .config
                    .active()
                    .is_some_and(ai_cloud::is_managed_ollama_profile)
                {
                    ollama_runtime::ensure_ready(&runtime, &proxy)?;
                }
                Ok(())
            })();
            drop(config_guard);
            let run_result = preparation.and_then(|()| {
                let budget_db = Arc::clone(&db);
                let outcome_db = Arc::clone(&db);
                let budget_operation_id = started.operation_id.clone();
                let budget_step_id = started.step_id.clone();
                let outcome_operation_id = started.operation_id.clone();
                let outcome_step_id = started.step_id.clone();
                let remaining = Duration::from_secs(
                    started
                        .deadline_at
                        .saturating_sub(chrono::Utc::now().timestamp())
                        .max(0) as u64,
                );
                let budget = RequestBudget::persistent_tagging_until(
                    started.request_limit as u64,
                    Instant::now() + remaining,
                    move |asset_id| {
                        tagging::persist_request_attempt_for_item(
                            &budget_db,
                            &budget_operation_id,
                            &budget_step_id,
                            asset_id,
                        )
                    },
                    move |asset_id, succeeded, error_code| {
                        tagging::record_request_outcome(
                            &outcome_db,
                            &outcome_operation_id,
                            &outcome_step_id,
                            asset_id,
                            succeeded,
                            error_code.as_deref(),
                        )
                    },
                );
                ai_cloud::run_cloud_batch_with_plan(
                    &db,
                    batch_id,
                    &started.config,
                    &started.facets,
                    &started.facets_video,
                    None,
                    &cancel,
                    Some(&budget),
                    Some(&started.allowed_asset_ids),
                    Some(&started.top_tags),
                    |progress: AiProgress| {
                        let _ = app.emit("ai://progress", progress);
                    },
                )?;
                tagging::batch_result(&db, batch_id)
            });
            match run_result {
                Ok(batch) => {
                    if batch.status == "cancelled"
                        || cancel.load(Ordering::Relaxed)
                        || tagging::cancellation_requested(
                            &db,
                            &started.operation_id,
                            &started.step_id,
                        )?
                    {
                        tagging::finish_cancelled(&db, &started.operation_id, &started.step_id)?;
                        tagging::batch_result(&db, batch_id)
                    } else {
                        tagging::finish(&db, &started.operation_id, &started.step_id, true)?;
                        Ok(batch)
                    }
                }
                Err(error) => {
                    tracing::warn!("已批准打标操作 {} 执行异常: {error}", started.operation_id);
                    let cancellation_requested = cancel.load(Ordering::Relaxed)
                        || tagging::cancellation_requested(
                            &db,
                            &started.operation_id,
                            &started.step_id,
                        )?;
                    if cancellation_requested {
                        tagging::finish_cancelled(&db, &started.operation_id, &started.step_id)?;
                        return tagging::batch_result(&db, batch_id);
                    }
                    tagging::finish(&db, &started.operation_id, &started.step_id, false)?;
                    Err(error)
                }
            }
        })();
        match worker_registry.lock() {
            Ok(mut active) => {
                active.remove(&batch_id);
            }
            Err(_) => tracing::error!("取消注册表锁中毒，batch {} 的 flag 未清理", batch_id),
        }
        execution
    })
    .await;
    crate::commands::import_cmd::wake_import_queue(queue_wakeup_app);
    match joined {
        Ok(result) => result,
        Err(error) => {
            if let Ok(mut active) = registry.lock() {
                active.remove(&batch_id);
            }
            Err(AppError::msg(format!("打标线程异常: {error}")))
        }
    }
}

#[tauri::command]
pub fn ai_cancel_batch(state: State<AppState>, batch_id: i64) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    // Approved tagging batches must close the durable operation/link send
    // gate before the in-memory cancellation flag is signalled. Unlinked
    // legacy/manual batches retain their original flag-only behavior.
    operations::cancel_tagging_batch(&state.db, &caller, batch_id)?;
    // B11：不再静默吞锁中毒
    let m = state
        .ai_cancel
        .lock()
        .map_err(|_| AppError::msg("取消注册表锁中毒，无法取消任务"))?;
    if let Some(flag) = m.get(&batch_id) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub fn ai_list_batches(state: State<AppState>) -> AppResult<Vec<AiBatch>> {
    let conn = lock_db(&state)?;
    ai::list_batches(&conn)
}

#[tauri::command]
pub fn ai_list_suggestions(state: State<AppState>, batch_id: i64) -> AppResult<Vec<AiSuggestion>> {
    let conn = lock_db(&state)?;
    ai::list_suggestions(&conn, batch_id)
}

#[tauri::command]
pub fn ai_list_suggestion_items(
    state: State<AppState>,
    suggestion_id: i64,
) -> AppResult<Vec<AiSuggestionItem>> {
    let conn = lock_db(&state)?;
    ai::list_suggestion_items(&conn, suggestion_id)
}

/// F6-d：全库「新词待确认」列表（pending 且词表里没有的候选）。
#[tauri::command]
pub fn ai_list_new_word_candidates(state: State<AppState>) -> AppResult<Vec<AiSuggestionItem>> {
    let conn = lock_db(&state)?;
    ai::list_new_word_candidates(&conn)
}

#[tauri::command]
pub fn ai_decide_suggestion_item(
    state: State<AppState>,
    item_id: i64,
    decision: String,
    replacement_tag_id: Option<i64>,
    replacement_name: Option<String>,
    reason: Option<String>,
) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai::decide_suggestion_item(
        &conn,
        item_id,
        &decision,
        replacement_tag_id,
        replacement_name.as_deref(),
        reason.as_deref(),
    )
}

/// 确认单条建议（tags 为最终值，含人工修改）
/// FB5-05（§7.6）：description 为审核后的最终描述；同一事务内写入素材。
#[tauri::command]
pub fn ai_confirm_suggestion(
    state: State<AppState>,
    id: i64,
    tags: CategorizedTags,
    description: Option<String>,
) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai::confirm_suggestion_with_description(&conn, id, &tags, description.as_deref())
}

/// 撤销拒绝（v2.11）：恢复为待确认
#[tauri::command]
pub fn ai_restore_suggestion(state: State<AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai::restore_suggestion(&conn, id)
}

#[tauri::command]
pub fn ai_reject_suggestion(state: State<AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai::reject_suggestion(&conn, id)
}

/// 批量确认该批次全部 pending 建议（按 AI 原建议写入）
/// 批量套用标签到任意素材（PRD 5.3：胶片条多选套用）
#[tauri::command]
pub fn ai_apply_tags(
    state: State<AppState>,
    asset_ids: Vec<i64>,
    tags: CategorizedTags,
) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai::apply_tags(&conn, &asset_ids, &tags)
}

#[tauri::command]
pub fn ai_confirm_all(state: State<AppState>, batch_id: i64) -> AppResult<()> {
    const CHUNK: i64 = 100;
    let mut after_id = 0;
    loop {
        let page = {
            let conn = lock_db(&state)?;
            ai::list_pending_confirmations(&conn, batch_id, after_id, CHUNK)?
        };
        let Some(last_id) = page.last_id else {
            return Ok(());
        };
        {
            let conn = lock_db(&state)?;
            ai::confirm_pending_batch(&conn, &page.items)?;
        }
        after_id = last_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ai_connections, init_memory};

    #[test]
    fn tagging_binding_overrides_stale_active_profile() {
        let conn = init_memory().unwrap();
        ai_connections::upsert(
            &conn,
            "local-1",
            "Local",
            "local",
            "openai_chat",
            "http://localhost:11434/v1",
            "qwen3.5:4b",
            None,
        )
        .unwrap();
        ai_connections::upsert(
            &conn,
            "cloud-1",
            "Cloud",
            "cloud",
            "openai_chat",
            "https://example.com/v1",
            "vision-model",
            None,
        )
        .unwrap();
        ai_connections::bind_usage(&conn, "tagging", "cloud-1").unwrap();

        let mut cfg = settings::get_settings(&conn).unwrap();
        cfg.ai.profiles.push(settings::ApiProfile {
            id: "stale-local".into(),
            name: "Stale Local".into(),
            api_mode: "openai".into(),
            kind: "local".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            model: "qwen3.5:4b".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        });
        cfg.ai.active_profile = "stale-local".into();

        require_tagging_binding(&conn, &mut cfg.ai).unwrap();
        assert_eq!(cfg.ai.active().unwrap().id, "cloud-1");
        assert_eq!(mode_for_active_profile(&cfg.ai), "cloud");

        ai_connections::bind_usage(&conn, "tagging", "local-1").unwrap();
        require_tagging_binding(&conn, &mut cfg.ai).unwrap();
        assert_eq!(cfg.ai.active().unwrap().id, "local-1");
        let expected_mode = if cfg!(target_os = "windows") {
            "local"
        } else {
            "cloud"
        };
        assert_eq!(mode_for_active_profile(&cfg.ai), expected_mode);
    }

    #[test]
    fn missing_tagging_binding_does_not_fall_back_to_legacy_profile() {
        let conn = init_memory().unwrap();
        let mut cfg = settings::get_settings(&conn).unwrap();
        cfg.ai.profiles.push(settings::ApiProfile {
            id: "legacy".into(),
            name: "Legacy".into(),
            api_mode: "openai".into(),
            kind: "cloud".into(),
            base_url: "https://example.com/v1".into(),
            api_key: "secret".into(),
            model: "vision-model".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        });
        cfg.ai.active_profile = "legacy".into();

        let err = require_tagging_binding(&conn, &mut cfg.ai).unwrap_err();
        assert!(err.to_string().contains("尚未绑定打标服务"));
    }

    // M-07 / PRD R-44：没有打标服务时不能连批次都建不出来——那会让「不配置 AI 也能手工填写」
    // 整条路线失效。降级成 manual 批次，素材照常进工作台，手工标签的 source 也才是 manual。
    #[test]
    fn batch_mode_degrades_to_manual_without_tagging_binding() {
        let conn = init_memory().unwrap();
        let mut cfg = settings::get_settings(&conn).unwrap();
        // 遗留 active_profile 不能当绑定用，否则降级判断会被旧配置骗过。
        cfg.ai.profiles.push(settings::ApiProfile {
            id: "legacy".into(),
            name: "Legacy".into(),
            api_mode: "openai".into(),
            kind: "cloud".into(),
            base_url: "https://example.com/v1".into(),
            api_key: "secret".into(),
            model: "vision-model".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        });
        cfg.ai.active_profile = "legacy".into();

        assert_eq!(resolve_batch_mode(&conn, &mut cfg.ai, "auto"), "manual");
        // 已绑定的连接被停用也算「没有可用服务」。
        ai_connections::upsert(
            &conn,
            "cloud-1",
            "Cloud",
            "cloud",
            "openai_chat",
            "https://example.com/v1",
            "vision-model",
            None,
        )
        .unwrap();
        ai_connections::bind_usage(&conn, "tagging", "cloud-1").unwrap();
        // 没有 set_enabled 命令：停用状态就是 ai_connections.enabled=0，这里直接写库模拟界面上关掉服务。
        conn.execute(
            "UPDATE ai_connections SET enabled = 0 WHERE id = 'cloud-1'",
            [],
        )
        .unwrap();
        assert_eq!(resolve_batch_mode(&conn, &mut cfg.ai, "auto"), "manual");

        // 恢复启用后回到真实部署类型，降级不是永久改行为。
        conn.execute(
            "UPDATE ai_connections SET enabled = 1 WHERE id = 'cloud-1'",
            [],
        )
        .unwrap();
        assert_eq!(resolve_batch_mode(&conn, &mut cfg.ai, "auto"), "cloud");
        // 显式 manual 请求保持 manual，不被绑定改写。
        assert_eq!(resolve_batch_mode(&conn, &mut cfg.ai, "manual"), "manual");
    }
}
