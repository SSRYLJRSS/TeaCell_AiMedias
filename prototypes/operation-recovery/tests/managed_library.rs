use std::fs;
use teacell_operation_recovery_poc::{
    digest,
    library_db::{AssetRecord, LibraryStore},
    managed_library::{
        classify, cleanup_source_fault, mark_fixture_root, migrate_copy, prepare_source_cleanup,
        relocate_already_moved, Classification,
    },
};

fn fixture() -> (tempfile::TempDir, LibraryStore) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("总仓 中文");
    mark_fixture_root(&root, "fixture-library-A").unwrap();
    let s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    s.initialize("fixture-library-A", &root).unwrap();
    for id in 1..=2 {
        let location = std::path::PathBuf::from(format!("分库/{id}.jpg"));
        fs::create_dir_all(root.join("分库")).unwrap();
        fs::write(root.join(&location), b"same-hash-different-assets").unwrap();
        s.seed(&AssetRecord {
            id,
            location: location.clone(),
            digest: digest(&root.join(location)).unwrap(),
            trusted_receipt: true,
            tags: format!("tags-{id}"),
            rating: id,
        })
        .unwrap();
    }
    (temp, s)
}

#[test]
fn copy_cancel_reopen_resume_preserves_identity_metadata_and_untracked_files() {
    let (temp, mut s) = fixture();
    let original = s.records().unwrap();
    let (_, source, epoch) = s.locator().unwrap();
    let untracked = source.join("untracked.txt");
    fs::write(&untracked, b"keep").unwrap();
    let target = temp.path().join("new-root");
    fs::create_dir(&target).unwrap();
    assert!(!migrate_copy(&mut s, &target, 1).unwrap());
    assert_eq!(s.locator().unwrap().1, source);
    drop(s);
    let mut s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    assert!(migrate_copy(&mut s, &target, usize::MAX).unwrap());
    assert_eq!(s.records().unwrap(), original);
    assert_eq!(s.locator().unwrap().2, epoch + 1);
    for record in s.records().unwrap() {
        assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
        assert!(source.join(record.location).exists());
    }
    assert_eq!(fs::read(&untracked).unwrap(), b"keep");
    assert!(!target.join("untracked.txt").exists());
}

#[test]
fn offline_is_not_external_and_wrong_same_named_root_is_rejected() {
    let (temp, mut s) = fixture();
    let original = s.records().unwrap();
    let (_, source, _) = s.locator().unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&source, &moved).unwrap();
    for record in &original {
        assert_eq!(classify(&s, record).unwrap(), Classification::Offline);
    }
    mark_fixture_root(&source, "other-library").unwrap();
    assert_eq!(
        classify(&s, &original[0]).unwrap(),
        Classification::WrongLibrary
    );
    assert!(relocate_already_moved(&mut s, &source).is_err());
    relocate_already_moved(&mut s, &moved).unwrap();
    assert_eq!(s.records().unwrap(), original);
    assert_eq!(
        classify(&s, &original[0]).unwrap(),
        Classification::Verified
    );
}

#[test]
fn legacy_inside_is_unknown_and_external_record_preserves_metadata() {
    let (temp, s) = fixture();
    let (_, root, _) = s.locator().unwrap();
    let outside = temp.path().join("outside.jpg");
    fs::write(&outside, b"original").unwrap();
    let record = AssetRecord {
        id: 3,
        location: outside.clone(),
        digest: digest(&outside).unwrap(),
        trusted_receipt: false,
        tags: "keep-legacy-tags".into(),
        rating: 5,
    };
    s.seed(&record).unwrap();
    assert_eq!(
        classify(&s, &record).unwrap(),
        Classification::ExternalPending
    );
    let mut inside = record.clone();
    inside.location = root.join("分库/1.jpg");
    assert_eq!(classify(&s, &inside).unwrap(), Classification::Unknown);
    assert_eq!(s.records().unwrap()[2], record);
    assert_eq!(fs::read(outside).unwrap(), b"original");
}

