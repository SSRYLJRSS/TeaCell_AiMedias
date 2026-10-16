//! QA 实测集成测试（2026-08-14 严过关）
//! 覆盖：migrations 幂等 / assets CRUD+级联删除 / LIKE 转义(% _ \) / FTS 搜索(海边vs上海湖边、引号文件名、ASCII 子串)
//!       批量删除 / tag 父子/循环校验 / 分页边界
//! 运行：cargo test --test qa_edge_tests

use teacell_ai_media_manager_lib::db::ai::CategorizedTags;
use teacell_ai_media_manager_lib::db::assets::AssetFilter;
use teacell_ai_media_manager_lib::db::{self, ai, asset_tags, assets, migrations, tags};
use teacell_ai_media_manager_lib::error::AppResult;
use teacell_ai_media_manager_lib::state::Database;

fn setup() -> rusqlite::Connection {
    db::init_memory().expect("内存库初始化失败")
}

fn add_asset(conn: &rusqlite::Connection, path: &str, name: &str, ext: &str, mime: &str) -> i64 {
    let id =
        assets::insert(conn, path, name, ext, 1024, mime, 1700000000000).expect("插入素材失败");
    conn.execute(
        "UPDATE libraries
            SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                library_epoch=lower(hex(randomblob(16)))
          WHERE singleton=1 AND (health!='ready' OR root_identity IS NULL)",
        [],
    )
    .unwrap();
    let library = db::libraries::current(conn).unwrap().unwrap();
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

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// ═══════════════ ① migrations 幂等 ═══════════════

/// 当前迁移链终版 user_version（新迁移追加时同步更新；防止硬编码断言过期）
/// W1：V19 数据列 / V20 分面合表 / V21 FTS 触发器 / V22a 分面能力矩阵+约束+review_state+溯源
const LATEST_VERSION: i64 = 22;

#[test]
fn migrate_twice_is_idempotent() -> AppResult<()> {
    let conn = setup();
    // 再次执行 migrate（已到终版，幂等）
    migrations::migrate(&conn)?;
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, LATEST_VERSION);
    // 表仍存在且可用
    add_asset(&conn, "d:/p/a.jpg", "a.jpg", "jpg", "image/jpeg");
    Ok(())
}

#[test]
fn migrate_after_data_preserves_rows() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/keep.jpg", "keep.jpg", "jpg", "image/jpeg");
    let tag = tags::create(&conn, "保留", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    migrations::migrate(&conn)?; // 数据在库时重复迁移
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM assets"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 1);
    assert_eq!(db::search::search_asset_ids_all(&conn, "保留")?, vec![id]);
    Ok(())
}

// ═══════════════ ② assets CRUD + 级联删除 ═══════════════

#[test]
fn delete_asset_cascades_tags_fts() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create(&conn, "海边", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    // 级联前
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM fts_content"), 1);

    let n = assets::delete(&conn, &[id])?;
    assert_eq!(n, 1);
    // 级联：asset_tags / fts_content / FTS 索引全部清除
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM assets"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM fts_content"), 0);
    assert!(db::search::search_asset_ids_all(&conn, "海边")?.is_empty());
    assert!(db::search::search_asset_ids_all(&conn, "海边日落")?.is_empty());
    Ok(())
}

#[test]
fn delete_tag_cascades_assignments_and_refreshes_fts() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p1.jpg", "p1.jpg", "jpg", "image/jpeg");
    let tag = tags::create(&conn, "山野花", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    // 3 字标签走 FTS 可命中
    assert_eq!(db::search::search_asset_ids_all(&conn, "山野花")?, vec![id]);

    tags::delete(&conn, tag.id)?;
    // 关联清除 + FTS 无幻影；素材仍在库（变未打标）
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 0);
    assert!(
        db::search::search_asset_ids_all(&conn, "山野花")?.is_empty(),
        "删标签后 FTS 幻影命中"
    );
    let page = assets::list(
        &conn,
        &AssetFilter {
            untagged_only: true,
            ..Default::default()
        },
    )?;
    assert_eq!(page.total, 1);
    Ok(())
}

#[test]
fn delete_parent_tag_cascades_children() -> AppResult<()> {
    let conn = setup();
    let parent = tags::create(&conn, "风景", None)?;
    let child = tags::create(&conn, "海边", Some(parent.id))?;
    let grand = tags::create(&conn, "日出", Some(child.id))?;
    let id = add_asset(&conn, "d:/p/g.jpg", "g.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[id], &[grand.id], "manual")?;

    tags::delete(&conn, parent.id)?;
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM tags"),
        0,
        "子标签应级联删除"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 0);
    assert!(db::search::search_asset_ids_all(&conn, "日出")?.is_empty());
    Ok(())
}

// ═══════════════ ③ LIKE 转义（% _ \）═══════════════

#[test]
fn like_escape_percent() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/进度100%.jpg",
        "进度100%.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/进度100X.jpg",
        "进度100X.jpg",
        "jpg",
        "image/jpeg",
    );
    // 1 字 → LIKE 兜底；% 必须转义为字面量（不应命中不含 % 的 100X）
    assert_eq!(db::search::search_asset_ids_all(&conn, "%")?, vec![hit]);
    // 2 字组合：%_ 连写 → LIKE 转义后应字面匹配
    let hit2 = add_asset(
        &conn,
        "d:/p/50%_off.jpg",
        "50%_off.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/50%Xoff.jpg",
        "50%Xoff.jpg",
        "jpg",
        "image/jpeg",
    );
    assert_eq!(db::search::search_asset_ids_all(&conn, "%_")?, vec![hit2]);
    Ok(())
}

#[test]
fn like_escape_underscore() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(&conn, "d:/p/a_b.jpg", "a_b.jpg", "jpg", "image/jpeg");
    add_asset(&conn, "d:/p/axb.jpg", "axb.jpg", "jpg", "image/jpeg");
    // "_" 必须转义为字面量，不能当单字符通配
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "_")?,
        vec![hit],
        "搜单下划线应只命中含 _ 的文件"
    );
    Ok(())
}

