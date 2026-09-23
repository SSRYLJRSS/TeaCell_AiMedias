//! 超级搜索命令（FB5-05 §9）：AI 自然语言 → SearchIntentV3 → QueryExpr + SearchPlanV3。
//! 薄壳：校验输入长度 → 短锁读配置/分面/标签 → 放锁 → spawn_blocking 网络请求
//! → V3 解析（degrade_parse_v3 内含 evidence 守卫/清洗/校验）→ 短锁生成 expr（V2 视图，
//! 兼容现有列表链路）+ plan（含 should 加分，供 U 波次三段式 UI）→ 返回。

use std::sync::Arc;
use tauri::{Manager, State};

use crate::db::search_plan::PlanDiagnostics;
use crate::db::settings;
use crate::db::tag_facets;
use crate::error::{AppError, AppResult};
use crate::services::super_search_ai;
use crate::services::super_search_ai::{AiSearchParseResult, SearchIntentV3};
use crate::state::AppState;

fn lock_db(
    db: &Arc<std::sync::Mutex<rusqlite::Connection>>,
) -> AppResult<std::sync::MutexGuard<'_, rusqlite::Connection>> {
    db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// AI 自然语言 → SearchIntentV3（required + preferred）→ 后端生成 QueryExpr（必须部分）
/// 与 SearchPlanV3（filter/must_not/should，加分语义完整）。expr 供现有 UI/列表执行，
/// plan 在存在加分项时返回给 U 波次三段式界面直接映射。
/// FB5-05（§9.5）：已删除未使用的 current_query 参数——append 由前端明确合并 expr。
#[tauri::command]
pub async fn ai_parse_search_query(
    state: State<'_, AppState>,
    text: String,
) -> AppResult<AiSearchParseResult> {
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        // 1. 校验文本长度
        if text.trim().is_empty() {
            return Err(AppError::invalid_arg("请输入搜索描述"));
        }
        if text.chars().count() > super_search_ai::MAX_INPUT_LEN {
            return Err(AppError::msg(format!(
                "查询描述过长（最多 {} 字）",
                super_search_ai::MAX_INPUT_LEN
            )));
        }
        // 2. 短锁读取 AI 档案（用途绑定优先）+ 分面 + 标签词典 + 库能力摘要，读取后立即放锁
        let (mut cfg, super_search_key_ref, facets, dict, capabilities) = {
            let conn = lock_db(&db)?;
            let mut s = settings::get_settings(&conn)?;
            // P1-01：先解析用途绑定，再判断「是否有可用配置」。
            // 新安装用户可能只用「服务连接 + 用途绑定」而没有任何 legacy profile；
            // 若先检查 s.ai.active() 会在解析绑定前就误报「请先添加 API 配置」。
            // §4.4/§10：超级搜索按用途绑定读取连接档案；db 锁内只注入不含明文密钥的 profile，
            //           无绑定时回退 legacy active 档案（升级兼容），密钥引用留到锁外补全。
            let key_ref = match crate::db::ai_connections::usage_profile_ref(&conn, "super_search")
            {
                Ok(Some(bref)) => {
                    let r = bref.api_key_ref.clone();
                    // apply_profile_to_cfg 会把绑定档案设为 active，使后续 active() 命中。
                    crate::db::ai_connections::apply_profile_to_cfg(&mut s.ai, bref.profile);
                    r
                }
                // 无绑定：回退 legacy active 档案（其明文密钥已在 settings 中，
                // 不走 keyring 补全，返回 None 保留原 api_key）。
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!("超级搜索读取用途绑定失败: {e}");
                    return Err(e);
                }
            };
            // 绑定与 legacy 都没有可用档案时，才返回明确的配置错误。
            if s.ai.active().is_none() {
                return Err(AppError::not_found(
                    "请先在设置页添加服务连接并把「超级搜索」用途绑定到该连接",
                ));
            }
            let facets = tag_facets::build_prompt_context(&conn, "all")?;
            let dict = super_search_ai::collect_tag_dictionary(&conn, &facets)?;
            // C-3：实时库能力摘要（缓存 60s，只告知不改写）；失败时静默给空串不阻塞搜索
            let capabilities = super_search_ai::library_capabilities(&conn).unwrap_or_default();
            (s.ai, key_ref, facets, dict, capabilities)
        };
        // §10：DB 锁已释放，用 keyring 补全 super_search 绑定档案的明文密钥（锁外阻塞调用）。
        if let Some(id) = super_search_key_ref {
            let key = crate::services::credentials::get_api_key(&id)?.unwrap_or_default();
            if let Some(active) = cfg.profiles.iter_mut().find(|p| p.id == id) {
                active.api_key = key;
            }
        }
        // 3. 锁外网络请求 + V3 解析（V3→V2→关键词 三层降级；配置错误仍真报错）。
        //    degrade_parse_v3 已含 sanitize + guard_preferred + validate。
        let (intent, ai_warnings): (SearchIntentV3, Vec<String>) =
            super_search_ai::request_intent(&cfg, &text, &facets, &dict, &capabilities)?;
        // W6-5：是否落在第 3 层（关键词兜底）→ 解释文案与前端三态据此
        let keyword_mode = super_search_ai::is_keyword_fallback_v3(&intent, &text);
        let mut warnings = ai_warnings;
        // 4. 短锁：从 V2 视图生成 expr（现有列表执行事实源）+ 从 V3 生成 plan（加分语义）
        let (expr, resolved_tags, plan, resolve_warnings) = {
            let conn = lock_db(&db)?;
            let v2_view = super_search_ai::v3_to_v2_view(&intent);
            let (expr, resolved_tags, ew) = super_search_ai::build_expr_from_v2(&conn, &v2_view)?;
            let (plan, pr, pw) = super_search_ai::build_plan_from_v3(&conn, &intent)?;
            let mut all_resolved = resolved_tags.clone();
            for r in pr {
                if !all_resolved.iter().any(|x| x.tag_id == r.tag_id) {
                    all_resolved.push(r);
                }
            }
            let mut all_w = ew;
            all_w.extend(pw);
            (expr, all_resolved, Some(plan), all_w)
        };
        warnings.extend(resolve_warnings);
        // §9.7：AI 结果通过后本地再校验一次；失败视为解析错误，不应用部分条件。
        // W6-2：此处失败同样降级为关键词搜索（永不红字报错）。
        let (expr, resolved_tags, plan) = match &expr {
            Some(e) => match crate::db::query_expr::validate_expr(e) {
                Ok(()) => (expr, resolved_tags, plan),
                Err(e) => {
                    warnings.push(format!("解析结果不合规（{e}），已按关键词搜索。"));
                    let fallback = super_search_ai::keyword_intent_v3(&text);
                    let (fe, fr, fw, fp) = {
                        let conn = lock_db(&db)?;
                        let v2 = super_search_ai::v3_to_v2_view(&fallback);
                        let (expr, r, w) = super_search_ai::build_expr_from_v2(&conn, &v2)?;
                        let (p, _, _) = super_search_ai::build_plan_from_v3(&conn, &fallback)?;
                        (expr, r, w, p)
                    };
                    warnings.extend(fw);
                    (fe, fr, Some(fp))
                }
            },
            None => (expr, resolved_tags, plan),
        };
        let explanation = if keyword_mode {
            "按关键词搜索".into()
        } else {
            super_search_ai::build_explanation_v3(&intent)
        };
        let parse_status = if keyword_mode {
            "keyword".to_string()
        } else if warnings.is_empty() {
            "full".to_string()
        } else {
            "partial".to_string()
        };
        let sort_by = intent
            .sort_by
            .clone()
            .unwrap_or_else(|| "created_at".into());
        let sort_dir = intent.sort_dir.clone().unwrap_or_else(|| "desc".into());
        Ok(AiSearchParseResult {
            intent,
            expr,
            plan,
            sort_by,
            sort_dir,
            explanation,
            warnings,
            resolved_tags,
            parse_status,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("AI 搜索任务失败: {e}")))?
}