#[test]
fn overlap_target_collision_and_invalid_relative_path_fail_closed() {
    let (temp, mut s) = fixture();
    let (_, root, _) = s.locator().unwrap();
    assert!(migrate_copy(&mut s, &root, usize::MAX).is_err());
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    assert!(migrate_copy(&mut s, &nested, usize::MAX).is_err());
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("foreign.txt"), b"keep").unwrap();
    assert!(migrate_copy(&mut s, &target, usize::MAX).is_err());
    assert_eq!(fs::read(target.join("foreign.txt")).unwrap(), b"keep");
    let mut bad = s.records().unwrap()[0].clone();
    bad.location = "../outside.jpg".into();
    assert_eq!(classify(&s, &bad).unwrap(), Classification::Unknown);
}

#[test]
fn manual_relocation_with_missing_or_changed_asset_keeps_original_locator() {
    let (temp, mut s) = fixture();
    let (_, original, epoch) = s.locator().unwrap();
    let target = temp.path().join("target");
    mark_fixture_root(&target, "fixture-library-A").unwrap();
    assert!(relocate_already_moved(&mut s, &target).is_err());
    assert_eq!(s.locator().unwrap().1, original);
    assert_eq!(s.locator().unwrap().2, epoch);
    fs::create_dir(target.join("分库")).unwrap();
    for record in s.records().unwrap() {
        fs::write(target.join(record.location), b"changed").unwrap();
    }
    assert!(relocate_already_moved(&mut s, &target).is_err());
    assert_eq!(s.locator().unwrap().1, original);
    assert_eq!(s.locator().unwrap().2, epoch);
}

#[test]
fn copy_process_exit_preserves_source_and_reopens_receipt_for_resume() {
    let (temp, s) = fixture();
    let original = s.records().unwrap();
    let (_, source, epoch) = s.locator().unwrap();
    drop(s);
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    let db = temp.path().join("library.sqlite");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
        .arg(&db)
        .arg(&target)
        .arg("2")
        .arg("migration-copy-first")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let mut s = LibraryStore::open(&db).unwrap();
    assert_eq!(s.locator().unwrap().1, source);
    assert_eq!(s.locator().unwrap().2, epoch);
    assert!(migrate_copy(&mut s, &target, usize::MAX).unwrap());
    assert_eq!(s.records().unwrap(), original);
    for record in original {
        assert!(source.join(&record.location).is_file());
        assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
    }
}

#[test]
fn directory_link_to_external_files_is_unknown_and_not_migrated() {
    let (temp, mut s) = fixture();
    let (_, root, _) = s.locator().unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("foreign.jpg"), b"keep").unwrap();
    let link = root.join("linked");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd.exe")
            .creation_flags(0x08000000)
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction fixture creation failed: {:?}",
            output
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let record = AssetRecord {
        id: 5,
        location: "linked/foreign.jpg".into(),
        digest: digest(&outside.join("foreign.jpg")).unwrap(),
        trusted_receipt: true,
        tags: "keep".into(),
        rating: 1,
    };
    assert_eq!(classify(&s, &record).unwrap(), Classification::Unknown);
    s.seed(&record).unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    assert!(migrate_copy(&mut s, &target, usize::MAX).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    assert_eq!(fs::read(outside.join("foreign.jpg")).unwrap(), b"keep");
}

