use std::fs;
use teacell_operation_recovery_poc::{
    digest,
    library_db::{AssetRecord, LibraryStore},
    managed_library::{
        mark_fixture_root, prepare_external_unlink, relocate_already_moved, submit_external_unlink,
    },
};

fn fixture() -> (tempfile::TempDir, LibraryStore, AssetRecord) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("总仓");
    mark_fixture_root(&root, "fixture-library").unwrap();
    fs::write(root.join("inside.jpg"), b"same-image").unwrap();
    let outside = temp.path().join("库外 原片.jpg");
    fs::write(&outside, b"same-image").unwrap();
    let store = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    store.initialize("fixture-library", &root).unwrap();
    let external = AssetRecord {
        id: 1,
        location: outside,
        digest: digest(&root.join("inside.jpg")).unwrap(),
        trusted_receipt: false,
        tags: "legacy-tags".into(),
        rating: 5,
    };
    store.seed(&external).unwrap();
    store
        .seed(&AssetRecord {
            id: 2,
            location: "inside.jpg".into(),
            trusted_receipt: true,
            tags: "different-managed-tags".into(),
            rating: 2,
            ..external.clone()
        })
        .unwrap();
    (temp, store, external)
}

#[test]
fn unlink_only_removes_reviewed_relationship_preserving_original_and_metadata_receipt() {
    let (_temp, mut s, external) = fixture();
    let plan = prepare_external_unlink(&s, 1).unwrap();
    assert_eq!(
        submit_external_unlink(&mut s, plan, 1, "")
            .unwrap_err()
            .to_string(),
        "LEGACY_REVIEW_REQUIRED"
    );
    s.approve_external_unlink_fixture(plan, 1).unwrap();
    submit_external_unlink(&mut s, plan, 1, "").unwrap();
    submit_external_unlink(&mut s, plan, 1, "").unwrap();
    assert_eq!(s.records().unwrap().len(), 1);
    assert_eq!(s.records().unwrap()[0].id, 2);
    assert_eq!(s.records().unwrap()[0].tags, "different-managed-tags");
    assert_eq!(s.external_unlink_fixture(plan).unwrap().record, external);
    assert_eq!(s.external_unlink_fixture(plan).unwrap().state, "complete");
    assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
    assert!(prepare_external_unlink(&s, 2).is_err());
}

#[test]
fn changed_record_after_review_rolls_back_and_does_not_consume_the_plan() {
    let (temp, mut s, external) = fixture();
    let plan = prepare_external_unlink(&s, 1).unwrap();
    s.approve_external_unlink_fixture(plan, 1).unwrap();
    let conn = rusqlite::Connection::open(temp.path().join("library.sqlite")).unwrap();
    let changed = AssetRecord {
        tags: "edited-after-review".into(),
        ..external.clone()
    };
    conn.execute(
        "UPDATE records SET payload=? WHERE id=1",
        [serde_json::to_string(&changed).unwrap()],
    )
    .unwrap();
    assert!(submit_external_unlink(&mut s, plan, 1, "").is_err());
    assert_eq!(s.records().unwrap()[0], changed);
    assert_eq!(s.external_unlink_fixture(plan).unwrap().state, "approved");
    assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
}

#[test]
fn deletion_failure_rolls_back_completion_and_preserves_record_for_retry() {
    let (temp, mut s, external) = fixture();
    let plan = prepare_external_unlink(&s, 1).unwrap();
    s.approve_external_unlink_fixture(plan, 1).unwrap();
    let conn = rusqlite::Connection::open(temp.path().join("library.sqlite")).unwrap();
    conn.execute_batch("CREATE TRIGGER deny_unlink BEFORE DELETE ON records BEGIN SELECT RAISE(ABORT,'fixture-fault'); END;").unwrap();
    assert!(submit_external_unlink(&mut s, plan, 1, "").is_err());
    assert_eq!(s.records().unwrap()[0], external);
    assert_eq!(s.external_unlink_fixture(plan).unwrap().state, "approved");
    conn.execute_batch("DROP TRIGGER deny_unlink").unwrap();
    submit_external_unlink(&mut s, plan, 1, "").unwrap();
    assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
}

#[test]
fn process_exit_before_and_after_commit_resumes_without_touching_original_file() {
    for fault in ["unlink-before-commit", "unlink-after-commit"] {
        let (temp, s, external) = fixture();
        let plan = prepare_external_unlink(&s, 1).unwrap();
        s.approve_external_unlink_fixture(plan, 1).unwrap();
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
            s.records().unwrap().len(),
            if fault == "unlink-before-commit" {
                2
            } else {
                1
            }
        );
        submit_external_unlink(&mut s, plan, 1, "").unwrap();
        assert_eq!(s.records().unwrap().len(), 1);
        assert_eq!(s.external_unlink_fixture(plan).unwrap().record, external);
        assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
    }
}

#[test]
fn offline_root_and_changed_epoch_cannot_use_old_unlink_plan() {
    let (temp, mut s, external) = fixture();
    let plan = prepare_external_unlink(&s, 1).unwrap();
    s.approve_external_unlink_fixture(plan, 1).unwrap();
    let (_, root, _) = s.locator().unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    assert!(submit_external_unlink(&mut s, plan, 1, "").is_err());
    assert!(prepare_external_unlink(&s, 1).is_err());
    relocate_already_moved(&mut s, &moved).unwrap();
    assert_eq!(
        submit_external_unlink(&mut s, plan, 1, "")
            .unwrap_err()
            .to_string(),
        "LIBRARY_EPOCH_CHANGED"
    );
    assert!(s.approve_external_unlink_fixture(plan, 2).is_err());
    assert_eq!(s.records().unwrap()[0], external);
    assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
}

#[test]
fn simultaneous_same_plan_submissions_return_the_same_completed_receipt() {
    let (temp, mut s, external) = fixture();
    let plan = prepare_external_unlink(&s, 1).unwrap();
    s.approve_external_unlink_fixture(plan, 1).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = temp.path().join("library.sqlite");
        let barrier = barrier.clone();
        let mut store = LibraryStore::open(&path).unwrap();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            submit_external_unlink(&mut store, plan, 1, "").map_err(|error| error.to_string())
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(results.iter().any(|result| result.is_ok()));
    assert!(results
        .iter()
        .all(|result| result.is_ok() || result.as_ref().unwrap_err() == "MAINTENANCE_BUSY"));
    // Busy is a resource conflict, not success. An explicit retry queries the
    // same completed receipt once the other maintenance guard has been released.
    submit_external_unlink(&mut s, plan, 1, "").unwrap();
    assert_eq!(s.records().unwrap().len(), 1);
    assert_eq!(s.external_unlink_fixture(plan).unwrap().state, "complete");
    assert_eq!(fs::read(&external.location).unwrap(), b"same-image");
}
