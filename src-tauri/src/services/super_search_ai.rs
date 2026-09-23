//! 超级搜索 AI 服务（FB5-05 §9）：自然语言 → SearchIntentV2 → QueryExpr。
//! - 模型只输出分组概念事实源（组内 AND、组间 OR），后端生成并校验 QueryExpr；
//! - AI parse result 只返回 expr/排序/解释/warnings/resolvedTags，无扁平 query；
//! - 未知/歧义标签 → content 搜索 leaf 或 warning；强制不查询回收站；零数据库写入。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::query_expr::{normalize_expr, LeafCond, QueryExpr};
use crate::db::search_query::{self, MetadataFilter};
use crate::db::settings::AiSettings;
use crate::db::tag_facets::FacetPromptContext;
use crate::db::tags;
use crate::error::{AppError, AppResult};
use crate::services::ai_cloud::{self, TextJsonTier};
use std::sync::Mutex;
use std::time::Instant;

pub const MAX_INPUT_LEN: usize = 200;

/// §9.2-2：共享停用词（单一常量）。Prompt 文本由本常量生成，本地清洗复用同一集合——
/// 禁止维护两份会漂移的列表。
pub const SEARCH_CONCEPT_STOPWORDS: &[&str] = &[
    "然后",
    "还有",
    "有",
    "和",
    "与",
    "并且",
    "再",
    "里面",
    "画面中",
    "上面",
    "中间",
    "旁边",
    "拍的",
    "一个",
    "一些",
];

/// §9.3：OR 触发词（本地 group 守卫用）
const OR_WORDS: &[&str] = &["或者", "或", "任一", "二选一"];

/// §9.3 assetType 守卫：只有原文含明确词才接受
const IMAGE_WORDS: &[&str] = &["图片", "照片", "相片", "图像"];
const VIDEO_WORDS: &[&str] = &["视频", "录像", "片段", "短片"];

/// §9.2.2：元数据 key 契约集 —— **单一事实源 = db/search_query.rs ALL_METADATA_KEYS**
/// （S0：提示词教学段与 schema enum 均引用它，禁止漂移；禁止本文件再维护一份列表）。
pub const METADATA_KEYS: &[&str] = crate::db::search_query::ALL_METADATA_KEYS;

/// 元数据操作符全集（= search_query.rs 各 key allowed_ops 的并集）
pub const METADATA_OPS: &[&str] = &["eq", "in", "gt", "gte", "lt", "lte", "between", "contains"];

/// concept 过长判定（§9.3：超过 12 个 Unicode 字符即句子化）
pub const MAX_CONCEPT_CHARS: usize = 12;
/// concept 清理后的首尾标点（本地清洗）
const CONCEPT_EDGE_PUNCT: &[char] = &[
    '。', '！', '？', '，', ',', '.', '!', '?', '；', ';', '：', ':', '、', '"', '"', '\'', '（',
    '）', '(', ')', '…', '·',
];

// ═══════════════ SearchIntent V2 结构（§9.1） ═══════════════

/// FB5-05：V2 单一概念协议。旧字段 search/tags/excludeTags/unresolved/relation 全部删除。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIntentV2 {
    /// 组间 OR；每个 group 内全部条件 AND（§9.1）
    #[serde(default)]
    pub groups: Vec<SearchGroupV2>,
    /// 全局排除（对整个正向结果 NOT）
    #[serde(default)]
    pub exclusions: Vec<SearchConceptV2>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub sort_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchGroupV2 {
    /// all | image | video（§9.3：本地守卫校验依据）
    #[serde(default = "default_asset_type")]
    pub asset_type: String,
    #[serde(default)]
    pub concepts: Vec<SearchConceptV2>,
    #[serde(default)]
    pub text_terms: Vec<IntentTextTerm>,
    #[serde(default)]
    pub metadata: Vec<MetadataFilter>,
    /// 「没打标签 / 未分类 / 无标签」→ true，编译为 LeafCond::Untagged（硬条件：没有任何标签）
    #[serde(default)]
    pub untagged_only: bool,
}

fn default_asset_type() -> String {
    "all".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConceptV2 {
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub facet_hint: Option<String>,
    /// 0~1；缺省（fallback 路径）按 0 处理 → 不进入硬筛选（§9.4）
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentTextTerm {
    pub text: String,
    /// all | content | description | fileName（映射到 SearchScope，列名只能由此枚举出）
    #[serde(default = "default_text_scope")]
    pub scope: String,
}

fn default_text_scope() -> String {
    "all".into()
}

// ═══════════════ S3：SearchIntentV3 —— preferred 加分项 + evidence 一致性守卫 ═══════════════

/// 概念必要性（V3）。Required = V2 concept 语义（全部进 filter，默认，兼容 V2）；
/// Preferred = 加分项（最好有/优先…，不进 filter，只影响相关度排序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Necessity {
    #[default]
    Required,
    Preferred,
}

/// V3 概念：V2 字段 + necessity/weight/evidence/term_match。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConceptV3 {
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub facet_hint: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Required（默认，兼容 V2）| Preferred
    #[serde(default)]
    pub necessity: Necessity,
    /// 只给三档 0.5 / 1.0 / 2.0
    #[serde(default)]
    pub weight: Option<f32>,
    /// 模型对「为什么判为加分」的**原文依据**。守卫用它做一致性校验。
    #[serde(default)]
    pub evidence: Option<String>,
    /// 【S5】词匹配方式（默认 Alias；Prefix/Contains/Fuzzy 只在零结果兜底用）。
    /// schema 允许 null（模型可省略）；本地以 default = Alias 兜底。
    #[serde(default, deserialize_with = "de_term_match_nullable")]
    pub term_match: crate::db::tags::TermMatch,
}

/// serde helper：termMatch 为 null/缺失时按 default（Alias）处理。
fn de_term_match_nullable<'de, D>(d: D) -> Result<crate::db::tags::TermMatch, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<crate::db::tags::TermMatch>::deserialize(d)?;
    Ok(v.unwrap_or_default())
}

/// V3 组：V2 字段语义不变（全部进 filter），新增 `preferred`（加分项 → should）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchGroupV3 {
    #[serde(default = "default_asset_type")]
    pub asset_type: String,
    #[serde(default)]
    pub concepts: Vec<SearchConceptV3>,
    #[serde(default)]
    pub text_terms: Vec<IntentTextTerm>,
    #[serde(default)]
    pub metadata: Vec<MetadataFilter>,
    /// 「没打标签 / 未分类」→ true，编译为 LeafCond::Untagged
    #[serde(default)]
    pub untagged_only: bool,
    /// V3 新增：最好有 / 优先 / 尽量 / 更好 … → should（加分，不淘汰）
    #[serde(default)]
    pub preferred: Vec<SearchConceptV3>,
}

/// S3：V3 顶层意图 —— 与 V2 同构（组间 OR、组内 AND、全局 exclusions），
/// 只是组用 V3（concepts 进 filter、preferred 进 should）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIntentV3 {
    #[serde(default)]
    pub groups: Vec<SearchGroupV3>,
    #[serde(default)]
    pub exclusions: Vec<SearchConceptV3>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub sort_dir: Option<String>,
}

/// ③ 兜底用的「强偏好信号词」：只提示不降级。
/// 注意故意**不含**「可有可无」——那是弱表达，原句只含它时记 info（见指南真值表）。
pub const CORE_PREF_HINTS: &[&str] = &["最好", "优先", "尽量", "更好", "倾向", "接近", "偏"];

/// 用于判断“偏好短语覆盖范围”的弱表达。它不能单独触发缺失偏好告警，
/// 但如果模型把相应概念放进了 concepts，仍应阻止它被当成硬条件。
const PREFERENCE_SCOPE_HINTS: &[&str] = &[
    "最好",
    "优先",
    "尽量",
    "更好",
    "倾向",
    "接近",
    "偏",
    "可有可无",
];

#[derive(Debug, Clone)]
struct PreferenceSpan {
    start: usize,
    end: usize,
    evidence: String,
}

fn fold_match_char(c: char) -> char {
    match c {
        'Ａ'..='Ｚ' => ((c as u32 - 'Ａ' as u32) as u8 + b'a') as char,
        'ａ'..='ｚ' => ((c as u32 - 'ａ' as u32) as u8 + b'a') as char,
        _ => c.to_ascii_lowercase(),
    }
}

fn folded_chars(text: &str) -> Vec<char> {
    text.chars().map(fold_match_char).collect()
}

fn clause_delimiter(c: char) -> bool {
    matches!(
        c,
        '，' | ',' | '。' | '.' | '；' | ';' | '！' | '!' | '？' | '?' | '\n'
    )
}

fn contains_at(haystack: &[char], start: usize, needle: &[char]) -> bool {
    !needle.is_empty()
        && start.saturating_add(needle.len()) <= haystack.len()
        && haystack[start..start + needle.len()] == *needle
}

fn preference_spans(input: &str) -> Vec<PreferenceSpan> {
    let chars: Vec<char> = input.chars().collect();
    let folded = folded_chars(input);
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for hint in PREFERENCE_SCOPE_HINTS {
        let needle = folded_chars(hint);
        if needle.is_empty() || needle.len() > folded.len() {
            continue;
        }
        for i in 0..=folded.len() - needle.len() {
            if !contains_at(&folded, i, &needle) {
                continue;
            }
            let mut start = i;
            while start > 0 && !clause_delimiter(chars[start - 1]) {
                start -= 1;
            }
            let mut end = i + needle.len();
            while end < chars.len() && !clause_delimiter(chars[end]) {
                end += 1;
            }
            if start < end {
                ranges.push((start, end));
            }
        }
    }

    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
        .into_iter()
        .filter_map(|(start, end)| {
            let evidence: String = chars[start..end].iter().collect();
            let evidence = evidence.trim().to_string();
            (!evidence.is_empty()).then_some(PreferenceSpan {
                start,
                end,
                evidence,
            })
        })
        .collect()
}

fn dictionary_surface_terms(dictionary: &[String], concept: &str) -> Vec<String> {
    let concept_key: String = folded_chars(concept).into_iter().collect();
    let mut out = vec![concept.trim().to_string()];

    for line in dictionary {
        let mut term: Option<String> = None;
        let mut aliases: Vec<String> = Vec::new();
        for part in line.split('|').map(str::trim) {
            if let Some(value) = part.strip_prefix("term:") {
                term = Some(value.trim().to_string());
            } else if let Some(value) = part.strip_prefix("aliases:") {
                aliases.extend(
                    value
                        .split('、')
                        .flat_map(|v| v.split(','))
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToOwned::to_owned),
                );
            }
        }
        let Some(canonical) = term else { continue };
        let matches = folded_chars(&canonical).into_iter().collect::<String>() == concept_key
            || aliases
                .iter()
                .any(|alias| folded_chars(alias).into_iter().collect::<String>() == concept_key);
        if matches {
            out.push(canonical);
            out.extend(aliases);
        }
    }

    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .filter(|term| {
            let key: String = folded_chars(term).into_iter().collect();
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

fn term_occurrences(haystack: &[char], term: &[char]) -> Vec<(usize, usize)> {
    if term.is_empty() || term.len() > haystack.len() {
        return Vec::new();
    }
    (0..=haystack.len() - term.len())
        .filter(|&i| contains_at(haystack, i, term))
        .map(|i| (i, i + term.len()))
        .collect()
}

/// 修正模型把“最好/优先”概念放入 concepts 的情况。
///
/// 这一步使用当前发送给模型的标签词典，因此既支持原文直接出现规范名，
/// 也支持“年轻”→“青年”这类别名归一化。只移动能在偏好短语中找到、且没有
/// 在偏好短语外再次出现的概念；后者保留为硬条件，避免误伤“要女性，最好年轻”。
pub fn repair_misplaced_preferred(
    input: &str,
    dictionary: &[String],
    intent: &mut SearchIntentV3,
) -> Vec<String> {
    let spans = preference_spans(input);
    if spans.is_empty() {
        return Vec::new();
    }
    let input_folded = folded_chars(input);
    let mut warnings = Vec::new();

    for group in &mut intent.groups {
        let mut kept = Vec::with_capacity(group.concepts.len());
        let mut moved = Vec::new();
        for concept in group.concepts.drain(..) {
            let terms = dictionary_surface_terms(dictionary, &concept.text);
            // 单字概念（如“人”）容易成为其他词的子串，宁可交给模型的
            // preferred 槽位处理，也不在这里猜测其语义范围。
            let terms: Vec<Vec<char>> = terms
                .iter()
                .filter_map(|term| {
                    let folded = folded_chars(term);
                    (folded.len() >= 2).then_some(folded)
                })
                .collect();
            let mut soft_evidence: Option<String> = None;
            let mut hard_occurrence = false;

            'terms: for term in terms {
                for (start, end) in term_occurrences(&input_folded, &term) {
                    if spans
                        .iter()
                        .any(|span| start >= span.start && end <= span.end)
                    {
                        if soft_evidence.is_none() {
                            soft_evidence = spans
                                .iter()
                                .find(|span| start >= span.start && end <= span.end)
                                .map(|span| span.evidence.clone());
                        }
                    } else {
                        hard_occurrence = true;
                        break 'terms;
                    }
                }
            }

            if let Some(evidence) = soft_evidence.filter(|_| !hard_occurrence) {
                let mut preferred = concept;
                preferred.necessity = Necessity::Preferred;
                preferred.weight = Some(preferred.weight.unwrap_or(1.0));
                preferred.evidence = Some(evidence);
                moved.push(preferred);
            } else {
                kept.push(concept);
            }
        }
        if !moved.is_empty() {
            let labels = moved
                .iter()
                .map(|c| c.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("、");
            warnings.push(format!(
                "检测到偏好条件被放入必须区，已将「{labels}」纠正为优先条件。"
            ));
            group.concepts = kept;
            group.preferred.extend(moved);
        } else {
            group.concepts = kept;
        }
    }

    // 同一概念同时出现在两个区时，只有在它没有被纠正为 preferred 的情况下才
    // 保留硬条件；硬条件优先，避免重复 should 造成解释和排序混乱。
    for group in &mut intent.groups {
        let required: std::collections::HashSet<String> = group
            .concepts
            .iter()
            .map(|c| folded_chars(&c.text).into_iter().collect())
            .collect();
        let mut seen = std::collections::HashSet::new();
        group.preferred.retain(|concept| {
            let key: String = folded_chars(&concept.text).into_iter().collect();
            if required.contains(&key) {
                warnings.push(format!(
                    "「{}」同时出现在必须和优先区，已按必须条件执行。",
                    concept.text
                ));
                false
            } else {
                seen.insert(key)
            }
        });
    }
    warnings
}

/// S3：preferred 的 evidence 一致性守卫（纯函数；只保留或丢弃，绝不反向升级）。
/// ① evidence 必须是原句的**真子串**（trim + 全角/半角 + 大小写归一后比较）——
///    模型编造依据 → 丢弃该 preferred + warning（软条件绝不能被强化成 required）。
/// ② 规范标签可能是原文同义词的归一名（如「年轻」→「青年」），因此概念与 evidence
///    的关联在数据库解析阶段用规范名+可搜索别名复核，不能在这里做规范名逐字比较。
/// ③ 兜底信号（只提示不降级）：原句含任一强偏好词则静默；不含 → info warning
///    「原文未见明显的偏好表述，已按加分项处理（可在下方改为必须）」。
/// 不满足证据要求时从 preferred 移除并保留 warning；required 条件只来自模型的 required 槽位。
pub fn guard_preferred(input: &str, group: &mut SearchGroupV3) -> Vec<String> {
    let mut warnings = Vec::new();
    let norm = |s: &str| crate::db::tags::normalize_name(s);
    let input_n = norm(input);
    if input_n.is_empty() {
        return warnings;
    }
    let core_hint = CORE_PREF_HINTS.iter().any(|h| input_n.contains(h));
    // 证据不可信的 preferred 下标（倒序移除保序）
    let mut drop: Vec<usize> = Vec::new();
    for (i, c) in group.preferred.iter().enumerate() {
        if norm(&c.text).is_empty() {
            drop.push(i);
            continue;
        }
        let Some(ev) = c.evidence.as_deref() else {
            warnings.push(format!(
                "「{}」被标为加分项但没给出依据，已忽略该加分项。",
                c.text
            ));
            drop.push(i);
            continue;
        };
        let ev_n = norm(ev);
        // ① 依据必须是原句真子串
        if ev_n.is_empty() || !input_n.contains(&ev_n) {
            warnings.push(format!(
                "「{}」的加分依据未落在原句（不可编造），已忽略该加分项。",
                c.text
            ));
            drop.push(i);
            continue;
        }
        // ② 概念与 evidence 的关联在 build_plan_from_v3 中用数据库词典复核。
        // 这里不能拿规范名做字面窗口匹配，否则「年轻女性」→「青年+女性」会被误删。
        // ③ 原句没有强偏好词 → info（保留 preferred）
        if !core_hint {
            warnings
                .push("原文未见明显的偏好表述，已按加分项处理（可在下方改为必须）。".to_string());
        }
    }
    group.preferred = group
        .preferred
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, c)| c.clone())
        .collect();
    warnings
}

fn warn_missing_preferred(input: &str, groups: &[SearchGroupV3]) -> Vec<String> {
    if !CORE_PREF_HINTS.iter().any(|hint| input.contains(hint)) {
        return Vec::new();
    }
    if groups.iter().any(|group| !group.preferred.is_empty()) {
        return Vec::new();
    }
    vec!["原文包含偏好表达，但未能生成可选加分条件；未自动改写，请检查解析结果。".into()]
}

/// 已解析标签（前端展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTag {
    pub facet_key: String,
    pub text: String,
    pub tag_id: i64,
    pub path: String,
}

/// 执行对象（ResolvedSearchQuery，contract-v1 §3）。
/// FB5-05：AI parse result 不再返回它（改用 expr）；保留给手动/兼容链路使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedQuery {
    pub search: String,
    pub asset_type: String,
    pub untagged_only: bool,
    pub facet_filters: Vec<ResolvedFacet>,
    pub exclude_tag_ids: Vec<i64>,
    pub missing_facet_keys: Vec<String>,
    pub metadata_filters: Vec<MetadataFilter>,
    pub sort_by: String,
    pub sort_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFacet {
    pub facet_key: String,
    pub tag_ids: Vec<i64>,
    pub mode: String,
    pub include_descendants: bool,
}

/// FB5-05（§9.5）：AI 解析结果。expr 为唯一执行事实源；排序单独返回。
/// W6-5（§W6-5）：parseStatus 供前端区分「完全理解 / 部分理解 / 按关键词搜索」三态。
/// S3：新增 plan —— V3 解析出 preferred（加分项）时非空，供 U 波次三段式 UI 直接消费；
/// 无加分项时 plan=None，前端继续用 expr 单链路（保持既有行为）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSearchParseResult {
    pub intent: SearchIntentV3,
    pub expr: Option<QueryExpr>,
    #[serde(default)]
    pub plan: Option<crate::db::search_plan::SearchPlanV3>,
    pub sort_by: String,
    pub sort_dir: String,
    pub explanation: String,
    pub warnings: Vec<String>,
    pub resolved_tags: Vec<ResolvedTag>,
    /// "full" = 完全理解（无警告）；"partial" = 部分理解（有警告仍执行）；"keyword" = 关键词兜底
    pub parse_status: String,
}

// ═══════════════ 校验（§9.2.2 结构层） ═══════════════

/// 校验 SearchIntentV2：未知字段/枚举/非法值一律拒绝（不静默忽略）。
/// 本地语义守卫（§9.3）由 guard_intent 承担；本函数只做结构性校验。
/// 注意：非法/未知 metadata 条件不在此报错——先经 sanitize_metadata 丢弃 + warning 降级，
/// 本函数收到的 metadata 均视为合法（编译期兜底见 query_expr::validate_expr）。
pub fn validate_intent(intent: &SearchIntentV2, facets: &[FacetPromptContext]) -> AppResult<()> {
    if let Some(sb) = &intent.sort_by {
        // R0-4/S0：排序白名单单一事实源（search_query::ALL_SORT_KEYS）——
        // validate_intent / is_valid_sort_by / assets VALID_SORT 三方必须一致，
        // 否则 rating 排序被整单降级为关键词搜索。
        if !crate::db::search_query::ALL_SORT_KEYS.contains(&sb.as_str()) {
            return Err(AppError::msg(format!("非法排序字段：{sb}")));
        }
    }
    if let Some(sd) = &intent.sort_dir {
        if !matches!(sd.as_str(), "asc" | "desc") {
            return Err(AppError::msg(format!("非法 sortDir：{sd}")));
        }
    }
    if intent.groups.len() > 6 {
        return Err(AppError::msg("分组数量超出上限（最多 6 组）"));
    }
    if intent.exclusions.len() > 20 {
        return Err(AppError::msg("排除概念数量超出上限"));
    }
    for g in &intent.groups {
        if !matches!(g.asset_type.as_str(), "all" | "image" | "video") {
            return Err(AppError::msg(format!("非法 assetType：{}", g.asset_type)));
        }
        if g.metadata.len() > 20 {
            return Err(AppError::msg("元数据条件数量超出上限"));
        }
        for c in &g.concepts {
            validate_concept_shape(c)?;
        }
        for tt in &g.text_terms {
            if tt.text.trim().is_empty() {
                return Err(AppError::msg("搜索词不能为空"));
            }
            if tt.text.chars().count() > 200 {
                return Err(AppError::msg("搜索词过长"));
            }
            if !matches!(
                tt.scope.as_str(),
                "all" | "content" | "description" | "fileName"
            ) {
                return Err(AppError::msg(format!("非法搜索范围：{}", tt.scope)));
            }
        }
    }
    for c in &intent.exclusions {
        validate_concept_shape(c)?;
    }
    let _ = facets; // 结构校验不再依赖分面集合（facet 收窄在解析层做）
    Ok(())
}

fn validate_concept_shape(c: &SearchConceptV2) -> AppResult<()> {
    if c.text.trim().is_empty() {
        return Err(AppError::msg("概念文字不能为空"));
    }
    if c.text.chars().count() > 100 {
        return Err(AppError::msg("概念文字过长"));
    }
    if let Some(h) = &c.facet_hint {
        if h.chars().count() > 60 {
            return Err(AppError::msg("分面提示过长"));
        }
    }
    Ok(())
}

/// 元数据条件容错降级（第六轮反馈改造）：单条非法/未知 metadata 条件（未知 key、
/// 不支持的 op、非法值）只丢弃该条件并记 warning，其余合法条件保留照常执行；
/// 结构性错误（排序/分组枚举等）仍由 validate_intent 整体报错。
/// 判定依据 = search_query::compile_metadata（与执行层同一校验，绝不漂移）。
pub fn sanitize_metadata(intent: &mut SearchIntentV2) -> Vec<String> {
    let mut warnings = Vec::new();
    for g in &mut intent.groups {
        g.metadata
            .retain(|f| match search_query::compile_metadata(f) {
                Ok(_) => true,
                Err(e) => {
                    let msg = format!("已忽略无效的元数据条件（{} {}）：{e}", f.key, f.op);
                    tracing::warn!(
                        "超级搜索丢弃非法元数据条件: key={} op={} 原因={e}",
                        f.key,
                        f.op
                    );
                    warnings.push(msg);
                    false
                }
            });
    }
    warnings
}