#[test]
fn resumed_copy_rejects_target_directory_link_before_any_external_publication() {
    let (temp, s) = fixture();
    let original = s.records().unwrap();
    let locator = s.locator().unwrap();
    drop(s);
    let db = temp.path().join("library.sqlite");
    let target = temp.path().join("target");
    let outside = temp.path().join("outside");
    fs::create_dir(&target).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"untouched").unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
        .arg(&db)
        .arg(&target)
        .arg("2")
        .arg("migration-partial")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let link = target.join("分库");
    fs::remove_dir(&link).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd.exe")
            .creation_flags(0x08000000)
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction creation failed: {output:?}"
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    struct LinkGuard(std::path::PathBuf);
    impl Drop for LinkGuard {
        fn drop(&mut self) {
            #[cfg(windows)]
            let _ = fs::remove_dir(&self.0);
            #[cfg(unix)]
            let _ = fs::remove_file(&self.0);
        }
    }
    let guard = LinkGuard(link.clone());
    let mut s = LibraryStore::open(&db).unwrap();
    let error = migrate_copy(&mut s, &target, usize::MAX).unwrap_err();
    assert!(error.to_string().contains("LINK_REJECTED"), "{error}");
    assert_eq!(s.locator().unwrap(), locator);
    assert_eq!(s.records().unwrap(), original);
    assert!(!outside.join("1.jpg").exists());
    assert!(!outside.join("2.jpg").exists());
    assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"untouched");
    for record in &original {
        assert_eq!(
            digest(&locator.1.join(&record.location)).unwrap(),
            record.digest
        );
    }
    drop(guard);
    // Explicitly resume after removing the offending link, preserving the
    // existing partial-copy receipt rather than starting another operation.
    assert!(migrate_copy(&mut s, &target, usize::MAX).unwrap());
    assert_eq!(s.locator().unwrap().2, locator.2 + 1);
    assert_eq!(s.records().unwrap(), original);
}

#[test]
fn migration_crash_windows_keep_consistent_locator_and_original_metadata() {
    for fault in [
        "migration-partial",
        "migration-published",
        "migration-before-switch",
        "migration-switched",
    ] {
        let (temp, s) = fixture();
        let original = s.records().unwrap();
        let (_, source, epoch) = s.locator().unwrap();
        drop(s);
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let db = temp.path().join("library.sqlite");
        let status =
            std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
                .arg(&db)
                .arg(&target)
                .arg("2")
                .arg(fault)
                .status()
                .unwrap();
        assert_eq!(status.code(), Some(77), "{fault}");
        let mut s = LibraryStore::open(&db).unwrap();
        if fault == "migration-switched" {
            assert_eq!(s.locator().unwrap().1, target.canonicalize().unwrap());
            assert_eq!(s.locator().unwrap().2, epoch + 1);
            assert!(migrate_copy(&mut s, &target, usize::MAX).unwrap());
            assert_eq!(s.locator().unwrap().2, epoch + 1);
        } else {
            assert_eq!(s.locator().unwrap().1, source);
            assert_eq!(s.locator().unwrap().2, epoch);
            assert!(migrate_copy(&mut s, &target, usize::MAX).unwrap());
        }
        assert_eq!(s.records().unwrap(), original);
        for record in original {
            assert!(source.join(&record.location).exists());
            assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
        }
    }
}

#[test]
fn single_library_registration_cannot_be_silently_replaced() {
    let (temp, s) = fixture();
    let before = s.locator().unwrap();
    assert!(s.initialize("second-library", temp.path()).is_err());
    assert_eq!(s.locator().unwrap(), before);
}

#[test]
fn equal_hash_staging_collision_is_not_owned_without_witness() {
    let (temp, mut s) = fixture();
    let (_, root, epoch) = s.locator().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    assert!(!migrate_copy(&mut s, &target, 0).unwrap());
    let record = s.records().unwrap()[0].clone();
    fs::copy(
        root.join(&record.location),
        target.join(".migration-stage/1"),
    )
    .unwrap();
    let error = migrate_copy(&mut s, &target, usize::MAX).unwrap_err();
    assert_eq!(error.to_string(), "STAGING_IDENTITY_UNKNOWN");
    assert!(!target.join(record.location).exists());
    assert_eq!(s.locator().unwrap().1, root);
    assert_eq!(s.locator().unwrap().2, epoch);
}

#[test]
fn cleanup_requires_separate_review_and_keeps_unknown_files_and_asset_metadata() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let records = s.records().unwrap();
    fs::write(source.join("unknown.txt"), b"keep").unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    assert!(prepare_source_cleanup(&s).is_err());
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    assert_eq!(prepare_source_cleanup(&s).unwrap(), 2);
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "CLEANUP_REVIEW_REQUIRED"
    );
    assert!(source.join(&records[0].location).exists());
    s.approve_cleanup_fixture(2).unwrap();
    cleanup_source_fault(&mut s, 2, "").unwrap();
    cleanup_source_fault(&mut s, 2, "").unwrap();
    assert_eq!(s.records().unwrap(), records);
    assert_eq!(fs::read(source.join("unknown.txt")).unwrap(), b"keep");
    for record in records {
        assert!(!source.join(&record.location).exists());
        assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
    }
    assert!(source.join(".teacell-library-id").is_file());
}

