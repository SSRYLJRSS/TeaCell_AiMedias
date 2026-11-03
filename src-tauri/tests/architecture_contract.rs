//! 架构分层契约的静态门（R52 / R58）。
//!
//! 为什么放在 Rust 集成测试里：A6-04 要求「需求矩阵指向的证据必须真的覆盖该需求」，
//! 而矩阵只索引 `src-tauri/**` 的 Rust 用例名与 `src/**/*.test.tsx` 的前端用例名
//! （`scripts/acceptance-evidence.mjs`）。R52/R58 的核心子句是**分层约束**，靠再写十几个
//! 行为用例证明不了——只要有人下次在图省事的地方直接 `invoke` 或把 `std::fs` 写进 `db/`，
//! 行为用例照样全绿。所以这里把约束本身做成可从矩阵引用的用例。
//!
//! 只做文本扫描：不编译前端、不链接原生库、不依赖网络，几毫秒量级。

use std::fs;
use std::path::{Path, PathBuf};

/// 仓库根目录（`src-tauri` 的上一级）。
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 必须有上一级目录")
        .to_path_buf()
}

/// 递归收集目录下的文件，按扩展名过滤；目录不存在时返回空列表。
fn collect_files(directory: &Path, extension: &str, into: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, extension, into);
        } else if path.extension().is_some_and(|value| value == extension) {
            into.push(path);
        }
    }
}

/// 仓库相对路径（正斜杠），用于断言消息里给出可点击的位置。
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 只保留生产代码：排除 `*.test.ts(x)` 与 `*.stories.tsx`。
fn is_production_typescript(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    !name.contains(".test.") && !name.contains(".stories.")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("读取 {} 失败：{error}", path.display()))
}

/// 是否订阅了 Tauri 事件（`listen(` / `listen<`）。
/// 不能直接 `contains("listen(")`：`unlisten()`、`UnlistenFn` 都会误伤。
fn subscribes_to_tauri_events(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut from = 0;
    while let Some(offset) = source[from..].find("listen") {
        let start = from + offset;
        let previous = start
            .checked_sub(1)
            .and_then(|index| bytes.get(index).copied())
            .map(char::from);
        let is_identifier_prefix =
            previous.is_some_and(|value| value.is_alphanumeric() || value == '_');
        let rest = &source[start + "listen".len()..];
        let is_call = rest.starts_with('(') || rest.starts_with('<');
        if is_call && !is_identifier_prefix {
            return true;
        }
        from = start + "listen".len();
    }
    false
}

/// 是否从 `@tauri-apps/api/core` 引入了 `invoke` 值。
/// 只看向 `from "@tauri-apps/api/core"` 之前的那个 `import` 片段，
/// 避免把「本组件不直接 invoke」这类注释误判成 import。
fn imports_invoke_from_core(source: &str) -> bool {
    const SOURCE_MODULE: &str = "from \"@tauri-apps/api/core\"";
    let mut from = 0;
    while let Some(offset) = source[from..].find(SOURCE_MODULE) {
        let at = from + offset;
        let window_start = source[..at].rfind("import").unwrap_or(0);
        let segment = &source[window_start..at];
        if segment
            .split(|value: char| !(value.is_alphanumeric() || value == '_'))
            .any(|word| word == "invoke")
        {
            return true;
        }
        from = at + SOURCE_MODULE.len();
    }
    false
}

