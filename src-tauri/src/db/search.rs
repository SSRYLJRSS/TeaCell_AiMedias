//! 搜索路由策略（fix-plan-2026-08-14 BUG-A/B/D 查询侧，超级搜索 P1A 改造为库内组合）：
//! - ≤2 字：LIKE 子串兜底（转义 % _ \，标签名走 EXISTS 子查询）
//! - 含非 CJK 字符且 >2 字（BUG-A/B）：FTS 短语 ∪ LIKE 子串并集，保证子串语义
//!   （FTS 命中 token 整词/跨标签边界；LIKE 命中 token 内部子串，取并集才完整）
//! - 纯 CJK ≥4 字（BUG-D）：2 字块 AND，顺序无关（只要各 2 字块在索引中各自相邻即命中）
//! - 纯 CJK 3 字：FTS 短语（防误命中，如「上海湖」不命中「上海公园湖边」）
//!
//! FB5-05（§8.2/§8.3）：SearchScope 化。所有路径都接收 scope：
//! - all：无列限制（三列：file_name + tag_names + content_description）
//! - content：{file_name content_description}
//! - description：仅 content_description
//! - fileName：仅 file_name
//!   列名只能由 SearchScope 枚举映射，绝不能来自模型或用户字符串；LIKE 参数数量按 scope 精确生成。
//!
//! 关键约束（架构 v1.3 §1.5，均有实测依据）：
//! - 短语必须加双引号，否则空格被当 AND，「海边」会误命中「上海…湖边」；
//! - LIKE 需转义 % _ \，标签名走 EXISTS 子查询避免 N+1。
//!
//! P1A 改造：不再把全部命中 ID 拉回 Rust 拼长 IN 列表，而是编译为可嵌入 WHERE 的
//! SQL 谓词（字符串 + 参数），在数据库内与其他条件组合。search_asset_ids 保留为
//! 兼容工具（供既有测试与少量全选场景使用），但不再作为 list / list_ids 的主链路。

use rusqlite::types::Value;
use rusqlite::Connection;

use super::libraries::VERIFIED_ASSET_SCOPE;
use super::query_expr::SearchScope;
use crate::error::AppResult;
use crate::utils::bigram::{cjk_bigram, is_cjk};

/// 编译好的搜索谓词：可嵌入 WHERE 的 sql 片段 + 位置参数。谓词以 `a` 作为素材表别名。
#[derive(Debug, Clone)]
pub struct SearchPredicate {
    pub sql: String,
    pub params: Vec<Value>,
}

impl SearchPredicate {
    /// 空谓词（无键词时使用），嵌入 WHERE 不产生条件。
    pub fn empty() -> Self {
        Self {
            sql: String::new(),
            params: Vec::new(),
        }
    }
}

/// FB5-05（§8.3）：FTS 列前缀。列名只由 scope 映射，绝不来自外部输入。
/// - all：`"银 杏"`（裸短语，搜全部列）
/// - content：`{file_name content_description}:"银 杏"`
/// - description：`content_description:"银 杏"`
/// - fileName：`file_name:"银 杏"`
pub fn scoped_fts_term(scope: SearchScope, quoted_term: &str) -> String {
    match scope {
        SearchScope::All => quoted_term.to_string(),
        SearchScope::Content => format!("{{file_name content_description}}:{quoted_term}"),
        SearchScope::Description => format!("content_description:{quoted_term}"),
        SearchScope::FileName => format!("file_name:{quoted_term}"),
    }
}

/// 将用户关键词编译为一个可嵌入 WHERE 的搜索谓词。返回 None 表示无搜索。
/// 该谓词引用外层素材表别名 `a`，子查询/JOIN 均在其内部完成，避免在 Rust 端拼 ID 列表。
pub fn build_search_predicate(
    conn: &Connection,
    raw: &str,
    scope: SearchScope,
) -> AppResult<Option<SearchPredicate>> {
    let q = raw.trim().to_lowercase();
    if q.is_empty() {
        return Ok(None);
    }
    let n = q.chars().count();
    if n <= 2 {
        return Ok(Some(like_predicate(&q, scope)));
    }
    // 含非 CJK（ASCII 字母/数字/符号）→ FTS ∪ LIKE 并集（BUG-A/B 子串语义）
    if q.chars().any(|c| !is_cjk(c)) {
        return Ok(Some(union_predicate(conn, &q, scope)?));
    }
    // 纯 CJK ≥4 字 → 2 字块 AND，顺序无关（BUG-D）
    if n >= 4 {
        return Ok(Some(chunk_and_predicate(conn, &q, scope)?));
    }
    // 纯 CJK 3 字 → 短语（防误命中，如「上海湖」不命中「上海公园湖边」）
    Ok(Some(fts_phrase_predicate(conn, &q, scope)?))
}

