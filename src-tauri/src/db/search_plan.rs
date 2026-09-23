//! S1/S2/S4/S6：SearchPlanV3 —— 硬性过滤 + 硬性排除 + 加权可选（should）+ 版本号 + 多路召回。
//!
//! SQL 编译形态（每个 should 只求值一次，两层派生表）：
//! ```sql
//! SELECT q.id, q.score, q.hits FROM (
//!   SELECT a.id, m1, m2, ...,
//!          (m1*w1 + m2*w2 + ...) AS score,
//!          (m1 + m2 + ...)       AS hits
//!     FROM assets a
//!    WHERE <filter> AND NOT (<must_not>) AND a.deleted_at IS NULL
//! ) q WHERE q.hits >= :min_should
//! ORDER BY score DESC, id DESC
//! ```
//! should 片段与 filter 走**同一套** compile_leaf/compile_expr（零新编译逻辑，
//! 白名单/参数绑定安全自动继承）；权重只影响 score（排序），不影响命中集合。
//!
//! S6 三个版本号单点声明在本模块；plan_schema_version 变更必须写迁移函数。

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::query_expr::{compile_expr_with, compile_leaf_with, validate_expr, LeafCond, QueryExpr};
use super::search_query::ALL_SORT_KEYS;
use super::sql_utils::offset_placeholders;
use crate::error::{AppError, AppResult};

/// S6：plan 结构版本（加/删/改字段语义时 +1，须写 migrate 函数）。
pub const PLAN_SCHEMA_VERSION: u32 = 3;
/// S6：归一化规则版本（规则变了 → 旧 plan 读到先重归一化一次）。
pub const NORMALIZATION_VERSION: u32 = 1;
/// S6：SQL 生成方式版本（每次现算，不持久化，只用于日志排查）。
pub const COMPILER_VERSION: u32 = 1;

fn default_plan_schema_version() -> u32 {
    PLAN_SCHEMA_VERSION
}
fn default_normalization_version() -> u32 {
    NORMALIZATION_VERSION
}
fn default_compiler_version() -> u32 {
    COMPILER_VERSION
}
fn default_weight() -> f32 {
    1.0
}

/// S1：加权可选子句（满足则加分，不满足不淘汰）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShouldClause {
    /// 与 filter 完全同一套叶子条件
    pub cond: LeafCond,
    /// 默认 1.0；UI 只给三档 0.5 / 1.0 / 2.0
    #[serde(default = "default_weight")]
    pub weight: f32,
    /// 给用户看（「蓝天（加分项）」）
    #[serde(default)]
    pub label: String,
    /// §3.5/§4.8：AI 证据原文回显（「最好是户外」）；手工添加的条件为空
    #[serde(default)]
    pub evidence: Option<String>,
}

/// S2：多路召回融合方式。RRF 只用排名（bm25 负值 / should 加分任意正数 不可比，
/// 排名可比）—— 默认；Linear 各路 min-max 归一到 [0,1] 后加权（可选项）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Fusion {
    #[default]
    Rrf,
    Linear,
}

/// S2：排序方式。用户显式选字段时用字段；否则相关度（should 加权 + 检索器融合）。
/// B10：`Ranking::Relevance` 是无载荷变体 —— `SearchPlanV3.retrievers` 是唯一来源，
/// 杜绝「数据写进一个字段、执行读另一个字段」的双源 bug。
/// serde tag="type"：线上形状与前端一致（{"type":"field",...} / {"type":"relevance"}）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Ranking {
    Field {
        key: String,
        dir: String,
    },
    /// 相关度排序（should 加权 + plan.retrievers 各路 RRF 融合）。本变体存在 = 走相关度。
    Relevance,
}

/// S4：多路召回计划（S2 默认走 RRF；本波次先用 should 加权一路，Fts/TagAlias S4 填）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrieverPlan {
    #[serde(default)]
    pub retrievers: Vec<WeightedRetriever>,
    #[serde(default)]
    pub fusion: Fusion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeightedRetriever {
    pub weight: f32,
    pub kind: Retriever,
}

/// S4：检索器（Fts / TagAlias）。明确不做向量（不建 asset_embeddings、不写 Vector 分支）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "type")]
pub enum Retriever {
    Fts {
        query: String,
        scope: super::query_expr::SearchScope,
    },
    TagAlias {
        text: String,
        #[serde(default)]
        facet_key: Option<String>,
    },
}

/// S1：完整搜索计划（超级搜索持久化/执行的单一结构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlanV3 {
    #[serde(default = "default_plan_schema_version")]
    pub plan_schema_version: u32,
    #[serde(default = "default_normalization_version")]
    pub normalization_version: u32,
    #[serde(default = "default_compiler_version")]
    pub compiler_version: u32,

    /// 硬性必须满足（沿用现有 QueryExpr，零改动复用全部编译逻辑）
    #[serde(default)]
    pub filter: Option<QueryExpr>,
    /// 硬性排除
    #[serde(default)]
    pub must_not: Option<QueryExpr>,
    /// 加权可选：满足则加分，不满足不淘汰
    #[serde(default)]
    pub should: Vec<ShouldClause>,
    /// 至少命中几条 should 才进结果。
    /// B6（方案 A）：filter 为空 + should 非空 + min=0 = 允许全库软排序（仅调顺序，不淘汰）；
    /// min>0 时未命中 N 条加分项的素材不显示。
    #[serde(default)]
    pub minimum_should_match: u32,
    #[serde(default)]
    pub retrievers: RetrieverPlan,
    pub ranking: Ranking,
}

/// should 条数上限（S1 validate：≤ 12 条）。
pub const MAX_SHOULD_CLAUSES: usize = 12;
/// §4.3：UI 只给三档权重（轻微 0.5 / 一般 1.0 / 强烈 2.0）。
pub const ALLOWED_SHOULD_WEIGHTS: [f32; 3] = [0.5, 1.0, 2.0];

/// B7/B8：执行链 warning。后端产出的永远是 source="plan"（"ai" 由前端 AI 解析链标注）。
/// zone 标识被剔除条件所在区（filter/mustNot/should），渲染层带区名前缀。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchWarning {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    pub message: String,
}

impl SearchWarning {
    pub fn plan(zone: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            source: "plan".into(),
            zone: zone.map(String::from),
            message: message.into(),
        }
    }
}

impl SearchPlanV3 {
    pub fn field_ranking(key: &str, dir: &str) -> SearchPlanV3 {
        SearchPlanV3 {
            ranking: Ranking::Field {
                key: key.into(),
                dir: dir.into(),
            },
            ..Default::default()
        }
    }
}

impl Default for SearchPlanV3 {
    fn default() -> Self {
        Self {
            plan_schema_version: PLAN_SCHEMA_VERSION,
            normalization_version: NORMALIZATION_VERSION,
            compiler_version: COMPILER_VERSION,
            filter: None,
            must_not: None,
            should: Vec::new(),
            minimum_should_match: 0,
            retrievers: RetrieverPlan::default(),
            ranking: Ranking::Field {
                key: "created_at".into(),
                dir: "desc".into(),
            },
        }
    }
}

/// §4.1b：must_not 极性白名单 —— `ExcludeTag` 本身已编译成 NOT EXISTS（排除动作），
/// 放进 must_not 会再被统一包一层 NOT 造成双重否定（B11 P0）。
fn leaf_allowed_in_must_not(cond: &LeafCond) -> bool {
    !matches!(cond, LeafCond::ExcludeTag { .. })
}

/// §4.1b：must_not 树内禁止 `QueryExpr::Not`（三重否定不可读）与 `ExcludeTag` 叶子。
fn validate_must_not_tree(expr: &QueryExpr) -> AppResult<()> {
    match expr {
        QueryExpr::Leaf { cond } => {
            if !leaf_allowed_in_must_not(cond) {
                return Err(AppError::msg(
                    "排除区只放正向条件，不得放入『排除标签』（会造成双重否定）",
                ));
            }
            Ok(())
        }
        QueryExpr::Not { .. } => Err(AppError::msg("排除区不支持否定节点（会造成三重否定）")),
        QueryExpr::And { children } | QueryExpr::Or { children } => {
            for c in children {
                validate_must_not_tree(c)?;
            }
            Ok(())
        }
    }
}

/// 校验计划：should ≤ 12；filter/must_not 走现有 validate_expr；
/// Field 排序 key 必须在 ALL_SORT_KEYS；must_not 极性白名单（§4.1b）；
/// §4.3：weight ∈ {0.5,1.0,2.0} 且有限、min ≤ should.len、dir ∈ {asc,desc}、
/// retriever weight 有限且 > 0、读到未来版本号 → 拒绝（§4.4）。
pub fn validate_search_plan(plan: &SearchPlanV3) -> AppResult<()> {
    if plan.plan_schema_version > PLAN_SCHEMA_VERSION {
        return Err(AppError::msg(
            "保存的搜索条件来自更新版本的应用，已无法执行（请重置搜索条件）",
        ));
    }
    if plan.normalization_version > NORMALIZATION_VERSION {
        return Err(AppError::msg(
            "保存的搜索条件使用了更新版本的归一化规则，已无法执行",
        ));
    }
    if plan.should.len() > MAX_SHOULD_CLAUSES {
        return Err(AppError::msg(format!(
            "加分条件过多（上限 {MAX_SHOULD_CLAUSES} 条）"
        )));
    }
    if plan.should.is_empty() && plan.minimum_should_match > 0 {
        return Err(AppError::msg("没有加分条件时不得要求最低命中数"));
    }
    if plan.minimum_should_match > plan.should.len() as u32 {
        return Err(AppError::msg(
            "『至少满足 N 项』超出加分条件条数（N ≤ 加分条数）",
        ));
    }
    for sc in &plan.should {
        if !sc.weight.is_finite() || !ALLOWED_SHOULD_WEIGHTS.contains(&sc.weight) {
            return Err(AppError::msg(format!(
                "非法加分权重：{}（只允许 0.5 / 1.0 / 2.0）",
                sc.weight
            )));
        }
        validate_expr(&QueryExpr::Leaf {
            cond: sc.cond.clone(),
        })?;
    }
    for wr in &plan.retrievers.retrievers {
        if !wr.weight.is_finite() || wr.weight <= 0.0 {
            return Err(AppError::msg("检索器权重必须为正的有限数"));
        }
    }
    if let Some(f) = &plan.filter {
        validate_expr(f)?;
    }
    if let Some(m) = &plan.must_not {
        validate_expr(m)?;
        validate_must_not_tree(m)?;
    }
    if let Ranking::Field { key, dir } = &plan.ranking {
        if !ALL_SORT_KEYS.contains(&key.as_str()) {
            return Err(AppError::msg(format!("非法排序字段：{key}")));
        }
        if !matches!(dir.as_str(), "asc" | "desc") {
            return Err(AppError::msg(format!("非法排序方向：{dir}")));
        }
    }
    Ok(())
}

/// 排序字段 → 资产列表达式（白名单映射，绝不来自外部字符串）。
fn sort_column_sql(key: &str) -> Option<String> {
    let col = match key {
        "created_at" | "taken_at" | "modified_at" => format!("a.{key}"),
        "name" => "a.file_name".to_string(),
        "size" => "a.file_size".to_string(),
        "resolution" => "(a.width * a.height)".to_string(),
        "rating" => "a.rating".to_string(),
        _ => return None,
    };
    Some(col)
}

/// 编译结果：完整 SQL + 顺序参数（?1..?N，可直接 bind）。
#[derive(Debug)]
pub struct CompiledPlan {
    pub sql: String,
    pub params: Vec<Value>,
}

/// 前兼容包装：丢弃编译层 warning（执行链请用 compile_search_plan_with，
/// 否则 compile 剔除/降级信息会丢——正是 §1.4 断链③的教训）。
pub fn compile_search_plan(conn: &Connection, plan: &SearchPlanV3) -> AppResult<CompiledPlan> {
    compile_search_plan_with(conn, plan, &mut Vec::new())
}

