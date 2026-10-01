//! NSIS installation identity. Read before acquiring any database lock.

#[cfg(any(windows, test))]
fn read_installation_id(directory: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(directory.join("installation-id.txt")).ok()?;
    let mut bytes = Vec::new();
    file.take(129).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 128 {
        return None;
    }
    let raw = std::str::from_utf8(&bytes).ok()?.trim();
    let id = uuid::Uuid::parse_str(raw).ok()?;
    (!id.is_nil()).then(|| id.to_string())
}

/// Other package formats keep the existing per-user onboarding behavior.
pub fn installed_tutorial_id() -> Option<String> {
    #[cfg(windows)]
    {
        let executable = std::env::current_exe().ok()?;
        read_installation_id(executable.parent()?)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_nsis_guid_and_rejects_missing_invalid_or_nil_marker() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("installation-id.txt");
        assert_eq!(read_installation_id(dir.path()), None);
        std::fs::write(&marker, "{12345678-1234-4234-8234-123456789abc}\r\n").unwrap();
        assert_eq!(
            read_installation_id(dir.path()).as_deref(),
            Some("12345678-1234-4234-8234-123456789abc")
        );
        for invalid in ["", "not-a-guid", "00000000-0000-0000-0000-000000000000"] {
            std::fs::write(&marker, invalid).unwrap();
            assert_eq!(read_installation_id(dir.path()), None);
        }
        std::fs::write(&marker, vec![b' '; 129]).unwrap();
        assert_eq!(read_installation_id(dir.path()), None);
        std::fs::write(&marker, [0xff]).unwrap();
        assert_eq!(read_installation_id(dir.path()), None);
    }
}
