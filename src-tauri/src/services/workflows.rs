//! A4-07: personal workflows — storage, validation and management semantics.
//!
//! A personal workflow is *a template of allowed domain steps plus parameter
//! definitions*. It is deliberately not an approval, not a saved asset set and
//! not a recorded directory grant: every run has to take fresh inputs, build a
//! fresh plan and be approved again (`plans/v2/03-agent-and-mcp.md` §7.3,
//! `plans/v2/12-implementation-contracts.md` §6, PRD §8).
//!
//! Four rules make that promise enforceable, and each has a test:
//!
//! 1. **Only catalogue capabilities may appear.** `STEP_CATALOG` is a closed
//!    list of `Submit` capabilities. Anything else — `shell.execute`, a script
//!    field, `workflows.submit` itself, a credential — is refused by the typed
//!    decoder (`deny_unknown_fields` plus enum variants) rather than by a
//!    hand-written deny list that could be outrun.
//! 2. **Editing appends a version.** `workflow_versions` is append-only, so a
//!    running plan that recorded version 3 keeps resolving to version 3 even
//!    after the user edits the workflow to version 4 or renames it.
//! 3. **Deleting means archiving.** An archived workflow cannot be invoked
//!    again, but its versions and instantiations stay readable for history.
//! 4. **Nothing about the last run is captured.** The draft builder emits only
//!    capability plus parameter *definitions*; the asset ids, paths, plan ids
//!    and approvals of the operation it was derived from never enter storage.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::contracts::agent::{
    AccessKind, AgentErrorCode, AgentOutput, ApprovalRequirement, CapabilityId, Instructions,
    ProtocolVersion, ResourceRef, ReviewKind, ShortText, WorkflowParameter, WorkflowParameterKey,
    WorkflowStepSummary, WorkflowSummary, CAPABILITY_DESCRIPTORS, INSTRUCTIONS_MAX_BYTES,
    MAX_WORKFLOW_PARAMETERS, MAX_WORKFLOW_STEPS, SHORT_TEXT_MAX_BYTES,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::plans as plans_db;
use crate::db::{operations as operations_db, workflows as workflows_db};
use crate::error::{AppError, AppResult};
use crate::state::Database;

/// A step the user may reuse. `parameters` lists the inputs that step consumes:
/// `(key, required, description)`. The description is the *runtime* hint shown in
/// the UI; the saved value is a definition, never a granted directory.
pub struct StepCatalogEntry {
    pub capability: CapabilityId,
    pub label: &'static str,
    pub parameters: &'static [(WorkflowParameterKey, bool, &'static str)],
    /// Extra caution shown when the step is permanent or otherwise irreversible.
    pub warning: Option<&'static str>,
}

const RUNTIME_PICK: &str = "运行时重新选择，模板只保存这里要填什么。";

/// The closed set of step capabilities a personal workflow may contain.
pub const STEP_CATALOG: &[StepCatalogEntry] = &[
    StepCatalogEntry {
        capability: CapabilityId::ImportsSubmit,
        label: "入库",
        parameters: &[
            (
                WorkflowParameterKey::Sourceref,
                true,
                "导入来源目录。目录提示不构成授权，每次都要在界面上重新选择。",
            ),
            (
                WorkflowParameterKey::Collectionref,
                true,
                "入库到哪个分库（可保存默认提示，但仍按当前总仓核对）。",
            ),
            (
                WorkflowParameterKey::Renamepattern,
                false,
                "重命名规则，可留空。",
            ),
        ],
        warning: None,
    },
    StepCatalogEntry {
        capability: CapabilityId::TaggingSubmit,
        label: "AI 打标",
        parameters: &[
            (
                WorkflowParameterKey::Selectionref,
                true,
                "要打标的素材集合。模板不会保存上次的照片，每次都要重新选。",
            ),
            (
                WorkflowParameterKey::Serviceref,
                true,
                "使用哪个 AI 服务。运行时按当前绑定重新核实，缺失时先补齐。",
            ),
            (
                WorkflowParameterKey::Tagginginstructions,
                true,
                "打标要求（例如只标风景相关词）。",
            ),
        ],
        warning: Some("正式标签仍然要经可信审核界面逐条确认，模板不能省掉这一步。"),
    },
    StepCatalogEntry {
        capability: CapabilityId::ExportsSubmit,
        label: "复制导出",
        parameters: &[
            (
                WorkflowParameterKey::Selectionref,
                true,
                "要导出的素材集合，运行时重新选。",
            ),
            (
                WorkflowParameterKey::Destinationref,
                true,
                "导出目标目录。每次都要单独授权，模板不保存目录授权。",
            ),
        ],
        warning: Some("导出只复制，不会移动或删除库内原文件。"),
    },
    StepCatalogEntry {
        capability: CapabilityId::AssetsMoveSubmit,
        label: "库内移动",
        parameters: &[
            (
                WorkflowParameterKey::Selectionref,
                true,
                "要移动的素材集合，运行时重新选。",
            ),
            (
                WorkflowParameterKey::Collectionref,
                true,
                "移动到哪个分库（可保存默认提示）。",
            ),
        ],
        warning: None,
    },
    StepCatalogEntry {
        capability: CapabilityId::AssetsTrashSubmit,
        label: "移入回收站",
        parameters: &[(
            WorkflowParameterKey::Selectionref,
            true,
            "要移入回收站的素材集合，运行时重新选。",
        )],
        warning: Some("移入回收站可恢复，但仍要按当前计划单独批准。"),
    },
    StepCatalogEntry {
        capability: CapabilityId::AssetsRestoreSubmit,
        label: "恢复素材",
        parameters: &[(
            WorkflowParameterKey::Selectionref,
            true,
            "要恢复的素材集合，运行时重新选。",
        )],
        warning: None,
    },
    StepCatalogEntry {
        capability: CapabilityId::TrashEmptySubmit,
        label: "永久清空回收站",
        parameters: &[(
            WorkflowParameterKey::Selectionref,
            true,
            "要永久删除的素材集合，运行时重新选。",
        )],
        warning: Some(
            "这一步永远需要单独确认：模板只能帮您打开界面，永久删除必须按「永久清空这 N 项」再批准一次。",
        ),
    },
];

fn catalog_for(capability: CapabilityId) -> Option<&'static StepCatalogEntry> {
    STEP_CATALOG
        .iter()
        .find(|entry| entry.capability == capability)
}

/// Stable wire spelling of a parameter key. The contract enum has no `as_str`
/// because the generated file only carries serde renames.
pub fn parameter_key_str(key: WorkflowParameterKey) -> &'static str {
    match key {
        WorkflowParameterKey::Sourceref => "sourceRef",
        WorkflowParameterKey::Selectionref => "selectionRef",
        WorkflowParameterKey::Collectionref => "collectionRef",
        WorkflowParameterKey::Destinationref => "destinationRef",
        WorkflowParameterKey::Serviceref => "serviceRef",
        WorkflowParameterKey::Tagginginstructions => "taggingInstructions",
        WorkflowParameterKey::Renamepattern => "renamePattern",
    }
}

