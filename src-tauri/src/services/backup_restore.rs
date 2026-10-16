//! 数据库恢复编排：备份校验与暂存、连接切换、原库保底和失败回滚。
//! 文件复制/重命名不持数据库连接 mutex；独占 Database lifecycle lease 阻止
//! 其他命令在 library.db 交换期间拿到旧连接或空占位连接。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db::{backup, init_for_restore};
use crate::error::{AppError, AppResult};
use crate::state::DatabaseMaintenanceGuard;

trait RestoreFileOps {
    fn copy(&mut self, from: &Path, to: &Path) -> std::io::Result<u64>;
    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn remove_file(&mut self, path: &Path) -> std::io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
}

struct SystemRestoreFileOps;

impl RestoreFileOps for SystemRestoreFileOps {
    fn copy(&mut self, from: &Path, to: &Path) -> std::io::Result<u64> {
        std::fs::copy(from, to)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove_file(&mut self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// 已校验的同目录暂存备份。Drop 只清理由本次操作创建的唯一临时文件；
/// 成功 rename 后暂存路径已不存在，用户的 `.old` 和失败隔离文件不会被清理。
#[derive(Debug)]
pub struct PreparedRestore {
    data_dir: PathBuf,
    staged_path: PathBuf,
}

impl Drop for PreparedRestore {
    fn drop(&mut self) {
        if self.staged_path.exists() {
            if let Err(error) = std::fs::remove_file(&self.staged_path) {
                tracing::warn!(
                    path = %self.staged_path.display(),
                    error = %error,
                    "数据库恢复暂存文件清理失败；文件已保留供人工检查"
                );
            }
        }
    }
}

/// 校验来源并复制到数据目录的唯一暂存文件；当前库仍保持打开且可用。
pub fn prepare_restore(data_dir: &Path, source: &Path) -> AppResult<PreparedRestore> {
    let mut ops = SystemRestoreFileOps;
    prepare_restore_with(data_dir, source, &mut ops)
}

fn prepare_restore_with(
    data_dir: &Path,
    source: &Path,
    ops: &mut impl RestoreFileOps,
) -> AppResult<PreparedRestore> {
    backup::validate_backup(source)?;
    let old_path = data_dir.join("library.db.old");
    if ops.exists(&old_path) {
        return Err(AppError::conflict(format!(
            "已存在保留文件 {}；为避免覆盖恢复点，本次恢复已停止。请先人工确认并移走该文件后重试。",
            old_path.display()
        )));
    }

    let staged_path = unique_path(data_dir, "library.db.restore", "tmp");
    if let Err(error) = ops.copy(source, &staged_path) {
        if ops.exists(&staged_path) {
            if let Err(cleanup_error) = ops.remove_file(&staged_path) {
                tracing::warn!(
                    path = %staged_path.display(),
                    error = %cleanup_error,
                    "备份暂存失败后的部分文件清理失败"
                );
            }
        }
        return Err(AppError::msg(format!(
            "备份暂存失败，当前数据库未改动：{error}"
        )));
    }
    if let Err(error) = backup::validate_backup(&staged_path) {
        if let Err(cleanup_error) = ops.remove_file(&staged_path) {
            tracing::warn!(
                path = %staged_path.display(),
                error = %cleanup_error,
                "无效恢复暂存文件清理失败"
            );
        }
        return Err(AppError::msg(format!(
            "备份暂存副本校验失败，当前数据库未改动：{error}"
        )));
    }
    crate::db::prepare_restored_copy(&staged_path)?;

    Ok(PreparedRestore {
        data_dir: data_dir.to_path_buf(),
        staged_path,
    })
}

/// 在独占生命周期门内交换连接与文件。文件系统失败时保留 `.old`，并尝试
/// 用副本恢复原路径；任何无法证明活动库有效的分支都会封锁后续 DB 命令。
pub fn install_prepared_restore(
    maintenance: &DatabaseMaintenanceGuard<'_>,
    prepared: PreparedRestore,
) -> AppResult<()> {
    install_prepared_restore_under_gate(
        maintenance,
        &prepared,
        &mut SystemRestoreFileOps,
        init_for_restore,
    )
}

fn install_prepared_restore_under_gate(
    maintenance: &DatabaseMaintenanceGuard<'_>,
    prepared: &PreparedRestore,
    ops: &mut impl RestoreFileOps,
    open_database: impl FnMut(&Path) -> AppResult<Connection>,
) -> AppResult<()> {
    let mut open_database = open_database;
    let data_dir = &prepared.data_dir;
    let db_path = data_dir.join("library.db");
    let old_path = data_dir.join("library.db.old");
    if ops.exists(&old_path) {
        return Err(AppError::conflict(format!(
            "已存在保留文件 {}；原库未改动，本次恢复已停止。",
            old_path.display()
        )));
    }

    // Checkpoint 与关闭都在连接 mutex 内短暂执行；之后释放 mutex，再进行文件 IO。
    let old_connection = maintenance.take_connection()?;
    if let Err(error) = old_connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        maintenance.replace_connection(old_connection)?;
        return Err(AppError::msg(format!(
            "现库 WAL 检查点失败，原库未改动：{error}"
        )));
    }
    let old_connection = match old_connection.close() {
        Ok(()) => None,
        Err((connection, error)) => Some((connection, error)),
    };
    if let Some((connection, error)) = old_connection {
        maintenance.replace_connection(connection)?;
        return Err(AppError::msg(format!(
            "现库连接无法安全关闭，原库未改动：{error}"
        )));
    }

    if let Err(error) = ops.rename(&db_path, &old_path) {
        return reopen_original_after_swap_failure(
            maintenance,
            &db_path,
            &old_path,
            ops,
            &mut open_database,
            format!("现库改名为保留文件失败：{error}"),
        );
    }

    if let Err(error) = ops.rename(&prepared.staged_path, &db_path) {
        if ops.exists(&db_path) {
            maintenance.block_access();
            return Err(AppError::msg(format!(
                "恢复文件安装失败：{error}；目标路径 {} 意外存在，未覆盖该文件。原库保留在 {}，数据库操作已封锁。",
                db_path.display(),
                old_path.display()
            )));
        }
        return restore_old_copy(
            maintenance,
            &db_path,
            &old_path,
            ops,
            &mut open_database,
            format!("恢复文件安装失败：{error}"),
        );
    }

    let restored_connection = match open_database(&db_path) {
        Ok(connection) => connection,
        Err(open_error) => {
            let failed_path = unique_path(data_dir, "library.db.restore-failed", "db");
            if let Err(quarantine_error) = ops.rename(&db_path, &failed_path) {
                maintenance.block_access();
                return Err(AppError::msg(format!(
                    "恢复后的数据库无法打开：{open_error}；失败文件无法隔离：{quarantine_error}。原库仍保留在 {}；数据库操作已封锁，请勿删除或覆盖任一文件。",
                    old_path.display()
                )));
            }
            return restore_old_copy(
                maintenance,
                &db_path,
                &old_path,
                ops,
                &mut open_database,
                format!(
                    "恢复后的数据库无法打开：{open_error}；失败副本保留在 {}",
                    failed_path.display()
                ),
            );
        }
    };
    if let Err(error) = maintenance.replace_connection(restored_connection) {
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "新库已验证，但无法切换活动连接：{error}。原库保留在 {}；数据库操作已封锁。",
            old_path.display()
        )));
    }
    tracing::info!(
        backup = %prepared.staged_path.display(),
        preserved_old = %old_path.display(),
        "数据库恢复成功；恢复前数据库保留为 library.db.old"
    );
    Ok(())
}