#[test]
fn like_escape_backslash() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/dir\\file.jpg",
        "dir\\file.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/dirXfile.jpg",
        "dirXfile.jpg",
        "jpg",
        "image/jpeg",
    );
    // 单反斜杠 1 字 → LIKE；反斜杠作为字面量（同时验证转义本身不把 \ 当转义符吞掉）
    assert_eq!(db::search::search_asset_ids_all(&conn, "\\")?, vec![hit]);
    Ok(())
}

#[test]
fn like_tag_name_with_special_char() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/t1.jpg", "t1.jpg", "jpg", "image/jpeg");
    let tag = tags::create(&conn, "50%优惠", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    // 标签名含 %，走 EXISTS LIKE 也应字面匹配（2 字 "50" 命中标签名的 50）
    assert_eq!(db::search::search_asset_ids_all(&conn, "50")?, vec![id]);
    // 含 % 的标签名用 2 字内 % 组合检索（LIKE 转义）
    let tag2 = tags::create(&conn, "A%B", None)?;
    asset_tags::assign(&conn, &[id], &[tag2.id], "manual")?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "%B")?, vec![id]);
    Ok(())
}

/// ⚠ FTS 路径含 % 的查询（3+ 字）：不崩溃但无法命中（token 边界问题，见 BUG-A/B）
#[test]
fn fts_special_char_percent_query() -> AppResult<()> {
    let conn = setup();
    add_asset(
        &conn,
        "d:/p/进度100%.jpg",
        "进度100%.jpg",
        "jpg",
        "image/jpeg",
    );
    // 预期（用户直觉）：含 % 文件名可被检索；实际：FTS 短语命中 token "100" 但索引 token 是 "度100" → 空
    let got = db::search::search_asset_ids_all(&conn, "100%")?;
    assert_eq!(
        got,
        vec![1],
        "搜索「100%」应命中进度100%.jpg；实际返回 {got:?}（FTS token 边界缺陷）"
    );
    Ok(())
}

// ═══════════════ ④ FTS 搜索 ═══════════════

#[test]
fn fts_cjk_phrase_3char_no_false_positive() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/上海公园湖边合影.jpg",
        "上海公园湖边合影.jpg",
        "jpg",
        "image/jpeg",
    );
    // 3 字 → FTS 短语：海边日 不得误命中「上海…湖边」
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "海边日")?,
        vec![hit]
    );
    Ok(())
}

#[test]
fn fts_cjk_phrase_no_false_positive_4char() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/上海公园湖边合影.jpg",
        "上海公园湖边合影.jpg",
        "jpg",
        "image/jpeg",
    );
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "海边日落")?,
        vec![hit]
    );
    Ok(())
}

#[test]
fn fts_cjk_prefix_tokens_ok() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/上海公园湖边合影.jpg",
        "上海公园湖边合影.jpg",
        "jpg",
        "image/jpeg",
    );
    // 「上海湖」跨 token 组合不应命中；「上海公园」应命中
    assert!(db::search::search_asset_ids_all(&conn, "上海湖")?.is_empty());
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "上海公园")?,
        vec![hit]
    );
    Ok(())
}

/// 引号文件名：写入侧原样存引号，查询侧 FTS5 用 "" 转义
#[test]
fn fts_filename_with_double_quote() -> AppResult<()> {
    let conn = setup();
    let name = "海边\"落日\".jpg";
    let hit = add_asset(&conn, &format!("d:/p/{name}"), name, "jpg", "image/jpeg");
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "海边\"落日\"")?,
        vec![hit]
    );
    Ok(())
}

#[test]
fn fts_filename_with_star() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(&conn, "d:/p/图*片.jpg", "图*片.jpg", "jpg", "image/jpeg");
    // * 在引号短语内为字面量
    assert_eq!(db::search::search_asset_ids_all(&conn, "图*片")?, vec![hit]);
    Ok(())
}

/// ⚠ 已知疑点：ASCII 3+ 字符子串（不落在 unicode61 token 边界）能否命中？
/// 预期（用户直觉/子串搜索）：命中；实际：见运行结果
#[test]
fn fts_ascii_partial_token_substring() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/IMG_2024_001.jpg",
        "IMG_2024_001.jpg",
        "jpg",
        "image/jpeg",
    );
    // token 对齐：整词可命中
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "IMG_2024")?,
        vec![hit]
    );
    assert_eq!(db::search::search_asset_ids_all(&conn, "001")?, vec![hit]);
    // token 内部子串：预期应命中（子串搜索一致性），实际待验证
    let got = db::search::search_asset_ids_all(&conn, "202")?;
    assert_eq!(
        got,
        vec![hit],
        "搜索「202」应命中 IMG_2024_001.jpg（子串）；实际返回 {got:?}"
    );
    let got2 = db::search::search_asset_ids_all(&conn, "IMG_202")?;
    assert_eq!(got2, vec![hit], "搜索「IMG_202」应命中；实际返回 {got2:?}");
    Ok(())
}

#[test]
fn fts_ascii_partial_token_photo() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    // "photo" 5 字 → FTS；预期子串命中，实际待验证
    let got = db::search::search_asset_ids_all(&conn, "photo")?;
    assert_eq!(
        got,
        vec![hit],
        "搜索「photo」应命中 photo001.jpg；实际返回 {got:?}"
    );
    Ok(())
}

#[test]
fn fts_punctuation_only_no_crash() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/a.jpg", "a.jpg", "jpg", "image/jpeg");
    // 纯标点 3 字 → FTS；不应崩溃，应返回空
    let got = db::search::search_asset_ids_all(&conn, "!!!")?;
    assert!(got.is_empty(), "纯标点搜索应返回空；实际 {got:?}");
    Ok(())
}

// ═══════════════ ⑤ 批量删除 ═══════════════