/// Phase 2 §4.1：plan 执行 —— 结果列表（分页）。入口 validate → prune → execute（单一编译器）。
#[tauri::command]
pub fn list_assets_by_plan(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    plan: Option<crate::db::search_plan::SearchPlanV3>,
    offset: Option<i64>,
    limit: Option<i64>,
) -> AppResult<crate::db::search_plan::PlanAssetPage> {
    let conn = lock_db(&state.db)?;
    let Some(plan) = plan else {
        return Ok(crate::db::search_plan::PlanAssetPage {
            items: Vec::new(),
            total: 0,
            has_more: false,
            warnings: Vec::new(),
        });
    };
    let page = crate::db::search_plan::run_plan_page(&conn, &plan, offset.unwrap_or(0), limit)?;
    for a in &page.items {
        let _ = app
            .asset_protocol_scope()
            .allow_file(std::path::Path::new(&a.file_path));
    }
    Ok(page)
}

/// Phase 2 §4.1（B2/B8）：plan 全选 ID —— PlanIdsResult 一路到底，不降级成裸数组。
#[tauri::command]
pub fn list_asset_ids_by_plan(
    state: State<'_, AppState>,
    plan: Option<crate::db::search_plan::SearchPlanV3>,
) -> AppResult<crate::db::search_plan::PlanIdsResult> {
    let conn = lock_db(&state.db)?;
    let Some(plan) = plan else {
        return Ok(crate::db::search_plan::PlanIdsResult {
            ids: Vec::new(),
            total: 0,
            truncated: false,
            warnings: Vec::new(),
        });
    };
    crate::db::search_plan::run_plan_ids(&conn, &plan)
}

/// C-2/§4.5：对当前 SearchPlanV3 做 AST 命中诊断（U-6 数据前提）。
/// 入口 validate → prune，返回剔除后的叶子/should 诊断 + 与列表命令同一批 warnings。
/// 叶子带 zone、加分项带 index（§3.7 不变式 9）；加分命中数为结果集内交集（B3）。
/// 只读 COUNT（毫秒级）；无 plan 时返回空。
#[tauri::command]
pub fn diagnose_search_plan_cmd(
    state: State<'_, AppState>,
    plan: Option<crate::db::search_plan::SearchPlanV3>,
    plan_revision: i64,
) -> AppResult<PlanDiagnostics> {
    let conn = lock_db(&state.db)?;
    let Some(plan) = plan else {
        return Ok(PlanDiagnostics::default());
    };
    crate::db::search_plan::diagnose_search_plan(&conn, &plan, plan_revision)
}