// ═══════════════ 本地确定性守卫（§9.3） ═══════════════

/// 本地守卫：不信任模型。返回非阻断 warnings。
/// ① group 与 OR：原文无 OR 词而模型输出多组 → 合并为单 AND 组；
///    原文含 OR 而模型只给一组 → 保留并提示「或关系未能准确分组」。
/// ② assetType：原文无明确词 → 改回 all。
/// ③ concept 清洗：trim/去首尾标点/去停用词/拒句子化文本/按 text+role 去重/confidence 钳制。
/// ④ 全量概念计数上限（groups+exclusions ≤ 20）；语义相同的重复 group 去重。
pub fn guard_intent(input: &str, intent: &mut SearchIntentV2) -> Vec<String> {
    let mut warnings = Vec::new();
    let has_or = OR_WORDS.iter().any(|w| input.contains(w));

    // ① group 与 OR 守卫
    if !has_or && intent.groups.len() > 1 {
        warnings.push("输入没有明确的「或」关系，已将多组条件合并为一组全部满足。".into());
        let mut merged = SearchGroupV2 {
            asset_type: "all".into(),
            concepts: Vec::new(),
            text_terms: Vec::new(),
            metadata: Vec::new(),
            untagged_only: false,
        };
        for g in intent.groups.drain(..) {
            merged.concepts.extend(g.concepts);
            merged.text_terms.extend(g.text_terms);
            merged.metadata.extend(g.metadata);
            merged.untagged_only |= g.untagged_only;
        }
        intent.groups = vec![merged];
    } else if has_or && intent.groups.len() == 1 {
        warnings.push("「或」关系未能准确分组，已按全部条件同时满足执行。".into());
    }
    // group 上限 6；清理后为空组删除
    intent.groups.truncate(6);

    // ② assetType 守卫
    for g in &mut intent.groups {
        let grounded = match g.asset_type.as_str() {
            "image" => IMAGE_WORDS.iter().any(|w| input.contains(w)),
            "video" => VIDEO_WORDS.iter().any(|w| input.contains(w)),
            _ => true,
        };
        if !grounded {
            warnings.push(format!(
                "原文未明确提到「{}」，已忽略类型条件。",
                if g.asset_type == "image" {
                    "图片/照片"
                } else {
                    "视频/录像"
                }
            ));
            g.asset_type = "all".into();
        }
    }

    // ③ concept 清洗（group 内 + exclusions）
    let mut total_concepts = 0usize;
    for g in &mut intent.groups {
        clean_concepts(&mut g.concepts, &mut warnings, &mut total_concepts);
    }
    clean_concepts(&mut intent.exclusions, &mut warnings, &mut total_concepts);
    if total_concepts > 20 {
        warnings.push(format!(
            "条件概念较多（{total_concepts} 个），已按置信度优先截取 20 个。"
        ));
    }

    // ④ 语义相同的重复 group 去重（序列化比较）
    let mut seen = std::collections::HashSet::new();
    intent.groups.retain(|g| {
        let key = serde_json::to_string(g).unwrap_or_default();
        seen.insert(key)
    });
    // 单个概念（text+role 标准化后）去重
    for g in &mut intent.groups {
        dedup_concepts(&mut g.concepts);
    }
    dedup_concepts(&mut intent.exclusions);

    warnings
}

/// 对用户明确写出的单位/构图约束做轻量覆盖检查。
/// 这不是替模型补条件，而是防止「条件被悄悄吃掉」：保留已解析结果，同时给出可理解 warning。
pub fn warn_missing_explicit_metadata(input: &str, intent: &SearchIntentV2) -> Vec<String> {
    let lower = input.to_ascii_lowercase();
    let has_key = |key: &str| {
        intent
            .groups
            .iter()
            .any(|g| g.metadata.iter().any(|m| m.key == key))
    };
    let mut warnings = Vec::new();
    if (lower.contains("kb")
        || lower.contains("mb")
        || lower.contains("gb")
        || input.contains("兆"))
        && !has_key("file_size")
    {
        warnings.push("原文包含文件大小条件，但未生成 file_size 条件，已保留其他条件。".into());
    }
    if (input.contains("秒") || input.contains("分钟")) && !has_key("duration_ms") {
        warnings.push("原文包含视频时长条件，但未生成 duration_ms 条件，已保留其他条件。".into());
    }
    if [
        "横图",
        "横版",
        "横屏",
        "竖图",
        "竖版",
        "竖屏",
        "方图",
        "正方形",
    ]
    .iter()
    .any(|word| input.contains(word))
        && !has_key("aspect_ratio")
    {
        warnings.push("原文包含横竖构图条件，但未生成 aspect_ratio 条件，已保留其他条件。".into());
    }
    warnings
}

fn clean_concepts(
    concepts: &mut Vec<SearchConceptV2>,
    warnings: &mut Vec<String>,
    total: &mut usize,
) {
    let mut kept: Vec<SearchConceptV2> = Vec::new();
    for c in concepts.drain(..) {
        let mut text = c.text.trim().to_string();
        // 去首尾标点
        text = text
            .trim_matches(|ch: char| CONCEPT_EDGE_PUNCT.contains(&ch))
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        // 停用词
        if SEARCH_CONCEPT_STOPWORDS.iter().any(|w| *w == text) {
            continue;
        }
        // 句子化文本拒绝
        if text.chars().count() > MAX_CONCEPT_CHARS {
            warnings.push(format!("「{text}」是句子而非原子概念，已忽略。"));
            continue;
        }
        let confidence = c.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
        kept.push(SearchConceptV2 {
            text,
            role: c.role.trim().to_string(),
            facet_hint: c
                .facet_hint
                .as_ref()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty()),
            confidence: Some(confidence),
        });
        *total += 1;
    }
    *concepts = kept;
}

fn dedup_concepts(concepts: &mut Vec<SearchConceptV2>) {
    let mut seen = std::collections::HashSet::new();
    concepts.retain(|c| {
        let key = format!(
            "{}|{}",
            tags::normalize_name(&c.text),
            c.role.to_ascii_lowercase()
        );
        seen.insert(key)
    });
}

// ═══════════════ 标签解析 + QueryExpr 生成（§9.4/§9.5） ═══════════════

enum ConceptOutcome {
    Tag(ResolvedTag),
    /// 未映射的具体画面概念 → content 范围搜索 leaf（§9.4）
    Content(String),
    /// 未采用（warning 文案）
    Dropped(String),
}

/// §9.4 标签解析策略：
///  1. 规范名精确匹配（全分面）；
///  2. alias 精确匹配；
///  3. facetHint 范围内只有一个前缀候选，且 confidence ≥ 0.85；
///  4. 其他包含/模糊多候选 → 不擅选（转 content 或 drop）。
///     无法可靠映射时：正向 concept 且 conf ≥ 0.55 → content 搜索 leaf（显式硬条件）；
///     exclusion 且 conf ≥ 0.55 → 全局 NOT(content)；conf < 0.55 → 未采用 warning。
fn resolve_concept(
    conn: &Connection,
    c: &SearchConceptV2,
    exclusion: bool,
) -> AppResult<ConceptOutcome> {
    let text = c.text.trim().to_string();
    let confidence = c.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    let normalized = tags::normalize_name(&text);

    // facetHint 收窄（hint 分面不存在时全分面）
    let scope: Option<&str> = c.facet_hint.as_deref().filter(|h| {
        conn.query_row("SELECT 1 FROM tag_facets WHERE key = ?1", [h], |_| Ok(()))
            .is_ok()
    });
    let candidates = tags::search_candidates(conn, scope, &text)?;

    // 1+2：规范名 / alias 精确匹配
    if let Some(t) = candidates.iter().find(|t| {
        t.normalized_name == normalized
            || t.aliases
                .iter()
                .any(|a| tags::normalize_name(a) == normalized)
    }) {
        return Ok(ConceptOutcome::Tag(ResolvedTag {
            facet_key: t.facet_key.clone(),
            text,
            tag_id: t.id,
            path: t.path.clone(),
        }));
    }
    // 3：facetHint 范围内唯一前缀候选 + 高置信
    if confidence >= 0.85 {
        let prefixes: Vec<&tags::Tag> = candidates
            .iter()
            .filter(|t| t.normalized_name.starts_with(&normalized))
            .collect();
        if prefixes.len() == 1 {
            let t = prefixes[0];
            return Ok(ConceptOutcome::Tag(ResolvedTag {
                facet_key: t.facet_key.clone(),
                text,
                tag_id: t.id,
                path: t.path.clone(),
            }));
        }
    }
    // 4：无法可靠映射
    if confidence >= 0.55 {
        // 正向概念 → group 内 content 搜索 leaf；exclusion → 全局 NOT(content)
        Ok(ConceptOutcome::Content(text))
    } else if exclusion {
        Ok(ConceptOutcome::Dropped(format!(
            "未采用排除概念「{text}」（置信度过低，无法可靠映射）"
        )))
    } else {
        Ok(ConceptOutcome::Dropped(format!(
            "未采用概念「{text}」（置信度过低，无法可靠映射）"
        )))
    }
}

/// 在规范化解析后复核 preferred 的 evidence 是否确实指向该概念。
///
/// AI 输出的是规范叶子名，而用户原文可能只出现别名或自然语言变体；因此这里
/// 同时检查标签名、模型使用的表面词和当前事实源中的可搜索别名。这样既保留
/// 「年轻女性」→「青年」这种合法归一化，也继续拦截「蓝天」配「最好清新」这类
/// 原文中没有概念依据的加分项。
fn preferred_evidence_matches(
    conn: &Connection,
    c: &SearchConceptV3,
    outcome: &ConceptOutcome,
) -> AppResult<bool> {
    let Some(evidence) = c.evidence.as_deref() else {
        return Ok(false);
    };
    let evidence = tags::normalize_name(evidence);
    if evidence.is_empty() {
        return Ok(false);
    }
    let contains_term = |term: &str| {
        let normalized = tags::normalize_name(term);
        !normalized.is_empty() && evidence.contains(&normalized)
    };
    match outcome {
        ConceptOutcome::Tag(resolved) => {
            let display_name = resolved
                .path
                .rsplit(" / ")
                .next()
                .unwrap_or(resolved.text.as_str());
            if contains_term(display_name) || contains_term(&resolved.text) {
                return Ok(true);
            }
            Ok(tags::searchable_aliases(conn, resolved.tag_id)?
                .iter()
                .any(|alias| contains_term(alias)))
        }
        ConceptOutcome::Content(term) => Ok(contains_term(term)),
        ConceptOutcome::Dropped(_) => Ok(false),
    }
}

/// §9.5：SearchIntentV2 → QueryExpr（AI 结果唯一执行事实源）。
/// 组内 leaf 独立生成（同一分面「树+花」绝不合并成 mode:any）；组间 OR；
/// exclusions 全局 NOT；最终 AND(positiveRoot, NOT...)。生成后统一 normalize + validate。
pub fn build_expr_from_v2(
    conn: &Connection,
    intent: &SearchIntentV2,
) -> AppResult<(Option<QueryExpr>, Vec<ResolvedTag>, Vec<String>)> {
    let mut warnings = Vec::new();
    let mut resolved_tags: Vec<ResolvedTag> = Vec::new();
    let mut group_exprs: Vec<QueryExpr> = Vec::new();

    for g in &intent.groups {
        let mut leaves: Vec<QueryExpr> = Vec::new();
        if g.asset_type != "all" {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::AssetType {
                    value: g.asset_type.clone(),
                },
            });
        }
        if g.untagged_only {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Untagged,
            });
        }
        for m in &g.metadata {
            // 双保险：漏过 sanitize 的残缺/非法 metadata（如 gte 缺 value）在此就近跳过并提示，
            // 绝不让一条坏条件进入 filter 导致整次 AI 解析失败（与 sanitize_metadata 同一判定，不漂移）。
            if let Err(e) = search_query::compile_metadata(m) {
                warnings.push(format!("已忽略无效的元数据条件（{} {}）：{e}", m.key, m.op));
                continue;
            }
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Metadata { filter: m.clone() },
            });
        }
        for tt in &g.text_terms {
            let scope = match tt.scope.as_str() {
                "content" => crate::db::query_expr::SearchScope::Content,
                "description" => crate::db::query_expr::SearchScope::Description,
                "fileName" => crate::db::query_expr::SearchScope::FileName,
                _ => crate::db::query_expr::SearchScope::All,
            };
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Search {
                    value: tt.text.trim().to_string(),
                    scope,
                },
            });
        }
        for c in &g.concepts {
            match resolve_concept(conn, c, false)? {
                ConceptOutcome::Tag(r) => {
                    leaves.push(QueryExpr::Leaf {
                        cond: LeafCond::Tag {
                            term_query: None,
                            term_match: Default::default(),
                            facet_key: r.facet_key.clone(),
                            tag_ids: vec![r.tag_id],
                            mode: Some("any".into()),
                            include_descendants: true,
                        },
                    });
                    resolved_tags.push(r);
                }
                ConceptOutcome::Content(term) => {
                    // 显式硬条件：显示成可单独移除的「内容：xxx」chip（§9.4）
                    leaves.push(QueryExpr::Leaf {
                        cond: LeafCond::Search {
                            value: term,
                            scope: crate::db::query_expr::SearchScope::Content,
                        },
                    });
                }
                ConceptOutcome::Dropped(w) => warnings.push(w),
            }
        }
        if let Some(e) = pack_and(leaves) {
            group_exprs.push(e);
        }
    }

    // 组间 OR；无组 → 无正向 root
    let positive_root: Option<QueryExpr> = match group_exprs.len() {
        0 => None,
        1 => group_exprs.pop(),
        _ => Some(QueryExpr::Or {
            children: group_exprs,
        }),
    };

    // exclusions：各自 NOT(tag/content)，全局生效
    let mut nots: Vec<QueryExpr> = Vec::new();
    let mut excluded_tag_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for c in &intent.exclusions {
        match resolve_concept(conn, c, true)? {
            ConceptOutcome::Tag(r) => {
                excluded_tag_ids.insert(r.tag_id);
                nots.push(QueryExpr::Not {
                    child: Box::new(QueryExpr::Leaf {
                        cond: LeafCond::Tag {
                            term_query: None,
                            term_match: Default::default(),
                            facet_key: r.facet_key.clone(),
                            tag_ids: vec![r.tag_id],
                            mode: Some("any".into()),
                            include_descendants: true,
                        },
                    }),
                });
                resolved_tags.push(r);
            }
            ConceptOutcome::Content(term) => {
                nots.push(QueryExpr::Not {
                    child: Box::new(QueryExpr::Leaf {
                        cond: LeafCond::Search {
                            value: term,
                            scope: crate::db::query_expr::SearchScope::Content,
                        },
                    }),
                });
            }
            ConceptOutcome::Dropped(w) => warnings.push(w),
        }
    }

    // §9.3：exclusions 与正向概念解析到同一 tagId → 以 exclusion 为准，移除正向该 leaf
    let positive_root: Option<QueryExpr> = if excluded_tag_ids.is_empty() {
        positive_root
    } else {
        match positive_root {
            Some(p) => match without_tag_leaves(&p, &excluded_tag_ids) {
                Some(cleaned) => {
                    warnings.push("部分条件同时被包含与排除，已按排除处理。".into());
                    Some(cleaned)
                }
                None => {
                    warnings.push("部分条件同时被包含与排除，已按排除处理。".into());
                    None
                }
            },
            None => None,
        }
    };

    // §9.5.5：AND(positiveRoot, NOT...)；无正向 root 时可只由 NOT 组成
    let mut root_children: Vec<QueryExpr> = Vec::new();
    if let Some(p) = positive_root {
        root_children.push(p);
    }
    root_children.extend(nots);
    let expr = pack_and(root_children).and_then(normalize_expr);
    if let Some(e) = &expr {
        crate::db::query_expr::validate_expr(e)?;
    }
    Ok((expr, resolved_tags, warnings))
}

/// S3：V3 intent → SearchPlanV3（filter=required AND 组间 OR；should=preferred 加权；
/// must_not=exclusions 的 OR——编译层 NOT()，多排除即 NOT(A OR B) = 全不命中）。
/// ranking：存在 preferred 或用户显式要相关度 → Relevance；否则 Field(sort_by/dir)。
/// 返回 plan + resolved_tags + warnings。与 V2 expr 链路并存（前端 persist 波次切换）。
pub fn build_plan_from_v3(
    conn: &Connection,
    intent: &SearchIntentV3,
) -> AppResult<(
    crate::db::search_plan::SearchPlanV3,
    Vec<ResolvedTag>,
    Vec<String>,
)> {
    use crate::db::search_plan::{
        Ranking, RetrieverPlan, SearchPlanV3, ShouldClause, MAX_SHOULD_CLAUSES,
    };
    let mut warnings = Vec::new();
    let mut resolved_tags: Vec<ResolvedTag> = Vec::new();

    // 把 V3 概念转 V2 引用（resolve_concept 只读 text/role/facet/confidence）
    let as_v2 = |c: &SearchConceptV3| SearchConceptV2 {
        text: c.text.clone(),
        role: c.role.clone(),
        facet_hint: c.facet_hint.clone(),
        confidence: c.confidence,
    };
    // 概念解析为一个可编译 leaf 的 (QueryExpr, ResolvedTag[], warning[])；不捕获外部可变状态。
    let concept_leaf =
        |c: &SearchConceptV3| -> AppResult<(Option<QueryExpr>, Vec<ResolvedTag>, Option<String>)> {
            match resolve_concept(conn, &as_v2(c), false)? {
                ConceptOutcome::Tag(r) => Ok((
                    Some(QueryExpr::Leaf {
                        cond: LeafCond::Tag {
                            // S5 5-3：回填 term_query —— termMatch（contains/prefix/fuzzy）
                            // 只有在词查在场时才会被编译层扩展；恒发 None 会让五种匹配永远不可达。
                            term_query: {
                                let t = c.text.trim();
                                (!t.is_empty()).then(|| t.to_string())
                            },
                            term_match: c.term_match,
                            facet_key: r.facet_key.clone(),
                            tag_ids: vec![r.tag_id],
                            mode: Some("any".into()),
                            include_descendants: true,
                        },
                    }),
                    vec![r],
                    None,
                )),
                ConceptOutcome::Content(term) => Ok((
                    Some(QueryExpr::Leaf {
                        cond: LeafCond::Search {
                            value: term,
                            scope: crate::db::query_expr::SearchScope::Content,
                        },
                    }),
                    Vec::new(),
                    None,
                )),
                ConceptOutcome::Dropped(w) => Ok((None, Vec::new(), Some(w))),
            }
        };
    // 结果收集 helper：分离 warning
    fn collect(
        out_w: &mut Vec<String>,
        out_r: &mut Vec<ResolvedTag>,
        r: (Option<QueryExpr>, Vec<ResolvedTag>, Option<String>),
    ) -> Option<QueryExpr> {
        if let Some(w) = r.2 {
            out_w.push(w);
        }
        out_r.extend(r.1);
        r.0
    }

    // ── filter：每组 required（assetType/metadata/textTerms/concepts）AND，组间 OR ──
    let mut group_exprs: Vec<QueryExpr> = Vec::new();
    for g in &intent.groups {
        let mut leaves: Vec<QueryExpr> = Vec::new();
        if g.asset_type != "all" {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::AssetType {
                    value: g.asset_type.clone(),
                },
            });
        }
        if g.untagged_only {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Untagged,
            });
        }
        for m in &g.metadata {
            // 双保险：漏过 sanitize 的残缺/非法 metadata（如 gte 缺 value）在此就近跳过并提示，
            // 绝不让一条坏条件进入 filter 导致整次 AI 解析失败（与 sanitize_metadata 同一判定，不漂移）。
            if let Err(e) = search_query::compile_metadata(m) {
                warnings.push(format!("已忽略无效的元数据条件（{} {}）：{e}", m.key, m.op));
                continue;
            }
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Metadata { filter: m.clone() },
            });
        }
        for tt in &g.text_terms {
            let scope = match tt.scope.as_str() {
                "content" => crate::db::query_expr::SearchScope::Content,
                "description" => crate::db::query_expr::SearchScope::Description,
                "fileName" => crate::db::query_expr::SearchScope::FileName,
                _ => crate::db::query_expr::SearchScope::All,
            };
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Search {
                    value: tt.text.trim().to_string(),
                    scope,
                },
            });
        }
        for c in &g.concepts {
            if let Some(leaf) = collect(&mut warnings, &mut resolved_tags, concept_leaf(c)?) {
                leaves.push(leaf);
            }
        }
        if let Some(e) = pack_and(leaves) {
            group_exprs.push(e);
        }
    }
    let filter = match group_exprs.len() {
        0 => None,
        1 => group_exprs.pop(),
        _ => Some(QueryExpr::Or {
            children: group_exprs,
        }),
    };

    // ── must_not：exclusions 的 OR（被排除条件命中即整组排除）──
    let mut excl_exprs: Vec<QueryExpr> = Vec::new();
    let mut excluded_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for c in &intent.exclusions {
        match resolve_concept(conn, &as_v2(c), true)? {
            ConceptOutcome::Tag(r) => {
                excluded_ids.insert(r.tag_id);
                excl_exprs.push(QueryExpr::Leaf {
                    cond: LeafCond::Tag {
                        // S5 5-3：排除路径同样回填词查（termMatch 语义一致）
                        term_query: {
                            let t = c.text.trim();
                            (!t.is_empty()).then(|| t.to_string())
                        },
                        term_match: c.term_match,
                        facet_key: r.facet_key.clone(),
                        tag_ids: vec![r.tag_id],
                        mode: Some("any".into()),
                        include_descendants: true,
                    },
                });
                resolved_tags.push(r);
            }
            ConceptOutcome::Content(term) => {
                excl_exprs.push(QueryExpr::Leaf {
                    cond: LeafCond::Search {
                        value: term,
                        scope: crate::db::query_expr::SearchScope::Content,
                    },
                });
            }
            ConceptOutcome::Dropped(w) => warnings.push(w),
        }
    }
    // filter 与排除撞同一 tagId → 以排除为准移除 filter 内该 leaf（沿用 §9.3 规则）
    let filter = if excluded_ids.is_empty() {
        filter
    } else {
        match filter {
            Some(f) => without_tag_leaves(&f, &excluded_ids),
            None => None,
        }
    };
    let must_not = match excl_exprs.len() {
        0 => None,
        1 => excl_exprs.pop(),
        _ => Some(QueryExpr::Or {
            children: excl_exprs,
        }),
    };

    // ── should：preferred 概念逐个 resolve（不命中不淘汰，只加分）──
    let mut should: Vec<ShouldClause> = Vec::new();
    for g in &intent.groups {
        for c in &g.preferred {
            // 规范名与用户原文可能是同义词；先按数据库词典复核 evidence，
            // 再进入 should，避免在解析阶段用规范名字面比较造成误删。
            let preferred_outcome = resolve_concept(conn, &as_v2(c), false)?;
            if c.evidence.is_some()
                && !matches!(&preferred_outcome, ConceptOutcome::Dropped(_))
                && !preferred_evidence_matches(conn, c, &preferred_outcome)?
            {
                warnings.push(format!(
                    "「{}」的加分依据与概念本身无关（应摘原文里围绕该词的片段），已忽略该加分项。",
                    c.text
                ));
                continue;
            }
            if let Some(leaf) = collect(&mut warnings, &mut resolved_tags, concept_leaf(c)?) {
                let label = format!("{}（加分项）", c.text.trim());
                // §4.3：权重收拢到三档 0.5/1.0/2.0（后端校验只认这三档）
                let weight = snap_to_allowed_weight(c.weight.unwrap_or(1.0));
                let cond = match leaf {
                    QueryExpr::Leaf { cond } => cond,
                    _ => continue,
                };
                should.push(ShouldClause {
                    cond,
                    weight,
                    label,
                    // §4.8：evidence 原文回显（不拼进 label 丢失）
                    evidence: c.evidence.clone(),
                });
            }
        }
    }

    if should.len() > MAX_SHOULD_CLAUSES {
        warnings.push(format!(
            "加分项超过 {MAX_SHOULD_CLAUSES} 条，已按当前优先顺序保留前 {MAX_SHOULD_CLAUSES} 条。"
        ));
        should.truncate(MAX_SHOULD_CLAUSES);
    }

    // ranking：有 preferred → 相关度（should 加权）；否则按意图 sort（命令层给默认）。
    // B10：Ranking::Relevance 无载荷 —— plan.retrievers 是唯一来源。
    let ranking = if !should.is_empty() {
        Ranking::Relevance
    } else {
        Ranking::Field {
            key: intent
                .sort_by
                .clone()
                .unwrap_or_else(|| "created_at".into()),
            dir: intent.sort_dir.clone().unwrap_or_else(|| "desc".into()),
        }
    };
    let plan = SearchPlanV3 {
        filter,
        must_not,
        should,
        minimum_should_match: 0,
        retrievers: RetrieverPlan::default(),
        ranking,
        ..Default::default()
    };
    crate::db::search_plan::validate_search_plan(&plan)?;
    Ok((plan, resolved_tags, warnings))
}