#[test]
fn batch_delete_assets() -> AppResult<()> {
    let conn = setup();
    let ids: Vec<i64> = (0..5)
        .map(|i| {
            add_asset(
                &conn,
                &format!("d:/p/b{i}.jpg"),
                &format!("b{i}.jpg"),
                "jpg",
                "image/jpeg",
            )
        })
        .collect();
    let tag = tags::create(&conn, "批量", None)?;
    asset_tags::assign(&conn, &ids, &[tag.id], "manual")?;

    let n = assets::delete(&conn, &[ids[0], ids[2], ids[4]])?;
    assert_eq!(n, 3);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM assets"), 2);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM asset_tags"),
        2,
        "级联后只剩 2 条关联"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM fts_content"), 2);
    Ok(())
}

#[test]
fn batch_delete_mixed_existing_and_missing() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/x.jpg", "x.jpg", "jpg", "image/jpeg");
    let n = assets::delete(&conn, &[id, 99999, 88888])?;
    assert_eq!(n, 1, "只应删除存在的 1 条");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM assets"), 0);
    Ok(())
}

#[test]
fn batch_delete_empty() -> AppResult<()> {
    let conn = setup();
    assert_eq!(assets::delete(&conn, &[])?, 0);
    Ok(())
}

#[test]
fn batch_delete_duplicate_ids() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/d.jpg", "d.jpg", "jpg", "image/jpeg");
    let n = assets::delete(&conn, &[id, id])?;
    assert_eq!(n, 1, "重复 id 不应重复计数");
    Ok(())
}

// ═══════════════ ⑥ tag 父子 / 循环校验 ═══════════════

#[test]
fn tag_reparent_to_self_rejected() -> AppResult<()> {
    let conn = setup();
    let a = tags::create(&conn, "A", None)?;
    assert!(tags::update(&conn, a.id, None, Some(Some(a.id))).is_err());
    Ok(())
}

#[test]
fn tag_reparent_deep_cycle_rejected() -> AppResult<()> {
    let conn = setup();
    let a = tags::create(&conn, "A", None)?;
    let b = tags::create(&conn, "B", Some(a.id))?;
    let c = tags::create(&conn, "C", Some(b.id))?;
    // A 挂到 B / C 下均构成环
    assert!(tags::update(&conn, a.id, None, Some(Some(b.id))).is_err());
    assert!(tags::update(&conn, a.id, None, Some(Some(c.id))).is_err());
    // B 挂到 C 下也构成环
    assert!(tags::update(&conn, b.id, None, Some(Some(c.id))).is_err());
    Ok(())
}

#[test]
fn tag_reparent_valid_moves() -> AppResult<()> {
    let conn = setup();
    let a = tags::create(&conn, "A", None)?;
    let b = tags::create(&conn, "B", None)?;
    // B 挂到 A 下 → 合法
    tags::update(&conn, b.id, None, Some(Some(a.id)))?;
    // 提升为根 → 合法
    tags::update(&conn, b.id, None, Some(None))?;
    // 改名 → 合法
    tags::update(&conn, b.id, Some("B2"), None)?;
    let tree = tags::list_tree(&conn)?;
    assert_eq!(tree.len(), 2);
    assert!(tree.iter().any(|n| n.tag.name == "B2"));
    Ok(())
}

#[test]
fn tag_reparent_to_nonexistent_rejected() -> AppResult<()> {
    let conn = setup();
    let a = tags::create(&conn, "A", None)?;
    assert!(
        tags::update(&conn, a.id, None, Some(Some(424242))).is_err(),
        "挂到不存在父级应被 FK 拒绝"
    );
    Ok(())
}

// ═══════════════ ⑦ 分页 / 偏移边界 ═══════════════

#[test]
fn pagination_limit_zero_clamped() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/p0.jpg", "p0.jpg", "jpg", "image/jpeg");
    add_asset(&conn, "d:/p/p1.jpg", "p1.jpg", "jpg", "image/jpeg");
    let page = assets::list(
        &conn,
        &AssetFilter {
            limit: 0,
            ..Default::default()
        },
    )?;
    assert_eq!(page.items.len(), 1, "limit=0 应被钳制为 1");
    assert_eq!(page.total, 2);
    Ok(())
}

#[test]
fn pagination_negative_offset_clamped() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/p0.jpg", "p0.jpg", "jpg", "image/jpeg");
    let page = assets::list(
        &conn,
        &AssetFilter {
            offset: -5,
            ..Default::default()
        },
    )?;
    assert_eq!(page.items.len(), 1, "负 offset 应钳制为 0");
    assert!(!page.has_more);
    Ok(())
}

#[test]
fn pagination_offset_beyond_total() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/p0.jpg", "p0.jpg", "jpg", "image/jpeg");
    let page = assets::list(
        &conn,
        &AssetFilter {
            offset: 10,
            ..Default::default()
        },
    )?;
    assert!(page.items.is_empty(), "offset 超界应返回空");
    assert!(!page.has_more);
    assert_eq!(page.total, 1);
    Ok(())
}

#[test]
fn pagination_has_more_boundary() -> AppResult<()> {
    let conn = setup();
    for i in 0..5 {
        add_asset(
            &conn,
            &format!("d:/p/m{i}.jpg"),
            &format!("m{i}.jpg"),
            "jpg",
            "image/jpeg",
        );
    }
    let p1 = assets::list(
        &conn,
        &AssetFilter {
            limit: 3,
            ..Default::default()
        },
    )?;
    assert!(p1.has_more);
    let p2 = assets::list(
        &conn,
        &AssetFilter {
            limit: 3,
            offset: 3,
            ..Default::default()
        },
    )?;
    assert!(!p2.has_more, "恰好取完应 has_more=false");
    assert_eq!(p2.items.len(), 2);
    Ok(())
}

// ═══════════════ ⑧ FTS 与写入侧一致性 ═══════════════

