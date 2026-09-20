//! 路径规范化与安全校验
//!
//! 三类数据必须分开（三端复核 B2）：
//! - **可打开的原生路径**：忠实保留系统实际路径；POSIX 反斜杠是普通字符，不做改写。
//! - **比较/搜索键**：仅作索引或同源判定，允许明确的大小写策略，不拿它打开文件。
//! - **迁移包相对路径**：自定义可移植格式，不用当前主机语义判断另一 OS 的路径。
//!
//! `normalize_path` 仅在 Windows 分支保留历史 DB 兼容（正斜杠 + 小写盘符）；
//! Unix 分支不替换 `\`、不改大小写、不删有语义的字节。

use crate::error::{AppError, AppResult};
use std::path::Path;

/// Windows 规范化：统一小写盘符 + 反斜杠转正斜杠（历史 DB 兼容，架构共享知识 #4）。
///
/// 仅限 Windows：Windows 上 `\` 与 `/` 都是目录分隔符，转换不丢信息；小写盘符沿用旧库规范。
#[cfg(windows)]
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    // 小写盘符：C:/... → c:/...
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_uppercase() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        s.replace_range(0..1, &drive.to_string());
    }
    // 去掉末尾斜杠（根 "x:/" 保留）
    while s.len() > 3 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// Unix 规范化：反斜杠是合法文件名字符，**不替换**；不改大小写；不删有语义的字节。
///
/// 仅去除末尾冗余分隔符（根 "/" 保留），这对普通文件路径不改变身份。
#[cfg(not(windows))]
pub fn normalize_path(p: &str) -> String {
    let mut s = p.to_string();
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// 使用**当前系统** Path 语义校验绝对路径。
///
/// 拒绝：空、含 NUL、相对路径、Windows 盘符相对 `C:foo`、根相对 `\foo`、设备命名空间 `\\.\`。
/// 保留：Windows 扩展前缀 `\\?\...` 与 UNC `\\server\share`（标准库产生的合法绝对路径，不一律裁掉）。
///
/// 绝对性判断本身不构成授权边界，也不等同防目录穿越；调用方仍需按来源做授权校验。
pub fn ensure_native_absolute(path: &Path) -> AppResult<()> {
    if path.as_os_str().is_empty() {
        return Err(AppError::invalid_arg("路径为空"));
    }
    if path_contains_nul(path) {
        return Err(AppError::invalid_arg("路径包含 NUL 字符"));
    }
    if !path.is_absolute() {
        return Err(AppError::invalid_arg("需要系统绝对路径"));
    }
    #[cfg(windows)]
    {
        // 设备命名空间 \\.\ 拒绝；\\?\ 扩展与 UNC 已由 is_absolute 认定为合法绝对路径。
        let text = path.as_os_str().to_string_lossy();
        if text.starts_with("\\\\.\\") || text.starts_with("//./") {
            return Err(AppError::invalid_arg("拒绝设备命名空间路径"));
        }
    }
    Ok(())
}

/// 兼容旧调用点的布尔封装：委托 `ensure_native_absolute`，修复原 POSIX 绝对路径被误拒的缺陷。
pub fn ensure_absolute(p: &str) -> bool {
    ensure_native_absolute(Path::new(p)).is_ok()
}

/// 原生路径 → UTF-8 字符串的**可失败**转换。
///
/// 避免 `to_string_lossy` 把 Unix 非 UTF-8 名字变成另一个字符串再入库/打开。
/// 首版对不可无损转换的路径返回 `unsupported`，由扫描页显示跳过原因，原文件保持不动。
pub fn encode_native_path(path: &Path) -> AppResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::unsupported("文件名不是有效 UTF-8，首版暂不支持无损入库"))
}

#[cfg(windows)]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().any(|c| c == 0)
}

#[cfg(not(windows))]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn normalize_windows_path() {
        assert_eq!(normalize_path("D:\\Photos\\A.JPG"), "d:/Photos/A.JPG");
        assert_eq!(normalize_path("e:/dcim/"), "e:/dcim");
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_unix_keeps_backslash_and_case() {
        // 反斜杠是合法文件名字符，不得改成目录分隔符
        assert_eq!(normalize_path("/home/u/a\\b.jpg"), "/home/u/a\\b.jpg");
        // 不改大小写
        assert_eq!(normalize_path("/Photos/A.JPG"), "/Photos/A.JPG");
    }

    #[cfg(windows)]
    #[test]
    fn native_absolute_windows() {
        assert!(ensure_native_absolute(Path::new("C:/a/b.jpg")).is_ok());
        assert!(ensure_native_absolute(Path::new("D:\\x")).is_ok());
        // 扩展前缀与 UNC 是合法绝对路径，不能一律裁掉
        assert!(ensure_native_absolute(Path::new("\\\\?\\C:\\a")).is_ok());
        assert!(ensure_native_absolute(Path::new("\\\\server\\share\\a")).is_ok());
        // 拒绝相对、盘符相对、根相对、设备命名空间
        assert!(ensure_native_absolute(Path::new("a/b.jpg")).is_err());
        assert!(ensure_native_absolute(Path::new("C:foo")).is_err());
        assert!(ensure_native_absolute(Path::new("\\foo")).is_err());
        assert!(ensure_native_absolute(Path::new("")).is_err());
        assert!(ensure_native_absolute(Path::new("\\\\.\\PhysicalDrive0")).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn native_absolute_unix() {
        // 原缺陷：POSIX 绝对路径被误判为非绝对；现在必须通过
        assert!(ensure_native_absolute(Path::new("/home/u/a.jpg")).is_ok());
        assert!(ensure_native_absolute(Path::new("home/u/a.jpg")).is_err());
        // Unix 上反斜杠开头是相对路径（一个含反斜杠的文件名）
        assert!(ensure_native_absolute(Path::new("\\foo")).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn encode_native_path_rejects_non_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = OsStr::from_bytes(&[0x2f, 0x66, 0x6f, 0x6f, 0x2f, 0xff, 0x2e, 0x6a, 0x70, 0x67]);
        let p = Path::new(bad);
        assert!(encode_native_path(p).is_err());
        // 合法 UTF-8 正常返回
        assert_eq!(
            encode_native_path(Path::new("/home/u/a.jpg")).unwrap(),
            "/home/u/a.jpg"
        );
    }

    #[test]
    fn ensure_absolute_bool_wrapper() {
        // 布尔封装行为与新实现一致
        assert_eq!(ensure_absolute(""), false);
        assert_eq!(ensure_absolute("a/b.jpg"), false);
    }
}
