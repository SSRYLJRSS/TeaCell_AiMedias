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

/// 同源组分类结果（三端复核 §11）。
///
/// 只有「恰好一个 RAW + 恰好一个非 RAW」才是可自动同步的配对（`Paired`）；其余任何组合
/// （单独一个、多 RAW、多非 RAW、`X.RAW + X.JPG + X.PNG` 等）都是 `Ambiguous`，
/// AI 去重、标签同步、数值分面同步、去重排除一律不得自动扩散。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KinshipGroup {
    /// 恰好一个 RAW + 恰好一个非 RAW：可安全双向同步。
    Paired { raw: i64, non_raw: i64 },
    /// 其他组合：不自动扩散。携带排序后的全部成员 id 供调用方展示/诊断。
    Ambiguous(Vec<i64>),
}

/// 对同 `kinship_key` 的**完整组**一次分类（§11）。
///
/// `members` 是同一组的全部 `(asset_id, is_raw)`。调用方必须先按 `kinship_key` 完整分组，
/// 不允许两两比较后再拼装——两两关系在 `1 RAW + 2 非 RAW` 等组合下会产生不对称同步。
pub fn classify_kinship_group(members: &[(i64, bool)]) -> KinshipGroup {
    let raws: Vec<i64> = members
        .iter()
        .filter(|(_, r)| *r)
        .map(|(id, _)| *id)
        .collect();
    let non_raws: Vec<i64> = members
        .iter()
        .filter(|(_, r)| !*r)
        .map(|(id, _)| *id)
        .collect();
    if raws.len() == 1 && non_raws.len() == 1 {
        KinshipGroup::Paired {
            raw: raws[0],
            non_raw: non_raws[0],
        }
    } else {
        let mut ids: Vec<i64> = members.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        KinshipGroup::Ambiguous(ids)
    }
}

/// 从完整 `(id, file_path)` 列表中取 `asset_id` 的**唯一同源兄弟**（§11）。
///
/// 仅当 `asset_id` 所在组恰好是「一个 RAW + 一个非 RAW」时返回对方 id；其余组合返回
/// `None`（Ambiguous 不自动扩散）。这是标签同步 / 数值分面同步的统一收口，禁止各调用点
/// 再自行比较 key。
pub fn paired_sibling(all: &[(i64, String)], asset_id: i64) -> Option<i64> {
    let target_path = all.iter().find(|(id, _)| *id == asset_id)?.1.as_str();
    let (target_key, _) = kinship_key(target_path);
    let members: Vec<(i64, bool)> = all
        .iter()
        .filter_map(|(id, p)| {
            let (k, r) = kinship_key(p);
            (k == target_key).then_some((*id, r))
        })
        .collect();
    match classify_kinship_group(&members) {
        KinshipGroup::Paired { raw, non_raw } => {
            if asset_id == raw {
                Some(non_raw)
            } else if asset_id == non_raw {
                Some(raw)
            } else {
                None
            }
        }
        KinshipGroup::Ambiguous(_) => None,
    }
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

    /// §11：只有 1 RAW + 1 非 RAW 才 Paired；其余组合 Ambiguous，不自动扩散。
    #[test]
    fn classify_group_only_pairs_exactly_one_raw_one_non_raw() {
        // 1 RAW + 1 非 RAW → Paired
        assert_eq!(
            classify_kinship_group(&[(1, true), (2, false)]),
            KinshipGroup::Paired { raw: 1, non_raw: 2 }
        );
        // 顺序无关
        assert_eq!(
            classify_kinship_group(&[(2, false), (1, true)]),
            KinshipGroup::Paired { raw: 1, non_raw: 2 }
        );
        // 单独一个 → Ambiguous
        assert_eq!(
            classify_kinship_group(&[(9, false)]),
            KinshipGroup::Ambiguous(vec![9])
        );
        // 1 RAW + 2 非 RAW（X.RAW + X.JPG + X.PNG）→ Ambiguous
        assert_eq!(
            classify_kinship_group(&[(1, true), (2, false), (3, false)]),
            KinshipGroup::Ambiguous(vec![1, 2, 3])
        );
        // 2 RAW + 1 非 RAW → Ambiguous
        assert_eq!(
            classify_kinship_group(&[(1, true), (2, true), (3, false)]),
            KinshipGroup::Ambiguous(vec![1, 2, 3])
        );
        // 2 RAW + 2 非 RAW → Ambiguous
        assert_eq!(
            classify_kinship_group(&[(1, true), (2, true), (3, false), (4, false)]),
            KinshipGroup::Ambiguous(vec![1, 2, 3, 4])
        );
    }

    /// §11：paired_sibling 只在恰好配对时返回对方，Ambiguous 一律 None。
    #[test]
    fn paired_sibling_covers_group_shapes() {
        // 1+1：互为兄弟
        let all = vec![(1, "d:/a/X.RW2".to_string()), (2, "d:/a/X.JPG".to_string())];
        assert_eq!(paired_sibling(&all, 1), Some(2));
        assert_eq!(paired_sibling(&all, 2), Some(1));

        // 1 RAW + 2 非 RAW：全部 None（不对称同步的根因组合）
        let all = vec![
            (1, "d:/a/X.RW2".to_string()),
            (2, "d:/a/X.JPG".to_string()),
            (3, "d:/a/X.PNG".to_string()),
        ];
        assert_eq!(paired_sibling(&all, 1), None);
        assert_eq!(paired_sibling(&all, 2), None);
        assert_eq!(paired_sibling(&all, 3), None);

        // 2 RAW + 1 非 RAW：全部 None
        let all = vec![
            (1, "d:/a/X.RW2".to_string()),
            (2, "d:/a/X.DNG".to_string()),
            (3, "d:/a/X.JPG".to_string()),
        ];
        assert_eq!(paired_sibling(&all, 3), None);

        // 同目录不同 stem：不同组，None
        let all = vec![(1, "d:/a/X.RW2".to_string()), (2, "d:/a/Y.JPG".to_string())];
        assert_eq!(paired_sibling(&all, 1), None);

        // 同 stem 不同目录：不同组，None
        let all = vec![(1, "d:/a/X.RW2".to_string()), (2, "d:/b/X.JPG".to_string())];
        assert_eq!(paired_sibling(&all, 1), None);

        // Windows 盘符 + 大小写主干名折叠：1+1 仍配对
        let all = vec![
            (1, "C:/all/ABC.RW2".to_string()),
            (2, "C:/all/abc.jpg".to_string()),
        ];
        assert_eq!(paired_sibling(&all, 1), Some(2));

        // 文件名含反斜杠（Unix 合法字符）：不同 stem，None
        let all = vec![
            (1, "/home/a\\b.RW2".to_string()),
            (2, "/home/a\\b.JPG".to_string()),
        ];
        // a\b 作为完整 stem 在两条上一致 → 1+1 配对
        assert_eq!(paired_sibling(&all, 1), Some(2));
    }
}
