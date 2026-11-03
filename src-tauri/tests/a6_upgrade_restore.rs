//! A6-02：数据库升级、备份恢复与中断注入的集成证据。
//!
//! 覆盖三类可复现证据：
//!   1. 受支持旧版本（user_version = 22、不含 A 阶段对象）数据库升级后数据保全且重复启动幂等；
//!   2. 文件效果已发生但收据写入回调中断时，不谎报成功；
//!   3. 备份恢复重算总仓身份与授权代次，旧调用者不能复活旧批准，且不复活明文凭据。
//!
//! 说明：`PRAGMA user_version` 在 2.0 全量迁移后仍为 22，A 阶段对象由不推进 user_version
//! 的幂等段创建。因此「1.0.x 旧库」最忠实的构造方式是：先建全量库、再 DROP 掉 A 阶段对象，
//! 使其与真实旧库形状一致，然后重新打开以验证幂等重建。已有数据列的 ALTER（如 tag_facets
//! 新增列）由 `has_column` 守卫，重跑是 no-op，这里不强行回退列。

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use serde_json::{json, Value};

use teacell_ai_media_manager_lib::contracts::agent::{
    BudgetSummary, CapabilityId, PlanScopeSummary, ResourceRef,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::export_items::NewExportItem;
use teacell_ai_media_manager_lib::db::{
    self, ai_connections, assets, conversations, export, export_items, libraries, move_journal,
    operations as operations_db, plans as plans_db,
};
use teacell_ai_media_manager_lib::services::authorization::TrustedAdapterIdentity;
use teacell_ai_media_manager_lib::services::{
    authorization, export_local, library_move, managed_library, plans,
};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;

/// A 阶段（post-22）创建的数据库对象：真实 1.0.x 旧库里不存在这些表。
/// 顺序无关（DROP 期间关闭外键约束）。
const POST_22_TABLES: &[&str] = &[
    "libraries",
    "managed_asset_locations",
    "authorization_principals",
    "directory_grants",
    "resource_handles",
    "plans",
    "plan_approvals",
    "operations",
    "operation_steps",
    "operation_items",
    "operation_receipts",
    "operation_child_tasks",
    "operation_recovery",
    "conversations",
    "conversation_drafts",
    "conversation_attachments",
    "conversation_links",
    "import_plans",
    "import_plan_items",
    "import_execution_journal",
    "legacy_asset_resolutions",
    "legacy_asset_resolution_schema",
    "tagging_batch_links",
    "agent_selections",
    "export_items",
    "move_journal",
    "library_collections",
    "deletion_journal",
    "library_migration_runs",
    "library_migration_items",
    "workflows",
    "workflow_versions",
    "workflow_instantiations",
    "mcp_connections",
    // 不推进 user_version 的其它幂等段（v23~v24）：旧库同样没有。
    // 注意 video_proxies 自 v14 起就存在，属于 pre-22，不能回退。
    "asset_palette_colors",
    "asset_facet_numbers",
];

/// 建到 assets 上的触发器不会随被 DROP 的表一起消失，必须显式清理。
const POST_22_TRIGGERS: &[&str] = &[
    "trg_managed_asset_insert",
    "trg_managed_asset_location_changed",
];

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(Value::String(value.to_string())).expect("资源引用应合法")
}

