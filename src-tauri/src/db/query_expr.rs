//! 布尔查询表达式树（超级搜索 UI 构建器 → 数据库查询的桥接）。
//!
//! 现状：`AssetFilter` 是扁平结构（facet/metadata 间隐式 AND，exclude 隐式 NOT），
//! 表达不了 `(A 或 B) 且 非C` 这类嵌套布尔。
//! 本模块引入 `QueryExpr`（And/Or/Not/Leaf），由 `assets::build_where` 递归编译，
//! 叶子直接复用现有白名单能力（facet EXISTS、exclude EXISTS、元数据编译、类型/未打标）。
//!
//! 原则：
//! - 列名/操作符来自白名单匹配，绝对不来自外部输入；
//! - 所有值参数绑定；
//! - 递归深度与节点总数有上限；
//! - 未知字段/操作符一律拒绝，不静默忽略。

use serde::{Deserialize, Serialize};

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OptionalExtension};

use super::sql_utils::offset_placeholders;
use super::tag_facets::EFF_SEARCH;
use super::tags::TermMatch;
use crate::error::{AppError, AppResult};

/// FB5-05（§8.2/§8.3）：搜索范围。列名只能由本枚举映射，绝不能来自模型或用户字符串。
/// serde camelCase + Default = All（旧数据缺 scope 时按 all 处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchScope {
    /// 文件名 + 标签 + 描述
    #[default]
    All,
    /// 文件名 + 描述（AI 未映射的具体画面概念使用此范围）
    Content,
    /// 只搜 content_description
    Description,
    /// 只搜 file_name
    FileName,
}

/// 单一叶子条件。字段都带 type 标记，便于前端序列化与校验。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "type")]
pub enum LeafCond {
    /// 标签分面：facetKey + tagIds，mode any/all，includeDescendants。
    /// S5：termQuery/termMatch —— 显式 tagIds 与按词查可同时存在；词先按 term_match
    /// 扩展成一组 tag_id 与 tagIds 求并集，再走 compile_tag_leaf（编译层零改动）。
    /// 缺省 = Alias（旧数据向后兼容）。
    #[serde(rename_all = "camelCase")]
    Tag {
        #[serde(default)]
        facet_key: String,
        tag_ids: Vec<i64>,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default = "default_true")]
        include_descendants: bool,
        /// 【S5】按词查（AI/搜索框路径）
        #[serde(default)]
        term_query: Option<String>,
        /// 【S5】词查的匹配模式（默认 Alias）
        #[serde(default)]
        term_match: TermMatch,
    },
    /// 排除标签（含后代）
    #[serde(rename_all = "camelCase")]
    ExcludeTag {
        #[serde(default)]
        facet_key: String,
        tag_ids: Vec<i64>,
    },
    /// 素材类型：all | image | video
    AssetType { value: String },
    /// 未打标
    Untagged,
    /// 元数据比较：复用 search_query::MetadataFilter
    #[serde(rename_all = "camelCase")]
    Metadata {
        // Wire contract: `{ type: "metadata", filter: { key, op, ... } }`.
        // Keep the filter nested so the Rust response matches the TypeScript
        // LeafCond shape and the SearchPlanV3 contract.
        filter: super::search_query::MetadataFilter,
    },
    /// 关键词（FTS/LIKE）。scope 缺省 = all（旧表达式兼容）。
    Search {
        value: String,
        #[serde(default)]
        scope: SearchScope,
    },
    /// W2-7：分面有任意标签（「场景 有任意标签」—— 打标补漏核心场景）
    #[serde(rename_all = "camelCase")]
    FacetHasAny { facet_key: String },
    /// W2-7：分面没有标签（「场景 没有标签」—— 精准补漏筛选）
    #[serde(rename_all = "camelCase")]
    FacetMissing { facet_key: String },
    /// V24（§7-2）：数值分面条件 —— 对 asset_facet_numbers 的 EXISTS 编译（照
    /// compile_palette_meta 对卫星表的形状）。正向谓词，允许进 must_not（§4.1b 极性白名单）。
    #[serde(rename_all = "camelCase")]
    FacetNumber {
        facet_key: String,
        /// eq | gte | lte | gt | lt | between
        op: String,
        value: f64,
        /// between 的上界（其余 op 忽略）
        #[serde(default)]
        max_value: Option<f64>,
    },
}

fn default_true() -> bool {
    true
}

/// 布尔表达式树
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "op")]
pub enum QueryExpr {
    #[serde(rename_all = "camelCase")]
    And { children: Vec<QueryExpr> },
    #[serde(rename_all = "camelCase")]
    Or { children: Vec<QueryExpr> },
    #[serde(rename_all = "camelCase")]
    Not { child: Box<QueryExpr> },
    #[serde(rename_all = "camelCase")]
    Leaf { cond: LeafCond },
}

