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

/// 批量处置的单条结果。
///
/// 批量**不是**一个新的所有权协议：每一条仍然走同一个逐项服务函数（同样的源文件
/// 复核、同样的收据、同样的 `outcome_unknown` 判定）。这里只负责把「一次点击 N 条」
/// 拆成 N 个独立结论，并如实报出哪几条没做成——卡片要求「部分失败有明确下一步」，
/// 而把失败吞成一句「操作完成」正是它禁止的。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyBatchEntry {
    pub asset_id: i64,
    /// `prepared` / `completed` / `unlinked` / `outcome_unknown` / `failed` / `skipped`
    pub state: String,
    pub resolution_id: Option<ResourceRef>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyBatchReport {
    pub action: String,
    pub prepared: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 用户在最前面的原生选择框里按了取消：没有做任何事，界面不该报失败。
    pub cancelled: bool,
    pub entries: Vec<LegacyBatchEntry>,
}

impl LegacyBatchReport {
    fn new(action: &str, entries: Vec<LegacyBatchEntry>) -> Self {
        let prepared = entries
            .iter()
            .filter(|entry| matches!(entry.state.as_str(), "prepared" | "completed" | "unlinked"))
            .count();
        let failed = entries
            .iter()
            .filter(|entry| entry.state == "failed")
            .count();
        let skipped = entries
            .iter()
            .filter(|entry| entry.state == "skipped")
            .count();
        Self {
            action: action.to_string(),
            prepared,
            failed,
            skipped,
            cancelled: false,
            entries,
        }
    }

    fn cancelled(action: &str) -> Self {
        Self {
            action: action.to_string(),
            prepared: 0,
            failed: 0,
            skipped: 0,
            cancelled: true,
            entries: Vec::new(),
        }
    }
}

fn failed_entry(asset_id: i64, error: &AppError) -> LegacyBatchEntry {
    LegacyBatchEntry {
        asset_id,
        state: "failed".to_string(),
        resolution_id: None,
        error_code: Some(error.code().to_string()),
        message: Some(error.to_string()),
    }
}

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
    // A4-06：迁仓/重定位期间不允许对旧记录做处置——那些收据绑定 library_epoch，
    // 换根后必然失效，让用户先做完维护再处置才有意义。
    crate::commands::library_maintenance_cmd::ensure_writable(state, "处置旧记录")?;
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
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "处置旧记录")?;
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
    crate::commands::library_maintenance_cmd::ensure_writable(state, "处置旧记录")?;
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

/// 一次最多处置多少条。批量是给「几十条历史遗留」用的，不是给整库用的：上限让
/// 失败摘要仍然可读，也让一次点击不会锁住客户端太久。
const LEGACY_BATCH_MAX: usize = 100;

fn clamp_batch(asset_ids: &mut Vec<i64>) -> AppResult<()> {
    asset_ids.sort_unstable();
    asset_ids.dedup();
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("请先选择要处置的旧记录"));
    }
    if asset_ids.len() > LEGACY_BATCH_MAX {
        return Err(AppError::invalid_arg(format!(
            "一次最多处置 {LEGACY_BATCH_MAX} 条旧记录，请分批进行"
        )));
    }
    Ok(())
}

/// 批量准备「解除关联」。
///
/// 解除关联不需要用户重新选文件（它是「不再把这条记录当作待处理的库外素材」），
/// 因此可以真正批量：逐条走同一个 `prepare_unlink`，一条失败不影响其他条。
#[tauri::command]
pub fn prepare_legacy_unlink_batch(
    state: State<AppState>,
    asset_ids: Vec<i64>,
) -> AppResult<LegacyBatchReport> {
    prepare_legacy_unlink_batch_with_state(&state, asset_ids)
}

fn prepare_legacy_unlink_batch_with_state(
    state: &AppState,
    mut asset_ids: Vec<i64>,
) -> AppResult<LegacyBatchReport> {
    crate::commands::library_maintenance_cmd::ensure_writable(state, "处置旧记录")?;
    clamp_batch(&mut asset_ids)?;
    let caller = gui_caller(state)?;
    let mut entries = Vec::with_capacity(asset_ids.len());
    for asset_id in asset_ids {
        match legacy_assets::prepare_unlink(&state.db, &caller, asset_id) {
            Ok(preview) => entries.push(LegacyBatchEntry {
                asset_id,
                state: "prepared".to_string(),
                resolution_id: Some(preview.resolution_id),
                error_code: None,
                message: None,
            }),
            Err(error) => entries.push(failed_entry(asset_id, &error)),
        }
    }
    Ok(LegacyBatchReport::new("unlink", entries))
}

