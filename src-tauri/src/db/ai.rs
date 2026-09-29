//! AI 打标：批次与建议 CRUD + 确认流（确认才写 asset_tags，防污染标签体系）

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{asset_tags, tag_facets, tags};
use crate::error::AppResult;

/// 返回仍待处理或执行中的 AI 批次数量。
pub fn count_pending_or_processing(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM ai_batches WHERE status IN ('pending', 'processing')",
        [],
        |row| row.get(0),
    )?)
}

/// 分类标签：{ 分类名: [标签...] }（PRD 5.5；BTreeMap 保证序列化键序稳定）
pub type CategorizedTags = std::collections::BTreeMap<String, Vec<String>>;

/// A1：AI 打标的单条标签提议 —— 取代「往 Vec<String> 里塞 JSON 字符串」的隐式协议。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TagProposal {
    /// 已经过 resolve_facet_key 归一的分面 key
    pub facet_key: String,
    /// 模型原始输出（未 normalize）
    pub raw_name: String,
    /// None = 模型未给
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PeoplePresenceStatus {
    Present,
    Absent,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PeoplePresence {
    pub status: PeoplePresenceStatus,
    pub confidence: f32,
}

impl Default for PeoplePresence {
    fn default() -> Self {
        Self {
            status: PeoplePresenceStatus::Unknown,
            confidence: 0.0,
        }
    }
}

fn is_unknown_people_presence(value: &PeoplePresence) -> bool {
    value == &PeoplePresence::default()
}

/// A1：单图分析结果（强类型）。warnings 回传前端（R2-1），不只进日志。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResult {
    pub description: String,
    /// 旧分析 JSON 读取兼容；新结果缺省时不再序列化此业务字段。
    #[serde(default, skip_serializing_if = "is_unknown_people_presence")]
    pub people_presence: PeoplePresence,
    pub proposals: Vec<TagProposal>,
    /// V24（§6.3④）：数值分面提议（平行字段，不改 CategorizedTags 形状）
    #[serde(default)]
    pub numbers: Vec<NumberProposal>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// V24（§6.3④）：数值提议 —— AI 对数值分面的单条建议。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NumberProposal {
    /// 已过 resolve_facet_key 归一的分面 key
    pub facet_key: String,
    /// 模型原始输出（"5" / "5人" / "大约5"）
    pub raw_text: String,
    /// 解析并按 num_min/num_max 校验后的值
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// V24（§6.5）：数值解析三态 —— 不变量 11：有歧义绝不静默取首个数字。
#[derive(Debug, Clone, PartialEq)]
pub enum NumberParse {
    /// 无歧义单值 → 可落建议
    Value(f64),
    /// 有歧义 → 进 pending / 转换预览「待确认」桶
    Ambiguous { reason: String },
    /// 完全没有数字
    None,
}

/// 约数限定词表（§6.5「数值解析的严格规则」）
const APPROX_WORDS: &[&str] = &["约", "大约", "左右", "上下", "approximately", "approx"];
/// 比较式限定词表
const COMPARISON_WORDS: &[&str] = &[
    "不少于",
    "不多于",
    "不超过",
    "超过",
    "以上",
    "以下",
    "至少",
    "最多",
    "大于",
    "小于",
    "多于",
    "少于",
];

/// 提取文本中的全部数字 token（支持整数/小数/负号）。
fn extract_number_tokens(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();
    let bytes: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() || (c == '-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let start = i;
            let mut seen_dot = false;
            let mut j = i + 1;
            while j < bytes.len() {
                let d = bytes[j];
                if d.is_ascii_digit() {
                    j += 1;
                } else if d == '.'
                    && !seen_dot
                    && j + 1 < bytes.len()
                    && bytes[j + 1].is_ascii_digit()
                {
                    seen_dot = true;
                    j += 1;
                } else {
                    break;
                }
            }
            let s: String = bytes[start..j].iter().collect();
            if let Ok(v) = s.parse::<f64>() {
                out.push(v);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// V24（§6.5）：数值解析严格规则 —— 同一函数同时服务 AI 落建议与 tag→number 转换预览。
///
/// | 输入 | 结果 |
/// |---|---|
/// | `"5"` `"5.0"` `"05"` | `Value(5.0)`（归一一致） |
/// | `"5人"` `"5 人"` `"人数5"` | `Value(5.0)`（恰好一个数字 token + 纯标签性文字） |
/// | `"-3"` | `Value(-3.0)`（负数合法；越界由 num_min/max 判） |
/// | `"5~6"` `"5-6"` `"5 到 6"` | `Ambiguous("范围")` |
/// | `"约5"` `"大约 5"` `"5左右"` | `Ambiguous("约数")` |
/// | `"不少于5"` `"超过5"` | `Ambiguous("比较式")` |
/// | `"3或4"` `"3、4"` | `Ambiguous("多值")` |
/// | `"很多"` | `None` |
pub fn parse_number_proposal(raw: &str) -> NumberParse {
    let s = raw.trim();
    if s.is_empty() {
        return NumberParse::None;
    }
    let lower = s.to_lowercase();
    // ① 比较式（先判，避免「以上」等词被当普通文字）
    if COMPARISON_WORDS.iter().any(|w| lower.contains(w)) {
        return NumberParse::Ambiguous {
            reason: "比较式".into(),
        };
    }
    // ② 约数
    if APPROX_WORDS.iter().any(|w| lower.contains(w)) {
        return NumberParse::Ambiguous {
            reason: "约数".into(),
        };
    }
    // ③ 范围：数字间夹着范围符号/范围词（「5~6」「5-6」「5 到 6」「5至6」「5—6」「5~ 6」）
    let tokens = extract_number_tokens(s);
    if tokens.len() >= 2 {
        // 两个以上数字 token 一定多值；两个则看中间是否范围词
        let range_words = ["~", "～", "—", "–", "到", "至", "-"];
        let between: String = {
            // 取第一个数字末尾到第二个数字开头之间的片段
            let chars: Vec<char> = s.chars().collect();
            let mut first_end = 0usize;
            let mut idx = 0usize;
            let mut found = 0usize;
            while idx < chars.len() {
                let c = chars[idx];
                if c.is_ascii_digit() && found == 0 {
                    let mut j = idx;
                    while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                        j += 1;
                    }
                    first_end = j;
                    found += 1;
                    idx = j;
                    continue;
                }
                if c.is_ascii_digit() && found >= 1 {
                    let mut seg = String::new();
                    for ch in &chars[first_end..idx] {
                        seg.push(*ch);
                    }
                    if range_words.iter().any(|w| seg.trim().contains(w)) {
                        return NumberParse::Ambiguous {
                            reason: "范围".into(),
                        };
                    }
                    break;
                }
                idx += 1;
            }
            String::new()
        };
        let _ = between;
        return NumberParse::Ambiguous {
            reason: "多值".into(),
        };
    }
    // ④ 单数字 + 剩余文字必须是纯标签性文字（字母数字中文，不含其他数字 —— 已由 tokens.len()==1 保证）
    if tokens.len() == 1 {
        // 剩余文字允许：中文/字母/空格/单位（数字已剥离）；只要不含范围/歧义符号即可
        return NumberParse::Value(tokens[0]);
    }
    NumberParse::None
}

/// V24：按分面配置校验值域（越界 → Ambiguous「超出范围」，绝不裁到边界）。
pub fn validate_number_in_range(
    value: f64,
    num_min: Option<f64>,
    num_max: Option<f64>,
) -> NumberParse {
    if !value.is_finite() {
        return NumberParse::Ambiguous {
            reason: "超出范围".into(),
        };
    }
    if let Some(lo) = num_min {
        if value < lo {
            return NumberParse::Ambiguous {
                reason: format!(
                    "超出范围 {}–{}",
                    num_min
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "−∞".into()),
                    num_max
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "+∞".into())
                ),
            };
        }
    }
    if let Some(hi) = num_max {
        if value > hi {
            return NumberParse::Ambiguous {
                reason: format!(
                    "超出范围 {}–{}",
                    num_min
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "−∞".into()),
                    num_max
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "+∞".into())
                ),
            };
        }
    }
    NumberParse::Value(value)
}

