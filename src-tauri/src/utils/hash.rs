//! 文件哈希：sha256 取前 16 字节 hex（入库去重/重复提示用，T03 起用）。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::AppResult;

/// 流式计算文件 sha256，返回前 16 字节 hex（32 字符）
pub fn sha256_16(path: &Path) -> AppResult<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}
