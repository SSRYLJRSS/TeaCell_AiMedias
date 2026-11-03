//! A4-04：永久清空与到期清理的归属核验。
//!
//! 本卡要关闭的不是「清空能删掉文件」，而是「删掉的确实是计划冻结的那个文件」。
//! 修复前的 `delete_target` 只核验叶子节点的文件身份，于是下面三种情况都能骗过它：
//!
//! 1. 父目录被换成指向总仓之外的 junction/符号链接。文件身份仍与核验证据一致
//!    （同一卷改名不改身份），但删除会顺着链接落到总仓之外。
//! 2. 素材记录的路径已经不在当前总仓内（换根、迁移残留、外部写入）。
//! 3. 总仓离线/未通过身份核验，此时根本没有可用的归属证据。
//!
//! 三种情况都必须报 `OWNERSHIP_UNVERIFIED` 并保留文件与数据库记录，且**两个入口
//! 用同一套判据**：可信界面的「永久清空」和宿主维护线程的到期清理共用
//! `trash::delete_target`，所以这里对两条链路分别断言。
//!
//! 创建不了 junction/符号链接时用例直接失败，不软跳过：本卡的必要证据就是链接。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;

use teacell_ai_media_manager_lib::contracts::agent::{
    OperationState, PrepareSelectionInput, ProtocolVersion, ResourceRef, SubmitPlanInput,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{
    self, assets, deletion_journal, libraries, operations as operations_db,
};
use teacell_ai_media_manager_lib::services::authorization::TrustedAdapterIdentity;
use teacell_ai_media_manager_lib::services::{
    agent_tools, authorization, managed_library, operation_dispatch, operations, plans, selections,
    trash,
};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;

const PI_HOST_PRINCIPAL: &str = "principal_pi_host";
const DAY_MS: i64 = 86_400_000;

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(serde_json::Value::String(value.to_string())).expect("测试引用应合法")
}

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_a4_purge_safety".to_string(),
            instance_id: "instance_a4_purge_safety".to_string(),
        },
        resource_ref("request_a4_purge_safety_gui"),
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
            session_id: "conversation_a4_purge_safety".to_string(),
            instance_id: "instance_a4_purge_safety_pi".to_string(),
        },
        resource_ref("request_a4_purge_safety_pi"),
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

/// 在 `link` 处建一个指向 `dest` 的目录链接。
///
/// Windows 用 junction（`mklink /J` 不需要管理员权限），其它平台用符号链接。
/// 建不出来就让用例失败：本卡的必要证据就是「父目录是链接」这一种布局，静默跳过
/// 等于没验证。
#[cfg(windows)]
fn link_dir(link: &Path, dest: &Path) {
    assert!(!link.exists(), "链接位置不能被占用：{}", link.display());
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(dest)
        .status()
        .expect("需要 cmd 才能创建 junction");
    assert!(
        status.success(),
        "junction 创建失败（{} → {}）；本用例不允许静默跳过",
        link.display(),
        dest.display()
    );
}

#[cfg(unix)]
fn link_dir(link: &Path, dest: &Path) {
    assert!(!link.exists(), "链接位置不能被占用：{}", link.display());
    std::os::unix::fs::symlink(dest, link).expect("需要符号链接能力；本用例不允许静默跳过");
}

fn seed_assets(conn: &Connection, root: &Path, dirs: &[(&str, &str)]) -> Vec<i64> {
    managed_library::adopt_initial_root(conn, root).expect("采纳初始根");
    let library = libraries::current(conn).unwrap().expect("总仓已建立");
    dirs.iter()
        .map(|(dir, name)| {
            let parent = if dir.is_empty() {
                root.to_path_buf()
            } else {
                let path = root.join(dir);
                std::fs::create_dir_all(&path).unwrap();
                path
            };
            let path = parent.join(name);
            std::fs::write(&path, format!("a4-04 purge safety {name}")).unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let modified_at = metadata
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let relative = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
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
                &relative,
                &identity,
                &"a".repeat(64),
                "a4-04-purge-safety",
            )
            .unwrap());
            id
        })
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    /// 总仓之外的落点：用来模拟「链接把删除目标带出总仓」。
    _outside: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    database: Arc<Database>,
    asset_ids: Vec<i64>,
}

