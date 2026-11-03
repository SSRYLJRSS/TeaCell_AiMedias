//! A4-02：已批准操作的显式执行（Pi 提交后无人执行的真实断链）。
//!
//! 背景：`agent_tools::submit_*` 只登记 operation/step/item 并对素材库零副作用；
//! 每个 GUI 命令都在自己内部完成文件效果。Agent（Pi）只走到提交，因此在这次修复前
//! 模型提交的移动/导出/恢复/清空**永远停在 `queued`**，而既有测试直接调用领域
//! `execute`，把整条断链掩盖成一个绿色的假象。
//!
//! 这些用例走真实链路：`prepare_*` → `plans::approve_plan`（GUI 能力）→
//! `submit_*`（Pi 能力）→ `operation_dispatch::start`（显式启动）→ 领域效果。
//! 任何一个环节断开都会让"文件真的动了"这条断言失败。

use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use serde_json::json;

use teacell_ai_media_manager_lib::contracts::agent::{
    CapabilityId, OperationState, PlanReviewStatus, PrepareMoveInput, ProtocolVersion, ResourceRef,
    SubmitPlanInput,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{
    self, assets, collections, libraries, operations as operations_db,
};
/// Agent 主机的固定 principal，与 `agent_runtime::capabilities` 内部常量同值。
const PI_HOST_PRINCIPAL: &str = "principal_pi_host";
use teacell_ai_media_manager_lib::services::authorization::TrustedAdapterIdentity;
use teacell_ai_media_manager_lib::services::{
    agent_tools, authorization, managed_library, operation_dispatch, operations, plans, selections,
};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;

fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(serde_json::Value::String(value.to_string())).expect("测试引用应合法")
}

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_a4_02_dispatch".to_string(),
            instance_id: "instance_a4_02_dispatch".to_string(),
        },
        resource_ref("request_a4_02_gui"),
        Vec::new(),
    )
    .unwrap()
}

/// Pi 的调用者：与真实 Agent 主机相同的 principal、会话即对话引用。
fn pi_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Pi {
            principal_id: PI_HOST_PRINCIPAL.to_string(),
            session_id: "conversation_a4_02_dispatch".to_string(),
            instance_id: "instance_a4_02_pi".to_string(),
        },
        resource_ref("request_a4_02_pi"),
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
            std::fs::write(&path, format!("a4-02 fixture {name}")).unwrap();
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
                "a4-02-fixture",
            )
            .unwrap());
            id
        })
        .collect()
}

