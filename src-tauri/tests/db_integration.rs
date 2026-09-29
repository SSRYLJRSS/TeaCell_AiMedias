//! T02 验收测试：迁移/触发器/搜索/标签树/分页/设置/AI 确认流
//! 运行：cargo test

use bagertea_ai_media_v2_lib::db::assets::AssetFilter;
use bagertea_ai_media_v2_lib::db::{
    self, ai, asset_tags, assets, dedup, migrations, settings, tag_facets, tag_ops, tags,
};
use bagertea_ai_media_v2_lib::error::AppResult;

fn setup() -> rusqlite::Connection {
    db::init_memory().expect("内存库初始化失败")
}

fn add_asset(conn: &rusqlite::Connection, path: &str, name: &str, ext: &str, mime: &str) -> i64 {
    assets::insert(conn, path, name, ext, 1024, mime, 1700000000000).expect("插入素材失败")
}

#[test]
fn core_taxonomy_seed_is_idempotent_and_new_words_stay_at_facet_root() -> AppResult<()> {
    let conn = setup();
    db::ensure_default_taxonomy(&conn)?;
    let first_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
    tags::seed_core_taxonomy(&conn)?;
    let second_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
    assert_eq!(first_count, second_count, "重复播种不得新增重复标签");

    let parent: String = conn.query_row(
        "SELECT p.name FROM tags t JOIN tags p ON p.id = t.parent_id
          WHERE t.facet_key = 'people' AND t.name = '女性'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(parent, "性别");
    assert!(tags::search_candidates(&conn, Some("subject"), "人像")?
        .iter()
        .any(|tag| tag.name == "人"));

    let new_tag_id = tags::find_or_create_canonical(&conn, "people", "赛博朋克少女")?;
    let (new_facet, new_parent): (String, Option<i64>) = conn.query_row(
        "SELECT facet_key, parent_id FROM tags WHERE id = ?1",
        [new_tag_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(new_facet, "people");
    assert_eq!(new_parent, None, "新词默认保留在所选分面的根级");

    assert_eq!(tag_facets::get(&conn, "subject")?.max_items, Some(3));
    assert_eq!(tag_facets::get(&conn, "people")?.max_items, Some(8));
    Ok(())
}

// ① 迁移 + 触发器：插入素材即可被 FTS 检索（文件名逐字切分）
#[test]
fn fts_index_on_insert() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    let ids = db::search::search_asset_ids_all(&conn, "海边日落")?;
    assert_eq!(ids, vec![id]);
    Ok(())
}

// ② 词中子串可查：「日落」命中「海边日落.jpg」
#[test]
fn fts_substring_hit() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    assert_eq!(db::search::search_asset_ids_all(&conn, "日落")?, vec![id]);
    Ok(())
}

// ③ 短语精确性：搜「海边」不得命中「上海公园湖边合影.jpg」
#[test]
fn fts_phrase_no_false_positive() -> AppResult<()> {
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
    assert_eq!(db::search::search_asset_ids_all(&conn, "海边")?, vec![hit]);
    Ok(())
}

// ④ ≤2 字 LIKE 兜底 + 按标签名搜索
#[test]
fn like_fallback_and_tag_search() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create(&conn, "海边", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    // 按标签名（2 字 → LIKE 走 EXISTS 子查询）
    assert_eq!(db::search::search_asset_ids_all(&conn, "海边")?, vec![id]);
    // 按文件名（2 字 ASCII）
    assert_eq!(db::search::search_asset_ids_all(&conn, "01")?, vec![id]);
    Ok(())
}

// ⑤ 摘除标签后无幻影命中（触发器 delete 顺序正确性）
#[test]
fn no_phantom_after_tag_removal() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/photo001.jpg",
        "photo001.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create(&conn, "山野", None)?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "山野")?, vec![id]);
    asset_tags::remove(&conn, &[id], &[tag.id])?;
    assert!(
        db::search::search_asset_ids_all(&conn, "山野")?.is_empty(),
        "摘除标签后仍命中（幻影）"
    );
    Ok(())
}

// ⑥ 文件改名索引同步
#[test]
fn rename_updates_index() -> AppResult<()> {
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

// ⑦ 删除素材后索引清除（CASCADE + 触发器）
#[test]
fn delete_asset_clears_index() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/海边日落.jpg",
        "海边日落.jpg",
        "jpg",
        "image/jpeg",
    );
    assets::delete(&conn, &[id])?;
    assert!(db::search::search_asset_ids_all(&conn, "海边日落")?.is_empty());
    Ok(())
}

// ⑧ 标签树：父子层级 + 连带计数（去重）
#[test]
fn tag_tree_counts() -> AppResult<()> {
    let conn = setup();
    let parent = tags::create(&conn, "人像", None)?;
    let child = tags::create(&conn, "宠物", Some(parent.id))?;
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[a1], &[parent.id], "manual")?;
    asset_tags::assign(&conn, &[a1, a2], &[child.id], "manual")?; // a1 同时挂父子

    let tree = tags::list_tree(&conn)?;
    let p = tree
        .iter()
        .find(|n| n.tag.id == parent.id)
        .expect("父标签缺失");
    assert_eq!(p.tag.asset_count, 1); // 自身直接关联
    assert_eq!(p.tag.total_count, 2); // a1+a2 去重合计
    assert_eq!(p.children.len(), 1);
    assert_eq!(p.children[0].tag.total_count, 2);
    Ok(())
}

// ⑨ 挂载防环
#[test]
fn tag_reparent_cycle_rejected() -> AppResult<()> {
    let conn = setup();
    let parent = tags::create(&conn, "人像", None)?;
    let child = tags::create(&conn, "宠物", Some(parent.id))?;
    let r = tags::update(&conn, parent.id, None, Some(Some(child.id)));
    assert!(r.is_err(), "把父标签挂到自己子标签下应被拒绝");
    Ok(())
}

// ⑩ 标签合并（M3-01）：关联改挂去重 + 子标签回挂 + FTS 联动
#[test]
fn tag_merge_moves_assets_and_children() -> AppResult<()> {
    let conn = setup();
    let src = tags::create(&conn, "海边", None)?;
    let dst = tags::create(&conn, "风景", None)?;
    let src_child = tags::create(&conn, "沙滩", Some(src.id))?;
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[a1, a2], &[src.id], "manual")?;
    asset_tags::assign(&conn, &[a1], &[dst.id], "manual")?; // a1 同时挂两边 → 合并后去重

    tags::merge(&conn, src.id, dst.id)?;

    // src 已删除；子标签回挂 dst
    let tree = tags::list_tree(&conn)?;
    assert!(tree.iter().all(|n| n.tag.id != src.id), "源标签应被删除");
    let d = tree
        .iter()
        .find(|n| n.tag.id == dst.id)
        .expect("目标标签缺失");
    assert_eq!(d.tag.asset_count, 2);
    assert_eq!(d.children.len(), 1);
    assert_eq!(d.children[0].tag.id, src_child.id);
    // FTS 按目标标签名仍可搜
    assert_eq!(db::search::search_asset_ids_all(&conn, "风景")?.len(), 2);
    assert!(
        db::search::search_asset_ids_all(&conn, "海边")?.is_empty(),
        "源标签名不应再命中"
    );
    Ok(())
}

// ⑪ 合并防环：不能合并到自己的子标签下
#[test]
fn tag_merge_into_descendant_rejected() -> AppResult<()> {
    let conn = setup();
    let parent = tags::create(&conn, "人像", None)?;
    let child = tags::create(&conn, "自拍", Some(parent.id))?;
    assert!(
        tags::merge(&conn, parent.id, parent.id).is_err(),
        "自合并应被拒绝"
    );
    assert!(
        tags::merge(&conn, parent.id, child.id).is_err(),
        "合并到自己的子标签下应被拒绝"
    );
    Ok(())
}

// ⑫ 重复素材扫描（M3-02）：hash 分组 + 最早优先排序
#[test]
fn dedup_scan_groups_by_hash() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    let a3 = add_asset(&conn, "d:/p/a3.jpg", "a3.jpg", "jpg", "image/jpeg");
    // a1/a2 同 hash 重复；a3 独立；a2 入库更晚（insert 固定 created_at，用 id 升序兼验排序稳定性）
    assets::set_hash(&conn, a1, "h_same")?;
    assets::set_hash(&conn, a2, "h_same")?;
    assets::set_hash(&conn, a3, "h_other")?;

    let groups = dedup::scan_groups(&conn)?;
    assert_eq!(groups.len(), 1, "只应有一组重复");
    assert_eq!(groups[0].hash, "h_same");
    assert_eq!(
        groups[0].assets.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![a1, a2]
    );
    Ok(())
}

// ⑩ 分页 + 筛选（类型/未打标/标签连带）
#[test]
fn assets_pagination_and_filters() -> AppResult<()> {
    let conn = setup();
    for i in 0..5 {
        add_asset(
            &conn,
            &format!("d:/p/img{i}.jpg"),
            &format!("img{i}.jpg"),
            "jpg",
            "image/jpeg",
        );
    }
    add_asset(&conn, "d:/p/v0.mp4", "v0.mp4", "mp4", "video/mp4");

    let page1 = assets::list(
        &conn,
        &AssetFilter {
            limit: 4,
            ..Default::default()
        },
    )?;
    assert_eq!(page1.total, 6);
    assert_eq!(page1.items.len(), 4);
    assert!(page1.has_more);
    let page2 = assets::list(
        &conn,
        &AssetFilter {
            limit: 4,
            offset: 4,
            ..Default::default()
        },
    )?;
    assert_eq!(page2.items.len(), 2);
    assert!(!page2.has_more);

    let videos = assets::list(
        &conn,
        &AssetFilter {
            asset_type: Some("video".into()),
            ..Default::default()
        },
    )?;
    assert_eq!(videos.total, 1);

    let untagged = assets::list(
        &conn,
        &AssetFilter {
            untagged_only: true,
            ..Default::default()
        },
    )?;
    assert_eq!(untagged.total, 6);

    // 标签连带筛选：父标签应捞出子标签素材
    let parent = tags::create(&conn, "风景", None)?;
    let child = tags::create(&conn, "海边", Some(parent.id))?;
    let pid = conn.query_row(
        "SELECT id FROM assets WHERE file_ext='jpg' LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    asset_tags::assign(&conn, &[pid], &[child.id], "manual")?;
    let filtered = assets::list(
        &conn,
        &AssetFilter {
            tag_id: Some(parent.id),
            ..Default::default()
        },
    )?;
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.items[0].tags.len(), 1);
    Ok(())
}

