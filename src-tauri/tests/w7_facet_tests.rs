//! W7-1 Rust 单测补齐（指导书 §W7-1）：高优模块关键路径。
//! 覆盖：tags（normalize_name / find_or_create_canonical 别名命中 / merge 三道防护）、
//! tag_ops undo 四条规则（D-2/D-3/D-4/D-5）、resolve_facet_key 三分支、
//! asset_tags 手工覆盖清 source_batch_id + W5h 同源同步与 remove 对称。
//! 运行：cargo test --test w7_facet_tests

use teacell_ai_media_manager_lib::db::{
    self, asset_tags, assets, settings, tag_facets, tag_ops, tags,
};

fn setup() -> rusqlite::Connection {
    db::init_memory().expect("内存库初始化失败")
}

fn add_asset(conn: &rusqlite::Connection, path: &str) -> i64 {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    let ext = name.rsplit('.').next().unwrap_or("").to_string();
    let mime = if ext.eq_ignore_ascii_case("mp4") {
        "video/mp4"
    } else {
        "image/jpeg"
    };
    let id =
        assets::insert(conn, path, &name, &ext, 1024, mime, 1700000000000).expect("插入素材失败");
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

/// 建一个参与 AI 的自建分面 + 标签，返回 (facet_key, tag_id)
fn make_facet_tag(conn: &rusqlite::Connection, key: &str) -> (String, i64) {
    tag_facets::create(conn, key, key, "测试分面", "multi", Some(3), "all").unwrap();
    let tag = tags::create_in_facet(conn, key, None, Some(key)).unwrap();
    (key.to_string(), tag.id)
}

// ═══════════════ ① tags：normalize_name 全角转换 ═══════════════

#[test]
fn normalize_name_fullwidth_and_case() {
    // 全角字母/数字 → 半角小写
    assert_eq!(tags::normalize_name("ＡＢＣ"), "abc");
    assert_eq!(tags::normalize_name("ａｂｃ"), "abc");
    assert_eq!(tags::normalize_name("０１２"), "012");
    // 全角空格（U+3000）→ 普通空格，连续空白折叠为单个
    assert_eq!(tags::normalize_name("　夜晚　树下　"), "夜晚 树下");
    // 大小写折叠（英文）
    assert_eq!(tags::normalize_name("Sunset"), "sunset");
    // 中文原样保留
    assert_eq!(tags::normalize_name("  海边 日落 "), "海边 日落");
}

// ═══════════════ ② tags：find_or_create_canonical 别名命中 ═══════════════

#[test]
fn canonical_hits_alias() {
    let conn = setup();
    let (facet, _) = make_facet_tag(&conn, "w7scene");
    let tag = tags::create_in_facet(&conn, "夜景", None, Some(&facet)).unwrap();
    tags::add_alias(&conn, tag.id, "夜晚", None, "synonym").unwrap();
    // 按规范名命中
    let by_name = tags::find_or_create_canonical(&conn, &facet, "夜景").unwrap();
    assert_eq!(by_name, tag.id);
    // 按别名命中（不新建）
    let before = count(&conn, "SELECT COUNT(*) FROM tags");
    let by_alias = tags::find_or_create_canonical(&conn, &facet, "夜晚").unwrap();
    assert_eq!(by_alias, tag.id);
    let after = count(&conn, "SELECT COUNT(*) FROM tags");
    assert_eq!(after, before, "别名命中不应新建标签");
    // 新名则创建
    let created = tags::find_or_create_canonical(&conn, &facet, "黄昏").unwrap();
    assert_ne!(created, tag.id);
}

// ═══════════════ ③ tags：merge_preserve_alias 三道防护 ═══════════════

#[test]
fn merge_guardrails() {
    let conn = setup();
    let (facet, a) = make_facet_tag(&conn, "w7mood");
    let b = tags::create_in_facet(&conn, "B", None, Some(&facet))
        .unwrap()
        .id;
    let c = tags::create_in_facet(&conn, "C", None, Some(&facet))
        .unwrap()
        .id;
    // 自合并
    assert!(tags::merge_preserve_alias(&conn, a, a).is_err());
    // 跨分面
    let (facet2, d) = make_facet_tag(&conn, "w7style");
    let _ = facet2;
    assert!(tags::merge_preserve_alias(&conn, a, d).is_err());
    // 后代环：b 挂到 a 下后，把 a 合并进 b（b 是 a 的后代）
    tags::update(&conn, b, None, Some(Some(a))).unwrap();
    assert!(
        tags::merge_preserve_alias(&conn, a, b).is_err(),
        "不能合并到自己的子标签"
    );
    let _ = c;
}

// ═══════════════ ④ tag_ops undo 四条规则（D-2/D-3/D-4/D-5） ═══════════════

/// D-3：只删「当前批次写入且非手工」的关联；D-4：重复撤销幂等；D-5：remove 重插不写 batch。
#[test]
fn undo_batch_rules() {
    let conn = setup();
    let (_facet, tag) = make_facet_tag(&conn, "w7scene");
    let a1 = add_asset(&conn, "d:/p/a.jpg");
    let a2 = add_asset(&conn, "d:/p/b.jpg");
    let a3 = add_asset(&conn, "d:/p/c.jpg");

    // 造批次与流水：batch=77 含 add(a1) + remove(a2)
    conn.execute(
        "INSERT INTO ai_batches (status, mode, total, created_at) VALUES ('done', 'auto', 2, 1)",
        [],
    )
    .unwrap();
    let batch_id = conn
        .query_row(
            "SELECT id FROM ai_batches ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO tag_ops (batch_id, asset_id, tag_id, op, actor, created_at)
         VALUES (?1, ?2, ?3, 'add', 'ai', 1), (?1, ?4, ?3, 'remove', 'manual', 2)",
        rusqlite::params![batch_id, a1, tag, a2],
    )
    .unwrap();
    // 关联行：a1 = AI 批次写入（会被删）；a3 = 手工确认（必须保留）；a2 无行（remove 撤销重插）
    conn.execute(
        "INSERT INTO asset_tags (asset_id, tag_id, source, source_batch_id, created_at, confirmation)
         VALUES (?1, ?2, 'ai', ?3, 1, 'confirmed')",
        rusqlite::params![a1, tag, batch_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO asset_tags (asset_id, tag_id, source, created_at, confirmation)
         VALUES (?1, ?2, 'manual', 1, 'confirmed')",
        rusqlite::params![a3, tag],
    )
    .unwrap();

    let n = tag_ops::undo_batch(&conn, batch_id).unwrap();
    // D-3：add 删批次关联（1）；D-5：remove 重插 a2（1）→ 共 2
    assert_eq!(n, 2, "D-3/D-5：add 删批次关联 + remove 重插");
    // a1 的批次关联被删
    let a1_gone: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE asset_id=?1 AND tag_id=?2 AND source_batch_id=?3",
            rusqlite::params![a1, tag, batch_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(a1_gone, 0, "D-3：本批次写入的 AI 关联应被撤销");
    // a3 的手工关联保留
    let a3_kept: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={a3} AND tag_id={tag}"),
    );
    assert_eq!(a3_kept, 1, "D-3：手工确认的关联不能被 undo 删除");
    // D-5：remove 撤销后 a2 被重插，且不带 source_batch_id
    let a2_row: Option<i64> = conn
        .query_row(
            "SELECT source_batch_id FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
            rusqlite::params![a2, tag],
            |r| r.get(0),
        )
        .unwrap();
    assert!(a2_row.is_none(), "D-5：重插的关联不带 source_batch_id");
    // D-4：重复撤销幂等返回 0
    assert_eq!(tag_ops::undo_batch(&conn, batch_id).unwrap(), 0);
}

/// D-1/D-2：手工覆盖清 source_batch_id —— 撤销 AI 批次不会误删手工确认后的标签
#[test]
fn manual_override_clears_source_batch() {
    let conn = setup();
    let (_facet, tag) = make_facet_tag(&conn, "w7scene");
    let a1 = add_asset(&conn, "d:/p/a.jpg");
    // AI 写入（带 batch）
    conn.execute(
        "INSERT INTO ai_batches (status, mode, total, created_at) VALUES ('done', 'auto', 1, 1)",
        [],
    )
    .unwrap();
    let batch_id = conn
        .query_row(
            "SELECT id FROM ai_batches ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    asset_tags::assign(&conn, &[a1], &[tag], "ai").unwrap();
    conn.execute(
        "UPDATE asset_tags SET source_batch_id=?1, source='ai' WHERE asset_id=?2 AND tag_id=?3",
        rusqlite::params![batch_id, a1, tag],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tag_ops (batch_id, asset_id, tag_id, op, actor, created_at) VALUES (?1, ?2, ?3, 'add', 'ai', 1)",
        rusqlite::params![batch_id, a1, tag],
    )
    .unwrap();
    // 手工覆盖：走 assign_inner 的 manual 分支 → 清 source_batch_id
    asset_tags::assign(&conn, &[a1], &[tag], "manual").unwrap();
    let batch_after: Option<i64> = conn
        .query_row(
            "SELECT source_batch_id FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
            rusqlite::params![a1, tag],
            |r| r.get(0),
        )
        .unwrap();
    assert!(batch_after.is_none(), "D-2：手工覆盖必须清 source_batch_id");
    // 撤销批次 → 不应删除该关联（source 已是 manual 且 batch 已清）
    assert_eq!(
        tag_ops::undo_batch(&conn, batch_id).unwrap(),
        0,
        "D-1：撤销不误删手工标签"
    );
    let kept: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={a1} AND tag_id={tag}"),
    );
    assert_eq!(kept, 1);
}

// ═══════════════ ⑤ resolve_facet_key 三分支 ═══════════════

#[test]
fn resolve_facet_key_three_branches() {
    let conn = setup();
    // ① DB 直存：自建分面原样返回
    let (facet, _) = make_facet_tag(&conn, "w7mood");
    let (k1, d1) = tag_facets::resolve_facet_key(&conn, "w7mood").unwrap();
    assert_eq!((k1.as_str(), d1.as_str()), ("w7mood", "w7mood"));
    let _ = facet;
    // ② 中文旧名映射（系统分面 scene 存在）
    let (k2, d2) = tag_facets::resolve_facet_key(&conn, "场景").unwrap();
    assert_eq!((k2.as_str(), d2.as_str()), ("scene", "scene"));
    // ③ 未知 key → custom
    let (k3, d3) = tag_facets::resolve_facet_key(&conn, "不存在的分面").unwrap();
    assert_eq!((k3.as_str(), d3.as_str()), ("custom", "custom"));
    // 自建分面不会被旧名表覆盖
    let (k4, _) = tag_facets::resolve_facet_key(&conn, "w7mood").unwrap();
    assert_eq!(k4, "w7mood");
}

// ═══════════════ ⑥ asset_tags 同源同步（W5h）与 remove 对称 ═══════════════

/// 开同源同步：assign 到 JPG 时 RAW 也写入；remove 对称摘除。
#[test]
fn kinship_sync_assign_and_remove_symmetric() {
    let conn = setup();
    let (_facet, tag) = make_facet_tag(&conn, "w7scene");
    let jpg = add_asset(&conn, "d:/p/DSC0001.JPG");
    let raw = add_asset(&conn, "d:/p/DSC0001.RW2");

    // 默认开关开（kinship.sync_tags_to_siblings = true）
    asset_tags::assign(&conn, &[jpg], &[tag], "manual").unwrap();
    let raw_has: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={raw} AND tag_id={tag}"),
    );
    assert_eq!(raw_has, 1, "同源 RAW 应同步打标");

    // remove 对称摘除
    asset_tags::remove(&conn, &[jpg], &[tag]).unwrap();
    let jpg_has: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={jpg} AND tag_id={tag}"),
    );
    let raw_has: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={raw} AND tag_id={tag}"),
    );
    assert_eq!(jpg_has, 0);
    assert_eq!(raw_has, 0, "remove 应同步摘除同源 RAW");
}

/// 关闭同源同步：JPG/RAW 独立打标（浏览层独立显示的落点）
#[test]
fn kinship_sync_disabled_keeps_independent() {
    let conn = setup();
    let (_facet, tag) = make_facet_tag(&conn, "w7scene");
    let jpg = add_asset(&conn, "d:/p/DSC0001.JPG");
    let raw = add_asset(&conn, "d:/p/DSC0001.RW2");
    // 关开关
    let mut s = settings::get_settings(&conn).unwrap();
    s.appearance.kinship.sync_tags_to_siblings = false;
    settings::save_settings(&conn, &s).unwrap();

    asset_tags::assign(&conn, &[jpg], &[tag], "manual").unwrap();
    let raw_has: i64 = count(
        &conn,
        &format!("SELECT COUNT(*) FROM asset_tags WHERE asset_id={raw} AND tag_id={tag}"),
    );
    assert_eq!(raw_has, 0, "关闭同步后 RAW 不应被打标");
}
