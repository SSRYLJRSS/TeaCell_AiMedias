use std::{fs, path::PathBuf, process::Command};
use teacell_operation_recovery_poc::{db::Store, execute, prepare, Caller};

struct Fixture {
    dir: tempfile::TempDir,
    database: PathBuf,
    root: PathBuf,
    sources: Vec<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("来源 照片");
        fs::create_dir(&source).unwrap();
        let sources = (0..2)
            .map(|n| {
                let p = source.join(format!("忽略批准-{n}.jpg"));
                fs::write(&p, vec![n as u8; 20001]).unwrap();
                p
            })
            .collect();
        let database = dir.path().join("fixture.sqlite");
        let root = dir.path().join("warehouse");
        Self {
            dir,
            database,
            root,
            sources,
        }
    }
    fn accepted(&self) -> (Store, i64, i64) {
        let mut s = Store::open(&self.database).unwrap();
        let plan = prepare(&s, &self.sources, self.sources[0].parent().unwrap()).unwrap();
        s.approve(plan, Caller::TrustedGui).unwrap();
        let op = s.submit(plan, "fixed-intent").unwrap();
        (s, plan, op)
    }
}

#[test]
fn process_crashes_reconcile_files_and_batches_without_duplicate_effects() {
    for fault in ["partial-copy", "published", "registered", "batch-created"] {
        let f = Fixture::new();
        let (s, plan, op) = f.accepted();
        drop(s);
        let status = Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
            .arg(&f.database)
            .arg(&f.root)
            .arg(op.to_string())
            .arg(fault)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77), "{fault}");
        let mut reopened = Store::open(&f.database).unwrap();
        assert_eq!(reopened.submit(plan, "fixed-intent").unwrap(), op);
        execute(&mut reopened, op, &f.root, "", usize::MAX).unwrap();
        execute(&mut reopened, op, &f.root, "", usize::MAX).unwrap();
        assert_eq!(reopened.counts().unwrap(), (1, 2, 1));
        for (step, source) in f.sources.iter().enumerate() {
            assert_eq!(
                fs::read(source).unwrap(),
                fs::read(f.root.join(format!("op-{op}/{step}.asset"))).unwrap()
            );
        }
    }
}

#[test]
fn forged_approval_and_unauthorized_directory_have_no_file_effect() {
    let f = Fixture::new();
    let mut s = Store::open(&f.database).unwrap();
    assert!(prepare(&s, &f.sources, f.dir.path().join("other").as_path()).is_err());
    let plan = prepare(&s, &f.sources, f.sources[0].parent().unwrap()).unwrap();
    assert!(s.approve(plan, Caller::Agent).is_err());
    assert!(s.submit(plan, "approved:true").is_err());
    assert_eq!(s.counts().unwrap(), (0, 0, 0));
    assert!(!f.root.exists());
}

#[test]
fn repeated_submission_is_atomic_and_conflicting_plan_is_rejected() {
    let f = Fixture::new();
    let (mut s, plan, op) = f.accepted();
    assert_eq!(s.submit(plan, "fixed-intent").unwrap(), op);
    assert!(s.submit(plan, "another-intent").is_err());
    let another = prepare(&s, &f.sources, f.sources[0].parent().unwrap()).unwrap();
    s.approve(another, Caller::TrustedGui).unwrap();
    assert!(s.submit(another, "fixed-intent").is_err());
    assert_eq!(s.counts().unwrap(), (1, 0, 0));
}

#[test]
fn cancellation_preserves_completed_file_and_review_resumes_remaining_only() {
    let f = Fixture::new();
    let (mut s, _, op) = f.accepted();
    execute(&mut s, op, &f.root, "", 1).unwrap();
    let target = f.root.join(format!("op-{op}/0.asset"));
    let original_modified = fs::metadata(&target).unwrap().modified().unwrap();
    s.cancel(op).unwrap();
    assert!(execute(&mut s, op, &f.root, "", usize::MAX).is_err());
    assert_eq!(s.counts().unwrap(), (1, 1, 1));
    drop(s);
    let mut s = Store::open(&f.database).unwrap();
    assert!(execute(&mut s, op, &f.root, "", usize::MAX).is_err());
    s.continue_after_review(op).unwrap();
    execute(&mut s, op, &f.root, "", usize::MAX).unwrap();
    assert_eq!(s.counts().unwrap(), (1, 2, 1));
    assert_eq!(
        fs::metadata(&target).unwrap().modified().unwrap(),
        original_modified
    );
}

