//! A4-02：移动的源/目标身份证据，以及「效果不可证实」时的收敛口径。
//!
//! 关闭 A4-02 需要证明的不是「移动能成功」，而是四类危险窗口都按同一判据处理：
//!
//! 1. 批准之后源文件被替换（`rename` 换掉、或删除后重建同名文件）：必须拒绝，且
//!    不能把新文件当成已批准的那个对象搬走；
//! 2. 批准之后只改了 mtime：既不能因此误杀合法移动，也绝不允许「唯一真实位置」
//!    说不清——文件必须恰好存在于一处；
//! 3. `rename` 已完成但数据库写入失败：不能宣称成功，也不能退化成普通失败，
//!    必须落到 `outcome_unknown` 并把 journal 停在 `moved` 留给对账；
//! 4. 恢复前目标文件被替换：对账必须拒绝认领，保持 `outcome_unknown`，
//!    而不是按旧 journal 把错误的对象登记为已核验素材。
//!
//! 另有第五条：同幂等键重试只回放原操作，不能再造一份活跃记录。
//!
//! 这些断言都走真实链路（`prepare_move` → `approve_plan` → `submit_move` →
//! `operation_dispatch::start`），与 GUI 用的是同一个执行器。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use teacell_ai_media_manager_lib::contracts::agent::{
    OperationState, PrepareMoveInput, ProtocolVersion, ResourceRef, SubmitPlanInput,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{
    self, assets, collections, libraries, move_journal, operations as operations_db,
    plans as plans_db,
};
use teacell_ai_media_manager_lib::services::authorization::TrustedAdapterIdentity;
use teacell_ai_media_manager_lib::services::{
    agent_tools, authorization, library_move, managed_library, operation_dispatch, operations,
    plans, selections,
};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;
use teacell_ai_media_manager_lib::utils::hash::sha256_full;

/// Agent 主机的固定 principal，与 `agent_runtime::capabilities` 内部常量同值。
const PI_HOST_PRINCIPAL: &str = "principal_pi_host";

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(serde_json::Value::String(value.to_string())).expect("测试引用应合法")
}

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_a4_move_safety".to_string(),
            instance_id: "instance_a4_move_safety".to_string(),
        },
        resource_ref("request_a4_move_safety_gui"),
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
            session_id: "conversation_a4_move_safety".to_string(),
            instance_id: "instance_a4_move_safety_pi".to_string(),
        },
        resource_ref("request_a4_move_safety_pi"),
        Vec::new(),
    )
    .unwrap()
}

