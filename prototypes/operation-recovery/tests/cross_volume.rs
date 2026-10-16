//! Opt-in real volume fixture. No user material, mount/drive changes, or full disk.
#[cfg(windows)]
mod windows {
    use std::{fs, path::Path};
    use teacell_operation_recovery_poc::{
        digest,
        legacy_return::{execute_return, prepare_return},
        library_db::{AssetRecord, LibraryStore},
        managed_library::{classify, mark_fixture_root, migrate_copy, Classification},
    };
    fn volume(path: &Path) -> u32 {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let file = fs::File::open(path).unwrap();
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
            0
        );
        info.dwVolumeSerialNumber
    }
    fn target_fixture() -> tempfile::TempDir {
        let supplied = std::env::var_os("TEACELL_CROSS_VOLUME_FIXTURE")
            .expect("explicit isolated target directory required");
        let target = Path::new(&supplied).canonicalize().unwrap();
        let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../artifacts/agent-cross-volume")
            .canonicalize()
            .unwrap();
        assert_eq!(
            target, allowed,
            "fixture must be the repository's isolated artifact directory"
        );
        tempfile::Builder::new()
            .prefix("owned-cross-volume-")
            .tempdir_in(target)
            .unwrap()
    }
    #[test]
    #[ignore = "requires explicitly configured isolated second-volume fixture"]
    fn actual_cross_volume_copy_and_return_preserve_metadata_original_and_receipts() {
        let source = tempfile::tempdir().unwrap();
        let target = target_fixture();
        let source_root = source.path().join("source-root");
        let target_root = target.path().join("target-root");
        mark_fixture_root(&source_root, "cross-volume-fixture").unwrap();
        fs::create_dir(&target_root).unwrap();
        fs::write(source_root.join("managed.jpg"), b"cross-volume-image").unwrap();
        fs::write(source_root.join("untracked.txt"), b"keep-unknown").unwrap();
        let probe = target.path().join("volume-probe");
        fs::write(&probe, b"probe").unwrap();
        assert_ne!(
            volume(&source_root.join("managed.jpg")),
            volume(&probe),
            "must actually be two filesystem volumes"
        );
        let mut store = LibraryStore::open(&source.path().join("library.sqlite")).unwrap();
        store
            .initialize("cross-volume-fixture", &source_root)
            .unwrap();
        let managed = AssetRecord {
            id: 1,
            location: "managed.jpg".into(),
            digest: digest(&source_root.join("managed.jpg")).unwrap(),
            trusted_receipt: true,
            tags: "keep-managed-tags".into(),
            rating: 4,
        };
        store.seed(&managed).unwrap();
        migrate_copy(&mut store, &target_root, usize::MAX).unwrap();
        assert_eq!(store.records().unwrap(), vec![managed.clone()]);
        assert_eq!(
            classify(&store, &managed).unwrap(),
            Classification::Verified
        );
        assert_eq!(
            fs::read(source_root.join("managed.jpg")).unwrap(),
            b"cross-volume-image"
        );
        assert!(!target_root.join("untracked.txt").exists());
        // Explicitly retain the source before retiring this maintenance context.
        teacell_operation_recovery_poc::managed_library::prepare_source_cleanup(&store).unwrap();
        store.retain_source_fixture(2).unwrap();
        store.archive_finished_fixture(2).unwrap();
        let outside = source.path().join("outside.jpg");
        fs::write(&outside, b"cross-volume-image").unwrap();
        let legacy = AssetRecord {
            id: 2,
            location: outside.clone(),
            trusted_receipt: false,
            tags: "legacy-tags".into(),
            rating: 5,
            ..managed.clone()
        };
        store.seed(&legacy).unwrap();
        let plan = prepare_return(&store, 2, Path::new("returned.jpg")).unwrap();
        store.approve_external_return_fixture(plan, 2).unwrap();
        execute_return(&mut store, plan, 2, "").unwrap();
        drop(store);
        let mut store = LibraryStore::open(&source.path().join("library.sqlite")).unwrap();
        execute_return(&mut store, plan, 2, "").unwrap();
        let records = store.records().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].id, 2);
        assert_eq!(records[1].tags, legacy.tags);
        assert_eq!(records[1].rating, legacy.rating);
        assert_eq!(
            classify(&store, &records[1]).unwrap(),
            Classification::Verified
        );
        assert_eq!(fs::read(outside).unwrap(), b"cross-volume-image");
        assert_eq!(
            fs::read(source_root.join("untracked.txt")).unwrap(),
            b"keep-unknown"
        );
    }

    #[test]
    #[ignore = "requires explicitly configured isolated second-volume fixture"]
    fn actual_cross_volume_exit_faults_resume_copy_and_legacy_return() {
        for fault in [
            "migration-partial",
            "migration-published",
            "return-partial",
            "return-published",
            "return-registered",
        ] {
            let source = tempfile::tempdir().unwrap();
            let target = target_fixture();
            let root = source.path().join("source-root");
            mark_fixture_root(&root, "cross-volume-fault").unwrap();
            let target_root = target.path().join("target-root");
            fs::create_dir(&target_root).unwrap();
            let probe = target.path().join("probe");
            fs::write(&probe, b"probe").unwrap();
            fs::write(root.join("managed.jpg"), b"cross-volume-fault-image").unwrap();
            assert_ne!(volume(&root.join("managed.jpg")), volume(&probe));
            let db = source.path().join("library.sqlite");
            let mut store = LibraryStore::open(&db).unwrap();
            store.initialize("cross-volume-fault", &root).unwrap();
            let record = AssetRecord {
                id: 1,
                location: "managed.jpg".into(),
                digest: digest(&root.join("managed.jpg")).unwrap(),
                trusted_receipt: true,
                tags: "keep-tags".into(),
                rating: 5,
            };
            store.seed(&record).unwrap();
            let legacy_fault = fault.starts_with("return-");
            let mut plan = 0;
            if legacy_fault {
                migrate_copy(&mut store, &target_root, usize::MAX).unwrap();
                teacell_operation_recovery_poc::managed_library::prepare_source_cleanup(&store)
                    .unwrap();
                store.retain_source_fixture(2).unwrap();
                store.archive_finished_fixture(2).unwrap();
                let external = AssetRecord {
                    id: 2,
                    location: root.join("managed.jpg"),
                    trusted_receipt: false,
                    ..record.clone()
                };
                store.seed(&external).unwrap();
                plan = prepare_return(&store, 2, Path::new("returned.jpg")).unwrap();
                store.approve_external_return_fixture(plan, 2).unwrap();
            }
            drop(store);
            let status =
                std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
                    .arg(&db)
                    .arg(if legacy_fault {
                        plan.to_string()
                    } else {
                        target_root.to_str().unwrap().to_owned()
                    })
                    .arg(if legacy_fault { "2" } else { "unused" })
                    .arg(fault)
                    .status()
                    .unwrap();
            assert_eq!(status.code(), Some(77), "{fault}");
            let mut store = LibraryStore::open(&db).unwrap();
            if legacy_fault {
                execute_return(&mut store, plan, 2, "").unwrap();
                let records = store.records().unwrap();
                assert_eq!(records.len(), 2);
                assert_eq!(records[1].id, 2);
                assert_eq!(records[1].tags, "keep-tags");
                assert_eq!(
                    classify(&store, &records[1]).unwrap(),
                    Classification::Verified
                );
            } else {
                migrate_copy(&mut store, &target_root, usize::MAX).unwrap();
                assert_eq!(store.records().unwrap(), vec![record.clone()]);
                assert_eq!(classify(&store, &record).unwrap(), Classification::Verified);
            }
            assert_eq!(
                fs::read(root.join("managed.jpg")).unwrap(),
                b"cross-volume-fault-image"
            );
            assert_eq!(store.locator().unwrap().2, 2);
        }
    }
}
