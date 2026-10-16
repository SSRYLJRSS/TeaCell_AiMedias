use std::{fs, path::Path};
use teacell_operation_recovery_poc::{
    digest,
    legacy_return::{execute_return, prepare_return},
    library_db::{AssetRecord, LibraryStore},
    managed_library::{classify, mark_fixture_root, Classification},
};

fn fixture() -> (tempfile::TempDir, LibraryStore, AssetRecord) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("总仓");
    mark_fixture_root(&root, "fixture-library").unwrap();
    fs::create_dir(root.join("分库")).unwrap();
    fs::write(root.join("分库/existing.jpg"), b"same-image").unwrap();
    let outside = temp.path().join("库外 原片.jpg");
    fs::write(&outside, b"same-image").unwrap();
    let store = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    store.initialize("fixture-library", &root).unwrap();
    let external = AssetRecord {
        id: 1,
        location: outside,
        digest: digest(&root.join("分库/existing.jpg")).unwrap(),
        trusted_receipt: false,
        tags: "legacy-tags".into(),
        rating: 5,
    };
    store.seed(&external).unwrap();
    store
        .seed(&AssetRecord {
            id: 2,
            location: "分库/existing.jpg".into(),
            trusted_receipt: true,
            tags: "independent-tags".into(),
            rating: 2,
            ..external.clone()
        })
        .unwrap();
    (temp, store, external)
}

#[test]
fn same_hash_return_preserves_old_id_and_metadata_without_merging_or_deleting_original() {
    let (_temp, mut s, original) = fixture();
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    assert_eq!(
        execute_return(&mut s, plan, 1, "").unwrap_err().to_string(),
        "LEGACY_REVIEW_REQUIRED"
    );
    s.approve_external_return_fixture(plan, 1).unwrap();
    execute_return(&mut s, plan, 1, "").unwrap();
    execute_return(&mut s, plan, 1, "").unwrap();
    let records = s.records().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, original.id);
    assert_eq!(records[0].tags, original.tags);
    assert_eq!(records[0].rating, original.rating);
    assert_eq!(records[0].location, Path::new("分库/returned.jpg"));
    assert_eq!(classify(&s, &records[0]).unwrap(), Classification::Verified);
    assert_eq!(records[1].tags, "independent-tags");
    assert_eq!(fs::read(&original.location).unwrap(), b"same-image");
    assert_eq!(s.external_return_fixture(plan).unwrap().record, original);
}

#[test]
fn conflicting_destination_is_preserved_even_when_hash_matches() {
    let (_temp, mut s, _) = fixture();
    assert!(prepare_return(&s, 1, Path::new("分库/existing.jpg")).is_err());
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let (_, root, _) = s.locator().unwrap();
    fs::write(root.join("分库/returned.jpg"), b"same-image").unwrap();
    assert_eq!(
        execute_return(&mut s, plan, 1, "").unwrap_err().to_string(),
        "DESTINATION_CONFLICT"
    );
    assert!(!s.records().unwrap()[0].trusted_receipt);
    assert_eq!(
        fs::read(root.join("分库/returned.jpg")).unwrap(),
        b"same-image"
    );
}

#[test]
fn same_hash_source_replacement_cannot_impersonate_the_reviewed_file() {
    let (_temp, mut s, original) = fixture();
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let held = fs::File::open(&original.location).unwrap();
    fs::rename(&original.location, original.location.with_extension("kept")).unwrap();
    fs::write(&original.location, b"same-image").unwrap();
    assert_eq!(
        execute_return(&mut s, plan, 1, "").unwrap_err().to_string(),
        "SOURCE_CHANGED"
    );
    assert!(!s.records().unwrap()[0].trusted_receipt);
    drop(held);
}

#[test]
fn return_faults_recover_same_asset_without_republishing_foreign_files() {
    for fault in ["return-partial", "return-published", "return-registered"] {
        let (temp, s, original) = fixture();
        let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
        s.approve_external_return_fixture(plan, 1).unwrap();
        drop(s);
        let status =
            std::process::Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
                .arg(temp.path().join("library.sqlite"))
                .arg(plan.to_string())
                .arg("1")
                .arg(fault)
                .status()
                .unwrap();
        assert_eq!(status.code(), Some(77));
        let mut s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
        assert_eq!(
            s.records().unwrap()[0].trusted_receipt,
            fault == "return-registered"
        );
        execute_return(&mut s, plan, 1, "").unwrap();
        assert_eq!(s.records().unwrap().len(), 2);
        assert_eq!(s.records().unwrap()[0].id, 1);
        assert_eq!(s.records().unwrap()[0].tags, original.tags);
        assert_eq!(fs::read(original.location).unwrap(), b"same-image");
    }
}

#[test]
fn failed_asset_update_keeps_published_receipt_recoverable_and_old_metadata_active() {
    let (temp, mut s, original) = fixture();
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let conn = rusqlite::Connection::open(temp.path().join("library.sqlite")).unwrap();
    conn.execute_batch("CREATE TRIGGER deny_return BEFORE UPDATE ON records BEGIN SELECT RAISE(ABORT,'fixture-fault'); END;").unwrap();
    assert!(execute_return(&mut s, plan, 1, "").is_err());
    assert_eq!(s.records().unwrap()[0], original);
    assert_eq!(s.external_return_fixture(plan).unwrap().state, "copying");
    conn.execute_batch("DROP TRIGGER deny_return").unwrap();
    execute_return(&mut s, plan, 1, "").unwrap();
    assert!(s.records().unwrap()[0].trusted_receipt);
}

#[test]
fn unsafe_paths_managed_records_and_offline_roots_cannot_prepare_or_execute() {
    let (temp, mut s, original) = fixture();
    assert!(prepare_return(&s, 1, Path::new("../escape.jpg")).is_err());
    assert!(prepare_return(&s, 1, Path::new(".legacy-stage/file.jpg")).is_err());
    assert!(prepare_return(&s, 2, Path::new("分库/returned.jpg")).is_err());
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let (_, root, _) = s.locator().unwrap();
    fs::rename(&root, temp.path().join("offline")).unwrap();
    assert!(execute_return(&mut s, plan, 1, "").is_err());
    assert_eq!(s.records().unwrap()[0], original);
    assert!(!temp.path().join("escape.jpg").exists());
}

#[test]
fn foreign_same_hash_staging_file_is_not_adopted_without_identity_witness() {
    let (_temp, mut s, original) = fixture();
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let (_, root, _) = s.locator().unwrap();
    fs::create_dir(root.join(".legacy-stage")).unwrap();
    let stage = root.join(format!(".legacy-stage/{plan}.stage"));
    fs::write(&stage, b"same-image").unwrap();
    assert!(execute_return(&mut s, plan, 1, "").is_err());
    assert_eq!(fs::read(stage).unwrap(), b"same-image");
    assert!(!root.join("分库/returned.jpg").exists());
    assert_eq!(s.records().unwrap()[0], original);
}

#[test]
fn changed_source_bytes_and_stale_epoch_cannot_update_the_legacy_record() {
    let (_temp, mut s, original) = fixture();
    let plan = prepare_return(&s, 1, Path::new("分库/returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    assert!(execute_return(&mut s, plan, 2, "").is_err());
    fs::write(&original.location, b"new-content").unwrap();
    assert_eq!(
        execute_return(&mut s, plan, 1, "").unwrap_err().to_string(),
        "SOURCE_CHANGED"
    );
    assert_eq!(s.records().unwrap()[0], original);
    assert_eq!(fs::read(&original.location).unwrap(), b"new-content");
}
