use std::path::Path;
use teacell_operation_recovery_poc::{db::Store, execute, request_external_once, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        return Err("usage: poc <fixture-db> <fixture-root> <operation-id> <fault-point>".into());
    }
    if args[4] == "maintenance-hold" {
        use std::io::Write;
        let library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        let _guard = library.maintenance_guard_fixture()?;
        writeln!(std::io::stdout(), "LOCK_HELD")?;
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        return Ok(());
    }
    if args[4].starts_with("return-") {
        let mut library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        return teacell_operation_recovery_poc::legacy_return::execute_return(
            &mut library,
            args[2].parse()?,
            args[3].parse()?,
            &args[4],
        );
    }
    if args[4].starts_with("unlink-") {
        let mut library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        return teacell_operation_recovery_poc::managed_library::submit_external_unlink(
            &mut library,
            args[2].parse()?,
            args[3].parse()?,
            &args[4],
        );
    }
    if args[4].starts_with("cleanup-") {
        let mut library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        return teacell_operation_recovery_poc::managed_library::cleanup_source_fault(
            &mut library,
            args[3].parse()?,
            &args[4],
        );
    }
    if args[4] == "migration-copy-first" {
        let mut library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        teacell_operation_recovery_poc::managed_library::migrate_copy(
            &mut library,
            Path::new(&args[2]),
            1,
        )?;
        std::process::exit(77);
    }
    if args[4].starts_with("migration-") {
        let mut library =
            teacell_operation_recovery_poc::library_db::LibraryStore::open(Path::new(&args[1]))?;
        teacell_operation_recovery_poc::managed_library::migrate_copy_fault(
            &mut library,
            Path::new(&args[2]),
            usize::MAX,
            &args[4],
        )?;
        return Ok(());
    }
    let mut store = Store::open(Path::new(&args[1]))?;
    if args[4] == "accepted" {
        store.submit(args[3].parse()?, "fixed-intent")?;
        std::process::exit(77);
    }
    if args[4] == "external-replied" {
        request_external_once(&store, args[3].parse()?, &args[4], || {
            std::fs::create_dir_all(&args[2])?;
            std::fs::write(Path::new(&args[2]).join("mock-request-count"), b"1")?;
            Ok(())
        })?;
        return Ok(());
    }
    execute(
        &mut store,
        args[3].parse()?,
        Path::new(&args[2]),
        &args[4],
        usize::MAX,
    )
}