impl Fixture {
    fn new(dirs: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().canonicalize().unwrap();
        let database = Arc::new(Database::new(
            db::init(&dir.path().join("library.db")).unwrap(),
        ));
        let asset_ids = {
            let conn = database.lock().unwrap();
            seed_assets(&conn, &root, dirs)
        };
        Self {
            _dir: dir,
            _outside: outside_dir,
            root,
            outside,
            database,
            asset_ids,
        }
    }

    fn library(&self) -> libraries::LibrarySnapshot {
        let conn = self.database.lock().unwrap();
        libraries::current(&conn).unwrap().expect("总仓存在")
    }

    fn trash_items(&self) {
        let conn = self.database.lock().unwrap();
        let moved = assets::soft_delete(&conn, &self.asset_ids).unwrap();
        assert_eq!(
            moved,
            self.asset_ids.len() as u64,
            "夹具应把全部素材移入回收站"
        );
    }

    /// 把 `deleted_at` 推到 40 天前，让到期清理认为它已超期。
    fn age_trash(&self, days: i64) {
        let conn = self.database.lock().unwrap();
        let stamp = now_millis() - days * DAY_MS;
        for id in &self.asset_ids {
            conn.execute("UPDATE assets SET deleted_at=?1 WHERE id=?2", [stamp, *id])
                .unwrap();
        }
    }