/// 从表达式树移除包含指定 tagId 的 Tag leaf（排除优先）。返回 None 表示正向部分被清空。
fn without_tag_leaves(
    expr: &QueryExpr,
    excluded: &std::collections::HashSet<i64>,
) -> Option<QueryExpr> {
    match expr {
        QueryExpr::Leaf {
            cond: LeafCond::Tag { tag_ids, .. },
        } => {
            if tag_ids.iter().any(|t| excluded.contains(t)) {
                None
            } else {
                Some(expr.clone())
            }
        }
        QueryExpr::And { children } => {
            let kept: Vec<QueryExpr> = children
                .iter()
                .filter_map(|c| without_tag_leaves(c, excluded))
                .collect();
            pack_and(kept)
        }
        QueryExpr::Or { children } => {
            let kept: Vec<QueryExpr> = children
                .iter()
                .filter_map(|c| without_tag_leaves(c, excluded))
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next(),
                _ => Some(QueryExpr::Or { children: kept }),
            }
        }
        QueryExpr::Not { child } => {
            without_tag_leaves(child, excluded).map(|c| QueryExpr::Not { child: Box::new(c) })
        }
        other => Some(other.clone()),
    }
}

fn pack_and(leaves: Vec<QueryExpr>) -> Option<QueryExpr> {
    match leaves.len() {
        0 => None,
        1 => leaves.into_iter().next(),
        _ => Some(QueryExpr::And { children: leaves }),
    }
}

/// §4.3：AI 权重收拢到三档（轻微 0.5 / 一般 1.0 / 强烈 2.0）。
/// 后端 validate_search_plan 只认这三档（0.5/1.0/2.0），非有限值回退 1.0。
fn snap_to_allowed_weight(w: f32) -> f32 {
    const ALLOWED: [f32; 3] = [0.5, 1.0, 2.0];
    if !w.is_finite() {
        return 1.0;
    }
    ALLOWED
        .iter()
        .copied()
        .min_by(|x, y| {
            (x - w)
                .abs()
                .partial_cmp(&(y - w).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(1.0)
}

// ═══════════════ 词典（§9.2.1） ═══════════════

const DICT_MAX_PER_FACET: usize = 50;
const DICT_MAX_ALIASES: usize = 5;
const DICT_CHAR_CAP: usize = 6000;

/// 提取分面标签词典（§9.2.1）：只读 active 标签 + is_searchable=1 的别名；
/// 每分面最多 50 个规范标签、每标签最多 5 个别名；去除换行与控制字符；
/// 超字符上限时按分面公平截断（不让一个分面占满上下文）。
pub fn collect_tag_dictionary(
    conn: &Connection,
    facets: &[FacetPromptContext],
) -> AppResult<Vec<String>> {
    let mut per_facet: Vec<Vec<String>> = Vec::new();
    for f in facets {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, p.name
               FROM tags t
              LEFT JOIN tags p ON p.id = t.parent_id
              WHERE t.facet_key = ?1 AND COALESCE(t.status,'active') = 'active'
              ORDER BY t.sort_order, t.id LIMIT ?2",
        )?;
        // 先收集行再读取别名，避免在同一个 SQLite statement 仍借用连接时
        // 重新查询 tag_terms/tag_aliases。
        let rows = stmt
            .query_map(
                rusqlite::params![f.key.as_str(), DICT_MAX_PER_FACET as i64],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut lines = Vec::new();
        for (tag_id, name, parent) in rows {
            let name = sanitize_dict_text(&name);
            if name.is_empty() {
                continue;
            }
            let path = match parent
                .map(|value| sanitize_dict_text(&value))
                .filter(|value| !value.is_empty())
            {
                Some(parent) => format!("{parent} / {name}"),
                None => name.clone(),
            };
            let aliases = tags::searchable_aliases(conn, tag_id)?;
            let list: Vec<String> = aliases
                .into_iter()
                .take(DICT_MAX_ALIASES)
                .map(|s| sanitize_dict_text(s.trim()))
                .filter(|s| !s.is_empty())
                .collect();
            let line = if list.is_empty() {
                format!("{path} | term: {name}")
            } else {
                format!("{path} | term: {name} | aliases: {}", list.join(", "))
            };
            lines.push(line);
        }
        per_facet.push(lines);
    }

    // 公平截断：总字符超限时，迭代丢弃「当前最长的分面」的最后一行
    let mut total_chars: usize = per_facet.iter().flatten().map(|l| l.chars().count()).sum();
    while total_chars > DICT_CHAR_CAP {
        let mut longest = 0usize;
        for (i, lines) in per_facet.iter().enumerate() {
            if !lines.is_empty()
                && (longest == usize::MAX
                    || lines.iter().map(|l| l.chars().count()).sum::<usize>()
                        > per_facet[longest]
                            .iter()
                            .map(|l| l.chars().count())
                            .sum::<usize>())
            {
                longest = i;
            }
        }
        let Some(dropped) = per_facet[longest].pop() else {
            break;
        };
        total_chars -= dropped.chars().count();
    }
    Ok(per_facet.into_iter().flatten().collect())
}

fn sanitize_dict_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

// ═══════════════ 解析与网络请求 ═══════════════

/// 解析模型回复为 SearchIntentV2。兼容 {query:{...}} 包一层。
pub fn parse_intent(content: &str) -> AppResult<SearchIntentV2> {
    let trimmed = content.trim();
    let parsed: Option<serde_json::Value> = serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        serde_json::from_str(&trimmed[start..=end]).ok()
    });
    let Some(v) = parsed else {
        return Err(AppError::msg("AI 未返回可解析的 JSON"));
    };
    let value = if v.get("query").is_some() && v.get("groups").is_none() {
        v.get("query").cloned().unwrap_or(v)
    } else {
        v
    };
    serde_json::from_value::<SearchIntentV2>(value)
        .map_err(|e| AppError::msg(format!("AI JSON 校验失败：{e}")))
}

/// S3：解析模型回复为 SearchIntentV3（含 preferred 加分项）。解包逻辑与 parse_intent 相同。
pub fn parse_intent_v3(content: &str) -> AppResult<SearchIntentV3> {
    let trimmed = content.trim();
    let parsed: Option<serde_json::Value> = serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        serde_json::from_str(&trimmed[start..=end]).ok()
    });
    let Some(v) = parsed else {
        return Err(AppError::msg("AI 未返回可解析的 JSON"));
    };
    let value = if v.get("query").is_some() && v.get("groups").is_none() {
        v.get("query").cloned().unwrap_or(v)
    } else {
        v
    };
    serde_json::from_value::<SearchIntentV3>(value)
        .map_err(|e| AppError::msg(format!("AI JSON 校验失败：{e}")))
}

/// V3 → V2 视图：concepts 降为 V2（暂时丢弃 preferred；调用方在回写 V3 时保留它）。
/// 清洗/守卫复用 V2 全套函数。
pub fn v3_to_v2_view(intent: &SearchIntentV3) -> SearchIntentV2 {
    SearchIntentV2 {
        groups: intent
            .groups
            .iter()
            .map(|g| SearchGroupV2 {
                asset_type: g.asset_type.clone(),
                concepts: g
                    .concepts
                    .iter()
                    .map(|c| SearchConceptV2 {
                        text: c.text.clone(),
                        role: c.role.clone(),
                        facet_hint: c.facet_hint.clone(),
                        confidence: c.confidence,
                    })
                    .collect(),
                text_terms: g.text_terms.clone(),
                metadata: g.metadata.clone(),
                untagged_only: g.untagged_only,
            })
            .collect(),
        exclusions: intent
            .exclusions
            .iter()
            .map(|c| SearchConceptV2 {
                text: c.text.clone(),
                role: c.role.clone(),
                facet_hint: c.facet_hint.clone(),
                confidence: c.confidence,
            })
            .collect(),
        sort_by: intent.sort_by.clone(),
        sort_dir: intent.sort_dir.clone(),
    }
}

/// V2 → V3 组（concepts 提升为 V3 概念，necessity=Required；preferred 空）。
pub fn v2_group_to_v3(g: &SearchGroupV2) -> SearchGroupV3 {
    SearchGroupV3 {
        asset_type: g.asset_type.clone(),
        concepts: g
            .concepts
            .iter()
            .map(|c| SearchConceptV3 {
                text: c.text.clone(),
                role: c.role.clone(),
                facet_hint: c.facet_hint.clone(),
                confidence: c.confidence,
                necessity: Necessity::Required,
                weight: None,
                evidence: None,
                term_match: crate::db::tags::TermMatch::Alias,
            })
            .collect(),
        text_terms: g.text_terms.clone(),
        metadata: g.metadata.clone(),
        untagged_only: g.untagged_only,
        preferred: Vec::new(),
    }
}

/// S3：V2 intent → V3 intent（不含 preferred；守卫后仍无加分项的路径用）。
pub fn v2_to_v3(intent: &SearchIntentV2) -> SearchIntentV3 {
    SearchIntentV3 {
        groups: intent.groups.iter().map(v2_group_to_v3).collect(),
        exclusions: intent
            .exclusions
            .iter()
            .map(|c| SearchConceptV3 {
                text: c.text.clone(),
                role: c.role.clone(),
                facet_hint: c.facet_hint.clone(),
                confidence: c.confidence,
                necessity: Necessity::Required,
                weight: None,
                evidence: None,
                term_match: crate::db::tags::TermMatch::Alias,
            })
            .collect(),
        sort_by: intent.sort_by.clone(),
        sort_dir: intent.sort_dir.clone(),
    }
}

/// S3 关键词兜底（V3 形态：整句进 textTerms scope=all）。
pub fn keyword_intent_v3(text: &str) -> SearchIntentV3 {
    SearchIntentV3 {
        groups: vec![SearchGroupV3 {
            asset_type: "all".into(),
            concepts: vec![],
            text_terms: vec![IntentTextTerm {
                text: text.trim().to_string(),
                scope: "all".into(),
            }],
            metadata: vec![],
            untagged_only: false,
            preferred: Vec::new(),
        }],
        exclusions: vec![],
        sort_by: None,
        sort_dir: None,
    }
}

// 网络 + 解析 + 校验：由调用方在短锁内收集 facets/dict 后，再在锁外调用本函数。
// 不持有 DB 锁；text 已由命令层校验长度。
//
// W6-2（§W6-2）三层降级：① strict 正常解析 → ② lenient 剔除非法项保留其余 + warning
// → ③ 关键词兜底（永不失败）。唯一例外：配置类错误（鉴权/连不上/超时）仍真报错
// —— 配置问题必须让用户知道，而不是假装搜到了。
// ═══════════════ C-3：库能力注入（只告知，不改写） ═══════════════

/// 库能力摘要缓存（60s TTL，全局单库）。措辞必须是陈述事实而非禁令。
static CAPS_CACHE: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// 实时库能力摘要 —— 注入 user prompt 让模型预判哪些条件可能 0 结果。
/// 后端绝不做基于库统计的条件剔除（P5）；用户明确搜无结果条件 → 如实输出，系统解释。
pub fn library_capabilities(conn: &Connection) -> AppResult<String> {
    // cfg!(test)：测试构建下不走全局缓存 —— 每个测试是独立内存库，
    // 共享 static 会把上一个测试库的统计泄漏给下一个测试（并行交错）。
    if !cfg!(test) {
        if let Ok(guard) = CAPS_CACHE.lock() {
            if let Some((at, cached)) = guard.as_ref() {
                if at.elapsed().as_secs() < 60 {
                    return Ok(cached.clone());
                }
            }
        }
    }
    let (total, imgs, vids): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN mime_type LIKE 'image/%' THEN 1 ELSE 0 END),
                SUM(CASE WHEN mime_type LIKE 'video/%' THEN 1 ELSE 0 END)
           FROM assets WHERE deleted_at IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1).unwrap_or(0), r.get(2).unwrap_or(0))),
    )?;
    let gps: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assets WHERE deleted_at IS NULL AND latitude IS NOT NULL AND longitude IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let (taken_n, min_d, max_d): (i64, Option<String>, Option<String>) = conn.query_row(
        "SELECT COUNT(*), MIN(strftime('%Y-%m-%d', taken_at/1000,'unixepoch','localtime')),
                MAX(strftime('%Y-%m-%d', taken_at/1000,'unixepoch','localtime'))
           FROM assets WHERE deleted_at IS NULL AND taken_at IS NOT NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (sz_min, sz_max): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT MIN(file_size), MAX(file_size) FROM assets WHERE deleted_at IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut line = String::new();
    line.push_str("本库现状（供你判断哪些条件可能无结果，但用户明确要求时仍应输出）：\n");
    line.push_str(&format!("- {total} 张，图片 {imgs} / 视频 {vids}\n"));
    line.push_str(&format!("- 定位：{gps} 张有 GPS\n"));
    match (taken_n, min_d, max_d) {
        (0, _, _) => line.push_str("拍摄时间：0 张有值\n"),
        (n, Some(a), Some(b)) => {
            line.push_str(&format!("- 拍摄时间：{n} 张有值，范围 {a} ~ {b}\n"))
        }
        _ => line.push_str("- 拍摄时间：少量有值\n"),
    }
    match (sz_min, sz_max) {
        (Some(mn), Some(mx)) => line.push_str(&format!(
            "- 文件大小：{:.2} MB ~ {:.2} MB（= {} ~ {} 字节）\n",
            mn as f64 / 1048576.0,
            mx as f64 / 1048576.0,
            mn,
            mx
        )),
        _ => line.push_str("- 文件大小：无样本\n"),
    }
    let exts: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(lower(file_ext),'?') AS e, COUNT(*) c FROM assets
              WHERE deleted_at IS NULL GROUP BY e ORDER BY c DESC LIMIT 3",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    if !exts.is_empty() {
        let fmt = exts
            .iter()
            .map(|(e, c)| format!("{e} {c}"))
            .collect::<Vec<_>>()
            .join("、");
        line.push_str(&format!("- 格式：{fmt}\n"));
    }
    let tag_rows: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT t.facet_key, COUNT(DISTINCT t.id) c FROM tags t
              WHERE COALESCE(t.status,'active')='active' GROUP BY t.facet_key ORDER BY c DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    if !tag_rows.is_empty() {
        let n: i64 = tag_rows.iter().map(|(_, c)| c).sum();
        let parts = tag_rows
            .iter()
            .map(|(k, c)| format!("{k} {c}"))
            .collect::<Vec<_>>()
            .join(" / ");
        line.push_str(&format!("- 标签 {n} 个：{parts}\n"));
    }
    line.push_str("如果用户的条件在本库必然无结果，仍要如实输出该条件 —— 系统会解释原因。");
    if !cfg!(test) {
        if let Ok(mut guard) = CAPS_CACHE.lock() {
            *guard = Some((Instant::now(), line.clone()));
        }
    }
    Ok(line)
}

/// C-3：组装 user prompt（标签词典 + 分面说明 + 查询 + 库能力摘要）。
/// 抽成纯函数便于断言注入；capabilities 为空时不注入（短锁失败的静默降级路径）。
pub fn build_user_prompt(
    dict: &[String],
    facets: &[FacetPromptContext],
    text: &str,
    capabilities: &str,
) -> String {
    let mut user = String::from(
        "标签词典（父类 / 叶子 | term: 规范叶子名 | aliases: 可搜索别名）\n\
         路径仅用于判断语义归类；输出 concept.text 时只写 term 对应的叶子名。\n",
    );
    for t in dict {
        user.push_str(&format!("- {t}\n"));
    }
    user.push_str("\n分面说明\n");
    for f in facets {
        // P1：max_items=None = 数量不限 —— 必须与打标侧（ai_cloud build_user_prompt 的
        // 「数量不限」）同语义，绝不能把「不限」渲染成 max=3（模型会照抄成硬上限）。
        let max_desc = match f.max_items {
            Some(n) => format!("max={n}"),
            None => "max=不限（无数量上限）".to_string(),
        };
        user.push_str(&format!(
            "- {}(key={}) selection={} {max_desc}: {}\n",
            f.display_name,
            f.key,
            f.selection_mode,
            f.description // W2-1：hint 已并入 description（V20 合表）
        ));
    }
    user.push_str("\n用户查询：<query>");
    user.push_str(text);
    user.push_str("</query>\n请输出解析结果。");
    // C-3：库能力注入 —— 放在 prompt 末尾作为独立小节（只告知，不改写）
    if !capabilities.trim().is_empty() {
        user.push('\n');
        user.push_str(capabilities);
    }
    user
}

pub fn request_intent(
    cfg: &AiSettings,
    text: &str,
    facets: &[FacetPromptContext],
    dict: &[String],
    capabilities: &str,
) -> AppResult<(SearchIntentV3, Vec<String>)> {
    let started = std::time::Instant::now();
    let profile = cfg
        .active()
        .ok_or_else(|| AppError::msg("请先在设置页添加 API 配置"))?;
    if profile.base_url.trim().is_empty() {
        return Err(AppError::msg("当前 API 配置缺少 base_url"));
    }
    tracing::info!(
        operation = "super_search_ai",
        stage = "request_start",
        model = %profile.model,
        query_chars = text.chars().count(),
        "AI 搜索解析开始"
    );

    let schema = intent_schema(facets);
    // 用户可在设置页覆盖搜索 system prompt（非空优先；空 = 内置默认）
    let system = if cfg.system_prompt_search.trim().is_empty() {
        build_system_prompt(facets)
    } else {
        cfg.system_prompt_search.clone()
    };
    let user = build_user_prompt(dict, facets, text, capabilities);

    // ① 网络请求（协议级降级 Structured → JsonObject → Plain 由 request_text_json 内部处理）。
    // 配置类错误（鉴权/连不上/超时）真报错；其他服务异常降级为关键词搜索（不打扰）。
    let (_tier, raw) = match ai_cloud::request_text_json(
        profile,
        &system,
        &user,
        Some(schema.clone()),
        TextJsonTier::Structured,
    ) {
        Ok(v) => v,
        Err(e) if is_config_error(&e) => {
            tracing::warn!(
                operation = "super_search_ai",
                stage = "config_error",
                error_code = e.code(),
                error = %e,
                duration_ms = started.elapsed().as_millis() as u64,
                "AI 搜索解析配置错误"
            );
            return Err(e);
        }
        Err(e) => {
            tracing::warn!(
                operation = "super_search_ai",
                stage = "keyword_fallback",
                error_code = e.code(),
                error = %e,
                duration_ms = started.elapsed().as_millis() as u64,
                "AI 搜索解析失败，已降级为关键词"
            );
            return Ok(keyword_fallback_v3(text));
        }
    };
    if ai_cloud::is_degenerate_text(&raw) {
        // 持续乱码/复读：属模型能力问题而非配置问题 → 关键词兜底
        tracing::warn!(
            operation = "super_search_ai",
            stage = "degenerate_fallback",
            duration_ms = started.elapsed().as_millis() as u64,
            "AI 搜索解析返回异常文本，已降级为关键词"
        );
        return Ok(keyword_fallback_v3(text));
    }
    // ②/③/④：V3 解析（含 preferred evidence 守卫）+ lenient + 结构校验，全部失败落第 3 层
    let (mut intent, mut warnings) = degrade_parse_v3(&raw, text, facets);
    // 模型偶尔会把“最好/优先”短语里的规范标签放进 concepts。
    // 解析阶段拿不到数据库连接，但这里的 dictionary 正是同一轮请求读取的当前标签词典，
    // 可用于把“年轻”→“青年”这类别名安全地归回 preferred。
    warnings.retain(|warning| {
        warning != "原文包含偏好表达，但未能生成可选加分条件；未自动改写，请检查解析结果。"
    });
    warnings.extend(repair_misplaced_preferred(text, dict, &mut intent));
    warnings.extend(warn_missing_preferred(text, &intent.groups));
    tracing::info!(
        operation = "super_search_ai",
        stage = "request_done",
        warnings = warnings.len(),
        duration_ms = started.elapsed().as_millis() as u64,
        "AI 搜索解析完成"
    );
    Ok((intent, warnings))
}

fn keyword_fallback_v3(text: &str) -> (SearchIntentV3, Vec<String>) {
    (
        keyword_intent_v3(text),
        vec!["未能理解搜索条件，已按关键词搜索。".into()],
    )
}

