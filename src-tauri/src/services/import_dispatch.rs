//! Dispatch accepted imports through the shared single-slot operation queue.
use std::sync::atomic::AtomicBool;

use crate::contracts::agent::ResourceRef;
use crate::error::{AppError, AppResult};
use crate::services::importer::ImportProgress;
use crate::services::{
    import_execute, import_submission, operations::OperationQueue, thumbnail::ThumbnailService,
};
use crate::state::Database;

struct Claim<'a> {
    queue: &'a OperationQueue,
    id: String,
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let _ = self.queue.release(&self.id);
    }
}

/// Multiple wakeups are harmless: only the queue owner can dispatch. The claim
/// is released on ordinary errors and unwinding, allowing subsequent wakeups.
pub fn drain<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    queue: &OperationQueue,
    thumbs: &ThumbnailService,
    progress: F,
) -> AppResult<u32> {
    let mut imported = 0_u32;
    loop {
        let Some(id) = queue.claim_next_for_capability(db, Some("imports.submit"))? else {
            return Ok(imported);
        };
        let claim = Claim { queue, id };
        let operation = ResourceRef::from_trusted(claim.id.clone())?;
        let result = (|| {
            // 执行身份必须按计划持久行里的 `origin_kind` 重建，不能固定成 GUI：
            // 原先这里调 `import_submission::execution_context`，它对
            // `principal_gui_host` 以外的主体直接返回 `UnsupportedCapability`，
            // 于是任何非 GUI 来源的导入操作即使已批准、已排到队首也永远排不空。
            // `operation_dispatch::execution_caller` 是同一手法的泛化版本
            // （principal/session/instance/grant_epoch 全部取自持久行），
            // 不新增任何权限：请求仍只作用于已经批准、已经提交且排到队首的那一个操作。
            // `import_ref` 仍然拒绝非 `imports.submit` 的操作。
            let caller = crate::services::operation_dispatch::execution_caller(db, &operation)?;
            let import_ref = import_submission::import_ref(db, &operation)?;
            import_execute::execute_import(
                db,
                thumbs,
                &caller,
                &import_ref,
                &operation,
                &AtomicBool::new(false),
                &progress,
            )
        })();
        match result {
            Ok(summary) => imported = imported.saturating_add(summary.imported),
            Err(error) => {
                tracing::error!(operation_id=%operation.as_str(), error_code=%error.code(), "导入操作执行未完成");
                // Worker owns domain reconciliation. Never turn an unknown file
                // effect into an unconditional success here.
                let conn = db.lock()?;
                if let Some(record) = crate::db::plans::operation(&conn, operation.as_str())? {
                    if matches!(record.state.as_str(), "queued" | "running") {
                        if error.code() == "GRANT_REVOKED" {
                            crate::db::operations::interrupt_for_reauthorization(
                                &conn,
                                operation.as_str(),
                                now()?,
                            )?;
                        } else {
                            crate::db::operations::complete_operation(
                                &conn,
                                operation.as_str(),
                                "outcome_unknown",
                                now()?,
                            )?;
                        }
                    }
                }
            }
        }
    }
}
fn now() -> AppResult<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|_| AppError::internal("系统时间无效"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{authorization, import_prepare, managed_library, operations};
    fn setup(root: &std::path::Path) -> (Database, crate::contracts::caller::CallerContext) {
        let conn = crate::db::init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        let caller = authorization::build_caller_context(
            &conn,
            authorization::TrustedAdapterIdentity::Gui {
                session_id: "session_dispatch".into(),
                instance_id: "instance_dispatch".into(),
            },
            ResourceRef::generated("request"),
            vec![],
        )
        .unwrap();
        (Database::new(conn), caller)
    }
    fn accept(
        db: &Database,
        caller: &crate::contracts::caller::CallerContext,
        file: &std::path::Path,
    ) -> ResourceRef {
        let plan = import_prepare::prepare_import(
            db,
            caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![file.to_str().unwrap().into()],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: vec![],
            },
        )
        .unwrap();
        import_submission::confirm(
            db,
            caller,
            &plan.plan_ref,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap()
        .operation
        .operation_id
    }

    fn accept_authorized_directory(
        db: &Database,
        caller: &crate::contracts::caller::CallerContext,
        directory: &std::path::Path,
    ) -> (
        ResourceRef,
        crate::services::authorization::DirectoryGrantView,
    ) {
        let grant = authorization::grant_selected_directory(
            db,
            caller,
            directory,
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        let plan = import_prepare::prepare_import(
            db,
            caller,
            import_prepare::PrepareImportRequest {
                source_ref: grant.directory_ref.clone(),
                paths: vec![],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: vec![],
            },
        )
        .unwrap();
        let operation = import_submission::confirm(
            db,
            caller,
            &plan.plan_ref,
            &ResourceRef::generated("idempotency"),
        )
        .unwrap()
        .operation
        .operation_id;
        (operation, grant)
    }

    fn write_large_png(path: &std::path::Path) {
        let image = image::RgbImage::from_fn(1200, 1200, |x, y| {
            let value = x.wrapping_mul(73_856_093)
                ^ y.wrapping_mul(19_349_663)
                ^ x.wrapping_mul(y).wrapping_mul(83_492_791);
            image::Rgb([value as u8, (value >> 8) as u8, (value >> 16) as u8])
        });
        image.save(path).unwrap();
        assert!(std::fs::metadata(path).unwrap().len() > 1024 * 1024);
    }
    fn asset_count(db: &Database) -> i64 {
        db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .unwrap()
    }
    #[test]
    fn queue_hash_read_observes_revocation_and_cancellation_before_the_next_chunk() {
        use crate::utils::hash::with_hash_chunk_hook;
        use std::cell::Cell;
        use std::rc::Rc;
        for (revoke, stop_after_chunk) in [(false, 1), (true, 1), (false, 10), (true, 10)] {
            let root = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let cache = tempfile::tempdir().unwrap();
            std::fs::write(source.path().join("large.mp4"), vec![7u8; 3 * 1024 * 1024]).unwrap();
            let (db, caller) = setup(root.path());
            let db = Rc::new(db);
            let (operation, grant) = accept_authorized_directory(&db, &caller, source.path());
            let hook_db = Rc::clone(&db);
            let hook_caller = caller.clone();
            let hook_operation = operation.clone();
            let count = Rc::new(Cell::new(0));
            let hook_count = Rc::clone(&count);
            with_hash_chunk_hook(
                move || {
                    hook_count.set(hook_count.get() + 1);
                    if hook_count.get() == stop_after_chunk {
                        if revoke {
                            authorization::revoke_directory_grant(
                                &hook_db,
                                &hook_caller,
                                &grant.grant_ref,
                            )
                            .unwrap();
                        } else {
                            operations::cancel_operation(&hook_db, &hook_caller, &hook_operation)
                                .unwrap();
                        }
                    }
                },
                || {
                    drain(
                        &db,
                        &OperationQueue::default(),
                        &ThumbnailService::new(cache.path()).unwrap(),
                        |_| {},
                    )
                    .unwrap();
                },
            );
            assert_eq!(count.get(), stop_after_chunk);
            assert_eq!(asset_count(&db), 0);
            assert!(!root.path().join("large.mp4").exists());
            let conn = db.lock().unwrap();
            let state = crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state;
            assert_eq!(state, if revoke { "interrupted" } else { "cancelled" });
        }
    }

    #[test]
    fn queue_dispatches_real_imports_and_repeated_wakeup_does_not_copy_again() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("test.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let queue = OperationQueue::default();
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        drain(&db, &queue, &thumbs, |_| {}).unwrap();
        drain(&db, &queue, &thumbs, |_| {}).unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn restart_requires_explicit_resume_before_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("test.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        operations::reconcile_startup(&db).unwrap();
        let queue = OperationQueue::default();
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        drain(&db, &queue, &thumbs, |_| {}).unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut restarted = caller.clone();
        restarted.session_id = Some(ResourceRef::generated("session"));
        restarted.instance_id = ResourceRef::generated("instance");
        operations::resume_operation(&db, &restarted, &operation).unwrap();
        drain(&db, &queue, &thumbs, |_| {}).unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn restart_resume_rebinds_only_the_same_verified_import_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let wrong_source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        image::RgbImage::new(8, 8)
            .save(source.path().join("test.jpg"))
            .unwrap();
        image::RgbImage::new(8, 8)
            .save(wrong_source.path().join("test.jpg"))
            .unwrap();
        let (db, original) = setup(root.path());
        let (operation, _) = accept_authorized_directory(&db, &original, source.path());
        operations::reconcile_startup(&db).unwrap();
        let restarted = authorization::build_caller_context(
            &db.lock().unwrap(),
            authorization::TrustedAdapterIdentity::Gui {
                session_id: "session_dispatch_restarted".into(),
                instance_id: "instance_dispatch_restarted".into(),
            },
            ResourceRef::generated("request"),
            vec![],
        )
        .unwrap();

        let wrong_grant = authorization::grant_selected_directory(
            &db,
            &restarted,
            wrong_source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        assert_eq!(
            operations::resume_import_operation(
                &db,
                &restarted,
                &operation,
                wrong_grant.directory_ref.as_ref().unwrap(),
            )
            .unwrap_err()
            .code(),
            "PLAN_CHANGED"
        );
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );

        let same_grant = authorization::grant_selected_directory(
            &db,
            &restarted,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        operations::resume_import_operation(
            &db,
            &restarted,
            &operation,
            same_grant.directory_ref.as_ref().unwrap(),
        )
        .unwrap();
        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM assets", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn revocation_during_staging_copy_leaves_no_published_file_and_resumes_after_reauthorization() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let source_file = source.path().join("large.png");
        write_large_png(&source_file);
        let (db, caller) = setup(root.path());
        let (operation, grant) = accept_authorized_directory(&db, &caller, source.path());
        let queue = OperationQueue::default();
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let revoked = AtomicBool::new(false);
        drain(&db, &queue, &thumbs, |event| {
            if event.message.as_deref() == Some("正在安全暂存并校验文件")
                && !revoked.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                authorization::revoke_directory_grant(&db, &caller, &grant.grant_ref).unwrap();
            }
        })
        .unwrap();

        assert!(
            revoked.load(std::sync::atomic::Ordering::SeqCst),
            "copy progress did not reach the staging checkpoint"
        );
        assert!(!root.path().join("large.png").exists());
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
        let stage_directory = root.path().join(".import-stage");
        let staged: Vec<_> = std::fs::read_dir(&stage_directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(staged.len(), 1);
        assert!(
            std::fs::metadata(&staged[0]).unwrap().len()
                < std::fs::metadata(&source_file).unwrap().len()
        );
        assert_eq!(asset_count(&db), 0);

        let restarted = authorization::build_caller_context(
            &db.lock().unwrap(),
            authorization::TrustedAdapterIdentity::Gui {
                session_id: "session_dispatch_reauthorized".into(),
                instance_id: "instance_dispatch_reauthorized".into(),
            },
            ResourceRef::generated("request"),
            vec![],
        )
        .unwrap();
        let fresh_grant = authorization::grant_selected_directory(
            &db,
            &restarted,
            source.path(),
            authorization::DirectoryPurpose::ImportSource,
            false,
        )
        .unwrap();
        operations::resume_import_operation(
            &db,
            &restarted,
            &operation,
            fresh_grant.directory_ref.as_ref().unwrap(),
        )
        .unwrap();
        drain(&db, &OperationQueue::default(), &thumbs, |_| {}).unwrap();

        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        assert!(root.path().join("large.png").exists());
        assert_eq!(asset_count(&db), 1);
        assert_eq!(std::fs::read_dir(stage_directory).unwrap().count(), 0);
    }
    #[test]
    fn cancelling_waiting_import_finishes_without_dispatch_or_files() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("test.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let snapshot = operations::cancel_operation(&db, &caller, &operation).unwrap();
        assert_eq!(
            snapshot.state,
            crate::contracts::agent::OperationState::Cancelled
        );
        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn revoking_source_grant_during_processing_stops_before_copy_or_registration() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("authorized.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let (operation, grant) = accept_authorized_directory(&db, &caller, source.path());
        let revoked = AtomicBool::new(false);

        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |event| {
                if event.phase == "processing"
                    && event.operation_id.as_deref() == Some(operation.as_str())
                    && !revoked.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    authorization::revoke_directory_grant(&db, &caller, &grant.grant_ref).unwrap();
                }
            },
        )
        .unwrap();

        assert!(revoked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!root.path().join("authorized.jpg").exists());
        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
    }

    #[test]
    fn explicit_source_mtime_change_during_processing_is_not_registered() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("mtime-only.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let changed = AtomicBool::new(false);

        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |event| {
                if event.phase == "processing"
                    && event.operation_id.as_deref() == Some(operation.as_str())
                    && !changed.swap(true, Ordering::SeqCst)
                {
                    let file_handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
                    let modified = file_handle.metadata().unwrap().modified().unwrap();
                    file_handle
                        .set_times(
                            std::fs::FileTimes::new()
                                .set_modified(modified + Duration::from_secs(5)),
                        )
                        .unwrap();
                }
            },
        )
        .unwrap();

        assert!(changed.load(Ordering::SeqCst));
        assert_eq!(asset_count(&db), 0);
        assert!(!root.path().join("mtime-only.jpg").exists());
        assert_ne!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    }

    #[test]
    fn explicit_source_mtime_change_after_publication_is_not_registered() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("published-mtime-only.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let changed = AtomicBool::new(false);

        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |event| {
                if event.message.as_deref() == Some("publication_verified")
                    && event.operation_id.as_deref() == Some(operation.as_str())
                    && !changed.swap(true, Ordering::SeqCst)
                {
                    let file_handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
                    let modified = file_handle.metadata().unwrap().modified().unwrap();
                    file_handle
                        .set_times(
                            std::fs::FileTimes::new()
                                .set_modified(modified + Duration::from_secs(5)),
                        )
                        .unwrap();
                }
            },
        )
        .unwrap();

        assert!(changed.load(Ordering::SeqCst));
        assert!(root.path().join("published-mtime-only.jpg").exists());
        assert_eq!(asset_count(&db), 0);
        assert_ne!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    }

    #[test]
    fn authorized_source_mtime_change_after_publication_is_not_registered() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("authorized-published-mtime-only.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let (operation, _grant) = accept_authorized_directory(&db, &caller, source.path());
        let changed = AtomicBool::new(false);

        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |event| {
                if event.message.as_deref() == Some("publication_verified")
                    && event.operation_id.as_deref() == Some(operation.as_str())
                    && !changed.swap(true, Ordering::SeqCst)
                {
                    let file_handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
                    let modified = file_handle.metadata().unwrap().modified().unwrap();
                    file_handle
                        .set_times(
                            std::fs::FileTimes::new()
                                .set_modified(modified + Duration::from_secs(5)),
                        )
                        .unwrap();
                }
            },
        )
        .unwrap();

        assert!(changed.load(Ordering::SeqCst));
        assert!(root
            .path()
            .join("authorized-published-mtime-only.jpg")
            .exists());
        assert_eq!(asset_count(&db), 0);
        assert_ne!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    }

    #[test]
    fn revoking_source_grant_after_publication_preserves_intent_without_registering_asset() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("published.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let (operation, grant) = accept_authorized_directory(&db, &caller, source.path());
        let revoked = AtomicBool::new(false);

        drain(
            &db,
            &OperationQueue::default(),
            &ThumbnailService::new(cache.path()).unwrap(),
            |event| {
                if event.message.as_deref() == Some("publication_verified")
                    && event.operation_id.as_deref() == Some(operation.as_str())
                    && !revoked.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    authorization::revoke_directory_grant(&db, &caller, &grant.grant_ref).unwrap();
                }
            },
        )
        .unwrap();

        assert!(revoked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(root.path().join("published.jpg").exists());
        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM import_execution_journal
                  WHERE created_file_identity IS NOT NULL OR published_file_identity IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
    }
    #[test]
    fn corrupt_mapping_is_visible_and_does_not_strand_later_imports() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("test.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let bad = accept(&db, &caller, &file);
        let good = accept(&db, &caller, &file);
        {
            let conn = db.lock().unwrap();
            conn.execute("UPDATE plans SET scope_ref='missing_import_plan' WHERE plan_id=(SELECT plan_id FROM operations WHERE operation_id=?1)",[bad.as_str()]).unwrap();
            conn.execute(
                "UPDATE operations SET created_at=1 WHERE operation_id=?1",
                [bad.as_str()],
            )
            .unwrap();
        }
        let queue = OperationQueue::default();
        drain(
            &db,
            &queue,
            &ThumbnailService::new(cache.path()).unwrap(),
            |_| {},
        )
        .unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(
            crate::db::plans::operation(&conn, bad.as_str())
                .unwrap()
                .unwrap()
                .state,
            "outcome_unknown"
        );
        assert_eq!(
            crate::db::plans::operation(&conn, good.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
    }

    fn recover_dispatch_checkpoint(checkpoint: &str, registered_before_restart: i64) {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("checkpoint.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let queue = OperationQueue::default();
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drain(&db, &queue, &thumbs, |event| {
                if (checkpoint == "publication_verified"
                    && event.message.as_deref() == Some(checkpoint))
                    || (checkpoint == "done" && event.phase == "done")
                {
                    panic!("injected dispatch checkpoint");
                }
            })
        }));
        assert!(interrupted.is_err());
        assert!(root.path().join("checkpoint.jpg").exists());
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM assets", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            registered_before_restart
        );
        std::fs::remove_file(&file).unwrap();
        operations::reconcile_startup(&db).unwrap();
        // A new process uses a fresh queue and GUI session; dispatch alone must
        // not continue until the trusted UI explicitly resumes this operation.
        let restarted_queue = OperationQueue::default();
        drain(&db, &restarted_queue, &thumbs, |_| {}).unwrap();
        assert_eq!(
            crate::db::plans::operation(&db.lock().unwrap(), operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "interrupted"
        );
        let mut restarted = caller.clone();
        restarted.session_id = Some(ResourceRef::generated("session"));
        restarted.instance_id = ResourceRef::generated("instance");
        operations::resume_operation(&db, &restarted, &operation).unwrap();
        drain(&db, &restarted_queue, &thumbs, |_| {}).unwrap();
        let conn = db.lock().unwrap();
        let record = crate::db::plans::operation(&conn, operation.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(record.state, "succeeded");
        assert_eq!(record.completed, 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM assets", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let step = crate::db::operations::steps(&conn, operation.as_str())
            .unwrap()
            .remove(0);
        let receipts =
            crate::db::operations::receipts(&conn, operation.as_str(), &step.step_id).unwrap();
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| receipt.effect_state == "registered")
                .count(),
            1
        );
        assert!(receipts.iter().any(|receipt| receipt
            .db_ref
            .as_deref()
            .is_some_and(|reference| reference.starts_with("asset:"))));
        assert!(root.path().join("checkpoint.jpg").exists());
        assert!(!root.path().join("checkpoint(1).jpg").exists());
    }

    #[test]
    fn dispatch_recovers_publication_before_database_after_explicit_restart_resume() {
        recover_dispatch_checkpoint("publication_verified", 0);
    }

    #[test]
    fn dispatch_recovers_database_before_receipt_after_explicit_restart_resume() {
        recover_dispatch_checkpoint("done", 1);
    }

    #[test]
    fn concurrent_drain_cannot_dispatch_while_first_worker_owns_queue() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = source.path().join("concurrent.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = setup(root.path());
        let operation = accept(&db, &caller, &file);
        let queue = OperationQueue::default();
        let thumbs = ThumbnailService::new(cache.path()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let paused = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                drain(&db, &queue, &thumbs, |event| {
                    if event.phase == "processing"
                        && !paused.swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        barrier.wait();
                        barrier.wait();
                    }
                })
                .unwrap();
            });
            barrier.wait();
            let events = std::sync::atomic::AtomicUsize::new(0);
            drain(&db, &queue, &thumbs, |_| {
                events.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .unwrap();
            let second_events = events.load(std::sync::atomic::Ordering::SeqCst);
            barrier.wait();
            first.join().unwrap();
            assert_eq!(
                second_events, 0,
                "the second dispatcher must not execute the claimed item"
            );
        });
        let conn = db.lock().unwrap();
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .state,
            "succeeded"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM assets", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let step = crate::db::operations::steps(&conn, operation.as_str())
            .unwrap()
            .remove(0);
        assert_eq!(
            crate::db::operations::receipts(&conn, operation.as_str(), &step.step_id)
                .unwrap()
                .iter()
                .filter(|receipt| receipt.effect_state == "registered")
                .count(),
            1
        );
        assert!(!root.path().join("concurrent(1).jpg").exists());
    }
}