// ⑪ 设置读写回环 + 默认值
#[test]
fn settings_roundtrip() -> AppResult<()> {
    let conn = setup();
    let s = settings::get_settings(&conn)?;
    assert_eq!(s.theme, "system");
    assert_eq!(s.thumbnail_cache_mb, 2048);
    assert!(!s.ai.auto_tagging);
    let mut s2 = settings::Settings::default();
    s2.ai.profiles.push(settings::ApiProfile {
        id: "p1".into(),
        name: "中转 A".into(),
        api_mode: "openai".into(),
        kind: "cloud".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: "sk-test".into(),
        model: "qwen-vl-plus".into(),
        max_concurrency: 0,
        requests_per_minute: 0,
        requests_per_hour: 0,
    });
    s2.ai.active_profile = "p1".into();
    s2.theme = "dark".into();
    settings::save_settings(&conn, &s2)?;
    let got = settings::get_settings(&conn)?;
    let active = got.ai.active().expect("应有激活档案");
    assert_eq!(active.base_url, "https://api.example.com/v1");
    assert_eq!(active.name, "中转 A");
    assert_eq!(active.kind, "cloud");
    assert_eq!(got.theme, "dark");
    Ok(())
}

// ⑫ AI 确认流：确认才写 asset_tags + 来源标记 + 可被搜索
#[test]
fn ai_confirm_flow() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p1.jpg", "p1.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    assert_eq!(batch.total, 1);
    let sugg = ai::list_suggestions(&conn, batch.id)?.remove(0);
    assert_eq!(sugg.status, "pending");
    assert_eq!(sugg.asset_path, "d:/p/p1.jpg");

    // 未确认 → 未打标
    let before = assets::list(
        &conn,
        &AssetFilter {
            untagged_only: true,
            ..Default::default()
        },
    )?;
    assert_eq!(before.total, 1);

    let tags_map = ai::CategorizedTags::from([(
        "场景".to_string(),
        vec!["夜景".to_string(), "城市".to_string()],
    )]);
    ai::set_suggestion_tags(&conn, sugg.id, &tags_map)?;
    let item_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_suggestion_items WHERE suggestion_id = ?1",
        [sugg.id],
        |r| r.get(0),
    )?;
    assert_eq!(item_count, 2);
    ai::confirm_suggestion(&conn, sugg.id, &tags_map)?;
    // 分类=父标签：「场景」应为根标签，「夜景」挂其下
    let tree = tags::list_tree(&conn)?;
    let scene = tree
        .iter()
        .find(|n| n.tag.name == "场景")
        .expect("应有分类父标签");
    assert!(scene.children.iter().any(|c| c.tag.name == "夜景"));

    let after = assets::list(
        &conn,
        &AssetFilter {
            untagged_only: true,
            ..Default::default()
        },
    )?;
    assert_eq!(after.total, 0);
    // 确认后新标签可被检索
    assert_eq!(db::search::search_asset_ids_all(&conn, "夜景")?, vec![id]);
    // 来源标记
    let src: String = conn.query_row(
        "SELECT source FROM asset_tags WHERE asset_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(src, "ai_cloud");
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.confirmed, 1);
    Ok(())
}

// 阶段5 §8.2：应用重启/中断时，把遗留 processing 批次标记为 interrupted（可一键续跑）
#[test]
fn mark_interrupted_batches_flags_processing() -> AppResult<()> {
    let conn = setup();
    let asset = add_asset(&conn, "d:/p/x.jpg", "x.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[asset], "cloud")?;
    // 模拟正在执行中
    ai::set_batch_status(&conn, batch.id, "processing")?;
    ai::mark_interrupted_batches(&conn)?;
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.status, "interrupted");
    // 再次调用幂等
    ai::mark_interrupted_batches(&conn)?;
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.status, "interrupted");
    // done 批次不受影响
    let done_batch = ai::create_batch(&conn, &[asset], "cloud")?;
    ai::set_batch_status(&conn, done_batch.id, "done")?;
    ai::mark_interrupted_batches(&conn)?;
    assert_eq!(ai::get_batch(&conn, done_batch.id)?.status, "done");
    Ok(())
}

// ⑬ 预置标签播种（幂等）
#[test]
fn seed_presets_idempotent() -> AppResult<()> {
    let conn = setup();
    tags::seed_presets(&conn)?;
    tags::seed_presets(&conn)?;
    let tree = tags::list_tree(&conn)?;
    assert!(tree.is_empty(), "新库不应自动创建旧版预置标签");
    Ok(())
}

#[test]
fn unused_legacy_presets_are_retired_but_used_ones_remain() -> AppResult<()> {
    let conn = setup();
    let unused = tags::create_in_facet(&conn, "风景", None, Some("scene"))?;
    conn.execute("UPDATE tags SET is_preset = 1 WHERE id = ?1", [unused.id])?;
    let asset = add_asset(
        &conn,
        "d:/p/used-preset.jpg",
        "used-preset.jpg",
        "jpg",
        "image/jpeg",
    );
    let used = tags::create_in_facet(&conn, "美食", None, Some("subject"))?;
    conn.execute("UPDATE tags SET is_preset = 1 WHERE id = ?1", [used.id])?;
    asset_tags::assign(&conn, &[asset], &[used.id], "manual")?;

    let changed = tags::retire_unused_presets(&conn)?;
    assert_eq!(changed, 1);
    let unused_status: String =
        conn.query_row("SELECT status FROM tags WHERE id = ?1", [unused.id], |r| {
            r.get(0)
        })?;
    let used_status: String =
        conn.query_row("SELECT status FROM tags WHERE id = ?1", [used.id], |r| {
            r.get(0)
        })?;
    assert_eq!(unused_status, "deprecated");
    assert_eq!(used_status, "active");
    Ok(())
}

// ⑭ 排序 + 多标签筛选（R-21）：taken_at 缺值排最后；all 模式逐标签 EXISTS
#[test]
fn sort_and_multi_tag_filter() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    let a3 = add_asset(&conn, "d:/p/a3.jpg", "a3.jpg", "jpg", "image/jpeg");
    // a1 拍摄时间最早；a2 最晚；a3 无 taken_at（应排最后）
    assets::set_exif(
        &conn,
        a1,
        &assets::ExifPatch {
            taken_at: Some(1600000000000),
            ..Default::default()
        },
    )?;
    assets::set_exif(
        &conn,
        a2,
        &assets::ExifPatch {
            taken_at: Some(1650000000000),
            ..Default::default()
        },
    )?;
    let sorted = assets::list(
        &conn,
        &AssetFilter {
            sort_by: Some("taken_at".into()),
            sort_dir: Some("asc".into()),
            ..Default::default()
        },
    )?;
    let ids: Vec<i64> = sorted.items.iter().map(|a| a.id).collect();
    assert_eq!(ids, vec![a1, a2, a3], "taken_at 升序且缺值排最后");

    // 多标签：a1 挂双标签，a2 只挂其一
    let ta = tags::create(&conn, "风景", None)?;
    let tb = tags::create(&conn, "海边", None)?;
    asset_tags::assign(&conn, &[a1], &[ta.id, tb.id], "manual")?;
    asset_tags::assign(&conn, &[a2], &[ta.id], "manual")?;
    let all_mode = assets::list(
        &conn,
        &AssetFilter {
            tag_ids: vec![ta.id, tb.id],
            tags_mode: Some("all".into()),
            ..Default::default()
        },
    )?;
    assert_eq!(all_mode.total, 1);
    assert_eq!(all_mode.items[0].id, a1);
    let any_mode = assets::list(
        &conn,
        &AssetFilter {
            tag_ids: vec![ta.id, tb.id],
            ..Default::default()
        },
    )?;
    assert_eq!(any_mode.total, 2);
    Ok(())
}

// ⑮ 回收站（R-22）：软删隔离 + trash_only 查看 + 恢复 + 超期清单
#[test]
fn trash_soft_delete_restore() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    assets::soft_delete(&conn, &[a1])?;

    // 默认列表不含已软删
    let normal = assets::list(&conn, &AssetFilter::default())?;
    assert_eq!(normal.total, 1);
    assert_eq!(normal.items[0].id, a2);
    // 回收站视图
    let trash = assets::list(
        &conn,
        &AssetFilter {
            trash_only: true,
            ..Default::default()
        },
    )?;
    assert_eq!(trash.total, 1);
    assert_eq!(trash.items[0].id, a1);
    // 恢复
    assert_eq!(assets::restore(&conn, &[a1])?, 1);
    assert_eq!(assets::list(&conn, &AssetFilter::default())?.total, 2);
    // 超期清单：刚软删的不超期；手工回拨 deleted_at 后应命中
    assert!(assets::list_expired_trash(&conn, 0)?.is_empty());
    assets::soft_delete(&conn, &[a1])?;
    conn.execute("UPDATE assets SET deleted_at = 1 WHERE id = ?1", [a1])?;
    let expired = assets::list_expired_trash(&conn, chrono::Utc::now().timestamp_millis())?;
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].0, a1);
    Ok(())
}

// ⑯ 打标历史（R-25）：挂/摘写流水 + AI 确认带 batch_id + 批次撤销幂等
#[test]
fn tag_ops_record_and_undo() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p1.jpg", "p1.jpg", "jpg", "image/jpeg");
    // 手工挂摘写流水
    let t = tags::create(&conn, "海边", None)?;
    asset_tags::assign(&conn, &[id], &[t.id], "manual")?;
    asset_tags::remove(&conn, &[id], &[t.id])?;
    let ops = tag_ops::recent(&conn, 100)?;
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|o| o.batch_id.is_none()));

    // AI 确认流写 add 流水带 batch_id
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sugg = ai::list_suggestions(&conn, batch.id)?.remove(0);
    let tags_map = ai::CategorizedTags::from([("场景".to_string(), vec!["夜景".to_string()])]);
    ai::confirm_suggestion(&conn, sugg.id, &tags_map)?;
    let ai_ops = tag_ops::recent(&conn, 100)?;
    let add_op = ai_ops
        .iter()
        .find(|o| o.batch_id == Some(batch.id))
        .expect("确认应写带批次的流水");
    assert_eq!(add_op.op, "add");
    assert_eq!(add_op.actor, "ai_cloud");

    // 撤销：AI 标签被摘除；重复撤销幂等
    assert!(assets::get(&conn, id)?
        .tags
        .iter()
        .any(|tg| tg.name == "夜景"));
    let n = tag_ops::undo_batch(&conn, batch.id)?;
    assert!(n >= 1);
    assert!(!assets::get(&conn, id)?
        .tags
        .iter()
        .any(|tg| tg.name == "夜景"));
    assert_eq!(tag_ops::undo_batch(&conn, batch.id)?, 0);
    Ok(())
}

// D-6：AI 添加后手工确认，撤销 AI 批次不删除手工确认的标签
#[test]
fn undo_ai_batch_keeps_manual_confirmed_tag() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/d6_1.jpg", "d6_1.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sug = ai::list_suggestions(&conn, batch.id)?.remove(0);
    let tags = ai::CategorizedTags::from([("subject".to_string(), vec!["杯子".to_string()])]);
    ai::confirm_suggestion(&conn, sug.id, &tags)?;
    // 手工确认同一标签 → 覆盖来源为 manual、清空 source_batch_id
    let t = tags::find_or_create_canonical(&conn, "subject", "杯子")?;
    asset_tags::assign(&conn, &[id], &[t], "manual")?;
    assert_eq!(
        tag_ops::undo_batch(&conn, batch.id)?,
        0,
        "AI 批次撤销不应删除手工确认的标签"
    );
    assert!(
        assets::get(&conn, id)?
            .tags
            .iter()
            .any(|tg| tg.name == "杯子"),
        "手工确认的标签在撤销 AI 批次后应保留"
    );
    Ok(())
}