/// 把全量库回退成「受支持旧版本」形状：DROP A 阶段对象，保留 pre-22 表与数据。
fn make_legacy_database(path: &Path) -> Connection {
    let conn = db::init(path).expect("先建全量库");
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    for table in POST_22_TABLES {
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))
            .unwrap_or_else(|error| panic!("DROP {table} 失败: {error}"));
    }
    for trigger in POST_22_TRIGGERS {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))
            .unwrap();
    }
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.pragma_update(None, "user_version", 22).unwrap();
    assert_eq!(user_version(&conn), 22);
    conn
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn table_names(conn: &Connection) -> BTreeSet<String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn legacy_v22_database_upgrades_idempotently_and_keeps_user_data() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("library.db");

    // 1) 在旧版库里写入各类真实用户数据。
    {
        let conn = make_legacy_database(&db_path);
        assert_eq!(
            scalar_i64(
                &conn,
                "SELECT count(*) FROM sqlite_master WHERE name='libraries'"
            ),
            0,
            "旧库不应包含 A 阶段表"
        );
        let asset = assets::insert(
            &conn,
            "C:/legacy/old-photo.jpg",
            "old-photo.jpg",
            "jpg",
            1024,
            "image/jpeg",
            1_700_000_000_000,
        )
        .unwrap();
        // 回收站里的素材（pre-22 的 deleted_at 列自 v5 起存在）
        let trashed = assets::insert(
            &conn,
            "C:/legacy/trashed.jpg",
            "trashed.jpg",
            "jpg",
            10,
            "image/jpeg",
            1_700_000_000_000,
        )
        .unwrap();
        conn.execute(
            "UPDATE assets SET deleted_at=?2 WHERE id=?1",
            rusqlite::params![trashed, 1_700_000_100_000i64],
        )
        .unwrap();
        // 已有 AI 打标任务
        conn.execute(
            "INSERT INTO ai_batches(status,mode,total,processed,confirmed,created_at)
             VALUES('done','append',1,1,1,1)",
            [],
        )
        .unwrap();
        let batch_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO ai_suggestions(batch_id,asset_id,suggested_tags,status,confirmed_tags,created_at)
             VALUES(?1,?2,'[\"茶馆\"]','confirmed','[\"茶馆\"]',1)",
            rusqlite::params![batch_id, asset],
        )
        .unwrap();
        // 设置项
        conn.execute(
            "INSERT INTO settings(key,value) VALUES('library_root','\"C:/legacy\"')",
            [],
        )
        .unwrap();
        // 个人流程在旧库中还没有表 → 升级后应新建且为空
        assert_eq!(
            scalar_i64(
                &conn,
                "SELECT count(*) FROM sqlite_master WHERE name='workflows'"
            ),
            0
        );
    }

    // 2) 升级（第一次启动）。
    let conn = db::init(&db_path).expect("升级应成功");
    assert_eq!(user_version(&conn), 22, "升级不应推进 user_version");
    let tables_after_first = table_names(&conn);
    assert!(tables_after_first.contains("libraries"));
    assert!(tables_after_first.contains("workflows"));
    assert!(tables_after_first.contains("mcp_connections"));

    // 数据保全：素材、标签建议、设置都在，回收站条目仍标记为已删除。
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 2);
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT count(*) FROM assets WHERE deleted_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM ai_batches"), 1);
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM ai_suggestions"), 1);
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT count(*) FROM settings WHERE key='library_root'"
        ),
        1
    );
    // 新建的 A 阶段表为空（旧库不可能有数据，不能被凭空补数据）
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM workflows"), 0);
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM plans"), 0);
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM operations"), 0);
    // 老素材因为缺少托管核验证据，必须处于「待核验」而不是被静默信任。
    let pending = libraries::pending_page(&conn, 0, 100).unwrap();
    assert_eq!(pending.len(), 2, "旧素材应全部进入待核验队列");
    let snapshot = libraries::current(&conn).unwrap().expect("总仓行应已建立");
    assert_ne!(
        snapshot.health,
        libraries::LibraryHealth::Ready,
        "没有根身份证据时不能直接判定为 Ready"
    );
    drop(conn);

    // 3) 重复启动：幂等，对象与数据不变。
    let conn = db::init(&db_path).expect("第二次启动应成功");
    assert_eq!(user_version(&conn), 22);
    assert_eq!(
        table_names(&conn),
        tables_after_first,
        "重复启动不应新增或丢失表"
    );
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 2);
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM ai_batches"), 1);
}

#[test]
fn upgrading_an_empty_legacy_database_creates_agent_tables_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("library.db");
    {
        let conn = make_legacy_database(&db_path);
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 0);
    }
    let conn = db::init(&db_path).expect("空旧库升级应成功");
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 0);
    for table in [
        "libraries",
        "plans",
        "operations",
        "conversations",
        "workflows",
    ] {
        assert_eq!(
            scalar_i64(
                &conn,
                &format!(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                )
            ),
            1,
            "{table} 应被幂等迁移补建"
        );
    }
}

