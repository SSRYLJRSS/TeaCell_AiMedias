//! 文件哈希：sha256 取前 16 字节 hex（入库去重/重复提示用，T03 起用）

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::AppResult;

/// 流式计算完整文件 sha256（64 字符小写 hex）。
pub fn sha256_full(path: &Path) -> AppResult<String> {
    sha256_full_with_checkpoint(path, || Ok(()))
}

pub fn sha256_full_with_checkpoint(
    path: &Path,
    mut checkpoint: impl FnMut() -> AppResult<()>,
) -> AppResult<String> {
    checkpoint()?;
    let mut reader = BufReader::new(File::open(path)?);
    sha256_reader_with_checkpoint(&mut reader, checkpoint)
}

/// Hash an already opened file handle so a recovery check can bind the digest
/// to the same filesystem object whose identity it records.
pub fn sha256_full_file(file: &mut File) -> AppResult<String> {
    sha256_full_file_with_checkpoint(file, || Ok(()))
}

pub fn sha256_full_file_with_checkpoint(
    file: &mut File,
    mut checkpoint: impl FnMut() -> AppResult<()>,
) -> AppResult<String> {
    checkpoint()?;
    file.seek(SeekFrom::Start(0))?;
    sha256_reader_with_checkpoint(file, checkpoint)
}

fn sha256_reader_with_checkpoint(
    reader: &mut impl Read,
    mut checkpoint: impl FnMut() -> AppResult<()>,
) -> AppResult<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        checkpoint()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        #[cfg(test)]
        AFTER_HASH_CHUNK.with(|hook| {
            if let Some(callback) = hook.borrow_mut().as_mut() {
                callback();
            }
        });
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
thread_local! {
    static AFTER_HASH_CHUNK: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

/// Deterministic fault seam for revocation/cancellation during a real file read.
#[cfg(test)]
pub(crate) fn with_hash_chunk_hook<T>(hook: impl FnMut() + 'static, body: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            AFTER_HASH_CHUNK.with(|hook| *hook.borrow_mut() = None);
        }
    }
    AFTER_HASH_CHUNK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _reset = Reset;
    body()
}

/// 流式计算文件 sha256，返回前 16 字节 hex（32 字符）
pub fn sha256_16(path: &Path) -> AppResult<String> {
    Ok(sha256_full(path)?[..32].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::agent::AgentErrorCode;
    use crate::error::AppError;
    use std::cell::Cell;
    use std::io::Cursor;

    #[test]
    fn checkpoint_stops_before_reading_the_next_hash_chunk() {
        for code in [
            AgentErrorCode::Cancelled,
            AgentErrorCode::GrantRevoked,
            AgentErrorCode::LibraryChanged,
        ] {
            let mut reader = Cursor::new(vec![42u8; 3 * 1024 * 1024]);
            let calls = Cell::new(0);
            let error = sha256_reader_with_checkpoint(&mut reader, || {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    return Err(AppError::from(code));
                }
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.code(), AppError::from(code).code());
            assert_eq!(reader.position(), 1024 * 1024);
        }
    }
}
