//! A5-03：MCP 与 GUI/内置助手并用的互操作验证。
//!
//! 这里跑的是**真的外部客户端**，不是 mock：
//!   * `packages/mcp-interop/client-node.mjs` —— 官方 `@modelcontextprotocol/sdk` 1.32.1
//!   * `packages/mcp-interop/client-python.py` —— PyPI `mcp` 1.28.1（另一套独立实现）
//!
//! 两条客户端都走 stdio 连 `packages/mcp-bridge`，桥接再用命名管道连到本进程里
//! 真的 `McpBridge` 监听器，后者复用与内置 Agent 完全相同的 `dispatch_capability`。
//!
//! 为什么客户端脚本只做「一步」：GUI 审批必须发生在两次调用之间，而审批只有
//! 可信 GUI 能做（`plans.approve` 的 callers 只有 `gui`）。所以剧本由本测试驱动，
//! 客户端保持诚实——它永远拿不到批准帧。
//!
//! 运行：`cargo test --test mcp_interop -- --test-threads=1`
//! 依赖：node 与 python 均在 PATH 上（可用 `TEACELL_INTEROP_NODE` /
//! `TEACELL_INTEROP_PYTHON` 覆盖），`packages/mcp-interop` 已 `npm install`。
//! 非 Windows x64 上 `platform_supported()` 为假，本套件显式跳过并打印原因，
//! 不假装通过（与「未运行平台明确未验证」一致）。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::Value;

use teacell_ai_media_manager_lib::contracts::agent::{
    BudgetSummary, CapabilityId, OperationState, PlanScopeSummary, ResourceRef,
};
use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
use teacell_ai_media_manager_lib::db::{self, assets, libraries};
use teacell_ai_media_manager_lib::services::authorization::{self, TrustedAdapterIdentity};
use teacell_ai_media_manager_lib::services::library_migration::LibraryMaintenanceGate;
use teacell_ai_media_manager_lib::services::operations::{self, OperationQueue};
use teacell_ai_media_manager_lib::services::plans::PlanDraft;
use teacell_ai_media_manager_lib::services::{managed_library, mcp_bridge, plans};
use teacell_ai_media_manager_lib::state::Database;
use teacell_ai_media_manager_lib::utils::file_identity::file_identity;

// ---------------------------------------------------------------------------
// 播种：一个真的、已核验的总仓
// ---------------------------------------------------------------------------

struct Fixture {
    _root: tempfile::TempDir,
    _data: tempfile::TempDir,
    db: Arc<Database>,
    bridge: mcp_bridge::McpBridge,
    gate: Arc<LibraryMaintenanceGate>,
    connection_id: String,
    endpoint: PathBuf,
    asset_ids: Vec<i64>,
}

/// 素材必须走完核验：否则大量能力按 `OWNERSHIP_UNVERIFIED` 拒绝，
/// 这不是可以绕过的前置条件，而是「未核验的文件不许被外部客户端操作」本身。
fn seed_assets(conn: &rusqlite::Connection, root: &Path, names: &[&str]) -> Vec<i64> {
    managed_library::adopt_initial_root(conn, root).expect("初始化总仓失败");
    names
        .iter()
        .map(|name| add_asset(conn, root, name))
        .collect()
}

/// 往已采纳的总仓里再加一个已核验素材。
///
/// 单独抽出来是因为「准备之后总仓又多了素材」正是选择集合快照要挡住的场景：
/// 客户端已经看到 N 项、用户已经按 N 项批准，之后新增的文件不该被顺手带走。
fn add_asset(conn: &rusqlite::Connection, root: &Path, name: &str) -> i64 {
    let library = libraries::current(conn)
        .expect("读取总仓失败")
        .expect("总仓应已采纳");
    let path = root.join(name);
    std::fs::write(&path, format!("interop fixture {name}")).expect("写素材失败");
    let modified_at = std::fs::metadata(&path)
        .unwrap()
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
        std::fs::metadata(&path).unwrap().len() as i64,
        "image/jpeg",
        modified_at,
    )
    .expect("登记素材失败");
    let candidate = libraries::pending_page(conn, 0, 100)
        .expect("读取待核验页失败")
        .into_iter()
        .find(|item| item.asset_id == id)
        .expect("新素材应出现在待核验页");
    let identity = {
        let file = std::fs::File::open(&path).unwrap();
        file_identity(&file).expect("读取文件指纹失败")
    };
    assert!(
        libraries::record_verified(
            conn,
            &library,
            &candidate,
            name,
            &identity,
            &"a".repeat(64),
            "interop-fixture",
        )
        .expect("登记核验证据失败"),
        "核验证据应写入成功"
    );
    id
}

fn fixture(names: &[&str], capabilities: &[&str]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let db_path = data.path().join("library.db");
    let conn = db::init(&db_path).expect("建库失败");
    let asset_ids = seed_assets(&conn, root.path(), names);
    let capabilities: Vec<String> = capabilities.iter().map(|id| id.to_string()).collect();
    let pairing =
        mcp_bridge::create_pairing(&conn, "外部编辑器", &capabilities).expect("创建配对失败");
    let db = Arc::new(Database::new(conn));
    let gate = Arc::new(LibraryMaintenanceGate::new());
    let bridge = mcp_bridge::McpBridge::new();
    bridge
        .start(Arc::clone(&db), data.path(), Arc::clone(&gate))
        .expect("启动 MCP 监听失败");
    let endpoint = mcp_bridge::endpoint_path(data.path());
    assert!(endpoint.is_file(), "监听启动后端点文件必须存在");
    Fixture {
        _root: root,
        _data: data,
        db,
        bridge,
        gate,
        connection_id: pairing.connection_id,
        endpoint,
        asset_ids,
    }
}

fn all_publishable() -> Vec<&'static str> {
    mcp_bridge::publishable_capability_ids()
}