#[test]
fn fts_consistency_tag_rename() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/r.jpg", "r.jpg", "jpg", "image/jpeg");
    let tag = tags::create(&conn, "山野花", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "山野花")?, vec![id]);

    tags::update(&conn, tag.id, Some("大山花"), None)?;
    // 触发器应刷新 tag_names
    assert!(
        db::search::search_asset_ids_all(&conn, "山野花")?.is_empty(),
        "改名后旧标签不应命中"
    );
    assert_eq!(db::search::search_asset_ids_all(&conn, "大山花")?, vec![id]);
    Ok(())
}

#[test]
fn fts_consistency_asset_rename() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/old.jpg", "old.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET file_name = '新名字.jpg' WHERE id = ?1",
        [id],
    )?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "新名字")?, vec![id]);
    assert!(db::search::search_asset_ids_all(&conn, "old")?.is_empty());
    Ok(())
}

#[test]
fn fts_asset_tag_join_order_independent() -> AppResult<()> {
    // 多标签 group_concat 顺序不影响命中（预期）；实际顺序相关 → BUG-D 回归标记
    let conn = setup();
    let id = add_asset(&conn, "d:/p/j.jpg", "j.jpg", "jpg", "image/jpeg");
    let t1 = tags::create(&conn, "日落", None)?;
    let t2 = tags::create(&conn, "海边", None)?;
    asset_tags::assign(&conn, &[id], &[t1.id, t2.id], "manual")?;
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "海边日落")?,
        vec![id]
    );
    Ok(())
}

// ═══════════════ ⑨ BUG-A 新增：ASCII token 中部子串（非前缀）═══════════════

#[test]
fn fts_ascii_middle_substring() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/IMG_2024_001.jpg",
        "IMG_2024_001.jpg",
        "jpg",
        "image/jpeg",
    );
    let hit2 = add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    // token 中部子串（非前缀）：024 命中 2024；oto 命中 photo001
    let mut got = db::search::search_asset_ids_all(&conn, "024")?;
    got.sort();
    assert_eq!(
        got,
        vec![hit],
        "搜「024」应命中 IMG_2024_001.jpg（token 中部子串）"
    );
    let mut got2 = db::search::search_asset_ids_all(&conn, "oto")?;
    got2.sort();
    assert_eq!(
        got2,
        vec![hit2],
        "搜「oto」应命中 photo001.jpg（token 中部子串）"
    );
    Ok(())
}

// ═══════════════ ⑩ BUG-B 新增：CJK↔ASCII 混合子串 ═══════════════

#[test]
fn fts_cjk_ascii_mixed_substring() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/进度100%.jpg",
        "进度100%.jpg",
        "jpg",
        "image/jpeg",
    );
    for q in ["100%", "100", "进度10"] {
        let mut got = db::search::search_asset_ids_all(&conn, q)?;
        got.sort();
        assert_eq!(got, vec![id], "搜索「{q}」应命中进度100%.jpg");
    }
    // 单字「度」走 LIKE 兜底
    let mut got = db::search::search_asset_ids_all(&conn, "度")?;
    got.sort();
    assert_eq!(got, vec![id]);
    Ok(())
}

// ═══════════════ ⑪ BUG-D 新增：标签顺序无关（2 字块 AND）═══════════════

#[test]
fn fts_tag_order_both_orders() -> AppResult<()> {
    let conn = setup();
    let t_ri = tags::create(&conn, "日落", None)?;
    let t_hb = tags::create(&conn, "海边", None)?;

    // 顺序一：日落 → 海边
    let id1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[id1], &[t_ri.id, t_hb.id], "manual")?;
    let mut g1 = db::search::search_asset_ids_all(&conn, "海边日落")?;
    g1.sort();
    assert_eq!(g1, vec![id1], "日落→海边 顺序应命中");

    // 顺序二：海边 → 日落
    let id2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[id2], &[t_hb.id, t_ri.id], "manual")?;
    let mut g2 = db::search::search_asset_ids_all(&conn, "海边日落")?;
    g2.sort();
    assert!(g2.contains(&id1) && g2.contains(&id2), "两种顺序均应命中");
    Ok(())
}

#[test]
fn fts_tag_order_three_tags() -> AppResult<()> {
    let conn = setup();
    let t1 = tags::create(&conn, "海边", None)?;
    let t2 = tags::create(&conn, "日落", None)?;
    let t3 = tags::create(&conn, "山峰", None)?;
    let id = add_asset(&conn, "d:/p/m.jpg", "m.jpg", "jpg", "image/jpeg");
    // 任意分配顺序（山峰、海边、日落）
    asset_tags::assign(&conn, &[id], &[t3.id, t1.id, t2.id], "manual")?;
    let mut got = db::search::search_asset_ids_all(&conn, "海边日落")?;
    got.sort();
    assert_eq!(got, vec![id]);
    Ok(())
}

// ═══════════════ ⑫ BUG-E 新增：list_ids 与 list 一致性 ═══════════════

#[test]
fn list_ids_matches_list_filter() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/cat.jpg", "cat.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/dog.mp4", "dog.mp4", "mp4", "video/mp4");
    // 未打标的第三张素材（驱动 untagged_only 筛选用例；变量本身无需直接引用）
    let _a3 = add_asset(&conn, "d:/p/bird.jpg", "bird.jpg", "jpg", "image/jpeg");
    let tag = tags::create(&conn, "动物", None)?;
    asset_tags::assign(&conn, &[a1, a2], &[tag.id], "manual")?;

    let mk = |asset_type: Option<&str>,
              untagged: bool,
              tag_id: Option<i64>,
              search: Option<&str>| AssetFilter {
        asset_type: asset_type.map(String::from),
        untagged_only: untagged,
        tag_id,
        search: search.map(String::from),
        ..Default::default()
    };

    let cases: Vec<AssetFilter> = vec![
        mk(None, false, None, None),
        mk(Some("image"), false, None, None),
        mk(Some("video"), false, None, None),
        mk(None, true, None, None),
        mk(None, false, Some(tag.id), None),
        mk(None, false, None, Some("cat")),
    ];
    for f in cases {
        let page = assets::list(&conn, &f)?;
        let ids = assets::list_ids(&conn, &f)?;
        let mut from_list: Vec<i64> = page.items.iter().map(|a| a.id).collect();
        let mut from_ids = ids.clone();
        from_list.sort();
        from_ids.sort();
        assert_eq!(from_list, from_ids, "list_ids 与 list 的 id 集合应一致");
    }
    Ok(())
}