// D-6：AI 添加后手工删除再重新添加，撤销 AI 批次不删除重新添加的标签
#[test]
fn undo_ai_batch_keeps_manually_readded_tag() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/d6_2.jpg", "d6_2.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sug = ai::list_suggestions(&conn, batch.id)?.remove(0);
    let tags = ai::CategorizedTags::from([("subject".to_string(), vec!["杯子".to_string()])]);
    ai::confirm_suggestion(&conn, sug.id, &tags)?;
    let t = tags::find_or_create_canonical(&conn, "subject", "杯子")?;
    // 手工删除后重新添加（走 manual，source_batch_id 应为 NULL）
    asset_tags::remove(&conn, &[id], &[t])?;
    asset_tags::assign(&conn, &[id], &[t], "manual")?;
    assert_eq!(
        tag_ops::undo_batch(&conn, batch.id)?,
        0,
        "撤销 AI 批次不应删除手工重新添加的标签"
    );
    assert!(
        assets::get(&conn, id)?
            .tags
            .iter()
            .any(|tg| tg.name == "杯子"),
        "手工重新添加的标签在撤销 AI 批次后应保留"
    );
    Ok(())
}

// D-6：批次 A 添加、批次 B 对同一素材同一标签确认——当前单归属边界：A 撤销会删除该关联，
// B 没有自己的关联可恢复（INSERT OR IGNORE 不产生新行）。测试名写明预期语义。
#[test]
fn cross_batch_single_ownership_undo_a_removes_tag_shared_with_b() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/d6_3.jpg", "d6_3.jpg", "jpg", "image/jpeg");
    let batch_a = ai::create_batch(&conn, &[id], "cloud")?;
    let sug_a = ai::list_suggestions(&conn, batch_a.id)?.remove(0);
    let tags = ai::CategorizedTags::from([("subject".to_string(), vec!["杯子".to_string()])]);
    ai::confirm_suggestion(&conn, sug_a.id, &tags)?;

    // 批次 B 确认同一素材同一标签（INSERT OR IGNORE 不产生新的关联行）
    let batch_b = ai::create_batch(&conn, &[id], "cloud")?;
    let sug_b = ai::list_suggestions(&conn, batch_b.id)?.remove(0);
    ai::confirm_suggestion(&conn, sug_b.id, &tags)?;
    let at_rows: i64 = conn.query_row("SELECT COUNT(*) FROM asset_tags", [], |r| r.get(0))?;
    assert_eq!(at_rows, 1, "单归属：同一关联只有一行");

    // 撤销批次 A → 删除该唯一关联（B 无自己的关联可恢复）
    assert!(tag_ops::undo_batch(&conn, batch_a.id)? >= 1);
    assert!(
        !assets::get(&conn, id)?
            .tags
            .iter()
            .any(|tg| tg.name == "杯子"),
        "单归属边界：撤销 A 删除共享的同一关联（B 已确认但无独立关联）"
    );
    // B 的确认计数不受影响（历史事实保留，不把 confused 计数伪装成 0）
    let b_after = ai::get_batch(&conn, batch_b.id)?;
    assert_eq!(
        b_after.confirmed, 1,
        "撤销 A 不应改动 B 的 confirmed 历史计数"
    );
    Ok(())
}

// D-6：remove 流水撤销后重新插入的关联 source_batch_id 为 NULL（恢复的关联不再属于被撤销批次）
#[test]
fn undo_remove_op_reinserts_with_null_source_batch() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/d6_4.jpg", "d6_4.jpg", "jpg", "image/jpeg");
    let t = tags::create(&conn, "海边", None)?;
    // 直接构造：往某批次插入一条 remove 流水（手工移除过该关联），随后撤销该批次
    asset_tags::assign(&conn, &[id], &[t.id], "manual")?;
    asset_tags::remove(&conn, &[id], &[t.id])?; // 写 remove 流水（batch None）
    let last_remove_id: i64 = conn.query_row("SELECT MAX(id) FROM tag_ops", [], |r| r.get(0))?;
    // 把这条 remove 流水关联到一个批次，使其可被该批次撤销
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    conn.execute(
        "UPDATE tag_ops SET batch_id = ?1 WHERE id = ?2",
        rusqlite::params![batch.id, last_remove_id],
    )?;
    assert!(tag_ops::undo_batch(&conn, batch.id)? >= 1);
    let batch_null: Option<i64> = conn.query_row(
        "SELECT source_batch_id FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
        rusqlite::params![id, t.id],
        |r| r.get(0),
    )?;
    assert!(
        batch_null.is_none(),
        "remove 恢复的关联 source_batch_id 应为 NULL（不再属于被撤销批次）"
    );
    Ok(())
}

// ⑯ v6：单条打标失败原因落库（last_error）可回读——本地打标错误详情的存储侧验证
#[test]
fn suggestion_last_error_roundtrip() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p2.jpg", "p2.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "local")?;
    let sugg = ai::list_suggestions(&conn, batch.id)?.remove(0);
    // 初始无错误
    assert!(sugg.last_error.is_none());
    // 失败 → set_suggestion_error + reject
    ai::set_suggestion_error(
        &conn,
        sugg.id,
        "模型未返回可解析的标签（原始返回：我无法查看图片）",
    )?;
    ai::reject_suggestion(&conn, sugg.id)?;
    let rej = ai::list_suggestions(&conn, batch.id)?.remove(0);
    assert_eq!(rej.status, "rejected");
    assert!(rej
        .last_error
        .as_deref()
        .unwrap_or("")
        .contains("未返回可解析"));
    Ok(())
}

#[test]
fn canonical_tag_governance_and_deactivation() -> AppResult<()> {
    let conn = setup();
    let asset = add_asset(&conn, "d:/p/gov.jpg", "gov.jpg", "jpg", "image/jpeg");
    let t = tags::find_or_create_canonical(&conn, "subject", "咖啡")?;
    asset_tags::assign(&conn, &[asset], &[t], "manual")?;
    let same = tags::find_or_create_canonical(&conn, "subject", " 咖啡 ")?;
    assert_eq!(same, t);
    assert!(tags::governance(&conn)?.iter().any(|g| {
        g.facet_key == "subject" && g.active_tag_count >= 1 && g.linked_asset_count >= 1
    }));
    tags::deactivate(&conn, t)?;
    assert!(tags::search_candidates(&conn, Some("subject"), "咖啡")?.is_empty());
    let links: i64 = conn.query_row("SELECT COUNT(*) FROM asset_tags", [], |r| r.get(0))?;
    assert_eq!(links, 1);
    Ok(())
}

#[test]
fn ai_suggestion_item_decision_and_final_mapping() -> AppResult<()> {
    let conn = setup();
    let asset = add_asset(
        &conn,
        "d:/p/decision.jpg",
        "decision.jpg",
        "jpg",
        "image/jpeg",
    );
    let batch = ai::create_batch(&conn, &[asset], "cloud")?;
    let suggestion = ai::list_suggestions(&conn, batch.id)?.remove(0);
    let tags = ai::CategorizedTags::from([("subject".to_string(), vec!["杯子".to_string()])]);
    ai::set_suggestion_tags(&conn, suggestion.id, &tags)?;
    let item = ai::list_suggestion_items(&conn, suggestion.id)?.remove(0);
    assert!(item.tag_id.is_none());
    let canonical = tags::find_or_create_canonical(&conn, "subject", "杯子")?;
    ai::decide_suggestion_item(
        &conn,
        item.id,
        "accepted",
        Some(canonical),
        None,
        Some("规范化"),
    )?;
    ai::confirm_suggestion(&conn, suggestion.id, &tags)?;
    let final_item = ai::list_suggestion_items(&conn, suggestion.id)?.remove(0);
    assert_eq!(final_item.tag_id, Some(canonical));
    assert_eq!(final_item.decision, "accepted");
    assert!(assets::get(&conn, asset)?
        .tags
        .iter()
        .any(|t| t.id == canonical));
    Ok(())
}

#[test]
fn cross_facet_merge_is_rejected() -> AppResult<()> {
    let conn = setup();
    let subject = tags::find_or_create_canonical(&conn, "subject", "主体")?;
    let scene = tags::find_or_create_canonical(&conn, "scene", "主体")?;
    assert!(tags::merge_preserve_alias(&conn, subject, scene).is_err());
    Ok(())
}

#[test]
fn alias_conflict_is_rejected() -> AppResult<()> {
    let conn = setup();
    let a = tags::find_or_create_canonical(&conn, "subject", "猫")?;
    let b = tags::find_or_create_canonical(&conn, "subject", "小猫")?;
    tags::add_alias(&conn, a, "cat", Some("en"), "translation")?;
    assert!(tags::add_alias(&conn, b, "CAT", Some("en"), "translation").is_err());
    Ok(())
}

#[test]
fn tag_facets_aliases_and_canonical_search() -> AppResult<()> {
    let conn = setup();
    let facets = db::tag_facets::list(&conn)?;
    assert!(facets.iter().any(|f| f.key == "subject"));
    assert!(facets.iter().any(|f| f.key == "scene"));

    let root = tags::find_or_create_facet_root(&conn, "subject", "主体对象")?;
    let tea = tags::create_in_facet(&conn, "茶", Some(root), Some("subject"))?;
    tags::add_alias(&conn, tea.id, "茶叶", Some("zh-CN"), "synonym")?;
    let id = add_asset(&conn, "d:/p/tea.jpg", "tea.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[id], &[tea.id], "manual")?;

    assert_eq!(db::search::search_asset_ids_all(&conn, "茶叶")?, vec![id]);
    let candidates = tags::search_candidates(&conn, Some("subject"), "茶叶")?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, tea.id);

    tags::update_preserve_alias(&conn, tea.id, Some("茶饮"), None)?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "茶")?, vec![id]);
    assert!(tags::aliases(&conn, tea.id)?.iter().any(|a| a == "茶"));
    Ok(())
}

