//! 路径规范化与安全校验。
//!
//! 原生路径、比较键和跨平台显示路径不是同一种数据。`normalize_path` 只做当前目标
//! 的数据库兼容规范化：Windows 可以安全地统一分隔符和盘符，Unix 必须保留反斜杠和
//! 大小写，因为它们可能是实际文件名/目录名的一部分。

#[cfg(windows)]
/// Windows 规范化：统一小写盘符 + 反斜杠转正斜杠（历史数据库兼容）。
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_uppercase() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        s.replace_range(0..1, &drive.to_string());
    }
    while s.len() > 3 && s.ends_with('/') {
        s.pop();
    }
    s
}

#[cfg(not(windows))]
/// Unix 规范化：反斜杠是合法文件名字符，不替换、不改大小写，只去掉末尾冗余 `/`。
pub fn normalize_path(p: &str) -> String {
    let mut s = p.to_owned();
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// 使用当前目标的原生路径语义检查绝对路径，拒绝空值、NUL 与相对路径。
pub fn ensure_absolute(p: &str) -> bool {
    if p.is_empty() || p.contains('\0') {
        return false;
    }
    let path = std::path::Path::new(p);
    if !path.is_absolute() {
        return false;
    }
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if text.starts_with("\\\\.\\") || text.starts_with("//./") {
            return false;
        }
    }
    true
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
    fn normalize_unix_path() {
        assert_eq!(normalize_path("/home/u/a\\b.jpg"), "/home/u/a\\b.jpg");
        assert_eq!(normalize_path("/Photos/A.JPG"), "/Photos/A.JPG");
        assert_eq!(normalize_path("/home/u/"), "/home/u");
    }

    #[cfg(windows)]
    #[test]
    fn absolute_check() {
        assert!(ensure_absolute("C:/a/b.jpg"));
        assert!(ensure_absolute("D:\\x"));
        assert!(!ensure_absolute("a/b.jpg"));
        assert!(!ensure_absolute(""));
        assert!(!ensure_absolute("\\\\.\\PhysicalDrive0"));
    }

    #[cfg(not(windows))]
    #[test]
    fn absolute_check_unix() {
        assert!(ensure_absolute("/home/u/a.jpg"));
        assert!(!ensure_absolute("home/u/a.jpg"));
        assert!(!ensure_absolute("\\foo"));
        assert!(!ensure_absolute(""));
    }
}
