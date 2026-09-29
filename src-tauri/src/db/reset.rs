//! 分类重置应用数据（设置页「存储与维护 → 重置数据」）
//! 原则：原始素材文件由 commands 层在锁外按删除结果逐项处理；本模块只负责
//! 数据库记录、派生缓存和日志等软件数据，避免“清素材库”误删用户文件。
//! FTS 一致性依赖既有触发器（删 assets / asset_tags / tag_aliases 会同步 fts_content），
//! 外键 CASCADE 已开启（db::configure），删主表即可级联清理关联表。

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

/// 要重置的数据分类（前端勾选传入；false = 保留）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResetSelection {
    /// 素材库记录（assets + 搜索索引 + 导出任务 + 视频代理记录）
    pub assets: bool,
    /// 原始素材文件（由 commands 层锁外删除；成功项同时删除对应记录）
    pub asset_files: bool,
    /// 导出任务历史（不影响素材记录）
    pub export_tasks: bool,
    /// 标签与分类（tags / tag_facets / tag_aliases / tag_ops）
    pub tags: bool,
    /// AI 打标任务（批次 + 建议 + 词条级建议）
    pub ai_tasks: bool,
    /// AI 服务配置（连接档案 + 用途绑定；keyring 清理由 commands 在 DB 锁外协调）
    pub ai_connections: bool,
    /// 偏好设置（settings 表恢复默认）
    pub preferences: bool,
    /// 本地搜索条件与最近使用字段（实际存储在前端 localStorage）
    pub search_state: bool,
    /// 缓存文件（thumbnails/ previews/ proxies/ 目录）
    pub caches: bool,
    /// 诊断日志文件（不影响数据库和素材）
    pub logs: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetReport {
    pub assets_deleted: i64,
    pub asset_files_deleted: u64,
    pub asset_files_failed: u64,
    pub export_tasks_deleted: i64,
    pub tags_deleted: i64,
    pub ai_tasks_deleted: i64,
    pub connections_deleted: i64,
    pub preferences_reset: bool,
    pub search_state_reset: bool,
    pub cache_files_deleted: u64,
    pub log_files_deleted: u64,
}

impl ResetSelection {
    pub fn any(&self) -> bool {
        self.assets
            || self.asset_files
            || self.export_tasks
            || self.tags
            || self.ai_tasks
            || self.ai_connections
            || self.preferences
            || self.search_state
            || self.caches
            || self.logs
    }
}

/// 执行分类重置。DB 变更在单事务内完成；缓存文件在事务提交后删除（失败不回滚 DB，
/// 只在报告中如实计数——缓存文件是派生数据，下次浏览会按需重建）。
pub fn reset(
    conn: &mut Connection,
    data_dir: &Path,
    sel: &ResetSelection,
) -> AppResult<ResetReport> {
    let (report, clear_cache_db, clear_logs) = reset_db(conn, sel)?;
    finish_reset(data_dir, report, clear_cache_db, clear_logs)
}