fn seed_assets(conn: &Connection, root: &Path, names: &[&str]) -> Vec<i64> {
    managed_library::adopt_initial_root(conn, root).expect("采纳初始根");
    let library = libraries::current(conn).unwrap().expect("总仓已建立");
    names
        .iter()
        .map(|name| {
            let path = root.join(name);
            std::fs::write(&path, format!("a4-02 move safety {name}")).unwrap();
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
                // 刻意写入占位摘要：任何「沿用旧摘要」的实现都能被这里抓到。
                &"a".repeat(64),
                "a4-02-move-safety",
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
    collection_ref: String,
}

impl Fixture {
    fn new(names: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let database = Arc::new(Database::new(
            db::init(&dir.path().join("library.db")).unwrap(),
        ));
        let (asset_ids, collection_ref) = {
            let conn = database.lock().unwrap();
            let ids = seed_assets(&conn, &root, names);
            let library = libraries::current(&conn).unwrap().expect("总仓已建立");
            let stored = collections::ensure(
                &conn,
                &library.library_id,
                &library.library_epoch,
                "分库一",
                1,
            )
            .unwrap();
            (ids, stored.collection_ref.as_str().to_string())
        };
        Self {
            _dir: dir,
            root,
            database,
            asset_ids,
            collection_ref,
        }
    }

    fn source(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn target(&self, name: &str) -> PathBuf {
        self.root.join("分库一").join(name)
    }

    fn item_id(&self, index: usize) -> String {
        library_move::move_item_id(self.asset_ids[index])
    }
}

/// 走完 `prepare_move` → GUI 批准 → Pi 提交，返回已排队的 operation_ref。
fn queue_move(fixture: &Fixture, idempotency_key: &str) -> String {
    let pi = pi_caller(&fixture.database);
    let gui = gui_caller(&fixture.database);
    let selection = selections::store(
        &fixture.database,
        &pi,
        "移动到分库",
        &fixture.asset_ids,
        fixture.asset_ids.len() as i64,
    )
    .expect("Pi 可以为自己存选择快照");
    let preview = agent_tools::prepare_move(
        &fixture.database,
        &pi,
        &PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
            collection_ref: resource_ref(&fixture.collection_ref),
        },
    )
    .expect("Pi 可以准备移动计划");
    plans::approve_plan(
        &fixture.database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .expect("GUI 可以批准 Pi 创建的计划");
    let submission = agent_tools::submit_move(
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
    submission.operation.operation_id.as_str().to_string()
}

fn start(fixture: &Fixture, operation_id: &str) -> OperationState {
    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    let snapshot = operation_dispatch::start(
        &fixture.database,
        &resource_ref(operation_id),
        &queue,
        &export_cancel,
        &|_progress| {},
    )
    .expect("排队中的操作应能开始执行");
    snapshot.state
}

fn item_state(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
) -> operations_db::OperationItemRecord {
    let step = operations_db::steps(conn, operation_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("操作应有步骤")
        .step_id;
    operations_db::operation_item(conn, operation_id, &step, item_id)
        .unwrap()
        .expect("操作项应存在")
}

fn assets_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM assets", [], |row| row.get(0))
        .unwrap()
}

fn journal(conn: &Connection, operation_id: &str, item_id: &str) -> move_journal::MoveJournalEntry {
    move_journal::entry(conn, operation_id, item_id)
        .unwrap()
        .expect("移动日志应存在")
}

/// 批准之后源文件被换成另一个真实文件：必须拒绝移动，新文件不得被认领。
#[test]
fn replaced_source_after_approval_is_rejected_and_left_in_place() {
    let fixture = Fixture::new(&["a.jpg"]);
    let operation_id = queue_move(&fixture, "idempotency_a4_move_replaced_source");
    let source = fixture.source("a.jpg");
    let target = fixture.target("a.jpg");

    // 用「删除 + 重建同名文件」制造身份差异：这既模拟了用户换文件，
    // 也覆盖了「路径看起来没变、内容早已不是批准时那个」的最危险窗口。
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, "a totally different file").unwrap();

    assert_eq!(start(&fixture, &operation_id), OperationState::Failed);

    assert!(source.exists(), "被拒的源文件必须原样留在原位置");
    assert!(!target.exists(), "拒绝移动时绝不能产生目标文件");

    let conn = fixture.database.lock().unwrap();
    let entry = journal(&conn, &operation_id, &fixture.item_id(0));
    assert_eq!(entry.state, move_journal::STATE_FAILED);
    assert_eq!(entry.error_code.as_deref(), Some("SOURCE_CHANGED"));

    let item = item_state(&conn, &operation_id, &fixture.item_id(0));
    assert_eq!(item.state, "failed");
    assert_eq!(item.effect_state, "none");
    assert_eq!(item.error_code.as_deref(), Some("SOURCE_CHANGED"));

    // 素材记录仍指向原路径，也没有第二份记录替新文件站台。
    let managed = assets::get_managed(&conn, fixture.asset_ids[0]).unwrap();
    assert_eq!(managed.file_path, source.to_string_lossy());
    assert_eq!(assets_count(&conn), 1);
}

/// 批准之后只改了 mtime：不能因此丢掉文件，也不能两边都不在。
///
/// 两种平台的行为不同但都合规：Windows 的文件身份不含 mtime，移动照常进行；
/// Unix 的 `ctime` 同样会被 `utimensat` 刷新，于是身份复验按「源可能被替换」拒绝。
/// 这里断言的是不变量——文件恰好存在于一处，且结果与日志口径一致。
#[test]
fn mtime_only_change_keeps_exactly_one_true_location() {
    let fixture = Fixture::new(&["b.jpg"]);
    let operation_id = queue_move(&fixture, "idempotency_a4_move_mtime_only");
    let source = fixture.source("b.jpg");
    let target = fixture.target("b.jpg");

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(3_600))
        .unwrap();
    drop(file);
    assert!(source.exists());

    let state = start(&fixture, &operation_id);
    let source_exists = source.exists();
    let target_exists = target.exists();
    assert!(
        source_exists ^ target_exists,
        "文件必须且只能存在于一处（源={source_exists}，目标={target_exists}）"
    );

    let conn = fixture.database.lock().unwrap();
    let entry = journal(&conn, &operation_id, &fixture.item_id(0));
    match state {
        OperationState::Succeeded => {
            assert!(!source_exists && target_exists);
            assert_eq!(entry.state, move_journal::STATE_COMMITTED);
        }
        OperationState::Failed => {
            assert!(source_exists && !target_exists);
            assert_eq!(entry.state, move_journal::STATE_FAILED);
            assert_eq!(entry.error_code.as_deref(), Some("SOURCE_CHANGED"));
        }
        other => panic!("只改 mtime 不该产生第三种结果：{other:?}"),
    }
    assert_eq!(assets_count(&conn), 1, "任何分支都不允许出现第二份活跃记录");
}

/// `rename` 已成功、但数据库写入失败：落 `outcome_unknown`，journal 停在 moved。
#[test]
fn database_failure_after_rename_is_reported_as_outcome_unknown() {
    let fixture = Fixture::new(&["c.jpg"]);
    let operation_id = queue_move(&fixture, "idempotency_a4_move_db_failure");
    let source = fixture.source("c.jpg");
    let target = fixture.target("c.jpg");

    // 让事务里的路径更新必然失败：素材记录被移除后 `update_file_location`
    // 会报 not_found。文件效果本身没有变化，正是要覆盖的「已搬走但记录没跟上」。
    {
        let conn = fixture.database.lock().unwrap();
        assets::delete(&conn, &fixture.asset_ids).unwrap();
    }

    assert_eq!(
        start(&fixture, &operation_id),
        OperationState::OutcomeUnknown,
        "效果已发生但无法记账时必须落 unknown，而不是 failed/partially_succeeded"
    );

    assert!(!source.exists(), "rename 已经发生，源位置不该还有文件");
    assert!(target.exists(), "已经搬走的文件必须留在目标位置供对账");

    let conn = fixture.database.lock().unwrap();
    let entry = journal(&conn, &operation_id, &fixture.item_id(0));
    assert_eq!(
        entry.state,
        move_journal::STATE_MOVED,
        "journal 必须停在 moved，等对账决定归属"
    );
    assert!(entry.target_identity.is_some(), "移动成功时要留下目标身份");
    assert!(entry.target_modified_at.is_some());

    let item = item_state(&conn, &operation_id, &fixture.item_id(0));
    assert_eq!(item.state, "outcome_unknown");
    assert_eq!(item.effect_state, "unknown");
    assert_eq!(item.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));
}

/// 恢复前目标文件被替换：对账必须拒绝认领，保持 unknown 且不改素材路径。
#[test]
fn reconciliation_refuses_a_target_that_no_longer_matches_the_journal() {
    let fixture = Fixture::new(&["d.jpg", "e.jpg"]);
    let (source_d, target_d, source_e, target_e) = (
        fixture.source("d.jpg"),
        fixture.target("d.jpg"),
        fixture.source("e.jpg"),
        fixture.target("e.jpg"),
    );
    std::fs::create_dir_all(fixture.root.join("分库一")).unwrap();
    // d 已搬到目标，e 还没动；随后 d 的目标文件被换掉。
    std::fs::rename(&source_d, &target_d).unwrap();
    let recorded_identity = file_identity(&File::open(&target_d).unwrap()).unwrap();
    let recorded_modified = std::fs::metadata(&target_d)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let operation_id = "operation_a4_move_reconcile_fixture";
    let step_id = "step_a4_move_reconcile_fixture";
    let item_d = fixture.item_id(0);
    let item_e = fixture.item_id(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    {
        let conn = fixture.database.lock().unwrap();
        let library = libraries::current(&conn).unwrap().expect("总仓已建立");
        plans_db::insert_operation(
            &conn,
            &plans_db::NewOperation {
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                plan_id: "plan_a4_move_reconcile_fixture".to_string(),
                principal_id: "principal_gui_host".to_string(),
                capability: "assets.move.submit".to_string(),
                idempotency_key: "idempotency_a4_move_reconcile_fixture".to_string(),
                library_id: library.library_id.clone(),
                library_epoch: library.library_epoch.clone(),
                state: "interrupted".to_string(),
                phase: "executing".to_string(),
                revision: 1,
                total: Some(2),
                completed: 0,
                failed: 0,
                created_at: now,
            },
        )
        .unwrap();
        operations_db::insert_step(
            &conn,
            &operations_db::NewOperationStep {
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                ordinal: 0,
                capability: "assets.move.submit".to_string(),
                total: Some(2),
                created_at: now,
            },
        )
        .unwrap();
        for (ordinal, item_id) in [(0i64, &item_d), (1i64, &item_e)] {
            operations_db::insert_item(
                &conn,
                &operations_db::NewOperationItem {
                    operation_id: operation_id.to_string(),
                    step_id: step_id.to_string(),
                    item_id: item_id.clone(),
                    ordinal,
                    input_fingerprint: None,
                    target_ref: None,
                    created_at: now,
                },
            )
            .unwrap();
        }
        move_journal::record_intent(
            &conn,
            &move_journal::NewMoveIntent {
                operation_id,
                item_id: &item_d,
                asset_id: fixture.asset_ids[0],
                source_path: source_d.to_string_lossy().as_ref(),
                target_path: target_d.to_string_lossy().as_ref(),
                source_identity: "",
                updated_at: now,
            },
        )
        .unwrap();
        move_journal::record_moved(
            &conn,
            operation_id,
            &item_d,
            &recorded_identity,
            recorded_modified,
            now,
        )
        .unwrap();
        move_journal::record_intent(
            &conn,
            &move_journal::NewMoveIntent {
                operation_id,
                item_id: &item_e,
                asset_id: fixture.asset_ids[1],
                source_path: source_e.to_string_lossy().as_ref(),
                target_path: target_e.to_string_lossy().as_ref(),
                source_identity: "",
                updated_at: now,
            },
        )
        .unwrap();
    }

    // 现在把目标文件换成别的对象：路径没变，身份变了。
    std::fs::remove_file(&target_d).unwrap();
    std::fs::write(&target_d, "an impostor that took the approved path").unwrap();

    let reconciled = library_move::reconcile_interrupted_moves(&fixture.database).unwrap();
    assert_eq!(reconciled, vec![operation_id.to_string()]);

    let conn = fixture.database.lock().unwrap();
    let entry = journal(&conn, operation_id, &item_d);
    assert_eq!(
        entry.state,
        move_journal::STATE_MOVED,
        "拒绝认领时不能把 journal 推到 committed"
    );

    let item = item_state(&conn, operation_id, &item_d);
    assert_eq!(item.state, "outcome_unknown");
    assert_eq!(item.effect_state, "unknown");
    assert_eq!(item.error_code.as_deref(), Some("OUTCOME_UNKNOWN"));

    // 素材仍记在原路径：既不认领冒名文件，也不假装它已经就位。
    let managed = assets::get_managed(&conn, fixture.asset_ids[0]).unwrap();
    assert_eq!(managed.file_path, source_d.to_string_lossy());
    let replaced = std::fs::read_to_string(&target_d).unwrap();
    assert_eq!(replaced, "an impostor that took the approved path");
}

/// 同幂等键重试：只回放原操作，不新增 operation/step/item。
#[test]
fn retrying_with_the_same_key_replays_instead_of_creating_a_second_operation() {
    let fixture = Fixture::new(&["f.jpg"]);
    let pi = pi_caller(&fixture.database);
    let gui = gui_caller(&fixture.database);
    let selection = selections::store(
        &fixture.database,
        &pi,
        "移动到分库",
        &fixture.asset_ids,
        fixture.asset_ids.len() as i64,
    )
    .unwrap();
    let preview = agent_tools::prepare_move(
        &fixture.database,
        &pi,
        &PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
            collection_ref: resource_ref(&fixture.collection_ref),
        },
    )
    .unwrap();
    plans::approve_plan(
        &fixture.database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .unwrap();

    let input = SubmitPlanInput {
        schema_version: ProtocolVersion::V1,
        plan_id: resource_ref(preview.plan_id.as_str()),
        idempotency_key: resource_ref("idempotency_a4_move_replay"),
    };
    let first = agent_tools::submit_move(&fixture.database, &pi, &input).unwrap();
    let counts_before = {
        let conn = fixture.database.lock().unwrap();
        (
            conn.query_row("SELECT count(*) FROM operations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row("SELECT count(*) FROM operation_steps", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row("SELECT count(*) FROM operation_items", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        )
    };

    let second = agent_tools::submit_move(&fixture.database, &pi, &input)
        .expect("同幂等键重试必须回放而不是报错");
    assert!(!first.replayed);
    assert!(second.replayed, "第二次提交必须标记为回放");
    assert_eq!(
        second.operation.operation_id.as_str(),
        first.operation.operation_id.as_str(),
        "回放必须指向原操作"
    );

    let conn = fixture.database.lock().unwrap();
    let counts_after = (
        conn.query_row("SELECT count(*) FROM operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        conn.query_row("SELECT count(*) FROM operation_steps", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        conn.query_row("SELECT count(*) FROM operation_items", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
    );
    assert_eq!(counts_before, counts_after, "回放不得新增任何操作记录");
}

/// 成功移动后必须按目标真实内容重算摘要，不能沿用批准前的旧证据。
#[test]
fn successful_move_recomputes_digest_instead_of_reusing_previous_evidence() {
    let fixture = Fixture::new(&["g.jpg"]);
    let operation_id = queue_move(&fixture, "idempotency_a4_move_digest");
    assert_eq!(start(&fixture, &operation_id), OperationState::Succeeded);

    let conn = fixture.database.lock().unwrap();
    let managed = assets::get_managed(&conn, fixture.asset_ids[0]).unwrap();
    assert_eq!(managed.id, fixture.asset_ids[0], "移动后仍是同一个素材 ID");
    assert!(managed.file_path.contains("分库一"));

    let location = libraries::verified_location(&conn, fixture.asset_ids[0])
        .unwrap()
        .expect("移动后必须仍是已核验的托管素材");
    let expected = sha256_full(Path::new(&managed.file_path)).unwrap();
    assert_eq!(
        location.content_sha256, expected,
        "摘要必须按移动后的真实内容重算"
    );
    assert_ne!(
        location.content_sha256,
        "a".repeat(64),
        "绝不能沿用批准前的旧摘要"
    );
}