fn write_capabilities() -> Vec<&'static str> {
    vec![
        "assets.search",
        "assets.trash.prepare",
        "assets.trash.submit",
        "operations.get",
    ]
}

// ---------------------------------------------------------------------------
// 客户端驱动
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 应有上级目录")
        .to_path_buf()
}

fn bridge_script() -> PathBuf {
    repo_root().join("packages/mcp-bridge/src/server.mjs")
}

fn node_client() -> PathBuf {
    repo_root().join("packages/mcp-interop/client-node.mjs")
}

fn python_client() -> PathBuf {
    repo_root().join("packages/mcp-interop/client-python.py")
}

/// node 用绝对路径传给客户端：Python SDK 的 `StdioServerParameters.env`
/// 会替换整个子进程环境，靠 PATH 解析 `node` 在那种调用里并不可靠。
///
/// 返回 `Option`：解析不到时返回 `None` 由 `prerequisites_available()` 统一判为
/// 「前置缺失，跳过」，而不是在构造函数里 `panic`——否则干净 CI（没装对应
/// 运行时、也没准备 `packages/mcp-interop` 依赖）会因断言直接变红，反而把
/// 「环境没准备」误报成「产品回归」。
fn node_executable() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("TEACELL_INTEROP_NODE") {
        if !value.trim().is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    if let Some(found) = which_first("node") {
        return Some(found);
    }
    let bundled = repo_root().join("artifacts/agent-runtime/node-v24.19.0-win-x64/node.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    None
}

/// python 解释器要能被 `import mcp` 才算合格；`where python` 的第一个候选
/// 可能是 Microsoft Store 的占位程序，所以逐个探测而不是盲信。
///
/// 返回 `Option`：没有任何候选能 `import mcp` 时返回 `None`（前置缺失），
/// 由调用方统一跳过，不再 `panic`。
fn python_executable() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(value) = std::env::var("TEACELL_INTEROP_PYTHON") {
        if !value.trim().is_empty() {
            candidates.push(PathBuf::from(value));
        }
    }
    if let Some(found) = which_first("python") {
        candidates.push(found);
    }
    if let Some(found) = which_first("python3") {
        candidates.push(found);
    }
    for candidate in &candidates {
        let probe = Command::new(candidate)
            .args(["-c", "import mcp; print(mcp.__file__)"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(probe, Ok(status) if status.success()) {
            return Some(candidate.clone());
        }
    }
    None
}

/// A6-04：环境缺项在严格模式下必须变成失败，不能静默通过。
/// 本机默认允许跳过（开发机可能没装 mcp pip 包），交付门禁以 `TEACELL_GATE_STRICT=1` 运行。
/// 返回值 true = 按制度跳过；false = 调用方应 fail closed。
fn missing_prerequisite_decision(missing: &str, strict: bool) -> bool {
    if strict {
        return false;
    }
    eprintln!("A5-03：真实客户端互操作前置缺失，跳过（非产品失败）：{missing}");
    true
}

fn interop_strict() -> bool {
    matches!(std::env::var("TEACELL_GATE_STRICT").as_deref(), Ok("1"))
}

/// 真实客户端互操作的全部前置：node 可解析、python 能 `import mcp`、
/// `packages/mcp-interop` 已装官方 `@modelcontextprotocol/sdk`。任缺其一即视为
/// 「本机未准备互操作环境」，测试诚实跳过（打印原因），不伪装通过、也不误报红。
/// 真正的 A5-03 验收环境（CI 的 windows 作业、开发机）会准备这些前置，届时才真正跑。
fn prerequisites_available() -> bool {
    let mut missing: Vec<&str> = Vec::new();
    if node_executable().is_none() {
        missing.push("node（PATH 或 TEACELL_INTEROP_NODE）");
    }
    if python_executable().is_none() {
        missing.push("python 未能 import mcp（TEACELL_INTEROP_PYTHON 或 pip install mcp）");
    }
    let sdk = repo_root()
        .join("packages/mcp-interop/node_modules/@modelcontextprotocol/sdk/package.json");
    if !sdk.is_file() {
        missing.push("packages/mcp-interop 未 npm install（缺 @modelcontextprotocol/sdk）");
    }
    if missing.is_empty() {
        return true;
    }
    let detail = missing.join("；");
    if !missing_prerequisite_decision(&detail, interop_strict()) {
        panic!("A5-03：真实客户端互操作前置缺失，严格模式下不得跳过：{detail}");
    }
    false
}

#[test]
fn strict_mode_turns_a_missing_interop_prerequisite_into_a_failure() {
    // 证明门禁的两种走向：本机默认跳过，交付门禁严格模式必须失败。
    assert!(
        missing_prerequisite_decision("node 缺失", false),
        "非严格模式应跳过而不是失败"
    );
    assert!(
        !missing_prerequisite_decision("node 缺失", true),
        "严格模式下缺少前置必须 fail closed，否则证据会静默通过"
    );
}

fn which_first(program: &str) -> Option<PathBuf> {
    let output = Command::new("where").arg(program).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

/// 客户端单步调用。真正的剧本（含 GUI 审批）由测试安排，
/// 因为批准帧只能由可信 GUI 产生。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Client {
    Node,
    Python,
}

fn run_client(fixture: &Fixture, client: Client, extra: &[&str]) -> Value {
    let mut command = match client {
        Client::Node => {
            let runtime = node_executable().expect(
                "prerequisites_available() 应已保证 node 可解析；run_client 不得在缺前置时被调用",
            );
            let mut command = Command::new(runtime);
            command.arg(node_client());
            command
        }
        Client::Python => {
            let interpreter = python_executable().expect(
                "prerequisites_available() 应已保证 python 可解析；run_client 不得在缺前置时被调用",
            );
            let mut command = Command::new(interpreter);
            command.arg(python_client());
            command
        }
    };
    let node = node_executable()
        .expect("prerequisites_available() 应已保证 node 可解析；run_client 不得在缺前置时被调用");
    command
        .arg("--endpoint")
        .arg(&fixture.endpoint)
        .arg("--connection")
        .arg(&fixture.connection_id)
        .arg("--bridge")
        .arg(bridge_script())
        .arg("--node")
        .arg(node);
    for value in extra {
        command.arg(value);
    }
    let label = match client {
        Client::Node => "node",
        Client::Python => "python",
    };
    let output = capture(command, label);
    parse_report(label, &output)
}

fn capture(mut command: Command, label: &str) -> String {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child: Child = command
        .spawn()
        .unwrap_or_else(|error| panic!("启动 {label} 客户端失败: {error}"));
    let stdout = child.stdout.take().expect("stdout 应可读");
    let stderr = child.stderr.take().expect("stderr 应可读");
    let reader = std::thread::spawn(move || {
        let lines: Vec<String> = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .collect();
        lines.join("\n")
    });
    let errors = std::thread::spawn(move || {
        let lines: Vec<String> = BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect();
        lines.join("\n")
    });
    // 客户端要等宿主握手 + 工具调用，给足预算；超时按失败处理，
    // 不静默跳过——「连上了但没结果」必须被看见。
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().unwrap_or_default();
                let stderr_text = errors.join().unwrap_or_default();
                // 退出码为 0 但没有输出同样是失败：报告是这条链路的唯一证据，
                // 静默的空输出不能当成通过。
                assert!(
                    !text.trim().is_empty(),
                    "{label} 客户端退出码 {:?} 但没有输出；stderr：\n{stderr_text}",
                    status.code()
                );
                if std::env::var("TEACELL_INTEROP_DEBUG").is_ok() {
                    eprintln!("[{label}] stderr：\n{stderr_text}");
                }
                return text;
            }
            Ok(None) => {}
            Err(error) => panic!("等待 {label} 客户端失败: {error}"),
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label} 客户端 180 秒内没有结束");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn parse_report(label: &str, stdout: &str) -> Value {
    let line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("{label} 客户端没有输出"));
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("{label} 客户端输出不是 JSON（{error}）：{line}");
    })
}