#[test]
fn facet_filter_any_all_and_exclude() -> AppResult<()> {
    let conn = setup();
    let subject = tags::find_or_create_facet_root(&conn, "subject", "主体对象")?;
    let scene = tags::find_or_create_facet_root(&conn, "scene", "场景/地点")?;
    let tea = tags::create_in_facet(&conn, "茶", Some(subject), Some("subject"))?;
    let coffee = tags::create_in_facet(&conn, "咖啡", Some(subject), Some("subject"))?;
    let room = tags::create_in_facet(&conn, "室内", Some(scene), Some("scene"))?;
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    asset_tags::assign(&conn, &[a1], &[tea.id, room.id], "manual")?;
    asset_tags::assign(&conn, &[a2], &[coffee.id, room.id], "manual")?;

    let filtered = assets::list(
        &conn,
        &AssetFilter {
            facet_filters: vec![
                assets::FacetTagFilter {
                    facet_key: "subject".into(),
                    tag_ids: vec![tea.id],
                    mode: Some("any".into()),
                    include_descendants: true,
                },
                assets::FacetTagFilter {
                    facet_key: "scene".into(),
                    tag_ids: vec![room.id],
                    mode: Some("all".into()),
                    include_descendants: true,
                },
            ],
            ..Default::default()
        },
    )?;
    assert_eq!(
        filtered.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![a1]
    );

    let excluded = assets::list(
        &conn,
        &AssetFilter {
            exclude_tag_ids: vec![tea.id],
            ..Default::default()
        },
    )?;
    assert!(!excluded.items.iter().any(|a| a.id == a1));
    assert!(excluded.items.iter().any(|a| a.id == a2));
    Ok(())
}

#[test]
fn metadata_facets_cover_folders_images_and_videos() -> AppResult<()> {
    let conn = setup();
    let image = add_asset(
        &conn,
        "d:/library/旅行/上海/photo.jpg",
        "photo.jpg",
        "jpg",
        "image/jpeg",
    );
    let video = add_asset(
        &conn,
        "d:/library/视频/clip.mp4",
        "clip.mp4",
        "mp4",
        "video/mp4",
    );
    conn.execute(
        "UPDATE assets SET width=4000, height=3000, camera='Test Camera', file_size=5242880 WHERE id=?1",
        [image],
    )?;
    conn.execute(
        "UPDATE assets SET width=1920, height=1080, duration_ms=75000, video_codec='h264', audio_codec='aac', file_size=52428800 WHERE id=?1",
        [video],
    )?;

    let facets = assets::list_metadata_facets(&conn, Some("d:/library"))?;
    let folders = facets
        .iter()
        .find(|facet| facet.key == "folder")
        .expect("应有文件夹分面");
    assert!(folders.items.iter().any(|item| item.label == "旅行/上海"));
    assert!(folders.items.iter().any(|item| item.label == "视频"));
    assert!(facets.iter().any(|facet| facet.key == "camera"));
    assert!(facets.iter().any(|facet| facet.key == "duration"));
    assert!(facets.iter().any(|facet| facet.key == "video_codec"));

    let folder_filtered = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![assets::MetadataFilter {
                key: "folder".into(),
                op: "eq".into(),
                value: Some("d:/library/旅行".into()),
                values: None,
                min: None,
                max: None,
            }],
            ..Default::default()
        },
    )?;
    assert_eq!(
        folder_filtered
            .items
            .iter()
            .map(|asset| asset.id)
            .collect::<Vec<_>>(),
        vec![image]
    );

    let video_filtered = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![
                assets::MetadataFilter {
                    key: "duration_ms".into(),
                    op: "between".into(),
                    value: None,
                    values: None,
                    min: Some(60000.into()),
                    max: Some(300000.into()),
                },
                assets::MetadataFilter {
                    key: "video_codec".into(),
                    op: "eq".into(),
                    value: Some("h264".into()),
                    values: None,
                    min: None,
                    max: None,
                },
            ],
            ..Default::default()
        },
    )?;
    assert_eq!(
        video_filtered
            .items
            .iter()
            .map(|asset| asset.id)
            .collect::<Vec<_>>(),
        vec![video]
    );
    Ok(())
}

// ⑰ P1A：元数据比较操作（数值/字符串/日期/分辨率/宽高比/NULL 排除/非法回退）
#[test]
fn metadata_comparison_operators() -> AppResult<()> {
    let conn = setup();
    // 大图：4000x3000 = 1200 万像素，camera=Sony，taken_at=2025-05-01
    let big = add_asset(&conn, "d:/p/big.jpg", "big.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET width=4000, height=3000, camera='Sony', file_size=5242880,
                taken_at=1746057600000 WHERE id=?1",
        [big],
    )?;
    // 小图：1000x800，camera=Canon，无 taken_at
    let small = add_asset(&conn, "d:/p/small.jpg", "small.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET width=1000, height=800, camera='Canon', file_size=1048576 WHERE id=?1",
        [small],
    )?;
    // 视频：1920x1080，duration 90s
    let video = add_asset(&conn, "d:/p/video.mp4", "video.mp4", "mp4", "video/mp4");
    conn.execute(
        "UPDATE assets SET width=1920, height=1080, duration_ms=90000, file_size=52428800 WHERE id=?1",
        [video],
    )?;

    let mf = |key: &str, op: &str, value: serde_json::Value| assets::MetadataFilter {
        key: key.into(),
        op: op.into(),
        value: Some(value),
        values: None,
        min: None,
        max: None,
    };
    let between =
        |key: &str, min: serde_json::Value, max: serde_json::Value| assets::MetadataFilter {
            key: key.into(),
            op: "between".into(),
            value: None,
            values: None,
            min: Some(min),
            max: Some(max),
        };
    let ids = |page: assets::AssetPage| {
        let mut v = page.items.iter().map(|a| a.id).collect::<Vec<_>>();
        v.sort();
        v
    };

    // file_size >= 5MB -> big(5MB) + video(50MB)
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("file_size", "gte", 5242880.into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big, video]);

    // camera contains "son" -> Sony (大小写不敏感？contains 用原列，不 lower，仅 substring)
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("camera", "contains", "Sony".into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // camera in [Canon] -> small
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![assets::MetadataFilter {
                key: "camera".into(),
                op: "in".into(),
                value: None,
                values: Some(vec!["Canon".into()]),
                min: None,
                max: None,
            }],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![small]);

    // resolution gte 10000000 (10MP) -> big only
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("resolution", "gte", 10000000.into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // duration_ms between 60000..120000 -> video
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![between("duration_ms", 60000.into(), 120000.into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![video]);

    // taken_at between 2025-01-01 .. 2025-12-31（左闭右开）-> big 仅
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![assets::MetadataFilter {
                key: "taken_at".into(),
                op: "between".into(),
                value: None,
                values: None,
                min: Some("2025-01-01".into()),
                max: Some("2025-12-31".into()),
            }],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // NULL 排除：taken_at gte 2020-01-01 不应命中无 taken_at 的 small
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("taken_at", "gte", "2020-01-01".into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // 非法 key → validate 拒绝
    let bad = AssetFilter {
        metadata_filters: vec![mf("nonexistent_key", "eq", "x".into())],
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());

    // 非法 op → validate 拒绝
    let bad = AssetFilter {
        metadata_filters: vec![mf("file_size", "prefix", 1.into())],
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());

    // between 缺 max → 拒绝
    let bad = AssetFilter {
        metadata_filters: vec![assets::MetadataFilter {
            key: "file_size".into(),
            op: "between".into(),
            value: None,
            values: None,
            min: Some(1.into()),
            max: None,
        }],
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());

    // in 空 values → 拒绝
    let bad = AssetFilter {
        metadata_filters: vec![assets::MetadataFilter {
            key: "camera".into(),
            op: "in".into(),
            value: None,
            values: Some(vec![]),
            min: None,
            max: None,
        }],
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());
    Ok(())
}

// ⑰b P0-2：数值分面支持 `in`（同时接受数字字符串），保证普通素材库点击 ISO/光圈/焦距分面不回归。
#[test]
fn metadata_numeric_in_facet() -> AppResult<()> {
    let conn = setup();
    // 大图：ISO 800 / f/2.8；小图：ISO 100 / f/5.6；none：无 ISO / aperture
    let big = add_asset(&conn, "d:/p/big.jpg", "big.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET width=4000, height=3000, iso=800, aperture=2.8, file_size=5242880 WHERE id=?1",
        [big],
    )?;
    let small = add_asset(&conn, "d:/p/small.jpg", "small.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET width=1000, height=800, iso=100, aperture=5.6, file_size=1048576 WHERE id=?1",
        [small],
    )?;
    let none = add_asset(&conn, "d:/p/none.jpg", "none.jpg", "jpg", "image/jpeg");

    let in_filter = |key: &str, values: Vec<serde_json::Value>| assets::MetadataFilter {
        key: key.into(),
        op: "in".into(),
        value: None,
        values: Some(values),
        min: None,
        max: None,
    };
    let mf = |key: &str, op: &str, value: serde_json::Value| assets::MetadataFilter {
        key: key.into(),
        op: op.into(),
        value: Some(value),
        values: None,
        min: None,
        max: None,
    };
    let ids = |page: assets::AssetPage| {
        let mut v = page.items.iter().map(|a| a.id).collect::<Vec<_>>();
        v.sort();
        v
    };

    // iso in ["800", "1600"]（数字字符串）→ 仅 big
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![in_filter("iso", vec!["800".into(), "1600".into()])],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // iso in [100, 800]（JSON number）→ big + small
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![in_filter("iso", vec![100.into(), 800.into()])],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big, small]);

    // iso eq 800 → 仅 big
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("iso", "eq", 800.into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big]);

    // aperture gte 2.8（数字字符串）→ big(2.8) + small(5.6)
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("aperture", "gte", "2.8".into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big, small]);

    // aperture gte 4.0 → 仅 small
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![mf("aperture", "gte", 4.0.into())],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![small]);

    // 非数字字符串 → 明确错误
    let bad = AssetFilter {
        metadata_filters: vec![in_filter("iso", vec!["abc".into()])],
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());

    // NULL 数值不命中（none 无 ISO，靠 IS NOT NULL 守卫排除）
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![in_filter("iso", vec![100.into(), 800.into()])],
            ..Default::default()
        },
    )?;
    assert_eq!(ids(r), vec![big, small]);
    let _ = none;

    // 普通素材库 metadata facet 点击后查询成功：list_metadata_facets 返回 iso 分面，
    // 把 facet item 的 value（字符串）转成 in 查询，不报错且命中。
    let facets = assets::list_metadata_facets(&conn, None)?;
    let iso_facet = facets
        .iter()
        .find(|f| f.key == "iso")
        .expect("存在 iso 分面");
    assert!(!iso_facet.items.is_empty());
    let iso_vals = iso_facet
        .items
        .iter()
        .map(|it| it.value.clone())
        .collect::<Vec<_>>();
    let r = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![in_filter(
                "iso",
                iso_vals.into_iter().map(|v| v.into()).collect(),
            )],
            ..Default::default()
        },
    )?;
    assert!(r.items.iter().any(|a| a.id == big || a.id == small));
    Ok(())
}

