//! A6-04 / R20：没有配置 AI 也能纯手动完成导入，不被模型配置阻塞。
//!
//! 既有证据只停在「界面能看到设置、被文案告知」（`src/App.test.tsx`、
//! `src/components/assistant/AssistantWorkbench.test.tsx`），没有一条断言
//! 「无 AI 绑定下真跑完一次导入」。本用例走真实链路：
//!
//! 建库（ready 托管库，AI 连接/绑定为空）→ `prepare_import`（显式拖入路径）
//! → `import_submission::confirm`（GUI 受理）→ `import_dispatch::drain`（真实复制入库）
//! → 逐项成功收据 + 素材落库。
//!
//! 链路里任何一处把「缺少 AI 配置」当前置条件都会让这条路径返回错误；
//! 因此这里的逐环 `Ok` 与最终收据就是「不被模型配置阻塞」的证据。

use teacell_ai_media_manager_lib::contracts::agent::{OperationState, ResourceRef};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{self, libraries};
use teacell_ai_media_manager_lib::services::{
    authorization, import_dispatch, import_execute, import_prepare, import_submission,
    managed_library, operations, plans, thumbnail::ThumbnailService,
};
use teacell_ai_media_manager_lib::state::Database;

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(serde_json::Value::String(value.to_string())).expect("测试引用应合法")
}

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        authorization::TrustedAdapterIdentity::Gui {
            session_id: "session_a6_04_manual_import".to_string(),
            instance_id: "instance_a6_04_manual_import".to_string(),
        },
        resource_ref("request_a6_04_manual_import"),
        Vec::new(),
    )
    .unwrap()
}

fn scalar(db: &Database, sql: &str) -> i64 {
    db.lock()
        .unwrap()
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .unwrap()
}

/// 把路径规约成可比较形式：统一分隔符、去掉 Windows 扩展前缀、小写盘符。
///
/// 库内 `file_path` 是带 `//?/` 扩展前缀的正斜杠形式，而测试里的根目录来自
/// `canonicalize`，两者直接比较一定不相等，所以先规约再比。
fn comparable(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text
        .strip_prefix("//?/")
        .map(str::to_string)
        .unwrap_or(text);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {
            format!("{}{}", first.to_ascii_lowercase(), chars.as_str())
        }
        _ => text,
    }
}

/// 没有任何 AI 连接、也没有任何用途绑定。
fn assert_no_ai_configuration(db: &Database) {
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM ai_connections"),
        0,
        "前置条件：不应存在任何 AI 连接档案"
    );
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM ai_usage_bindings"),
        0,
        "前置条件：不应存在任何 AI 用途绑定"
    );
}