#[test]
fn equal_hash_source_replacement_after_review_is_preserved() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let record = s.records().unwrap()[0].clone();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    let path = source.join(&record.location);
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"same-hash-different-assets").unwrap();
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "SOURCE_IDENTITY_CHANGED"
    );
    assert_eq!(fs::read(path).unwrap(), b"same-hash-different-assets");
    assert!(source.join("分库/2.jpg").is_file());
}

#[test]
fn equal_hash_target_replacement_prevents_source_cleanup() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    fs::remove_file(target.join("分库/1.jpg")).unwrap();
    fs::write(target.join("分库/1.jpg"), b"same-hash-different-assets").unwrap();
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "DESTINATION_CHANGED"
    );
    for record in s.records().unwrap() {
        assert!(source.join(record.location).exists());
    }
}

#[test]
fn cleanup_crash_recovers_only_reviewed_items_after_explicit_call() {
    for fault in [
        "cleanup-intent",
        "cleanup-unlinked",
        "cleanup-receipt",
        "cleanup-witness",
    ] {
        let (temp, mut s) = fixture();
        let (_, source, _) = s.locator().unwrap();
        let original = s.records().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        migrate_copy(&mut s, &target, usize::MAX).unwrap();
        prepare_source_cleanup(&s).unwrap();
        s.approve_cleanup_fixture(2).unwrap();
        drop(s);
        let status =
            std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
                .arg(temp.path().join("library.sqlite"))
                .arg(&target)
                .arg("2")
                .arg(fault)
                .status()
                .unwrap();
        assert_eq!(status.code(), Some(77));
        let mut s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
        assert!(source.join("分库/2.jpg").exists());
        cleanup_source_fault(&mut s, 2, "").unwrap();
        assert_eq!(s.records().unwrap(), original);
        for record in original {
            assert!(!source.join(&record.location).exists());
            assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
        }
    }
}

#[test]
fn cleanup_target_offline_preserves_remaining_source_files() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    fs::rename(&target, temp.path().join("offline")).unwrap();
    assert!(cleanup_source_fault(&mut s, 2, "").is_err());
    for record in s.records().unwrap() {
        assert!(source.join(record.location).exists());
    }
}

#[test]
fn changing_the_reviewed_record_scope_cannot_remove_source_items() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    let record = s.records().unwrap()[0].clone();
    s.seed(&AssetRecord { id: 3, ..record }).unwrap();
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "CLEANUP_SCOPE_CHANGED"
    );
    assert!(source.join("分库/1.jpg").exists());
    assert!(source.join("分库/2.jpg").exists());
}

#[test]
fn repeated_maintenance_preserves_history_and_old_epoch_cannot_approve_or_execute() {
    let (temp, mut s) = fixture();
    let records = s.records().unwrap();
    let (_, source, initial_epoch) = s.locator().unwrap();
    let target = temp.path().join("second-root");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    cleanup_source_fault(&mut s, 2, "").unwrap();
    s.archive_finished_fixture(2).unwrap();
    let history = s.history_fixture().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].source, source.canonicalize().unwrap());
    assert_eq!(history[0].target, target.canonicalize().unwrap());
    assert_eq!(history[0].records, records);
    assert_eq!(history[0].items.len(), 2);
    assert!(history[0].items.iter().all(|item| item.state == "complete"));

    let next = temp.path().join("third-root");
    fs::create_dir(&next).unwrap();
    migrate_copy(&mut s, &next, usize::MAX).unwrap();
    assert_eq!(s.locator().unwrap().2, initial_epoch + 2);
    prepare_source_cleanup(&s).unwrap();
    assert!(s.approve_cleanup_fixture(2).is_err());
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "LIBRARY_EPOCH_CHANGED"
    );
    assert!(target.join("分库/1.jpg").exists());
    assert_eq!(
        cleanup_source_fault(&mut s, 3, "").unwrap_err().to_string(),
        "CLEANUP_REVIEW_REQUIRED"
    );
    s.approve_cleanup_fixture(3).unwrap();
    cleanup_source_fault(&mut s, 3, "").unwrap();
    s.archive_finished_fixture(3).unwrap();
    drop(s);
    let s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    let complete = s.history_fixture().unwrap();
    assert_eq!(complete.len(), 2);
    assert_eq!(complete[0], history[0]);
    assert_eq!(complete[1].source, target.canonicalize().unwrap());
    assert_eq!(complete[1].target, next.canonicalize().unwrap());
    assert_eq!(complete[1].epoch, 3);
    assert_eq!(complete[1].records, records);
    assert_eq!(s.records().unwrap(), records);
    for record in records {
        assert_eq!(classify(&s, &record).unwrap(), Classification::Verified);
    }
}