/// 编译 SearchPlanV3 → 两层派生表 SQL（每个 should 只求值一次）。
/// 排序：Relevance → score DESC, id DESC；Field → sortv {dir}, score DESC, id DESC（B9）。
/// compile 层剔除/降级 warning（W2-6 分面不一致、维度人话等）按区名写入 `warnings`。
/// 调用方必须先 validate + prune（§4.1：校验在剔除之前）。
pub fn compile_search_plan_with(
    conn: &Connection,
    plan: &SearchPlanV3,
    warnings: &mut Vec<SearchWarning>,
) -> AppResult<CompiledPlan> {
    validate_search_plan(plan)?;
    // B6（方案 A）：允许真正的「全库软排序」—— filter 为空 + should 非空 + min=0 时
    // 不再强制 ≥1，完全不命中加分项的素材也保留（仅调整顺序）。UI 明示「显示全部 N 张，仅调整顺序」。
    let min_should = plan.minimum_should_match as i64;
    // 1) 基础 WHERE：filter AND NOT(must_not) AND deleted_at IS NULL
    let mut base_sql: String;
    let mut params: Vec<Value> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut zone_sink: Vec<String> = Vec::new();
    if let Some(f) = &plan.filter {
        zone_sink.clear();
        let (sql, p) = compile_expr_with(conn, f, &mut zone_sink)?;
        if !sql.is_empty() {
            parts.push(format!("({})", offset_placeholders(&sql, params.len())));
            params.extend(p);
        }
        warnings.extend(
            zone_sink
                .drain(..)
                .map(|w| SearchWarning::plan(Some("filter"), w)),
        );
    }
    if let Some(m) = &plan.must_not {
        zone_sink.clear();
        let (sql, p) = compile_expr_with(conn, m, &mut zone_sink)?;
        if !sql.is_empty() {
            parts.push(format!("NOT ({})", offset_placeholders(&sql, params.len())));
            params.extend(p);
        }
        warnings.extend(
            zone_sink
                .drain(..)
                .map(|w| SearchWarning::plan(Some("mustNot"), w)),
        );
    }
    parts.push("a.deleted_at IS NULL".to_string());
    base_sql = parts.join(" AND ");
    if base_sql.is_empty() {
        base_sql = "1=1".to_string();
    }

    // 2) should 标记：每个片段只出现一次（CASE WHEN）
    let mut marker_sql = String::new();
    let mut score_terms: Vec<String> = Vec::new();
    let mut hits_terms: Vec<String> = Vec::new();
    for (i, sc) in plan.should.iter().enumerate() {
        zone_sink.clear();
        let (frag, p) = compile_leaf_with(conn, &sc.cond, &mut zone_sink)?;
        let shifted = offset_placeholders(&frag, params.len());
        let m = format!("m{}", i + 1);
        if !marker_sql.is_empty() {
            marker_sql.push_str(", ");
        }
        marker_sql.push_str(&format!("CASE WHEN ({shifted}) THEN 1 ELSE 0 END AS {m}"));
        params.extend(p);
        score_terms.push(format!("{m} * {w}", w = fmt_weight(sc.weight)));
        hits_terms.push(m);
        warnings.extend(
            zone_sink
                .drain(..)
                .map(|w| SearchWarning::plan(Some("should"), w)),
        );
    }
    let score_expr = if score_terms.is_empty() {
        "0.0".to_string()
    } else {
        score_terms.join(" + ")
    };
    let hits_expr = if hits_terms.is_empty() {
        "0".to_string()
    } else {
        hits_terms.join(" + ")
    };

    // 3) 排序：Field → 额外算 sortv（内层）；Relevance → score DESC, id DESC
    let (order_sql, sortv_keep, sortv_inner) = match &plan.ranking {
        Ranking::Relevance => (
            "score DESC, id DESC".to_string(),
            String::new(),
            String::new(),
        ),
        Ranking::Field { key, dir } => {
            let col = sort_column_sql(key)
                .ok_or_else(|| AppError::msg(format!("非法排序字段：{key}")))?;
            let dir = if dir.eq_ignore_ascii_case("asc") {
                "ASC"
            } else {
                "DESC"
            };
            // B9（方案 A）：字段排序为主键，score DESC 为次级（同值命中优先项的排前面），
            // 尾缀仍为 id DESC → 分页稳定。
            (
                format!("sortv {dir}, score DESC, id DESC"),
                ", x.sortv AS sortv".to_string(),
                format!(", {col} AS sortv"),
            )
        }
    };

    // 4) 组装（三层：内层算 0/1 标记 → 中层算 score/hits → 外层过滤/排序）。
    //    每个 should 片段只在内层出现一次（别名不能在同一个 SELECT 里复用）。
    let inner_sel = if marker_sql.is_empty() {
        format!("SELECT a.id{sortv_inner}\n    FROM assets a\n   WHERE {base_sql}")
    } else {
        format!("SELECT a.id, {marker_sql}{sortv_inner}\n    FROM assets a\n   WHERE {base_sql}")
    };
    let mid_sel = format!(
        "SELECT x.id, {score_expr} AS score, {hits_expr} AS hits{sortv_keep}\n    FROM (\n  {inner_sel}\n) x"
    );
    params.push(Value::Integer(min_should));
    let min_param = params.len();
    let sql = format!(
        "SELECT q.id, q.score, q.hits FROM (\n  {mid_sel}\n) q\n\
         WHERE q.hits >= ?{min_param}\nORDER BY {order_sql}"
    );
    Ok(CompiledPlan { sql, params })
}

fn fmt_weight(w: f32) -> String {
    // 权重打印为可读小数（1.0 → 1.0；0.5 → 0.5；2.0 → 2.0）
    format!("{w}")
}

/// 执行计划：返回 (asset_id, score, hits) 列表。
/// compile 层 warning 请走 run_search_plan_with（本包装丢弃）。
pub fn run_search_plan(
    conn: &Connection,
    plan: &SearchPlanV3,
    limit: Option<i64>,
    offset: i64,
) -> AppResult<Vec<(i64, f64, f64)>> {
    run_search_plan_with(conn, plan, limit, offset, &mut Vec::new())
}