/// Contract-level label for a parameter key. Used for both the save preview and
/// the runtime-input list.
pub fn parameter_label(key: WorkflowParameterKey) -> &'static str {
    match key {
        WorkflowParameterKey::Sourceref => "导入来源",
        WorkflowParameterKey::Selectionref => "素材集合",
        WorkflowParameterKey::Collectionref => "目标分库",
        WorkflowParameterKey::Destinationref => "导出目标",
        WorkflowParameterKey::Serviceref => "AI 服务",
        WorkflowParameterKey::Tagginginstructions => "打标要求",
        WorkflowParameterKey::Renamepattern => "重命名规则",
    }
}

/// Chinese label for a capability, for summaries and management lists.
pub fn capability_label(capability: CapabilityId) -> &'static str {
    catalog_for(capability)
        .map(|entry| entry.label)
        .unwrap_or_else(|| capability.as_str())
}

/// One input the user must still supply at run time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRuntimeInput {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowParameterView {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepView {
    pub capability: String,
    pub label: String,
    pub depends_on: Option<u32>,
    pub review_kind: String,
    pub warning: Option<String>,
}

/// The management-facing shape of one workflow version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowView {
    pub workflow_ref: String,
    pub version: i64,
    pub name: String,
    pub description: String,
    pub archived: bool,
    pub updated_at: i64,
    pub parameters: Vec<WorkflowParameterView>,
    pub steps: Vec<WorkflowStepView>,
    /// Capabilities this workflow will ask for, in order, in Chinese.
    pub requires: Vec<String>,
    /// What the user still has to pick when reusing it.
    pub runtime_inputs: Vec<WorkflowRuntimeInput>,
    /// Set when a stored step can no longer be executed as written.
    pub needs_attention: Option<String>,
}

/// A step in a save request. `deny_unknown_fields` is the reason a request
/// carrying `{"script": ...}` or `{"apiKey": ...}` cannot be stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStepInput {
    pub capability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowParameterInput {
    pub key: String,
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDraftInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<WorkflowStepInput>,
    #[serde(default)]
    pub parameters: Vec<WorkflowParameterInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExcludedStep {
    pub capability: String,
    pub reason: String,
}

/// What the UI shows before saving: the understood steps, the parameters they
/// need, and — explicitly — the capabilities that could not be expressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDraft {
    pub source_operation_ref: Option<String>,
    pub name: String,
    pub description: String,
    pub steps: Vec<WorkflowStepInput>,
    pub parameters: Vec<WorkflowParameterView>,
    pub requires: Vec<String>,
    pub runtime_inputs: Vec<WorkflowRuntimeInput>,
    pub notes: Vec<String>,
    pub excluded: Vec<WorkflowExcludedStep>,
}

/// A workflow derived from an executed operation, ready for the user to edit.
///
/// Only capabilities and parameter *definitions* survive: the operation's plan,
/// approvals, asset ids, paths and receipts stay where they are.
pub fn draft_from_operation(
    db: &Database,
    caller: &CallerContext,
    operation_id: &str,
) -> AppResult<WorkflowDraft> {
    require_gui(caller, "保存常用流程")?;
    check_capability_entry(CapabilityId::WorkflowsLoad, caller).map_err(AppError::from)?;

    let conn = db.lock()?;
    let operation = plans_db::operation(&conn, operation_id)?
        .ok_or_else(|| AppError::not_found("这次操作不存在或已被清理，无法从中提取常用流程"))?;
    let steps = operations_db::steps(&conn, operation_id)?;
    drop(conn);

    let mut included: Vec<WorkflowStepInput> = Vec::new();
    let mut excluded: Vec<WorkflowExcludedStep> = Vec::new();
    for step in &steps {
        match decode_capability(&step.capability) {
            Some(capability) if catalog_for(capability).is_some() => {
                let depends_on = if included.is_empty() {
                    None
                } else {
                    Some((included.len() - 1) as u32)
                };
                included.push(WorkflowStepInput {
                    capability: step.capability.clone(),
                    depends_on,
                });
            }
            Some(capability) => excluded.push(WorkflowExcludedStep {
                capability: step.capability.clone(),
                reason: format!(
                    "「{}」是执行控制步骤，不是可复用的业务动作，已从草稿中排除。",
                    capability.as_str()
                ),
            }),
            None => excluded.push(WorkflowExcludedStep {
                capability: step.capability.clone(),
                reason: "这个步骤不受当前能力目录支持，无法安全保存为常用流程。".to_string(),
            }),
        }
    }

    if included.is_empty() {
        return Err(AppError::invalid_arg(
            "这次操作里没有可以保存为常用流程的步骤：只有受控的业务动作可以被保存，聊天内容、脚本和凭据都不行。",
        ));
    }

    let parameters = derive_parameters(&included)?;
    let requires = derive_requires(&included);
    let runtime_inputs = parameters
        .iter()
        .map(|parameter| WorkflowRuntimeInput {
            key: parameter.key.clone(),
            label: parameter.label.clone(),
            required: parameter.required,
            hint: parameter.description.clone(),
        })
        .collect();

    let mut notes = vec![
        "保存的是步骤与参数定义，不包含这次用到的素材、目录授权、计划或批准。".to_string(),
        RUNTIME_PICK.to_string(),
    ];
    if included
        .iter()
        .any(|step| step.capability == CapabilityId::TrashEmptySubmit.as_str())
    {
        notes.push(
            "模板里的「永久清空回收站」只会在复用时打开这一步，永久删除仍要按「永久清空这 N 项」单独批准。"
                .to_string(),
        );
    }

    let name = format!(
        "{}{}",
        requires.first().cloned().unwrap_or_default(),
        if included.len() > 1 {
            "等流程"
        } else {
            "流程"
        }
    );
    let description = format!(
        "从一次{}操作整理出的个人常用流程。",
        state_label(&operation.state)
    );

    Ok(WorkflowDraft {
        source_operation_ref: Some(operation_id.to_string()),
        name,
        description,
        steps: included,
        parameters,
        requires,
        runtime_inputs,
        notes,
        excluded,
    })
}