fn text_of<'a>(report: &'a Value, key: &str) -> &'a str {
    report
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("报告缺少字段 {key}：{report}"))
}

// ---------------------------------------------------------------------------
// 宿主侧动作（真正的「茶馆可信界面」）
// ---------------------------------------------------------------------------

fn gui_caller(db: &Database) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: "session_interop_gui".to_string(),
            instance_id: "instance_interop_gui".to_string(),
        },
        // `generated` 是 host-only；集成测试的外部身份只能像真实调用那样给一个引用。
        resource_ref("req_interop_gui_fixture"),
        Vec::new(),
    )
    .expect("构造 GUI 调用者失败")
}

/// `ResourceRef::from_trusted` 是 host-only，集成测试只能走 serde。
fn resource_ref(value: &str) -> ResourceRef {
    serde_json::from_value(Value::String(value.to_string())).expect("资源引用不合法")
}

fn pi_caller(db: &Database, principal: &str) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Pi {
            principal_id: principal.to_string(),
            session_id: "session_interop_pi".to_string(),
            instance_id: "instance_interop_pi".to_string(),
        },
        resource_ref("req_interop_pi_fixture"),
        Vec::new(),
    )
    .expect("构造内置 Agent 调用者失败")
}

fn mcp_caller(db: &Database, principal: &str) -> CallerContext {
    let conn = db.lock().unwrap();
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Mcp {
            principal_id: principal.to_string(),
            instance_id: "instance_interop_mcp_fixture".to_string(),
        },
        resource_ref("req_interop_mcp_fixture"),
        Vec::new(),
    )
    .expect("构造外部客户端调用者失败")
}

fn pairing_of(fixture: &Fixture) -> db::mcp_pairings::PairingRecord {
    let conn = fixture.db.lock().unwrap();
    db::mcp_pairings::get(&conn, &fixture.connection_id)
        .expect("读取配对失败")
        .expect("配对应存在")
}

fn principal_id(db: &Database, operation_ref: &str) -> String {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT principal_id FROM operations WHERE operation_id=?1",
        rusqlite::params![operation_ref],
        |row| row.get(0),
    )
    .expect("读取操作主体失败")
}

