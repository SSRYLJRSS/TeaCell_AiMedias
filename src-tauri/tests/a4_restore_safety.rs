//! A4-03：回收站恢复的原子性与重启对账口径。
//!
//! 本卡要关闭的不是「恢复能成功」，而是三个危险窗口都留下可核对的证据：
//!
//! 1. 清 `deleted_at` 与写成功收据必须同生共死。早先是两个事务：收据写失败时
//!    记录其实已经回到库中，操作项却停在 `pending`，重启对账把「已恢复」报成
//!    失败，用户重试又撞「已被其他操作处理」。这里让收据写入必然失败，断言恢复
//!    也被回滚——两者要么都成立，要么都不成立。
//! 2. 文件缺失、文件被别的入口先恢复，都必须报成可修复的逐项结果，且不得凭空
//!    清 `deleted_at`。
//! 3. 重启对账必须按 `deleted_at` 的真实取值分派：仍是时间戳记 `failed`（可原样
//!    重试）、已为 NULL 记 `succeeded`（不重复计账）、行已查不到记 `outcome_unknown`
//!    （不猜）。
//!
//! 断言全部走真实链路（`prepare_restore` → `approve_plan` → `submit_restore` →
//! `operation_dispatch::start`），与 GUI 用的是同一个执行器。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;

use teacell_ai_media_manager_lib::contracts::agent::{
    OperationState, PrepareSelectionInput, ProtocolVersion, ResourceRef, SubmitPlanInput,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{
    self, asset_tags, assets, libraries, operations as operations_db, plans as plans_db, tags,
};
use teacell_ai_media_manager_lib::services::authorization::TrustedAdapterIdentity;
use teacell_ai_media_manager_lib::services::{
    agent_tools, authorization, managed_library, operation_dispatch, operations, plans, selections,
    trash,
};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;

const PI_HOST_PRINCIPAL: &str = "principal_pi_host";

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(serde_json::Value::String(value.to_string())).expect("测试引用应合法")
}

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_a4_restore_safety".to_string(),
            instance_id: "instance_a4_restore_safety".to_string(),
        },
        resource_ref("request_a4_restore_safety_gui"),
        Vec::new(),
    )
    .unwrap()
}

fn pi_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Pi {
            principal_id: PI_HOST_PRINCIPAL.to_string(),
            session_id: "conversation_a4_restore_safety".to_string(),
            instance_id: "instance_a4_restore_safety_pi".to_string(),
        },
        resource_ref("request_a4_restore_safety_pi"),
        Vec::new(),
    )
    .unwrap()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn seed_assets(conn: &Connection, root: &Path, names: &[&str]) -> Vec<i64> {
    managed_library::adopt_initial_root(conn, root).expect("采纳初始根");
    let library = libraries::current(conn).unwrap().expect("总仓已建立");
    names
        .iter()
        .map(|name| {
            let path = root.join(name);
            std::fs::write(&path, format!("a4-03 restore safety {name}")).unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let modified_at = metadata
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let id = assets::insert(
                conn,
                path.to_string_lossy().as_ref(),
                name,
                "jpg",
                metadata.len() as i64,
                "image/jpeg",
                modified_at,
            )
            .unwrap();
            let candidate = libraries::pending_page(conn, 0, 100)
                .unwrap()
                .into_iter()
                .find(|item| item.asset_id == id)
                .expect("新素材应出现在待核验页");
            let identity = file_identity(&File::open(&path).unwrap()).unwrap();
            assert!(libraries::record_verified(
                conn,
                &library,
                &candidate,
                name,
                &identity,
                &"a".repeat(64),
                "a4-03-restore-safety",
            )
            .unwrap());
            id
        })
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    database: Arc<Database>,
    asset_ids: Vec<i64>,
}

impl Fixture {
    fn new(names: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let database = Arc::new(Database::new(
            db::init(&dir.path().join("library.db")).unwrap(),
        ));
        let asset_ids = {
            let conn = database.lock().unwrap();
            seed_assets(&conn, &root, names)
        };
        Self {
            _dir: dir,
            root,
            database,
            asset_ids,
        }
    }