// ⑱ P1B：V10 迁移——旧 tag_categories 转 ai_facet_configs 并落库（幂等）
#[test]
fn v10_migrates_legacy_tag_categories_to_facet_configs() -> AppResult<()> {
    let conn = setup();
    // 设置写入旧格式 tag_categories（含中文分类名）
    let legacy = serde_json::json!({
        "theme": "dark",
        "thumbnailCacheMb": 1024,
        "tagCategories": [
            {"name":"场景","hint":"如公园/街道","single":true,"max":1},
            {"name":"未知分类","hint":"保留","single":false,"max":3}
        ],
        "libraryRoot": "",
        "trashRetentionDays": 30
    });
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('app_settings', ?1)",
        [legacy.to_string()],
    )?;

    // 回退 user_version 触发 V10
    conn.pragma_update(None, "user_version", 9)?;
    migrations::migrate(&conn)?;

    // 迁移链现跑到 V20：旧分类已并入 tag_facets（V10 转换 + V20 合表），JSON 侧清空。
    let s = settings::get_settings(&conn)?;
    assert!(s.tag_categories.is_empty(), "旧分类应被清空");
    // scene 的旧 hint 由 V20 并入 tag_facets.description
    let scene_desc: String = conn.query_row(
        "SELECT description FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert!(
        scene_desc.contains("如公园/街道"),
        "旧 hint 应并入 description: {scene_desc}"
    );
    let scene_mode: String = conn.query_row(
        "SELECT input_mode FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(scene_mode, "ai_and_manual", "scene 应参与 AI");
    // 其他设置字段不丢失
    assert_eq!(s.theme, "dark");
    assert_eq!(s.thumbnail_cache_mb, 1024);
    assert_eq!(s.trash_retention_days, 30);

    // 幂等：再次迁移不重复配置（description 不重复拼接）
    conn.pragma_update(None, "user_version", 9)?;
    migrations::migrate(&conn)?;
    let scene_desc2: String = conn.query_row(
        "SELECT description FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(scene_desc2, scene_desc, "重跑不得重复拼接 hint");
    Ok(())
}

// C-5/V11：存量库补齐独立 color 分面（tag_facets 行 + ai_facet_configs 配置），幂等
#[test]
fn v11_adds_color_facet_to_existing_settings() -> AppResult<()> {
    let conn = setup();
    // 模拟存量库：settings 只有 scene 配置、缺 color；tag_facets 缺 color 行
    let legacy = serde_json::json!({
        "ai": { "profiles": [], "activeProfile": "" },
        "theme": "system",
        "aiFacetConfigs": [{"facetKey":"scene","hint":"如房间","enabledForAi":true}]
    });
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('app_settings', ?1)",
        [legacy.to_string()],
    )?;
    // 删除 color tag_facets 行（模拟老库可能确实缺），回退 user_version 触发 V11
    conn.execute("DELETE FROM tag_facets WHERE key = 'color'", [])?;
    conn.pragma_update(None, "user_version", 10)?;
    migrations::migrate(&conn)?;

    // tag_facets 补齐 color（FB2-08/V16 起 color 为 inactive，list() 只回 active，
    // 改用 list_all() 断言行存在且状态 inactive；旧行为断言 color ∈ list() 已随 V16 失效）
    let facets = db::tag_facets::list_all(&conn)?;
    let color = facets
        .iter()
        .find(|f| f.key == "color")
        .expect("tag_facets 应补齐 color 分面");
    assert_eq!(color.status, "inactive", "V16 起 color 分面应为 inactive");
    // V20 后：color 的 AI 语义在 tag_facets.input_mode（manual_only，V16 已停用）。
    // 旧 JSON 的 scene hint（如房间）在重放链中经 V10→V20 读改写后被默认 hint 替换
    // （ai_facet_configs 已 skip_serializing，JSON 侧不再持久化）——description 承载的是
    // 最终生效的默认 hint，语义正确。
    let scene_mode: String = conn.query_row(
        "SELECT input_mode FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(scene_mode, "ai_and_manual", "scene 应参与 AI");
    let color_mode: String = conn.query_row(
        "SELECT input_mode FROM tag_facets WHERE key='color'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(color_mode, "manual_only", "V16 起 color 不参与 AI");

    // 幂等：再跑一次不重复
    conn.pragma_update(None, "user_version", 10)?;
    migrations::migrate(&conn)?;
    let scene_mode2: String = conn.query_row(
        "SELECT input_mode FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(scene_mode2, "ai_and_manual");
    let color_mode2: String = conn.query_row(
        "SELECT input_mode FROM tag_facets WHERE key='color'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(color_mode2, "manual_only");
    Ok(())
}

// 阶段6 §9.7 + W2-1：AI 打标与 AI 超级搜索共用同一 FacetPromptContext。
// V20 合表后 hint/displayName 覆盖语义搬进 tag_facets 本体——改 description 或
// display_name 直接写库，两边同步读新值；显示名改变不影响 facet_key。
#[test]
fn prompt_context_reflects_facet_config_overrides() -> AppResult<()> {
    let conn = setup();
    conn.execute(
        "UPDATE tag_facets SET description = '识别拍摄场景', display_name = '场景' WHERE key = 'scene'",
        [],
    )?;
    let ctx = db::tag_facets::build_prompt_context(&conn, "all")?;
    let scene = ctx
        .iter()
        .find(|c| c.key == "scene")
        .expect("存在 scene 分面");
    assert_eq!(scene.description, "识别拍摄场景");
    assert_eq!(scene.display_name, "场景", "显示名改动生效");
    assert_eq!(scene.key, "scene", "显示名改变不影响 facetKey");
    Ok(())
}

// W2-1：manual_only 分面不进提示词；description 进提示词。
#[test]
fn prompt_context_excludes_manual_only_and_includes_description() -> AppResult<()> {
    let conn = setup();
    let defaults = db::tag_facets::build_prompt_context(&conn, "all")?;
    for key in ["purpose", "technical"] {
        assert!(
            !defaults.iter().any(|c| c.key == key),
            "{key} 是人工判断项，不得进入 AI 提示词"
        );
    }
    tag_facets::create(
        &conn,
        "manual_field",
        "手工字段",
        "只手工填写",
        "multi",
        None,
        "all",
    )?;
    // F4：参与 AI 的事实源是 cfg_ai_assignable（input_mode 已降级为派生列），置 0 = 不进提示词
    conn.execute(
        "UPDATE tag_facets SET cfg_ai_assignable = 0 WHERE key = 'manual_field'",
        [],
    )?;
    tag_facets::create(
        &conn,
        "ai_field",
        "AI字段",
        "这段描述会原样给 AI 看",
        "multi",
        None,
        "all",
    )?;
    let ctx = db::tag_facets::build_prompt_context(&conn, "all")?;
    assert!(
        !ctx.iter().any(|c| c.key == "manual_field"),
        "cfg_ai_assignable=0 分面不得进 AI 提示词"
    );
    let ai = ctx
        .iter()
        .find(|c| c.key == "ai_field")
        .expect("参与 AI 的分面应进提示词");
    assert_eq!(ai.description, "这段描述会原样给 AI 看");
    Ok(())
}

// ⑲ P4：布尔表达式树——(含 A 或 含 B) 且 非C 的嵌套查询
use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr};

#[test]
fn expr_nested_and_or_not_query() -> AppResult<()> {
    let conn = setup();
    // 素材：a1 挂「海边」，a2 挂「沙滩」，a3 挂「海边 + 夜景」
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    let a3 = add_asset(&conn, "d:/p/a3.jpg", "a3.jpg", "jpg", "image/jpeg");
    let sea = tags::find_or_create_canonical(&conn, "scene", "海边")?;
    let beach = tags::find_or_create_canonical(&conn, "scene", "沙滩")?;
    let night = tags::find_or_create_canonical(&conn, "lighting", "夜景")?;
    asset_tags::assign(&conn, &[a1, a3], &[sea], "manual")?;
    asset_tags::assign(&conn, &[a2], &[beach], "manual")?;
    asset_tags::assign(&conn, &[a3], &[night], "manual")?;

    // (含「海边」或含「沙滩」) 且 NOT 含「夜景」
    let expr = QueryExpr::And {
        children: vec![
            QueryExpr::Or {
                children: vec![
                    QueryExpr::Leaf {
                        cond: LeafCond::Tag {
                            term_query: None,
                            term_match: Default::default(),
                            facet_key: "scene".into(),
                            tag_ids: vec![sea],
                            mode: Some("any".into()),
                            include_descendants: true,
                        },
                    },
                    QueryExpr::Leaf {
                        cond: LeafCond::Tag {
                            term_query: None,
                            term_match: Default::default(),
                            facet_key: "scene".into(),
                            tag_ids: vec![beach],
                            mode: Some("any".into()),
                            include_descendants: true,
                        },
                    },
                ],
            },
            QueryExpr::Not {
                child: Box::new(QueryExpr::Leaf {
                    cond: LeafCond::Tag {
                        term_query: None,
                        term_match: Default::default(),
                        facet_key: "lighting".into(),
                        tag_ids: vec![night],
                        mode: Some("any".into()),
                        include_descendants: true,
                    },
                }),
            },
        ],
    };
    let page = assets::list(
        &conn,
        &AssetFilter {
            expr: Some(expr),
            ..Default::default()
        },
    )?;
    let mut ids = page.items.iter().map(|a| a.id).collect::<Vec<_>>();
    ids.sort();
    // 期望命中 a1（海边且非夜景）与 a2（沙滩且非夜景）；排除 a3（含夜景）
    assert_eq!(ids, vec![a1, a2]);
    Ok(())
}

#[test]
fn expr_metadata_combined_with_tag() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    conn.execute("UPDATE assets SET file_size=5242880 WHERE id=?1", [a1])?;
    conn.execute("UPDATE assets SET file_size=1048576 WHERE id=?1", [a2])?;
    let sea = tags::find_or_create_canonical(&conn, "scene", "海边")?;
    asset_tags::assign(&conn, &[a1, a2], &[sea], "manual")?;

    // 含「海边」且 file_size>=5MB（只 a1）
    let expr = QueryExpr::And {
        children: vec![
            QueryExpr::Leaf {
                cond: LeafCond::Tag {
                    term_query: None,
                    term_match: Default::default(),
                    facet_key: "scene".into(),
                    tag_ids: vec![sea],
                    mode: Some("any".into()),
                    include_descendants: true,
                },
            },
            QueryExpr::Leaf {
                cond: LeafCond::Metadata {
                    filter: bagertea_ai_media_v2_lib::db::search_query::MetadataFilter {
                        key: "file_size".into(),
                        op: "gte".into(),
                        value: Some(5242880.into()),
                        values: None,
                        min: None,
                        max: None,
                    },
                },
            },
        ],
    };
    let page = assets::list(
        &conn,
        &AssetFilter {
            expr: Some(expr),
            ..Default::default()
        },
    )?;
    assert_eq!(
        page.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![a1]
    );
    Ok(())
}

#[test]
fn expr_invalid_rejected_by_validate() -> AppResult<()> {
    let conn = setup();
    add_asset(&conn, "d:/p/a.jpg", "a.jpg", "jpg", "image/jpeg");
    // 空 Or 组 → validate 拒绝
    let bad = AssetFilter {
        expr: Some(QueryExpr::Or { children: vec![] }),
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad).is_err());
    // 未知分面标签 → tag_ids 空拒绝
    let bad2 = AssetFilter {
        expr: Some(QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: "scene".into(),
                tag_ids: vec![],
                mode: Some("any".into()),
                include_descendants: true,
            },
        }),
        ..Default::default()
    };
    assert!(assets::list(&conn, &bad2).is_err());
    Ok(())
}

