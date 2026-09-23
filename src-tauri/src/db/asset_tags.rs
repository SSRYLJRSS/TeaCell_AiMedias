//! 素材-标签关联：分配 / 移除 / 批量（FTS tag_names 由触发器聚合刷新；R-25 同步写 tag_ops 流水）

use rusqlite::Connection;

use super::{tag_ops, tags::Tag, tags::FACET_EFFECTIVE};
use crate::error::AppResult;

/// W5h-b / §11：查出 asset 的**唯一同源兄弟**（同目录同主干名，且整组恰好 1 RAW + 1 非 RAW）。
/// 通过统一的 `kinship::paired_sibling` 分类：多 RAW / 多非 RAW / `X.RAW+X.JPG+X.PNG`
/// 等歧义组合一律不返回兄弟，避免不对称同步。返回 0 或 1 个 id。
fn kinship_sibling_ids(conn: &Connection, asset_id: i64) -> Vec<i64> {
    let mut stmt = match conn.prepare("SELECT id, file_path FROM assets WHERE deleted_at IS NULL") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    let all: Vec<(i64, String)> = rows.flatten().collect();
    crate::services::kinship::paired_sibling(&all, asset_id)
        .into_iter()
        .collect()
}

/// W5h-b：设置开关是否开启同源同步（读设置失败时按默认开启处理——设置损坏不应静默关闭功能）
fn kinship_sync_enabled(conn: &Connection) -> bool {
    super::settings::get_settings(conn)
        .map(|s| s.appearance.kinship.sync_tags_to_siblings)
        .unwrap_or(true)
}

/// 不带事务的内部版：供外层已开事务的调用方使用（如 ai::confirm_suggestion）
/// R-25：真实新增的关联写 add 流水（batch_id 由 AI 确认流传入，手工为 None）
/// W5h-b：assign_inner 是关联写入的唯一收口（ai::confirm/apply/tags_cmd::assign 全走它）——
/// 同源同步在这里做（五个调用点分别处理必然漏一个）。同源写入各有独立流水行，
/// undo_batch 倒序回滚自动覆盖两条，无需特殊处理。
pub(crate) fn assign_inner(
    conn: &Connection,
    asset_ids: &[i64],
    tag_ids: &[i64],
    source: &str,
    batch_id: Option<i64>,
) -> AppResult<()> {
    // W5h-b：开启同源同步时展开 asset_ids（每个素材追加其同源 id；去重防止重复写入）
    let effective_ids: Vec<i64> = if kinship_sync_enabled(conn) {
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for &aid in asset_ids {
            if seen.insert(aid) {
                ids.push(aid);
            }
            for sib in kinship_sibling_ids(conn, aid) {
                if seen.insert(sib) {
                    ids.push(sib);
                }
            }
        }
        ids
    } else {
        asset_ids.to_vec()
    };
    let now = chrono::Utc::now().timestamp_millis();
    for &aid in &effective_ids {
        for &tid in tag_ids {
            let n = conn.execute(
                "INSERT OR IGNORE INTO asset_tags
                 (asset_id, tag_id, source, created_at, confirmation, confirmed_at, confirmed_by, source_batch_id, review_state)
                 VALUES (?1, ?2, ?3, ?4, 'confirmed', ?4, ?3, ?5, ?6)",
                rusqlite::params![
                    aid,
                    tid,
                    source,
                    now,
                    batch_id,
                    // F1-f/A3：source='manual' → manual（永不覆盖语义）；
                    // AI 写入一律 ai_unreviewed（用户未审核）
                    if source == "manual" { "manual" } else { "ai_unreviewed" }
                ],
            )?;
            if n > 0 {
                tag_ops::record(conn, aid, tid, "add", source, batch_id)?;
            } else if source == "manual" {
                // 人工确认优先于历史 AI 来源，但关联已存在时不重复记录 add 流水。
                // D-1/D-2：手工覆盖必须清空 source_batch_id，否则撤销 AI 批次会误删手工确认后的标签。
                // F1-f/A3：手工覆盖 → manual（绝不因重跑被清）
                conn.execute(
                    "UPDATE asset_tags SET source='manual', confidence=NULL,
                            source_batch_id=NULL,
                            review_state='manual',
                            confirmation='confirmed', confirmed_at=?3, confirmed_by='manual'
                      WHERE asset_id=?1 AND tag_id=?2 AND source != 'manual'",
                    rusqlite::params![aid, tid, now],
                )?;
            }
        }
    }
    Ok(())
}