/// A1：AnalysisResult → 传统 CategorizedTags（描述 + 标签树）。
/// 仅兼容历史持久化/前端契约；新写入建议用 proposals 的 typed 语义。
impl AnalysisResult {
    pub fn to_categorized(&self) -> CategorizedTags {
        let mut out = CategorizedTags::new();
        for p in &self.proposals {
            out.entry(p.facet_key.clone())
                .or_default()
                .push(p.raw_name.clone());
        }
        out
    }
}

/// 宽容解析历史数据：旧格式是扁平数组 → 收进「未分类」；新格式是分类对象
pub fn parse_tags_json(raw: &str) -> CategorizedTags {
    let v: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
    if let Some(arr) = v.as_array() {
        let tags: Vec<String> = arr
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        return if tags.is_empty() {
            CategorizedTags::new()
        } else {
            CategorizedTags::from([("未分类".to_string(), tags)])
        };
    }
    serde_json::from_value(v).unwrap_or_default()
}

/// 分类标签写入标签树：分类建/复用父标签，标签建/复用子标签，返回全部子标签 id
fn categorized_tag_ids(conn: &Connection, tags: &CategorizedTags) -> AppResult<Vec<i64>> {
    let mut ids = Vec::new();
    for (category, names) in tags {
        let category = category.trim();
        if category.is_empty() {
            continue;
        }
        // W2-10：分面 key 路由唯一入口 —— DB 有该 key（自建分面）原样用；
        // 中文旧名命中映射；都不中落 custom。旧代码直接查映射表，自建分面永远落 custom。
        let (facet_key, _resolved) = tag_facets::resolve_facet_key(conn, category)?;
        for name in names {
            let name = name.trim();
            if !name.is_empty() {
                // 旧 AI 协议传中文分类名时保留根节点兼容；新协议传稳定 facet key 时直接创建规范标签。
                if category.trim() == facet_key {
                    ids.push(tags::find_or_create_canonical(conn, &facet_key, name)?);
                } else {
                    let parent = tags::find_or_create_facet_root(conn, &facet_key, category)?;
                    ids.push(tags::find_or_create_child(conn, parent, name)?);
                }
            }
        }
    }
    Ok(ids)
}