// ── FB5-05（§7.6）：一句话描述 确认链 ──

/// 单条确认：确认标签同一事务内写入非空描述；suggested_description 由 set_suggestion_result 写入。
#[test]
fn fb5_confirm_writes_description_with_tags() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p1.jpg", "p1.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sugg = ai::list_suggestions(&conn, batch.id)?.remove(0);

    let tags_map = ai::CategorizedTags::from([("场景".to_string(), vec!["夜景".to_string()])]);
    ai::set_suggestion_result(&conn, sugg.id, &tags_map, "夜晚树下多人合影")?;

    let sugg2 = ai::list_suggestions(&conn, batch.id)?.remove(0);
    assert_eq!(sugg2.suggested_description, "夜晚树下多人合影");
    assert_eq!(sugg2.current_description, "");

    // 确认时带描述：同一事务写入 assets.content_description + confirmed_description
    ai::confirm_suggestion_with_description(&conn, sugg.id, &tags_map, Some("夜晚树下多人合影"))?;
    let (desc, confirmed): (String, Option<String>) = conn.query_row(
        "SELECT content_description, confirmed_description FROM assets a
          JOIN ai_suggestions s ON s.asset_id = a.id WHERE a.id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(desc, "夜晚树下多人合影");
    assert_eq!(confirmed.as_deref(), Some("夜晚树下多人合影"));
    // 描述可被普通搜索命中（§8.1 全局行为）
    assert_eq!(db::search::search_asset_ids_all(&conn, "合影")?, vec![id]);
    Ok(())
}

/// 确认时描述为空：保留素材已有描述，不覆盖为空（§7.6）。
#[test]
fn fb5_confirm_empty_description_keeps_existing() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(&conn, "d:/p/p2.jpg", "p2.jpg", "jpg", "image/jpeg");
    conn.execute(
        "UPDATE assets SET content_description = '已有描述' WHERE id = ?1",
        [id],
    )?;
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sugg = ai::list_suggestions(&conn, batch.id)?.remove(0);
    let tags_map = ai::CategorizedTags::from([("场景".to_string(), vec!["夜景".to_string()])]);
    ai::set_suggestion_result(&conn, sugg.id, &tags_map, "")?;
    ai::confirm_suggestion_with_description(&conn, sugg.id, &tags_map, Some(""))?;
    let desc: String = conn.query_row(
        "SELECT content_description FROM assets WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(desc, "已有描述", "空描述不得覆盖已有描述");
    Ok(())
}

/// 批量确认：逐条应用各自描述，不得把第一张描述套给整批（§7.6）。
#[test]
fn fb5_confirm_all_applies_per_suggestion_description() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/a1.jpg", "a1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/a2.jpg", "a2.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[a1, a2], "cloud")?;
    let suggs = ai::list_suggestions(&conn, batch.id)?;
    let t1 = ai::CategorizedTags::from([("场景".to_string(), vec!["夜景".to_string()])]);
    let t2 = ai::CategorizedTags::from([("场景".to_string(), vec!["白天".to_string()])]);
    ai::set_suggestion_result(&conn, suggs[0].id, &t1, "夜景街道")?;
    ai::set_suggestion_result(&conn, suggs[1].id, &t2, "白天公园")?;
    ai::confirm_all_pending(&conn, batch.id)?;
    let rows: Vec<(i64, String)> = conn
        .prepare("SELECT id, content_description FROM assets ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|(i, d)| *i == a1 && d == "夜景街道"));
    assert!(rows.iter().any(|(i, d)| *i == a2 && d == "白天公园"));
    // 描述空的那条不覆盖：a1/a2 之外的素材保留原值
    Ok(())
}

// ── W1-3（V21）：FTS 触发器分面状态联动 ──

// 停用分面 → 该分面下的标签不再被全文搜到
// （scene 等系统分面不允许停用，用用户自建分面验证——触发器对 status 变化通用）
#[test]
fn deactivate_facet_hides_from_fts() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(&conn, "mood", "氛围", "", "multi", None, "all")?;
    let id = add_asset(
        &conn,
        "d:/p/photo010.jpg",
        "photo010.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create_in_facet(&conn, "海边", None, Some("mood"))?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    assert_eq!(db::search::search_asset_ids_all(&conn, "海边")?, vec![id]);
    tag_facets::deactivate(&conn, "mood")?;
    assert!(
        db::search::search_asset_ids_all(&conn, "海边")?.is_empty(),
        "停用分面后其标签不得再被全文搜到"
    );
    Ok(())
}

// 恢复分面 → 标签重新可搜（trg_facet_status_au 刷新回来）
#[test]
fn restore_facet_shows_in_fts() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(&conn, "mood", "氛围", "", "multi", None, "all")?;
    let id = add_asset(
        &conn,
        "d:/p/photo011.jpg",
        "photo011.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create_in_facet(&conn, "山野", None, Some("mood"))?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    tag_facets::deactivate(&conn, "mood")?;
    assert!(db::search::search_asset_ids_all(&conn, "山野")?.is_empty());
    tag_facets::restore(&conn, "mood")?;
    assert_eq!(
        db::search::search_asset_ids_all(&conn, "山野")?,
        vec![id],
        "恢复分面后标签应重新可搜"
    );
    Ok(())
}

// 停用分面不动 asset_tags 数据行（只是 FTS 不可见）
#[test]
fn deactivate_facet_keeps_asset_tags() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(&conn, "mood", "氛围", "", "multi", None, "all")?;
    let id = add_asset(
        &conn,
        "d:/p/photo012.jpg",
        "photo012.jpg",
        "jpg",
        "image/jpeg",
    );
    let tag = tags::create_in_facet(&conn, "日落", None, Some("mood"))?;
    asset_tags::assign(&conn, &[id], &[tag.id], "manual")?;
    tag_facets::deactivate(&conn, "mood")?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM asset_tags WHERE asset_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    assert_eq!(n, 1, "停用分面不得删除 asset_tags 数据行");
    Ok(())
}

// ── W2-10【阻断级】：自建分面全链路 ──
// 建分面 → build_prompt_context 含它 → 模拟 AI 返回该 key → confirm →
// 标签 facet_key == 自建 key（不是 custom）→ list_tags_by_facet 能查到 → 删分面后残留为 0。
#[test]
fn user_created_facet_full_pipeline() -> AppResult<()> {
    let conn = setup();
    // ① 用户自建分面 clothing_color
    tag_facets::create(
        &conn,
        "clothing_color",
        "人物服装颜色",
        "人物服装的主色",
        "multi",
        None,
        "all",
    )?;

    // ② build_prompt_context 含它（V20 后 input_mode=ai_and_manual 默认参与 AI）
    let ctx = bagertea_ai_media_v2_lib::db::tag_facets::build_prompt_context(&conn, "all")?;
    assert!(
        ctx.iter().any(|f| f.key == "clothing_color"),
        "自建分面必须进 AI 提示词上下文，实际: {:?}",
        ctx.iter().map(|f| f.key.clone()).collect::<Vec<_>>()
    );

    // ③ 模拟 AI 返回 {"clothing_color":["红色"]}，走真实解析函数（R0-3：
    // 旧测试用 serde_json::from_str::<CategorizedTags> 直接反序列化，跳过
    // parse_categorized_checked_ex —— 正是它漏掉了「自建 key 被 custom 吞掉」的 bug）
    let id = add_asset(&conn, "d:/p/dress.jpg", "dress.jpg", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[id], "cloud")?;
    let sid: i64 = conn.query_row(
        "SELECT id FROM ai_suggestions WHERE batch_id = ?1",
        [batch.id],
        |r| r.get(0),
    )?;
    let analysis = bagertea_ai_media_v2_lib::services::ai_cloud::parse_media_analysis(
        r#"{"description":"人物穿着红色裙子","peoplePresence":{"status":"present","confidence":0.9},"tags":{"clothing_color":[{"name":"红色","confidence":0.95}]}}"#,
        &ctx,
        0.30,
    )?;
    assert!(
        analysis.tags.contains_key("clothing_color"),
        "自建分面 key 必须原样保留：{:?}",
        analysis.tags.keys().collect::<Vec<_>>()
    );
    ai::confirm_suggestion(&conn, sid, &analysis.tags)?;

    // ④ 标签 facet_key == clothing_color（不是 custom！旧代码这里全落 custom）
    let facet: String = conn.query_row(
        "SELECT facet_key FROM tags WHERE normalized_name = '红色'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        facet, "clothing_color",
        "自建分面的标签必须挂在该分面下，不是 custom"
    );

    // ⑤ list_tags_by_facet 能查到
    let nodes = tags::list_by_facet(&conn, "clothing_color")?;
    assert!(
        nodes.iter().any(|n| n.tag.name == "红色"),
        "list_by_facet 应能查到「红色」"
    );

    // ⑥ 删分面后残留为 0（W2-3 的级联删除；走真实 delete_facet 命令）
    let report = tag_facets::delete_facet(&conn, "clothing_color")?;
    assert!(
        report.tags_deleted >= 1,
        "删除报告应含被删标签数: {:?}",
        report
    );
    let leftover: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE facet_key='clothing_color'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(leftover, 0, "删除后不得有残留标签");
    Ok(())
}

// W2-10：resolve_facet_key 三分支单元行为
#[test]
fn resolve_facet_key_three_branches() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(
        &conn,
        "clothing_color",
        "人物服装颜色",
        "",
        "multi",
        None,
        "all",
    )?;
    // ① DB 存在 → 原样返回（自建分面）
    let (k1, _) = tag_facets::resolve_facet_key(&conn, "clothing_color")?;
    assert_eq!(k1, "clothing_color");
    // ② 中文旧名 → 映射（场景→scene，DB 存在）
    let (k2, _) = tag_facets::resolve_facet_key(&conn, "场景")?;
    assert_eq!(k2, "scene");
    // 英文 key 直存（scene 在 DB）
    let (k3, _) = tag_facets::resolve_facet_key(&conn, "scene")?;
    assert_eq!(k3, "scene");
    // ③ 未知 key → custom
    let (k4, _) = tag_facets::resolve_facet_key(&conn, "nonexistent_thing")?;
    assert_eq!(k4, "custom");
    Ok(())
}

// ── W2-2/3/4：合并编辑 / 级联删除 / 影响统计 ──

// W2-2：合并命令一个事务；部分字段非法时全部不生效
#[test]
fn update_facet_single_transaction() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(
        &conn,
        "my_field",
        "原名字",
        "原描述",
        "multi",
        Some(3),
        "all",
    )?;
    // 非法 input_mode → 报错且 DB 不变
    let err = tag_facets::update_facet(
        &conn,
        "my_field",
        "新名字",
        "新描述",
        "bad_mode",
        "multi",
        Some(5),
        "all",
    );
    assert!(err.is_err());
    let (name, mode, _max): (String, String, Option<i64>) = conn.query_row(
        "SELECT display_name, input_mode, max_items FROM tag_facets WHERE key='my_field'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(name, "原名字", "非法时全部不生效（单事务）");
    assert_eq!(mode, "ai_and_manual");
    // 合法路径：全字段一次生效
    tag_facets::update_facet(
        &conn,
        "my_field",
        "新名字",
        "新描述",
        "manual_only",
        "single",
        None,
        "video",
    )?;
    let (name2, mode2, max2, applies): (String, String, Option<i64>, String) = conn.query_row(
        "SELECT display_name, input_mode, max_items, applies_to FROM tag_facets WHERE key='my_field'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    assert_eq!(
        (name2.as_str(), mode2.as_str(), max2, applies.as_str()),
        ("新名字", "manual_only", Some(1), "video")
    );
    Ok(())
}