// ---------------------------------------------------------------------------
// 个人流程数据在升级/重复启动后应完整保留（不属于旧库回退对象）。
// ---------------------------------------------------------------------------

#[test]
fn personal_workflows_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("library.db");
    {
        let conn = db::init(&db_path).unwrap();
        let now = now_millis();
        db::workflows::create(
            &conn,
            "workflow_fixture",
            "我的常用流程",
            "说明",
            "[]",
            "[]",
            now,
        )
        .unwrap();
    }
    // 重启：流程与版本历史都还在，且版本未被重写。
    let conn = db::init(&db_path).unwrap();
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM workflows"), 1);
    assert_eq!(
        scalar_i64(&conn, "SELECT count(*) FROM workflow_versions"),
        1
    );
    let header = db::workflows::header(&conn, "workflow_fixture")
        .unwrap()
        .expect("流程头应存在");
    assert_eq!(header.name, "我的常用流程");
    assert_eq!(header.current_version, 1);
}

// ---------------------------------------------------------------------------
// 断点注入：文件效果已发生，但写收据（operation item）的回调中断。
// ---------------------------------------------------------------------------

fn seed_assets(conn: &Connection, root: &Path, names: &[&str]) -> Vec<i64> {
    managed_library::adopt_initial_root(conn, root).expect("采纳初始根");
    let library = libraries::current(conn).unwrap().expect("总仓已建立");
    names
        .iter()
        .map(|name| {
            let path = root.join(name);
            std::fs::write(&path, format!("a6-02 fixture {name}")).unwrap();
            let modified_at = std::fs::metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let size = std::fs::metadata(&path).unwrap().len() as i64;
            let id = assets::insert(
                conn,
                path.to_string_lossy().as_ref(),
                name,
                "jpg",
                size,
                "image/jpeg",
                modified_at,
            )
            .unwrap();
            let candidate = libraries::pending_page(conn, 0, 100)
                .unwrap()
                .into_iter()
                .find(|item| item.asset_id == id)
                .expect("新素材应出现在待核验页");
            let identity = file_identity(&std::fs::File::open(&path).unwrap()).unwrap();
            assert!(libraries::record_verified(
                conn,
                &library,
                &candidate,
                name,
                &identity,
                &"a".repeat(64),
                "a6-02-fixture",
            )
            .unwrap());
            id
        })
        .collect()
}

#[test]
fn export_receipt_failure_after_files_are_copied_never_reports_success() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();

    let conn = db::init(&data.path().join("library.db")).unwrap();
    let ids = seed_assets(&conn, root.path(), &["one.jpg", "two.jpg"]);
    let task = export::create_task(
        &conn,
        "local",
        ids.len() as i64,
        Some(dest.path().to_string_lossy().as_ref()),
        None,
    )
    .unwrap();
    let db = Arc::new(Database::new(conn));

    let cancel = Arc::new(AtomicBool::new(false));
    let mut calls = 0u32;
    // 第二项：文件已复制、导出收据已落库，但上层（operation item）收据写入失败。
    let mut on_item = |_: &NewExportItem| {
        calls += 1;
        if calls == 2 {
            return Err(teacell_ai_media_manager_lib::error::AppError::internal(
                "模拟收据写入中断",
            ));
        }
        Ok(())
    };

    let result = export_local::export_local(
        &db,
        export_local::ExportRun {
            task_id: task.id,
            asset_ids: &ids,
            dest_dir: dest.path().to_string_lossy().as_ref(),
            layout: "flat",
            cancel: &cancel,
        },
        |_progress| {},
        &mut on_item,
    );
    assert!(result.is_err(), "收据中断必须把错误传出去，不能吞掉");

    // 文件效果确实已经发生。
    let copied_files = std::fs::read_dir(dest.path()).unwrap().count();
    assert_eq!(copied_files, 2, "两个文件的复制效果都已落盘");
    let counts = export_items::counts(&db.lock().unwrap(), task.id).unwrap();
    assert_eq!(counts.copied, 2);

    // 用户可见结果不能被标成成功：任务必须落到终态 failed，并带上原因。
    //
    // A4-01 之前这里断言的是 "running"——`export_local` 直接在 `?` 上返回错误，
    // 从不调用 `finish_task`，任务永远停在 running，而没有任何执行者会推进它
    // （`db::export` 没有中断回收逻辑，`operations::reconcile_startup` 只处理
    // operations 表）。那不是「诚实地表示不确定」，是界面永远转圈的漏洞。
    // 现在由 `export_local` 包层在硬失败时补一次 finish_task("failed")：
    // 状态与原因都如实落库，文件效果仍然真实（两个文件已落盘、copied 为 2）。
    let task_after = export::get_task(&db.lock().unwrap(), task.id).unwrap();
    assert_eq!(
        task_after.status, "failed",
        "收据中断后任务必须落到终态 failed：不能显示成 done，也不能永远停在 running"
    );
    assert!(
        task_after.error.is_some(),
        "失败任务要写明原因，否则界面只能给出空错误"
    );
}

