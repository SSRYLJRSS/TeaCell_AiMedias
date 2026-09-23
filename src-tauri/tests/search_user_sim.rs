//! 超级搜索链路「用户行为模拟」集成测试（2026-09-18 复盘专项）
//! 不从实现角度出发，而是模拟真实用户输入/操作，验证整条链路（关键词路由 →
//! QueryExpr 编译 → SearchPlanV3 执行/分页/全选/诊断 → AI 解析降级）的行为是否符合直觉预期。
//! 运行：cargo test --test search_user_sim
//!
//! 这些用例覆盖复盘中确认的搜索、分页、诊断和 AI 降级行为，作为链路回归基线。

use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr, SearchScope};
use bagertea_ai_media_v2_lib::db::search::{self};
use bagertea_ai_media_v2_lib::db::search_plan::{self, Ranking, SearchPlanV3, ShouldClause};
use bagertea_ai_media_v2_lib::db::{self, asset_tags, assets, tags};
use bagertea_ai_media_v2_lib::services::super_search_ai as ai;
use rusqlite::{params, Connection};

fn setup() -> Connection {
    db::init_memory().expect("内存库初始化失败")
}

fn add(conn: &Connection, name: &str) -> i64 {
    assets::insert(
        conn,
        &format!("d:/sim/{name}"),
        name,
        "jpg",
        1024,
        "image/jpeg",
        1_700_000_000_000,
    )
    .expect("插入素材失败")
}

fn set_desc(conn: &Connection, id: i64, desc: &str) {
    conn.execute(
        "UPDATE assets SET content_description = ?1 WHERE id = ?2",
        params![desc, id],
    )
    .unwrap();
}

fn set_size(conn: &Connection, id: i64, size: i64) {
    conn.execute(
        "UPDATE assets SET file_size = ?1 WHERE id = ?2",
        params![size, id],
    )
    .unwrap();
}

fn tag_in(conn: &Connection, facet: &str, name: &str) -> i64 {
    tags::create_in_facet(conn, name, None, Some(facet))
        .expect("建标签失败")
        .id
}

fn assign(conn: &Connection, asset: i64, tag: i64) {
    asset_tags::assign(conn, &[asset], &[tag], "manual").unwrap();
}

fn leaf_expr(cond: LeafCond) -> QueryExpr {
    QueryExpr::Leaf { cond }
}

fn tag_leaf(facet: &str, id: i64) -> LeafCond {
    LeafCond::Tag {
        facet_key: facet.into(),
        tag_ids: vec![id],
        mode: Some("any".into()),
        include_descendants: true,
        term_query: None,
        term_match: Default::default(),
    }
}

fn search_leaf(value: &str) -> LeafCond {
    LeafCond::Search {
        value: value.into(),
        scope: SearchScope::All,
    }
}

fn field_plan(filter: Option<QueryExpr>, key: &str) -> SearchPlanV3 {
    SearchPlanV3 {
        filter,
        ranking: Ranking::Field {
            key: key.into(),
            dir: "desc".into(),
        },
        ..Default::default()
    }
}

// ═══════════════ ① 用户关键词路由：特殊字符 / 中文长度矩阵 ═══════════════

/// 用户输入 LIKE 通配符（% _）时应当作字面量，而不是吞掉全库。
#[test]
fn sim_special_chars_are_literal() {
    let c = setup();
    let hit = add(&c, "100%完成_a.jpg");
    let _other = add(&c, "1000张其他图.jpg"); // 若 % 未转义会误命中
    let ids = search::search_asset_ids_all(&c, "100%").unwrap();
    assert!(
        ids.contains(&hit),
        "「100%」应命中文件名含字面 100% 的素材: {ids:?}"
    );
    assert!(
        !ids.contains(&_other),
        "「100%」不应命中不含字面 % 的素材: {ids:?}"
    );
    // 下划线同样按字面量
    let ids = search::search_asset_ids_all(&c, "成_a").unwrap();
    assert!(
        ids.contains(&hit),
        "「成_a」应命中（_ 已转义为字面量）: {ids:?}"
    );
    assert!(search::search_asset_ids_all(&c, "完成x")
        .unwrap()
        .is_empty());
}

