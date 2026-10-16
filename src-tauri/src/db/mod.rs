//! 数据层入口：连接初始化（WAL + foreign_keys + cjk_bigram 注册 + 迁移）

pub mod ai;
pub mod ai_connections;
pub mod asset_tags;
pub mod assets;
pub mod authorization;
pub mod backup;
pub mod conversations;
pub mod dedup;
pub mod export;
pub mod facet_numbers;
pub mod import_execution;
pub mod import_plans;
pub mod legacy_assets;
pub mod libraries;
pub mod migrations;
pub mod operations;
pub mod palette_bucket;
pub mod plans;
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
pub mod tagging;
pub mod tags;
pub mod video_proxy;

use std::path::Path;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};
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
    let recovered_copy = recover_missing_library_from_preserved_old(path)?;
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrations::migrate(&conn)?;
    libraries::initialize_candidate(&conn, &settings::get_settings(&conn)?.library_root)?;
    if recovered_copy {
        libraries::invalidate_restored_locator(&conn)?;
    }
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

/// If a failed restore left `library.db` absent but preserved `library.db.old`,
/// restore a validated copy before opening the app DB. Never create a new empty
/// library over that recovery point.
fn recover_missing_library_from_preserved_old(path: &Path) -> AppResult<bool> {
    if path.file_name().and_then(|name| name.to_str()) != Some("library.db") || path.exists() {
        return Ok(false);
    }
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    let old_path = parent.join("library.db.old");
    if !old_path.exists() {
        return Ok(false);
    }

    crate::db::backup::validate_backup(&old_path).map_err(|error| {
        AppError::msg(format!(
            "library.db 不存在，且保留库 {} 无法验证；为防止创建空库覆盖数据，启动已停止：{error}",
            old_path.display()
        ))
    })?;

    let staged_path = parent.join(format!("library.db.recovery-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(error) = std::fs::copy(&old_path, &staged_path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(AppError::msg(format!(
            "保留库复制回 library.db 失败；原文件仍保留在 {}，启动已停止：{error}",
            old_path.display()
        )));
    }
    if let Err(error) = crate::db::backup::validate_backup(&staged_path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(AppError::msg(format!(
            "保留库副本校验失败；原文件仍保留在 {}，启动已停止：{error}",
            old_path.display()
        )));
    }
    // Publish only a durably invalidated copy. A crash after rename must not revive
    // the backup's ready locator/epoch on the next ordinary startup.
    if let Err(error) = prepare_restored_copy(&staged_path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staged_path, path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(AppError::msg(format!(
            "保留库已验证但无法恢复到 library.db；原文件仍保留在 {}，启动已停止：{error}",
            old_path.display()
        )));
    }
    tracing::warn!(
        preserved_old = %old_path.display(),
        "library.db 缺失，已从保留副本恢复；原 library.db.old 保持不动"
    );
    Ok(true)
}

/// Restored or rollback copies need a fresh epoch before the maintenance gate opens.
pub fn init_for_restore(path: &Path) -> AppResult<Connection> {
    let conn = init(path)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    libraries::invalidate_restored_locator(&conn)?;
    Ok(conn)
}

/// Transform an owned staging copy before publication; never call on the source backup.
/// Closing a DELETE-journal connection leaves a self-contained durable database file.
pub fn prepare_restored_copy(path: &Path) -> AppResult<()> {
    let conn = init_for_restore(path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    if mode != "delete" {
        return Err(AppError::conflict("恢复暂存库无法安全关闭日志"));
    }
    conn.close().map_err(|(_, error)| AppError::from(error))?;
    Ok(())
}

/// 内存库（单元测试用）
pub fn init_memory() -> AppResult<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrations::migrate(&conn)?;
    libraries::initialize_candidate(&conn, &settings::get_settings(&conn)?.library_root)?;
    tag_facets::seed_system_facets_if_empty(&conn)?;
    let _ = schema_features::verify_schema_features(&conn);
    Ok(conn)
}