/// 执行计划 + 回传编译层 warning（与列表命令同批，§4.1）。
/// 调用方必须先 validate + prune（§4.1：校验在剔除之前）。
pub fn run_search_plan_with(
    conn: &Connection,
    plan: &SearchPlanV3,
    limit: Option<i64>,
    offset: i64,
    warnings: &mut Vec<SearchWarning>,
) -> AppResult<Vec<(i64, f64, f64)>> {
    // S2/S4：Relevance 且配了检索器 → should 加权排序转排名，与各路检索器 RRF 融合
    if matches!(&plan.ranking, Ranking::Relevance) && !plan.retrievers.retrievers.is_empty() {
        return run_relevance_fused_with(conn, plan, limit, offset, warnings);
    }
    let compiled = compile_search_plan_with(conn, plan, warnings)?;
    let mut sql = compiled.sql.clone();
    let mut params = compiled.params;
    if let Some(l) = limit {
        let li = params.len() + 1;
        // 列表分页在 run_plan_page 单独限制到 1000；这里不能再次钳制，
        // 否则全选 ID 路径传入 PLAN_IDS_CAP 时会静默只返回前 1000 条。
        params.push(Value::Integer(l.max(0)));
        let oi = params.len() + 1;
        params.push(Value::Integer(offset.max(0)));
        sql.push_str(&format!(" LIMIT ?{li} OFFSET ?{oi}"));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// S2/S4：候选集 = filter/must_not/min_should（compile_search_plan 的 WHERE），
/// should 加权分排序成一路排名；各路检索器（Fts bm25 / TagAlias）各成一排名；
/// 全部经 RRF 融合 —— 量纲统一（bm25 负值 / should 加分任意正数 / 别名 0-1 只比排名）。
fn run_relevance_fused_with(
    conn: &Connection,
    plan: &SearchPlanV3,
    limit: Option<i64>,
    offset: i64,
    warnings: &mut Vec<SearchWarning>,
) -> AppResult<Vec<(i64, f64, f64)>> {
    // ① 候选 + should 相关度顺序（现有 SQL 已按 score DESC, id DESC）
    let compiled = compile_search_plan_with(conn, plan, warnings)?;
    let mut stmt = conn.prepare(&compiled.sql)?;
    let candidates = stmt
        .query_map(rusqlite::params_from_iter(compiled.params.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let cand_set: std::collections::HashSet<i64> = candidates.iter().map(|(id, _)| *id).collect();
    // ② should 一路 = 候选集按 score 排序的排名
    let should_ranked: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
    // ③ 各路检索器排名（各自 ≥0 条）
    let mut lists: Vec<(&[i64], f32)> = Vec::new();
    lists.push((&should_ranked, 1.0));
    let mut owned: Vec<Vec<i64>> = Vec::new();
    for wr in &plan.retrievers.retrievers {
        let ids = run_retriever(conn, &wr.kind)?;
        owned.push(ids);
    }
    for (wr, ids) in plan.retrievers.retrievers.iter().zip(owned.iter()) {
        if !ids.is_empty() {
            lists.push((ids.as_slice(), wr.weight));
        }
    }
    let fused = rrf_fuse(&lists);
    // ④ 结果限制在候选集内（filter 是硬性必须）；分页
    let page: Vec<(i64, f64, f64)> = fused
        .into_iter()
        .filter(|(id, _)| cand_set.contains(id))
        .skip(offset.max(0) as usize)
        .take(limit.map(|l| l.max(0) as usize).unwrap_or(usize::MAX))
        .map(|(id, s)| (id, s, 0.0))
        .collect();
    Ok(page)
}

/// S2：RRF（Reciprocal Rank Fusion）—— `score = Σ w_i / (k + rank_i)`，k=60。
/// 只用排名不用原始分数：bm25 是负值（越小越相关）、should 加分任意正数、
/// 别名命中 0/1 —— 三者的**排名**可比，**分数**不可比，RRF 天然免疫量纲。
pub fn rrf_fuse(lists: &[(&[i64], f32)]) -> Vec<(i64, f64)> {
    let k = 60.0f64;
    let mut acc: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for (ids, w) in lists {
        for (rank, &id) in ids.iter().enumerate() {
            let e = acc.entry(id).or_insert(0.0);
            *e += *w as f64 / (k + rank as f64);
        }
    }
    let mut out: Vec<(i64, f64)> = acc.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// S4：执行单路检索器 → 该路按相关度排序的素材 id 列表。
/// - `Fts`：FTS5 bm25 排序（MATCH 词与编译期谓词同一套语义 —— 复用
///   build_search_predicate 的 MATCH 参数，排名用 bm25(assets_fts)；明确不做向量）。
/// - `TagAlias`：经 tag_terms 别名扩展命中 tag（受 F5-d feature gate 控制，
///   关时返回空 + warning）再查素材（≤10 上限语义由调用方在融合前截断）。
pub fn run_retriever(conn: &Connection, r: &Retriever) -> AppResult<Vec<i64>> {
    match r {
        Retriever::Fts { query, scope } => {
            let Some(pred) = super::search::build_search_predicate(conn, query, *scope)? else {
                return Ok(Vec::new());
            };
            let Some(Value::Text(term)) = pred.params.first() else {
                return Ok(Vec::new());
            };
            let mut stmt = conn.prepare(
                "SELECT a.id
                   FROM assets a
                   JOIN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?1
                          ORDER BY bm25(assets_fts) LIMIT 200) f ON f.rowid = a.id
                  WHERE a.deleted_at IS NULL",
            )?;
            let rows = stmt
                .query_map([term.as_str()], |r| r.get::<_, i64>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
        Retriever::TagAlias { text, facet_key } => {
            let terms_enabled =
                crate::db::schema_features::feature_enabled(conn, "tag_unique_terms")
                    .unwrap_or(false);
            if !terms_enabled {
                tracing::warn!("标签别名检索需先在设置页启用标签约束，已跳过该路召回");
                return Ok(Vec::new());
            }
            let facet = facet_key.as_deref().unwrap_or("");
            let normalized = crate::db::tags::normalize_name(text);
            // Alias 单点（canonical/synonym 均命中；cap 1 语义 = 单标签）
            let lookup = crate::db::tags::find_by_term(
                conn,
                facet,
                &normalized,
                crate::db::tags::TermMatch::Alias,
            )?;
            let Some(hit) = lookup.hits.into_iter().next() else {
                return Ok(Vec::new());
            };
            let mut stmt = conn.prepare(
                "SELECT a.id
                   FROM assets a JOIN asset_tags at ON at.asset_id = a.id
                  WHERE at.tag_id = ?1 AND a.deleted_at IS NULL
                  ORDER BY a.id DESC LIMIT 10",
            )?;
            let rows = stmt
                .query_map([hit.tag_id], |r| r.get::<_, i64>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
    }
}

// ═══════════════ §4.2：prune_invalid（AST 层上下文相关剔除） ═══════════════

/// 叶子编译折叠结果（B1）：compile_leaf_with 会把「无效条件」折叠成常量。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fold {
    /// 折叠成 1=1（忽略该条件 → 各区都删）
    AlwaysTrue,
    /// 折叠成 1=0（条件本身不可满足）
    AlwaysFalse,
    /// 正常可编译
    Value,
}

/// 探测单叶子的编译折叠 + 底层剔除原因（compile 侧 warning 原文）。
fn probe_leaf(conn: &Connection, cond: &LeafCond) -> AppResult<(Fold, String)> {
    let mut sink: Vec<String> = Vec::new();
    let (sql, _) = compile_leaf_with(conn, cond, &mut sink)?;
    let why = if sink.is_empty() {
        cond_label(cond, None)
    } else {
        format!("{}（{}）", cond_label(cond, None), sink.join("；"))
    };
    let fold = match sql.trim() {
        "1=1" => Fold::AlwaysTrue,
        "1=0" => Fold::AlwaysFalse,
        _ => Fold::Value,
    };
    Ok((fold, why))
}

/// 子表达式整体是否已是常量（NOT(常量) 无意义：true→永不匹配 / false→恒匹配）。
fn expr_is_constant(conn: &Connection, expr: &QueryExpr) -> AppResult<bool> {
    let mut sink: Vec<String> = Vec::new();
    let (sql, _) = compile_expr_with(conn, expr, &mut sink)?;
    Ok(is_const_sql(&sql))
}

fn is_const_sql(sql: &str) -> bool {
    let mut s = sql.trim().to_string();
    loop {
        let t = s.trim();
        if t.starts_with('(') && t.ends_with(')') {
            // 剥掉首尾成对括号（compile 层包了一到多层）
            let inner = &t[1..t.len() - 1];
            s = inner.to_string();
        } else {
            s = t.to_string();
            break;
        }
    }
    s == "1=1" || s == "1=0"
}

/// 在一棵表达式里递归剔除无效叶子。
/// - zone == "mustNot" 时恒假叶子也必须删（NOT(1=0) 会让排除条件静默消失）；
/// - filter 内恒真叶子删（忽略该条件）；恒假叶子保留（用户搜了不存在的词 = 明确零命中）；
/// - Not 子树只在 filter 合法；其子表达式若已被删空或折叠成常量 → 整棵 Not 删（NOT(1=1) 会清库）。
fn prune_expr(
    conn: &Connection,
    expr: &QueryExpr,
    zone: &str,
    warns: &mut Vec<SearchWarning>,
) -> AppResult<Option<QueryExpr>> {
    match expr {
        QueryExpr::Leaf { cond } => {
            let (fold, why) = probe_leaf(conn, cond)?;
            let drop = match fold {
                Fold::AlwaysTrue => true,
                Fold::AlwaysFalse => zone == "mustNot",
                Fold::Value => false,
            };
            if drop {
                warns.push(SearchWarning::plan(
                    Some(zone),
                    format!("{why} —— 已忽略该条件。"),
                ));
                Ok(None)
            } else {
                Ok(Some(expr.clone()))
            }
        }
        QueryExpr::And { children } => prune_group(conn, children, true, zone, warns),
        QueryExpr::Or { children } => prune_group(conn, children, false, zone, warns),
        QueryExpr::Not { child } => {
            if zone == "mustNot" {
                // 校验层已拒绝；防御性兜底：不解释、直接删（绝不让三重否定进 SQL）
                warns.push(SearchWarning::plan(
                    Some(zone),
                    "排除区不支持否定节点，已忽略该条件。",
                ));
                return Ok(None);
            }
            match prune_expr(conn, child, zone, warns)? {
                None => Ok(None),
                Some(c) => {
                    if expr_is_constant(conn, &c)? {
                        warns.push(SearchWarning::plan(
                            Some(zone),
                            "否定内的条件已失效（恒真/恒假），该否定条件已忽略。",
                        ));
                        Ok(None)
                    } else {
                        Ok(Some(QueryExpr::Not { child: Box::new(c) }))
                    }
                }
            }
        }
    }
}

fn prune_group(
    conn: &Connection,
    children: &[QueryExpr],
    is_and: bool,
    zone: &str,
    warns: &mut Vec<SearchWarning>,
) -> AppResult<Option<QueryExpr>> {
    let mut kept: Vec<QueryExpr> = Vec::new();
    for c in children {
        if let Some(k) = prune_expr(conn, c, zone, warns)? {
            kept.push(k);
        }
    }
    match kept.len() {
        0 => Ok(None),
        1 => Ok(kept.pop()),
        _ => Ok(Some(if is_and {
            QueryExpr::And { children: kept }
        } else {
            QueryExpr::Or { children: kept }
        })),
    }
}

/// §4.2：执行前在 AST 层做上下文相关的结构性删除（B1 修法）。
/// 返回剔除后的 plan + 带区名的 warning（每个删除位置一条人话）。
/// 调用方必须先 validate（校验在剔除之前，否则非法 weight 会被剔除逻辑先碰到）。
pub fn prune_invalid(
    conn: &Connection,
    plan: &SearchPlanV3,
) -> AppResult<(SearchPlanV3, Vec<SearchWarning>)> {
    let mut warns: Vec<SearchWarning> = Vec::new();
    let filter = match &plan.filter {
        Some(f) => prune_expr(conn, f, "filter", &mut warns)?,
        None => None,
    };
    let must_not = match &plan.must_not {
        Some(m) => prune_expr(conn, m, "mustNot", &mut warns)?,
        None => None,
    };
    // should：恒真加分项会污染 score / 让全库通过 → 整条删；恒假保留（该加分项恒不命中，无害）
    let mut should: Vec<ShouldClause> = Vec::new();
    for sc in &plan.should {
        let (fold, why) = probe_leaf(conn, &sc.cond)?;
        if fold == Fold::AlwaysTrue {
            warns.push(SearchWarning::plan(
                Some("should"),
                format!("{why} —— 已忽略该加分项。"),
            ));
        } else {
            should.push(sc.clone());
        }
    }
    // minimum_should_match 收敛到新 should 长度
    let mut minimum_should_match = plan.minimum_should_match;
    if minimum_should_match > should.len() as u32 {
        minimum_should_match = should.len() as u32;
        warns.push(SearchWarning::plan(
            Some("should"),
            format!("『至少满足 N 项』已随剔除收敛为 {minimum_should_match}。"),
        ));
    }
    let pruned = SearchPlanV3 {
        filter,
        must_not,
        should,
        minimum_should_match,
        ..plan.clone()
    };
    Ok((pruned, warns))
}

/// 单个叶子条件的诊断。path = 从根到该叶子的子节点索引路径（OR/AND children 下标）。
/// zone 标识叶子所在区（"filter" | "mustNot"）—— 仅凭 path 无法区分两区的同下标（§3.7 不变式 9）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeafDiagnostic {
    /// "filter" | "mustNot"（UI 据此删除对应区的条件）
    pub zone: String,
    pub path: Vec<usize>,
    /// §3.7 不变式 9：发起诊断时的 plan 代次 —— 返回时代次过期则整批丢弃（防删错条件）
    pub plan_revision: i64,
    pub label: String,
    /// ① 该条件单独执行的命中数
    pub self_count: i64,
    /// ② 完整表达式的命中数（所有叶子共享同一个值）
    pub result_count: i64,
    /// ③ 把该叶子从 AST 中移除后的命中数
    pub count_without_leaf: i64,
    /// ④ delta = count_without_leaf - result_count
    ///    AND 下为正（砍掉多少）；OR 下为负（贡献多少）；NOT 下反转 —— 唯一三态都有意义的量
    pub delta: i64,
}

/// should（加分项）诊断：不淘汰结果，只显示「命中该加分项的素材数 / 结果总数」。
/// index 对应 plan.should 的下标（UI 删除/改权重按 index 定位）；
/// hit_count 是当前结果集 ∩ 该加分项的交集（B3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShouldDiagnostic {
    pub index: usize,
    /// §3.7 不变式 9：发起诊断时的 plan 代次（过期整批丢弃）
    pub plan_revision: i64,
    pub label: String,
    pub hit_count: i64,
    pub total_count: i64,
}

/// §4.5：诊断命令返回值 —— 与列表命令同一批 prune warning。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlanDiagnostics {
    pub leaves: Vec<LeafDiagnostic>,
    pub should: Vec<ShouldDiagnostic>,
    pub warnings: Vec<SearchWarning>,
}

/// 对 SearchPlanV3 做 AST 命中诊断（N 叶子 = 2N+1 次 COUNT；毫秒级）。
/// §4.5 同链：入口先 validate → prune，用剔除后的 plan 做全部诊断，
/// 返回的 warnings 与列表命令是同一批。
/// filter/must_not 叶子进 LeafDiagnostic（带 zone）；should 单独进 ShouldDiagnostic
/// （带 index，hit_count = 当前结果集 ∩ 该加分项，修 B3 分母错集）。
pub fn diagnose_search_plan(
    conn: &Connection,
    plan: &SearchPlanV3,
    plan_revision: i64,
) -> AppResult<PlanDiagnostics> {
    validate_search_plan(plan)?;
    let (pruned, warnings) = prune_invalid(conn, plan)?;
    let tag_names = load_plan_tag_names(conn, &pruned)?;
    let result_count = count_plan(conn, &pruned)?;
    let mut leaves: Vec<LeafDiagnostic> = Vec::new();
    // filter 树叶子（zone=filter）
    if let Some(f) = &pruned.filter {
        collect_leaves(
            conn,
            &pruned,
            f,
            &[],
            "filter",
            result_count,
            plan_revision,
            &tag_names,
            &mut leaves,
        )?;
    }
    // must_not 树叶子（zone=mustNot；NOT 语境）
    if let Some(m) = &pruned.must_not {
        collect_leaves(
            conn,
            &pruned,
            m,
            &[],
            "mustNot",
            result_count,
            plan_revision,
            &tag_names,
            &mut leaves,
        )?;
    }
    // should 命中/总数（B3：分子 = 当前结果集内命中该加分项的素材数）
    let should_hits = count_intersections(
        conn,
        &pruned,
        &pruned.should.iter().map(|sc| &sc.cond).collect::<Vec<_>>(),
    )?;
    let mut should_diag = Vec::new();
    for (index, (sc, hit)) in pruned.should.iter().zip(should_hits).enumerate() {
        should_diag.push(ShouldDiagnostic {
            index,
            plan_revision,
            label: if sc.label.is_empty() {
                cond_label(&sc.cond, Some(&tag_names))
            } else {
                sc.label.clone()
            },
            hit_count: hit,
            total_count: result_count,
        });
    }
    Ok(PlanDiagnostics {
        leaves,
        should: should_diag,
        warnings,
    })
}

/// 计划匹配总数（列表 total / 全选 total 用）。复用 compile_search_plan（§4.1 单一编译器）。
pub fn count_plan(conn: &Connection, plan: &SearchPlanV3) -> AppResult<i64> {
    let compiled = compile_search_plan(conn, plan)?;
    let sql = format!("SELECT COUNT(*) FROM (\n{}\n) _diag", compiled.sql);
    let n: i64 = conn.query_row(
        &sql,
        rusqlite::params_from_iter(compiled.params.iter()),
        |r| r.get(0),
    )?;
    Ok(n)
}

/// B3：批量统计「当前结果集 ∩ 每个加分条件」的命中数。
/// 将多个 should 条件合并到一次 SQLite 查询，避免每个条件重复编译/扫描完整结果集。
fn count_intersections(
    conn: &Connection,
    plan: &SearchPlanV3,
    conds: &[&LeafCond],
) -> AppResult<Vec<i64>> {
    if conds.is_empty() {
        return Ok(Vec::new());
    }
    let compiled = compile_search_plan(conn, plan)?;
    let mut params = compiled.params;
    let mut projections = Vec::with_capacity(conds.len());
    for cond in conds {
        let mut sink: Vec<String> = Vec::new();
        let (leaf_sql, leaf_params) = compile_leaf_with(conn, cond, &mut sink)?;
        let shifted = offset_placeholders(&leaf_sql, params.len());
        projections.push(format!(
            "COALESCE(SUM(CASE WHEN ({shifted}) THEN 1 ELSE 0 END), 0)"
        ));
        params.extend(leaf_params);
    }
    let sql = format!(
        "SELECT {} FROM (\n{}\n) _si JOIN assets a ON a.id = _si.id",
        projections.join(", "),
        compiled.sql,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let Some(row) = rows.next()? else {
        return Ok(vec![0; conds.len()]);
    };
    let mut counts = Vec::with_capacity(conds.len());
    for index in 0..conds.len() {
        counts.push(row.get::<_, i64>(index)?);
    }
    Ok(counts)
}

/// 递归收集叶子。pos = 相对当前树根的路径。
/// zone："filter"（正向语境）| "mustNot"（NOT 语境）；叶子按所在区打标（§3.7 不变式 9），
/// 且必须区叶子与排除区叶子的删除目标由此确定。
/// 参数均为本次诊断的一次性上下文，聚合进结构体只会增加递归调用噪音，故保留平铺参数。
#[allow(clippy::too_many_arguments)]
fn collect_leaves(
    conn: &Connection,
    plan: &SearchPlanV3,
    node: &QueryExpr,
    prefix: &[usize],
    zone: &str,
    result_count: i64,
    plan_revision: i64,
    tag_names: &HashMap<i64, String>,
    out: &mut Vec<LeafDiagnostic>,
) -> AppResult<()> {
    match node {
        QueryExpr::Leaf { cond } => {
            // self_count：叶子单独执行（在 NOT 语境下也是「该条件本身」的命中数）
            let solo = SearchPlanV3 {
                filter: Some(QueryExpr::Leaf { cond: cond.clone() }),
                minimum_should_match: 0,
                ..SearchPlanV3::default()
            };
            let self_count = count_plan(conn, &solo)?;
            // without：从原树移除该叶子（按 prefix 定位）后重算
            let positive = zone == "filter";
            let mut variant = plan.clone();
            if positive {
                variant.filter = remove_leaf(variant.filter.as_ref(), prefix);
            } else {
                variant.must_not = remove_leaf(variant.must_not.as_ref(), prefix);
            }
            let without = count_plan(conn, &variant)?;
            let label = cond_label(cond, Some(tag_names));
            out.push(LeafDiagnostic {
                zone: zone.to_string(),
                path: prefix.to_vec(),
                plan_revision,
                label,
                self_count,
                result_count,
                count_without_leaf: without,
                delta: without - result_count,
            });
            Ok(())
        }
        QueryExpr::And { children } | QueryExpr::Or { children } => {
            for (i, c) in children.iter().enumerate() {
                let mut p = prefix.to_vec();
                p.push(i);
                collect_leaves(
                    conn,
                    plan,
                    c,
                    &p,
                    zone,
                    result_count,
                    plan_revision,
                    tag_names,
                    out,
                )?;
            }
            Ok(())
        }
        QueryExpr::Not { child } => {
            // NOT 子树：仍在 filter 区，delta 语义在 remove_leaf 的 NOT 分支下自然成立
            let mut p = prefix.to_vec();
            p.push(0);
            collect_leaves(
                conn,
                plan,
                child,
                &p,
                zone,
                result_count,
                plan_revision,
                tag_names,
                out,
            )
        }
    }
}

/// 从 expr 移除 path 指定的叶子；返回 None 表示整棵被移除（子树空）。
fn remove_leaf(expr: Option<&QueryExpr>, path: &[usize]) -> Option<QueryExpr> {
    let e = expr?;
    if path.is_empty() {
        return None; // 移除根
    }
    match e {
        QueryExpr::Leaf { .. } => None, // 路径不匹配（叶子上还有下标）→ 不变
        QueryExpr::And { children } | QueryExpr::Or { children } => {
            let i = path[0];
            if i >= children.len() {
                return Some(e.clone());
            }
            let mut kids = children.clone();
            if path.len() == 1 {
                kids.remove(i);
            } else {
                let sub = remove_leaf(Some(&kids[i]), &path[1..]);
                match sub {
                    Some(n) => kids[i] = n,
                    None => {
                        kids.remove(i);
                    }
                }
            }
            match kids.len() {
                0 => None,
                1 => kids.pop(),
                _ => Some(match e {
                    QueryExpr::And { .. } => QueryExpr::And { children: kids },
                    _ => QueryExpr::Or { children: kids },
                }),
            }
        }
        QueryExpr::Not { child } => {
            if path.len() == 1 {
                return None; // collect_leaves 为 Not 子叶压入 [0]，此时应移除整个 Not
            }
            let sub = remove_leaf(Some(child), &path[1..]);
            sub.map(|n| QueryExpr::Not { child: Box::new(n) })
        }
    }
}

fn cond_label(cond: &LeafCond, tag_names: Option<&HashMap<i64, String>>) -> String {
    let format_tags = |ids: &[i64]| {
        if ids.is_empty() {
            return "标签×0".to_string();
        }
        if let Some(names) = tag_names {
            return ids
                .iter()
                .map(|id| {
                    names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| format!("标签#{id}"))
                })
                .collect::<Vec<_>>()
                .join("、");
        }
        format!("标签×{}", ids.len())
    };
    match cond {
        LeafCond::Tag {
            facet_key,
            tag_ids,
            term_query,
            ..
        } => {
            if tag_ids.is_empty() {
                if let Some(tq) = term_query.as_deref().filter(|s| !s.is_empty()) {
                    return format!("{facet_key}: {tq}");
                }
            }
            if !tag_ids.is_empty() && tag_names.is_some() {
                format!("{facet_key}: {}", format_tags(tag_ids))
            } else if let Some(tq) = term_query.as_deref().filter(|s| !s.is_empty()) {
                format!("{facet_key}: {tq}")
            } else {
                format!("{facet_key}: {}", format_tags(tag_ids))
            }
        }
        LeafCond::ExcludeTag { facet_key, tag_ids } => {
            if tag_names.is_some() {
                format!("排除 {facet_key}: {}", format_tags(tag_ids))
            } else {
                format!("排除 {facet_key}×{}", tag_ids.len())
            }
        }
        LeafCond::AssetType { value } => format!("类型: {value}"),
        LeafCond::Untagged => "未打标".into(),
        LeafCond::Metadata { filter } => format!("{} {} {:?}", filter.key, filter.op, filter.value),
        LeafCond::Search { value, .. } => format!("关键词: {value}"),
        LeafCond::FacetHasAny { facet_key } => format!("{facet_key} 有任意标签"),
        LeafCond::FacetMissing { facet_key } => format!("{facet_key} 没有标签"),
        LeafCond::FacetNumber {
            facet_key,
            op,
            value,
            max_value,
        } => {
            let op_label = match op.as_str() {
                "eq" => format!("= {value}"),
                "gt" => format!("> {value}"),
                "gte" => format!("≥ {value}"),
                "lt" => format!("< {value}"),
                "lte" => format!("≤ {value}"),
                "between" => format!("{value} ~ {}", max_value.unwrap_or(*value)),
                _ => format!("{op} {value}"),
            };
            format!("{facet_key} {op_label}")
        }
    }
}

fn collect_tag_ids_from_cond(cond: &LeafCond, out: &mut HashSet<i64>) {
    match cond {
        LeafCond::Tag { tag_ids, .. } | LeafCond::ExcludeTag { tag_ids, .. } => {
            out.extend(tag_ids.iter().copied());
        }
        _ => {}
    }
}

fn collect_tag_ids_from_expr(expr: &QueryExpr, out: &mut HashSet<i64>) {
    match expr {
        QueryExpr::Leaf { cond } => collect_tag_ids_from_cond(cond, out),
        QueryExpr::Not { child } => collect_tag_ids_from_expr(child, out),
        QueryExpr::And { children } | QueryExpr::Or { children } => {
            for child in children {
                collect_tag_ids_from_expr(child, out);
            }
        }
    }
}

fn load_plan_tag_names(conn: &Connection, plan: &SearchPlanV3) -> AppResult<HashMap<i64, String>> {
    let mut ids = HashSet::new();
    if let Some(filter) = &plan.filter {
        collect_tag_ids_from_expr(filter, &mut ids);
    }
    if let Some(must_not) = &plan.must_not {
        collect_tag_ids_from_expr(must_not, &mut ids);
    }
    for clause in &plan.should {
        collect_tag_ids_from_cond(&clause.cond, &mut ids);
    }
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut sorted_ids: Vec<i64> = ids.into_iter().collect();
    sorted_ids.sort_unstable();
    let placeholders = (0..sorted_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id, name FROM tags WHERE id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(sorted_ids.iter()), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut names = HashMap::new();
    for row in rows {
        let (id, name) = row?;
        names.insert(id, name);
    }
    Ok(names)
}

/// S6：schema 版本迁移（plan_schema_version 3 → 4 时在此加 migrate_plan_v3_to_v4；
/// 现在结构未变，直接原样返回）。读到旧版本先迁移再执行。
pub fn migrate_plan(plan: &mut SearchPlanV3) {
    match plan.plan_schema_version {
        v if v < PLAN_SCHEMA_VERSION => {
            // 未来：plan = migrate_plan_v3_to_v4(plan) 后再走下一段
            plan.plan_schema_version = PLAN_SCHEMA_VERSION;
        }
        v if v > PLAN_SCHEMA_VERSION => {
            // 用户降级了应用：不在此迁移，由调用方丢弃 + warning（superSearchStore hydrate）
        }
        _ => {}
    }
}

// ═══════════════ Phase 2 §4.1：plan 列表 / 全选 ID（单一事实源执行链） ═══════════════

/// B8：全选/批量操作上限（与 assets::list_ids 的 100000 一致）。
pub const PLAN_IDS_CAP: i64 = 100_000;

/// B2/B8：全选 ID 一路到底的返回类型（不许在 store 层退化成裸 number[]，否则 truncated/warnings 丢失）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanIdsResult {
    pub ids: Vec<i64>,
    pub total: i64,
    pub truncated: bool,
    pub warnings: Vec<SearchWarning>,
}

/// §4.1：plan 执行的分页结果（items/total/hasMore 与 AssetPage 同形；warnings 为 SearchWarning[]）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanAssetPage {
    pub items: Vec<crate::db::assets::Asset>,
    pub total: i64,
    pub has_more: bool,
    pub warnings: Vec<SearchWarning>,
}

/// §4.1 单条执行链：validate → prune → 执行（顺序不可调换：
/// 非法 weight 应先被校验拦住，而不是先被剔除逻辑碰到）。
pub fn prepare_execution(
    conn: &Connection,
    plan: &SearchPlanV3,
) -> AppResult<(SearchPlanV3, Vec<SearchWarning>)> {
    validate_search_plan(plan)?;
    prune_invalid(conn, plan)
}

/// 全选 / 反选 ID：total（未截断）、truncated（触到 100000 上限）、warnings。
pub fn run_plan_ids(conn: &Connection, plan: &SearchPlanV3) -> AppResult<PlanIdsResult> {
    let (pruned, mut warnings) = prepare_execution(conn, plan)?;
    let total = count_plan(conn, &pruned)?;
    let rows = run_search_plan_with(conn, &pruned, Some(PLAN_IDS_CAP), 0, &mut warnings)?;
    Ok(PlanIdsResult {
        ids: rows.iter().map(|r| r.0).collect(),
        total,
        truncated: total > PLAN_IDS_CAP,
        warnings,
    })
}

/// 结果列表（分页）：items 按 plan 排序取页，total 与列表同源，warnings 同批。
pub fn run_plan_page(
    conn: &Connection,
    plan: &SearchPlanV3,
    offset: i64,
    limit: Option<i64>,
) -> AppResult<PlanAssetPage> {
    let (pruned, mut warnings) = prepare_execution(conn, plan)?;
    let total = count_plan(conn, &pruned)?;
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let offset = offset.max(0);
    let rows = run_search_plan_with(conn, &pruned, Some(limit), offset, &mut warnings)?;
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let items = crate::db::assets::by_ids_ordered(conn, &ids)?;
    Ok(PlanAssetPage {
        has_more: (offset + ids.len() as i64) < total,
        items,
        total,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use crate::db::{asset_tags, assets, tags};

    fn insert_asset(c: &Connection, path: &str) -> i64 {
        assets::insert(c, path, "a.jpg", "jpg", 1024, "image/jpeg", 1700000000000).unwrap()
    }
    fn tag(c: &Connection, facet: &str, name: &str) -> i64 {
        tags::create_in_facet(c, name, None, Some(facet))
            .unwrap()
            .id
    }
    fn tag_leaf(facet: &str, ids: Vec<i64>) -> LeafCond {
        LeafCond::Tag {
            term_query: None,
            term_match: Default::default(),
            facet_key: facet.into(),
            tag_ids: ids,
            mode: None,
            include_descendants: true,
        }
    }
    fn tag_plan(facet: &str, tag_id: i64, should: &[(&str, i64, f32)]) -> SearchPlanV3 {
        SearchPlanV3 {
            filter: Some(QueryExpr::Leaf {
                cond: tag_leaf(facet, vec![tag_id]),
            }),
            should: should
                .iter()
                .map(|(label, id, w)| ShouldClause {
                    cond: tag_leaf(facet, vec![*id]),
                    weight: *w,
                    label: label.to_string(),
                    evidence: None,
                })
                .collect(),
            minimum_should_match: 0,
            ranking: Ranking::Relevance,
            ..Default::default()
        }
    }

    #[test]
    fn should_does_not_eliminate_nonmatching() {
        let c = init_memory().unwrap();
        let (grass, sky) = (tag(&c, "scene", "草地"), tag(&c, "scene", "蓝天"));
        let a = insert_asset(&c, "d:/1.jpg");
        let b = insert_asset(&c, "d:/2.jpg");
        asset_tags::assign(&c, &[a], &[grass, sky], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[grass], "manual").unwrap();
        let plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|x| x.0).collect();
        assert!(ids.contains(&b), "无蓝天的草地照不得被淘汰");
        assert!(ids.contains(&a));
        assert_eq!(out[0].0, a, "有蓝天应排前");
    }

    #[test]
    fn should_affects_order_only_when_min_is_zero() {
        let c = init_memory().unwrap();
        let (grass, sky) = (tag(&c, "scene", "草地"), tag(&c, "scene", "蓝天"));
        let a = insert_asset(&c, "d:/1.jpg");
        let b = insert_asset(&c, "d:/2.jpg");
        asset_tags::assign(&c, &[a], &[grass, sky], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[grass], "manual").unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        plan.minimum_should_match = 0;
        let out0 = run_search_plan(&c, &plan, None, 0).unwrap();
        assert_eq!(out0.len(), 2, "min=0 时 should 只影响顺序不影响集合");
        assert_eq!(out0[0].0, a);
        plan.minimum_should_match = 1;
        let out1 = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out1.iter().map(|x| x.0).collect();
        assert_eq!(ids, vec![a], "min=1 时只有命中的进结果");
    }

    #[test]
    fn min_should_match_filters() {
        let c = init_memory().unwrap();
        let (grass, sky, night) = (
            tag(&c, "scene", "草地"),
            tag(&c, "scene", "蓝天"),
            tag(&c, "scene", "夜景"),
        );
        let a = insert_asset(&c, "d:/1.jpg");
        let b = insert_asset(&c, "d:/2.jpg");
        let d = insert_asset(&c, "d:/3.jpg");
        asset_tags::assign(&c, &[a], &[grass, sky, night], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[grass, sky], "manual").unwrap();
        asset_tags::assign(&c, &[d], &[grass], "manual").unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0), ("夜景", night, 2.0)]);
        plan.minimum_should_match = 2;
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|x| x.0).collect();
        assert_eq!(ids, vec![a], "min=2 → 只有两条加分都命中的 {ids:?}");
    }

    /// B6（方案 A）：filter 空 + should 非空 + min=0 = 全库软排序，
    /// 完全不命中加分项的素材也保留（仅调整顺序）。
    #[test]
    fn empty_filter_with_should_min_zero_returns_all() {
        let c = init_memory().unwrap();
        let sky = tag(&c, "scene", "蓝天");
        let a = insert_asset(&c, "d:/1.jpg");
        let b = insert_asset(&c, "d:/2.jpg");
        asset_tags::assign(&c, &[a], &[sky], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[tag(&c, "scene", "其他")], "manual").unwrap();
        let mut plan = tag_plan("scene", sky, &[("蓝天", sky, 1.0)]);
        plan.filter = None; // 空 filter + min=0 → 允许全库软排序
        plan.minimum_should_match = 0;
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|x| x.0).collect();
        assert_eq!(ids.len(), 2, "方案 A：应返回全部素材，只调整顺序：{ids:?}");
        assert_eq!(ids[0], a, "命中加分项的排前面");
        // min=1 时仍会淘汰不命中的
        plan.minimum_should_match = 1;
        let out1 = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids1: Vec<i64> = out1.iter().map(|x| x.0).collect();
        assert_eq!(ids1, vec![a]);
    }

    #[test]
    fn should_sql_evaluated_once() {
        let c = init_memory().unwrap();
        let (grass, sky, night) = (
            tag(&c, "scene", "草地"),
            tag(&c, "scene", "蓝天"),
            tag(&c, "scene", "夜景"),
        );
        let _a = insert_asset(&c, "d:/1.jpg");
        let plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0), ("夜景", night, 2.0)]);
        let compiled = compile_search_plan(&c, &plan).unwrap();
        let count_case = compiled.sql.matches("CASE WHEN").count();
        assert_eq!(
            count_case, 2,
            "每个 should 片段只应求值一次: {}",
            compiled.sql
        );
        // 每个分面 EXISTS 片段应各出现一次（含后代子查询体只出现一次）
        assert_eq!(
            compiled.sql.matches("EXISTS").count(),
            1 + 2,
            "filter 1 + should 2 各一次"
        );
    }

    #[test]
    fn validates_should_limit_and_sort_key() {
        let plan = SearchPlanV3 {
            ranking: Ranking::Field {
                key: "magic".into(),
                dir: "desc".into(),
            },
            ..Default::default()
        };
        assert!(validate_search_plan(&plan).is_err(), "非法排序字段必须拒绝");
        let p2 = SearchPlanV3 {
            minimum_should_match: 1,
            ..Default::default()
        }; // should 空但 min>0
        assert!(validate_search_plan(&p2).is_err());
    }

    /// S2：RRF 只用排名 —— 把某一路权重全乘 100（等效把该路分数放大），排序不变。
    #[test]
    fn rrf_fusion_is_scale_invariant() {
        let a = [10i64, 20, 30];
        let b = [25i64, 5];
        let base = rrf_fuse(&[(&a, 1.0), (&b, 2.0)]);
        let scaled = rrf_fuse(&[(&a, 100.0), (&b, 200.0)]);
        let order = |v: &[(i64, f64)]| v.iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(order(&base), order(&scaled), "整体权重放大不改变排序");
        // 交集元素按排名融合（id 20 在两路都靠前 → 总分高于只在单路的 30）
        let score_of = |v: &[(i64, f64)], id: i64| v.iter().find(|x| x.0 == id).map(|x| x.1);
        let s20 = score_of(&base, 20).unwrap();
        let s30 = score_of(&base, 30).unwrap();
        assert!(s20 > s30, "20 两路命中应高于只在单路的 30");
        // 单路权重缩放不改变内部次序（bm25 负值场景下仍然只用排名）
        let only = rrf_fuse(&[(&a, 1.0)]);
        let only_scaled = rrf_fuse(&[(&a, 1000.0)]);
        assert_eq!(order(&only), order(&only_scaled));
        assert_eq!(order(&only), vec![10, 20, 30]);
    }

    /// S2：用户显式选了排序字段 → 不走相关度（should 只影响集合/加分不影响顺序）。
    #[test]
    fn field_ranking_overrides_relevance() {
        let c = init_memory().unwrap();
        let (grass, sky) = (tag(&c, "scene", "草地"), tag(&c, "scene", "蓝天"));
        let a = insert_asset(&c, "d:/1.jpg"); // rating 高但无蓝天
        let b = insert_asset(&c, "d:/2.jpg"); // 低 rating 但有蓝天
        asset_tags::assign(&c, &[a], &[grass], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[grass, sky], "manual").unwrap();
        c.execute(
            "UPDATE assets SET rating = ?1 WHERE id = ?2",
            rusqlite::params![5, a],
        )
        .unwrap();
        c.execute(
            "UPDATE assets SET rating = ?1 WHERE id = ?2",
            rusqlite::params![1, b],
        )
        .unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        plan.ranking = Ranking::Field {
            key: "rating".into(),
            dir: "desc".into(),
        };
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|x| x.0).collect();
        assert_eq!(ids[0], a, "显式 rating desc → 5 星排前（无视应蓝天加分）");
        assert_eq!(ids.len(), 2, "should 仍不影响集合（min=0）");
    }

    // ═══════════════ S4：两路真实检索器（TagAlias 别名扩展 / Fts bm25） ═══════════════

    fn s4_terms_db() -> Connection {
        let c = init_memory().unwrap();
        crate::db::migrations::apply_v22b_constraints(&c).unwrap();
        crate::db::schema_features::set_feature(&c, "tag_unique_terms", true, None).unwrap();
        c
    }
    fn s4_asset(c: &Connection, file_name: &str) -> i64 {
        assets::insert(
            c,
            &format!("d:/{file_name}"),
            file_name,
            "jpg",
            1024,
            "image/jpeg",
            1700000000000,
        )
        .unwrap()
    }

    /// S4：TagAlias 检索 —— 搜「海滨」经 tag_terms 命中「海边」→ 返回其素材。
    #[test]
    fn alias_retriever_expands_via_tag_terms() {
        let c = s4_terms_db();
        let t = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
        tags::add_alias(&c, t.id, "海滨", None, "synonym").unwrap();
        let aid = s4_asset(&c, "a.jpg");
        asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();
        let ids = run_retriever(
            &c,
            &Retriever::TagAlias {
                text: "海滨".into(),
                facet_key: Some("scene".into()),
            },
        )
        .unwrap();
        assert!(ids.contains(&aid), "搜「海滨」应命中「海边」素材：{ids:?}");
    }

    /// S4：feature gate 关时 TagAlias 返回空（不误命中旧表别名）。
    #[test]
    fn alias_retriever_disabled_without_terms() {
        let c = init_memory().unwrap(); // tag_unique_terms 默认关
        let t = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
        let aid = s4_asset(&c, "a.jpg");
        asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();
        let ids = run_retriever(
            &c,
            &Retriever::TagAlias {
                text: "海边".into(),
                facet_key: Some("scene".into()),
            },
        )
        .unwrap();
        assert!(ids.is_empty(), "gate 关时不得走 tag_terms 扩展：{ids:?}");
    }

    /// S4：TagAlias 素材上限 ≤10。
    #[test]
    fn alias_retriever_respects_cap() {
        let c = s4_terms_db();
        let t = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
        let mut aids = Vec::new();
        for i in 0..12 {
            let a = s4_asset(&c, &format!("a{i}.jpg"));
            asset_tags::assign(&c, &[a], &[t.id], "manual").unwrap();
            aids.push(a);
        }
        let ids = run_retriever(
            &c,
            &Retriever::TagAlias {
                text: "海边".into(),
                facet_key: Some("scene".into()),
            },
        )
        .unwrap();
        assert_eq!(ids.len(), 10, "TagAlias 素材上限 10：{ids:?}");
    }

    /// S4：Fts 检索 —— bm25 相关度排序（命中词素材排前，无关素材不出现）。
    #[test]
    fn fts_retriever_exposes_bm25_rank() {
        let c = init_memory().unwrap();
        let hit = s4_asset(&c, "wxyzportrait");
        let miss = s4_asset(&c, "zzqotherimage");
        let _other = s4_asset(&c, "wxyzportrait2"); // 同词另一张（不同 token：portrait2）
        let ids = run_retriever(
            &c,
            &Retriever::Fts {
                query: "wxyzportrait".into(),
                scope: crate::db::query_expr::SearchScope::FileName,
            },
        )
        .unwrap();
        assert!(!ids.is_empty(), "Fts 检索应命中含词素材");
        assert_eq!(ids[0], hit, "精确命中素材应排最前：{ids:?}");
        assert!(!ids.contains(&miss), "无关素材不得出现在 bm25 排序结果里");
    }

    // ═══════════════ C-2：AST 命中诊断 ═══════════════
    fn d_tag(facet: &str, id: i64) -> QueryExpr {
        QueryExpr::Leaf {
            cond: LeafCond::Tag {
                facet_key: facet.into(),
                tag_ids: vec![id],
                mode: Some("any".into()),
                include_descendants: true,
                term_query: None,
                term_match: crate::db::tags::TermMatch::Alias,
            },
        }
    }
    fn diag_leaves(conn: &Connection, plan: &SearchPlanV3) -> Vec<LeafDiagnostic> {
        diagnose_search_plan(conn, plan, 1).unwrap().leaves
    }

    /// C-2：AND 中把结果砍到 0 的叶子 —— delta>0 且 result_count==0（罪魁祸首标红依据）。
    #[test]
    fn diagnostic_delta_identifies_zeroing_leaf() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let b = tag(&c, "scene", "标签乙");
        let a1 = insert_asset(&c, "d:/d1.jpg");
        let a2 = insert_asset(&c, "d:/d2.jpg");
        let _b1 = insert_asset(&c, "d:/d3.jpg");
        asset_tags::assign(&c, &[a1], &[a], "manual").unwrap();
        asset_tags::assign(&c, &[a2], &[a], "manual").unwrap();
        asset_tags::assign(&c, &[_b1], &[b], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![d_tag("scene", a), d_tag("scene", b)],
            }),
            should: Vec::new(),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let leaves = diag_leaves(&c, &plan);
        assert_eq!(leaves.len(), 2);
        // children 顺序 = [标签甲, 标签乙]
        let al = &leaves[0];
        let bl = &leaves[1];
        assert_eq!(bl.result_count, 0);
        assert!(bl.delta > 0, "乙把结果砍到 0 → delta 为正：{bl:?}");
        assert_eq!(al.self_count, 2);
        assert_eq!(bl.self_count, 1);
        assert!(al.label.contains("标签甲"), "诊断应显示实际标签名：{al:?}");
        assert!(bl.label.contains("标签乙"), "诊断应显示实际标签名：{bl:?}");
    }

    /// C-2：self_count==0 单独标注（区分「条件本身无效」与「与其他条件冲突」）。
    #[test]
    fn diagnostic_self_count_zero_is_distinguished() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let z = tag(&c, "scene", "无人标签");
        let a1 = insert_asset(&c, "d:/e1.jpg");
        asset_tags::assign(&c, &[a1], &[a], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![d_tag("scene", a), d_tag("scene", z)],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let leaves = diag_leaves(&c, &plan);
        let al = &leaves[0];
        let zl = &leaves[1];
        assert_eq!(zl.self_count, 0, "无人标签自身 0 命中");
        assert_eq!(zl.result_count, 0);
        assert!(zl.delta > 0, "移除无人标签后有结果");
        assert!(al.self_count > 0);
    }

    /// C-2：OR / NOT 下 delta 语义 —— OR 叶子 delta<0（贡献），NOT 叶子 delta>0。
    #[test]
    fn diagnostic_works_under_or_and_not() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let b = tag(&c, "scene", "标签乙");
        let mut a_ids = Vec::new();
        for i in 0..3 {
            let id = insert_asset(&c, &format!("d:/or_a{i}.jpg"));
            asset_tags::assign(&c, &[id], &[a], "manual").unwrap();
            a_ids.push(id);
        }
        let mut b_ids = Vec::new();
        for i in 0..2 {
            let id = insert_asset(&c, &format!("d:/or_b{i}.jpg"));
            asset_tags::assign(&c, &[id], &[b], "manual").unwrap();
            b_ids.push(id);
        }
        // OR：结果 5；乙贡献 → delta = without(甲 3) - result(5) = -2
        let or_plan = SearchPlanV3 {
            filter: Some(QueryExpr::Or {
                children: vec![d_tag("scene", a), d_tag("scene", b)],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let leaves = diag_leaves(&c, &or_plan);
        let bl = &leaves[1]; // [甲, 乙]
        assert_eq!(bl.self_count, 2);
        assert_eq!(bl.result_count, 5);
        assert!(bl.delta < 0, "OR 下乙贡献 → delta 为负：{bl:?}");
        // NOT（must_not 单叶子 B，需与甲有交集才能体现排除量）：新库单独构造
        let c2 = init_memory().unwrap();
        let a2 = tag(&c2, "scene", "标签甲");
        let b2 = tag(&c2, "scene", "标签乙");
        let _p1 = insert_asset(&c2, "d:/n1.jpg");
        let _p2 = insert_asset(&c2, "d:/n2.jpg");
        let p3 = insert_asset(&c2, "d:/n3.jpg");
        asset_tags::assign(&c2, &[_p1, _p2, p3], &[a2], "manual").unwrap();
        asset_tags::assign(&c2, &[p3], &[b2], "manual").unwrap(); // 甲 3 张，其中 1 张含乙
        let not_plan = SearchPlanV3 {
            filter: Some(d_tag("scene", a2)),
            must_not: Some(d_tag("scene", b2)),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let nl = diag_leaves(&c2, &not_plan);
        let bl2 = &nl[1]; // [filter甲, must_not乙]
        assert_eq!(bl2.self_count, 1, "乙单独命中 1：{bl2:?}");
        assert_eq!(bl2.delta, 1, "NOT 下乙排除了 1：{bl2:?}");
        assert!(
            bl2.label.contains("标签乙"),
            "排除诊断也应显示实际标签名：{bl2:?}"
        );
    }

    #[test]
    fn diagnostic_removes_leaf_wrapped_by_not() {
        let c = init_memory().unwrap();
        let excluded = tag(&c, "scene", "被排除标签");
        let a = insert_asset(&c, "d:/not_a.jpg");
        let _b = insert_asset(&c, "d:/not_b.jpg");
        asset_tags::assign(&c, &[a], &[excluded], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::Not {
                child: Box::new(d_tag("scene", excluded)),
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let leaves = diag_leaves(&c, &plan);
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].path, vec![0]);
        assert_eq!(leaves[0].result_count, 1, "NOT 应保留未命中的素材");
        assert_eq!(leaves[0].count_without_leaf, 2, "移除 Not 子叶后应恢复全库");
        assert_eq!(leaves[0].delta, 1);
    }

    /// C-2：should 显示「命中该加分项的素材数 / 结果总数」。
    #[test]
    fn diagnostic_should_shows_hit_ratio() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let s = tag(&c, "scene", "加分乙");
        let a1 = insert_asset(&c, "d:/f1.jpg");
        let a2 = insert_asset(&c, "d:/f2.jpg");
        asset_tags::assign(&c, &[a1, a2], &[a], "manual").unwrap();
        asset_tags::assign(&c, &[a1], &[s], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(d_tag("scene", a)),
            should: vec![ShouldClause {
                cond: LeafCond::Tag {
                    facet_key: "scene".into(),
                    tag_ids: vec![s],
                    mode: Some("any".into()),
                    include_descendants: true,
                    term_query: None,
                    term_match: crate::db::tags::TermMatch::Alias,
                },
                weight: 1.0,
                label: "加分乙（蓝天）".into(),
                evidence: None,
            }],
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let diag = diagnose_search_plan(&c, &plan, 7).unwrap();
        assert_eq!(diag.should.len(), 1);
        assert_eq!(diag.should[0].index, 0, "ShouldDiagnostic 带 index");
        assert_eq!(
            diag.should[0].plan_revision, 7,
            "ShouldDiagnostic 回显 plan_revision"
        );
        assert_eq!(diag.should[0].hit_count, 1, "命中 = 当前结果集内 ∩ 加分项");
        assert_eq!(diag.should[0].total_count, 2);
        assert!(
            diag.leaves.iter().all(|l| l.zone == "filter"),
            "叶子带 zone"
        );
    }

    // ═══════════════ Phase 2 契约测试（§4.1b 排除极性 / §4.2 prune / §4.3 校验 / B3 / B9） ═══════════════

    fn scene_tag_leaf(id: i64) -> LeafCond {
        LeafCond::Tag {
            facet_key: "scene".into(),
            tag_ids: vec![id],
            mode: Some("any".into()),
            include_descendants: true,
            term_query: None,
            term_match: crate::db::tags::TermMatch::Alias,
        }
    }
    fn leaf_expr(cond: LeafCond) -> QueryExpr {
        QueryExpr::Leaf { cond }
    }
    fn facet_mismatch_leaf(scene_tag_id: i64) -> LeafCond {
        // 声明 color 分面但标签属于 scene → filter_tags_by_facet 全剔 → 编译折叠 1=1
        LeafCond::Tag {
            facet_key: "color".into(),
            tag_ids: vec![scene_tag_id],
            mode: None,
            include_descendants: true,
            term_query: None,
            term_match: Default::default(),
        }
    }

    /// §4.1b：must_not 只放正向条件 —— must_not=Tag{夜景} 排除夜景素材（而不是只显示夜景）。
    #[test]
    fn must_not_single_exclusion_excludes_not_includes() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let night = tag(&c, "scene", "夜景");
        let a = insert_asset(&c, "d:/m1.jpg");
        let b = insert_asset(&c, "d:/m2.jpg");
        asset_tags::assign(&c, &[a, b], &[grass], "manual").unwrap();
        asset_tags::assign(&c, &[a], &[night], "manual").unwrap(); // a 是夜景
        let plan = SearchPlanV3 {
            filter: Some(leaf_expr(scene_tag_leaf(grass))),
            must_not: Some(leaf_expr(scene_tag_leaf(night))),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec![b], "排除夜景 → 只剩草地非夜景：{ids:?}");
    }

    /// §4.1b：must_not = Or([Tag A, Tag B]) ≡ NOT(A OR B) —— 多条排除是「任一命中即排除」。
    #[test]
    fn must_not_multiple_exclusions_are_or_semantics() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let sky = tag(&c, "scene", "蓝天");
        let night = tag(&c, "scene", "夜景");
        let a = insert_asset(&c, "d:/o1.jpg"); // 蓝天
        let b = insert_asset(&c, "d:/o2.jpg"); // 夜景
        let d = insert_asset(&c, "d:/o3.jpg"); // 都不命中排除
        for id in [a, b, d] {
            asset_tags::assign(&c, &[id], &[grass], "manual").unwrap();
        }
        asset_tags::assign(&c, &[a], &[sky], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[night], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(leaf_expr(scene_tag_leaf(grass))),
            must_not: Some(QueryExpr::Or {
                children: vec![
                    leaf_expr(scene_tag_leaf(sky)),
                    leaf_expr(scene_tag_leaf(night)),
                ],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec![d], "蓝天或夜景任一命中即排除：{ids:?}");
    }

    #[test]
    fn validate_rejects_exclude_tag_in_must_not() {
        let plan = SearchPlanV3 {
            must_not: Some(leaf_expr(LeafCond::ExcludeTag {
                facet_key: "scene".into(),
                tag_ids: vec![1],
            })),
            ..Default::default()
        };
        let e = validate_search_plan(&plan).unwrap_err();
        assert!(e.to_string().contains("双重否定"), "{e}");
    }

    #[test]
    fn validate_rejects_not_node_in_must_not() {
        let plan = SearchPlanV3 {
            must_not: Some(QueryExpr::Not {
                child: Box::new(leaf_expr(scene_tag_leaf(1))),
            }),
            ..Default::default()
        };
        assert!(validate_search_plan(&plan).is_err(), "三重否定必须拒绝");
        // 同一 Not 树在 filter 里仍然合法
        let ok_plan = SearchPlanV3 {
            filter: Some(QueryExpr::Not {
                child: Box::new(leaf_expr(scene_tag_leaf(1))),
            }),
            ..Default::default()
        };
        assert!(validate_search_plan(&ok_plan).is_ok());
    }

    #[test]
    fn validate_rejects_bad_weight() {
        let mut plan = tag_plan("scene", 1, &[("x", 2, 1.3)]); // 1.3 不在三档内
        assert!(validate_search_plan(&plan).is_err(), "1.3 必须被拒绝");
        plan.should[0].weight = f32::NAN;
        assert!(validate_search_plan(&plan).is_err(), "NaN 必须被拒绝");
        plan.should[0].weight = 0.5;
        assert!(validate_search_plan(&plan).is_ok(), "0.5 三档内合法");
    }

    #[test]
    fn validate_rejects_min_gt_should_len() {
        let mut plan = tag_plan("scene", 1, &[("x", 2, 1.0)]);
        plan.minimum_should_match = 5; // 只有 1 条加分
        assert!(validate_search_plan(&plan).is_err());
    }

    #[test]
    fn validate_rejects_bad_dir() {
        let plan = SearchPlanV3 {
            ranking: Ranking::Field {
                key: "taken_at".into(),
                dir: "sideways".into(),
            },
            ..Default::default()
        };
        assert!(validate_search_plan(&plan).is_err(), "非法方向必须拒绝");
    }

    #[test]
    fn validate_rejects_nonpositive_retriever_weight() {
        let mut plan = SearchPlanV3::default();
        plan.retrievers.retrievers.push(WeightedRetriever {
            weight: 0.0,
            kind: Retriever::Fts {
                query: "x".into(),
                scope: super::super::query_expr::SearchScope::All,
            },
        });
        assert!(validate_search_plan(&plan).is_err(), "0 权重必须拒绝");
        plan.retrievers.retrievers[0].weight = -1.0;
        assert!(validate_search_plan(&plan).is_err());
        plan.retrievers.retrievers[0].weight = 2.0;
        assert!(validate_search_plan(&plan).is_ok());
    }

    #[test]
    fn validate_rejects_future_schema_version() {
        let plan = SearchPlanV3 {
            plan_schema_version: PLAN_SCHEMA_VERSION + 1,
            ..Default::default()
        };
        let e = validate_search_plan(&plan).unwrap_err();
        assert!(e.to_string().contains("更新版本"), "{e}");
    }

    /// §4.2：must_not 里 1=1（无效条件）被整条删除而不是 NOT(1=1) 清库；warning 带区名。
    #[test]
    fn must_not_invalid_leaf_is_removed_not_negated() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let a = insert_asset(&c, "d:/p1.jpg");
        let b = insert_asset(&c, "d:/p2.jpg");
        asset_tags::assign(&c, &[a, b], &[grass], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(leaf_expr(scene_tag_leaf(grass))),
            // color 分面下塞 scene 的标签 → filter_tags_by_facet 全剔 → 1=1 → 必须删（否则 NOT(1=1) 清库）
            must_not: Some(leaf_expr(facet_mismatch_leaf(grass))),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let (pruned, warns) = prune_invalid(&c, &plan).unwrap();
        assert!(pruned.must_not.is_none(), "无效排除项应被整条删除");
        assert_eq!(
            serde_json::to_string(&pruned.filter).unwrap(),
            serde_json::to_string(&plan.filter).unwrap(),
            "filter 必须原样保留"
        );
        assert_eq!(warns.len(), 1);
        assert_eq!(
            warns[0].zone.as_deref(),
            Some("mustNot"),
            "warning 带区名：{warns:?}"
        );
        let out = run_search_plan(&c, &pruned, None, 0).unwrap();
        assert_eq!(out.len(), 2, "排除被剔除后不再清库");
    }

    /// §4.2：should 里 1=1 的整条加分被删除（否则每张都加分）；minimum_should_match 收敛。
    #[test]
    fn should_invalid_clause_is_removed_and_min_converges() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let sky = tag(&c, "scene", "蓝天");
        let a = insert_asset(&c, "d:/s1.jpg");
        asset_tags::assign(&c, &[a], &[grass, sky], "manual").unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        // 把加分项换成「跨分面标签」→ 编译折叠 1=1（恒真）→ 整条删 + min 收敛
        plan.should[0].cond = facet_mismatch_leaf(sky);
        plan.minimum_should_match = 1;
        let (pruned, warns) = prune_invalid(&c, &plan).unwrap();
        assert!(pruned.should.is_empty(), "恒真加分项应被整条删除");
        assert_eq!(pruned.minimum_should_match, 0, "min 收敛到新 should 长度");
        assert!(
            warns.iter().any(|w| w.zone.as_deref() == Some("should")),
            "{warns:?}"
        );
        // 执行不再报错且集合不受污染
        let out = run_search_plan(&c, &pruned, None, 0).unwrap();
        assert_eq!(out.len(), 1);
    }

    /// §4.2：filter 里的无效叶子被删除且保留其余条件；warning 区名 = filter。
    #[test]
    fn filter_invalid_leaf_is_removed_keeping_rest() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let a = insert_asset(&c, "d:/f1.jpg");
        let b = insert_asset(&c, "d:/f2.jpg");
        asset_tags::assign(&c, &[a, b], &[grass], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![
                    leaf_expr(scene_tag_leaf(grass)),
                    leaf_expr(facet_mismatch_leaf(grass)),
                ],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let (pruned, warns) = prune_invalid(&c, &plan).unwrap();
        let f = pruned.filter.expect("草地条件应保留");
        assert!(
            matches!(f, QueryExpr::Leaf { .. }),
            "无效叶子删除后只剩草地：{f:?}"
        );
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].zone.as_deref(), Some("filter"));
    }

    /// §4.5：诊断入口 prune 后与列表同批 —— 被剔除叶子不再被诊断，warnings 与列表同批。
    #[test]
    fn diagnose_uses_same_pruned_plan_and_omits_pruned_leaf() {
        let c = init_memory().unwrap();
        let grass = tag(&c, "scene", "草地");
        let a = insert_asset(&c, "d:/d1.jpg");
        asset_tags::assign(&c, &[a], &[grass], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![
                    leaf_expr(scene_tag_leaf(grass)),
                    leaf_expr(facet_mismatch_leaf(grass)),
                ],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let diag = diagnose_search_plan(&c, &plan, 3).unwrap();
        assert_eq!(diag.leaves.len(), 1, "无效叶子被剔除后不再诊断");
        assert_eq!(
            diag.leaves[0].plan_revision, 3,
            "LeafDiagnostic 回显 plan_revision"
        );
        assert_eq!(diag.leaves[0].zone, "filter");
        assert_eq!(diag.warnings.len(), 1, "诊断 warnings 与列表同批");
        let (pruned, pw) = prune_invalid(&c, &plan).unwrap();
        assert_eq!(diag.warnings, pw);
        assert_eq!(
            diag.leaves[0].result_count,
            count_plan(&c, &pruned).unwrap()
        );
    }

    /// §3.7 不变式 9：叶子诊断带 zone + plan_revision ——
    /// 「必须区第 0 条」与「排除区第 0 条」path 都是 [0]，仅凭 path 无法区分，
    /// zone 必须由后端打标；revision 供前端对在途旧诊断整批丢弃。
    #[test]
    fn leaf_diagnostic_carries_zone_and_revision() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let b = tag(&c, "scene", "标签丙");
        let x = tag(&c, "scene", "排除乙");
        let y = tag(&c, "scene", "排除丁");
        let hit = insert_asset(&c, "d:/z1.jpg");
        asset_tags::assign(&c, &[hit], &[a, b], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![leaf_expr(scene_tag_leaf(a)), leaf_expr(scene_tag_leaf(b))],
            }),
            must_not: Some(QueryExpr::Or {
                children: vec![leaf_expr(scene_tag_leaf(x)), leaf_expr(scene_tag_leaf(y))],
            }),
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let diag = diagnose_search_plan(&c, &plan, 42).unwrap();
        assert_eq!(diag.leaves.len(), 4);
        let mut fs: Vec<&LeafDiagnostic> =
            diag.leaves.iter().filter(|l| l.zone == "filter").collect();
        fs.sort_by_key(|l| l.path.clone());
        let f = fs[0];
        let m = diag
            .leaves
            .iter()
            .find(|l| l.zone == "mustNot")
            .expect("mustNot 叶子");
        assert_eq!(f.path, vec![0]);
        assert_eq!(m.path, vec![0], "两区同为下标 [0] 也能区分");
        assert_eq!(f.plan_revision, 42);
        assert_eq!(m.plan_revision, 42);
        assert!(f.result_count >= 0 && m.result_count >= 0);
    }

    /// B3：加分项命中数 = 当前结果集 ∩ 加分项（不是全库命中数）。
    #[test]
    fn should_hit_count_is_intersection_with_result_set() {
        let c = init_memory().unwrap();
        let a = tag(&c, "scene", "标签甲");
        let s = tag(&c, "scene", "加分乙");
        let a1 = insert_asset(&c, "d:/i1.jpg");
        let a2 = insert_asset(&c, "d:/i2.jpg");
        let outside = insert_asset(&c, "d:/outside.jpg"); // 有加分乙但不在 filter 结果里
        asset_tags::assign(&c, &[a1, a2], &[a], "manual").unwrap();
        asset_tags::assign(&c, &[a1], &[s], "manual").unwrap();
        asset_tags::assign(&c, &[outside], &[s], "manual").unwrap();
        let plan = SearchPlanV3 {
            filter: Some(d_tag("scene", a)),
            should: vec![ShouldClause {
                cond: scene_tag_leaf(s),
                weight: 1.0,
                label: "加分乙".into(),
                evidence: Some("最好有加分乙".into()),
            }],
            ranking: Ranking::Relevance,
            ..Default::default()
        };
        let diag = diagnose_search_plan(&c, &plan, 1).unwrap();
        assert_eq!(diag.should.len(), 1);
        assert_eq!(
            diag.should[0].hit_count, 1,
            "只有结果集内的 a1 算命中（outside 不算）：{:?}",
            diag.should
        );
        assert_eq!(diag.should[0].total_count, 2);
        assert!(diag.should[0].hit_count <= diag.should[0].total_count);
    }

    /// B9：字段排序为主键，score DESC 为次级 —— 同 rating 时命中加分项的排前面。
    #[test]
    fn field_ranking_uses_should_score_as_tiebreak() {
        let c = init_memory().unwrap();
        let (grass, sky) = (tag(&c, "scene", "草地"), tag(&c, "scene", "蓝天"));
        let a = insert_asset(&c, "d:/t1.jpg");
        let b = insert_asset(&c, "d:/t2.jpg");
        asset_tags::assign(&c, &[a], &[grass, sky], "manual").unwrap();
        asset_tags::assign(&c, &[b], &[grass], "manual").unwrap();
        c.execute("UPDATE assets SET rating = 3, taken_at = 1700000000000", [])
            .unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        plan.ranking = Ranking::Field {
            key: "rating".into(),
            dir: "desc".into(),
        };
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert_eq!(ids[0], a, "同 rating 时命中加分项的排前面：{ids:?}");
    }

    /// B9：字段排序主键优先于 score —— 不同 rating 时字段顺序不被加分打乱。
    #[test]
    fn field_ranking_primary_key_wins_over_score() {
        let c = init_memory().unwrap();
        let (grass, sky) = (tag(&c, "scene", "草地"), tag(&c, "scene", "蓝天"));
        let high = insert_asset(&c, "d:/h1.jpg");
        let low = insert_asset(&c, "d:/h2.jpg");
        asset_tags::assign(&c, &[high], &[grass], "manual").unwrap();
        asset_tags::assign(&c, &[low], &[grass, sky], "manual").unwrap(); // low 命中加分但 rating 低
        c.execute(
            "UPDATE assets SET rating = ?1 WHERE id = ?2",
            rusqlite::params![5, high],
        )
        .unwrap();
        c.execute(
            "UPDATE assets SET rating = ?1 WHERE id = ?2",
            rusqlite::params![1, low],
        )
        .unwrap();
        let mut plan = tag_plan("scene", grass, &[("蓝天", sky, 1.0)]);
        plan.ranking = Ranking::Field {
            key: "rating".into(),
            dir: "desc".into(),
        };
        let out = run_search_plan(&c, &plan, None, 0).unwrap();
        let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
        assert_eq!(
            ids[0], high,
            "rating 5 必须排前（加分不能压过主键）：{ids:?}"
        );
    }

    /// B2：plan 列表分页与全选 ID 同源同序 —— 第 1 页 = ids 前段，集合一致。
    #[test]
    fn plan_list_and_ids_return_same_set() {
        let c = init_memory().unwrap();
        let sky = tag(&c, "scene", "蓝天");
        let mut all = Vec::new();
        for i in 0..5 {
            let id = insert_asset(&c, &format!("d:/id_{i}.jpg"));
            asset_tags::assign(&c, &[id], &[sky], "manual").unwrap();
            all.push(id);
        }
        let plan = SearchPlanV3 {
            filter: Some(leaf_expr(scene_tag_leaf(sky))),
            ranking: Ranking::Field {
                key: "created_at".into(),
                dir: "asc".into(),
            },
            ..Default::default()
        };
        let ids_res = run_plan_ids(&c, &plan).unwrap();
        assert_eq!(ids_res.ids.len(), 5);
        assert!(!ids_res.truncated);
        assert_eq!(ids_res.total, 5);
        let page = run_plan_page(&c, &plan, 0, Some(3)).unwrap();
        assert_eq!(page.items.len(), 3);
        assert!(page.has_more);
        let page_ids: Vec<i64> = page.items.iter().map(|x| x.id).collect();
        assert_eq!(page_ids, ids_res.ids[0..3], "列表与全选同源同序");
        let page2 = run_plan_page(&c, &plan, 3, Some(3)).unwrap();
        let rest: Vec<i64> = page2.items.iter().map(|x| x.id).collect();
        assert_eq!(rest, ids_res.ids[3..]);
        assert!(!page2.has_more);
    }

    #[test]
    fn plan_ids_do_not_use_the_list_page_cap() {
        let c = init_memory().unwrap();
        for i in 0..1_001 {
            insert_asset(&c, &format!("d:/bulk_{i}.jpg"));
        }
        let result = run_plan_ids(&c, &SearchPlanV3::default()).unwrap();
        assert_eq!(result.total, 1_001);
        assert_eq!(
            result.ids.len(),
            1_001,
            "全选 ID 不得被列表 1000 条限制截断"
        );
        assert!(!result.truncated);
    }

    /// §4.2：空 plan 三区全空也可执行（返回全库无 filter = 直接列表），这里只验证不 panic。
    #[test]
    fn prune_and_execute_empty_plan_is_stable() {
        let c = init_memory().unwrap();
        let _a = insert_asset(&c, "d:/z1.jpg");
        let plan = SearchPlanV3::default();
        let (pruned, warns) = prune_invalid(&c, &plan).unwrap();
        assert!(warns.is_empty());
        let out = run_search_plan(&c, &pruned, Some(10), 0).unwrap();
        assert_eq!(out.len(), 1);
    }

    // ═══════════════ V24（§7-2）：LeafCond::FacetNumber ═══════════════

    fn num_leaf(facet: &str, op: &str, v: f64, mv: Option<f64>) -> LeafCond {
        LeafCond::FacetNumber {
            facet_key: facet.into(),
            op: op.into(),
            value: v,
            max_value: mv,
        }
    }

    /// 数值分面夹具：「人数」0–50（与 v24_numbers::number_facet_setup 同语义）
    fn number_facet(c: &Connection) {
        crate::db::tag_facets::create(c, "people_count", "人数", "", "single", None, "all")
            .unwrap();
        c.execute(
            "UPDATE tag_facets SET facet_kind='number', num_min=0, num_max=50, num_unit='人',
             num_decimals=0, num_step=1 WHERE key='people_count'",
            [],
        )
        .unwrap();
    }

    /// 唯一 file_name 的素材插入（通用 insert_asset 的 file_name 恒为 "a.jpg"，
    /// 会触发 set_facet_number 的同源扇出把值写到所有素材上 —— 数值测试必须唯一）。
    fn insert_asset_named(c: &Connection, path: &str, name: &str) -> i64 {
        assets::insert(c, path, name, "jpg", 1024, "image/jpeg", 1700000000000).unwrap()
    }

    /// 编译层基础：gte / eq / between 三种 op 各自正确过滤（EXISTS 对 asset_facet_numbers）。
    #[test]
    fn facet_number_leaf_compiles_and_filters() {
        let c = init_memory().unwrap();
        number_facet(&c);
        let a = insert_asset_named(&c, "d:/n1.jpg", "n1.jpg");
        let b = insert_asset_named(&c, "d:/n2.jpg", "n2.jpg");
        let _d = insert_asset_named(&c, "d:/n3.jpg", "n3.jpg");
        crate::db::facet_numbers::set_facet_number(&c, &[a], "people_count", 5.0).unwrap();
        crate::db::facet_numbers::set_facet_number(&c, &[b], "people_count", 20.0).unwrap();
        // d 无值
        let run = |leaf: LeafCond| -> Vec<i64> {
            let plan = SearchPlanV3 {
                filter: Some(QueryExpr::Leaf { cond: leaf }),
                ..Default::default()
            };
            run_search_plan(&c, &plan, None, 0)
                .unwrap()
                .into_iter()
                .map(|x| x.0)
                .collect()
        };
        assert_eq!(
            run(num_leaf("people_count", "gte", 10.0, None)),
            vec![b],
            "≥10 → 只有 20 的"
        );
        assert_eq!(
            run(num_leaf("people_count", "eq", 5.0, None)),
            vec![a],
            "=5 → 只有 5 的"
        );
        let between_ids = run(num_leaf("people_count", "between", 4.0, Some(20.0)));
        assert_eq!(
            between_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [a, b]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "4~20 → 两个都有"
        );
        assert!(
            run(num_leaf("people_count", "gte", 100.0, None)).is_empty(),
            "越界 → 0 结果"
        );
    }

    /// §4.1b 极性白名单：FacetNumber 是正向谓词，进 must_not 语义 =「排除满足它的」
    /// （NOT(值≥10) 保留 5 的那张，与 ExcludeTag 双重否定 P0 划清界限）。
    #[test]
    fn facet_number_in_must_not_is_positive_predicate() {
        let c = init_memory().unwrap();
        number_facet(&c);
        let a = insert_asset_named(&c, "d:/m1.jpg", "m1.jpg");
        let b = insert_asset_named(&c, "d:/m2.jpg", "m2.jpg");
        crate::db::facet_numbers::set_facet_number(&c, &[a], "people_count", 5.0).unwrap();
        crate::db::facet_numbers::set_facet_number(&c, &[b], "people_count", 20.0).unwrap();
        let plan = SearchPlanV3 {
            must_not: Some(QueryExpr::Leaf {
                cond: num_leaf("people_count", "gte", 10.0, None),
            }),
            ..Default::default()
        };
        assert!(
            validate_search_plan(&plan).is_ok(),
            "正向数值谓词必须允许进 must_not"
        );
        let ids: Vec<i64> = run_search_plan(&c, &plan, None, 0)
            .unwrap()
            .into_iter()
            .map(|x| x.0)
            .collect();
        assert_eq!(ids, vec![a], "must_not=人数≥10 → 保留 5 的、排除 20 的");
    }

    /// §4.2 剔除链：分面不存在/不是数值型 → 整叶剔除 + warning，不影响其余条件。
    #[test]
    fn facet_number_invalid_facet_pruned_with_warning() {
        let c = init_memory().unwrap();
        number_facet(&c);
        let a = insert_asset_named(&c, "d:/p1.jpg", "p1.jpg");
        crate::db::facet_numbers::set_facet_number(&c, &[a], "people_count", 5.0).unwrap();
        // 情形一：分面根本不存在
        let plan = SearchPlanV3 {
            filter: Some(QueryExpr::And {
                children: vec![
                    QueryExpr::Leaf {
                        cond: num_leaf("ghost_facet", "gte", 1.0, None),
                    },
                    QueryExpr::Leaf {
                        cond: num_leaf("people_count", "gte", 1.0, None),
                    },
                ],
            }),
            ..Default::default()
        };
        let (pruned, warns) = prune_invalid(&c, &plan).unwrap();
        assert!(
            warns.iter().any(|w| w.message.contains("ghost_facet")),
            "必须有剔除 warning：{warns:?}"
        );
        let ids: Vec<i64> = run_search_plan(&c, &pruned, None, 0)
            .unwrap()
            .into_iter()
            .map(|x| x.0)
            .collect();
        assert_eq!(ids, vec![a], "剔除无效叶后剩余条件照常生效");
        // 情形二：分面存在但仍是标签型（facet_kind != 'number'）
        let plan2 = SearchPlanV3 {
            filter: Some(QueryExpr::Leaf {
                cond: num_leaf("scene", "gte", 1.0, None),
            }),
            ..Default::default()
        };
        let (_, warns2) = prune_invalid(&c, &plan2).unwrap();
        assert!(
            warns2.iter().any(|w| w.message.contains("scene")),
            "标签型分面做数值条件必须剔除并 warning：{warns2:?}"
        );
    }
}