/// 纯中文 2 字（LIKE）/ 4 字（2 字块 AND）/ 逆序（应零命中）。
#[test]
fn sim_cjk_length_matrix() {
    let c = setup();
    let a = add(&c, "a.jpg");
    set_desc(&c, a, "海边日落人像");
    let _b = add(&c, "b.jpg");
    set_desc(&c, _b, "山区雪景");

    assert_eq!(
        search::search_asset_ids_all(&c, "海边").unwrap(),
        vec![a],
        "2 字走 LIKE"
    );
    assert_eq!(
        search::search_asset_ids_all(&c, "海边日落").unwrap(),
        vec![a],
        "4 字走 2 字块 AND"
    );
    assert!(
        search::search_asset_ids_all(&c, "边海").unwrap().is_empty(),
        "逆序 2 字块不应命中（短语保相邻）"
    );
    assert!(
        search::search_asset_ids_all(&c, "雪景人像")
            .unwrap()
            .is_empty(),
        "跨素材各取一词不应命中"
    );
}

/// 用户输入引号/FTS 敏感字符不应让查询报错（永不红字的底线）。
#[test]
fn sim_fts_hostile_input_no_panic() {
    let c = setup();
    let a = add(&c, "a.jpg");
    set_desc(&c, a, "夜景人像");
    for hostile in ["夜景\"", "\" OR", "夜 景", "夜景 AND 人像", "-夜景"] {
        let r = search::search_asset_ids_all(&c, hostile);
        assert!(r.is_ok(), "输入 {hostile:?} 不应报错: {:?}", r.err());
    }
}

// ═══════════════ ② 多条件组合：参数占位对齐（编号 ?N + 裸 ? 混合） ═══════════════

/// 用户同时给「文件大小 ≥ N」+ 关键词：LIKE 用裸 ?、metadata 用编号 ?N，
/// 组内拼接后必须按序绑定——错位会把大小阈值绑成关键词（或反过来）。
#[test]
fn sim_mixed_params_align_in_group() {
    let c = setup();
    let big = add(&c, "big.jpg");
    set_desc(&c, big, "夜景人像");
    let small = add(&c, "small.jpg");
    set_desc(&c, small, "夜景人像");
    set_size(&c, small, 10); // 不满足 ≥1000
    let unrelated = add(&c, "unrelated.jpg");
    set_desc(&c, unrelated, "白天风景");
    set_size(&c, unrelated, 99999);

    let plan = field_plan(
        Some(QueryExpr::And {
            children: vec![
                leaf_expr(LeafCond::Metadata {
                    filter: db::search_query::MetadataFilter {
                        key: "file_size".into(),
                        op: "gte".into(),
                        value: Some(serde_json::json!(1000)),
                        values: None,
                        min: None,
                        max: None,
                    },
                }),
                leaf_expr(search_leaf("夜景")),
            ],
        }),
        "created_at",
    );
    let ids: Vec<i64> = search_plan::run_search_plan(&c, &plan, None, 0)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(ids, vec![big], "大小+关键词组合错位：{ids:?}");
}

// ═══════════════ ③ 字段排序分页一致性（压力：1500 条同值排序键） ═══════════════

#[test]
fn sim_field_ranking_pagination_is_stable() {
    let c = setup();
    let sky = tag_in(&c, "scene", "蓝天");
    let mut all = Vec::new();
    for i in 0..1500 {
        let id = add(&c, &format!("p{i}.jpg"));
        if i % 3 == 0 {
            assign(&c, id, sky);
        }
        all.push(id);
    }
    let plan = field_plan(Some(leaf_expr(tag_leaf("scene", sky))), "created_at");
    let ids = search_plan::run_plan_ids(&c, &plan).unwrap();
    assert_eq!(ids.total, 500);
    // 逐页拼接必须 == 全选 ID 序列（同 created_at → score/id 次级键必须兜住稳定性）
    let mut seen: Vec<i64> = Vec::new();
    let mut offset = 0i64;
    loop {
        let page = search_plan::run_plan_page(&c, &plan, offset, Some(200)).unwrap();
        assert_eq!(page.total, 500, "字段排序 total 必须全量");
        seen.extend(page.items.iter().map(|x| x.id));
        if !page.has_more {
            break;
        }
        offset += page.items.len() as i64;
        assert!(offset <= 600, "has_more 失控");
    }
    assert_eq!(seen, ids.ids, "分页拼接 ≠ 全选序列（排序不稳定/丢条）");
}

// ═══════════════ ④ AI 加分（Relevance）分页：total 必须是全量命中数 ═══════════════