/// 一个「已受理但还没被任何 worker 领走」的计划草稿。
///
/// `submit_plan`（而不是 `submit_plan_with_effect`）会留下空 effect 的提交，
/// 操作因此停在 `queued`/`ready` —— 这正是排队语义唯一可观测的窗口。
fn queued_draft() -> PlanDraft {
    PlanDraft {
        capability: CapabilityId::ImportsSubmit,
        scope: PlanScopeSummary {
            scope_ref: resource_ref("scope_interop_queue"),
            source_ref: None,
            selection_ref: None,
            target_ref: None,
            service_ref: None,
            service_revision: None,
            model: None,
        },
        item_count: 1,
        summary: serde_json::from_value(Value::String("互操作排队测试".to_string()))
            .expect("摘要不合法"),
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

/// 由 `caller` 自己准备并被 GUI 批准，然后由 `caller` 自己提交。
///
/// 外部主体能提交、但不能批准：`plans.approve` 只对 GUI 开放，
/// 所以跨来源排队测试必须走「外部提交 + 本机确认」这条真实链路。
fn queue_queued_operation(
    db: &Database,
    caller: &CallerContext,
    idempotency_key: &str,
) -> ResourceRef {
    let gui = gui_caller(db);
    let draft = queued_draft();
    let preview = plans::prepare_plan(db, caller, draft.clone()).expect("准备计划失败");
    plans::approve_plan(db, &gui, &preview.plan_id, &preview.review_ref).expect("GUI 确认失败");
    let submitted = plans::submit_plan(
        db,
        caller,
        &preview.plan_id,
        &resource_ref(idempotency_key),
        &draft,
    )
    .expect("提交计划失败");
    submitted.operation.operation_id
}

fn queued_operation_ids(db: &Database) -> Vec<String> {
    let conn = db.lock().unwrap();
    let mut statement = conn
        .prepare(
            "SELECT operation_id FROM operations
              WHERE state='queued' AND phase='ready'
              ORDER BY created_at,operation_id",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn operations_total(db: &Database) -> i64 {
    let conn = db.lock().unwrap();
    conn.query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
        .unwrap()
}

fn active_count(db: &Database) -> i64 {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT count(*) FROM assets WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn trashed_count(db: &Database) -> i64 {
    let conn = db.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM assets WHERE deleted_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    count
}

fn skip_on_unsupported_platform() -> bool {
    if mcp_bridge::platform_supported() {
        return false;
    }
    eprintln!(
        "A5-03：当前平台不支持 MCP 本机接入（platform_supported() == false），\
         真实客户端互操作未在此平台验证，按发布门禁处理。"
    );
    true
}

// ---------------------------------------------------------------------------
// ① 初始化与工具发现：两个独立实现的客户端看到同一份工具表与协议版本
// ---------------------------------------------------------------------------

#[test]
fn two_independent_clients_agree_on_protocol_and_tool_table() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg"], &all_publishable());

    let node = run_client(&fixture, Client::Node, &["--mode", "discover"]);
    let python = run_client(&fixture, Client::Python, &["--mode", "discover"]);

    assert_eq!(node["ok"], Value::Bool(true), "node 发现失败：{node}");
    assert_eq!(python["ok"], Value::Bool(true), "python 发现失败：{python}");

    let expected: Vec<String> = all_publishable().iter().map(|id| id.to_string()).collect();
    let expected_tools: Vec<String> = expected
        .iter()
        .map(|id| format!("tea_{}", id.replace('.', "_")))
        .collect();
    let mut expected_tools = expected_tools;
    expected_tools.sort();

    let node_tools: Vec<String> = node["tools"]
        .as_array()
        .expect("node 报告缺少 tools")
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    let python_tools: Vec<String> = python["tools"]
        .as_array()
        .expect("python 报告缺少 tools")
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();

    assert_eq!(node_tools, expected_tools, "node 工具表应等于可发布集合");
    assert_eq!(
        python_tools, node_tools,
        "两个独立客户端必须看到同一份工具表——差异只能来自体验，不能来自授权"
    );
    assert_eq!(node["toolCount"], Value::from(expected_tools.len() as u64));

    // 客户端自报名称只影响显示，不参与授权；两个客户端都拿到同一个宿主身份。
    assert_eq!(
        node["server"]["name"], python["server"]["name"],
        "同一个宿主对两个客户端应报告同一身份"
    );
}

// ---------------------------------------------------------------------------
// ② 查询 → 准备 → 茶馆确认 → 查看收据，以及「已完成操作不被重放」
// ---------------------------------------------------------------------------

#[test]
fn node_client_prepares_then_only_writes_after_the_gui_approves() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg", "b.jpg", "keep.jpg"], &write_capabilities());

    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(prepared["ok"], Value::Bool(true), "准备失败：{prepared}");
    let seeded = fixture.asset_ids.len() as u64;
    assert_eq!(prepared["total"], Value::from(seeded));
    assert_eq!(prepared["itemCount"], Value::from(seeded));
    assert_eq!(
        prepared["originKind"], "mcp",
        "计划必须写明发起方是外部客户端"
    );
    assert_eq!(
        prepared["originLabel"], "外部编辑器",
        "审核界面要能指名是哪个配对客户端"
    );
    assert_eq!(
        active_count(&fixture.db),
        seeded as i64,
        "准备阶段不得改动任何素材"
    );

    // 未批准就提交：外部调用永远是 APPROVAL_REQUIRED（桥接映射成 NOT_ALLOWED）。
    let unapproved = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit-unapproved",
            "--plan-id",
            text_of(&prepared, "planId"),
        ],
    );
    assert_eq!(unapproved["ok"], Value::Bool(false), "未批准不应成功");
    assert_eq!(text_of(&unapproved, "code"), "NOT_ALLOWED");
    assert_eq!(operations_total(&fixture.db), 0, "未批准不得产生操作");
    assert_eq!(active_count(&fixture.db), 3);

    // 茶馆可信界面确认。
    let gui = gui_caller(&fixture.db);
    plans::approve_plan(
        &fixture.db,
        &gui,
        &resource_ref(text_of(&prepared, "planId")),
        &resource_ref(text_of(&prepared, "reviewRef")),
    )
    .expect("GUI 确认外部计划失败");

    let submitted = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            text_of(&prepared, "planId"),
            "--idempotency-key",
            "interop_node_once",
        ],
    );
    assert_eq!(submitted["ok"], Value::Bool(true), "提交失败：{submitted}");
    assert_eq!(submitted["replayed"], Value::Bool(false));
    assert_eq!(submitted["state"], "succeeded");
    assert_eq!(submitted["total"], Value::from(3));
    assert_eq!(operations_total(&fixture.db), 1);
    assert_eq!(active_count(&fixture.db), 0, "三项都应移入回收站");
    assert_eq!(trashed_count(&fixture.db), 3);

    // 同一幂等键重试：返回同一个操作，不会被重放成第二次写入。
    let replayed = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            text_of(&prepared, "planId"),
            "--idempotency-key",
            "interop_node_once",
        ],
    );
    assert_eq!(replayed["ok"], Value::Bool(true), "重试失败：{replayed}");
    assert_eq!(replayed["replayed"], Value::Bool(true));
    assert_eq!(
        text_of(&replayed, "operationId"),
        text_of(&submitted, "operationId"),
        "重试必须命中同一个操作 id"
    );
    assert_eq!(operations_total(&fixture.db), 1, "重试不得新建操作");
    assert_eq!(trashed_count(&fixture.db), 3, "重试不得重复删除");
}

