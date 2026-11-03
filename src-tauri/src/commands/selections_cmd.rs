//! GUI entry points for handing a library selection to the assistant (A3-08).
//!
//! The reference produced here is an immutable snapshot of the ids the user had
//! selected at that moment. Later selection changes in the library never rewrite
//! it: the handoff either carries the exact set the user saw, or it fails.

use tauri::State;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::AppResult;
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::selections::{self, SelectionPage, SelectionSummary, SelectionView};
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

/// Snapshot the exact selected ids so a handoff cannot drift.
///
/// `total` is the number of matches the query had (which may exceed the ids the
/// GUI managed to load); the difference is reported back as `truncated`.
#[tauri::command]
pub fn snapshot_selection(
    state: State<AppState>,
    query: String,
    asset_ids: Vec<i64>,
    total: i64,
) -> AppResult<SelectionView> {
    let caller = gui_caller(&state)?;
    selections::store(&state.db, &caller, &query, &asset_ids, total)
}

/// Counts for the snapshot without loading asset rows.
///
/// The assistant panel and the result view header both show "how many handed-off
/// items are still available"; this keeps them from disagreeing with each other
/// and with the grid.
#[tauri::command]
pub fn summary_selection(
    state: State<AppState>,
    selection_ref: ResourceRef,
) -> AppResult<SelectionSummary> {
    let caller = gui_caller(&state)?;
    selections::summary(&state.db, &caller, &selection_ref)
}

/// One page of the snapshot, for the library "current result" view.
#[tauri::command]
pub fn page_selection(
    state: State<AppState>,
    selection_ref: ResourceRef,
    offset: u32,
    limit: u32,
) -> AppResult<SelectionPage> {
    let caller = gui_caller(&state)?;
    selections::page(&state.db, &caller, &selection_ref, offset, limit)
}