/// W6-2：解析层三层降级的纯函数（不触网，单测直接打）。
/// ① strict 解析 → ② lenient 剔除非法项（sanitize_all）→ ③ 关键词兜底。
pub fn degrade_parse(
    raw: &str,
    text: &str,
    facets: &[FacetPromptContext],
) -> (SearchIntentV2, Vec<String>) {
    let mut intent = match parse_intent(raw) {
        Ok(i) => i,
        Err(_) => return keyword_fallback(text),
    };
    // lenient：部分剔除规则（W6-3），原则「能救一条算一条」
    let mut warnings = sanitize_all(&mut intent, facets);
    warnings.extend(warn_missing_explicit_metadata(text, &intent));
    // 只有排除项没有正向主体时，无法形成用户期望的搜索集合；按整句关键词兜底，
    // 避免「乱码 + 不要 X」退化成几乎全库的纯 mustNot 查询。
    if intent.groups.is_empty() {
        warnings.push("未能理解搜索条件，已按关键词搜索。".into());
        return (keyword_intent(text), warnings);
    }
    if let Err(e) = validate_intent(&intent, facets) {
        warnings.push(format!("解析结果不合规（{e}），已按关键词搜索。"));
        return (keyword_intent(text), warnings);
    }
    (intent, warnings)
}

/// S3：V3 解析层（V3→V2→关键词 三层降级）。
/// ① strict V3：parse SearchIntentV3（含 preferred）→ 每组跑 guard_preferred（evidence 守卫）
///    → V3 侧清洗（concepts/preferred 都过 clean_concepts_v3）→ 转 V2 视图跑
///    sanitize_all_preserving_preferred + guard_intent + validate_intent（复用既有确定性守卫）
///    → 通过则保留 preferred 返回 V3。模型把偏好放进 concepts 的纠偏在 request_intent
///    中结合本轮标签词典执行，避免把规范名/别名误判为硬条件。
/// ② V3 解析失败 → 试 V2（把 preferred 当 concepts 的语义 = V2 JSON 无 preferred 字段）→ v2_to_v3。
/// ③ 都失败 → 关键词兜底（V3 形态）。永不 Err。
pub fn degrade_parse_v3(
    raw: &str,
    text: &str,
    facets: &[FacetPromptContext],
) -> (SearchIntentV3, Vec<String>) {
    let mut intent = match parse_intent_v3(raw) {
        Ok(i) => i,
        Err(_) => {
            // ② V3 失败 → V2（V2 JSON 本身无 preferred；degrade_parse 内部再落关键词）
            let (v2, w) = degrade_parse(raw, text, facets);
            return (v2_to_v3(&v2), w);
        }
    };
    // ① strict V3 路径
    let mut warnings: Vec<String> = Vec::new();
    // evidence 守卫（无效 preferred 只丢弃；绝不反向升级为 required）
    for g in &mut intent.groups {
        warnings.extend(guard_preferred(text, g));
    }
    // V3 侧概念清洗（preferred 与 concepts 同规则；空/句子化/停用词剔除）
    let mut total = 0usize;
    for g in &mut intent.groups {
        clean_concepts_v3(&mut g.concepts, &mut warnings, &mut total);
        clean_concepts_v3(&mut g.preferred, &mut warnings, &mut total);
    }
    clean_concepts_v3(&mut intent.exclusions, &mut warnings, &mut total);
    warnings.extend(warn_missing_preferred(text, &intent.groups));
    if total > 20 {
        warnings.push(format!(
            "条件概念较多（{total} 个），已按置信度优先截取 20 个。"
        ));
    }
    if intent.groups.is_empty() {
        warnings.push("未能理解搜索条件，已按关键词搜索。".into());
        return (keyword_intent_v3(text), warnings);
    }
    // V2 视图确定性守卫（assetType / OR 合并 / 组去重 / metadata 白名单编译）
    let mut v2_view = v3_to_v2_view(&intent);
    // V2 视图看不到 preferred；纯“最好有 X”查询虽然 filter 为空，仍必须保留
    // 这个占位 group，才能把 preferred 继续带到 SearchPlanV3.should。没有任何
    // preferred 时仍使用普通空组清理，避免畸形空 JSON 退化成全库查询。
    if intent.groups.iter().any(|g| !g.preferred.is_empty()) {
        warnings.extend(sanitize_all_preserving_preferred(&mut v2_view, facets));
    } else {
        warnings.extend(sanitize_all(&mut v2_view, facets));
    }
    warnings.extend(guard_intent(text, &mut v2_view));
    warnings.extend(warn_missing_explicit_metadata(text, &v2_view));
    if v2_view.groups.is_empty() {
        warnings.push("未能理解搜索条件，已按关键词搜索。".into());
        return (keyword_intent_v3(text), warnings);
    }
    if let Err(e) = validate_intent(&v2_view, facets) {
        warnings.push(format!("解析结果不合规（{e}），已按关键词搜索。"));
        return (keyword_intent_v3(text), warnings);
    }
    // 把 V2 视图的清洗结果（组数/assetType/组顺序）映射回 V3，保留 preferred
    // （sanitize_all 只剔 metadata/组；guard_intent 可能合并 OR 组 —— 按组序映射 concepts）
    if intent.groups.len() == v2_view.groups.len() {
        for (g3, g2) in intent.groups.iter_mut().zip(v2_view.groups.iter()) {
            g3.asset_type = g2.asset_type.clone();
            // 关键：sanitize_all 在 V2 视图上丢弃了编译不过的 metadata（如 gte 缺 value、
            // 未知 key、非法值），必须把清洗后的 metadata 回写 V3 —— 否则残缺条件仍留在 V3，
            // build_plan_from_v3 编译时会以「字段 x 的 op 需要 value」整体报错，拖垮整次 AI 解析。
            g3.metadata = g2.metadata.clone();
            // concepts 的 text/facet 已在 V2 清洗中 trim/去停用/去重 —— 直接替换
            //（preferred 不进 V2 视图，保留原 V3 值）
            g3.concepts = g2
                .concepts
                .iter()
                .map(|c| SearchConceptV3 {
                    text: c.text.clone(),
                    role: c.role.clone(),
                    facet_hint: c.facet_hint.clone(),
                    confidence: c.confidence,
                    necessity: Necessity::Required,
                    weight: None,
                    evidence: None,
                    term_match: crate::db::tags::TermMatch::Alias,
                })
                .collect();
        }
        intent.exclusions = v2_view
            .exclusions
            .iter()
            .map(|c| SearchConceptV3 {
                text: c.text.clone(),
                role: c.role.clone(),
                facet_hint: c.facet_hint.clone(),
                confidence: c.confidence,
                necessity: Necessity::Required,
                weight: None,
                evidence: None,
                term_match: crate::db::tags::TermMatch::Alias,
            })
            .collect();
    } else {
        // guard_intent 合并了组（无 OR 词多组 → 单 AND 组）。
        // 旧实现直接 v2_to_v3 重建，会把模型误拆到独立组里的 preferred 加分项全部丢弃、
        // 并经 concepts 通道升级成硬必须（「优先近景」被错误当成「必须近景」）。
        // 这里改为：合并必须条件的同时，把原各组 preferred 汇总保留为加分项。
        warnings.push("多组条件已合并为同时满足，其中的优先项仍按加分（不淘汰结果）处理。".into());
        let rescued_preferred: Vec<SearchConceptV3> =
            intent.groups.drain(..).flat_map(|g| g.preferred).collect();
        let mut merged = v2_to_v3(&v2_view);
        if !rescued_preferred.is_empty() {
            if let Some(g0) = merged.groups.get_mut(0) {
                // 互斥：已作为必须条件的同名概念不再重复加分
                let required: std::collections::HashSet<String> =
                    g0.concepts.iter().map(|c| c.text.clone()).collect();
                for c in rescued_preferred {
                    if !required.contains(&c.text) {
                        g0.preferred.push(c);
                    }
                }
            }
        }
        intent = merged;
    }
    (intent, warnings)
}

/// V3 概念清洗：与 clean_concepts（V2）同规则，保留 V3 特有字段（evidence/weight/term_match）。
fn clean_concepts_v3(
    concepts: &mut Vec<SearchConceptV3>,
    warnings: &mut Vec<String>,
    total: &mut usize,
) {
    let mut kept: Vec<SearchConceptV3> = Vec::new();
    for c in concepts.drain(..) {
        let mut text = c.text.trim().to_string();
        text = text
            .trim_matches(|ch: char| CONCEPT_EDGE_PUNCT.contains(&ch))
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        if SEARCH_CONCEPT_STOPWORDS.iter().any(|w| *w == text) {
            continue;
        }
        if text.chars().count() > MAX_CONCEPT_CHARS {
            warnings.push(format!("「{text}」是句子而非原子概念，已忽略。"));
            continue;
        }
        let confidence = c.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
        kept.push(SearchConceptV3 {
            text,
            role: c.role.trim().to_string(),
            facet_hint: c
                .facet_hint
                .as_ref()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty()),
            confidence: Some(confidence),
            necessity: c.necessity,
            weight: c.weight.map(|w| w.clamp(0.5, 2.0)),
            evidence: c
                .evidence
                .as_ref()
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty()),
            term_match: c.term_match,
        });
        *total += 1;
    }
    *concepts = kept;
}

/// W6-2：配置类错误判定（鉴权 / 连不上 / 超时）—— 这类错误必须真报错，不做降级。
pub fn is_config_error(e: &AppError) -> bool {
    let m = e.to_string().to_lowercase();
    [
        "401",
        "403",
        "api key",
        "apikey",
        "unauthorized",
        "authentication",
        "鉴权",
        "无法连接",
        "连接失败",
        "error sending request",
        "timeout",
        "timed out",
        "超时",
        "请求失败",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// W6-2 第 3 层：关键词兜底 intent —— 整句进 textTerms(scope=all)，永不失败。
/// pub：命令层在 validate_expr 失败回退时也会构造兜底 intent 重新生成 expr。
pub fn keyword_intent(text: &str) -> SearchIntentV2 {
    SearchIntentV2 {
        groups: vec![SearchGroupV2 {
            asset_type: "all".into(),
            concepts: vec![],
            text_terms: vec![IntentTextTerm {
                text: text.trim().to_string(),
                scope: "all".into(),
            }],
            metadata: vec![],
            untagged_only: false,
        }],
        exclusions: vec![],
        sort_by: None,
        sort_dir: None,
    }
}

fn keyword_fallback(text: &str) -> (SearchIntentV2, Vec<String>) {
    (
        keyword_intent(text),
        vec!["未能理解搜索条件，已按关键词搜索。".into()],
    )
}

/// 判断 intent 是否处于关键词兜底态（命令层据此输出「按关键词搜索」解释文案）。
pub fn is_keyword_fallback(intent: &SearchIntentV2, text: &str) -> bool {
    intent.groups.len() == 1
        && intent.exclusions.is_empty()
        && intent.groups[0].asset_type == "all"
        && intent.groups[0].concepts.is_empty()
        && intent.groups[0].metadata.is_empty()
        && intent.groups[0].text_terms.len() == 1
        && intent.groups[0].text_terms[0].scope == "all"
        && intent.groups[0].text_terms[0].text.trim() == text.trim()
}

/// S3：V3 intent 的关键词兜底判定（V3 形态 —— 判断逻辑与 V2 相同，组可带空 preferred）。
pub fn is_keyword_fallback_v3(intent: &SearchIntentV3, text: &str) -> bool {
    is_keyword_fallback(&v3_to_v2_view(intent), text)
}

/// W6-3 部分解析剔除规则：能救一条算一条，全部不合法才落第 3 层。
/// 覆盖：sortBy / sortDir / assetType 非法→默认；单条 metadata 非法→剔除（复用 sanitize_metadata）；
/// concept.facetHint 未知→降级全分面搜索；空概念剔除；全空 group 剔除。
pub fn sanitize_all(intent: &mut SearchIntentV2, facets: &[FacetPromptContext]) -> Vec<String> {
    sanitize_all_with_options(intent, facets, false)
}

/// V3 解析专用清洗：V2 视图可能只有一个由 preferred 支撑的空 group，不能在
/// 丢失 preferred 的视图阶段把它删掉。普通 V2 调用仍使用 `sanitize_all` 的旧语义。
fn sanitize_all_preserving_preferred(
    intent: &mut SearchIntentV2,
    facets: &[FacetPromptContext],
) -> Vec<String> {
    sanitize_all_with_options(intent, facets, true)
}

fn sanitize_all_with_options(
    intent: &mut SearchIntentV2,
    facets: &[FacetPromptContext],
    preserve_empty_groups: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    // sortBy 非法 → 默认（由命令层 sort_by unwrap_or created_at 兜底）
    if let Some(sb) = &intent.sort_by {
        if !is_valid_sort_by(sb) {
            warnings.push(format!("不支持的排序字段「{sb}」，已用默认排序。"));
            intent.sort_by = None;
        }
    }
    // sortDir 非法 → 默认 desc
    if let Some(sd) = &intent.sort_dir {
        if !matches!(sd.as_str(), "asc" | "desc") {
            warnings.push(format!("不支持的排序方向「{sd}」，已用默认排序。"));
            intent.sort_dir = None;
        }
    }
    // 每组的 assetType 非法 → all
    for g in &mut intent.groups {
        if !matches!(g.asset_type.as_str(), "all" | "image" | "video") {
            warnings.push(format!("不认识的类型「{}」，已改为全部。", g.asset_type));
            g.asset_type = "all".into();
        }
        // concept.facetHint 未知 → 清掉（降级全分面搜索），保留 concept 本体
        let known_keys: Vec<&str> = facets.iter().map(|f| f.key.as_str()).collect();
        for c in &mut g.concepts {
            if let Some(h) = &c.facet_hint {
                if !known_keys.contains(&h.as_str()) {
                    warnings.push(format!(
                        "「{}」的分类提示不在当前分类列表中，已按全部分类搜索。",
                        c.text
                    ));
                    c.facet_hint = None;
                }
            }
        }
    }
    // 单条 metadata 非法 → 剔除该条（已有逻辑）
    warnings.extend(sanitize_metadata(intent));
    // 空概念 text / 空 textTerm 剔除；清空后的 group（concepts+textTerms+metadata 全空）剔除。
    // 注意：纯素材类型组（assetType=image/video，如「视频素材」「照片」）必须保留 ——
    // 漏算 asset_type 会把最基础的类型查询误判为空组删掉，导致 expr=null。
    let before = intent.groups.len();
    intent.groups.retain(|g| {
        let has_concept = g.concepts.iter().any(|c| !c.text.trim().is_empty());
        let has_term = g.text_terms.iter().any(|t| !t.text.trim().is_empty());
        let has_meta = !g.metadata.is_empty();
        let has_type = g.asset_type != "all";
        let has_untagged = g.untagged_only;
        if !has_concept && !has_term && !has_meta && !has_type && !has_untagged {
            if preserve_empty_groups {
                return true;
            }
            warnings.push("一组条件为空，已忽略。".into());
        }
        has_concept || has_term || has_meta || has_type || has_untagged
    });
    if intent.groups.len() < before {
        warnings.push(format!(
            "已忽略 {} 组无法识别的条件。",
            before - intent.groups.len()
        ));
    }
    warnings
}

/// 排序字段白名单（S0：单一事实源 = db/search_query.rs ALL_SORT_KEYS，与 assets VALID_SORT 一致）
pub fn is_valid_sort_by(s: &str) -> bool {
    crate::db::search_query::ALL_SORT_KEYS.contains(&s)
}

/// §9.2 Prompt 硬规则（停用词由 SEARCH_CONCEPT_STOPWORDS 生成，与本地清洗同一集合）。
pub fn build_system_prompt(facets: &[FacetPromptContext]) -> String {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let y = today.year();
    let today_iso = today.format("%Y-%m-%d").to_string();
    let this_year_start = format!("{y}-01-01");
    let this_year_end = format!("{y}-12-31");
    let last_year = y - 1;
    let stop = SEARCH_CONCEPT_STOPWORDS.join("、");
    let mut p = String::new();
    p.push_str(
        "你是「茶包素材库」的搜索条件解析器，不是聊天助手。输入一句自然语言，输出严格 JSON。\n",
    );
    p.push_str("结构：组内 AND、组间 OR。根对象字段 groups/exclusions/sortBy/sortDir。\n");
    p.push_str("每个 group 必填 assetType(all|image|video)/concepts/textTerms/metadata。\n");
    p.push_str(&format!("硬规则：\n1. 每个 concept 是原子化规范名词或短名词短语（中文 1-6 字），禁止「晚上拍的树」「画面中有很多人」这类句子片段。\n2. 连接/方位/语法词不作 concept。共享停用词：{stop}。\n"));
    p.push_str("3. 同义概念只输出一次：如「多人、人群」按词典二选一，不同时输出。\n");
    p.push_str("3.1 人物统一走 people：男子/女子/老人/年轻人/男孩/女孩等人物词必须映射到 people 的原子属性；多人/单人/双人/人群/无人也属于 people。禁止把人物、人数、性别、年龄或穿着写成 subject。\n");
    p.push_str("3.2 subject 中的人只写「人」；具体性别、年龄、穿着、人数和动作必须拆成 people 的多个 concept。拆词只改变标签名称，不改变原句的必须/优先性质：例如「年轻女性」→ people:[青年,女性]；「最好要年轻女性」→ concepts:[]、preferred:[青年,女性]；「要女性，最好年轻」→ concepts:[女性]、preferred:[青年]；「必须是年轻女性」→ concepts:[青年,女性]、preferred:[]。\n");
    p.push_str("3.3 有树、看到建筑等视觉对象走 subject；在树林里、在城市街道等空间地点走 scene。不得把同一个物体词同时写入 subject 和 scene。\n");
    p.push_str("4. assetType 只有用户明确说 图片/照片/相片/图像（image）或 视频/录像/片段/短片（video）时才填；「拍的」不算。\n");
    p.push_str("5. 「晚上拍的」解析为「夜间」或「夜景」概念，不保留整句。\n");
    p.push_str("6. 同一个词只允许出现一次：凡是能映射为标签（concepts）或元数据（metadata）的词，绝不再写进 textTerms；禁止对同一概念既出标签又出全文（如「草地」已进 concepts，就不得再出 textTerms「草地」）。\n");
    p.push_str("7. 只有明确文件名片段、引号原文、专有名词或确实无法映射成任何标签/元数据的具体内容才进 textTerms；scope 取 content（搜描述或文件名），不得把整句放进 all 或 description。连接词、语气词、「拍了/画面/素材」这类泛词一律不进 textTerms。\n");
    p.push_str("8. 没有明确「或/或者/任一」时只输出一个 group；「或/或者/任一」连接的每个完整子句各输出一个 group，组内概念保持 AND。\n");
    p.push_str("9. 「不要/排除/除了」对应的原子概念放入全局 exclusions，不混入正向 group。\n");
    p.push_str("10. 不输出 tagId、SQL、分页、空字符串条件或 schema 之外字段。\n");
    p.push_str("11. confidence 0-1：能从词典精确命中给 0.9+；只能猜测给 0.6 左右；完全不确认给 0.5 以下。\n");
    // S3：必须 vs 加分的判断（§S3）—— 加分项必须带 evidence（原文摘句），
    // 证据不足时忽略加分项，绝不把软条件升级为硬条件。
    p.push_str("必须 vs 加分的判断：\n");
    p.push_str("- 「一定要有 / 只要 / 必须」→ concepts（必须）；「不要 / 排除 / 除了」→ 全局 exclusions（排除）。\n");
    p.push_str("- 「最好有 / 优先 / 尽量 / 更好 / 可有可无 / 倾向 / 接近 / 偏」→ 该 group 的 preferred（加分，不淘汰结果）。\n");
    p.push_str("- 互斥铁律：一个概念进了 preferred，就绝不能再出现在同一 group 的 concepts 里。preferred 是「可有可无、只影响排序」，concepts 是「必须有、不满足就不出现」；把同一个词两边都放等于把软偏好变成硬门槛，是错误。\n");
    p.push_str("- 同组铁律：一句话里的必须项和它的优先项必须放在**同一个 group**（必须项进 concepts、优先项进该组 preferred），绝不能为「优先/尽量」的词单独再建一个 group——没有「或」的句子永远只输出一个 group。例「草地，优先近景，尽量自然光」→ 只有一个 group：concepts=[草地]、preferred=[近景,自然光]。\n");
    p.push_str("- 组合偏好要按短语范围判断：偏好词后面的多个原子概念都属于 preferred，除非同一概念还在偏好短语外被明确要求必须。例如「最好要年轻女性」的「青年、女性」都只能加分；不要因为「青年」是规范名或因为拆成两个 concept，就把它放回 concepts。\n");
    p.push_str("- 加分项必须填 evidence：从原句中**逐字摘出**让你判断为「加分」的片段（如 evidence: \"有蓝天更好\"）。摘不出原文片段就忽略该加分项，绝不改成必须。\n");
    p.push_str("- concept 必须输出词典规范名；evidence 始终保留用户原词，即使它是规范名的可搜索别名（例如「年轻」对应规范名「青年」），不要把 evidence 改写成规范名。\n");
    p.push_str("- 权重只给三档：0.5（略微偏好）/ 1.0（一般偏好）/ 2.0（强偏好）。\n");
    p.push_str("- 不确定时只能保留为加分项或忽略，绝不能把可选条件升级为必须条件。\n");
    p.push_str("词匹配方式（termMatch，默认 alias = 精确匹配规范名或别名）：\n");
    p.push_str("- 用户给出完整词（「海边」「人像」）→ 不填，用默认 alias。\n");
    p.push_str("- 用户说「带…的」「关于…的」「跟…有关」→ contains。\n");
    p.push_str("- 用户明显打错字或用了近义词 → 仍用 alias。系统会在零结果时建议相近词。\n");
    p.push_str("- 不要主动用 prefix 或 fuzzy —— 那是零结果时的兜底，不是首选。\n");
    p.push_str("颜色是算法计算的文件属性，用 metadata（dominant_hue 0-359：红色 min=345 max=15 表示跨 0°；橙 15-45、黄 45-70、绿 70-155、青 155-225、蓝 225-295、紫 295-345；dominant_sat/dominant_lum 0-100；灰/黑/白用 dominant_sat lte 10），不写进 tags。\n");
    p.push_str(
        "元数据条件（metadata）能力清单——key 与 op 只能从下面选，单位与格式必须严格遵守：\n",
    );
    p.push_str("- file_size：文件大小，单位字节（1MB=1048576）。op 用 gt/gte/lt/lte/between。示例「10~105MB」→ {\"key\":\"file_size\",\"op\":\"between\",\"min\":10485760,\"max\":110100480}。\n");
    p.push_str("- file_size 注意：写区间必须换算成字节，禁止输出「5..10」这类 MB 原值（会查不到结果）。换算演示：5~10MB → min=5242880, max=10485760；50~100MB → min=52428800, max=104857600。\n");
    p.push_str(&format!("- taken_at：拍摄日期，值必须是严格 YYYY-MM-DD 的**真实日期**字符串，本地时区，区间左闭右开；op 只用 gte/lte/between。今天是 {today_iso}、今年是 {y} 年。禁止把「今年」「当年」「上月」这类中文/相对词原样写进 min/max，必须换算成真实日期：「今年」→ between min=\"{this_year_start}\" max=\"{this_year_end}\"；「去年」→ between min=\"{last_year}-01-01\" max=\"{last_year}-12-31\"；「今年8月」→ between min=\"{y}-08-01\" max=\"{y}-08-31\"；「最近一周/最近N天」以 {today_iso} 为基准往前回推。\n"));
    p.push_str("- duration_ms：视频时长，单位毫秒（1秒=1000）。op 用 gt/gte/lt/lte/between；仅对视频有意义。\n");
    p.push_str("- width/height：像素整数，op 用 gt/gte/lt/lte/between。分辨率档位按像素换算：「4K/UHD/超高清」→ {\"key\":\"width\",\"op\":\"gte\",\"value\":3840}；「2K」→ width gte 2048；「1080P/全高清」→ {\"key\":\"height\",\"op\":\"gte\",\"value\":1080}；「720P」→ height gte 720。resolution=宽×高总像素。\n");
    p.push_str("- aspect_ratio=宽÷高，用于横竖构图：「横图/横构图/横屏」→ {\"key\":\"aspect_ratio\",\"op\":\"gte\",\"value\":1}（宽≥高）；「竖图/竖构图/竖屏」→ aspect_ratio lte 1；「方图/正方形」→ between 0.95 1.05。禁止把「4K」「横构图」这类词只写文字而不给数值，更不许丢进 textTerms。\n");
    p.push_str("- dominant_sat/dominant_lum：0-100（饱和/明度），与颜色教学配合使用。\n");
    p.push_str("- latitude/longitude：拍摄定位，有符号十进制度（北纬/东经为正，南纬/西经为负；纬度 -90~90、经度 -180~180）。op 用 eq/gt/gte/lt/lte/between。示例「杭州附近」→ {\"key\":\"latitude\",\"op\":\"between\",\"min\":29.8,\"max\":30.6} 与 {\"key\":\"longitude\",\"op\":\"between\",\"min\":119.6,\"max\":120.7}。注意：城市名只有能映射到标签时才进 concepts，经纬度区间才进 metadata。\n");
    p.push_str("- has_location：只用于判断「有没有拍摄定位/GPS」，值是字符串 \"yes\"/\"no\"，op 只用 eq。「有定位/带GPS/有地理位置/开了定位」→ {\"key\":\"has_location\",\"op\":\"eq\",\"value\":\"yes\"}；「没定位/无GPS」→ value \"no\"。只有问「有无」定位才用 has_location；找「某地附近」才用上面的经纬度区间，不要用经纬度 gte 去表达「有没有定位」。\n");
    p.push_str("组级开关 untaggedOnly：用户说「没打标签/未打标/无标签/还没分类/没有任何标签」时，把该 group 的 untaggedOnly 设为 true（筛选没有任何标签的素材）。严禁编造 tags_count、tag_count 等不存在的 metadata key。它可与 assetType/metadata 共存，如「没打标签的视频」= assetType:\"video\" + untaggedOnly:true。\n");
    p.push_str("- 其他可用 key：iso、aperture、focal、camera、lens、shutter、file_ext、mime_type、video_codec、audio_codec、created_at、modified_at、folder、rating（按字段含义使用，不确定就不输出）。\n");
    p.push_str("- 不确定的数值/日期/坐标不要猜：宁可不出 metadata 条件，也不要编造。\n");
    p.push_str(&format!("相对时间一律以今天 {today_iso} 为基准换算成真实日期，禁止输出中文相对词。只生成查询，不创建标签。\n"));
    p.push_str("元数据输出示例 A：输入「大于100MB的视频」→ metadata:[{\"key\":\"file_size\",\"op\":\"gt\",\"value\":104857600,\"values\":null,\"min\":null,\"max\":null}]，assetType 为 video。\n");
    p.push_str(&format!("元数据输出示例 B：输入「今年拍的照片」（今年={y}）→ assetType image，metadata:[{{\"key\":\"taken_at\",\"op\":\"between\",\"value\":null,\"values\":null,\"min\":\"{this_year_start}\",\"max\":\"{this_year_end}\"}}]。\n"));
    p.push_str("元数据输出示例 C：输入「4K横图」→ assetType image，metadata:[{\"key\":\"width\",\"op\":\"gte\",\"value\":3840},{\"key\":\"aspect_ratio\",\"op\":\"gte\",\"value\":1}]。\n");
    p.push_str("元数据输出示例 D：输入「没打标签的视频」→ assetType video、untaggedOnly:true、concepts/textTerms/metadata 均为空。\n");
    // §9.2 回归基准：本轮截图用例
    p.push_str("示例 1：输入「晚上拍的然后有树还有多人」→ 期望：\n");
    p.push_str("{\"groups\":[{\"assetType\":\"all\",\"concepts\":[{\"text\":\"夜间\",\"role\":\"lighting\",\"facetHint\":\"lighting\",\"confidence\":0.95},{\"text\":\"树木\",\"role\":\"subject\",\"facetHint\":\"subject\",\"confidence\":0.95},{\"text\":\"多人\",\"role\":\"people\",\"facetHint\":\"people\",\"confidence\":0.9}],\"textTerms\":[],\"metadata\":[],\"untaggedOnly\":false,\"preferred\":[]}],\"exclusions\":[],\"sortBy\":null,\"sortDir\":null}\n");
    p.push_str(
        "示例 2：输入「晚上拍的树或者白天拍的建筑」→ 两个 group：(夜间∧树) OR (白天∧建筑)。\n",
    );
    p.push_str(
        "示例 3：输入「不要夜景的人像」→ 一个含 subject「人」的 group + exclusions 含 lighting「夜景」。\n",
    );
    p.push_str("示例 4：输入「IMG_1097」→ groups 里 textTerms=[{\"text\":\"IMG_1097\",\"scope\":\"fileName\"}]。\n");
    // W6-4（§W6-4）：显式约束句 —— 分面 key 只能从这里选，不要发明新 key。
    // 分面说明段（user prompt）里同样列出 key；schema 的 facetHint enum 同步收窄。
    let keys = facets.iter().map(|f| f.key.as_str()).collect::<Vec<_>>();
    if keys.is_empty() {
        p.push_str("当前库没有可用分类（facetHint 一律给 null，不要发明分类）。\n");
    } else {
        p.push_str(&format!(
            "分类 key（facetHint 只能填下面这些，不要发明新 key）：{}\n",
            keys.join("、")
        ));
    }
    p
}

/// §9.2.2 V2 严格 JSON Schema：根与嵌套全部 additionalProperties:false；旧字段必须不存在。
/// W6-1（§W6-1）：facetHint 的 enum = 实时分面 key（新建分面后自动包含），
/// 支持 json_schema 的服务商在服务端就拒绝非法 key，根本到不了本地校验层。
fn intent_schema(facets: &[FacetPromptContext]) -> serde_json::Value {
    let nullable_string = serde_json::json!({
        "anyOf": [{"type": "string"}, {"type": "null"}]
    });
    let nullable_number_or_string = serde_json::json!({
        "anyOf": [{"type": "string"}, {"type": "number"}, {"type": "null"}]
    });
    // W6-1：facetHint enum = 实时分面 keys + null（无分面时空 enum，模型只能给 null）
    let facet_key_enum: Vec<serde_json::Value> = facets
        .iter()
        .map(|f| serde_json::Value::String(f.key.clone()))
        .collect();
    let facet_hint = serde_json::json!({
        "anyOf": [{"type": "string", "enum": facet_key_enum}, {"type": "null"}]
    });
    let concept = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "text": {"type": "string", "minLength": 1, "maxLength": 12},
            "role": {"type": "string", "maxLength": 40},
            "facetHint": facet_hint,
            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0}
        },
        "required": ["text", "role", "facetHint", "confidence"]
    });
    // S3：加分项概念 —— concepts 同构 + evidence/weight/termMatch（模型可省略 → null）。
    // 服务端 strict 模式下 required 字段齐全（V2 模型不输出 preferred 数组 → 掉到 JsonObject 层，
    // 本地解析仍兼容：preferred 缺省为空 = V2 语义）。见 S3「V3→V2→关键词」三层降级。
    let preferred_concept = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "text": {"type": "string", "minLength": 1, "maxLength": 12},
            "role": {"type": "string", "maxLength": 40},
            "facetHint": facet_hint,
            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
            "evidence": {"anyOf": [{"type": "string", "minLength": 1, "maxLength": 40}, {"type": "null"}]},
            "weight": {"anyOf": [{"type": "number", "enum": [0.5, 1.0, 2.0]}, {"type": "null"}]},
            "termMatch": {"anyOf": [
                {"type": "string", "enum": ["exact", "alias", "prefix", "contains", "fuzzy"]},
                {"type": "null"}
            ]}
        },
        "required": ["text", "role", "facetHint", "confidence", "evidence", "weight", "termMatch"]
    });
    let text_term = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "text": {"type": "string", "minLength": 1, "maxLength": 200},
            "scope": {"type": "string", "enum": ["all", "content", "description", "fileName"]}
        },
        "required": ["text", "scope"]
    });
    let metadata_item = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "key": {"type": "string", "enum": METADATA_KEYS},
            "op": {"type": "string", "enum": METADATA_OPS},
            "value": nullable_number_or_string,
            "values": {"anyOf": [
                {"type": "array", "items": {"anyOf": [{"type": "string"}, {"type": "number"}]}},
                {"type": "null"}
            ]},
            "min": nullable_number_or_string,
            "max": nullable_number_or_string
        },
        "required": ["key", "op", "value", "values", "min", "max"]
    });
    // C-3 单位约束强化：仅当 key=file_size 时数值下限拦截「5..10」这类 MB 原值。
    // 阈值以 NumericDomain.suspicious_below 为单一事实源（§5.3 ④），此处不再写死 1024。
    // 用条件子 schema（if/then），不影响 latitude 等负值/小数 key；支持 json_schema 的服务商在服务端即拒绝。
    let file_size_floor = crate::db::search_query::numeric_domain("file_size")
        .and_then(|d| d.suspicious_below)
        .unwrap_or(1024.0);
    let file_size_numeric = serde_json::json!({
        "anyOf": [
            {"type": "number", "minimum": file_size_floor},
            {"type": "string"},
            {"type": "null"}
        ]
    });
    let metadata_item = {
        let mut base = metadata_item;
        base["allOf"] = serde_json::json!([{
            "if": {"properties": {"key": {"const": "file_size"}}, "required": ["key"]},
            "then": {
                "properties": {
                    "value": file_size_numeric,
                    "min": file_size_numeric,
                    "max": file_size_numeric
                }
            }
        }]);
        base
    };
    let group = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "assetType": {"type": "string", "enum": ["all", "image", "video"]},
            "concepts": {"type": "array", "maxItems": 20, "items": concept},
            "textTerms": {"type": "array", "maxItems": 10, "items": text_term},
            "metadata": {"type": "array", "maxItems": 20, "items": metadata_item},
            "untaggedOnly": {"type": "boolean"},
            "preferred": {"type": "array", "maxItems": 12, "items": preferred_concept}
        },
        "required": ["assetType", "concepts", "textTerms", "metadata", "untaggedOnly", "preferred"]
    });
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "groups": {"type": "array", "minItems": 0, "maxItems": 6, "items": group},
            "exclusions": {"type": "array", "maxItems": 20, "items": concept},
            "sortBy": nullable_string,
            "sortDir": {"anyOf": [{"type": "string", "enum": ["asc", "desc"]}, {"type": "null"}]}
        },
        "required": ["groups", "exclusions", "sortBy", "sortDir"]
    })
}