pub const MAX_DEPTH: usize = 5;
pub const MAX_NODES: usize = 100;

/// 校验表达式树：深度/节点上限 + 叶子合法性。
pub fn validate_expr(expr: &QueryExpr) -> AppResult<()> {
    let mut count = 0;
    validate_node(expr, 0, &mut count)?;
    Ok(())
}

fn validate_node(expr: &QueryExpr, depth: usize, count: &mut usize) -> AppResult<()> {
    if depth > MAX_DEPTH {
        return Err(AppError::msg(format!("表达式嵌套过深（上限 {MAX_DEPTH}）")));
    }
    *count += 1;
    if *count > MAX_NODES {
        return Err(AppError::msg(format!("表达式节点过多（上限 {MAX_NODES}）")));
    }
    match expr {
        QueryExpr::And { children } | QueryExpr::Or { children } => {
            if children.is_empty() {
                return Err(AppError::msg("且/或 分组不能为空"));
            }
            for c in children {
                validate_node(c, depth + 1, count)?;
            }
        }
        QueryExpr::Not { child } => validate_node(child, depth + 1, count)?,
        QueryExpr::Leaf { cond } => validate_leaf(cond)?,
    }
    Ok(())
}

fn validate_leaf(cond: &LeafCond) -> AppResult<()> {
    match cond {
        LeafCond::Tag {
            tag_ids,
            term_query,
            mode,
            ..
        } => {
            let has_term = term_query
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty());
            if tag_ids.is_empty() && !has_term {
                return Err(AppError::msg("标签条件不能为空"));
            }
            if let Some(m) = mode {
                if !matches!(m.as_str(), "any" | "all") {
                    return Err(AppError::msg(format!("非法标签 mode：{m}")));
                }
            }
            Ok(())
        }
        LeafCond::ExcludeTag { tag_ids, .. } => {
            if tag_ids.is_empty() {
                return Err(AppError::msg("排除标签条件不能为空"));
            }
            Ok(())
        }
        LeafCond::AssetType { value } => {
            if !matches!(value.as_str(), "all" | "image" | "video") {
                return Err(AppError::msg(format!("非法 assetType：{value}")));
            }
            Ok(())
        }
        LeafCond::Untagged => Ok(()),
        LeafCond::Metadata { filter } => super::search_query::compile_metadata(filter).map(|_| ()),
        LeafCond::Search { value, .. } => {
            if value.chars().count() > 200 {
                return Err(AppError::msg("搜索关键词过长"));
            }
            Ok(())
        }
        LeafCond::FacetHasAny { facet_key } | LeafCond::FacetMissing { facet_key } => {
            if facet_key.trim().is_empty() {
                return Err(AppError::msg("分面 key 不能为空"));
            }
            Ok(())
        }
        LeafCond::FacetNumber {
            facet_key,
            op,
            value,
            max_value,
        } => {
            if facet_key.trim().is_empty() {
                return Err(AppError::msg("分面 key 不能为空"));
            }
            if !matches!(op.as_str(), "eq" | "gt" | "gte" | "lt" | "lte" | "between") {
                return Err(AppError::msg(format!("非法数值分面 op：{op}")));
            }
            if !value.is_finite() {
                return Err(AppError::msg("数值分面的值必须是有限数"));
            }
            if op == "between" {
                match max_value {
                    Some(m) if m.is_finite() && *value <= *m => Ok(()),
                    Some(_) => Err(AppError::msg("介于 的上界必须是有限数且不小于下界")),
                    None => Err(AppError::msg("介于 需要上下界两个值")),
                }
            } else {
                Ok(())
            }
        }
    }
}

