//! 库内分库移动（A4-02）
//!
//! 分库不是拥有素材的实体，只是总仓内的一个目录；素材归属只由 `assets.file_path` 表达
//! （见 [`crate::db::collections`]）。因此「移动」= 在同一个总仓内把已核验的托管文件从
//! 一个相对目录搬到另一个相对目录，并在同一事务里改写 `assets.file_path` 与
//! `managed_asset_locations` 的核验证据；素材 ID、标签、评分与历史全部保留。
//!
//! 三条硬约束：
//!
//! 1. **源与目标都必须是当前总仓内经核验的素材/目录**。目标目录来自 `library_collections`
//!    的 `library_id + library_epoch`，所以总仓代次变化后旧计划的引用直接失效，不会
//!    解析到另一个根。
//! 2. **事务不假装能回滚文件系统**。先写 `move_journal` 的逐文件意图，再用
//!    `state='moved'` 标记「文件已经搬过去了」，最后才在数据库事务里落路径与证据并把
//!    journal 推到 `committed`。中途崩溃时 journal 停在 `intent`/`moved`，由
//!    [`reconcile_interrupted_moves`] 按磁盘真实状态收尾，绝不无条件算成功。
//! 3. **只做同一卷内的 `rename`**。跨卷复制会换文件身份，既破坏「同一素材」语义，也让
//!    回滚失去唯一真实位置；总仓内目录之间的 `rename` 不跨卷，跨卷场景交给后续卡设计。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::collections::{self, StoredCollection};
use crate::db::libraries::{self, LibrarySnapshot};
use crate::db::move_journal::{self, MoveJournalEntry, NewMoveIntent};
use crate::db::{assets, operations as operations_db, plans as plans_db};
use crate::error::{AppError, AppResult};
use crate::services::managed_library;
use crate::services::operations;
use crate::state::Database;
use crate::utils::file_identity::file_identity;
use crate::utils::path::{encode_native_path, normalize_path};

/// 移动计划里的单个文件。计划阶段冻结源/目标路径与源文件身份；执行时在触碰
/// 文件系统前按冻结身份复验，`fs::rename` 前再次比对，防止源被替换后仍沿用旧摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovePlanItem {
    pub asset_id: i64,
    pub source_path: String,
    pub target_path: String,
    /// 批准快照里冻结的源文件身份。空表示旧计划快照没带身份，执行时回退到
    /// 「执行开始时采集一次」，仍会在触碰文件系统前复验。
    pub source_identity: String,
}

/// 逐文件执行结果汇总。`unknown` 与 `failed` 必须分开：`failed` 表示可以重试的
/// 确定性失败，`unknown` 表示文件效果可能已经发生但证据不完整，宣称失败会误导
/// 用户以为重试即可恢复（同一形状见 `trash::DeletionSummary`）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveSummary {
    pub moved: i64,
    pub failed: i64,
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
}

impl MoveSummary {
    fn empty() -> Self {
        Self {
            moved: 0,
            failed: 0,
            unknown: 0,
            skipped: 0,
            cancelled: false,
        }
    }
}

/// 一次移动执行需要的全部输入（避免把参数摊在签名上）。
pub struct MoveExecution<'a> {
    pub operation_id: &'a str,
    pub operation_ref: &'a ResourceRef,
    pub step_ref: &'a ResourceRef,
    pub items: &'a [MovePlanItem],
    pub cancelled: &'a AtomicBool,
}

/// 逐文件 item id：计划、收据、日志与前端摘要必须使用同一套 ID。
pub fn move_item_id(asset_id: i64) -> String {
    format!("move_{asset_id:016}")
}

