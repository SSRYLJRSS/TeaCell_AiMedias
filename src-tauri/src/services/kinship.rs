//! W5h：同源文件组（RAW+JPG）—— 同源判定纯函数层。
//!
//! 三层边界（用户定案 2026-08-31）：
//! - 数据层：两条 `assets` 记录永远独立（不合并行、不加列、不建表）
//! - 打标层：视为同一张照片（AI 只打一次，标签自动同步）—— 见 db/ai.rs create_batch 与 asset_tags assign_inner
//! - 浏览层：默认独立显示；设置开关可切合并显示（纯前端折叠，见 libraryStore）
//!
//! 同源关系从 `file_path` 派生（同目录 + 同主干名（大小写不敏感）+ 一个 RAW 一个非 RAW），
//! 零解码、零新列。不建表不加列：派生计算零维护成本（改名/移动/导出 move 无需同步）。
//!
//! 判定纯函数，便于单测（W7）。

/// 同源判定 key：目录 + 主干名。
///
/// 三端复核 B2 收紧（修复 `/A` 与 `/a` 被错误合并）：
/// - **目录部分不做全路径 lowercase**：保留已入库 parent 的大小写；不同实际目录不合并，
///   宁可保守漏配对，也不把不同目录的素材当同源自动扩散标签或跳过 AI。
/// - **只对主干名（stem）做大小写不敏感**：延续摄影工作流约定。
/// - **扩展名**按现有 RAW 列表识别（`is_raw_ext` 内部已 lowercase）。
/// - 不做 `\` → `/` 替换：Windows 侧路径已由 `normalize_path` 统一为正斜杠；
///   Unix 侧反斜杠是合法文件名字符，字符串替换会破坏语义。
///
/// 返回值附带「是否 RAW」信息以支持「一个 RAW 一个非 RAW」约束：
/// (kinship_key, is_raw)。kinship_key 相同且 is_raw 不同的两条记录为同源。
pub fn kinship_key(file_path: &str) -> (String, bool) {
    // 目录/文件名切分只按 '/'（Windows 已规范化为正斜杠；Unix 不引入盘符语义）。
    let (dir, name) = match file_path.rfind('/') {
        Some(i) => (&file_path[..i], &file_path[i + 1..]),
        None => ("", file_path),
    };
    // 主干名 = 最后一个 '.' 之前；无扩展名则整个名字
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    };
    let is_raw = crate::utils::mime::is_raw_ext(ext);
    // 目录保留原大小写；主干名折叠大小写。
    (format!("{dir}/{}", stem.to_lowercase()), is_raw)
}

/// 两条路径是否同源（同 key 且一个 RAW 一个非 RAW）。
pub fn is_kinship(a: &str, b: &str) -> bool {
    let (ka, ra) = kinship_key(a);
    let (kb, rb) = kinship_key(b);
    ka == kb && ra != rb
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W7 单测前置：kinship_key 边界
    #[test]
    fn kinship_key_boundaries() {
        // 典型配对（实测库形态：f:/all/_1091396.JPG + _1091396.RW2）
        let (k1, r1) = kinship_key("f:/all/_1091396.JPG");
        let (k2, r2) = kinship_key("f:/all/_1091396.RW2");
        assert_eq!(k1, k2);
        assert_ne!(r1, r2, "JPG 非 RAW、RW2 是 RAW");

        // 无扩展名：整个名字当主干
        let (k3, r3) = kinship_key("d:/x/photo");
        assert!(k3.ends_with("/photo"));
        assert!(!r3);

        // 多点文件名：最后一个点为分隔
        let (k4, _) = kinship_key("d:/x/my.photo.v2.jpg");
        assert!(k4.ends_with("/my.photo.v2"));

        // 主干名大小写不敏感（同目录、同盘符规范）
        let (k5, _) = kinship_key("d:/all/ABC.JPG");
        let (k6, _) = kinship_key("d:/all/abc.jpg");
        assert_eq!(k5, k6, "同目录下主干名大小写应折叠");

        // 三端复核 B2 收紧：目录大小写不同 = 不同实际目录，不得合并
        let (kd1, _) = kinship_key("/photos/A/X.RW2");
        let (kd2, _) = kinship_key("/photos/a/X.JPG");
        assert_ne!(kd1, kd2, "不同大小写目录不算同源，保守漏配对");
        assert!(
            !is_kinship("/photos/A/X.RW2", "/photos/a/X.JPG"),
            "不同大小写目录：即使一 RAW 一非 RAW 也不同源"
        );

        // 同名不同目录：不算同源
        let (k7, _) = kinship_key("d:/a/IMG_0001.RW2");
        let (k8, _) = kinship_key("d:/b/IMG_0001.RW2");
        assert_ne!(k7, k8);

        // 两个都是 RAW：key 相同但 is_kinship 为 false
        assert!(!is_kinship("d:/a/X.RW2", "d:/a/X.DNG"), "两个 RAW 不算同源");
        // 两个都是 JPG：同理
        assert!(
            !is_kinship("d:/a/X.JPG", "d:/a/X.PNG"),
            "两个非 RAW 不算同源"
        );
        // 一 RAW 一 JPG：算
        assert!(is_kinship("d:/a/X.JPG", "d:/a/X.RW2"));
    }
}
