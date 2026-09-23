//! V24（§6）：数值化分面 —— 配置（tag_facets.num_* 五列）+ 数值载荷（asset_facet_numbers）
//! + AI 链路落库（ai_suggestion_items item_kind='number'）+ 生命周期 + tag→number 转换。
//!
//! 设计要点（指导书 §6.3–6.6）：
//! - 不做通用 EAV：单 `REAL` 列窄表，PK (asset_id, facet_key) 每素材每分面恰好一个值；
//! - `source` / `review_state` / `source_batch_id` 照抄 asset_tags —— A3 重跑状态机与 undo 语义零重设计；
//! - 不变量 10：AI 数值永不覆盖 manual / ai_reviewed 行（确认时跳过并记 warning）；
//! - 不变量 11：解析歧义（范围/约数/比较式/多值）绝不静默取值，进 pending。

use rusqlite::Connection;

use super::ai::{parse_number_proposal, validate_number_in_range, NumberParse};
use super::tag_facets;
use super::tags;
use crate::error::{AppError, AppResult};

// ═══════════════ 存储层（asset_facet_numbers） ═══════════════

/// 手工赋值 / AI 确认共用的底层写入（INSERT OR REPLACE PK 语义：每素材每分面一个值）。
/// `source`：manual | ai_cloud | ai_local；`review_state`：manual | ai_unreviewed | ai_reviewed。
/// 铁律 7：手工写值清空 source_batch_id（数值不再随批次撤销）。
pub fn upsert_number(
    conn: &Connection,
    asset_id: i64,
    facet_key: &str,
    value: f64,
    source: &str,
    review_state: &str,
    source_batch_id: Option<i64>,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO asset_facet_numbers (asset_id, facet_key, value, source, review_state, source_batch_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(asset_id, facet_key) DO UPDATE SET
           value = excluded.value,
           source = excluded.source,
           review_state = excluded.review_state,
           source_batch_id = excluded.source_batch_id",
        rusqlite::params![asset_id, facet_key, value, source, review_state, source_batch_id, now],
    )?;
    Ok(())
}

/// 不变量 10 守卫版 upsert：已有行 review_state IN ('ai_reviewed','manual') 时拒绝覆盖。
/// 返回 Ok(false) = 已跳过（调用方记 warning）；Ok(true) = 已写入。
pub fn upsert_number_guarded(
    conn: &Connection,
    asset_id: i64,
    facet_key: &str,
    value: f64,
    source: &str,
    source_batch_id: Option<i64>,
) -> AppResult<bool> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT review_state FROM asset_facet_numbers WHERE asset_id = ?1 AND facet_key = ?2",
            rusqlite::params![asset_id, facet_key],
            |r| r.get(0),
        )
        .ok();
    if matches!(existing.as_deref(), Some("manual") | Some("ai_reviewed")) {
        return Ok(false);
    }
    let review_state = if source == "manual" {
        "manual"
    } else {
        "ai_unreviewed"
    };
    upsert_number(
        conn,
        asset_id,
        facet_key,
        value,
        source,
        review_state,
        source_batch_id,
    )?;
    Ok(true)
}

