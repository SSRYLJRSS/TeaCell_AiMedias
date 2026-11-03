//! A3-03: selection snapshots used as plan/tool targets.
//!
//! The service layer owns the authority checks; the database only stores rows.
//! A reference is valid only for the principal that created it, the library and
//! epoch it was resolved against, and its expiry window.

use serde::Serialize;

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::assets::{self, Asset};
use crate::db::selections::{self, NewSelection, SELECTION_TTL_SECONDS};
use crate::error::{AppError, AppResult};
use crate::state::Database;

/// Upper bound for one result-view page. The library grid renders 1000 assets per
/// page elsewhere, so the handed-off set uses the same envelope.
pub const SELECTION_PAGE_MAX_ITEMS: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionView {
    pub selection_ref: ResourceRef,
    pub total: i64,
    pub truncated: bool,
}

/// A3-08: counts for a fixed snapshot without loading any asset rows.
///
/// The assistant panel and the library result view both need "how many of the
/// handed-off items are still usable" before rendering a grid, and the number
/// has to agree with what the grid then shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionSummary {
    pub selection_ref: ResourceRef,
    pub total: i64,
    pub truncated: bool,
    pub snapshot: i64,
    pub available: i64,
    pub missing: i64,
    /// A3-08：交接时记录的来源描述（「搜索「猫」的结果」等），由后端返回。
    ///
    /// 前端曾各自硬编码「助手会话」，于是同一份快照在助手卡片与结果视图里
    /// 显示成两种来源。来源是快照自身的事实，只能有一个出处。
    pub query: String,
}
///
/// The GUI shows this set in the library "current result" view and as the
/// handed-off collection in the assistant panel. `missing` counts snapshot
/// members that no longer resolve (deleted, moved outside the managed library,
/// or modified so the location evidence no longer matches) so the UI can say
/// how much of the original set is gone instead of silently shrinking.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionPage {
    pub selection_ref: ResourceRef,
    pub total: i64,
    pub truncated: bool,
    pub snapshot: i64,
    pub available: i64,
    pub missing: i64,
    /// 同 `SelectionSummary::query`：来源由后端给出，界面不得自己编。
    pub query: String,
    pub items: Vec<Asset>,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSelection {
    pub selection_ref: ResourceRef,
    pub query: String,
    pub asset_ids: Vec<i64>,
    pub total: i64,
    pub truncated: bool,
}

