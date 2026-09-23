//! 重复素材扫描（M3-02 R-20）：按入库 hash 精确分组 + W5d 感知相似分组。
//! hash 相同 = 字节级完全一致（入库时 sha256 计算写入 assets.hash）。
//! W5d（§W5d）：相似分组 —— 短锁取 (id, file_path, phash) 全表入内存 → 高 16 位前缀分桶 →
//! 桶内两两汉明 ≤ threshold → 并查集合并（410 行全表分桶是微秒级；SQL 两两比较已被拒绝）。

use rusqlite::Connection;
use serde::Serialize;

use super::assets::{from_row, Asset, COLUMNS};
use crate::error::AppResult;
use crate::services::perceptual::{bucket_by_prefix, hamming};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupKind {
    /// 字节级完全一致（sha256 相同）
    Exact,
    /// 感知相似（dHash 汉明距离 ≤ 阈值）
    Similar,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    pub hash: String,
    /// exact = 相同 sha256；similar = 组内代表 phash（前端据此区分文案/排序）
    pub kind: GroupKind,
    /// 按 created_at 升序，首项最早（前端高亮为保留候选）
    pub assets: Vec<Asset>,
}

/// 扫描全库 hash 重复分组。
/// 走 idx_assets_hash 索引：子查询 GROUP BY 定位重复 hash，外层按 hash + created_at 排序取明细。
pub fn scan_groups(conn: &Connection) -> AppResult<Vec<DupGroup>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM assets a
          WHERE a.hash IS NOT NULL
            AND a.hash IN (SELECT hash FROM assets WHERE hash IS NOT NULL GROUP BY hash HAVING COUNT(*) > 1)
          ORDER BY a.hash, a.created_at, a.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], from_row)?
        .collect::<Result<Vec<Asset>, _>>()?;

    // 已按 hash 排序，顺序切块即分组
    let mut groups: Vec<DupGroup> = Vec::new();
    for asset in rows {
        let hash = asset.hash.clone().unwrap_or_default();
        match groups.last_mut() {
            Some(g) if g.kind == GroupKind::Exact && g.hash == hash => g.assets.push(asset),
            _ => groups.push(DupGroup {
                hash,
                kind: GroupKind::Exact,
                assets: vec![asset],
            }),
        }
    }
    Ok(groups)
}