// ---------------------------------------------------------------------------
// ③ 拒绝不产生写入：另一个实现的客户端同样是只读的
// ---------------------------------------------------------------------------

#[test]
fn python_client_prepares_and_gui_rejection_keeps_the_library_untouched() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg", "b.jpg"], &write_capabilities());

    let prepared = run_client(&fixture, Client::Python, &["--mode", "prepare"]);
    assert_eq!(prepared["ok"], Value::Bool(true), "准备失败：{prepared}");
    assert_eq!(prepared["itemCount"], Value::from(2));
    assert_eq!(prepared["originKind"], "mcp");
    assert_eq!(prepared["originLabel"], "外部编辑器");

    let gui = gui_caller(&fixture.db);
    let rejected = plans::reject_plan(
        &fixture.db,
        &gui,
        &resource_ref(text_of(&prepared, "planId")),
        &resource_ref(text_of(&prepared, "reviewRef")),
    )
    .expect("GUI 拒绝外部计划失败");
    assert_eq!(
        format!("{:?}", rejected.review_status),
        "Invalidated",
        "拒绝复用 invalidated 状态"
    );

    let submitted = run_client(
        &fixture,
        Client::Python,
        &[
            "--mode",
            "submit",
            "--plan-id",
            text_of(&prepared, "planId"),
        ],
    );
    assert_eq!(submitted["ok"], Value::Bool(false), "被拒计划不应能提交");
    assert_eq!(
        text_of(&submitted, "code"),
        "NOT_ALLOWED",
        "被拒计划应被桥接映射为不可写入：{submitted}"
    );
    assert_eq!(operations_total(&fixture.db), 0, "拒绝不得产生操作");
    assert_eq!(active_count(&fixture.db), 2, "拒绝不得改动素材");
    assert_eq!(trashed_count(&fixture.db), 0);
}

// ---------------------------------------------------------------------------
// ④ 迁仓/恢复期间：外部写入被维护门挡住，查询仍然可用
// ---------------------------------------------------------------------------

#[test]
fn maintenance_blocks_external_writes_from_a_real_client_but_keeps_queries() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg", "b.jpg"], &write_capabilities());

    // 先证明维护前是通的，否则「被挡住」可能只是因为别的错误。
    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(
        prepared["ok"],
        Value::Bool(true),
        "维护前应可准备：{prepared}"
    );

    let lease = Arc::clone(&fixture.gate)
        .enter("互操作测试：模拟迁仓")
        .expect("维护门应当能取到租约");
    assert!(fixture.gate.is_active());

    // 查询类保持可用：客户端仍然能列出工具。
    let discover = run_client(&fixture, Client::Node, &["--mode", "discover"]);
    assert_eq!(
        discover["ok"],
        Value::Bool(true),
        "维护期查询应可用：{discover}"
    );
    assert!(
        discover["toolCount"].as_u64().unwrap_or(0) > 0,
        "维护期客户端仍应看到工具表"
    );

    // 写类必须被挡，并且映射成「总仓不可用」而不是内部错误。
    let blocked = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(blocked["ok"], Value::Bool(false), "维护期外部写入必须失败");
    assert_eq!(
        text_of(&blocked, "code"),
        "LIBRARY_OFFLINE",
        "维护期写类应映射为 LIBRARY_OFFLINE：{blocked}"
    );
    assert_eq!(operations_total(&fixture.db), 0);

    drop(lease);
    assert!(!fixture.gate.is_active());

    // 维护结束后恢复可用，并且还是一个全新计划（不重放维护前的计划与收据）。
    let after = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(after["ok"], Value::Bool(true), "维护结束后应恢复：{after}");
    assert_ne!(
        text_of(&after, "planId"),
        text_of(&prepared, "planId"),
        "维护后必须生成新计划，不能复用旧审批"
    );
}

// ---------------------------------------------------------------------------
// ⑤ 干净退出与重连：宿主停下后客户端如实报不可用，重新启动后恢复
// ---------------------------------------------------------------------------

#[test]
fn stopping_the_bridge_is_reported_honestly_and_restarting_restores_service() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg"], &write_capabilities());

    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(
        prepared["ok"],
        Value::Bool(true),
        "基线准备失败：{prepared}"
    );

    fixture.bridge.stop();
    assert!(!fixture.endpoint.exists(), "停止后端点文件必须被删除");
    assert!(!fixture.bridge.status().running);

    // 宿主不在：桥接仍能作为 MCP 服务启动，但工具表为空、调用如实报不可用。
    let offline = run_client(&fixture, Client::Node, &["--mode", "discover"]);
    assert_eq!(
        offline["ok"],
        Value::Bool(true),
        "客户端不应崩溃：{offline}"
    );
    assert_eq!(
        offline["toolCount"],
        Value::from(0),
        "宿主不可用时不得谎报工具表"
    );
    let refused = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(refused["ok"], Value::Bool(false));
    assert_eq!(text_of(&refused, "code"), "HOST_UNAVAILABLE");

    // 重新启动后，另一套实现的客户端也能接着用（客户端进程本身无状态）。
    fixture
        .bridge
        .start(
            Arc::clone(&fixture.db),
            fixture._data.path(),
            Arc::clone(&fixture.gate),
        )
        .expect("重启 MCP 监听失败");
    let recovered = run_client(&fixture, Client::Python, &["--mode", "discover"]);
    assert_eq!(
        recovered["ok"],
        Value::Bool(true),
        "重启后应恢复：{recovered}"
    );
    assert!(
        recovered["toolCount"].as_u64().unwrap_or(0) > 0,
        "重启后工具表应恢复"
    );
}