// ═══════════════ ⑬ V3 迁移：回源重算 + 幂等可重入 ═══════════════

#[test]
fn v3_rebuild_normalizes_fts_content() -> AppResult<()> {
    let conn = setup(); // 已是 V3
    let id = add_asset(
        &conn,
        "d:/p/进度100%.jpg",
        "进度100%.jpg",
        "jpg",
        "image/jpeg",
    );

    // 模拟 V2 旧索引产物（旧 cjk_bigram：CJK↔非CJK 边界无空格 → "进 度100%.jpg"）
    conn.execute(
        "UPDATE fts_content SET file_name = '进 度100%.jpg' WHERE asset_id = ?1",
        [id],
    )?;
    // 回退 user_version 到 2，模拟「未跑 V3」状态（trg_fc_au 已同步旧产物到 assets_fts）
    conn.pragma_update(None, "user_version", 2)?;

    // 跑 V3 迁移（重建触发器 + 回源重算 fts_content + FTS rebuild）
    migrations::migrate(&conn)?;

    // 断言 fts_content 已回源重算为新切分产物
    let fname: String = conn.query_row(
        "SELECT file_name FROM fts_content WHERE asset_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(fname, "进 度 100%.jpg", "V3 应将旧产物回源重算为新切分");

    // 断言搜索可用（100% 命中）
    let mut got = db::search::search_asset_ids_all(&conn, "100%")?;
    got.sort();
    assert_eq!(got, vec![id]);

    // user_version 已到终版（V3 及后续迁移链全部执行）
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, LATEST_VERSION);

    // 幂等：再跑一次 migrate 无副作用
    migrations::migrate(&conn)?;
    let fname2: String = conn.query_row(
        "SELECT file_name FROM fts_content WHERE asset_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(fname2, "进 度 100%.jpg");
    let v2: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v2, LATEST_VERSION);
    Ok(())
}

// ═══════════════ ⑭ 边界断言（QA fresh-eyes 补充 ①）：纯符号 / 纯空格 / emoji 查询 ═══════════════

#[test]
fn fts_pure_symbols_no_false_positive() -> AppResult<()> {
    let conn = setup();
    add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    // 3+ 字纯符号串 → FTS∪LIKE 混合分支：不得崩溃、不得误命中普通文件
    for q in [
        "!!!",
        "@@@",
        "$$$",
        "^^^",
        "(((",
        "~~~",
        "!@#",
        "~!@#$%^&*()",
    ] {
        let got = db::search::search_asset_ids_all(&conn, q)?;
        assert!(got.is_empty(), "纯符号「{q}」应返回空；实际 {got:?}");
    }
    // 对照：符号夹在真实子串中应命中（验证符号不是被整体吞掉）
    let hit2 = add_asset(&conn, "d:/p/a_b!c.jpg", "a_b!c.jpg", "jpg", "image/jpeg");
    let got2 = db::search::search_asset_ids_all(&conn, "b!c")?;
    assert_eq!(got2, vec![hit2], "符号夹在真实子串中应命中；实际 {got2:?}");
    Ok(())
}

#[test]
fn fts_space_only_no_crash() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/a.jpg", "a.jpg", "jpg", "image/jpeg");
    // 纯空白 → trim 后为空 → 提前返回空且不崩溃
    for q in [" ", "   ", "\t", " \t \n"] {
        let got = db::search::search_asset_ids_all(&conn, q)?;
        assert!(got.is_empty(), "纯空白「{q:?}」应返回空；实际 {got:?}");
    }
    // 尾随空白应被 trim 忽略后正常命中
    let hit = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    let got = db::search::search_asset_ids_all(&conn, "海边  ")?;
    assert_eq!(got, vec![hit], "尾随空白应被 trim 后命中；实际 {got:?}");
    Ok(())
}

#[test]
fn fts_emoji_query_no_false_positive() -> AppResult<()> {
    let conn = setup();
    add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    // 纯 emoji（非 CJK，3+ 字）→ 混合分支：不得崩溃、不得误命中
    let got = db::search::search_asset_ids_all(&conn, "😀😀😀")?;
    assert!(got.is_empty(), "纯 emoji 应返回空；实际 {got:?}");
    // emoji 夹在 ASCII 中（3 字）同样不崩溃、不误命中
    let got2 = db::search::search_asset_ids_all(&conn, "a😀b")?;
    assert!(got2.is_empty(), "「a😀b」不应命中；实际 {got2:?}");
    Ok(())
}

// ═══════════════ ⑮ 边界断言（QA fresh-eyes 补充 ②）：混合查询（CJK+ASCII）路由 ═══════════════