/// A3：重跑模式。任何模式下都**永不删除** `review_state IN ('ai_reviewed','manual')` 的行（铁律 10）。
/// 该枚举承载重跑意图；落库语义由 `create_batch_with_retag` 的 ReplaceAiOnly 分支执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetagMode {
    /// 只加新标签，已有一律不动（默认，最安全）
    Append,
    /// 清掉「AI 生成且未经审核」的标签后重打
    /// （`WHERE review_state = 'ai_unreviewed' AND source_batch_id IS NOT NULL`）
    ReplaceAiOnly,
    /// 只生成建议不写入，用户在工作台逐条审核（当前产品即此形态：确认才落库）
    ReviewOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiBatch {
    pub id: i64,
    pub status: String, // pending|processing|done|cancelled
    pub mode: String,   // cloud|local；manual 仅用于读取历史批次
    pub total: i64,
    pub processed: i64,
    pub confirmed: i64,
    pub created_at: i64,
    /// R2-3：建批时合并的同源组数（原始 ids 数 − 去重后数）；打标页提示「已合并 N 组同源文件」
    #[serde(default)]
    pub merged_groups: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSuggestion {
    pub id: i64,
    pub batch_id: i64,
    pub asset_id: i64,
    pub asset_path: String,
    /// B-3/B-4：素材 MIME（供前端判断批次是否含视频、是否需提示开启视频打标）
    pub mime_type: Option<String>,
    pub suggested_tags: CategorizedTags,
    pub status: String, // pending|confirmed|rejected|modified
    pub confirmed_tags: CategorizedTags,
    /// 单条打标失败原因（v6：失败详情落库，前端可展示，不再只看到 rejected）
    pub last_error: Option<String>,
    pub created_at: i64,
    // FB5-05（§7.6）：一句话描述。AI 建议值 / 审核后确认值 / 素材当前值。
    #[serde(default)]
    pub suggested_description: String,
    pub confirmed_description: Option<String>,
    #[serde(default)]
    pub current_description: String,
    /// 是否已有一次完整分析结果（用于续跑判定，不返回前端）。
    #[serde(skip)]
    pub has_analysis: bool,
}

fn batch_from_row(r: &rusqlite::Row) -> rusqlite::Result<AiBatch> {
    Ok(AiBatch {
        id: r.get(0)?,
        status: r.get(1)?,
        mode: r.get(2)?,
        total: r.get(3)?,
        processed: r.get(4)?,
        confirmed: r.get(5)?,
        created_at: r.get(6)?,
        // merged_groups 不落库（派生量）：持久化读取回 0，建批返回时由创建路径填
        merged_groups: 0,
    })
}

const BATCH_COLS: &str = "id, status, mode, total, processed, confirmed, created_at";

pub fn create_batch(conn: &Connection, asset_ids: &[i64], mode: &str) -> AppResult<AiBatch> {
    create_batch_with_retag(conn, asset_ids, mode, RetagMode::Append)
}

/// A3：带重跑模式建批。ReplaceAiOnly 在建批同一事务内先清掉这批素材的
/// 「AI 生成且未经审核」标签（`ai_reviewed`/`manual` 不动）——新一批确认后旧
/// 候选被替换，而不是叠加成两份同样结论。ReviewOnly/Append 不触碰既有标签。
pub fn create_batch_with_retag(
    conn: &Connection,
    asset_ids: &[i64],
    mode: &str,
    retag: RetagMode,
) -> AppResult<AiBatch> {
    let now = chrono::Utc::now().timestamp_millis();
    let tx = conn.unchecked_transaction()?;
    // W5h-c：同源组内只保留一个代表（非 RAW 优先——JPG 有内嵌预览、解码快）。
    // 代表确认后标签经 assign_inner 自动同步给 RAW → 最终两条都有标签。
    // 批次 total 记去重后数量（与 ai_suggestions 行数一致）。
    // 复用 sync_tags_to_siblings 同一开关：关掉则不去重不同步（回到独立行为）。
    let effective_ids: Vec<i64> = {
        let sync = super::settings::get_settings(&tx)
            .map(|s| s.appearance.kinship.sync_tags_to_siblings)
            .unwrap_or(true);
        if !sync {
            asset_ids.to_vec()
        } else {
            // 读取全库完整同源组；只有恰好 1 RAW + 1 非 RAW 且两者均被选中时，才折叠 RAW。
            let mut stmt =
                tx.prepare("SELECT id, file_path FROM assets WHERE deleted_at IS NULL")?;
            let rows: Vec<(i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);

            let mut groups: std::collections::HashMap<String, Vec<(i64, bool)>> =
                Default::default();
            for (id, path) in &rows {
                let (key, is_raw) = crate::services::kinship::kinship_key(path);
                groups.entry(key).or_default().push((*id, is_raw));
            }

            let mut drop_ids: std::collections::HashSet<i64> = Default::default();
            for members in groups.values() {
                if let crate::services::kinship::KinshipGroup::Paired { raw, non_raw } =
                    crate::services::kinship::classify_kinship_group(members)
                {
                    // 只选中 RAW 时不能凭空丢掉用户选择；两者都选中才将 RAW 作为同步代表折叠。
                    if asset_ids.contains(&raw) && asset_ids.contains(&non_raw) {
                        drop_ids.insert(raw);
                    }
                }
            }

            // 保持用户传入顺序，只移除已确认可折叠的 RAW。
            asset_ids
                .iter()
                .copied()
                .filter(|id| !drop_ids.contains(id))
                .collect()
        }
    };
    tx.execute(
        "INSERT INTO ai_batches (status, mode, total, created_at) VALUES ('pending', ?1, ?2, ?3)",
        rusqlite::params![mode, effective_ids.len() as i64, now],
    )?;
    let batch_id = tx.last_insert_rowid();
    for &aid in &effective_ids {
        tx.execute(
            "INSERT INTO ai_suggestions (batch_id, asset_id, suggested_tags, created_at)
             VALUES (?1, ?2, '[]', ?3)",
            rusqlite::params![batch_id, aid, now],
        )?;
    }
    // A3：ReplaceAiOnly —— 同事务清旧「未审核 AI 标签」，新批确认后即替换
    //（ai_reviewed/manual 永不触碰，铁律 10 由 asset_tags::retag_clear_unreviewed 守卫）
    if retag == RetagMode::ReplaceAiOnly {
        asset_tags::retag_clear_unreviewed(&tx, &effective_ids)?;
        // V24（§6.4）：数值同步重跑 —— manual / ai_reviewed 数值行保留
        crate::db::facet_numbers::retag_clear_unreviewed_numbers(&tx, &effective_ids)?;
    }
    tx.commit()?;
    let mut batch = get_batch(conn, batch_id)?;
    // R2-3：合并组数 = 原始选择 − 去重后代表数（让用户知道省了什么）
    batch.merged_groups = (asset_ids.len() as i64 - effective_ids.len() as i64).max(0);
    Ok(batch)
}

pub fn get_batch(conn: &Connection, id: i64) -> AppResult<AiBatch> {
    Ok(conn.query_row(
        &format!("SELECT {BATCH_COLS} FROM ai_batches WHERE id = ?1"),
        [id],
        batch_from_row,
    )?)
}

pub fn list_batches(conn: &Connection) -> AppResult<Vec<AiBatch>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BATCH_COLS} FROM ai_batches ORDER BY id DESC"
    ))?;
    let rows = stmt
        .query_map([], batch_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_batch_status(conn: &Connection, id: i64, status: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_batches SET status = ?1 WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    Ok(())
}

/// 批次执行时按当前 tagging 用途连接修正实际部署类型。
/// 允许用户建批后切换在线/本地服务，下一次开始或续跑立即使用新连接。
pub fn set_batch_mode(conn: &Connection, id: i64, mode: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_batches SET mode = ?1 WHERE id = ?2",
        rusqlite::params![mode, id],
    )?;
    Ok(())
}

/// 应用启动/任务中断时：把遗留的 processing 批次置为 interrupted（指导书阶段 5 §8.2）。
/// 允许一键续跑剩余 pending（避免僵尸 processing 态无法重试）。
pub fn mark_interrupted_batches(conn: &Connection) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_batches SET status = 'interrupted' WHERE status = 'processing'",
        [],
    )?;
    Ok(())
}

pub fn inc_batch_processed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_batches SET processed = processed + 1 WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// FB5-05（§7.6）：写/覆盖某条建议的 AI 候选结果（标签 + 一句话描述）。
/// tagging_service 用；描述为空也照写（空描述不导致有效标签整条失败）。
pub fn set_suggestion_result(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    description: &str,
) -> AppResult<()> {
    set_suggestion_tags(conn, id, tags)?;
    conn.execute(
        "UPDATE ai_suggestions SET suggested_description = ?1 WHERE id = ?2",
        rusqlite::params![description, id],
    )?;
    Ok(())
}

/// 写/覆盖某条建议的 AI 候选标签。
/// A1：typed 收口 —— items 一律由 proposals 驱动（facet_key/raw_name/confidence 强类型，
/// 名字字符串里不再二次解析 {"t","c"}，那是隐式协议的死代码，已删除）。
pub fn set_suggestion_tags(conn: &Connection, id: i64, tags: &CategorizedTags) -> AppResult<()> {
    // 兼容调用方：把 plain CategorizedTags 转成无置信度提议
    let mut proposals = Vec::new();
    for (category, names) in tags {
        let (facet_key, _) = tag_facets::resolve_facet_key(conn, category)?;
        for name in names {
            let n = name.trim();
            if n.is_empty() {
                continue;
            }
            proposals.push(TagProposal {
                facet_key: facet_key.clone(),
                raw_name: n.to_string(),
                confidence: None,
            });
        }
    }
    replace_suggestion_items(conn, id, tags, &proposals)
}

