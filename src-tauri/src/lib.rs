//! 茶馆AI素材管理 —— Tauri 应用入口
//! T03：核心服务 + M1 commands 注册（网盘已移除，见 §6.8）
//! 指导书 阶段 1 §5.2：setup 关键路径只保留「建目录 → 开库 → 迁移 → 注册状态/命令/协议」；
//! 缩略图 LRU、旧标签整理、历史任务状态修复全部移入具名后台线程（不阻塞窗口显示）。

pub mod commands;
pub mod contracts;
pub mod db;
pub mod error;
pub mod observability;
pub mod services;
pub mod state;
pub mod utils;

use crate::db::settings;
use crate::error::AppError;
use crate::observability::LoggingGuard;
use crate::services::thumbnail::ThumbnailService;
use state::AppState;
use tauri::Emitter;
use tauri::Manager;

/// W0-10：迁移/初始化失败给用户可见出路（发布版无控制台，panic 等于静默崩溃）。
/// 先在 tauri app 启动前用 rfd 弹原生 dialog（plugin dialog 需要 AppHandle，此时还没有），
/// 弹失败（无桌面环境）时退回同步日志 + eprintln。
fn fatal_db_error(db_path: &std::path::Path, logs_dir: &std::path::Path, e: &AppError) -> ! {
    let msg = format!(
        "数据库升级失败：{e}。\n\n请把日志目录打包发给支持：\n{}",
        logs_dir.display()
    );
    tracing::error!(?db_path, "数据库初始化失败: {e}");
    // 非阻塞 writer 可能尚未刷盘；退出前同步补一份事实，避免关键启动错误丢失。
    observability::write_sync_diagnostic(logs_dir, &format!("database_init_failed: {e}"));
    // tauri-plugin-dialog 2.7 依赖 rfd 0.15：直接用它做无 AppHandle 的阻塞弹窗
    let _shown = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("数据库升级失败")
        .set_description(&msg)
        .show();
    eprintln!("{msg}");
    // R0-6：正常退出而非 panic —— panic 会留下堆栈与「已崩溃」的误导信号，
    // 弹窗已经给了用户可见出路，进程干净退出即可。
    std::process::exit(1);
}