/// Persist the exact asset-id set one caller resolved.
///
/// `total` is the count the query would return; `asset_ids` may be shorter when
/// the snapshot hit the persistence cap, which is reported as `truncated`.
///
/// A3-08：落库前先确认这份 id 集合真的属于当前总库。以前这里只校验长度与
/// `total`，任何调用方传错的 id 都能落成快照，之后 `available/missing` 会把它
/// 报成「素材已被删除」——一条编造出来的历史。宁可在交接时报 INVALID_INPUT，
/// 也不要让结果视图对着不存在的 id 撒谎。
pub fn store(
    db: &Database,
    caller: &CallerContext,
    query: &str,
    asset_ids: &[i64],
    total: i64,
) -> AppResult<SelectionView> {
    if total < 0
        || asset_ids.len() > selections::MAX_SELECTION_ASSETS
        || total < asset_ids.len() as i64
    {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    // 重复 id 会让「快照项数」虚高，missing 计算随之失真。
    let mut unique: Vec<i64> = asset_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != asset_ids.len() {
        return Err(AppError::invalid_arg("交接的素材里有重复项，请重新选择"));
    }
    let truncated = total > asset_ids.len() as i64;
    let now = now_unix_seconds()?;
    let selection_ref = ResourceRef::generated("selection");
    {
        let conn = db.lock()?;
        // A3-08：`delete_expired` 此前全仓零调用，`agent_selections` 只增不删。
        // 写入是唯一天然的清理时机：同一把锁、同一次交接，顺手把过期行清掉，
        // 不必引入后台定时器（定时器还要额外处理退出与并发）。
        selections::delete_expired(&conn, now)?;
        let known = assets::count_known(&conn, asset_ids)? as i64;
        if known != asset_ids.len() as i64 {
            return Err(AppError::invalid_arg(
                "这次交接的素材已不在当前素材库中，请刷新后重新选择",
            ));
        }
        selections::insert(
            &conn,
            &NewSelection {
                selection_ref: selection_ref.as_str(),
                principal_id: caller.principal_id.as_str(),
                library_id: caller.library.library_id.as_str(),
                library_epoch: caller.library.library_epoch.as_str(),
                query,
                asset_ids,
                total,
                truncated,
                created_at: now,
                expires_at: now.saturating_add(SELECTION_TTL_SECONDS),
            },
        )?;
    }
    Ok(SelectionView {
        selection_ref,
        total,
        truncated,
    })
}

/// Load a selection as a usable target set, re-checking principal, epoch and expiry.
pub fn resolve(
    db: &Database,
    caller: &CallerContext,
    selection_ref: &ResourceRef,
) -> AppResult<ResolvedSelection> {
    let now = now_unix_seconds()?;
    let stored = {
        let conn = db.lock()?;
        selections::get(&conn, selection_ref.as_str())?
    };
    let Some(stored) = stored else {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    };
    if stored.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if stored.expires_at <= now {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    if stored.library_id != caller.library.library_id.as_str()
        || stored.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(ResolvedSelection {
        selection_ref: selection_ref.clone(),
        query: stored.query,
        asset_ids: stored.asset_ids,
        total: stored.total,
        truncated: stored.truncated,
    })
}

/// A3-08: counts for a fixed selection snapshot, without loading asset rows.
///
/// `missing` exists so the result view can say "3 of the 12 handed-off items are no
/// longer available" instead of silently rendering a shorter grid than the user
/// handed over.
pub fn summary(
    db: &Database,
    caller: &CallerContext,
    selection_ref: &ResourceRef,
) -> AppResult<SelectionSummary> {
    let resolved = resolve(db, caller, selection_ref)?;
    let available = {
        let conn = db.lock()?;
        assets::count_active(&conn, &resolved.asset_ids)? as i64
    };
    let snapshot = resolved.asset_ids.len() as i64;
    Ok(SelectionSummary {
        selection_ref: resolved.selection_ref,
        total: resolved.total,
        truncated: resolved.truncated,
        snapshot,
        available,
        missing: snapshot.saturating_sub(available),
        query: resolved.query,
    })
}

/// A3-08: read one page of a fixed selection snapshot for the library result view.
///
/// Only the trusted GUI renders this list, but the authority check is the same as
/// every other selection consumer: principal, library epoch and expiry are all
/// re-validated on each page so a stale reference can never page past its window.
///
/// `available` / `missing` always describe the whole snapshot, not just this
/// window: the header of the result view must not change its "how many are gone"
/// number as the user scrolls.
pub fn page(
    db: &Database,
    caller: &CallerContext,
    selection_ref: &ResourceRef,
    offset: u32,
    limit: u32,
) -> AppResult<SelectionPage> {
    if limit == 0 || limit > SELECTION_PAGE_MAX_ITEMS {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let resolved = resolve(db, caller, selection_ref)?;
    let start = offset as usize;
    if start > resolved.asset_ids.len() {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let end = (start + limit as usize).min(resolved.asset_ids.len());
    let window = &resolved.asset_ids[start..end];
    let (items, available) = {
        let conn = db.lock()?;
        // The window keeps the snapshot order; `by_ids_ordered_active` re-sorts by
        // the ids we pass and drops anything already trashed, so paging never
        // reshuffles relative to the snapshot and never renders deleted items.
        let items = assets::by_ids_ordered_active(&conn, window)?;
        let available = assets::count_active(&conn, &resolved.asset_ids)? as i64;
        (items, available)
    };
    let next_offset = if end < resolved.asset_ids.len() {
        u32::try_from(end).ok()
    } else {
        None
    };
    let snapshot = resolved.asset_ids.len() as i64;
    Ok(SelectionPage {
        selection_ref: resolved.selection_ref,
        total: resolved.total,
        truncated: resolved.truncated,
        snapshot,
        available,
        missing: snapshot.saturating_sub(available),
        query: resolved.query,
        items,
        next_offset,
    })
}

fn now_unix_seconds() -> AppResult<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))?;
    i64::try_from(now.as_secs()).map_err(|_| AppError::internal("系统时间超出范围"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_memory, libraries};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use crate::utils::file_identity::file_identity;
    use std::fs::File;
    use std::time::UNIX_EPOCH;

    /// Seed `names` into a fresh managed library so `VERIFIED_ASSET_SCOPE` matches.
    fn seeded_library(names: &[&str]) -> (tempfile::TempDir, Database, Vec<i64>) {
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
        (root, Database::new(conn), ids)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_selection_fixture".to_string(),
                instance_id: "instance_selection_fixture".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    #[test]
    fn page_keeps_snapshot_order_and_reports_the_next_offset() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg", "beta.jpg", "gamma.jpg"]);
        let caller = gui_caller(&db);
        // Store in an order that differs from insertion so ordering is provable.
        let order = vec![ids[2], ids[0], ids[1]];
        let view = store(&db, &caller, "fixture", &order, 3).unwrap();
        assert!(!view.truncated);

        let first = page(&db, &caller, &view.selection_ref, 0, 2).unwrap();
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.items[0].id, ids[2]);
        assert_eq!(first.items[1].id, ids[0]);
        assert_eq!(first.snapshot, 3);
        // Counts describe the whole snapshot, so they do not change while paging.
        assert_eq!(first.available, 3);
        assert_eq!(first.missing, 0);
        assert_eq!(first.next_offset, Some(2));

        let second = page(&db, &caller, &view.selection_ref, 2, 2).unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, ids[1]);
        assert_eq!(second.snapshot, 3);
        assert_eq!(second.available, 3);
        assert_eq!(second.next_offset, None);
    }

    #[test]
    fn summary_matches_page_counts_without_loading_rows() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg", "beta.jpg", "gamma.jpg"]);
        let caller = gui_caller(&db);
        let view = store(&db, &caller, "fixture", &ids, 3).unwrap();
        {
            let conn = db.lock().unwrap();
            crate::db::assets::soft_delete(&conn, &[ids[0]]).unwrap();
        }

        let summary = summary(&db, &caller, &view.selection_ref).unwrap();
        assert_eq!(summary.snapshot, 3);
        assert_eq!(summary.available, 2);
        assert_eq!(summary.missing, 1);

        // The grid header and the panel badge must agree with each other.
        let first = page(&db, &caller, &view.selection_ref, 0, 1).unwrap();
        assert_eq!(first.available, summary.available);
        assert_eq!(first.missing, summary.missing);
        assert_eq!(first.total, summary.total);
        assert_eq!(first.truncated, summary.truncated);
    }

    #[test]
    fn page_reports_missing_members_instead_of_silently_shrinking() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg", "beta.jpg"]);
        let caller = gui_caller(&db);
        let view = store(&db, &caller, "fixture", &ids, 2).unwrap();
        {
            let conn = db.lock().unwrap();
            crate::db::assets::soft_delete(&conn, &[ids[1]]).unwrap();
        }

        let page = page(&db, &caller, &view.selection_ref, 0, 10).unwrap();
        assert_eq!(page.snapshot, 2);
        assert_eq!(page.items.len(), 1, "the deleted id must not be rendered");
        assert_eq!(page.available, 1);
        assert_eq!(
            page.missing, 1,
            "the UI must be able to say one item is gone"
        );
    }

    #[test]
    fn store_rejects_ids_that_are_not_in_this_library() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg"]);
        let caller = gui_caller(&db);

        // 编造的 id：必须在这里失败，而不是落库后让结果视图把它报成「已被删除」。
        let error = store(&db, &caller, "fixture", &[ids[0], 987_654], 2).unwrap_err();
        assert_eq!(error_code(error), "INVALID_ARG");

        // 重复 id 会虚增快照项数，同样拒绝。
        let error = store(&db, &caller, "fixture", &[ids[0], ids[0]], 2).unwrap_err();
        assert_eq!(error_code(error), "INVALID_ARG");

        // 空集合是合法的：搜索无结果也要能返回一个空快照，不能因此报错。
        assert!(store(&db, &caller, "fixture", &[], 0).is_ok());
    }

    #[test]
    fn store_prunes_expired_selections_so_the_table_does_not_grow_forever() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg", "beta.jpg"]);
        let caller = gui_caller(&db);
        let first = store(&db, &caller, "fixture", &ids[..1], 1).unwrap();

        // 把第一条改成已过期，模拟一小时后再交接。
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE agent_selections SET expires_at = 1 WHERE selection_ref = ?1",
                rusqlite::params![first.selection_ref.as_str()],
            )
            .unwrap();
        }

        let second = store(&db, &caller, "fixture", &ids[1..], 1).unwrap();
        let remaining: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM agent_selections", [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(
            remaining, 1,
            "the expired row must be pruned on the next store"
        );
        assert_eq!(
            error_code(resolve(&db, &caller, &first.selection_ref).unwrap_err()),
            "REFERENCE_EXPIRED"
        );
        assert!(resolve(&db, &caller, &second.selection_ref).is_ok());
    }

    #[test]
    fn summary_and_page_carry_the_stored_query() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg"]);
        let caller = gui_caller(&db);
        let view = store(&db, &caller, "搜索「猫」的结果", &ids, 1).unwrap();

        // 来源是快照自己的事实：助手卡片与结果视图必须读到同一句话。
        assert_eq!(
            summary(&db, &caller, &view.selection_ref).unwrap().query,
            "搜索「猫」的结果"
        );
        assert_eq!(
            page(&db, &caller, &view.selection_ref, 0, 10)
                .unwrap()
                .query,
            "搜索「猫」的结果"
        );
    }

    #[test]
    fn page_rejects_another_principal_and_bad_ranges() {
        let (_root, db, ids) = seeded_library(&["alpha.jpg"]);
        let owner = gui_caller(&db);
        let view = store(&db, &owner, "fixture", &ids, 1).unwrap();

        let intruder = {
            let conn = db.lock().unwrap();
            authorization::build_caller_context(
                &conn,
                TrustedAdapterIdentity::Pi {
                    principal_id: "principal_pi_intruder".to_string(),
                    session_id: "session_selection_intruder".to_string(),
                    instance_id: "instance_selection_intruder".to_string(),
                },
                ResourceRef::generated("request"),
                Vec::new(),
            )
            .unwrap()
        };
        assert_eq!(
            error_code(page(&db, &intruder, &view.selection_ref, 0, 10).unwrap_err()),
            "UNAUTHORIZED"
        );
        assert_eq!(
            error_code(page(&db, &owner, &view.selection_ref, 0, 0).unwrap_err()),
            "INVALID_INPUT"
        );
        assert_eq!(
            error_code(page(&db, &owner, &view.selection_ref, 5, 10).unwrap_err()),
            "INVALID_INPUT"
        );
        assert_eq!(
            error_code(
                page(
                    &db,
                    &owner,
                    &ResourceRef::from_trusted("selection_missing_0000000000".to_string()).unwrap(),
                    0,
                    10,
                )
                .unwrap_err()
            ),
            "REFERENCE_EXPIRED"
        );
    }
}