/// 用户视角：AI 搜索「海边，最好有蓝天」→ 结果 300 条，网格应显示 300 项并能翻页。
/// 回归守护：run_plan_page 的 total 必须来自 count_plan（全量），hasMore 不得第一页就 false。
#[test]
fn sim_relevance_pagination_reports_real_total() {
    let c = setup();
    let sea = tag_in(&c, "scene", "海边");
    let sky = tag_in(&c, "scene", "蓝天");
    for i in 0..300 {
        let id = add(&c, &format!("r{i}.jpg"));
        assign(&c, id, sea);
        if i % 2 == 0 {
            assign(&c, id, sky);
        }
    }
    let plan = SearchPlanV3 {
        filter: Some(leaf_expr(tag_leaf("scene", sea))),
        should: vec![ShouldClause {
            cond: tag_leaf("scene", sky),
            weight: 2.0,
            label: "蓝天（加分项）".into(),
            evidence: Some("最好有蓝天".into()),
        }],
        ranking: Ranking::Relevance,
        ..Default::default()
    };
    let page1 = search_plan::run_plan_page(&c, &plan, 0, Some(100)).unwrap();
    assert_eq!(
        page1.total, 300,
        "Relevance 分页 total={}（应为 300，命中集合大小）",
        page1.total
    );
    assert!(page1.has_more, "第一页后必须还能翻页");
    // 逐页拼接 == 全选序列（同源同序）
    let ids = search_plan::run_plan_ids(&c, &plan).unwrap();
    let mut seen: Vec<i64> = Vec::new();
    let mut offset = 0i64;
    loop {
        let page = search_plan::run_plan_page(&c, &plan, offset, Some(100)).unwrap();
        seen.extend(page.items.iter().map(|x| x.id));
        if !page.has_more {
            break;
        }
        offset += page.items.len() as i64;
        assert!(offset <= 300, "has_more 失控");
    }
    assert_eq!(seen, ids.ids, "Relevance 分页拼接 ≠ 全选序列");
    // 加分语义：i 为偶数的素材（id 为奇数）全部命中加分，必须整体排在未命中前面
    let (top, rest) = seen.split_at(150);
    assert!(
        top.iter().all(|id| id % 2 == 1),
        "前 150 名必须全是命中加分项的素材（id 奇数）"
    );
    assert!(
        rest.iter().all(|id| id % 2 == 0),
        "未命中加分的必须排在加分项之后"
    );
}

// ═══════════════ ⑤ 诊断：归零条件必须能被识别（前端 zeroing 的数据前提） ═══════════════

/// 用户视角：搜出 0 结果时，页面应列出「把结果砍到 0」的条件（zeroingActions）。
/// 语义：delta = count_without_leaf - result_count；当前 0 结果时，
/// delta>0 的叶子就是「砍掉它就能出结果」的归零条件。
#[test]
fn sim_diagnose_identifies_zeroing_leaf() {
    let c = setup();
    let grass = tag_in(&c, "scene", "草地");
    let night = tag_in(&c, "scene", "夜景");
    let a = add(&c, "d1.jpg");
    assign(&c, a, grass);
    let _b = add(&c, "d2.jpg");
    assign(&c, _b, night); // 只有它带「夜景」→ 组合必然 0 结果

    let plan = field_plan(
        Some(QueryExpr::And {
            children: vec![
                leaf_expr(tag_leaf("scene", grass)),
                leaf_expr(tag_leaf("scene", night)),
            ],
        }),
        "created_at",
    );
    assert!(
        search_plan::run_search_plan(&c, &plan, None, 0)
            .unwrap()
            .is_empty(),
        "前提：组合 0 结果"
    );
    let diag = search_plan::diagnose_search_plan(&c, &plan, 7).unwrap();
    assert_eq!(diag.leaves.len(), 2);
    assert!(
        diag.leaves.iter().all(|l| l.plan_revision == 7),
        "代次必须回显（前端据此丢弃过期诊断）"
    );
    // 叶子顺序仍按 AST 定位；诊断 label 同时应包含实际标签名。
    let grass_leaf = &diag.leaves[0]; // AND 第 1 个孩子 = 草地
    let night_leaf = &diag.leaves[1]; // AND 第 2 个孩子 = 夜景
                                      // 「夜景」是归零条件：砍掉它 → 剩草地 1 条
    assert_eq!(night_leaf.result_count, 0);
    assert_eq!(
        night_leaf.count_without_leaf, 1,
        "去掉夜景后应剩 1 条（草地）"
    );
    assert!(
        night_leaf.delta > 0 && night_leaf.result_count == 0,
        "夜景必须被 zeroing 条件（delta>0 && result=0）命中: {:?}",
        night_leaf
    );
    // 「草地」同样是归零条件：砍掉它 → 剩夜景 1 条（d2）
    assert_eq!(grass_leaf.count_without_leaf, 1);
    assert_eq!(grass_leaf.delta, 1, "草地同样是归零条件（两标签不相交）");
}