/// 公共 items 写入：suggested_tags JSON + 清空重建 items + tag_id 反查 + 近似提示（F6-b）+ confidence。
fn replace_suggestion_items(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    proposals: &[TagProposal],
) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_suggestions SET suggested_tags = ?1 WHERE id = ?2",
        rusqlite::params![serde_json::to_string(tags)?, id],
    )?;
    conn.execute(
        "DELETE FROM ai_suggestion_items WHERE suggestion_id = ?1",
        [id],
    )?;
    let now = chrono::Utc::now().timestamp_millis();
    for p in proposals {
        let raw = p.raw_name.trim();
        if raw.is_empty() {
            continue;
        }
        let normalized = tags::normalize_name(raw);
        // F3-a：tag_id 反查收敛到 find_by_term（mode=Alias）
        let mut decision_reason: Option<String> = None;
        let tag_id: Option<i64> =
            tags::find_by_term(conn, &p.facet_key, &normalized, tags::TermMatch::Alias)
                .ok()
                .and_then(|l| l.hits.into_iter().next())
                .map(|h| h.tag_id);
        // F6-b：词表里没有精确命中 → 近似匹配「只提示，不自动改写」
        if tag_id.is_none() {
            if let Some((_, owner, reason)) =
                tags::find_similar_tag(conn, &p.facet_key, &normalized)?
            {
                decision_reason = Some(match reason {
                    tags::SimilarReason::Substring => format!("疑似与「{owner}」重复"),
                    tags::SimilarReason::Spell => format!("拼写相近：「{owner}」"),
                });
            }
        }
        let confidence = p.confidence.map(|c| c as f64);
        conn.execute(
            "INSERT INTO ai_suggestion_items
             (suggestion_id, facet_key, raw_name, normalized_name, tag_id, confidence, decision, decision_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
            rusqlite::params![id, p.facet_key, raw, normalized, tag_id, confidence, decision_reason, now],
        )?;
    }
    Ok(())
}

/// A1：typed 版本的 set_suggestion_result（runner 用）——tags/描述照旧，confidence 走 proposals。
pub fn set_suggestion_result_typed(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    proposals: &[TagProposal],
    description: &str,
) -> AppResult<()> {
    replace_suggestion_items(conn, id, tags, proposals)?;
    conn.execute(
        "UPDATE ai_suggestions SET suggested_description = ?1 WHERE id = ?2",
        rusqlite::params![description, id],
    )?;
    Ok(())
}

/// AI 结果进入人工审核前的唯一阈值策略。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfidencePolicy {
    /// confidence < 此值不入库（连 pending 都不进）；默认 0.30
    pub min_suggest: f64,
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self { min_suggest: 0.30 }
    }
}

/// 带置信度策略的建议写入（runner 用，取代无策略的 set_suggestion_result_typed）。
///  - confidence < min_suggest → 不入库（连 pending 都不进，suggested_tags 同步剔除）
///  - 精确命中 canonical/synonym → 记录 tag_id，但仍保持 pending
///  - 近似命中 → pending + decision_reason（F6-b 只提示，绝不自动改写）
///  - 完全新词 → pending，等待用户在确认流程中处理
pub fn set_suggestion_result_policy(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    proposals: &[TagProposal],
    description: &str,
    policy: &ConfidencePolicy,
) -> AppResult<usize> {
    // 先按阈值裁剪：低置信词既不入库也不留在 suggested_tags（确认全部不会复活它）
    let filtered_tags: CategorizedTags = {
        let mut out: CategorizedTags = CategorizedTags::new();
        for (category, names) in tags {
            let mut kept = Vec::new();
            'names: for n in names {
                for p in proposals {
                    let below = p
                        .confidence
                        .map(|c| (c as f64) < policy.min_suggest)
                        .unwrap_or(false);
                    if below && p.raw_name.trim() == n.trim() {
                        continue 'names;
                    }
                }
                kept.push(n.clone());
            }
            if !kept.is_empty() {
                out.insert(category.clone(), kept);
            }
        }
        out
    };
    conn.execute(
        "UPDATE ai_suggestions SET suggested_tags = ?1 WHERE id = ?2",
        rusqlite::params![serde_json::to_string(&filtered_tags)?, id],
    )?;
    conn.execute(
        "DELETE FROM ai_suggestion_items WHERE suggestion_id = ?1",
        [id],
    )?;
    conn.execute(
        "UPDATE ai_suggestions SET suggested_description = ?1 WHERE id = ?2",
        rusqlite::params![description, id],
    )?;

    let now = chrono::Utc::now().timestamp_millis();
    let mut blocked_low_confidence = 0usize;
    for p in proposals {
        let raw = p.raw_name.trim();
        if raw.is_empty() {
            continue;
        }
        let normalized = tags::normalize_name(raw);
        let low_conf = p
            .confidence
            .map(|c| (c as f64) < policy.min_suggest)
            .unwrap_or(false);
        if low_conf {
            blocked_low_confidence += 1;
            continue; // 低置信：不入库（连 pending 都不进）
        }
        let mut decision_reason: Option<String> = None;
        // F3-a：精确反查（canonical/synonym 命中 → tag_id Some）
        let tag_id: Option<i64> =
            tags::find_by_term(conn, &p.facet_key, &normalized, tags::TermMatch::Alias)
                .ok()
                .and_then(|l| l.hits.into_iter().next())
                .map(|h| h.tag_id);
        // 近似命中：只提示不自动改写（F6-b）
        if tag_id.is_none() {
            if let Some((_, owner, reason)) =
                tags::find_similar_tag(conn, &p.facet_key, &normalized)?
            {
                decision_reason = Some(match reason {
                    tags::SimilarReason::Substring => format!("疑似与「{owner}」重复"),
                    tags::SimilarReason::Spell => format!("拼写相近：「{owner}」"),
                });
            }
        }
        let confidence = p.confidence.map(|c| c as f64);
        conn.execute(
            "INSERT INTO ai_suggestion_items
             (suggestion_id, facet_key, raw_name, normalized_name, tag_id, confidence, decision, decision_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
            rusqlite::params![
                id, p.facet_key, raw, normalized, tag_id, confidence, decision_reason, now
            ],
        )?;
    }
    Ok(blocked_low_confidence)
}

/// A4 + V24：数值提议落库（§6.4）—— 建议项写入后追加 item_kind='number' 的条目。
/// 解析（含歧义进 pending）与越界校验收口在 facet_numbers::record_number_proposals。
pub fn record_number_proposals_for_suggestion(
    conn: &Connection,
    id: i64,
    numbers: &[NumberProposal],
) -> AppResult<Vec<String>> {
    let pairs: Vec<(String, String)> = numbers
        .iter()
        .map(|n| (n.facet_key.clone(), n.raw_text.clone()))
        .collect();
    crate::db::facet_numbers::record_number_proposals(conn, id, &pairs)
}