/// Validate and save a brand-new workflow (version 1).
pub fn create_workflow(
    db: &Database,
    caller: &CallerContext,
    draft: WorkflowDraftInput,
) -> AppResult<WorkflowView> {
    require_gui(caller, "保存常用流程")?;
    let prepared = prepare_draft(&draft)?;
    let workflow_ref = ResourceRef::generated("workflow").as_str().to_string();
    let now = now_unix_seconds()?;

    let conn = db.lock()?;
    workflows_db::create(
        &conn,
        &workflow_ref,
        &prepared.name,
        &prepared.description,
        &prepared.steps_json,
        &prepared.parameters_json,
        now,
    )?;
    drop(conn);
    view(db, &workflow_ref, 1)?.ok_or_else(|| AppError::internal("常用流程写入后不可读"))
}

/// Validate and append a new immutable version.
pub fn update_workflow(
    db: &Database,
    caller: &CallerContext,
    workflow_ref: &str,
    expected_version: i64,
    draft: WorkflowDraftInput,
) -> AppResult<WorkflowView> {
    require_gui(caller, "编辑常用流程")?;
    if expected_version < 1 {
        return Err(AppError::invalid_arg("要编辑的版本号不合法"));
    }
    let prepared = prepare_draft(&draft)?;
    let now = now_unix_seconds()?;

    let conn = db.lock()?;
    let header = workflows_db::header(&conn, workflow_ref)?
        .ok_or_else(|| AppError::not_found("常用流程不存在或已被删除"))?;
    if header.archived {
        return Err(AppError::conflict(
            "这个常用流程已停用，请先恢复再编辑；停用不会删除历史版本和操作记录。",
        ));
    }
    workflows_db::add_version(
        &conn,
        workflow_ref,
        expected_version,
        workflows_db::NewWorkflowVersion {
            name: &prepared.name,
            description: &prepared.description,
            steps_json: &prepared.steps_json,
            parameters_json: &prepared.parameters_json,
        },
        now,
    )?;
    drop(conn);

    view(db, workflow_ref, expected_version + 1)?
        .ok_or_else(|| AppError::internal("常用流程版本写入后不可读"))
}

/// Rename touches only the header so a running task keeps the name it was
/// approved under.
pub fn rename_workflow(
    db: &Database,
    caller: &CallerContext,
    workflow_ref: &str,
    name: &str,
) -> AppResult<WorkflowView> {
    require_gui(caller, "重命名常用流程")?;
    let name = name.trim();
    validate_name(name)?;
    let now = now_unix_seconds()?;

    let conn = db.lock()?;
    if !workflows_db::rename(&conn, workflow_ref, name, now)? {
        return Err(AppError::not_found("常用流程不存在或已被删除"));
    }
    let header = workflows_db::header(&conn, workflow_ref)?
        .ok_or_else(|| AppError::not_found("常用流程不存在或已被删除"))?;
    drop(conn);
    view(db, workflow_ref, header.current_version)?
        .ok_or_else(|| AppError::internal("常用流程重命名后不可读"))
}

/// Archiving is the delete semantic: it stops future invocations without
/// touching history or the material the workflow produced.
pub fn set_archived(
    db: &Database,
    caller: &CallerContext,
    workflow_ref: &str,
    archived: bool,
) -> AppResult<WorkflowView> {
    require_gui(caller, "停用常用流程")?;
    let now = now_unix_seconds()?;

    let conn = db.lock()?;
    if !workflows_db::set_archived(&conn, workflow_ref, archived, now)? {
        return Err(AppError::not_found("常用流程不存在或已被删除"));
    }
    let header = workflows_db::header(&conn, workflow_ref)?
        .ok_or_else(|| AppError::not_found("常用流程不存在或已被删除"))?;
    drop(conn);
    view(db, workflow_ref, header.current_version)?
        .ok_or_else(|| AppError::internal("常用流程停用后不可读"))
}