/// 诊断在「有结果」时：delta 应等于该叶子砍掉的条数（多素材场景量化验证）。
#[test]
fn sim_diagnose_delta_quantifies_each_leaf() {
    let c = setup();
    let grass = tag_in(&c, "scene", "草地");
    let sky = tag_in(&c, "scene", "蓝天");
    for i in 0..10 {
        let id = add(&c, &format!("q{i}.jpg"));
        assign(&c, id, grass);
        if i < 4 {
            assign(&c, id, sky); // 4 条同时有蓝天
        }
    }
    let plan = field_plan(
        Some(QueryExpr::And {
            children: vec![
                leaf_expr(tag_leaf("scene", grass)),
                leaf_expr(tag_leaf("scene", sky)),
            ],
        }),
        "created_at",
    );
    let diag = search_plan::diagnose_search_plan(&c, &plan, 1).unwrap();
    assert_eq!(diag.leaves.len(), 2);
    // 两个叶子 shared result_count = 4（完整表达式命中数）
    assert!(diag.leaves.iter().all(|l| l.result_count == 4));
    // 蓝天（AND 第 2 个孩子）：self_count=4；去掉它 → 剩草地 10 条 → delta=6（蓝天砍掉了 6 条）
    let sky_leaf = &diag.leaves[1];
    assert_eq!(sky_leaf.self_count, 4, "蓝天单独命中 4 条");
    assert_eq!(sky_leaf.count_without_leaf, 10);
    assert_eq!(sky_leaf.delta, 6);
    // 草地（AND 第 1 个孩子）：self_count=10；去掉它 → 剩蓝天 4 条 → delta=0
    let grass_leaf = &diag.leaves[0];
    assert_eq!(grass_leaf.self_count, 10);
    assert_eq!(grass_leaf.count_without_leaf, 4);
    assert_eq!(grass_leaf.delta, 0);
}

// ═══════════════ ⑥ AI 解析链（纯函数，不触网） ═══════════════

fn concept_v3(text: &str, necessity: ai::Necessity, evidence: Option<&str>) -> ai::SearchConceptV3 {
    ai::SearchConceptV3 {
        text: text.into(),
        role: "scene".into(),
        facet_hint: None,
        confidence: Some(0.9),
        necessity,
        weight: None,
        evidence: evidence.map(|e| e.into()),
        term_match: Default::default(),
    }
}

fn intent_v3(
    concepts: Vec<ai::SearchConceptV3>,
    preferred: Vec<ai::SearchConceptV3>,
) -> ai::SearchIntentV3 {
    ai::SearchIntentV3 {
        groups: vec![ai::SearchGroupV3 {
            asset_type: "all".into(),
            concepts,
            text_terms: vec![],
            metadata: vec![],
            untagged_only: false,
            preferred,
        }],
        exclusions: vec![],
        sort_by: None,
        sort_dir: None,
    }
}

/// 用户输入乱码/非 JSON → 三层降级必须兜成关键词，永不报错。
#[test]
fn sim_ai_garbage_input_falls_back_to_keyword() {
    let c = setup();
    let (intent, warnings) = ai::degrade_parse_v3("@@@ 不是 JSON @@@", "海边 日落", &[]);
    assert!(
        ai::is_keyword_fallback_v3(&intent, "海边 日落"),
        "非 JSON 必须落关键词兜底: {intent:?}"
    );
    assert!(!warnings.is_empty());
    // 兜底 plan 必须可执行（整句关键词）
    let (plan, _, _) = ai::build_plan_from_v3(&c, &intent).unwrap();
    search_plan::validate_search_plan(&plan).unwrap();
}