/// 解析目标分库，并校验它属于当前总仓代次。
pub fn resolve_target(
    conn: &rusqlite::Connection,
    library: &LibrarySnapshot,
    collection_ref: &ResourceRef,
) -> AppResult<StoredCollection> {
    let stored = collections::find(conn, collection_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if stored.library_id != library.library_id || stored.library_epoch != library.library_epoch {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(stored)
}

/// 生成移动计划：冻结每个文件的源路径与目标路径。
///
/// 已经在目标目录里的文件会被跳过（no-op），所以返回的集合就是「真正需要移动的文件」。
pub fn plan_items(
    db: &Database,
    library: &LibrarySnapshot,
    asset_ids: &[i64],
    target: &StoredCollection,
) -> AppResult<Vec<MovePlanItem>> {
    let root = managed_library::verified_root_path(library)?;
    let target_dir = if target.relative_path.is_empty() {
        root.clone()
    } else {
        root.join(&target.relative_path)
    };
    managed_library::validate_import_target(library, &target_dir)?;
    let target_key = normalize_path(&encode_native_path(&target_dir)?);

    // 锁内只取纯数据；文件身份核验放到锁外，避免在数据库锁内做文件 IO。
    let mut pending: Vec<(i64, String, String)> = Vec::new();
    {
        let conn = db.lock()?;
        for asset_id in asset_ids {
            let asset = assets::get_managed(&conn, *asset_id)?;
            let source = PathBuf::from(&asset.file_path);
            // 「已在目标分库」必须比 canonical 形式：库里存的是归一化路径 `c:/…`，
            // 而 `verified_root_path` 取得的目标目录在 Windows 上带 `\\?\` 前缀，
            // 直接比较永不相等，会把本来无需移动的素材又排进移动计划。
            let same_dir = source
                .parent()
                .and_then(|parent| fs::canonicalize(parent).ok())
                .and_then(|parent| encode_native_path(&parent).ok())
                .map(|text| normalize_path(&text) == target_key)
                .unwrap_or(false);
            if same_dir {
                continue;
            }
            pending.push((*asset_id, asset.file_path.clone(), asset.file_name.clone()));
        }
    }

    let mut taken: HashSet<String> = HashSet::new();
    let mut items = Vec::with_capacity(pending.len());
    for (asset_id, source_path, file_name) in pending {
        let source = PathBuf::from(&source_path);
        // 计划阶段就冻结源文件身份：批准之后源被替换时，执行阶段必须能发现，
        // 否则会拿「批准时以为的那个文件」的旧摘要去认领一个陌生文件。
        let opened = fs::File::open(&source)
            .map_err(|_| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let source_identity = file_identity(&opened)?;
        let target_path = unique_target(&target_dir, &file_name, &mut taken)?;
        items.push(MovePlanItem {
            asset_id,
            source_path: normalize_path(&source_path),
            target_path,
            source_identity,
        });
    }
    Ok(items)
}

/// 目标目录内的可用文件名：同名依次尝试 `(1)`、`(2)`…，绝不覆盖既有文件。
///
/// 返回的是**原生路径**而不是 `normalize_path` 的展示形式：计划冻结的目标路径会被
/// `fs::rename` 直接使用，而 `verified_root_path` 返回的 canonical root 在 Windows 上
/// 带 `\\?\` 扩展前缀——归一化后变成 `//?/c:/…`，交给文件系统只会得到 `PERMISSION_DENIED`。
/// 数据库侧仍写归一化路径，由执行阶段各自转换。
fn unique_target(dir: &Path, file_name: &str, taken: &mut HashSet<String>) -> AppResult<String> {
    let first = dir.join(file_name);
    let key = normalize_path(&encode_native_path(&first)?);
    if !first.exists() && taken.insert(key) {
        return encode_native_path(&first);
    }
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::invalid_arg("文件名不合法"))?;
    let ext = path.extension().and_then(|value| value.to_str());
    for index in 1..1000 {
        let name = match ext {
            Some(ext) => format!("{stem}({index}).{ext}"),
            None => format!("{stem}({index})"),
        };
        let candidate = dir.join(&name);
        let key = normalize_path(&encode_native_path(&candidate)?);
        if taken.contains(&key) {
            continue;
        }
        if !candidate.exists() {
            taken.insert(key);
            return encode_native_path(&candidate);
        }
    }
    Err(AppError::conflict(format!(
        "目标分库同名文件过多: {file_name}"
    )))
}

/// 逐文件执行移动。
pub fn execute(
    db: &Database,
    caller: &CallerContext,
    library: &LibrarySnapshot,
    run: MoveExecution<'_>,
) -> AppResult<MoveSummary> {
    let mut summary = MoveSummary::empty();
    for item in run.items {
        // 取消有两个来源：进程内的原子标志（直接调用方与本进程内取消），以及数据库里
        // 已被 `operations::cancel_operation` 标记为 cancelling 的操作记录（跨 WebView）。
        if run.cancelled.load(Ordering::Relaxed) || cancel_requested(db, run.operation_id)? {
            summary.cancelled = true;
            summary.skipped += 1;
            continue;
        }
        let item_id = move_item_id(item.asset_id);
        let item_ref = ResourceRef::from_trusted(item_id.clone())?;

        // 1. 源身份基线（FS 触碰前）。优先使用批准快照里冻结的 `source_identity`：
        //    采集时机从「执行开始」前移到「计划冻结」，批准之后的任何替换都会在 2b 被抓住。
        //    只有旧快照没带身份时，才回退到执行开始时采集一次（仍会立刻复验）。
        //    打不开源即视为源已失效，直接失败，不写空身份。
        let source = PathBuf::from(&item.source_path);
        let target = PathBuf::from(&item.target_path);
        let source_identity = if item.source_identity.is_empty() {
            match fs::File::open(&source) {
                Ok(opened) => match file_identity(&opened) {
                    Ok(identity) => identity,
                    Err(error) => {
                        record_failure(
                            db,
                            caller,
                            &run,
                            &item_ref,
                            move_journal::CODE_SOURCE_CHANGED,
                            &format!("无法计算源文件身份: {error}"),
                        )?;
                        summary.failed += 1;
                        continue;
                    }
                },
                Err(_) => {
                    record_failure(
                        db,
                        caller,
                        &run,
                        &item_ref,
                        move_journal::CODE_SOURCE_MISSING,
                        "源文件已不在原位置",
                    )?;
                    summary.failed += 1;
                    continue;
                }
            }
        } else {
            // 冻结身份来自批准快照；这里只确认源此刻仍可打开，把「源已消失」与
            // 「源被替换」区分开，避免把前者误报成身份变化。
            if fs::File::open(&source).is_err() {
                record_failure(
                    db,
                    caller,
                    &run,
                    &item_ref,
                    move_journal::CODE_SOURCE_MISSING,
                    "源文件已不在原位置",
                )?;
                summary.failed += 1;
                continue;
            }
            item.source_identity.clone()
        };

        // 1b. 冻结意图（记录非空 source_identity）。重复执行同一计划不产生第二份记录。
        {
            let conn = db.lock()?;
            move_journal::record_intent(
                &conn,
                &NewMoveIntent {
                    operation_id: run.operation_id,
                    item_id: &item_id,
                    asset_id: item.asset_id,
                    source_path: &item.source_path,
                    target_path: &item.target_path,
                    source_identity: &source_identity,
                    updated_at: now_unix_seconds()?,
                },
            )?;
        }

        // 2. 源必须仍是原位置上的普通文件。
        match fs::symlink_metadata(&source) {
            Ok(metadata) if !metadata.is_dir() => {}
            Ok(_) => {
                record_failure(
                    db,
                    caller,
                    &run,
                    &item_ref,
                    move_journal::CODE_SOURCE_CHANGED,
                    "源路径已不是普通文件",
                )?;
                summary.failed += 1;
                continue;
            }
            Err(_) => {
                record_failure(
                    db,
                    caller,
                    &run,
                    &item_ref,
                    move_journal::CODE_SOURCE_MISSING,
                    "源文件已不在原位置",
                )?;
                summary.failed += 1;
                continue;
            }
        }

        // 2b. 复验源身份：重新打开并与冻结的 source_identity 比对。若源被替换
        //     （inode/volume 序列号与创建时间变化），拒绝移动，绝不用旧摘要继续。
        {
            let recheck: AppResult<String> = match fs::File::open(&source) {
                Ok(opened) => file_identity(&opened),
                Err(error) => Err(error.into()),
            };
            match recheck {
                Ok(identity) if identity == source_identity => {}
                Ok(_) => {
                    record_failure(
                        db,
                        caller,
                        &run,
                        &item_ref,
                        move_journal::CODE_SOURCE_CHANGED,
                        "源文件身份与批准时不一致，已拒绝移动（源可能被替换）",
                    )?;
                    summary.failed += 1;
                    continue;
                }
                Err(error) => {
                    record_failure(
                        db,
                        caller,
                        &run,
                        &item_ref,
                        move_journal::CODE_SOURCE_CHANGED,
                        &format!("复验源文件身份失败: {error}"),
                    )?;
                    summary.failed += 1;
                    continue;
                }
            }
        }

        // 3. 目标目录：先校验在总仓内，再创建，创建后复校（防止并发挂上链接）。
        let Some(parent) = target.parent() else {
            record_failure(
                db,
                caller,
                &run,
                &item_ref,
                move_journal::CODE_IO_ERROR,
                "目标路径没有父目录",
            )?;
            summary.failed += 1;
            continue;
        };
        if let Err(error) = fs::create_dir_all(parent) {
            record_failure(
                db,
                caller,
                &run,
                &item_ref,
                move_journal::CODE_PERMISSION_DENIED,
                &format!("无法创建目标分库目录: {error}"),
            )?;
            summary.failed += 1;
            continue;
        }
        if let Err(error) = managed_library::validate_import_target(library, parent) {
            record_failure(
                db,
                caller,
                &run,
                &item_ref,
                move_journal::CODE_PERMISSION_DENIED,
                &format!("目标分库不在当前总仓内: {error}"),
            )?;
            summary.failed += 1;
            continue;
        }
        if target.exists() {
            record_failure(
                db,
                caller,
                &run,
                &item_ref,
                move_journal::CODE_TARGET_EXISTS,
                "目标位置已存在同名文件",
            )?;
            summary.failed += 1;
            continue;
        }

        // 4. 同一卷内的改名。失败即真实失败，不做复制降级（会换文件身份）。
        if let Err(error) = fs::rename(&source, &target) {
            let code = classify_rename_error(&error);
            record_failure(
                db,
                caller,
                &run,
                &item_ref,
                code,
                &format!("移动失败: {error}"),
            )?;
            summary.failed += 1;
            continue;
        }

        // 5. 文件已经搬过去了：先冻结「已移动」，再落数据库，最后才 committed。
        let target_modified_at = fs::metadata(&target)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        let target_identity = match fs::File::open(&target) {
            Ok(opened) => file_identity(&opened)?,
            Err(error) => {
                record_unknown(db, caller, &run, &item_ref, &format!("{error}"))?;
                summary.unknown += 1;
                continue;
            }
        };
        {
            let conn = db.lock()?;
            move_journal::record_moved(
                &conn,
                run.operation_id,
                &item_id,
                &target_identity,
                target_modified_at,
                now_unix_seconds()?,
            )?;
        }

        // 摘要绝不沿用旧值：源可能已被替换，只有实际内容摘要才能证明「这就是原来那份文件」。
        let evidence = match managed_library::prepare_import_evidence(library, &target, "") {
            Ok(evidence) => evidence,
            Err(error) => {
                record_unknown(db, caller, &run, &item_ref, &format!("{error}"))?;
                summary.unknown += 1;
                continue;
            }
        };

        // 6. 一个事务里改路径、重写核验证据、把 journal 推到 committed。
        let transaction_result = {
            let conn = db.lock()?;
            let transaction = conn.unchecked_transaction()?;
            let outcome = (|| -> AppResult<()> {
                assets::update_file_location(
                    &transaction,
                    item.asset_id,
                    &normalize_path(&encode_native_path(&target)?),
                    target
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| AppError::invalid_arg("目标文件名不合法"))?,
                    target_modified_at,
                )?;
                let candidate = libraries::verification_candidate(&transaction, item.asset_id)?
                    .ok_or_else(|| AppError::not_found("素材记录已不在素材库中"))?;
                if !libraries::record_verified(
                    &transaction,
                    library,
                    &candidate,
                    &evidence.relative_path,
                    &evidence.file_identity,
                    &evidence.content_sha256,
                    &evidence.ownership_receipt,
                )? {
                    return Err(AppError::from(AgentErrorCode::LibraryChanged));
                }
                move_journal::record_committed(
                    &transaction,
                    run.operation_id,
                    &item_id,
                    now_unix_seconds()?,
                )?;
                Ok(())
            })();
            match outcome {
                Ok(()) => {
                    transaction.commit()?;
                    Ok(())
                }
                // 事务已回滚，但文件确实已经搬走：journal 停在 moved 留给对账，绝不假装成功。
                Err(error) => Err(error),
            }
        };

        if let Err(error) = transaction_result {
            record_unknown(db, caller, &run, &item_ref, &format!("{error}"))?;
            summary.unknown += 1;
            continue;
        }

        operations::record_item_state(
            db,
            caller,
            run.operation_ref,
            run.step_ref,
            &item_ref,
            "succeeded",
            "registered",
            None,
            &ResourceRef::generated("receipt"),
            "moved",
            Some(&encode_native_path(&target)?),
        )?;
        summary.moved += 1;
    }
    Ok(summary)
}

/// 把逐文件失败写进 journal 与操作收据（两处必须一致，避免摘要与日志互相矛盾）。
fn record_failure(
    db: &Database,
    caller: &CallerContext,
    run: &MoveExecution<'_>,
    item_ref: &ResourceRef,
    code: &'static str,
    message: &str,
) -> AppResult<()> {
    let asset_id = item_asset_id(item_ref)?;
    {
        let conn = db.lock()?;
        move_journal::record_failed(
            &conn,
            run.operation_id,
            &move_item_id(asset_id),
            code,
            now_unix_seconds()?,
        )?;
    }
    operations::record_item_state(
        db,
        caller,
        run.operation_ref,
        run.step_ref,
        item_ref,
        "failed",
        "none",
        Some(code),
        &ResourceRef::generated("receipt"),
        "move_failed",
        Some(message),
    )
}

/// 文件效果已发生但数据库未落地：记 `outcome_unknown`，交给对账判定。
fn record_unknown(
    db: &Database,
    caller: &CallerContext,
    run: &MoveExecution<'_>,
    item_ref: &ResourceRef,
    detail: &str,
) -> AppResult<()> {
    operations::record_item_state(
        db,
        caller,
        run.operation_ref,
        run.step_ref,
        item_ref,
        "outcome_unknown",
        "unknown",
        Some("OUTCOME_UNKNOWN"),
        &ResourceRef::generated("receipt"),
        "item_move_outcome_unknown",
        Some(detail),
    )
}

fn item_asset_id(item_ref: &ResourceRef) -> AppResult<i64> {
    item_ref
        .as_str()
        .strip_prefix("move_")
        .and_then(|rest| rest.parse::<i64>().ok())
        .ok_or_else(|| AppError::internal("移动项引用格式无效"))
}

fn classify_rename_error(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => move_journal::CODE_PERMISSION_DENIED,
        std::io::ErrorKind::NotFound => move_journal::CODE_SOURCE_MISSING,
        std::io::ErrorKind::AlreadyExists => move_journal::CODE_TARGET_EXISTS,
        _ if is_sharing_violation(error) => move_journal::CODE_TARGET_OCCUPIED,
        _ => move_journal::CODE_IO_ERROR,
    }
}

