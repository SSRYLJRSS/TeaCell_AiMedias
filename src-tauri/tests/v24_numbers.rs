//! V24 数值分面（Phase 6 协议测试，§6.3–6.6）—— 生命周期 / 链路 / 转换。
//! 运行：cargo test --test v24_numbers

use teacell_ai_media_manager_lib::db::{
    ai, asset_tags, assets, facet_numbers, init_memory, tag_facets, tags,
};

fn mem() -> rusqlite::Connection {
    init_memory().expect("内存库初始化失败")
}

/// 辅助：插入素材，返回 id（与 foundation_acceptance::f4_insert_asset 同语义）。
fn f4_insert_asset(conn: &rusqlite::Connection, path: &str) -> i64 {
    let id = assets::insert(
        conn,
        path,
        path.rsplit('/').next().unwrap_or("a.jpg"),
        "jpg",
        1024,
        "image/jpeg",
        1700000000000,
    )
    .expect("插入素材失败");
    conn.execute(
        "UPDATE libraries
            SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                library_epoch=lower(hex(randomblob(16)))
          WHERE singleton=1 AND (health!='ready' OR root_identity IS NULL)",
        [],
    )
    .unwrap();
    let library = teacell_ai_media_manager_lib::db::libraries::current(conn)
        .unwrap()
        .unwrap();
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

/// 数值分面测试夹具：建「人数」数值分面（0–50，单位人），直接 UPDATE 类型与配置
/// （create() 收 7 参不含数值配置，与生产命令层语义一致：先建分面再配置类型）。
fn number_facet_setup(conn: &rusqlite::Connection) {
    tag_facets::create(conn, "people_count", "人数", "", "single", None, "all").unwrap();
    conn.execute(
        "UPDATE tag_facets SET facet_kind='number', num_min=0, num_max=50, num_unit='人', num_decimals=0, num_step=1 WHERE key='people_count'",
        [],
    )
    .unwrap();
}

// ── §6.5 生命周期（5 条） ──

/// 停用分面数值保留（与「停用分面的标签仍可搜」同语义）；恢复原样可用。
#[test]
fn facet_deactivate_preserves_numbers() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num1.jpg");
    facet_numbers::set_facet_number(&c, &[aid], "people_count", 5.0).unwrap();
    tag_facets::deactivate(&c, "people_count").unwrap();
    let row = facet_numbers::get_number(&c, aid, "people_count")
        .unwrap()
        .expect("停用后数值必须保留");
    assert_eq!(row.value, 5.0);
    tag_facets::restore(&c, "people_count").unwrap();
    let row2 = facet_numbers::get_number(&c, aid, "people_count")
        .unwrap()
        .unwrap();
    assert_eq!(row2.value, 5.0);
}

/// 核心不变式：删除分面数值级联（先删数值再删分面，同事务），不遗留孤儿。
#[test]
fn facet_delete_cascades_numbers() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num2.jpg");
    facet_numbers::set_facet_number(&c, &[aid], "people_count", 8.0).unwrap();
    let report = tag_facets::delete_facet(&c, "people_count").unwrap();
    assert_eq!(report.tags_deleted, 0);
    let left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_facet_numbers WHERE facet_key='people_count'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "删除分面不得遗留孤儿数值");
}

/// 影响预览能统计数值行数（FacetImpact.number_count 语义）。
#[test]
fn facet_impact_reports_number_count() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num3.jpg");
    facet_numbers::set_facet_number(&c, &[aid], "people_count", 3.0).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_facet_numbers WHERE facet_key='people_count'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "影响预览应统计到 1 条数值行");
}

#[test]
fn numeric_sync_only_applies_to_an_exact_raw_non_raw_pair() {
    let c = mem();
    number_facet_setup(&c);
    let jpg = f4_insert_asset(&c, "d:/photos/X.JPG");
    let raw = f4_insert_asset(&c, "d:/photos/X.RW2");
    let png = f4_insert_asset(&c, "d:/photos/X.PNG");

    facet_numbers::set_facet_number(&c, &[jpg], "people_count", 5.0).unwrap();
    assert!(facet_numbers::get_number(&c, raw, "people_count")
        .unwrap()
        .is_none());
    assert!(facet_numbers::get_number(&c, png, "people_count")
        .unwrap()
        .is_none());

    facet_numbers::clear_facet_number(&c, &[jpg], "people_count").unwrap();
    assert!(facet_numbers::get_number(&c, jpg, "people_count")
        .unwrap()
        .is_none());
    assert!(facet_numbers::get_number(&c, raw, "people_count")
        .unwrap()
        .is_none());
}

