//! 维护工具：清空素材库记录（仅出库，原文件不动）。
//!
//! B4/X-17：本模块曾把 `#[ignore]` 的破坏性 `purge_all_assets` 直接指向真实用户库路径，
//! 而 `npm run test:ignored`（`cargo test -- --ignored`）会**全量**跑所有 ignored 测试，
//! 意味着一次“跑被忽略的测试”就会无备份、无确认地清空用户库。现拆分为：
//!
//! 1. `purge_all(conn)`：纯逻辑，任何连接都能用；
//! 2. `purge_all_on_tempfile`：常规单测（非 ignore），只用 tempfile，永不碰真实库；
//! 3. `purge_real_library`：真正的维护动作，`#[ignore]` 且**默认安全退出**——
//!    必须显式提供环境变量 `DEV_MAINTENANCE_DB_PATH` 和确认令牌
//!    `DEV_MAINTENANCE_CONFIRM=PURGE-ALL-ASSETS`，执行前自动生成备份；
//!    缺任一前提就打印原因并返回，绝不误删。
//!
//! `package.json::test:ignored` 已改为只显式列出非破坏性目标（perf_probe），不再全量扫 ignored。

use rusqlite::Connection;

use bagertea_ai_media_v2_lib::db;

/// 清空素材相关记录（出库；原文件不动）。事务内完成，触发器自动清 fts_content。
/// 返回 (清理前素材数, 清理后素材数, fts 剩余)。
fn purge_all(conn: &Connection) -> rusqlite::Result<(i64, i64, i64)> {
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM asset_tags", [])?;
    tx.execute("DELETE FROM ai_suggestions", [])?;
    tx.execute("DELETE FROM ai_batches", [])?;
    // 触发器自动清 fts_content（cjk_bigram 已由 db::init 注册）
    tx.execute("DELETE FROM assets", [])?;
    tx.commit()?;
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))?;
    let fts: i64 = conn.query_row("SELECT COUNT(*) FROM fts_content", [], |r| r.get(0))?;
    Ok((before, after, fts))
}

/// 常规单测：只用 tempfile 建真实 schema 库，插入一条素材后清空并断言归零。
/// 不带 `#[ignore]`，随普通 `cargo test` 一起跑，永不触碰用户库。
#[test]
fn purge_all_on_tempfile() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("library.db");
    let conn = db::init(&db_path).expect("建库失败");
    conn.execute(
        "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
         VALUES ('/tmp/a.jpg', 'a.jpg', '.jpg', 1, 'image/jpeg', 1, 1)",
        [],
    )
    .unwrap();

    let (before, after, fts) = purge_all(&conn).unwrap();
    assert_eq!(before, 1, "清理前应有 1 条素材");
    assert_eq!(after, 0, "清理后素材应归零");
    assert_eq!(fts, 0, "fts_content 应被触发器清空");
}

/// 真实库维护动作：默认安全退出，必须显式声明路径 + 确认令牌，且先自动备份。
///
/// 用法（关闭应用后）：
/// ```powershell
/// $env:DEV_MAINTENANCE_DB_PATH="C:\Users\<you>\AppData\Roaming\bagertea_ai_media_v2\library.db"
/// $env:DEV_MAINTENANCE_CONFIRM="PURGE-ALL-ASSETS"
/// cargo test --test dev_maintenance purge_real_library -- --ignored --nocapture
/// ```
#[test]
#[ignore = "破坏性：需显式路径+确认令牌，先自动备份，仅手动触发"]
fn purge_real_library() {
    const CONFIRM_TOKEN: &str = "PURGE-ALL-ASSETS";

    let db_path = match std::env::var("DEV_MAINTENANCE_DB_PATH") {
        Ok(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => {
            eprintln!("跳过：未设置 DEV_MAINTENANCE_DB_PATH。此维护动作绝不默认指向真实用户库。");
            return;
        }
    };
    match std::env::var("DEV_MAINTENANCE_CONFIRM") {
        Ok(t) if t == CONFIRM_TOKEN => {}
        _ => {
            eprintln!("跳过：确认令牌不匹配（需 DEV_MAINTENANCE_CONFIRM={CONFIRM_TOKEN}）。");
            return;
        }
    }
    if !db_path.exists() {
        eprintln!("跳过：库文件不存在：{}", db_path.display());
        return;
    }

    // 破坏前自动备份到同目录带时间戳文件，失败即中止（不冒险清库）。
    let conn = db::init(&db_path).expect("打开数据库失败");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup_path = db_path.with_file_name(format!("library.pre-purge.{ts}.db"));
    db::backup::backup_to(&conn, &backup_path).expect("清库前自动备份失败——已中止，未做任何删除");
    println!("已自动备份到：{}", backup_path.display());

    let (before, after, fts) = purge_all(&conn).expect("清库失败");
    println!("清理完成：{before} → {after} 条素材；fts_content 剩余 {fts}");
    assert_eq!(after, 0);
    assert_eq!(fts, 0);
}