#[test]
fn incomplete_maintenance_cannot_be_archived_or_overwritten_by_next_copy() {
    let (temp, mut s) = fixture();
    let target = temp.path().join("target");
    let next = temp.path().join("next");
    fs::create_dir(&target).unwrap();
    fs::create_dir(&next).unwrap();
    assert!(!migrate_copy(&mut s, &target, 1).unwrap());
    assert!(s.archive_finished_fixture(1).is_err());
    assert!(migrate_copy(&mut s, &next, usize::MAX).is_err());
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    assert_eq!(
        s.archive_finished_fixture(2).unwrap_err().to_string(),
        "MAINTENANCE_UNFINISHED"
    );
    assert!(migrate_copy(&mut s, &next, usize::MAX).is_err());
    assert!(s.history_fixture().unwrap().is_empty());
    assert!(fs::read_dir(next).unwrap().next().is_none());
    s.approve_cleanup_fixture(2).unwrap();
    cleanup_source_fault(&mut s, 2, "").unwrap();
}

#[test]
fn archive_database_failure_rolls_back_history_and_keeps_current_receipts() {
    let (temp, mut s) = fixture();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    s.approve_cleanup_fixture(2).unwrap();
    cleanup_source_fault(&mut s, 2, "").unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_cleanup_retirement BEFORE DELETE ON cleanup BEGIN SELECT RAISE(ABORT,'fixture-fault'); END;").unwrap();
    assert!(s.archive_finished_fixture(2).is_err());
    assert!(s.history_fixture().unwrap().is_empty());
    cleanup_source_fault(&mut s, 2, "").unwrap();
    db.execute_batch("DROP TRIGGER deny_cleanup_retirement")
        .unwrap();
    s.archive_finished_fixture(2).unwrap();
    assert_eq!(s.history_fixture().unwrap().len(), 1);
    assert!(s.archive_finished_fixture(2).is_err());
    assert_eq!(s.history_fixture().unwrap().len(), 1);
}

#[test]
fn explicitly_retained_source_can_be_archived_without_deletion_or_reusable_approval() {
    let (temp, mut s) = fixture();
    let (_, source, _) = s.locator().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    migrate_copy(&mut s, &target, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    assert!(s.retain_source_fixture(1).is_err());
    s.retain_source_fixture(2).unwrap();
    assert!(s.approve_cleanup_fixture(2).is_err());
    assert_eq!(
        cleanup_source_fault(&mut s, 2, "").unwrap_err().to_string(),
        "CLEANUP_REVIEW_REQUIRED"
    );
    s.archive_finished_fixture(2).unwrap();
    let history = s.history_fixture().unwrap();
    assert_eq!(history[0].state, "retained");
    assert!(history[0].items.is_empty());
    for record in s.records().unwrap() {
        assert!(source.join(record.location).exists());
    }
    let next = temp.path().join("next");
    fs::create_dir(&next).unwrap();
    migrate_copy(&mut s, &next, usize::MAX).unwrap();
    prepare_source_cleanup(&s).unwrap();
    assert!(s.approve_cleanup_fixture(2).is_err());
    assert!(cleanup_source_fault(&mut s, 2, "").is_err());
    assert!(target.join("分库/1.jpg").exists());
    assert_eq!(s.history_fixture().unwrap(), history);
}
