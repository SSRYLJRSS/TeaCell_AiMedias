//! 超级搜索命令（FB5-05 §9）：AI 自然语言 → SearchIntentV3 → QueryExpr + SearchPlanV3。
//! 薄壳：校验输入长度 → 短锁读配置/分面/标签 → 放锁 → spawn_blocking 网络请求
//! → V3 解析（degrade_parse_v3 内含 evidence 守卫/清洗/校验）→ 短锁生成 expr（V2 视图，
//! 兼容现有列表链路）+ plan（含 should 加分，供 U 波次三段式 UI）→ 返回。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::search_plan::PlanDiagnostics;
use crate::db::settings;
use crate::db::tag_facets;
use crate::error::{AppError, AppResult};
use crate::services::super_search_ai;
use crate::services::super_search_ai::{AiSearchParseResult, AiSearchStage, SearchIntentV3};
use crate::state::{AppState, Database, DbConnectionGuard};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum AiSearchProgressPhase {
    Queued,
    Requesting,
    Validating,
    Completed,
    Failed,
    Cancelled,
    Cancelling,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AiSearchProgress {
    request_id: String,
    phase: AiSearchProgressPhase,
    elapsed_ms: u64,
    error_code: Option<String>,
}

struct SearchRequestRegistration {
    registry: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    request_id: String,
    cancel: Arc<AtomicBool>,
}

impl Drop for SearchRequestRegistration {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .get(&self.request_id)
                .is_some_and(|registered| Arc::ptr_eq(registered, &self.cancel))
            {
                registry.remove(&self.request_id);
            }
        }
    }
}

fn emit_search_progress(
    app: &AppHandle,
    request_id: &str,
    phase: AiSearchProgressPhase,
    started_at: Instant,
    error_code: Option<String>,
) {
    let elapsed_ms = started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
    if let Err(error) = app.emit(
        "super-search://progress",
        AiSearchProgress {
            request_id: request_id.to_string(),
            phase,
            elapsed_ms,
            error_code,
        },
    ) {
        tracing::debug!(
            operation = "super_search_ai",
            stage = "progress_emit_failed",
            error = %error,
            "AI 搜索进度事件发送失败"
        );
    }
}

fn ensure_search_request_active(cancel: &AtomicBool, deadline: Instant) -> AppResult<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::cancelled("AI 搜索请求已取消"));
    }
    if Instant::now() >= deadline {
        return Err(AppError::timeout("AI 搜索解析超过 180 秒总时限"));
    }
    Ok(())
}

fn prepare_search_ai_settings(
    settings: &mut crate::db::settings::AiSettings,
    usage_profile: Option<crate::db::settings::ApiProfile>,
) -> AppResult<()> {
    let profile = usage_profile.ok_or_else(|| {
        AppError::not_found("尚未解析出超级搜索 AI 服务，请配置在线服务或手动选择本机服务")
    })?;
    crate::db::ai_connections::apply_profile(settings, profile);
    if settings.active().is_none() {
        return Err(AppError::not_found("请先在设置页添加或绑定 AI 服务"));
    }
    Ok(())
}

fn lock_db(db: &Arc<Database>) -> AppResult<DbConnectionGuard<'_>> {
    db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// AI 自然语言 → SearchIntentV3（required + preferred）→ 后端生成 QueryExpr（必须部分）