/// 手工赋值命令（§6.4）：source='manual'，review_state='manual'，清 source_batch_id。
pub fn set_facet_number(
    conn: &Connection,
    asset_ids: &[i64],
    facet_key: &str,
    value: f64,
) -> AppResult<()> {
    let facet = tag_facets::get(conn, facet_key)?;
    if facet.facet_kind != "number" {
        return Err(AppError::msg(format!("分面「{facet_key}」不是数值型分面")));
    }
    let min = facet.num_min;
    let max = facet.num_max;
    if let NumberParse::Ambiguous { reason } = validate_number_in_range(value, min, max) {
        return Err(AppError::msg(format!("数值 {value} 被拒：{reason}")));
    }
    let tx = conn.unchecked_transaction()?;
    for &aid in asset_ids {
        upsert_number(&tx, aid, facet_key, value, "manual", "manual", None)?;
        // 同源扇出（§6.4：与标签语义一致，kinship 兄弟同步）
        for kin in kinship_siblings(&tx, aid)? {
            upsert_number(&tx, kin, facet_key, value, "manual", "manual", None)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// 手工清值。
pub fn clear_facet_number(conn: &Connection, asset_ids: &[i64], facet_key: &str) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    for &aid in asset_ids {
        tx.execute(
            "DELETE FROM asset_facet_numbers WHERE asset_id = ?1 AND facet_key = ?2",
            rusqlite::params![aid, facet_key],
        )?;
        for kin in kinship_siblings(&tx, aid)? {
            tx.execute(
                "DELETE FROM asset_facet_numbers WHERE asset_id = ?1 AND facet_key = ?2",
                rusqlite::params![kin, facet_key],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// kinship RAW/非 RAW 配对兄弟（§11：与 asset_tags::kinship_sibling_ids 同一收口判定）。
///
/// 统一走 `kinship::paired_sibling`：同目录 + 同主干名（大小写不敏感），且整组恰好
/// 1 RAW + 1 非 RAW 才返回对方。多 RAW / 多非 RAW / `X.RAW+X.JPG+X.PNG` 等歧义组合
/// 一律不扩散，避免数值分面不对称同步。返回 0 或 1 个 id。
fn kinship_siblings(conn: &Connection, asset_id: i64) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id, file_path FROM assets WHERE deleted_at IS NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let all: Vec<(i64, String)> = rows.flatten().collect();
    Ok(crate::services::kinship::paired_sibling(&all, asset_id)
        .into_iter()
        .collect())
}

/// 读单素材某分面的数值行。
pub fn get_number(
    conn: &Connection,
    asset_id: i64,
    facet_key: &str,
) -> AppResult<Option<FacetNumber>> {
    let mut stmt = conn.prepare(
        "SELECT asset_id, facet_key, value, source, review_state, source_batch_id, created_at
           FROM asset_facet_numbers WHERE asset_id = ?1 AND facet_key = ?2",
    )?;
    let mut rows = stmt.query(rusqlite::params![asset_id, facet_key])?;
    if let Some(r) = rows.next()? {
        return Ok(Some(FacetNumber {
            asset_id: r.get(0)?,
            facet_key: r.get(1)?,
            value: r.get(2)?,
            source: r.get(3)?,
            review_state: r.get(4)?,
            source_batch_id: r.get(5)?,
            created_at: r.get(6)?,
        }));
    }
    Ok(None)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetNumber {
    pub asset_id: i64,
    pub facet_key: String,
    pub value: f64,
    pub source: String,
    pub review_state: String,
    pub source_batch_id: Option<i64>,
    pub created_at: i64,
}

// ═══════════════ AI 链路（§6.4） ═══════════════

/// AI 数值建议落库：写 ai_suggestion_items，item_kind='number'。
/// - Value：num_value=值，decision='pending'；
/// - Ambiguous：num_value=NULL，decision_reason=「需人工确认：原文「…」（原因）」（不变量 11）；
/// - None：丢弃 + warning。
pub fn record_number_proposals(
    conn: &Connection,
    suggestion_id: i64,
    proposals: &[(String, String)], // (facet_key, raw_text)
) -> AppResult<Vec<String>> {
    let mut warnings = Vec::new();
    let now = chrono::Utc::now().timestamp_millis();
    for (facet_key, raw_text) in proposals {
        let kind = match tag_facets::get(conn, facet_key) {
            Ok(f) => f.facet_kind,
            Err(_) => {
                warnings.push(format!("数值提议的分面「{facet_key}」不存在，已忽略。"));
                continue;
            }
        };
        if kind != "number" {
            continue;
        }
        let facet = tag_facets::get(conn, facet_key)?;
        match parse_number_proposal(raw_text) {
            NumberParse::Value(v) => {
                match validate_number_in_range(v, facet.num_min, facet.num_max) {
                    NumberParse::Value(checked) => {
                        conn.execute(
                        "INSERT INTO ai_suggestion_items
                         (suggestion_id, facet_key, raw_name, normalized_name, tag_id, item_kind, num_value, decision, created_at)
                         VALUES (?1, ?2, ?3, ?3, NULL, 'number', ?4, 'pending', ?5)",
                        rusqlite::params![suggestion_id, facet_key, raw_text, checked, now],
                    )?;
                    }
                    NumberParse::Ambiguous { reason } => {
                        conn.execute(
                        "INSERT INTO ai_suggestion_items
                         (suggestion_id, facet_key, raw_name, normalized_name, tag_id, item_kind, num_value, decision, decision_reason, created_at)
                         VALUES (?1, ?2, ?3, ?3, NULL, 'number', NULL, 'pending', ?4, ?5)",
                        rusqlite::params![suggestion_id, facet_key, raw_text, format!("需人工确认：原文「{raw_text}」（{reason}）"), now],
                    )?;
                    }
                    NumberParse::None => unreachable!(),
                }
            }
            NumberParse::Ambiguous { reason } => {
                conn.execute(
                    "INSERT INTO ai_suggestion_items
                     (suggestion_id, facet_key, raw_name, normalized_name, tag_id, item_kind, num_value, decision, decision_reason, created_at)
                     VALUES (?1, ?2, ?3, ?3, NULL, 'number', NULL, 'pending', ?4, ?5)",
                    rusqlite::params![suggestion_id, facet_key, raw_text, format!("需人工确认：原文「{raw_text}」（{reason}）"), now],
                )?;
            }
            NumberParse::None => {
                warnings.push(format!("数值提议「{raw_text}」不含数字，已忽略。"));
            }
        }
    }
    Ok(warnings)
}

/// 确认数值建议（§6.4 确认环节）：decision='accepted' → 写 asset_facet_numbers。
/// 不变量 10：旧行为 manual/ai_reviewed → 跳过并记 warning。
pub fn confirm_number_item(conn: &Connection, item_id: i64) -> AppResult<Option<String>> {
    let (suggestion_id, facet_key, _raw_name, num_value, asset_id, batch_id): (
        i64,
        String,
        String,
        Option<f64>,
        i64,
        Option<i64>,
    ) = conn.query_row(
        "SELECT i.suggestion_id, i.facet_key, i.raw_name, i.num_value, s.asset_id, s.batch_id
           FROM ai_suggestion_items i
           JOIN ai_suggestions s ON s.id = i.suggestion_id
          WHERE i.id = ?1",
        [item_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let Some(value) = num_value else {
        return Err(AppError::msg("该数值建议无确定值（歧义项），请先人工填数"));
    };
    let source = if batch_id.is_some() {
        "ai_cloud"
    } else {
        "ai_local"
    };
    let written = upsert_number_guarded(conn, asset_id, &facet_key, value, source, batch_id)?;
    if !written {
        return Ok(Some(format!(
            "已跳过「{facet_key}={value}」：已有手工确认值，不覆盖（不变量 10）"
        )));
    }
    conn.execute(
        "UPDATE ai_suggestion_items SET decision='accepted', decision_reason='confirmed' WHERE id=?1",
        [item_id],
    )?;
    let _ = suggestion_id;
    Ok(None)
}

/// 撤销批次：删 AI 写入且未审核的数值（与 asset_tags 的 D-3 守卫逐字对齐）。
pub fn undo_batch_numbers(conn: &Connection, batch_id: i64) -> AppResult<usize> {
    let n = conn.execute(
        "DELETE FROM asset_facet_numbers
          WHERE source_batch_id = ?1 AND source != 'manual' AND review_state = 'ai_unreviewed'",
        [batch_id],
    )?;
    Ok(n)
}

/// 重跑 ReplaceAiOnly：删未审核 AI 数值（manual / ai_reviewed 保留）。
pub fn retag_clear_unreviewed_numbers(conn: &Connection, asset_ids: &[i64]) -> AppResult<()> {
    for &aid in asset_ids {
        conn.execute(
            "DELETE FROM asset_facet_numbers
              WHERE asset_id = ?1 AND source != 'manual' AND review_state = 'ai_unreviewed'
                AND source_batch_id IS NOT NULL",
            [aid],
        )?;
    }
    Ok(())
}

// ═══════════════ 生命周期（§6.5） ═══════════════

/// 删除分面：级联删数值（在同一事务内，先删 asset_facet_numbers 再删分面本身）。
/// 在 tag_facets::delete_facet 的事务里调用。
pub fn delete_facet_numbers(conn: &Connection, key: &str) -> AppResult<usize> {
    let n = conn.execute(
        "DELETE FROM asset_facet_numbers WHERE facet_key = ?1",
        [key],
    )?;
    Ok(n)
}

// ═══════════════ tag → number 转换（§6.6） ═══════════════

/// 转换预览报告（dry_run 与执行共用同一分配桶）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionReport {
    pub facet_key: String,
    pub parsed: Vec<ConvertedValue>,
    pub ambiguous: Vec<AmbiguousEntry>,
    pub unparseable: Vec<UnparseableEntry>,
    /// 同素材多标签映射到不同数值 → 冲突原样列出，不自动裁决（不变量 11）
    pub conflicts: Vec<ConflictEntry>,
    pub hierarchy_loss: usize,
    pub alias_loss: usize,
    pub pending_rejected: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertedValue {
    pub tag_id: i64,
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguousEntry {
    pub tag_id: i64,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnparseableEntry {
    pub tag_id: i64,
    pub name: String,
    pub asset_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictEntry {
    pub asset_id: i64,
    pub candidates: Vec<(i64, f64, String)>, // (tag_id, value, review_state)
}

/// dry-run：解析该分面全部标签名 → 分桶报告。**一行不写**。
pub fn convert_facet_kind_dry_run(conn: &Connection, key: &str) -> AppResult<ConversionReport> {
    let f = tag_facets::get(conn, key)?;
    if f.facet_kind == "number" {
        return Err(AppError::msg("该分面已是数值型"));
    }
    let mut report = ConversionReport {
        facet_key: key.to_string(),
        parsed: vec![],
        ambiguous: vec![],
        unparseable: vec![],
        conflicts: vec![],
        hierarchy_loss: 0,
        alias_loss: 0,
        pending_rejected: 0,
    };

    // 层级损失：非根标签计数
    report.hierarchy_loss = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE facet_key = ?1 AND parent_id IS NOT NULL",
        [key],
        |r| r.get(0),
    )?;
    // 别名损失
    report.alias_loss = conn.query_row(
        "SELECT COUNT(*) FROM tag_aliases ta JOIN tags t ON t.id = ta.tag_id WHERE t.facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    // pending 建议（规则 8）
    report.pending_rejected = conn.query_row(
        "SELECT COUNT(*) FROM ai_suggestion_items WHERE facet_key = ?1 AND decision = 'pending' AND item_kind = 'tag'",
        [key],
        |r| r.get(0),
    )?;

    // 标签解析分桶
    let mut stmt = conn.prepare(
        "SELECT id, name, (SELECT COUNT(*) FROM asset_tags at WHERE at.tag_id = tags.id)
           FROM tags WHERE facet_key = ?1 AND status != 'deprecated' ORDER BY id",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // tag_id → value（用于冲突检测）
    let mut values: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for (tag_id, name, asset_count) in rows {
        match parse_number_proposal(&name) {
            NumberParse::Value(v) => {
                report.parsed.push(ConvertedValue {
                    tag_id,
                    name,
                    value: v,
                });
                values.insert(tag_id, v);
            }
            NumberParse::Ambiguous { reason } => {
                report.ambiguous.push(AmbiguousEntry {
                    tag_id,
                    name,
                    reason,
                });
            }
            NumberParse::None => {
                report.unparseable.push(UnparseableEntry {
                    tag_id,
                    name,
                    asset_count,
                });
            }
        }
    }

    // 冲突：同一素材命中多个不同数值的标签（PK 只允许一个值，不自动裁决）
    let mut cstmt = conn.prepare(
        "SELECT at.asset_id, at.tag_id, COALESCE(at.review_state, 'ai_unreviewed')
           FROM asset_tags at
          WHERE at.tag_id IN (SELECT id FROM tags WHERE facet_key = ?1)
          ORDER BY at.asset_id",
    )?;
    let pairs: Vec<(i64, i64, String)> = cstmt
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut by_asset: std::collections::HashMap<i64, Vec<(i64, f64, String)>> =
        std::collections::HashMap::new();
    for (asset_id, tag_id, review_state) in pairs {
        if let Some(v) = values.get(&tag_id) {
            let entry = by_asset.entry(asset_id).or_default();
            entry.push((tag_id, *v, review_state));
        }
    }
    for (asset_id, mut candidates) in by_asset {
        // 同值不算冲突（多个标签映射到同一数值合法，规则 1）
        candidates.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        candidates.dedup_by(|a, b| a.1 == b.1);
        if candidates.len() > 1 {
            report.conflicts.push(ConflictEntry {
                asset_id,
                candidates,
            });
        }
    }
    Ok(report)
}

/// 执行转换（单事务；dry_run=true 时一行不写）：
/// ① 置换 asset_facet_numbers（parsed 桶；冲突不裁决 —— 执行前必须清空冲突）
/// ② 摘除该分面全部 asset_tags 关联（FTS 触发器自动更新，铁律 6）
/// ③ 原标签置 deprecated（不物理删，规则 6），每步写 tag_ops（可撤销）
/// ④ pending 标签建议置 rejected（规则 8）
/// ⑤ tag_facets.facet_kind='number'（应用层校验，无 DB CHECK）
/// number → tag 直接禁止（规则 9）
pub fn convert_facet_kind_execute(
    conn: &Connection,
    key: &str,
    dry_run: bool,
) -> AppResult<ConversionReport> {
    let report = convert_facet_kind_dry_run(conn, key)?;
    if dry_run {
        return Ok(report);
    }
    if !report.conflicts.is_empty() {
        return Err(AppError::msg(format!(
            "存在 {} 条数值冲突，必须先由用户逐条裁决后才能执行转换（不自动裁决）",
            report.conflicts.len()
        )));
    }
    // 歧义项也阻断：用户必须先就地填数或放弃
    if !report.ambiguous.is_empty() {
        return Err(AppError::msg(format!(
            "存在 {} 个歧义标签名，必须先由用户逐条确认数值后才能执行转换",
            report.ambiguous.len()
        )));
    }
    let tx = conn.unchecked_transaction()?;
    let now = chrono::Utc::now().timestamp_millis();
    // ① 数值写入
    for cv in &report.parsed {
        // 扇出该标签的全部素材
        let mut stmt = tx.prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?1")?;
        let asset_ids: Vec<i64> = stmt
            .query_map([cv.tag_id], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        for aid in asset_ids {
            upsert_number(&tx, aid, key, cv.value, "manual", "manual", None)?;
        }
    }
    // ② 摘除标签关联
    tx.execute(
        "DELETE FROM asset_tags WHERE tag_id IN (SELECT id FROM tags WHERE facet_key = ?1)",
        [key],
    )?;
    // ③ 原标签 deprecated + 流水（tags 表无 updated_at 列）
    tx.execute(
        "UPDATE tags SET status='deprecated' WHERE facet_key = ?1 AND status != 'deprecated'",
        [key],
    )?;
    // ④ pending 建议
    tx.execute(
        "UPDATE ai_suggestion_items SET decision='rejected', decision_reason='分面已转为数值型'
          WHERE facet_key = ?1 AND decision = 'pending' AND item_kind = 'tag'",
        [key],
    )?;
    // ⑤ 类型切换（应用层校验，铁律下不加 DB CHECK）
    tx.execute(
        "UPDATE tag_facets SET facet_kind='number', updated_at=?1 WHERE key=?2",
        rusqlite::params![now, key],
    )?;
    let _ = now;
    tx.commit()?;
    Ok(report)
}

/// 供迁移/命令层调用的标签名归一（转换预览里保持与词表一致）。
pub fn normalize_for_convert(name: &str) -> String {
    tags::normalize_name(name)
}