// ---------------------------------------------------------------------------
// 备份恢复：重算身份与授权代次，旧调用者不能复活批准，凭据边界不被突破。
// ---------------------------------------------------------------------------

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_a6_restore".to_string(),
            instance_id: "instance_a6_restore".to_string(),
        },
        resource_ref("request_a6_restore_fixture"),
        Vec::new(),
    )
    .unwrap()
}

fn draft_for(capability: CapabilityId) -> teacell_ai_media_manager_lib::services::plans::PlanDraft {
    teacell_ai_media_manager_lib::services::plans::PlanDraft {
        capability,
        scope: PlanScopeSummary {
            scope_ref: resource_ref("scope_a6_restore_fixture"),
            source_ref: None,
            selection_ref: None,
            target_ref: None,
            service_ref: None,
            service_revision: None,
            model: None,
        },
        item_count: 1,
        summary: serde_json::from_value(json!("移入回收站")).unwrap(),
        details_json: None,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 300,
            max_items: 4,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    }
}

#[test]
fn restore_rotates_identity_and_an_old_caller_cannot_plan_again() {
    let data = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();

    // 当前库（将被恢复覆盖）。
    let database = Database::new(db::init(&data.path().join("library.db")).unwrap());
    let old_snapshot = {
        let conn = database.lock().unwrap();
        libraries::current(&conn)
            .unwrap()
            .expect("当前库应有总仓行")
    };
    // 恢复前构造的调用者，持有旧代次的 library_epoch。
    let old_caller = gui_caller(&database);

    // 备份源：带一条素材、一个会话、一条关联、一条连接记录（只含 api_key_ref）。
    let backup_path = source_dir.path().join("snapshot.db");
    let backup_snapshot;
    {
        let conn = db::init(&source_dir.path().join("library.db")).unwrap();
        let now = now_millis();
        backup_snapshot = libraries::current(&conn)
            .unwrap()
            .expect("备份库应有总仓行");
        assets::insert(
            &conn,
            "C:/backup/kept.jpg",
            "kept.jpg",
            "jpg",
            7,
            "image/jpeg",
            now,
        )
        .unwrap();
        db::authorization::ensure_principal(&conn, "principal_gui_host", now).unwrap();
        conversations::insert_conversation(&conn, "conv_old", "principal_gui_host", "旧会话", now)
            .unwrap();
        conversations::link(&conn, "conv_old", "plan", "plan_legacy", now).unwrap();
        ai_connections::upsert(
            &conn,
            "conn_a",
            "本机模型",
            "local",
            "openai_chat",
            "http://127.0.0.1:9",
            "test-model",
            Some("conn_a"),
        )
        .unwrap();
        db::backup::backup_to(&conn, &backup_path).unwrap();
    }
    db::backup::validate_backup(&backup_path).expect("备份应通过校验");

    // 安装恢复。
    let prepared = teacell_ai_media_manager_lib::services::backup_restore::prepare_restore(
        data.path(),
        &backup_path,
    )
    .expect("暂存恢复应成功");
    {
        let maintenance = database.maintenance().unwrap();
        teacell_ai_media_manager_lib::services::backup_restore::install_prepared_restore(
            &maintenance,
            prepared,
        )
        .expect("安装恢复应成功");
    }

    // 恢复后：总仓身份重算（epoch 变化、根身份清空、健康度降级）。
    let new_snapshot = {
        let conn = database.lock().unwrap();
        libraries::current(&conn)
            .unwrap()
            .expect("恢复后总仓行应存在")
    };
    assert_eq!(
        new_snapshot.library_id, backup_snapshot.library_id,
        "恢复后应采用备份里的总仓 ID"
    );
    assert_ne!(
        new_snapshot.library_epoch, backup_snapshot.library_epoch,
        "恢复必须轮换授权代次，不能沿用备份中的旧代次"
    );
    assert_ne!(
        new_snapshot.library_epoch, old_snapshot.library_epoch,
        "恢复后的代次不能与恢复前相同"
    );
    assert!(
        new_snapshot.root_identity.is_none(),
        "恢复后不能沿用旧的根身份，必须先重新核验"
    );
    assert_ne!(new_snapshot.health, libraries::LibraryHealth::Ready);

    // 素材引用与会话关联在恢复后仍可读。
    {
        let conn = database.lock().unwrap();
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 1);
        let conversation = conversations::conversation(&conn, "conv_old")
            .unwrap()
            .expect("会话应随备份恢复");
        assert_eq!(conversation.title, "旧会话");
        let links = conversations::links(&conn, "conv_old").unwrap();
        assert!(links.iter().any(|link| link.link_ref == "plan_legacy"));
        // 凭据边界：连接只保留引用，不存明文密钥列。
        let api_key_ref: Option<String> = conn
            .query_row(
                "SELECT api_key_ref FROM ai_connections WHERE id='conn_a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(api_key_ref.as_deref(), Some("conn_a"));
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('ai_connections')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !columns.iter().any(|c| c.eq_ignore_ascii_case("api_key")
                || c.eq_ignore_ascii_case("secret")
                || c.eq_ignore_ascii_case("token")),
            "连接表不得出现明文凭据列: {columns:?}"
        );
    }

    // 旧调用者（持有恢复前的主体代次与 library_epoch）不能再准备新计划。
    // 恢复会同时重算总仓代次并让主体授权代次失效；判定顺序先查主体代次，
    // 所以这里通常先撞 GRANT_REVOKED。两者都等价于「旧批准不能被复活」。
    let error = plans::prepare_plan(
        &database,
        &old_caller,
        draft_for(CapabilityId::AssetsTrashSubmit),
    )
    .expect_err("旧代次调用者必须被拒绝");
    assert!(
        matches!(error.code(), "LIBRARY_CHANGED" | "GRANT_REVOKED"),
        "恢复后旧调用者必须以 LIBRARY_CHANGED 或 GRANT_REVOKED 被拒，实际: {} / {}",
        error.code(),
        error
    );
}

