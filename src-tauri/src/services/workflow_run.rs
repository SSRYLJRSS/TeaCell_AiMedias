//! A4-08：个人常用流程 —— 每次实例化为**新计划**，再走统一审批
//!
//! 这一层把「常用流程版本」翻译成一次性的执行计划：
//!
//! 1. [`prepare_instance`] 只读地把流程里每一步的**本次输入**解析成冻结快照，写成一份
//!    新的 `plans` 记录（`workflows.submit` + Ordinary 审批）。它不移动、不复制、不删除
//!    任何文件，也不创建 operation。同一条流程连调两次会得到两份互不相干的计划，
//!    因此「上一次的批准」永远不会被下一次复用。
//! 2. [`submit_instance`] 在用户于对话框点击「确认并开始」后，显式
//!    `plans::approve_plan`（GUI 可信确认，与 `export_cmd` 自批准同一条路径），再用
//!    与准备阶段**完全相同的** draft 构造器提交，这样 `plan_hash` 才能一致；任何
//!    快照/版本/目录授权变化都会在这里变成 `PlanChanged` 而不是静默执行旧内容。
//! 3. [`execute_instance`] 按 ordinal 顺序执行。`imports.submit` / `tagging.submit` /
//!    `trash.empty.submit` 这三类步骤**永远不执行**：流程计划的批准不等于这些步骤的
//!    批准，它们只被记成 `skipped` 并在计划里点名要去的界面。
//!
//! 数据库锁纪律：`Database::lock()` 不可重入，所以所有对账都是离散短临界区，
//! 绝不在持锁时调用 `plans::*` / `selections::resolve` / 自开事务的函数。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contracts::agent::{
    AgentErrorCode, BudgetSummary, CapabilityId, Instructions, OperationState, PlanPreview,
    PlanScopeSummary, ProtocolVersion, ResourceRef,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::assets;
use crate::db::collections::StoredCollection;
use crate::db::export;
use crate::db::export_items::{self, NewExportItem};
use crate::db::libraries::{self, LibraryHealth, LibrarySnapshot};
use crate::db::operations::{self as operations_db, NewOperationItem, NewOperationStep};
use crate::db::plans as plans_db;
use crate::error::{AppError, AppResult};
use crate::services::agent_tools;
use crate::services::authorization;
use crate::services::export_local::{self, ExportProgress, ExportRun};
use crate::services::library_move::{self, MovePlanItem};
use crate::services::managed_library;
use crate::services::operations;
use crate::services::plans as plan_service;
use crate::services::selections as selection_service;
use crate::services::trash;
use crate::services::workflows;
use crate::state::Database;
use crate::utils::path::encode_native_path;

/// 本次实例化要填的运行时输入。缺省即 `None`，前端只发送非空值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WorkflowInstanceInputs {
    pub source_ref: Option<String>,
    pub selection_ref: Option<String>,
    pub collection_ref: Option<String>,
    pub destination_ref: Option<String>,
    pub service_ref: Option<String>,
    pub tagging_instructions: Option<String>,
    pub rename_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInstancePreviewView {
    pub plan_id: String,
    pub review_ref: String,
    pub workflow_ref: String,
    pub version: u32,
    pub workflow_name: String,
    pub summary: String,
    pub expires_at_unix_seconds: u32,
    pub steps: Vec<WorkflowInstanceStepPreview>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInstanceStepPreview {
    pub ordinal: u32,
    pub capability: String,
    pub label: String,
    pub requires_separate_action: bool,
    pub depends_on: Option<u32>,
    pub item_count: u32,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunReport {
    pub plan_id: String,
    pub operation_id: String,
    pub operation_state: String,
    pub steps: Vec<WorkflowRunStepResult>,
    pub summary: String,
}

/// 「受理即返回」的回执：用户点下确认后立刻拿到稳定的 operation 句柄，
/// 之后靠 [`instance_report`] 轮询进度，不再让命令层一直等到流程跑完。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunStart {
    pub plan_id: String,
    pub operation_id: String,
    pub operation_state: String,
    /// 同一份计划此前已经提交过，这次是幂等命中，没有新建长任务。
    pub replayed: bool,
    pub can_cancel: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunStepResult {
    pub ordinal: u32,
    pub capability: String,
    pub label: String,
    pub state: String,
    pub message: Option<String>,
    pub done: i64,
    pub failed: i64,
    pub total: i64,
}

/// 冻结在 `plans.details_json` 里的实例快照。它参与 `planHash`，所以准备与提交必须
/// 逐字节一致；任何字段被改动都会让 `plan_hash` 不再匹配。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowRunSnapshot {
    pub schema_version: ProtocolVersion,
    pub workflow_ref: String,
    pub version: u32,
    pub workflow_name: String,
    pub steps: Vec<FrozenStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenStep {
    pub ordinal: u32,
    pub capability: String,
    pub label: String,
    pub depends_on: Option<u32>,
    pub requires_separate_action: bool,
    pub selection_ref: Option<String>,
    pub asset_ids: Vec<i64>,
    pub destination_ref: Option<String>,
    pub collection_ref: Option<String>,
    pub collection_relative_path: Option<String>,
    pub service_ref: Option<String>,
    pub note: Option<String>,
}

/// 提交成功后交给执行阶段的只读上下文。
#[derive(Clone)]
pub struct SubmittedInstance {
    pub plan_id: String,
    pub operation_id: String,
    pub operation_ref: ResourceRef,
    pub snapshot: WorkflowRunSnapshot,
    /// 同一份计划此前已经提交过，这次命中幂等键，没有新建长任务。
    pub replayed: bool,
}

/// 单步执行后的真实结论。它只用于**门控下游**，不写库：库里的真相仍是
/// `operation_items` 与收据。
#[derive(Debug, Clone)]
enum StepVerdict {
    /// 本步骤的全部意图都已被文件与收据证明完成。
    Completed,
    /// 本步骤部分或全部没有完成（失败、结果未知、被取消、依赖未满足）。
    /// 下游步骤必须整步跳过，而不能按子集继续 —— 按子集继续会静默收窄范围。
    Degraded { message: String },
}

/// 依赖未满足时留给收据的事件名。把「没有执行」与「执行了但被取消」分开记，
/// 这样重启后的报告仍能复现当时为什么跳过。
const EVENT_DEPENDENCY_UNMET: &str = "dependency_unmet";
const CODE_DEPENDENCY_UNMET: &str = "DEPENDENCY_UNMET";

struct PreparedSubmission {
    plan_ref: ResourceRef,
    review_ref: ResourceRef,
    current: plan_service::PlanDraft,
    snapshot: WorkflowRunSnapshot,
}

// ---------------------------------------------------------------------------
// 能力分类：本流程可直接执行 vs 必须用户在自己界面单独确认
// ---------------------------------------------------------------------------

/// 只能由用户在自己界面单独确认的步骤。流程计划的批准**绝不**等于这些步骤的批准，
/// 执行阶段它们只会被记成 `skipped`，一个文件都不动。
fn requires_separate_action(capability: CapabilityId) -> bool {
    matches!(
        capability,
        CapabilityId::ImportsSubmit | CapabilityId::TaggingSubmit | CapabilityId::TrashEmptySubmit
    )
}

fn separate_action_place(capability: &str) -> &'static str {
    match capability {
        "imports.submit" => "入库页",
        "tagging.submit" => "打标界面",
        _ => "回收站界面",
    }
}

/// `workflows::catalog_for` 是私有函数，这里按同一份 `STEP_CATALOG` 做等价查表。
fn catalog_entry(capability: CapabilityId) -> Option<&'static workflows::StepCatalogEntry> {
    workflows::STEP_CATALOG
        .iter()
        .find(|entry| entry.capability == capability)
}

fn parse_capability(value: &str) -> Option<CapabilityId> {
    serde_json::from_value::<CapabilityId>(Value::String(value.to_string())).ok()
}

// ---------------------------------------------------------------------------
// 准备：只出计划，不执行、不写文件
// ---------------------------------------------------------------------------