#[test]
fn fts_mixed_cjk_ascii_routing() -> AppResult<()> {
    let conn = setup();
    let hit = add_asset(
        &conn,
        "d:/p/海边100.jpg",
        "海边100.jpg",
        "jpg",
        "image/jpeg",
    );
    let other = add_asset(&conn, "d:/p/海边10.jpg", "海边10.jpg", "jpg", "image/jpeg");
    let cjk = add_asset(
        &conn,
        "d:/p/上海公园湖边合影.jpg",
        "上海公园湖边合影.jpg",
        "jpg",
        "image/jpeg",
    );

    // 「海边100」：子串语义，仅命中 海边100.jpg（海边10.jpg 不含完整子串）
    let mut got = db::search::search_asset_ids_all(&conn, "海边100")?;
    got.sort();
    assert_eq!(got, vec![hit], "「海边100」应命中海边100.jpg；实际 {got:?}");
    // 「海边1」：两个都含子串
    let mut got1 = db::search::search_asset_ids_all(&conn, "海边1")?;
    got1.sort();
    assert_eq!(
        got1,
        vec![hit, other],
        "「海边1」应命中海边100与海边10；实际 {got1:?}"
    );
    // 「100」：纯 ASCII 3 字 → 并集，FTS token 100 与 LIKE 均命中
    let mut got100 = db::search::search_asset_ids_all(&conn, "100")?;
    got100.sort();
    assert_eq!(
        got100,
        vec![hit],
        "「100」应命中海边100.jpg；实际 {got100:?}"
    );
    // 纯 CJK 4 字走 2 字块 AND：不受混合查询影响，仍命中上海公园湖边合影
    let mut gotc = db::search::search_asset_ids_all(&conn, "上海公园")?;
    gotc.sort();
    assert_eq!(
        gotc,
        vec![cjk],
        "「上海公园」应命中上海公园湖边合影；实际 {gotc:?}"
    );
    Ok(())
}

// ═══════════════ ⑯ 边界断言（QA fresh-eyes 补充 ③）：list_ids 在 tagId 子树/组合筛选下与 list 一致 ═══════════════

#[test]
fn list_ids_matches_list_tag_subtree_and_combined() -> AppResult<()> {
    let conn = setup();
    let parent = tags::create(&conn, "父", None)?;
    let child = tags::create(&conn, "子", Some(parent.id))?;
    let a1 = add_asset(&conn, "d:/p/x1.jpg", "x1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/x2.jpg", "x2.jpg", "jpg", "image/jpeg");
    let _a3 = add_asset(&conn, "d:/p/x3.jpg", "x3.jpg", "jpg", "image/jpeg");
    // a1 挂子标签；a2 挂父标签；a3 未打标
    asset_tags::assign(&conn, &[a1], &[child.id], "manual")?;
    asset_tags::assign(&conn, &[a2], &[parent.id], "manual")?;

    let mk = |asset_type: Option<&str>,
              untagged: bool,
              tag_id: Option<i64>,
              search: Option<&str>| AssetFilter {
        asset_type: asset_type.map(String::from),
        untagged_only: untagged,
        tag_id,
        search: search.map(String::from),
        ..Default::default()
    };

    let cases: Vec<AssetFilter> = vec![
        // 父标签筛选应连带子标签素材（递归 CTE）→ a1, a2
        mk(None, false, Some(parent.id), None),
        // 子标签筛选只含 a1
        mk(None, false, Some(child.id), None),
        // 组合：父标签 + 搜索命中 a1（x1.jpg）
        mk(None, false, Some(parent.id), Some("x1")),
        // 组合：父标签 + 搜索不命中 → 空
        mk(None, false, Some(parent.id), Some("zzz")),
        // 组合：untagged_only + 搜索 → 只 a3
        mk(None, true, None, Some("x3")),
        // 组合：untagged_only + tag_id → 必然空（打标素材不可能是未打标）
        mk(None, true, Some(parent.id), None),
        // 类型 + 父标签组合
        mk(Some("image"), false, Some(parent.id), None),
    ];
    for f in cases {
        let page = assets::list(&conn, &f)?;
        let mut from_list: Vec<i64> = page.items.iter().map(|a| a.id).collect();
        let mut from_ids = assets::list_ids(&conn, &f)?;
        from_list.sort();
        from_ids.sort();
        assert_eq!(
            from_list, from_ids,
            "list_ids 与 list 的 id 集合应一致（filter={f:?}）"
        );
    }
    Ok(())
}

// ═══════════════ ⑰ V3 触发器 group_concat ORDER BY 实证（写入侧加固是否真生效） ═══════════════

#[test]
fn v3_tag_names_follow_sort_order() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/o.jpg", "o.jpg", "jpg", "image/jpeg");
    let t1 = tags::create(&conn, "甲", None)?; // id 小
    let t2 = tags::create(&conn, "乙", None)?; // id 大
    asset_tags::assign(&conn, &[id], &[t1.id, t2.id], "manual")?;

    // 注意：cjk_bigram 会再次处理 group_concat 的分隔空格，CJK 前会再插一空格
    // （如 "甲 乙" → "甲  乙"），故只断言顺序、不断言精确空格数。
    let names = || -> AppResult<String> {
        Ok(conn.query_row(
            "SELECT COALESCE(tag_names, '') FROM fts_content WHERE asset_id = ?1",
            [id],
            |r| r.get(0),
        )?)
    };
    let pos = |s: &str, c: char| s.find(c).expect("tag_names 应含该标签");
    // 默认同 sort_order=0 → 按 t.id 升序：甲 在 乙 前
    let n1 = names()?;
    assert!(
        pos(&n1, '甲') < pos(&n1, '乙'),
        "同 sort_order 应按 id 升序聚合；实际 {n1:?}"
    );
    // 把乙的 sort_order 调小，并用改名触发器强制重算（UPDATE OF name 即使值不变也会触发）
    conn.execute("UPDATE tags SET sort_order = -1 WHERE id = ?1", [t2.id])?;
    conn.execute("UPDATE tags SET name = '乙' WHERE id = ?1", [t2.id])?;
    let n2 = names()?;
    assert!(
        pos(&n2, '乙') < pos(&n2, '甲'),
        "sort_order 更小的乙应排前（ORDER BY t.sort_order, t.id）；实际 {n2:?}"
    );
    Ok(())
}

// ═══════════════ ⑱ B19：list limit 上限 1000（防一次拉全库） ═══════════════