/// 将扁平 `AssetFilter` 拍平为一棵合规的 `QueryExpr`（多组 AND + 若干 NOT），
/// 供统一走 expr 路径或前端回填构建器使用。
pub fn from_filter(
    search: Option<&str>,
    asset_type: Option<&str>,
    untagged_only: bool,
    facet_filters: &[super::assets::FacetTagFilter],
    exclude_tag_ids: &[i64],
    metadata_filters: &[super::search_query::MetadataFilter],
) -> QueryExpr {
    let mut leaves = Vec::new();
    if let Some(s) = search {
        if !s.trim().is_empty() {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::Search {
                    value: s.to_string(),
                    scope: SearchScope::All,
                },
            });
        }
    }
    if let Some(t) = asset_type {
        if !t.is_empty() && t != "all" {
            leaves.push(QueryExpr::Leaf {
                cond: LeafCond::AssetType {
                    value: t.to_string(),
                },
            });
        }
    }
    if untagged_only {
        leaves.push(QueryExpr::Leaf {
            cond: LeafCond::Untagged,
        });
    }
    for f in facet_filters {
        if f.tag_ids.is_empty() {
            continue;
        }
        leaves.push(QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: f.facet_key.clone(),
                tag_ids: f.tag_ids.clone(),
                mode: f.mode.clone(),
                include_descendants: f.include_descendants,
            },
        });
    }
    for &tid in exclude_tag_ids {
        leaves.push(QueryExpr::Leaf {
            cond: LeafCond::ExcludeTag {
                facet_key: String::new(),
                tag_ids: vec![tid],
            },
        });
    }
    for m in metadata_filters {
        leaves.push(QueryExpr::Leaf {
            cond: LeafCond::Metadata { filter: m.clone() },
        });
    }
    if let [only] = leaves.as_slice() {
        only.clone()
    } else {
        QueryExpr::And { children: leaves }
    }
}

/// 编译叶子为可嵌入 WHERE 的片段。素材表别名为 `a`；无搜索时 conn 仅用于 FTS 谓词。
/// 返回的 SQL 使用从 ?1 起的占位符（与单独编译一致，由上层 offset）。
/// R2-1 前兼容包装：丢弃 warning 的单条件编译（新调用方请用 compile_leaf_with）。
pub fn compile_leaf(conn: &Connection, cond: &LeafCond) -> AppResult<(String, Vec<Value>)> {
    compile_leaf_with(conn, cond, &mut Vec::new())
}