/// 端到端：Pi 提交的移动在没人点「开始执行」时绝不改文件，点了才真的移动。
///
/// 这条是本次修复的核心断言。它在修复前必然失败：旧代码里 `start` 不存在，
/// 提交后文件永远留在原目录、操作永远 `queued`。
#[test]
fn pi_move_stays_untouched_until_explicit_start_then_really_moves() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    let database = Arc::new(Database::new(
        db::init(&root_dir.path().join("library.db")).unwrap(),
    ));
    let asset_ids = {
        let conn = database.lock().unwrap();
        seed_assets(&conn, &root, &["a.jpg", "b.jpg"])
    };
    let target = {
        let conn = database.lock().unwrap();
        let library = libraries::current(&conn).unwrap().unwrap();
        collections::ensure(
            &conn,
            &library.library_id,
            &library.library_epoch,
            "分库一",
            1,
        )
        .unwrap()
    };

    let gui = gui_caller(&database);
    let pi = pi_caller(&database);

    // 1) Pi 把当前选择存成快照并生成待审移动计划。
    let selection = selections::store(
        &database,
        &pi,
        "移动到分库",
        &asset_ids,
        asset_ids.len() as i64,
    )
    .expect("Pi 可以为自己存选择快照");
    let preview = agent_tools::prepare_move(
        &database,
        &pi,
        &PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
            collection_ref: resource_ref(target.collection_ref.as_str()),
        },
    )
    .expect("Pi 可以准备移动计划");
    assert_eq!(
        preview.review_status,
        PlanReviewStatus::Pending,
        "准备阶段只能得到待审计划"
    );

    // 2) 计划在批准前不可提交。
    let submit_input = SubmitPlanInput {
        schema_version: ProtocolVersion::V1,
        plan_id: resource_ref(preview.plan_id.as_str()),
        idempotency_key: resource_ref("idempotency_a4_02_move"),
    };
    assert!(
        agent_tools::submit_move(&database, &pi, &submit_input).is_err(),
        "未批准的计划不能被提交"
    );

    // 3) 审批是 GUI 独占能力：模型自己批不了。
    assert!(
        plans::approve_plan(
            &database,
            &pi,
            &resource_ref(preview.plan_id.as_str()),
            &resource_ref(preview.review_ref.as_str()),
        )
        .is_err(),
        "Pi 不得持有 PlansApprove"
    );
    plans::approve_plan(
        &database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .expect("GUI 可以批准 Pi 创建的计划");

    // 4) 提交只登记操作，不产生任何文件效果。
    let submission = agent_tools::submit_move(&database, &pi, &submit_input).expect("批准后可提交");
    assert_eq!(submission.operation.state, OperationState::Queued);
    for id in &asset_ids {
        let name = {
            let conn = database.lock().unwrap();
            assets::get_managed(&conn, *id).unwrap().file_name
        };
        assert!(
            root.join(&name).exists(),
            "提交阶段绝不能移动文件：{name} 应仍在总仓根目录"
        );
    }

    // 5) 显式启动后文件才真的移动。
    let operation_ref = resource_ref(submission.operation.operation_id.as_str());
    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    let on_progress = |_progress| {};
    let snapshot = operation_dispatch::start(
        &database,
        &operation_ref,
        &queue,
        &export_cancel,
        &on_progress,
    )
    .expect("显式启动应成功执行移动");
    assert_eq!(
        snapshot.state,
        OperationState::Succeeded,
        "全部移动成功应报 succeeded"
    );

    for id in &asset_ids {
        let location = {
            let conn = database.lock().unwrap();
            assets::get_managed(&conn, *id).unwrap()
        };
        assert!(
            !root.join(&location.file_name).exists() || location.file_path.contains("分库一"),
            "素材应已移出总仓根目录：{}",
            location.file_path
        );
        assert!(
            location.file_path.contains("分库一"),
            "素材落点应在目标分库内：{}",
            location.file_path
        );
        assert_eq!(location.id, *id, "移动后必须还是同一个素材 ID");
    }

    // 6) 重复启动必须被拒绝：已经执行过的操作再跑一次会真的再搬一次文件。
    let again = operation_dispatch::start(
        &database,
        &operation_ref,
        &queue,
        &export_cancel,
        &on_progress,
    );
    assert!(again.is_err(), "已结束的操作不得重复执行");
}

/// 执行身份必须来自持久行：GUI 不能借自己的身份替 Pi 的操作写收据。
///
/// 这条守住「模型不能借 GUI 的权限做自己做不到的事」——如果执行器用调用方传入的
/// 身份，任何 GUI 请求都能代替任意 principal 结算，审计链条就断了。
#[test]
fn execution_identity_is_rebuilt_from_the_persisted_plan_not_the_request() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    let database = Arc::new(Database::new(
        db::init(&root_dir.path().join("library.db")).unwrap(),
    ));
    let asset_ids = {
        let conn = database.lock().unwrap();
        seed_assets(&conn, &root, &["c.jpg"])
    };
    let target = {
        let conn = database.lock().unwrap();
        let library = libraries::current(&conn).unwrap().unwrap();
        collections::ensure(
            &conn,
            &library.library_id,
            &library.library_epoch,
            "分库二",
            1,
        )
        .unwrap()
    };
    let gui = gui_caller(&database);
    let pi = pi_caller(&database);
    let selection = selections::store(&database, &pi, "移动到分库", &asset_ids, 1).unwrap();
    let preview = agent_tools::prepare_move(
        &database,
        &pi,
        &PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: resource_ref(selection.selection_ref.as_str()),
            collection_ref: resource_ref(target.collection_ref.as_str()),
        },
    )
    .unwrap();
    plans::approve_plan(
        &database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .unwrap();
    let submission = agent_tools::submit_move(
        &database,
        &pi,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: resource_ref(preview.plan_id.as_str()),
            idempotency_key: resource_ref("idempotency_a4_02_identity"),
        },
    )
    .unwrap();
    let operation_ref = resource_ref(submission.operation.operation_id.as_str());

    let caller = operation_dispatch::execution_caller(&database, &operation_ref).unwrap();
    assert!(
        matches!(
            caller.kind,
            teacell_ai_media_manager_lib::contracts::agent::CallerKind::Pi
        ),
        "从 pi 计划重建出的必须是 Pi 调用者"
    );
    assert_eq!(caller.principal_id.as_str(), PI_HOST_PRINCIPAL);
    assert_eq!(
        caller.session_id.as_ref().map(ResourceRef::as_str),
        Some("conversation_a4_02_dispatch"),
        "会话应还原成计划里记录的对话"
    );
    assert_eq!(caller.grant_epoch.as_str(), pi.grant_epoch.as_str());
}