/// Windows 上「文件被其它进程占用」是 `ERROR_SHARING_VIOLATION (32)`，被归类为 Other。
fn is_sharing_violation(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(32)
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

fn now_unix_seconds() -> AppResult<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))?;
    i64::try_from(now.as_secs()).map_err(|_| AppError::internal("系统时间超出范围"))
}

/// 操作记录上是否已经收到取消请求。
///
/// 「停止移动」走 `operations::cancel_operation`：它把 `phase` 置为 `cancelling` 并交给
/// 执行侧自己收尾。执行循环因此每项都回读一次这条记录，而不是只信任进程内的原子标志——
/// WebView 重载后新进程拿到的是一份新标志，只有数据库里的状态是权威的。
fn cancel_requested(db: &Database, operation_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    let Some(record) = plans_db::operation(&conn, operation_id)? else {
        return Ok(false);
    };
    Ok(record.phase == "cancelling" || record.state == "cancelled")
}

/// 启动对账：按磁盘真实状态收尾 `intent`/`moved` 的移动日志。
///
/// 三种可判定状态：
/// - 源在、目标不在 → 从未离开原位置，日志回滚，item 复位为待执行；
/// - 源不在、目标在 → 文件已搬走但数据库未落地，补做路径与证据写入；
/// - 两者都在或都不在 → 唯一真实位置不可判定，记 `outcome_unknown`，不猜。
pub fn reconcile_interrupted_moves(db: &Database) -> AppResult<Vec<String>> {
    let operation_ids = {
        let conn = db.lock()?;
        operations_db::interrupted_operation_ids_for_capability(&conn, "assets.move.submit")?
    };
    let mut reconciled = Vec::new();
    for operation_id in operation_ids {
        reconcile_move_operation(db, &operation_id)?;
        let conn = db.lock()?;
        operations_db::refresh_recovery_unknown_items(&conn, &operation_id)?;
        reconciled.push(operation_id);
    }
    Ok(reconciled)
}