/// 编译单一叶子 → (sql, params)。warning（剔除/降级）写入 `warnings`（R2-1 回传前端）。
pub fn compile_leaf_with(
    conn: &Connection,
    cond: &LeafCond,
    warnings: &mut Vec<String>,
) -> AppResult<(String, Vec<Value>)> {
    match cond {
        LeafCond::Search { value, scope } => {
            let pred = super::search::build_search_predicate(conn, value, *scope)?
                .unwrap_or_else(super::search::SearchPredicate::empty);
            let sql = if pred.sql.is_empty() {
                "1=0".to_string()
            } else {
                pred.sql
            };
            Ok((sql, pred.params))
        }
        LeafCond::AssetType { value } => {
            let sql = match value.as_str() {
                "image" => "a.mime_type LIKE 'image/%'".to_string(),
                "video" => "a.mime_type LIKE 'video/%'".to_string(),
                _ => "1=1".to_string(),
            };
            Ok((sql, Vec::new()))
        }
        LeafCond::Untagged => Ok((
            "NOT EXISTS (SELECT 1 FROM asset_tags at WHERE at.asset_id = a.id)".to_string(),
            Vec::new(),
        )),
        LeafCond::Tag {
            facet_key,
            tag_ids,
            mode,
            include_descendants,
            term_query,
            term_match,
        } => {
            // W2-6：facet 一致性校验改为「剔除 + warning，不报错整次查询」。
            // 旧语义（一个标签跨分面就拒绝整次查询）在分面删除/重建后会卡死已保存的搜索。
            let valid_ids = filter_tags_by_facet(conn, facet_key, tag_ids, warnings);
            let mut ids = valid_ids;
            // S5：termQuery 先按 term_match 扩展成一组 tag_id，与显式 tag_ids 求并集
            let has_term = term_query.as_deref().is_some_and(|s| !s.trim().is_empty());
            if has_term {
                let raw = term_query.as_deref().unwrap_or("").trim();
                let normalized = super::tags::normalize_name(raw);
                let cap = match term_match {
                    TermMatch::Prefix => super::tags::PREFIX_EXPAND_CAP,
                    TermMatch::Contains => super::tags::CONTAINS_EXPAND_CAP,
                    TermMatch::Fuzzy => super::tags::FUZZY_EXPAND_CAP,
                    _ => 1,
                };
                let (hits, warns) =
                    super::tags::expand_term_query(conn, facet_key, &normalized, *term_match, cap)?;
                for h in &hits {
                    if !ids.contains(&h.tag_id) {
                        ids.push(h.tag_id);
                    }
                }
                for w in &warns {
                    tracing::warn!("词查「{raw}」({term_match:?}): {w}");
                    warnings.push(format!("词查「{raw}」：{w}"));
                }
                if ids.is_empty() {
                    // 词查明确但一个都没命中 → 条件不可满足（0 结果），
                    // 区别于「空 tag_ids = 无约束恒真」——搜索框零结果由此给出。
                    // S5 5-4（不变量 11）：**不改写条件**，只给「试试相近的词」建议——
                    // 用户点了才改（前端零结果建议）。cap=5 与 fuzzy 上限一致。
                    let (fuzzy_hits, _) = super::tags::expand_term_query(
                        conn,
                        facet_key,
                        &normalized,
                        TermMatch::Fuzzy,
                        super::tags::FUZZY_EXPAND_CAP,
                    )?;
                    if !fuzzy_hits.is_empty() {
                        let names: Vec<&str> =
                            fuzzy_hits.iter().map(|h| h.matched_term.as_str()).collect();
                        warnings.push(format!(
                            "词查「{raw}」没有命中。试试相近的词：{}",
                            names.join("、")
                        ));
                    }
                    return Ok(("1=0".to_string(), Vec::new()));
                }
            } else if ids.is_empty() {
                return Ok(("1=1".to_string(), Vec::new()));
            }
            compile_tag_leaf(&ids, mode.as_deref(), *include_descendants)
        }
        LeafCond::FacetHasAny { facet_key } => {
            if !facet_searchable(conn, facet_key) {
                tracing::warn!(
                    "分面 {facet_key} 不存在或 cfg_searchable=0，剔除该条件（查询继续）"
                );
                warnings.push(format!("分面「{facet_key}」已停用或不存在，已忽略该条件。"));
                return Ok(("1=1".to_string(), Vec::new()));
            }
            Ok((
                "EXISTS (SELECT 1 FROM asset_tags at2 JOIN tags t2 ON t2.id = at2.tag_id
                  WHERE at2.asset_id = a.id AND t2.facet_key = ?1 AND t2.status = 'active')"
                    .to_string(),
                vec![Value::Text(facet_key.clone())],
            ))
        }
        LeafCond::FacetMissing { facet_key } => {
            if !facet_searchable(conn, facet_key) {
                tracing::warn!(
                    "分面 {facet_key} 不存在或 cfg_searchable=0，剔除该条件（查询继续）"
                );
                warnings.push(format!("分面「{facet_key}」已停用或不存在，已忽略该条件。"));
                return Ok(("1=1".to_string(), Vec::new()));
            }
            Ok((
                "NOT EXISTS (SELECT 1 FROM asset_tags at3 JOIN tags t3 ON t3.id = at3.tag_id
                  WHERE at3.asset_id = a.id AND t3.facet_key = ?1 AND t3.status = 'active')"
                    .to_string(),
                vec![Value::Text(facet_key.clone())],
            ))
        }
        LeafCond::ExcludeTag { tag_ids, .. } => {
            let mut sql = String::new();
            let mut params: Vec<Value> = Vec::new();
            for &tid in tag_ids {
                params.push(tid.into());
                // F1-d：后代递归 CTE 加 d < 12 上限
                sql.push_str(&format!(
                    " AND NOT EXISTS (SELECT 1 FROM asset_tags ate WHERE ate.asset_id=a.id AND ate.tag_id IN (
                        WITH RECURSIVE sub(id, d) AS (SELECT ?{}, 0 UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                        SELECT id FROM sub))", params.len()
                ));
            }
            Ok((sql.trim_start_matches(" AND ").to_string(), params))
        }
        LeafCond::FacetNumber {
            facet_key,
            op,
            value,
            max_value,
        } => {
            // §7-2：分面必须存在且 facet_kind='number'，否则剔除（§4.2，与 FacetHasAny 同策略：
            // 分面删除/改型后已保存的搜索不应被卡死）。prune_invalid 探测 1=1 折叠自动接链。
            let kind: Option<String> = conn
                .query_row(
                    "SELECT facet_kind FROM tag_facets WHERE key = ?1",
                    [facet_key],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if kind.as_deref() != Some("number") {
                tracing::warn!("数值分面 {facet_key} 不存在或不是数值型，剔除该条件（查询继续）");
                warnings.push(format!(
                    "数值分面「{facet_key}」不存在或不是数值型，已忽略该条件。"
                ));
                return Ok(("1=1".to_string(), Vec::new()));
            }
            let mut params: Vec<Value> = vec![Value::Text(facet_key.clone())];
            let mut sql =
                "EXISTS (SELECT 1 FROM asset_facet_numbers afn WHERE afn.asset_id = a.id AND afn.facet_key = ?1"
                    .to_string();
            match op.as_str() {
                "eq" => {
                    params.push((*value).into());
                    sql.push_str(" AND ABS(afn.value - ?2) < 1e-9");
                }
                "gt" | "gte" => {
                    params.push((*value).into());
                    let cmp = if op == "gt" { ">" } else { ">=" };
                    sql.push_str(&format!(" AND afn.value {cmp} ?2"));
                }
                "lt" | "lte" => {
                    params.push((*value).into());
                    let cmp = if op == "lt" { "<" } else { "<=" };
                    sql.push_str(&format!(" AND afn.value {cmp} ?2"));
                }
                "between" => {
                    params.push((*value).into());
                    params.push((max_value.unwrap_or(*value)).into());
                    sql.push_str(" AND afn.value >= ?2 AND afn.value <= ?3");
                }
                _ => unreachable!("validate_leaf 已拒绝非法 op"),
            }
            sql.push(')');
            Ok((sql, params))
        }
        LeafCond::Metadata { filter } => {
            // R2-2：量纲人话（expr 路径与扁平路径同规则；不阻断执行）
            for w in super::search_query::dimension_warnings(filter) {
                warnings.push(w);
            }
            let compiled = super::search_query::compile_metadata(filter)?;
            match compiled {
                Some(c) => Ok((c.sql, c.params)),
                None => Ok(("1=1".to_string(), Vec::new())),
            }
        }
    }
}