// ---------------------------------------------------------------------------
// ⑦ 断线重连后凭稳定任务 ID 恢复，且已完成的操作不被重放
// ---------------------------------------------------------------------------

#[test]
fn a_reconnecting_client_recovers_a_finished_operation_without_replaying_it() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg", "b.jpg"], &write_capabilities());
    let total = fixture.asset_ids.len();

    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(prepared["ok"], Value::Bool(true), "准备失败：{prepared}");
    let plan_id = text_of(&prepared, "planId").to_string();
    let review_ref = text_of(&prepared, "reviewRef").to_string();

    let gui = gui_caller(&fixture.db);
    plans::approve_plan(
        &fixture.db,
        &gui,
        &resource_ref(&plan_id),
        &resource_ref(&review_ref),
    )
    .expect("确认失败");

    let submitted = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            &plan_id,
            "--idempotency-key",
            "interop_reconnect_once",
        ],
    );
    assert_eq!(submitted["ok"], Value::Bool(true), "提交失败：{submitted}");
    let operation_id = text_of(&submitted, "operationId").to_string();
    assert_eq!(submitted["replayed"], Value::Bool(false));

    // 客户端进程已经退出（上面的 run_client 每次都起一个新进程）。
    // 一个全新的进程、全新的连接、没有任何本地状态，只凭 operationId 就能恢复结果。
    let recovered = run_client(
        &fixture,
        Client::Node,
        &["--mode", "status", "--operation-id", &operation_id],
    );
    assert_eq!(
        recovered["ok"],
        Value::Bool(true),
        "恢复查询失败：{recovered}"
    );
    assert_eq!(text_of(&recovered, "operationId"), operation_id.as_str());
    assert_eq!(text_of(&recovered, "state"), "succeeded");
    assert_eq!(recovered["total"], Value::from(total as u64));
    assert_eq!(
        recovered["canCancel"],
        Value::Bool(false),
        "已完成的任务不得还宣称可以取消"
    );

    // 换一套独立实现的客户端再查一次：稳定 ID 与实现无关。
    let cross_client = run_client(
        &fixture,
        Client::Python,
        &["--mode", "status", "--operation-id", &operation_id],
    );
    assert_eq!(
        cross_client["ok"],
        Value::Bool(true),
        "另一套客户端也应能恢复：{cross_client}"
    );
    assert_eq!(
        text_of(&cross_client, "state"),
        "succeeded",
        "两个客户端看到的任务状态必须一致"
    );

    // 断线重连不是重做：原样重试同一个幂等键只会拿回同一次执行。
    let retried = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            &plan_id,
            "--idempotency-key",
            "interop_reconnect_once",
        ],
    );
    assert_eq!(retried["ok"], Value::Bool(true), "幂等重试失败：{retried}");
    assert_eq!(retried["replayed"], Value::Bool(true));
    assert_eq!(text_of(&retried, "operationId"), operation_id.as_str());
    assert_eq!(
        operations_total(&fixture.db),
        1,
        "重连后的重试不得产生第二次执行"
    );
    assert_eq!(trashed_count(&fixture.db), total as i64, "素材只能处理一遍");
}

// ---------------------------------------------------------------------------
// ⑧ 宿主崩溃留下的过期端点必须如实报告，不得假装能用
// ---------------------------------------------------------------------------

#[test]
fn a_stale_endpoint_from_a_crashed_host_is_reported_instead_of_faked() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg"], &write_capabilities());
    let baseline = run_client(&fixture, Client::Node, &["--mode", "discover"]);
    assert!(
        baseline["toolCount"].as_u64().unwrap_or(0) > 0,
        "基线应能列出工具：{baseline}"
    );

    // 模拟「上一次宿主被强杀」留下的现场：端点文件还在，监听它的管道早已不存在。
    // 用 stop() 先干净收尾（它会删端点文件），再手写一份过期端点 —— 等价于
    // 崩溃前的那个进程写下端点后没有机会清理。
    //
    // 端点文件是契约 §3 定义的公开格式，所以这里按格式直接写，
    // 同时也证明了「端点存在」本身不被当作「宿主可用」的证据。
    fixture.bridge.stop();
    assert!(!fixture.endpoint.exists(), "干净退出会删掉端点文件");
    let stale = serde_json::json!({
        "schemaVersion": 1,
        "protocol": 1,
        "instance": "inst_0000000000000000000000000000000000000000000000000000000000000000",
        "pipeName": "teacell-mcp-v1-inst_crashed_host_fixture",
        "key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    });
    std::fs::write(
        &fixture.endpoint,
        serde_json::to_vec_pretty(&stale).unwrap(),
    )
    .expect("写下过期端点失败");
    assert!(
        fixture.endpoint.is_file(),
        "崩溃时端点文件会残留在磁盘上，这正是需要被识别的现场"
    );

    // 桥接读到过期端点，但连不上管道：必须如实报不可用，而不是给出空工具表假装没事。
    let crashed = run_client(&fixture, Client::Node, &["--mode", "discover"]);
    assert_eq!(
        crashed["ok"],
        Value::Bool(true),
        "客户端自身不应崩溃：{crashed}"
    );
    assert_eq!(
        crashed["toolCount"],
        Value::from(0),
        "宿主已崩溃时不得列出任何工具"
    );
    let refused = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(refused["ok"], Value::Bool(false));
    assert_eq!(
        text_of(&refused, "code"),
        "HOST_UNAVAILABLE",
        "过期端点必须报宿主不可用：{refused}"
    );
    assert_eq!(operations_total(&fixture.db), 0, "崩溃期间不得写入任何操作");

    // 用户重新打开茶馆后，同一个端点仍能恢复服务（端点文件被重写）。
    fixture
        .bridge
        .start(
            Arc::clone(&fixture.db),
            fixture._data.path(),
            Arc::clone(&fixture.gate),
        )
        .expect("重新启动失败");
    let restarted = run_client(&fixture, Client::Python, &["--mode", "discover"]);
    assert!(
        restarted["toolCount"].as_u64().unwrap_or(0) > 0,
        "重启后工具表应恢复：{restarted}"
    );
}

// ---------------------------------------------------------------------------
// ⑨ 撤销配对立即影响新请求（真实客户端视角，不是进程内伪造）
// ---------------------------------------------------------------------------
#[test]
fn revoking_the_pairing_takes_effect_on_the_next_real_client_call() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg"], &write_capabilities());

    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(
        prepared["ok"],
        Value::Bool(true),
        "撤销前应可用：{prepared}"
    );
    let gui = gui_caller(&fixture.db);
    plans::approve_plan(
        &fixture.db,
        &gui,
        &resource_ref(text_of(&prepared, "planId")),
        &resource_ref(text_of(&prepared, "reviewRef")),
    )
    .expect("确认失败");

    // 撤销发生在「已批准但还没提交」之间：这是最危险的时间窗。
    {
        let conn = fixture.db.lock().unwrap();
        db::mcp_pairings::revoke(&conn, &fixture.connection_id, 1_900_000_000).expect("撤销失败");
    }

    let submitted = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            text_of(&prepared, "planId"),
        ],
    );
    assert_eq!(submitted["ok"], Value::Bool(false), "撤销后提交必须失败");
    assert_eq!(
        text_of(&submitted, "code"),
        "PAIRING_REVOKED",
        "撤销应映射为 PAIRING_REVOKED：{submitted}"
    );
    assert_eq!(
        operations_total(&fixture.db),
        0,
        "撤销后未执行的敏感步骤不得执行"
    );
    assert_eq!(active_count(&fixture.db), 1, "素材不得被改动");
}