    fn source(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// 把素材移进回收站（只有软删除，文件不动）。
    fn trash_items(&self) {
        let conn = self.database.lock().unwrap();
        let moved = assets::soft_delete(&conn, &self.asset_ids).unwrap();
        assert_eq!(
            moved,
            self.asset_ids.len() as u64,
            "夹具应把全部素材移入回收站"
        );
    }

    fn trashed_count(&self) -> u64 {
        let conn = self.database.lock().unwrap();
        assets::count_trashed(&conn, &self.asset_ids).unwrap()
    }

    fn deleted_at(&self, asset_id: i64) -> Option<i64> {
        let conn = self.database.lock().unwrap();
        conn.query_row(
            "SELECT deleted_at FROM assets WHERE id=?1",
            [asset_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .unwrap()
    }
}

/// 走完 `prepare_restore` → GUI 批准 → Pi 提交，返回已排队的 operation 与 step。
fn queue_restore(fixture: &Fixture, idempotency_key: &str) -> (String, String) {
    let pi = pi_caller(&fixture.database);
    let gui = gui_caller(&fixture.database);
    let selection = selections::store(
        &fixture.database,
        &pi,
        "从回收站恢复",
        &fixture.asset_ids,
        fixture.asset_ids.len() as i64,
    )
    .expect("Pi 可以为自己存选择快照");
    let preview = agent_tools::prepare_restore(
        &fixture.database,
        &pi,
        &PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
        },
    )
    .expect("Pi 可以准备恢复计划");
    plans::approve_plan(
        &fixture.database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .expect("GUI 可以批准 Pi 创建的计划");
    let submission = agent_tools::submit_restore(
        &fixture.database,
        &pi,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: resource_ref(preview.plan_id.as_str()),
            idempotency_key: resource_ref(idempotency_key),
        },
    )
    .expect("批准后可提交");
    assert_eq!(submission.operation.state, OperationState::Queued);
    let operation_id = submission.operation.operation_id.as_str().to_string();
    let step_id = {
        let conn = fixture.database.lock().unwrap();
        let steps = operations_db::steps(&conn, &operation_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].capability, "assets.restore.submit");
        steps[0].step_id.clone()
    };
    (operation_id, step_id)
}

fn start(fixture: &Fixture, operation_id: &str) -> Result<OperationState, String> {
    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    operation_dispatch::start(
        &fixture.database,
        &resource_ref(operation_id),
        &queue,
        &export_cancel,
        &|_progress| {},
    )
    .map(|snapshot| snapshot.state)
    .map_err(|error| format!("{error:?}"))
}

fn item_state(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    item_id: &str,
) -> operations_db::OperationItemRecord {
    operations_db::operation_item(conn, operation_id, step_id, item_id)
        .unwrap()
        .expect("操作项应存在")
}

fn operation_state(conn: &Connection, operation_id: &str) -> String {
    plans_db::operation(conn, operation_id)
        .unwrap()
        .expect("操作记录应存在")
        .state
}

fn receipt_events(conn: &Connection, operation_id: &str, step_id: &str) -> Vec<String> {
    operations_db::receipts(conn, operation_id, step_id)
        .unwrap()
        .into_iter()
        .filter(|receipt| receipt.item_id.is_some())
        .map(|receipt| receipt.event)
        .collect()
}

/// 收据写入必然失败时，清 `deleted_at` 必须一起回滚。
///
/// 这是本卡的核心危险窗口：两个事务时，记录会「已恢复但操作项没结算」，
/// 重启对账只能把它报成失败。把操作项行删掉即可让同事务里的收据写入报
/// `REFERENCE_EXPIRED`，从而只留下「全成功」或「全回滚」两种可能。
#[test]
fn restore_commit_rolls_back_when_its_receipt_cannot_be_written() {
    let fixture = Fixture::new(&["a.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = queue_restore(&fixture, "idempotency_a4_restore_atomic");
    let item_id = trash::restore_item_id(fixture.asset_ids[0]);

    // 删掉操作项：事务里的 `update_item` 一定影响 0 行。
    {
        let conn = fixture.database.lock().unwrap();
        conn.execute(
            "DELETE FROM operation_items WHERE operation_id=?1 AND step_id=?2",
            rusqlite::params![operation_id, step_id],
        )
        .unwrap();
    }

    let outcome = start(&fixture, &operation_id);
    assert!(outcome.is_err(), "收据写不进去时执行必须报错：{outcome:?}");

    assert!(fixture.source("a.jpg").exists(), "回滚不应碰文件系统");
    assert_eq!(
        fixture.trashed_count(),
        1,
        "收据没写成时记录必须仍留在回收站，不能出现「已恢复但没记账」"
    );
    assert!(
        fixture.deleted_at(fixture.asset_ids[0]).is_some(),
        "deleted_at 必须还是时间戳"
    );

    let conn = fixture.database.lock().unwrap();
    assert!(
        operations_db::operation_item(&conn, &operation_id, &step_id, &item_id)
            .unwrap()
            .is_none(),
        "夹具前提：操作项确实不在了"
    );
    assert!(
        receipt_events(&conn, &operation_id, &step_id).is_empty(),
        "回滚后不该留下任何逐项收据"
    );
    assert_eq!(
        operation_state(&conn, &operation_id),
        "failed",
        "中途硬失败要如实结算成 failed，不能停在 running"
    );
}

/// 成功恢复：素材 ID、标签都不变，收据落在同一事务里。
#[test]
fn successful_restore_keeps_identifier_tags_and_writes_one_receipt() {
    let fixture = Fixture::new(&["b.jpg", "c.jpg"]);
    let tag_id = {
        let conn = fixture.database.lock().unwrap();
        let tag = tags::create(&conn, "恢复标签", None).unwrap();
        asset_tags::assign(&conn, &fixture.asset_ids, &[tag.id], "manual").unwrap();
        tag.id
    };
    fixture.trash_items();
    let (operation_id, step_id) = queue_restore(&fixture, "idempotency_a4_restore_success");

    assert_eq!(
        start(&fixture, &operation_id),
        Ok(OperationState::Succeeded)
    );
    assert_eq!(fixture.trashed_count(), 0);
    assert_eq!(fixture.asset_ids.len(), 2, "恢复不是重新入库，不产生新 ID");

    let conn = fixture.database.lock().unwrap();
    for asset_id in &fixture.asset_ids {
        let item_id = trash::restore_item_id(*asset_id);
        let item = item_state(&conn, &operation_id, &step_id, &item_id);
        assert_eq!(item.state, "succeeded");
        assert_eq!(item.effect_state, "registered");
        assert!(item.error_code.is_none());
        // 元数据跟着原记录走：标签仍在同一个素材 ID 上。
        let kept = asset_tags::get_asset_tags(&conn, *asset_id)
            .unwrap()
            .into_iter()
            .map(|tag| tag.id)
            .collect::<Vec<_>>();
        assert_eq!(kept, vec![tag_id]);
    }
    let events = receipt_events(&conn, &operation_id, &step_id);
    assert_eq!(events, vec!["restored".to_string(), "restored".to_string()]);
}

/// 文件在批准后消失：报可修复异常，绝不凭空清 `deleted_at`。
#[test]
fn missing_file_is_reported_and_the_record_stays_in_trash() {
    let fixture = Fixture::new(&["d.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = queue_restore(&fixture, "idempotency_a4_restore_missing");
    std::fs::remove_file(fixture.source("d.jpg")).unwrap();

    assert_eq!(start(&fixture, &operation_id), Ok(OperationState::Failed));
    assert_eq!(fixture.trashed_count(), 1, "记录必须仍留在回收站");
    assert!(fixture.deleted_at(fixture.asset_ids[0]).is_some());

    let conn = fixture.database.lock().unwrap();
    let item = item_state(
        &conn,
        &operation_id,
        &step_id,
        &trash::restore_item_id(fixture.asset_ids[0]),
    );
    assert_eq!(item.state, "failed");
    assert_eq!(item.effect_state, "none");
    assert_eq!(item.error_code.as_deref(), Some("FILE_MISSING"));
}

/// 别的入口已经先恢复了：本次不重复计账，也不用假成功掩盖竞争。
#[test]
fn already_restored_by_another_actor_is_reported_not_double_counted() {
    let fixture = Fixture::new(&["e.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = queue_restore(&fixture, "idempotency_a4_restore_race");

    // 模拟另一个入口在排队之后、执行之前完成了恢复。
    {
        let conn = fixture.database.lock().unwrap();
        assert_eq!(assets::restore(&conn, &fixture.asset_ids).unwrap(), 1);
    }

    assert_eq!(start(&fixture, &operation_id), Ok(OperationState::Failed));
    assert!(fixture.source("e.jpg").exists());

    let conn = fixture.database.lock().unwrap();
    let item = item_state(
        &conn,
        &operation_id,
        &step_id,
        &trash::restore_item_id(fixture.asset_ids[0]),
    );
    assert_eq!(item.state, "failed");
    assert_eq!(item.error_code.as_deref(), Some("NOT_IN_TRASH"));
    assert!(
        receipt_events(&conn, &operation_id, &step_id)
            .iter()
            .all(|event| event == "restore_failed"),
        "被别的入口抢先时不能写成功收据"
    );
}

/// 手工造一个「上次恢复被打断」的操作：状态 interrupted、逐项停在 pending。
fn insert_interrupted_restore(fixture: &Fixture) -> (String, String) {
    let operation_id = "operation_restore_interrupt_fixture".to_string();
    let step_id = "step_restore_interrupt_fixture".to_string();
    let now = now_millis();
    let conn = fixture.database.lock().unwrap();
    let library = libraries::current(&conn).unwrap().expect("总仓已建立");
    plans_db::insert_operation(
        &conn,
        &plans_db::NewOperation {
            operation_id: operation_id.clone(),
            step_id: step_id.clone(),
            plan_id: "plan_restore_interrupt_fixture".to_string(),
            principal_id: "principal_gui_host".to_string(),
            capability: "assets.restore.submit".to_string(),
            idempotency_key: "idempotency_restore_interrupt_fixture".to_string(),
            library_id: library.library_id.clone(),
            library_epoch: library.library_epoch.clone(),
            state: "interrupted".to_string(),
            phase: "executing".to_string(),
            revision: 1,
            total: Some(fixture.asset_ids.len() as i64),
            completed: 0,
            failed: 0,
            created_at: now,
        },
    )
    .unwrap();
    operations_db::insert_step(
        &conn,
        &operations_db::NewOperationStep {
            operation_id: operation_id.clone(),
            step_id: step_id.clone(),
            ordinal: 0,
            capability: "assets.restore.submit".to_string(),
            total: Some(fixture.asset_ids.len() as i64),
            created_at: now,
        },
    )
    .unwrap();
    for (ordinal, asset_id) in fixture.asset_ids.iter().enumerate() {
        operations_db::insert_item(
            &conn,
            &operations_db::NewOperationItem {
                operation_id: operation_id.clone(),
                step_id: step_id.clone(),
                item_id: trash::restore_item_id(*asset_id),
                ordinal: ordinal as i64,
                input_fingerprint: None,
                target_ref: None,
                created_at: now,
            },
        )
        .unwrap();
    }
    (operation_id, step_id)
}

/// 恢复已提交、收据没写成：对账要认出来记 `succeeded`，不能报成失败。
#[test]
fn reconcile_recognizes_a_restore_that_committed_without_its_receipt() {
    let fixture = Fixture::new(&["f.jpg", "g.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = insert_interrupted_restore(&fixture);
    // `deleted_at` 已被清空，但操作项停在 pending：这正是旧实现留下的现场。
    {
        let conn = fixture.database.lock().unwrap();
        assert_eq!(assets::restore(&conn, &fixture.asset_ids).unwrap(), 2);
    }

    let reconciled = trash::reconcile_interrupted_restores(&fixture.database).unwrap();
    assert_eq!(reconciled, vec![operation_id.clone()]);

    let conn = fixture.database.lock().unwrap();
    for asset_id in &fixture.asset_ids {
        let item = item_state(
            &conn,
            &operation_id,
            &step_id,
            &trash::restore_item_id(*asset_id),
        );
        assert_eq!(item.state, "succeeded", "已回到库中就必须记成功");
        assert_eq!(item.effect_state, "registered");
        assert!(item.error_code.is_none());
    }
    let events = receipt_events(&conn, &operation_id, &step_id);
    assert!(
        events
            .iter()
            .all(|event| event == "restore_item_completed_after_restart"),
        "{events:?}"
    );
    assert_eq!(operation_state(&conn, &operation_id), "succeeded");
    // 不允许出现第二份活跃记录。
    assert_eq!(
        conn.query_row("SELECT count(*) FROM assets", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

/// 记录仍在回收站：对账记 `failed`，用户可以原样重试。
#[test]
fn reconcile_keeps_a_still_trashed_record_as_retryable_failure() {
    let fixture = Fixture::new(&["h.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = insert_interrupted_restore(&fixture);

    let reconciled = trash::reconcile_interrupted_restores(&fixture.database).unwrap();
    assert_eq!(reconciled, vec![operation_id.clone()]);
    assert_eq!(
        fixture.trashed_count(),
        1,
        "对账不能顺手把记录恢复，它只负责如实记账"
    );

    let conn = fixture.database.lock().unwrap();
    let item = item_state(
        &conn,
        &operation_id,
        &step_id,
        &trash::restore_item_id(fixture.asset_ids[0]),
    );
    assert_eq!(item.state, "failed");
    assert_eq!(item.effect_state, "none");
    assert_eq!(item.error_code.as_deref(), Some("INTERRUPTED"));
    assert_eq!(operation_state(&conn, &operation_id), "failed");
}

/// 素材行已经不在了：读不到 `deleted_at`，只能记 `outcome_unknown`。
#[test]
fn reconcile_reports_unknown_when_the_asset_row_is_gone() {
    let fixture = Fixture::new(&["i.jpg"]);
    fixture.trash_items();
    let (operation_id, step_id) = insert_interrupted_restore(&fixture);
    {
        let conn = fixture.database.lock().unwrap();
        assets::delete(&conn, &fixture.asset_ids).unwrap();
    }

    let reconciled = trash::reconcile_interrupted_restores(&fixture.database).unwrap();
    assert_eq!(reconciled, vec![operation_id.clone()]);

    let conn = fixture.database.lock().unwrap();
    let item = item_state(
        &conn,
        &operation_id,
        &step_id,
        &trash::restore_item_id(fixture.asset_ids[0]),
    );
    assert_eq!(item.state, "outcome_unknown");
    assert_eq!(item.effect_state, "unknown");
    assert_eq!(item.error_code.as_deref(), Some("INTERRUPTED"));
    assert_eq!(
        operation_state(&conn, &operation_id),
        "outcome_unknown",
        "无法证实的效果必须落 unknown，不能压成 failed"
    );
}