/// 具名后台线程：setup 中的非关键维护任务统一入口。
/// 失败只记录 warning，绝不阻塞窗口显示；任务有明确名字便于日志定位。
fn spawn_maintenance(name: &'static str, f: impl FnOnce() + Send + 'static) {
    let builder = std::thread::Builder::new().name(format!("maintenance::{name}"));
    match builder.spawn(move || {
        if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            tracing::warn!("后台维护任务 {name} panic: {e:?}");
        }
    }) {
        Ok(_) => tracing::info!("后台维护任务已启动: {name}"),
        Err(e) => tracing::warn!("后台维护任务 {name} 启动失败: {e}"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // W0-9：数据目录计算提前到日志初始化之前（文件日志要写 data_dir/logs）
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("bagertea_ai_media_v2");
    // WorkerGuard 必须绑定 run() 栈生命周期：绑到局部会立即 drop → 文件日志一条不写
    let _log_guard: LoggingGuard = observability::init_logging(&data_dir);

    // 应用数据目录：$APP_DATA_DIR/bagertea_ai_media_v2/library.db
    let db_path = data_dir.join("library.db");
    let installation_id = services::installation::installed_tutorial_id();

    // 关键路径（保留）：创建数据目录 + 打开数据库 + 必要迁移。
    // W0-10：迁移失败弹原生 dialog 给用户可见出路，不再裸 panic 静默崩溃
    let conn = match db::init(&db_path) {
        Ok(c) => c,
        Err(e) => fatal_db_error(&db_path, &data_dir.join("logs"), &e),
    };
    if let Err(error) = services::managed_library::initialize_runtime(&conn) {
        tracing::error!("总仓身份初始化失败，受管素材将保持不可见: {error}");
    }
    if let Some(id) = installation_id {
        if let Err(error) = settings::normalize_tutorial_installation(&conn, &id) {
            tracing::warn!("安装后的教程提示状态同步失败，下次启动将重试: {error}");
        }
    }
    // 用户保存的日志级别在数据库可用后应用；初始化阶段仍使用环境变量或 info。
    if let Ok(saved) = settings::get_settings(&conn) {
        if let Err(e) = observability::set_log_level(&saved.log_level) {
            tracing::warn!("应用已保存的日志级别失败: {e}");
        }
    }
    if let Err(e) = db::ensure_default_taxonomy(&conn) {
        tracing::warn!("核心标签词表补齐失败（不影响启动，可在设置页重置标签后重试）: {e}");
    }

    // asset 协议放行用（data_dir 稍后会 move 进 AppState）
    let scope_dir = data_dir.clone();

    // F1-e：启动时读一次 schema_features（迁移+自检已完成），供命令层判断能力开关
    let state = AppState::new(conn, data_dir);
    state.refresh_schema_features();

    tauri::Builder::default()
        // B05：先 manage(AppState)，setup 闭包中 app.state::<AppState>() 才可用
        .manage(state)
        // asset 协议按需放行（安全收敛）：
        // B08：只放行 thumbnails/ + previews/ 子目录，不放行 data_dir 根（含 library.db）
        // 素材原文件由 list/get 命令逐路径放行
        .setup(move |app| {
            // The Rust desktop process owns one Pi Host. The event adapter only
            // forwards validated protocol frames; persistence remains with A3-04.
            let agent_state = app.state::<AppState>();
            let agent_app = app.handle().clone();
            agent_state.agent_runtime.configure_for_app(
                app.handle(),
                &scope_dir,
                std::sync::Arc::clone(&agent_state.db),
                // A4-05：Host 的写能力与 GUI、MCP 共用同一道维护门。
                std::sync::Arc::clone(&agent_state.library_maintenance),
                std::sync::Arc::new(move |frame| {
                    if agent_app.emit("agent-host-event", frame).is_err() {
                        tracing::debug!(operation = "agent_host_event", error_code = "emit_failed");
                    }
                }),
            );
            let thumbs_dir = scope_dir.join("thumbnails");
            let previews_dir = scope_dir.join("previews");
            let proxies_dir = scope_dir.join("proxies");
            if let Err(e) = app
                .asset_protocol_scope()
                .allow_directory(&thumbs_dir, true)
            {
                tracing::warn!("asset 协议放行缩略图目录失败: {e}");
            }
            if let Err(e) = app
                .asset_protocol_scope()
                .allow_directory(&previews_dir, true)
            {
                tracing::warn!("asset 协议放行预览目录失败: {e}");
            }
            // §8.3：放行视频兼容代理目录（原文件不兼容时的按需转码产物）
            if let Err(e) = app
                .asset_protocol_scope()
                .allow_directory(&proxies_dir, true)
            {
                tracing::warn!("asset 协议放行视频代理目录失败: {e}");
            }
            // 关键路径之外的非关键维护：旧预置标签整理（§5.2 移出关键路径）
            let state = app.state::<AppState>();
            // FX-14：SQLite 无统计信息时不会选用 dominant_* 等窄索引（实测：按主色筛选会退化成
            // idx_assets_deleted 全扫）。PRAGMA optimize 增量更新 sqlite_stat1，成本与库规模成正比、
            // 259 项量级是毫秒级。放后台维护线程：它不在启动关键路径上（§5.2）。
            let opt_db = std::sync::Arc::clone(&state.db);
            spawn_maintenance("sqlite-optimize", move || {
                if let Ok(c) = opt_db.lock().map_err(|_| AppError::msg("数据库锁中毒")) {
                    // PRAGMA optimize 只统计"本连接使用过的表"：启动瞬间没有任何查询跑过时
                    // optimize 是 no-op（实测 sqlite_stat1 未生成）。先跑一组代表性查询
                    // 把 assets 纳入本连接的使用集合，optimize 才会为它生成统计。
                    let _ = c.execute_batch(
                        "SELECT COUNT(*) FROM assets WHERE deleted_at IS NULL;

                         SELECT id FROM assets WHERE dominant_hue BETWEEN 200 AND 250 AND dominant_hue IS NOT NULL LIMIT 1;

                         SELECT id FROM assets WHERE dominant_sat <= 10 LIMIT 1;

                         SELECT id FROM assets WHERE dominant_lum BETWEEN 10 AND 90 LIMIT 1;",
                    );
                    if let Err(e) = c.execute_batch("PRAGMA optimize;") {
                        tracing::warn!("PRAGMA optimize 失败（不影响功能，仅查询计划可能次优）: {e}");
                    }
                }
            });

            let preset_db = std::sync::Arc::clone(&state.db);
            spawn_maintenance("preset-tags-cleanup", move || {
                let conn = preset_db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
                if let Ok(c) = conn {
                    if let Err(e) = db::tags::retire_unused_presets(&c) {
                        tracing::warn!("旧预置标签清理失败: {e}");
                    }
                }
            });

            // A1-02：旧记录分类是后台、分页、限批工作；不阻塞启动，也不让
            // 未核验记录进入普通查询或自动清理。
            let reconcile_db = std::sync::Arc::clone(&state.db);
            spawn_maintenance("library-reconcile", move || {
                match services::managed_library::reconcile_pending(&reconcile_db) {
                    Ok(report) => tracing::info!(
                        inspected = report.inspected,
                        classified = report.classified,
                        "总仓旧记录分类完成"
                    ),
                    Err(error) => tracing::warn!("总仓旧记录分类失败: {error}"),
                }
            });

            // B05：启动时执行一次 LRU 清理（读 settings 短锁 → 锁外清理）。
            // §5.2：移出关键路径——缩略图清理是纯文件系统 IO，放后台线程不阻塞窗口显示。
            let lru_db = std::sync::Arc::clone(&state.db);
            let lru_dir = scope_dir.clone();
            spawn_maintenance("thumbnail-lru-cleanup", move || {
                let cache_mb = {
                    let conn = lru_db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
                    match conn {
                        Ok(c) => settings::get_settings(&c)
                            .ok()
                            .map(|s| s.thumbnail_cache_mb),
                        Err(_) => None,
                    }
                };
                if let Some(max_mb) = cache_mb {
                    if let Ok(thumbs) = ThumbnailService::new(&lru_dir) {
                        let _ = thumbs.cleanup_lru(max_mb);
                    }
                }
            });

            // 日志按文件数滚动，这里再按总字节数和 30 天期限兜底；
            // 文件 IO 放后台线程，避免大日志目录拖慢启动。
            let log_dir = scope_dir.join("logs");
            spawn_maintenance("log-retention", move || {
                match observability::prune_log_files(&log_dir) {
                    Ok(report) if report.removed_files > 0 => tracing::info!(
                        operation = "log_retention",
                        removed_files = report.removed_files,
                        removed_bytes = report.removed_bytes,
                        kept_files = report.kept_files,
                        "日志保留清理完成"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        operation = "log_retention",
                        error = %e,
                        "日志保留清理失败，不影响应用启动"
                    ),
                }
            });

            // 阶段 5 §8.2：应用重启时把遗留 processing 批次标记为 interrupted（可一键续跑）
            // §5.2：历史任务状态修复属可延迟维护，移入后台线程。
            let ai_db = std::sync::Arc::clone(&state.db);
            spawn_maintenance("batch-interrupt-mark", move || {
                let conn = ai_db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
                if let Ok(c) = conn {
                    if let Err(e) = db::ai::mark_interrupted_batches(&c) {
                        tracing::warn!("标记中断批次失败: {e}");
                    }
                }
            });

            // A1-05：启动只对账并生成恢复摘要，不自动推进任何副作用。
            //
            // A4-04：超期回收站清理**排在恢复对账之后、且用同一个线程**执行。
            // 两件事分开起线程无法保证先后（A4-04 停下条件明确禁止在迁移/恢复
            // 之前就跑自动清理）；排在同一个线程里是唯一不引入额外同步原语就
            // 能保证顺序的写法。清理本身是文件 IO，仍在后台线程，不堵启动。
            let operation_db = std::sync::Arc::clone(&state.db);
            let trash_db = std::sync::Arc::clone(&state.db);
            let trash_dir = scope_dir.clone();
            // A4-05：迁仓期间禁止自动清理。维护门是应用层标志，这里显式读它——
            // 光靠「同一个线程」不够，用户可能在应用运行中启动迁仓。
            let expiry_gate = std::sync::Arc::clone(&state.library_maintenance);
            spawn_maintenance("operation-recovery", move || {
                if let Err(error) = services::operations::reconcile_startup(&operation_db) {
                    tracing::warn!("操作恢复对账失败: {error}");
                }
                // A4-05：对账刚把半迁移收到稳定态，此刻若仍有未结束的迁仓，
                // 说明它没被对账收掉（例如 copying 阶段被中断后等待用户重试），
                // 这时碰回收站可能删掉新根还没核验到的文件。
                if expiry_gate.is_active() {
                    tracing::warn!(
                        "回收站自动清理跳过：总仓维护进行中（{}）",
                        expiry_gate.reason().unwrap_or_else(|| "未知原因".to_string())
                    );
                    return;
                }
                // R-22：恢复对账完成后才清理超期回收站。
                let days = {
                    let conn = trash_db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
                    match conn {
                        Ok(c) => settings::get_settings(&c)
                            .ok()
                            .map(|s| s.trash_retention_days),
                        Err(_) => None,
                    }
                };
                if let Some(days) = days {
                    if days > 0 {
                        if let Err(e) = commands::purge_expired_trash(&trash_db, &trash_dir, days) {
                            tracing::warn!("回收站自动清理失败: {e}");
                        }
                    }
                }
            });

            // A5-01：本机外部接入（MCP）。设置里默认关闭；只有用户显式开启过
            // 才建端点文件并监听。这里不阻塞启动——监听线程自己会回报就绪或失败，
            // 失败原因会如实出现在设置界面（不假装已启用）。
            commands::mcp_cmd::start_from_settings(&app.state::<AppState>());

            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            // 素材
            commands::list_assets,
            commands::list_asset_ids,
            // A3-08：把当前选择做成不可变快照，供交接与「本次结果视图」复用
            commands::snapshot_selection,
            commands::summary_selection,
            commands::page_selection,
            // A1-03：目录授权和受控资源句柄
            commands::pick_directory_grant,
            commands::list_directory_grants,
            commands::reissue_directory_grant,
            commands::revoke_directory_grant,
            commands::revoke_all_directory_grants,
            commands::page_authorized_directory,
            // A1-04：计划审核与操作查询
            commands::list_pending_plans,
            commands::approve_plan,
            commands::reject_plan,
            commands::get_operation,
            commands::cancel_operation,
            commands::resume_operation,
            commands::list_operation_recovery,
            // A4-02：已批准操作的显式执行入口
            commands::start_operation,
            // A1-06：会话索引与持久工作台草稿
            commands::create_conversation,
            commands::list_conversations,
            commands::get_conversation,
            commands::rename_conversation,
            commands::set_conversation_archived,
            commands::delete_conversation,
            commands::save_conversation_draft,
            commands::attach_conversation_asset,
            commands::link_conversation,
            commands::get_conversation_work,
            // A3-01：Rust-owned, offline-only Pi Host lifecycle/protocol.
            commands::agent_host_start,
            commands::agent_host_open_conversation,
            commands::agent_host_send,
            commands::agent_host_conversation_history,
            commands::agent_host_cancel_run,
            commands::agent_host_close_conversation,
            commands::get_agent_model_proxy,
            commands::set_agent_model_proxy,
            // A2-01：导入准备计划（扫描/检查与正式入库分离）
            commands::prepare_import,
            commands::cancel_prepare_import,
            commands::get_import_plan,
            commands::get_import_plan_preview_paths,
            commands::get_import_result,
            commands::verify_import_plan,
            commands::confirm_import,
            // 一句话描述列表（标签设置页展示）
            commands::list_content_descriptions,
            commands::list_metadata_facets,
            commands::get_numeric_domains,
            commands::get_asset,
            commands::delete_assets,
            // A4-03：回收站恢复迁到 commands::restore_assets（逐项核验）
            // W2-8：收藏/评级/手动旋转（② T17）
            commands::set_favorite,
            commands::set_rating,
            commands::set_user_rotation,
            commands::dedup_scan,
            commands::dedup_scan_similar,
            commands::get_asset_urls,
            commands::reveal_in_folder,
            // 媒体元数据回填（指导书 §7.5）
            commands::rescan_asset_metadata,
            commands::cancel_media_refill,
            // FB2-08：算法色板回算（独立的 rescan_*）
            commands::rescan_asset_palette,
            commands::rescan_palette_colors,
            // V18：GPS 定位 + 视频拍摄时间存量回填（独立的 rescan_*）
            commands::rescan_asset_geo_taken,
            // W1-4：图片宽高存量回填（RAW 分辨率修复）
            commands::rescan_image_dimensions,
            // W5d：感知哈希存量回填
            commands::rescan_asset_phash,
            // FB4-03：色板状态查询 + 定向补丁（设置页状态行 / 局部同步）
            commands::get_palette_status,
            commands::get_asset_palette_patches,
            // 视频兼容代理（指导书 §8.3）
            commands::ensure_video_proxy,
            commands::get_video_proxy_status,
            commands::cancel_video_proxy,
            commands::clear_video_proxy,
            commands::video_proxy_cache_stats,
            commands::clear_all_video_proxies,
            // 超级搜索
            commands::ai_parse_search_query,
            commands::cancel_ai_search,
            // C-2：AST 命中诊断（U-6 界面数据前提）
            commands::diagnose_search_plan_cmd,
            // Phase 2 §4.1：plan 列表分页 + 全选 ID（B2 同源）
            commands::list_assets_by_plan,
            commands::list_asset_ids_by_plan,
            // 入库
            commands::inspect_import,
            commands::preview_rename,
            commands::open_file_external,
            // A2-03：GUI-only, receipt-backed handling of historical external assets.
            commands::list_legacy_assets,
            commands::list_unlinked_legacy_assets,
            commands::prepare_legacy_unlink,
            commands::prepare_legacy_copy_back,
            // A4-06：批量处置（逐条仍走同一收据协议）
            commands::prepare_legacy_unlink_batch,
            commands::prepare_legacy_copy_back_batch,
            commands::confirm_legacy_resolutions_batch,
            commands::cancel_legacy_asset_resolution,
            commands::confirm_legacy_asset_resolution,
            commands::reopen_unlinked_legacy_asset,
            // 标签
            commands::list_tags,
            commands::list_tag_facets,
            commands::list_all_tag_facets,
            commands::create_tag_facet,
            commands::save_tag_facet,
            // W2-2/W2-3：合并编辑 + 级联删除（分面契约）
            commands::update_tag_facet,
            commands::delete_tag_facet,
            commands::reorder_tag_facets,
            commands::deactivate_tag_facet,
            commands::restore_tag_facet,
            commands::get_tag_facet_impact,
            commands::list_tags_by_facet,
            commands::list_tag_governance,
            // V24（Phase 7-3/7-6）：数值分面 —— 手工赋值/清值/读值 + tag→number 转换
            commands::set_facet_number,
            commands::clear_facet_number,
            commands::get_facet_number,
            commands::get_facet_numbers,
            commands::convert_facet_kind,
            commands::set_facet_kind,
            commands::scan_duplicate_tags,
            commands::search_tag_candidates,
            commands::create_canonical_tag,
            commands::add_tag_alias,
            commands::create_tag,
            commands::update_tag,
            commands::delete_tag,
            commands::deactivate_tag,
            commands::tag_merge,
            commands::merge_tags_preserve_alias,
            commands::assign_tags,
            commands::remove_tags,
            commands::get_asset_tags,
            commands::tag_recent_ops,
            commands::tag_undo_batch,
            // F2-e：数据完整性（冲突预检 + 约束能力状态 + apply + facet 一致性）
            commands::detect_tag_constraints_conflicts,
            commands::list_tag_constraint_features,
            commands::apply_tag_constraints,
            commands::check_terms_facet_consistency,
            // 缩略图
            commands::get_thumbnail,
            commands::clear_thumbnail_cache,
            commands::get_preview,
            // 导出
            commands::export_local_files,
            commands::export_csv_manifest,
            commands::list_export_tasks,
            commands::export_report,
            commands::cancel_export,
            // 库内分库移动（A4-02）
            commands::list_move_targets,
            commands::move_assets,
            // 回收站与恢复（A4-03 / A4-04）
            commands::list_trash,
            commands::trash_retention,
            commands::restore_assets,
            commands::empty_trash,
            // 总仓维护（A4-05）：软件迁移 / 重新定位 / 维护态 / 收尾
            commands::preflight_library_migration,
            commands::library_task_impact,
            commands::start_library_migration,
            commands::cancel_library_migration,
            commands::library_maintenance_status,
            commands::library_migration_report,
            commands::resume_library_migration,
            // 个人常用流程（A4-07）：草稿提取 / 建版本 / 重命名 / 停用 / 管理列表
            commands::draft_workflow_from_operation,
            commands::workflow_step_catalog,
            // 个人常用流程（A4-08）：每次实例化为新计划 + 统一审批
            commands::prepare_workflow_instance,
            commands::run_workflow_instance,
            commands::workflow_run_instance_report,
            commands::cancel_workflow_instance,
            commands::create_workflow,
            commands::update_workflow,
            commands::rename_workflow,
            commands::set_workflow_archived,
            commands::list_workflows,
            commands::get_workflow,
            commands::workflow_for_plan,
            // AI 打标
            commands::ai_create_batch,
            commands::ai_prepare_batch,
            commands::ai_prepare_import_batch,
            commands::ai_submit_tagging_plan,
            commands::ai_start_tagging,
            commands::ai_cancel_batch,
            commands::ai_list_batches,
            commands::ai_list_suggestions,
            commands::ai_list_suggestion_items,
            commands::ai_list_new_word_candidates,
            commands::ai_decide_suggestion_item,
            commands::ai_confirm_suggestion,
            commands::ai_reject_suggestion,
            commands::ai_restore_suggestion,
            commands::ai_confirm_all,
            commands::ai_apply_tags,
            // Ollama 一键配置（方案 A2）+ 一键安装（方案 A3）
            commands::ollama_ping,
            commands::ollama_probe_hardware,
            commands::ollama_pull,
            commands::ollama_open_download_page,
            commands::ollama_install_status,
            commands::ollama_installer_cache_info,
            commands::ollama_download_install,
            commands::ollama_start_service,
            commands::ollama_runtime_status,
            commands::ollama_stop_service,
            commands::ollama_remove_installer,
            // 下载源自选/测速（改造方案）
            commands::ollama_list_sources,
            commands::ollama_probe_sources,
            commands::ollama_add_custom_source,
            commands::ollama_remove_custom_source,
            // 本地打标：模型管理（列表/删除/目录）
            commands::ollama_list_local_models,
            commands::ollama_delete_model,
            commands::ollama_model_dir,
            commands::ollama_open_model_dir,
            // 平台能力（三端复核 R0）
            commands::get_platform_capabilities,
            // 设置
            commands::get_settings,
            commands::save_settings,
            // A5-01：本机外部接入（MCP）。只由可信设置界面调用，
            // 外部客户端无法创建、扩权或撤销自己的配对。
            commands::mcp_bridge_status,
            commands::set_mcp_bridge_enabled,
            commands::create_mcp_pairing,
            commands::update_mcp_pairing,
            commands::revoke_mcp_pairing,
            commands::get_data_dir,
            commands::open_data_dir,
            // W0-9：设置页「关于」打开日志目录（tracing-appender 滚动文件）
            commands::open_logs_dir,
            commands::open_help_page,
            commands::get_help_page_url,
            commands::open_project_page,
            commands::get_project_page_url,
            commands::open_license_page,
            commands::get_license_page_url,
            commands::open_author_page,
            commands::get_author_page_url,
            commands::open_feedback_page,
            commands::get_feedback_page_url,
            commands::open_agnes_api_key_docs,
            // 前端异常与关键事件回传统一 tracing 文件
            commands::log_frontend,
            commands::export_diagnostics,
            commands::reset_app_data,
            // W5c：数据库备份/恢复
            commands::backup_db,
            commands::restore_db,
            // AI 连接档案 + 用途绑定（指导书 §6.3/§4.4）
            commands::list_ai_connections,
            commands::save_ai_connection,
            commands::delete_ai_connection,
            commands::set_ai_usage_binding,
            commands::get_ai_usage_bindings,
            commands::get_super_search_service_resolution,
            commands::get_legacy_active_profile,
            commands::test_ai_connection,
            commands::discover_ai_models,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // L2（§8.2）：应用退出只停止 AppOwned；External（用户自启）永不杀
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    state.agent_runtime.shutdown();
                    let rt = std::sync::Arc::clone(&state.ollama_runtime);
                    {
                        let mut runtime = rt.lock().ok();
                        if let Some(r) = runtime.as_mut() {
                            r.stop_app_owned();
                        }
                    }
                }
            }
        });
}
