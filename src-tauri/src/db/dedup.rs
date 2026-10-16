//! 重复素材扫描（M3-02 R-20）：按入库 hash 精确分组 + W5d 感知相似分组。
//! hash 相同 = 字节级完全一致（入库时 sha256 计算写入 assets.hash）。
//! W5d（§W5d）：相似分组 —— 短锁取 (id, file_path, phash) 全表入内存 → 高 16 位前缀分桶 →
//! 桶内两两汉明 ≤ threshold → 并查集合并（410 行全表分桶是微秒级；SQL 两两比较已被拒绝）。

use rusqlite::Connection;
use serde::Serialize;

use super::assets::{from_row, Asset, COLUMNS};
use super::libraries::VERIFIED_ASSET_SCOPE;
use crate::error::AppResult;
use crate::services::kinship::{classify_kinship_group, kinship_key, KinshipGroup};
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
            AND {VERIFIED_ASSET_SCOPE}
            AND a.hash IN (
                SELECT hash FROM assets a
                 WHERE a.hash IS NOT NULL AND {VERIFIED_ASSET_SCOPE}
                 GROUP BY hash HAVING COUNT(*) > 1
            )
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
/// `exclude_kinship`：只排除全库完整组恰好 1 RAW + 1 非 RAW 的精确配对；歧义组仍参与相似判定。
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
    // 1. 一次短锁读全库成员。无 phash 的素材仍计入同源完整组分类，但不参与相似比较。
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, a.file_path, a.phash FROM assets a
          WHERE a.deleted_at IS NULL AND {VERIFIED_ASSET_SCOPE}"
    ))?;
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
        library.push((id, path, phash.map(|value| value as u64)));
    }

    let excluded_pairs = if exclude_kinship {
        build_kinship_exclusion(&library)
    } else {
        std::collections::HashSet::new()
    };

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

    // 3. 前缀分桶 → 桶内两两比较 → 并查集合并
    let id_to_idx: std::collections::HashMap<i64, usize> = rows_all
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    let phash_rows = rows_all.clone();
    for (_prefix, bucket) in bucket_by_prefix(phash_rows) {
        for i in 0..bucket.len() {
            let (ida, pha) = bucket[i];
            let idxa = id_to_idx[&ida];
            for &(idb, phb) in bucket.iter().skip(i + 1) {
                if excluded_pairs.contains(&pair_key(ida, idb)) {
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

    // 4. 连通块 → ≥2 成员的组
    let mut groups: std::collections::HashMap<usize, Vec<i64>> = std::collections::HashMap::new();
    for (i, row) in rows_all.iter().enumerate().take(n) {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(row.0);
    }
    let member_lists: Vec<Vec<i64>> = groups.into_values().filter(|v| v.len() >= 2).collect();
    if member_lists.is_empty() {
        return Ok(Vec::new());
    }

    // 5. 回读完整 Asset（一次短锁；组内按 created_at 升序，与 scan_groups 语义一致）
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM assets a WHERE a.id = ?1 AND {VERIFIED_ASSET_SCOPE}"
    ))?;
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

fn pair_key(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// 只把全库完整分组中的精确 RAW/非-RAW 配对加入排除集。`need_ids` 子集不能改变组形状。
fn build_kinship_exclusion(
    library: &[(i64, String, Option<u64>)],
) -> std::collections::HashSet<(i64, i64)> {
    let mut groups: std::collections::HashMap<String, Vec<(i64, bool)>> =
        std::collections::HashMap::new();
    for (id, path, _phash) in library {
        let (key, is_raw) = kinship_key(path);
        groups.entry(key).or_default().push((*id, is_raw));
    }

    groups
        .values()
        .filter_map(|members| match classify_kinship_group(members) {
            KinshipGroup::Paired { raw, non_raw } => Some(pair_key(raw, non_raw)),
            KinshipGroup::Ambiguous(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn mem() -> rusqlite::Connection {
        let conn = init_memory().unwrap();
        conn.execute(
            "UPDATE libraries
                SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                    library_epoch=lower(hex(randomblob(16)))
              WHERE singleton=1",
            [],
        )
        .unwrap();
        conn
    }

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
        let id = conn.last_insert_rowid();
        let library = crate::db::libraries::current(conn).unwrap().unwrap();
        conn.execute(
            "UPDATE managed_asset_locations
                SET status='verified',reason='verified',library_id=?1,library_epoch=?2,
                    relative_path=printf('asset-%d.jpg',asset_id),
                    file_identity='fixture-file',content_sha256=?3,
                    ownership_receipt='fixture-receipt'
              WHERE asset_id=?4",
            rusqlite::params![
                library.library_id,
                library.library_epoch,
                "a".repeat(64),
                id
            ],
        )
        .unwrap();
        id
    }

    #[test]
    fn similar_group_merges_near_hashes() {
        let conn = mem();
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
        let conn = mem();
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
        let conn = mem();
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

    const PH0: i64 = 0xABCD_0000_0000_0001u64 as i64;
    const PH1: i64 = 0xABCD_0000_0000_0003u64 as i64;
    const PH2: i64 = 0xABCD_0000_0000_0007u64 as i64;

    #[test]
    fn ambiguous_kinship_group_is_not_excluded_from_similarity() {
        let conn = mem();
        insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH1));
        insert_asset(&conn, "C:/p/X.PNG", "p", Some(PH2));

        let groups = scan_similar_groups(&conn, 8, true, &[]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 3);
    }

    #[test]
    fn need_ids_subset_still_classifies_the_full_kinship_group() {
        let conn = mem();
        let raw = insert_asset(&conn, "C:/p/X.RW2", "r", Some(PH0));
        let jpg = insert_asset(&conn, "C:/p/X.JPG", "j", Some(PH1));
        // 即使第三个同源成员没有 phash，也必须计入完整组形状。
        insert_asset(&conn, "C:/p/X.PNG", "p", None);

        let groups = scan_similar_groups(&conn, 8, true, &[raw, jpg]).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].assets.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![raw, jpg]
        );
    }

    #[test]
    fn threshold_zero_disables_similar() {
        let conn = mem();
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
        let conn = mem();
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