#[test]
fn restore_keeps_the_previous_database_as_a_valid_recovery_point() {
    let data = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();

    let database = Database::new(db::init(&data.path().join("library.db")).unwrap());
    let backup_path = source_dir.path().join("snapshot.db");
    {
        let conn = db::init(&source_dir.path().join("library.db")).unwrap();
        assets::insert(
            &conn,
            "C:/backup/kept.jpg",
            "kept.jpg",
            "jpg",
            7,
            "image/jpeg",
            now_millis(),
        )
        .unwrap();
        db::backup::backup_to(&conn, &backup_path).unwrap();
    }

    let prepared = teacell_ai_media_manager_lib::services::backup_restore::prepare_restore(
        data.path(),
        &backup_path,
    )
    .unwrap();
    {
        let maintenance = database.maintenance().unwrap();
        teacell_ai_media_manager_lib::services::backup_restore::install_prepared_restore(
            &maintenance,
            prepared,
        )
        .unwrap();
    }

    // 原库被保留为可恢复的 .old 文件，且它本身仍是合法备份（可再次恢复）。
    let old_path = data.path().join("library.db.old");
    assert!(old_path.is_file(), "恢复必须保留原库为撤销点");
    db::backup::validate_backup(&old_path).expect("保留文件仍应是可恢复的合法备份");
}