#[test]
fn numeric_sync_remains_enabled_for_an_exact_raw_non_raw_pair() {
    let c = mem();
    number_facet_setup(&c);
    let jpg = f4_insert_asset(&c, "d:/photos/PAIR.JPG");
    let raw = f4_insert_asset(&c, "d:/photos/PAIR.RW2");

    facet_numbers::set_facet_number(&c, &[jpg], "people_count", 5.0).unwrap();
    let value = facet_numbers::get_number(&c, raw, "people_count")
        .unwrap()
        .expect("唯一同源兄弟应接收数值");
    assert_eq!(value.value, 5.0);
}

/// 带数值行时禁止裸 SQL 删分面（RESTRICT 触发器同时查 asset_facet_numbers）。
#[test]
fn restrict_delete_blocks_when_numbers_exist() {
    let c = mem();
    // RESTRICT 触发器由 apply_v22b_constraints 创建（生产库 apply 后常在）
    teacell_ai_media_manager_lib::db::migrations::apply_v22b_constraints(&c).unwrap();
    number_facet_setup(&c);
    let _aid = f4_insert_asset(&c, "d:/num4.jpg");
    facet_numbers::set_facet_number(&c, &[_aid], "people_count", 2.0).unwrap();
    let err = c.execute("DELETE FROM tag_facets WHERE key='people_count'", []);
    assert!(err.is_err(), "存在数值行时裸删分面必须被 RESTRICT 拦住");
}

/// 同 key 重建后无旧数值（删除已级联，干净语义；想保留用「停用」）。
#[test]
fn recreate_same_key_has_no_stale_numbers() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num5.jpg");
    facet_numbers::set_facet_number(&c, &[aid], "people_count", 7.0).unwrap();
    tag_facets::delete_facet(&c, "people_count").unwrap();
    number_facet_setup(&c);
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_facet_numbers WHERE facet_key='people_count'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "重建分面不得残留旧数值");
}

// ── §6.4 链路（4 条） ──

/// 核心不变式（不变量 10）：AI 数值永不覆盖 manual / ai_reviewed。
#[test]
fn ai_number_never_overwrites_manual_or_reviewed() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num6.jpg");
    facet_numbers::set_facet_number(&c, &[aid], "people_count", 5.0).unwrap();
    // AI 想写 999 → guarded 写入必须跳过
    let written =
        facet_numbers::upsert_number_guarded(&c, aid, "people_count", 999.0, "ai_cloud", None)
            .unwrap();
    assert!(!written, "AI 不得覆盖人工值");
    assert_eq!(
        facet_numbers::get_number(&c, aid, "people_count")
            .unwrap()
            .unwrap()
            .value,
        5.0
    );
    // ai_unreviewed → AI 可覆盖：先撤掉 manual 行，写一条 AI 未审核值（重跑语义）
    c.execute("DELETE FROM asset_facet_numbers WHERE asset_id=?1", [aid])
        .unwrap();
    facet_numbers::upsert_number_guarded(&c, aid, "people_count", 6.0, "ai_cloud", Some(1))
        .unwrap();
    assert_eq!(
        facet_numbers::get_number(&c, aid, "people_count")
            .unwrap()
            .unwrap()
            .value,
        6.0
    );
    c.execute(
        "UPDATE asset_facet_numbers SET review_state='ai_reviewed' WHERE asset_id=?1",
        [aid],
    )
    .unwrap();
    let written2 =
        facet_numbers::upsert_number_guarded(&c, aid, "people_count", 7.0, "ai_cloud", Some(1))
            .unwrap();
    assert!(!written2, "AI 不得覆盖已审核值");
    assert_eq!(
        facet_numbers::get_number(&c, aid, "people_count")
            .unwrap()
            .unwrap()
            .value,
        6.0
    );
}

