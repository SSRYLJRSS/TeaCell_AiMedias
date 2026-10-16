//! GUI entry points for reviewing prepared plans and polling operations.
//!
//! `prepare_plan` and `submit_plan` are domain interfaces called by capability
//! handlers; they are not exposed as raw commands. Only the trusted GUI can
//! approve a plan, and it must present the exact review reference that came
//! with that plan.

use tauri::State;

use crate::contracts::agent::{ApprovalResult, OperationSnapshot, PlanPreview, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::error::AppResult;
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::plans;
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

#[tauri::command]
pub fn list_pending_plans(state: State<AppState>) -> AppResult<Vec<PlanPreview>> {
    let caller = gui_caller(&state)?;
    plans::pending_plans(&state.db, &caller)
}

#[tauri::command]
pub fn approve_plan(
    state: State<AppState>,
    plan_ref: ResourceRef,
    review_ref: ResourceRef,
) -> AppResult<ApprovalResult> {
    let caller = gui_caller(&state)?;
    plans::approve_plan(&state.db, &caller, &plan_ref, &review_ref)
}

#[tauri::command]
pub fn get_operation(
    state: State<AppState>,
    operation_ref: ResourceRef,
) -> AppResult<OperationSnapshot> {
    let caller = gui_caller(&state)?;
    plans::get_operation(&state.db, &caller, &operation_ref)
}