// ---------------------------------------------------------------------------
// ⑦ 三个来源共用同一条写队列，排队与来源无关
// ---------------------------------------------------------------------------

#[test]
fn gui_pi_and_mcp_writes_share_one_queue_in_arrival_order() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg"], &write_capabilities());
    let mcp_principal = pairing_of(&fixture).principal_id;

    let gui = gui_caller(&fixture.db);
    let pi = pi_caller(&fixture.db, "principal_pi_interop");
    let mcp = mcp_caller(&fixture.db, &mcp_principal);

    // 交错发起，顺序刻意打乱来源：GUI → 外部客户端 → 内置助手。
    //
    // 受理时间记录到秒，同一秒内的先后只能靠 operation_id 兜底，
    // 因此这里刻意把三次受理摊到不同秒，让「到达顺序」本身可观测；
    // 秒内并列的兜底顺序见 `same_second_arrivals_are_ordered_without_regard_to_source`。
    let from_gui = queue_queued_operation(&fixture.db, &gui, "idem_interop_queue_gui");
    std::thread::sleep(Duration::from_millis(1_100));
    let from_mcp = queue_queued_operation(&fixture.db, &mcp, "idem_interop_queue_mcp");
    std::thread::sleep(Duration::from_millis(1_100));
    let from_pi = queue_queued_operation(&fixture.db, &pi, "idem_interop_queue_pi");

    let recorded = queued_operation_ids(&fixture.db);
    assert_eq!(
        recorded,
        vec![
            from_gui.as_str().to_string(),
            from_mcp.as_str().to_string(),
            from_pi.as_str().to_string(),
        ],
        "队列必须按受理先后排列，不按来源分层"
    );
    assert_eq!(
        principal_id(&fixture.db, from_gui.as_str()),
        gui.principal_id.as_str()
    );
    assert_eq!(
        principal_id(&fixture.db, from_mcp.as_str()),
        mcp.principal_id.as_str(),
        "外部客户端的写任务记在自己主体名下，不是冒充本机"
    );
    assert_eq!(
        principal_id(&fixture.db, from_pi.as_str()),
        pi.principal_id.as_str()
    );

    // 单槽：同一时刻只有一个写任务能动，且必须是最早受理的那个。
    let queue = Arc::new(OperationQueue::default());
    assert!(
        queue
            .try_reserve_operation(&fixture.db, from_pi.as_str(), "imports.submit")
            .unwrap()
            .is_none(),
        "后到的内置助手任务不能插队到更早的外部客户端任务之前"
    );
    assert!(
        queue
            .try_reserve_operation(&fixture.db, from_mcp.as_str(), "imports.submit")
            .unwrap()
            .is_none(),
        "后到的外部客户端任务同样不能插队"
    );
    let lease = queue
        .try_reserve_operation(&fixture.db, from_gui.as_str(), "imports.submit")
        .unwrap()
        .expect("最早受理的写任务应能占到唯一的槽位");
    assert!(
        queue.claim_next(&fixture.db).unwrap().is_none(),
        "槽位被占用时其它来源的任务一个都不能启动"
    );

    // 只释放槽位不会让任务前进：仍是队列里最早的那一条。
    drop(lease);
    assert_eq!(
        queue.claim_next(&fixture.db).unwrap().as_deref(),
        Some(from_gui.as_str()),
        "释放槽位不等于完成，队首仍然是那一条"
    );

    // `claim_next` 会把槽位占给被取到的任务；结束任务后必须显式归还，
    // 槽位不随操作状态自动释放（防止两个 worker 同时相信自己是队首）。
    operations::finish_operation(&fixture.db, &gui, &from_gui, OperationState::Succeeded)
        .expect("结束队首任务失败");
    assert!(
        queue.claim_next(&fixture.db).unwrap().is_none(),
        "未归还槽位时，即使队首已完成也不得有任务启动"
    );
    queue.release(from_gui.as_str()).expect("归还槽位失败");

    // 归还之后才轮到第二个受理的来源 —— 与来源无关的 FIFO。
    assert_eq!(
        queue.claim_next(&fixture.db).unwrap().as_deref(),
        Some(from_mcp.as_str()),
        "队首完成并归还槽位后，应由第二个受理的外部客户端任务接管"
    );
    let next_principal = principal_id(&fixture.db, from_mcp.as_str());
    assert_eq!(
        next_principal,
        mcp.principal_id.as_str(),
        "接着跑的是外部客户端自己的任务，没有被换成别的来源"
    );
}