// W2-3：级联删除清空 6 张表；系统分面拒绝；FTS 同步清
#[test]
fn delete_facet_cascades_all_refs() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(&conn, "to_delete", "待删", "", "multi", None, "all")?;
    let a1 = add_asset(&conn, "d:/p/x1.jpg", "x1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/x2.jpg", "x2.jpg", "jpg", "image/jpeg");
    let t1 = tags::create_in_facet(&conn, "标签一", None, Some("to_delete"))?;
    let t2 = tags::create_in_facet(&conn, "标签二", None, Some("to_delete"))?;
    asset_tags::assign(&conn, &[a1], &[t1.id], "manual")?;
    asset_tags::assign(&conn, &[a1, a2], &[t2.id], "manual")?;
    // FTS 可见（前置）
    assert!(!db::search::search_asset_ids_all(&conn, "标签一")?.is_empty());

    // W2-4：impact 数字与实际删除量一致
    let impact = tag_facets::get_impact(&conn, "to_delete")?;
    assert_eq!(impact.tag_count, 2);
    assert_eq!(impact.asset_count, 2);

    let report = tag_facets::delete_facet(&conn, "to_delete")?;
    assert_eq!(report.tags_deleted, 2);
    assert_eq!(report.unlinked, 3);
    // 6 张表全空
    for (sql, label) in [
        ("SELECT COUNT(*) FROM tags WHERE facet_key='to_delete'", "tags"),
        ("SELECT COUNT(*) FROM asset_tags at JOIN tags t ON t.id=at.tag_id WHERE t.facet_key='to_delete'", "asset_tags"),
        ("SELECT COUNT(*) FROM tag_aliases ta JOIN tags t ON t.id=ta.tag_id WHERE t.facet_key='to_delete'", "tag_aliases"),
        ("SELECT COUNT(*) FROM ai_suggestion_items WHERE facet_key='to_delete'", "ai_suggestion_items"),
        ("SELECT COUNT(*) FROM tag_ops o JOIN tags t ON t.id=o.tag_id WHERE t.facet_key='to_delete'", "tag_ops"),
        ("SELECT COUNT(*) FROM tag_facets WHERE key='to_delete'", "tag_facets"),
    ] {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        assert_eq!(n, 0, "删除后 {label} 应为 0");
    }
    // FTS 已清（delete_facet_clears_fts）
    assert!(db::search::search_asset_ids_all(&conn, "标签一")?.is_empty());
    // 素材本体还在
    let assets: i64 = conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))?;
    assert_eq!(assets, 2, "删除分面不得删素材");
    Ok(())
}

// W2-3：系统分面拒绝删除
#[test]
fn delete_facet_rejects_system() -> AppResult<()> {
    let conn = setup();
    assert!(tag_facets::delete_facet(&conn, "scene").is_err());
    let still: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_facets WHERE key='scene'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(still, 1);
    Ok(())
}

// W2-5：候选搜索排除已停用分面下的标签
#[test]
fn candidates_include_inactive_facet_but_exclude_searchable_off() -> AppResult<()> {
    let conn = setup();
    tag_facets::create(&conn, "mood2", "氛围", "", "multi", None, "all")?;
    let _t = tags::create_in_facet(&conn, "宁静感", None, Some("mood2"))?;
    let hits = tags::search_candidates(&conn, None, "宁静")?;
    assert_eq!(hits.len(), 1, "停用前应能搜到");
    tag_facets::deactivate(&conn, "mood2")?;
    // F4 语义变更：停用分面（cfg 保持）的标签仍可搜 —— SEARCHABLE_TAG 不看 f.status
    let hits2 = tags::search_candidates(&conn, None, "宁静")?;
    assert_eq!(
        hits2.len(),
        1,
        "停用分面标签仍可搜（cfg_searchable 保持 1）"
    );
    // 真正退出候选的是 cfg_searchable = 0
    conn.execute(
        "UPDATE tag_facets SET cfg_searchable = 0 WHERE key = 'mood2'",
        [],
    )?;
    let hits3 = tags::search_candidates(&conn, None, "宁静")?;
    assert!(
        hits3.is_empty(),
        "cfg_searchable=0 分面的标签不得出现在候选"
    );
    Ok(())
}

// ── W2-6/7：query_expr facet 剔除 + 两个新叶子 ──

// W2-6：tag_id 不属于声明 facet_key 时剔除 + 查询继续（不报错整次）
#[test]
fn query_expr_drops_facet_mismatch() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/q1.jpg", "q1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/q2.jpg", "q2.jpg", "jpg", "image/jpeg");
    let t_scene = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    let t_subject = tags::create_in_facet(&conn, "人像", None, Some("subject"))?;
    asset_tags::assign(&conn, &[a1], &[t_scene.id], "manual")?;
    asset_tags::assign(&conn, &[a2], &[t_subject.id], "manual")?;
    // scene 叶子里混入 subject 的 tag_id：剔除后只剩海边 → 只命中 a1
    let expr = QueryExpr::And {
        children: vec![QueryExpr::Leaf {
            cond: LeafCond::Tag {
                term_query: None,
                term_match: Default::default(),
                facet_key: "scene".into(),
                tag_ids: vec![t_scene.id, t_subject.id],
                mode: None,
                include_descendants: true,
            },
        }],
    };
    let sql = {
        let (frag, _params) = bagertea_ai_media_v2_lib::db::query_expr::compile_leaf(
            &conn,
            &LeafCond::Tag {
                facet_key: "scene".into(),
                tag_ids: vec![t_scene.id, t_subject.id],
                mode: None,
                include_descendants: true,
                term_query: None,
                term_match: Default::default(),
            },
        )?;
        frag
    };
    assert!(!sql.is_empty());
    // 端到端：整棵树编译后能正常查询（旧语义这里会直接 Err）
    let filter = AssetFilter {
        expr: Some(expr),
        ..Default::default()
    };
    let page = assets::list(&conn, &filter)?;
    assert_eq!(page.total, 1, "剔除后查询应继续且只命中 scene 标签的素材");
    Ok(())
}

// W2-7：facet_has_any / facet_missing 编译与语义
#[test]
fn facet_has_any_and_missing_compile() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/h1.jpg", "h1.jpg", "jpg", "image/jpeg");
    let _a2 = add_asset(&conn, "d:/p/h2.jpg", "h2.jpg", "jpg", "image/jpeg");
    let t = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    asset_tags::assign(&conn, &[a1], &[t.id], "manual")?;

    // has_any：命中打了 scene 标签的 a1
    let has_any = assets::list(
        &conn,
        &AssetFilter {
            expr: Some(QueryExpr::Leaf {
                cond: LeafCond::FacetHasAny {
                    facet_key: "scene".into(),
                },
            }),
            ..Default::default()
        },
    )?;
    assert_eq!(has_any.total, 1);

    // missing：命中没打 scene 标签的 a2
    let missing = assets::list(
        &conn,
        &AssetFilter {
            expr: Some(QueryExpr::Leaf {
                cond: LeafCond::FacetMissing {
                    facet_key: "scene".into(),
                },
            }),
            ..Default::default()
        },
    )?;
    assert_eq!(missing.total, 1);

    // F4：未知/不可搜分面 → warning + 整叶剔除（1=1），不再报错整次查询
    let (frag, _params) = bagertea_ai_media_v2_lib::db::query_expr::compile_leaf(
        &conn,
        &LeafCond::FacetHasAny {
            facet_key: "nope".into(),
        },
    )?;
    assert_eq!(frag, "1=1", "未知分面剔除为恒真条件，查询继续");
    // validate_expr 覆盖新变体：空 key 报错
    assert!(
        bagertea_ai_media_v2_lib::db::query_expr::validate_expr(&QueryExpr::Leaf {
            cond: LeafCond::FacetMissing {
                facet_key: "  ".into()
            }
        })
        .is_err()
    );
    Ok(())
}

// ── W2-8：评级 ──

#[test]
fn rating_compiles_and_filters() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/r1.jpg", "r1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/r2.jpg", "r2.jpg", "jpg", "image/jpeg");
    assets::set_rating(&conn, &[a1], 5)?;
    assets::set_rating(&conn, &[a2], 2)?;
    // rating gte 3 只命中 a1
    let page = assets::list(
        &conn,
        &AssetFilter {
            metadata_filters: vec![serde_json::from_value(serde_json::json!(
                {"key": "rating", "op": "gte", "value": 3}
            ))?],
            ..Default::default()
        },
    )?;
    assert_eq!(page.total, 1);
    // 按评级排序：5 星在前，未评级（rating=0）最后
    let a3 = add_asset(&conn, "d:/p/r3.jpg", "r3.jpg", "jpg", "image/jpeg");
    let _ = a3;
    let sorted = assets::list(
        &conn,
        &AssetFilter {
            sort_by: Some("rating".into()),
            sort_dir: Some("desc".into()),
            ..Default::default()
        },
    )?;
    let first = sorted.items.first().expect("非空");
    assert_eq!(first.rating, 5, "评级排序：5 星应排第一");
    assert_eq!(sorted.items.last().unwrap().rating, 0, "未评级排最后");
    // 清除评级
    assets::set_rating(&conn, &[a1], 0)?;
    let cleared = assets::get(&conn, a1)?;
    assert_eq!(cleared.rating, 0);
    // 非法评级拒绝
    assert!(assets::set_rating(&conn, &[a1], 6).is_err());
    Ok(())
}

#[test]
fn rating_facet_appears() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/g1.jpg", "g1.jpg", "jpg", "image/jpeg");
    assets::set_rating(&conn, &[a1], 5)?;
    let facets = assets::list_metadata_facets(&conn, None)?;
    let rating = facets.iter().find(|f| f.key == "rating").expect("评级分面");
    assert!(
        rating.items.iter().any(|i| i.value == "5"),
        "评级分面应有 5 星桶"
    );
    assert!(facets.iter().all(|f| f.key != "favorite"));
    Ok(())
}

// ── W2-9：Top-N 标签 ──