#[test]
fn b19_list_limit_hard_cap_1000() -> AppResult<()> {
    let conn = setup();
    // 插入 1001 条（直接 DB INSERT，无文件 IO）
    for i in 0..1001 {
        add_asset(
            &conn,
            &format!("d:/p/cap_{i}.jpg"),
            &format!("cap_{i}.jpg"),
            "jpg",
            "image/jpeg",
        );
    }
    // 前端传 limit=999999 → 服务端应钳制为 1000 上限
    let page = assets::list(
        &conn,
        &AssetFilter {
            limit: 999_999,
            ..Default::default()
        },
    )?;
    assert_eq!(
        page.items.len(),
        1000,
        "list limit 应被钳制为 1000 上限，实际 {}",
        page.items.len()
    );
    assert_eq!(page.total, 1001);
    assert!(page.has_more, "1000 < 1001 应有更多");
    Ok(())
}

#[test]
fn b19_list_ids_capped_at_100000() -> AppResult<()> {
    let conn = setup();
    for i in 0..5 {
        add_asset(
            &conn,
            &format!("d:/p/ids_{i}.jpg"),
            &format!("ids_{i}.jpg"),
            "jpg",
            "image/jpeg",
        );
    }
    // list_ids 用于全选，上限 100000（远大于 5）→ 返回全部 5
    let ids = assets::list_ids(
        &conn,
        &AssetFilter {
            limit: 999_999,
            ..Default::default()
        },
    )?;
    assert_eq!(
        ids.len(),
        5,
        "list_ids 应返回全部 5 条（上限 100000 未触发）"
    );
    Ok(())
}

// ═══════════════ ⑲ B37：V2 ALTER 容错（逐列 PRAGMA table_info 检查） ═══════════════

/// 辅助：获取 assets 表的所有列名
fn asset_columns(conn: &rusqlite::Connection) -> std::collections::HashSet<String> {
    let mut stmt = conn.prepare("PRAGMA table_info(assets)").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

/// V2 的 6 个 EXIF 列
const V2_COLS: &[&str] = &["camera", "lens", "iso", "aperture", "shutter", "focal"];

#[test]
fn b37_v2_crash_recovery_all_columns_present() -> AppResult<()> {
    // 场景：V2 迁移中途崩溃（列已全部添加但 user_version 未提交为 2）
    // 重启后 version 仍为 1 → 重跑 V2 → migrate_v2 跳过已存在列 → 不 panic
    let conn = setup(); // version=3, 全部列存在
                        // 回退 version 到 1（模拟崩溃：列已加但 version 未提交）
    conn.pragma_update(None, "user_version", 1)?;
    // 重新迁移
    migrations::migrate(&conn)?;
    // version 应到终版
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, LATEST_VERSION);
    // 所有 V2 列仍在
    let cols = asset_columns(&conn);
    for c in V2_COLS {
        assert!(cols.contains(*c), "列 {c} 应存在");
    }
    // 库仍可用
    add_asset(
        &conn,
        "d:/p/recover.jpg",
        "recover.jpg",
        "jpg",
        "image/jpeg",
    );
    Ok(())
}

#[test]
fn b37_v2_partial_columns_recovery() -> AppResult<()> {
    // 场景：V2 迁移只加了一部分列就崩溃（如断电）
    // 重启后 version=1 + 部分列已存在 → migrate_v2 只添加缺失列
    let conn = setup(); // version=3, 全部列存在
                        // 模拟部分迁移：先回退 version，再删掉 3 个列
    conn.pragma_update(None, "user_version", 1)?;
    conn.execute("ALTER TABLE assets DROP COLUMN lens", [])?;
    conn.execute("ALTER TABLE assets DROP COLUMN aperture", [])?;
    conn.execute("ALTER TABLE assets DROP COLUMN focal", [])?;
    // 验证删除生效
    let cols_before = asset_columns(&conn);
    assert!(!cols_before.contains("lens"), "lens 应已被删除");
    assert!(!cols_before.contains("aperture"));
    assert!(!cols_before.contains("focal"));
    assert!(cols_before.contains("camera"), "camera 应仍在");
    assert!(cols_before.contains("iso"));
    assert!(cols_before.contains("shutter"));

    // 重新迁移：migrate_v2 应补回缺失的 3 列
    migrations::migrate(&conn)?;

    let cols_after = asset_columns(&conn);
    for c in V2_COLS {
        assert!(cols_after.contains(*c), "迁移后列 {c} 应存在");
    }
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, LATEST_VERSION);
    Ok(())
}

#[test]
fn b37_fresh_install_all_v2_columns() -> AppResult<()> {
    // 场景：全新安装 → V1→V2（全部 ALTER）→ V3…终版
    let conn = db::init_memory()?;
    let cols = asset_columns(&conn);
    for c in V2_COLS {
        assert!(cols.contains(*c), "全新安装后列 {c} 应存在");
    }
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, LATEST_VERSION);
    Ok(())
}

// ═══════════════ ⑳ B20：confirm_all_pending 分页确认 ═══════════════

fn mk_categorized_tags() -> CategorizedTags {
    let mut m = std::collections::BTreeMap::new();
    m.insert("风景".to_string(), vec!["海边".to_string()]);
    m
}

#[test]
fn b20_confirm_all_pending_success() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/b20_1.jpg", "b20_1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/b20_2.jpg", "b20_2.jpg", "jpg", "image/jpeg");
    let a3 = add_asset(&conn, "d:/p/b20_3.jpg", "b20_3.jpg", "jpg", "image/jpeg");

    // 创建批次（3 条 pending 建议）
    let batch = ai::create_batch(&conn, &[a1, a2, a3], "cloud")?;
    let suggs = ai::list_suggestions(&conn, batch.id)?;
    assert_eq!(suggs.len(), 3);
    assert!(suggs.iter().all(|s| s.status == "pending"));

    // 设置 AI 建议标签
    let tags = mk_categorized_tags();
    for s in &suggs {
        ai::set_suggestion_tags(&conn, s.id, &tags)?;
    }

    // 批量确认
    ai::confirm_all_pending(&conn, batch.id)?;

    // 验证：全部 confirmed
    let after = ai::list_suggestions(&conn, batch.id)?;
    assert_eq!(after.len(), 3);
    assert!(
        after.iter().all(|s| s.status == "confirmed"),
        "所有建议应已确认"
    );
    // 批次计数
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.confirmed, 3);
    // asset_tags 已写入（每条 1 个标签 → 3 条关联）
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 3);
    Ok(())
}