    fn record_count(&self, asset_id: i64) -> i64 {
        let conn = self.database.lock().unwrap();
        conn.query_row(
            "SELECT count(*) FROM assets WHERE id=?1",
            [asset_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// 把记录路径改写成新路径并重新核验，模拟「证据完整、但路径已经不在当前总仓内」。
    ///
    /// 不能只 `UPDATE assets.file_path`：`trg_managed_asset_location_changed` 会在
    /// 路径变化时把定位证据整体打成 `unknown` 并清掉库归属，那样素材根本进不了
    /// `VERIFIED_ASSET_SCOPE`，测的就不是归属复检而是「查不到」。
    fn retarget_record(&self, asset_id: i64, path: &Path) {
        let conn = self.database.lock().unwrap();
        let text = path.to_string_lossy().to_string();
        let rows = conn
            .execute(
                "UPDATE assets SET file_path=?1 WHERE id=?2",
                rusqlite::params![text, asset_id],
            )
            .unwrap();
        assert_eq!(rows, 1, "夹具必须真的改到素材记录");

        let library = libraries::current(&conn).unwrap().expect("总仓存在");
        let candidate = libraries::pending_page(&conn, 0, 100)
            .unwrap()
            .into_iter()
            .find(|item| item.asset_id == asset_id)
            .expect("改写路径后素材应回到待核验页");
        let identity = file_identity(&File::open(path).unwrap()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            libraries::record_verified(
                &conn,
                &library,
                &candidate,
                &name,
                &identity,
                &"a".repeat(64),
                "a4-04-purge-safety-retarget",
            )
            .unwrap(),
            "夹具必须把库外路径重新核验成 verified，否则测不到归属复检"
        );
    }

    fn journal_states(&self, scope_id: &str) -> Vec<String> {
        let conn = self.database.lock().unwrap();
        deletion_journal::entries(&conn, scope_id)
            .unwrap()
            .into_iter()
            .map(|entry| entry.state)
            .collect()
    }
}

/// 走完 `prepare_empty` → 批准 → `submit_empty`，返回已排队的 operation 与 step。
///
/// 永久清空只对可信界面开放（能力目录里 `trash.empty.prepare/submit` 的 `callers`
/// 都只有 `Gui`），所以这里三步都用 GUI 身份：这正是产品约束，不是测试取巧。
fn queue_empty(fixture: &Fixture, idempotency_key: &str) -> (String, String) {
    let gui = gui_caller(&fixture.database);
    let selection = selections::store(
        &fixture.database,
        &gui,
        "永久清空回收站",
        &fixture.asset_ids,
        fixture.asset_ids.len() as i64,
    )
    .expect("GUI 可以为自己存选择快照");
    let preview = agent_tools::prepare_empty(
        &fixture.database,
        &gui,
        &PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
        },
    )
    .expect("GUI 可以准备永久清空计划");
    plans::approve_plan(
        &fixture.database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .expect("GUI 可以批准永久清空计划");
    let submission = agent_tools::submit_empty(
        &fixture.database,
        &gui,
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
        assert_eq!(steps[0].capability, "trash.empty.submit");
        steps[0].step_id.clone()
    };
    (operation_id, step_id)
}

/// 直接跑清空服务（与 `operation_dispatch::run_empty` 同一入口）。
fn run_empty(fixture: &Fixture, operation_id: &str, step_id: &str) -> trash::DeletionSummary {
    let library = fixture.library();
    let caller = gui_caller(&fixture.database);
    let operation_ref = resource_ref(operation_id);
    let step_ref = resource_ref(step_id);
    let cancelled = AtomicBool::new(false);
    trash::empty(
        &fixture.database,
        &caller,
        &library,
        trash::EmptyExecution {
            operation_id,
            operation_ref: &operation_ref,
            step_ref: &step_ref,
            asset_ids: &fixture.asset_ids,
            cancelled: &cancelled,
        },
    )
    .unwrap()
}

fn run_expiry(fixture: &Fixture) -> trash::DeletionSummary {
    let library = fixture.library();
    let mut removed = Vec::new();
    let summary =
        trash::purge_expired(&fixture.database, &library, 30, &mut |id| removed.push(id)).unwrap();
    // 只有真正删掉的项才允许触发缩略图清理：不明项一律不进这个回调。
    assert_eq!(removed.len() as i64, summary.removed);
    summary
}

fn item_state(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    asset_id: i64,
) -> operations_db::OperationItemRecord {
    operations_db::operation_item(
        conn,
        operation_id,
        step_id,
        &trash::deletion_item_id(asset_id),
    )
    .unwrap()
    .expect("操作项应存在")
}

/// 父目录被换成指向总仓之外的 junction/链接后，「永久清空」不得跟过去删文件。
///
/// 夹具用的是**改名**（同一卷改名不改文件身份），所以叶子节点的身份核验会全部通过
/// —— 修复前这一项会被当成正常文件删掉，被删的其实是总仓外的文件。
#[test]
fn manual_empty_refuses_a_parent_link_that_points_outside_the_library() {
    let fixture = Fixture::new(&[("sub", "a.jpg")]);
    let staged = fixture.root.join("sub").join("a.jpg");
    // 先把文件搬到总仓之外（改名保身份），再把父目录换成指向那里的链接。
    let escaped = fixture.outside.join("a.jpg");
    std::fs::rename(&staged, &escaped).unwrap();
    std::fs::remove_dir(fixture.root.join("sub")).unwrap();
    link_dir(&fixture.root.join("sub"), &fixture.outside);
    fixture.trash_items();

    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_link");
    let summary = run_empty(&fixture, &operation_id, &step_id);

    assert_eq!(summary.removed, 0, "顺着父目录链接删文件是不能接受的");
    assert_eq!(summary.unknown, 1);
    assert_eq!(summary.failed, 0);
    assert!(escaped.exists(), "总仓之外的文件必须原样保留");
    assert_eq!(
        fixture.record_count(fixture.asset_ids[0]),
        1,
        "记录必须保留"
    );
    let conn = fixture.database.lock().unwrap();
    let item = item_state(&conn, &operation_id, &step_id, fixture.asset_ids[0]);
    assert_eq!(item.state, "outcome_unknown");
    assert_eq!(item.error_code.as_deref(), Some("OWNERSHIP_UNVERIFIED"));
}

/// 同一个判据必须在到期清理入口同样生效：维护线程不经过任何审批，不能更宽松。
#[test]
fn expiry_purge_refuses_a_parent_link_that_points_outside_the_library() {
    let fixture = Fixture::new(&[("sub", "a.jpg")]);
    let staged = fixture.root.join("sub").join("a.jpg");
    let escaped = fixture.outside.join("a.jpg");
    std::fs::rename(&staged, &escaped).unwrap();
    std::fs::remove_dir(fixture.root.join("sub")).unwrap();
    link_dir(&fixture.root.join("sub"), &fixture.outside);
    fixture.trash_items();
    fixture.age_trash(40);

    let summary = run_expiry(&fixture);

    assert_eq!(summary.removed, 0);
    assert_eq!(summary.unknown, 1);
    assert!(escaped.exists(), "总仓之外的文件必须原样保留");
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);
}

/// 记录里的路径已经不在当前总仓内时，两个入口都只能报告不明。
#[test]
fn a_record_outside_the_root_is_reported_by_both_entries() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    let escaped = fixture.outside.join("moved-out.jpg");
    std::fs::rename(fixture.root.join("a.jpg"), &escaped).unwrap();
    fixture.retarget_record(fixture.asset_ids[0], &escaped);
    fixture.trash_items();

    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_outside");
    let summary = run_empty(&fixture, &operation_id, &step_id);
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.unknown, 1);
    assert!(escaped.exists(), "库外文件绝不能被永久删除");
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);

    fixture.age_trash(40);
    let summary = run_expiry(&fixture);
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.unknown, 1);
    assert!(escaped.exists());
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);
}