/// 逐字写模型原始返回 + AnalysisResult 序列化及协议版本。
pub fn set_suggestion_provenance(
    conn: &Connection,
    id: i64,
    raw_response: &str,
    analysis_json: &str,
    analysis_schema_version: i64,
) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_suggestions
            SET raw_response = ?1, analysis_json = ?2, analysis_schema_version = ?3
          WHERE id = ?4",
        rusqlite::params![raw_response, analysis_json, analysis_schema_version, id],
    )?;
    Ok(())
}

/// A2：写批次级请求溯源（F1-g 列）——档案/模型标识 + 提示词版本 + 请求配置 JSON 及其稳定 hash。
/// request_config_hash 由服务层对六项配置 JSON 做键排序稳定序列化后 sha256 前 16 位。
// 8 参数为溯源记录字段的内聚集合，收进结构体需同步改全部调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn set_batch_provenance(
    conn: &Connection,
    batch_id: i64,
    model_id: &str,
    model_version: Option<&str>,
    profile_id: &str,
    prompt_version: &str,
    request_config_hash: &str,
    request_config_json: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_batches
            SET model_id = ?1, model_version = ?2, profile_id = ?3,
                prompt_version = ?4, request_config_hash = ?5, request_config_json = ?6
          WHERE id = ?7",
        rusqlite::params![
            model_id,
            model_version,
            profile_id,
            prompt_version,
            request_config_hash,
            request_config_json,
            batch_id
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSuggestionItem {
    pub id: i64,
    pub suggestion_id: i64,
    pub facet_key: String,
    pub raw_name: String,
    pub normalized_name: String,
    pub tag_id: Option<i64>,
    pub confidence: Option<f64>,
    pub decision: String,
    pub decision_reason: Option<String>,
    pub created_at: i64,
    /// V24（§6.3③）：tag | number（数值建议项）
    #[serde(default)]
    pub item_kind: String,
    /// V24：item_kind='number' 时为确认值；歧义项为 NULL（需人工填数）
    #[serde(default)]
    pub num_value: Option<f64>,
}

fn suggestion_from_row(r: &rusqlite::Row) -> rusqlite::Result<AiSuggestion> {
    let suggested: String = r.get(4)?;
    let confirmed: Option<String> = r.get(6)?;
    let last_error: Option<String> = r.get(8)?;
    Ok(AiSuggestion {
        id: r.get(0)?,
        batch_id: r.get(1)?,
        asset_id: r.get(2)?,
        asset_path: r.get(3)?,
        mime_type: r.get(9)?,
        suggested_tags: parse_tags_json(&suggested),
        status: r.get(5)?,
        confirmed_tags: confirmed.map(|s| parse_tags_json(&s)).unwrap_or_default(),
        last_error,
        created_at: r.get(7)?,
        // FB5-05（§7.6）：suggested_description(10) / confirmed_description(11) / current_description(12)
        suggested_description: r.get(10)?,
        confirmed_description: r.get(11)?,
        current_description: r.get(12)?,
        has_analysis: r.get(13)?,
    })
}

const SUGG_COLS: &str = "s.id, s.batch_id, s.asset_id, a.file_path, s.suggested_tags, s.status, \
                         s.confirmed_tags, s.created_at, s.last_error, a.mime_type, \
                         s.suggested_description, s.confirmed_description, a.content_description, \
                         (COALESCE(s.raw_response, '') != '' OR COALESCE(s.analysis_json, '') != '')";

pub fn list_suggestions(conn: &Connection, batch_id: i64) -> AppResult<Vec<AiSuggestion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SUGG_COLS} FROM ai_suggestions s JOIN assets a ON a.id = s.asset_id
          WHERE s.batch_id = ?1 ORDER BY s.id"
    ))?;
    let rows = stmt
        .query_map([batch_id], suggestion_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_suggestion_items(
    conn: &Connection,
    suggestion_id: i64,
) -> AppResult<Vec<AiSuggestionItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, suggestion_id, facet_key, raw_name, normalized_name, tag_id,
                confidence, decision, decision_reason, created_at, item_kind, num_value
           FROM ai_suggestion_items
          WHERE suggestion_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([suggestion_id], |r| {
            Ok(AiSuggestionItem {
                id: r.get(0)?,
                suggestion_id: r.get(1)?,
                facet_key: r.get(2)?,
                raw_name: r.get(3)?,
                normalized_name: r.get(4)?,
                tag_id: r.get(5)?,
                confidence: r.get(6)?,
                decision: r.get(7)?,
                decision_reason: r.get(8)?,
                created_at: r.get(9)?,
                item_kind: r.get(10)?,
                num_value: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// F6-d：全库「新词待确认」候选 —— decision='pending' 且 tag_id IS NULL 的条目
/// （词表里没有的词，留在 ai_suggestion_items；设置页据此列出三动作：采纳/合并/拒绝）。
pub fn list_new_word_candidates(conn: &Connection) -> AppResult<Vec<AiSuggestionItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, suggestion_id, facet_key, raw_name, normalized_name, tag_id,
                confidence, decision, decision_reason, created_at, item_kind, num_value
           FROM ai_suggestion_items
          WHERE decision = 'pending' AND tag_id IS NULL
          ORDER BY created_at DESC, id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AiSuggestionItem {
                id: r.get(0)?,
                suggestion_id: r.get(1)?,
                facet_key: r.get(2)?,
                raw_name: r.get(3)?,
                normalized_name: r.get(4)?,
                tag_id: r.get(5)?,
                confidence: r.get(6)?,
                decision: r.get(7)?,
                decision_reason: r.get(8)?,
                created_at: r.get(9)?,
                item_kind: r.get(10)?,
                num_value: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn tag_matches_facet(conn: &Connection, tag_id: i64, facet_key: &str) -> AppResult<bool> {
    let found: Option<String> = conn
        .query_row(
            "SELECT facet_key FROM tags WHERE id = ?1 AND status = 'active'",
            [tag_id],
            |r| r.get(0),
        )
        .ok();
    Ok(found.as_deref() == Some(facet_key))
}

/// 逐条处理候选项。这个接口只改变候选审计状态，不提前把标签写入素材；
/// 整条建议仍需通过 confirm_suggestion 才会落入 asset_tags。
pub fn decide_suggestion_item(
    conn: &Connection,
    item_id: i64,
    decision: &str,
    replacement_tag_id: Option<i64>,
    replacement_name: Option<&str>,
    reason: Option<&str>,
) -> AppResult<()> {
    if !matches!(decision, "accepted" | "modified" | "rejected") {
        return Err(crate::error::AppError::msg("无效的候选决策"));
    }
    // V24（§6.4）：数值建议项分流 —— accepted 写 asset_facet_numbers（confirm_number_item
    // 内含不变量 10 守卫）；rejected 仅置决策。modified 对数值无意义（用户在打标台就地改值）。
    {
        let kind: String = conn.query_row(
            "SELECT item_kind FROM ai_suggestion_items WHERE id = ?1",
            [item_id],
            |r| r.get(0),
        )?;
        if kind == "number" {
            return match decision {
                "accepted" => {
                    if let Some(warn) =
                        crate::db::facet_numbers::confirm_number_item(conn, item_id)?
                    {
                        tracing::info!("数值建议确认跳过：{warn}");
                    }
                    Ok(())
                }
                "rejected" => {
                    conn.execute(
                        "UPDATE ai_suggestion_items SET decision='rejected', decision_reason=COALESCE(?2, '用户拒绝') WHERE id=?1",
                        rusqlite::params![item_id, reason],
                    )?;
                    Ok(())
                }
                _ => Err(crate::error::AppError::msg(
                    "数值建议不支持「修改」——请手工赋值或拒绝",
                )),
            };
        }
    }
    let tx = conn.unchecked_transaction()?;
    let (facet_key, current_tag_id, raw_name): (String, Option<i64>, String) = tx.query_row(
        "SELECT facet_key, tag_id, raw_name FROM ai_suggestion_items WHERE id = ?1",
        [item_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let merge_name: Option<String> = replacement_name
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from);
    let tag_id = match decision {
        "rejected" => None,
        "accepted" => {
            // 采纳：已选规范标签 → 用它；否则用名称（新词 → find_or_create_canonical 真建标签）
            if let Some(id) = replacement_tag_id.or(current_tag_id) {
                if !tag_matches_facet(&tx, id, &facet_key)? {
                    return Err(crate::error::AppError::msg("候选标签与分面不匹配"));
                }
                Some(id)
            } else if let Some(name) = merge_name.clone().or_else(|| {
                let r = raw_name.trim();
                if r.is_empty() {
                    None
                } else {
                    Some(r.to_string())
                }
            }) {
                Some(tags::find_or_create_canonical(&tx, &facet_key, &name)?)
            } else {
                return Err(crate::error::AppError::msg("采纳为新词需要有效名称"));
            }
        }
        "modified" => {
            if let Some(id) = replacement_tag_id {
                if !tag_matches_facet(&tx, id, &facet_key)? {
                    return Err(crate::error::AppError::msg("替换标签与分面不匹配"));
                }
                // F6-a/F6-c：用户「合并到已有词」——给目标标签补 synonym 别名（候选词 =
                // 语义等价词，永久可搜；不是 old_name）。撞词（已被占用）静默跳过，不阻断合并。
                let alias_src = raw_name.trim();
                if !alias_src.is_empty() {
                    let _ = tags::add_alias(&tx, id, alias_src, None, "synonym");
                }
                Some(id)
            } else if let Some(name) = merge_name {
                Some(tags::find_or_create_canonical(&tx, &facet_key, &name)?)
            } else {
                return Err(crate::error::AppError::msg(
                    "修改候选时必须提供规范标签或名称",
                ));
            }
        }
        _ => unreachable!(),
    };
    tx.execute(
        "UPDATE ai_suggestion_items
            SET tag_id = ?1, decision = ?2, decision_reason = ?3
          WHERE id = ?4",
        rusqlite::params![tag_id, decision, reason, item_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 确认建议（内部版，不开事务）：供外层已开事务的调用方使用（confirm_all_pending）
/// B20：拆出 inner 版，与 asset_tags::assign / assign_inner 模式一致
/// FB5-05（§7.6）：description = 审核后的最终描述值；非空 → 同一事务内写入
/// assets.content_description + confirmed_description；空 → 只记 confirmed_description=NULL，
/// 不覆盖素材已有描述（「新建议描述为空：保留素材已有」）。
fn confirm_suggestion_inner(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    description: Option<&str>,
) -> AppResult<()> {
    let (asset_id, batch_id, mode, status): (i64, i64, String, String) = conn.query_row(
        "SELECT s.asset_id, s.batch_id, b.mode, s.status FROM ai_suggestions s
         JOIN ai_batches b ON b.id = s.batch_id WHERE s.id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    // 幂等守卫：已确认的建议重复调用直接返回，防止 ai_batches.confirmed 计数虚增
    if status == "confirmed" {
        return Ok(());
    }
    let source = if mode == "cloud" {
        "ai_cloud"
    } else {
        "ai_local"
    };

    let tag_ids = categorized_tag_ids(conn, tags)?;
    let final_pairs: Vec<(String, String, i64)> = tags
        .iter()
        .flat_map(|(category, names)| {
            let facet_key = tag_facets::resolve_facet_key(conn, category)
                .map(|(k, _)| k)
                .unwrap_or_else(|_| "custom".to_string());
            names.iter().filter_map(move |name| {
                let normalized = tags::normalize_name(name);
                if normalized.is_empty() {
                    return None;
                }
                // F3-a：反查收敛到 find_by_term（mode=Alias）。不需要「跳转」逻辑 ——
                // 合并时旧词已永久归属目标标签，find_by_term 命中即正确 tag。
                let id = tags::find_by_term(conn, &facet_key, &normalized, tags::TermMatch::Alias)
                    .ok()
                    .and_then(|l| l.hits.into_iter().next())
                    .map(|h| h.tag_id)?;
                Some((facet_key.clone(), normalized, id))
            })
        })
        .collect();
    asset_tags::assign_inner(conn, &[asset_id], &tag_ids, source, Some(batch_id))?;
    // A3：确认 = 用户审核通过。assign_inner 可能把同一批次的同一组标签同步给
    // RAW/JPG 同源文件，因此按批次 + 本次确认的 tag_id 晋升全部实际写入行。
    // 这样同源副本也受“审核后永不被重打清理”的铁律保护；manual 行保持 manual。
    let placeholders = tag_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE asset_tags SET review_state = 'ai_reviewed'
          WHERE source_batch_id = ?1 AND source != 'manual'
            AND review_state = 'ai_unreviewed' AND tag_id IN ({placeholders})"
    );
    let params = std::iter::once(batch_id).chain(tag_ids.iter().copied());
    conn.execute(&sql, rusqlite::params_from_iter(params))?;
    let original: String = conn.query_row(
        "SELECT suggested_tags FROM ai_suggestions WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let status = if parse_tags_json(&original) == *tags {
        "confirmed"
    } else {
        "modified"
    };
    conn.execute(
        "UPDATE ai_suggestions SET status = ?1, confirmed_tags = ?2 WHERE id = ?3",
        rusqlite::params![status, serde_json::to_string(tags)?, id],
    )?;
    // FB5-05（§7.6）：同一事务内写描述（确认标签 + 描述原子落库）
    match description.map(str::trim).filter(|d| !d.is_empty()) {
        Some(desc) => {
            conn.execute(
                "UPDATE assets SET content_description = ?1 WHERE id = ?2",
                rusqlite::params![desc, asset_id],
            )?;
            conn.execute(
                "UPDATE ai_suggestions SET confirmed_description = ?1 WHERE id = ?2",
                rusqlite::params![desc, id],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE ai_suggestions SET confirmed_description = NULL WHERE id = ?1",
                [id],
            )?;
        }
    }
    conn.execute(
        "UPDATE ai_batches SET confirmed = confirmed + 1 WHERE id = ?1",
        [batch_id],
    )?;
    let items = list_suggestion_items(conn, id)?;
    for item in items {
        if item.decision != "pending" {
            continue;
        }
        // V24（§6.4 确认分流）：数值项不走标签配对 —— 写 asset_facet_numbers
        //（不变量 10 守卫在 confirm_number_item：manual/ai_reviewed 跳过并记 warning）。
        // 歧义项（num_value=NULL）确认会报「需人工填数」→ 保持 pending，不阻塞整条建议。
        if item.item_kind == "number" {
            if let Err(e) = crate::db::facet_numbers::confirm_number_item(conn, item.id) {
                tracing::info!("数值建议保持待确认：{e}");
            }
            continue;
        }
        if let Some((_, _, tag_id)) = final_pairs.iter().find(|(facet, normalized, _)| {
            facet == &item.facet_key && normalized == &item.normalized_name
        }) {
            conn.execute(
                "UPDATE ai_suggestion_items SET decision='accepted', decision_reason='confirmed', tag_id=?1 WHERE id=?2",
                rusqlite::params![tag_id, item.id],
            )?;
        } else {
            conn.execute(
                "UPDATE ai_suggestion_items SET decision='rejected', decision_reason='removed during review' WHERE id=?1",
                [item.id],
            )?;
        }
    }
    for (facet, normalized, tag_id) in &final_pairs {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_suggestion_items WHERE suggestion_id=?1 AND facet_key=?2 AND normalized_name=?3)",
            rusqlite::params![id, facet, normalized],
            |r| r.get(0),
        )?;
        if !exists {
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO ai_suggestion_items
                 (suggestion_id, facet_key, raw_name, normalized_name, tag_id, decision, decision_reason, created_at)
                 SELECT ?1, ?2, t.name, ?3, ?4, 'modified', 'added during review', ?5 FROM tags t WHERE t.id=?4",
                rusqlite::params![id, facet, normalized, tag_id, now],
            )?;
        }
    }
    Ok(())
}

/// 确认建议：tags 为最终确认值（含人工修改）；写入 asset_tags 并联动批次计数。
/// FB5-05（§7.6）：description 为审核后的最终描述（None = 不修改描述）。
/// B20：公开版开单事务调 inner
pub fn confirm_suggestion_with_description(
    conn: &Connection,
    id: i64,
    tags: &CategorizedTags,
    description: Option<&str>,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    confirm_suggestion_inner(&tx, id, tags, description)?;
    tx.commit()?;
    Ok(())
}

/// 确认建议（不传描述，等价于 description=None）：兼容既有调用方
pub fn confirm_suggestion(conn: &Connection, id: i64, tags: &CategorizedTags) -> AppResult<()> {
    confirm_suggestion_with_description(conn, id, tags, None)
}

/// 批量套用标签到任意素材（PRD 5.3：胶片条多选套用；来源 manual）
pub fn apply_tags(conn: &Connection, asset_ids: &[i64], tags: &CategorizedTags) -> AppResult<()> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let tag_ids = categorized_tag_ids(&tx, tags)?;
    asset_tags::assign_inner(&tx, asset_ids, &tag_ids, "manual", None)?;
    tx.commit()?;
    Ok(())
}

/// 撤销拒绝（v2.11）：已拒绝建议恢复为待确认，防误触
pub fn restore_suggestion(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_suggestions
         SET status = 'pending', last_error = NULL
         WHERE id = ?1 AND status = 'rejected'",
        rusqlite::params![id],
    )?;
    conn.execute(
        "UPDATE ai_suggestion_items SET decision = 'pending', decision_reason = NULL
          WHERE suggestion_id = ?1 AND decision = 'rejected'",
        [id],
    )?;
    Ok(())
}

/// 记录单条建议打标失败原因（v6）：失败详情落库，前端可展示
pub fn set_suggestion_error(conn: &Connection, id: i64, error: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE ai_suggestions SET last_error = ?1 WHERE id = ?2",
        rusqlite::params![error, id],
    )?;
    Ok(())
}

pub fn reject_suggestion(conn: &Connection, id: i64) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE ai_suggestions SET status = 'rejected' WHERE id = ?1",
        [id],
    )?;
    tx.execute(
        "UPDATE ai_suggestion_items SET decision='rejected', decision_reason='suggestion rejected'
          WHERE suggestion_id=?1 AND decision='pending'",
        [id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 批量确认某批次全部 pending 建议（按 AI 原建议写入）
/// B20：外层包裹单事务，保证原子性（部分失败整批回滚）
/// B-2：只处理解析后标签非空的建议——历史数据可能有 `{}`、空数组或空白 JSON，
///     不能只依赖 SQL 字符串比较；空建议不写入、不虚增批次 confirmed 计数。
/// FB5-05（§7.6）：逐条应用各自描述（不得把第一张描述套给整批）；描述为空 → 保留素材已有描述。
/// 批量确认的一页。`last_id` 用于即使整页都是空建议也能继续向后扫描。
#[derive(Debug, Clone)]
pub struct PendingConfirmationPage {
    pub items: Vec<(i64, CategorizedTags, Option<String>)>,
    pub last_id: Option<i64>,
}

/// 按 id 分页读取可确认的 pending 建议。空标签且无描述的历史占位项跳过。
pub fn list_pending_confirmations(
    conn: &Connection,
    batch_id: i64,
    after_id: i64,
    limit: i64,
) -> AppResult<PendingConfirmationPage> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn.prepare(
        "SELECT id, suggested_tags, suggested_description
           FROM ai_suggestions
          WHERE batch_id = ?1 AND status = 'pending' AND id > ?2
          ORDER BY id LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![batch_id, after_id, limit], |r| {
            let id: i64 = r.get(0)?;
            let raw: String = r.get(1)?;
            let desc: String = r.get(2)?;
            let desc = desc.trim();
            Ok((
                id,
                parse_tags_json(&raw),
                (!desc.is_empty()).then(|| desc.to_string()),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let last_id = rows.last().map(|(id, _, _)| *id);
    let items = rows
        .into_iter()
        .filter(|(_, tags, desc)| !tags.is_empty() || desc.is_some())
        .collect();
    Ok(PendingConfirmationPage { items, last_id })
}

/// 确认一页建议。单页一个事务；页与页之间由命令层释放 DB 锁，避免整批大事务阻塞全应用。
pub fn confirm_pending_batch(
    conn: &Connection,
    items: &[(i64, CategorizedTags, Option<String>)],
) -> AppResult<()> {
    if items.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (id, tags, description) in items {
        confirm_suggestion_inner(&tx, *id, tags, description.as_deref())?;
    }
    tx.commit()?;
    Ok(())
}

/// 兼容单连接调用方的整批确认；生产命令使用分页 API 在页间释放锁。
pub fn confirm_all_pending(conn: &Connection, batch_id: i64) -> AppResult<()> {
    const CHUNK: i64 = 100;
    let mut after_id = 0;
    loop {
        let page = list_pending_confirmations(conn, batch_id, after_id, CHUNK)?;
        let Some(last_id) = page.last_id else {
            return Ok(());
        };
        confirm_pending_batch(conn, &page.items)?;
        after_id = last_id;
    }
}

#[cfg(test)]
mod number_tests {
    use super::*;

    #[test]
    fn analysis_result_reads_legacy_people_presence_but_omits_default_field() {
        let legacy: AnalysisResult = serde_json::from_str(
            r#"{"description":"旧摘要","peoplePresence":{"status":"present","confidence":0.9},"proposals":[],"numbers":[],"warnings":[]}"#,
        )
        .unwrap();
        assert_eq!(legacy.people_presence.status, PeoplePresenceStatus::Present);

        let current = AnalysisResult {
            description: "新摘要".into(),
            people_presence: PeoplePresence::default(),
            proposals: Vec::new(),
            numbers: Vec::new(),
            warnings: Vec::new(),
        };
        let serialized = serde_json::to_value(current).unwrap();
        assert!(serialized.get("peoplePresence").is_none());
    }

    // ═══════════ Phase 6 · §6.5 数值解析严格规则（8 条） ═══════════

    #[test]
    fn number_parse_accepts_unambiguous_single_value() {
        assert_eq!(parse_number_proposal("5"), NumberParse::Value(5.0));
        assert_eq!(parse_number_proposal("5人"), NumberParse::Value(5.0));
        assert_eq!(parse_number_proposal("5 人"), NumberParse::Value(5.0));
        assert_eq!(parse_number_proposal("人数5"), NumberParse::Value(5.0));
        assert_eq!(parse_number_proposal(" -3 "), NumberParse::Value(-3.0));
    }

    #[test]
    fn number_parse_rejects_range_forms() {
        for s in ["5~6", "5-6", "5 到 6", "5至6", "5～6", "5—6"] {
            assert_eq!(
                parse_number_proposal(s),
                NumberParse::Ambiguous {
                    reason: "范围".into()
                },
                "范围「{s}」不得静默取首个数字"
            );
        }
    }

    #[test]
    fn number_parse_rejects_approximation_words() {
        for s in ["约5", "大约 5", "5左右", "5上下", "approximately 5"] {
            assert_eq!(
                parse_number_proposal(s),
                NumberParse::Ambiguous {
                    reason: "约数".into()
                },
                "约数「{s}」必须进 pending"
            );
        }
    }

    #[test]
    fn number_parse_rejects_comparison_words() {
        for s in [
            "不少于5",
            "不多于5",
            "超过5",
            "5以上",
            "5以下",
            "至少5",
            "最多5",
            "大于5",
            "小于5",
        ] {
            assert_eq!(
                parse_number_proposal(s),
                NumberParse::Ambiguous {
                    reason: "比较式".into()
                },
                "比较式「{s}」必须进 pending"
            );
        }
    }

    #[test]
    fn number_parse_rejects_multiple_numbers() {
        for s in ["3或4", "3、4", "3 个或 4 个"] {
            assert_eq!(
                parse_number_proposal(s),
                NumberParse::Ambiguous {
                    reason: "多值".into()
                },
                "多值「{s}」不得自动裁决"
            );
        }
    }

    #[test]
    fn number_parse_rejects_nan_inf_and_out_of_range() {
        // NaN / Infinity 在解析层就进不了 Value（无数字 token 或非有限值）
        assert_eq!(parse_number_proposal("NaN"), NumberParse::None);
        assert_eq!(parse_number_proposal("很多"), NumberParse::None);
        assert_eq!(parse_number_proposal("一群人"), NumberParse::None);
        // 非有限值与越界由 validate_number_in_range 拒绝（绝不裁到边界）
        assert_eq!(
            validate_number_in_range(f64::NAN, Some(0.0), Some(50.0)),
            NumberParse::Ambiguous {
                reason: "超出范围".into()
            }
        );
        assert_eq!(
            validate_number_in_range(f64::INFINITY, Some(0.0), Some(50.0)),
            NumberParse::Ambiguous {
                reason: "超出范围".into()
            }
        );
        assert_eq!(
            validate_number_in_range(51.0, Some(0.0), Some(50.0)),
            NumberParse::Ambiguous {
                reason: "超出范围 0–50".into()
            }
        );
        assert_eq!(
            validate_number_in_range(-1.0, Some(0.0), Some(50.0)),
            NumberParse::Ambiguous {
                reason: "超出范围 0–50".into()
            }
        );
        assert_eq!(
            validate_number_in_range(50.0, Some(0.0), Some(50.0)),
            NumberParse::Value(50.0)
        );
    }

    #[test]
    fn ai_ambiguous_number_goes_to_pending_not_value() {
        // 三态处置表：AI 落建议路径 Value→落值；Ambiguous→num_value=NULL + decision_reason；None→丢弃。
        // 本测试锁解析函数的处置输入：模糊输出不能产出 Value（调用方据此写 pending）。
        let ambiguous_inputs = ["5~6", "约5", "不少于5", "3或4"];
        for s in ambiguous_inputs {
            assert!(
                !matches!(parse_number_proposal(s), NumberParse::Value(_)),
                "「{s}」绝不能解析为可落库的值"
            );
        }
    }

    #[test]
    fn five_and_five_point_zero_normalize_equal() {
        // "5" / "5.0" / "05" / "5人" 归一为同一个值（PK 上唯一）
        let a = parse_number_proposal("5");
        let b = parse_number_proposal("5.0");
        let c = parse_number_proposal("05");
        let d = parse_number_proposal("5人");
        match (a, b, c, d) {
            (
                NumberParse::Value(x),
                NumberParse::Value(y),
                NumberParse::Value(z),
                NumberParse::Value(w),
            ) => {
                assert_eq!(x, 5.0);
                assert_eq!(y, 5.0);
                assert_eq!(z, 5.0);
                assert_eq!(w, 5.0);
            }
            other => panic!("四者都应解析为 Value(5.0)：{other:?}"),
        }
    }
}
