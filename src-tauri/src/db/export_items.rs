//! 导出逐文件结果（A4-01）：一个导出任务下每个素材的落地结果。
//!
//! 为什么单独建表而不是只靠 `export_tasks.done`：卡片要求「重名处理、空间不足、
//! 部分成功和取消按逐文件结果处理」，且完成摘要要能列出失败项。单一计数字段
//! 无法表达「哪些失败、为什么失败」，也无法在重试前告诉用户具体撞了哪个文件。
//!
//! 状态语义（`state`）：
//! - `copied`：已落盘并核验成功（目标文件存在且大小与源一致）
//! - `failed`：本文件失败，`error_code` 说明原因，任务继续处理后续文件
//! - `skipped`：未尝试（导出被取消后剩余的文件），`error_code = "CANCELLED"`
//!
//! `error_code` 是需求级字符串而非 `AppErrorCode`：`AppErrorCode` 是跨进程协议
//! 枚举，新增变体会波及 Agent 错误映射的全量匹配，而磁盘错误细分（同名、权限、
//! 空间不足）只在导出摘要里消费。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

/// 已落盘且核验成功
pub const STATE_COPIED: &str = "copied";
/// 本文件失败，任务继续
pub const STATE_FAILED: &str = "failed";
/// 未尝试（取消后剩余）
pub const STATE_SKIPPED: &str = "skipped";

/// 目标位置已有同名文件（本次导出不会覆盖）
pub const CODE_TARGET_EXISTS: &str = "TARGET_EXISTS";
/// 目标目录拒绝写入（权限/只读/被占用）
pub const CODE_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
/// 磁盘空间不足
pub const CODE_DISK_FULL: &str = "DISK_FULL";
/// 源文件已不在原位置（被移动/删除/离线）
pub const CODE_SOURCE_MISSING: &str = "SOURCE_MISSING";
/// 导出在轮到本文件前被取消
pub const CODE_CANCELLED: &str = "CANCELLED";
/// 目标子目录本身不安全（越界，或是链接/重解析点），本文件拒绝写入
pub const CODE_PATH_REJECTED: &str = "PATH_REJECTED";
/// 其他 IO 失败
pub const CODE_IO_ERROR: &str = "IO_ERROR";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportItem {
    pub id: i64,
    pub task_id: i64,
    pub ordinal: i64,
    pub asset_id: Option<i64>,
    pub file_name: String,
    /// 目标路径（native encoded）；失败/跳过时为 None
    pub dest_path: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
}

/// 写入一条逐文件结果。`ordinal` 唯一，重复写入返回既有记录而不覆盖——
/// 导出重试会建新任务，同一任务内同一序号不应被改写（保留首次真实结果）。
///
/// 字段持有 `String` 而非 `&'a str`：调用方在逐文件循环里会先拼出错误消息再落库，
/// 借用版本会逼迫调用方额外借用生命周期，收益只是少一次分配。
#[derive(Debug, Clone)]
pub struct NewExportItem {
    pub task_id: i64,
    pub ordinal: i64,
    pub asset_id: Option<i64>,
    pub file_name: String,
    pub dest_path: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
}

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS export_items (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id       INTEGER NOT NULL,
  ordinal       INTEGER NOT NULL,
  asset_id      INTEGER,
  file_name     TEXT NOT NULL,
  dest_path     TEXT,
  state         TEXT NOT NULL,
  error_code    TEXT,
  error_message TEXT,
  created_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_export_items_task_ordinal
  ON export_items(task_id, ordinal);
"#,
    )?;
    Ok(())
}

const COLS: &str = "id, task_id, ordinal, asset_id, file_name, dest_path, state, error_code, error_message, created_at";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<ExportItem> {
    Ok(ExportItem {
        id: r.get(0)?,
        task_id: r.get(1)?,
        ordinal: r.get(2)?,
        asset_id: r.get(3)?,
        file_name: r.get(4)?,
        dest_path: r.get(5)?,
        state: r.get(6)?,
        error_code: r.get(7)?,
        error_message: r.get(8)?,
        created_at: r.get(9)?,
    })
}