fn reconcile_move_operation(db: &Database, operation_id: &str) -> AppResult<()> {
    let (library, step_id, entries) = {
        let conn = db.lock()?;
        let Some(record) = plans_db::operation(&conn, operation_id)? else {
            return Ok(());
        };
        if record.capability != "assets.move.submit"
            || !matches!(record.state.as_str(), "queued" | "running" | "interrupted")
        {
            return Ok(());
        }
        let Some(library) = libraries::current(&conn)? else {
            return Ok(());
        };
        let Some(step) = operations_db::steps(&conn, operation_id)?
            .into_iter()
            .find(|step| step.capability == "assets.move.submit")
        else {
            return Ok(());
        };
        (
            library,
            step.step_id,
            move_journal::entries(&conn, operation_id)?,
        )
    };

    for entry in entries {
        if matches!(
            entry.state.as_str(),
            move_journal::STATE_COMMITTED
                | move_journal::STATE_FAILED
                | move_journal::STATE_ROLLED_BACK
        ) {
            continue;
        }
        let source_exists = Path::new(&entry.source_path).exists();
        let target_exists = Path::new(&entry.target_path).exists();
        match (source_exists, target_exists) {
            (true, false) => {
                let conn = db.lock()?;
                move_journal::record_rolled_back(
                    &conn,
                    operation_id,
                    &entry.item_id,
                    now_unix_seconds()?,
                )?;
                apply_move_observation(
                    &conn,
                    operation_id,
                    &step_id,
                    &entry.item_id,
                    MoveObservation::NoEffect,
                )?;
            }
            (false, true) => {
                if let Err(error) = finish_moved_entry(db, &library, &entry, operation_id, &step_id)
                {
                    let conn = db.lock()?;
                    apply_move_observation(
                        &conn,
                        operation_id,
                        &step_id,
                        &entry.item_id,
                        MoveObservation::Unknown,
                    )?;
                    tracing::warn!(
                        operation_id,
                        item_id = entry.item_id,
                        code = error.code(),
                        "移动对账无法完成数据库写入"
                    );
                }
            }
            _ => {
                let conn = db.lock()?;
                apply_move_observation(
                    &conn,
                    operation_id,
                    &step_id,
                    &entry.item_id,
                    MoveObservation::Unknown,
                )?;
            }
        }
    }
    Ok(())
}

