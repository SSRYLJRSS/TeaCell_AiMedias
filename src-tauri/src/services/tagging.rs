//! Approved AI tagging plans, import receipt selection, and explicit GUI start.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::contracts::agent::{
    AgentErrorCode, BudgetSummary, CapabilityId, Instructions, MoneyLimit, PlanPreview,
    PlanScopeSummary, ResourceRef, ShortText,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::ai::{self, AiSuggestion, RetagMode};
use crate::db::operations::{self as operations_db, OperationReceiptRecord};
use crate::db::plans::{self as plans_db, PlanRecord};
use crate::db::{
    ai_connections, assets, libraries, settings, tag_facets, tagging as tagging_db, tags,
};
use crate::error::{AppError, AppResult};
use crate::services::{ai_cloud, authorization as authorization_service, credentials, plans};
use crate::state::Database;

const MAX_REQUESTS_PER_IMAGE_UNIT: u32 = 24;
const MAX_APPROVED_TAGGING_SECONDS: u32 = 3_600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptReference {
    receipt_id: String,
    item_id: String,
    event: String,
    asset_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaggingSelectionItem {
    asset_id: i64,
    mime_type: String,
    suggestion_id: Option<i64>,
    receipts: Vec<ReceiptReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrozenTaggingConfig {
    ai: settings::AiSettings,
    connection: settings::ApiProfile,
    connection_revision: String,
    facets: Vec<tag_facets::FacetPromptContext>,
    facets_video: Vec<tag_facets::FacetPromptContext>,
    top_tags: Vec<(String, String)>,
    system_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaggingPlanSnapshot {
    schema_version: u32,
    source_operation_id: Option<String>,
    source_step_id: Option<String>,
    included_duplicate_item_ids: Vec<String>,
    source_fingerprint: String,
    existing_batch_id: Option<i64>,
    selected_limit: Option<u32>,
    /// A3-03：按「用户选中的一批素材」准备的计划才有这一项。提交和开始打标时用
    /// 它重新解析选择快照，复验素材集合与 MIME 没有被替换；为空表示来自导入操作
    /// 或已有批次，那两条路径各自复验自己的源。老计划反序列化时缺省为空。
    #[serde(default)]
    selection_source_ref: Option<String>,
    selection: Vec<TaggingSelectionItem>,
    config_hash: String,
    config: FrozenTaggingConfig,
    request_limit: u32,
    max_seconds: u32,
    request_config_json: String,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaggingOperation {
    pub operation_id: String,
    pub step_id: String,
    pub batch_id: i64,
    pub request_limit: u32,
    pub requests_attempted: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaggingPreparation {
    pub requires_approval: bool,
    pub summary: String,
    pub preview: Option<PlanPreview>,
    pub operation: Option<TaggingOperation>,
}

pub struct StartedTagging {
    pub operation_id: String,
    pub step_id: String,
    pub batch_id: i64,
    pub request_limit: u32,
    pub deadline_at: i64,
    pub allowed_asset_ids: Vec<i64>,
    pub config: settings::AiSettings,
    pub facets: Vec<tag_facets::FacetPromptContext>,
    pub facets_video: Vec<tag_facets::FacetPromptContext>,
    pub top_tags: Vec<(String, String)>,
}

struct PrepareInput<'a> {
    source: Option<(&'a str, &'a str)>,
    included_duplicate_item_ids: Vec<String>,
    existing_batch_id: Option<i64>,
    selected_limit: Option<u32>,
    /// 素材集合来自 agent 选择快照时的引用；提交/开始打标时用它复验源集合。
    selection_source_ref: Option<&'a str>,
    selection: Vec<TaggingSelectionItem>,
    config: FrozenTaggingConfig,
    /// 模型在自然语言里补充的打标要求，只进入审批摘要（让人看清它想做什么），
    /// 不改变冻结的请求预算、分类或提示词版本。
    operator_note: Option<String>,
}

pub fn prepare_existing_batch(
    db: &Database,
    caller: &CallerContext,
    batch_id: i64,
    limit: Option<u32>,
) -> AppResult<TaggingPreparation> {
    check_capability_entry(CapabilityId::TaggingPrepare, caller).map_err(AppError::from)?;
    if limit == Some(0) {
        return Err(AppError::invalid_arg("打标范围至少要包含一项素材"));
    }
    let (selection, config, existing_link) = {
        let conn = db.lock()?;
        let suggestions = ai::list_suggestions(&conn, batch_id)?;
        let eligible = eligible_suggestions(&suggestions);
        let selected_limit = limit.unwrap_or(u32::MAX) as usize;
        let selected = eligible
            .into_iter()
            .take(selected_limit)
            .map(|suggestion| TaggingSelectionItem {
                asset_id: suggestion.asset_id,
                mime_type: suggestion.mime_type.clone().unwrap_or_default(),
                suggestion_id: Some(suggestion.id),
                receipts: Vec::new(),
            })
            .collect::<Vec<_>>();
        let config = frozen_config(&conn)?;
        let existing_link = tagging_db::by_batch(&conn, batch_id)?;
        (selected, config, existing_link)
    };
    if let Some(link) = existing_link.filter(|link| {
        matches!(
            link.state.as_str(),
            "awaiting_user_start" | "started" | "interrupted"
        )
    }) {
        return reuse_existing_batch_plan(db, caller, link, &selection, limit, &config);
    }
    if selection.is_empty() {
        return Err(AppError::not_found("当前批次没有尚未生成建议的素材"));
    }
    let asset_ids = selection
        .iter()
        .map(|item| item.asset_id)
        .collect::<Vec<_>>();
    {
        let conn = db.lock()?;
        libraries::ensure_managed_assets(&conn, &asset_ids)?;
    }
    prepare(
        db,
        caller,
        PrepareInput {
            source: None,
            included_duplicate_item_ids: Vec::new(),
            existing_batch_id: Some(batch_id),
            selected_limit: limit,
            selection_source_ref: None,
            selection,
            config,
            operator_note: None,
        },
    )
}

/// Reuse the single approved operation attached to a batch. A partial run
/// shrinks the remaining eligible suggestions, so it cannot become a new plan.
fn reuse_existing_batch_plan(
    db: &Database,
    caller: &CallerContext,
    link: tagging_db::TaggingBatchLink,
    remaining: &[TaggingSelectionItem],
    selected_limit: Option<u32>,
    current_config: &FrozenTaggingConfig,
) -> AppResult<TaggingPreparation> {
    if link.state == "completed" {
        return Err(AppError::not_found("此打标批次已完成"));
    }
    if !matches!(link.state.as_str(), "awaiting_user_start" | "interrupted") {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    let conn = db.lock()?;
    authorization_service::validate_caller(&conn, caller)?;
    let plan = plans_db::plan(&conn, &link.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if plan.principal_id != caller.principal_id.as_str()
        || plan.library_id != caller.library.library_id.as_str()
        || plan.library_epoch != caller.library.library_epoch.as_str()
        || plan.review_status != "consumed"
        || plan.capability != "tagging.submit"
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let stored: TaggingPlanSnapshot = serde_json::from_str(
        plan.details_json
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
    )?;
    link_matches_snapshot(&link, &stored)?;
    validate_source_unchanged(&conn, caller, &stored)?;
    let operation_step = operations_db::operation_step(&conn, &link.operation_id, &link.step_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let deadline = link.deadline_at.or_else(|| {
        operation_step
            .started_at
            .map(|started| started.saturating_add(stored.max_seconds as i64))
    });
    let approval_expired =
        deadline.is_some_and(|deadline| deadline <= chrono::Utc::now().timestamp());
    let config_changed = full_hash(&serde_json::to_value(current_config)?)? != stored.config_hash;
    let frozen_assets: BTreeSet<_> = stored
        .selection
        .iter()
        .map(|item| (item.asset_id, item.mime_type.as_str()))
        .collect();
    let suggestions = ai::list_suggestions(&conn, link.batch_id)?;
    let authorized_remaining = stored
        .selection
        .iter()
        .filter(|item| {
            suggestions.iter().any(|suggestion| {
                suggestion.asset_id == item.asset_id
                    && item.suggestion_id.is_none_or(|id| suggestion.id == id)
                    && suggestion.status == "pending"
                    && suggestion.suggested_tags.is_empty()
                    && suggestion.suggested_description.trim().is_empty()
                    && !suggestion.has_analysis
            })
        })
        .map(|item| (item.asset_id, item.mime_type.as_str(), item.suggestion_id))
        .collect::<Vec<_>>();
    let requested_scope = remaining
        .iter()
        .map(|item| (item.asset_id, item.mime_type.as_str(), item.suggestion_id))
        .collect::<Vec<_>>();
    let remaining_scope_changed = remaining
        .iter()
        .any(|item| !frozen_assets.contains(&(item.asset_id, item.mime_type.as_str())))
        || requested_scope != authorized_remaining;
    let request_budget_exhausted = link.requests_attempted >= link.request_limit;
    if config_changed || approval_expired || request_budget_exhausted || remaining_scope_changed {
        let batch_id = link.batch_id;
        let operation_id = link.operation_id.clone();
        let step_id = link.step_id.clone();
        drop(conn);
        {
            let conn = db.lock()?;
            tagging_db::cancel_inactive_for_replan(
                &conn,
                &operation_id,
                &step_id,
                chrono::Utc::now().timestamp(),
            )?;
        }
        return prepare(
            db,
            caller,
            PrepareInput {
                source: None,
                included_duplicate_item_ids: Vec::new(),
                existing_batch_id: Some(batch_id),
                selected_limit,
                selection_source_ref: None,
                selection: remaining.to_vec(),
                config: current_config.clone(),
                operator_note: None,
            },
        );
    }
    libraries::ensure_managed_assets(
        &conn,
        &stored
            .selection
            .iter()
            .map(|item| item.asset_id)
            .collect::<Vec<_>>(),
    )?;
    let summary = stored.summary;
    drop(conn);
    Ok(TaggingPreparation {
        requires_approval: false,
        summary,
        preview: None,
        operation: Some(operation_view(&link)?),
    })
}

pub fn prepare_from_import(
    db: &Database,
    caller: &CallerContext,
    source_operation_id: &ResourceRef,
    source_step_id: &ResourceRef,
    included_duplicate_item_ids: &[String],
) -> AppResult<TaggingPreparation> {
    check_capability_entry(CapabilityId::TaggingPrepare, caller).map_err(AppError::from)?;
    let included = canonical_duplicate_ids(included_duplicate_item_ids)?;
    let (selection, config) = {
        let conn = db.lock()?;
        let selection = import_selection(
            &conn,
            source_operation_id.as_str(),
            source_step_id.as_str(),
            &included,
        )?;
        let asset_ids = selection
            .iter()
            .map(|item| item.asset_id)
            .collect::<Vec<_>>();
        if asset_ids.is_empty() {
            if tagging_db::by_source(&conn, source_operation_id.as_str(), source_step_id.as_str())?
                .is_some()
            {
                return Err(AppError::from(AgentErrorCode::ResourceConflict));
            }
            return Err(AppError::invalid_arg("导入成功收据中没有可打标素材"));
        }
        libraries::ensure_managed_assets(&conn, &asset_ids)?;
        let config = frozen_config(&conn)?;
        (selection, config)
    };
    prepare(
        db,
        caller,
        PrepareInput {
            source: Some((source_operation_id.as_str(), source_step_id.as_str())),
            included_duplicate_item_ids: included,
            existing_batch_id: None,
            selected_limit: None,
            selection_source_ref: None,
            selection,
            config,
            operator_note: None,
        },
    )
}

/// A3-03：把「用户选中的一批素材 + 指定的打标服务」映射成待审打标计划。
///
/// 这是模型可调用的入口（`tea_tagging_prepare`）。安全边界：
/// * 素材范围来自 `selection_ref`，只能解析调用方自己拥有的选择；模型无法凭空指定素材 ID。
/// * `service_ref` 必须等于当前**已绑定**的打标连接。绑定点是用户在打标页做出的决定，
///   直接用它冻结模型与预算；模型点另一家服务只会得到明确拒绝，而不是悄悄换一个
///   更贵的模型去外发。
/// * 真正的外发仍要 GUI 批准 + 单独「开始打标」，这里只产出 `pending` 计划。
pub fn prepare_from_selection(
    db: &Database,
    caller: &CallerContext,
    selection_ref: &ResourceRef,
    service_ref: &ResourceRef,
    instructions: &str,
) -> AppResult<TaggingPreparation> {
    check_capability_entry(CapabilityId::TaggingPrepare, caller).map_err(AppError::from)?;
    let resolved = crate::services::selections::resolve(db, caller, selection_ref)?;
    if resolved.asset_ids.is_empty() {
        return Err(AppError::invalid_arg("没有可提交的打标素材"));
    }
    if resolved.truncated {
        return Err(AppError::invalid_arg(
            "这次选择的素材太多，请缩小范围后再试",
        ));
    }
    let (selection, config) = {
        let conn = db.lock()?;
        libraries::ensure_managed_assets(&conn, &resolved.asset_ids)?;
        let mut items = Vec::with_capacity(resolved.asset_ids.len());
        for asset_id in &resolved.asset_ids {
            let asset = crate::db::assets::get(&conn, *asset_id)?;
            items.push(TaggingSelectionItem {
                asset_id: *asset_id,
                mime_type: asset.mime_type,
                suggestion_id: None,
                receipts: Vec::new(),
            });
        }
        let config = frozen_config(&conn)?;
        (items, config)
    };
    // 绑定的连接是用户的选择；模型只能确认它，不能替换它。
    if config.connection.id != service_ref.as_str() {
        return Err(AppError::invalid_arg(
            "指定的打标服务不是当前已绑定的服务，请先在打标页切换或改选当前服务",
        ));
    }
    // instructions 只进入审批摘要，让审批卡片如实反映模型想做什么；它不改变冻结参数。
    let operator_note = {
        let trimmed = instructions.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    prepare(
        db,
        caller,
        PrepareInput {
            source: None,
            included_duplicate_item_ids: Vec::new(),
            existing_batch_id: None,
            selected_limit: None,
            selection_source_ref: Some(selection_ref.as_str()),
            selection,
            config,
            operator_note,
        },
    )
}

fn prepare(
    db: &Database,
    caller: &CallerContext,
    input: PrepareInput<'_>,
) -> AppResult<TaggingPreparation> {
    let PrepareInput {
        source,
        included_duplicate_item_ids,
        existing_batch_id,
        selected_limit,
        selection_source_ref,
        selection,
        config,
        operator_note,
    } = input;
    if selection.is_empty() {
        return Err(AppError::invalid_arg("没有可提交的打标素材"));
    }
    if selection.len() > u32::MAX as usize {
        return Err(AppError::invalid_arg("打标素材数量超出计划范围"));
    }
    validate_video_scope(&selection, &config)?;
    let source_fingerprint = selection_fingerprint(
        source,
        &selection,
        &included_duplicate_item_ids,
        existing_batch_id,
        selected_limit,
        selection_source_ref,
    )?;
    let config_json = serde_json::to_value(&config)?;
    let config_hash = full_hash(&config_json)?;
    let (request_limit, request_config_json) = request_budget(&selection, &config)?;
    let summary = build_review_summary(
        source,
        &selection,
        &included_duplicate_item_ids,
        &config,
        request_limit,
    );
    // 模型的补充要求只影响人读的摘要；预算、分类和提示词版本都已冻结，
    // 所以它不能通过措辞扩大外发范围。
    let summary = match operator_note.as_deref() {
        Some(note) => {
            let mut text = summary;
            text.push_str("\n模型补充的打标要求：");
            text.push_str(note);
            text
        }
        None => summary,
    };
    let snapshot = TaggingPlanSnapshot {
        schema_version: 1,
        source_operation_id: source.map(|value| value.0.to_string()),
        source_step_id: source.map(|value| value.1.to_string()),
        included_duplicate_item_ids,
        source_fingerprint: source_fingerprint.clone(),
        existing_batch_id,
        selected_limit,
        selection_source_ref: selection_source_ref.map(|value| value.to_string()),
        selection,
        config_hash: config_hash.clone(),
        config,
        request_limit,
        max_seconds: MAX_APPROVED_TAGGING_SECONDS,
        request_config_json,
        summary: summary.clone(),
    };

    // A source step and a frozen batch scope can each have only one approved
    // operation. Replays return it; a changed source selection conflicts.
    {
        let conn = db.lock()?;
        let existing = if let Some((operation_id, step_id)) = source {
            tagging_db::by_source(&conn, operation_id, step_id)?
        } else if let Some(batch_id) = existing_batch_id {
            tagging_db::by_batch_and_fingerprint(&conn, batch_id, &source_fingerprint)?
        } else {
            None
        };
        if let Some(existing) = existing {
            if existing.state == "completed" {
                return Err(AppError::not_found("此打标范围已完成，没有待执行素材"));
            }
            if existing.state == "cancelled" && source.is_some() {
                return Err(AppError::from(AgentErrorCode::ResourceConflict));
            }
            if existing.state != "cancelled" {
                if existing.source_fingerprint != source_fingerprint {
                    return Err(AppError::from(AgentErrorCode::ResourceConflict));
                }
                if existing.config_hash != config_hash {
                    return Err(AppError::from(AgentErrorCode::PlanChanged));
                }
                let plan = plans_db::plan(&conn, &existing.plan_id)?
                    .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
                let stored: TaggingPlanSnapshot = serde_json::from_str(
                    plan.details_json
                        .as_deref()
                        .ok_or_else(|| AppError::internal("打标计划快照缺失"))?,
                )?;
                return Ok(TaggingPreparation {
                    requires_approval: false,
                    summary: stored.summary,
                    preview: None,
                    operation: Some(operation_view(&existing)?),
                });
            }
        }
    }

    let source_ref = source
        .map(|(operation_id, _)| {
            ResourceRef::from_trusted(operation_id.to_string()).map_err(AppError::from)
        })
        .transpose()?;
    let selection_ref = ResourceRef::from_trusted(format!("resource_{source_fingerprint}"))
        .map_err(AppError::from)?;
    let service_revision =
        ResourceRef::from_trusted(format!("resource_{config_hash}")).map_err(AppError::from)?;
    let service_ref = ResourceRef::generated("service");
    let model: ShortText = serde_json::from_value(serde_json::Value::String(
        snapshot.config.connection.model.clone(),
    ))
    .map_err(|_| AppError::invalid_arg("模型名称超出计划协议限制"))?;
    let summary_value: Instructions = serde_json::from_value(serde_json::Value::String(summary))
        .map_err(|_| AppError::internal("打标审批摘要超出协议限制"))?;
    let details_json = serde_json::to_string(&snapshot)?;
    let budget = BudgetSummary {
        max_requests: request_limit,
        max_tokens: 0,
        max_seconds: snapshot.max_seconds,
        max_items: snapshot.selection.len() as u32,
        monetary_limit: None,
    };
    let draft = plans::PlanDraft {
        capability: CapabilityId::TaggingSubmit,
        scope: PlanScopeSummary {
            scope_ref: ResourceRef::generated("tagging"),
            source_ref,
            selection_ref: Some(selection_ref),
            target_ref: None,
            service_ref: Some(service_ref),
            service_revision: Some(service_revision),
            model: Some(model),
        },
        item_count: snapshot.selection.len() as u32,
        summary: summary_value,
        details_json: Some(details_json),
        budget,
        ttl_seconds: 0,
    };
    let preview = plans::prepare_plan(db, caller, draft)?;
    Ok(TaggingPreparation {
        requires_approval: true,
        summary: preview.summary.as_str().to_string(),
        preview: Some(preview),
        operation: None,
    })
}

pub fn submit_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
    idempotency_key: &ResourceRef,
) -> AppResult<TaggingOperation> {
    check_capability_entry(CapabilityId::TaggingSubmit, caller).map_err(AppError::from)?;
    let record = {
        let conn = db.lock()?;
        authorization_service::validate_caller(&conn, caller)?;
        let record = plans_db::plan(&conn, plan_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if record.principal_id != caller.principal_id.as_str()
            || record.library_id != caller.library.library_id.as_str()
            || record.library_epoch != caller.library.library_epoch.as_str()
            || record.capability != "tagging.submit"
        {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        record
    };
    let stored: TaggingPlanSnapshot = serde_json::from_str(
        record
            .details_json
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
    )?;

    if let Some(existing) = existing_link_for_snapshot(db, &stored)? {
        if existing.state != "cancelled" || stored.source_operation_id.is_some() {
            return link_matches_snapshot(&existing, &stored)
                .and_then(|()| operation_view(&existing));
        }
    }

    let current_snapshot = capture_current_snapshot(db, caller, &stored)?;
    let current_json = serde_json::to_string(&current_snapshot)?;
    let current_draft = draft_from_record(&record, current_json)?;
    let mut created_batch_id = None;
    let submitted = plans::submit_plan_with_effect(
        db,
        caller,
        plan_ref,
        idempotency_key,
        &current_draft,
        |tx, operation| {
            let batch_id = if let Some(batch_id) = stored.existing_batch_id {
                ai::get_batch(tx, batch_id)?;
                ai::set_batch_mode(tx, batch_id, mode_for_config(&stored.config))?;
                batch_id
            } else {
                let asset_ids = stored
                    .selection
                    .iter()
                    .map(|item| item.asset_id)
                    .collect::<Vec<_>>();
                let batch = ai::create_batch_with_retag_in_tx(
                    tx,
                    &asset_ids,
                    mode_for_config(&stored.config),
                    RetagMode::Append,
                )?;
                batch.id
            };
            ai::set_batch_provenance(
                tx,
                batch_id,
                &stored.config.connection.model,
                None,
                &stored.config.connection.id,
                ai_cloud::PROMPT_VERSION,
                &stored.config_hash,
                &stored.request_config_json,
            )?;
            operations_db::bind_child_task_in_tx(
                tx,
                &operation.operation_id,
                &operation.step_id,
                "ai_batch_scope",
                &format!(
                    "{batch_id}:{}:{}",
                    stored.source_fingerprint, operation.operation_id
                ),
                chrono::Utc::now().timestamp(),
            )?;
            let suggestions = ai::list_suggestions(tx, batch_id)?;
            for (ordinal, selected) in stored.selection.iter().enumerate() {
                let suggestion = suggestions
                    .iter()
                    .find(|suggestion| {
                        suggestion.asset_id == selected.asset_id
                            && selected.suggestion_id.is_none_or(|id| id == suggestion.id)
                    })
                    .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
                operations_db::insert_item(
                    tx,
                    &operations_db::NewOperationItem {
                        operation_id: operation.operation_id.clone(),
                        step_id: operation.step_id.clone(),
                        item_id: tagging_db::operation_item_id(selected.asset_id),
                        ordinal: ordinal as i64,
                        input_fingerprint: Some(format!(
                            "suggestion:{}:asset:{}",
                            suggestion.id, selected.asset_id
                        )),
                        target_ref: Some(format!("asset:{}", selected.asset_id)),
                        created_at: chrono::Utc::now().timestamp(),
                    },
                )?;
            }
            tagging_db::insert_link_in_tx(
                tx,
                &tagging_db::TaggingBatchLink {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    plan_id: operation.plan_id.clone(),
                    batch_id,
                    source_operation_id: stored.source_operation_id.clone(),
                    source_step_id: stored.source_step_id.clone(),
                    source_fingerprint: stored.source_fingerprint.clone(),
                    config_hash: stored.config_hash.clone(),
                    request_limit: stored.request_limit as i64,
                    requests_attempted: 0,
                    deadline_at: None,
                    state: "awaiting_user_start".to_string(),
                },
                chrono::Utc::now().timestamp(),
            )?;
            created_batch_id = Some(batch_id);
            Ok(())
        },
    )?;
    let conn = db.lock()?;
    let step_id = operations_db::steps(&conn, submitted.operation.operation_id.as_str())?
        .into_iter()
        .next()
        .map(|step| step.step_id)
        .ok_or_else(|| AppError::internal("打标操作已提交但步骤缺失"))?;
    let link =
        tagging_db::by_operation(&conn, submitted.operation.operation_id.as_str(), &step_id)?
            .ok_or_else(|| AppError::internal("打标操作已提交但批次映射缺失"))?;
    if created_batch_id.is_some_and(|id| id != link.batch_id) {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    operation_view(&link)
}

pub fn start(
    db: &Database,
    caller: &CallerContext,
    operation_id: &ResourceRef,
    step_id: &ResourceRef,
) -> AppResult<StartedTagging> {
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    check_capability_entry(CapabilityId::PlansApprove, caller).map_err(AppError::from)?;
    let (link, stored) = {
        let conn = db.lock()?;
        authorization_service::validate_caller(&conn, caller)?;
        let owner = operations_db::operation_owner(&conn, operation_id.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if owner.principal_id != caller.principal_id.as_str()
            || owner.library_id != caller.library.library_id.as_str()
            || owner.library_epoch != caller.library.library_epoch.as_str()
        {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        let link = tagging_db::by_operation(&conn, operation_id.as_str(), step_id.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if !matches!(link.state.as_str(), "awaiting_user_start" | "interrupted") {
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        let plan = plans_db::plan(&conn, &link.plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if plan.review_status != "consumed" || plan.capability != "tagging.submit" {
            return Err(AppError::from(AgentErrorCode::ApprovalRequired));
        }
        let stored: TaggingPlanSnapshot = serde_json::from_str(
            plan.details_json
                .as_deref()
                .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
        )?;
        if u32::try_from(plan.max_seconds).ok() != Some(stored.max_seconds) {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        let current_time = chrono::Utc::now().timestamp();
        let original_start =
            operations_db::operation_step(&conn, operation_id.as_str(), step_id.as_str())?
                .and_then(|step| step.started_at);
        let deadline = link
            .deadline_at
            .or_else(|| original_start.map(|started| started + stored.max_seconds as i64));
        if deadline.is_some_and(|deadline| deadline <= current_time) {
            return Err(AppError::budget_exceeded(
                "已达到本次审核批准的打标时长上限，请重新准备并审核计划",
            ));
        }
        link_matches_snapshot(&link, &stored)?;
        let current_config = frozen_config(&conn)?;
        let current_hash = full_hash(&serde_json::to_value(&current_config)?)?;
        if current_hash != stored.config_hash {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        validate_source_unchanged(&conn, caller, &stored)?;
        let suggestions = ai::list_suggestions(&conn, link.batch_id)?;
        let allowed = stored
            .selection
            .iter()
            .filter_map(|item| {
                suggestions
                    .iter()
                    .find(|suggestion| {
                        suggestion.asset_id == item.asset_id
                            && item.suggestion_id.is_none_or(|id| suggestion.id == id)
                    })
                    .filter(|suggestion| {
                        suggestion.status == "pending"
                            && suggestion.suggested_tags.is_empty()
                            && suggestion.suggested_description.trim().is_empty()
                            && !suggestion.has_analysis
                    })
                    .map(|suggestion| suggestion.asset_id)
            })
            .collect::<Vec<_>>();
        if allowed.is_empty() {
            return Err(AppError::not_found("本计划范围内没有仍待处理的素材"));
        }
        let batch = ai::get_batch(&conn, link.batch_id)?;
        if batch.status == "processing" {
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        (link, stored)
    };

    // Credential IO occurs only after the database guard is released.
    let (connection, credential) =
        credentials::connection_with_system_credential(db, &stored.config.connection.id)?;
    let current_connection = ai_connections::profile_from_connection(&connection);
    if profile_hash(&current_connection)? != stored.config.connection_revision {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let mut config = stored.config.ai.clone();
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == stored.config.connection.id)
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    *profile = current_connection;
    profile.api_key = credential.unwrap_or_default();
    config.active_profile = stored.config.connection.id.clone();

    let started_at = chrono::Utc::now().timestamp();
    let (allowed_asset_ids, deadline_at) = {
        // Credential lookup above is intentionally outside the DB lock. It can
        // take time, so revalidate the approved source/config and begin the
        // durable operation in one transaction after that IO completes.
        let mut conn = db.lock()?;
        let tx = conn.transaction()?;
        let latest_link = validate_start_snapshot_in_conn(&tx, caller, &link, &stored)?;
        let suggestions = ai::list_suggestions(&tx, latest_link.batch_id)?;
        let mut operation_items =
            operations_db::items(&tx, &latest_link.operation_id, &latest_link.step_id)?;
        if operation_items.is_empty() {
            if latest_link.requests_attempted > 0 {
                return Err(AppError::from(AgentErrorCode::ResourceConflict));
            }
            for (ordinal, selected) in stored.selection.iter().enumerate() {
                let suggestion = suggestions
                    .iter()
                    .find(|suggestion| {
                        suggestion.asset_id == selected.asset_id
                            && selected.suggestion_id.is_none_or(|id| id == suggestion.id)
                    })
                    .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
                operations_db::insert_item(
                    &tx,
                    &operations_db::NewOperationItem {
                        operation_id: latest_link.operation_id.clone(),
                        step_id: latest_link.step_id.clone(),
                        item_id: tagging_db::operation_item_id(selected.asset_id),
                        ordinal: ordinal as i64,
                        input_fingerprint: Some(format!(
                            "suggestion:{}:asset:{}",
                            suggestion.id, selected.asset_id
                        )),
                        target_ref: Some(format!("asset:{}", selected.asset_id)),
                        created_at: started_at,
                    },
                )?;
            }
            operation_items =
                operations_db::items(&tx, &latest_link.operation_id, &latest_link.step_id)?;
        }
        let pending_asset_ids = operation_items
            .iter()
            .filter(|item| item.state == "pending")
            .filter_map(|item| item.target_ref.as_deref()?.strip_prefix("asset:"))
            .filter_map(|asset_id| asset_id.parse::<i64>().ok())
            .collect::<BTreeSet<_>>();
        let allowed_asset_ids = stored
            .selection
            .iter()
            .filter_map(|item| {
                suggestions
                    .iter()
                    .find(|suggestion| {
                        suggestion.asset_id == item.asset_id
                            && item.suggestion_id.is_none_or(|id| id == suggestion.id)
                    })
                    .filter(|suggestion| {
                        pending_asset_ids.contains(&suggestion.asset_id)
                            && suggestion.status == "pending"
                            && suggestion.suggested_tags.is_empty()
                            && suggestion.suggested_description.trim().is_empty()
                            && !suggestion.has_analysis
                    })
                    .map(|suggestion| suggestion.asset_id)
            })
            .collect::<Vec<_>>();
        if allowed_asset_ids.is_empty() {
            return Err(AppError::not_found("本计划范围内没有仍待处理的素材"));
        }
        libraries::ensure_managed_assets(
            &tx,
            &stored
                .selection
                .iter()
                .map(|item| item.asset_id)
                .collect::<Vec<_>>(),
        )?;
        if !tagging_db::begin_in_tx(
            &tx,
            &link.operation_id,
            &link.step_id,
            started_at,
            stored.max_seconds as i64,
        )? {
            if link
                .deadline_at
                .is_some_and(|deadline| deadline <= started_at)
            {
                return Err(AppError::budget_exceeded(
                    "已达到本次审核批准的打标时长上限，请重新准备并审核计划",
                ));
            }
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        let deadline_at = tagging_db::by_operation(&tx, &link.operation_id, &link.step_id)?
            .and_then(|current| current.deadline_at)
            .ok_or_else(|| AppError::internal("打标操作缺少持久时长截止点"))?;
        tx.commit()?;
        (allowed_asset_ids, deadline_at)
    };
    if deadline_at <= started_at {
        return Err(AppError::budget_exceeded(
            "已达到本次审核批准的打标时长上限，请重新准备并审核计划",
        ));
    }
    Ok(StartedTagging {
        operation_id: link.operation_id,
        step_id: link.step_id,
        batch_id: link.batch_id,
        request_limit: link.request_limit as u32,
        deadline_at,
        allowed_asset_ids,
        config,
        facets: stored.config.facets,
        facets_video: stored.config.facets_video,
        top_tags: stored.config.top_tags,
    })
}

/// Resolve the batch key for cancellation registration before the worker starts.
/// `start` repeats all checks; this early check only closes the GUI cancel race.
pub fn batch_id_for_start(
    db: &Database,
    caller: &CallerContext,
    operation_id: &ResourceRef,
    step_id: &ResourceRef,
) -> AppResult<i64> {
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    check_capability_entry(CapabilityId::PlansApprove, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    authorization_service::validate_caller(&conn, caller)?;
    let owner = operations_db::operation_owner(&conn, operation_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if owner.principal_id != caller.principal_id.as_str()
        || owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let link = tagging_db::by_operation(&conn, operation_id.as_str(), step_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if !matches!(link.state.as_str(), "awaiting_user_start" | "interrupted") {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    Ok(link.batch_id)
}

/// Read runtime-only settings through the tagging service so command handlers
/// do not reach into SQLite while coordinating the approved worker.
pub fn model_download_proxy(db: &Database) -> AppResult<String> {
    let conn = db.lock()?;
    Ok(settings::get_settings(&conn)?.model_download_proxy)
}

/// Reload the final batch projection after an approved worker finishes.
pub fn batch_result(db: &Database, batch_id: i64) -> AppResult<ai::AiBatch> {
    let conn = db.lock()?;
    ai::get_batch(&conn, batch_id)
}

/// Resolve the active batch attached to a GUI-owned operation so the generic
/// operation cancel command can signal the worker's in-memory cancellation
/// token after the durable cancel request is accepted.
pub fn batch_id_for_cancel(
    db: &Database,
    caller: &CallerContext,
    operation_id: &ResourceRef,
) -> AppResult<Option<i64>> {
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    check_capability_entry(CapabilityId::OperationsCancel, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    authorization_service::validate_caller(&conn, caller)?;
    let owner = operations_db::operation_owner(&conn, operation_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if owner.principal_id != caller.principal_id.as_str()
        || owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    for step in operations_db::steps(&conn, operation_id.as_str())? {
        if step.capability != "tagging.submit" {
            continue;
        }
        if let Some(link) = tagging_db::by_operation(&conn, operation_id.as_str(), &step.step_id)? {
            return Ok(Some(link.batch_id));
        }
    }
    Ok(None)
}

pub fn finish(db: &Database, operation_id: &str, step_id: &str, completed: bool) -> AppResult<()> {
    let conn = db.lock()?;
    tagging_db::finish_operation_step(
        &conn,
        operation_id,
        step_id,
        completed,
        chrono::Utc::now().timestamp(),
    )
}

pub fn finish_cancelled(db: &Database, operation_id: &str, step_id: &str) -> AppResult<()> {
    let conn = db.lock()?;
    tagging_db::finish_cancelled_operation_step(
        &conn,
        operation_id,
        step_id,
        chrono::Utc::now().timestamp(),
    )
}

pub fn cancellation_requested(db: &Database, operation_id: &str, step_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    Ok(tagging_db::by_operation(&conn, operation_id, step_id)?
        .is_some_and(|link| link.state == "cancelled"))
}

#[cfg(test)]
fn persist_request_attempt(db: &Database, operation_id: &str, step_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    tagging_db::consume_request_attempt(
        &conn,
        operation_id,
        step_id,
        0,
        chrono::Utc::now().timestamp(),
    )
}

pub fn persist_request_attempt_for_item(
    db: &Database,
    operation_id: &str,
    step_id: &str,
    asset_id: i64,
) -> AppResult<bool> {
    let conn = db.lock()?;
    tagging_db::consume_request_attempt(
        &conn,
        operation_id,
        step_id,
        asset_id,
        chrono::Utc::now().timestamp(),
    )
}

pub fn record_request_outcome(
    db: &Database,
    operation_id: &str,
    step_id: &str,
    asset_id: i64,
    succeeded: bool,
    error_code: Option<&str>,
) -> AppResult<()> {
    let conn = db.lock()?;
    tagging_db::record_request_outcome(
        &conn,
        operation_id,
        step_id,
        asset_id,
        succeeded,
        error_code,
        chrono::Utc::now().timestamp(),
    )
}

fn eligible_suggestions(suggestions: &[AiSuggestion]) -> Vec<&AiSuggestion> {
    suggestions
        .iter()
        .filter(|suggestion| {
            suggestion.status == "pending"
                && suggestion.suggested_tags.is_empty()
                && suggestion.suggested_description.trim().is_empty()
                && !suggestion.has_analysis
        })
        .collect()
}

fn canonical_duplicate_ids(values: &[String]) -> AppResult<Vec<String>> {
    let mut unique = BTreeSet::new();
    for value in values {
        if value.is_empty() || value.len() > 128 || !unique.insert(value.clone()) {
            return Err(AppError::invalid_arg("重复映射纳入清单无效或存在重复"));
        }
    }
    Ok(unique.into_iter().collect())
}

fn import_selection(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    include_duplicates: &[String],
) -> AppResult<Vec<TaggingSelectionItem>> {
    let operation = plans_db::operation(conn, operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if operation.capability != "imports.submit" {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let step = operations_db::operation_step(conn, operation_id, step_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if step.capability != "imports.submit" {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    if !matches!(
        step.state.as_str(),
        "succeeded" | "partially_succeeded" | "failed" | "cancelled"
    ) {
        return Err(AppError::conflict(
            "导入步骤尚未结束或仍需恢复，完成对账后再准备 AI 打标",
        ));
    }
    let items = operations_db::items(conn, operation_id, step_id)?;
    let receipts = operations_db::receipts(conn, operation_id, step_id)?;
    let included: BTreeSet<_> = include_duplicates.iter().cloned().collect();
    let mut seen_duplicate_ids = BTreeSet::new();
    let mut selected = BTreeMap::<i64, TaggingSelectionItem>::new();
    for item in items {
        if item.state != "succeeded" || item.effect_state != "registered" {
            continue;
        }
        let Some(receipt) = latest_item_receipt(&receipts, &item.item_id) else {
            continue;
        };
        if receipt.revision != item.revision
            || receipt.effect_state != "registered"
            || receipt.error_code.is_some()
        {
            continue;
        }
        let include = match receipt.event.as_str() {
            "item_imported" => true,
            "duplicate_mapped" if included.contains(&item.item_id) => {
                seen_duplicate_ids.insert(item.item_id.clone());
                true
            }
            "duplicate_mapped" => false,
            _ => false,
        };
        if !include {
            continue;
        }
        let asset_id = receipt
            .db_ref
            .as_deref()
            .and_then(|value| value.strip_prefix("asset:"))
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
        let asset = assets::get_managed(conn, asset_id)?;
        let selection = selected
            .entry(asset_id)
            .or_insert_with(|| TaggingSelectionItem {
                asset_id,
                mime_type: asset.mime_type.clone(),
                suggestion_id: None,
                receipts: Vec::new(),
            });
        selection.receipts.push(ReceiptReference {
            receipt_id: receipt.receipt_id.clone(),
            item_id: item.item_id,
            event: receipt.event.clone(),
            asset_id,
        });
    }
    if seen_duplicate_ids.len() != included.len() {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let selected = selected.into_values().collect::<Vec<_>>();
    let asset_ids = selected
        .iter()
        .map(|item| item.asset_id)
        .collect::<Vec<_>>();
    if !asset_ids.is_empty() {
        libraries::ensure_managed_assets(conn, &asset_ids)?;
    }
    Ok(selected)
}

fn latest_item_receipt<'a>(
    receipts: &'a [OperationReceiptRecord],
    item_id: &str,
) -> Option<&'a OperationReceiptRecord> {
    receipts
        .iter()
        .filter(|receipt| receipt.item_id.as_deref() == Some(item_id))
        .max_by_key(|receipt| (receipt.revision, receipt.recorded_at))
}

fn frozen_config(conn: &Connection) -> AppResult<FrozenTaggingConfig> {
    let Some(connection) = ai_connections::binding_for(conn, "tagging")? else {
        return Err(AppError::msg(
            "尚未绑定打标服务，请先在打标页选择在线或本地模型",
        ));
    };
    let mut app_settings = settings::get_settings(conn)?;
    let raw_connection_profile = ai_connections::profile_from_connection(&connection);
    let connection_revision = profile_hash(&raw_connection_profile)?;
    let connection_profile = sanitize_connection_for_snapshot(raw_connection_profile)?;
    ai_connections::apply_profile(&mut app_settings.ai, connection_profile.clone());
    // A tagging plan needs only its bound connection. Keeping unrelated legacy
    // profiles could persist credentials embedded in their endpoint strings.
    app_settings
        .ai
        .profiles
        .retain(|profile| profile.id == connection_profile.id);
    app_settings.ai.system_prompt_tagging = settings::DEFAULT_TAGGING_PROMPT.to_string();
    let facets = tag_facets::build_prompt_context(conn, "image")?;
    let facets_video = tag_facets::build_prompt_context(conn, "video")?;
    ai_cloud::validate_tagging_facet_descriptions(facets.iter().chain(facets_video.iter()))?;
    Ok(FrozenTaggingConfig {
        ai: app_settings.ai,
        connection: connection_profile,
        connection_revision,
        facets,
        facets_video,
        top_tags: tags::top_tags_per_facet(conn, 60)?,
        system_prompt: settings::DEFAULT_TAGGING_PROMPT.to_string(),
    })
}

fn validate_video_scope(
    selection: &[TaggingSelectionItem],
    config: &FrozenTaggingConfig,
) -> AppResult<()> {
    if ai_cloud::is_managed_ollama_profile(&config.connection)
        && !ai_cloud::model_supports_vision(&config.connection.model)
    {
        return Err(AppError::unsupported(format!(
            "当前本地模型「{}」不支持视觉输入，无法执行图片或视频打标",
            config.connection.model
        )));
    }
    let has_video = selection
        .iter()
        .any(|item| item.mime_type.starts_with("video/"));
    if has_video && !config.ai.video_tagging {
        return Err(AppError::unsupported(
            "视频 AI 打标未开启，请先在设置中启用",
        ));
    }
    if has_video
        && config.ai.video_tagging_mode == "frames"
        && !crate::services::video::ffmpeg_available()
    {
        return Err(AppError::unsupported(
            "当前视频抽帧计划需要可用的 ffmpeg，请更换为封面打标或安装 ffmpeg",
        ));
    }
    Ok(())
}

fn request_budget(
    selection: &[TaggingSelectionItem],
    config: &FrozenTaggingConfig,
) -> AppResult<(u32, String)> {
    let mut image_units = 0u32;
    let mut video_units = 0u32;
    for item in selection {
        let is_video = item.mime_type.starts_with("video/");
        if is_video && config.ai.video_tagging_mode == "frames" {
            video_units = video_units
                .checked_add(config.ai.video_frame_count.clamp(2, 8) as u32)
                .ok_or_else(|| AppError::invalid_arg("视频请求预算超出范围"))?;
        } else if is_video {
            video_units = video_units.saturating_add(1);
        } else {
            image_units = image_units.saturating_add(1);
        }
    }
    // Use a conservative protocol bound: native structured fallback plus three
    // format tiers, resolution fallback, repair request, and one outer retry.
    let units = image_units
        .checked_add(video_units)
        .ok_or_else(|| AppError::invalid_arg("打标请求预算超出范围"))?;
    let max_requests = units
        .checked_mul(MAX_REQUESTS_PER_IMAGE_UNIT)
        .ok_or_else(|| AppError::invalid_arg("打标请求预算超出范围"))?;
    let request_config = serde_json::json!({
        "promptVersion": ai_cloud::PROMPT_VERSION,
        "systemPrompt": config.system_prompt,
        "connection": config.connection,
        "imagePreprocess": {"format":"image/jpeg","maxPixels": crate::services::imaging::AI_IMAGE_MAX_PX},
        "video": {
            "enabled": config.ai.video_tagging,
            "mode": config.ai.video_tagging_mode,
            "frameCount": config.ai.video_frame_count
        },
        "facets": config.facets,
        "facetsVideo": config.facets_video,
        "topTags": config.top_tags,
        "maxAttemptsPerImageUnit": MAX_REQUESTS_PER_IMAGE_UNIT,
        "maxSeconds": MAX_APPROVED_TAGGING_SECONDS,
        "requestUnits": units,
        "maxRequests": max_requests
    });
    Ok((max_requests, serde_json::to_string(&request_config)?))
}

fn build_review_summary(
    source: Option<(&str, &str)>,
    selection: &[TaggingSelectionItem],
    included_duplicates: &[String],
    config: &FrozenTaggingConfig,
    max_requests: u32,
) -> String {
    let profile = &config.connection;
    let categories = config
        .facets
        .iter()
        .map(|facet| facet.display_name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let video = if config.ai.video_tagging_mode == "frames" {
        format!("视频按最多{}帧", config.ai.video_frame_count)
    } else {
        "视频只发送封面".to_string()
    };
    let source_text = source.map_or_else(
        || "素材来自当前 AI 批次".to_string(),
        |(operation_id, step_id)| format!("素材来自导入操作 {operation_id} / 步骤 {step_id}"),
    );
    format!(
        "将把 {count} 项素材发送给 {service}（{endpoint}，模型 {model}）。{source_text}；图片使用 imaging 压缩后的 JPEG（最长边 {max_px}px），{video}；分类：{categories}。包含 {duplicate_count} 项明确纳入的重复映射。使用提示词版本 {prompt}；最多尝试 {max_requests} 次模型请求（每图像单元最多 {per_unit} 次，含格式/尺寸回退、结构修复和一次重试）。API 密钥由系统凭据服务读取，不会写入计划。连接限流仍生效。",
        count = selection.len(),
        service = profile.name,
        endpoint = profile.base_url,
        model = profile.model,
        source_text = source_text,
        max_px = crate::services::imaging::AI_IMAGE_MAX_PX,
        video = video,
        categories = categories,
        duplicate_count = included_duplicates.len(),
        prompt = ai_cloud::PROMPT_VERSION,
        max_requests = max_requests,
        per_unit = MAX_REQUESTS_PER_IMAGE_UNIT,
    )
}

fn selection_fingerprint(
    source: Option<(&str, &str)>,
    selection: &[TaggingSelectionItem],
    included_duplicates: &[String],
    existing_batch_id: Option<i64>,
    selected_limit: Option<u32>,
    selection_source_ref: Option<&str>,
) -> AppResult<String> {
    full_hash(&serde_json::json!({
        "sourceOperationId": source.map(|value| value.0),
        "sourceStepId": source.map(|value| value.1),
        "selection": selection,
        "includedDuplicateItemIds": included_duplicates,
        "existingBatchId": existing_batch_id,
        "selectedLimit": selected_limit,
        "selectionSourceRef": selection_source_ref
    }))
}

fn full_hash(value: &serde_json::Value) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn profile_hash(profile: &settings::ApiProfile) -> AppResult<String> {
    let mut safe = profile.clone();
    safe.api_key.clear();
    full_hash(&serde_json::to_value(safe)?)
}

fn sanitize_connection_for_snapshot(
    mut profile: settings::ApiProfile,
) -> AppResult<settings::ApiProfile> {
    profile.api_key.clear();
    let mut endpoint = reqwest::Url::parse(&profile.base_url)
        .map_err(|_| AppError::invalid_arg("打标服务地址格式无效，无法安全地准备审批"))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(AppError::invalid_arg(
            "打标服务地址必须使用 HTTP 或 HTTPS 主机地址",
        ));
    }
    endpoint
        .set_username("")
        .map_err(|_| AppError::invalid_arg("打标服务地址中的凭据无法安全处理"))?;
    endpoint
        .set_password(None)
        .map_err(|_| AppError::invalid_arg("打标服务地址中的凭据无法安全处理"))?;
    endpoint.set_fragment(None);
    if endpoint.query().is_some() {
        let keys = endpoint
            .query_pairs()
            .map(|(key, _)| key.into_owned())
            .collect::<Vec<_>>();
        endpoint
            .query_pairs_mut()
            .clear()
            .extend_pairs(keys.iter().map(|key| (key.as_str(), "[REDACTED]")));
    }
    profile.base_url = endpoint.to_string();
    Ok(profile)
}

fn mode_for_config(config: &FrozenTaggingConfig) -> &'static str {
    if ai_cloud::is_managed_ollama_profile(&config.connection) {
        "local"
    } else {
        "cloud"
    }
}

fn operation_view(link: &tagging_db::TaggingBatchLink) -> AppResult<TaggingOperation> {
    Ok(TaggingOperation {
        operation_id: link.operation_id.clone(),
        step_id: link.step_id.clone(),
        batch_id: link.batch_id,
        request_limit: u32::try_from(link.request_limit)
            .map_err(|_| AppError::internal("持久请求预算越界"))?,
        requests_attempted: u32::try_from(link.requests_attempted)
            .map_err(|_| AppError::internal("持久请求计数越界"))?,
    })
}

fn existing_link_for_snapshot(
    db: &Database,
    snapshot: &TaggingPlanSnapshot,
) -> AppResult<Option<tagging_db::TaggingBatchLink>> {
    let conn = db.lock()?;
    if let (Some(operation_id), Some(step_id)) = (
        snapshot.source_operation_id.as_deref(),
        snapshot.source_step_id.as_deref(),
    ) {
        tagging_db::by_source(&conn, operation_id, step_id)
    } else if let Some(batch_id) = snapshot.existing_batch_id {
        tagging_db::by_batch_and_fingerprint(&conn, batch_id, &snapshot.source_fingerprint)
    } else {
        Ok(None)
    }
}

fn link_matches_snapshot(
    link: &tagging_db::TaggingBatchLink,
    snapshot: &TaggingPlanSnapshot,
) -> AppResult<()> {
    if link.source_fingerprint != snapshot.source_fingerprint
        || link.config_hash != snapshot.config_hash
        || link.request_limit != snapshot.request_limit as i64
    {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    Ok(())
}

fn validate_start_snapshot_in_conn(
    conn: &Connection,
    caller: &CallerContext,
    link: &tagging_db::TaggingBatchLink,
    stored: &TaggingPlanSnapshot,
) -> AppResult<tagging_db::TaggingBatchLink> {
    authorization_service::validate_caller(conn, caller)?;
    let owner = operations_db::operation_owner(conn, &link.operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if owner.principal_id != caller.principal_id.as_str()
        || owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let current_link = tagging_db::by_operation(conn, &link.operation_id, &link.step_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if !matches!(
        current_link.state.as_str(),
        "awaiting_user_start" | "interrupted"
    ) {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    link_matches_snapshot(&current_link, stored)?;
    if current_link.requests_attempted >= current_link.request_limit {
        return Err(AppError::budget_exceeded(
            "已达到本次审核批准的请求次数上限，请重新准备并审核计划",
        ));
    }
    let plan = plans_db::plan(conn, &current_link.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if plan.principal_id != caller.principal_id.as_str()
        || plan.library_id != caller.library.library_id.as_str()
        || plan.library_epoch != caller.library.library_epoch.as_str()
        || plan.review_status != "consumed"
        || plan.capability != "tagging.submit"
        || u32::try_from(plan.max_seconds).ok() != Some(stored.max_seconds)
    {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    capture_current_snapshot_in_conn(conn, caller, stored)?;
    Ok(current_link)
}

fn capture_current_snapshot(
    db: &Database,
    caller: &CallerContext,
    original: &TaggingPlanSnapshot,
) -> AppResult<TaggingPlanSnapshot> {
    let conn = db.lock()?;
    capture_current_snapshot_in_conn(&conn, caller, original)
}

fn capture_current_snapshot_in_conn(
    conn: &Connection,
    caller: &CallerContext,
    original: &TaggingPlanSnapshot,
) -> AppResult<TaggingPlanSnapshot> {
    validate_source_unchanged(conn, caller, original)?;
    let selection = if let Some(batch_id) = original.existing_batch_id {
        let suggestions = ai::list_suggestions(conn, batch_id)?;
        let eligible = eligible_suggestions(&suggestions);
        let limit = original.selected_limit.unwrap_or(u32::MAX) as usize;
        let current = eligible
            .into_iter()
            .take(limit)
            .map(|suggestion| TaggingSelectionItem {
                asset_id: suggestion.asset_id,
                mime_type: suggestion.mime_type.clone().unwrap_or_default(),
                suggestion_id: Some(suggestion.id),
                receipts: Vec::new(),
            })
            .collect::<Vec<_>>();
        if current != original.selection {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        current
    } else if let Some(selection_ref) = original.selection_source_ref.as_deref() {
        selection_source_items(conn, caller, selection_ref)?
    } else {
        import_selection(
            conn,
            original
                .source_operation_id
                .as_deref()
                .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
            original
                .source_step_id
                .as_deref()
                .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
            &original.included_duplicate_item_ids,
        )?
    };
    if selection != original.selection {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let config = frozen_config(conn)?;
    let config_hash = full_hash(&serde_json::to_value(&config)?)?;
    if config_hash != original.config_hash {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let request_config_json = request_budget(&selection, &config)?.1;
    Ok(TaggingPlanSnapshot {
        schema_version: 1,
        source_operation_id: original.source_operation_id.clone(),
        source_step_id: original.source_step_id.clone(),
        included_duplicate_item_ids: original.included_duplicate_item_ids.clone(),
        source_fingerprint: original.source_fingerprint.clone(),
        existing_batch_id: original.existing_batch_id,
        selected_limit: original.selected_limit,
        selection_source_ref: original.selection_source_ref.clone(),
        selection,
        config_hash,
        config,
        request_limit: original.request_limit,
        max_seconds: original.max_seconds,
        request_config_json,
        summary: original.summary.clone(),
    })
}

/// Re-read the frozen agent selection and rebuild its items exactly as
/// `prepare_from_selection` did, so submit/start can prove the素材集合 wasn't swapped.
///
/// The selection row is insert-only and primary-keyed, so it cannot be rebound;
/// its own TTL only governs handing off a *new* plan, while an already approved
/// plan is governed by the plan's expiry. Here we therefore re-check authority
/// (principal / library / epoch) and the media types, not the handoff window.
fn selection_source_items(
    conn: &Connection,
    caller: &CallerContext,
    selection_ref: &str,
) -> AppResult<Vec<TaggingSelectionItem>> {
    let stored = crate::db::selections::get(conn, selection_ref)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if stored.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if stored.library_id != caller.library.library_id.as_str()
        || stored.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if stored.truncated {
        return Err(AppError::invalid_arg(
            "这次选择的素材太多，请缩小范围后再试",
        ));
    }
    let mut items = Vec::with_capacity(stored.asset_ids.len());
    for asset_id in &stored.asset_ids {
        let asset = crate::db::assets::get(conn, *asset_id)?;
        items.push(TaggingSelectionItem {
            asset_id: *asset_id,
            mime_type: asset.mime_type,
            suggestion_id: None,
            receipts: Vec::new(),
        });
    }
    Ok(items)
}

fn validate_source_unchanged(
    conn: &Connection,
    caller: &CallerContext,
    snapshot: &TaggingPlanSnapshot,
) -> AppResult<()> {
    if snapshot.existing_batch_id.is_some() {
        return Ok(());
    }
    if let Some(selection_ref) = snapshot.selection_source_ref.as_deref() {
        let selection = selection_source_items(conn, caller, selection_ref)?;
        if selection != snapshot.selection {
            return Err(AppError::from(AgentErrorCode::PlanChanged));
        }
        return Ok(());
    }
    let selection = import_selection(
        conn,
        snapshot
            .source_operation_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
        snapshot
            .source_step_id
            .as_deref()
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?,
        &snapshot.included_duplicate_item_ids,
    )?;
    if selection != snapshot.selection {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    Ok(())
}

fn draft_from_record(record: &PlanRecord, details_json: String) -> AppResult<plans::PlanDraft> {
    let scope_ref = ResourceRef::from_trusted(record.scope_ref.clone()).map_err(AppError::from)?;
    let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
        value
            .as_ref()
            .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
            .transpose()
    };
    let summary = serde_json::from_value(serde_json::Value::String(record.summary.clone()))
        .map_err(|_| AppError::internal("持久计划摘要损坏"))?;
    let monetary_limit = match (&record.currency, record.amount_microunits) {
        (Some(currency), Some(amount)) => Some(MoneyLimit {
            currency: serde_json::from_value(serde_json::Value::String(currency.clone()))
                .map_err(|_| AppError::internal("持久计划币种损坏"))?,
            amount_microunits: u32::try_from(amount)
                .map_err(|_| AppError::internal("持久预算金额越界"))?,
        }),
        (None, None) => None,
        _ => return Err(AppError::internal("持久计划预算不完整")),
    };
    Ok(plans::PlanDraft {
        capability: CapabilityId::TaggingSubmit,
        scope: PlanScopeSummary {
            scope_ref,
            source_ref: optional_ref(&record.source_ref)?,
            selection_ref: optional_ref(&record.selection_ref)?,
            target_ref: optional_ref(&record.target_ref)?,
            service_ref: optional_ref(&record.service_ref)?,
            service_revision: optional_ref(&record.service_revision)?,
            model: record
                .model
                .clone()
                .map(|model| {
                    serde_json::from_value(serde_json::Value::String(model))
                        .map_err(|_| AppError::internal("持久模型名称损坏"))
                })
                .transpose()?,
        },
        item_count: u32::try_from(record.item_count)
            .map_err(|_| AppError::internal("持久素材数量越界"))?,
        summary,
        details_json: Some(details_json),
        budget: BudgetSummary {
            max_requests: u32::try_from(record.max_requests)
                .map_err(|_| AppError::internal("持久请求预算越界"))?,
            max_tokens: u32::try_from(record.max_tokens)
                .map_err(|_| AppError::internal("持久 token 预算越界"))?,
            max_seconds: u32::try_from(record.max_seconds)
                .map_err(|_| AppError::internal("持久时长预算越界"))?,
            max_items: u32::try_from(record.max_items)
                .map_err(|_| AppError::internal("持久素材预算越界"))?,
            monetary_limit,
        },
        ttl_seconds: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::agent::ResourceRef;
    use crate::db::{ai_connections, operations};
    use crate::services::thumbnail::ThumbnailService;
    use crate::services::{
        authorization::{self, TrustedAdapterIdentity},
        import_execute, import_prepare, import_submission, managed_library,
    };
    use std::sync::atomic::AtomicBool;

    fn fixture(root: &std::path::Path) -> (Database, CallerContext) {
        let conn = crate::db::init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        ai_connections::upsert(
            &conn,
            "tagging-test",
            "Tagging test",
            "cloud",
            "openai_chat",
            "https://embedded-user:embedded-pass@example.invalid/v1?api_key=query-secret&version=3",
            "vision-test-model",
            None,
        )
        .unwrap();
        ai_connections::bind_usage(&conn, "tagging", "tagging-test").unwrap();
        let caller = authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_tagging_test".to_string(),
                instance_id: "instance_tagging_test".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap();
        (Database::new(conn), caller)
    }

    fn import_file(
        db: &Database,
        caller: &CallerContext,
        thumbs: &ThumbnailService,
        file: &std::path::Path,
    ) -> ResourceRef {
        let prepared = import_prepare::prepare_import(
            db,
            caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![file.to_string_lossy().into_owned()],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap();
        let accepted = import_submission::confirm(
            db,
            caller,
            &prepared.plan_ref,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap();
        let operation_ref = accepted.operation.operation_id;
        import_execute::execute_import(
            db,
            thumbs,
            caller,
            &prepared.plan_ref,
            &operation_ref,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        operation_ref
    }

    fn import_step_and_item(db: &Database, operation_ref: &ResourceRef) -> (String, String) {
        let conn = db.lock().unwrap();
        let step = operations::steps(&conn, operation_ref.as_str())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let item = operations::items(&conn, operation_ref.as_str(), &step.step_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        (step.step_id, item.item_id)
    }

    fn write_test_image(path: &std::path::Path, seed: u8) {
        image::RgbImage::from_fn(24, 24, |x, y| {
            image::Rgb([(x as u8).wrapping_add(seed), y as u8, seed])
        })
        .save(path)
        .unwrap();
    }

    fn started_tagging_fixture() -> (
        Vec<tempfile::TempDir>,
        Database,
        CallerContext,
        TaggingOperation,
    ) {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("started.jpg");
        write_test_image(&file, 91);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let preparation = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = preparation.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("started_fixture"),
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
        }
        (vec![managed, source, cache], db, caller, operation)
    }

    fn mock_429_server() -> (String, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut target_len = None;
            let mut buffer = [0; 4096];
            while target_len.is_none_or(|target| request.len() < target) {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if target_len.is_none() {
                    if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        target_len = Some(header_end + 4 + content_length);
                    }
                }
            }
            let body = r#"{"error":{"message":"provider rate limit"}}"#;
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });
        (format!("http://{address}/v1"), worker)
    }

    #[test]
    fn approved_submit_is_atomic_idempotent_and_does_not_start_network_work() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("one.jpg");
        let second_file = source.path().join("two.jpg");
        write_test_image(&file, 1);
        write_test_image(&second_file, 2);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let import_operation = import_file(&db, &caller, &thumbs, &file);
        let _second_import_operation = import_file(&db, &caller, &thumbs, &second_file);
        let _ = import_step_and_item(&db, &import_operation);
        let asset_ids = {
            let conn = db.lock().unwrap();
            let mut statement = conn.prepare("SELECT id FROM assets ORDER BY id").unwrap();
            statement
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let batch_id = {
            let conn = db.lock().unwrap();
            ai::create_batch(&conn, &asset_ids, "cloud").unwrap().id
        };

        let prepared = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(prepared.requires_approval);
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let current_draft = {
            let conn = db.lock().unwrap();
            let record = plans_db::plan(&conn, preview.plan_id.as_str())
                .unwrap()
                .unwrap();
            draft_from_record(&record, record.details_json.clone().unwrap()).unwrap()
        };
        assert!(plans::submit_plan_with_effect(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
            &current_draft,
            |_, _| Err(AppError::internal("injected child mapping failure")),
        )
        .is_err());
        {
            let conn = db.lock().unwrap();
            assert_eq!(
                plans_db::plan(&conn, preview.plan_id.as_str())
                    .unwrap()
                    .unwrap()
                    .review_status,
                "approved",
                "child mapping failure must roll back approval consumption"
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM operations", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                2,
                "only the two source import operations pre-exist"
            );
        }
        let submitted = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap();
        assert_eq!(submitted.batch_id, batch_id);
        assert_eq!(
            submitted.request_limit,
            MAX_REQUESTS_PER_IMAGE_UNIT * asset_ids.len() as u32
        );

        let replay = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap();
        assert_eq!(replay.operation_id, submitted.operation_id);
        assert_eq!(replay.batch_id, submitted.batch_id);

        {
            let conn = db.lock().unwrap();
            let suggestion = ai::list_suggestions(&conn, batch_id).unwrap()[0].id;
            conn.execute(
                r#"UPDATE ai_suggestions SET suggested_tags='{"scene":["已生成"]}' WHERE id=?1"#,
                [suggestion],
            )
            .unwrap();
        }
        let resumed = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(!resumed.requires_approval);
        assert_eq!(
            resumed.operation.unwrap().operation_id,
            submitted.operation_id
        );

        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT status FROM ai_batches WHERE id=?1",
                [batch_id],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
            "pending",
            "submit must leave the batch awaiting explicit GUI start"
        );
        assert_eq!(
            conn.query_row(
                "SELECT requests_attempted FROM tagging_batch_links WHERE batch_id=?1",
                [batch_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0,
            "plan submission must not consume request budget"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM operation_child_tasks WHERE operation_id=?1",
                [&submitted.operation_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1,
            "batch child mapping is committed with operation creation"
        );
        assert!(tagging_db::begin(
            &conn,
            &submitted.operation_id,
            &submitted.step_id,
            10,
            MAX_APPROVED_TAGGING_SECONDS as i64,
        )
        .unwrap());
        let asset_id = ai::list_suggestions(&conn, batch_id).unwrap()[0].asset_id;
        for _ in 0..submitted.request_limit {
            assert!(tagging_db::consume_request_attempt(
                &conn,
                &submitted.operation_id,
                &submitted.step_id,
                asset_id,
                11,
            )
            .unwrap());
        }
        assert!(!tagging_db::consume_request_attempt(
            &conn,
            &submitted.operation_id,
            &submitted.step_id,
            asset_id,
            12,
        )
        .unwrap());
        assert_eq!(
            tagging_db::by_batch(&conn, batch_id)
                .unwrap()
                .unwrap()
                .requests_attempted,
            submitted.request_limit as i64
        );
        operations::reconcile_startup(&conn, 13).unwrap();
        assert_eq!(tagging_db::recover_interrupted(&conn, 13).unwrap(), 1);
        assert_eq!(
            tagging_db::by_batch(&conn, batch_id)
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
        assert_eq!(
            tagging_db::by_batch(&conn, batch_id)
                .unwrap()
                .unwrap()
                .deadline_at,
            Some(10 + MAX_APPROVED_TAGGING_SECONDS as i64),
            "startup recovery must preserve the first-start deadline"
        );
        assert!(tagging_db::begin(
            &conn,
            &submitted.operation_id,
            &submitted.step_id,
            3_609,
            MAX_APPROVED_TAGGING_SECONDS as i64,
        )
        .unwrap());
        operations::reconcile_startup(&conn, 3_610).unwrap();
        assert_eq!(tagging_db::recover_interrupted(&conn, 3_610).unwrap(), 1);
        assert!(!tagging_db::begin(
            &conn,
            &submitted.operation_id,
            &submitted.step_id,
            3_611,
            MAX_APPROVED_TAGGING_SECONDS as i64,
        )
        .unwrap());
    }

    #[test]
    fn cancelled_tagging_step_and_operation_keep_cancelled_terminal_state() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("cancelled.jpg");
        write_test_image(&file, 31);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let preparation = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = preparation.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("cancelled_operation"),
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
        }
        let cancel_snapshot =
            crate::services::operations::cancel_tagging_batch(&db, &caller, batch_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            cancel_snapshot.phase,
            crate::contracts::agent::OperationPhase::Cancelling
        );
        assert_eq!(
            tagging_db::by_operation(
                &db.lock().unwrap(),
                &operation.operation_id,
                &operation.step_id
            )
            .unwrap()
            .unwrap()
            .state,
            "cancelled",
            "legacy batch cancellation must durably close its tagging link"
        );
        assert!(
            !persist_request_attempt(&db, &operation.operation_id, &operation.step_id).unwrap(),
            "generic operation cancellation must close the durable network-send gate"
        );
        {
            let conn = db.lock().unwrap();
            tagging_db::finish_cancelled_operation_step(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                now + 1,
            )
            .unwrap();
        }

        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state FROM operations WHERE operation_id=?1",
                [&operation.operation_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "cancelled"
        );
        assert_eq!(
            operations::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "cancelled"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        assert_eq!(ai::get_batch(&conn, batch_id).unwrap().status, "cancelled");
        assert_eq!(
            ai::list_suggestions(&conn, batch_id).unwrap()[0].status,
            "pending",
            "cancel must preserve work that the model did not finish"
        );
    }

    #[test]
    fn interrupted_tagging_step_commits_batch_and_operation_together() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        let batch_id = {
            let conn = db.lock().unwrap();
            let link = tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap();
            ai::set_batch_status(&conn, link.batch_id, "processing").unwrap();
            link.batch_id
        };

        finish(&db, &operation.operation_id, &operation.step_id, false).unwrap();

        let conn = db.lock().unwrap();
        assert_eq!(
            plans_db::operation(&conn, &operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "interrupted"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
        assert_eq!(
            ai::get_batch(&conn, batch_id).unwrap().status,
            "interrupted"
        );
    }

    #[test]
    fn interrupted_tagging_finish_rolls_back_batch_when_operation_update_fails() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        let batch_id = {
            let conn = db.lock().unwrap();
            let link = tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap();
            ai::set_batch_status(&conn, link.batch_id, "processing").unwrap();
            conn.execute(
                "UPDATE operations SET state='succeeded' WHERE operation_id=?1",
                [&operation.operation_id],
            )
            .unwrap();
            link.batch_id
        };

        assert!(finish(&db, &operation.operation_id, &operation.step_id, false).is_err());

        let conn = db.lock().unwrap();
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "started"
        );
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "running"
        );
        assert_eq!(ai::get_batch(&conn, batch_id).unwrap().status, "processing");
    }

    #[test]
    fn restarted_tagging_keeps_sent_asset_unknown_and_does_not_authorize_a_replay() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        let asset_id = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT asset_id FROM ai_suggestions WHERE batch_id=?1",
                [operation.batch_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };

        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
        )
        .unwrap());
        {
            let conn = db.lock().unwrap();
            let item = operations_db::items(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .remove(0);
            assert_eq!(item.state, "running");
            assert_eq!(item.effect_state, "unknown");
            assert_eq!(
                operations_db::receipts(&conn, &operation.operation_id, &operation.step_id)
                    .unwrap()
                    .len(),
                1
            );
            operations_db::reconcile_startup(&conn, chrono::Utc::now().timestamp()).unwrap();
            tagging_db::recover_interrupted(&conn, chrono::Utc::now().timestamp()).unwrap();
        }

        {
            let conn = db.lock().unwrap();
            let item = operations_db::items(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .remove(0);
            assert_eq!(item.state, "outcome_unknown");
            assert_eq!(item.effect_state, "unknown");
            assert_eq!(
                operations_db::receipts(&conn, &operation.operation_id, &operation.step_id)
                    .unwrap()
                    .last()
                    .unwrap()
                    .event,
                "tagging_request_outcome_unknown_after_restart"
            );
            assert_eq!(
                plans_db::operation(&conn, &operation.operation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                "outcome_unknown",
                "an unknown request with no remaining work is terminal and cannot be resumed"
            );
            assert_eq!(
                operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
                "outcome_unknown"
            );
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                chrono::Utc::now().timestamp(),
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .is_err());
        }

        let retry = persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
        )
        .unwrap();
        assert!(!retry, "an outcome-unknown request is never replayed");
        let conn = db.lock().unwrap();
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .requests_attempted,
            1,
            "the failed replay gate rolls its budget increment back"
        );
    }

    #[test]
    fn finishing_remaining_tagging_work_preserves_prior_unknown_results() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        let asset_id = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT asset_id FROM ai_suggestions WHERE batch_id=?1",
                [operation.batch_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
        )
        .unwrap());
        record_request_outcome(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
            false,
            Some("REQUEST_OUTCOME_UNKNOWN"),
        )
        .unwrap();

        finish(&db, &operation.operation_id, &operation.step_id, true).unwrap();

        let conn = db.lock().unwrap();
        let operation_record = plans_db::operation(&conn, &operation.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation_record.state, "outcome_unknown");
        assert_eq!(operation_record.phase, "complete");
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "outcome_unknown"
        );
        let summaries = operations_db::recovery_summaries(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].unknown_items, 1);
        assert!(!summaries[0].requires_resume);
    }

    #[test]
    fn resumed_mixed_tagging_keeps_unknown_recovery_after_pending_items_finish() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let first_file = source.path().join("first.jpg");
        let second_file = source.path().join("second.jpg");
        write_test_image(&first_file, 91);
        write_test_image(&second_file, 92);

        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let prepared = import_prepare::prepare_import(
            &db,
            &caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![
                    first_file.to_string_lossy().into_owned(),
                    second_file.to_string_lossy().into_owned(),
                ],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: Vec::new(),
            },
        )
        .unwrap();
        let accepted = import_submission::confirm(
            &db,
            &caller,
            &prepared.plan_ref,
            &ResourceRef::generated("mixed_tagging_import"),
        )
        .unwrap();
        import_execute::execute_import(
            &db,
            &thumbs,
            &caller,
            &prepared.plan_ref,
            &accepted.operation.operation_id,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_ids = conn
                .prepare("SELECT id FROM assets ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(asset_ids.len(), 2);
            ai::create_batch(&conn, &asset_ids, "cloud").unwrap().id
        };
        let preparation = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = preparation.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("mixed_tagging_submit"),
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
        }

        let asset_ids = {
            let conn = db.lock().unwrap();
            let mut statement = conn
                .prepare("SELECT asset_id FROM ai_suggestions WHERE batch_id=?1 ORDER BY asset_id")
                .unwrap();
            statement
                .query_map([batch_id], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(asset_ids.len(), 2);
        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_ids[0],
        )
        .unwrap());

        let recoveries = crate::services::operations::reconcile_startup(&db).unwrap();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].unknown_items, 1);
        assert!(recoveries[0].requires_resume);

        let operation_ref = ResourceRef::from_trusted(operation.operation_id.clone()).unwrap();
        let resumed =
            crate::services::operations::resume_operation(&db, &caller, &operation_ref).unwrap();
        assert_eq!(
            resumed.state,
            crate::contracts::agent::OperationState::Queued
        );
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                chrono::Utc::now().timestamp(),
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
        }

        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_ids[0],
        )
        .is_err());
        assert_eq!(
            tagging_db::by_operation(
                &db.lock().unwrap(),
                &operation.operation_id,
                &operation.step_id
            )
            .unwrap()
            .unwrap()
            .requests_attempted,
            1,
            "rejecting a replay does not consume another approved request"
        );
        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_ids[1],
        )
        .unwrap());
        record_request_outcome(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_ids[1],
            true,
            None,
        )
        .unwrap();
        finish(&db, &operation.operation_id, &operation.step_id, true).unwrap();

        let conn = db.lock().unwrap();
        let items =
            operations_db::items(&conn, &operation.operation_id, &operation.step_id).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].state, "outcome_unknown");
        assert_eq!(items[1].state, "succeeded");
        let operation_record = plans_db::operation(&conn, &operation.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation_record.state, "outcome_unknown");
        assert_eq!(operation_record.phase, "complete");
        let summaries = operations_db::recovery_summaries(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].unknown_items, 1);
        assert!(!summaries[0].requires_resume);
    }

    #[test]
    fn startup_surfaces_legacy_batch_attempt_without_item_receipts_as_unknown() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE tagging_batch_links SET requests_attempted=1
                  WHERE operation_id=?1 AND step_id=?2",
                rusqlite::params![operation.operation_id, operation.step_id],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM operation_items WHERE operation_id=?1 AND step_id=?2",
                rusqlite::params![operation.operation_id, operation.step_id],
            )
            .unwrap();
        }

        let summaries = crate::services::operations::reconcile_startup(&db).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].unknown_items, 1);
        assert!(!summaries[0].requires_resume);

        let conn = db.lock().unwrap();
        let link = tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
            .unwrap()
            .unwrap();
        assert_eq!(link.state, "interrupted");
        assert_eq!(link.requests_attempted, 1);
        assert!(
            operations_db::items(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .is_empty()
        );
        let asset_id: i64 = conn
            .query_row(
                "SELECT asset_id FROM ai_suggestions WHERE batch_id=?1 LIMIT 1",
                [operation.batch_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!tagging_db::consume_request_attempt(
            &conn,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
            chrono::Utc::now().timestamp(),
        )
        .unwrap());
        assert_eq!(
            plans_db::operation(&conn, &operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "outcome_unknown"
        );
    }

    #[test]
    fn startup_recovers_a_saved_suggestion_before_marking_the_request_unknown() {
        let (_directories, db, _caller, operation) = started_tagging_fixture();
        let asset_id = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT asset_id FROM ai_suggestions WHERE batch_id=?1",
                [operation.batch_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert!(persist_request_attempt_for_item(
            &db,
            &operation.operation_id,
            &operation.step_id,
            asset_id,
        )
        .unwrap());

        {
            let conn = db.lock().unwrap();
            let suggestion = ai::list_suggestions(&conn, operation.batch_id)
                .unwrap()
                .remove(0);
            let tags =
                ai::CategorizedTags::from([("scene".to_string(), vec!["已保存建议".to_string()])]);
            let proposals = vec![ai::TagProposal {
                facet_key: "scene".to_string(),
                raw_name: "已保存建议".to_string(),
                confidence: Some(0.9),
            }];
            ai::set_suggestion_result_policy(
                &conn,
                suggestion.id,
                &tags,
                &proposals,
                "已经写入数据库的分析",
                &ai::ConfidencePolicy::default(),
            )
            .unwrap();
            ai::set_suggestion_provenance(&conn, suggestion.id, "saved response", "{}", 1).unwrap();
        }

        let summaries = crate::services::operations::reconcile_startup(&db).unwrap();
        assert!(summaries.is_empty(), "verified saved work needs no resume");

        let conn = db.lock().unwrap();
        let operation_state = plans_db::operation(&conn, &operation.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation_state.state, "succeeded");
        assert_eq!(operation_state.phase, "complete");
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "succeeded"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "completed"
        );
        assert_eq!(
            ai::get_batch(&conn, operation.batch_id).unwrap().status,
            "done"
        );
        assert!(operations_db::recovery_summaries(&conn).unwrap().is_empty());
        let item = operations_db::items(&conn, &operation.operation_id, &operation.step_id)
            .unwrap()
            .remove(0);
        assert_eq!(item.state, "succeeded");
        assert_eq!(item.effect_state, "registered");
        let receipts =
            operations_db::receipts(&conn, &operation.operation_id, &operation.step_id).unwrap();
        assert_eq!(
            receipts.last().unwrap().event,
            "tagging_suggestion_saved_reconciled"
        );
        assert!(receipts
            .last()
            .unwrap()
            .db_ref
            .as_deref()
            .is_some_and(|reference| reference.starts_with("suggestion:")));
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .requests_attempted,
            1
        );
    }

    #[test]
    fn cancelling_an_unstarted_batch_finishes_batch_and_operation_together() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("queued-cancel.jpg");
        write_test_image(&file, 27);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let preview = prepare_existing_batch(&db, &caller, batch_id, None)
            .unwrap()
            .preview
            .unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("queued_cancel"),
        )
        .unwrap();

        let cancellation =
            crate::services::operations::cancel_tagging_batch(&db, &caller, batch_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            cancellation.state,
            crate::contracts::agent::OperationState::Cancelled
        );
        assert!(
            !persist_request_attempt(&db, &operation.operation_id, &operation.step_id).unwrap()
        );
        let conn = db.lock().unwrap();
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "cancelled"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        assert_eq!(ai::get_batch(&conn, batch_id).unwrap().status, "cancelled");
        assert_eq!(
            ai::list_suggestions(&conn, batch_id).unwrap()[0].status,
            "pending"
        );
    }

    #[test]
    fn persisted_cancel_wins_over_late_success_or_interruption() {
        for completed in [true, false] {
            let (_directories, db, caller, operation) = started_tagging_fixture();
            let operation_ref = ResourceRef::from_trusted(operation.operation_id.clone()).unwrap();
            let cancellation =
                crate::services::operations::cancel_operation(&db, &caller, &operation_ref)
                    .unwrap();
            assert_eq!(
                cancellation.phase,
                crate::contracts::agent::OperationPhase::Cancelling
            );

            {
                let conn = db.lock().unwrap();
                tagging_db::finish_operation_step(
                    &conn,
                    &operation.operation_id,
                    &operation.step_id,
                    completed,
                    chrono::Utc::now().timestamp(),
                )
                .unwrap();
            }

            let conn = db.lock().unwrap();
            assert_eq!(
                plans_db::operation(&conn, &operation.operation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                "cancelled"
            );
            assert_eq!(
                operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
                "cancelled"
            );
            assert_eq!(
                tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                    .unwrap()
                    .unwrap()
                    .state,
                "cancelled"
            );
            assert_eq!(
                ai::list_suggestions(&conn, operation.batch_id).unwrap()[0].status,
                "pending"
            );
        }
    }

    #[test]
    fn startup_reconciliation_keeps_persisted_tagging_cancellation_terminal() {
        let (_directories, db, caller, operation) = started_tagging_fixture();
        crate::services::operations::cancel_tagging_batch(&db, &caller, operation.batch_id)
            .unwrap()
            .unwrap();

        let recoverable = crate::services::operations::reconcile_startup(&db).unwrap();

        assert!(recoverable.is_empty());
        let conn = db.lock().unwrap();
        assert_eq!(
            plans_db::operation(&conn, &operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        assert_eq!(
            operations_db::steps(&conn, &operation.operation_id).unwrap()[0].state,
            "cancelled"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        assert!(operations_db::recovery_summaries(&conn).unwrap().is_empty());
        assert_eq!(
            ai::get_batch(&conn, operation.batch_id).unwrap().status,
            "cancelled"
        );
        assert_eq!(
            ai::list_suggestions(&conn, operation.batch_id).unwrap()[0].status,
            "pending"
        );
    }

    #[test]
    fn late_ai_results_and_failures_cannot_replace_user_review() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("reviewed.jpg");
        write_test_image(&file, 32);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let conn = db.lock().unwrap();
        let asset_id = conn
            .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
            .unwrap();
        let batch_id = ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id;
        let suggestion = ai::list_suggestions(&conn, batch_id).unwrap().remove(0);
        ai::set_suggestion_error(&conn, suggestion.id, "模型请求失败").unwrap();
        let pending_after_failure = ai::list_suggestions(&conn, batch_id).unwrap().remove(0);
        assert_eq!(pending_after_failure.status, "pending");
        assert_eq!(
            pending_after_failure.last_error.as_deref(),
            Some("模型请求失败")
        );

        conn.execute(
            "UPDATE ai_suggestions SET status='confirmed',suggested_tags=?1,
                    suggested_description='用户保留的描述' WHERE id=?2",
            rusqlite::params![r#"{"scene":["用户确认"]}"#, suggestion.id],
        )
        .unwrap();
        let tags = std::collections::BTreeMap::from([(
            "scene".to_string(),
            vec!["迟到模型结果".to_string()],
        )]);
        let proposals = vec![ai::TagProposal {
            facet_key: "scene".to_string(),
            raw_name: "迟到模型结果".to_string(),
            confidence: Some(0.9),
        }];
        assert_eq!(
            ai::set_suggestion_result_policy(
                &conn,
                suggestion.id,
                &tags,
                &proposals,
                "迟到模型描述",
                &ai::ConfidencePolicy::default(),
            )
            .unwrap(),
            None
        );
        assert!(
            !ai::set_suggestion_error(&conn, suggestion.id, "迟到请求失败").unwrap(),
            "a late failure must not update an already confirmed suggestion"
        );
        let (status, suggested_tags, description): (String, String, String) = conn
            .query_row(
                "SELECT status,suggested_tags,suggested_description FROM ai_suggestions WHERE id=?1",
                [suggestion.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "confirmed");
        assert_eq!(suggested_tags, r#"{"scene":["用户确认"]}"#);
        assert_eq!(description, "用户保留的描述");
        assert!(ai::list_suggestion_items(&conn, suggestion.id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn completed_limited_batch_scope_allows_a_new_approved_scope_for_remaining_items() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let first_file = source.path().join("first.jpg");
        let second_file = source.path().join("second.jpg");
        write_test_image(&first_file, 21);
        write_test_image(&second_file, 22);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &first_file);
        let _ = import_file(&db, &caller, &thumbs, &second_file);
        let (batch_id, suggestions) = {
            let conn = db.lock().unwrap();
            let asset_ids = {
                let mut statement = conn.prepare("SELECT id FROM assets ORDER BY id").unwrap();
                statement
                    .query_map([], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            let batch_id = ai::create_batch(&conn, &asset_ids, "cloud").unwrap().id;
            (batch_id, ai::list_suggestions(&conn, batch_id).unwrap())
        };
        assert_eq!(suggestions.len(), 2);

        let first_scope = prepare_existing_batch(&db, &caller, batch_id, Some(1)).unwrap();
        let preview = first_scope.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let first_operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("first_scope"),
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            ai::set_suggestion_provenance(
                &conn,
                suggestions[0].id,
                "{}",
                "{}",
                ai_cloud::ANALYSIS_SCHEMA_VERSION,
            )
            .unwrap();
            ai::set_batch_status(&conn, batch_id, "pending").unwrap();
            let now = chrono::Utc::now().timestamp();
            assert!(tagging_db::begin(
                &conn,
                &first_operation.operation_id,
                &first_operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
            tagging_db::finish_operation_step(
                &conn,
                &first_operation.operation_id,
                &first_operation.step_id,
                true,
                now + 1,
            )
            .unwrap();
        }

        let remaining_scope = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(remaining_scope.requires_approval);
        let preview = remaining_scope.preview.unwrap();
        let snapshot: TaggingPlanSnapshot = serde_json::from_str(
            plans_db::plan(&db.lock().unwrap(), preview.plan_id.as_str())
                .unwrap()
                .unwrap()
                .details_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.selection.len(), 1);
        assert_eq!(snapshot.selection[0].asset_id, suggestions[1].asset_id);
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let second_operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("second_scope"),
        )
        .unwrap();
        assert_eq!(second_operation.batch_id, batch_id);
        assert_ne!(second_operation.operation_id, first_operation.operation_id);

        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM tagging_batch_links WHERE batch_id=?1",
                [batch_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM operation_child_tasks WHERE task_kind='ai_batch_scope'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            2
        );
    }

    #[test]
    fn duplicate_import_requires_explicit_item_selection_and_persists_receipt_scope() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("duplicate.jpg");
        write_test_image(&file, 9);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _first = import_file(&db, &caller, &thumbs, &file);
        let duplicate_operation = import_file(&db, &caller, &thumbs, &file);
        let (step_id, item_id) = import_step_and_item(&db, &duplicate_operation);

        assert!(prepare_from_import(
            &db,
            &caller,
            &duplicate_operation,
            &ResourceRef::from_trusted(step_id.clone()).unwrap(),
            &[]
        )
        .is_err());
        let prepared = prepare_from_import(
            &db,
            &caller,
            &duplicate_operation,
            &ResourceRef::from_trusted(step_id.clone()).unwrap(),
            std::slice::from_ref(&item_id),
        )
        .unwrap();
        assert!(prepared.requires_approval);
        assert!(prepared.summary.contains("包含 1 项明确纳入的重复映射"));
        let details = {
            let conn = db.lock().unwrap();
            let plan = plans_db::plan(&conn, prepared.preview.as_ref().unwrap().plan_id.as_str())
                .unwrap()
                .unwrap();
            plan.details_json.unwrap()
        };
        assert!(details.contains(&item_id));
        assert!(details.contains("duplicate_mapped"));
        assert!(!details.contains("embedded-user"));
        assert!(!details.contains("embedded-pass"));
        assert!(!details.contains("query-secret"));
        assert!(!prepared.summary.contains("embedded-pass"));
        assert!(!prepared.summary.contains("query-secret"));
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let submitted = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap();
        let replay = prepare_from_import(
            &db,
            &caller,
            &duplicate_operation,
            &ResourceRef::from_trusted(step_id.clone()).unwrap(),
            std::slice::from_ref(&item_id),
        )
        .unwrap();
        assert!(!replay.requires_approval);
        assert_eq!(
            replay.operation.unwrap().operation_id,
            submitted.operation_id
        );
        assert_eq!(
            prepare_from_import(
                &db,
                &caller,
                &duplicate_operation,
                &ResourceRef::from_trusted(step_id.clone()).unwrap(),
                &[],
            )
            .unwrap_err()
            .code(),
            "RESOURCE_CONFLICT",
            "changing the duplicate inclusion set must not reuse an approved operation"
        );
        let conn = db.lock().unwrap();
        let link = tagging_db::by_operation(&conn, &submitted.operation_id, &submitted.step_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            link.source_operation_id.as_deref(),
            Some(duplicate_operation.as_str())
        );
        assert_eq!(link.source_step_id.as_deref(), Some(step_id.as_str()));
        assert_eq!(link.batch_id, submitted.batch_id);
    }

    #[test]
    fn changed_connection_invalidates_approved_plan_before_batch_creation() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("changed.jpg");
        write_test_image(&file, 3);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let imported = import_file(&db, &caller, &thumbs, &file);
        let (step_id, _) = import_step_and_item(&db, &imported);
        let prepared = prepare_from_import(
            &db,
            &caller,
            &imported,
            &ResourceRef::from_trusted(step_id).unwrap(),
            &[],
        )
        .unwrap();
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        {
            let conn = db.lock().unwrap();
            ai_connections::upsert(
                &conn,
                "tagging-test",
                "Tagging test",
                "cloud",
                "openai_chat",
                "https://changed.invalid/v1",
                "vision-test-model",
                None,
            )
            .unwrap();
        }
        assert_eq!(
            submit_plan(
                &db,
                &caller,
                &preview.plan_id,
                &ResourceRef::generated("idempotency"),
            )
            .unwrap_err()
            .code(),
            "PLAN_CHANGED"
        );
        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM ai_batches", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "a changed approved source must not create a batch"
        );
    }

    #[test]
    fn start_revalidates_connection_after_credential_lookup_before_durable_begin() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("start-revision.jpg");
        write_test_image(&file, 18);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let preview = prepare_existing_batch(&db, &caller, batch_id, None)
            .unwrap()
            .preview
            .unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("start_revision"),
        )
        .unwrap();
        let (link, stored) = {
            let conn = db.lock().unwrap();
            let link = tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap();
            let details = plans_db::plan(&conn, &link.plan_id)
                .unwrap()
                .unwrap()
                .details_json
                .unwrap();
            (
                link,
                serde_json::from_str::<TaggingPlanSnapshot>(&details).unwrap(),
            )
        };

        // Models the gap while the start path performs keyring IO: a changed
        // connection must invalidate the approved plan before it becomes
        // started and before a request can consume budget.
        let conn = db.lock().unwrap();
        ai_connections::upsert(
            &conn,
            "tagging-test",
            "Tagging test",
            "cloud",
            "openai_chat",
            "https://changed-before-start.invalid/v1",
            "vision-test-model",
            None,
        )
        .unwrap();
        assert_eq!(
            validate_start_snapshot_in_conn(&conn, &caller, &link, &stored)
                .unwrap_err()
                .code(),
            "PLAN_CHANGED"
        );
        assert_eq!(
            tagging_db::by_operation(&conn, &operation.operation_id, &operation.step_id)
                .unwrap()
                .unwrap()
                .state,
            "awaiting_user_start"
        );
        drop(conn);
        assert!(
            !persist_request_attempt(&db, &operation.operation_id, &operation.step_id).unwrap()
        );
    }

    #[test]
    fn changed_connection_cancels_inactive_approval_and_requires_a_new_plan() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("replan.jpg");
        write_test_image(&file, 33);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let (batch_id, asset_id) = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            (
                ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id,
                asset_id,
            )
        };
        let prepared = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let first_preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(
            &db,
            &caller,
            &first_preview.plan_id,
            &first_preview.review_ref,
        )
        .unwrap();
        let first_operation = submit_plan(
            &db,
            &caller,
            &first_preview.plan_id,
            &ResourceRef::generated("first_idempotency"),
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            ai_connections::upsert(
                &conn,
                "tagging-test",
                "Tagging test",
                "cloud",
                "openai_chat",
                "https://changed.invalid/v1",
                "vision-test-model",
                None,
            )
            .unwrap();
        }

        let refreshed = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(refreshed.requires_approval);
        let second_preview = refreshed.preview.unwrap();
        assert_ne!(second_preview.plan_id, first_preview.plan_id);
        {
            let conn = db.lock().unwrap();
            assert_eq!(
                plans_db::operation(&conn, &first_operation.operation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                "cancelled"
            );
            assert_eq!(
                tagging_db::by_operation(
                    &conn,
                    &first_operation.operation_id,
                    &first_operation.step_id,
                )
                .unwrap()
                .unwrap()
                .state,
                "cancelled"
            );
            assert_eq!(ai::get_batch(&conn, batch_id).unwrap().status, "pending");
            assert_eq!(
                ai::list_suggestions(&conn, batch_id).unwrap()[0].asset_id,
                asset_id
            );
        }
        crate::services::plans::approve_plan(
            &db,
            &caller,
            &second_preview.plan_id,
            &second_preview.review_ref,
        )
        .unwrap();
        let second_operation = submit_plan(
            &db,
            &caller,
            &second_preview.plan_id,
            &ResourceRef::generated("second_idempotency"),
        )
        .unwrap();
        assert_ne!(second_operation.operation_id, first_operation.operation_id);
        assert_eq!(second_operation.batch_id, batch_id);
    }

    #[test]
    fn expired_interrupted_approval_is_cancelled_before_a_fresh_plan_is_prepared() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("expired.jpg");
        write_test_image(&file, 34);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let prepared = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let old_operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("expired_idempotency"),
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        let expired_start = now - MAX_APPROVED_TAGGING_SECONDS as i64 - 5;
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &old_operation.operation_id,
                &old_operation.step_id,
                expired_start,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
            operations::reconcile_startup(&conn, now).unwrap();
            tagging_db::recover_interrupted(&conn, now).unwrap();
        }

        let refreshed = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(refreshed.requires_approval);
        assert!(refreshed.preview.is_some());
        let conn = db.lock().unwrap();
        assert_eq!(
            plans_db::operation(&conn, &old_operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        let old_items =
            operations_db::items(&conn, &old_operation.operation_id, &old_operation.step_id)
                .unwrap();
        assert_eq!(old_items.len(), 1);
        assert_eq!(old_items[0].state, "cancelled");
        assert!(operations_db::receipts(
            &conn,
            &old_operation.operation_id,
            &old_operation.step_id,
        )
        .unwrap()
        .iter()
        .any(|receipt| receipt.event == "tagging_item_cancelled_for_replan"));
        assert_eq!(ai::get_batch(&conn, batch_id).unwrap().status, "pending");
        assert_eq!(
            ai::list_suggestions(&conn, batch_id).unwrap()[0].status,
            "pending"
        );
    }

    #[test]
    fn exhausted_interrupted_request_budget_requires_a_new_approval() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("budget.jpg");
        write_test_image(&file, 35);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &file);
        let batch_id = {
            let conn = db.lock().unwrap();
            let asset_id = conn
                .query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let prepared = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let old_operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("budget_idempotency"),
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        let attempts = {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &old_operation.operation_id,
                &old_operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
            let link = tagging_db::by_operation(
                &conn,
                &old_operation.operation_id,
                &old_operation.step_id,
            )
            .unwrap()
            .unwrap();
            let asset_id = ai::list_suggestions(&conn, batch_id).unwrap()[0].asset_id;
            for attempt in 0..link.request_limit {
                assert!(tagging_db::consume_request_attempt(
                    &conn,
                    &old_operation.operation_id,
                    &old_operation.step_id,
                    asset_id,
                    now + attempt,
                )
                .unwrap());
            }
            link.request_limit
        };
        let interrupted_at = now + attempts + 1;
        {
            let conn = db.lock().unwrap();
            operations::reconcile_startup(&conn, interrupted_at).unwrap();
            tagging_db::recover_interrupted(&conn, interrupted_at).unwrap();
        }

        let refreshed = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        assert!(refreshed.requires_approval);
        let fresh_preview = refreshed.preview.unwrap();
        assert_ne!(fresh_preview.plan_id, preview.plan_id);
        let conn = db.lock().unwrap();
        let old_link =
            tagging_db::by_operation(&conn, &old_operation.operation_id, &old_operation.step_id)
                .unwrap()
                .unwrap();
        assert_eq!(old_link.state, "cancelled");
        assert_eq!(old_link.requests_attempted, attempts);
        assert_eq!(
            plans_db::operation(&conn, &old_operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "outcome_unknown"
        );
    }

    #[test]
    fn narrowing_an_interrupted_scope_creates_a_new_approval_for_only_that_scope() {
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let first = source.path().join("first.jpg");
        let second = source.path().join("second.jpg");
        write_test_image(&first, 36);
        write_test_image(&second, 37);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let _ = import_file(&db, &caller, &thumbs, &first);
        let _ = import_file(&db, &caller, &thumbs, &second);
        let batch_id = {
            let conn = db.lock().unwrap();
            let mut statement = conn.prepare("SELECT id FROM assets ORDER BY id").unwrap();
            let ids = statement
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            ai::create_batch(&conn, &ids, "cloud").unwrap().id
        };
        let prepared = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = prepared.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let old_operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("wide_scope_idempotency"),
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            let now = chrono::Utc::now().timestamp();
            assert!(tagging_db::begin(
                &conn,
                &old_operation.operation_id,
                &old_operation.step_id,
                now,
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
            operations::reconcile_startup(&conn, now + 1).unwrap();
            tagging_db::recover_interrupted(&conn, now + 1).unwrap();
        }

        let narrowed = prepare_existing_batch(&db, &caller, batch_id, Some(1)).unwrap();
        assert!(narrowed.requires_approval);
        let narrowed_preview = narrowed.preview.unwrap();
        assert_eq!(narrowed_preview.budget.max_items, 1);
        let conn = db.lock().unwrap();
        assert_eq!(
            plans_db::operation(&conn, &old_operation.operation_id)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        let new_snapshot: TaggingPlanSnapshot = serde_json::from_str(
            plans_db::plan(&conn, narrowed_preview.plan_id.as_str())
                .unwrap()
                .unwrap()
                .details_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(new_snapshot.selection.len(), 1);
    }

    #[test]
    fn provider_429_consumes_one_request_and_leaves_suggestion_pending() {
        let (server_url, server) = mock_429_server();
        let managed = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("limited.jpg");
        write_test_image(&file, 5);
        let (db, caller) = fixture(managed.path());
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        {
            let conn = db.lock().unwrap();
            ai_connections::upsert(
                &conn,
                "tagging-test",
                "Tagging test",
                "cloud",
                "openai_chat",
                &server_url,
                "vision-test-model",
                None,
            )
            .unwrap();
        }
        let imported = import_file(&db, &caller, &thumbs, &file);
        let asset_id = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT id FROM assets", [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        let batch_id = {
            let conn = db.lock().unwrap();
            ai::create_batch(&conn, &[asset_id], "cloud").unwrap().id
        };
        let preparation = prepare_existing_batch(&db, &caller, batch_id, None).unwrap();
        let preview = preparation.preview.unwrap();
        crate::services::plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)
            .unwrap();
        let operation = submit_plan(
            &db,
            &caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            assert!(tagging_db::begin(
                &conn,
                &operation.operation_id,
                &operation.step_id,
                chrono::Utc::now().timestamp(),
                MAX_APPROVED_TAGGING_SECONDS as i64,
            )
            .unwrap());
        }
        let (cfg, facets, facets_video, top_tags) = {
            let conn = db.lock().unwrap();
            let connection = ai_connections::binding_for(&conn, "tagging")
                .unwrap()
                .unwrap();
            let mut all_settings = settings::get_settings(&conn).unwrap();
            ai_connections::apply_profile(
                &mut all_settings.ai,
                ai_connections::profile_from_connection(&connection),
            );
            (
                all_settings.ai,
                tag_facets::build_prompt_context(&conn, "image").unwrap(),
                tag_facets::build_prompt_context(&conn, "video").unwrap(),
                tags::top_tags_per_facet(&conn, 60).unwrap(),
            )
        };
        let db = std::sync::Arc::new(db);
        let budget_db = std::sync::Arc::clone(&db);
        let outcome_db = std::sync::Arc::clone(&db);
        let budget_operation_id = operation.operation_id.clone();
        let budget_step_id = operation.step_id.clone();
        let outcome_operation_id = operation.operation_id.clone();
        let outcome_step_id = operation.step_id.clone();
        let deadline_at = tagging_db::by_operation(
            &db.lock().unwrap(),
            &operation.operation_id,
            &operation.step_id,
        )
        .unwrap()
        .unwrap()
        .deadline_at
        .unwrap();
        let remaining = std::time::Duration::from_secs(
            deadline_at.saturating_sub(chrono::Utc::now().timestamp()) as u64,
        );
        let budget = crate::services::ai_cloud::RequestBudget::persistent_tagging_until(
            operation.request_limit as u64,
            std::time::Instant::now() + remaining,
            move |asset_id| {
                crate::services::tagging::persist_request_attempt_for_item(
                    &budget_db,
                    &budget_operation_id,
                    &budget_step_id,
                    asset_id,
                )
            },
            move |asset_id, succeeded, error_code| {
                crate::services::tagging::record_request_outcome(
                    &outcome_db,
                    &outcome_operation_id,
                    &outcome_step_id,
                    asset_id,
                    succeeded,
                    error_code.as_deref(),
                )
            },
        );
        let result = ai_cloud::run_cloud_batch_with_plan(
            &db,
            batch_id,
            &cfg,
            &facets,
            &facets_video,
            None,
            &std::sync::Arc::new(AtomicBool::new(false)),
            Some(&budget),
            Some(&[asset_id]),
            Some(&top_tags),
            |_| {},
        );
        let error = result.unwrap_err();
        assert_eq!(error.code(), "AI_RATE_LIMITED");
        let request = String::from_utf8_lossy(&server.join().unwrap()).to_ascii_lowercase();
        assert!(request.starts_with("post /v1/chat/completions "));
        let conn = db.lock().unwrap();
        assert_eq!(
            tagging_db::by_batch(&conn, batch_id)
                .unwrap()
                .unwrap()
                .requests_attempted,
            1
        );
        assert_eq!(
            ai::get_batch(&conn, batch_id).unwrap().status,
            "interrupted"
        );
        let suggestion = ai::list_suggestions(&conn, batch_id).unwrap().remove(0);
        assert_eq!(suggestion.status, "pending");
        assert!(suggestion.suggested_tags.is_empty());
        tagging_db::finish_operation_step(
            &conn,
            &operation.operation_id,
            &operation.step_id,
            false,
            21,
        )
        .unwrap();
        let _ = imported;
    }
}
