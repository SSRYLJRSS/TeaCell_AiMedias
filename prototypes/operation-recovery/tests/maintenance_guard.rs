use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use teacell_operation_recovery_poc::{
    digest,
    legacy_return::{execute_return, prepare_return},
    library_db::{AssetRecord, LibraryStore},
    managed_library::mark_fixture_root,
};

struct Holder(Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn hold(db: &std::path::Path) -> Holder {
    let mut holder = Holder(
        Command::new(env!("CARGO_BIN_EXE_teacell-operation-recovery-poc"))
            .arg(db)
            .arg("unused")
            .arg("unused")
            .arg("maintenance-hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = holder.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        "LOCK_HELD\n"
    );
    holder
}

#[test]
fn same_database_is_exclusive_but_other_databases_and_reads_remain_available() {
    let temp = tempfile::tempdir().unwrap();
    let a = LibraryStore::open(&temp.path().join("a.sqlite")).unwrap();
    let b = LibraryStore::open(&temp.path().join("b.sqlite")).unwrap();
    let guard = a.maintenance_guard_fixture().unwrap();
    let alias = LibraryStore::open(&temp.path().join("./a.sqlite")).unwrap();
    assert_eq!(
        alias.maintenance_guard_fixture().err().unwrap().to_string(),
        "MAINTENANCE_BUSY"
    );
    assert!(b.maintenance_guard_fixture().is_ok());
    assert!(alias.records().unwrap().is_empty());
    drop(guard);
    assert!(alias.maintenance_guard_fixture().is_ok());
}

#[test]
fn live_child_blocks_maintenance_and_normal_exit_releases_the_existing_lock_file() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("library.sqlite");
    let s = LibraryStore::open(&db).unwrap();
    let mut holder = hold(&db);
    assert_eq!(
        s.maintenance_guard_fixture().err().unwrap().to_string(),
        "MAINTENANCE_BUSY"
    );
    assert!(s.records().unwrap().is_empty());
    drop(holder.0.stdin.take());
    assert!(holder.0.wait().unwrap().success());
    assert!(s.maintenance_guard_fixture().is_ok());
    assert!(temp
        .path()
        .join("library.sqlite.maintenance.lock")
        .is_file());
}

#[test]
fn strong_child_termination_releases_os_lock_without_deleting_a_stale_marker() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("library.sqlite");
    let s = LibraryStore::open(&db).unwrap();
    let mut holder = hold(&db);
    assert!(s.maintenance_guard_fixture().is_err());
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    assert!(s.maintenance_guard_fixture().is_ok());
    assert!(temp.path().join("library.sqlite.maintenance.lock").exists());
}

#[test]
fn busy_return_does_not_create_staging_or_change_approved_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    mark_fixture_root(&root, "fixture-library").unwrap();
    let outside = temp.path().join("outside.jpg");
    fs::write(&outside, b"image").unwrap();
    let mut s = LibraryStore::open(&temp.path().join("library.sqlite")).unwrap();
    s.initialize("fixture-library", &root).unwrap();
    s.seed(&AssetRecord {
        id: 1,
        location: outside.clone(),
        digest: digest(&outside).unwrap(),
        trusted_receipt: false,
        tags: "keep".into(),
        rating: 5,
    })
    .unwrap();
    let plan = prepare_return(&s, 1, std::path::Path::new("returned.jpg")).unwrap();
    s.approve_external_return_fixture(plan, 1).unwrap();
    let held = s.maintenance_guard_fixture().unwrap();
    assert_eq!(
        execute_return(&mut s, plan, 1, "").unwrap_err().to_string(),
        "MAINTENANCE_BUSY"
    );
    assert_eq!(s.external_return_fixture(plan).unwrap().state, "approved");
    assert!(!root.join(".legacy-stage").exists());
    drop(held);
    execute_return(&mut s, plan, 1, "").unwrap();
    assert_eq!(fs::read(outside).unwrap(), b"image");
}