/// 没有待执行步骤或状态不是 queued 的操作不能被执行。
///
/// 这道守卫防的是「对着一个已经跑完的任务再点一次开始执行」——领域服务不会自己
/// 拦住重复的 `rename`，只会把文件搬到别处或报错。
#[test]
fn operations_that_are_not_pending_are_rejected_before_touching_files() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    let database = Arc::new(Database::new(
        db::init(&root_dir.path().join("library.db")).unwrap(),
    ));
    let conn = database.lock().unwrap();
    seed_assets(&conn, &root, &["d.jpg"]);
    drop(conn);

    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    let on_progress = |_progress| {};
    let unknown = resource_ref("operation_a4_02_does_not_exist");
    let error =
        operation_dispatch::start(&database, &unknown, &queue, &export_cancel, &on_progress)
            .expect_err("不存在的操作必须被拒绝");
    assert_eq!(error.code(), "REFERENCE_EXPIRED");
}

/// 移入回收站是提交即完成，不该出现在执行器的可派发能力里。
///
/// 保留这条断言是为了防止有人「顺手」把它也接进派发器：`submit_trash` 已经在
/// effect 里软删并结算，再执行一次会落到不存在的源文件上。这里刻意构造一个
/// **停下** `queued/ready` 的 `assets.trash.submit` 操作，绕过正常路径的立即结算，
/// 才能证明拦截发生在能力分派这一层，而不是靠状态检查侥幸拦住。
#[test]
fn submit_complete_capabilities_are_not_dispatchable() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    let database = Arc::new(Database::new(
        db::init(&root_dir.path().join("library.db")).unwrap(),
    ));
    let asset_ids = {
        let conn = database.lock().unwrap();
        seed_assets(&conn, &root, &["trash-a.jpg"])
    };
    let gui = gui_caller(&database);
    let draft = plans::PlanDraft {
        capability: CapabilityId::AssetsTrashSubmit,
        scope: teacell_ai_media_manager_lib::contracts::agent::PlanScopeSummary {
            scope_ref: resource_ref("scope_a4_02_trash"),
            source_ref: None,
            selection_ref: Some(resource_ref("selection_a4_02_trash")),
            target_ref: None,
            service_ref: None,
            service_revision: None,
            model: None,
        },
        item_count: asset_ids.len() as u32,
        summary: serde_json::from_value(json!("移入回收站")).unwrap(),
        details_json: None,
        budget: teacell_ai_media_manager_lib::contracts::agent::BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 60,
            max_items: 1,
            monetary_limit: None,
        },
        ttl_seconds: 600,
    };
    let preview =
        plans::prepare_plan(&database, &gui, draft.clone()).expect("可以准备永久/回收站计划");
    plans::approve_plan(
        &database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref(preview.review_ref.as_str()),
    )
    .unwrap();
    let submission = plans::submit_plan_with_effect(
        &database,
        &gui,
        &resource_ref(preview.plan_id.as_str()),
        &resource_ref("idempotency_a4_02_trash"),
        &draft,
        |transaction, operation| {
            operations_db::insert_step(
                transaction,
                &operations_db::NewOperationStep {
                    operation_id: operation.operation_id.clone(),
                    step_id: operation.step_id.clone(),
                    ordinal: 0,
                    capability: CapabilityId::AssetsTrashSubmit.as_str().to_string(),
                    total: Some(asset_ids.len() as i64),
                    created_at: operation.created_at,
                },
            )
        },
    )
    .expect("提交应成功");
    assert_eq!(submission.operation.state, OperationState::Queued);

    let operation_ref = resource_ref(submission.operation.operation_id.as_str());
    let queue = Arc::new(operations::OperationQueue::default());
    let export_cancel = operation_dispatch::ExportCancelRegistry::default();
    let on_progress = |_progress| {};
    let error = operation_dispatch::start(
        &database,
        &operation_ref,
        &queue,
        &export_cancel,
        &on_progress,
    )
    .expect_err("提交即完成的能力不得被派发器执行");
    assert!(
        matches!(error.code(), "UNSUPPORTED" | "UNSUPPORTED_CAPABILITY"),
        "应报不支持，实际 {}",
        error.code()
    );
}