/// W2-6：Tag 叶子的 facet 一致性过滤 —— 不属于声明 facet_key 的 tag_id 直接剔除并 warning，
/// 不再报错整次查询（分面删除/重建后，已保存的搜索不应被一个失效 tag_id 卡死）。
/// 全部剔除时返回空 Vec，compile_leaf 侧折叠为 1=1（条件恒真，查询继续）。
fn filter_tags_by_facet(
    conn: &Connection,
    facet_key: &str,
    tag_ids: &[i64],
    warnings: &mut Vec<String>,
) -> Vec<i64> {
    if facet_key.is_empty() {
        return tag_ids.to_vec();
    }
    let placeholders = tag_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    if placeholders.is_empty() {
        return Vec::new();
    }
    let mut vals: Vec<Value> = vec![Value::Text(facet_key.to_string())];
    for &t in tag_ids {
        vals.push(Value::Integer(t));
    }
    let mut valid: Vec<i64> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT id FROM tags WHERE facet_key = ? AND id IN ({placeholders})"
    )) {
        if let Ok(rows) = stmt.query_map(params_from_iter(vals), |r| r.get::<_, i64>(0)) {
            valid.extend(rows.filter_map(|r| r.ok()));
        }
    }
    let dropped = tag_ids.len() - valid.len();
    if dropped > 0 {
        tracing::warn!("剔除 {dropped} 个不属于分面 {facet_key} 的标签（查询继续）");
        warnings.push(format!(
            "有 {dropped} 个标签不属于分面「{facet_key}」，已剔除。"
        ));
    }
    valid
}

/// W2-7 + F4：FacetHasAny / FacetMissing 的分面可搜性校验（编译期剔除，区别于 Tag 的剔除策略）。
/// 分面不存在或 cfg_searchable=0 时 warning + 整叶剔除（1=1），不再报错整次查询
/// （与 W2-6 一致：分面删除/重建后已保存的搜索不应被卡死）。
fn facet_searchable(conn: &Connection, facet_key: &str) -> bool {
    let searchable: Option<i64> = conn
        .query_row(
            &format!("SELECT 1 FROM tag_facets f WHERE f.key = ?1 AND {EFF_SEARCH}"),
            [facet_key],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    searchable.is_some()
}

/// 标签叶子（含后代/any-all），复用 build_where 的 EXISTS 形态。
fn compile_tag_leaf(
    tag_ids: &[i64],
    mode: Option<&str>,
    include_descendants: bool,
) -> AppResult<(String, Vec<Value>)> {
    let all_mode = mode == Some("all");
    let mut params: Vec<Value> = Vec::new();
    if all_mode {
        // all：每个标签各一条 EXISTS，AND 连接（防 JOIN 行数爆炸）
        let mut ands = String::new();
        for &tid in tag_ids {
            params.push(tid.into());
            if !ands.is_empty() {
                ands.push_str(" AND ");
            }
            if include_descendants {
                ands.push_str(&format!(
                    "EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id IN (
                        WITH RECURSIVE sub(id, d) AS (SELECT ?{} , 0 UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                        SELECT id FROM sub))", params.len()
                ));
            } else {
                ands.push_str(&format!(
                    "EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id=?{})",
                    params.len()
                ));
            }
        }
        Ok((ands, params))
    } else if include_descendants {
        // any + 后代：多 seed 合一条 EXISTS
        let mut seeds = String::new();
        for &tid in tag_ids {
            params.push(tid.into());
            if !seeds.is_empty() {
                seeds.push_str(" UNION ALL");
            }
            seeds.push_str(&format!(" SELECT ?{}, 0", params.len()));
        }
        Ok((
            format!(
                "EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id IN (
                    WITH RECURSIVE sub(id, d) AS ({seeds} UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                    SELECT id FROM sub))"
            ),
            params,
        ))
    } else {
        // any + 不含后代：tag_id IN (直接 id)
        let placeholders = tag_ids
            .iter()
            .map(|tid| {
                params.push((*tid).into());
                format!("?{}", params.len())
            })
            .collect::<Vec<_>>()
            .join(",");
        Ok((
            format!(
                "EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id IN ({placeholders}))"
            ),
            params,
        ))
    }
}