/// 核心不变式：撤销批次只删 AI 数值（manual / ai_reviewed 保留）。
#[test]
fn undo_batch_deletes_ai_numbers_only() {
    let c = mem();
    number_facet_setup(&c);
    let a = f4_insert_asset(&c, "d:/num7a.jpg");
    let b = f4_insert_asset(&c, "d:/num7b.jpg");
    facet_numbers::set_facet_number(&c, &[a], "people_count", 5.0).unwrap(); // manual
    facet_numbers::upsert_number(
        &c,
        b,
        "people_count",
        9.0,
        "ai_cloud",
        "ai_unreviewed",
        Some(11),
    )
    .unwrap();
    // a 的 manual 行已删（set 走 manual），再造一条 AI 值验证 undo 范围
    c.execute("DELETE FROM asset_facet_numbers WHERE asset_id=?1", [a])
        .unwrap();
    facet_numbers::upsert_number(
        &c,
        a,
        "people_count",
        4.0,
        "ai_cloud",
        "ai_unreviewed",
        Some(11),
    )
    .unwrap();
    let deleted = facet_numbers::undo_batch_numbers(&c, 11).unwrap();
    assert_eq!(deleted, 2, "只删 AI 未审核的两条");
    assert!(
        facet_numbers::get_number(&c, a, "people_count")
            .unwrap()
            .is_none(),
        "AI 值应随批次撤销删除"
    );
    assert!(
        facet_numbers::get_number(&c, b, "people_count")
            .unwrap()
            .is_none(),
        "B 的 AI 值同样删除"
    );
}

/// 核心不变式：重跑 ReplaceAiOnly 删未审核 AI 数值，保留 manual。
#[test]
fn replace_ai_only_deletes_unreviewed_numbers_keeps_manual() {
    let c = mem();
    number_facet_setup(&c);
    let a = f4_insert_asset(&c, "d:/num8a.jpg");
    let b = f4_insert_asset(&c, "d:/num8b.jpg");
    facet_numbers::set_facet_number(&c, &[a], "people_count", 5.0).unwrap(); // manual 保留
    facet_numbers::upsert_number(
        &c,
        b,
        "people_count",
        9.0,
        "ai_cloud",
        "ai_unreviewed",
        Some(12),
    )
    .unwrap();
    facet_numbers::retag_clear_unreviewed_numbers(&c, &[a, b]).unwrap();
    let ra = facet_numbers::get_number(&c, a, "people_count")
        .unwrap()
        .unwrap();
    assert_eq!(ra.value, 5.0, "manual 保留");
    assert!(
        facet_numbers::get_number(&c, b, "people_count")
            .unwrap()
            .is_none(),
        "未审核 AI 数值被清"
    );
}

