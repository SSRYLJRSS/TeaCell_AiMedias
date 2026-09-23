//! 数据层入口：连接初始化（WAL + foreign_keys + cjk_bigram 注册 + 迁移）

pub mod ai;
pub mod ai_connections;
pub mod asset_tags;
pub mod assets;
pub mod backup;
pub mod dedup;
pub mod export;
pub mod facet_numbers;
pub mod migrations;
pub mod palette_bucket;
pub mod query_expr;
pub mod reset;
pub mod schema_features;
pub mod search;
pub mod search_plan;
pub mod search_query;
pub mod settings;
mod sql_utils;
pub mod tag_facets;
pub mod tag_ops;
pub mod tags;
pub mod video_proxy;

use std::path::Path;

use rusqlite::Connection;

use crate::error::AppResult;
use crate::utils::bigram;

fn configure(conn: &Connection) -> AppResult<()> {
    // CASCADE 删除依赖外键开关（rusqlite 默认关闭）
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    bigram::register(conn)?;
    Ok(())
}

/// 标签数据保护属于应用内部约束，正常启动时自动检查并启用。
/// 已有历史冲突时不强行建约束，记录阻断原因后继续启动，避免应用无法打开。
pub fn ensure_required_tag_constraints(
    conn: &Connection,
) -> AppResult<Option<tags::TagConflictReport>> {
    let features = [
        "tag_unique_terms",
        "tag_facet_fk",
        "tag_facet_restrict_delete",
    ];
    let mut all_enabled = true;
    for feature in features {
        if !schema_features::feature_enabled(conn, feature)? {
            all_enabled = false;
            break;
        }
    }
    if all_enabled {
        return Ok(None);
    }

    let report = tags::detect_tag_conflicts(conn)?;
    if !report.is_clean() {
        let blocked_by = format!("data_conflict:{}", report.total());
        for feature in features {
            schema_features::set_feature(conn, feature, false, Some(&blocked_by))?;
        }
        return Ok(Some(report));
    }

    migrations::apply_v22b_constraints(conn)?;
    for feature in features {
        schema_features::set_feature(conn, feature, true, None)?;
    }
    migrations::rebuild_fts_triggers_for_gated_terms(conn)?;
    Ok(None)
}

/// 打开（必要时创建）指定路径的库并完成迁移
pub fn init(path: &Path) -> AppResult<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrations::migrate(&conn)?;
    // 自愈兜底：历史「重置标签」清空 tag_facets 且未补种的库，启动时重建系统分面
    tag_facets::seed_system_facets_if_empty(&conn)?;
    // F1-e：schema_features 声称的能力与 DB 实际结构对齐（漂移则修正登记表 + warn）
    match schema_features::verify_schema_features(&conn) {
        Ok(diffs) => {
            for d in &diffs {
                tracing::warn!("schema_features 自检差异（已按实际修正）: {d}");
            }
            if diffs.is_empty() {
                tracing::debug!("schema_features 自检通过");
            }
        }
        Err(e) => tracing::warn!("schema_features 自检失败（不阻断启动）: {e}"),
    }
    match ensure_required_tag_constraints(&conn) {
        Ok(Some(report)) => tracing::warn!(
            "标签数据保护未启用：检测到 {} 处历史数据冲突，需先处理数据",
            report.total()
        ),
        Ok(None) => tracing::debug!("标签数据保护已启用"),
        Err(e) => return Err(e),
    }
    Ok(conn)
}

/// 内存库（单元测试用）
pub fn init_memory() -> AppResult<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrations::migrate(&conn)?;
    tag_facets::seed_system_facets_if_empty(&conn)?;
    let _ = schema_features::verify_schema_features(&conn);
    Ok(conn)
}

/// 生产启动和恢复库后的统一补齐入口：
/// 只升级未改过的系统分面默认值；标签表为空时才播种核心层级词表。
pub fn ensure_default_taxonomy(conn: &Connection) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    tag_facets::refresh_system_facet_defaults(conn, now)?;
    tag_facets::seed_system_facets_if_empty(conn)?;
    tags::seed_core_taxonomy_if_empty(conn)?;
    tags::ensure_core_taxonomy_aliases(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_FEATURES: [&str; 3] = [
        "tag_unique_terms",
        "tag_facet_fk",
        "tag_facet_restrict_delete",
    ];

    #[test]
    fn init_bootstraps_required_tag_constraints_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");

        {
            let conn = init(&path).unwrap();
            for feature in REQUIRED_FEATURES {
                assert!(
                    schema_features::feature_enabled(&conn, feature).unwrap(),
                    "{feature} 应在首次初始化时自动启用"
                );
            }
            assert!(schema_features::verify_schema_features(&conn)
                .unwrap()
                .is_empty());
        }

        let conn = init(&path).unwrap();
        for feature in REQUIRED_FEATURES {
            assert!(
                schema_features::feature_enabled(&conn, feature).unwrap(),
                "{feature} 在重复初始化后应保持启用"
            );
        }
    }

    #[test]
    fn bootstrap_records_conflict_without_forcing_constraints() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        let conn = init(&path).unwrap();

        conn.execute_batch(
            "DROP TABLE tag_terms;
             INSERT INTO tags (name, normalized_name, facet_key)
             VALUES ('海边', '海边', 'scene'), ('海边', '海边', 'scene');",
        )
        .unwrap();
        schema_features::verify_schema_features(&conn).unwrap();

        let report = ensure_required_tag_constraints(&conn)
            .unwrap()
            .expect("存在重名标签时应返回冲突报告");
        assert!(!report.term_conflicts.is_empty());
        for feature in REQUIRED_FEATURES {
            assert!(!schema_features::feature_enabled(&conn, feature).unwrap());
        }
        let blocked: String = conn
            .query_row(
                "SELECT blocked_by FROM schema_features WHERE feature='tag_unique_terms'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(blocked.starts_with("data_conflict:"));
    }
}