#[test]
fn top_tags_orders_by_usage() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/t1.jpg", "t1.jpg", "jpg", "image/jpeg");
    let a2 = add_asset(&conn, "d:/p/t2.jpg", "t2.jpg", "jpg", "image/jpeg");
    let hot = tags::create_in_facet(&conn, "高频", None, Some("scene"))?;
    let cold = tags::create_in_facet(&conn, "低频", None, Some("scene"))?;
    asset_tags::assign(&conn, &[a1, a2], &[hot.id], "manual")?; // 2 次使用
    asset_tags::assign(&conn, &[a1], &[cold.id], "manual")?; // 1 次使用
    let top = tags::top_tags_per_facet(&conn, 20)?;
    let scene = top.iter().find(|(f, _)| f == "scene").expect("scene 组");
    let words = scene.1.clone();
    let hot_pos = words.find("高频").expect("高频在列");
    let cold_pos = words.find("低频").expect("低频在列");
    assert!(hot_pos < cold_pos, "使用多的排前：{words}");
    Ok(())
}

#[test]
fn top_tags_respects_char_cap() -> AppResult<()> {
    let conn = setup();
    let a1 = add_asset(&conn, "d:/p/c1.jpg", "c1.jpg", "jpg", "image/jpeg");
    // 造 500 个长名标签（每个截断为 12 字），足够触发 5000 上限
    for i in 0..500 {
        let name = format!("超长标签名称第{:03}号占位", i);
        let t = tags::create_in_facet(&conn, &name, None, Some("scene"))?;
        asset_tags::assign(&conn, &[a1], &[t.id], "manual")?;
    }
    let top = tags::top_tags_per_facet(&conn, 600)?;
    let words_total: usize = top.iter().map(|(_, w)| w.chars().count()).sum();
    assert!(
        words_total <= 5000,
        "候选词总字符应受 5000 上限约束，实际 {words_total}"
    );
    // 每个分面至少保留 1 个词（大分面不得把词丢光）
    assert!(
        top.iter().all(|(_, w)| !w.is_empty()),
        "每分面至少 1 个词：{top:?}"
    );
    Ok(())
}

// ── W5h：同源文件组（RAW+JPG）──
fn add_kinship_pair(conn: &rusqlite::Connection) -> (i64, i64) {
    let jpg = add_asset(conn, "d:/all/_0001.JPG", "_0001.JPG", "jpg", "image/jpeg");
    let raw = add_asset(conn, "d:/all/_0001.RW2", "_0001.RW2", "rw2", "image/x-raw");
    (jpg, raw)
}

#[test]
fn assign_syncs_to_kinship_siblings() -> AppResult<()> {
    let conn = setup();
    let (jpg, raw) = add_kinship_pair(&conn);
    let t = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    // 给 JPG 打标 → RAW 也有
    asset_tags::assign(&conn, &[jpg], &[t.id], "manual")?;
    let raw_tags = asset_tags::get_asset_tags(&conn, raw)?;
    assert!(
        raw_tags.iter().any(|x| x.id == t.id),
        "给 JPG 打标应同步到同源 RAW"
    );
    Ok(())
}

#[test]
fn ambiguous_kinship_group_does_not_expand_tag_assignment() -> AppResult<()> {
    let conn = setup();
    let jpg = add_asset(&conn, "d:/all/X.JPG", "X.JPG", "jpg", "image/jpeg");
    let raw = add_asset(&conn, "d:/all/X.RW2", "X.RW2", "rw2", "image/x-raw");
    let png = add_asset(&conn, "d:/all/X.PNG", "X.PNG", "png", "image/png");
    let tag = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;

    asset_tags::assign(&conn, &[jpg], &[tag.id], "manual")?;

    assert_eq!(asset_tags::get_asset_tags(&conn, jpg)?.len(), 1);
    assert!(asset_tags::get_asset_tags(&conn, raw)?.is_empty());
    assert!(asset_tags::get_asset_tags(&conn, png)?.is_empty());
    Ok(())
}

#[test]
fn restoring_rejected_suggestion_clears_stale_error() -> AppResult<()> {
    let conn = setup();
    let id = add_asset(
        &conn,
        "d:/p/restore-error.jpg",
        "restore-error.jpg",
        "jpg",
        "image/jpeg",
    );
    let batch = ai::create_batch(&conn, &[id], "local")?;
    let suggestion = ai::list_suggestions(&conn, batch.id)?.remove(0);
    ai::set_suggestion_error(&conn, suggestion.id, "旧的模型失败原因")?;
    ai::reject_suggestion(&conn, suggestion.id)?;

    ai::restore_suggestion(&conn, suggestion.id)?;

    let restored = ai::list_suggestions(&conn, batch.id)?.remove(0);
    assert_eq!(restored.status, "pending");
    assert_eq!(restored.last_error, None);
    Ok(())
}

#[test]
fn assign_records_ops_for_both() -> AppResult<()> {
    let conn = setup();
    let (jpg, _raw) = add_kinship_pair(&conn);
    let t = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    asset_tags::assign(&conn, &[jpg], &[t.id], "manual")?;
    // tag_ops 两行（每条真实写入各一行 → undo_batch 可对称回滚）
    let ops: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_ops WHERE tag_id = ?1",
        [t.id],
        |r| r.get(0),
    )?;
    assert_eq!(ops, 2, "同源写入必须各记一行流水（jpg + raw）");
    Ok(())
}

#[test]
fn undo_reverts_both() -> AppResult<()> {
    let conn = setup();
    let (jpg, raw) = add_kinship_pair(&conn);
    let _t = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    // R2-4：走真实 assign_inner 同源展开（ai::confirm_suggestion），不手写 INSERT
    let batch = ai::create_batch(&conn, &[jpg], "cloud")?;
    let sug = ai::list_suggestions(&conn, batch.id)?
        .into_iter()
        .next()
        .unwrap();
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["海边".to_string()])]);
    ai::confirm_suggestion(&conn, sug.id, &tags_map)?;
    // assign_inner 同源展开写了两条（jpg + raw）且都带本批次流水
    let ops: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_ops WHERE batch_id = ?1",
        [batch.id],
        |r| r.get(0),
    )?;
    assert_eq!(ops, 2, "同源展开应写两条 add 流水");
    let undone = tag_ops::undo_batch(&conn, batch.id)?;
    assert!(undone > 0);
    let jpg_tags = asset_tags::get_asset_tags(&conn, jpg)?;
    let raw_tags = asset_tags::get_asset_tags(&conn, raw)?;
    assert!(jpg_tags.is_empty(), "撤销应回滚 JPG 的标签");
    assert!(raw_tags.is_empty(), "撤销应回滚同源 RAW 的标签");
    Ok(())
}

#[test]
fn confirming_ai_tags_reviews_kinship_sibling_rows() -> AppResult<()> {
    let conn = setup();
    let (jpg, raw) = add_kinship_pair(&conn);
    let batch = ai::create_batch(&conn, &[jpg], "local")?;
    let suggestion = ai::list_suggestions(&conn, batch.id)?
        .into_iter()
        .next()
        .expect("JPG suggestion");
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["海边".to_string()])]);

    ai::confirm_suggestion(&conn, suggestion.id, &tags_map)?;

    let states: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT asset_id, review_state FROM asset_tags
              WHERE source_batch_id = ?1 ORDER BY asset_id",
        )?;
        let rows = stmt
            .query_map([batch.id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        rows
    };
    assert_eq!(
        states,
        vec![(jpg, "ai_reviewed".into()), (raw, "ai_reviewed".into())],
        "确认代表图后，同批同步到 RAW/JPG 的标签都必须受审核保护"
    );

    let removed = asset_tags::retag_clear_unreviewed(&conn, &[jpg, raw])?;
    assert_eq!(removed, 0, "ReplaceAiOnly 不得清理已确认的同源标签");
    Ok(())
}

#[test]
fn sync_off_does_not_touch_sibling() -> AppResult<()> {
    let conn = setup();
    let (jpg, raw) = add_kinship_pair(&conn);
    // 关闭同源同步
    conn.execute(
        "UPDATE settings SET value = json_set(value, '$.appearance.kinship.syncTagsToSiblings', json('false'))
          WHERE key = 'app_settings'",
        [],
    )
    .or_else(|_| {
        // settings 行可能不存在（内存库未写过）：写一份最小 JSON
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('app_settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [r#"{"appearance":{"kinship":{"syncTagsToSiblings":false}}}"#],
        )
    })?;
    let t = tags::create_in_facet(&conn, "海边", None, Some("scene"))?;
    asset_tags::assign(&conn, &[jpg], &[t.id], "manual")?;
    let raw_tags = asset_tags::get_asset_tags(&conn, raw)?;
    assert!(raw_tags.is_empty(), "关闭同步后不得动同源文件");
    Ok(())
}

#[test]
fn create_batch_dedups_kinship() -> AppResult<()> {
    let conn = setup();
    let (jpg, raw) = add_kinship_pair(&conn);
    let batch = ai::create_batch(&conn, &[jpg, raw], "cloud")?;
    // 同源组只保留一个代表 → total = 1
    assert_eq!(batch.total, 1, "同源组应去重（2 张 → 1 次请求）");
    // 代表是 JPG（非 RAW 优先）
    let rep: i64 = conn.query_row(
        "SELECT asset_id FROM ai_suggestions WHERE batch_id = ?1",
        [batch.id],
        |r| r.get(0),
    )?;
    assert_eq!(rep, jpg, "代表应为非 RAW（JPG 解码快有内嵌预览）");
    Ok(())
}

#[test]
fn ambiguous_kinship_group_keeps_each_selected_ai_suggestion() -> AppResult<()> {
    let conn = setup();
    let jpg = add_asset(&conn, "d:/all/X.JPG", "X.JPG", "jpg", "image/jpeg");
    let raw = add_asset(&conn, "d:/all/X.RW2", "X.RW2", "rw2", "image/x-raw");
    let png = add_asset(&conn, "d:/all/X.PNG", "X.PNG", "png", "image/png");

    // 完整库里 1 RAW + 2 非 RAW 是歧义组；即使本次只选 RAW/JPG，也不能静默折叠。
    let partial = ai::create_batch(&conn, &[raw, jpg], "cloud")?;
    assert_eq!(partial.total, 2);
    let all = ai::create_batch(&conn, &[jpg, raw, png], "cloud")?;
    assert_eq!(all.total, 3);
    Ok(())
}

#[test]
fn batch_total_reflects_dedup() -> AppResult<()> {
    let conn = setup();
    // 两组同源 + 一张独立 = 5 张 → 3 次请求
    let j1 = add_asset(&conn, "d:/a/A.JPG", "A.JPG", "jpg", "image/jpeg");
    let r1 = add_asset(&conn, "d:/a/A.RW2", "A.RW2", "rw2", "image/x-raw");
    let j2 = add_asset(&conn, "d:/a/B.JPG", "B.JPG", "jpg", "image/jpeg");
    let r2 = add_asset(&conn, "d:/a/B.RW2", "B.RW2", "rw2", "image/x-raw");
    let solo = add_asset(&conn, "d:/a/C.JPG", "C.JPG", "jpg", "image/jpeg");
    let batch = ai::create_batch(&conn, &[j1, r1, j2, r2, solo], "cloud")?;
    assert_eq!(batch.total, 3, "5 张（两组同源 + 1 独立）应去重为 3 次请求");
    Ok(())
}