/// 同一秒内受理的任务没有可观测的先后，`(created_at, operation_id)` 是唯一裁决。
///
/// 这条边界本身要被证明是**与来源无关**的：三个来源在同一秒受理后，
/// 队列取到的顺序必须正好等于按 `(created_at, operation_id)` 排序的结果，
/// 而不是「先把 GUI 排完再排外部」之类的来源分层。
#[test]
fn same_second_arrivals_are_ordered_without_regard_to_source() {
    let fixture = fixture(&["a.jpg"], &write_capabilities());
    let mcp_principal = pairing_of(&fixture).principal_id;

    let gui = gui_caller(&fixture.db);
    let pi = pi_caller(&fixture.db, "principal_pi_interop");
    let mcp = mcp_caller(&fixture.db, &mcp_principal);

    // 先踩到秒边界再一起受理，让三次受理落在同一秒内。
    let start = std::time::SystemTime::now();
    std::thread::sleep(Duration::from_millis(
        1_000 - (start.duration_since(UNIX_EPOCH).unwrap().subsec_millis() as u64),
    ));
    let gui_op = queue_queued_operation(&fixture.db, &gui, "idem_interop_tie_gui");
    let mcp_op = queue_queued_operation(&fixture.db, &mcp, "idem_interop_tie_mcp");
    let pi_op = queue_queued_operation(&fixture.db, &pi, "idem_interop_tie_pi");

    let timestamps: Vec<(i64, String)> = {
        let conn = fixture.db.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT created_at,operation_id FROM operations")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let same_second = timestamps
        .iter()
        .all(|(created_at, _)| *created_at == timestamps[0].0);
    if !same_second {
        // 跨秒时先后本就可知，这条用例的前提不成立；真正的先后断言在主用例里。
        // 但「前提不成立」不能静默降级：交付门禁要求它在严格模式下失败，否则 R25 的证据
        // 会变成「有时只测了跨秒顺序」。
        let what = "三次受理跨了秒边界，R25 的「同秒按 operation_id 裁决」前提不成立";
        if interop_strict() {
            panic!("A6-04：{what}（严格模式下不得跳过）");
        }
        eprintln!("跳过：{what}");
        return;
    }

    let mut expected: Vec<&str> = vec![gui_op.as_str(), mcp_op.as_str(), pi_op.as_str()];
    expected.sort_unstable();
    assert_eq!(
        queued_operation_ids(&fixture.db),
        expected
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
        "同一秒内的顺序只能由 operation_id 裁决，不得按来源分组"
    );
}

// ---------------------------------------------------------------------------
// ⑧ 选择集合是快照：准备之后新增的素材不会被顺手带走
// ---------------------------------------------------------------------------

#[test]
fn an_external_plan_only_acts_on_the_selection_it_froze() {
    if skip_on_unsupported_platform() {
        return;
    }
    if !prerequisites_available() {
        return;
    }
    let fixture = fixture(&["a.jpg", "b.jpg"], &write_capabilities());
    let frozen = fixture.asset_ids.len();

    let prepared = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(prepared["ok"], Value::Bool(true), "准备失败：{prepared}");
    assert_eq!(prepared["itemCount"], Value::from(frozen as u64));

    // 计划已发出、用户正在审核：此时总仓又入库一张。
    let late = {
        let conn = fixture.db.lock().unwrap();
        add_asset(&conn, fixture._root.path(), "late.jpg")
    };
    assert!(late > 0);

    // 客户端再查一次能看到 3 项，但计划仍然是那 2 项 —— 快照没有跟着长大。
    let rescanned = run_client(&fixture, Client::Node, &["--mode", "prepare"]);
    assert_eq!(
        rescanned["ok"],
        Value::Bool(true),
        "重新准备失败：{rescanned}"
    );
    assert_eq!(
        rescanned["itemCount"],
        Value::from(frozen as u64 + 1),
        "新的一份计划才应包含新入库的素材"
    );
    assert_ne!(
        text_of(&rescanned, "planId"),
        text_of(&prepared, "planId"),
        "每次准备都是新计划，不能复用旧快照"
    );
    assert_eq!(
        prepared["itemCount"],
        Value::from(frozen as u64),
        "已发出的计划条目数不随风变化"
    );

    let gui = gui_caller(&fixture.db);
    plans::approve_plan(
        &fixture.db,
        &gui,
        &resource_ref(text_of(&prepared, "planId")),
        &resource_ref(text_of(&prepared, "reviewRef")),
    )
    .expect("确认失败");

    let submitted = run_client(
        &fixture,
        Client::Node,
        &[
            "--mode",
            "submit",
            "--plan-id",
            text_of(&prepared, "planId"),
            "--idempotency-key",
            "interop_snapshot_once",
        ],
    );
    assert_eq!(submitted["ok"], Value::Bool(true), "提交失败：{submitted}");
    assert_eq!(
        submitted["total"],
        Value::from(frozen as u64),
        "执行的条目数必须等于批准时冻结的条目数，而不是此刻的总仓大小"
    );
    assert_eq!(
        trashed_count(&fixture.db),
        frozen as i64,
        "只应移入回收站被冻结的那几项"
    );
    assert_eq!(
        active_count(&fixture.db),
        1,
        "审核期间新入库的素材必须仍然活动：它从未出现在被批准的计划里"
    );
}
