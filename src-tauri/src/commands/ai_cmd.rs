//! AI 打标命令（T05a）：批次创建/执行/取消 + 建议确认流

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::db::ai::{AiBatch, AiSuggestion, AiSuggestionItem, CategorizedTags};
use crate::db::{ai, ai_connections, settings};
use crate::error::{AppError, AppResult};
use crate::services::ai_cloud::{self, AiProgress};
use crate::services::ollama_runtime;
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
    // 新前端不再创建 manual 批次；保留历史协议兼容，存量 manual 批次仍可查看和手工编辑。
    let mode = if mode == "manual" {
        mode
    } else {
        require_tagging_binding(&conn, &mut s.ai)?;
        mode_for_active_profile(&s.ai).to_string()
    };
    // 指导书阶段 5 §8.1/§8.3：用户选择的素材**完整**进入逻辑批次，不做静默截断。
    // 「批量上限」不再作为总批次截断——执行层按「分块大小」内存分块、限流、重试。
    // 若确需保护上限，必须在提交前明确展示与阻断，而非默认取前 N 张。
    let ids: Vec<i64> = asset_ids;
    ai::create_batch_with_retag(&conn, &ids, &mode, retag)
}

/// 执行批次（云端）：spawn_blocking 工作线程跑，进度走 ai://progress 事件；
/// 预检短锁即用即放，批次执行期间不持 DB 锁（网络等待不阻塞全应用 DB 读写）
#[tauri::command]
pub async fn ai_start_batch(
    app: AppHandle,
    state: State<'_, AppState>,
    batch_id: i64,
    limit: Option<i64>,
) -> AppResult<AiBatch> {
    let db = std::sync::Arc::clone(&state.db);
    let registry = std::sync::Arc::clone(&state.ai_cancel);
    let ai_config_guard = std::sync::Arc::clone(&state.ai_config_guard);
    let runtime = std::sync::Arc::clone(&state.ollama_runtime);
    let cancel = Arc::new(AtomicBool::new(false));

    tauri::async_runtime::spawn_blocking(move || {
        // 与设置及分类保存共用配置锁，保证业务说明和分类说明来自同一快照。
        let config_guard = ai_config_guard
            .lock()
            .map_err(|_| AppError::msg("AI 配置锁中毒"))?;
        // Resolve usage metadata under a short DB guard, release it, then read the
        // system keyring before taking any subsequent DB guard.
        let tagging_profile = crate::services::credentials::usage_profile_with_system_credential(
            &db,
            "tagging",
        )?
        .ok_or_else(|| {
            AppError::msg("尚未绑定打标服务，请先在打标页选择在线或本地模型")
        })?;
        // 预检与配置读取：短锁作用域，读完即放
        let (all, facets, facets_video) = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            let batch = ai::get_batch(&conn, batch_id)?;
            // v2.12：仅执行中拒绝；done/cancelled 允许续跑剩余 pending 建议
            if batch.status == "processing" {
                return Err(AppError::conflict("批次正在执行中"));
            }
            // F15a（2026-08-22）：待打标 = pending 且尚无候选（set_suggestion_tags 不改 status，
            // 只看 status 会把「已生成候选未确认」的条目误判为待处理 → 续跑重复请求）
            let has_pending = ai::list_suggestions(&conn, batch_id)?
                .iter()
                .any(|s| s.status == "pending" && s.suggested_tags.is_empty());
            if !has_pending {
                return Err(AppError::not_found(
                    "当前没有待打标的建议（已全部处理或确认）",
                ));
            }
            let mut s = settings::get_settings(&conn)?;
            // 用已在 DB/keyring 锁外解析出的连接快照；本作用域不做凭据 IO。
            ai_connections::apply_profile(&mut s.ai, tagging_profile.clone());
            let facets = crate::db::tag_facets::build_prompt_context(&conn, "image")?;
            let facets_video = crate::db::tag_facets::build_prompt_context(&conn, "video")?;
            crate::services::ai_cloud::validate_tagging_facet_descriptions(
                facets.iter().chain(facets_video.iter()),
            )?;
            let profile_is_local = s
                .ai
                .active()
                .is_some_and(crate::services::ai_cloud::is_managed_ollama_profile);
            // 建批后允许切换服务；执行时按当前 tagging 绑定修正批次类型。
            ai::set_batch_mode(&conn, batch_id, mode_for_active_profile(&s.ai))?;
            // FB-03 §9.3 视频批次预检（前后端一致；后端为最终校验，service 层兜底保留）：
            // 待打标条目是否含视频 → 开关/ffmpeg/本地视觉模型三项检查，启动前阻断而非逐条启动后失败。
            {
                let suggestions = ai::list_suggestions(&conn, batch_id)?;
                let pending_items: Vec<_> = suggestions
                    .iter()
                    .filter(|s| s.status == "pending" && s.suggested_tags.is_empty())
                    .collect();
                let has_video = pending_items.iter().any(|s| {
                    let by_mime = s
                        .mime_type
                        .as_deref()
                        .map(|m| m.starts_with("video/"))
                        .unwrap_or(false);
                    let by_ext = {
                        let lower = s.asset_path.to_ascii_lowercase();
                        [".mp4", ".mov", ".avi", ".mkv", ".webm", ".m4v", ".wmv", ".flv", ".ts"]
                            .iter()
                            .any(|ext| lower.ends_with(ext))
                    };
                    by_mime || by_ext
                });
                if has_video {
                    if !s.ai.video_tagging {
                        return Err(AppError::unsupported(
                            "视频 AI 打标未开启。请打开「设置 → AI 设置 → 自动打标 → 视频 AI 打标」，保存后重新开始批次。",
                        ));
                    }
                    if s.ai.video_tagging_mode == "frames"
                        && !crate::services::video::ffmpeg_available()
                    {
                        return Err(AppError::unsupported(
                            "视频打标模式为「抽帧打标」，但未检测到 ffmpeg。请安装 ffmpeg 并加入 PATH，或在设置中改为「封面打标」后重试。",
                        ));
                    }
                }
                // W5a（a11）：本地模型视觉能力检查对图片批次也生效（此前只在 has_video 块内——
                // 图片库（如本库 0 视频）该检查从未运行过，纯图片批次会带着纯文本模型起跑后逐条失败）
                if profile_is_local {
                    if let Some(active) = s.ai.active() {
                        if !crate::services::ai_cloud::model_supports_vision(&active.model) {
                            return Err(AppError::unsupported(format!(
                                "当前本地模型「{}」不支持视觉（图片/视频）输入，无法打标。请更换支持图片输入的视觉模型（如 qwen3.5、llava、moondream），保存后重新开始批次。",
                                active.model
                            )));
                        }
                    }
                }
            }
            (s, facets, facets_video)
        };
        let proxy = all.model_download_proxy.clone();
        let cfg = all.ai;
        let managed_ollama = cfg
            .active()
            .is_some_and(ai_cloud::is_managed_ollama_profile);
        if managed_ollama {
            // Windows 应用托管的默认 Ollama 允许按需启动；其它本机兼容服务绝不接管。
            ollama_runtime::ensure_ready(&runtime, &proxy)?;
        }
        registry
            .lock()
            .map_err(|_| AppError::msg("取消注册表锁中毒"))?
            .insert(batch_id, Arc::clone(&cancel));
        drop(config_guard);
        let r = ai_cloud::run_cloud_batch(&db, batch_id, &cfg, &facets, &facets_video, limit, &cancel, |p: AiProgress| {
            let _ = app.emit("ai://progress", p);
        });
        // B12：收尾清理 flag——锁中毒不再静默吞
        match registry.lock() {
            Ok(mut m) => {
                m.remove(&batch_id);
            }
            Err(_) => tracing::error!("取消注册表锁中毒，batch {} 的 flag 未清理", batch_id),
        }
        if let Err(e) = r {
            // 批次级异常（DB/配置等）收尾：置 cancelled 防卡死在 processing 无法重试
            // （单条失败已在 run_cloud_batch 内部置 rejected，不进这里）
            tracing::warn!("批次 {} 执行异常，标记 cancelled: {e}", batch_id);
            if let Ok(conn) = db.lock() {
                // 限流或连续失败时 run_cloud_batch 已落为 interrupted；保留可续跑语义，
                // 只回收仍处于 processing 的异常，避免误报为用户主动取消。
                let should_cancel = ai::get_batch(&conn, batch_id)
                    .map(|batch| batch.status == "processing")
                    .unwrap_or(true);
                if should_cancel {
                    let _ = ai::set_batch_status(&conn, batch_id, "cancelled");
                }
            }
            return Err(e);
        }
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        ai::get_batch(&conn, batch_id)
    })
    .await
    .map_err(|e| AppError::msg(format!("打标线程异常: {e}")))?
}

#[tauri::command]
pub fn ai_cancel_batch(state: State<AppState>, batch_id: i64) -> AppResult<()> {
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
}