fn finish_moved_entry(
    db: &Database,
    library: &LibrarySnapshot,
    entry: &MoveJournalEntry,
    operation_id: &str,
    step_id: &str,
) -> AppResult<()> {
    let target = PathBuf::from(&entry.target_path);
    // 目标证据核对：journal 里冻结点在 rename 之后采集的 target_identity，恢复时必须与
    // 磁盘上现在这个文件逐字相同。不然「目标位置上的文件已被替换」会被当成移动成功，
    // 用别人的内容去认领这条素材记录。旧 journal 没有 identity 时保持原行为。
    if let Some(expected) = entry.target_identity.as_deref() {
        if !expected.is_empty() {
            let opened = fs::File::open(&target).map_err(|error| {
                AppError::conflict(format!("恢复移动时目标文件不可读: {error}"))
            })?;
            let actual = file_identity(&opened)?;
            if actual != expected {
                return Err(AppError::conflict(
                    "目标文件身份与移动记录不一致，已拒绝认领（目标可能被替换）",
                ));
            }
        }
    }
    // 摘要不沿用旧值：恢复路径同样必须按实际内容重新计算，旧摘要只属于移动前的那个文件。
    let evidence = managed_library::prepare_import_evidence(library, &target, "")?;
    let target_modified_at = entry.target_modified_at.unwrap_or(0);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::invalid_arg("目标文件名不合法"))?
        .to_string();
    let conn = db.lock()?;
    let transaction = conn.unchecked_transaction()?;
    assets::update_file_location(
        &transaction,
        entry.asset_id,
        &normalize_path(&encode_native_path(&target)?),
        &file_name,
        target_modified_at,
    )?;
    let candidate = libraries::verification_candidate(&transaction, entry.asset_id)?
        .ok_or_else(|| AppError::not_found("素材记录已不在素材库中"))?;
    if !libraries::record_verified(
        &transaction,
        library,
        &candidate,
        &evidence.relative_path,
        &evidence.file_identity,
        &evidence.content_sha256,
        &evidence.ownership_receipt,
    )? {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    move_journal::record_committed(
        &transaction,
        operation_id,
        &entry.item_id,
        now_unix_seconds()?,
    )?;
    apply_move_observation_in(
        &transaction,
        operation_id,
        step_id,
        &entry.item_id,
        MoveObservation::Moved,
    )?;
    transaction.commit()?;
    Ok(())
}