/// Perform only transactional database mutations. The caller must drop its
/// `Database` guard before invoking `finish_reset`, which removes cache/log files.
pub fn reset_db(
    conn: &mut Connection,
    sel: &ResetSelection,
) -> AppResult<(ResetReport, bool, bool)> {
    let mut report = ResetReport::default();
    let tx = conn.transaction()?;

    // 导出任务：可单独清理；选择“素材库记录”时一并清理，保持原有语义。
    if sel.assets || sel.export_tasks {
        report.export_tasks_deleted = tx.execute("DELETE FROM export_tasks", [])? as i64;
    }

    // 素材库：先删派生记录再删主表（FK 级联 asset_tags / ai_suggestions / video_proxies，
    // 触发器同步清 fts_content + assets_fts，不留幻影命中）。原始文件删除由 commands 层
    // 先完成；若同时勾选原文件，commands 层会关闭此全表分支以保留删除失败的记录。
    if sel.assets {
        tx.execute("DELETE FROM video_proxies", [])?;
        tx.execute("DELETE FROM assets", [])?;
        report.assets_deleted = tx.changes() as i64;
    }

    // 标签与分类：tag_ops 是打标流水，随标签一起清才有「全新开始」的语义。
    // 用户标签/分面清空，但系统分面必须重建——否则 AI 提示词无分面上下文（build_prompt_context
    // 跳过库里不存在的分面），模型返回的一切都会归到 custom（曾导致「只打出 custom 标」的线上事故）。
    if sel.tags {
        // 先计数：后续 DELETE 的 changes() 只会表示最后一条 SQL，不能代表标签删除量。
        let tags_deleted = tx.execute("DELETE FROM tags", [])? as i64;
        report.tags_deleted = tags_deleted;
        // 兼容历史库中的孤儿流水；正常库中它们已由 tags 外键级联清除。
        tx.execute("DELETE FROM tag_ops", [])?;
        // 数值分面值没有 tags 外键，且分面删除保护会检查此表，必须先清。
        tx.execute("DELETE FROM asset_facet_numbers", [])?;
        tx.execute("DELETE FROM tag_facets", [])?;
        super::tag_facets::seed_system_facets(&tx)?;
        super::tags::seed_core_taxonomy(&tx)?;
        // V16 语义：color 分面停用（颜色由算法主色呈现，AI 侧摘除）
        let now = chrono::Utc::now().timestamp_millis();
        tx.execute(
            "UPDATE tag_facets SET status = 'inactive', updated_at = ?1 WHERE key = 'color'",
            rusqlite::params![now],
        )?;
    }

    // AI 打标任务：batches → suggestions → suggestion_items 级联，逐层删确保无残留
    if sel.ai_tasks {
        tx.execute("DELETE FROM ai_suggestion_items", [])?;
        tx.execute("DELETE FROM ai_suggestions", [])?;
        tx.execute("DELETE FROM ai_batches", [])?;
        report.ai_tasks_deleted = tx.changes() as i64;
    }

    // AI 服务配置：keyring 不属于数据库事务，外层 command 已在拿 DB guard 之前
    // 读取凭据引用并完成可补偿删除；本层只负责事务内删除绑定与档案行。
    if sel.ai_connections {
        tx.execute("DELETE FROM ai_usage_bindings", [])?;
        tx.execute("DELETE FROM ai_connections", [])?;
        report.connections_deleted = tx.changes() as i64;
    }

    // 偏好设置：settings 是单 key JSON（app_settings），整表清空后 get_settings 走默认值
    if sel.preferences {
        tx.execute("DELETE FROM settings", [])?;
        tx.execute("DELETE FROM cloud_accounts", [])?;
        report.preferences_reset = true;
    }

    // 搜索条件与最近使用字段保存在 localStorage，由前端在数据库事务成功后清除；
    // 这里返回标记，便于前端与后端选择保持同一份结果报告。
    report.search_state_reset = sel.search_state;

    // 缓存文件对应的 DB 字段/记录：缓存被清时必须回写，否则 DB 指向已删文件导致破图（B27 语义）
    let clear_cache_db = sel.caches || sel.assets || sel.asset_files;
    if clear_cache_db {
        tx.execute(
            "UPDATE assets SET placeholder_path = NULL, hd_thumbnail_path = NULL",
            [],
        )?;
        tx.execute("DELETE FROM video_proxies", [])?;
    }

    tx.commit()?;
    Ok((report, clear_cache_db, sel.logs))
}

/// Complete post-commit filesystem cleanup without holding the database guard.
pub fn finish_reset(
    data_dir: &Path,
    mut report: ResetReport,
    clear_cache: bool,
    clear_logs: bool,
) -> AppResult<ResetReport> {
    if clear_cache {
        report.cache_files_deleted = clear_cache_dirs(data_dir)?;
    }
    if clear_logs {
        report.log_files_deleted = clear_log_files(data_dir)?;
    }
    Ok(report)
}

/// 清空缩略图/预览/视频代理缓存目录（整个目录删除后重建空目录），返回删除的文件数
fn clear_cache_dirs(data_dir: &Path) -> AppResult<u64> {
    let mut removed = 0u64;
    for name in ["thumbnails", "previews", "proxies"] {
        let dir = data_dir.join(name);
        if !dir.exists() {
            continue;
        }
        let before = count_files(&dir);
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            // 部分删除失败时只报告实际消失的文件，避免界面显示“清空成功”但文件仍在。
            let remaining = count_files(&dir);
            removed += before.saturating_sub(remaining);
            tracing::warn!("清空缓存目录 {} 失败: {e}", dir.display());
        } else {
            removed += before;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("重建缓存目录 {} 失败: {e}", dir.display());
        }
    }
    Ok(removed)
}

fn count_files(dir: &Path) -> u64 {
    let mut n = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                n += count_files(&entry.path());
            } else {
                n += 1;
            }
        }
    }
    n
}