/// FTS5 短语谓词：整串相邻，作为精确层（带 scope 列前缀）。
fn fts_phrase_predicate(
    _conn: &Connection,
    q: &str,
    scope: SearchScope,
) -> AppResult<SearchPredicate> {
    let expanded = cjk_bigram(q);
    let phrase = format!("\"{}\"", expanded.replace('"', "\"\""));
    let term = scoped_fts_term(scope, &phrase);
    Ok(SearchPredicate {
        sql: "a.id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?)".into(),
        params: vec![Value::Text(term)],
    })
}

/// 纯 CJK ≥4 字：按 2 字切块，每块经 cjk_bigram 成「字 字」短语，块间 AND。
/// 每个 chunk 都加对应 scope 列前缀（不能只在整串最前面加一次，§8.3）。
fn chunk_and_predicate(
    _conn: &Connection,
    q: &str,
    scope: SearchScope,
) -> AppResult<SearchPredicate> {
    let chars: Vec<char> = q.chars().collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() {
            let pair: String = chars[i..i + 2].iter().collect();
            chunks.push(cjk_bigram(&pair));
            i += 2;
        } else {
            chunks.push(chars[i].to_string());
            i += 1;
        }
    }
    let query = chunks
        .iter()
        .map(|c| {
            let quoted = format!("\"{}\"", c.replace('"', "\"\""));
            scoped_fts_term(scope, &quoted)
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    Ok(SearchPredicate {
        sql: "a.id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?)".into(),
        params: vec![Value::Text(query)],
    })
}

/// FB5-05（§8.3）：LIKE 谓词，按 scope 精确生成列与参数数量：
/// - all：file_name OR content_description OR 规范标签/可搜索别名（EXISTS 子查询，4 参数）
/// - content：file_name OR content_description（2 参数）
/// - description：仅 content_description（1 参数）
/// - fileName：仅 file_name（1 参数）
fn like_predicate(q: &str, scope: SearchScope) -> SearchPredicate {
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let like = |cols: &[&str]| -> String {
        cols.iter()
            .map(|c| format!("a.{c} LIKE ? ESCAPE '\\'"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let sql = match scope {
        SearchScope::All => format!(
            "({} OR EXISTS (
                SELECT 1 FROM asset_tags at
                  JOIN tags t ON t.id = at.tag_id
                  LEFT JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
                WHERE at.asset_id = a.id
                  AND t.status = 'active'
                  AND EXISTS (SELECT 1 FROM tag_facets f
                              WHERE f.key = t.facet_key AND f.status = 'active')
                  AND (t.name LIKE ? ESCAPE '\\' OR ta.alias LIKE ? ESCAPE '\\')
            ))",
            like(&["file_name", "content_description"])
        ),
        SearchScope::Content => format!("({})", like(&["file_name", "content_description"])),
        SearchScope::Description => format!("({})", like(&["content_description"])),
        SearchScope::FileName => format!("({})", like(&["file_name"])),
    };
    // 参数数量 = 列数（每个 LIKE 一个 pattern）+ all 额外 2 个（标签名 + 别名）
    let mut params: Vec<Value> = Vec::new();
    let col_count = match scope {
        SearchScope::All => 2,
        SearchScope::Content => 2,
        SearchScope::Description => 1,
        SearchScope::FileName => 1,
    };
    for _ in 0..col_count {
        params.push(Value::Text(pattern.clone()));
    }
    if scope == SearchScope::All {
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    SearchPredicate { sql, params }
}

/// FTS ∪ LIKE 并集谓词（子串语义）：两条都编译为库内子查询，UNION 由 OR 表达。
fn union_predicate(conn: &Connection, q: &str, scope: SearchScope) -> AppResult<SearchPredicate> {
    let fts = fts_phrase_predicate(conn, q, scope)?;
    let like = like_predicate(q, scope);
    Ok(SearchPredicate {
        sql: format!("({} OR {})", fts.sql, like.sql),
        params: {
            let mut all = fts.params;
            all.extend(like.params);
            all
        },
    })
}

/// 兼容工具：返回当前搜索命中素材 id 列表。
/// 仅供既有测试与少数「全量 id」场景使用；list / list_ids 走 build_search_predicate 库内组合。
pub fn search_asset_ids(conn: &Connection, raw: &str, scope: SearchScope) -> AppResult<Vec<i64>> {
    let Some(pred) = build_search_predicate(conn, raw, scope)? else {
        return Ok(Vec::new());
    };
    let sql = format!(
        "SELECT a.id FROM assets a WHERE ({}) AND {VERIFIED_ASSET_SCOPE}",
        pred.sql
    );
    let mut stmt = conn.prepare(&sql)?;
    let ids = stmt
        .query_map(rusqlite::params_from_iter(pred.params.iter()), |r| r.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

/// 兼容便捷函数：普通素材库语义（默认范围 = all 三列）。供既有测试/工具调用。
pub fn search_asset_ids_all(conn: &Connection, raw: &str) -> AppResult<Vec<i64>> {
    search_asset_ids(conn, raw, SearchScope::All)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn seed(conn: &rusqlite::Connection, file_name: &str, desc: &str) -> i64 {
        conn.execute(
            "UPDATE libraries
                SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                    library_epoch=lower(hex(randomblob(16)))
              WHERE singleton=1 AND health='unconfigured'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO assets (file_path, file_name, content_description, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES (?1, ?2, ?3, '.jpg', 10, 'image/jpeg', 1, 1)",
            rusqlite::params![format!("/seed/{file_name}"), file_name, desc],
        )
        .unwrap();
        let id = conn
            .query_row(
                "SELECT id FROM assets WHERE file_name = ?1",
                [file_name],
                |r| r.get(0),
            )
            .unwrap();
        let library = crate::db::libraries::current(conn).unwrap().unwrap();
        conn.execute(
            "UPDATE managed_asset_locations
                SET status='verified',reason='verified',library_id=?1,library_epoch=?2,
                    relative_path=printf('asset-%d.jpg',asset_id),
                    file_identity='fixture-file',content_sha256=?3,
                    ownership_receipt='fixture-receipt'
              WHERE asset_id=?4",
            rusqlite::params![
                library.library_id,
                library.library_epoch,
                "a".repeat(64),
                id
            ],
        )
        .unwrap();
        id
    }

    #[test]
    fn like_params_follow_scope() {
        let pred = like_predicate("夜", SearchScope::All);
        assert_eq!(
            pred.params.len(),
            4,
            "all：file_name + description + tag + alias"
        );
        let pred = like_predicate("夜", SearchScope::Content);
        assert_eq!(pred.params.len(), 2, "content：file_name + description");
        let pred = like_predicate("夜", SearchScope::Description);
        assert_eq!(pred.params.len(), 1, "description：仅一列");
        let pred = like_predicate("夜", SearchScope::FileName);
        assert_eq!(pred.params.len(), 1, "fileName：仅一列");
        // 参数全部是同一 pattern
        assert!(pred.params.iter().all(|v| v == &Value::Text("%夜%".into())));
    }

    #[test]
    fn fts_term_scopes_columns() {
        assert_eq!(scoped_fts_term(SearchScope::All, "\"银 杏\""), "\"银 杏\"");
        assert_eq!(
            scoped_fts_term(SearchScope::Content, "\"银 杏\""),
            "{file_name content_description}:\"银 杏\""
        );
        assert_eq!(
            scoped_fts_term(SearchScope::Description, "\"银 杏\""),
            "content_description:\"银 杏\""
        );
        assert_eq!(
            scoped_fts_term(SearchScope::FileName, "\"银 杏\""),
            "file_name:\"银 杏\""
        );
    }

    #[test]
    fn description_scope_hits_description_only() {
        let conn = init_memory().unwrap();
        // 文件名不含关键词，描述含关键词
        seed(&conn, "IMG_0001.jpg", "夜晚树下多人合影");
        seed(&conn, "IMG_0002.jpg", "");
        let ids = search_asset_ids(&conn, "夜景", SearchScope::Description).unwrap();
        assert!(ids.is_empty(), "「夜景」不在任何描述中");
        let ids = search_asset_ids(&conn, "夜晚", SearchScope::Description).unwrap();
        assert_eq!(ids.len(), 1, "「夜晚」只命中描述含它的素材");
        // content scope：文件名 OR 描述
        let ids = search_asset_ids(&conn, "IMG_0001", SearchScope::Content).unwrap();
        assert_eq!(ids.len(), 1, "content scope 也搜文件名");
        // fileName scope：只搜文件名
        let ids = search_asset_ids(&conn, "夜晚", SearchScope::FileName).unwrap();
        assert!(ids.is_empty(), "fileName scope 不搜描述");
    }

    #[test]
    fn all_scope_hits_description_and_file_name() {
        let conn = init_memory().unwrap();
        seed(&conn, "IMG_0001.jpg", "夜晚树下多人合影");
        let by_desc = search_asset_ids(&conn, "合影", SearchScope::All).unwrap();
        assert_eq!(by_desc.len(), 1, "all scope 命中描述");
        let by_name = search_asset_ids(&conn, "img_0001", SearchScope::All).unwrap();
        assert_eq!(by_name.len(), 1, "all scope 命中文件名");
    }

    #[test]
    fn two_char_scope_routes_to_like() {
        // ≤2 字必须走 LIKE：纯 FTS 无法命中「夜景」（描述 bigram 后逐字切分）
        let conn = init_memory().unwrap();
        seed(&conn, "IMG_0001.jpg", "夜景人像");
        let ids = search_asset_ids(&conn, "夜景", SearchScope::Description).unwrap();
        assert_eq!(ids.len(), 1, "两字描述经 LIKE 命中");
    }
}
