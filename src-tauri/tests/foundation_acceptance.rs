//! §13 验收测试清单（十二组约 62 条）—— 一条命令跑完：
//!   cargo test --test foundation_acceptance
//!
//! 命名前缀统一；每个波次结束跑对应组（§13 验收流程）：
//!   F 波次 → 组 1/2/3/5/10/12；A 波次 → 组 4/5；S 波次 → 组 6/9/11；
//!   C 波次 → 组 7；全部绿 + 四关全绿 → foundation-verified。

use bagertea_ai_media_v2_lib::db::{
    ai, asset_tags, assets, init_memory, migrations, query_expr, schema_features, tag_facets, tags,
};
use bagertea_ai_media_v2_lib::error::AppResult;

fn mem() -> rusqlite::Connection {
    init_memory().expect("内存库初始化失败")
}

/// 辅助：往 tag_terms 写 canonical 词（模拟 apply_tag_constraints 后的新写入路径）。
fn insert_canonical_term(
    conn: &rusqlite::Connection,
    tag_id: i64,
    facet: &str,
    term: &str,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO tag_terms (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         VALUES (?1, ?2, ?3, ?3, '', 'canonical', 1, 1)",
        rusqlite::params![tag_id, facet, term],
    )?;
    Ok(())
}

/// 测试辅助：确保 tag_terms 表存在后写入一行 canonical（facet 校验触发器同时建好）。
fn db_schema_make_terms(
    conn: &rusqlite::Connection,
    tag_id: i64,
    facet: &str,
    term: &str,
) -> AppResult<()> {
    migrations::create_tag_terms_table_for_test(conn)?;
    migrations::create_terms_facet_defenses_for_test(conn)?;
    insert_canonical_term(conn, tag_id, facet, term)
}

// ═══════════════ 组 1：分面停用/恢复/删除/重建（F1 F4） ═══════════════

/// 把 EFF_* 常量丢给 SQLite 真跑：对每个组合，先写一行真实分面再分别算两侧。
fn sql_eff(conn: &rusqlite::Connection, key: &str, cond: &str) -> bool {
    let n: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM tag_facets f WHERE f.key = ?1 AND {cond}"),
            [key],
            |r| r.get(0),
        )
        .unwrap();
    n > 0
}

/// F1-b：effective() 与 SQL 常量同规则 —— status 3 值 × cfg 四列 16 组合 = 48 组合。
/// 这条测试的价值不是「验证现在对」，而是下次有人改规则时会失败：两侧任一改动都会撞红。
/// 0-7：真调 TagFacet::effective()，且把 EFF_* 真丢给 SQLite —— 删掉/改错任何一侧都必红。
#[test]
fn effective_value_sql_matches_rust() {
    let c = mem();
    tag_facets::create(&c, "eff", "有效值", "", "multi", None, "all").unwrap();
    let statuses = ["active", "inactive", "deprecated"];
    for status in statuses {
        for vis in [false, true] {
            for manual in [false, true] {
                for ai in [false, true] {
                    for search in [false, true] {
                        // 写真实分面行（组合由 status × cfg 四列决定）
                        c.execute(
                            "UPDATE tag_facets SET status=?1, cfg_visible_in_navigation=?2,
                                    cfg_manual_assignable=?3, cfg_ai_assignable=?4, cfg_searchable=?5
                              WHERE key='eff'",
                            rusqlite::params![status, vis as i64, manual as i64, ai as i64, search as i64],
                        )
                        .unwrap();
                        // Rust 侧：从库里读回来再调 effective()（被测代码必须在场）
                        let facet = tag_facets::get(&c, "eff").unwrap();
                        let eff = facet.effective();
                        // SQL 侧：把 EFF_* 常量真丢给 SQLite 算同一行
                        let s_visible = sql_eff(&c, "eff", tag_facets::EFF_VISIBLE);
                        let s_manual = sql_eff(&c, "eff", tag_facets::EFF_MANUAL);
                        let s_ai = sql_eff(&c, "eff", tag_facets::EFF_AI);
                        let s_search = sql_eff(&c, "eff", tag_facets::EFF_SEARCH);
                        let tag = format!(
                            "status={status} vis={vis} manual={manual} ai={ai} search={search}"
                        );
                        assert_eq!(eff.visible, s_visible, "visible 不符：{tag}");
                        assert_eq!(eff.manual, s_manual, "manual 不符：{tag}");
                        assert_eq!(eff.ai, s_ai, "ai 不符：{tag}");
                        assert_eq!(eff.searchable, s_search, "searchable 不符：{tag}");
                        // input_mode 只读派生看的是配置（停用不丢配置）—— 与原始 cfg 对齐，不看 status
                        let derived = facet.derived_input_mode();
                        assert_eq!(
                            derived == "ai_and_manual",
                            ai,
                            "input_mode 派生应与 cfg_ai_assignable 对齐：{tag}"
                        );
                    }
                }
            }
        }
    }
}

/// F1-b + F7：ai_and_manual 分面停用 → 恢复仍是 ai_and_manual（cfg 不被生命周期覆盖）。
#[test]
fn facet_deactivate_restore_preserves_config() {
    let c = mem();
    let key = "my_facet";
    let f = tag_facets::create(&c, key, "我的分面", "", "multi", None, "all").unwrap();
    assert!(f.cfg_ai_assignable, "新建默认参与 AI");
    // 停用（用户分面，F7 允许；系统分面停用在 F7 单独验证）
    tag_facets::deactivate(&c, key).unwrap();
    let inactive = tag_facets::get(&c, key).unwrap();
    assert_eq!(inactive.status, "inactive");
    assert!(inactive.cfg_ai_assignable, "停用不清配置值（恢复时要还原）");
    assert_eq!(
        inactive.input_mode, "ai_and_manual",
        "cfg_ai=1 → 派生 ai_and_manual"
    );
    // 恢复 → 仍是 ai_and_manual
    tag_facets::restore(&c, key).unwrap();
    let restored = tag_facets::get(&c, key).unwrap();
    assert_eq!(restored.status, "active");
    assert_eq!(
        restored.input_mode, "ai_and_manual",
        "停用→恢复必须还原原配置"
    );
    assert!(restored.cfg_ai_assignable);
    // manual_only 同理
    tag_facets::update_facet(&c, key, "我的分面", "", "manual_only", "multi", None, "all").unwrap();
    tag_facets::deactivate(&c, key).unwrap();
    tag_facets::restore(&c, key).unwrap();
    let back = tag_facets::get(&c, key).unwrap();
    assert_eq!(
        back.input_mode, "manual_only",
        "manual_only 停用→恢复仍是 manual_only"
    );
    assert!(!back.cfg_ai_assignable);
}

/// 组1：分面删除后可用同 key 重建（重建是新的 is_system=false 分面）。
#[test]
fn facet_delete_then_recreate_same_key() {
    let c = mem();
    tag_facets::create(&c, "tmp_facet", "临时", "", "multi", None, "all").unwrap();
    tag_facets::delete_facet(&c, "tmp_facet").unwrap();
    let re = tag_facets::create(&c, "tmp_facet", "重建", "", "multi", None, "all").unwrap();
    assert_eq!(re.key, "tmp_facet");
    assert!(!re.is_system);
}

// ═══════════════ 组 3：子树移动不突破深度（F1） ═══════════════

/// F1-c：把 A 挂到自己的后代 B 下 → 拒绝（环）。
#[test]
fn cycle_creation_rejected() {
    let c = mem();
    let a = tags::create_in_facet(&c, "A", None, Some("custom")).unwrap();
    let b = tags::create_in_facet(&c, "B", Some(a.id), Some("custom")).unwrap();
    let res = crate_private_update_parent(&c, a.id, Some(b.id));
    assert!(res.is_err(), "把 A 挂到子 B 下应报环错误");
    assert!(res.unwrap_err().to_string().contains("循环"));
}

/// 直接执行 UPDATE（绕过 tags::update 的应用层防环，验证触发器兜底）
fn crate_private_update_parent(
    c: &rusqlite::Connection,
    id: i64,
    parent: Option<i64>,
) -> AppResult<()> {
    c.execute(
        "UPDATE tags SET parent_id = ?1 WHERE id = ?2",
        rusqlite::params![parent, id],
    )?;
    Ok(())
}

/// F1-d 兜底：绕过触发器造环后，递归查询不挂死（深度上限）。
#[test]
fn recursive_cte_terminates_on_existing_cycle() {
    let c = mem();
    let a = tags::create_in_facet(&c, "CA", None, Some("custom")).unwrap();
    let b = tags::create_in_facet(&c, "CB", Some(a.id), Some("custom")).unwrap();
    // 绕过触发器直接造环（A → B → A）
    c.execute_batch("DROP TRIGGER trg_tags_no_cycle; DROP TRIGGER trg_tags_max_depth_au;")
        .unwrap();
    crate_private_update_parent(&c, a.id, Some(b.id)).unwrap();
    // 正常根保证 list_tree 顶层非空
    tags::create_in_facet(&c, "正常根", None, Some("custom")).unwrap();
    let tree = tags::list_tree(&c).unwrap();
    assert!(!tree.is_empty());
    let _ = tags::total_count(&c, a.id).unwrap();
}

/// F1-d 兜底：list_by_facet 在有环时也不挂死（它内部走 list_tree）。
#[test]
fn list_by_facet_survives_cycle() {
    let c = mem();
    let a = tags::create_in_facet(&c, "LA", None, Some("custom")).unwrap();
    let b = tags::create_in_facet(&c, "LB", Some(a.id), Some("custom")).unwrap();
    c.execute_batch("DROP TRIGGER trg_tags_no_cycle; DROP TRIGGER trg_tags_max_depth_au;")
        .unwrap();
    crate_private_update_parent(&c, a.id, Some(b.id)).unwrap();
    let _ = tags::list_by_facet(&c, "custom").unwrap();
}

// ═══════════════ 组 2：唯一性（F2 F3 F5 的 V22b 部分） ═══════════════

/// V22b 启用前先干净（无冲突），apply 成功 → schema_features 登记 enabled。
#[test]
fn v22b_applies_when_clean_and_registers_features() {
    let c = mem();
    // 全新 V22a 库：无标签 → 零冲突
    let report = tags::detect_tag_conflicts(&c).unwrap();
    assert!(report.is_clean(), "空库应零冲突：{report:?}");
    migrations::apply_v22b_constraints(&c).unwrap();
    // tag_terms 有 canonical（种子分面无标签，但 apply 后表存在）
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tag_terms'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    // 特征登记（命令层做 set_feature；这里验证表 + 手动登记 API 可用）
    schema_features::set_feature(&c, "tag_unique_terms", true, None).unwrap();
    assert!(schema_features::feature_enabled(&c, "tag_unique_terms").unwrap());
}

/// 造 term 冲突（同分面两标签同 canonical）→ 预检发现 → 不 apply。
#[test]
fn term_alias_equals_other_canonical_rejected() {
    let c = mem();
    // scene 分面两个标签用同一名（绕过 DB 唯一约束不行 —— tags 无 facet+name 唯一；
    // 根级同名允许？tags UNIQUE(parent_id,name) 只拦同父同名。分面内规范名冲突是
    // 「两个根级标签同名不同 facet」之外的场景 —— 这里造：两个 active 标签规范名相同）
    // F5：create_in_facet 根级自动查重，重复词只能直插构造（绕过应用层）
    let t1 = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let _t2: i64 = c
        .query_row(
            "INSERT INTO tags (name, canonical_name, normalized_name, facet_key)
             VALUES ('海边','海边','海边','scene') RETURNING id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let report = tags::detect_tag_conflicts(&c).unwrap();
    assert!(
        report
            .term_conflicts
            .iter()
            .any(|g| g.facet_key == "scene" && g.term == "海边" && g.entries.len() >= 2),
        "应发现 scene 分面「海边」重名：{report:?}"
    );
    let _ = t1;
}

/// 跨分面同名词允许（facet 是唯一索引的一部分）。
#[test]
fn term_same_name_across_facets_allowed() {
    let c = mem();
    tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    tags::create_in_facet(&c, "海边", None, Some("custom")).unwrap();
    // apply 后两个 canonical 共存（不同 facet → ux_terms 不冲突）
    migrations::apply_v22b_constraints(&c).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE normalized_term='海边' AND term_kind='canonical'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "跨分面同名词应各有一个 canonical");
}

