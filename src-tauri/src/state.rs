//! 全局状态：单写连接 + WAL（架构共享知识 #9）
//! T03 扩展：数据目录（缩略图服务定位）+ 入库/导出取消标志
//! T04 修订：db / import_cancel 改 Arc，长任务命令 spawn_blocking 时可 Move 进工作线程

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rusqlite::Connection;

use crate::db::schema_features::SchemaFeatureStatus;
use crate::services::ollama_runtime::OllamaRuntimeState;

/// Database connection access is serialized with a lifecycle gate.
/// Ordinary callers take a shared lifecycle lease plus the connection mutex;
/// restore/reset maintenance takes the exclusive lease and may release the
/// connection mutex while doing filesystem work without exposing a half-swapped DB.
pub struct Database {
    lifecycle: RwLock<()>,
    connection: Mutex<Connection>,
    blocked: AtomicBool,
}

impl Database {
    pub fn new(connection: Connection) -> Self {
        Self {
            lifecycle: RwLock::new(()),
            connection: Mutex::new(connection),
            blocked: AtomicBool::new(false),
        }
    }

    pub fn lock(&self) -> crate::error::AppResult<DbConnectionGuard<'_>> {
        let lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| crate::error::AppError::msg("数据库生命周期锁中毒"))?;
        if self.blocked.load(Ordering::Acquire) {
            return Err(crate::error::AppError::internal(
                "数据库恢复未完成，已暂停所有数据库操作。请按错误提示恢复备份或重启应用。",
            ));
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
        Ok(DbConnectionGuard {
            _lifecycle: lifecycle,
            connection,
        })
    }

    pub fn maintenance(&self) -> crate::error::AppResult<DatabaseMaintenanceGuard<'_>> {
        let lifecycle = self
            .lifecycle
            .write()
            .map_err(|_| crate::error::AppError::msg("数据库生命周期锁中毒"))?;
        Ok(DatabaseMaintenanceGuard {
            database: self,
            _lifecycle: lifecycle,
        })
    }
}

pub struct DbConnectionGuard<'a> {
    _lifecycle: RwLockReadGuard<'a, ()>,
    connection: MutexGuard<'a, Connection>,
}

impl Deref for DbConnectionGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for DbConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

/// Exclusive lifecycle lease. The inner connection mutex is held only for
/// checkpoint/close or connection replacement, never across file IO.
pub struct DatabaseMaintenanceGuard<'a> {
    database: &'a Database,
    _lifecycle: RwLockWriteGuard<'a, ()>,
}

impl DatabaseMaintenanceGuard<'_> {
    pub fn block_access(&self) {
        self.database.blocked.store(true, Ordering::Release);
    }

    pub fn with_connection<T>(
        &self,
        f: impl FnOnce(&Connection) -> crate::error::AppResult<T>,
    ) -> crate::error::AppResult<T> {
        let connection = self
            .database
            .connection
            .lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
        f(&connection)
    }

    pub fn take_connection(&self) -> crate::error::AppResult<Connection> {
        let placeholder = Connection::open_in_memory()
            .map_err(|e| crate::error::AppError::msg(format!("占位连接创建失败: {e}")))?;
        let mut current = self
            .database
            .connection
            .lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
        Ok(std::mem::replace(&mut *current, placeholder))
    }

    pub fn replace_connection(&self, connection: Connection) -> crate::error::AppResult<()> {
        let mut current = self
            .database
            .connection
            .lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
        *current = connection;
        Ok(())
    }
}

pub struct AppState {
    pub db: Arc<Database>,
    /// 应用数据目录（$APP_DATA_DIR/bagertea_ai_media_v2）
    pub data_dir: PathBuf,
    /// 入库取消标志
    pub import_cancel: Arc<AtomicBool>,
    /// W5c：入库进行中标志（restore_db 用它阻断恢复；取消标志语义不同，不能复用）
    pub import_running: Arc<AtomicBool>,
    /// 导出任务取消标志注册表（task_id → flag）
    pub export_cancel: Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>,
    /// AI 批次取消标志注册表（batch_id → flag）
    pub ai_cancel: Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>,
    /// AI 批次提示词/分面配置快照与配置写入的互斥闸。
    pub ai_config_guard: Arc<Mutex<()>>,
    /// 超级搜索解析取消标志注册表（request_id → flag）
    pub search_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// 媒体元数据回填任务取消标志（单槽，同一时刻一个回填）
    pub media_refill_cancel: Arc<AtomicBool>,
    /// 回填类长任务互斥闸（元数据回填 / 色板回算共用一条，同一时刻只允许一个）。
    /// 不拆成两个取消标志：两者都吃解码许可与 DB 锁，并发只会互相拖慢（FX-12）。
    pub refill_running: Arc<AtomicBool>,
    /// 视频兼容代理取消标志注册表（"asset_id:variant" → flag）
    pub video_proxy_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// Ollama 本地服务运行态（L2 ownership，§8.2）：External 永不停 / AppOwned 可停
    pub ollama_runtime: Arc<Mutex<OllamaRuntimeState>>,
    /// F1-e：schema_features 缓存（启动读一次，apply_tag_constraints 后刷新）。
    /// 命令层读它判断 tag_unique_terms 等能力是否生效（设置页展示 + feature gate）。
    pub schema_features: Arc<Mutex<Vec<SchemaFeatureStatus>>>,
}

impl AppState {
    pub fn new(conn: Connection, data_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Database::new(conn)),
            data_dir,
            import_cancel: Arc::new(AtomicBool::new(false)),
            import_running: Arc::new(AtomicBool::new(false)),
            export_cancel: Arc::new(Mutex::new(HashMap::new())),
            ai_cancel: Arc::new(Mutex::new(HashMap::new())),
            ai_config_guard: Arc::new(Mutex::new(())),
            search_cancel: Arc::new(Mutex::new(HashMap::new())),
            media_refill_cancel: Arc::new(AtomicBool::new(false)),
            refill_running: Arc::new(AtomicBool::new(false)),
            video_proxy_cancel: Arc::new(Mutex::new(HashMap::new())),
            ollama_runtime: Arc::new(Mutex::new(OllamaRuntimeState::new())),
            schema_features: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// F1-e：从库刷新 schema_features 缓存（启动 / apply_tag_constraints 后调用）。
    pub fn refresh_schema_features(&self) {
        let snapshot = {
            let lock = self
                .db
                .lock()
                .map_err(|_| crate::error::AppError::msg("数据库锁中毒"));
            match lock {
                Ok(conn) => crate::db::schema_features::list_features(&conn).ok(),
                Err(_) => None,
            }
        };
        if let Some(list) = snapshot {
            if let Ok(mut cache) = self.schema_features.lock() {
                *cache = list;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Database;
    use rusqlite::Connection;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn ordinary_database_access_waits_for_maintenance_without_holding_connection_mutex() {
        let database = Arc::new(Database::new(Connection::open_in_memory().unwrap()));
        let maintenance = database.maintenance().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let reader_db = Arc::clone(&database);
        let reader = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _connection = reader_db.lock().unwrap();
            acquired_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(25)).is_err());
        drop(maintenance);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        reader.join().unwrap();
    }
}