enum MoveObservation {
    NoEffect,
    Moved,
    Unknown,
}

fn apply_move_observation(
    conn: &rusqlite::Connection,
    operation_id: &str,
    step_id: &str,
    item_id: &str,
    observation: MoveObservation,
) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    apply_move_observation_in(&transaction, operation_id, step_id, item_id, observation)?;
    transaction.commit()?;
    Ok(())
}

fn apply_move_observation_in(
    transaction: &rusqlite::Transaction<'_>,
    operation_id: &str,
    step_id: &str,
    item_id: &str,
    observation: MoveObservation,
) -> AppResult<()> {
    let Some(item) = operations_db::operation_item(transaction, operation_id, step_id, item_id)?
    else {
        return Ok(());
    };
    // 已经结算过的项不再覆盖：对账只补齐未完结的状态。
    if item.state == "succeeded" || item.state == "failed" {
        return Ok(());
    }
    let (state, effect_state, error_code, event) = match observation {
        MoveObservation::NoEffect => ("pending", "none", None, "move_item_reset_after_restart"),
        MoveObservation::Moved => (
            "succeeded",
            "registered",
            None,
            "move_item_committed_after_restart",
        ),
        MoveObservation::Unknown => (
            "outcome_unknown",
            "unknown",
            Some("OUTCOME_UNKNOWN"),
            "move_item_outcome_unknown",
        ),
    };
    let updated = operations_db::update_item(
        transaction,
        operation_id,
        step_id,
        item_id,
        state,
        effect_state,
        error_code,
        now_unix_seconds()?,
    )?;
    let receipt_id = format!(
        "system:item:{operation_id}:{step_id}:{item_id}:r{}",
        updated.revision
    );
    if !operations_db::receipt_exists(transaction, &receipt_id)? {
        operations_db::insert_receipt(
            transaction,
            &operations_db::NewOperationReceipt {
                receipt_id,
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                item_id: Some(item_id.to_string()),
                revision: updated.revision,
                event: event.to_string(),
                input_fingerprint: updated.input_fingerprint.clone(),
                target_ref: updated.target_ref.clone(),
                child_task_kind: updated.child_task_kind.clone(),
                child_task_id: updated.child_task_id.clone(),
                db_ref: updated.target_ref.clone(),
                effect_state: effect_state.to_string(),
                error_code: error_code.map(str::to_string),
                recorded_at: now_unix_seconds()?,
            },
        )?;
    }
    operations_db::refresh_progress(transaction, operation_id, now_unix_seconds()?)?;
    Ok(())
}