/// 批量准备「复制回仓」。
///
/// 一次原生多选：**每条仍然是同一套复核**（所选文件必须与记录指向的库外文件是同一个
/// 真实文件、身份/大小/修改时间/完整摘要全部对上，且不在当前总仓内）。没被选中的记录
/// 记为 `skipped`，不是失败——用户可能只想先处理一部分。违反契约直接 `failed`。
#[tauri::command]
pub async fn prepare_legacy_copy_back_batch(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_ids: Vec<i64>,
) -> AppResult<LegacyBatchReport> {
    let picker_app = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        picker_app
            .dialog()
            .file()
            .set_title("选择这些旧记录对应的原文件（可多选）")
            .blocking_pick_files()
    })
    .await
    .map_err(|_| AppError::internal("文件选择线程异常"))?;
    let Some(selected) = selected else {
        return Ok(LegacyBatchReport::cancelled("copy_back"));
    };
    let mut picked = Vec::with_capacity(selected.len());
    for entry in selected {
        picked.push(
            entry
                .into_path()
                .map_err(|_| AppError::invalid_arg("所选对象不是本机文件路径"))?,
        );
    }
    prepare_legacy_copy_back_batch_with_state(&state, asset_ids, &picked).await
}

async fn prepare_legacy_copy_back_batch_with_state(
    state: &AppState,
    mut asset_ids: Vec<i64>,
    picked: &[std::path::PathBuf],
) -> AppResult<LegacyBatchReport> {
    crate::commands::library_maintenance_cmd::ensure_writable(state, "处置旧记录")?;
    clamp_batch(&mut asset_ids)?;
    let operation = FileOperationGuard::acquire(state)?;
    let db = Arc::clone(&state.db);
    let caller = gui_caller(state)?;
    let picked = picked.to_vec();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        // 先把所选文件归一到真实路径，再按记录的真实路径配索引；用「文件名相同」配对
        // 会让两条同名不同目录的记录拿到同一个文件。
        let mut by_path: std::collections::HashMap<std::path::PathBuf, std::path::PathBuf> =
            std::collections::HashMap::new();
        for path in picked {
            let canonical = match path.canonicalize() {
                Ok(canonical) => canonical,
                Err(_) => continue,
            };
            by_path.insert(canonical, path);
        }
        let mut entries = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            let recorded = match legacy_assets::pending_record(&db, &caller, asset_id) {
                Ok(Some(asset)) => asset,
                Ok(None) => {
                    entries.push(LegacyBatchEntry {
                        asset_id,
                        state: "skipped".to_string(),
                        resolution_id: None,
                        error_code: None,
                        message: Some("该记录已不在待处理清单中".to_string()),
                    });
                    continue;
                }
                Err(error) => {
                    entries.push(failed_entry(asset_id, &error));
                    continue;
                }
            };
            let chosen = std::path::Path::new(&recorded.file_path)
                .canonicalize()
                .ok()
                .and_then(|canonical| by_path.get(&canonical).cloned());
            let Some(chosen) = chosen else {
                entries.push(LegacyBatchEntry {
                    asset_id,
                    state: "skipped".to_string(),
                    resolution_id: None,
                    error_code: None,
                    message: Some("没有为这条记录选择原文件".to_string()),
                });
                continue;
            };
            match legacy_assets::prepare_copy_back(&db, &caller, asset_id, &chosen) {
                Ok(preview) => entries.push(LegacyBatchEntry {
                    asset_id,
                    state: "prepared".to_string(),
                    resolution_id: Some(preview.resolution_id),
                    error_code: None,
                    message: preview.keep_separate_required.then(|| {
                        format!(
                            "总仓中已有 {} 条完整 SHA-256 相同的素材；本次仍保留为独立记录，不合并标签。",
                            preview.matching_assets.len()
                        )
                    }),
                }),
                Err(error) => entries.push(failed_entry(asset_id, &error)),
            }
        }
        Ok(LegacyBatchReport::new("copy_back", entries))
    })
    .await
    .map_err(|_| AppError::internal("旧记录批量检查线程异常"))?
}