fn restore_old_copy(
    maintenance: &DatabaseMaintenanceGuard<'_>,
    db_path: &Path,
    old_path: &Path,
    ops: &mut impl RestoreFileOps,
    open_database: &mut impl FnMut(&Path) -> AppResult<Connection>,
    cause: String,
) -> AppResult<()> {
    if ops.exists(db_path) {
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "{cause}；原库保留在 {}，但目标路径 {} 已存在，未覆盖该路径。数据库操作已封锁。",
            old_path.display(),
            db_path.display()
        )));
    }
    let rollback_path = unique_path(
        db_path.parent().unwrap_or_else(|| Path::new(".")),
        "library.db.rollback",
        "tmp",
    );
    if let Err(error) = ops.copy(old_path, &rollback_path) {
        if ops.exists(&rollback_path) {
            if let Err(cleanup_error) = ops.remove_file(&rollback_path) {
                tracing::warn!(
                    path = %rollback_path.display(),
                    error = %cleanup_error,
                    "原库回滚暂存复制失败后的清理失败"
                );
            }
        }
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "{cause}；原库副本恢复失败：{error}。原库仍保留在 {}，数据库操作已封锁。",
            old_path.display()
        )));
    }
    if let Err(error) = backup::validate_backup(&rollback_path) {
        if let Err(cleanup_error) = ops.remove_file(&rollback_path) {
            tracing::warn!(
                path = %rollback_path.display(),
                error = %cleanup_error,
                "无效原库回滚暂存文件清理失败"
            );
        }
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "{cause}；原库回滚副本校验失败：{error}。原库仍保留在 {}，数据库操作已封锁。",
            old_path.display()
        )));
    }
    if let Err(error) = crate::db::prepare_restored_copy(&rollback_path) {
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "{cause}；回滚副本保护初始化失败：{error}。原库保留，数据库操作已封锁。"
        )));
    }
    if let Err(error) = ops.rename(&rollback_path, db_path) {
        if ops.exists(&rollback_path) {
            if let Err(cleanup_error) = ops.remove_file(&rollback_path) {
                tracing::warn!(
                    path = %rollback_path.display(),
                    error = %cleanup_error,
                    "原库回滚暂存文件重命名失败后的清理失败"
                );
            }
        }
        maintenance.block_access();
        return Err(AppError::msg(format!(
            "{cause}；已校验的原库回滚副本无法安装：{error}。原库仍保留在 {}，数据库操作已封锁。",
            old_path.display()
        )));
    }
    match open_database(db_path) {
        Ok(connection) => {
            if let Err(error) = maintenance.replace_connection(connection) {
                maintenance.block_access();
                return Err(AppError::msg(format!(
                    "{cause}；library.db 已恢复但活动连接切换失败：{error}。原库仍保留在 {}，数据库操作已封锁。",
                    old_path.display()
                )));
            }
            Err(AppError::msg(format!(
                "{cause}；原库已恢复并重新打开，保留副本仍在 {}。",
                old_path.display()
            )))
        }
        Err(error) => {
            maintenance.block_access();
            Err(AppError::msg(format!(
                "{cause}；原库文件已复制回 library.db，但重新打开失败：{error}。独立保留副本仍在 {}；数据库操作已封锁。",
                old_path.display()
            )))
        }
    }
}