/// 删除日志目录中的文件但保留目录本身。
/// 当前日志文件可能仍被 tracing-appender 占用，删除失败时保留并如实计数，
/// 不影响其它重置项成功。
fn clear_log_files(data_dir: &Path) -> AppResult<u64> {
    let dir = data_dir.join("logs");
    if !dir.exists() {
        return Ok(0);
    }
    let mut removed = 0u64;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("读取日志目录失败，保留日志: {e}");
            return Ok(0);
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("读取日志目录失败: {e}");
                continue;
            }
        };
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        } else {
            tracing::warn!("删除日志文件失败，保留: {}", path.display());
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    /// 重置标签后系统分面必须重建（color 停用），否则 AI 提示词失去分面上下文，
    /// 模型返回的所有标签都会归入 custom（「只打出 custom 标」事故的根因）。
    #[test]
    fn reset_tags_reseeds_system_facets_with_color_inactive() {
        let mut conn = init_memory().unwrap();
        let dir = std::env::temp_dir();
        let sel = ResetSelection {
            tags: true,
            ..Default::default()
        };
        let report = reset(&mut conn, &dir, &sel).unwrap();
        assert_eq!(report.tags_deleted, 0); // 库里本无 tag_ops 行

        let facets = crate::db::tag_facets::list(&conn).unwrap();
        let keys: Vec<&str> = facets.iter().map(|f| f.key.as_str()).collect();
        assert!(keys.contains(&"subject"), "系统分面应重建：{keys:?}");
        assert!(
            !keys.contains(&"color"),
            "color 应保持 inactive 不进 active 列表"
        );
        assert!(!keys.contains(&"style"), "style 已下线，重置后不得重建");
        let color = crate::db::tag_facets::get(&conn, "color").unwrap();
        assert_eq!(color.status, "inactive");
        for key in ["purpose", "technical"] {
            assert!(crate::db::tag_facets::get(&conn, key).is_err());
        }
        // 重置后重启自愈不应再改动（幂等）
        crate::db::tag_facets::seed_system_facets_if_empty(&conn).unwrap();
        assert_eq!(
            crate::db::tag_facets::list(&conn).unwrap().len(),
            facets.len()
        );
    }

    #[test]
    fn reset_tags_deletes_all_tag_data_and_reports_tag_count() {
        use crate::db::{asset_tags, assets, migrations, tag_facets, tags};

        let mut conn = init_memory().unwrap();
        tag_facets::create(
            &conn,
            "reset_test_facet",
            "重置测试分面",
            "",
            "multi",
            None,
            "all",
        )
        .unwrap();
        let tag =
            tags::create_in_facet(&conn, "重置测试标签", None, Some("reset_test_facet")).unwrap();
        let asset = assets::insert(
            &conn,
            "d:/reset-test/plain.jpg",
            "plain.jpg",
            "jpg",
            1024,
            "image/jpeg",
            1_700_000_000_000,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tag_aliases
               (tag_id, alias, normalized_alias, locale, alias_type, is_searchable, created_at)
             VALUES (?1, 'reset alias', 'reset alias', '', 'synonym', 1, 1)",
            [tag.id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO asset_facet_numbers (asset_id, facet_key, value, source, created_at)
             VALUES (?1, 'reset_test_facet', 1.0, 'manual', 1)",
            [asset],
        )
        .unwrap();
        asset_tags::assign(&conn, &[asset], &[tag.id], "manual").unwrap();

        // 生产启动会启用分面删除保护；测试必须覆盖这条真实约束路径。
        migrations::apply_v22b_constraints(&conn).unwrap();

        let report = reset(
            &mut conn,
            &std::env::temp_dir(),
            &ResetSelection {
                tags: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(report.tags_deleted, 1);
        for (sql, label) in [
            ("SELECT COUNT(*) FROM tag_ops", "tag_ops"),
            ("SELECT COUNT(*) FROM asset_tags", "asset_tags"),
            (
                "SELECT COUNT(*) FROM asset_facet_numbers",
                "asset_facet_numbers",
            ),
            (
                "SELECT COUNT(*) FROM tag_facets WHERE key='reset_test_facet'",
                "用户分面",
            ),
        ] {
            let count: i64 = conn.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(count, 0, "{label} 应被完整清除");
        }
        let seeded_tags: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        assert!(seeded_tags > 0, "重置后应重建核心默认词表");
        let old_tag_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE name = '重置测试标签'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_tag_count, 0, "原用户标签不得残留");

        let asset_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(asset_count, 1, "仅重置标签时不得误删素材");
        let fts_tags: String = conn
            .query_row(
                "SELECT tag_names FROM fts_content WHERE asset_id=?1",
                [asset],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_tags, "", "标签级联删除后 FTS 标签词必须同步清空");
    }

    #[test]
    fn reset_can_clear_export_tasks_logs_and_report_local_state() {
        let mut conn = init_memory().unwrap();
        conn.execute(
            "INSERT INTO export_tasks (target, status, total, done, created_at)
             VALUES ('local', 'done', 1, 1, 1)",
            [],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("old.log"), b"old").unwrap();

        let report = reset(
            &mut conn,
            dir.path(),
            &ResetSelection {
                export_tasks: true,
                search_state: true,
                logs: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(report.export_tasks_deleted, 1);
        assert!(report.search_state_reset);
        assert_eq!(report.log_files_deleted, 1);
        assert!(!logs.join("old.log").exists());
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM export_tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
