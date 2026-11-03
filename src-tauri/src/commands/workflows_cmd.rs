//! GUI entry points for personal workflows (A4-07).
//!
//! Every command here goes through `services::workflows`, which owns validation,
//! versioning and the catalogue allow-list. The command layer only unwraps
//! arguments and constructs the trusted GUI caller: a model adapter must never be
//! able to create or edit a stored template, because a template is code-like —
//! it decides which capabilities get asked for later.

use tauri::State;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::workflows::{self, WorkflowDraft, WorkflowDraftInput, WorkflowView};
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

/// The closed catalogue the editor may offer.
///
/// The frontend must not invent step names: this list *is* the allow-list, and
/// the save path re-validates against the same catalogue, so a UI that renders a
/// stale option still cannot store it.
#[tauri::command]
pub fn workflow_step_catalog() -> Vec<WorkflowStepCatalogView> {
    workflows::STEP_CATALOG
        .iter()
        .map(|entry| WorkflowStepCatalogView {
            capability: entry.capability.as_str().to_string(),
            label: entry.label.to_string(),
            warning: entry.warning.map(|value| value.to_string()),
            parameters: entry
                .parameters
                .iter()
                .map(|(key, required, description)| WorkflowStepParameterView {
                    key: workflows::parameter_key_str(*key).to_string(),
                    label: workflows::parameter_label(*key).to_string(),
                    required: *required,
                    description: (*description).to_string(),
                })
                .collect(),
        })
        .collect()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepCatalogView {
    pub capability: String,
    pub label: String,
    pub warning: Option<String>,
    pub parameters: Vec<WorkflowStepParameterView>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepParameterView {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub description: String,
}

/// Turn an executed operation into an editable draft.
///
/// Steps that cannot be expressed legally are reported in `excluded` with a
/// reason instead of being silently dropped or turned into a script.
#[tauri::command]
pub fn draft_workflow_from_operation(
    state: State<AppState>,
    operation_id: String,
) -> AppResult<WorkflowDraft> {
    let caller = gui_caller(&state)?;
    workflows::draft_from_operation(&state.db, &caller, &operation_id)
}

#[tauri::command]
pub fn create_workflow(
    state: State<AppState>,
    draft: WorkflowDraftInput,
) -> AppResult<WorkflowView> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "保存常用流程")?;
    let caller = gui_caller(&state)?;
    workflows::create_workflow(&state.db, &caller, draft)
}

/// Editing appends a version. `expected_version` is a compare-and-set so a stale
/// window cannot overwrite a newer edit.
#[tauri::command]
pub fn update_workflow(
    state: State<AppState>,
    workflow_ref: String,
    expected_version: i64,
    draft: WorkflowDraftInput,
) -> AppResult<WorkflowView> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "编辑常用流程")?;
    let caller = gui_caller(&state)?;
    workflows::update_workflow(&state.db, &caller, &workflow_ref, expected_version, draft)
}

#[tauri::command]
pub fn rename_workflow(
    state: State<AppState>,
    workflow_ref: String,
    name: String,
) -> AppResult<WorkflowView> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "重命名常用流程")?;
    let caller = gui_caller(&state)?;
    workflows::rename_workflow(&state.db, &caller, &workflow_ref, &name)
}

/// Archiving is the delete semantic: it stops new invocations and keeps history.
#[tauri::command]
pub fn set_workflow_archived(
    state: State<AppState>,
    workflow_ref: String,
    archived: bool,
) -> AppResult<WorkflowView> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "停用常用流程")?;
    let caller = gui_caller(&state)?;
    workflows::set_archived(&state.db, &caller, &workflow_ref, archived)
}

#[tauri::command]
pub fn list_workflows(
    state: State<AppState>,
    include_archived: Option<bool>,
    name_query: Option<String>,
) -> AppResult<Vec<WorkflowView>> {
    let caller = gui_caller(&state)?;
    workflows::list_workflows(
        &state.db,
        &caller,
        include_archived.unwrap_or(false),
        name_query.as_deref(),
    )
}

#[tauri::command]
pub fn get_workflow(
    state: State<AppState>,
    workflow_ref: String,
    version: i64,
) -> AppResult<WorkflowView> {
    let caller = gui_caller(&state)?;
    // Reading is deliberately not gated by the maintenance lock: the card
    // requires browsing and old conversations to keep working during a move.
    let _ = &caller;
    workflows::view(&state.db, &workflow_ref, version)?
        .ok_or_else(|| AppError::not_found("常用流程或该版本不存在"))
}

/// Which workflow version a plan came from, for history views.
///
/// Returns `null` when the plan was not produced from a workflow, so the UI can
/// distinguish "not from a template" from "template deleted".
#[tauri::command]
pub fn workflow_for_plan(
    state: State<AppState>,
    plan_id: String,
) -> AppResult<Option<WorkflowInstantiationView>> {
    let caller = gui_caller(&state)?;
    let _ = &caller;
    let Some((workflow_ref, version)) = workflows::instantiation(&state.db, &plan_id)? else {
        return Ok(None);
    };
    let name = workflows::view(&state.db, &workflow_ref, version)?
        .map(|view| view.name)
        .unwrap_or_else(|| "已删除的常用流程".to_string());
    Ok(Some(WorkflowInstantiationView {
        workflow_ref,
        version,
        name,
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInstantiationView {
    pub workflow_ref: String,
    pub version: i64,
    pub name: String,
}