/// 扫描感知相似分组。
///
/// `threshold`：汉明距离阈值（0 = 禁用相似检测，直接返回空）。
/// `exclude_kinship`：同源文件（同目录 + 同主干名 + **完整组恰好一 RAW 一非 RAW**，W5h/§11）
///   不算相似 —— 你的库 205 组 RAW+JPG 同画面会被 dHash 识别为「相似」，不开排除会被真实
///   连拍重复淹没。排除集按**全库完整组**分类（§11 `classify_kinship_group`）：歧义组
///   （多 RAW、多非 RAW、1 RAW + 多非 RAW…）不排除，避免把真重复当同源跳过。
/// `need_ids`：只对给定 id 子集做相似检测（空 = 全库）。
///
/// 算法：一次短锁取 (id, file_path, phash) 全表（只三列）→ 过滤 phash 非空 / 哨兵 0 →
/// 按高 16 位前缀分桶 → 桶内两两汉明比较（同源且 exclude_kinship 时跳过）→ 并查集合并 →
/// 按 id 回读完整 Asset 明细（与 scan_groups 的 COLUMNS 一致，组内按 created_at 升序）。
pub fn scan_similar_groups(
    conn: &Connection,
    threshold: u32,
    exclude_kinship: bool,
    need_ids: &[i64],
) -> AppResult<Vec<DupGroup>> {
    if threshold == 0 {
        return Ok(Vec::new());
    }
    // 1. 一次短锁取全库非删除资产 (id, file_path, phash)。
    //    phash 可空：无 phash 的成员不参与相似比较，但**仍要计入同源完整组分类**。
    //    否则 `X.RW2 + X.JPG + X.PNG(无 phash)` 会被当成 1 RAW + 1 非 RAW 误判为 Paired。
    let mut stmt =
        conn.prepare("SELECT id, file_path, phash FROM assets WHERE deleted_at IS NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut library: Vec<(i64, String, Option<u64>)> = Vec::new();
    for row in rows {
        let (id, path, phash) = row?;
        library.push((id, path, phash.map(|v| v as u64)));
    }

    // 2. 同源排除集（§11）：**全库视角**按 kinship_key 建完整组，逐组一次
    //    classify_kinship_group；只有恰好 1 RAW + 1 非 RAW 的 Paired 组，才把其精确
    //    (min,max) id 对加入排除集。Ambiguous 组（多 RAW / 多非 RAW / 1 RAW + 多非 RAW）
    //    不排除任何对。排除集与 need_ids 无关：即使本次只扫子集，也用完整库成员判定。
    let exclusion = if exclude_kinship {
        build_kinship_exclusion(&library)
    } else {
        std::collections::HashSet::new()
    };

    // 3. 相似候选：phash 非空且非哨兵 0，再按 need_ids 过滤。
    let mut rows_all: Vec<(i64, u64)> = Vec::new();
    for (id, _path, phash) in &library {
        let Some(ph) = *phash else { continue };
        if ph == 0 {
            continue; // 0 是 set_phash 的哨兵（纯色/无差分图）
        }
        if !need_ids.is_empty() && !need_ids.contains(id) {
            continue;
        }
        rows_all.push((*id, ph));
    }
    if rows_all.len() < 2 {
        return Ok(Vec::new());
    }

    // 2. 并查集（按 rows_all 下标）
    let n = rows_all.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    // 4. 前缀分桶 → 桶内两两比较 → 并查集合并
    let id_to_idx: std::collections::HashMap<i64, usize> = rows_all
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    let phash_rows: Vec<(i64, u64)> = rows_all.clone();
    for (_prefix, bucket) in bucket_by_prefix(phash_rows) {
        for i in 0..bucket.len() {
            let (ida, pha) = bucket[i];
            let idxa = id_to_idx[&ida];
            for &(idb, phb) in bucket.iter().skip(i + 1) {
                // §11：只跳过完整组分类得出的**精确 Paired 对**（同 key 且恰好一 RAW
                // 一非 RAW）。歧义组（两个 RAW、两个非 RAW、1 RAW + 多非 RAW…）不在
                // 排除集内，它们可能是真重复，仍参与相似判定。
                if exclusion.contains(&pair_key(ida, idb)) {
                    continue;
                }
                if hamming(pha, phb) <= threshold {
                    let idxb = id_to_idx[&idb];
                    let ra = find(&mut parent, idxa);
                    let rb = find(&mut parent, idxb);
                    if ra != rb {
                        parent[rb] = ra;
                    }
                }
            }
        }
    }

    // 5. 连通块 → ≥2 成员的组
    let mut groups: std::collections::HashMap<usize, Vec<i64>> = std::collections::HashMap::new();
    for (i, row) in rows_all.iter().enumerate().take(n) {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(row.0);
    }
    let member_lists: Vec<Vec<i64>> = groups.into_values().filter(|v| v.len() >= 2).collect();
    if member_lists.is_empty() {
        return Ok(Vec::new());
    }

    // 6. 回读完整 Asset（一次短锁；组内按 created_at 升序，与 scan_groups 语义一致）
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM assets WHERE id = ?1"))?;
    let mut out: Vec<DupGroup> = Vec::new();
    for mut member_ids in member_lists {
        member_ids.sort_unstable();
        let mut assets: Vec<Asset> = member_ids
            .iter()
            .filter_map(|&id| stmt.query_row([id], from_row).ok())
            .collect();
        assets.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let rep = assets.first().and_then(|a| a.phash).unwrap_or(0);
        out.push(DupGroup {
            hash: format!("{rep:016x}"),
            kind: GroupKind::Similar,
            assets,
        });
    }
    // 组间按代表 phash 排序（稳定输出，方便测试与 UI 复现）
    out.sort_by(|a, b| a.hash.cmp(&b.hash));
    Ok(out)
}

/// 有序 id 对（排除集键）：始终 (min, max)，与比较时的先后无关。
fn pair_key(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// 从**完整库**成员按 `kinship_key` 建同源排除集（§11）。
///
/// 只有完整组恰好是「一个 RAW + 一个非 RAW」（`KinshipGroup::Paired`）时，才把这对精确
/// id 加入排除集。歧义组（单成员、多 RAW、多非 RAW、1 RAW + 多非 RAW 等）不排除任何对，
/// 避免把真重复误当同源跳过。分组基于全库成员，不受 `need_ids` 子集影响。
fn build_kinship_exclusion(
    library: &[(i64, String, Option<u64>)],
) -> std::collections::HashSet<(i64, i64)> {
    use crate::services::kinship::{classify_kinship_group, kinship_key, KinshipGroup};
    // 按 kinship_key 聚合完整组的 (id, is_raw)。
    let mut groups: std::collections::HashMap<String, Vec<(i64, bool)>> =
        std::collections::HashMap::new();
    for (id, path, _phash) in library {
        let (key, is_raw) = kinship_key(path);
        groups.entry(key).or_default().push((*id, is_raw));
    }
    let mut exclusion = std::collections::HashSet::new();
    for members in groups.values() {
        if let KinshipGroup::Paired { raw, non_raw } = classify_kinship_group(members) {
            exclusion.insert(pair_key(raw, non_raw));
        }
    }
    exclusion
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn insert_asset(
        conn: &rusqlite::Connection,
        file_path: &str,
        hash: &str,
        phash: Option<i64>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at, hash, phash)
             VALUES (?1, ?1, '.jpg', 1, 'image/jpeg', 0, 0, ?2, ?3)",
            rusqlite::params![file_path, hash, phash],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn similar_group_merges_near_hashes() {
        let conn = init_memory().unwrap();
        let a = insert_asset(
            &conn,
            "C:/photos/a.jpg",
            "h1",
            Some(0xABCD_0000_0000_0001u64 as i64),
        );
        let b = insert_asset(
            &conn,
            "C:/photos/b.jpg",
            "h2",
            Some(0xABCD_0000_0000_0003u64 as i64),
        ); // 汉明 1
        insert_asset(
            &conn,
            "C:/photos/c.jpg",
            "h3",
            Some(0xFFFF_FFFF_FFFF_FFFFu64 as i64),
        ); // 远
        let groups = scan_similar_groups(&conn, 8, false, &[]).unwrap();
        assert_eq!(groups.len(), 1, "只有 a/b 接近成组");
        assert_eq!(groups[0].kind, GroupKind::Similar);
        let ids: Vec<i64> = groups[0].assets.iter().map(|x| x.id).collect();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn far_hashes_never_group() {
        let conn = init_memory().unwrap();
        insert_asset(
            &conn,
            "C:/photos/a.jpg",
            "h1",
            Some(0x0000_0000_0000_FFFFu64 as i64),
        );
        insert_asset(
            &conn,
            "C:/photos/b.jpg",
            "h2",
            Some(0xFFFF_FFFF_FFFF_0000u64 as i64),
        );
        let groups = scan_similar_groups(&conn, 8, false, &[]).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn kinship_exclusion_skips_raw_jpeg_pair() {
        let conn = init_memory().unwrap();
        // 同目录 + 同主干名（DSC0001）+ 一 RAW 一 JPG = 同源文件
        insert_asset(
            &conn,
            "C:/photos/DSC0001.RW2",
            "r",
            Some(0xABCD_0000_0000_0001u64 as i64),
        );
        insert_asset(
            &conn,
            "C:/photos/DSC0001.JPG",
            "j",
            Some(0xABCD_0000_0000_0003u64 as i64),
        );
        // 排除同源 → 不成组
        let excluded = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert!(excluded.is_empty(), "同源 RAW+JPG 应被排除");
        // 不排除 → 成组
        let included = scan_similar_groups(&conn, 8, false, &[]).unwrap();
        assert_eq!(included.len(), 1);
    }

    // 近似 phash 常量：以下同源歧义组测试都令三/四个成员两两汉明 ≤ 阈值，
    // 使其在「不因同源排除」时必然成一组，从而暴露误排除。
    const PH0: i64 = 0xABCD_0000_0000_0001u64 as i64;
    const PH1: i64 = 0xABCD_0000_0000_0003u64 as i64; // 与 PH0 汉明 1
    const PH2: i64 = 0xABCD_0000_0000_0007u64 as i64; // 与 PH0 汉明 2
    const PH3: i64 = 0xABCD_0000_0000_000Fu64 as i64; // 与 PH0 汉明 3

    /// §11：完整组恰好 1 RAW + 1 非 RAW → 仅这一对被排除。
    #[test]
    fn kinship_only_excludes_exact_one_raw_one_non_raw() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH1));
        // 恰好配对被排除 → 不成组
        assert!(scan_similar_groups(&conn, 8, true, &[]).unwrap().is_empty());
    }

    /// §11：1 RAW + 2 非 RAW（歧义组）→ 三者都不因同源排除。
    #[test]
    fn kinship_one_raw_two_non_raw_not_excluded() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH1));
        insert_asset(&conn, "C:/p/X.PNG", "p", Some(PH2));
        // 歧义组不排除任何对 → 三者相似应成一组
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 3);
    }

    /// §11：2 RAW + 1 非 RAW（歧义组）→ 不排除。
    #[test]
    fn kinship_two_raw_one_non_raw_not_excluded() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/X.DNG", "d", Some(PH1));
        insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH2));
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 3);
    }

    /// §11：2 RAW → 不排除。
    #[test]
    fn kinship_two_raw_not_excluded() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/X.DNG", "d", Some(PH1));
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 2);
    }

    /// §11：2 非 RAW → 不排除。
    #[test]
    fn kinship_two_non_raw_not_excluded() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH0));
        insert_asset(&conn, "C:/p/X.PNG", "p", Some(PH1));
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 2);
    }

    /// §11：同 stem 不同目录 → 不配对，不排除（RAW+JPG 在不同目录仍是真重复候选）。
    #[test]
    fn kinship_same_stem_different_dir_not_paired() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "C:/a/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/b/X.JPG", "j", Some(PH1));
        // 不同目录不同源 → 不排除 → 成组
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 2);
    }

    /// §11：POSIX 目录大小写不同 → 视为不同目录 → 不配对，不排除。
    #[test]
    fn kinship_posix_dir_case_differs_not_paired() {
        let conn = init_memory().unwrap();
        insert_asset(&conn, "/photos/A/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "/photos/a/X.JPG", "j", Some(PH1));
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 2);
    }

    /// §11 关键回归：need_ids 只含 1 RAW + 1 非 RAW 两项，但全库同 key 还有第三个非 RAW
    /// 成员 → 完整组是 Ambiguous，这一对不得被排除。
    #[test]
    fn kinship_need_ids_subset_still_uses_full_group() {
        let conn = init_memory().unwrap();
        let raw = insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        let jpg = insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH1));
        // 全库还有同 key 第三成员（非 RAW），使完整组变 Ambiguous
        insert_asset(&conn, "C:/p/X.PNG", "p", Some(PH2));
        // 只扫 RAW+JPG 子集：若按两两判断会误排除；按完整组分类应保留 → 成组
        let groups = scan_similar_groups(&conn, 8, true, &[raw, jpg]).unwrap();
        assert_eq!(groups.len(), 1);
        let ids: Vec<i64> = groups[0].assets.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![raw, jpg]);
    }

    /// §11：四项 phash 相近（1 RAW + 1 非 RAW 精确配对 + 另两张无关非 RAW）时，
    /// 排除对不影响其余真重复分组。
    #[test]
    fn kinship_exclusion_does_not_break_other_duplicates() {
        let conn = init_memory().unwrap();
        // 精确配对（应排除）
        insert_asset(&conn, "C:/p/A.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/A.JPG", "j", Some(PH1));
        // 另一对真重复（不同 stem，不同源）
        insert_asset(&conn, "C:/p/B.JPG", "b1", Some(PH2));
        insert_asset(&conn, "C:/p/C.JPG", "b2", Some(PH3));
        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        // A.RW2/A.JPG 被排除后，四张里剩 B/C（+可能与 A 的非配对项相似）。
        // 断言：A.RW2 与 A.JPG 不会因同源出现在同一组的这条配对边被删掉。
        // 至少 B.JPG 与 C.JPG 应成一组。
        assert!(
            groups.iter().any(|g| {
                let paths: Vec<&str> = g.assets.iter().map(|a| a.file_path.as_str()).collect();
                paths.contains(&"C:/p/B.JPG") && paths.contains(&"C:/p/C.JPG")
            }),
            "非同源的 B/C 真重复应正常成组"
        );
    }

    #[test]
    fn threshold_zero_disables_similar() {
        let conn = init_memory().unwrap();
        insert_asset(
            &conn,
            "C:/photos/a.jpg",
            "h1",
            Some(0xABCD_0000_0000_0001u64 as i64),
        );
        insert_asset(
            &conn,
            "C:/photos/b.jpg",
            "h2",
            Some(0xABCD_0000_0000_0003u64 as i64),
        );
        assert!(scan_similar_groups(&conn, 0, false, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn need_ids_restricts_scope() {
        let conn = init_memory().unwrap();
        let a = insert_asset(
            &conn,
            "C:/photos/a.jpg",
            "h1",
            Some(0xABCD_0000_0000_0001u64 as i64),
        );
        insert_asset(
            &conn,
            "C:/photos/b.jpg",
            "h2",
            Some(0xABCD_0000_0000_0003u64 as i64),
        );
        insert_asset(
            &conn,
            "C:/photos/c.jpg",
            "h3",
            Some(0xFFFF_FFFF_FFFF_FFFFu64 as i64),
        );
        // 只检测 [a]：与任何其他 id 距离再近都不参与（b/c 不在范围内）→ 空
        let groups = scan_similar_groups(&conn, 8, false, &[a]).unwrap();
        assert!(groups.is_empty());
    }
}