fn reopen_original_after_swap_failure(
    maintenance: &DatabaseMaintenanceGuard<'_>,
    db_path: &Path,
    old_path: &Path,
    ops: &mut impl RestoreFileOps,
    open_database: &mut impl FnMut(&Path) -> AppResult<Connection>,
    cause: String,
) -> AppResult<()> {
    if ops.exists(db_path) {
        match open_database(db_path) {
            Ok(connection) => {
                if let Err(error) = maintenance.replace_connection(connection) {
                    maintenance.block_access();
                    return Err(AppError::msg(format!(
                        "{cause}；原库文件仍在原路径，但活动连接无法恢复：{error}。数据库操作已封锁。"
                    )));
                }
                return Err(AppError::msg(format!(
                    "{cause}；原库已重新打开，未执行恢复。"
                )));
            }
            Err(open_error) => {
                maintenance.block_access();
                return Err(AppError::msg(format!(
                    "{cause}；原库文件仍在原路径但重新打开失败：{open_error}。数据库操作已封锁。"
                )));
            }
        }
    }
    restore_old_copy(maintenance, db_path, old_path, ops, open_database, cause)
}

fn unique_path(data_dir: &Path, stem: &str, extension: &str) -> PathBuf {
    data_dir.join(format!("{stem}-{}.{}", uuid::Uuid::new_v4(), extension))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{assets, init};
    use crate::state::Database;
    use std::io;

    #[derive(Default)]
    struct FaultOps<'a> {
        fail_copy: bool,
        fail_rename_target_suffix: Option<String>,
        connection_probe: Option<Box<dyn FnMut() + 'a>>,
    }

    impl RestoreFileOps for FaultOps<'_> {
        fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64> {
            if let Some(probe) = &mut self.connection_probe {
                probe();
            }
            if self.fail_copy {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected copy failure",
                ));
            }
            std::fs::copy(from, to)
        }

        fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
            if let Some(probe) = &mut self.connection_probe {
                probe();
            }
            if self
                .fail_rename_target_suffix
                .as_ref()
                .is_some_and(|suffix| {
                    to.file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(suffix))
                })
            {
                self.fail_rename_target_suffix = None;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected rename failure",
                ));
            }
            std::fs::rename(from, to)
        }

        fn remove_file(&mut self, path: &Path) -> io::Result<()> {
            std::fs::remove_file(path)
        }

        fn exists(&self, path: &Path) -> bool {
            path.exists()
        }
    }

    fn create_backup(path: &Path, with_asset: bool) {
        let connection = crate::db::init_memory().unwrap();
        if with_asset {
            assets::insert(
                &connection,
                "backup-asset.jpg",
                "backup-asset.jpg",
                "jpg",
                1,
                "image/jpeg",
                1,
            )
            .unwrap();
        }
        backup::backup_to(&connection, path).unwrap();
    }

    fn asset_count(connection: &Connection) -> i64 {
        connection
            .query_row("SELECT count(*) FROM assets", [], |row| row.get(0))
            .unwrap()
    }

    fn open_current_database(data_dir: &Path) -> Database {
        Database::new(init(&data_dir.join("library.db")).unwrap())
    }

    #[test]
    fn successful_restore_keeps_the_previous_database_as_old_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);

        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        install_prepared_restore_under_gate(
            &maintenance,
            &prepared,
            &mut SystemRestoreFileOps,
            init,
        )
        .unwrap();
        assert_eq!(
            maintenance
                .with_connection(|connection| Ok(asset_count(connection)))
                .unwrap(),
            1
        );
        drop(maintenance);
        assert!(data_dir.path().join("library.db.old").is_file());
        assert!(backup::validate_backup(&data_dir.path().join("library.db.old")).is_ok());
    }

    #[test]
    fn public_restore_installs_fresh_unverified_locator_before_opening_access() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let source_conn = Connection::open(&source).unwrap();
        let old =
            crate::db::libraries::initialize_candidate(&source_conn, "C:/fixture-root").unwrap();
        drop(source_conn);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        install_prepared_restore(&maintenance, prepared).unwrap();
        let installed = maintenance
            .with_connection(|conn| Ok(crate::db::libraries::current(conn)?.unwrap()))
            .unwrap();
        assert_eq!(installed.library_id, old.library_id);
        assert_ne!(installed.library_epoch, old.library_epoch);
        assert_eq!(
            installed.health,
            crate::db::libraries::LibraryHealth::Unverified
        );
        assert!(installed.root_identity.is_none());
        assert_eq!(
            maintenance
                .with_connection(|conn| Ok(asset_count(conn)))
                .unwrap(),
            1
        );
    }

    #[test]
    fn existing_old_file_is_preserved_and_blocks_new_restore() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let old_path = data_dir.path().join("library.db.old");
        std::fs::write(&old_path, b"operator recovery point").unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);

        let error = prepare_restore(data_dir.path(), &source).unwrap_err();
        assert!(error.to_string().contains("已存在保留文件"));
        assert_eq!(
            std::fs::read(&old_path).unwrap(),
            b"operator recovery point"
        );
        assert_eq!(asset_count(&database.lock().unwrap()), 0);
    }

    #[test]
    fn staging_copy_failure_leaves_the_active_database_untouched() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let mut ops = FaultOps {
            fail_copy: true,
            ..Default::default()
        };

        let error = prepare_restore_with(data_dir.path(), &source, &mut ops).unwrap_err();
        assert!(error.to_string().contains("暂存失败"));
        assert_eq!(asset_count(&database.lock().unwrap()), 0);
        assert!(!data_dir.path().join("library.db.old").exists());
    }

    #[test]
    fn old_database_rename_failure_reopens_the_original_database() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        let mut ops = FaultOps {
            fail_rename_target_suffix: Some("library.db.old".into()),
            ..Default::default()
        };

        let error = install_prepared_restore_under_gate(&maintenance, &prepared, &mut ops, init)
            .unwrap_err();
        assert!(error.to_string().contains("改名为保留文件失败"));
        assert_eq!(
            maintenance
                .with_connection(|connection| Ok(asset_count(connection)))
                .unwrap(),
            0
        );
        assert!(!data_dir.path().join("library.db.old").exists());
    }

    #[test]
    fn installing_backup_rename_failure_copies_old_back_and_keeps_old_copy() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        let mut ops = FaultOps {
            fail_rename_target_suffix: Some("library.db".into()),
            ..Default::default()
        };

        let error = install_prepared_restore_under_gate(&maintenance, &prepared, &mut ops, init)
            .unwrap_err();
        assert!(error.to_string().contains("恢复文件安装失败"));
        assert_eq!(
            maintenance
                .with_connection(|connection| Ok(asset_count(connection)))
                .unwrap(),
            0
        );
        assert!(data_dir.path().join("library.db.old").is_file());
        assert!(data_dir.path().join("library.db").is_file());
    }

    #[test]
    fn rollback_copy_failure_blocks_db_and_startup_recovers_from_old() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        let mut ops = FaultOps {
            fail_copy: true,
            fail_rename_target_suffix: Some("library.db".into()),
            ..Default::default()
        };

        let error = install_prepared_restore_under_gate(&maintenance, &prepared, &mut ops, init)
            .unwrap_err();
        assert!(error.to_string().contains("原库副本恢复失败"));
        assert!(!data_dir.path().join("library.db").exists());
        assert!(data_dir.path().join("library.db.old").is_file());
        drop(maintenance);
        match database.lock() {
            Ok(_) => panic!("未验证的占位连接不得对普通命令开放"),
            Err(error) => assert!(error.to_string().contains("数据库恢复未完成")),
        }

        let recovered = init(&data_dir.path().join("library.db")).unwrap();
        assert_eq!(asset_count(&recovered), 0);
        assert!(data_dir.path().join("library.db.old").is_file());
    }

    #[test]
    fn database_connection_mutex_is_released_during_filesystem_operations() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        let probe_maintenance = &maintenance;
        let mut ops = FaultOps {
            connection_probe: Some(Box::new(move || {
                probe_maintenance
                    .with_connection(|connection| {
                        let _: i64 = connection.query_row("SELECT 1", [], |row| row.get(0))?;
                        Ok(())
                    })
                    .unwrap();
            })),
            ..Default::default()
        };

        install_prepared_restore_under_gate(&maintenance, &prepared, &mut ops, init).unwrap();
        assert_eq!(
            maintenance
                .with_connection(|connection| Ok(asset_count(connection)))
                .unwrap(),
            1
        );
    }

    #[test]
    fn unopenable_restored_database_is_quarantined_and_old_database_reopened() {
        let data_dir = tempfile::tempdir().unwrap();
        let database = open_current_database(data_dir.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("backup.db");
        create_backup(&source, true);
        let prepared = prepare_restore(data_dir.path(), &source).unwrap();
        let maintenance = database.maintenance().unwrap();
        let mut opens = 0;
        let error = install_prepared_restore_under_gate(
            &maintenance,
            &prepared,
            &mut SystemRestoreFileOps,
            |path| {
                opens += 1;
                if opens == 1 {
                    Err(AppError::msg("injected database reopen failure"))
                } else {
                    init(path)
                }
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("失败副本保留在"));
        assert_eq!(
            maintenance
                .with_connection(|connection| Ok(asset_count(connection)))
                .unwrap(),
            0
        );
        assert!(data_dir.path().join("library.db.old").is_file());
        assert!(std::fs::read_dir(data_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.db.restore-failed-")));
    }
}