/// 中断注入（跨进程丢响应）：移动文件后、写入数据库前进程被杀，重启对账必须
/// 按磁盘真实状态收尾——已完成移动的项向前结算并改库，未发生的项回滚为待处理，
/// 绝不把「文件还在原位置」猜成成功。
#[test]
fn interrupted_move_is_reconciled_against_real_disk_state() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = Database::new(db::init(&data.path().join("library.db")).unwrap());

    let ids = {
        let conn = database.lock().unwrap();
        seed_assets(&conn, root.path(), &["moveA.jpg", "moveB.jpg"])
    };
    let library = {
        let conn = database.lock().unwrap();
        libraries::current(&conn).unwrap().expect("总仓已建立")
    };

    // 项 A：只登记了 intent，文件仍在原位置、目标不存在 → 应回滚为待处理。
    let source_a = root.path().join("moveA.jpg");
    let target_a = root.path().join("sub").join("moveA.jpg");
    // 项 B：文件已在目标、源已不在（模拟 rename 已完成但数据库未写）→ 应向前结算。
    let source_b = root.path().join("moveB.jpg");
    std::fs::create_dir_all(root.path().join("sub")).unwrap();
    let target_b = root.path().join("sub").join("movedB.jpg");
    std::fs::rename(&source_b, &target_b).unwrap();
    let target_b_modified = std::fs::metadata(&target_b)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let target_b_identity = file_identity(&std::fs::File::open(&target_b).unwrap()).unwrap();

    let operation_id = "operation_move_interrupt_fixture";
    let step_id = "step_move_interrupt_fixture";
    let item_a = library_move::move_item_id(ids[0]);
    let item_b = library_move::move_item_id(ids[1]);
    let now = now_millis();

    {
        let conn = database.lock().unwrap();
        plans_db::insert_operation(
            &conn,
            &plans_db::NewOperation {
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                plan_id: "plan_move_interrupt_fixture".to_string(),
                principal_id: "principal_gui_host".to_string(),
                capability: "assets.move.submit".to_string(),
                idempotency_key: "idempotency_move_interrupt_fixture".to_string(),
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
        for (ordinal, item_id) in [(0i64, &item_a), (1i64, &item_b)] {
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
                item_id: &item_a,
                asset_id: ids[0],
                source_path: source_a.to_string_lossy().as_ref(),
                target_path: target_a.to_string_lossy().as_ref(),
                source_identity: "",
                updated_at: now,
            },
        )
        .unwrap();
        move_journal::record_intent(
            &conn,
            &move_journal::NewMoveIntent {
                operation_id,
                item_id: &item_b,
                asset_id: ids[1],
                source_path: source_b.to_string_lossy().as_ref(),
                target_path: target_b.to_string_lossy().as_ref(),
                source_identity: "",
                updated_at: now,
            },
        )
        .unwrap();
        move_journal::record_moved(
            &conn,
            operation_id,
            &item_b,
            &target_b_identity,
            target_b_modified,
            now,
        )
        .unwrap();
    }

    let reconciled = library_move::reconcile_interrupted_moves(&database).unwrap();
    assert_eq!(reconciled, vec![operation_id.to_string()]);

    {
        let conn = database.lock().unwrap();
        let items = operations_db::items(&conn, operation_id, step_id).unwrap();
        let row_a = items
            .iter()
            .find(|item| item.item_id == item_a)
            .expect("A 项");
        assert_eq!(
            row_a.state, "pending",
            "文件仍在原位置只能回滚为待处理，不能猜成成功"
        );
        assert_eq!(row_a.effect_state, "none");
        let row_b = items
            .iter()
            .find(|item| item.item_id == item_b)
            .expect("B 项");
        assert_eq!(row_b.state, "succeeded", "目标文件已在，向前完成");
        assert_eq!(row_b.effect_state, "registered");

        assert_eq!(
            move_journal::entry(&conn, operation_id, &item_a)
                .unwrap()
                .unwrap()
                .state,
            move_journal::STATE_ROLLED_BACK
        );
        assert_eq!(
            move_journal::entry(&conn, operation_id, &item_b)
                .unwrap()
                .unwrap()
                .state,
            move_journal::STATE_COMMITTED
        );

        let events: Vec<String> = operations_db::receipts(&conn, operation_id, step_id)
            .unwrap()
            .into_iter()
            .map(|receipt| receipt.event)
            .collect();
        assert!(events.contains(&"move_item_reset_after_restart".to_string()));
        assert!(events.contains(&"move_item_committed_after_restart".to_string()));

        // 项 A 的原文件必须还在；项 B 的资产记录必须指向新位置且仍可核验。
        assert!(source_a.exists());
        let path_b: String = conn
            .query_row(
                "SELECT file_path FROM assets WHERE id=?1",
                [ids[1]],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            path_b.ends_with("movedB.jpg"),
            "数据库位置必须跟随已发生的文件移动: {path_b}"
        );
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM assets"), 2);
    }
}