#[test]
fn b20_confirm_all_pending_no_pending_is_noop() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/b20_np.jpg", "b20_np.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    // 先确认单条
    let suggs = ai::list_suggestions(&conn, batch.id)?;
    ai::confirm_suggestion(&conn, suggs[0].id, &mk_categorized_tags())?;
    // 再调 confirm_all_pending（无 pending）
    ai::confirm_all_pending(&conn, batch.id)?;
    // 不应有异常，confirmed 仍为 1
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.confirmed, 1);
    Ok(())
}

// B-2：确认标签或纯描述；完全空的历史占位项不确认、不虚增计数。
#[test]
fn b21_confirm_all_pending_skips_empty_tags() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/b21_1.jpg", "b21_1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/b21_2.jpg", "b21_2.jpg", "jpg", "image/jpeg");
    let a3 = add_asset(&conn, "d:/p/b21_3.jpg", "b21_3.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[a1, a2, a3], "cloud")?;
    let suggs = ai::list_suggestions(&conn, batch.id)?;
    assert_eq!(suggs.len(), 3);
    // 第 1 条设非空标签；第 2 条只有描述；第 3 条保持完全为空
    ai::set_suggestion_tags(&conn, suggs[0].id, &mk_categorized_tags())?;
    conn.execute(
        "UPDATE ai_suggestions SET suggested_description = ?1 WHERE id = ?2",
        rusqlite::params!["纯描述建议也可人工确认", suggs[1].id],
    )?;

    ai::confirm_all_pending(&conn, batch.id)?;

    let after = ai::list_suggestions(&conn, batch.id)?;
    let confirmed = after.iter().filter(|s| s.status == "confirmed").count();
    let pending_empty = after
        .iter()
        .filter(|s| s.status == "pending" && s.suggested_tags.is_empty())
        .count();
    assert_eq!(confirmed, 2, "标签建议和纯描述建议都应确认");
    assert_eq!(pending_empty, 1, "完全空建议应保持 pending 不被误确认");
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.confirmed, 2, "完全空建议不应虚增 confirmed 计数");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM asset_tags"), 1);
    let desc: String = conn.query_row(
        "SELECT content_description FROM assets WHERE id = ?1",
        [a2],
        |r| r.get(0),
    )?;
    assert_eq!(desc, "纯描述建议也可人工确认");
    Ok(())
}

// ═══════════════ ㉑ B27：清缓存回写 DB（placeholder/hd path → NULL） ═══════════════

#[test]
fn b27_clear_all_thumbnail_paths_writes_null() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/b27.jpg", "b27.jpg", "jpg", "image/jpeg");
    // 模拟有缩略图路径
    assets::set_placeholder_path(&conn, id, "d:/data/thumbnails/placeholder/1.webp")?;
    assets::set_hd_thumbnail_path(&conn, id, "d:/data/thumbnails/hd/1_512.webp")?;
    let a = assets::get(&conn, id)?;
    assert!(a.placeholder_path.is_some());
    assert!(a.hd_thumbnail_path.is_some());

    // B27：清缓存回写 NULL
    assets::clear_all_placeholder_paths(&conn)?;
    assets::clear_all_hd_thumbnail_paths(&conn)?;

    let a2 = assets::get(&conn, id)?;
    assert!(
        a2.placeholder_path.is_none(),
        "清缓存后 placeholder_path 应为 NULL"
    );
    assert!(
        a2.hd_thumbnail_path.is_none(),
        "清缓存后 hd_thumbnail_path 应为 NULL"
    );
    Ok(())
}

/// W1-4 真机验收：对真实库的 205 张 RW2 跑宽高回填（需真机样本；库为开发者本机库才跑）。
/// 直接调 media_refill::rescan_assets_dimensions（与 rescan_image_dimensions 命令同一路径）。
#[test]
#[ignore = "真机验收：写真实库，仅手动跑（cargo test -- --ignored）"]
fn w1_refill_real_library_rw2_dimensions() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    let db_path = std::path::PathBuf::from(
        std::env::var("APPDATA").unwrap().replace('\\', "/") + "/bagertea_ai_media_v2/library.db",
    );
    if !db_path.exists() {
        eprintln!("跳过：真机库不存在");
        return;
    }
    let conn = rusqlite::Connection::open(&db_path).expect("打开真机库");
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE lower(file_ext)='rw2' AND width IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("RW2 宽高缺失（回填前）: {before}");
    drop(conn);

    let db = Arc::new(Database::new(rusqlite::Connection::open(&db_path).unwrap()));
    let cancel = AtomicBool::new(false);
    let ids = {
        let c = db.lock().unwrap();
        teacell_ai_media_manager_lib::db::assets::list_ids_needing_dimensions(&c).unwrap()
    };
    println!("候选 id 数: {}", ids.len());
    let start = std::time::Instant::now();
    let summary = teacell_ai_media_manager_lib::services::media_refill::rescan_assets_dimensions(
        &db,
        &ids,
        &cancel,
        |p| {
            if p.done % 20 == 0 {
                println!("进度 {}/{}", p.done, p.total);
            }
        },
    )
    .unwrap();
    println!(
        "回填完成：total={} success={} failed={} skipped={} 耗时 {:?}",
        summary.total,
        summary.success,
        summary.failed,
        summary.skipped,
        start.elapsed()
    );
    let c = db.lock().unwrap();
    let after: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE lower(file_ext)='rw2' AND width IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("RW2 宽高缺失（回填后）: {after}");
    assert_eq!(after, 0, "回填后 RW2 宽高缺失应为 0");
}