/// 与 SearchPlanV3（filter/must_not/should，加分语义完整）。expr 供现有 UI/列表执行，
/// plan 在存在加分项时返回给 U 波次三段式界面直接映射。
/// FB5-05（§9.5）：已删除未使用的 current_query 参数——append 由前端明确合并 expr。
#[tauri::command]
pub async fn ai_parse_search_query(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    request_id: String,
) -> AppResult<AiSearchParseResult> {
    if text.trim().is_empty() {
        return Err(AppError::invalid_arg("请输入搜索描述"));
    }
    if text.chars().count() > super_search_ai::MAX_INPUT_LEN {
        return Err(AppError::invalid_arg(format!(
            "查询描述过长（最多 {} 字）",
            super_search_ai::MAX_INPUT_LEN
        )));
    }
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::invalid_arg("搜索请求 ID 不合法"));
    }
    let db = Arc::clone(&state.db);
    let registry = Arc::clone(&state.search_cancel);
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = registry
            .lock()
            .map_err(|_| AppError::internal("搜索取消注册表锁中毒"))?;
        if active.contains_key(&request_id) {
            return Err(AppError::conflict("搜索请求 ID 已在使用中"));
        }
        active.insert(request_id.clone(), Arc::clone(&cancel));
    }
    let started_at = Instant::now();
    let deadline = started_at + crate::services::ai_cloud::AI_SEARCH_TOTAL_TIMEOUT;
    emit_search_progress(
        &app,
        &request_id,
        AiSearchProgressPhase::Queued,
        started_at,
        None,
    );
    let request_id_for_join = request_id.clone();
    let worker_app = app.clone();
    let join = tauri::async_runtime::spawn_blocking(move || {
        let registration = SearchRequestRegistration {
            registry,
            request_id: request_id.clone(),
            cancel: Arc::clone(&cancel),
        };
        let result: AppResult<AiSearchParseResult> = (|| {
            // 2. 凭据服务先解析用途绑定（DB guard 已释放后才访问 keyring），随后短锁读取
            // AI 设置、分面、标签词典和库能力摘要。
            let usage_profile =
                crate::services::credentials::resolve_super_search_profile_with_system_credential(
                    &db,
                )?;
            let (cfg, facets, dict, capabilities) = {
                let conn = lock_db(&db)?;
                let mut s = settings::get_settings(&conn)?;
                // 用途解析已在锁外按「显式绑定优先，否则只选在线服务」完成；
                // API Key 已从系统凭据读取，旧 active 档案不会作为搜索回退。
                prepare_search_ai_settings(&mut s.ai, Some(usage_profile.profile.clone()))?;
                let facets = tag_facets::build_prompt_context(&conn, "all")?;
                let dict = super_search_ai::collect_tag_dictionary(&conn, &facets)?;
                // C-3：实时库能力摘要（缓存 60s，只告知不改写）；失败时静默给空串不阻塞搜索
                let capabilities = super_search_ai::library_capabilities(&conn).unwrap_or_default();
                (s.ai, facets, dict, capabilities)
            };
            // 3. 锁外网络请求 + V3 解析；请求错误如实返回，只有模型内容不合规才降级。
            //    degrade_parse_v3 已含 sanitize + guard_preferred + validate。
            let progress_app = worker_app.clone();
            let progress_request_id = request_id.clone();
            let (intent, ai_warnings): (SearchIntentV3, Vec<String>) =
                super_search_ai::request_intent_with_control(
                    &cfg,
                    &text,
                    &facets,
                    &dict,
                    &capabilities,
                    &cancel,
                    deadline,
                    &|stage| {
                        let phase = match stage {
                            AiSearchStage::Queued => AiSearchProgressPhase::Queued,
                            AiSearchStage::Requesting => AiSearchProgressPhase::Requesting,
                            AiSearchStage::Validating => AiSearchProgressPhase::Validating,
                        };
                        emit_search_progress(
                            &progress_app,
                            &progress_request_id,
                            phase,
                            started_at,
                            None,
                        );
                    },
                )?;
            ensure_search_request_active(&cancel, deadline)?;
            // W6-5：是否落在第 3 层（关键词兜底）→ 解释文案与前端三态据此
            let keyword_mode = super_search_ai::is_keyword_fallback_v3(&intent, &text);
            let mut warnings = ai_warnings;
            // 4. 短锁：从 V2 视图生成 expr（现有列表执行事实源）+ 从 V3 生成 plan（加分语义）
            let (expr, resolved_tags, plan, resolve_warnings) = {
                let conn = lock_db(&db)?;
                let v2_view = super_search_ai::v3_to_v2_view(&intent);
                let (expr, resolved_tags, ew) =
                    super_search_ai::build_expr_from_v2(&conn, &v2_view)?;
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
            ensure_search_request_active(&cancel, deadline)?;
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
            ensure_search_request_active(&cancel, deadline)?;
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
        })();
        drop(registration);
        let (phase, error_code) = match &result {
            Ok(_) => (AiSearchProgressPhase::Completed, None),
            Err(error) if error.code() == "CANCELLED" => (
                AiSearchProgressPhase::Cancelled,
                Some(error.code().to_string()),
            ),
            Err(error) => (
                AiSearchProgressPhase::Failed,
                Some(error.code().to_string()),
            ),
        };
        emit_search_progress(&worker_app, &request_id, phase, started_at, error_code);
        result
    });
    match join.await {
        Ok(result) => result,
        Err(error) => {
            emit_search_progress(
                &app,
                &request_id_for_join,
                AiSearchProgressPhase::Failed,
                started_at,
                Some("INTERNAL".into()),
            );
            Err(AppError::internal(format!("AI 搜索任务失败: {error}")))
        }
    }
}

#[tauri::command]
pub fn cancel_ai_search(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
) -> AppResult<()> {
    let cancel = state
        .search_cancel
        .lock()
        .map_err(|_| AppError::internal("搜索取消注册表锁中毒"))?
        .get(&request_id)
        .cloned();
    if let Some(cancel) = cancel {
        cancel.store(true, Ordering::Relaxed);
        emit_search_progress(
            &app,
            &request_id,
            AiSearchProgressPhase::Cancelling,
            Instant::now(),
            None,
        );
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::prepare_search_ai_settings;
    use crate::db::settings::{AiSettings, ApiProfile};

    fn profile() -> ApiProfile {
        ApiProfile {
            id: "bound-search-service".into(),
            name: "Bound search service".into(),
            api_mode: "openai".into(),
            kind: "cloud".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: String::new(),
            model: "test-model".into(),
            max_concurrency: 1,
            requests_per_minute: 0,
            requests_per_hour: 0,
        }
    }

    #[test]
    fn valid_usage_binding_works_without_legacy_active_profile() {
        let mut settings = AiSettings::default();
        settings.active_profile.clear();

        prepare_search_ai_settings(&mut settings, Some(profile()))
            .expect("有效的搜索用途绑定应能独立于旧默认服务使用");
        assert_eq!(
            settings.active().map(|active| active.id.as_str()),
            Some("bound-search-service")
        );
    }

    #[test]
    fn missing_resolved_service_is_actionable_even_if_legacy_profile_exists() {
        let mut settings = AiSettings {
            profiles: vec![profile()],
            active_profile: "bound-search-service".into(),
            ..AiSettings::default()
        };
        assert_eq!(
            prepare_search_ai_settings(&mut settings, None)
                .expect_err("未解析到显式或自动在线服务时不得回退旧 active profile")
                .code(),
            "NOT_FOUND"
        );
    }
}