#[test]
fn database_layer_has_no_network_subprocess_or_decoder_dependency() {
    // R52：SQL 层只做 SQL。网络、子进程、解码都属于服务层，一旦下沉到 db/ 就会在
    // 持锁期间做长耗时 IO（DEVELOPMENT.md §2 的锁内禁令）。
    let root = repository_root();
    let mut files = Vec::new();
    collect_files(&root.join("src-tauri/src/db"), "rs", &mut files);
    assert!(
        !files.is_empty(),
        "src-tauri/src/db 下没有任何 Rust 文件，门禁失去意义"
    );

    const FORBIDDEN: [&str; 7] = [
        "reqwest::",
        "std::net::",
        "tokio::net::",
        "std::process::",
        "image::",
        "libheif",
        "ravif",
    ];
    let mut violations = Vec::new();
    for file in &files {
        let source = read(file);
        for token in FORBIDDEN {
            if source.contains(token) {
                violations.push(format!("{} 含 {token}", relative(&root, file)));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "db 层不得依赖网络/子进程/解码：{violations:#?}"
    );
}

#[test]
fn file_io_stays_out_of_the_sql_layer_except_the_database_file_itself() {
    // R52：`db/` 里允许碰文件系统的只有「数据库文件本身」这条线——
    // mod.rs 打开/暂存/发布库文件、reset.rs 清库、backup.rs 备份恢复。
    // 业务素材的复制、搬运、删除必须留在 services/。
    let root = repository_root();
    let mut db_files = Vec::new();
    collect_files(&root.join("src-tauri/src/db"), "rs", &mut db_files);

    const DATABASE_FILE_ALLOWLIST: [&str; 3] = ["mod.rs", "reset.rs", "backup.rs"];
    let mut offenders = Vec::new();
    let mut sql_found = false;
    for file in &db_files {
        let source = read(file);
        if source.contains("std::fs::") || source.contains("tokio::fs::") {
            let name = file
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            if !DATABASE_FILE_ALLOWLIST.contains(&name.as_str()) {
                offenders.push(relative(&root, file));
            }
        }
        if source.contains("SELECT ")
            || source.contains("INSERT INTO")
            || source.contains("UPDATE ")
        {
            sql_found = true;
        }
    }
    assert!(
        sql_found,
        "db 层应当承载 SQL；找不到任何 SQL 语句，说明索引范围错了"
    );
    assert!(
        offenders.is_empty(),
        "文件 IO 只允许出现在 db/{{mod,reset,backup}}.rs（数据库文件本身），以下文件越界，业务 IO 应放在 services/：{offenders:#?}"
    );

    // 反向哨兵：业务 IO 必须在服务层存在，否则这条门禁会因为「谁都不做 IO」而假绿。
    let mut service_files = Vec::new();
    collect_files(
        &root.join("src-tauri/src/services"),
        "rs",
        &mut service_files,
    );
    let service_io = service_files
        .iter()
        .filter(|file| read(file).contains("std::fs::"))
        .count();
    assert!(
        service_io >= 5,
        "服务层应当承载业务文件 IO，实际只有 {service_io} 个文件使用 std::fs"
    );
}

#[test]
fn operation_progress_is_projected_from_backend_events_instead_of_recomputed_in_the_ui() {
    // R52 第二子句：UI 与 Operation 不得成为第二份子任务真相。
    // 打标进度的唯一写入口是 aiStore.patchProgress，而它只被后端的进度事件回灌调用。
    let root = repository_root();
    let mut files = Vec::new();
    collect_files(&root.join("src"), "ts", &mut files);
    let mut tsx = Vec::new();
    collect_files(&root.join("src"), "tsx", &mut tsx);
    files.extend(tsx);

    const WRITERS: [&str; 3] = [
        "src/stores/aiStore.ts",
        "src/stores/aiStore.test.ts",
        "src/pages/AiTaggingPage.tsx",
    ];
    let mut callers = Vec::new();
    for file in &files {
        let relative_path = relative(&root, file);
        let source = read(file);
        if source.contains("patchProgress(") {
            callers.push(relative_path.clone());
        }
        // 数值化的进度展示只允许出现在 AgentCards 的投影里，其他组件再算一遍就是第二份真相。
        if relative_path != "src/components/agent/AgentCards.tsx"
            && source.contains("processed / ")
            && source.contains("/ total")
        {
            panic!("{relative_path} 自行计算了进度百分比，应改为消费后端投影");
        }
    }
    for caller in &callers {
        assert!(
            WRITERS.contains(&caller.as_str()),
            "patchProgress 只允许被 {WRITERS:?} 写入，{caller} 越界"
        );
    }
    assert!(
        callers
            .iter()
            .any(|path| path == "src/pages/AiTaggingPage.tsx"),
        "打标页应当把后端进度事件喂给 patchProgress，否则进度不会更新"
    );

    let page = read(&root.join("src/pages/AiTaggingPage.tsx"));
    assert!(
        page.contains("aiTaggingProgress") || page.contains("on("),
        "打标页必须订阅后端进度事件，不允许自己按批次推算进度"
    );
}

#[test]
fn frontend_invokes_commands_only_through_the_api_module() {
    // R58：页面禁止直接 invoke，统一走 src/api/*。
    // 唯一例外是 src/utils/logger.ts：生产 WebView 控制台不可见，日志回传必须绕开
    // client.ts，否则 invoke 失败会递归调用 logger（ARCHITECTURE.md §3.3、DEVELOPMENT.md §4.2）。
    let root = repository_root();
    let mut files = Vec::new();
    collect_files(&root.join("src"), "ts", &mut files);
    let mut tsx = Vec::new();
    collect_files(&root.join("src"), "tsx", &mut tsx);
    files.extend(tsx);
    files.retain(|path| is_production_typescript(path));

    const LOGGER_EXCEPTION: &str = "src/utils/logger.ts";
    let mut offenders = Vec::new();
    for file in &files {
        let relative_path = relative(&root, file);
        let in_api = relative_path.starts_with("src/api/");
        let source = read(file);
        let calls_command = source.contains("invoke<") || source.contains("invoke(");
        let imports_core_invoke = imports_invoke_from_core(&source);
        if (calls_command || imports_core_invoke) && !in_api && relative_path != LOGGER_EXCEPTION {
            offenders.push(relative_path.clone());
        }
        // 事件订阅也属于协议层：页面直接 listen 等于自己重造一套进度协议。
        assert!(
            in_api || !subscribes_to_tauri_events(&source),
            "{relative_path} 直接订阅了 Tauri 事件，应走 src/api 的 on()"
        );
        if !in_api && relative_path != LOGGER_EXCEPTION {
            assert!(
                !source.contains("import { listen") || source.contains("import type { listen"),
                "{relative_path} 从 @tauri-apps/api/event 引入了值，只允许 import type"
            );
        }
    }
    assert!(
        offenders.is_empty(),
        "以下文件绕开了 src/api 直接调用 Tauri 命令（唯一例外是 {LOGGER_EXCEPTION}）：{offenders:#?}"
    );

    // 反向哨兵：api 层必须真的在用 invoke，且包了关联 ID，否则这条门禁空转。
    let mut api_modules = 0;
    for file in &files {
        let relative_path = relative(&root, file);
        if !relative_path.starts_with("src/api/") || relative_path == "src/api/client.ts" {
            continue;
        }
        if read(file).contains("invoke<") {
            api_modules += 1;
        }
    }
    assert!(
        api_modules >= 10,
        "src/api 下使用 invoke 的模块只有 {api_modules} 个，索引范围可疑"
    );
    let client = read(&root.join("src/api/client.ts"));
    assert!(
        client.contains("tauriInvoke"),
        "src/api/client.ts 应当是对 @tauri-apps/api/core 的唯一封装点"
    );
    assert!(
        client.contains("createLogCorrelationId"),
        "统一封装点必须带关联 ID，否则失败日志无法定位"
    );
}

#[test]
fn only_one_application_window_exists_and_no_desktop_floating_window_is_created() {
    // R16：「仅应用内的可开关悬浮助手，切到其他应用不出现系统桌面悬浮窗」。
    // 这条子句没法用行为用例证明（jsdom / WebDriver 都看不到「别的窗口」），
    // 只能把「窗口面」本身钉住：配置里只有一个 main 窗口、没有托盘图标，
    // 代码里没有任何动态建窗调用。加上一条，就允许有人在某处悄悄开第二个窗。
    let root = repository_root();

    let configuration = read(&root.join("src-tauri/tauri.conf.json"));
    let parsed: serde_json::Value =
        serde_json::from_str(&configuration).expect("tauri.conf.json 必须是合法 JSON");
    let windows = parsed
        .get("app")
        .and_then(|app| app.get("windows"))
        .and_then(|windows| windows.as_array())
        .expect("tauri.conf.json 必须在 app.windows 声明窗口");
    assert_eq!(
        windows.len(),
        1,
        "应用只允许一个主窗口，实际声明了 {} 个：{windows:#?}",
        windows.len()
    );
    assert_eq!(
        windows[0].get("label").and_then(|label| label.as_str()),
        Some("main"),
        "唯一窗口必须是 main"
    );
    assert!(
        parsed
            .get("app")
            .and_then(|app| app.get("trayIcon"))
            .is_none(),
        "R16 只做应用内悬浮 Logo，不允许托盘/桌面常驻入口"
    );

    // Rust 侧不得动态建窗：Tauri 的所有建窗入口都在下面这几个符号里。
    const WINDOW_BUILDERS: [&str; 5] = [
        "WebviewWindowBuilder",
        "WindowBuilder",
        "WebviewWindow::new",
        "window::Window::new",
        "get_webview_window(",
    ];
    let mut rust_files = Vec::new();
    collect_files(&root.join("src-tauri/src"), "rs", &mut rust_files);
    assert!(
        !rust_files.is_empty(),
        "src-tauri/src 下没有任何 Rust 文件，门禁失去意义"
    );
    for file in &rust_files {
        let relative_path = relative(&root, file);
        let source = read(file);
        for forbidden in WINDOW_BUILDERS {
            assert!(
                !source.contains(forbidden),
                "{relative_path} 出现了 {forbidden}：2.0 不允许在主窗口之外另建窗口（R16）"
            );
        }
    }

    // 前端侧同样不得自己开窗（`new WebviewWindow(...)` / `new Window(...)`）。
    let mut frontend_files = Vec::new();
    collect_files(&root.join("src"), "ts", &mut frontend_files);
    let mut tsx = Vec::new();
    collect_files(&root.join("src"), "tsx", &mut tsx);
    frontend_files.extend(tsx);
    frontend_files.retain(|path| is_production_typescript(path));
    assert!(
        !frontend_files.is_empty(),
        "src 下没有任何生产 TypeScript 文件，门禁失去意义"
    );
    for file in &frontend_files {
        let relative_path = relative(&root, file);
        let source = read(file);
        for forbidden in ["new WebviewWindow(", "new Window("] {
            assert!(
                !source.contains(forbidden),
                "{relative_path} 出现了 {forbidden}：悬浮助手必须留在主窗口内（R16）"
            );
        }
    }

    // 反向哨兵：悬浮助手确实存在，否则这条门禁可能只是在描述一个空实现。
    let floating = read(&root.join("src/components/assistant/FloatingAssistant.tsx"));
    assert!(
        floating.contains("snapPosition"),
        "悬浮助手必须保留吸边逻辑，否则 R16 的「拖动吸边」失去实现"
    );
}