/// 总仓离线时没有任何可用的归属证据：只报告，不删。
#[test]
fn an_offline_library_never_deletes_files_or_records() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    fixture.trash_items();
    // 计划必须在总仓还 ready 时排好：`selections::store` 自己就会拒绝离线的库，
    // 先离线就测不到「执行期才发现库掉了」这条真实故障窗口。
    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_offline");

    // 注意：`Database::lock()` 不可重入，先取快照再拿锁。
    let library = fixture.library();
    {
        let conn = fixture.database.lock().unwrap();
        libraries::set_health(&conn, &library, libraries::LibraryHealth::Offline).unwrap();
    }

    let summary = run_expiry(&fixture);
    assert_eq!(summary.removed, 0, "总仓离线时不能删除任何文件");
    // 到期清理的候选集本身就走 `VERIFIED_ASSET_SCOPE`；库不 ready 时这一项
    // 连候选都进不去，所以三个计数都是 0（既不是 unknown 也不是 skipped）。
    // 这条断言锁的是「离线时第一层就拦住」，比断言某一项是否为 unknown 更准。
    assert_eq!(summary.unknown, 0);
    assert_eq!(summary.skipped, 0);

    // 界面「永久清空」是另一条路：它不靠候选集的 SQL 预筛，会直接撞上
    // `validate_caller` 的库就绪校验，整批拒绝，同样一个字节都不删。
    let caller = gui_caller(&fixture.database);
    let operation_ref = resource_ref(&operation_id);
    let step_ref = resource_ref(&step_id);
    let cancelled = AtomicBool::new(false);
    let error = trash::empty(
        &fixture.database,
        &caller,
        &library,
        trash::EmptyExecution {
            operation_id: &operation_id,
            operation_ref: &operation_ref,
            step_ref: &step_ref,
            asset_ids: &fixture.asset_ids,
            cancelled: &cancelled,
        },
    )
    .expect_err("总仓离线时清空必须直接拒绝，不能逐项猜");
    assert_eq!(error.code(), "LIBRARY_OFFLINE");

    assert!(fixture.root.join("a.jpg").exists(), "离线时文件必须保留");
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);
}

/// 同名替换：身份对不上时只能报「文件已变」，不能删。
#[test]
fn a_same_name_replacement_is_never_deleted() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    fixture.trash_items();
    let path = fixture.root.join("a.jpg");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "a different file with the same name").unwrap();

    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_swapped");
    let summary = run_empty(&fixture, &operation_id, &step_id);
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.unknown, 1);
    assert!(path.exists(), "冒名文件不能被当成原素材删除");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a different file with the same name"
    );
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);
}

/// 对照组：路径与身份都对得上时必须真的删掉文件和记录，否则前面几条断言没有意义。
#[test]
fn a_verified_item_is_removed_and_committed() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    fixture.trash_items();
    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_ok");
    let summary = run_empty(&fixture, &operation_id, &step_id);

    assert_eq!(summary.removed, 1);
    assert_eq!(summary.unknown, 0);
    assert_eq!(summary.failed, 0);
    assert!(!fixture.root.join("a.jpg").exists());
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 0);
    assert_eq!(
        fixture.journal_states(&operation_id),
        vec![deletion_journal::STATE_COMMITTED.to_string()]
    );
    let conn = fixture.database.lock().unwrap();
    let item = item_state(&conn, &operation_id, &step_id, fixture.asset_ids[0]);
    assert_eq!(item.state, "succeeded");
    assert_eq!(item.effect_state, "removed");
}