/// 生产启动和恢复库后的统一补齐入口：只在分面表为空时播种，绝不改写已有分类说明。
pub fn ensure_default_taxonomy(conn: &Connection) -> AppResult<()> {
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
    fn init_recovers_missing_library_from_valid_old_without_removing_old() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("library.db.old");
        let source = init_memory().unwrap();
        let old_locator = libraries::initialize_candidate(&source, "C:/fixture-root").unwrap();
        source
            .execute(
                "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at, hash)
                 VALUES ('preserved.jpg', 'preserved.jpg', '.jpg', 1, 'image/jpeg', 1, 1, 'preserved')",
                [],
            )
            .unwrap();
        crate::db::backup::backup_to(&source, &old_path).unwrap();
        let library_path = dir.path().join("library.db");

        let recovered = init(&library_path).unwrap();
        let count: i64 = recovered
            .query_row("SELECT count(*) FROM assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let restored_locator = libraries::current(&recovered).unwrap().unwrap();
        assert_ne!(restored_locator.library_epoch, old_locator.library_epoch);
        assert_eq!(restored_locator.library_id, old_locator.library_id);
        drop(recovered);
        let reopened = init(&library_path).unwrap();
        assert_eq!(
            libraries::current(&reopened).unwrap().unwrap(),
            restored_locator
        );
        assert!(old_path.is_file(), "恢复源 library.db.old 必须保留");
    }

    #[test]
    fn init_refuses_to_create_empty_library_when_old_recovery_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("library.db.old");
        std::fs::write(&old_path, b"not a database").unwrap();
        let library_path = dir.path().join("library.db");

        let error = init(&library_path).unwrap_err();
        assert!(error.to_string().contains("防止创建空库覆盖数据"));
        assert!(!library_path.exists());
        assert!(old_path.is_file());
    }

    #[test]
    fn published_restore_survives_restart_without_reviving_backup_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let source = init_memory().unwrap();
        libraries::initialize_candidate(&source, "C:/fixture-root").unwrap();
        source.execute("UPDATE libraries SET root_identity='fixture-root-id',health='ready',library_epoch=lower(hex(randomblob(16)))",[]).unwrap();
        let old = libraries::current(&source).unwrap().unwrap();
        source.execute("INSERT INTO assets(file_path,file_name,file_ext,file_size,mime_type,created_at,modified_at) VALUES('C:/fixture-root/a.jpg','a.jpg','.jpg',1,'image/jpeg',1,1)",[]).unwrap();
        source.execute("UPDATE managed_asset_locations SET status='verified',library_id=?1,library_epoch=?2,relative_path='a.jpg',file_identity='fixture-file',content_sha256=?3,ownership_receipt='fixture-receipt'",rusqlite::params![old.library_id,old.library_epoch,"a".repeat(64)]).unwrap();
        let backup_path = dir.path().join("source.db");
        backup::backup_to(&source, &backup_path).unwrap();
        let staged = dir.path().join("restore.tmp");
        std::fs::copy(&backup_path, &staged).unwrap();
        prepare_restored_copy(&staged).unwrap();
        let published = dir.path().join("library.db");
        std::fs::rename(&staged, &published).unwrap();
        // Simulate process loss immediately after publication: ordinary startup,
        // with no in-memory recovery flag or post-publication restore callback.
        let reopened = init(&published).unwrap();
        let protected = libraries::current(&reopened).unwrap().unwrap();
        assert_ne!(protected.library_epoch, old.library_epoch);
        assert_eq!(protected.health, libraries::LibraryHealth::Unverified);
        assert_eq!(
            reopened
                .query_row(
                    &format!(
                        "SELECT count(*) FROM assets a WHERE {}",
                        libraries::VERIFIED_ASSET_SCOPE
                    ),
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(reopened);
        assert_eq!(
            libraries::current(&init(&published).unwrap())
                .unwrap()
                .unwrap(),
            protected
        );
        assert_eq!(libraries::current(&source).unwrap().unwrap(), old);
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