/// 用户说「海边，最好有日落」→ 日落应进 should（加分不淘汰），海边进 filter。
#[test]
fn sim_ai_preferred_becomes_should_not_filter() {
    let c = setup();
    let sea = tag_in(&c, "scene", "海边");
    let sun = tag_in(&c, "scene", "日落");
    let text = "海边，最好有日落";
    let intent = intent_v3(
        vec![concept_v3("海边", ai::Necessity::Required, None)],
        vec![concept_v3(
            "日落",
            ai::Necessity::Preferred,
            Some("最好有日落"),
        )],
    );
    // evidence 守卫（degrade_parse_v3 内会做；这里先手动验证守卫本身）
    let mut g = intent.groups[0].clone();
    let gw = ai::guard_preferred(text, &mut g);
    assert!(gw.is_empty(), "真子串+覆盖邻域的 evidence 不应被剔: {gw:?}");
    assert_eq!(g.preferred.len(), 1);

    let (plan, resolved, _) = ai::build_plan_from_v3(&c, &intent).unwrap();
    assert!(
        matches!(plan.ranking, Ranking::Relevance),
        "有加分项 → 相关度排序"
    );
    assert_eq!(plan.should.len(), 1, "preferred → should");
    assert!(plan.filter.is_some(), "海边 → filter");
    assert!(resolved.iter().any(|r| r.tag_id == sea));
    assert!(resolved.iter().any(|r| r.tag_id == sun));
    // should 不淘汰：只带海边的素材也必须在结果集里
    let a = add(&c, "f1.jpg");
    assign(&c, a, sea);
    let ids: Vec<i64> = search_plan::run_search_plan(&c, &plan, None, 0)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert!(ids.contains(&a), "加分项绝不应淘汰未命中素材: {ids:?}");
}

/// 用户说「最好是年轻女性」时，模型按规范词输出「青年」也必须保留为 should，
/// 不能因为 evidence 只出现同义词「年轻」而被守卫误删。
#[test]
fn sim_ai_preferred_canonical_age_survives_surface_alias() {
    let c = setup();
    let female = tag_in(&c, "people", "女性");
    let youth = tag_in(&c, "people", "青年");
    tags::add_alias(&c, youth, "年轻", None, "synonym").unwrap();
    let text = "要女生人像，在室内，最好是年轻女性";
    let intent = intent_v3(
        vec![concept_v3("女性", ai::Necessity::Required, None)],
        vec![ai::SearchConceptV3 {
            text: "青年".into(),
            role: "people".into(),
            facet_hint: Some("people".into()),
            confidence: Some(0.95),
            necessity: ai::Necessity::Preferred,
            weight: Some(1.0),
            evidence: Some("最好是年轻女性".into()),
            term_match: Default::default(),
        }],
    );
    let mut group = intent.groups[0].clone();
    assert!(ai::guard_preferred(text, &mut group).is_empty());
    assert_eq!(group.preferred.len(), 1);

    let intent = intent_v3(intent.groups[0].concepts.clone(), group.preferred.clone());
    let (plan, resolved, warnings) = ai::build_plan_from_v3(&c, &intent).unwrap();
    assert_eq!(plan.should.len(), 1, "青年应进入 should: {warnings:?}");
    assert!(resolved.iter().any(|r| r.tag_id == female));
    assert!(resolved.iter().any(|r| r.tag_id == youth));
    assert!(warnings.is_empty(), "规范名/别名证据不应告警: {warnings:?}");

    let preferred_asset = add(&c, "young.jpg");
    let other_asset = add(&c, "adult.jpg");
    assign(&c, preferred_asset, female);
    assign(&c, preferred_asset, youth);
    assign(&c, other_asset, female);
    let ids: Vec<i64> = search_plan::run_search_plan(&c, &plan, None, 0)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(ids, vec![preferred_asset, other_asset]);
}

/// 模型编造的 evidence（非原句子串）→ 加分项必须被丢弃并 warning，绝不升级为必须。
#[test]
fn sim_ai_fabricated_evidence_is_dropped() {
    let text = "海边 日落";
    let intent = intent_v3(
        vec![concept_v3("海边", ai::Necessity::Required, None)],
        vec![concept_v3(
            "日落",
            ai::Necessity::Preferred,
            Some("用户上次搜过"),
        )],
    );
    let mut g = intent.groups[0].clone();
    let warnings = ai::guard_preferred(text, &mut g);
    assert_eq!(g.preferred.len(), 0, "编造依据的加分项必须丢弃");
    assert!(
        warnings.iter().any(|w| w.contains("未落在原句")),
        "必须给出可理解 warning: {warnings:?}"
    );
}