/// 到期边界：只在回收站里待了一天的项，不能被 30 天的清理带走。
#[test]
fn expiry_leaves_items_that_are_not_yet_due() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    fixture.trash_items();
    fixture.age_trash(1);

    // 一天不到的项被 `list_expired_trash` 的 cutoff（`deleted_at < now-30d`）
    // 直接排除在候选集之外，所以三个计数都是 0 —— 它压根没进 `delete_target`。
    let summary = run_expiry(&fixture);
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.unknown, 0);
    assert_eq!(summary.skipped, 0);
    assert!(fixture.root.join("a.jpg").exists());
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);

    // 到期的同一项必须能被删掉，证明上面不是因为「永远删不动」才通过。
    fixture.age_trash(40);
    let summary = run_expiry(&fixture);
    assert_eq!(summary.removed, 1);
    assert!(!fixture.root.join("a.jpg").exists());
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 0);
}

/// 走完整 dispatcher 链路时判据一致：链接项被拦下，正常项被删除。
#[test]
fn dispatcher_entry_shares_the_same_ownership_rules() {
    let fixture = Fixture::new(&[("", "ok.jpg"), ("sub", "linked.jpg")]);
    let linked = fixture.root.join("sub").join("linked.jpg");
    let escaped = fixture.outside.join("linked.jpg");
    std::fs::rename(&linked, &escaped).unwrap();
    std::fs::remove_dir(fixture.root.join("sub")).unwrap();
    link_dir(&fixture.root.join("sub"), &fixture.outside);
    fixture.trash_items();

    let (operation_id, step_id) = queue_empty(&fixture, "idempotency_a4_purge_dispatch");
    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    let snapshot = operation_dispatch::start(
        &fixture.database,
        &resource_ref(&operation_id),
        &queue,
        &export_cancel,
        &|_progress| {},
    )
    .expect("已排队的清空应能启动");

    // 一项成功、一项不明：整次操作不能报成功。
    assert_eq!(snapshot.state, OperationState::OutcomeUnknown);
    assert!(!fixture.root.join("ok.jpg").exists(), "正常项应被删除");
    assert!(escaped.exists(), "链接项必须原样保留");
    let conn = fixture.database.lock().unwrap();
    let blocked = item_state(&conn, &operation_id, &step_id, fixture.asset_ids[1]);
    assert_eq!(blocked.state, "outcome_unknown");
    assert_eq!(blocked.error_code.as_deref(), Some("OWNERSHIP_UNVERIFIED"));
}

/// A4-04 的验收里还有一条：修复归属核验不等于放宽入口 ——
/// Agent 侧连「准备永久清空」都拿不到，普通计划确认也不能替代专用审批。
#[test]
fn an_agent_still_cannot_reach_the_permanent_wipe() {
    let fixture = Fixture::new(&[("", "a.jpg")]);
    fixture.trash_items();
    let pi = pi_caller(&fixture.database);
    let gui = gui_caller(&fixture.database);
    let selection = selections::store(
        &fixture.database,
        &gui,
        "永久清空回收站",
        &fixture.asset_ids,
        fixture.asset_ids.len() as i64,
    )
    .expect("GUI 可以为自己存选择快照");
    let selection_ref = resource_ref(selection.selection_ref.as_str());

    let error = agent_tools::prepare_empty(
        &fixture.database,
        &pi,
        &PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection_ref.clone(),
        },
    )
    .expect_err("Agent 不得准备永久清空");
    assert_eq!(error.code(), "UNAUTHORIZED");
    let error = agent_tools::submit_empty(
        &fixture.database,
        &pi,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: selection_ref.clone(),
            idempotency_key: resource_ref("idempotency_a4_purge_agent"),
        },
    )
    .expect_err("Agent 不得提交永久清空");
    assert_eq!(error.code(), "UNAUTHORIZED");

    // 什么都没被删。
    assert!(fixture.root.join("a.jpg").exists());
    assert_eq!(fixture.record_count(fixture.asset_ids[0]), 1);
}