pub fn prepare_instance(
    db: &Database,
    caller: &CallerContext,
    workflow_ref: &str,
    version: i64,
    inputs: &WorkflowInstanceInputs,
) -> AppResult<WorkflowInstancePreviewView> {
    check_capability_entry(CapabilityId::WorkflowsPrepare, caller).map_err(AppError::from)?;
    let version_u32 =
        u32::try_from(version).map_err(|_| AppError::invalid_arg("常用流程版本号无效"))?;
    // 能力语义已变化时这里返回 conflict（可修复：重新编辑流程），原样上抛。
    let contract = workflows::contract_summary(db, workflow_ref, version)?;

    let mut frozen: Vec<FrozenStep> = Vec::with_capacity(contract.steps.len());

    for (index, step) in contract.steps.iter().enumerate() {
        let ordinal = u32::try_from(index + 1).map_err(|_| AppError::internal("流程步骤过多"))?;
        let label = workflows::capability_label(step.capability).to_string();
        let capability = step.capability;
        let warning = catalog_entry(capability).and_then(|entry| entry.warning);

        if requires_separate_action(capability) {
            check_separate_action_inputs(db, inputs, capability, ordinal, &label)?;
            frozen.push(FrozenStep {
                ordinal,
                capability: capability.as_str().to_string(),
                label,
                depends_on: step.depends_on,
                requires_separate_action: true,
                selection_ref: None,
                asset_ids: Vec::new(),
                destination_ref: None,
                collection_ref: None,
                collection_relative_path: None,
                service_ref: inputs
                    .service_ref
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                note: Some(format!(
                    "需要您在{}单独确认，本流程不会执行这一步。",
                    separate_action_place(capability.as_str())
                )),
            });
            continue;
        }

        // 可执行步骤：素材集合、导出目标、分库全部按本次输入重新核验，模板里没有旧值可用。
        let selection = required_selection(inputs, ordinal, &label)?;
        let resolved = selection_service::resolve(db, caller, &selection)?;
        if resolved.asset_ids.is_empty() {
            return Err(AppError::invalid_arg("选中的素材集合是空的，请重新选择"));
        }
        if resolved.truncated {
            return Err(AppError::invalid_arg(
                "这次选择的素材太多，请缩小范围后再试",
            ));
        }

        let mut destination_ref: Option<String> = None;
        let mut collection_ref: Option<String> = None;
        let mut collection_relative_path: Option<String> = None;

        if capability == CapabilityId::ExportsSubmit {
            let value = required_text(
                inputs.destination_ref.as_deref(),
                format!("第 {ordinal} 步「{label}」需要本次的导出目标目录，请先在导出界面选好目录再回来。"),
            )?;
            let destination = reference(&value)?;
            // 授权已撤销会在这里失败，文案本身就是可修复指引，直接上抛。
            authorization::resolve_export_directory(db, caller, &destination)?;
            destination_ref = Some(destination.as_str().to_string());
        } else if capability == CapabilityId::AssetsMoveSubmit {
            let value = required_text(
                inputs.collection_ref.as_deref(),
                format!("第 {ordinal} 步「{label}」需要目标分库，请先选好分库再回来。"),
            )?;
            let collection = reference(&value)?;
            let (library, target) = {
                let conn = db.lock()?;
                let library = libraries::current(&conn)?
                    .ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
                let target = library_move::resolve_target(&conn, &library, &collection)?;
                (library, target)
            };
            if library.health != LibraryHealth::Ready {
                return Err(AppError::from(AgentErrorCode::LibraryOffline));
            }
            collection_ref = Some(collection.as_str().to_string());
            collection_relative_path = Some(target.relative_path);
        } else if capability != CapabilityId::AssetsTrashSubmit
            && capability != CapabilityId::AssetsRestoreSubmit
        {
            return Err(AppError::unsupported(format!(
                "「{}」不能作为常用流程步骤执行",
                capability.as_str()
            )));
        }

        frozen.push(FrozenStep {
            ordinal,
            capability: capability.as_str().to_string(),
            label,
            depends_on: step.depends_on,
            requires_separate_action: false,
            selection_ref: Some(resolved.selection_ref.as_str().to_string()),
            asset_ids: resolved.asset_ids,
            destination_ref,
            collection_ref,
            collection_relative_path,
            service_ref: None,
            note: warning.map(str::to_string),
        });
    }

    if !frozen.iter().any(|step| !step.requires_separate_action) {
        return Err(AppError::invalid_arg(
            "这个流程的步骤都需要在各自界面单独操作，无法作为一次流程执行。请在对应界面逐步完成。",
        ));
    }

    let snapshot = WorkflowRunSnapshot {
        schema_version: ProtocolVersion::V1,
        workflow_ref: workflow_ref.to_string(),
        version: version_u32,
        workflow_name: contract.name.as_str().to_string(),
        steps: frozen,
    };

    // scope_ref 优先用第一个可执行步骤的素材集合；计划没有 collection 字段，
    // 分库信息只活在冻结快照里。
    let first_executable = snapshot
        .steps
        .iter()
        .find(|step| !step.requires_separate_action);
    let scope_ref_value = first_executable
        .and_then(|step| step.selection_ref.clone())
        .unwrap_or_else(|| workflow_ref.to_string());
    let scope = PlanScopeSummary {
        scope_ref: reference(&scope_ref_value)?,
        source_ref: None,
        selection_ref: match first_executable.and_then(|step| step.selection_ref.as_deref()) {
            Some(value) => Some(reference(value)?),
            None => None,
        },
        target_ref: None,
        service_ref: None,
        service_revision: None,
        model: None,
    };

    let details_json = serde_json::to_string(&snapshot)
        .map_err(|_| AppError::internal("流程实例快照无法序列化"))?;
    let summary = snapshot_summary(&snapshot)?;
    let draft = build_draft(&snapshot, scope, Some(details_json), summary)?;

    let preview: PlanPreview = plan_service::prepare_plan(db, caller, draft)?;
    workflows::record_instantiation(db, preview.plan_id.as_str(), workflow_ref, version)?;

    Ok(preview_view(&preview, &snapshot))
}

fn check_separate_action_inputs(
    db: &Database,
    inputs: &WorkflowInstanceInputs,
    capability: CapabilityId,
    ordinal: u32,
    label: &str,
) -> AppResult<()> {
    if capability == CapabilityId::ImportsSubmit {
        // 入库的来源目录必须在入库页由用户亲自选择，流程不能代替；缺项先补齐。
        let _ = required_text(
            inputs.source_ref.as_deref(),
            format!(
                "第 {ordinal} 步「{label}」需要您在入库页单独选择来源目录，流程不能代替选择；请在入库页完成这一步。"
            ),
        )?;
        return Ok(());
    }
    if capability == CapabilityId::TaggingSubmit {
        let service_ref = required_text(
            inputs.service_ref.as_deref(),
            format!("第 {ordinal} 步「{label}」需要 AI 服务，请先在设置里配置并绑定后再试。"),
        )?;
        let exists = {
            let conn = db.lock()?;
            crate::db::ai_connections::get(&conn, service_ref.as_str())?.is_some()
        };
        if !exists {
            return Err(AppError::invalid_arg(format!(
                "第 {ordinal} 步「{label}」使用的 AI 服务已不存在，请到设置里重新绑定。"
            )));
        }
    }
    // 永久清空不需要额外输入，但无论如何都只报告不执行。
    Ok(())
}