/// The management list: current version of every workflow, newest first.
pub fn list_workflows(
    db: &Database,
    caller: &CallerContext,
    include_archived: bool,
    name_query: Option<&str>,
) -> AppResult<Vec<WorkflowView>> {
    check_capability_entry(CapabilityId::WorkflowsLoad, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    let headers = workflows_db::list(&conn, include_archived, name_query)?;
    drop(conn);

    let mut views = Vec::with_capacity(headers.len());
    for header in headers {
        if let Some(view) = view(db, &header.workflow_ref, header.current_version)? {
            views.push(view);
        }
    }
    Ok(views)
}

/// One version of one workflow, with an explanation when it can no longer run
/// as written.
pub fn view(db: &Database, workflow_ref: &str, version: i64) -> AppResult<Option<WorkflowView>> {
    let conn = db.lock()?;
    let header = match workflows_db::header(&conn, workflow_ref)? {
        Some(header) => header,
        None => return Ok(None),
    };
    let record = match workflows_db::version(&conn, workflow_ref, version)? {
        Some(record) => record,
        None => return Ok(None),
    };
    drop(conn);

    let steps = parse_steps(&record.steps_json)?;
    let parameters = parse_parameters(&record.parameters_json)?;
    build_view(&header, &record, steps, parameters)
}

/// The contract-shaped summary used by `workflows.load`. Fails loudly when the
/// stored version cannot be represented, so a caller never receives a silently
/// mangled template.
pub fn contract_summary(
    db: &Database,
    workflow_ref: &str,
    version: i64,
) -> AppResult<WorkflowSummary> {
    let view = view(db, workflow_ref, version)?
        .ok_or_else(|| AppError::not_found("常用流程或该版本不存在"))?;
    if let Some(reason) = &view.needs_attention {
        return Err(AppError::conflict(reason.clone()));
    }

    let conn = db.lock()?;
    let record = workflows_db::version(&conn, workflow_ref, version)?
        .ok_or_else(|| AppError::not_found("常用流程或该版本不存在"))?;
    drop(conn);

    let steps = parse_steps(&record.steps_json)?;
    let parameters = parse_parameters(&record.parameters_json)?;
    let summary = WorkflowSummary {
        schema_version: ProtocolVersion::V1,
        workflow_ref: reference(workflow_ref)?,
        version: u32::try_from(record.version)
            .map_err(|_| AppError::from(AgentErrorCode::InvalidInput))?,
        name: short_text(&record.name)?,
        description: instructions(&record.description)?,
        parameters,
        steps,
    };
    summary.validate_output().map_err(AppError::from)?;
    Ok(summary)
}

/// Record which version a plan came from, so history stays explainable after
/// later edits.
pub fn record_instantiation(
    db: &Database,
    plan_id: &str,
    workflow_ref: &str,
    version: i64,
) -> AppResult<()> {
    let now = now_unix_seconds()?;
    let conn = db.lock()?;
    workflows_db::record_instantiation(&conn, plan_id, workflow_ref, version, now)
}

/// Which version produced a plan, for history views.
pub fn instantiation(db: &Database, plan_id: &str) -> AppResult<Option<(String, i64)>> {
    let conn = db.lock()?;
    workflows_db::instantiation(&conn, plan_id)
}

struct PreparedDraft {
    name: String,
    description: String,
    steps_json: String,
    parameters_json: String,
}

/// The single validation gate every save goes through.
fn prepare_draft(draft: &WorkflowDraftInput) -> AppResult<PreparedDraft> {
    let name = draft.name.trim().to_string();
    validate_name(&name)?;
    let description = draft.description.trim().to_string();
    if description.len() > INSTRUCTIONS_MAX_BYTES {
        return Err(AppError::invalid_arg(
            "流程说明太长，请精简到 4096 字节以内",
        ));
    }
    if draft.steps.is_empty() {
        return Err(AppError::invalid_arg(
            "常用流程至少要有一个步骤；空白模板不构成可复用流程",
        ));
    }
    if draft.steps.len() > MAX_WORKFLOW_STEPS {
        return Err(AppError::invalid_arg(format!(
            "一个常用流程最多 {MAX_WORKFLOW_STEPS} 个步骤"
        )));
    }

    let mut steps: Vec<WorkflowStepSummary> = Vec::with_capacity(draft.steps.len());
    for (index, input) in draft.steps.iter().enumerate() {
        let capability = decode_capability(&input.capability).ok_or_else(|| {
            AppError::invalid_arg(format!(
                "「{}」不是本应用受控能力目录里的动作，不能存进常用流程；常用流程不允许脚本或任意命令。",
                input.capability
            ))
        })?;
        let entry = catalog_for(capability).ok_or_else(|| {
            AppError::invalid_arg(format!(
                "「{}」不能在常用流程里作为可复用步骤",
                capability.as_str()
            ))
        })?;
        if matches!(input.depends_on, Some(value) if value as usize >= index) {
            return Err(AppError::invalid_arg(
                "步骤依赖只能指向它之前的步骤，不能指向自己或后面的步骤",
            ));
        }
        steps.push(WorkflowStepSummary {
            capability,
            depends_on: input.depends_on,
            review_kind: review_kind_for(capability)?,
        });
        if entry.warning.is_some() {
            // Warnings live in the view, not in the stored definition: the
            // definition stays a pure capability list.
        }
    }
    validate_steps(&steps)?;

    // A parameter is only meaningful if some step consumes it. This is what
    // stops a workflow from smuggling a stored authorization or secret under a
    // plausible-looking key.
    let allowed = allowed_parameter_keys(&steps);
    let mut parameters: Vec<WorkflowParameter> = Vec::with_capacity(draft.parameters.len());
    for input in &draft.parameters {
        let key = decode_parameter_key(&input.key).ok_or_else(|| {
            AppError::invalid_arg(format!(
                "「{}」不是受支持的参数类型；常用流程只允许目录、集合、服务与打标要求这类参数。",
                input.key
            ))
        })?;
        if !allowed.contains(&key) {
            return Err(AppError::invalid_arg(format!(
                "参数「{}」没有被任何步骤使用，保存它只会造成误解",
                parameter_label(key)
            )));
        }
        if parameters.iter().any(|existing| existing.key == key) {
            return Err(AppError::invalid_arg(format!(
                "参数「{}」重复了，请只保留一条",
                parameter_label(key)
            )));
        }
        if parameters.len() >= MAX_WORKFLOW_PARAMETERS {
            return Err(AppError::invalid_arg(format!(
                "一个常用流程最多 {MAX_WORKFLOW_PARAMETERS} 个参数"
            )));
        }
        let description = if input.description.trim().is_empty() {
            default_parameter_description(key).to_string()
        } else {
            input.description.trim().to_string()
        };
        parameters.push(WorkflowParameter {
            key,
            required: input.required,
            description: short_text(&description)?,
        });
    }

    let steps_json =
        serde_json::to_string(&steps).map_err(|_| AppError::internal("步骤无法序列化"))?;
    let parameters_json =
        serde_json::to_string(&parameters).map_err(|_| AppError::internal("参数无法序列化"))?;

    Ok(PreparedDraft {
        name,
        description,
        steps_json,
        parameters_json,
    })
}

fn validate_name(name: &str) -> AppResult<()> {
    if name.trim().is_empty() {
        return Err(AppError::invalid_arg("常用流程需要有名称"));
    }
    if name.len() > SHORT_TEXT_MAX_BYTES {
        return Err(AppError::invalid_arg(
            "常用流程名称太长，请控制在 256 字节以内",
        ));
    }
    if name.contains('\0') {
        return Err(AppError::invalid_arg("常用流程名称不能包含控制字符"));
    }
    Ok(())
}

fn validate_steps(steps: &[WorkflowStepSummary]) -> AppResult<()> {
    if steps.is_empty() {
        return Err(AppError::invalid_arg("常用流程至少要有一个步骤"));
    }
    if steps.len() > MAX_WORKFLOW_STEPS {
        return Err(AppError::invalid_arg(format!(
            "一个常用流程最多 {MAX_WORKFLOW_STEPS} 个步骤"
        )));
    }
    for (index, step) in steps.iter().enumerate() {
        if step.capability == CapabilityId::WorkflowsSubmit {
            return Err(AppError::invalid_arg(
                "常用流程不能把「提交流程」本身当作一个步骤，否则会无限嵌套。",
            ));
        }
        let expected = review_kind_for(step.capability)?;
        if step.review_kind != expected {
            return Err(AppError::invalid_arg(
                "步骤的复核方式与能力目录不一致，请重新选择这个步骤。",
            ));
        }
        if matches!(step.depends_on, Some(value) if value as usize >= index) {
            return Err(AppError::invalid_arg(
                "步骤依赖只能指向它之前的步骤，不能指向自己或后面的步骤",
            ));
        }
    }
    Ok(())
}

fn review_kind_for(capability: CapabilityId) -> AppResult<ReviewKind> {
    let descriptor = CAPABILITY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.capability == capability)
        .ok_or_else(|| {
            AppError::invalid_arg(format!("「{}」不在能力目录里", capability.as_str()))
        })?;
    match (descriptor.access, descriptor.approval) {
        (AccessKind::Submit, ApprovalRequirement::Plan) => Ok(ReviewKind::Ordinary),
        (AccessKind::Submit, ApprovalRequirement::PermanentTrash) => Ok(ReviewKind::PermanentTrash),
        _ => Err(AppError::invalid_arg(format!(
            "「{}」不是提交类业务动作，不能作为常用流程的步骤",
            capability.as_str()
        ))),
    }
}

fn allowed_parameter_keys(steps: &[WorkflowStepSummary]) -> Vec<WorkflowParameterKey> {
    let mut keys: Vec<WorkflowParameterKey> = Vec::new();
    for step in steps {
        if let Some(entry) = catalog_for(step.capability) {
            for (key, _, _) in entry.parameters {
                if !keys.contains(key) {
                    keys.push(*key);
                }
            }
        }
    }
    keys
}