#[test]
fn changed_inputs_and_foreign_destinations_fail_without_overwriting() {
    let f = Fixture::new();
    let (mut s, _, op) = f.accepted();
    fs::write(&f.sources[0], b"changed").unwrap();
    assert!(execute(&mut s, op, &f.root, "", usize::MAX).is_err());
    assert_eq!(s.counts().unwrap(), (1, 0, 0));
    let f = Fixture::new();
    let (mut s, _, op) = f.accepted();
    let directory = f.root.join(format!("op-{op}"));
    fs::create_dir_all(&directory).unwrap();
    let target = directory.join("0.asset");
    fs::write(&target, b"foreign").unwrap();
    assert!(execute(&mut s, op, &f.root, "", usize::MAX).is_err());
    assert_eq!(fs::read(target).unwrap(), b"foreign");
}

#[test]
fn failed_acceptance_rolls_back_operation_and_keeps_approval_consumable() {
    let f = Fixture::new();
    let mut s = Store::open(&f.database).unwrap();
    let plan = prepare(&s, &f.sources, f.sources[0].parent().unwrap()).unwrap();
    s.approve(plan, Caller::TrustedGui).unwrap();
    let injected = rusqlite::Connection::open(&f.database).unwrap();
    injected.execute_batch("CREATE TRIGGER reject_item BEFORE INSERT ON items BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    assert!(s.submit(plan, "fixed-intent").is_err());
    assert_eq!(s.counts().unwrap(), (0, 0, 0));
    injected.execute_batch("DROP TRIGGER reject_item").unwrap();
    let op = s.submit(plan, "fixed-intent").unwrap();
    execute(&mut s, op, &f.root, "", usize::MAX).unwrap();
    assert_eq!(s.counts().unwrap(), (1, 2, 1));
}

#[test]
fn simultaneous_retries_accept_exactly_one_operation() {
    let f = Fixture::new();
    let s = Store::open(&f.database).unwrap();
    let plan = prepare(&s, &f.sources, f.sources[0].parent().unwrap()).unwrap();
    s.approve(plan, Caller::TrustedGui).unwrap();
    drop(s);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let db = f.database.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&db).unwrap();
                barrier.wait();
                s.submit(plan, "fixed-intent").unwrap()
            })
        })
        .collect();
    barrier.wait();
    let ids: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(
        Store::open(&f.database).unwrap().counts().unwrap(),
        (1, 0, 0)
    );
}

#[test]
fn ambiguous_external_reply_never_triggers_automatic_resubmission() {
    let f = Fixture::new();
    let (s, _, op) = f.accepted();
    drop(s);
    let status = Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
        .arg(&f.database)
        .arg(&f.root)
        .arg(op.to_string())
        .arg("external-replied")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let s = Store::open(&f.database).unwrap();
    assert_eq!(
        s.external_state(op).unwrap().as_deref(),
        Some("outcome_unknown")
    );
    assert!(
        !teacell_operation_recovery_poc::request_external_once(&s, op, "", || panic!(
            "unknown request must not be repeated"
        ))
        .unwrap()
    );
    assert_eq!(fs::read(f.root.join("mock-request-count")).unwrap(), b"1");
    assert_eq!(
        s.external_state(op).unwrap().as_deref(),
        Some("outcome_unknown")
    );
}

#[test]
fn accepted_but_response_lost_returns_original_operation() {
    let f = Fixture::new();
    let s = Store::open(&f.database).unwrap();
    let plan = prepare(&s, &f.sources, f.sources[0].parent().unwrap()).unwrap();
    s.approve(plan, Caller::TrustedGui).unwrap();
    drop(s);
    let status = Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
        .arg(&f.database)
        .arg(&f.root)
        .arg(plan.to_string())
        .arg("accepted")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let mut s = Store::open(&f.database).unwrap();
    assert_eq!(s.counts().unwrap(), (1, 0, 0));
    let op = s.submit(plan, "fixed-intent").unwrap();
    assert_eq!(s.submit(plan, "fixed-intent").unwrap(), op);
    execute(&mut s, op, &f.root, "", usize::MAX).unwrap();
    assert_eq!(s.counts().unwrap(), (1, 2, 1));
}

#[test]
fn same_hash_foreign_file_cannot_impersonate_published_receipt() {
    let f = Fixture::new();
    let (s, _, op) = f.accepted();
    drop(s);
    let status = Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
        .arg(&f.database)
        .arg(&f.root)
        .arg(op.to_string())
        .arg("published")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let target = f.root.join(format!("op-{op}/0.asset"));
    // Replace only this fixture's file with equal bytes but a distinct filesystem identity.
    fs::remove_file(&target).unwrap();
    fs::copy(&f.sources[0], &target).unwrap();
    let mut s = Store::open(&f.database).unwrap();
    let error = execute(&mut s, op, &f.root, "", usize::MAX).unwrap_err();
    assert_eq!(error.to_string(), "UNOWNED_DESTINATION");
    assert_eq!(s.counts().unwrap(), (1, 0, 0));
    assert_eq!(fs::read(&target).unwrap(), fs::read(&f.sources[0]).unwrap());
}