/// FB5-05（§9.5.6）：统一归一化——空组删除、单子节点组折叠、连续相同 AND/OR 扁平化、
/// 重复 leaf 去重。返回 None 表示整树无有效条件（调用方置 expr=None，不创建空 AND）。
pub fn normalize_expr(expr: QueryExpr) -> Option<QueryExpr> {
    match expr {
        QueryExpr::Leaf { cond } => Some(QueryExpr::Leaf { cond }),
        QueryExpr::And { children } => normalize_group(children, true),
        QueryExpr::Or { children } => normalize_group(children, false),
        QueryExpr::Not { child } => {
            normalize_expr(*child).map(|c| QueryExpr::Not { child: Box::new(c) })
        }
    }
}

fn normalize_group(children: Vec<QueryExpr>, is_and: bool) -> Option<QueryExpr> {
    let mut out: Vec<QueryExpr> = Vec::new();
    for c in children {
        let Some(norm) = normalize_expr(c) else {
            continue; // 空组删除
        };
        match norm {
            // 连续相同节点扁平化
            QueryExpr::And { children: subs } if is_and => out.extend(subs),
            QueryExpr::Or { children: subs } if !is_and => out.extend(subs),
            other => out.push(other),
        }
    }
    // 重复 leaf 去重（序列化比较，稳定）
    let mut seen = std::collections::HashSet::new();
    out.retain(|e| {
        let key = serde_json::to_string(e).unwrap_or_default();
        seen.insert(key)
    });
    match out.len() {
        0 => None,
        1 => out.into_iter().next(),
        _ => Some(if is_and {
            QueryExpr::And { children: out }
        } else {
            QueryExpr::Or { children: out }
        }),
    }
}

/// R2-1 前兼容包装：丢弃 warning 的表达式编译（新调用方请用 compile_expr_with）。
pub fn compile_expr(conn: &Connection, expr: &QueryExpr) -> AppResult<(String, Vec<Value>)> {
    compile_expr_with(conn, expr, &mut Vec::new())
}

/// 递归编译表达式树 → 可嵌入 WHERE 的片段（同样从 ?1 起占位，供上层 offset）。
/// warning（剔除/降级）写入 `warnings`，回传前端（R2-1）。
pub fn compile_expr_with(
    conn: &Connection,
    expr: &QueryExpr,
    warnings: &mut Vec<String>,
) -> AppResult<(String, Vec<Value>)> {
    match expr {
        QueryExpr::Leaf { cond } => compile_leaf_with(conn, cond, warnings),
        QueryExpr::And { children } => compile_group_with(conn, children, "AND", warnings),
        QueryExpr::Or { children } => compile_group_with(conn, children, "OR", warnings),
        QueryExpr::Not { child } => {
            let (sql, params) = compile_expr_with(conn, child, warnings)?;
            Ok((format!("NOT ({sql})"), params))
        }
    }
}