fn preview_view(
    preview: &PlanPreview,
    snapshot: &WorkflowRunSnapshot,
) -> WorkflowInstancePreviewView {
    let mut warnings: Vec<String> = Vec::new();
    let steps: Vec<WorkflowInstanceStepPreview> = snapshot
        .steps
        .iter()
        .map(|step| {
            let count = u32::try_from(step.asset_ids.len()).unwrap_or(u32::MAX);
            // 每个步骤的能力级提醒（来自 STEP_CATALOG），与是否会被本流程执行无关。
            if let Some(warning) = parse_capability(&step.capability)
                .and_then(catalog_entry)
                .and_then(|entry| entry.warning)
            {
                push_unique(
                    &mut warnings,
                    format!("第 {} 步「{}」：{warning}", step.ordinal, step.label),
                );
            }
            if step.requires_separate_action {
                push_unique(
                    &mut warnings,
                    format!(
                        "第 {} 步「{}」：流程计划的批准不包含这一步，请稍后在{}单独确认后自行完成。",
                        step.ordinal,
                        step.label,
                        separate_action_place(&step.capability)
                    ),
                );
            }
            WorkflowInstanceStepPreview {
                ordinal: step.ordinal,
                capability: step.capability.clone(),
                label: step.label.clone(),
                requires_separate_action: step.requires_separate_action,
                depends_on: step.depends_on,
                item_count: if step.requires_separate_action {
                    0
                } else {
                    count
                },
                note: step.note.clone(),
            }
        })
        .collect();

    WorkflowInstancePreviewView {
        plan_id: preview.plan_id.as_str().to_string(),
        review_ref: preview.review_ref.as_str().to_string(),
        workflow_ref: snapshot.workflow_ref.clone(),
        version: snapshot.version,
        workflow_name: snapshot.workflow_name.clone(),
        summary: preview.summary.as_str().to_string(),
        expires_at_unix_seconds: preview.expires_at_unix_seconds,
        steps,
        warnings,
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

// ---------------------------------------------------------------------------
// 快照 → 摘要 / draft（准备与提交共用，保证 plan_hash 一致）
// ---------------------------------------------------------------------------

fn snapshot_summary(snapshot: &WorkflowRunSnapshot) -> AppResult<Instructions> {
    let mut text = format!(
        "常用流程「{}」第 {} 版的本次计划，共 {} 步：",
        snapshot.workflow_name,
        snapshot.version,
        snapshot.steps.len()
    );
    for step in &snapshot.steps {
        if step.requires_separate_action {
            text.push_str(&format!(
                "\n第 {} 步「{}」：需要您在{}单独确认（本流程不会执行）",
                step.ordinal,
                step.label,
                separate_action_place(&step.capability)
            ));
        } else {
            text.push_str(&format!(
                "\n第 {} 步「{}」：{} 项素材（会执行）",
                step.ordinal,
                step.label,
                step.asset_ids.len()
            ));
        }
    }
    text.push_str(
        "\n流程只执行上面标记「会执行」的步骤；素材、目录与授权都按本次重新核验，不复用上一次的批准。",
    );
    instructions(text)
}

fn build_draft(
    snapshot: &WorkflowRunSnapshot,
    scope: PlanScopeSummary,
    details_json: Option<String>,
    summary: Instructions,
) -> AppResult<plan_service::PlanDraft> {
    let mut item_count: u32 = 0;
    for step in &snapshot.steps {
        if step.requires_separate_action {
            continue;
        }
        item_count = item_count
            .checked_add(
                u32::try_from(step.asset_ids.len())
                    .map_err(|_| AppError::invalid_arg("这次选择的素材太多，请缩小范围后再试"))?,
            )
            .ok_or_else(|| AppError::invalid_arg("这次选择的素材太多，请缩小范围后再试"))?;
    }
    if item_count == 0 {
        return Err(AppError::invalid_arg(
            "这个流程的步骤都需要在各自界面单独操作，无法作为一次流程执行。请在对应界面逐步完成。",
        ));
    }
    Ok(plan_service::PlanDraft {
        capability: CapabilityId::WorkflowsSubmit,
        scope,
        item_count,
        summary,
        details_json,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 0,
            max_items: item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    })
}

// ---------------------------------------------------------------------------
// 提交：GUI 可信确认 + 与准备阶段一致的 draft
// ---------------------------------------------------------------------------

fn prepare_submission(
    db: &Database,
    caller: &CallerContext,
    plan_id: &str,
) -> AppResult<PreparedSubmission> {
    check_capability_entry(CapabilityId::WorkflowsSubmit, caller).map_err(AppError::from)?;
    let record = {
        let conn = db.lock()?;
        plans_db::plan(&conn, plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?
    };
    if record.capability != CapabilityId::WorkflowsSubmit.as_str() {
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    let details_json = record
        .details_json
        .clone()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let snapshot: WorkflowRunSnapshot = serde_json::from_str(&details_json)
        .map_err(|_| AppError::from(AgentErrorCode::PlanChanged))?;
    let summary: Instructions = serde_json::from_value(Value::String(record.summary.clone()))
        .map_err(|_| AppError::internal("持久计划摘要损坏"))?;

    let optional_ref = |value: &Option<String>| -> AppResult<Option<ResourceRef>> {
        value
            .as_ref()
            .map(|value| ResourceRef::from_trusted(value.clone()).map_err(AppError::from))
            .transpose()
    };
    let scope = PlanScopeSummary {
        scope_ref: reference(&record.scope_ref)?,
        source_ref: optional_ref(&record.source_ref)?,
        selection_ref: optional_ref(&record.selection_ref)?,
        target_ref: optional_ref(&record.target_ref)?,
        service_ref: optional_ref(&record.service_ref)?,
        service_revision: optional_ref(&record.service_revision)?,
        model: match record.model.clone() {
            Some(value) => Some(
                serde_json::from_value(Value::String(value))
                    .map_err(|_| AppError::internal("持久计划模型名损坏"))?,
            ),
            None => None,
        },
    };
    let current = build_draft(&snapshot, scope, Some(details_json), summary)?;

    Ok(PreparedSubmission {
        plan_ref: reference(&record.plan_id)?,
        review_ref: reference(&record.review_ref)?,
        current,
        snapshot,
    })
}

pub fn submit_instance(
    db: &Database,
    caller: &CallerContext,
    plan_id: &str,
) -> AppResult<SubmittedInstance> {
    let prepared = prepare_submission(db, caller, plan_id)?;
    let idempotency_key = idempotency_for(&prepared.plan_ref)?;
    // 同一份计划重试必须先查已有的 operation：计划一旦被消费（`review_status`
    // 变成 `consumed`），再走 `approve_plan` 只会得到 `PlanChanged`，而且新建一条
    // operation 会让上一轮已经落盘的效果变成孤儿。命中幂等键时直接复用原任务。
    if let Some(existing) = operation_for_plan(db, &prepared.plan_ref)? {
        let snapshot = prepared.snapshot.clone();
        return Ok(SubmittedInstance {
            plan_id: prepared.plan_ref.as_str().to_string(),
            operation_id: existing.operation_id.clone(),
            operation_ref: reference(&existing.operation_id)?,
            snapshot,
            replayed: true,
        });
    }
    // 用户在对话框点了「确认并开始」，这就是 GUI 的可信确认；与 export_cmd 的
    // GUI 自批准同一条路径，而不是跳过审批。
    plan_service::approve_plan(db, caller, &prepared.plan_ref, &prepared.review_ref)?;
    submit_prepared(db, caller, prepared, idempotency_key)
}

/// 由计划派生的幂等键：同一份计划永远得到同一个键。
///
/// 用随机键会让「用户发现没反应又点一次」产生第二条 operation，第二条又因为
/// 计划已被消费而失败，用户看到的是一个孤儿任务。派生键让第二次提交命中
/// `submit_plan_with_effect` 的重放分支，拿回同一条 operation。
fn idempotency_for(plan_ref: &ResourceRef) -> AppResult<ResourceRef> {
    let plan_id = plan_ref.as_str();
    // ResourceRef 只允许 `[A-Za-z0-9_-]` 且长度 ≤128，plan_id 本身已满足字符集；
    // 超长时保留尾部（uuid 在后半段，尾部才是唯一的部分）。
    let tail = if plan_id.len() <= 100 {
        plan_id
    } else {
        &plan_id[plan_id.len() - 100..]
    };
    reference(&format!("wfidem_{tail}"))
}

/// 一份计划已经提交出的 operation（若有）。`operations_for_plan` 按 rowid 升序，
/// 取最后一条即最近一次提交。
fn operation_for_plan(
    db: &Database,
    plan_ref: &ResourceRef,
) -> AppResult<Option<plans_db::OperationRecord>> {
    let conn = db.lock()?;
    let operations = plans_db::operations_for_plan(&conn, plan_ref.as_str())?;
    Ok(operations.into_iter().next_back())
}

/// 受理回执：刚提交（或幂等命中）时立刻可以展示的状态。
pub fn start_view(db: &Database, submitted: &SubmittedInstance) -> AppResult<WorkflowRunStart> {
    let operation_state = read_operation_state(db, &submitted.operation_id)?;
    let can_cancel = matches!(operation_state.as_str(), "queued" | "running");
    Ok(WorkflowRunStart {
        plan_id: submitted.plan_id.clone(),
        operation_id: submitted.operation_id.clone(),
        operation_state,
        replayed: submitted.replayed,
        can_cancel,
    })
}

/// 轮询一次流程实例的当前状态。
///
/// 「受理即返回」之后，命令层不再持有整次执行，前端改为反复调用这里：它总是从
/// 数据库读回真实状态，因此取消、崩溃重启、另一个窗口推进过进度都不会让它说谎。
pub fn instance_report(
    db: &Database,
    caller: &CallerContext,
    plan_id: &str,
) -> AppResult<WorkflowRunReport> {
    check_capability_entry(CapabilityId::WorkflowsSubmit, caller)?;
    let plan_ref = reference(plan_id)?;
    let existing = operation_for_plan(db, &plan_ref)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if existing.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let snapshot = frozen_snapshot(db, &plan_ref)?;
    let submitted = SubmittedInstance {
        plan_id: plan_id.to_string(),
        operation_id: existing.operation_id.clone(),
        operation_ref: reference(&existing.operation_id)?,
        snapshot,
        replayed: true,
    };
    let steps = collect_step_results(db, &submitted)?;
    let operation_state = read_operation_state(db, &submitted.operation_id)?;
    let summary = report_summary(&submitted, &steps, &operation_state);
    Ok(WorkflowRunReport {
        plan_id: submitted.plan_id,
        operation_id: submitted.operation_id,
        operation_state,
        steps,
        summary,
    })
}

/// 提交一个**已经由 GUI 批准**的流程计划，不再自批准。
///
/// A3-03 的 Pi 适配器走这条路径：模型准备好计划 → 用户在计划卡片上批准 →
/// 模型补齐 idempotencyKey 提交。审批凭据只能由 `plans::approve_plan`
/// （`PlansApprove`，GUI 独占能力）写入，这里只消费它；`submit_plan_with_effect`
/// 会自己校验 `review_status == approved`，所以未经批准的调用必然得到
/// `ApprovalRequired`，模型无法绕过。
pub fn submit_instance_after_approval(
    db: &Database,
    caller: &CallerContext,
    plan_id: &str,
    idempotency_key: &ResourceRef,
) -> AppResult<SubmittedInstance> {
    let prepared = prepare_submission(db, caller, plan_id)?;
    submit_prepared(db, caller, prepared, idempotency_key.clone())
}

/// 读取一个已批准流程计划里冻结的执行快照，交给执行器。
///
/// 执行器（`operation_dispatch`）不经过 `prepare_submission`，因为它不需要重建
/// 审核摘要；但步骤集合必须逐字来自计划持久行，不能按当前流程定义重新展开——
/// 用户批准的是当时那几步。
pub fn frozen_snapshot(db: &Database, plan_ref: &ResourceRef) -> AppResult<WorkflowRunSnapshot> {
    let conn = db.lock()?;
    let record = plans_db::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.capability != CapabilityId::WorkflowsSubmit.as_str() {
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    let details_json = record
        .details_json
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    serde_json::from_str(details_json).map_err(|_| AppError::from(AgentErrorCode::PlanChanged))
}

/// 准备与提交共用的落库部分：把冻结的流程步骤写成 operation 的 step/item。
fn submit_prepared(
    db: &Database,
    caller: &CallerContext,
    prepared: PreparedSubmission,
    idempotency_key: ResourceRef,
) -> AppResult<SubmittedInstance> {
    let snapshot = prepared.snapshot.clone();
    let planned = snapshot.clone();
    let submission = plan_service::submit_plan_with_effect(
        db,
        caller,
        &prepared.plan_ref,
        &idempotency_key,
        &prepared.current,
        move |transaction, operation| {
            for step in &planned.steps {
                if step.requires_separate_action {
                    // 这类步骤不进 operation：它不会被本流程执行。
                    continue;
                }
                let capability = parse_capability(&step.capability)
                    .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
                let ordinal = i64::from(step.ordinal);
                // ordinal 0 已由 consume_and_create_operation_with 自动插入（workflows.submit），
                // 所以流程步骤从 1 开始；每一步用自己的 step_id（PK 是 operation+step）。
                let step_id = ResourceRef::generated("step").as_str().to_string();
                operations_db::insert_step(
                    transaction,
                    &NewOperationStep {
                        operation_id: operation.operation_id.clone(),
                        step_id: step_id.clone(),
                        ordinal,
                        capability: step.capability.clone(),
                        total: Some(step.asset_ids.len() as i64),
                        created_at: operation.created_at,
                    },
                )?;
                let target_ref = match capability {
                    CapabilityId::ExportsSubmit => step.destination_ref.clone(),
                    CapabilityId::AssetsMoveSubmit => step.collection_relative_path.clone(),
                    _ => None,
                };
                for (item_ordinal, asset_id) in step.asset_ids.iter().enumerate() {
                    operations_db::insert_item(
                        transaction,
                        &NewOperationItem {
                            operation_id: operation.operation_id.clone(),
                            step_id: step_id.clone(),
                            item_id: item_id_for(capability, *asset_id)?,
                            ordinal: item_ordinal as i64,
                            input_fingerprint: None,
                            target_ref: target_ref.clone(),
                            created_at: operation.created_at,
                        },
                    )?;
                }
            }
            Ok(())
        },
    )?;

    let operation_ref = submission.operation.operation_id.clone();
    Ok(SubmittedInstance {
        plan_id: prepared.plan_ref.as_str().to_string(),
        operation_id: operation_ref.as_str().to_string(),
        operation_ref,
        snapshot,
        // 幂等键命中时数据库没有新建 operation，把它如实告诉调用方：前端据此
        // 决定「这是新任务，去启动」还是「这是原任务，只轮询就好」。
        replayed: submission.replayed,
    })
}

fn item_id_for(capability: CapabilityId, asset_id: i64) -> AppResult<String> {
    match capability {
        CapabilityId::ExportsSubmit => Ok(agent_tools::export_item_id(asset_id)),
        CapabilityId::AssetsMoveSubmit => Ok(library_move::move_item_id(asset_id)),
        CapabilityId::AssetsRestoreSubmit => Ok(trash::restore_item_id(asset_id)),
        CapabilityId::AssetsTrashSubmit => Ok(trash::deletion_item_id(asset_id)),
        _ => Err(AppError::from(AgentErrorCode::UnsupportedCapability)),
    }
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

pub fn execute_instance(
    db: Arc<Database>,
    caller: CallerContext,
    submitted: SubmittedInstance,
) -> AppResult<WorkflowRunReport> {
    let cancelled = match run_steps(&db, &caller, &submitted) {
        Ok(cancelled) => cancelled,
        Err(error) => {
            // 任何中途错误都必须先把 operation 收成 Failed，否则它永远停在 running，
            // 恢复链会挂死。
            if let Err(finish_error) = operations::finish_operation(
                &db,
                &caller,
                &submitted.operation_ref,
                OperationState::Failed,
            ) {
                tracing::warn!(
                    operation = "workflow_run",
                    operation_id = %submitted.operation_id,
                    stage = "finish_failed",
                    error = %finish_error,
                    "流程中途失败后收尾失败"
                );
            }
            return Err(error);
        }
    };

    if cancelled {
        operations::finish_cancelled(&db, &caller, &submitted.operation_ref)?;
    } else {
        let preliminary = collect_step_results(&db, &submitted)?;
        operations::finish_operation(
            &db,
            &caller,
            &submitted.operation_ref,
            overall_state(&preliminary),
        )?;
    }

    // 终态一律从数据库读回，保证「撤销/取消」之后的读数是真实的。
    let steps = collect_step_results(&db, &submitted)?;
    let operation_state = read_operation_state(&db, &submitted.operation_id)?;
    let summary = report_summary(&submitted, &steps, &operation_state);
    Ok(WorkflowRunReport {
        plan_id: submitted.plan_id.clone(),
        operation_id: submitted.operation_id.clone(),
        operation_state,
        steps,
        summary,
    })
}

/// 按 ordinal 顺序执行，并用**每一步的真实结算结果**门控依赖它的下游步骤。
///
/// 旧实现在这里既不读 `depends_on`、也不看步骤结果：某个导出步骤逐项失败仍返回
/// `Ok(())`，下游「移动这些素材」照样开跑，等于把未完成的意图当成了已完成。
/// 返回是否被取消。每步的真实结论只在本函数内部用于门控下游，不向上暴露：
/// 上报用的结论一律由 `collect_step_results` 从数据库反推。
fn run_steps(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
) -> AppResult<bool> {
    let step_rows = {
        let conn = db.lock()?;
        operations_db::steps(&conn, &submitted.operation_id)?
    };
    let mut verdicts: BTreeMap<u32, StepVerdict> = BTreeMap::new();
    for step in &submitted.snapshot.steps {
        if step.requires_separate_action {
            // 这类步骤永远不由流程执行，它的「结果」由用户在各自界面产生，
            // 流程既不能观测也不该臆断（见 `dependency_verdict` 的处理）。
            continue;
        }
        if cancel_requested(db, &submitted.operation_id)? {
            return Ok(true);
        }
        let row = step_rows
            .iter()
            .find(|row| row.ordinal == i64::from(step.ordinal))
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
        let step_ref = reference(&row.step_id)?;

        // 依赖门：前置步骤没有全部完成时，整步跳过而不是「按子集继续」。
        // 按子集继续会让用户在计划里批准的范围被静默收窄，这比跳过更危险。
        if let Some(blocked) = dependency_block(submitted, step, &verdicts)? {
            skip_step_for_dependency(db, caller, submitted, &row.step_id, &blocked)?;
            verdicts.insert(step.ordinal, StepVerdict::Degraded { message: blocked });
            continue;
        }

        let capability = parse_capability(&step.capability)
            .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
        if capability == CapabilityId::ExportsSubmit {
            run_export_step(db, caller, submitted, step, &step_ref)?;
        } else if capability == CapabilityId::AssetsMoveSubmit {
            run_move_step(db, caller, submitted, step, &step_ref)?;
        } else if capability == CapabilityId::AssetsTrashSubmit {
            run_trash_step(db, caller, submitted, step, &step_ref)?;
        } else if capability == CapabilityId::AssetsRestoreSubmit {
            run_restore_step(db, caller, submitted, step, &step_ref)?;
        } else {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        // 结算数据一律从现在数据库里的 item 状态反推，而不是相信任何 `Ok(())`：
        // 「函数没报错」与「文件效果已经达成」是两件事。
        verdicts.insert(step.ordinal, step_verdict(db, submitted, &row.step_id)?);
    }
    cancel_requested(db, &submitted.operation_id)
}

/// 该步骤是否被前置步骤阻断；返回阻断原因（写给收据，也写进报告）。
fn dependency_block(
    submitted: &SubmittedInstance,
    step: &FrozenStep,
    verdicts: &BTreeMap<u32, StepVerdict>,
) -> AppResult<Option<String>> {
    let Some(depends_on) = step.depends_on else {
        // 没有依赖的步骤是独立的，照常执行。
        return Ok(None);
    };
    let target_ordinal = depends_on.saturating_add(1);
    let Some(target) = submitted
        .snapshot
        .steps
        .iter()
        .find(|candidate| candidate.ordinal == target_ordinal)
    else {
        // 依赖指向一个不存在的步骤：说明快照与其自身不自洽，宁可报错也不猜。
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    };
    if let Some(StepVerdict::Degraded { message }) = verdicts.get(&target_ordinal) {
        return Ok(Some(format!(
            "第 {target_ordinal} 步「{}」没有全部完成（{message}），本步骤没有执行。",
            target.label
        )));
    }
    if target.requires_separate_action {
        // 依赖的是一个「必须在别的界面单独确认」的步骤。流程从不执行它，也就
        // 永远看不到它的结果；把它当成未完成会让「先导入再导出」这类流程的
        // 后半段永远跑不起来，而这不是本卡要解决的缺陷。
        return Ok(None);
    }
    match verdicts.get(&target_ordinal) {
        Some(StepVerdict::Completed) => Ok(None),
        // 目标步骤本应执行却没有结论：同样按未完成处理，不默认放行。
        _ => Ok(Some(format!(
            "第 {target_ordinal} 步「{}」没有执行结论，本步骤没有执行。",
            target.label
        ))),
    }
}

/// 从数据库里的 item 终态反推一步的真实结论。
fn step_verdict(
    db: &Arc<Database>,
    submitted: &SubmittedInstance,
    step_id: &str,
) -> AppResult<StepVerdict> {
    let items = {
        let conn = db.lock()?;
        operations_db::items(&conn, &submitted.operation_id, step_id)?
    };
    let unknown = items
        .iter()
        .filter(|item| item.state == "outcome_unknown")
        .count();
    let failed = items.iter().filter(|item| item.state == "failed").count();
    let cancelled = items
        .iter()
        .filter(|item| item.state == "cancelled")
        .count();
    let open = items
        .iter()
        .filter(|item| item.state == "pending" || item.state == "running")
        .count();
    if unknown > 0 {
        return Ok(StepVerdict::Degraded {
            message: format!("{unknown} 项结果未知"),
        });
    }
    if failed > 0 {
        return Ok(StepVerdict::Degraded {
            message: format!("{failed} 项未成功"),
        });
    }
    if cancelled > 0 {
        return Ok(StepVerdict::Degraded {
            message: format!("{cancelled} 项被取消"),
        });
    }
    if open > 0 {
        return Ok(StepVerdict::Degraded {
            message: format!("{open} 项没有结算"),
        });
    }
    Ok(StepVerdict::Completed)
}

/// 把因依赖未满足而整步跳过的 open items 结清。
///
/// `operation_items.state` 的 CHECK 里没有 `skipped`，而把它们留在 `pending` 会让
/// `refresh_progress` 把它们算进「应做」却不计入完成。这里按「从未开始」记
/// `cancelled`/`none`，并带上专属错误码，收尾与重启后的报告才能把「依赖未满足
/// 而跳过」和「执行到一半被取消」区分开。
fn skip_step_for_dependency(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
    step_id: &str,
    message: &str,
) -> AppResult<()> {
    let items = {
        let conn = db.lock()?;
        operations_db::items(&conn, &submitted.operation_id, step_id)?
    };
    for item in items {
        if item.state != "pending" && item.state != "running" {
            continue;
        }
        let item_ref = reference(&item.item_id)?;
        operations::record_item_state(
            db,
            caller,
            &submitted.operation_ref,
            &reference(step_id)?,
            &item_ref,
            "cancelled",
            "none",
            Some(CODE_DEPENDENCY_UNMET),
            &ResourceRef::generated("receipt"),
            EVENT_DEPENDENCY_UNMET,
            None,
        )?;
    }
    tracing::info!(
        operation = "workflow_run",
        operation_id = %submitted.operation_id,
        step_id,
        message,
        "依赖未满足，整步跳过"
    );
    Ok(())
}

fn cancel_requested(db: &Database, operation_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    let record = plans_db::operation(&conn, operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    Ok(record.phase == "cancelling" || record.state == "cancelled")
}

fn run_export_step(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
    step: &FrozenStep,
    step_ref: &ResourceRef,
) -> AppResult<()> {
    let destination_ref = step
        .destination_ref
        .as_deref()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let destination_ref = reference(destination_ref)?;
    // 快照里只留目录句柄，路径按当前授权重新解析，绝不信任冻结的裸路径。
    let path = authorization::resolve_export_directory(db, caller, &destination_ref)?;
    let dest_dir = encode_native_path(&path)?;
    let asset_ids = &step.asset_ids;
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

    let operation_ref = &submitted.operation_ref;
    // 与 `operation_dispatch::run_export` 同一范式：数据库侧的 `cancelling` 只能靠
    // 逐项回调轮询转达给 `export_local`，所以本地 flag 必须在每次 `on_item` 里刷新，
    // 否则它就是一段永远不生效的死代码。
    let cancel = Arc::new(AtomicBool::new(false));
    let mut on_item = |row: &NewExportItem| -> AppResult<()> {
        if !cancel.load(Ordering::Relaxed) && cancel_requested(db, &submitted.operation_id)? {
            cancel.store(true, Ordering::Relaxed);
        }
        let Some(asset_id) = row.asset_id else {
            return Ok(());
        };
        let (state, effect_state, error_code, event) = if row.state == export_items::STATE_COPIED {
            ("succeeded", "published", None, "copied")
        } else if row.state == export_items::STATE_FAILED {
            ("failed", "none", row.error_code.as_deref(), "copy_failed")
        } else {
            // 跳过项（中途取消）留给收尾统一结算。
            return Ok(());
        };
        let item_ref = reference(&agent_tools::export_item_id(asset_id))?;
        operations::record_item_state(
            db,
            caller,
            operation_ref,
            step_ref,
            &item_ref,
            state,
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
        layout: "flat",
        cancel: &cancel,
    };
    export_local::export_local(db, run, |_progress: ExportProgress| {}, &mut on_item)?;
    Ok(())
}

fn run_move_step(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
    step: &FrozenStep,
    step_ref: &ResourceRef,
) -> AppResult<()> {
    let relative_path = step
        .collection_relative_path
        .clone()
        .ok_or_else(|| AppError::from(AgentErrorCode::PlanChanged))?;
    let library: LibrarySnapshot = {
        let conn = db.lock()?;
        libraries::current(&conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?
    };
    if library.health != LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    let root = managed_library::verified_root_path(&library)?;
    let target_dir = if relative_path.is_empty() {
        root
    } else {
        root.join(&relative_path)
    };
    managed_library::validate_import_target(&library, &target_dir)?;

    let target = StoredCollection {
        collection_ref: step.collection_ref.clone().unwrap_or_default(),
        library_id: library.library_id.clone(),
        library_epoch: library.library_epoch.clone(),
        relative_path: relative_path.clone(),
    };
    // 复用 plan_items 的筛选与校验语义（会跳过已在目标分库的素材、核验源文件），
    // 但目标文件名不重新起名：占用即失败，绝不悄悄改成 (1)(2)。
    let planned = library_move::plan_items(db, &library, &step.asset_ids, &target)?;
    let mut moved: Vec<i64> = Vec::with_capacity(planned.len());
    let mut items: Vec<MovePlanItem> = Vec::with_capacity(planned.len());
    for item in planned {
        let file_name = Path::new(&item.source_path)
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string)
            .ok_or_else(|| AppError::invalid_arg("文件名不合法"))?;
        items.push(MovePlanItem {
            asset_id: item.asset_id,
            source_path: item.source_path,
            target_path: encode_native_path(&target_dir.join(&file_name))?,
            // 沿用 plan_items 刚在计划时冻结的源身份，执行阶段才有一致的复验基线。
            source_identity: item.source_identity,
        });
        moved.push(item.asset_id);
    }

    // 已在目标分库的素材：意图本来就已满足，按实际结果记成功，不留下永远不会终态的待办项。
    for asset_id in &step.asset_ids {
        if moved.contains(asset_id) {
            continue;
        }
        let item_ref = reference(&library_move::move_item_id(*asset_id))?;
        operations::record_item_state(
            db,
            caller,
            &submitted.operation_ref,
            step_ref,
            &item_ref,
            "succeeded",
            "registered",
            None,
            &ResourceRef::generated("receipt"),
            "move_already_in_place",
            None,
        )?;
    }

    if items.is_empty() {
        return Ok(());
    }
    let cancel = AtomicBool::new(false);
    library_move::execute(
        db,
        caller,
        &library,
        library_move::MoveExecution {
            operation_id: &submitted.operation_id,
            operation_ref: &submitted.operation_ref,
            step_ref,
            items: &items,
            cancelled: &cancel,
        },
    )?;
    Ok(())
}

fn run_trash_step(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
    step: &FrozenStep,
    step_ref: &ResourceRef,
) -> AppResult<()> {
    let deleted = {
        let conn = db.lock()?;
        assets::soft_delete(&conn, &step.asset_ids)?
    };
    if deleted != step.asset_ids.len() as u64 {
        // 数量不符说明计划里的素材已经被别的入口处理过：交给 plan_hash 之外的一致性
        // 检查报错，而不是记一半成功。
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    for asset_id in &step.asset_ids {
        let item_ref = reference(&trash::deletion_item_id(*asset_id))?;
        operations::record_item_state(
            db,
            caller,
            &submitted.operation_ref,
            step_ref,
            &item_ref,
            "succeeded",
            "registered",
            None,
            &ResourceRef::generated("receipt"),
            "trashed",
            None,
        )?;
    }
    Ok(())
}

fn run_restore_step(
    db: &Arc<Database>,
    caller: &CallerContext,
    submitted: &SubmittedInstance,
    step: &FrozenStep,
    step_ref: &ResourceRef,
) -> AppResult<()> {
    let library = {
        let conn = db.lock()?;
        libraries::current(&conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?
    };
    if library.health != LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    let cancel = AtomicBool::new(false);
    // trash::execute 内部已经逐项写过成功/失败收据，这里不能再写第二遍终态。
    trash::execute(
        db,
        caller,
        &library,
        trash::RestoreExecution {
            operation_id: &submitted.operation_id,
            operation_ref: &submitted.operation_ref,
            step_ref,
            asset_ids: &step.asset_ids,
            cancelled: &cancel,
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 收尾与报告
// ---------------------------------------------------------------------------

fn collect_step_results(
    db: &Database,
    submitted: &SubmittedInstance,
) -> AppResult<Vec<WorkflowRunStepResult>> {
    let step_rows = {
        let conn = db.lock()?;
        operations_db::steps(&conn, &submitted.operation_id)?
    };
    let mut results: Vec<WorkflowRunStepResult> =
        Vec::with_capacity(submitted.snapshot.steps.len());
    for step in &submitted.snapshot.steps {
        if step.requires_separate_action {
            results.push(WorkflowRunStepResult {
                ordinal: step.ordinal,
                capability: step.capability.clone(),
                label: step.label.clone(),
                state: "skipped".to_string(),
                message: Some(format!(
                    "需要您在{}单独确认，本流程不会执行这一步。",
                    separate_action_place(&step.capability)
                )),
                done: 0,
                failed: 0,
                total: 0,
            });
            continue;
        }
        let row = step_rows
            .iter()
            .find(|row| row.ordinal == i64::from(step.ordinal));
        let items = match row {
            Some(row) => {
                let conn = db.lock()?;
                operations_db::items(&conn, &submitted.operation_id, &row.step_id)?
            }
            None => Vec::new(),
        };
        let done = items
            .iter()
            .filter(|item| item.state == "succeeded")
            .count() as i64;
        let failed = items.iter().filter(|item| item.state == "failed").count() as i64;
        let unknown = items
            .iter()
            .filter(|item| item.state == "outcome_unknown")
            .count() as i64;
        let cancelled = items
            .iter()
            .filter(|item| item.state == "cancelled")
            .count() as i64;
        // 依赖未满足而整步跳过的步骤，其 item 也被结成了 `cancelled`/`none`；
        // 靠专属错误码把它们和「执行到一半被取消」分开，否则用户会看到「已取消」
        // 却不知道是因为前置步骤没完成。
        let dependency_unmet = !items.is_empty()
            && items.iter().all(|item| {
                item.state == "cancelled"
                    && item.error_code.as_deref() == Some(CODE_DEPENDENCY_UNMET)
            });
        let total = match row {
            Some(row) => row.total.unwrap_or(step.asset_ids.len() as i64),
            None => step.asset_ids.len() as i64,
        };
        let state = if unknown > 0 {
            "outcome_unknown"
        } else if failed > 0 {
            "failed"
        } else if !items.is_empty() && done == items.len() as i64 {
            "succeeded"
        } else if dependency_unmet {
            "skipped"
        } else if cancelled > 0 {
            "cancelled"
        } else {
            "skipped"
        };
        let message = if dependency_unmet {
            Some(format!(
                "前置步骤没有全部完成，本步骤（{} 项）没有执行。请在处理完第 {} 步后重新实例化这个流程。",
                total,
                step.depends_on.map(|value| value + 1).unwrap_or(0)
            ))
        } else {
            None
        };
        results.push(WorkflowRunStepResult {
            ordinal: step.ordinal,
            capability: step.capability.clone(),
            label: step.label.clone(),
            state: state.to_string(),
            message,
            done,
            failed: failed + unknown,
            total,
        });
    }
    Ok(results)
}

fn overall_state(results: &[WorkflowRunStepResult]) -> OperationState {
    let executable: Vec<&WorkflowRunStepResult> = results
        .iter()
        .filter(|result| result.state != "skipped")
        .collect();
    if executable
        .iter()
        .any(|result| result.state == "outcome_unknown")
    {
        return OperationState::OutcomeUnknown;
    }
    if !executable.is_empty() && executable.iter().all(|result| result.state == "succeeded") {
        return OperationState::Succeeded;
    }
    if executable.iter().any(|result| result.state == "succeeded") {
        return OperationState::PartiallySucceeded;
    }
    OperationState::Failed
}

fn read_operation_state(db: &Database, operation_id: &str) -> AppResult<String> {
    let record = {
        let conn = db.lock()?;
        plans_db::operation(&conn, operation_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?
    };
    // 前端只认这七个值；interrupted 在读回时按 outcome_unknown 汇报。
    let state = match record.state.as_str() {
        "queued" => "queued",
        "running" => "running",
        "succeeded" => "succeeded",
        "partially_succeeded" => "partially_succeeded",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => "outcome_unknown",
    };
    Ok(state.to_string())
}

fn report_summary(
    submitted: &SubmittedInstance,
    results: &[WorkflowRunStepResult],
    operation_state: &str,
) -> String {
    let mut text = format!(
        "常用流程「{}」第 {} 版：{}。",
        submitted.snapshot.workflow_name,
        submitted.snapshot.version,
        operation_state_text(operation_state)
    );
    for step in results {
        text.push_str(&format!(
            "\n第 {} 步「{}」：{}",
            step.ordinal,
            step.label,
            step_state_text(&step.state)
        ));
        if step.done + step.failed > 0 {
            text.push_str(&format!(
                "（成功 {} 项，未完成 {} 项）",
                step.done, step.failed
            ));
        }
    }
    text
}

fn operation_state_text(state: &str) -> &'static str {
    match state {
        "succeeded" => "已全部完成",
        "partially_succeeded" => "部分完成",
        "failed" => "未成功",
        "cancelled" => "已取消，后续步骤没有执行",
        "outcome_unknown" => "有步骤结果未知，请到操作记录里核对",
        _ => "仍在执行",
    }
}

fn step_state_text(state: &str) -> &'static str {
    match state {
        "succeeded" => "已成功",
        "failed" => "未成功",
        "cancelled" => "已取消",
        "outcome_unknown" => "结果未知",
        _ => "已跳过",
    }
}

// ---------------------------------------------------------------------------
// 取消：只停止后续步骤，已完成步骤保持原样，不做隐式回滚
// ---------------------------------------------------------------------------

pub fn cancel_instance(db: &Database, caller: &CallerContext, operation_id: &str) -> AppResult<()> {
    let operation_ref = reference(operation_id)?;
    operations::cancel_operation(db, caller, &operation_ref)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn reference(value: &str) -> AppResult<ResourceRef> {
    ResourceRef::from_trusted(value.to_string()).map_err(AppError::from)
}

fn instructions(value: String) -> AppResult<Instructions> {
    serde_json::from_value(Value::String(value))
        .map_err(|_| AppError::invalid_arg("流程摘要超出长度上限"))
}

fn required_text(value: Option<&str>, message: String) -> AppResult<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::invalid_arg(message))
}

fn required_selection(
    inputs: &WorkflowInstanceInputs,
    ordinal: u32,
    label: &str,
) -> AppResult<ResourceRef> {
    let value = required_text(
        inputs.selection_ref.as_deref(),
        format!("第 {ordinal} 步「{label}」需要本次选中的素材集合，请先在素材库选好再回来。"),
    )?;
    reference(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collections;
    use crate::db::libraries;
    use crate::db::{init_memory, plans as plans_db};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use crate::services::selections;
    use crate::services::workflows::{
        WorkflowDraftInput, WorkflowParameterInput, WorkflowStepInput,
    };
    use crate::utils::file_identity::file_identity;
    use rusqlite::params;
    use std::fs::File;
    use std::time::UNIX_EPOCH;
    use tempfile::TempDir;

    fn seeded_library(names: &[&str]) -> (TempDir, Arc<Database>, Vec<i64>) {
        let root = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        let library = managed_library::adopt_initial_root(&conn, root.path()).unwrap();
        let mut ids = Vec::new();
        for name in names {
            let path = root.path().join(name);
            std::fs::write(&path, format!("fixture {name}")).unwrap();
            let modified_at = std::fs::metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let id = assets::insert(
                &conn,
                path.to_string_lossy().as_ref(),
                name,
                "jpg",
                std::fs::metadata(&path).unwrap().len() as i64,
                "image/jpeg",
                modified_at,
            )
            .unwrap();
            let candidate = libraries::pending_page(&conn, 0, 100)
                .unwrap()
                .into_iter()
                .find(|item| item.asset_id == id)
                .unwrap();
            let identity = {
                let file = File::open(&path).unwrap();
                file_identity(&file).unwrap()
            };
            assert!(libraries::record_verified(
                &conn,
                &library,
                &candidate,
                name,
                &identity,
                &"a".repeat(64),
                "fixture-receipt",
            )
            .unwrap());
            ids.push(id);
        }
        (root, Arc::new(Database::new(conn)), ids)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: format!("session_{}", uuid::Uuid::new_v4().simple()),
                instance_id: format!("instance_{}", uuid::Uuid::new_v4().simple()),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn export_directory_ref(db: &Database, caller: &CallerContext, dir: &Path) -> ResourceRef {
        authorization::grant_selected_directory(
            db,
            caller,
            dir,
            authorization::DirectoryPurpose::ExportDestination,
            false,
        )
        .unwrap()
        .directory_ref
        .expect("一次授权必须返回可用的目录句柄")
    }

    fn store_selection(db: &Database, caller: &CallerContext, ids: &[i64]) -> String {
        selections::store(db, caller, "本次选中的素材", ids, ids.len() as i64)
            .unwrap()
            .selection_ref
            .as_str()
            .to_string()
    }

    fn workflow(
        db: &Database,
        caller: &CallerContext,
        name: &str,
        steps: &[&str],
        parameters: &[&str],
    ) -> String {
        let view = workflows::create_workflow(
            db,
            caller,
            WorkflowDraftInput {
                name: name.to_string(),
                description: "测试用常用流程".to_string(),
                steps: steps
                    .iter()
                    .map(|capability| WorkflowStepInput {
                        capability: (*capability).to_string(),
                        depends_on: None,
                    })
                    .collect(),
                parameters: parameters
                    .iter()
                    .map(|key| WorkflowParameterInput {
                        key: (*key).to_string(),
                        required: true,
                        description: String::new(),
                    })
                    .collect(),
            },
        )
        .unwrap();
        view.workflow_ref
    }

    fn export_inputs(selection: &str, destination: &ResourceRef) -> WorkflowInstanceInputs {
        WorkflowInstanceInputs {
            selection_ref: Some(selection.to_string()),
            destination_ref: Some(destination.as_str().to_string()),
            ..Default::default()
        }
    }

    fn operation_count(db: &Database) -> i64 {
        let conn = db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .unwrap()
    }

    fn deletion_journal_count(db: &Database) -> i64 {
        let conn = db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM deletion_journal", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn two_calls_of_the_same_workflow_produce_independent_plans() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let inputs = export_inputs(&selection, &destination);

        let first = prepare_instance(&db, &caller, &workflow_ref, 1, &inputs).unwrap();
        let second = prepare_instance(&db, &caller, &workflow_ref, 1, &inputs).unwrap();

        assert_ne!(
            first.plan_id, second.plan_id,
            "同一条流程每次实例化都必须是新计划"
        );
        let conn = db.lock().unwrap();
        assert!(plans_db::plan(&conn, &first.plan_id).unwrap().is_some());
        assert!(plans_db::plan(&conn, &second.plan_id).unwrap().is_some());
    }

    #[test]
    fn prepare_only_plans_and_never_touches_files() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);

        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();

        assert_eq!(preview.steps.len(), 1);
        assert_eq!(preview.steps[0].item_count, 2);
        assert!(
            std::fs::read_dir(&out).unwrap().next().is_none(),
            "准备阶段不能往目标目录写任何文件"
        );
        assert_eq!(
            {
                let conn = db.lock().unwrap();
                assets::count_active(&conn, &ids).unwrap()
            },
            2,
            "准备阶段素材必须仍处于活动状态"
        );
        assert_eq!(operation_count(&db), 0, "准备阶段不能创建 operation");
    }

    #[test]
    fn submit_requires_the_gui_plan_approval() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();

        let prepared = prepare_submission(&db, &caller, &preview.plan_id).unwrap();
        let error = plan_service::submit_plan_with_effect(
            &db,
            &caller,
            &prepared.plan_ref,
            &ResourceRef::generated("idempotency"),
            &prepared.current,
            |_transaction, _operation| Ok(()),
        )
        .expect_err("没有 GUI 确认就提交必须被拒绝");
        assert_eq!(error.code(), AgentErrorCode::ApprovalRequired.as_str());
    }

    #[test]
    fn separate_action_steps_are_never_executed() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出后清空",
            &["exports.submit", "trash.empty.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();
        assert!(preview.steps[1].requires_separate_action);

        let submitted = submit_instance(&db, &caller, &preview.plan_id).unwrap();
        let report = execute_instance(db.clone(), caller.clone(), submitted).unwrap();

        let empty_step = &report.steps[1];
        assert_eq!(empty_step.state, "skipped");
        assert!(empty_step.message.as_deref().unwrap().contains("单独确认"));
        assert_eq!(
            {
                let conn = db.lock().unwrap();
                assets::count_trashed(&conn, &ids).unwrap()
            },
            0,
            "永久清空步骤绝不能被本流程执行"
        );
        for name in ["a.jpg", "b.jpg"] {
            assert!(root.path().join(name).exists());
        }
        assert_eq!(deletion_journal_count(&db), 0, "流程不能写任何删除意图");
        assert_eq!(report.operation_state, "succeeded");
    }

    #[test]
    fn revoked_directory_grant_blocks_reuse() {
        let (root, db, ids) = seeded_library(&["a.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let inputs = export_inputs(&selection, &destination);
        prepare_instance(&db, &caller, &workflow_ref, 1, &inputs).unwrap();

        authorization::revoke_all_directory_grants(&db, &caller).unwrap();

        let error = prepare_instance(&db, &caller, &workflow_ref, 1, &inputs)
            .expect_err("目录授权撤销后不能继续复用");
        assert!(
            matches!(error.code(), "GRANT_REVOKED" | "REFERENCE_EXPIRED"),
            "撤销后的错误必须可读且指向重新授权，实际为 {}",
            error.code()
        );
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn plan_hash_mismatch_is_detected_when_the_workflow_changes() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();

        // 模拟流程内容在准备之后被改动：快照里的素材集合变了，planHash 就不再匹配。
        {
            let conn = db.lock().unwrap();
            let record = plans_db::plan(&conn, &preview.plan_id).unwrap().unwrap();
            let mut snapshot: WorkflowRunSnapshot =
                serde_json::from_str(record.details_json.as_deref().unwrap()).unwrap();
            snapshot.steps[0].asset_ids.push(ids[0] + 999);
            let tampered = serde_json::to_string(&snapshot).unwrap();
            conn.execute(
                "UPDATE plans SET details_json=?2 WHERE plan_id=?1",
                params![preview.plan_id, tampered],
            )
            .unwrap();
        }

        let error = submit_instance(&db, &caller, &preview.plan_id)
            .err()
            .expect("快照被改动后必须拒绝执行旧内容");
        assert_eq!(error.code(), AgentErrorCode::PlanChanged.as_str());
        assert_eq!(operation_count(&db), 0);
    }

    // ------------------------------------------------------------------
    // A4-08 关闭证据：依赖阻断、部分成功、取消、受理后可查询、同键重试对账
    // ------------------------------------------------------------------

    /// 与 [`workflow`] 相同，但允许逐步指定依赖。
    ///
    /// 卡面要求验证「前置没完成就不能继续」**并且**「独立步骤同时覆盖」，所以测试
    /// 必须能写出「带依赖的步骤」与「不带依赖的步骤」混在一条流程里；而 [`workflow`]
    /// 恒传 `depends_on: None`（既有 6 个用例靠它验证无依赖流程按序执行），不能改它
    /// 的默认语义。`depends_on` 是 0 基的前序下标，依赖目标的 ordinal 是它加一。
    fn workflow_with_deps(
        db: &Database,
        caller: &CallerContext,
        name: &str,
        steps: &[(&str, Option<u32>)],
        parameters: &[&str],
    ) -> String {
        let view = workflows::create_workflow(
            db,
            caller,
            WorkflowDraftInput {
                name: name.to_string(),
                description: "测试用常用流程".to_string(),
                steps: steps
                    .iter()
                    .map(|(capability, depends_on)| WorkflowStepInput {
                        capability: (*capability).to_string(),
                        depends_on: *depends_on,
                    })
                    .collect(),
                parameters: parameters
                    .iter()
                    .map(|key| WorkflowParameterInput {
                        key: (*key).to_string(),
                        required: true,
                        description: String::new(),
                    })
                    .collect(),
            },
        )
        .unwrap();
        view.workflow_ref
    }

    /// 在托管根下建一个真实存在的分库，并返回它的稳定引用。
    fn collection_ref_in_root(root: &Path, db: &Database) -> String {
        std::fs::create_dir_all(root.join("分库一")).unwrap();
        let conn = db.lock().unwrap();
        let library = libraries::current(&conn).unwrap().expect("总仓已建立");
        collections::ensure(
            &conn,
            &library.library_id,
            &library.library_epoch,
            "分库一",
            1,
        )
        .unwrap()
        .collection_ref
    }

    /// 按 ordinal 取该步的全部 item，便于核对「整步跳过」而不是「部分跳过」。
    fn items_of_ordinal(
        db: &Database,
        operation_id: &str,
        ordinal: i64,
    ) -> Vec<operations_db::OperationItemRecord> {
        let conn = db.lock().unwrap();
        let steps = operations_db::steps(&conn, operation_id).unwrap();
        let step = steps
            .iter()
            .find(|row| row.ordinal == ordinal)
            .expect("每一步都应该有一行 operation_steps");
        operations_db::items(&conn, operation_id, &step.step_id).unwrap()
    }

    /// 阻断行一：导出没有全部成功时，依赖它的步骤必须**整步**不执行；
    /// 同一份计划里没有依赖的步骤照常执行（卡面「独立步骤……同时覆盖」）。
    #[test]
    fn dependent_step_is_skipped_when_the_export_partially_fails() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let target = collection_ref_in_root(root.path(), &db);
        let workflow_ref = workflow_with_deps(
            &db,
            &caller,
            "导出后移动，再单独移入回收站",
            &[
                ("exports.submit", None),
                // 依赖目标的 ordinal = depends_on + 1，所以 Some(0) 指第 1 步（导出）。
                ("assets.move.submit", Some(0)),
                // 没有依赖的独立步骤：它不该被第 1 步的失败牵连。
                ("assets.trash.submit", None),
            ],
            &["selectionRef", "destinationRef", "collectionRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let inputs = WorkflowInstanceInputs {
            selection_ref: Some(selection),
            destination_ref: Some(destination.as_str().to_string()),
            collection_ref: Some(target.clone()),
            ..Default::default()
        };
        // 第 1 步会在真实文件上失败：源文件被移走，但库内记录仍然可信，
        // 于是导出把这一项记成 failed 并继续，而不是整批中止。
        std::fs::remove_file(root.path().join("a.jpg")).unwrap();

        let preview = prepare_instance(&db, &caller, &workflow_ref, 1, &inputs).unwrap();
        assert_eq!(preview.steps[0].depends_on, None, "第 1 步是独立的");
        assert_eq!(
            preview.steps[1].depends_on,
            Some(0),
            "第 2 步依赖下标 0，即第 1 步（导出）"
        );
        assert_eq!(
            preview.steps[2].depends_on, None,
            "第 3 步没有依赖，是独立步骤"
        );

        let submitted = submit_instance(&db, &caller, &preview.plan_id).unwrap();
        let report = execute_instance(db.clone(), caller.clone(), submitted.clone()).unwrap();

        assert_eq!(
            report.steps[0].state, "failed",
            "有一项没复制成功，导出这一步就不能算成功"
        );
        assert_eq!(report.steps[0].failed, 1);
        assert_eq!(report.steps[1].state, "skipped", "依赖未满足的步骤不能执行");
        let message = report.steps[1].message.as_deref().unwrap_or_default();
        assert!(
            message.contains("前置步骤没有全部完成"),
            "跳过原因必须说清是前置没完成，实际为 {message}"
        );
        assert_eq!(
            report.steps[2].state, "succeeded",
            "没有依赖的步骤不该被第 1 步的失败牵连"
        );
        assert_eq!(
            report.operation_state, "partially_succeeded",
            "部分成功必须如实汇报，不能报成功也不能报全失败"
        );

        // 观察真实文件与逐步收据，而不只是最终摘要。
        assert!(
            root.path().join("b.jpg").exists(),
            "被跳过的移动不能动任何文件"
        );
        assert!(
            std::fs::read_dir(root.path().join("分库一"))
                .unwrap()
                .next()
                .is_none(),
            "目标分库必须保持为空"
        );
        assert_eq!(
            {
                let conn = db.lock().unwrap();
                assets::count_trashed(&conn, &ids).unwrap()
            },
            2,
            "独立步骤（移入回收站）必须真的执行了"
        );
        assert_eq!(deletion_journal_count(&db), 0, "软删不能写永久删除意图");
        let skipped_items = items_of_ordinal(&db, &submitted.operation_id, 2);
        assert_eq!(skipped_items.len(), 2, "跳过的是整步，不是部分项");
        for item in &skipped_items {
            assert_eq!(item.state, "cancelled");
            assert_eq!(item.error_code.as_deref(), Some(CODE_DEPENDENCY_UNMET));
            assert_eq!(item.effect_state, "none", "跳过的项不能留下任何文件效果");
        }
    }

    /// 阻断行二：受理后立刻就有稳定 operation 可以查询，且排队态可取消；
    /// 执行结束后同一份计划仍读得回结论，不需要重新准备。
    #[test]
    fn an_accepted_run_is_queryable_before_and_after_it_finishes() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();

        let submitted = submit_instance(&db, &caller, &preview.plan_id).unwrap();
        let start = start_view(&db, &submitted).unwrap();
        assert_eq!(start.plan_id, preview.plan_id);
        assert_eq!(start.operation_id, submitted.operation_id);
        assert_eq!(start.operation_state, "queued");
        assert!(!start.replayed, "第一次受理不是重放");
        assert!(start.can_cancel, "排队中的任务必须允许取消");

        let report = execute_instance(db.clone(), caller.clone(), submitted.clone()).unwrap();
        assert_eq!(report.operation_state, "succeeded");

        // 轮询/重开窗口走的就是这条路：只给 planId，不重新准备计划。
        let polled = instance_report(&db, &caller, &preview.plan_id).unwrap();
        assert_eq!(polled.operation_id, report.operation_id);
        assert_eq!(polled.operation_state, "succeeded");
        assert_eq!(polled.steps[0].state, "succeeded");
        assert_eq!(polled.steps[0].done, 2);
    }

    /// 阻断行二：`operations.cancel_operation` 置的 cancelling 必须真的让执行器停下，
    /// 不能只改数据库状态而文件照样被写。
    #[test]
    fn a_cancelled_operation_never_executes_its_steps() {
        let (root, db, ids) = seeded_library(&["a.jpg", "b.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();
        let submitted = submit_instance(&db, &caller, &preview.plan_id).unwrap();

        // 这一条正是 `cancel_workflow_instance` 内部会做的事：写 cancelling，
        // 由执行器在步骤边界轮询到。
        {
            let conn = db.lock().unwrap();
            assert!(operations_db::request_cancel(&conn, &submitted.operation_id, 1).unwrap());
        }

        let report = execute_instance(db.clone(), caller.clone(), submitted).unwrap();
        assert_eq!(report.operation_state, "cancelled");
        assert_eq!(report.steps[0].state, "cancelled");
        assert!(
            std::fs::read_dir(&out).unwrap().next().is_none(),
            "取消后不能往目标目录写任何文件"
        );
    }

    /// 阻断行二：同一份计划重试必须复用原任务，不产生孤儿 operation，
    /// 也不要求用户重新走一次审批。
    #[test]
    fn resubmitting_the_same_plan_replays_the_existing_operation() {
        let (root, db, ids) = seeded_library(&["a.jpg"]);
        let caller = gui_caller(&db);
        let out = root.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = export_directory_ref(&db, &caller, &out);
        let workflow_ref = workflow(
            &db,
            &caller,
            "导出一次",
            &["exports.submit"],
            &["selectionRef", "destinationRef"],
        );
        let selection = store_selection(&db, &caller, &ids);
        let preview = prepare_instance(
            &db,
            &caller,
            &workflow_ref,
            1,
            &export_inputs(&selection, &destination),
        )
        .unwrap();

        let first = submit_instance(&db, &caller, &preview.plan_id).unwrap();
        assert!(!first.replayed);
        assert_eq!(operation_count(&db), 1);

        let second = submit_instance(&db, &caller, &preview.plan_id).unwrap();
        assert!(second.replayed, "同一份计划重试必须是重放，而不是新任务");
        assert_eq!(
            second.operation_id, first.operation_id,
            "重放必须指向原来那个 operation"
        );
        assert_eq!(operation_count(&db), 1, "重试不能留下孤儿任务");
        assert_eq!(
            start_view(&db, &second).unwrap().operation_state,
            "queued",
            "重放的任务状态以库里的真实状态为准"
        );
    }
}