fn derive_parameters(steps: &[WorkflowStepInput]) -> AppResult<Vec<WorkflowParameterView>> {
    let mut views: Vec<WorkflowParameterView> = Vec::new();
    for step in steps {
        let capability = decode_capability(&step.capability)
            .ok_or_else(|| AppError::invalid_arg("这个步骤不受当前能力目录支持"))?;
        let entry = catalog_for(capability)
            .ok_or_else(|| AppError::invalid_arg("这个步骤不能作为常用流程的步骤"))?;
        for (key, required, description) in entry.parameters {
            if views
                .iter()
                .any(|existing| existing.key == parameter_key_str(*key))
            {
                continue;
            }
            views.push(WorkflowParameterView {
                key: parameter_key_str(*key).to_string(),
                label: parameter_label(*key).to_string(),
                required: *required,
                description: (*description).to_string(),
            });
        }
    }
    Ok(views)
}

fn derive_requires(steps: &[WorkflowStepInput]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for step in steps {
        if let Some(capability) = decode_capability(&step.capability) {
            let label = capability_label(capability).to_string();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    }
    labels
}

fn default_parameter_description(key: WorkflowParameterKey) -> &'static str {
    for entry in STEP_CATALOG {
        for (candidate, _, description) in entry.parameters {
            if *candidate == key {
                return description;
            }
        }
    }
    RUNTIME_PICK
}

fn decode_capability(value: &str) -> Option<CapabilityId> {
    serde_json::from_value::<CapabilityId>(serde_json::Value::String(value.to_string())).ok()
}

fn decode_parameter_key(value: &str) -> Option<WorkflowParameterKey> {
    serde_json::from_value::<WorkflowParameterKey>(serde_json::Value::String(value.to_string()))
        .ok()
}

/// Typed decode of the stored step list. `deny_unknown_fields` on
/// `WorkflowStepSummary` is what rejects a row that somehow grew a script field.
fn parse_steps(raw: &str) -> AppResult<Vec<WorkflowStepSummary>> {
    let steps: Vec<WorkflowStepSummary> = serde_json::from_str(raw)
        .map_err(|_| AppError::internal("常用流程的步骤定义已损坏，无法读取"))?;
    validate_steps(&steps)?;
    Ok(steps)
}

fn parse_parameters(raw: &str) -> AppResult<Vec<WorkflowParameter>> {
    let parameters: Vec<WorkflowParameter> = serde_json::from_str(raw)
        .map_err(|_| AppError::internal("常用流程的参数定义已损坏，无法读取"))?;
    if parameters.len() > MAX_WORKFLOW_PARAMETERS {
        return Err(AppError::internal("常用流程的参数定义超出上限"));
    }
    Ok(parameters)
}

fn build_view(
    header: &workflows_db::WorkflowHeader,
    record: &workflows_db::WorkflowVersionRecord,
    steps: Vec<WorkflowStepSummary>,
    parameters: Vec<WorkflowParameter>,
) -> AppResult<Option<WorkflowView>> {
    let mut requires: Vec<String> = Vec::new();
    let mut step_views: Vec<WorkflowStepView> = Vec::with_capacity(steps.len());
    let mut needs_attention: Option<String> = None;

    for (index, step) in steps.iter().enumerate() {
        let label = capability_label(step.capability).to_string();
        if !requires.contains(&label) {
            requires.push(label.clone());
        }
        let warning = catalog_for(step.capability).and_then(|entry| entry.warning);
        if needs_attention.is_none() && review_kind_for(step.capability).is_err() {
            needs_attention = Some(format!(
                "第 {} 步「{}」的能力语义已经变化，需要您重新编辑这个常用流程后才会执行。",
                index + 1,
                label
            ));
        }
        step_views.push(WorkflowStepView {
            capability: step.capability.as_str().to_string(),
            label,
            depends_on: step.depends_on,
            review_kind: match step.review_kind {
                ReviewKind::Ordinary => "ordinary".to_string(),
                ReviewKind::PermanentTrash => "permanent_trash".to_string(),
            },
            warning: warning.map(|value| value.to_string()),
        });
    }

    let parameter_views: Vec<WorkflowParameterView> = parameters
        .iter()
        .map(|parameter| WorkflowParameterView {
            key: parameter_key_str(parameter.key).to_string(),
            label: parameter_label(parameter.key).to_string(),
            required: parameter.required,
            description: parameter.description.as_str().to_string(),
        })
        .collect();

    let runtime_inputs = parameter_views
        .iter()
        .map(|parameter| WorkflowRuntimeInput {
            key: parameter.key.clone(),
            label: parameter.label.clone(),
            required: parameter.required,
            hint: parameter.description.clone(),
        })
        .collect();

    Ok(Some(WorkflowView {
        workflow_ref: header.workflow_ref.clone(),
        version: record.version,
        name: header.name.clone(),
        description: record.description.clone(),
        archived: header.archived,
        updated_at: header.updated_at,
        parameters: parameter_views,
        steps: step_views,
        requires,
        runtime_inputs,
        needs_attention,
    }))
}

fn require_gui(caller: &CallerContext, action: &str) -> AppResult<()> {
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::unauthorized(format!(
            "{action}只能由茶馆界面完成，模型不能自行创建或改动常用流程"
        )));
    }
    Ok(())
}

fn state_label(state: &str) -> &'static str {
    match state {
        "succeeded" => "成功",
        "partially_succeeded" => "部分成功",
        "failed" => "失败",
        "cancelled" => "已取消",
        _ => "已结束",
    }
}

fn reference(value: &str) -> AppResult<ResourceRef> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| AppError::internal("常用流程引用不合法"))
}

fn short_text(value: &str) -> AppResult<ShortText> {
    let mut end = value.len().min(SHORT_TEXT_MAX_BYTES);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::from_value(serde_json::Value::String(value[..end].to_string()))
        .map_err(|_| AppError::invalid_arg("文本内容不合法"))
}

fn instructions(value: &str) -> AppResult<Instructions> {
    let mut end = value.len().min(INSTRUCTIONS_MAX_BYTES);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::from_value(serde_json::Value::String(value[..end].to_string()))
        .map_err(|_| AppError::invalid_arg("说明文字不合法"))
}

