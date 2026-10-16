//! GUI-only controls for historical assets outside the managed library.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::legacy_assets::{
    self, PendingLegacyAssetPage, ResolutionPreview, ResolutionResult,
};
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

struct FileOperationGuard(Arc<AtomicBool>);

impl FileOperationGuard {
    fn acquire(state: &AppState) -> AppResult<Self> {
        state
            .import_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .map_err(|_| AppError::conflict("已有文件检查、入库或维护任务正在运行"))?;
        Ok(Self(Arc::clone(&state.import_running)))
    }
}

impl Drop for FileOperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[tauri::command]
pub fn list_legacy_assets(
    state: State<AppState>,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<PendingLegacyAssetPage> {
    list_legacy_assets_with_state(&state, after_asset_id, limit)
}

fn list_legacy_assets_with_state(
    state: &AppState,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<PendingLegacyAssetPage> {
    let caller = gui_caller(state)?;
    legacy_assets::list_pending(&state.db, &caller, after_asset_id, limit)
}

#[tauri::command]
pub fn list_unlinked_legacy_assets(
    state: State<AppState>,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<Vec<crate::db::legacy_assets::PendingLegacyAsset>> {
    let caller = gui_caller(&state)?;
    legacy_assets::list_unlinked(&state.db, &caller, after_asset_id, limit)
}

#[tauri::command]
pub fn prepare_legacy_unlink(
    state: State<AppState>,
    asset_id: i64,
) -> AppResult<ResolutionPreview> {
    prepare_legacy_unlink_with_state(&state, asset_id)
}

fn prepare_legacy_unlink_with_state(
    state: &AppState,
    asset_id: i64,
) -> AppResult<ResolutionPreview> {
    let caller = gui_caller(state)?;
    legacy_assets::prepare_unlink(&state.db, &caller, asset_id)
}

/// The selected source remains inside the native host; the WebView never sends
/// an arbitrary filesystem path across IPC.
#[tauri::command]
pub async fn prepare_legacy_copy_back(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: i64,
) -> AppResult<Option<ResolutionPreview>> {
    let picker_app = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        picker_app
            .dialog()
            .file()
            .set_title("选择这条旧记录对应的原文件")
            .blocking_pick_file()
    })
    .await
    .map_err(|_| AppError::internal("文件选择线程异常"))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let selected = selected
        .into_path()
        .map_err(|_| AppError::invalid_arg("所选对象不是本机文件路径"))?;
    let _operation = FileOperationGuard::acquire(&state)?;
    let db = Arc::clone(&state.db);
    let caller = gui_caller(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = _operation;
        legacy_assets::prepare_copy_back(&db, &caller, asset_id, &selected)
    })
    .await
    .map_err(|_| AppError::internal("旧记录检查线程异常"))?
    .map(Some)
}

#[tauri::command]
pub fn cancel_legacy_asset_resolution(
    state: State<AppState>,
    resolution_id: ResourceRef,
) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    legacy_assets::cancel_prepared(&state.db, &caller, &resolution_id)
}

#[tauri::command]
pub async fn confirm_legacy_asset_resolution(
    state: State<'_, AppState>,
    resolution_id: ResourceRef,
) -> AppResult<ResolutionResult> {
    confirm_legacy_asset_resolution_with_state(&state, resolution_id).await
}

async fn confirm_legacy_asset_resolution_with_state(
    state: &AppState,
    resolution_id: ResourceRef,
) -> AppResult<ResolutionResult> {
    let operation = FileOperationGuard::acquire(state)?;
    let db = Arc::clone(&state.db);
    let caller = gui_caller(state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        legacy_assets::confirm(&db, &caller, &resolution_id)
    })
    .await
    .map_err(|_| AppError::internal("旧记录处置线程异常"))?
}

#[tauri::command]
pub fn reopen_unlinked_legacy_asset(state: State<AppState>, asset_id: i64) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    legacy_assets::reopen_unlinked(&state.db, &caller, asset_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn legacy_unlink_command_flow_updates_real_database_and_preserves_source() {
        let library_root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("历史素材.jpg");
        std::fs::write(&source, b"legacy asset bytes").unwrap();

        let conn = crate::db::init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, library_root.path()).unwrap();
        crate::services::managed_library::initialize_runtime(&conn).unwrap();
        let metadata = std::fs::metadata(&source).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let asset_id = crate::db::assets::insert(
            &conn,
            &crate::utils::path::encode_native_path(&source).unwrap(),
            "历史素材.jpg",
            ".jpg",
            metadata.len() as i64,
            "image/jpeg",
            modified_at,
        )
        .unwrap();
        conn.execute(
            "UPDATE managed_asset_locations SET status='external_pending',reason='outside_managed_root' WHERE asset_id=?1",
            [asset_id],
        )
        .unwrap();

        let state = AppState::new(conn, library_root.path().to_path_buf());
        let before = list_legacy_assets_with_state(&state, 0, 100).unwrap();
        assert_eq!(before.items.len(), 1);
        assert_eq!(before.items[0].asset_id, asset_id);

        let preview = prepare_legacy_unlink_with_state(&state, asset_id).unwrap();
        assert_eq!(preview.action, "unlink");
        let result = tauri::async_runtime::block_on(confirm_legacy_asset_resolution_with_state(
            &state,
            preview.resolution_id,
        ))
        .unwrap();
        assert_eq!(result.state, "unlinked");
        assert_eq!(std::fs::read(&source).unwrap(), b"legacy asset bytes");

        let pending = list_legacy_assets_with_state(&state, 0, 100).unwrap();
        assert!(pending.items.is_empty());
        let caller = gui_caller(&state).unwrap();
        let unlinked = legacy_assets::list_unlinked(&state.db, &caller, 0, 100).unwrap();
        assert_eq!(unlinked.len(), 1);

        let conn = state.db.lock().unwrap();
        let (file_path, status): (String, String) = conn
            .query_row(
                "SELECT a.file_path,ml.status FROM assets a JOIN managed_asset_locations ml ON ml.asset_id=a.id WHERE a.id=?1",
                [asset_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            file_path,
            crate::utils::path::encode_native_path(&source).unwrap()
        );
        assert_eq!(status, "external_pending");
    }
}