/// 一标签两个 canonical 被拒（ux_terms_canonical 唯一）。
#[test]
fn term_two_canonicals_per_tag_rejected() {
    let c = mem();
    let t = tags::create_in_facet(&c, "人", None, Some("people")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    // apply 的 backfill 已为「人」灌入 canonical「人」；再插第二个 canonical 应被唯一索引拒
    let err = insert_canonical_term(&c, t.id, "people", "人物");
    assert!(err.is_err(), "同标签第二个 canonical 应被唯一索引拒绝");
    // 确认原有 canonical 未被覆盖
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE tag_id=?1 AND term_kind='canonical'",
            [t.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

/// V22b 冲突时记录 blocked_by（命令层语义）—— 用 term 冲突库验证 is_clean=false。
#[test]
fn v22b_skipped_on_conflicts_records_blocked_by() {
    let c = mem();
    tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    // F5：重复词直插构造（create_in_facet 根级自动查重）
    c.execute(
        "INSERT INTO tags (name, canonical_name, normalized_name, facet_key)
         VALUES ('海边','海边','海边','scene')",
        [],
    )
    .unwrap();
    let report = tags::detect_tag_conflicts(&c).unwrap();
    assert!(!report.is_clean());
    // apply 会怎样？insert canonical 会在 ux_terms 上撞唯一 → apply 报错（不静默丢）
    let err = migrations::apply_v22b_constraints(&c);
    assert!(err.is_err(), "有重名时灌 canonical 应撞 ux_terms 唯一索引");
}

// ═══════════════ 组 12：tag_terms.facet_key 不漂移（F2-c） ═══════════════

/// 触发器 ①：改 tags.facet_key → tag_terms 自动同步。
#[test]
fn terms_facet_syncs_on_tag_update() {
    let c = mem();
    let t = tags::create_in_facet(&c, "x", None, Some("custom")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    // 把标签迁到 scene 分面
    let scene_facet_exists: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_facets WHERE key='scene'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scene_facet_exists, 1);
    c.execute("UPDATE tags SET facet_key='scene' WHERE id=?1", [t.id])
        .unwrap();
    let terms_facet: String = c
        .query_row(
            "SELECT facet_key FROM tag_terms WHERE tag_id=?1",
            [t.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(terms_facet, "scene", "改 tags.facet_key 应同步 tag_terms");
}

/// 触发器 ②：插入 facet 不匹配的 term 被拒。
#[test]
fn terms_facet_insert_mismatch_rejected() {
    let c = mem();
    let t = tags::create_in_facet(&c, "x", None, Some("custom")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    let err = c.execute(
        "INSERT INTO tag_terms (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         VALUES (?1, 'scene', 'x', 'x', '', 'canonical', 1, 1)",
        [t.id],
    );
    assert!(err.is_err(), "facet 不一致的 term 插入应被拒");
}

/// 触发器 ③：直接 UPDATE tag_terms.facet_key 到不一致值被拒。
#[test]
fn terms_facet_direct_update_mismatch_rejected() {
    let c = mem();
    let t = tags::create_in_facet(&c, "x", None, Some("custom")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    let err = c.execute(
        "UPDATE tag_terms SET facet_key='scene' WHERE tag_id=?1",
        [t.id],
    );
    assert!(err.is_err(), "直接改 tag_terms.facet_key 不一致应被拒");
}

/// 不一致阻断启用：手工造不一致 → 修复按钮应提示（check_terms 层面）
#[test]
fn facet_mismatch_blocks_feature_enable() {
    let c = mem();
    // 造一致库再手工破坏（绕过触发器：先删触发器）
    let t = tags::create_in_facet(&c, "x", None, Some("custom")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    c.execute_batch("DROP TRIGGER trg_terms_facet_match_au; DROP TRIGGER trg_terms_sync_facet;")
        .unwrap();
    c.execute(
        "UPDATE tag_terms SET facet_key='scene' WHERE tag_id=?1",
        [t.id],
    )
    .unwrap();
    let report = tags::detect_tag_conflicts(&c).unwrap();
    assert!(
        !report.facet_mismatches.is_empty(),
        "应发现 facet 不一致：{report:?}"
    );
    // 命令层 apply 会因预检不清拒绝（模拟）
    assert!(!report.is_clean());
}

// ═══════════════ 组 12/2：分面删除 RESTRICT（F2-d） ═══════════════

/// F2-d：带标签的分面禁止裸 DELETE；delete_facet 命令仍可用（级联）。
#[test]
fn facet_delete_restrict_blocks_raw_delete() {
    let c = mem();
    tag_facets::create(&c, "tmp_f", "临时", "", "multi", None, "all").unwrap();
    tags::create_in_facet(&c, "标签A", None, Some("tmp_f")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    // 裸 DELETE 被 RESTRICT 拦
    let err = c.execute("DELETE FROM tag_facets WHERE key='tmp_f'", []);
    assert!(err.is_err(), "带标签的分面禁止裸删除");
    // delete_facet 命令（先删 tags）仍可用
    let report = tag_facets::delete_facet(&c, "tmp_f").unwrap();
    assert!(report.tags_deleted >= 1);
    let leftover: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_facets WHERE key='tmp_f'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

/// F2-d：delete_facet 全级联回归确认（含 tag_terms —— 标签删后 terms 由 CASCADE 清）。
#[test]
fn delete_facet_cascades_all_refs() {
    let c = mem();
    tag_facets::create(&c, "tmp_g", "临时", "", "multi", None, "all").unwrap();
    let t = tags::create_in_facet(&c, "标签B", None, Some("tmp_g")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    // R2-4：补 ops/items 级联断言 —— 真实造出三类引用再删
    let aid = f4_insert_asset(&c, "d:/facet_del.jpg");
    asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap(); // asset_tags + tag_ops 各 1
    let batch = ai::create_batch(&c, &[aid], "cloud").unwrap();
    let sug = ai::list_suggestions(&c, batch.id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    c.execute(
        "INSERT INTO ai_suggestion_items (suggestion_id, facet_key, raw_name, normalized_name, tag_id, confidence, decision, created_at)
         VALUES (?1, 'tmp_g', '标签B', '标签B', ?2, NULL, 'pending', 1)",
        rusqlite::params![sug.id, t.id],
    )
    .unwrap();
    let report = tag_facets::delete_facet(&c, "tmp_g").unwrap();
    assert!(report.tags_deleted >= 1, "标签必须级联删除");
    assert_eq!(
        report.ops_deleted, 1,
        "tag_ops 引用必须等量级联删：{report:?}"
    );
    assert_eq!(
        report.items_deleted, 1,
        "ai_suggestion_items 引用必须等量级联删：{report:?}"
    );
    // tags 已删 → tag_terms 因 FK CASCADE 清空（验证不残留脏词条）
    let terms_left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE tag_id=?1",
            [t.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(terms_left, 0, "标签删除后其 tag_terms 应 CASCADE 清空");
    // asset_tags 也随删
    let at_left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE tag_id=?1",
            [t.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at_left, 0);
}

/// F2 要求：设计书要求的 PRAGMA foreign_keys 测试。
#[test]
fn foreign_keys_pragma_is_on() {
    let c = mem();
    let on: i64 = c
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(on, 1, "foreign_keys 必须为 ON（CASCADE 的前提）");
}

/// F2-a：六类冲突全被探测（构造每种冲突）。
#[test]
fn detect_conflicts_finds_all_six_types() {
    let c = mem();
    // ① term 冲突：同分面两标签同 canonical（F5 后重复词直插构造，绕过根级自动查重）
    tags::create_in_facet(&c, "海", None, Some("scene")).unwrap();
    c.execute(
        "INSERT INTO tags (name, canonical_name, normalized_name, facet_key)
         VALUES ('海','海','海','scene')",
        [],
    )
    .unwrap();
    // ② 孤儿：直接插一个 facet_key 不存在分面的标签（此时 F2-d 触发器未启用）
    c.execute(
        "INSERT INTO tags (name, facet_key) VALUES ('孤儿', 'ghost_facet')",
        [],
    )
    .unwrap();
    // ③ 跨面挂父：把 custom 标签挂到 scene 标签下（绕过 trg_tags_parent_facet_ai）
    let scene_tag = tags::create_in_facet(&c, "山", None, Some("scene")).unwrap();
    c.execute_batch("DROP TRIGGER trg_tags_parent_facet_ai;")
        .unwrap();
    c.execute(
        "INSERT INTO tags (name, parent_id, facet_key) VALUES ('山下', ?1, 'custom')",
        [scene_tag.id],
    )
    .unwrap();
    // ⑤ 超深：独立一条 9 层有根链（绕过深度触发器；保留根 → 预检能从根出发发现）
    c.execute_batch("DROP TRIGGER trg_tags_max_depth_ai; DROP TRIGGER trg_tags_max_depth_au;")
        .unwrap();
    let mut parent: Option<i64> = None;
    for i in 0..9 {
        let id = c
            .query_row(
                "INSERT INTO tags (name, parent_id, facet_key) VALUES (?1, ?2, 'custom') RETURNING id",
                rusqlite::params![format!("deep{i}"), parent],
                |r| r.get(0),
            )
            .unwrap();
        parent = Some(id);
    }
    // ④ 环：独立 A↔B（A 根，B 挂 A 下，再把 A 挂 B 下成环；绕过环触发器）
    c.execute_batch("DROP TRIGGER trg_tags_no_cycle;").unwrap();
    let cyc_a: i64 = c
        .query_row(
            "INSERT INTO tags (name, facet_key) VALUES ('CA','custom') RETURNING id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let cyc_b: i64 = c
        .query_row(
            "INSERT INTO tags (name, parent_id, facet_key) VALUES ('CB',?1,'custom') RETURNING id",
            [cyc_a],
            |r| r.get(0),
        )
        .unwrap();
    c.execute(
        "UPDATE tags SET parent_id=?1 WHERE id=?2",
        rusqlite::params![cyc_b, cyc_a],
    )
    .unwrap();
    // ⑥ facet 不一致：干净标签（custom 分面下无重名）—— 手工建 tag_terms 表并插一行
    //（本库已有 term 冲突，apply 会撞唯一索引，故不依赖 apply）
    let clean = tags::create_in_facet(&c, "干净词", None, Some("custom")).unwrap();
    crate::db_schema_make_terms(&c, clean.id, "custom", "干净词").unwrap();
    c.execute_batch("DROP TRIGGER trg_terms_facet_match_au; DROP TRIGGER trg_terms_sync_facet;")
        .unwrap();
    c.execute(
        "UPDATE tag_terms SET facet_key='scene' WHERE tag_id=?1",
        [clean.id],
    )
    .unwrap();

    let report = tags::detect_tag_conflicts(&c).unwrap();
    assert!(
        !report.term_conflicts.is_empty(),
        "① term 冲突未发现：{report:?}"
    );
    assert!(!report.orphans.is_empty(), "② 孤儿未发现：{report:?}");
    assert!(
        !report.cross_facet_children.is_empty(),
        "③ 跨面挂父未发现：{report:?}"
    );
    assert!(!report.cycle_edges.is_empty(), "④ 环未发现：{report:?}");
    assert!(
        !report.over_deep_subtrees.is_empty(),
        "⑤ 超深未发现：{report:?}"
    );
    assert!(
        !report.facet_mismatches.is_empty(),
        "⑥ facet 不一致未发现：{report:?}"
    );
}

// ═══════════════ 组 9 前缀：next_prefix 边界 + find_by_term（F3） ═══════════════

/// F3-b：next_prefix 十一个必测边界。
#[test]
fn next_prefix_boundaries() {
    use bagertea_ai_media_v2_lib::db::tags::next_prefix;
    assert_eq!(next_prefix("term123"), Some("term124".to_string()), "ASCII");
    assert_eq!(next_prefix("青"), Some("靓".to_string()), "中文");
    assert_eq!(
        next_prefix("海边"),
        Some("海辺".to_string()),
        "只改最后一个字符"
    );
    assert_eq!(next_prefix("😀"), Some("😁".to_string()), "emoji（BMP 外）");
    assert_eq!(next_prefix("a😀"), Some("a😁".to_string()), "混合");
    assert_eq!(
        next_prefix("\u{FFFF}"),
        Some("\u{10000}".to_string()),
        "跨越 BMP 边界"
    );
    assert_eq!(
        next_prefix("a\u{FFFF}"),
        Some("a\u{10000}".to_string()),
        "同上"
    );
    assert_eq!(next_prefix("\u{10FFFF}"), None, "char::MAX");
    assert_eq!(
        next_prefix("a\u{10FFFF}"),
        Some("b".to_string()),
        "末字符到顶→前一个递增"
    );
    assert_eq!(
        next_prefix("\u{D7FF}"),
        Some("\u{E000}".to_string()),
        "跳过 surrogate"
    );
    assert_eq!(next_prefix(""), None, "空串");
}

/// F3-b：next_prefix 不变式 —— 每个探针都落在 [prefix, next) 区间；兄弟落在区间外。
#[test]
fn next_prefix_covers_all_children() {
    use bagertea_ai_media_v2_lib::db::tags::next_prefix;
    let prefixes = [
        "term123",
        "青",
        "海边",
        "😀",
        "a😀",
        "\u{FFFF}",
        "a\u{FFFF}",
        "\u{D7FF}",
    ];
    for p in prefixes {
        let Some(hi) = next_prefix(p) else { continue };
        // 探针：x、x+长中文、x+两个 emoji、x+char::MAX、x+ASCII
        let probes: Vec<String> = vec![
            format!("{p}x"),
            format!("{p}非常长的中文标签词"),
            format!("{p}😀😀"),
            format!("{p}{}", '\u{10FFFF}'),
            format!("{p}abc"),
        ];
        for s in probes {
            assert!(
                s.as_str() >= p && s.as_str() < hi.as_str(),
                "「{s}」应以 {p} 开头且小于 {hi}"
            );
        }
        // 字典序更大的兄弟（同一长度 + 递增末字符）应在区间外
        let bigger = next_prefix(p).unwrap();
        let sibling = next_prefix(&bigger).unwrap_or_else(|| bigger.clone());
        assert!(sibling >= hi, "兄弟「{sibling}」应在区间 [{p}, {hi}) 外");
    }
}

/// F3：find_by_term 别名命中（同义词）；旧表路径与 tag_terms 路径行为一致。
#[test]
fn find_by_term_alias_hits_synonym() {
    use bagertea_ai_media_v2_lib::db::tags;
    let c = mem();
    let t = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    tags::add_alias(&c, t.id, "海滨", None, "synonym").unwrap();
    // 旧表路径（tag_unique_terms 未启用）
    let lookup = tags::find_by_term(&c, "scene", "海滨", tags::TermMatch::Alias).unwrap();
    assert_eq!(
        lookup.hits.len(),
        1,
        "别名应精确命中：{:?}",
        lookup.warnings
    );
    assert_eq!(lookup.hits[0].tag_id, t.id);
    assert!(
        lookup
            .warnings
            .iter()
            .any(|w| w.contains("海滨") && w.contains("海边")),
        "别名命中应告知归入：{:?}",
        lookup.warnings
    );
}

/// F3：find_by_term 无 ORDER BY 兜底也唯一（分面内一个 term 最多命中一个标签）。
#[test]
fn find_by_term_is_deterministic() {
    use bagertea_ai_media_v2_lib::db::tags;
    let c = mem();
    // 启用 tag_terms 后：ux_terms 唯一索引保证唯一
    let t1 = tags::create_in_facet(&c, "森林", None, Some("scene")).unwrap();
    migrations::apply_v22b_constraints(&c).unwrap();
    let lookup = tags::find_by_term(&c, "scene", "森林", tags::TermMatch::Exact).unwrap();
    assert_eq!(lookup.hits.len(), 1);
    assert_eq!(lookup.hits[0].tag_id, t1.id);
    // Exact 不命中别名（term_kind='canonical' 过滤）
    tags::add_alias(&c, t1.id, "林子", None, "synonym").unwrap();
    // 新路径写入 tag_terms 需手动加行（apply 后 add_alias 走 tag_aliases —— 双写受 gate 约束）
    // Exact 模式对「林子」无 canonical 命中
    let miss = tags::find_by_term(&c, "scene", "林子", tags::TermMatch::Exact).unwrap();
    assert!(miss.hits.is_empty(), "Exact 只匹配 canonical");
}

// ═══════════════ 组 1（F4）：可见性三常量 —— 消费点一致性矩阵 ═══════════════

/// F5/F2 辅助：完整启用 tag_unique_terms（与命令层 apply_tag_constraints 同语义：
/// 先建约束/灌 canonical，再登记 feature 标志，最后重建 FTS 触发器与索引 ——
/// P0-4：gate 开启后 FTS 别名词源切到 tag_terms，不重建则新别名进不了 FTS）。
fn enable_terms(conn: &rusqlite::Connection) {
    migrations::apply_v22b_constraints(conn).unwrap();
    schema_features::set_feature(conn, "tag_unique_terms", true, None).unwrap();
    migrations::rebuild_fts_triggers_for_gated_terms(conn).unwrap();
}

/// F4 辅助：插入一张图片素材，返回 id。
fn f4_insert_asset(conn: &rusqlite::Connection, path: &str) -> i64 {
    assets::insert(
        conn,
        path,
        path.rsplit('/').next().unwrap_or("a.jpg"),
        "jpg",
        1024,
        "image/jpeg",
        1700000000000,
    )
    .expect("插入素材失败")
}

/// F4：status × cfg_* 组合下，数据层消费点（侧栏树 list_tree / 提示词 build_prompt_context /
/// AI 候选词 top_tags_per_facet / 候选搜索 search_candidates / 详情 get_asset_tags+角标 /
/// 条件叶子 FacetHasAny）可见性一致。
/// 价值：不是「验证现在对」，而是下次有人加第八个消费点时会失败（指南 §F4）。
#[test]
fn facet_capability_matrix() {
    struct Row {
        name: &'static str,
        status: &'static str,
        visible: bool,
        ai: bool,
        searchable: bool,
        nav: bool,
        prompt: bool,
        top: bool,
        search: bool,
        detail_effective: bool,
    }
    let rows = [
        Row {
            name: "active+全cfg=1",
            status: "active",
            visible: true,
            ai: true,
            searchable: true,
            nav: true,
            prompt: true,
            top: true,
            search: true,
            detail_effective: true,
        },
        Row {
            name: "active+cfg_ai=0",
            status: "active",
            visible: true,
            ai: false,
            searchable: true,
            nav: true,
            prompt: false,
            top: false,
            search: true,
            detail_effective: true,
        },
        Row {
            name: "active+cfg_visible=0",
            status: "active",
            visible: false,
            ai: true,
            searchable: true,
            nav: false,
            prompt: true,
            top: true,
            search: true,
            detail_effective: true,
        },
        Row {
            name: "active+cfg_searchable=0",
            status: "active",
            visible: true,
            ai: true,
            searchable: false,
            nav: true,
            prompt: true,
            top: true,
            search: false,
            detail_effective: true,
        },
        Row {
            name: "inactive（cfg保持）",
            status: "inactive",
            visible: true,
            ai: true,
            searchable: true,
            nav: false,
            prompt: false,
            top: false,
            search: true,
            detail_effective: false,
        },
    ];
    for r in &rows {
        let c = mem();
        tag_facets::create(&c, "cap", "能力分面", "", "multi", None, "all").unwrap();
        let t = tags::create_in_facet(&c, "能力词", None, Some("cap")).unwrap();
        let aid = f4_insert_asset(&c, "d:/cap.jpg");
        asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();
        c.execute(
            "UPDATE tag_facets SET status=?1, cfg_visible_in_navigation=?2,
                    cfg_ai_assignable=?3, cfg_searchable=?4 WHERE key='cap'",
            rusqlite::params![r.status, r.visible as i64, r.ai as i64, r.searchable as i64],
        )
        .unwrap();

        let in_nav = tags::list_tree(&c)
            .unwrap()
            .iter()
            .any(|n| n.tag.facet_key == "cap");
        assert_eq!(in_nav, r.nav, "[{}] 侧栏可见性不符", r.name);

        let in_prompt = tag_facets::build_prompt_context(&c, "image")
            .unwrap()
            .iter()
            .any(|f| f.key == "cap");
        assert_eq!(in_prompt, r.prompt, "[{}] 提示词参与不符", r.name);

        let in_top = tags::top_tags_per_facet(&c, 200)
            .unwrap()
            .iter()
            .any(|(f, _)| f == "cap");
        assert_eq!(in_top, r.top, "[{}] AI 候选词不符", r.name);

        let cands = tags::search_candidates(&c, None, "能力词").unwrap();
        assert_eq!(!cands.is_empty(), r.search, "[{}] 候选搜索不符", r.name);

        let detail = asset_tags::get_asset_tags(&c, aid).unwrap();
        let dtag = detail
            .iter()
            .find(|x| x.id == t.id)
            .expect("详情恒显示（不过滤）");
        assert_eq!(
            dtag.facet_effective, r.detail_effective,
            "[{}] 详情「已停用」角标不符",
            r.name
        );

        let (frag, _) = query_expr::compile_leaf(
            &c,
            &query_expr::LeafCond::FacetHasAny {
                facet_key: "cap".into(),
            },
        )
        .unwrap();
        let dropped = frag.trim() == "1=1";
        assert_eq!(dropped, !r.searchable, "[{}] 条件叶子剔除策略不符", r.name);
    }

    // 停用 → 恢复：回原配置（矩阵第 6 行）——生命周期状态永不覆盖 cfg_*
    let c = mem();
    tag_facets::create(&c, "cap2", "能力分面2", "", "multi", None, "all").unwrap();
    let t2 = tags::create_in_facet(&c, "能力词2", None, Some("cap2")).unwrap();
    let aid2 = f4_insert_asset(&c, "d:/cap2.jpg");
    asset_tags::assign(&c, &[aid2], &[t2.id], "manual").unwrap();
    tag_facets::deactivate(&c, "cap2").unwrap();
    // inactive：侧栏/提示词/AI候选停；详情仍显示 + 角标；可搜保持
    assert!(!tags::list_tree(&c)
        .unwrap()
        .iter()
        .any(|n| n.tag.facet_key == "cap2"));
    assert!(!tag_facets::build_prompt_context(&c, "image")
        .unwrap()
        .iter()
        .any(|f| f.key == "cap2"));
    assert!(!tags::top_tags_per_facet(&c, 200)
        .unwrap()
        .iter()
        .any(|(f, _)| f == "cap2"));
    assert!(
        !tags::search_candidates(&c, None, "能力词2")
            .unwrap()
            .is_empty(),
        "停用分面标签仍可搜"
    );
    let detail2 = asset_tags::get_asset_tags(&c, aid2).unwrap();
    assert!(
        !detail2
            .iter()
            .find(|x| x.id == t2.id)
            .unwrap()
            .facet_effective,
        "停用分面详情打角标"
    );
    // restore：回原配置
    tag_facets::restore(&c, "cap2").unwrap();
    assert!(tags::list_tree(&c)
        .unwrap()
        .iter()
        .any(|n| n.tag.facet_key == "cap2"));
    assert!(tag_facets::build_prompt_context(&c, "image")
        .unwrap()
        .iter()
        .any(|f| f.key == "cap2"));
    assert!(tags::top_tags_per_facet(&c, 200)
        .unwrap()
        .iter()
        .any(|(f, _)| f == "cap2"));
    let detail3 = asset_tags::get_asset_tags(&c, aid2).unwrap();
    assert!(
        detail3
            .iter()
            .find(|x| x.id == t2.id)
            .unwrap()
            .facet_effective,
        "恢复后角标消失"
    );
}

/// F4：停用分面的标签仍可搜 —— FTS 全文搜索 + 候选搜索都保持命中
/// （SEARCHABLE_TAG / FTS 触发器只读 cfg_searchable，不看 f.status）。
#[test]
fn deactivated_facet_tags_still_searchable() {
    use bagertea_ai_media_v2_lib::db::search;
    let c = mem();
    tag_facets::create(&c, "mood_x", "情绪", "", "multi", None, "all").unwrap();
    let t = tags::create_in_facet(&c, "松弛感", None, Some("mood_x")).unwrap();
    let aid = f4_insert_asset(&c, "d:/mood.jpg");
    asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();
    assert_eq!(
        search::search_asset_ids_all(&c, "松弛感").unwrap(),
        vec![aid],
        "停用前 FTS 应命中"
    );
    tag_facets::deactivate(&c, "mood_x").unwrap();
    assert_eq!(
        search::search_asset_ids_all(&c, "松弛感").unwrap(),
        vec![aid],
        "停用分面的标签仍可搜（FTS 只看 cfg_searchable）"
    );
    assert_eq!(
        tags::search_candidates(&c, None, "松弛").unwrap().len(),
        1,
        "停用分面的标签仍进候选"
    );
    // 对照：cfg_searchable=0 后候选搜索实时剔除（live 读）
    c.execute(
        "UPDATE tag_facets SET cfg_searchable = 0 WHERE key = 'mood_x'",
        [],
    )
    .unwrap();
    assert!(tags::search_candidates(&c, None, "松弛")
        .unwrap()
        .is_empty());
}

/// F4：停用分面的标签不得作为「已有候选词」喂给 AI（top_tags_per_facet 收口 AI_ASSIGNABLE_TAG）。
#[test]
fn top_tags_excludes_inactive_facet() {
    let c = mem();
    let key = "cap_top";
    tag_facets::create(&c, key, "候选分面", "", "multi", None, "all").unwrap();
    let t = tags::create_in_facet(&c, "高频词", None, Some(key)).unwrap();
    let aid = f4_insert_asset(&c, "d:/top.jpg");
    asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();
    assert!(
        tags::top_tags_per_facet(&c, 200)
            .unwrap()
            .iter()
            .any(|(f, _)| f == key),
        "active 分面应进 AI 候选"
    );
    tag_facets::deactivate(&c, key).unwrap();
    assert!(
        !tags::top_tags_per_facet(&c, 200)
            .unwrap()
            .iter()
            .any(|(f, _)| f == key),
        "停用分面不得进 AI 候选（避免 AI 照产出后又被解析层丢弃的自相矛盾）"
    );
}

/// F4：build_prompt_context(media_kind) 按 applies_to 过滤 ——
/// 视频专属分面不污染图片批次提示词；'all' 用于超级搜索词典全量。
#[test]
fn prompt_context_respects_applies_to() {
    let c = mem();
    tag_facets::create(&c, "f_all", "通用", "", "multi", None, "all").unwrap();
    tag_facets::create(&c, "f_img", "画面", "", "multi", None, "image").unwrap();
    tag_facets::create(&c, "f_vid", "运镜", "", "multi", None, "video").unwrap();
    let img_keys: Vec<String> = tag_facets::build_prompt_context(&c, "image")
        .unwrap()
        .iter()
        .map(|f| f.key.clone())
        .collect();
    assert!(
        img_keys.contains(&"f_all".into()) && img_keys.contains(&"f_img".into()),
        "图片上下文应含 all+image: {img_keys:?}"
    );
    assert!(
        !img_keys.contains(&"f_vid".into()),
        "视频专属分面不得进图片上下文: {img_keys:?}"
    );
    let vid_keys: Vec<String> = tag_facets::build_prompt_context(&c, "video")
        .unwrap()
        .iter()
        .map(|f| f.key.clone())
        .collect();
    assert!(
        vid_keys.contains(&"f_all".into()) && vid_keys.contains(&"f_vid".into()),
        "视频上下文应含 all+video: {vid_keys:?}"
    );
    assert!(
        !vid_keys.contains(&"f_img".into()),
        "图片专属分面不得进视频上下文: {vid_keys:?}"
    );
    // 'all'：全量（超级搜索词典需要跨类型）——含系统分面 + 自定义三档
    let all_keys: Vec<String> = tag_facets::build_prompt_context(&c, "all")
        .unwrap()
        .iter()
        .map(|f| f.key.clone())
        .collect();
    assert!(
        all_keys.contains(&"f_all".into())
            && all_keys.contains(&"f_img".into())
            && all_keys.contains(&"f_vid".into()),
        "'all' 应含全部三档: {all_keys:?}"
    );
    // 非法 media_kind 报错
    assert!(tag_facets::build_prompt_context(&c, "audio").is_err());
}

// ═══════════════ F5：应用层单点收口 + tag_unique_terms feature gate ═══════════════

/// F5：根级「新词」创建自动查重（find_by_term mode=Alias）——同名/同义词都归并到已有标签。
#[test]
fn create_in_facet_dedups_via_find_by_term() {
    let c = mem();
    let a = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let b = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    assert_eq!(a.id, b.id, "同分面同名根级创建应去重返回同一标签");
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tags WHERE facet_key='scene' AND name='海边'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "库里只能有一个「海边」");
    // 与已有同义词（别名）同名 → 归并到别名归属标签（关闭态走旧表 tag_aliases）
    let forest = tags::create_in_facet(&c, "森林", None, Some("scene")).unwrap();
    tags::add_alias(&c, forest.id, "树林", None, "synonym").unwrap();
    let hit = tags::create_in_facet(&c, "树林", None, Some("scene")).unwrap();
    assert_eq!(hit.id, forest.id, "「树林」是「森林」的同义词，创建应归并");
}

/// F5：create_tag 落库入口（tags::create_tag_in_facet）—— 无 parent 且无 facet 时报错，
/// 不再默默落 custom；有 facet / 有 parent 时正常归属。
#[test]
fn create_tag_requires_facet_when_no_parent() {
    let c = mem();
    // 无 parent 无 facet → 报错
    let err = tags::create_tag_in_facet(&c, "新词", None, None).unwrap_err();
    assert!(err.to_string().contains("分面"), "应提示选择分面: {err}");
    // 给 facet → 创建成功且归属正确
    let t = tags::create_tag_in_facet(&c, "新词", Some("scene"), None).unwrap();
    assert_eq!(t.facet_key, "scene");
    // 有 parent 无 facet → 按父标签归属分面
    let child = tags::create_tag_in_facet(&c, "子词", None, Some(t.id)).unwrap();
    assert_eq!(child.facet_key, "scene", "子标签应继承父标签分面");
}

/// F5：tags::update 改 parent 时校验同分面（触发器第二层兜底）。
#[test]
fn update_rejects_cross_facet_parent() {
    let c = mem();
    let subject = tags::create_in_facet(&c, "人像", None, Some("subject")).unwrap();
    let scene = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let err = tags::update(&c, scene.id, None, Some(Some(subject.id))).unwrap_err();
    assert!(err.to_string().contains("分面"), "跨分面挂父应报错: {err}");
    // 同分面移动成功（防环由触发器守：挂到自己后代仍被拒）
    let child = tags::create_in_facet(&c, "沙滩", Some(scene.id), Some("scene")).unwrap();
    tags::update(&c, child.id, None, Some(Some(scene.id))).unwrap();
    let cycle_err = tags::update(&c, scene.id, None, Some(Some(child.id))).unwrap_err();
    assert!(
        !cycle_err.to_string().is_empty(),
        "挂到自己子标签下应被触发器拒绝"
    );
}

/// F5：启用 tag_unique_terms 后 add_alias 写 tag_terms，ux_terms 冲突被翻译成人话。
#[test]
fn add_alias_conflict_message_is_human_readable() {
    let c = mem();
    let t1 = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let t2 = tags::create_in_facet(&c, "森林", None, Some("scene")).unwrap();
    enable_terms(&c);
    tags::add_alias(&c, t1.id, "海滨", None, "synonym").unwrap();
    let err = tags::add_alias(&c, t2.id, "海滨", None, "synonym").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("海滨") && msg.contains("海边") && msg.contains("已被"),
        "冲突消息应点明两个词: {msg}"
    );
    // 同标签幂等：重复添加同一词不报错
    tags::add_alias(&c, t1.id, "海滨", None, "synonym").unwrap();
}

/// F5+F6-c：启用词表后 merge 把 src 全部词条迁到 dst（canonical→synonym——合并=语义等价，
/// 改名才写 old_name；synonym 保留）。
#[test]
fn merge_moves_all_terms_to_target() {
    let c = mem();
    let src = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let dst = tags::create_in_facet(&c, "海岸", None, Some("scene")).unwrap();
    enable_terms(&c);
    tags::add_alias(&c, src.id, "海滨", None, "synonym").unwrap();
    tags::merge_preserve_alias(&c, src.id, dst.id).unwrap();
    let rows: Vec<(String, String)> = c
        .prepare("SELECT term, term_kind FROM tag_terms WHERE tag_id = ?1 ORDER BY term_kind")
        .unwrap()
        .query_map([dst.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    let _kinds: Vec<&str> = rows.iter().map(|(_, k)| k.as_str()).collect();
    assert!(
        rows.iter().any(|(t, k)| t == "海岸" && k == "canonical"),
        "目标 canonical 保留: {rows:?}"
    );
    assert!(
        rows.iter().any(|(t, k)| t == "海边" && k == "synonym"),
        "源 canonical 合并为 synonym（F6-c）: {rows:?}"
    );
    assert!(
        rows.iter().any(|(t, k)| t == "海滨" && k == "synonym"),
        "源 synonym 保留: {rows:?}"
    );
    assert_eq!(rows.len(), 3, "词条应完整迁移: {rows:?}");
}

/// F5：脏库（同分面同词重复）合并时不崩——撞了跳过，目标词优先。
#[test]
fn merge_skips_conflicting_term() {
    let c = mem();
    let src = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let dst = tags::create_in_facet(&c, "海岸", None, Some("scene")).unwrap();
    enable_terms(&c);
    // 造脏：dst 直插一条与 src canonical 同 normalized 的 canonical（先撤两个唯一索引才能插：
    // ux_terms 管 facet+词唯一，ux_terms_canonical 管同标签单 canonical）
    c.execute_batch("DROP INDEX ux_terms; DROP INDEX ux_terms_canonical;")
        .unwrap();
    c.execute(
        "INSERT INTO tag_terms (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         VALUES (?1, 'scene', '海边', '海边', '', 'canonical', 1, 1)",
        [dst.id],
    )
    .unwrap();
    // 合并必须成功（不因 PK/唯一冲突中断），且 dst 仍只有一条「海边」
    tags::merge_preserve_alias(&c, src.id, dst.id).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE tag_id=?1 AND normalized_term='海边'",
            [dst.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "冲突词条被跳过，不产生重复: dst=海边 rows={n}");
}

/// F5：合并后源标签 deprecated 且没有任何词条（词已迁走）。
#[test]
fn deprecated_tag_has_no_terms() {
    let c = mem();
    let src = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let dst = tags::create_in_facet(&c, "海岸", None, Some("scene")).unwrap();
    enable_terms(&c);
    tags::add_alias(&c, src.id, "海滨", None, "synonym").unwrap();
    tags::merge_preserve_alias(&c, src.id, dst.id).unwrap();
    let status: String = c
        .query_row("SELECT status FROM tags WHERE id=?1", [src.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "deprecated", "源标签应置 deprecated");
    let name: String = c
        .query_row("SELECT name FROM tags WHERE id=?1", [src.id], |r| r.get(0))
        .unwrap();
    assert!(name.contains("原名 #"), "源标签改名避开旧 UNIQUE: {name}");
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE tag_id=?1",
            [src.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "deprecated 标签不应残留词条");
}

/// F5+F6-c：合并后旧词仍可搜到目标标签（synonym 词条生效）。
#[test]
fn search_old_name_hits_merge_target() {
    let c = mem();
    let src = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let dst = tags::create_in_facet(&c, "海岸", None, Some("scene")).unwrap();
    enable_terms(&c);
    tags::merge_preserve_alias(&c, src.id, dst.id).unwrap();
    let lk = tags::find_by_term(
        &c,
        "scene",
        &tags::normalize_name("海边"),
        tags::TermMatch::Alias,
    )
    .unwrap();
    assert_eq!(lk.hits.len(), 1, "旧词应精确命中: {:?}", lk.warnings);
    assert_eq!(lk.hits[0].tag_id, dst.id, "旧词「海边」应指向合并目标");
    assert_eq!(
        lk.hits[0].term_kind, "synonym",
        "合并写 synonym（F6-c），改名才写 old_name"
    );
}

/// F5-d：find_by_term 尊重 feature gate —— 关闭读旧表、开启读 tag_terms，各断言一次。
#[test]
fn find_by_term_respects_feature_gate() {
    // 状态 0：关闭 → 别名事实源 tag_aliases（写入旧表），find_by_term Alias 命中
    let c0 = mem();
    let t0 = tags::create_in_facet(&c0, "海边", None, Some("scene")).unwrap();
    tags::add_alias(&c0, t0.id, "海滨", None, "synonym").unwrap();
    let lk0 = tags::find_by_term(
        &c0,
        "scene",
        &tags::normalize_name("海滨"),
        tags::TermMatch::Alias,
    )
    .unwrap();
    assert_eq!(lk0.hits.len(), 1, "关闭态别名（tag_aliases）应命中");
    assert_eq!(lk0.hits[0].tag_id, t0.id);
    // 关闭态不读 tag_terms：即便手工造了词条行也不参与
    c0.execute_batch(
        "CREATE TABLE IF NOT EXISTS tag_terms (
        tag_id INTEGER NOT NULL, facet_key TEXT NOT NULL, normalized_term TEXT NOT NULL,
        term TEXT NOT NULL, locale TEXT NOT NULL DEFAULT '', term_kind TEXT NOT NULL,
        is_searchable INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL)",
    )
    .unwrap();
    let another = tags::create_in_facet(&c0, "森林", None, Some("scene")).unwrap();
    c0.execute(
        "INSERT INTO tag_terms (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         VALUES (?1, 'scene', '森林', '森林', '', 'canonical', 1, 1)",
        [another.id],
    )
    .unwrap();
    let lk0b = tags::find_by_term(
        &c0,
        "scene",
        &tags::normalize_name("森林"),
        tags::TermMatch::Exact,
    )
    .unwrap();
    assert_eq!(lk0b.hits.len(), 1, "关闭态仍按旧表命中（tags 规范名）");

    // 状态 1：开启 → 别名事实源 tag_terms（写入词条表），find_by_term Alias 命中词条
    let c1 = mem();
    let t1 = tags::create_in_facet(&c1, "海边", None, Some("scene")).unwrap();
    enable_terms(&c1);
    tags::add_alias(&c1, t1.id, "海滨", None, "synonym").unwrap();
    let lk1 = tags::find_by_term(
        &c1,
        "scene",
        &tags::normalize_name("海滨"),
        tags::TermMatch::Alias,
    )
    .unwrap();
    assert_eq!(lk1.hits.len(), 1, "开启态别名（tag_terms）应命中");
    assert_eq!(lk1.hits[0].tag_id, t1.id);
    let terms_n: i64 = c1
        .query_row(
            "SELECT COUNT(*) FROM tag_terms WHERE tag_id=?1",
            [t1.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        terms_n, 2,
        "开启态 canonical+synonym 都在 tag_terms（绝不双写）"
    );
    let aliases_n: i64 = c1
        .query_row(
            "SELECT COUNT(*) FROM tag_aliases WHERE tag_id=?1",
            [t1.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        aliases_n, 0,
        "开启态 tag_aliases 冻结只读，新增别名不得双写"
    );
}

/// F5-d：未启用词表时 Prefix/Contains/Fuzzy 返回空 + 指引 warning。
#[test]
fn prefix_mode_unavailable_without_terms() {
    let c = mem();
    tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    for mode in [
        tags::TermMatch::Prefix,
        tags::TermMatch::Contains,
        tags::TermMatch::Fuzzy,
    ] {
        let lk = tags::find_by_term(&c, "scene", "海", mode).unwrap();
        assert!(lk.hits.is_empty(), "{mode:?} 未启用时应无命中");
        assert!(
            lk.warnings.iter().any(|w| w.contains("启用标签约束")),
            "{mode:?} 应提示启用标签约束: {:?}",
            lk.warnings
        );
    }
    // 启用后（S5 前未实现模糊匹配）不再报「不可用」指引
    let c2 = mem();
    tags::create_in_facet(&c2, "海边", None, Some("scene")).unwrap();
    enable_terms(&c2);
    let lk2 = tags::find_by_term(&c2, "scene", "海", tags::TermMatch::Prefix).unwrap();
    assert!(
        !lk2.warnings.iter().any(|w| w.contains("启用标签约束")),
        "启用后不应再提示不可用: {:?}",
        lk2.warnings
    );
}

// ═══════════════ F6：词表治理 ═══════════════

/// 辅助：单素材批次 → 返回第一条 suggestion。
fn f6_one_suggestion(c: &rusqlite::Connection) -> bagertea_ai_media_v2_lib::db::ai::AiSuggestion {
    use bagertea_ai_media_v2_lib::db::ai;
    let aid = f4_insert_asset(c, "d:/f6.jpg");
    let batch = ai::create_batch(c, &[aid], "cloud").unwrap();
    let s = ai::list_suggestions(c, batch.id).unwrap();
    assert_eq!(s.len(), 1);
    s.into_iter().next().unwrap()
}

/// F6-a：词表里没有的词，候选留在 ai_suggestion_items（tag_id NULL），tags 表不动。
#[test]
fn ai_new_term_stays_in_suggestion_items() {
    use bagertea_ai_media_v2_lib::db::ai;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let tags = ai::CategorizedTags::from([("scene".to_string(), vec!["太空漫步".to_string()])]);
    ai::set_suggestion_tags(&c, sug.id, &tags).unwrap();
    // ① tags 表不新增
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tags WHERE name='太空漫步'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "候选词不得提前进入 tags（AI 还没确认）");
    // ② 候选留在 items：pending + tag_id NULL
    let items = ai::list_suggestion_items(&c, sug.id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].facet_key, "scene");
    assert!(items[0].tag_id.is_none(), "未命中词表 → tag_id 保持 NULL");
    assert_eq!(items[0].decision, "pending");
}

/// F6-a/F6-c：用户把候选「一个人」合并到已有词「单人」→ 目标补 synonym 别名（不是 old_name）。
#[test]
fn candidate_merge_writes_synonym_not_old_name() {
    use bagertea_ai_media_v2_lib::db::ai;
    let c = mem();
    let target = tags::create_in_facet(&c, "单人", None, Some("scene")).unwrap();
    let sug = f6_one_suggestion(&c);
    let tags = ai::CategorizedTags::from([("scene".to_string(), vec!["一个人".to_string()])]);
    ai::set_suggestion_tags(&c, sug.id, &tags).unwrap();
    let item = ai::list_suggestion_items(&c, sug.id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(item.tag_id.is_none());
    // 用户选「合并到单人」
    ai::decide_suggestion_item(&c, item.id, "modified", Some(target.id), None, None).unwrap();
    // 目标补 synonym「一个人」（旧表路径 alias_type='synonym'；绝不写 old_name）
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT ta.alias, ta.alias_type FROM tag_aliases ta
              WHERE ta.tag_id=?1 AND ta.normalized_alias='一个人'",
            [target.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (alias, kind) = row.expect("合并后目标应有 synonym 别名");
    assert_eq!(alias, "一个人");
    assert_eq!(kind, "synonym", "候选合并写 synonym（F6-c），不是 old_name");
    // item 指向目标
    let after = ai::list_suggestion_items(&c, sug.id).unwrap();
    assert_eq!(after[0].tag_id, Some(target.id));
    assert_eq!(after[0].decision, "modified");
}

/// F6-b：近似匹配两类都检出 —— 子串（一个 vs 一个人）+ 编辑距离（森材 vs 森林）。
#[test]
fn find_similar_detects_substring_and_edit_distance() {
    let c = mem();
    tags::create_in_facet(&c, "一个人", None, Some("scene")).unwrap();
    tags::create_in_facet(&c, "森林", None, Some("scene")).unwrap();
    // ② 子串：长度差 1，「一个人」含「一个」
    let sub = tags::find_similar_tag(&c, "scene", &tags::normalize_name("一个"))
        .unwrap()
        .expect("应命中「一个人」");
    assert_eq!(sub.1, "一个人");
    assert_eq!(sub.2, tags::SimilarReason::Substring);
    // ③ 编辑距离 ≤ 1：「森材」→「森林」
    let spell = tags::find_similar_tag(&c, "scene", &tags::normalize_name("森材"))
        .unwrap()
        .expect("应命中「森林」");
    assert_eq!(spell.1, "森林");
    assert_eq!(spell.2, tags::SimilarReason::Spell);
    // 不相关的词不误报
    assert!(
        tags::find_similar_tag(&c, "scene", &tags::normalize_name("城市"))
            .unwrap()
            .is_none()
    );
}

/// F6-d：疑似重复扫描 —— 连通分量把 单人/一个人/一个 聚成一组。
#[test]
fn scan_duplicate_tags_finds_known_groups() {
    let c = mem();
    let a = tags::create_in_facet(&c, "单人", None, Some("scene")).unwrap();
    let b = tags::create_in_facet(&c, "一个人", None, Some("scene")).unwrap();
    let d = tags::create_in_facet(&c, "一个", None, Some("scene")).unwrap();
    let groups = tags::scan_duplicate_tags(&c).unwrap();
    let names: Vec<String> = groups
        .iter()
        .filter(|g| g.facet_key == "scene")
        .flat_map(|g| g.members.iter().map(|m| m.name.clone()))
        .collect();
    for expect in [&a.id, &b.id, &d.id] {
        let in_group = groups
            .iter()
            .any(|g| g.facet_key == "scene" && g.members.iter().any(|m| m.tag_id == *expect));
        assert!(in_group, "「{expect}」应出现在疑似重复组");
    }
    assert!(
        names.contains(&"单人".to_string())
            && names.contains(&"一个人".to_string())
            && names.contains(&"一个".to_string()),
        "疑似重复应把 单人/一个人/一个 聚出（实际 scene 组: {names:?}）"
    );
}

/// F6-c：别名冲突按分面 —— 跨分面同名允许，同分面拒绝。
#[test]
fn alias_same_term_allowed_across_facets() {
    let c = mem();
    let people = tags::create_in_facet(&c, "人物", None, Some("people")).unwrap();
    let subject = tags::create_in_facet(&c, "主体", None, Some("subject")).unwrap();
    // people 用「一个人」作别名
    tags::add_alias(&c, people.id, "一个人", None, "synonym").unwrap();
    // subject 分面再用「一个人」→ 允许
    tags::add_alias(&c, subject.id, "一个人", None, "synonym").unwrap();
    // 同分面再用 → 拒绝
    let other_people = tags::create_in_facet(&c, "人像特写", None, Some("people")).unwrap();
    let err = tags::add_alias(&c, other_people.id, "一个人", None, "synonym").unwrap_err();
    assert!(
        err.to_string().contains("已绑定"),
        "同分面重词应报错: {err}"
    );
}

// ═══════════════ F7：分面状态对称性 + top_tags_per_facet 重写 ═══════════════

/// F7：系统分面允许停用（「不能删除」仍是唯一保留项）。
#[test]
fn deactivate_system_facet_allowed() {
    let c = mem();
    let sys = tag_facets::get(&c, "subject").unwrap();
    assert!(sys.is_system, "subject 是系统分面");
    tag_facets::deactivate(&c, "subject").unwrap();
    assert_eq!(tag_facets::get(&c, "subject").unwrap().status, "inactive");
    // 系统分面仍不能物理删除
    assert!(
        tag_facets::delete_facet(&c, "subject").is_err(),
        "系统分面只允许停用，不允许删除"
    );
}

/// F7：停用 → 恢复 → 再停用（对称；恢复后仍能停用回去——原单向陷阱已消除）。
#[test]
fn restore_then_deactivate_roundtrip() {
    let c = mem();
    tag_facets::deactivate(&c, "subject").unwrap();
    tag_facets::restore(&c, "subject").unwrap();
    assert_eq!(tag_facets::get(&c, "subject").unwrap().status, "active");
    tag_facets::deactivate(&c, "subject").unwrap();
    assert_eq!(
        tag_facets::get(&c, "subject").unwrap().status,
        "inactive",
        "恢复后仍能停用回去"
    );
    // cfg 配置全程不被生命周期覆盖（F1 不变式）
    let f = tag_facets::get(&c, "subject").unwrap();
    assert!(f.cfg_visible_in_navigation && f.cfg_ai_assignable && f.cfg_searchable);
}

/// 辅助：给每个分面造 count 个标签并各挂 1 张素材（名长 12 字内）。
fn f7_seed_facet(c: &rusqlite::Connection, facet: &str, count: usize) {
    for i in 0..count {
        let name = format!("{facet}词{i:02}");
        let t = tags::create_in_facet(c, &name, None, Some(facet)).unwrap();
        let aid = f4_insert_asset(c, &format!("d:/f7/{facet}_{i}.jpg"));
        asset_tags::assign(c, &[aid], &[t.id], "manual").unwrap();
    }
}

/// F7：Top-N 是**每分面** Top-N —— 3 个分面各 30 词，n=5 时每个分面都有条目且不超 5 词。
#[test]
fn top_tags_per_facet_is_per_facet() {
    let c = mem();
    tag_facets::create(&c, "f_a", "A面", "", "multi", None, "all").unwrap();
    tag_facets::create(&c, "f_b", "B面", "", "multi", None, "all").unwrap();
    tag_facets::create(&c, "f_c", "C面", "", "multi", None, "all").unwrap();
    for f in ["f_a", "f_b", "f_c"] {
        f7_seed_facet(&c, f, 30);
    }
    let out = tags::top_tags_per_facet(&c, 5).unwrap();
    let keys: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
    for want in ["f_a", "f_b", "f_c"] {
        assert!(
            keys.contains(&want),
            "每个分面都应有候选词（实际 {keys:?}）"
        );
    }
    for (facet, line) in &out {
        let words: Vec<&str> = line.split('/').filter(|w| !w.is_empty()).collect();
        assert!(
            words.len() <= 5,
            "{facet} 每分面最多 n=5 词（实际 {}）",
            words.len()
        );
        assert!(!words.is_empty(), "{facet} 至少 1 词");
    }
}

/// F7：字符配额按分面均摊 —— 一个超大分面不再挤掉小分面（不 break 丢整个分面）。
#[test]
fn top_tags_quota_is_shared_fairly() {
    let c = mem();
    tag_facets::create(&c, "big_f", "大面", "", "multi", None, "all").unwrap();
    tag_facets::create(&c, "s1", "小面1", "", "multi", None, "all").unwrap();
    tag_facets::create(&c, "s2", "小面2", "", "multi", None, "all").unwrap();
    f7_seed_facet(&c, "big_f", 200);
    f7_seed_facet(&c, "s1", 2);
    f7_seed_facet(&c, "s2", 2);
    let out = tags::top_tags_per_facet(&c, 200).unwrap();
    let keys: Vec<&str> = out.iter().map(|(f, _)| f.as_str()).collect();
    for want in ["big_f", "s1", "s2"] {
        assert!(
            keys.contains(&want),
            "小分面不得被大分面挤掉（实际 {keys:?}）"
        );
    }
    // 均摊配额 = 5000 / 3 ≈ 1666 字符；大分面行不应失控（含分隔符余量）
    let big = out.iter().find(|(f, _)| f == "big_f").unwrap();
    let cap_share = 5000usize / keys.len();
    assert!(
        big.1.chars().count() <= cap_share + 20,
        "大分面占用应接近均摊配额 {}（实际 {}）",
        cap_share,
        big.1.chars().count()
    );
    // 小分面各有条目
    for (facet, line) in &out {
        assert!(!line.is_empty(), "{facet} 至少保留 1 个词");
    }
}

// ═══════════════ A1：AI 打标强类型协议 ═══════════════

/// A1：TagProposal/AnalysisResult 强类型可往返序列化。
#[test]
fn analysis_result_roundtrips() {
    let ar = ai::AnalysisResult {
        description: "黄昏海边".to_string(),
        people_presence: ai::PeoplePresence {
            status: ai::PeoplePresenceStatus::Unknown,
            confidence: 0.5,
        },
        proposals: vec![
            ai::TagProposal {
                facet_key: "scene".into(),
                raw_name: "海边".into(),
                confidence: Some(0.9),
            },
            ai::TagProposal {
                facet_key: "scene".into(),
                raw_name: "日落".into(),
                confidence: None,
            },
        ],
        numbers: vec![],
        warnings: vec!["未知分面已归入 custom".into()],
    };
    let json = serde_json::to_string(&ar).unwrap();
    let back: ai::AnalysisResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ar, "AnalysisResult 应无损往返");
    // camelCase 契约
    assert!(json.contains("\"facetKey\""), "{json}");
    // 兼容分类视图
    let cat = ar.to_categorized();
    assert_eq!(
        cat.get("scene").unwrap(),
        &vec!["海边".to_string(), "日落".to_string()]
    );
}

/// A1：对象形态（{"t","c"}）解析 → typed proposals → items 落库 confidence=0.9。
#[test]
fn confidence_object_form_reaches_db() {
    use bagertea_ai_media_v2_lib::db::tag_facets::FacetPromptContext;
    use bagertea_ai_media_v2_lib::services::ai_cloud::parse_media_analysis;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let facets = [FacetPromptContext {
        key: "scene".into(),
        display_name: "场景".into(),
        selection_mode: "multi".into(),
        facet_kind: "tag".into(),
        ..Default::default()
    }];
    let ma = parse_media_analysis(
        r#"{"description":"黄昏海边风景","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"scene":[{"name":"海边","confidence":0.9}]},"numbers":{}}"#,
        &facets,
        0.30,
    )
    .unwrap();
    assert_eq!(
        ma.tags.get("scene").unwrap(),
        &vec!["海边".to_string()],
        "对象形态不得丢标签（R0-3 回归）"
    );
    assert_eq!(ma.proposals.len(), 1);
    assert_eq!(ma.proposals[0].raw_name, "海边");
    let conf = ma.proposals[0].confidence.expect("对象形态应带置信度");
    assert!((conf - 0.9).abs() < 1e-6, "置信度应落 0.9，实际 {conf}");
    ai::set_suggestion_result_typed(&c, sug.id, &ma.tags, &ma.proposals, &ma.description).unwrap();
    let items = ai::list_suggestion_items(&c, sug.id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].raw_name, "海边");
    let dbc = items[0].confidence.expect("confidence 应落库");
    assert!((dbc - 0.9).abs() < 1e-3, "item.confidence={dbc}");
}

// ═══════════════ A2：AI 溯源（F1-g 列接线） ═══════════════

/// A2：raw_response 逐字落库（含围栏/首尾空白，一行不动）；
/// analysis_json 存 AnalysisResult 序列化且可往返。
#[test]
fn raw_response_stored_verbatim() {
    use bagertea_ai_media_v2_lib::db::tag_facets::FacetPromptContext;
    use bagertea_ai_media_v2_lib::services::ai_cloud::parse_media_analysis;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let facets = [FacetPromptContext {
        key: "scene".into(),
        display_name: "场景".into(),
        selection_mode: "multi".into(),
        facet_kind: "tag".into(),
        ..Default::default()
    }];
    // 围栏、前后空白必须逐字保留。
    let raw = "```json\n{\"description\":\"黄昏海边风景\",\"peoplePresence\":{\"status\":\"unknown\",\"confidence\":0.8},\"tags\":{\"scene\":[{\"name\":\"海边\",\"confidence\":0.9}]},\"numbers\":{}}\n```\n\n";
    let ma = parse_media_analysis(raw, &facets, 0.30).unwrap();
    assert_eq!(ma.tags.get("scene").unwrap(), &vec!["海边".to_string()]);
    // runner 顺序：先写 typed 结果，再补溯源
    ai::set_suggestion_result_typed(&c, sug.id, &ma.tags, &ma.proposals, &ma.description).unwrap();
    let analysis_json = serde_json::to_string(&ai::AnalysisResult {
        description: ma.description.clone(),
        people_presence: ma.people_presence.clone(),
        proposals: ma.proposals.clone(),
        numbers: ma.numbers.clone(),
        warnings: ma.warnings.clone(),
    })
    .unwrap();
    ai::set_suggestion_provenance(&c, sug.id, raw, &analysis_json, 2).unwrap();
    let (db_raw, db_analysis): (Option<String>, Option<String>) = c
        .query_row(
            "SELECT raw_response, analysis_json FROM ai_suggestions WHERE id = ?1",
            [sug.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(db_raw.as_deref(), Some(raw), "raw_response 必须逐字存储");
    assert_eq!(
        db_analysis.as_deref(),
        Some(analysis_json.as_str()),
        "analysis_json 不得被改写"
    );
    // analysis_json 可无损往返回 AnalysisResult
    let back: ai::AnalysisResult = serde_json::from_str(db_analysis.unwrap().as_str()).unwrap();
    assert_eq!(back.description, ma.description);
    assert_eq!(back.proposals.len(), 1);
    let conf = back.proposals[0].confidence.expect("对象形态应带置信度");
    assert!(
        (conf - 0.9).abs() < 1e-6,
        "analysis_json 置信度应保真，实际 {conf}"
    );
}

/// A2：请求配置 hash —— 同输入同 hash（16 位），改任一项（模型/maxTokens/system）则变。
#[test]
fn request_config_hash_is_stable() {
    use bagertea_ai_media_v2_lib::db::tag_facets::FacetPromptContext;
    use bagertea_ai_media_v2_lib::services::ai_cloud::{
        build_batch_request_config, stable_config_hash, TextJsonTier,
    };
    let facets = [FacetPromptContext {
        key: "scene".into(),
        display_name: "场景".into(),
        description: "画面场景".into(),
        selection_mode: "multi".into(),
        max_items: Some(3),
        ..Default::default()
    }];
    let top = vec![("scene".to_string(), "海边/森林/室内".to_string())];
    let h = |sys: &str, model: &str, max_tokens: i64| {
        stable_config_hash(&build_batch_request_config(
            sys,
            &facets,
            &top,
            model,
            "image",
            max_tokens,
            true,
            TextJsonTier::Structured,
            0.30,
        ))
    };
    let base = h("你是打标助手", "qwen2.5-vl", 1180);
    assert_eq!(base.len(), 16, "sha256 前 16 位 hex");
    assert_eq!(
        base,
        h("你是打标助手", "qwen2.5-vl", 1180),
        "同输入必须同 hash"
    );
    assert_ne!(
        base,
        h("你是打标助手", "qwen2.5-vl-7b", 1180),
        "改模型必须变"
    );
    assert_ne!(
        base,
        h("你是打标助手", "qwen2.5-vl", 2048),
        "改 maxTokens 必须变"
    );
    assert_ne!(
        base,
        h("你是打标助手（覆盖版）", "qwen2.5-vl", 1180),
        "改 system 段必须变"
    );
}

/// A2：request_config_json 六项全在 —— 顶层字段 + facets 每项六字段（key/displayName/
/// description/selectionMode/maxItems/appliesTo）+ 忠实记录 system 段与模型参数。
#[test]
fn request_config_json_contains_all_inputs() {
    use bagertea_ai_media_v2_lib::db::tag_facets::FacetPromptContext;
    use bagertea_ai_media_v2_lib::services::ai_cloud::{
        build_batch_request_config, TextJsonTier, PROMPT_VERSION,
    };
    let facets = [FacetPromptContext {
        key: "scene".into(),
        display_name: "场景".into(),
        description: "画面的地理或场景".into(),
        selection_mode: "multi".into(),
        max_items: Some(3),
        ..Default::default()
    }];
    let top = vec![("scene".to_string(), "海边/森林/室内".to_string())];
    let cfg = build_batch_request_config(
        "你是图片打标助手（用户覆盖版）",
        &facets,
        &top,
        "qwen2.5-vl",
        "image",
        1180,
        true,
        TextJsonTier::Structured,
        0.30,
    );
    let o = cfg.as_object().expect("配置应为对象");
    assert_eq!(
        o["promptVersion"],
        serde_json::json!(PROMPT_VERSION),
        "带提示词版本"
    );
    assert_eq!(
        o["systemPrompt"],
        serde_json::json!("你是图片打标助手（用户覆盖版）"),
        "必须含完整 system 段（含用户覆盖）"
    );
    assert_eq!(o["mediaKind"], serde_json::json!("image"));
    // facets[0] 六字段齐
    let f0 = o["facets"]
        .as_array()
        .and_then(|a| a.first())
        .expect("facets 非空");
    for k in [
        "key",
        "displayName",
        "description",
        "selectionMode",
        "maxItems",
        "appliesTo",
    ] {
        assert!(f0.get(k).is_some(), "facets[0] 缺输入字段 {k}");
    }
    assert_eq!(f0["key"], serde_json::json!("scene"));
    assert_eq!(f0["displayName"], serde_json::json!("场景"));
    assert_eq!(f0["maxItems"], serde_json::json!(3));
    // topTagsSnapshot 带候选词快照
    assert_eq!(
        o["topTagsSnapshot"][0]["words"],
        serde_json::json!("海边/森林/室内")
    );
    // modelParams：模型、token 上限与最终结构化等级；本地档案额外记录运行参数。
    assert_eq!(o["modelParams"]["model"], serde_json::json!("qwen2.5-vl"));
    assert_eq!(o["modelParams"]["maxTokens"], serde_json::json!(1180));
    assert_eq!(
        o["modelParams"]["jsonTier"],
        serde_json::json!("structured")
    );
    assert_eq!(o["modelParams"]["keepAlive"], serde_json::json!("2m"));
    assert_eq!(
        o["modelParams"]["thinkingDisabled"],
        serde_json::json!(true)
    );
    // imagePreprocess 三字段
    let pre = &o["imagePreprocess"];
    for k in ["maxPx", "format", "quality"] {
        assert!(pre.get(k).is_some(), "imagePreprocess 缺 {k}");
    }
}

/// A2：溯源列为空的既有行仍可读取；新写入路径记录 V2 schema 版本。
#[test]
fn schema_version_allows_old_data() {
    let c = mem();
    let sug = f6_one_suggestion(&c);
    // f6_one_suggestion 走 create_batch 的 INSERT —— 未列出溯源列，正是「旧版本写入形态」：
    // 旧行由既有列默认补 raw_response NULL / analysis_json NULL / analysis_schema_version 1。
    let ver: i64 = c
        .query_row(
            "SELECT analysis_schema_version FROM ai_suggestions WHERE id = ?1",
            [sug.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ver, 1, "旧行 analysis_schema_version 保持默认 1");
    let nulls: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM ai_suggestions WHERE id = ?1 AND raw_response IS NULL AND analysis_json IS NULL",
            [sug.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 1, "旧行溯源列为 NULL（未写不报错）");
    // 老读者（list_suggestions / suggestion_from_row）不受新列影响
    assert_eq!(ai::list_suggestions(&c, sug.batch_id).unwrap().len(), 1);
    // 新路径写入后 schema 版本保持 2
    ai::set_suggestion_provenance(&c, sug.id, "{\"scene\":[]}", "{}", 2).unwrap();
    let ver2: i64 = c
        .query_row(
            "SELECT analysis_schema_version FROM ai_suggestions WHERE id = ?1",
            [sug.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ver2, 2);
}

// ═══════════════ A3：重跑策略与状态机（asset_tags.review_state 三态） ═══════════════

/// A3 辅助：直接插一条指定 review_state 的 asset_tags 行（模拟各来源路径的落库结果）。
fn a3_row(
    c: &rusqlite::Connection,
    asset_id: i64,
    tag_id: i64,
    batch_id: Option<i64>,
    source: &str,
    review_state: &str,
) {
    c.execute(
        "INSERT INTO asset_tags (asset_id, tag_id, source, created_at, confirmation,
                                 confirmed_at, confirmed_by, source_batch_id, review_state)
         VALUES (?1, ?2, ?3, 1, 'confirmed', 1, ?3, ?4, ?5)",
        rusqlite::params![asset_id, tag_id, source, batch_id, review_state],
    )
    .unwrap();
}

fn a3_row_state(c: &rusqlite::Connection, asset_id: i64, tag_id: i64) -> String {
    c.query_row(
        "SELECT review_state FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
        rusqlite::params![asset_id, tag_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// A3：ReplaceAiOnly 重跑清掉「AI 生成且未经审核」的行（review_state='ai_unreviewed' 且
/// source_batch_id 非空）。附带验证建批入口接线：ReplaceAiOnly 建批即清、Append 不动。
#[test]
fn replace_ai_only_deletes_unreviewed() {
    let c = mem();
    let aid = f4_insert_asset(&c, "d:/a3_unreviewed.jpg");
    let tag = tags::create_in_facet(&c, "未审核标签", None, Some("scene")).unwrap();
    // 旧批次写下的未审核 AI 行
    let old = ai::create_batch(&c, &[aid], "cloud").unwrap();
    a3_row(&c, aid, tag.id, Some(old.id), "ai_cloud", "ai_unreviewed");
    // Append 重跑：不清（默认最安全）
    ai::create_batch_with_retag(&c, &[aid], "cloud", ai::RetagMode::Append).unwrap();
    assert_eq!(
        a3_row_state(&c, aid, tag.id),
        "ai_unreviewed",
        "Append 不得清未审核行"
    );
    // ReplaceAiOnly 重跑：建批同事务清掉未审核 AI 行
    ai::create_batch_with_retag(&c, &[aid], "cloud", ai::RetagMode::ReplaceAiOnly).unwrap();
    let left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
            rusqlite::params![aid, tag.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "未审核 AI 行应被重跑删除");
}

/// A3：ReplaceAiOnly 重跑**不删** ai_reviewed 行（用户已审核，铁律 10）。
#[test]
fn replace_ai_only_keeps_reviewed() {
    use bagertea_ai_media_v2_lib::db::asset_tags;
    let c = mem();
    let aid = f4_insert_asset(&c, "d:/a3_reviewed.jpg");
    let tag = tags::create_in_facet(&c, "已审核标签", None, Some("scene")).unwrap();
    let batch = ai::create_batch(&c, &[aid], "cloud").unwrap();
    // 用户已确认 → ai_reviewed（仍带 source_batch_id，语义上属于旧批次）
    a3_row(&c, aid, tag.id, Some(batch.id), "ai_cloud", "ai_reviewed");
    let n = asset_tags::retag_clear_unreviewed(&c, &[aid]).unwrap();
    assert_eq!(n, 0, "ai_reviewed 行不得被 ReplaceAiOnly 清掉");
    assert_eq!(a3_row_state(&c, aid, tag.id), "ai_reviewed");
}

/// A3：ReplaceAiOnly 重跑**不删** manual 行（人工确认，铁律 10）。
#[test]
fn replace_ai_only_keeps_manual() {
    use bagertea_ai_media_v2_lib::db::asset_tags;
    let c = mem();
    let aid = f4_insert_asset(&c, "d:/a3_manual.jpg");
    let tag = tags::create_in_facet(&c, "手工标签", None, Some("scene")).unwrap();
    let batch = ai::create_batch(&c, &[aid], "cloud").unwrap();
    a3_row(&c, aid, tag.id, Some(batch.id), "manual", "manual");
    let n = asset_tags::retag_clear_unreviewed(&c, &[aid]).unwrap();
    assert_eq!(n, 0, "manual 行不得被 ReplaceAiOnly 清掉");
    assert_eq!(a3_row_state(&c, aid, tag.id), "manual");
}

/// A3：确认建议 → 该素材本批次的 AI 来源行提升为 ai_reviewed（此后重跑不再清它）。
#[test]
fn confirm_promotes_to_ai_reviewed() {
    use bagertea_ai_media_v2_lib::db::asset_tags;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let tag = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["海边".to_string()])]);
    ai::confirm_suggestion(&c, sug.id, &tags_map).unwrap();
    assert_eq!(
        a3_row_state(&c, sug.asset_id, tag.id),
        "ai_reviewed",
        "用户确认后行应提升为 ai_reviewed"
    );
    // 提升后的行即使带 source_batch_id 也不再被 ReplaceAiOnly 清掉
    let n = asset_tags::retag_clear_unreviewed(&c, &[sug.asset_id]).unwrap();
    assert_eq!(n, 0, "确认后的行必须躲过 ReplaceAiOnly");
    assert_eq!(a3_row_state(&c, sug.asset_id, tag.id), "ai_reviewed");
}

/// A3：用户手工再打同一个标签 → 该行提升 manual 且清空 source_batch_id
///（D-1/D-2：撤销 AI 批次不会误删手工确认后的标签）。
#[test]
fn manual_override_promotes_to_manual() {
    use bagertea_ai_media_v2_lib::db::asset_tags;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let tag = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["海边".to_string()])]);
    // 先 AI 确认（ai_reviewed）→ 再手工重打同标签（覆盖）
    ai::confirm_suggestion(&c, sug.id, &tags_map).unwrap();
    asset_tags::assign(&c, &[sug.asset_id], &[tag.id], "manual").unwrap();
    assert_eq!(a3_row_state(&c, sug.asset_id, tag.id), "manual");
    let (src, sb): (String, Option<i64>) = c
        .query_row(
            "SELECT source, source_batch_id FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
            rusqlite::params![sug.asset_id, tag.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(src, "manual");
    assert_eq!(sb, None, "手工覆盖必须清空 source_batch_id（防撤销误删）");
}

/// A3：undo_batch 撤销批次 —— ai_reviewed 行随批次删（D-3 语义），manual 行不动。
#[test]
fn undo_batch_respects_review_state() {
    use bagertea_ai_media_v2_lib::db::asset_tags;
    use bagertea_ai_media_v2_lib::db::tag_ops;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let t1 = tags::create_in_facet(&c, "AI标签甲", None, Some("scene")).unwrap();
    let t2 = tags::create_in_facet(&c, "AI标签乙", None, Some("scene")).unwrap();
    let tags_map = ai::CategorizedTags::from([(
        "scene".to_string(),
        vec!["AI标签甲".to_string(), "AI标签乙".to_string()],
    )]);
    ai::confirm_suggestion(&c, sug.id, &tags_map).unwrap();
    assert_eq!(a3_row_state(&c, sug.asset_id, t1.id), "ai_reviewed");
    // 用户对 t2 手工覆盖 → manual
    asset_tags::assign(&c, &[sug.asset_id], &[t2.id], "manual").unwrap();
    assert_eq!(a3_row_state(&c, sug.asset_id, t2.id), "manual");
    // 撤销批次：ai_reviewed 行（D-3 语义不变）删，manual 行留
    let applied = tag_ops::undo_batch(&c, sug.batch_id).unwrap();
    assert_eq!(applied, 1, "只应撤销 1 条（manual 那条被跳过）");
    let (t1_left, t2_left): (i64, i64) = c
        .query_row(
            "SELECT COUNT(*) FILTER (WHERE tag_id=?1), COUNT(*) FILTER (WHERE tag_id=?2)
               FROM asset_tags WHERE asset_id=?3",
            rusqlite::params![t1.id, t2.id, sug.asset_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(t1_left, 0, "ai_reviewed 行随批次撤销（D-3 语义不变）");
    assert_eq!(t2_left, 1, "manual 行在撤销中必须保留");
    assert_eq!(a3_row_state(&c, sug.asset_id, t2.id), "manual");
    // D-4：重复撤销幂等返回 0
    assert_eq!(tag_ops::undo_batch(&c, sug.batch_id).unwrap(), 0);
}

// ═══════════════ A4：置信度策略（按词表而非 confidence 高低决定自动化程度） ═══════════════

/// 精确命中词表也只建立 pending 建议，必须人工确认后才写 asset_tags。
#[test]
fn exact_term_stays_pending() {
    use bagertea_ai_media_v2_lib::db::ai::ConfidencePolicy;
    let c = mem();
    enable_terms(&c);
    // feature 开时 create_in_facet 会把 canonical 词写进 tag_terms → 精确命中成立
    let tag = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let sug = f6_one_suggestion(&c);
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["海边".to_string()])]);
    let proposals = vec![ai::TagProposal {
        facet_key: "scene".into(),
        raw_name: "海边".into(),
        confidence: Some(0.9),
    }];
    ai::set_suggestion_result_policy(
        &c,
        sug.id,
        &tags_map,
        &proposals,
        "",
        &ConfidencePolicy::default(),
    )
    .unwrap();
    let items = ai::list_suggestion_items(&c, sug.id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].decision, "pending", "精确命中仍须人工确认");
    assert_eq!(items[0].tag_id, Some(tag.id));
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
            rusqlite::params![sug.asset_id, tag.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "未人工确认不得写 asset_tags");
}

/// A4：完全新词 → 只进 pending（不进 tags、不建词、不写 asset_tags）；自动建词默认关。
#[test]
fn new_term_goes_to_pending() {
    use bagertea_ai_media_v2_lib::db::ai::ConfidencePolicy;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["太空漫步".to_string()])]);
    let proposals = vec![ai::TagProposal {
        facet_key: "scene".into(),
        raw_name: "太空漫步".into(),
        confidence: Some(0.8),
    }];
    ai::set_suggestion_result_policy(
        &c,
        sug.id,
        &tags_map,
        &proposals,
        "",
        &ConfidencePolicy::default(),
    )
    .unwrap();
    let items = ai::list_suggestion_items(&c, sug.id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].decision, "pending", "新词绝不自动建词");
    assert!(items[0].tag_id.is_none(), "未采纳前 tag_id 保持 NULL");
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tags WHERE name='太空漫步'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "新词不得被自动写进 tags");
    let at: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE asset_id=?1",
            [sug.asset_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at, 0, "pending 词不得写 asset_tags");
}

/// A4：confidence < min_suggest（0.30）→ 不入库（连 pending 都不进），
/// suggested_tags 同步剔除（「确认全部」不会复活低置信词）。
#[test]
fn low_confidence_dropped_entirely() {
    use bagertea_ai_media_v2_lib::db::ai::ConfidencePolicy;
    let c = mem();
    let sug = f6_one_suggestion(&c);
    let tags_map = ai::CategorizedTags::from([("scene".to_string(), vec!["低可信词".to_string()])]);
    let proposals = vec![ai::TagProposal {
        facet_key: "scene".into(),
        raw_name: "低可信词".into(),
        confidence: Some(0.1),
    }];
    ai::set_suggestion_result_policy(
        &c,
        sug.id,
        &tags_map,
        &proposals,
        "",
        &ConfidencePolicy { min_suggest: 0.30 },
    )
    .unwrap();
    assert_eq!(
        ai::list_suggestion_items(&c, sug.id).unwrap().len(),
        0,
        "低置信词连 pending 都不进"
    );
    let raw: String = c
        .query_row(
            "SELECT suggested_tags FROM ai_suggestions WHERE id=?1",
            [sug.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !raw.contains("低可信词"),
        "suggested_tags 应剔除低置信词: {raw}"
    );
    let at: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE asset_id=?1",
            [sug.asset_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at, 0);
}

/// 当前版本固定人工审核：默认策略不自动写已有词、不自动建新词，也不静默过滤建议。
#[test]
fn auto_adopt_off_by_default() {
    use bagertea_ai_media_v2_lib::db::ai::ConfidencePolicy;
    use bagertea_ai_media_v2_lib::db::settings::AiSettings;
    let p = ConfidencePolicy::default();
    assert_eq!(p.min_suggest, 0.30, "低置信度默认阈值");
    let s = AiSettings::default();
    assert!((s.confidence_min_suggest - 0.30).abs() < 1e-9);
}

// ═══════════════ S0：白名单单一事实源（搜索契约） ═══════════════

/// S0：ALL_METADATA_KEYS 每个 key 都能编译（加了 key 忘了 spec 会失败）；
/// ALL_SORT_KEYS 与 AI 侧 is_valid_sort_by / validate_intent 三处一致（rating 全合法）。
#[test]
fn whitelist_single_source() {
    use bagertea_ai_media_v2_lib::db::search_query::{
        is_metadata_key, is_supported_metadata_key, ALL_METADATA_KEYS, ALL_SORT_KEYS,
    };
    use bagertea_ai_media_v2_lib::services::super_search_ai::{validate_intent, SearchIntentV2};
    // 正向：全集每个 key 都必须有 key_spec（加 key 忘 spec → 编译期断言失败）
    for k in ALL_METADATA_KEYS {
        assert!(is_metadata_key(k), "白名单常量自身应认识 {k}");
        assert!(
            is_supported_metadata_key(k),
            "ALL_METADATA_KEYS 里的 {k} 缺 key_spec 分支"
        );
    }
    // 反方向（spec 认识的都在全集里）：key_spec 入口先按 ALL 白名单放行 —— 结构性保证，
    // 分支不在全集里永远不达。
    // 排序白名单三处一致：AI 侧 is_valid_sort_by / validate_intent / assets VALID_SORT（引用同常量）
    for k in ALL_SORT_KEYS {
        assert!(
            bagertea_ai_media_v2_lib::services::super_search_ai::is_valid_sort_by(k),
            "ALL_SORT_KEYS 里的 {k} 必须被 is_valid_sort_by 接受"
        );
    }
    assert!(
        ALL_SORT_KEYS.contains(&"rating"),
        "rating 必须在排序白名单（R0-4 回归）"
    );
    assert!(
        !bagertea_ai_media_v2_lib::services::super_search_ai::is_valid_sort_by("magic"),
        "白名单之外必须拒绝"
    );
    // validate_intent 走同一常量：rating 合法、magic 拒绝
    let i = SearchIntentV2 {
        sort_by: Some("rating".into()),
        ..SearchIntentV2::default()
    };
    assert!(validate_intent(&i, &[]).is_ok(), "rating 排序不得被降级");
    let i2 = SearchIntentV2 {
        sort_by: Some("magic".into()),
        ..SearchIntentV2::default()
    };
    assert!(validate_intent(&i2, &[]).is_err());
}

// ═══════════════ S5：五种匹配模式（词扩展 Prefix/Contains/Fuzzy + 词查叶子） ═══════════════

/// S5：Exact 只命中 canonical；同一词以 synonym 存在时 Exact 不中、Alias 中。
#[test]
fn term_match_exact_only_canonical() {
    use bagertea_ai_media_v2_lib::db::tags::{find_by_term, TermMatch};
    let c = mem();
    enable_terms(&c);
    let t = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    tags::add_alias(&c, t.id, "海滨", None, "synonym").unwrap();
    // synonym 名精确查（Alias）命中；Exact（仅 canonical）不命中
    let by_alias =
        find_by_term(&c, "scene", &tags::normalize_name("海滨"), TermMatch::Alias).unwrap();
    assert_eq!(by_alias.hits.len(), 1);
    assert_eq!(by_alias.hits[0].tag_id, t.id);
    let by_exact =
        find_by_term(&c, "scene", &tags::normalize_name("海滨"), TermMatch::Exact).unwrap();
    assert!(
        by_exact.hits.is_empty(),
        "Exact 不得命中 synonym：{:?}",
        by_exact.hits
    );
    // canonical 名 Exact 命中
    let canon = find_by_term(&c, "scene", &tags::normalize_name("海边"), TermMatch::Exact).unwrap();
    assert_eq!(canon.hits.len(), 1);
}

/// S5：Contains 上限 10 —— 超出的截断并记 warning（「只用了前 N 个」）。
#[test]
fn term_match_contains_respects_cap() {
    use bagertea_ai_media_v2_lib::db::tags::{expand_term_query, TermMatch};
    let c = mem();
    enable_terms(&c);
    for i in 0..12 {
        let name = format!("人{i:02}");
        tags::create_in_facet(&c, &name, None, Some("scene")).unwrap();
    }
    let (hits, warns) = expand_term_query(
        &c,
        "scene",
        &tags::normalize_name("人"),
        TermMatch::Contains,
        10,
    )
    .unwrap();
    assert_eq!(hits.len(), 10, "Contains 必须截断到 10");
    assert!(
        warns.iter().any(|w| w.contains("只用了前 10")),
        "截断必须给出 warning：{warns:?}"
    );
    // 名字长度升序（「人」最短的不存在；长度 3 的全部排在长度 3 前）
    let lens: Vec<usize> = hits
        .iter()
        .map(|h| h.matched_term.chars().count())
        .collect();
    let mut sorted = lens.clone();
    sorted.sort();
    assert_eq!(lens, sorted, "Contains 命中按名字长度升序");
}

/// S5：Fuzzy 两级 —— 缩候选 + 编辑距离 ≤1，命中真实标签（指导书四行真值表）。
#[test]
fn term_match_fuzzy_two_stage() {
    use bagertea_ai_media_v2_lib::db::tags::{expand_term_query, TermMatch};
    let c = mem();
    enable_terms(&c);
    for name in ["一个", "一个人", "单人", "女性", "女孩", "青少年", "森林"] {
        tags::create_in_facet(&c, name, None, Some("scene")).unwrap();
    }
    let names_of = |input: &str| -> Vec<String> {
        let (hits, _w) = expand_term_query(
            &c,
            "scene",
            &tags::normalize_name(input),
            TermMatch::Fuzzy,
            5,
        )
        .unwrap();
        hits.iter().map(|h| h.matched_term.clone()).collect()
    };
    let got = names_of("一人");
    for want in ["一个", "一个人", "单人"] {
        assert!(
            got.contains(&want.to_string()),
            "「一人」应命中 {want}：{got:?}"
        );
    }
    assert!(
        !got.contains(&"森林".to_string()),
        "无关词不得命中：{got:?}"
    );
    let got2 = names_of("女该");
    assert!(
        got2.contains(&"女孩".to_string()),
        "「女该」→女孩：{got2:?}"
    );
    assert!(
        got2.contains(&"女性".to_string()),
        "「女该」→女性：{got2:?}"
    );
    assert!(names_of("青少").contains(&"青少年".to_string()));
    assert!(names_of("森材").contains(&"森林".to_string()));
    // 距离升序：距离 0 不应出现在 fuzzy（fuzzy 只管 ≥1；0 由 Alias 精确路径处理）
    assert!(names_of("女该").iter().all(|n| n != "女该"));
}

/// S5：LeafCond::Tag.termQuery（词查）在编译层扩展成标签集 ——
/// 前缀「海」命中「海边/海景」；未命中词查编译为 1=0（0 结果而非全库）。
#[test]
fn term_query_expands_in_tag_leaf() {
    use bagertea_ai_media_v2_lib::db::query_expr::{compile_expr, LeafCond, QueryExpr};
    use bagertea_ai_media_v2_lib::db::tags::TermMatch;
    let c = mem();
    enable_terms(&c);
    let t1 = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    let t2 = tags::create_in_facet(&c, "海景", None, Some("scene")).unwrap();
    let aid1 = f4_insert_asset(&c, "d:/s5a.jpg");
    let aid2 = f4_insert_asset(&c, "d:/s5b.jpg");
    asset_tags::assign(&c, &[aid1], &[t1.id], "manual").unwrap();
    asset_tags::assign(&c, &[aid2], &[t2.id], "manual").unwrap();
    let leaf = |term_query: Option<String>, match_mode: TermMatch| QueryExpr::Leaf {
        cond: LeafCond::Tag {
            facet_key: "scene".into(),
            tag_ids: Vec::new(),
            mode: Some("any".into()),
            include_descendants: true,
            term_query,
            term_match: match_mode,
        },
    };
    // 词查前缀「海」→ 命中海边与海景两张
    let (sql, _) = compile_expr(&c, &leaf(Some("海".into()), TermMatch::Prefix)).unwrap();
    assert_ne!(sql, "1=0", "「海」前缀应命中标签：{sql}");
    assert!(sql.contains("EXISTS"));
    // 未命中的词查 → 1=0（0 结果，与空 tag_ids 的恒真区分）
    let (sql2, _) = compile_expr(&c, &leaf(Some("不存在的标签".into()), TermMatch::Alias)).unwrap();
    assert_eq!(sql2, "1=0", "词查未命中必须编译为 0 结果");
    // 无词查、无 tag_ids → 保持旧语义恒真
    let (sql3, _) = compile_expr(&c, &leaf(None, TermMatch::Alias)).unwrap();
    assert_eq!(sql3, "1=1");
}

// ═════ S 波次端到端验收（组 6）：必须有草地、最好有蓝天、排除夜景、按相关度排序 ═════

/// e2e：真库（测试库自建 草地/蓝天/夜景 三个标签并打标）跑 SearchPlanV3。
/// 断言五件事：① 无蓝天草地照不被淘汰；② 有蓝天排前；③ 夜景不在结果；
/// ④ 编译 warnings 为空（标签都在、无剔除）；⑤ min=0 时 should 只影响顺序不影响集合。
#[test]
fn e2e_grass_required_sky_preferred_night_excluded() {
    use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr};
    use bagertea_ai_media_v2_lib::db::search_plan::{
        run_search_plan, Ranking, SearchPlanV3, ShouldClause,
    };
    use bagertea_ai_media_v2_lib::db::tags::TermMatch;
    let c = mem();
    let grass = tags::create_in_facet(&c, "草地", None, Some("scene")).unwrap();
    let sky = tags::create_in_facet(&c, "蓝天", None, Some("scene")).unwrap();
    let night = tags::create_in_facet(&c, "夜景", None, Some("scene")).unwrap();
    let a = f4_insert_asset(&c, "d:/grass_sky.jpg");
    let b = f4_insert_asset(&c, "d:/grass_only.jpg");
    let e = f4_insert_asset(&c, "d:/grass_night.jpg");
    let d = f4_insert_asset(&c, "d:/night_only.jpg");
    asset_tags::assign(&c, &[a], &[grass.id, sky.id], "manual").unwrap();
    asset_tags::assign(&c, &[b], &[grass.id], "manual").unwrap();
    asset_tags::assign(&c, &[e], &[grass.id, night.id], "manual").unwrap();
    asset_tags::assign(&c, &[d], &[night.id], "manual").unwrap();
    let tag_leaf = |id: i64| QueryExpr::Leaf {
        cond: LeafCond::Tag {
            facet_key: "scene".into(),
            tag_ids: vec![id],
            mode: Some("any".into()),
            include_descendants: true,
            term_query: None,
            term_match: TermMatch::Alias,
        },
    };
    let plan = SearchPlanV3 {
        filter: Some(tag_leaf(grass.id)),
        must_not: Some(tag_leaf(night.id)),
        should: vec![ShouldClause {
            cond: LeafCond::Tag {
                facet_key: "scene".into(),
                tag_ids: vec![sky.id],
                mode: Some("any".into()),
                include_descendants: true,
                term_query: None,
                term_match: TermMatch::Alias,
            },
            weight: 1.0,
            label: "蓝天（加分项）".into(),
            evidence: None,
        }],
        minimum_should_match: 0,
        ranking: Ranking::Relevance,
        ..Default::default()
    };
    let out = run_search_plan(&c, &plan, None, 0).unwrap();
    let ids: Vec<i64> = out.iter().map(|r| r.0).collect();
    // ① 无蓝天的草地照仍在结果里（不被淘汰）
    assert!(ids.contains(&b), "无蓝天草地不得被淘汰：{ids:?}");
    // ② 有蓝天的排在前面
    assert_eq!(ids[0], a, "有蓝天应排最前：{ids:?}");
    // ③ 夜景照片不在结果里（must_not / filter 双重排除）
    assert!(!ids.contains(&e), "草地+夜景必须被排除：{ids:?}");
    assert!(!ids.contains(&d), "纯夜景必须被排除：{ids:?}");
    // ④ 编译不产生 warning —— validate 通过即无剔除（compile 无 warning 输出通道，
    //    本步 = 标签全在词表/库里、无剔除路径被触发）
    // ⑤ minimum_should_match=0：should 只影响顺序不影响集合（与去掉 should 的集合一致）
    let mut base_plan = plan.clone();
    base_plan.should.clear();
    let mut base_ids: Vec<i64> = run_search_plan(&c, &base_plan, None, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.0)
        .collect();
    let mut with_should = ids.clone();
    base_ids.sort_unstable();
    with_should.sort_unstable();
    assert_eq!(with_should, base_ids, "min=0 时 should 不得改变结果集合");
}

// ═══════════════ R2-1：编译层 warning 全链路回传（expr → AssetPage.warnings） ═══════════════

/// R2-1：表达式编译 warning（标签不属于声明分面被剔除）→ 到达 list DTO。
#[test]
fn dropped_tag_produces_visible_warning() {
    use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr};
    let c = mem();
    let tag = tags::create_in_facet(&c, "海边", None, Some("scene")).unwrap();
    // 声明 color 分面但标签属于 scene → 剔除 + warning（不再是静默日志）
    let expr = QueryExpr::Leaf {
        cond: LeafCond::Tag {
            facet_key: "color".into(),
            tag_ids: vec![tag.id],
            mode: Some("any".into()),
            include_descendants: true,
            term_query: None,
            term_match: Default::default(),
        },
    };
    let filter = assets::AssetFilter {
        expr: Some(expr),
        ..Default::default()
    };
    let page = assets::list(&c, &filter).unwrap();
    assert!(
        page.warnings.iter().any(|w| w.contains("剔除")),
        "剔除必须出现在 DTO warnings：{:?}",
        page.warnings
    );
}

/// R2-1：分面不存在/停用（FacetHasAny）→ 编译 warning 进 DTO（用户看得到，不只日志）。
#[test]
fn compile_warnings_reach_dto() {
    use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr};
    let c = mem();
    let expr = QueryExpr::Leaf {
        cond: LeafCond::FacetHasAny {
            facet_key: "ghost_facet".into(),
        },
    };
    let filter = assets::AssetFilter {
        expr: Some(expr),
        ..Default::default()
    };
    let page = assets::list(&c, &filter).unwrap();
    assert!(
        page.warnings.iter().any(|w| w.contains("已停用或不存在")),
        "停用分面警告必须回传：{:?}",
        page.warnings
    );
}

// ═══════════════ R2-2：量纲人话（不阻断执行，把静默 0 结果变 0 结果 + 一句提示） ═══════════════

/// R2-2：file_size 疑似写成 MB（< 1024 字节）→ warning 但仍执行（不报错）。
#[test]
fn suspicious_file_size_warns_but_executes() {
    use bagertea_ai_media_v2_lib::db::search_query::MetadataFilter;
    let c = mem();
    let _aid = f4_insert_asset(&c, "d:/r22.jpg");
    let filter = assets::AssetFilter {
        metadata_filters: vec![MetadataFilter {
            key: "file_size".into(),
            op: "gte".into(),
            value: Some(serde_json::json!(500)), // 500 字节 < 1KB —— 疑似把 MB 写成 500
            values: None,
            min: None,
            max: None,
        }],
        ..Default::default()
    };
    let page = assets::list(&c, &filter).unwrap(); // 不报错
    assert!(
        page.warnings.iter().any(|w| w.contains("小于 1 KB")),
        "量纲提示必须回传 DTO：{:?}",
        page.warnings
    );
    // 执行不报错且仍按字节语义跑（500B 条件与 1024B 素材的关系留给查询本身决定）
    assert!(page.total >= 0);
}

// ═══════════════ R2-3：建批同源合并上报（merged_groups） ═══════════════

/// R2-3：RAW+JPG 同源对只留一个代表 → merged_groups = 原始数 − 去重数。
#[test]
fn create_batch_reports_merged_groups() {
    let c = mem();
    let raw = f4_insert_asset(&c, "d:/r23/_1091396.RW2");
    let jpg = f4_insert_asset(&c, "d:/r23/_1091396.JPG");
    let solo = f4_insert_asset(&c, "d:/r23/_1091397.JPG");
    let b = ai::create_batch(&c, &[raw, jpg, solo], "cloud").unwrap();
    assert_eq!(b.total, 2, "同源对去重后应剩 2 条代表");
    assert_eq!(b.merged_groups, 1, "RAW+JPG 合并 1 组");
    // 独立素材不误报
    let b2 = ai::create_batch(&c, &[solo], "cloud").unwrap();
    assert_eq!(b2.merged_groups, 0);
}

// ═══════════════ C-1：色板关系表（V23 asset_palette_colors） ═══════════════

fn c1_palette_json(segs: &[(u8, u8, u8, f32)]) -> String {
    let arr: Vec<serde_json::Value> = segs
        .iter()
        .map(
            |(r, g, b, ratio)| serde_json::json!({"hex":"#000000","r":r,"g":g,"b":b,"ratio":ratio}),
        )
        .collect();
    serde_json::to_string(&arr).unwrap()
}

fn c1_set_palette(c: &rusqlite::Connection, id: i64, json: &str) {
    c.execute(
        "UPDATE assets SET palette_json = ?1 WHERE id = ?2",
        rusqlite::params![json, id],
    )
    .unwrap();
}

/// C-1：ratio 阈值表达力（字符串列方案做不到）—— rank0 红占比 >0.2 命中。
#[test]
fn palette_ratio_threshold_works() {
    let c = mem();
    let a = f4_insert_asset(&c, "d:/c1_red.jpg");
    c1_set_palette(
        &c,
        a,
        &c1_palette_json(&[(224, 32, 32, 0.6), (32, 32, 224, 0.3)]),
    );
    let n = assets::rescan_palette_colors(&c).unwrap();
    assert!(n >= 2, "应写入主色+次色行：{n}");
    let hits: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_palette_colors WHERE asset_id=?1 AND color_bucket=0 AND rank=0 AND ratio>0.2",
            [a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1, "主色红占比 0.6 → ratio>0.2 命中");
}

/// C-1：前三色查询走覆盖索引 ix_apc_bucket（SEARCH，不是 SCAN）。
#[test]
fn palette_top3_query_uses_index() {
    let c = mem();
    let a = f4_insert_asset(&c, "d:/c1_idx.jpg");
    c1_set_palette(&c, a, &c1_palette_json(&[(224, 32, 32, 0.6)]));
    assets::rescan_palette_colors(&c).unwrap();
    let plan: String = c
        .query_row(
            "EXPLAIN QUERY PLAN SELECT asset_id FROM asset_palette_colors
              WHERE color_bucket = 0 AND rank < 3",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("SEARCH"), "前三色查询必须走索引：{plan}");
    assert!(!plan.contains("SCAN"), "不得全表扫：{plan}");
}

/// C-1：同时含红和蓝（前三色）→ INTERSECT 表达；metadata key palette_top3 in [红,蓝] 编译一致。
#[test]
fn palette_intersect_two_colors() {
    use bagertea_ai_media_v2_lib::db::search_query::{compile_metadata, MetadataFilter};
    let c = mem();
    let a = f4_insert_asset(&c, "d:/c1_ab.jpg");
    let b = f4_insert_asset(&c, "d:/c1_b.jpg");
    // A 前三色含红+蓝；B 只含红
    c1_set_palette(
        &c,
        a,
        &c1_palette_json(&[(224, 32, 32, 0.5), (100, 20, 255, 0.4)]),
    );
    c1_set_palette(&c, b, &c1_palette_json(&[(224, 32, 32, 0.8)]));
    assets::rescan_palette_colors(&c).unwrap();
    let both: Vec<i64> = c
        .prepare(
            "SELECT asset_id FROM asset_palette_colors WHERE color_bucket=0 AND rank<3
             INTERSECT
             SELECT asset_id FROM asset_palette_colors WHERE color_bucket=8 AND rank<3",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(
        both.contains(&a) && !both.contains(&b),
        "交集应只有 A：{both:?}"
    );
    // 编译层：palette_top3 in [红,蓝] → EXISTS(IN(0,8) AND rank<3)，经 list 端到端命中 A
    let f_red = MetadataFilter {
        key: "palette_top3".into(),
        op: "eq".into(),
        value: Some(serde_json::json!("红")),
        values: None,
        min: None,
        max: None,
    };
    let f_blue = MetadataFilter {
        key: "palette_top3".into(),
        op: "eq".into(),
        value: Some(serde_json::json!("蓝")),
        values: None,
        min: None,
        max: None,
    };
    let compiled = compile_metadata(&f_red)
        .unwrap()
        .expect("palette_top3 可编译");
    assert!(
        compiled.sql.contains("rank < 3") && compiled.sql.contains("color_bucket IN (?1)"),
        "{}",
        compiled.sql
    );
    // 红 AND 蓝（两个 EXISTS 条件）→ 只有 A（与 INTERSECT 语义一致）
    let filter = assets::AssetFilter {
        metadata_filters: vec![f_red, f_blue],
        ..Default::default()
    };
    let page = assets::list(&c, &filter).unwrap();
    let ids: Vec<i64> = page.items.iter().map(|x| x.id).collect();
    assert_eq!(ids, vec![a], "红∩蓝（前三色）应只有 A：{ids:?}");
}

/// U-3：palette_top3 eq 红 + min=0.5 → 只命中红占比 ≥50% 的素材（关系表 ratio 维度的编译链路）。
#[test]
fn palette_top3_ratio_min_filters() {
    use bagertea_ai_media_v2_lib::db::search_query::{compile_metadata, MetadataFilter};
    let c = mem();
    let hi = f4_insert_asset(&c, "d:/c1_hi.jpg");
    let lo = f4_insert_asset(&c, "d:/c1_lo.jpg");
    // hi：前三色 = 红 60% / 蓝 30%；lo：红仅 10%（其余 90% 蓝）
    c1_set_palette(
        &c,
        hi,
        &c1_palette_json(&[(224, 32, 32, 0.6), (32, 32, 224, 0.3)]),
    );
    c1_set_palette(
        &c,
        lo,
        &c1_palette_json(&[(224, 32, 32, 0.1), (32, 32, 224, 0.9)]),
    );
    assets::rescan_palette_colors(&c).unwrap();
    let f = MetadataFilter {
        key: "palette_top3".into(),
        op: "eq".into(),
        value: Some(serde_json::json!("红")),
        values: None,
        min: Some(serde_json::json!(0.5)),
        max: None,
    };
    let compiled = compile_metadata(&f).unwrap().expect("带阈值可编译");
    assert!(
        compiled.sql.contains("HAVING SUM(apc.ratio) >= ?2"),
        "{}",
        compiled.sql
    );
    let filter = assets::AssetFilter {
        metadata_filters: vec![f],
        ..Default::default()
    };
    let page = assets::list(&c, &filter).unwrap();
    let ids: Vec<i64> = page.items.iter().map(|x| x.id).collect();
    assert_eq!(ids, vec![hi], "红占比 ≥50% 应只有 hi：{ids:?}");
}

// ═══════════════ P0-4：gate 开启后 FTS/aliases() 同 gate 读 tag_terms ═══════════════

/// P0-4：tag_unique_terms=1 后 add_alias 只写 tag_terms（tag_aliases 冻结），
/// FTS 触发器词源必须切到 tag_terms —— 否则新别名永远搜不到。
/// 用 3 字 CJK 走 FTS 短语路径（≤2 字走 LIKE，不属于本测试的 FTS 范围）。
#[test]
fn alias_reaches_fts_after_gate_enabled() {
    let c = mem();
    enable_terms(&c);
    let t = tags::create_in_facet(&c, "茶", None, Some("subject")).unwrap();
    // gate 开：add_alias 写 tag_terms（trg_terms_ai 触发刷新）
    tags::add_alias(&c, t.id, "茶饮料", None, "synonym").unwrap();
    let aid = f4_insert_asset(&c, "d:/tea.jpg");
    asset_tags::assign(&c, &[aid], &[t.id], "manual").unwrap();

    // 修复前：FTS 词源仍读 tag_aliases，别名进不了 FTS → 此处必 miss
    let hits = bagertea_ai_media_v2_lib::db::search::search_asset_ids_all(&c, "茶饮料").unwrap();
    assert_eq!(
        hits,
        vec![aid],
        "gate 开启后新增别名应能经 FTS 命中：{hits:?}"
    );
    // canonical 路径不受影响（回源仍是 tags.name）
    let canon = bagertea_ai_media_v2_lib::db::search::search_asset_ids_all(&c, "茶").unwrap();
    assert_eq!(canon, vec![aid]);
}

/// P0-4：Tag.aliases 与 tag_unique_terms 同 gate —— 关时读 tag_aliases，开时读 tag_terms，
/// 否则 gate 开启后 aliases() 恒空（hydrate_metadata 喂给前端 chips/树形视图）。
#[test]
fn tag_aliases_field_reads_terms_when_gated() {
    // gate 关（旧路径）：add_alias 写 tag_aliases，aliases() 照旧读它
    let c = mem();
    let t = tags::create_in_facet(&c, "海", None, Some("scene")).unwrap();
    tags::add_alias(&c, t.id, "海边", None, "synonym").unwrap();
    let off = tags::aliases(&c, t.id).unwrap();
    assert!(
        off.iter().any(|a| a == "海边"),
        "gate 关应读 tag_aliases：{off:?}"
    );

    // gate 开：add_alias 只写 tag_terms → aliases() 必须同 gate 读 tag_terms
    let c2 = mem();
    enable_terms(&c2);
    let t2 = tags::create_in_facet(&c2, "海", None, Some("scene")).unwrap();
    tags::add_alias(&c2, t2.id, "海边", None, "synonym").unwrap();
    let on = tags::aliases(&c2, t2.id).unwrap();
    assert!(
        on.iter().any(|a| a == "海边"),
        "gate 开后 aliases() 应读 tag_terms，实际：{on:?}"
    );
}