#[test]
fn manual_import_completes_end_to_end_without_any_ai_binding() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    let database = Database::new(db::init(&root_dir.path().join("library.db")).unwrap());
    {
        let conn = database.lock().unwrap();
        managed_library::adopt_initial_root(&conn, &root).unwrap();
        assert!(
            libraries::current(&conn).unwrap().is_some(),
            "托管库应当已建立"
        );
    }
    assert_no_ai_configuration(&database);

    let caller = gui_caller(&database);

    // 源目录在托管库之外：模拟用户手动拖入一批新素材。
    let source = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for index in 0..3u8 {
        let path = source.path().join(format!("manual_{index}.jpg"));
        image::RgbImage::from_pixel(16, 16, image::Rgb([index * 40, 20, 10]))
            .save(&path)
            .unwrap();
        paths.push(path.to_string_lossy().to_string());
    }

    // 1) 扫描/准备：没有 AI 配置也必须能给出可提交的导入清单。
    let plan = import_prepare::prepare_import(
        &database,
        &caller,
        import_prepare::PrepareImportRequest {
            source_ref: None,
            paths,
            collection: None,
            rename_pattern: None,
            base_plan_ref: None,
            excluded_item_refs: Vec::new(),
        },
    )
    .expect("无 AI 配置时也必须能准备导入计划");
    assert_eq!(plan.item_count, 3);
    assert_eq!(plan.ready_count, 3, "三个候选都应可导入");
    assert_eq!(plan.unsupported_count, 0);
    assert_eq!(plan.limited_count, 0);

    // 2) 受理：GUI 手动确认，不需要任何模型参与。
    let submission = import_submission::confirm(
        &database,
        &caller,
        &plan.plan_ref,
        &resource_ref("idempotency_a6_04_manual_import"),
    )
    .expect("无 AI 配置时也必须能受理导入");
    assert_eq!(submission.operation.state, OperationState::Queued);
    assert!(
        submission.operation.error_code.is_none(),
        "受理阶段不应报任何错误码：{:?}",
        submission.operation.error_code
    );
    let operation_ref = resource_ref(submission.operation.operation_id.as_str());

    // 3) 执行：队列排空，文件真的复制进托管库。
    let queue = operations::OperationQueue::default();
    let cache = tempfile::tempdir().unwrap();
    let thumbs = ThumbnailService::new(cache.path()).unwrap();
    let drained = import_dispatch::drain(&database, &queue, &thumbs, |_| {})
        .expect("无 AI 配置时导入队列必须能排空");
    assert_eq!(drained, 3, "一次排空应完成全部三个候选");

    // 4) 逐项成功收据：成功项数 == asset 落库数。
    let result =
        import_execute::import_result(&database, &caller, &operation_ref).expect("读回逐项结果");
    assert_eq!(result.imported, 3, "三个候选都应拿到 item_imported 收据");
    assert_eq!(result.failed, 0);
    assert!(
        result.failures.is_empty(),
        "不应有任何失败项：{:?}",
        result
            .failures
            .iter()
            .map(|f| (&f.display_name, &f.error_code))
            .collect::<Vec<_>>()
    );
    assert_eq!(result.duplicates, 0, "全新文件不应被判成重复");
    // 逐项回执：每个计划项各自有一条 `item_imported`，而不是一条汇总回执。
    let imported_item_receipts = {
        let conn = database.lock().unwrap();
        db::operations::receipts(
            &conn,
            operation_ref.as_str(),
            result.source_step_ref.as_str(),
        )
        .expect("读回 receipt 行")
        .into_iter()
        .filter(|receipt| receipt.event == "item_imported")
        .filter_map(|receipt| receipt.item_id)
        .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        imported_item_receipts.len(),
        3,
        "三个计划项都应各有独立的成功回执"
    );

    let snapshot = plans::get_operation(&database, &caller, &operation_ref).expect("读回操作");
    assert_eq!(snapshot.state, OperationState::Succeeded);
    assert!(
        snapshot.error_code.is_none(),
        "整条链路不应以任何错误码收尾（含「缺少 AI 配置」）：{:?}",
        snapshot.error_code
    );
    assert_eq!(snapshot.failed, 0);

    // 落库与文件效果一致：3 条素材、文件都在托管库内且真实存在。
    assert_eq!(
        scalar(&database, "SELECT COUNT(*) FROM assets"),
        3,
        "成功收据数必须与 asset 落库数一致"
    );
    let file_paths = {
        let conn = database.lock().unwrap();
        let mut statement = conn.prepare("SELECT file_path FROM assets").unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(file_paths.len(), 3);
    let root_prefix = comparable(&root);
    let mut landed_names = Vec::new();
    for path in &file_paths {
        let actual = comparable(std::path::Path::new(path));
        assert!(
            actual.starts_with(&root_prefix),
            "导入后的素材必须落在托管库内：{actual}"
        );
        assert!(
            std::fs::metadata(&actual).is_ok(),
            "素材文件应真实存在：{actual}"
        );
        landed_names.push(actual.rsplit('/').next().unwrap_or_default().to_string());
    }
    landed_names.sort();
    assert_eq!(
        landed_names,
        vec![
            "manual_0.jpg".to_string(),
            "manual_1.jpg".to_string(),
            "manual_2.jpg".to_string()
        ],
        "拖入的三张素材应原名落库"
    );

    // 5) 全程没有要求、也没有暗中创建任何 AI 配置。
    assert_no_ai_configuration(&database);
}