pub fn build_explanation(intent: &SearchIntentV2) -> String {
    let mut parts = Vec::new();
    for (i, g) in intent.groups.iter().enumerate() {
        let mut inner = Vec::new();
        match g.asset_type.as_str() {
            "image" => inner.push("图片".into()),
            "video" => inner.push("视频".into()),
            _ => {}
        }
        for c in &g.concepts {
            inner.push(format!("「{}」", c.text));
        }
        for tt in &g.text_terms {
            inner.push(format!("内容「{}」", tt.text));
        }
        for m in &g.metadata {
            inner.push(format!("{} {}", m.key, m.op));
        }
        if !inner.is_empty() {
            parts.push(if intent.groups.len() > 1 {
                format!("任一组 {}：{}", i + 1, inner.join(" 且 "))
            } else {
                inner.join(" 且 ")
            });
        }
    }
    for c in &intent.exclusions {
        parts.push(format!("排除「{}」", c.text));
    }
    if parts.is_empty() {
        "未解析出明确条件".into()
    } else {
        format!("筛选{}", parts.join("；"))
    }
}

/// S3：V3 解释文案 —— V2 语义 + 加分项以「可加分」呈现（与 UI 三段式措辞一致）。
pub fn build_explanation_v3(intent: &SearchIntentV3) -> String {
    let mut parts = Vec::new();
    for (i, g) in intent.groups.iter().enumerate() {
        let mut inner = Vec::new();
        match g.asset_type.as_str() {
            "image" => inner.push("图片".into()),
            "video" => inner.push("视频".into()),
            _ => {}
        }
        for c in &g.concepts {
            inner.push(format!("「{}」", c.text));
        }
        for tt in &g.text_terms {
            inner.push(format!("内容「{}」", tt.text));
        }
        for m in &g.metadata {
            inner.push(format!("{} {}", m.key, m.op));
        }
        for c in &g.preferred {
            inner.push(format!("「{}」(可加分)", c.text));
        }
        if !inner.is_empty() {
            parts.push(if intent.groups.len() > 1 {
                format!("任一组 {}：{}", i + 1, inner.join(" 且 "))
            } else {
                inner.join(" 且 ")
            });
        }
    }
    for c in &intent.exclusions {
        parts.push(format!("排除「{}」", c.text));
    }
    if parts.is_empty() {
        "未解析出明确条件".into()
    } else {
        format!("筛选{}", parts.join("；"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn mk_concept(text: &str, hint: Option<&str>, conf: f32) -> SearchConceptV2 {
        SearchConceptV2 {
            text: text.into(),
            role: String::new(),
            facet_hint: hint.map(String::from),
            confidence: Some(conf),
        }
    }

    fn mk_group(concepts: Vec<SearchConceptV2>) -> SearchGroupV2 {
        SearchGroupV2 {
            asset_type: "all".into(),
            concepts,
            text_terms: vec![],
            metadata: vec![],
            untagged_only: false,
        }
    }

    // ── 解析 ──

    #[test]
    fn parses_clean_v2_json() {
        let intent = parse_intent(
            r#"{"groups":[{"assetType":"all","concepts":[{"text":"树","role":"subject","facetHint":"subject","confidence":0.95}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":"resolution","sortDir":"desc"}"#,
        )
        .unwrap();
        assert_eq!(intent.groups.len(), 1);
        assert_eq!(intent.groups[0].concepts[0].text, "树");
        assert_eq!(intent.sort_by.as_deref(), Some("resolution"));
    }

    #[test]
    fn parses_noisy_reply_by_slicing() {
        let intent = parse_intent(
            "好的：\n{\"groups\":[{\"assetType\":\"video\",\"concepts\":[],\"textTerms\":[],\"metadata\":[]}],\"exclusions\":[],\"sortBy\":null,\"sortDir\":null}",
        )
        .unwrap();
        assert_eq!(intent.groups[0].asset_type, "video");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_intent("我无法理解").is_err());
    }

    // ── 校验 ──

    #[test]
    fn validates_rejects_bad_asset_type() {
        let mut i = SearchIntentV2::default();
        i.groups.push(SearchGroupV2 {
            asset_type: "banana".into(),
            ..Default::default()
        });
        assert!(validate_intent(&i, &[]).is_err());
    }

    #[test]
    fn validates_rejects_bad_sort() {
        let i = SearchIntentV2 {
            sort_by: Some("magic".into()),
            ..SearchIntentV2::default()
        };
        assert!(validate_intent(&i, &[]).is_err());
    }

    /// R0-4：rating 是合法排序字段（assets.rs VALID_SORT 与 is_valid_sort_by 均含），
    /// validate_intent 不得把它降级 —— 否则 rating 排序整个 intent 变关键词搜索。
    #[test]
    fn intent_with_rating_sort_is_not_degraded() {
        let i = SearchIntentV2 {
            sort_by: Some("rating".into()),
            sort_dir: Some("desc".into()),
            ..SearchIntentV2::default()
        };
        assert!(
            validate_intent(&i, &[]).is_ok(),
            "rating 排序必须通过校验（R0-4）"
        );
    }

    #[test]
    fn validates_rejects_bad_text_scope() {
        let mut i = SearchIntentV2::default();
        i.groups.push(SearchGroupV2 {
            asset_type: "all".into(),
            text_terms: vec![IntentTextTerm {
                text: "x".into(),
                scope: "everything".into(),
            }],
            ..Default::default()
        });
        assert!(validate_intent(&i, &[]).is_err());
    }

    #[test]
    fn schema_has_no_legacy_fields() {
        // §9.2.2：schema 必须断言旧字段不存在
        let s = intent_schema(&[]);
        let props = s["properties"].as_object().unwrap();
        for legacy in [
            "search",
            "tags",
            "excludeTags",
            "unresolved",
            "relation",
            "concepts",
            "metadata",
        ] {
            assert!(
                !props.contains_key(legacy),
                "旧字段 {legacy} 必须不存在于 V2 schema"
            );
        }
        assert!(props.contains_key("groups"));
        assert!(props.contains_key("exclusions"));
        // 根与嵌套 additionalProperties:false
        assert_eq!(s["additionalProperties"], false);
        let g = &s["properties"]["groups"]["items"];
        assert_eq!(g["additionalProperties"], false);
        let c = &g["properties"]["concepts"]["items"];
        assert_eq!(c["additionalProperties"], false);
        assert_eq!(c["properties"]["confidence"]["maximum"], 1.0);
    }

    #[test]
    fn schema_requires_v2_fields() {
        let s = intent_schema(&[]);
        let req: Vec<String> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(req.contains(&"groups".into()));
        assert!(req.contains(&"exclusions".into()));
        assert!(req.contains(&"sortBy".into()));
        assert!(req.contains(&"sortDir".into()));
    }

    // ── §9.3 守卫 ──

    #[test]
    fn guard_merges_groups_without_or_word() {
        // 「晚上拍的然后有树还有多人」→ 模型多组 → 合并为单 AND 组 + warning
        let mut intent = SearchIntentV2 {
            groups: vec![
                mk_group(vec![mk_concept("夜间", None, 0.95)]),
                mk_group(vec![mk_concept("树", None, 0.95)]),
            ],
            ..Default::default()
        };
        let warnings = guard_intent("晚上拍的然后有树还有多人", &mut intent);
        assert_eq!(intent.groups.len(), 1);
        assert_eq!(intent.groups[0].concepts.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("合并")));
    }

    #[test]
    fn guard_keeps_groups_with_or_word() {
        // 「晚上拍的树或者白天拍的建筑」→ 保留 2 组（组内 AND、组间 OR）
        let mut intent = SearchIntentV2 {
            groups: vec![
                mk_group(vec![
                    mk_concept("夜间", None, 0.95),
                    mk_concept("树", None, 0.95),
                ]),
                mk_group(vec![
                    mk_concept("白天", None, 0.95),
                    mk_concept("建筑", None, 0.95),
                ]),
            ],
            ..Default::default()
        };
        let warnings = guard_intent("晚上拍的树或者白天拍的建筑", &mut intent);
        assert_eq!(intent.groups.len(), 2);
        assert!(!warnings.iter().any(|w| w.contains("合并")));
    }

    #[test]
    fn guard_ungrounded_asset_type_reverts_to_all() {
        // 原文没提「图片/照片」→ 模型给 image → 改回 all + warning
        let mut intent = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "image".into(),
                concepts: vec![mk_concept("夜景", None, 0.9)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let warnings = guard_intent("晚上拍的夜景", &mut intent);
        assert_eq!(intent.groups[0].asset_type, "all");
        assert!(warnings.iter().any(|w| w.contains("图片/照片")));
        // 原文明确「照片」→ 保留 image
        let mut intent2 = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "image".into(),
                concepts: vec![mk_concept("夜景", None, 0.9)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let w2 = guard_intent("这张照片是夜景", &mut intent2);
        assert_eq!(intent2.groups[0].asset_type, "image");
        assert!(w2.is_empty());
    }

    #[test]
    fn guard_cleans_stopwords_and_sentences() {
        let mut intent = SearchIntentV2 {
            groups: vec![mk_group(vec![
                mk_concept("然后", None, 0.9),                       // 停用词
                mk_concept("画面中", None, 0.9),                     // 停用词
                mk_concept("晚上拍的树还有很多人在一起", None, 0.9), // 句子化（>12 字）
                mk_concept(" 银杏树 ", None, 0.9),                   // trim
                mk_concept("银杏树", None, 0.8),                     // 去重
            ])],
            ..Default::default()
        };
        let warnings = guard_intent("银杏树下的老人", &mut intent);
        assert_eq!(intent.groups[0].concepts.len(), 1);
        assert_eq!(intent.groups[0].concepts[0].text, "银杏树");
        assert!(warnings.iter().any(|w| w.contains("句子而非原子概念")));
    }

    #[test]
    fn guard_clamps_confidence() {
        let mut intent = SearchIntentV2 {
            exclusions: vec![mk_concept("夜景", None, 7.0)],
            ..Default::default()
        };
        let _ = guard_intent("不要夜景", &mut intent);
        assert_eq!(intent.exclusions[0].confidence, Some(1.0));
    }

    // ── §9.4/§9.5 expr 生成（真实 DB） ──

    fn setup_facets_and_tags(conn: &rusqlite::Connection) {
        let now = 1_700_000_000_000i64;
        for (k, n) in [
            ("subject", "主体"),
            ("scene", "场景"),
            ("lighting", "光线"),
            ("people", "人物"),
        ] {
            conn.execute(
                "INSERT OR IGNORE INTO tag_facets
                 (key, display_name, description, selection_mode, max_items, sort_order, is_system, status, applies_to, created_at, updated_at)
                 VALUES (?1, ?2, '', 'multi', 5, 0, 1, 'active', 'all', ?3, ?3)",
                rusqlite::params![k, n, now],
            )
            .unwrap();
        }
        for (facet, name) in [
            ("subject", "树"),
            ("subject", "花"),
            ("subject", "老人"),
            ("people", "多人"),
            ("people", "人群"),
            ("lighting", "夜景"),
            ("lighting", "白天"),
            ("scene", "建筑"),
        ] {
            tags::create_in_facet(conn, name, None, Some(facet)).unwrap();
        }
        // 人群 → 多人 的可搜索别名（同 tagId 验证用）
        let duo = tags::search_candidates(conn, Some("people"), "多人").unwrap();
        let duo_id = duo.iter().find(|t| t.name == "多人").map(|t| t.id).unwrap();
        conn.execute(
            "INSERT INTO tag_aliases (tag_id, alias, normalized_alias, is_searchable, created_at)
             VALUES (?1, '人群', '人群', 1, ?2)",
            rusqlite::params![duo_id, now],
        )
        .unwrap();
    }

    #[test]
    fn expr_group_and_or_not_shape() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 「晚上拍的树或者白天拍的建筑」→ (夜间∧树) OR (白天∧建筑)
        let intent = SearchIntentV2 {
            groups: vec![
                SearchGroupV2 {
                    asset_type: "all".into(),
                    concepts: vec![
                        mk_concept("夜景", Some("lighting"), 0.95),
                        mk_concept("树", Some("subject"), 0.95),
                    ],
                    ..Default::default()
                },
                SearchGroupV2 {
                    asset_type: "all".into(),
                    concepts: vec![
                        mk_concept("白天", Some("lighting"), 0.95),
                        mk_concept("建筑", Some("scene"), 0.95),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (expr, resolved, warnings) = build_expr_from_v2(&conn, &intent).unwrap();
        assert!(warnings.is_empty());
        let expr = expr.unwrap();
        match &expr {
            QueryExpr::Or { children } => {
                assert_eq!(children.len(), 2);
                match &children[0] {
                    QueryExpr::And { children } => assert_eq!(children.len(), 2),
                    _ => panic!("组内应为 AND"),
                }
            }
            _ => panic!("组间应为 OR"),
        }
        assert_eq!(resolved.len(), 4);
        crate::db::query_expr::validate_expr(&expr).unwrap();
    }

    #[test]
    fn expr_same_facet_two_concepts_are_two_leaves_not_any() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 「有树还有花」→ 两个独立 tag leaf（同一分面 subject），禁止 mode:any 合并
        let intent = SearchIntentV2 {
            groups: vec![mk_group(vec![
                mk_concept("树", Some("subject"), 0.95),
                mk_concept("花", Some("subject"), 0.95),
            ])],
            ..Default::default()
        };
        let (expr, resolved, _) = build_expr_from_v2(&conn, &intent).unwrap();
        assert_eq!(resolved.len(), 2);
        let expr = expr.unwrap();
        match &expr {
            QueryExpr::And { children } => {
                let tag_leaves: Vec<_> = children
                    .iter()
                    .filter_map(|c| match c {
                        QueryExpr::Leaf {
                            cond: LeafCond::Tag { tag_ids, .. },
                        } => Some(tag_ids.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(tag_leaves.len(), 2, "树+花 必须是两个独立 tag leaf");
                assert!(tag_leaves.iter().all(|t| t.len() == 1));
            }
            _ => panic!("应组装为 AND"),
        }
    }

    #[test]
    fn expr_unmapped_concept_becomes_content_search() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 「银杏树下的老人」：无「银杏树」标签 → 老人(tag) AND content(银杏树)
        let intent = SearchIntentV2 {
            groups: vec![mk_group(vec![
                mk_concept("银杏树", Some("subject"), 0.7), // 无标签 → content leaf
                mk_concept("老人", Some("subject"), 0.95),  // 有标签
            ])],
            ..Default::default()
        };
        let (expr, resolved, warnings) = build_expr_from_v2(&conn, &intent).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(resolved.len(), 1);
        let expr = expr.unwrap();
        let mut has_content_leaf = false;
        let mut has_tag_leaf = false;
        if let QueryExpr::And { children } = &expr {
            for c in children {
                if let QueryExpr::Leaf { cond } = c {
                    match cond {
                        LeafCond::Search { scope, .. } => {
                            has_content_leaf =
                                *scope == crate::db::query_expr::SearchScope::Content;
                        }
                        LeafCond::Tag { .. } => has_tag_leaf = true,
                        _ => {}
                    }
                }
            }
        }
        assert!(has_content_leaf, "银杏树 应转 content 搜索 leaf");
        assert!(has_tag_leaf, "老人 应为 tag leaf");
    }

    #[test]
    fn expr_low_confidence_concept_is_dropped_with_warning() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        let intent = SearchIntentV2 {
            groups: vec![mk_group(vec![mk_concept("某种不确定的东西", None, 0.3)])],
            ..Default::default()
        };
        let (expr, _, warnings) = build_expr_from_v2(&conn, &intent).unwrap();
        assert!(expr.is_none(), "低置信不产生硬条件");
        assert!(warnings.iter().any(|w| w.contains("未采用概念")));
    }

    #[test]
    fn expr_synonyms_resolve_to_same_tag_id_single_leaf() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 「人群」「多人」→ 同一 tagId（人群是 多人的可搜索别名）→ 只产生一个 leaf
        let intent = SearchIntentV2 {
            groups: vec![mk_group(vec![
                mk_concept("多人", Some("people"), 0.95),
                mk_concept("人群", Some("people"), 0.95),
            ])],
            ..Default::default()
        };
        let (expr, resolved, _) = build_expr_from_v2(&conn, &intent).unwrap();
        // 解析层分别命中同一 tagId：expr 归一化后按 tagId 去重 → 1 个 leaf
        let expr = expr.unwrap();
        let tag_ids: Vec<i64> = match &expr {
            QueryExpr::And { children } => children
                .iter()
                .filter_map(|c| match c {
                    QueryExpr::Leaf {
                        cond: LeafCond::Tag { tag_ids, .. },
                    } => Some(tag_ids.clone()),
                    _ => None,
                })
                .flatten()
                .collect(),
            QueryExpr::Leaf {
                cond: LeafCond::Tag { tag_ids, .. },
            } => tag_ids.clone(),
            _ => vec![],
        };
        assert_eq!(resolved.len(), 2, "两个概念各自解析命中");
        let ids = resolved.iter().map(|r| r.tag_id).collect::<Vec<_>>();
        assert_eq!(ids[0], ids[1], "人群与多人应命中同一 tagId");
        assert_eq!(tag_ids.len(), 1, "同 tagId 只保留一个 leaf");
    }

    #[test]
    fn expr_exclusion_wins_over_positive_same_tag() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 正向「夜景」+ 排除「夜景」→ 以排除为准，正向 leaf 被移除
        let intent = SearchIntentV2 {
            groups: vec![mk_group(vec![mk_concept("夜景", Some("lighting"), 0.95)])],
            exclusions: vec![mk_concept("夜景", Some("lighting"), 0.95)],
            ..Default::default()
        };
        let (expr, _, warnings) = build_expr_from_v2(&conn, &intent).unwrap();
        let expr = expr.unwrap();
        // 根 = AND(NOT(夜景)) 或直接 NOT(夜景) —— 正向被移除
        let mut tag_leaf_count = 0;
        collect_tag_leaves(&expr, &mut tag_leaf_count);
        assert_eq!(tag_leaf_count, 1, "只有排除的 tag leaf，正向被移除");
        assert!(warnings.iter().any(|w| w.contains("包含与排除")));
    }

    fn collect_tag_leaves(e: &QueryExpr, count: &mut usize) {
        match e {
            QueryExpr::Leaf {
                cond: LeafCond::Tag { .. },
            } => *count += 1,
            QueryExpr::And { children } | QueryExpr::Or { children } => {
                for c in children {
                    collect_tag_leaves(c, count);
                }
            }
            QueryExpr::Not { child } => collect_tag_leaves(child, count),
            _ => {}
        }
    }

    #[test]
    fn expr_file_name_text_term() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 「IMG_1097」→ fileName 范围 text term
        let intent = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "all".into(),
                text_terms: vec![IntentTextTerm {
                    text: "IMG_1097".into(),
                    scope: "fileName".into(),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let (expr, _, _) = build_expr_from_v2(&conn, &intent).unwrap();
        let expr = expr.unwrap();
        match &expr {
            QueryExpr::Leaf {
                cond: LeafCond::Search { value, scope },
            } => {
                assert_eq!(value, "IMG_1097");
                assert_eq!(*scope, crate::db::query_expr::SearchScope::FileName);
            }
            _ => panic!("应为 fileName 搜索 leaf"),
        }
    }

    #[test]
    fn expr_exclusion_content_not_shape() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        // 排除未映射概念（conf ≥ 0.55）→ NOT(content)
        let intent = SearchIntentV2 {
            exclusions: vec![mk_concept("银幕", None, 0.7)],
            ..Default::default()
        };
        let (expr, _, _) = build_expr_from_v2(&conn, &intent).unwrap();
        let expr = expr.unwrap();
        match &expr {
            QueryExpr::Not { child } => match child.as_ref() {
                QueryExpr::Leaf {
                    cond: LeafCond::Search { scope, .. },
                } => assert_eq!(*scope, crate::db::query_expr::SearchScope::Content),
                _ => panic!("排除应为 NOT(content)"),
            },
            _ => panic!("应为 NOT"),
        }
    }

    // ── 词典（§9.2.1） ──

    #[test]
    fn dictionary_includes_searchable_aliases_and_sanitizes() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        let facets = vec![FacetPromptContext {
            key: "people".into(),
            display_name: "人物".into(),
            description: String::new(),
            selection_mode: "multi".into(),
            max_items: Some(5),
            ..Default::default()
        }];
        let dict = collect_tag_dictionary(&conn, &facets).unwrap();
        let line = dict
            .iter()
            .find(|l| l.contains("多人"))
            .expect("词典应含 多人");
        assert!(line.contains("人群"), "可搜索别名应进词典：{line}");
        // 非可搜索别名不出现（未造，跳过）；控制字符被清除
        assert!(dict.iter().all(|l| !l.chars().any(|c| c.is_control())));
    }

    #[test]
    fn core_taxonomy_paths_and_person_queries_resolve_to_people() {
        let conn = init_memory().unwrap();
        crate::db::ensure_default_taxonomy(&conn).unwrap();
        let facets = crate::db::tag_facets::build_prompt_context(&conn, "all").unwrap();
        let dict = collect_tag_dictionary(&conn, &facets).unwrap();
        assert!(
            dict.iter()
                .any(|line| line.contains("性别 / 女性") && line.contains("term: 女性")),
            "词典应包含父子路径与叶子名：{dict:?}"
        );

        match resolve_concept(&conn, &mk_concept("女性", Some("people"), 0.95), false).unwrap() {
            ConceptOutcome::Tag(tag) => {
                assert_eq!(tag.facet_key, "people");
                assert_eq!(tag.path, "性别 / 女性");
            }
            _ => panic!("女性应解析为 people 标签"),
        }
        match resolve_concept(&conn, &mk_concept("人像", Some("subject"), 0.95), false).unwrap() {
            ConceptOutcome::Tag(tag) => {
                assert_eq!(tag.facet_key, "subject");
                assert_eq!(tag.text, "人像");
            }
            _ => panic!("人像应通过别名解析为 subject「人」"),
        }
    }

    #[test]
    fn confirmed_new_word_enters_dictionary_without_restart() {
        let conn = init_memory().unwrap();
        crate::db::ensure_default_taxonomy(&conn).unwrap();
        let tag_id = crate::db::tags::find_or_create_canonical(&conn, "subject", "蒲公英")
            .expect("确认新词应写入「其他」");
        let facets = crate::db::tag_facets::build_prompt_context(&conn, "all").unwrap();
        let dict = collect_tag_dictionary(&conn, &facets).unwrap();

        assert!(
            dict.iter()
                .any(|line| line.contains("其他 / 蒲公英") && line.contains("term: 蒲公英")),
            "新词确认后应进入下一次搜索词典：{dict:?}"
        );
        match resolve_concept(&conn, &mk_concept("蒲公英", Some("subject"), 0.95), false).unwrap()
        {
            ConceptOutcome::Tag(tag) => {
                assert_eq!(tag.tag_id, tag_id);
                assert_eq!(tag.facet_key, "subject");
                assert_eq!(tag.path, "其他 / 蒲公英");
            }
            _ => panic!("已确认新词应直接解析为标签"),
        }
    }

    // ── 元数据能力清单 / schema 收窄 / 容错降级（第六轮反馈） ──

    fn mk_meta(
        key: &str,
        op: &str,
        value: Option<serde_json::Value>,
        values: Option<Vec<serde_json::Value>>,
        min: Option<serde_json::Value>,
        max: Option<serde_json::Value>,
    ) -> MetadataFilter {
        MetadataFilter {
            key: key.into(),
            op: op.into(),
            value,
            values,
            min,
            max,
        }
    }

    #[test]
    fn prompt_teaches_metadata_capabilities() {
        let p = build_system_prompt(&[]);
        // key 名与单位/格式教学必须存在，模型不再靠猜
        for needle in [
            "file_size",
            "字节",
            "taken_at",
            "YYYY-MM-DD",
            "duration_ms",
            "毫秒",
            "width/height",
            "resolution",
            "dominant_sat",
            "latitude",
            "longitude",
            "有符号十进制度",
            "aspect_ratio",
            "has_location",
            "untaggedOnly",
        ] {
            assert!(p.contains(needle), "提示词应教学：{needle}");
        }
        // 关键换算示例：10~105MB → 字节区间
        assert!(p.contains("10485760"), "file_size 示例应含 10MB 字节数");
        assert!(p.contains("110100480"), "file_size 示例应含 105MB 字节数");
        // 四个 metadata 输出示例（file_size / taken_at / 4K横图 / 未打标）
        assert!(p.contains("元数据输出示例 A"));
        assert!(p.contains("元数据输出示例 B"));
        assert!(p.contains("元数据输出示例 C"));
        assert!(p.contains("元数据输出示例 D"));
        // 分辨率档位 → 像素；横竖图 → aspect_ratio；优先互斥；禁止编造 tags_count
        assert!(p.contains("3840"), "4K 应教学为 width gte 3840");
        assert!(p.contains("互斥铁律"), "preferred/concepts 互斥必须强调");
        assert!(p.contains("最好要年轻女性"), "必须覆盖组合偏好的回归示例");
        assert!(
            p.contains("要女性，最好年轻"),
            "必须覆盖硬条件+偏好的回归示例"
        );
        assert!(p.contains("tags_count"), "必须明令禁止编造 tags_count");
        // 相对日期：注入真实当前年份，且不再出现「当年」占位字样
        let year = chrono::Local::now().format("%Y").to_string();
        assert!(p.contains(&year), "taken_at 教学应注入当前真实年份 {year}");
        // 不得再把「当年8月1日」这类占位中文当成 min/max 的值（禁令中可以提到「当年」这个词）
        assert!(
            !p.contains("当年8月1日"),
            "taken_at 示例值必须是真实日期，不得是占位中文"
        );
    }

    #[test]
    fn schema_metadata_enums_match_whitelist_contract() {
        let s = intent_schema(&[]);
        let m = &s["properties"]["groups"]["items"]["properties"]["metadata"]["items"];
        let keys: Vec<&str> = m["properties"]["key"]["enum"]
            .as_array()
            .expect("key 必须有 enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let ops: Vec<&str> = m["properties"]["op"]["enum"]
            .as_array()
            .expect("op 必须有 enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(keys, METADATA_KEYS, "enum 必须直接取自契约常量");
        assert_eq!(ops, METADATA_OPS, "enum 必须直接取自契约常量");
        // S0：单一事实源 —— METADATA_KEYS（= schema enum）= db/search_query.rs
        // ALL_METADATA_KEYS（= key_spec 全部分支，含 rating/has_location）。
        assert_eq!(
            METADATA_KEYS,
            crate::db::search_query::ALL_METADATA_KEYS,
            "本文件不得再维护一份会漂移的 key 列表"
        );
        // 白名单 key 必须个个能编译（加了 key 忘了 spec 由 whitelist_single_source 抓）
        for k in keys {
            assert!(
                crate::db::search_query::is_supported_metadata_key(k),
                "schema 枚举了无法编译的 key：{k}"
            );
        }
        // op 全集与白名单各 key allowed_ops 并集一致
        assert_eq!(
            ops,
            vec!["eq", "in", "gt", "gte", "lt", "lte", "between", "contains"]
        );
    }

    #[test]
    fn sanitize_drops_invalid_metadata_and_keeps_valid() {
        let mut intent = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "all".into(),
                metadata: vec![
                    // 未知 key → 丢弃
                    mk_meta(
                        "magic_field",
                        "eq",
                        Some(serde_json::json!(1)),
                        None,
                        None,
                        None,
                    ),
                    // 白名单 key + 不支持的 op（taken_at 不支持 eq）→ 丢弃
                    mk_meta(
                        "taken_at",
                        "eq",
                        Some(serde_json::json!("2026-08-01")),
                        None,
                        None,
                        None,
                    ),
                    // 白名单 key + 非法值（file_size 为负）→ 丢弃
                    mk_meta(
                        "file_size",
                        "gte",
                        Some(serde_json::json!(-5)),
                        None,
                        None,
                        None,
                    ),
                    // 合法条件 → 保留
                    mk_meta(
                        "width",
                        "gte",
                        Some(serde_json::json!(1920)),
                        None,
                        None,
                        None,
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let warnings = sanitize_metadata(&mut intent);
        assert_eq!(warnings.len(), 3, "三条非法条件各产生一条 warning");
        assert!(warnings.iter().any(|w| w.contains("magic_field")));
        assert!(warnings.iter().any(|w| w.contains("taken_at")));
        let kept = &intent.groups[0].metadata;
        assert_eq!(kept.len(), 1, "合法条件保留");
        assert_eq!(kept[0].key, "width");
        // 降级后结构校验不再因 metadata 报错（其余合法条件可继续执行）
        validate_intent(&intent, &[]).unwrap();
    }

    #[test]
    fn sanitize_keeps_file_size_between_and_taken_month_examples() {
        // 提示词教学的两个标杆示例必须能通过白名单编译（不被降级丢弃）
        let mut intent = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "all".into(),
                metadata: vec![
                    // 「10~105MB」→ between 10485760..110100480
                    mk_meta(
                        "file_size",
                        "between",
                        None,
                        None,
                        Some(serde_json::json!(10485760)),
                        Some(serde_json::json!(110100480)),
                    ),
                    // 「8月份」→ between 当月首末日（YYYY-MM-DD，左闭右开）
                    mk_meta(
                        "taken_at",
                        "between",
                        None,
                        None,
                        Some(serde_json::json!("2026-08-01")),
                        Some(serde_json::json!("2026-08-31")),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let warnings = sanitize_metadata(&mut intent);
        assert!(warnings.is_empty(), "教学示例必须全部合法：{warnings:?}");
        assert_eq!(intent.groups[0].metadata.len(), 2);
        // 进一步确认能编译为 SQL 片段（与执行层同一入口）
        for f in &intent.groups[0].metadata {
            assert!(
                crate::db::search_query::compile_metadata(f)
                    .unwrap()
                    .is_some(),
                "{} {} 应可编译",
                f.key,
                f.op
            );
        }
    }
    // ── W6 搜索健壮化（§W6）──

    /// W6-1：facetHint enum 收窄 —— 实时分面 key 必须出现在 schema enum 中
    #[test]
    fn intent_schema_enum_contains_user_facet() {
        let facets = vec![
            FacetPromptContext {
                key: "scene".into(),
                display_name: "场景".into(),
                selection_mode: "single".into(),
                max_items: Some(3),
                description: String::new(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "mood".into(),
                display_name: "氛围".into(),
                selection_mode: "multi".into(),
                max_items: None,
                description: String::new(),
                ..Default::default()
            },
        ];
        let schema = intent_schema(&facets);
        let hint_enum = &schema["properties"]["groups"]["items"]["properties"]["concepts"]["items"]
            ["properties"]["facetHint"]["anyOf"][0]["enum"];
        let keys: Vec<&str> = hint_enum
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(keys.contains(&"scene"));
        assert!(keys.contains(&"mood"));
        assert!(!keys.contains(&"不存在的分面"));
    }

    /// W6-2：8 种畸形 AI 返回 —— 全部降级为关键词搜索或部分理解，绝不 Err
    #[test]
    fn search_never_errors_on_malformed_ai() {
        let facets: Vec<FacetPromptContext> = vec![];
        let text = "海边日落";
        // 1. 空串
        let (i, w) = degrade_parse("", text, &facets);
        assert!(
            is_keyword_fallback(&i, text),
            "空串应关键词兜底；warnings={w:?}"
        );
        // 2. 非 JSON
        let (i, w) = degrade_parse("我觉得你搜不到", text, &facets);
        assert!(
            is_keyword_fallback(&i, text),
            "非 JSON 应关键词兜底；warnings={w:?}"
        );
        // 3. 未知 facet key → 保留 concept，清掉 hint（lenient）
        let (i, w) = degrade_parse(
            r#"{"groups":[{"assetType":"all","concepts":[{"text":"海边","role":"scene","facetHint":"not_a_facet","confidence":0.9}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
            text,
            &facets,
        );
        assert!(!is_keyword_fallback(&i, text), "未知 key 不应兜底");
        assert!(
            i.groups[0].concepts[0].facet_hint.is_none(),
            "未知 hint 应清空"
        );
        assert!(!w.is_empty(), "应有 warning");
        // 4. 非法 op → 剔除该条 metadata，保留其余
        let (i, w) = degrade_parse(
            r#"{"groups":[{"assetType":"all","concepts":[{"text":"海边","role":"scene","facetHint":null,"confidence":0.9}],"textTerms":[],"metadata":[{"key":"file_size","op":"mega","value":100,"values":null,"min":null,"max":null}]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
            text,
            &facets,
        );
        assert!(!is_keyword_fallback(&i, text));
        assert!(i.groups[0].metadata.is_empty(), "非法 metadata 应被剔除");
        assert!(!w.is_empty());
        // 5. 值类型错（字符串当数值）→ 剔除该条 metadata
        let (i, w) = degrade_parse(
            r#"{"groups":[{"assetType":"all","concepts":[{"text":"海边","role":"scene","facetHint":null,"confidence":0.9}],"textTerms":[],"metadata":[{"key":"file_size","op":"gt","value":"not-a-number","values":null,"min":null,"max":null}]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
            text,
            &facets,
        );
        assert!(i.groups[0].metadata.is_empty());
        assert!(!w.is_empty());
        // 6. 空 groups → 关键词兜底
        let (i, w) = degrade_parse(
            r#"{"groups":[],"exclusions":[],"sortBy":null,"sortDir":null}"#,
            text,
            &facets,
        );
        assert!(
            is_keyword_fallback(&i, text),
            "空 groups 应兜底；warnings={w:?}"
        );
        // 空主体 + 非空排除也必须关键词兜底，不能执行成纯 mustNot（近似全库）。
        let (i, w) = degrade_parse(
            r#"{"groups":[],"exclusions":[{"text":"夜景","role":"scene","facetHint":null,"confidence":0.9}],"sortBy":null,"sortDir":null}"#,
            text,
            &facets,
        );
        assert!(
            is_keyword_fallback(&i, text),
            "只有排除项时也应关键词兜底；warnings={w:?}"
        );
        // 7. 超长概念 → 结构校验失败 → 兜底
        let long = "很".repeat(200);
        let (i, w) = degrade_parse(
            &format!(
                r#"{{"groups":[{{"assetType":"all","concepts":[{{"text":"{long}","role":"scene","facetHint":null,"confidence":0.9}}],"textTerms":[],"metadata":[]}}],"exclusions":[],"sortBy":null,"sortDir":null}}"#
            ),
            text,
            &facets,
        );
        assert!(
            is_keyword_fallback(&i, text),
            "超长概念应兜底；warnings={w:?}"
        );
        // 8. 混入 markdown 围栏 → 剥围栏后正常解析（不兜底）
        let (i, w) = degrade_parse(
            "```json\n{\"groups\":[{\"assetType\":\"all\",\"concepts\":[{\"text\":\"海边\",\"role\":\"scene\",\"facetHint\":null,\"confidence\":0.9}],\"textTerms\":[],\"metadata\":[]}],\"exclusions\":[],\"sortBy\":null,\"sortDir\":null}\n```",
            text,
            &facets,
        );
        assert!(
            !is_keyword_fallback(&i, text),
            "围栏剥除后应正常解析；warnings={w:?}"
        );
    }

    /// W6-2 例外：配置类错误（鉴权/连不上/超时）仍应判定为真报错（命令层不降级）
    #[test]
    fn search_config_error_still_errors() {
        for msg in [
            "云端请求失败: 401 Unauthorized",
            "云端请求失败: 403 Forbidden",
            "无法连接本地服务 ...: Connection refused",
            "请求失败: request timed out",
            "云端请求失败: error sending request for url ...",
        ] {
            let e = AppError::msg(msg);
            assert!(is_config_error(&e), "应判定为配置错误: {msg}");
        }
        for msg in [
            "AI 未返回可解析的 JSON",
            "非法排序字段：foo",
            "分组数量超出上限",
        ] {
            let e = AppError::msg(msg);
            assert!(!is_config_error(&e), "不应判定为配置错误: {msg}");
        }
    }

    /// W6-3：部分剔除规则 —— 非法 sortBy/sortDir/assetType → 默认；未知 hint → 全分面
    #[test]
    fn sanitize_all_corrects_known_keys() {
        let facets: Vec<FacetPromptContext> = vec![];
        let mut intent = serde_json::from_str::<SearchIntentV2>(
            r#"{"groups":[{"assetType":"whatever","concepts":[{"text":"海边","role":"scene","facetHint":"bogus","confidence":0.9}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":"rank","sortDir":"sideways"}"#,
        )
        .unwrap();
        let warnings = sanitize_all(&mut intent, &facets);
        assert!(intent.sort_by.is_none(), "非法 sortBy 应置默认");
        assert!(intent.sort_dir.is_none(), "非法 sortDir 应置默认");
        assert_eq!(
            intent.groups[0].asset_type, "all",
            "非法 assetType 应改 all"
        );
        assert!(intent.groups[0].concepts[0].facet_hint.is_none());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn warns_when_explicit_units_are_missing_from_metadata() {
        let intent = SearchIntentV2 {
            groups: vec![SearchGroupV2::default()],
            ..Default::default()
        };
        let warnings = warn_missing_explicit_metadata("横图，大于 5MB，时长 10 秒", &intent);
        assert!(warnings.iter().any(|w| w.contains("file_size")));
        assert!(warnings.iter().any(|w| w.contains("duration_ms")));
        assert!(warnings.iter().any(|w| w.contains("aspect_ratio")));
    }

    /// W6-3：全非法 group → 剔除；全部 group 被剔 → 落第 3 层
    #[test]
    fn sanitize_all_drops_empty_groups_and_falls_back() {
        let facets: Vec<FacetPromptContext> = vec![];
        // 两个 group：一个只有合法概念，一个全空
        let mut intent = serde_json::from_str::<SearchIntentV2>(
            r#"{"groups":[{"assetType":"all","concepts":[{"text":"海边","role":"scene","facetHint":null,"confidence":0.9}],"textTerms":[],"metadata":[]},{"assetType":"all","concepts":[],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
        )
        .unwrap();
        let warnings = sanitize_all(&mut intent, &facets);
        assert_eq!(intent.groups.len(), 1, "全空 group 应被剔除");
        assert!(!warnings.is_empty());
        // 全空 → degrade_parse 落第 3 层
        let (i, _w) = degrade_parse(
            r#"{"groups":[{"assetType":"all","concepts":[],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
            "海边日落",
            &facets,
        );
        assert!(is_keyword_fallback(&i, "海边日落"));
    }

    /// 回归：纯素材类型组（assetType=video/image，无 concept/term/metadata，如「视频素材」）
    /// 绝不能被当空组剔除 —— 修复前 sanitize_all 漏算 asset_type 导致整组被删、expr=null。
    #[test]
    fn sanitize_keeps_pure_asset_type_group() {
        let facets: Vec<FacetPromptContext> = vec![];
        let mut intent = serde_json::from_str::<SearchIntentV2>(
            r#"{"groups":[{"assetType":"video","concepts":[],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
        )
        .unwrap();
        let _ = sanitize_all(&mut intent, &facets);
        assert_eq!(intent.groups.len(), 1, "纯视频类型组必须保留");
        assert_eq!(intent.groups[0].asset_type, "video");
    }

    /// 回归：纯 untaggedOnly 组（无 concept/term/metadata/类型）也不能被当空组剔除。
    #[test]
    fn sanitize_keeps_pure_untagged_group() {
        let facets: Vec<FacetPromptContext> = vec![];
        let mut intent = serde_json::from_str::<SearchIntentV2>(
            r#"{"groups":[{"assetType":"all","concepts":[],"textTerms":[],"metadata":[],"untaggedOnly":true}],"exclusions":[],"sortBy":null,"sortDir":null}"#,
        )
        .unwrap();
        let _ = sanitize_all(&mut intent, &facets);
        assert_eq!(intent.groups.len(), 1, "纯未打标组必须保留");
        assert!(intent.groups[0].untagged_only);
    }

    /// 「没打标签」→ 单个 Untagged 叶子；「没打标签的视频」→ AssetType(video) AND Untagged。
    #[test]
    fn build_expr_emits_untagged_leaf() {
        use crate::db::query_expr::{LeafCond, QueryExpr};
        let conn = init_memory().unwrap();
        fn count_untagged(e: &QueryExpr) -> usize {
            match e {
                QueryExpr::And { children } | QueryExpr::Or { children } => {
                    children.iter().map(count_untagged).sum()
                }
                QueryExpr::Not { child } => count_untagged(child),
                QueryExpr::Leaf { cond } => usize::from(matches!(cond, LeafCond::Untagged)),
            }
        }
        let pure = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "all".into(),
                untagged_only: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let (expr, _, w) = build_expr_from_v2(&conn, &pure).unwrap();
        let expr = expr.expect("纯未打标必须产出非空 expr");
        assert_eq!(count_untagged(&expr), 1, "应产出 1 个 Untagged 叶子：{w:?}");

        let vid = SearchIntentV2 {
            groups: vec![SearchGroupV2 {
                asset_type: "video".into(),
                untagged_only: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let (expr2, _, _) = build_expr_from_v2(&conn, &vid).unwrap();
        let expr2 = expr2.expect("未打标视频必须产出非空 expr");
        assert_eq!(count_untagged(&expr2), 1);
        match &expr2 {
            QueryExpr::And { children } => {
                let has_video = children.iter().any(|c| {
                    matches!(c, QueryExpr::Leaf { cond: LeafCond::AssetType { value } } if value == "video")
                });
                assert!(has_video, "未打标视频应同时含 AssetType(video) 叶子");
            }
            other => panic!("未打标视频应为 And 组，实际：{other:?}"),
        }
    }

    /// 回归（#14）：模型把「优先近景」误拆进独立 group 时，无 OR 词合并多组后，
    /// 该优先项必须保留为 preferred（加分），而不是被 v2_to_v3 升级成硬必须 concepts。
    #[test]
    fn merge_groups_keeps_misplaced_preferred_soft() {
        let raw = r#"{"groups":[
          {"assetType":"all","concepts":[{"text":"草地","role":"scene","facetHint":"scene","confidence":0.95,"necessity":"required","evidence":null,"weight":null,"termMatch":"alias"}],"textTerms":[],"metadata":[],"untaggedOnly":false,"preferred":[]},
          {"assetType":"all","concepts":[],"textTerms":[],"metadata":[],"untaggedOnly":false,"preferred":[{"text":"近景","role":"composition","facetHint":"composition","confidence":0.9,"necessity":"preferred","evidence":"优先近景","weight":1.0,"termMatch":"alias"}]}
        ],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (out, _w) = degrade_parse_v3(raw, "草地，优先近景", &[]);
        assert_eq!(out.groups.len(), 1, "无「或」词的多组应合并为一组");
        let g = &out.groups[0];
        assert!(
            g.concepts.iter().any(|c| c.text == "草地"),
            "必须项草地保留"
        );
        assert!(
            g.preferred.iter().any(|c| c.text == "近景"),
            "误拆到独立组的优先项应保留为 preferred"
        );
        assert!(
            !g.concepts.iter().any(|c| c.text == "近景"),
            "优先项近景不得被升级为硬必须"
        );
    }

    #[test]
    fn repair_moves_alias_concepts_from_preferred_clause() {
        let mut intent = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![
                    SearchConceptV3 {
                        text: "青年".into(),
                        role: "people".into(),
                        facet_hint: Some("people".into()),
                        confidence: Some(0.95),
                        necessity: Necessity::Required,
                        weight: None,
                        evidence: None,
                        term_match: crate::db::tags::TermMatch::Alias,
                    },
                    SearchConceptV3 {
                        text: "女性".into(),
                        role: "people".into(),
                        facet_hint: Some("people".into()),
                        confidence: Some(0.95),
                        necessity: Necessity::Required,
                        weight: None,
                        evidence: None,
                        term_match: crate::db::tags::TermMatch::Alias,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let warnings = repair_misplaced_preferred(
            "最好要年轻女性",
            &[
                "性别 / 女性 | term: 女性".into(),
                "年龄 / 青年 | term: 青年 | aliases: 年轻".into(),
            ],
            &mut intent,
        );
        let group = &intent.groups[0];
        assert!(group.concepts.is_empty(), "整段偏好不能留在必须区");
        assert_eq!(
            group
                .preferred
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["青年", "女性"]
        );
        assert!(group
            .preferred
            .iter()
            .all(|c| c.evidence.as_deref() == Some("最好要年轻女性")));
        assert!(warnings.iter().any(|w| w.contains("纠正为优先")));
    }

    #[test]
    fn repair_keeps_explicit_required_concept_outside_preference_clause() {
        let mut intent = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![
                    SearchConceptV3 {
                        text: "女性".into(),
                        role: "people".into(),
                        facet_hint: Some("people".into()),
                        confidence: Some(0.95),
                        necessity: Necessity::Required,
                        weight: None,
                        evidence: None,
                        term_match: crate::db::tags::TermMatch::Alias,
                    },
                    SearchConceptV3 {
                        text: "青年".into(),
                        role: "people".into(),
                        facet_hint: Some("people".into()),
                        confidence: Some(0.95),
                        necessity: Necessity::Required,
                        weight: None,
                        evidence: None,
                        term_match: crate::db::tags::TermMatch::Alias,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        repair_misplaced_preferred(
            "要女性，最好年轻",
            &[
                "性别 / 女性 | term: 女性".into(),
                "年龄 / 青年 | term: 青年 | aliases: 年轻".into(),
            ],
            &mut intent,
        );
        let group = &intent.groups[0];
        assert_eq!(
            group
                .concepts
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["女性"]
        );
        assert_eq!(
            group
                .preferred
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["青年"]
        );
    }

    #[test]
    fn v3_parse_preserves_pure_preferred_group() {
        let raw = r#"{"groups":[{"assetType":"all","concepts":[],"preferred":[{"text":"蓝天","role":"scene","facetHint":null,"confidence":0.8,"evidence":"最好有蓝天","weight":1.0,"termMatch":null}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (intent, warnings) = degrade_parse_v3(raw, "最好有蓝天", &[]);
        assert_eq!(intent.groups.len(), 1, "纯偏好仍需保留占位 group");
        assert!(intent.groups[0].concepts.is_empty());
        assert_eq!(intent.groups[0].preferred.len(), 1);
        assert!(!is_keyword_fallback_v3(&intent, "最好有蓝天"));
        assert!(warnings.is_empty(), "合法纯偏好不应告警：{warnings:?}");
    }

    #[test]
    fn v3_empty_group_without_preferred_still_falls_back() {
        let raw = r#"{"groups":[{"assetType":"all","concepts":[],"preferred":[],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (intent, warnings) = degrade_parse_v3(raw, "海边", &[]);
        assert!(
            is_keyword_fallback_v3(&intent, "海边"),
            "没有 preferred 的空 group 不得变成全库查询：{warnings:?}"
        );
    }

    // ═══════════════ S3：preferred evidence 守卫与词典关联 ═══════════════

    fn v3_group(input: &str, pref_text: &str, evidence: &str) -> (SearchGroupV3, Vec<String>) {
        let mut g = SearchGroupV3 {
            asset_type: "all".into(),
            preferred: vec![SearchConceptV3 {
                text: pref_text.into(),
                role: "scene".into(),
                facet_hint: Some("scene".into()),
                confidence: Some(0.8),
                necessity: Necessity::Preferred,
                weight: Some(1.0),
                evidence: Some(evidence.into()),
                term_match: crate::db::tags::TermMatch::Alias,
            }],
            ..Default::default()
        };
        let warns = guard_preferred(input, &mut g);
        (g, warns)
    }

    /// S3 真值表：原句引用由纯解析守卫校验，规范名/别名关联在 plan 构建时校验。
    #[test]
    fn guard_preferred_accepts_six_chinese_expressions() {
        // ① ② 都过 + 强偏好词命中 → 静默（保留 preferred）
        for (input, pref, ev) in [
            ("最好有蓝天", "蓝天", "最好有蓝天"),
            ("有蓝天更好", "蓝天", "有蓝天更好"),
            ("尽量有蓝天", "蓝天", "尽量有蓝天"),
        ] {
            let (g, warns) = v3_group(input, pref, ev);
            assert!(
                g.preferred
                    .iter()
                    .any(|c| c.text == pref && c.necessity == Necessity::Preferred),
                "{input} 应保留为加分项（preferred={:?} concepts={:?}）",
                g.preferred.iter().map(|c| &c.text).collect::<Vec<_>>(),
                g.concepts.iter().map(|c| &c.text).collect::<Vec<_>>()
            );
            assert!(
                !warns.iter().any(|w| w.contains("已忽略该加分项")),
                "{input} warns={warns:?}"
            );
        }
        // 「蓝天可有可无」：①② 过，原句无强偏好词（可有可无不在 CORE）→ info 但不降级
        let (g2, w2) = v3_group("蓝天可有可无", "蓝天", "蓝天可有可无");
        assert!(
            g2.preferred.iter().any(|c| c.text == "蓝天"),
            "可有可无不得降级"
        );
        assert!(
            w2.iter().any(|w| w.contains("未见明显的偏好表述")),
            "应记 info：{w2:?}"
        );
    }

    /// S3：evidence 编造（不是原句子串）→ 丢弃该 preferred，不能升级 required。
    #[test]
    fn guard_rejects_fabricated_evidence() {
        // 原句「必须有蓝天」没有「最好」；模型编造 evidence「最好有蓝天」→ 忽略加分项
        let (g, warns) = v3_group("必须有蓝天", "蓝天", "最好有蓝天");
        assert!(g.preferred.is_empty(), "编造依据必须忽略：{warns:?}");
        assert!(!g.concepts.iter().any(|c| c.text == "蓝天"));
        assert!(
            warns.iter().any(|w| w.contains("已忽略该加分项")),
            "{warns:?}"
        );
    }

    /// S3：evidence 是原句子串但与 concept 无关 → plan 构建时丢弃该 preferred。
    #[test]
    fn plan_rejects_unrelated_evidence_after_term_resolution() {
        // 原句「横图，最好清新」；concept=蓝天 evidence=最好清新（在原文，但离蓝天很远）
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        tags::create_in_facet(&conn, "蓝天", None, Some("scene")).unwrap();
        let input = "横图，最好清新";
        let mut g = SearchGroupV3 {
            asset_type: "all".into(),
            preferred: vec![SearchConceptV3 {
                text: "蓝天".into(),
                role: "scene".into(),
                facet_hint: None,
                confidence: None,
                necessity: Necessity::Preferred,
                weight: None,
                evidence: Some("最好清新".into()),
                term_match: crate::db::tags::TermMatch::Alias,
            }],
            ..Default::default()
        };
        let warns = guard_preferred(input, &mut g);
        assert_eq!(
            g.preferred.len(),
            1,
            "原句引用本身有效，应留到词典复核：{warns:?}"
        );
        let intent = SearchIntentV3 {
            groups: vec![g],
            ..Default::default()
        };
        let (plan, _resolved, plan_warnings) = build_plan_from_v3(&conn, &intent).unwrap();
        assert!(
            plan.should.is_empty(),
            "无关依据不得进入 should: {plan_warnings:?}"
        );
        assert!(
            plan_warnings.iter().any(|w| w.contains("已忽略该加分项")),
            "{plan_warnings:?}"
        );
    }

    #[test]
    fn preferred_canonical_term_accepts_surface_alias_in_evidence() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        let youth = tags::create_in_facet(&conn, "青年", None, Some("people"))
            .unwrap()
            .id;
        tags::add_alias(&conn, youth, "年轻", None, "synonym").unwrap();
        let mut group = SearchGroupV3 {
            asset_type: "all".into(),
            preferred: vec![SearchConceptV3 {
                text: "青年".into(),
                role: "people".into(),
                facet_hint: Some("people".into()),
                confidence: Some(0.95),
                necessity: Necessity::Preferred,
                weight: Some(1.0),
                evidence: Some("最好是年轻女性".into()),
                term_match: crate::db::tags::TermMatch::Alias,
            }],
            ..Default::default()
        };
        let parse_warnings = guard_preferred("要女生人像，在室内，最好是年轻女性", &mut group);
        assert!(
            parse_warnings.is_empty(),
            "原句证据合法: {parse_warnings:?}"
        );
        let intent = SearchIntentV3 {
            groups: vec![group],
            ..Default::default()
        };
        let (plan, resolved, warnings) = build_plan_from_v3(&conn, &intent).unwrap();
        assert_eq!(plan.should.len(), 1, "青年应进入 should: {warnings:?}");
        assert!(resolved.iter().any(|r| r.tag_id == youth));
        assert!(
            warnings.is_empty(),
            "规范名/别名 evidence 不应告警: {warnings:?}"
        );
    }

    #[test]
    fn dictionary_and_candidates_follow_tag_terms_when_feature_enabled() {
        let conn = init_memory().unwrap();
        setup_facets_and_tags(&conn);
        crate::db::migrations::create_tag_terms_table_for_test(&conn).unwrap();
        crate::db::schema_features::set_feature(&conn, "tag_unique_terms", true, None).unwrap();
        let youth = tags::create_in_facet(&conn, "青年", None, Some("people"))
            .unwrap()
            .id;
        tags::add_alias(&conn, youth, "年轻", None, "synonym").unwrap();
        let facets = vec![FacetPromptContext {
            key: "people".into(),
            display_name: "人物".into(),
            description: String::new(),
            selection_mode: "multi".into(),
            max_items: Some(5),
            ..Default::default()
        }];
        let dict = collect_tag_dictionary(&conn, &facets).unwrap();
        let youth_line = dict.iter().find(|line| line.contains("term: 青年"));
        assert!(
            youth_line.is_some_and(|line| line.contains("年轻")),
            "tag_terms 别名必须进入 AI 词典: {dict:?}"
        );
        let candidates = tags::search_candidates(&conn, Some("people"), "年轻").unwrap();
        assert!(
            candidates.iter().any(|tag| tag.id == youth),
            "tag_terms 别名必须参与候选解析: {candidates:?}"
        );
    }

    /// S3：required 概念绝不反向升级为 preferred；invalid preferred 也绝不升级为 required。
    #[test]
    fn preferred_never_upgraded_to_required() {
        let mut g = SearchGroupV3 {
            asset_type: "all".into(),
            concepts: vec![SearchConceptV3 {
                text: "夜景".into(),
                role: "scene".into(),
                facet_hint: None,
                confidence: Some(0.9),
                necessity: Necessity::Required,
                weight: None,
                evidence: Some("排除夜景".into()), // 即便给了 evidence，也必须保持 required
                term_match: crate::db::tags::TermMatch::Alias,
            }],
            preferred: vec![SearchConceptV3 {
                text: "蓝天".into(),
                role: "scene".into(),
                facet_hint: None,
                confidence: Some(0.8),
                necessity: Necessity::Preferred,
                weight: Some(1.0),
                evidence: Some("最好有蓝天".into()),
                term_match: crate::db::tags::TermMatch::Alias,
            }],
            ..Default::default()
        };
        let _warns = guard_preferred("不要夜景，最好有蓝天", &mut g);
        // 夜景（required）仍在 concepts 且 necessity=Required
        assert!(g
            .concepts
            .iter()
            .any(|c| c.text == "夜景" && c.necessity == Necessity::Required));
        assert!(
            !g.preferred.iter().any(|c| c.text == "夜景"),
            "required 不得升级为 preferred"
        );
        // 蓝天（证据合法）保留加分
        assert!(g.preferred.iter().any(|c| c.text == "蓝天"));
    }

    // ── C-3：库能力注入（只告知，不改写） ──

    fn insert_asset_row(
        conn: &rusqlite::Connection,
        path: &str,
        mime: &str,
        size: i64,
        taken: Option<i64>,
        lat: Option<f64>,
        lon: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at, taken_at, latitude, longitude)
             VALUES (?1, ?2, ?3, ?4, ?5, 1700000000000, 1700000000000, ?6, ?7, ?8)",
            rusqlite::params![path, path.rsplit('/').next().unwrap(), "jpg", size, mime, taken, lat, lon],
        )
        .unwrap();
    }

    /// C-3：schema 对 file_size 设 minimum:1024（服务端拦截「5..10」这类 MB 原值）。
    #[test]
    fn schema_file_size_has_minimum() {
        let s = intent_schema(&[]);
        let m = &s["properties"]["groups"]["items"]["properties"]["metadata"]["items"];
        // allOf[0] = if key==file_size then value/min/max number 需 >= 1024
        let cond = &m["allOf"][0];
        assert_eq!(
            cond["if"]["properties"]["key"]["const"], "file_size",
            "条件分支必须锁 file_size"
        );
        let then_props = &cond["then"]["properties"];
        for field in ["value", "min", "max"] {
            let anyof = &then_props[field]["anyOf"];
            let nums = anyof
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["type"] == "number")
                .collect::<Vec<_>>();
            assert!(!nums.is_empty(), "file_size 的 {field} 必须允许数值类型");
            for n in nums {
                assert_eq!(
                    n["minimum"], 1024.0,
                    "file_size 的 {field} 数值下限必须 1024 字节"
                );
            }
        }
        // 非 file_size 的 key 不应被该条件误伤（latitude 允许负值/小数）
        let s2 = intent_schema(&[]);
        let m2 = &s2["properties"]["groups"]["items"]["properties"]["metadata"]["items"];
        let cond2 = &m2["allOf"][0];
        assert_eq!(cond2["if"]["properties"]["key"]["const"], "file_size");
    }

    /// C-3：库能力摘要是「陈述事实」，不含「不要输出」这类禁令措辞。
    /// 措辞是给模型判断 0 结果的背景，不是让它删条件的命令。
    #[test]
    fn library_stats_injected_as_facts_not_prohibitions() {
        let conn = init_memory().unwrap();
        // 小库：2 图 + 1 视频，含 GPS 与时间，验证聚合数字确实进文本
        insert_asset_row(
            &conn,
            "d:/a/1.jpg",
            "image/jpeg",
            6_272_000,
            Some(1_760_000_000_000),
            Some(31.2),
            Some(121.5),
        );
        insert_asset_row(
            &conn,
            "d:/a/2.jpg",
            "image/jpeg",
            36_200_000,
            Some(1_760_000_000_000),
            None,
            None,
        );
        insert_asset_row(
            &conn,
            "d:/a/3.mp4",
            "video/mp4",
            12_000_000,
            None,
            None,
            None,
        );
        let caps = library_capabilities(&conn).unwrap();
        // 陈述事实而非禁令：允许「该条件无结果仍输出」，不允许「不要输出 xxx」
        for banned in ["不要输出", "不要生成", "禁止输出", "绝不能", "只能"] {
            assert!(
                !caps.contains(banned),
                "库能力摘要不得含禁令措辞「{banned}」：\n{caps}"
            );
        }
        // 事实数字在文本中
        assert!(caps.contains("3 张"), "应聚合出总数：{caps}");
        assert!(caps.contains("图片 2"), "应聚合出图片数：{caps}");
        assert!(caps.contains("视频 1"), "应聚合出视频数：{caps}");
        assert!(caps.contains("1 张有 GPS"), "应聚合出 GPS 数：{caps}");
        assert!(
            caps.contains("如果用户的条件在本库必然无结果，仍要如实输出该条件"),
            "{caps}"
        );
    }

    /// C-3：用户明确搜无 GPS 条件 → 后端绝不因库统计剔除该条件（原则 P5）。
    /// 库一张 GPS 都没有，用户搜「有定位的照片」仍保留条件并如实返回 0 命中，
    /// 而不是删掉条件返回全库。
    #[test]
    fn gps_condition_survives_when_library_has_none() {
        let conn = init_memory().unwrap();
        // 库中没有任何 GPS 数据
        insert_asset_row(
            &conn,
            "d:/a/1.jpg",
            "image/jpeg",
            6_272_000,
            Some(1_760_000_000_000),
            None,
            None,
        );
        insert_asset_row(
            &conn,
            "d:/a/2.jpg",
            "image/jpeg",
            36_200_000,
            Some(1_760_000_000_000),
            None,
            None,
        );
        let caps = library_capabilities(&conn).unwrap();
        assert!(
            caps.contains("0 张有 GPS"),
            "库能力摘要应如实说明 0 GPS：{caps}"
        );
        // AI 明确输出「有定位」条件 → 解析层不得剔除（has_location 值域 yes/no）
        let text = "有定位的照片";
        let raw = r#"{"groups":[{"assetType":"all","concepts":[],"textTerms":[],"metadata":[{"key":"has_location","op":"eq","value":"yes","values":null,"min":null,"max":null}]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (intent, warnings) = degrade_parse(raw, text, &[]);
        assert!(
            !is_keyword_fallback(&intent, text),
            "明确 GPS 条件不应被兜底：{warnings:?}"
        );
        assert_eq!(
            intent.groups[0].metadata.len(),
            1,
            "GPS 条件必须保留（绝不做库统计剔除）"
        );
        assert_eq!(intent.groups[0].metadata[0].key, "has_location");
        // 该条件真实执行后应为 0 命中 —— 由编译层自然得出，这里验证 key 合法
        assert!(
            crate::db::search_query::is_supported_metadata_key("has_location"),
            "has_location 必须在白名单内"
        );
    }

    /// C-3：build_user_prompt 把库能力摘要作为独立小节拼到查询之后（注入路径可测）。
    #[test]
    fn user_prompt_appends_capabilities_after_query() {
        let dict = vec!["海边 | aliases: 海滩".to_string()];
        let facets: Vec<FacetPromptContext> = vec![];
        let caps = "本库现状：\n- 3 张，图片 2 / 视频 1\n如果用户的条件在本库必然无结果，仍要如实输出该条件 —— 系统会解释原因。";
        let p = build_user_prompt(&dict, &facets, "海边", caps);
        assert!(
            p.contains("用户查询：<query>海边</query>"),
            "查询块保持完整：{p}"
        );
        assert!(p.ends_with(caps), "能力摘要应拼在 prompt 最末尾：{p}");
        // 空 capabilities → 不注入（命令层短锁失败静默降级路径）
        let p2 = build_user_prompt(&dict, &facets, "海边", "");
        assert!(!p2.contains("本库现状"), "空能力摘要不应注入：{p2}");
    }

    /// P1：搜索侧与打标侧同语义 —— max_items=None（不限）渲染成「不限」而非 max=3，
    /// 否则模型会照抄硬上限（与 ai_cloud 的「数量不限」对着干）。
    #[test]
    fn search_prompt_renders_unlimited_not_max3() {
        let dict: Vec<String> = vec![];
        let unlimited = FacetPromptContext {
            key: "people".into(),
            display_name: "人物".into(),
            description: "画面里的人".into(),
            selection_mode: "multi".into(),
            max_items: None,
            ..Default::default()
        };
        let capped = FacetPromptContext {
            max_items: Some(5),
            ..unlimited.clone()
        };
        let facets = vec![capped, unlimited];
        let p = build_user_prompt(&dict, &facets, "人", "");
        assert!(p.contains("max=5"), "有上限要如实写 max=5：{p}");
        assert!(!p.contains("max=3"), "None 不限绝不能渲染成 max=3：{p}");
        assert!(p.contains("max=不限（无数量上限）"), "不限要写明：{p}");
    }

    // ═══════════════ S3 解析层接线（V3→V2→关键词 三层） ═══════════════

    /// S3：V3 解析成功且含合法 preferred → 保留加分项，返回 V3 结构。
    #[test]
    fn v3_parse_keeps_valid_preferred() {
        let facets: Vec<FacetPromptContext> = vec![];
        let raw = r#"{"groups":[{"assetType":"all","concepts":[{"text":"草地","role":"scene","facetHint":null,"confidence":0.9}],"preferred":[{"text":"蓝天","role":"scene","facetHint":null,"confidence":0.8,"evidence":"最好有蓝天","weight":1.0,"termMatch":null}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (intent, warnings) = degrade_parse_v3(raw, "草地，最好有蓝天", &facets);
        assert_eq!(intent.groups.len(), 1, "应保留单组：{warnings:?}");
        assert!(
            intent.groups[0].preferred.iter().any(|c| c.text == "蓝天"),
            "合法加分项应保留：preferred={:?} warnings={warnings:?}",
            intent.groups[0]
                .preferred
                .iter()
                .map(|c| &c.text)
                .collect::<Vec<_>>()
        );
    }

    /// S3：V3 解析失败（JSON 结构 V2 合法但 V3 非法，如 preferred 类型错）→ 落 V2 语义。
    /// 模拟旧模型输出 —— 无 preferred 字段的 V2 JSON 也能被 V3 解析（serde default），
    /// 真正的「V3 解析失败」= 顶层结构 V3 无法读取 → 降级路径由 request_intent 兜底；
    /// 本测试验证 V2 JSON 进 V3 管线不炸、结果语义与 V2 一致。
    #[test]
    fn v3_parse_failure_falls_back_to_v2() {
        let facets: Vec<FacetPromptContext> = vec![];
        // ① 畸形输入（非 JSON）→ V3 管线落关键词（V3 形态）
        let (i1, w1) = degrade_parse_v3("模型在胡言乱语", "草地", &facets);
        assert!(
            i1.groups[0]
                .text_terms
                .iter()
                .any(|t| t.text.contains("草地")),
            "应关键词兜底（整句进 textTerms）：{w1:?}"
        );
        // ② V2 合法 JSON（无 preferred）→ V3 管线解析出与 V2 相同的概念集
        let v2_raw = r#"{"groups":[{"assetType":"all","concepts":[{"text":"草地","role":"scene","facetHint":null,"confidence":0.9}],"textTerms":[],"metadata":[]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (v3i, _) = degrade_parse_v3(v2_raw, "草地", &facets);
        let v2i = v3_to_v2_view(&v3i);
        assert_eq!(
            v2i.groups[0].concepts[0].text, "草地",
            "V2 JSON 应保持原语义"
        );
        assert!(v3i.groups[0].preferred.is_empty(), "V2 无加分项");
    }

    /// 回归：V3 strict 路径里 metadata「file_size gte 缺 value」必须在 sanitize 后正确回写 V3，
    /// 残缺条件被就近剔除、其余概念保留 —— 修复前漏回写 metadata，残缺条件留到 build_plan 整体报「需要 value」。
    #[test]
    fn v3_strict_drops_incomplete_metadata_after_sanitize() {
        let facets: Vec<FacetPromptContext> = vec![];
        let raw = r#"{"groups":[{"assetType":"all","concepts":[{"text":"海边","role":"scene","facetHint":null,"confidence":0.9}],"preferred":[],"textTerms":[],"metadata":[{"key":"file_size","op":"gte","value":null,"values":null,"min":null,"max":null}]}],"exclusions":[],"sortBy":null,"sortDir":null}"#;
        let (intent, _warnings) = degrade_parse_v3(raw, "海边，大于5MB", &facets);
        assert_eq!(intent.groups.len(), 1, "组应保留");
        assert!(
            intent.groups[0].metadata.is_empty(),
            "残缺的 file_size gte（缺 value）必须被剔除，不能留到 build_plan 报错：{:?}",
            intent.groups[0].metadata
        );
        assert!(
            intent.groups[0].concepts.iter().any(|c| c.text == "海边"),
            "其余合法概念必须保留"
        );
    }

    /// S3：schema 已扩展 V3 字段 —— group.preferred 与 evidence/weight/termMatch 可被 json_schema 服务商接受。
    #[test]
    fn schema_supports_v3_preferred_field() {
        let s = intent_schema(&[]);
        let g = &s["properties"]["groups"]["items"];
        assert!(
            g["properties"]["preferred"].is_object(),
            "group schema 必须声明 preferred 数组"
        );
        let pref_items = &g["properties"]["preferred"]["items"];
        for field in ["evidence", "weight", "termMatch"] {
            assert!(
                pref_items["properties"][field].is_object(),
                "加分项 schema 缺 {field}"
            );
        }
    }

    /// S3：V3 概念清洗复用 V2 规则（句子化/停用词/空文本剔除），同时保留 V3 特有字段。
    #[test]
    fn v3_clean_concepts_keeps_v3_fields() {
        let mut warns = Vec::new();
        let mut total = 0usize;
        let mut cs = vec![SearchConceptV3 {
            text: " 最好有蓝天 ".into(),
            role: "scene".into(),
            facet_hint: Some("scene".into()),
            confidence: Some(0.9),
            necessity: Necessity::Preferred,
            weight: Some(2.0),
            evidence: Some("最好有蓝天".into()),
            term_match: crate::db::tags::TermMatch::Alias,
        }];
        clean_concepts_v3(&mut cs, &mut warns, &mut total);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].text, "最好有蓝天", "首尾空白应去除");
        assert_eq!(cs[0].necessity, Necessity::Preferred, "V3 字段不因清洗丢失");
        assert_eq!(cs[0].weight, Some(2.0));
        assert_eq!(cs[0].evidence.as_deref(), Some("最好有蓝天"));
        // 句子化文本剔除（沿用 V2 规则：超过 12 字）
        let mut long = vec![SearchConceptV3 {
            text: "这是一个非常长的句子描述画面".into(),
            role: String::new(),
            facet_hint: None,
            confidence: None,
            necessity: Necessity::Required,
            weight: None,
            evidence: None,
            term_match: crate::db::tags::TermMatch::Alias,
        }];
        clean_concepts_v3(&mut long, &mut warns, &mut total);
        assert!(long.is_empty(), "句子化概念应剔除");
        assert!(warns.iter().any(|w| w.contains("句子而非原子概念")));
    }

    // ═══════════════ S3：V3 intent → SearchPlanV3（build_plan_from_v3，真实 DB） ═══════════════

    fn v3_insert_asset(conn: &rusqlite::Connection, path: &str) -> i64 {
        conn.query_row(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES (?1, ?2, 'jpg', 1000, 'image/jpeg', 1700000000000, 1700000000000) RETURNING id",
            rusqlite::params![path, path.rsplit('/').next().unwrap()],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// S3：required（filter）∧ preferred（should，不淘汰）∧ exclusions（must_not）
    /// 经 build_plan_from_v3 正确拆分，run_search_plan 可执行且加分不淘汰。
    #[test]
    fn build_plan_from_v3_splits_required_preferred_excluded() {
        use crate::db::query_expr::QueryExpr;
        use crate::db::search_plan::run_search_plan;
        use crate::db::tags;
        let conn = init_memory().unwrap();
        let grass = tags::create_in_facet(&conn, "草地", None, Some("scene")).unwrap();
        let sky = tags::create_in_facet(&conn, "蓝天", None, Some("scene")).unwrap();
        let night = tags::create_in_facet(&conn, "夜景", None, Some("scene")).unwrap();
        let a = v3_insert_asset(&conn, "d:/s3/grass_sky.jpg");
        let b = v3_insert_asset(&conn, "d:/s3/grass_only.jpg");
        let e = v3_insert_asset(&conn, "d:/s3/grass_night.jpg");
        crate::db::asset_tags::assign(&conn, &[a], &[grass.id, sky.id], "manual").unwrap();
        crate::db::asset_tags::assign(&conn, &[b], &[grass.id], "manual").unwrap();
        crate::db::asset_tags::assign(&conn, &[e], &[grass.id, night.id], "manual").unwrap();
        let c3 =
            |text: &str, role: &str, necessity: Necessity, ev: Option<&str>, w: Option<f32>| {
                SearchConceptV3 {
                    text: text.into(),
                    role: role.into(),
                    facet_hint: Some("scene".into()),
                    confidence: Some(0.95),
                    necessity,
                    weight: w,
                    evidence: ev.map(String::from),
                    term_match: crate::db::tags::TermMatch::Alias,
                }
            };
        let intent = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![c3("草地", "scene", Necessity::Required, None, None)],
                preferred: vec![c3(
                    "蓝天",
                    "scene",
                    Necessity::Preferred,
                    Some("最好有蓝天"),
                    Some(1.0),
                )],
                text_terms: vec![],
                metadata: vec![],
                untagged_only: false,
            }],
            exclusions: vec![c3("夜景", "scene", Necessity::Required, None, None)],
            sort_by: None,
            sort_dir: None,
        };
        let (plan, resolved, warns) = build_plan_from_v3(&conn, &intent).unwrap();
        // filter = 草地（required）；must_not = 夜景；should = 蓝天 加分
        let leaf_ids = |e: &Option<QueryExpr>| -> Vec<i64> {
            let mut out = Vec::new();
            if let Some(QueryExpr::Leaf {
                cond: crate::db::query_expr::LeafCond::Tag { tag_ids, .. },
            }) = e
            {
                out.extend(tag_ids);
            }
            out
        };
        assert_eq!(
            leaf_ids(&plan.filter),
            vec![grass.id],
            "filter 应只含草地（required）"
        );
        assert_eq!(plan.should.len(), 1, "蓝天应是唯一加分项：{warns:?}");
        assert_eq!(
            leaf_ids(&plan.must_not),
            vec![night.id],
            "must_not 应含夜景"
        );
        assert!(resolved.iter().any(|r| r.tag_id == grass.id));
        assert!(resolved.iter().any(|r| r.tag_id == sky.id));
        // 执行：草地照都在（含无蓝天），夜景照被排除，蓝天加分排最前
        let out = run_search_plan(&conn, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert!(ids.contains(&b), "无蓝天的草地不得被淘汰：{ids:?}");
        assert!(!ids.contains(&e), "草地+夜景必须排除");
        assert_eq!(ids[0], a, "有蓝天应排最前：{ids:?}");
    }

    #[test]
    fn build_plan_from_v3_caps_preferred_clauses_with_warning() {
        use crate::db::search_plan::MAX_SHOULD_CLAUSES;
        use crate::db::tags;
        let conn = init_memory().unwrap();
        let mut preferred = Vec::new();
        for i in 0..(MAX_SHOULD_CLAUSES + 1) {
            let name = format!("偏好{i}");
            tags::create_in_facet(&conn, &name, None, Some("scene")).unwrap();
            preferred.push(SearchConceptV3 {
                text: name,
                role: "scene".into(),
                facet_hint: Some("scene".into()),
                confidence: Some(0.95),
                necessity: Necessity::Preferred,
                weight: Some(1.0),
                evidence: None,
                term_match: crate::db::tags::TermMatch::Alias,
            });
        }
        let intent = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![],
                preferred,
                text_terms: vec![],
                metadata: vec![],
                untagged_only: false,
            }],
            exclusions: vec![],
            sort_by: None,
            sort_dir: None,
        };
        let (plan, _, warnings) = build_plan_from_v3(&conn, &intent).unwrap();
        assert_eq!(plan.should.len(), MAX_SHOULD_CLAUSES);
        assert!(warnings.iter().any(|w| w.contains("加分项超过")));
    }

    /// S3：keyword 兜底形态转 plan 后 filter 含整句 content 搜索（可执行、不 panic）。
    #[test]
    fn build_plan_from_v3_keyword_fallback_runs() {
        use crate::db::search_plan::run_search_plan;
        let conn = init_memory().unwrap();
        let v3 = keyword_intent_v3("海边日落");
        let (plan, _, _) = build_plan_from_v3(&conn, &v3).unwrap();
        assert!(plan.should.is_empty(), "关键词兜底无加分项");
        let _ = run_search_plan(&conn, &plan, Some(10), 0).unwrap();
    }

    /// S5 5-3：concept_leaf 必须回填 term_query（c.text），不得恒发 None ——
    /// 否则 termMatch（contains/prefix/fuzzy）永远不可达，AI 端「带…的」措辞失效。
    /// 同时验证：模型 termMatch=contains 时，同一概念经扩展命中多个标签并执行成功。
    #[test]
    fn concept_leaf_fills_term_query_for_term_match() {
        use crate::db::query_expr::{LeafCond, QueryExpr};
        use crate::db::search_plan::run_search_plan;
        use crate::db::tags;
        let conn = init_memory().unwrap();
        // contains 扩展需 tag_terms 事实源（S5 前提：V22b 约束 + feature gate）
        crate::db::migrations::apply_v22b_constraints(&conn).unwrap();
        crate::db::schema_features::set_feature(&conn, "tag_unique_terms", true, None).unwrap();
        let forest = tags::create_in_facet(&conn, "森林", None, Some("scene")).unwrap();
        let person = tags::create_in_facet(&conn, "人物", None, Some("subject")).unwrap();
        let single = tags::create_in_facet(&conn, "单人", None, Some("subject")).unwrap();
        let a = v3_insert_asset(&conn, "d:/s5/forest.jpg");
        let b = v3_insert_asset(&conn, "d:/s5/person.jpg");
        let c_img = v3_insert_asset(&conn, "d:/s5/single.jpg");
        crate::db::asset_tags::assign(&conn, &[a], &[forest.id], "manual").unwrap();
        crate::db::asset_tags::assign(&conn, &[b], &[person.id], "manual").unwrap();
        crate::db::asset_tags::assign(&conn, &[c_img], &[single.id], "manual").unwrap();
        let mk = |text: &str, m: crate::db::tags::TermMatch| SearchConceptV3 {
            text: text.into(),
            role: "scene".into(),
            facet_hint: None,
            confidence: Some(0.9),
            necessity: Necessity::Required,
            weight: None,
            evidence: None,
            term_match: m,
        };
        // contains「人」→ 命中「人物」+「单人」（多命中，cap 内）；termQuery 回填原文
        let intent = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![mk("人", crate::db::tags::TermMatch::Contains)],
                preferred: vec![],
                text_terms: vec![],
                metadata: vec![],
                untagged_only: false,
            }],
            exclusions: vec![],
            sort_by: None,
            sort_dir: None,
        };
        let (plan, _resolved, warns) = build_plan_from_v3(&conn, &intent).unwrap();
        match &plan.filter {
            Some(QueryExpr::Leaf {
                cond:
                    LeafCond::Tag {
                        term_query,
                        term_match,
                        ..
                    },
            }) => {
                assert_eq!(
                    term_query.as_deref(),
                    Some("人"),
                    "concept_leaf 必须回填 term_query"
                );
                assert_eq!(*term_match, crate::db::tags::TermMatch::Contains);
            }
            other => panic!("filter 应为 Tag leaf：{other:?}"),
        }
        // 扩展发生在编译层：contains「人」应命中 人物+单人两张照，森林不命中
        let out = run_search_plan(&conn, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert!(
            ids.contains(&b) && ids.contains(&c_img),
            "contains 应命中 人物+单人：{ids:?} {warns:?}"
        );
        assert!(!ids.contains(&a), "森林不得命中");

        // 对照：默认 alias 时同概念走精确解析（resolve_concept 直接映射不到 → content 搜索），
        // 但只要落到 Tag leaf 就必须带 term_query（这是本条锁定的不变式）。
        let intent2 = SearchIntentV3 {
            groups: vec![SearchGroupV3 {
                asset_type: "all".into(),
                concepts: vec![mk("人", crate::db::tags::TermMatch::Alias)],
                preferred: vec![],
                text_terms: vec![],
                metadata: vec![],
                untagged_only: false,
            }],
            exclusions: vec![],
            sort_by: None,
            sort_dir: None,
        };
        let (plan2, _, _) = build_plan_from_v3(&conn, &intent2).unwrap();
        if let Some(QueryExpr::Leaf {
            cond: LeafCond::Tag { term_query, .. },
        }) = &plan2.filter
        {
            assert_eq!(term_query.as_deref(), Some("人"), "alias 也回填 term_query");
        }
    }
}