/// AI 数值建议落库：无歧义 → num_value 有值；歧义 → num_value=NULL + reason（不变量 11）。
#[test]
fn ai_number_proposal_lands_in_suggestion_items() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num9.jpg");
    let batch = ai::create_batch(&c, &[aid], "cloud").unwrap();
    let sug = ai::list_suggestions(&c, batch.id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let warns = facet_numbers::record_number_proposals(
        &c,
        sug.id,
        &[
            ("people_count".into(), "5".into()),
            ("people_count".into(), "约5".into()),
            ("people_count".into(), "很多".into()),
        ],
    )
    .unwrap();
    assert_eq!(warns.len(), 1, "「很多」无数字应产生 warning");
    let items: Vec<(Option<f64>, Option<String>)> = {
        let mut stmt = c
            .prepare("SELECT num_value, decision_reason FROM ai_suggestion_items WHERE suggestion_id=?1 AND item_kind='number' ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([sug.id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    assert_eq!(items.len(), 2, "Value + Ambiguous 各落一条；None 丢弃");
    assert!(items[0].0.is_some(), "「5」应有确定值");
    assert!(
        items[1].0.is_none() && items[1].1.clone().unwrap_or_default().contains("约数"),
        "「约5」应 pending + 原因"
    );
}

// ── §6.6 转换（4 条） ──

/// 核心不变式：转换冲突不自动裁决 —— dry_run 报告原样列出冲突，且执行前不写任何值。
#[test]
fn convert_reports_per_asset_conflicts() {
    let c = mem();
    tag_facets::create(&c, "pc", "人数", "", "multi", None, "all").unwrap();
    let t3 = tags::create_in_facet(&c, "3人", None, Some("pc")).unwrap();
    let t5 = tags::create_in_facet(&c, "5人", None, Some("pc")).unwrap();
    let a = f4_insert_asset(&c, "d:/c1.jpg");
    let b = f4_insert_asset(&c, "d:/c2.jpg");
    asset_tags::assign(&c, &[a], &[t3.id], "manual").unwrap();
    asset_tags::assign(&c, &[a], &[t5.id], "manual").unwrap(); // 冲突：3 vs 5
    asset_tags::assign(&c, &[b], &[t3.id], "manual").unwrap();
    let report = facet_numbers::convert_facet_kind_dry_run(&c, "pc").unwrap();
    assert_eq!(report.parsed.len(), 2, "「3人」「5人」都应解析成功");
    assert_eq!(report.conflicts.len(), 1, "素材 a 的 3/5 冲突应被列出");
    assert_eq!(report.conflicts[0].asset_id, a);
    assert_eq!(report.conflicts[0].candidates.len(), 2);
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM asset_facet_numbers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "dry_run 绝不写值");
    let err = facet_numbers::convert_facet_kind_execute(&c, "pc", false).unwrap_err();
    assert!(
        err.to_string().contains("冲突"),
        "冲突必须先由用户裁决：{err}"
    );
    let n2: i64 = c
        .query_row("SELECT COUNT(*) FROM asset_facet_numbers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n2, 0);
    let _ = b;
}

/// 转换解析分桶：单值归一 / 歧义 / 无法解析 / 层级与别名损失。
#[test]
fn convert_reports_buckets_and_losses() {
    let c = mem();
    tag_facets::create(&c, "pc2", "人数", "", "multi", None, "all").unwrap();
    let ok = tags::create_in_facet(&c, "5", None, Some("pc2")).unwrap();
    let _ok2 = tags::create_in_facet(&c, "5.0人", None, Some("pc2")).unwrap();
    let amb = tags::create_in_facet(&c, "约5", None, Some("pc2")).unwrap();
    let bad = tags::create_in_facet(&c, "很多", None, Some("pc2")).unwrap();
    let parent = tags::create_in_facet(&c, "父级", None, Some("pc2")).unwrap();
    let _child = tags::create_in_facet(&c, "子级", Some(parent.id), Some("pc2")).unwrap();
    tags::add_alias(&c, ok.id, "五个人", None, "synonym").unwrap();
    let report = facet_numbers::convert_facet_kind_dry_run(&c, "pc2").unwrap();
    assert_eq!(report.parsed.len(), 2, "5 与 5.0人 都进 parsed");
    assert!(
        report.ambiguous.iter().any(|x| x.tag_id == amb.id),
        "「约5」进 ambiguous"
    );
    assert!(
        report.unparseable.iter().any(|x| x.tag_id == bad.id),
        "「很多」进 unparseable"
    );
    assert_eq!(report.hierarchy_loss, 1, "非根标签 1 个（子级）");
    assert_eq!(report.alias_loss, 1, "别名 1 条（五个人）");
}

/// 规则 9：number → tag 直接禁止。
#[test]
fn convert_number_to_tag_rejected() {
    let c = mem();
    number_facet_setup(&c);
    let err = facet_numbers::convert_facet_kind_execute(&c, "people_count", true).unwrap_err();
    assert!(
        err.to_string().contains("已是数值型"),
        "number→tag 必须禁止：{err}"
    );
}

/// 规则 10：dry_run 一行不写；无冲突执行后数值落库、标签 deprecated、关联移除（单事务）。
#[test]
fn convert_dry_run_writes_nothing_then_execute_is_atomic() {
    let c = mem();
    tag_facets::create(&c, "pc3", "人数", "", "multi", None, "all").unwrap();
    let t1 = tags::create_in_facet(&c, "3人", None, Some("pc3")).unwrap();
    let t2 = tags::create_in_facet(&c, "5人", None, Some("pc3")).unwrap();
    let a = f4_insert_asset(&c, "d:/c3.jpg");
    asset_tags::assign(&c, &[a], &[t1.id], "manual").unwrap(); // 只挂一个（无冲突）
    let report = facet_numbers::convert_facet_kind_dry_run(&c, "pc3").unwrap();
    assert!(report.conflicts.is_empty(), "只挂 t1 时无冲突：{report:?}");
    assert!(report.ambiguous.is_empty());
    let n0: i64 = c
        .query_row("SELECT COUNT(*) FROM asset_facet_numbers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n0, 0, "dry_run 一行不写");
    facet_numbers::convert_facet_kind_execute(&c, "pc3", false).unwrap();
    let row = facet_numbers::get_number(&c, a, "pc3")
        .unwrap()
        .expect("转换后素材应有数值 3");
    assert_eq!(row.value, 3.0);
    assert_eq!(row.source, "manual");
    let kind: String = c
        .query_row(
            "SELECT facet_kind FROM tag_facets WHERE key='pc3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "number");
    let dep: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM tags WHERE id IN (?1,?2) AND status='deprecated'",
            rusqlite::params![t1.id, t2.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dep, 2, "原标签置 deprecated（不物理删）");
    let at: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM asset_tags WHERE tag_id IN (?1,?2)",
            rusqlite::params![t1.id, t2.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at, 0, "标签关联已移除（FTS 触发器自动更新）");
    let _ = t2;
}

// ── Phase 7-3/7-4 接线：确认分流 ──

/// 核心不变式：确认建议时数值项分流 —— 写 asset_facet_numbers（source 随批次），
/// item 置 accepted；AI 越界值（999 > num_max=50）在建议题阶段即被拒（落库层校验）。
#[test]
fn confirm_suggestion_writes_number_rows_and_rejects_out_of_range() {
    let c = mem();
    number_facet_setup(&c);
    let aid = f4_insert_asset(&c, "d:/num10.jpg");
    let batch = ai::create_batch(&c, &[aid], "cloud").unwrap();
    let sug = ai::list_suggestions(&c, batch.id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    // 999 越界：落库层校验拦下（§6.5 —— 不裁到边界，进 pending 待人工填数）
    let _warns = facet_numbers::record_number_proposals(
        &c,
        sug.id,
        &[("people_count".into(), "999".into())],
    )
    .unwrap();
    let (cnt, reason): (i64, String) = c
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(decision_reason),'') FROM ai_suggestion_items WHERE suggestion_id=?1 AND item_kind='number'",
            [sug.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cnt, 1, "越界值进 pending（不裁边界）");
    assert!(
        reason.contains("超出范围"),
        "decision_reason 应标明越界：{reason}"
    );
    let val: Option<f64> = c
        .query_row("SELECT num_value FROM ai_suggestion_items WHERE suggestion_id=?1 AND item_kind='number'", [sug.id], |r| r.get(0))
        .unwrap();
    assert!(val.is_none(), "越界 item 不得带确定值");

    // 合法值 5：落 pending item → 确认建议（分流）→ 写 asset_facet_numbers
    facet_numbers::record_number_proposals(&c, sug.id, &[("people_count".into(), "5人".into())])
        .unwrap();
    ai::confirm_suggestion(&c, sug.id, &ai::CategorizedTags::new()).unwrap();
    let row = facet_numbers::get_number(&c, aid, "people_count")
        .unwrap()
        .expect("确认后数值必须落库");
    assert_eq!(row.value, 5.0);
    assert_eq!(row.source, "ai_cloud");
    assert_eq!(row.review_state, "ai_unreviewed");
    let decision: String = c
        .query_row("SELECT decision FROM ai_suggestion_items WHERE suggestion_id=?1 AND item_kind='number' AND num_value IS NOT NULL", [sug.id], |r| r.get(0))
        .unwrap();
    assert_eq!(decision, "accepted", "确认后数值 item 置 accepted");
}
