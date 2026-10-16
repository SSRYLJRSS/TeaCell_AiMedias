//! 打标操作流水（R-25）：挂/摘标签写流水，撤销按 batch_id 反向操作（粒度=批次）

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::libraries::VERIFIED_ASSET_SCOPE;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagOp {
    pub id: i64,
    pub asset_id: i64,
    pub tag_id: i64,
    /// add | remove
    pub op: String,
    /// manual | ai_cloud | ai_local
    pub actor: String,
    /// AI 批次 id（手工操作为 NULL）
    pub batch_id: Option<i64>,
    pub created_at: i64,
    // ---- 展示辅助（查询时 JOIN 填充，写入不涉及）----
    pub tag_name: String,
    pub asset_name: String,
}

/// 写一条流水（必须在调用方事务内调用，与 asset_tags 变更同事务保证原子）
pub(crate) fn record(
    conn: &Connection,
    asset_id: i64,
    tag_id: i64,
    op: &str,
    actor: &str,
    batch_id: Option<i64>,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO tag_ops (asset_id, tag_id, op, actor, batch_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![asset_id, tag_id, op, actor, batch_id, now],
    )?;
    Ok(())
}

/// 最近打标列表（打标页左栏）：按时间倒序，JOIN 标签名/素材名供展示
pub fn recent(conn: &Connection, limit: i64) -> AppResult<Vec<TagOp>> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn.prepare(&format!(
        "SELECT o.id, o.asset_id, o.tag_id, o.op, o.actor, o.batch_id, o.created_at,
                t.name, a.file_name
           FROM tag_ops o
           JOIN tags t ON t.id = o.tag_id
           JOIN assets a ON a.id = o.asset_id
          WHERE {VERIFIED_ASSET_SCOPE}
          ORDER BY o.id DESC LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map([limit], |r| {
            Ok(TagOp {
                id: r.get(0)?,
                asset_id: r.get(1)?,
                tag_id: r.get(2)?,
                op: r.get(3)?,
                actor: r.get(4)?,
                batch_id: r.get(5)?,
                created_at: r.get(6)?,
                tag_name: r.get(7)?,
                asset_name: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 批次撤销（R-25 + 指导书 D）：按流水倒序反向操作——add→摘除、remove→挂回（actor 沿用原值）；
/// 反向操作本身不再写流水（撤销即回滚，历史保留原记录）。
/// D-2/D-3：摘除关联必须带来源批次约束（source_batch_id = 当前批次）且排除手工来源（source != 'manual'），
///          保证撤销不误删用户后续手工/重新添加的标签。
/// D-4：撤销成功后置批次状态为 undone；重复撤销幂等返回 0（已撤销批次直接返回，不修改数据）。
/// D-5：remove 反串挂回的关联 source_batch_id 为 NULL（恢复后的关联不再属于被撤销批次）；本轮不实现 redo。
pub fn undo_batch(conn: &Connection, batch_id: i64) -> AppResult<u64> {
    // D-4：已撤销批次重复撤销幂等返回 0
    let status: String = conn
        .query_row(
            "SELECT status FROM ai_batches WHERE id = ?1",
            [batch_id],
            |r| r.get(0),
        )
        .unwrap_or_default();
    if status == "undone" {
        return Ok(0);
    }
    let ops: Vec<(i64, i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT asset_id, tag_id, op, actor FROM tag_ops
              WHERE batch_id = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map([batch_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    // V24：数值-only 批次（无 tag_ops 流水但确认过数值建议）也必须可撤销
    let numbers_present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM asset_facet_numbers
          WHERE source_batch_id = ?1 AND source != 'manual' AND review_state = 'ai_unreviewed'",
        [batch_id],
        |r| r.get(0),
    )?;
    if ops.is_empty() && numbers_present == 0 {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut applied: u64 = 0;
    for (asset_id, tag_id, op, actor) in &ops {
        match op.as_str() {
            "add" => {
                // D-3：只删「当前批次写入且非手工」的关联，防误删用户后续手工/重新添加的标签。
                // A3：review_state='manual' 的行同时排除（手工来源/手工覆盖都会置 manual，与
                //     source != 'manual' 双保险）；ai_reviewed 行随批次撤销删除，D-3 语义不变。
                applied += tx.execute(
                    "DELETE FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2
                        AND source_batch_id = ?3 AND source != 'manual' AND review_state != 'manual'",
                    rusqlite::params![asset_id, tag_id, batch_id],
                )? as u64;
            }
            "remove" => {
                // D-5：重插不写 source_batch_id（恢复后的关联不再属于被撤销批次）
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO asset_tags
                     (asset_id, tag_id, source, created_at, confirmation, confirmed_at, confirmed_by)
                     VALUES (?1, ?2, ?3, ?4, 'confirmed', ?4, ?3)",
                    rusqlite::params![asset_id, tag_id, actor, now],
                )?;
                applied += inserted as u64;
            }
            _ => {}
        }
    }
    // D-4：撤销成功后置 undone（不再显示可点击撤销；不影响历史 confirmed 计数语义）
    // V24（§6.4）：数值批次撤销 —— 与 asset_tags 的 D-3 守卫逐字对齐
    //（source_batch_id 匹配 + source != 'manual' + review_state='ai_unreviewed'）。
    let numbers_removed = crate::db::facet_numbers::undo_batch_numbers(&tx, batch_id)?;
    let applied = applied + numbers_removed as u64;
    tx.execute(
        "UPDATE ai_batches SET status = 'undone' WHERE id = ?1",
        [batch_id],
    )?;
    tx.commit()?;
    Ok(applied)
}