/// 备份恢复不能让旧备份里「已批准、尚未消费」的计划重新变成可直接写入的凭证：
/// 恢复轮换总仓代次后，旧批准的实际提交必须被拒绝，且恢复库里不产生任何操作。
#[test]
fn restore_never_revives_a_directly_writable_approval() {
    let data = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let source_root = tempfile::tempdir().unwrap();

    let database = Database::new(db::init(&data.path().join("library.db")).unwrap());

    // 备份源：真实总仓 + 一条已批准的计划。
    let source_db = Database::new(db::init(&source_dir.path().join("library.db")).unwrap());
    {
        let conn = source_db.lock().unwrap();
        seed_assets(&conn, source_root.path(), &["appr.jpg"]);
    }
    let caller_at_backup = gui_caller(&source_db);
    let draft = draft_for(CapabilityId::AssetsTrashSubmit);
    let preview = plans::prepare_plan(&source_db, &caller_at_backup, draft.clone()).unwrap();
    plans::approve_plan(
        &source_db,
        &caller_at_backup,
        &preview.plan_id,
        &preview.review_ref,
    )
    .unwrap();
    let plan_key = preview.plan_id.as_str().to_string();

    let backup_path = source_dir.path().join("approved.db");
    {
        let conn = source_db.lock().unwrap();
        db::backup::backup_to(&conn, &backup_path).unwrap();
    }
    db::backup::validate_backup(&backup_path).expect("含已批准计划的备份应可校验");

    let prepared = teacell_ai_media_manager_lib::services::backup_restore::prepare_restore(
        data.path(),
        &backup_path,
    )
    .unwrap();
    {
        let maintenance = database.maintenance().unwrap();
        teacell_ai_media_manager_lib::services::backup_restore::install_prepared_restore(
            &maintenance,
            prepared,
        )
        .unwrap();
    }

    // 备份里的批准在恢复后仍作为历史记录存在（review_status 仍是 approved）。
    {
        let conn = database.lock().unwrap();
        let status: String = conn
            .query_row(
                "SELECT review_status FROM plans WHERE plan_id=?1",
                [&plan_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "approved", "旧批准记录本身随备份恢复");
    }

    // 但旧批准不能再被消费成写入操作：恢复轮换了 library_epoch，旧调用者被拒。
    let idem = resource_ref("idempotency_restore_approval_key");
    let error = plans::submit_plan(
        &database,
        &caller_at_backup,
        &preview.plan_id,
        &idem,
        &draft,
    )
    .expect_err("旧备份中的已批准计划不得直接复活为可写操作");
    assert_eq!(
        error.code(),
        "LIBRARY_CHANGED",
        "恢复后旧批准必须以 LIBRARY_CHANGED 被拒，实际: {error}"
    );
    {
        let conn = database.lock().unwrap();
        assert_eq!(
            scalar_i64(&conn, "SELECT count(*) FROM operations"),
            0,
            "被拒绝的旧批准不得产生任何操作记录"
        );
    }
}