/// 批量确认一组已准备好的处置收据。
///
/// 逐条确认，独立报结论。任何一条进入 `outcome_unknown` 都不会拖累其他条，也不会
/// 被算成成功——卡片要求逐项收据，这里只是把 N 次确认合并成一次点击。
#[tauri::command]
pub async fn confirm_legacy_resolutions_batch(
    state: State<'_, AppState>,
    resolutions: Vec<ResourceRef>,
) -> AppResult<LegacyBatchReport> {
    confirm_legacy_resolutions_batch_with_state(&state, resolutions).await
}

async fn confirm_legacy_resolutions_batch_with_state(
    state: &AppState,
    mut resolutions: Vec<ResourceRef>,
) -> AppResult<LegacyBatchReport> {
    crate::commands::library_maintenance_cmd::ensure_writable(state, "处置旧记录")?;
    if resolutions.is_empty() {
        return Err(AppError::invalid_arg("没有待确认的处置"));
    }
    if resolutions.len() > LEGACY_BATCH_MAX {
        return Err(AppError::invalid_arg(format!(
            "一次最多确认 {LEGACY_BATCH_MAX} 条处置，请分批进行"
        )));
    }
    resolutions.dedup_by(|left, right| left.as_str() == right.as_str());
    let operation = FileOperationGuard::acquire(state)?;
    let db = Arc::clone(&state.db);
    let caller = gui_caller(state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        let mut entries = Vec::with_capacity(resolutions.len());
        let mut action = String::new();
        for resolution in resolutions {
            // 逐条拿资产 id：确认成功后资产可能已从待处理投影里消失，所以要提前读。
            let known_asset_id = legacy_assets::resolution_asset_id(&db, &caller, &resolution)
                .ok()
                .flatten();
            match legacy_assets::confirm(&db, &caller, &resolution) {
                Ok(result) => {
                    if action.is_empty() {
                        action = result.action.clone();
                    }
                    entries.push(LegacyBatchEntry {
                        asset_id: result.asset_id,
                        state: result.state.clone(),
                        resolution_id: Some(result.resolution_id),
                        error_code: result.error_code,
                        message: None,
                    });
                }
                Err(error) => {
                    let asset_id = known_asset_id.unwrap_or(0);
                    entries.push(failed_entry(asset_id, &error));
                }
            }
        }
        Ok(LegacyBatchReport::new(
            if action.is_empty() {
                "legacy"
            } else {
                action.as_str()
            },
            entries,
        ))
    })
    .await
    .map_err(|_| AppError::internal("旧记录批量确认线程异常"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    /// 造一条「已确认在总仓外」的旧记录，返回 (asset_id, 原文件路径)。
    fn external_pending_asset(
        conn: &rusqlite::Connection,
        dir: &std::path::Path,
        name: &str,
    ) -> (i64, std::path::PathBuf) {
        let source = dir.join(name);
        std::fs::write(&source, format!("legacy bytes {name}")).unwrap();
        let metadata = std::fs::metadata(&source).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let asset_id = crate::db::assets::insert(
            conn,
            &crate::utils::path::encode_native_path(&source).unwrap(),
            name,
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
        (asset_id, source)
    }

    #[test]
    fn batch_unlink_keeps_each_receipt_and_reports_the_one_that_cannot_be_handled() {
        let library_root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let conn = crate::db::init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, library_root.path()).unwrap();
        crate::services::managed_library::initialize_runtime(&conn).unwrap();
        let (first, first_source) = external_pending_asset(&conn, source_dir.path(), "a.jpg");
        let (second, second_source) = external_pending_asset(&conn, source_dir.path(), "b.jpg");
        let state = AppState::new(conn, library_root.path().to_path_buf());

        // 第三个 id 根本不存在：批量不能因为一条坏 id 就整批失败。
        let report =
            prepare_legacy_unlink_batch_with_state(&state, vec![first, 9999, second]).unwrap();
        assert_eq!(report.action, "unlink");
        assert_eq!(report.prepared, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.skipped, 0);
        assert!(!report.cancelled);
        let failed = report
            .entries
            .iter()
            .find(|entry| entry.asset_id == 9999)
            .expect("坏 id 必须有独立结论");
        assert_eq!(failed.state, "failed");
        assert!(failed.message.is_some());

        let resolutions: Vec<ResourceRef> = report
            .entries
            .iter()
            .filter_map(|entry| entry.resolution_id.clone())
            .collect();
        assert_eq!(resolutions.len(), 2);
        let confirmed = tauri::async_runtime::block_on(
            confirm_legacy_resolutions_batch_with_state(&state, resolutions),
        )
        .unwrap();
        assert_eq!(confirmed.action, "unlink");
        assert_eq!(confirmed.prepared, 2);
        assert_eq!(confirmed.failed, 0);
        assert!(confirmed
            .entries
            .iter()
            .all(|entry| entry.state == "unlinked"));

        // 解除关联绝不删除或改写库外原文件。
        assert_eq!(std::fs::read(&first_source).unwrap(), b"legacy bytes a.jpg");
        assert_eq!(
            std::fs::read(&second_source).unwrap(),
            b"legacy bytes b.jpg"
        );

        let pending = list_legacy_assets_with_state(&state, 0, 100).unwrap();
        assert!(pending.items.is_empty());
        let caller = gui_caller(&state).unwrap();
        let unlinked = legacy_assets::list_unlinked(&state.db, &caller, 0, 100).unwrap();
        assert_eq!(unlinked.len(), 2);
    }

    #[test]
    fn batch_entries_reject_empty_and_oversized_selections() {
        let (_root, state) = seeded_state_for_batch();
        // 空选择不是「完成」：界面不该显示「已处置 0 条」。
        assert!(prepare_legacy_unlink_batch_with_state(&state, Vec::new()).is_err());
        let too_many: Vec<i64> = (1..=(LEGACY_BATCH_MAX as i64 + 1)).collect();
        assert!(prepare_legacy_unlink_batch_with_state(&state, too_many).is_err());
    }

    #[test]
    fn batch_writes_are_blocked_while_the_library_is_under_maintenance() {
        let (_root, state) = seeded_state_for_batch();
        // 维护期整批处置必须被拒，且拒绝信息要说清原因——不能静默什么都不做。
        let lease = state
            .library_maintenance
            .enter("正在迁移总仓")
            .expect("维护门应当能取到租约");
        let error = prepare_legacy_unlink_batch_with_state(&state, vec![1]).unwrap_err();
        assert!(error.to_string().contains("总仓维护"), "{error}");
        drop(lease);
        // 维护结束后同一条调用不再因为维护被拒（这里只是没找到记录，属于正常业务错误）。
        let after = prepare_legacy_unlink_batch_with_state(&state, vec![1]).unwrap();
        assert_eq!(after.failed, 1);
    }

    fn seeded_state_for_batch() -> (tempfile::TempDir, AppState) {
        let root = tempfile::tempdir().unwrap();
        let conn = crate::db::init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, root.path()).unwrap();
        let data_dir = root.path().to_path_buf();
        (root, AppState::new(conn, data_dir))
    }

    #[test]
    fn batch_copy_back_pairs_each_record_with_its_own_file_and_skips_unpicked_ones() {
        let library_root = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let conn = crate::db::init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, library_root.path()).unwrap();
        crate::services::managed_library::initialize_runtime(&conn).unwrap();
        let (first, first_source) = external_pending_asset(&conn, source_dir.path(), "a.jpg");
        let (second, _second_source) = external_pending_asset(&conn, source_dir.path(), "b.jpg");
        let state = AppState::new(conn, library_root.path().to_path_buf());

        // 只为第一条选文件：第二条必须是 skipped（用户可以先处理一部分），不是失败。
        let report = tauri::async_runtime::block_on(prepare_legacy_copy_back_batch_with_state(
            &state,
            vec![first, second],
            std::slice::from_ref(&first_source),
        ))
        .unwrap();
        assert_eq!(report.action, "copy_back");
        assert_eq!(report.prepared, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 1);
        let skipped = report
            .entries
            .iter()
            .find(|entry| entry.asset_id == second)
            .expect("未选择的记录必须有独立结论");
        assert_eq!(skipped.state, "skipped");
        assert!(skipped
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("没有为这条记录选择原文件"));

        // 准备阶段绝不复制、绝不改库：源文件与待处理投影都还在。
        assert!(first_source.exists());
        let pending = list_legacy_assets_with_state(&state, 0, 100).unwrap();
        assert_eq!(pending.items.len(), 2);
    }

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