/// 模型把同一概念同时给「包含」和「排除」→ 以排除为准，且查询必须仍可执行。
#[test]
fn sim_include_exclude_same_tag_exclusion_wins() {
    let c = setup();
    let sea = tag_in(&c, "scene", "海边");
    let mut intent = intent_v3(
        vec![concept_v3("海边", ai::Necessity::Required, None)],
        vec![],
    );
    intent.exclusions = vec![concept_v3("海边", ai::Necessity::Required, None)];
    let (expr, _r, w) = ai::build_expr_from_v2(&c, &ai::v3_to_v2_view(&intent)).unwrap();
    assert!(
        w.iter().any(|x| x.contains("同时被包含与排除")),
        "撞车必须提示: {w:?}"
    );
    // filter 里的正向海边 leaf 应被移除，expr 只剩 NOT
    let e = expr.expect("应有 expr");
    let s = serde_json::to_string(&e).unwrap();
    assert!(
        !s.contains(&format!("\"tagIds\":[{sea}]")) || s.contains("not"),
        "正向 leaf 应被排除规则移除: {s}"
    );

    let (plan, _, _) = ai::build_plan_from_v3(&c, &intent).unwrap();
    search_plan::validate_search_plan(&plan).unwrap();
    let a = add(&c, "g1.jpg");
    assign(&c, a, sea);
    let b = add(&c, "g2.jpg");
    let _ = b;
    let ids: Vec<i64> = search_plan::run_search_plan(&c, &plan, None, 0)
        .unwrap()
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert!(!ids.contains(&a), "被排除的素材不得出现在结果: {ids:?}");
}

// ═══════════════ ⑦ must_not 极性白名单 ═══════════════

#[test]
fn sim_must_not_polarity_whitelist() {
    let plan_with_exclude_tag = SearchPlanV3 {
        must_not: Some(leaf_expr(LeafCond::ExcludeTag {
            facet_key: String::new(),
            tag_ids: vec![1],
        })),
        ..Default::default()
    };
    assert!(
        search_plan::validate_search_plan(&plan_with_exclude_tag).is_err(),
        "ExcludeTag 进 must_not 必须被拒绝（双重否定）"
    );
    let plan_with_not = SearchPlanV3 {
        must_not: Some(QueryExpr::Not {
            child: Box::new(leaf_expr(tag_leaf("scene", 1))),
        }),
        ..Default::default()
    };
    assert!(
        search_plan::validate_search_plan(&plan_with_not).is_err(),
        "QueryExpr::Not 进 must_not 必须被拒绝（三重否定）"
    );
    let plan_positive = SearchPlanV3 {
        must_not: Some(leaf_expr(tag_leaf("scene", 1))),
        ..Default::default()
    };
    assert!(search_plan::validate_search_plan(&plan_positive).is_ok());
}

// ═══════════════ ⑧ 压力：软排序全库 + 12 条加分项不炸 ═══════════════

#[test]
fn sim_stress_empty_filter_soft_sort_with_12_should() {
    let c = setup();
    let mut tag_ids = Vec::new();
    for i in 0..12 {
        tag_ids.push(tag_in(&c, "scene", &format!("加分{i}")));
    }
    for i in 0..800 {
        let id = add(&c, &format!("s{i}.jpg"));
        if i % 50 == 0 {
            assign(&c, id, tag_ids[i % 12]);
        }
    }
    let should: Vec<ShouldClause> = tag_ids
        .iter()
        .map(|&t| ShouldClause {
            cond: tag_leaf("scene", t),
            weight: 1.0,
            label: "x".into(),
            evidence: None,
        })
        .collect();
    let plan = SearchPlanV3 {
        filter: None,
        should,
        minimum_should_match: 0,
        ranking: Ranking::Relevance,
        ..Default::default()
    };
    search_plan::validate_search_plan(&plan).unwrap();
    let page = search_plan::run_plan_page(&c, &plan, 0, Some(200)).unwrap();
    assert_eq!(page.items.len(), 200);
    let _ = page; // total 语义由 ④ 覆盖；本用例只验证空 filter + 12 加分可执行
}