fn compile_group_with(
    conn: &Connection,
    children: &[QueryExpr],
    joiner: &str,
    warnings: &mut Vec<String>,
) -> AppResult<(String, Vec<Value>)> {
    let mut parts = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    for c in children {
        let (sql, p) = compile_expr_with(conn, c, warnings)?;
        let shifted = offset_placeholders(&sql, params.len());
        parts.push(format!("({shifted})"));
        params.extend(p);
    }
    let combined = parts.join(&format!(" {joiner} "));
    Ok((combined, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn validates_nested_and_or_not() {
        let expr = QueryExpr::And {
            children: vec![
                QueryExpr::Leaf {
                    cond: LeafCond::Untagged,
                },
                QueryExpr::Or {
                    children: vec![
                        QueryExpr::Leaf {
                            cond: LeafCond::AssetType {
                                value: "image".into(),
                            },
                        },
                        QueryExpr::Not {
                            child: Box::new(QueryExpr::Leaf {
                                cond: LeafCond::ExcludeTag {
                                    facet_key: String::new(),
                                    tag_ids: vec![9],
                                },
                            }),
                        },
                    ],
                },
            ],
        };
        assert!(validate_expr(&expr).is_ok());
    }

    #[test]
    fn rejects_empty_group() {
        let expr = QueryExpr::Or { children: vec![] };
        assert!(validate_expr(&expr).is_err());
    }

    #[test]
    fn rejects_bad_asset_type() {
        let expr = QueryExpr::Leaf {
            cond: LeafCond::AssetType {
                value: "banana".into(),
            },
        };
        assert!(validate_expr(&expr).is_err());
    }

    #[test]
    fn accepts_term_only_tag_condition() {
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Tag {
                facet_key: "scene".into(),
                tag_ids: vec![],
                mode: Some("any".into()),
                include_descendants: true,
                term_query: Some("海边".into()),
                term_match: Default::default(),
            },
        };
        assert!(validate_expr(&expr).is_ok());
    }

    #[test]
    fn rejects_empty_tag_condition_without_term() {
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Tag {
                facet_key: "scene".into(),
                tag_ids: vec![],
                mode: Some("any".into()),
                include_descendants: true,
                term_query: Some("  ".into()),
                term_match: Default::default(),
            },
        };
        assert!(validate_expr(&expr).is_err());
    }

    #[test]
    fn metadata_wire_shape_is_nested_and_round_trips() {
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Metadata {
                filter: crate::db::search_query::MetadataFilter {
                    key: "file_size".into(),
                    op: "gte".into(),
                    value: Some(serde_json::json!(5 * 1024 * 1024)),
                    values: None,
                    min: None,
                    max: None,
                },
            },
        };
        let wire = serde_json::to_value(&expr).unwrap();
        assert_eq!(wire["cond"]["type"], "metadata");
        assert_eq!(wire["cond"]["filter"]["key"], "file_size");
        assert!(wire["cond"].get("key").is_none());
        let decoded: QueryExpr = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    }

    #[test]
    fn rejects_bad_metadata_key() {
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Metadata {
                filter: crate::db::search_query::MetadataFilter {
                    key: "nope".into(),
                    op: "eq".into(),
                    value: Some(serde_json::json!("x")),
                    values: None,
                    min: None,
                    max: None,
                },
            },
        };
        assert!(validate_expr(&expr).is_err());
    }

    #[test]
    fn rejects_too_deep() {
        let mut expr = QueryExpr::Leaf {
            cond: LeafCond::Untagged,
        };
        for _ in 0..8 {
            expr = QueryExpr::Not {
                child: Box::new(expr),
            };
        }
        assert!(validate_expr(&expr).is_err());
    }

    #[test]
    fn compiles_flat_expr_and_shifts_params() {
        let conn = init_memory().unwrap();
        let expr = QueryExpr::And {
            children: vec![
                QueryExpr::Leaf {
                    cond: LeafCond::Tag {
                        term_query: None,
                        term_match: Default::default(),
                        facet_key: "scene".into(),
                        tag_ids: vec![1, 2],
                        mode: Some("any".into()),
                        include_descendants: true,
                    },
                },
                QueryExpr::Leaf {
                    cond: LeafCond::Metadata {
                        filter: crate::db::search_query::MetadataFilter {
                            key: "file_size".into(),
                            op: "gte".into(),
                            value: Some(serde_json::json!(5242880)),
                            values: None,
                            min: None,
                            max: None,
                        },
                    },
                },
            ],
        };
        // W2-6：tag_ids 需真实存在于声明分面（不存在会被剔除+折叠 1=1）——先造标签
        conn.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('海边', '海边', '海边', 'scene', 0, 'active', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('日落', '日落', '日落', 'scene', 0, 'active', 1)",
            [],
        )
        .unwrap();
        let (sql, params) = compile_expr(&conn, &expr).unwrap();
        assert!(sql.contains("EXISTS"));
        assert!(sql.contains(">="));
        assert!(params.len() >= 3);
        // 两次编译结果一致（参数偏移稳定）
        let (sql2, params2) = compile_expr(&conn, &expr).unwrap();
        assert_eq!(sql, sql2);
        assert_eq!(params.len(), params2.len());
    }

    #[test]
    fn unknown_facet_dropped_not_error_not_silent() {
        // W2-6：未知 facet（或标签不属于声明分面）→ 剔除 + warning，查询继续（1=1），
        // 不再报错整次查询（旧行为会卡死分面删除后已保存的搜索）
        let conn = init_memory().unwrap();
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: "nonexistent_facet".into(),
                tag_ids: vec![1],
                mode: Some("any".into()),
                include_descendants: true,
            },
        };
        let (sql, params) = compile_expr(&conn, &expr).unwrap();
        assert_eq!(sql, "1=1", "剔除后折叠为恒真");
        assert!(params.is_empty());
    }

    #[test]
    fn tag_belonging_to_wrong_facet_is_dropped() {
        let conn = init_memory().unwrap();
        // 造一个在 scene 分面下的标签
        conn.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('海边', '海边', '海边', 'scene', 0, 'active', 0)",
            [],
        )
        .unwrap();
        // W2-6：声明为 color 分面但 tag 属于 scene → 剔除该 tag（warning），折叠 1=1
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: "color".into(),
                tag_ids: vec![1],
                mode: Some("any".into()),
                include_descendants: false,
            },
        };
        let (sql, params) = compile_expr(&conn, &expr).unwrap();
        assert_eq!(sql, "1=1", "剔除后折叠为恒真");
        assert!(params.is_empty());
    }

    #[test]
    fn accepts_tag_belonging_to_declared_facet() {
        let conn = init_memory().unwrap();
        conn.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('海边', '海边', '海边', 'scene', 0, 'active', 0)",
            [],
        )
        .unwrap();
        let expr = QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: "scene".into(),
                tag_ids: vec![1],
                mode: Some("any".into()),
                include_descendants: false,
            },
        };
        let (sql, _) = compile_expr(&conn, &expr).unwrap();
        assert!(sql.contains("EXISTS"));
    }

    #[test]
    fn from_filter_builds_and_root() {
        let expr = from_filter(
            Some("海边"),
            Some("image"),
            false,
            &[crate::db::assets::FacetTagFilter {
                facet_key: "scene".into(),
                tag_ids: vec![8],
                mode: Some("any".into()),
                include_descendants: true,
            }],
            &[44],
            &[],
        );
        match expr {
            QueryExpr::And { children } => assert!(children.len() >= 4),
            _ => panic!("应组装为 AND"),
        }
    }

    /// S5 5-5：词查零命中 → 条件保持原样（1=0 由原 leaf 产生，不改写 termQuery），
    /// 且 warning 给出「试试相近的词」可点建议（Fuzzy 扩展词列表）。不变量 11：
    /// 系统可以建议，但不能悄悄改用户的语义。
    #[test]
    fn zero_result_suggests_fuzzy_without_rewriting() {
        use crate::db::migrations;
        use crate::db::schema_features;
        use crate::db::tags;
        let c = init_memory().unwrap();
        migrations::apply_v22b_constraints(&c).unwrap();
        schema_features::set_feature(&c, "tag_unique_terms", true, None).unwrap();
        let t = tags::create_in_facet(&c, "森林", None, Some("scene")).unwrap();
        assert!(t.id > 0);

        // 错别字「森材」：Alias 精确零命中
        let leaf = LeafCond::Tag {
            facet_key: "scene".into(),
            tag_ids: vec![],
            mode: Some("any".into()),
            include_descendants: true,
            term_query: Some("森材".into()),
            term_match: tags::TermMatch::Alias,
        };
        let mut warnings = Vec::new();
        let (sql, _) = compile_leaf_with(&c, &leaf, &mut warnings).unwrap();
        assert_eq!(sql, "1=0", "零命中 → 不可满足（搜索框零结果归因于此）");
        // 条件本身未被改写
        match &leaf {
            LeafCond::Tag {
                term_query,
                term_match,
                ..
            } => {
                assert_eq!(term_query.as_deref(), Some("森材"), "termQuery 不得被改写");
                assert_eq!(*term_match, tags::TermMatch::Alias, "termMatch 不得被改写");
            }
            _ => unreachable!(),
        }
        // 有可点建议：相近词命中「森林」
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("试试相近的词") && w.contains("森林")),
            "零命中应建议相近词「森林」: {warnings:?}"
        );

        // 对照：完全无相近词时不给建议（避免空建议文案）
        let leaf2 = LeafCond::Tag {
            facet_key: "scene".into(),
            tag_ids: vec![],
            mode: Some("any".into()),
            include_descendants: true,
            term_query: Some("不存在的词".into()),
            term_match: tags::TermMatch::Alias,
        };
        let mut w2 = Vec::new();
        let (sql2, _) = compile_leaf_with(&c, &leaf2, &mut w2).unwrap();
        assert_eq!(sql2, "1=0");
        assert!(
            !w2.iter().any(|w| w.contains("试试相近的词")),
            "无相近词时不得出现空建议: {w2:?}"
        );
    }
}