pub fn insert_result(conn: &Connection, item: &NewExportItem) -> AppResult<()> {
    conn.execute(
        "INSERT INTO export_items
           (task_id, ordinal, asset_id, file_name, dest_path, state, error_code, error_message, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(task_id, ordinal) DO NOTHING",
        rusqlite::params![
            item.task_id,
            item.ordinal,
            item.asset_id,
            item.file_name,
            item.dest_path,
            item.state,
            item.error_code,
            item.error_message,
            item.created_at,
        ],
    )?;
    Ok(())
}

pub fn items(conn: &Connection, task_id: i64) -> AppResult<Vec<ExportItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM export_items WHERE task_id = ?1 ORDER BY ordinal ASC"
    ))?;
    let rows = stmt
        .query_map([task_id], from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 逐状态计数：完成摘要用它给出「复制成功 / 失败 / 未尝试」三项真实数字
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub copied: i64,
    pub failed: i64,
    pub skipped: i64,
}

pub fn counts(conn: &Connection, task_id: i64) -> AppResult<ExportCounts> {
    let mut stmt =
        conn.prepare("SELECT state, COUNT(*) FROM export_items WHERE task_id = ?1 GROUP BY state")?;
    let mut out = ExportCounts::default();
    let rows = stmt.query_map([task_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (state, n) = row?;
        match state.as_str() {
            STATE_COPIED => out.copied += n,
            STATE_FAILED => out.failed += n,
            STATE_SKIPPED => out.skipped += n,
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(conn: &Connection, task_id: i64, ordinal: i64, state: &str, code: Option<&str>) {
        insert_result(
            conn,
            &NewExportItem {
                task_id,
                ordinal,
                asset_id: Some(ordinal),
                file_name: format!("a{ordinal}.jpg"),
                dest_path: (state == STATE_COPIED).then(|| "C:/out/a.jpg".to_string()),
                state: state.to_string(),
                error_code: code.map(str::to_string),
                error_message: code.map(|_| "示例失败".to_string()),
                created_at: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn structure_is_idempotent_and_round_trips_items() {
        let conn = crate::db::init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        seed(&conn, 7, 0, STATE_COPIED, None);
        seed(&conn, 7, 1, STATE_FAILED, Some(CODE_TARGET_EXISTS));
        seed(&conn, 7, 2, STATE_SKIPPED, Some(CODE_CANCELLED));

        let list = items(&conn, 7).unwrap();
        assert_eq!(list.len(), 3);
        // 按 ordinal 升序，失败项带上真实原因码
        assert_eq!(list[1].file_name, "a1.jpg");
        assert_eq!(list[1].error_code.as_deref(), Some(CODE_TARGET_EXISTS));
        assert_eq!(list[1].dest_path, None);
        assert_eq!(list[0].dest_path.as_deref(), Some("C:/out/a.jpg"));

        let c = counts(&conn, 7).unwrap();
        assert_eq!(c.copied, 1);
        assert_eq!(c.failed, 1);
        assert_eq!(c.skipped, 1);
        // 其他任务不受影响
        assert_eq!(counts(&conn, 8).unwrap(), ExportCounts::default());
        assert!(items(&conn, 8).unwrap().is_empty());
    }

    #[test]
    fn duplicate_ordinal_keeps_the_first_recorded_result() {
        let conn = crate::db::init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        seed(&conn, 3, 0, STATE_FAILED, Some(CODE_DISK_FULL));
        // 同序号再次写入不应把失败覆盖成成功
        seed(&conn, 3, 0, STATE_COPIED, None);

        let list = items(&conn, 3).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, STATE_FAILED);
        assert_eq!(list[0].error_code.as_deref(), Some(CODE_DISK_FULL));
    }
}