pub fn assign(
    conn: &Connection,
    asset_ids: &[i64],
    tag_ids: &[i64],
    source: &str,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    assign_inner(&tx, asset_ids, tag_ids, source, None)?;
    tx.commit()?;
    Ok(())
}

/// R-25：真实移除的关联写 remove 流水（actor 读原关联 source，保证 AI 标签可溯源）
pub fn remove(conn: &Connection, asset_ids: &[i64], tag_ids: &[i64]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    // W5h-b：摘标签同步到同源文件（与 assign_inner 对称；关闭开关则回到独立行为）
    let effective_ids: Vec<i64> = if kinship_sync_enabled(&tx) {
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for &aid in asset_ids {
            if seen.insert(aid) {
                ids.push(aid);
            }
            for sib in kinship_sibling_ids(&tx, aid) {
                if seen.insert(sib) {
                    ids.push(sib);
                }
            }
        }
        ids
    } else {
        asset_ids.to_vec()
    };
    for &aid in &effective_ids {
        for &tid in tag_ids {
            let src: Option<String> = tx
                .query_row(
                    "SELECT source FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
                    rusqlite::params![aid, tid],
                    |r| r.get(0),
                )
                .ok();
            let n = tx.execute(
                "DELETE FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
                rusqlite::params![aid, tid],
            )?;
            if n > 0 {
                let actor = src.unwrap_or_else(|| "manual".to_string());
                tag_ops::record(&tx, aid, tid, "remove", &actor, None)?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// A3（ReplaceAiOnly 重跑）：清掉「AI 生成且未经审核」的自动标签后重打。
/// 删除范围 = `review_state='ai_unreviewed' AND source_batch_id IS NOT NULL`；
/// `ai_reviewed` / `manual` 的行任何重跑都不动（铁律 10）。
/// 无命中幂等返回 0。真实移除写 remove 流水（actor 沿用原来源，可溯源）。
pub fn retag_clear_unreviewed(conn: &Connection, asset_ids: &[i64]) -> AppResult<u64> {
    if asset_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = asset_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT asset_id, tag_id, source FROM asset_tags
          WHERE review_state = 'ai_unreviewed' AND source_batch_id IS NOT NULL
            AND asset_id IN ({placeholders})"
    ))?;
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map(rusqlite::params_from_iter(asset_ids.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    let mut removed: u64 = 0;
    for (aid, tid, src) in rows {
        let n = conn.execute(
            "DELETE FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2
               AND review_state = 'ai_unreviewed' AND source_batch_id IS NOT NULL",
            rusqlite::params![aid, tid],
        )?;
        if n > 0 {
            // R-25：真实移除记 remove 流水（演员 = 原来源；该批次撤销语义已由本操作消费，batch 记 None）
            tag_ops::record(conn, aid, tid, "remove", &src, None)?;
            removed += n as u64;
        }
    }
    Ok(removed)
}

pub fn get_asset_tags(conn: &Connection, asset_id: i64) -> AppResult<Vec<Tag>> {
    // F4：详情恒显示（不过滤停用分面/标签），facet_effective 供 UI 打「已停用」角标
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.name, COALESCE(t.canonical_name,t.name),
                COALESCE(t.normalized_name,lower(trim(t.name))), COALESCE(t.facet_key,'custom'),
                t.parent_id, COALESCE(t.status,'active'), COALESCE(t.is_system,0),
                t.is_preset, t.sort_order, {FACET_EFFECTIVE} AS facet_effective
           FROM asset_tags at JOIN tags t ON t.id = at.tag_id
          WHERE at.asset_id = ?1 AND COALESCE(t.status,'active') != 'blocked'
          ORDER BY t.sort_order, t.id",
    ))?;
    let mut tags = stmt
        .query_map([asset_id], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                canonical_name: r.get(2)?,
                normalized_name: r.get(3)?,
                facet_key: r.get(4)?,
                parent_id: r.get(5)?,
                status: r.get(6)?,
                is_system: r.get::<_, i64>(7)? != 0,
                is_preset: r.get::<_, i64>(8)? != 0,
                sort_order: r.get(9)?,
                facet_effective: r.get::<_, i64>(10)? != 0,
                asset_count: 0,
                total_count: 0,
                aliases: Vec::new(),
                path: String::new(),
            })
        })?
        .collect::<Result<_, _>>()?;
    for tag in &mut tags {
        super::tags::hydrate_metadata(conn, tag)?;
    }
    Ok(tags)
}