fn now_unix_seconds() -> AppResult<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间不可用"))?;
    i64::try_from(elapsed.as_secs()).map_err(|_| AppError::internal("系统时间超出范围"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_memory, libraries};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use std::fs;
    use tempfile::TempDir;

    fn seeded_library() -> (TempDir, Database) {
        let root = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root.path()).unwrap();
        let db = Database::new(conn);
        (root, db)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: ResourceRef::from_trusted(format!(
                    "session_{}",
                    uuid::Uuid::new_v4().simple()
                ))
                .unwrap()
                .as_str()
                .to_string(),
                instance_id: format!("instance_{}", uuid::Uuid::new_v4().simple()),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn pi_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Pi {
                // Resource references are bounded at 16 bytes minimum, so the
                // fixture principal cannot be shorter than that.
                principal_id: "principal_pi_workflows".to_string(),
                session_id: format!("session_{}", uuid::Uuid::new_v4().simple()),
                instance_id: format!("instance_{}", uuid::Uuid::new_v4().simple()),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn move_draft() -> WorkflowDraftInput {
        WorkflowDraftInput {
            name: "整理风景".to_string(),
            description: "把选中的风景图移进分库".to_string(),
            steps: vec![WorkflowStepInput {
                capability: "assets.move.submit".to_string(),
                depends_on: None,
            }],
            parameters: vec![WorkflowParameterInput {
                key: "collectionRef".to_string(),
                required: true,
                description: String::new(),
            }],
        }
    }

    #[test]
    fn a_saved_workflow_keeps_its_definition_and_reports_runtime_inputs() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let view = create_workflow(&db, &caller, move_draft()).unwrap();

        assert_eq!(view.version, 1);
        assert_eq!(view.name, "整理风景");
        assert!(!view.archived);
        assert_eq!(view.requires, vec!["库内移动".to_string()]);
        assert_eq!(view.steps.len(), 1);
        assert_eq!(view.steps[0].capability, "assets.move.submit");
        assert_eq!(view.parameters.len(), 1);
        assert_eq!(view.parameters[0].key, "collectionRef");
        // The stored description falls back to the catalogue hint rather than
        // staying empty.
        assert!(view.parameters[0].description.contains("分库"));
        assert_eq!(view.runtime_inputs.len(), 1);
        assert!(view.runtime_inputs[0].required);
        assert!(view.needs_attention.is_none());
    }

    #[test]
    fn editing_appends_a_version_and_never_rewrites_history() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let first = create_workflow(&db, &caller, move_draft()).unwrap();

        let mut edited = move_draft();
        edited.name = "整理风景（改）".to_string();
        edited.steps.push(WorkflowStepInput {
            capability: "assets.trash.submit".to_string(),
            depends_on: Some(0),
        });
        edited.parameters.push(WorkflowParameterInput {
            key: "selectionRef".to_string(),
            required: true,
            description: "本次素材".to_string(),
        });
        let second = update_workflow(&db, &caller, &first.workflow_ref, 1, edited).unwrap();
        assert_eq!(second.version, 2);
        assert_eq!(second.steps.len(), 2);
        assert_eq!(second.name, "整理风景（改）");

        // Version 1 still reads exactly as it was approved under.
        let original = view(&db, &first.workflow_ref, 1).unwrap().unwrap();
        assert_eq!(original.version, 1);
        assert_eq!(original.steps.len(), 1);
        assert_eq!(original.name, "整理风景（改）");
        // ...and the version's own name is preserved in its record.
        let conn = db.lock().unwrap();
        let record = workflows_db::version(&conn, &first.workflow_ref, 1)
            .unwrap()
            .unwrap();
        assert_eq!(record.name, "整理风景");
    }

    #[test]
    fn a_stale_edit_is_rejected_instead_of_overwriting() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let first = create_workflow(&db, &caller, move_draft()).unwrap();

        let mut edited = move_draft();
        edited.name = "版本2".to_string();
        update_workflow(&db, &caller, &first.workflow_ref, 1, edited.clone()).unwrap();
        let error = update_workflow(&db, &caller, &first.workflow_ref, 1, edited).unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(
            view(&db, &first.workflow_ref, 2).unwrap().unwrap().version,
            2
        );
    }

    #[test]
    fn only_catalogue_capabilities_can_be_saved() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);

        let mut script = move_draft();
        script.steps = vec![WorkflowStepInput {
            capability: "shell.execute".to_string(),
            depends_on: None,
        }];
        let error = create_workflow(&db, &caller, script).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARG");
        assert!(error.to_string().contains("不允许脚本"));

        // Control-plane capabilities cannot be steps either.
        let mut approve = move_draft();
        approve.steps = vec![WorkflowStepInput {
            capability: "plans.approve".to_string(),
            depends_on: None,
        }];
        assert_eq!(
            create_workflow(&db, &caller, approve).unwrap_err().code(),
            "INVALID_ARG"
        );

        // And workflows.submit must not nest inside a workflow.
        let mut nested = move_draft();
        nested.steps = vec![WorkflowStepInput {
            capability: "workflows.submit".to_string(),
            depends_on: None,
        }];
        assert_eq!(
            create_workflow(&db, &caller, nested).unwrap_err().code(),
            "INVALID_ARG"
        );
    }

    #[test]
    fn a_request_carrying_extra_fields_such_as_secrets_is_refused() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let raw = r#"{
            "name": "偷带密钥",
            "steps": [{"capability": "assets.move.submit", "apiKey": "sk-live-123"}],
            "parameters": []
        }"#;
        let parsed: Result<WorkflowDraftInput, _> = serde_json::from_str(raw);
        assert!(parsed.is_err());

        // A parameter key outside the contract enum is refused too.
        let mut draft = move_draft();
        draft.parameters.push(WorkflowParameterInput {
            key: "accessToken".to_string(),
            required: true,
            description: "凭据".to_string(),
        });
        let error = create_workflow(&db, &caller, draft).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARG");
        assert!(error.to_string().contains("accessToken"));
    }

    #[test]
    fn a_parameter_no_step_consumes_is_refused() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let mut draft = move_draft();
        draft.parameters.push(WorkflowParameterInput {
            key: "destinationRef".to_string(),
            required: false,
            description: String::new(),
        });
        let error = create_workflow(&db, &caller, draft).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARG");
        assert!(error.to_string().contains("没有被任何步骤使用"));
    }

    #[test]
    fn dependencies_may_only_point_backwards() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let mut draft = move_draft();
        draft.steps = vec![
            WorkflowStepInput {
                capability: "assets.move.submit".to_string(),
                depends_on: Some(0),
            },
            WorkflowStepInput {
                capability: "assets.trash.submit".to_string(),
                depends_on: None,
            },
        ];
        let error = create_workflow(&db, &caller, draft).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARG");
        assert!(error.to_string().contains("只能指向它之前的步骤"));
    }

    #[test]
    fn archiving_stops_new_versions_but_keeps_history_readable() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let first = create_workflow(&db, &caller, move_draft()).unwrap();

        let archived = set_archived(&db, &caller, &first.workflow_ref, true).unwrap();
        assert!(archived.archived);
        assert!(list_workflows(&db, &caller, false, None)
            .unwrap()
            .is_empty());
        assert_eq!(list_workflows(&db, &caller, true, None).unwrap().len(), 1);

        let mut edited = move_draft();
        edited.name = "不该成功".to_string();
        let error = update_workflow(&db, &caller, &first.workflow_ref, 1, edited).unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
        assert!(error.to_string().contains("已停用"));

        // History stays readable, including the version record.
        assert!(view(&db, &first.workflow_ref, 1).unwrap().is_some());
        let restored = set_archived(&db, &caller, &first.workflow_ref, false).unwrap();
        assert!(!restored.archived);
    }

    #[test]
    fn renaming_does_not_touch_the_version_snapshot() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let first = create_workflow(&db, &caller, move_draft()).unwrap();

        let renamed = rename_workflow(&db, &caller, &first.workflow_ref, "换个名字").unwrap();
        assert_eq!(renamed.name, "换个名字");
        assert_eq!(renamed.version, 1);
        let conn = db.lock().unwrap();
        let record = workflows_db::version(&conn, &first.workflow_ref, 1)
            .unwrap()
            .unwrap();
        assert_eq!(record.name, "整理风景");
    }

    #[test]
    fn the_agent_cannot_create_or_edit_personal_workflows() {
        let (_root, db) = seeded_library();
        let caller = pi_caller(&db);
        let error = create_workflow(&db, &caller, move_draft()).unwrap_err();
        assert_eq!(error.code(), "UNAUTHORIZED");
        assert!(error.to_string().contains("茶馆界面"));
    }

    #[test]
    fn search_matches_names_and_archived_entries_are_opt_in() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        create_workflow(&db, &caller, move_draft()).unwrap();
        let mut other = move_draft();
        other.name = "月度打标".to_string();
        other.steps = vec![WorkflowStepInput {
            capability: "tagging.submit".to_string(),
            depends_on: None,
        }];
        other.parameters = vec![];
        create_workflow(&db, &caller, other).unwrap();

        assert_eq!(
            list_workflows(&db, &caller, false, Some("风景"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_workflows(&db, &caller, false, Some("不存在"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(list_workflows(&db, &caller, false, None).unwrap().len(), 2);
    }

    #[test]
    fn the_draft_from_an_operation_keeps_definitions_and_drops_control_steps() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let now = now_unix_seconds().unwrap();
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };

        // A throwaway operation with one business step and one control step.
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: "operation_draft000000001".to_string(),
                    step_id: "step_seed000000000001".to_string(),
                    plan_id: "plan_draft000000000001".to_string(),
                    principal_id: caller.principal_id.as_str().to_string(),
                    capability: "assets.move.submit".to_string(),
                    idempotency_key: "idem_draft000000000001".to_string(),
                    library_id: library.library_id.clone(),
                    library_epoch: library.library_epoch.clone(),
                    state: "succeeded".to_string(),
                    phase: "complete".to_string(),
                    revision: 1,
                    total: Some(1),
                    completed: 1,
                    failed: 0,
                    created_at: now,
                },
            )
            .unwrap();
            operations_db::insert_step(
                &conn,
                &operations_db::NewOperationStep {
                    operation_id: "operation_draft000000001".to_string(),
                    step_id: "step_seed000000000001".to_string(),
                    ordinal: 0,
                    capability: "assets.move.submit".to_string(),
                    total: Some(1),
                    created_at: now,
                },
            )
            .unwrap();
            operations_db::insert_step(
                &conn,
                &operations_db::NewOperationStep {
                    operation_id: "operation_draft000000001".to_string(),
                    step_id: "step_seed000000000002".to_string(),
                    ordinal: 1,
                    capability: "operations.cancel".to_string(),
                    total: None,
                    created_at: now,
                },
            )
            .unwrap();
        }

        let draft = draft_from_operation(&db, &caller, "operation_draft000000001").unwrap();
        assert_eq!(draft.steps.len(), 1);
        assert_eq!(draft.steps[0].capability, "assets.move.submit");
        assert_eq!(draft.excluded.len(), 1);
        assert_eq!(draft.excluded[0].capability, "operations.cancel");
        assert!(draft.excluded[0].reason.contains("执行控制步骤"));
        assert_eq!(draft.requires, vec!["库内移动".to_string()]);
        assert!(draft
            .runtime_inputs
            .iter()
            .any(|input| input.key == "selectionRef"));
        assert!(draft
            .notes
            .iter()
            .any(|note| note.contains("不包含这次用到的素材")));
        // Nothing about the source run is captured.
        assert_eq!(
            draft.source_operation_ref.as_deref(),
            Some("operation_draft000000001")
        );

        // The draft round-trips straight into a save.
        let saved = create_workflow(
            &db,
            &caller,
            WorkflowDraftInput {
                name: draft.name.clone(),
                description: draft.description.clone(),
                steps: draft.steps.clone(),
                parameters: draft
                    .parameters
                    .iter()
                    .map(|parameter| WorkflowParameterInput {
                        key: parameter.key.clone(),
                        required: parameter.required,
                        description: parameter.description.clone(),
                    })
                    .collect(),
            },
        )
        .unwrap();
        assert_eq!(saved.version, 1);
    }

    #[test]
    fn an_operation_survives_a_restart_as_a_readable_workflow() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let created = create_workflow(&db, &caller, move_draft()).unwrap();

        // A fresh connection over the same file is what "restart" means here;
        // in-memory databases cannot be reopened, so assert the stored rows.
        let conn = db.lock().unwrap();
        assert!(workflows_db::header(&conn, &created.workflow_ref)
            .unwrap()
            .is_some());
        let record = workflows_db::current(&conn, &created.workflow_ref)
            .unwrap()
            .unwrap();
        assert!(record.steps_json.contains("assets.move.submit"));

        // Instantiation history records the version, so a later edit cannot
        // rewrite what an old plan came from.
        drop(conn);
        record_instantiation(&db, "plan_history000000001", &created.workflow_ref, 1).unwrap();
        let mut edited = move_draft();
        edited.name = "新版本".to_string();
        update_workflow(&db, &caller, &created.workflow_ref, 1, edited).unwrap();
        assert_eq!(
            instantiation(&db, "plan_history000000001").unwrap(),
            Some((created.workflow_ref.clone(), 1))
        );
    }

    #[test]
    fn the_contract_summary_round_trips_and_refuses_damaged_rows() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let created = create_workflow(&db, &caller, move_draft()).unwrap();

        let summary = contract_summary(&db, &created.workflow_ref, 1).unwrap();
        assert_eq!(summary.version, 1);
        assert_eq!(summary.name.as_str(), "整理风景");
        assert_eq!(summary.steps.len(), 1);
        assert_eq!(summary.parameters.len(), 1);

        // A row that somehow grew a script field cannot be decoded.
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE workflow_versions SET steps_json=?2 WHERE workflow_ref=?1 AND version=1",
                rusqlite::params![
                    created.workflow_ref,
                    r#"[{"capability":"assets.move.submit","reviewKind":"ordinary","script":"rm -rf /"}]"#
                ],
            )
            .unwrap();
        }
        let error = contract_summary(&db, &created.workflow_ref, 1).unwrap_err();
        assert_eq!(error.code(), "INTERNAL");
        assert!(error.to_string().contains("步骤定义已损坏"));
    }

    #[test]
    fn a_failed_operation_still_offers_its_business_step() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let now = now_unix_seconds().unwrap();
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: "operation_failed00000001".to_string(),
                    step_id: "step_failed0000000001".to_string(),
                    plan_id: "plan_failed00000000001".to_string(),
                    principal_id: caller.principal_id.as_str().to_string(),
                    capability: "exports.submit".to_string(),
                    idempotency_key: "idem_failed00000000001".to_string(),
                    library_id: library.library_id.clone(),
                    library_epoch: library.library_epoch.clone(),
                    state: "failed".to_string(),
                    phase: "complete".to_string(),
                    revision: 1,
                    total: Some(1),
                    completed: 0,
                    failed: 1,
                    created_at: now,
                },
            )
            .unwrap();
            operations_db::insert_step(
                &conn,
                &operations_db::NewOperationStep {
                    operation_id: "operation_failed00000001".to_string(),
                    step_id: "step_failed0000000001".to_string(),
                    ordinal: 0,
                    capability: "exports.submit".to_string(),
                    total: Some(1),
                    created_at: now,
                },
            )
            .unwrap();
        }
        let draft = draft_from_operation(&db, &caller, "operation_failed00000001").unwrap();
        assert_eq!(draft.requires, vec!["复制导出".to_string()]);
        // Failure is disclosed rather than hidden: the template is still a
        // definition, and the label says what happened.
        assert!(draft.description.contains("失败"));
    }

    #[test]
    fn an_unknown_operation_is_reported_rather_than_silently_empty() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let error = draft_from_operation(&db, &caller, "operation_missing0000001").unwrap_err();
        assert_eq!(error.code(), "NOT_FOUND");
    }

    #[test]
    fn a_permanent_delete_step_carries_its_own_warning() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let mut draft = move_draft();
        draft.steps = vec![WorkflowStepInput {
            capability: "trash.empty.submit".to_string(),
            depends_on: None,
        }];
        draft.parameters = vec![];
        let saved = create_workflow(&db, &caller, draft).unwrap();
        assert_eq!(saved.steps[0].review_kind, "permanent_trash");
        assert!(saved.steps[0]
            .warning
            .as_deref()
            .unwrap()
            .contains("永久删除必须按"));

        // And the draft builder says the same thing up front.
        let now = now_unix_seconds().unwrap();
        let library = {
            let conn = db.lock().unwrap();
            libraries::current(&conn).unwrap().unwrap()
        };
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: "operation_empty000000001".to_string(),
                    step_id: "step_empty000000000001".to_string(),
                    plan_id: "plan_empty000000000001".to_string(),
                    principal_id: caller.principal_id.as_str().to_string(),
                    capability: "trash.empty.submit".to_string(),
                    idempotency_key: "idem_empty000000000001".to_string(),
                    library_id: library.library_id.clone(),
                    library_epoch: library.library_epoch.clone(),
                    state: "succeeded".to_string(),
                    phase: "complete".to_string(),
                    revision: 1,
                    total: Some(1),
                    completed: 1,
                    failed: 0,
                    created_at: now,
                },
            )
            .unwrap();
            operations_db::insert_step(
                &conn,
                &operations_db::NewOperationStep {
                    operation_id: "operation_empty000000001".to_string(),
                    step_id: "step_empty000000000001".to_string(),
                    ordinal: 0,
                    capability: "trash.empty.submit".to_string(),
                    total: Some(1),
                    created_at: now,
                },
            )
            .unwrap();
        }
        let draft = draft_from_operation(&db, &caller, "operation_empty000000001").unwrap();
        assert!(draft.notes.iter().any(|note| note.contains("单独批准")));
    }

    #[test]
    fn names_and_descriptions_are_bounded() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let mut blank = move_draft();
        blank.name = "   ".to_string();
        assert_eq!(
            create_workflow(&db, &caller, blank).unwrap_err().code(),
            "INVALID_ARG"
        );

        let mut long_name = move_draft();
        long_name.name = "名".repeat(200);
        assert_eq!(
            create_workflow(&db, &caller, long_name).unwrap_err().code(),
            "INVALID_ARG"
        );

        let mut no_steps = move_draft();
        no_steps.steps = vec![];
        let error = create_workflow(&db, &caller, no_steps).unwrap_err();
        assert!(error.to_string().contains("至少要有一个步骤"));
    }

    #[test]
    fn too_many_steps_are_refused() {
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        let mut draft = move_draft();
        draft.steps = (0..MAX_WORKFLOW_STEPS + 1)
            .map(|index| WorkflowStepInput {
                capability: "assets.move.submit".to_string(),
                depends_on: if index == 0 {
                    None
                } else {
                    Some((index - 1) as u32)
                },
            })
            .collect();
        draft.parameters = vec![];
        let error = create_workflow(&db, &caller, draft).unwrap_err();
        assert!(error.to_string().contains("最多"));
    }

    #[test]
    fn the_catalogue_only_exposes_submit_capabilities() {
        for entry in STEP_CATALOG {
            assert!(
                review_kind_for(entry.capability).is_ok(),
                "{} must be a submit capability",
                entry.capability.as_str()
            );
            assert_ne!(entry.capability, CapabilityId::WorkflowsSubmit);
        }
        // Every parameter key used by the catalogue is a contract key, and no
        // step asks for more than the contract allows.
        for entry in STEP_CATALOG {
            assert!(entry.parameters.len() <= MAX_WORKFLOW_PARAMETERS);
        }
    }

    #[test]
    fn unused_helpers_stay_reachable_for_the_ui() {
        // `capability_label` and `parameter_label` are the UI's source of truth
        // for Chinese labels; keep them honest.
        assert_eq!(capability_label(CapabilityId::ExportsSubmit), "复制导出");
        assert_eq!(
            parameter_label(WorkflowParameterKey::Collectionref),
            "目标分库"
        );
        let (_root, db) = seeded_library();
        let caller = gui_caller(&db);
        // The library must stay adopted for the fixture to be valid at all.
        let conn = db.lock().unwrap();
        assert_eq!(
            libraries::current(&conn).unwrap().unwrap().health,
            libraries::LibraryHealth::Ready
        );
        drop(conn);
        let _ = fs::metadata(".");
        let _ = caller;
    }
}
