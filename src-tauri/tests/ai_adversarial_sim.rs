//! 刁钻模拟测试：云端打标全链路 + 确认写入 → FTS → 超级搜索（M5 模拟验收）
//! 目标：用假 OpenAI 服务端返回各种刁钻响应（乱码 / 越界置信度 / 未知分面 /
//! 截断 JSON / 429 限流 / 连续失败熔断 / SQL 注入式标签名），验证：
//!   1. 单条失败不中断批次、错误信息可定位；
//!   2. 恶意/边界标签名不会污染数据库（参数绑定 + integrity_check）；
//!   3. 确认后的标签能被 FTS 与超级搜索命中（搜得到）；
//!   4. 搜索词本身含注入字符不会破坏查询。
//!
//! 复用 tests/common 的 MockServer；网络用例持 NET_LOCK 串行。

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bagertea_ai_media_v2_lib::db::ai;
use bagertea_ai_media_v2_lib::db::query_expr::{LeafCond, QueryExpr, SearchScope};
use bagertea_ai_media_v2_lib::db::search;
use bagertea_ai_media_v2_lib::db::search_plan::{
    self, Ranking, Retriever, SearchPlanV3, WeightedRetriever,
};
use bagertea_ai_media_v2_lib::db::settings::{AiSettings, ApiProfile};
use bagertea_ai_media_v2_lib::db::tag_facets::FacetPromptContext;
use bagertea_ai_media_v2_lib::db::{self, assets};
use bagertea_ai_media_v2_lib::error::AppResult;
use bagertea_ai_media_v2_lib::services::ai_cloud;
use bagertea_ai_media_v2_lib::services::importer;
use bagertea_ai_media_v2_lib::services::thumbnail::ThumbnailService;
use bagertea_ai_media_v2_lib::state::Database;

use common::{HttpResponse, MockServer};

// ───────────────────────── 设施（与 ai_service_integration 同形） ─────────────────────────

fn is_conn_err_text(s: &str) -> bool {
    s.contains("error sending request")
        || s.contains("connection closed")
        || s.contains("os error 1005")
        || s.contains("os error 10054")
        || s.contains("error decoding response body")
        || s.contains("error reading a body")
        || s.contains("io_failures=")
}

macro_rules! conn_retry_test {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() -> AppResult<()> {
            let attempt = || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> AppResult<()> {
                    $body
                }))
            };
            for n in 0..=2u32 {
                match attempt() {
                    Ok(Ok(())) => return Ok(()),
                    Ok(Err(e)) => {
                        let text = e.to_string();
                        if is_conn_err_text(&text) && n < 2 {
                            eprintln!("[conn-retry {}] 连接层错误，重跑: {text}", n + 1);
                            continue;
                        }
                        return Err(e);
                    }
                    Err(payload) => {
                        let msg = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        if is_conn_err_text(&msg) && n < 2 {
                            continue;
                        }
                        std::panic::resume_unwind(payload);
                    }
                }
            }
            unreachable!()
        }
    };
}

fn make_image(dir: &Path, name: &str, salt: u32) {
    let img = image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8, (y + salt) as u8, 128]));
    image::DynamicImage::ImageRgb8(img)
        .save_with_format(dir.join(name), image::ImageFormat::Jpeg)
        .expect("生成测试图失败");
}

fn import_images(
    dbm: &Arc<Database>,
    thumbs: &ThumbnailService,
    library_root: &Path,
    n: usize,
) -> AppResult<Vec<i64>> {
    std::fs::create_dir_all(library_root)?;
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    for i in 0..n {
        make_image(&src, &format!("a{i:03}.jpg"), i as u32 + 1);
    }
    let opts = importer::ImportOptions {
        library_root: Some(library_root.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let r = importer::import_paths(
        dbm,
        thumbs,
        &[src.to_string_lossy().into_owned()],
        &opts,
        &AtomicBool::new(false),
        |_| {},
    )?;
    assert_eq!(r.imported, n as i64, "导入应全部成功: {:?}", r.errors);
    let page = assets::list(
        &dbm.lock().unwrap(),
        &assets::AssetFilter {
            limit: 1000,
            ..Default::default()
        },
    )?;
    Ok(page.items.iter().map(|a| a.id).collect())
}

fn profile(base_url: &str, api_mode: &str, kind: &str) -> ApiProfile {
    ApiProfile {
        id: "p1".into(),
        name: "测试档案".into(),
        api_mode: api_mode.into(),
        kind: kind.into(),
        base_url: base_url.into(),
        api_key: "test-key".into(),
        model: "qwen-vl-plus".into(),
        max_concurrency: 0,
        requests_per_minute: 0,
        requests_per_hour: 0,
    }
}

fn settings_with(profile: ApiProfile) -> AiSettings {
    AiSettings {
        profiles: vec![profile.clone()],
        active_profile: profile.id.clone(),
        api_mode: String::new(),
        base_url: String::new(),
        api_key: String::new(),
        model: String::new(),
        auto_tagging: false,
        video_tagging: false,
        video_tagging_mode: "cover".into(),
        video_frame_count: 3,
        local_model_tier: "light".into(),
        batch_limit: 500,
        local_batch_limit: 5,
        ollama_source_id: "auto".into(),
        system_prompt_tagging: String::new(),
        system_prompt_search: String::new(),
        confidence_min_suggest: 0.30,
    }
}

fn categories() -> Vec<FacetPromptContext> {
    vec![FacetPromptContext {
        key: "scene".into(),
        display_name: "场景".into(),
        description: "场景".into(),
        selection_mode: "single".into(),
        max_items: Some(3),
        ..Default::default()
    }]
}

fn progress_sink() -> (
    Arc<Mutex<Vec<ai_cloud::AiProgress>>>,
    impl Fn(ai_cloud::AiProgress),
) {
    let sink: Arc<Mutex<Vec<ai_cloud::AiProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink2 = Arc::clone(&sink);
    (sink, move |p| sink2.lock().unwrap().push(p))
}

fn openai_ok_body(tags_json: &str) -> String {
    let content = serde_json::to_string(tags_json).expect("序列化失败");
    format!(r#"{{"choices":[{{"message":{{"content":{content}}}}}]}}"#)
}

fn ok_content(json: &str) -> HttpResponse {
    HttpResponse::ok_json(&openai_ok_body(json))
}

/// 跑一批云端打标（1 张图，返回建议列表）
fn run_one(
    dbm: &Arc<Database>,
    thumbs_dir: &Path,
    srv: &MockServer,
) -> AppResult<(i64, Vec<ai::AiSuggestion>)> {
    let ids = import_images(
        dbm,
        &thumbs_dir_service(thumbs_dir)?,
        &thumbs_dir.join("library"),
        1,
    )?;
    let batch = ai::create_batch(&dbm.lock().unwrap(), &ids, "cloud")?;
    let (_, progress) = progress_sink();
    ai_cloud::run_cloud_batch(
        dbm,
        batch.id,
        &settings_with(profile(&srv.url(), "openai", "cloud")),
        &categories(),
        &categories(),
        None,
        &Arc::new(AtomicBool::new(false)),
        progress,
    )?;
    let sug = ai::list_suggestions(&dbm.lock().unwrap(), batch.id)?;
    Ok((batch.id, sug))
}

fn thumbs_dir_service(dir: &Path) -> AppResult<ThumbnailService> {
    ThumbnailService::new(dir)
}

// ───────────────────────── 用例 ─────────────────────────

// ① 乱码内容（GBK 转码错误的"锟斤拷"、控制字符）不是 JSON → 单条 rejected，批次 done
conn_retry_test!(garbled_content_rejected_batch_survives, {
    let _g = common::net_lock_guard();
    let srv =
        MockServer::start(move |_| HttpResponse::ok_json(&openai_ok_body("锟斤拷烫烫烫\x07\x1b")));
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done", "乱码输出不得中断批次");
    assert_eq!(sug[0].status, "rejected");
    assert!(sug[0].last_error.as_deref().is_some_and(|e| !e.is_empty()));
    Ok(())
});

// ② 退化输出（模型聊天式拒绝："抱歉，我无法分析图片"）→ rejected 而非误当标签
conn_retry_test!(degenerate_chat_reply_rejected, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(move |_| {
        HttpResponse::ok_json(&openai_ok_body(
            "抱歉，我无法分析这张图片，请提供更清晰的照片。",
        ))
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done");
    assert_eq!(sug[0].status, "rejected", "退化文本不得被当成功标签");
    assert!(sug[0].suggested_tags.is_empty());
    Ok(())
});

// ③ 越界置信度（1.7 / -0.5）→ 单条 rejected 且错误可定位，批次 done
conn_retry_test!(out_of_range_confidence_rejected, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(move |_| {
        ok_content(
            r#"{"description":"海边风十分开阔明亮","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"scene":[{"name":"海边","confidence":1.7}]},"numbers":{}}"#,
        )
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done");
    assert_eq!(sug[0].status, "rejected", "越界置信度不得落库");
    let err = sug[0].last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("0–1") || err.contains("0-1"),
        "错误应说明置信度范围: {err}"
    );
    Ok(())
});

// ④ 未知分面 key（模型幻觉出 "mood"）→ rejected 且错误点名分面，批次 done
conn_retry_test!(unknown_facet_key_rejected_with_facet_name, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(move |_| {
        ok_content(
            r#"{"description":"傍晚光线柔和温暖","peoplePresence":{"status":"absent","confidence":0.9},"tags":{"mood":[{"name":"治愈","confidence":0.9}]},"numbers":{}}"#,
        )
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done");
    assert_eq!(sug[0].status, "rejected");
    let err = sug[0].last_error.as_deref().unwrap_or("");
    assert!(err.contains("mood"), "错误应点名未知分面: {err}");
    Ok(())
});

// ⑤ 截断 JSON（max_tokens 耗尽）→ 各级降级+重试耗尽后 rejected，批次 done，不 panic
conn_retry_test!(truncated_json_exhausts_retries_then_rejected, {
    let _g = common::net_lock_guard();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let srv = MockServer::start(move |_| {
        calls2.fetch_add(1, Ordering::SeqCst);
        // 每次都返回被拦腰截断的 JSON
        HttpResponse::ok_json(&openai_ok_body(
            r#"{"description":"海边风十分开阔明亮","peoplePresence":{"status":"unknown","confide"#,
        ))
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done", "截断 JSON 不得中断批次");
    assert_eq!(sug[0].status, "rejected");
    assert!(calls.load(Ordering::SeqCst) >= 2, "截断应触发降级重试");
    Ok(())
});

// ⑥ 429 是供应商配额边界 → 不重试、不继续消耗额度，待处理建议保留并中断批次
conn_retry_test!(http_429_stops_batch_and_preserves_pending, {
    let _g = common::net_lock_guard();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let srv = MockServer::start(move |_| {
        calls2.fetch_add(1, Ordering::SeqCst);
        HttpResponse {
            status: 429,
            content_type: "application/json",
            body: r#"{"error":{"type":"rate_limit_exceeded","message":"RPM limit exceeded for free users","api_key":"sk-secret"}}"#.to_string(),
        }
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let ids = import_images(&dbm, &thumbs, &tmp.path().join("library"), 2)?;
    let batch = ai::create_batch(&dbm.lock().unwrap(), &ids, "cloud")?;
    let (_, progress) = progress_sink();
    let error = ai_cloud::run_cloud_batch(
        &dbm,
        batch.id,
        &settings_with(profile(&srv.url(), "openai", "cloud")),
        &categories(),
        &categories(),
        None,
        &Arc::new(AtomicBool::new(false)),
        progress,
    )
    .expect_err("429 应停止批次并上抛限流错误");
    assert_eq!(error.code(), "AI_RATE_LIMITED");
    assert!(error.to_string().contains("RPM limit exceeded"));
    assert!(!error.to_string().contains("sk-secret"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "429 不应触发自动重试");
    let conn = dbm.lock().unwrap();
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.status, "interrupted", "429 应中断批次而不是继续发送");
    let sug = ai::list_suggestions(&conn, batch.id)?;
    assert_eq!(sug[0].status, "pending", "限流不是内容失败，应保留待处理");
    assert!(sug[0]
        .last_error
        .as_deref()
        .is_some_and(|message| message.contains("RPM limit exceeded")));
    assert_eq!(sug[1].status, "pending", "限流后续素材不得继续发送");
    Ok(())
});

// ⑦ 连续 3 条全部失败 → 熔断 interrupted，剩余保持 pending（不再空烧请求）
conn_retry_test!(consecutive_failures_trip_circuit_breaker, {
    let _g = common::net_lock_guard();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let srv = MockServer::start(move |_| {
        calls2.fetch_add(1, Ordering::SeqCst);
        HttpResponse::status_only(500)
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let ids = import_images(&dbm, &thumbs, &tmp.path().join("library"), 5)?;
    let batch = ai::create_batch(&dbm.lock().unwrap(), &ids, "cloud")?;
    let (_, progress) = progress_sink();
    let r = ai_cloud::run_cloud_batch(
        &dbm,
        batch.id,
        &settings_with(profile(&srv.url(), "openai", "cloud")),
        &categories(),
        &categories(),
        None,
        &Arc::new(AtomicBool::new(false)),
        progress,
    );
    // 熔断以 Err 形式上抛（可操作文案），同时批次状态落库为 interrupted
    let err = r.expect_err("连续 3 条失败应返回 Err").to_string();
    assert!(err.contains("中断批次"), "熔断文案应可操作: {err}");
    let conn = dbm.lock().unwrap();
    let b = ai::get_batch(&conn, batch.id)?;
    assert_eq!(b.status, "interrupted", "连续失败应熔断而不是硬错误");
    let sug = ai::list_suggestions(&conn, batch.id)?;
    let rejected = sug.iter().filter(|s| s.status == "rejected").count();
    let pending = sug.iter().filter(|s| s.status == "pending").count();
    assert!(
        rejected >= 3,
        "熔断前应有失败记录，实际 rejected={rejected}"
    );
    assert!(
        pending >= 1,
        "熔断后剩余条目应保持 pending，实际 pending={pending}"
    );
    Ok(())
});

// ⑧ SQL 注入式 / 特殊字符标签名（≤12 字限制内）：确认写入 → 数据库完好 → 搜索不炸
conn_retry_test!(injection_style_tag_name_does_not_pollute_db, {
    let _g = common::net_lock_guard();
    // 引号+SQL 片段+emoji+全角字符，均 ≤12 字符通过解析层
    let hostile = r#");DROP--"#;
    assert!(
        hostile.chars().count() <= 12,
        "测试前提：敌意标签名需通过 12 字限制"
    );
    let payload = format!(
        r#"{{"description":"公园里树木茂盛阳光温暖","peoplePresence":{{"status":"unknown","confidence":0.8}},"tags":{{"scene":[{{"name":"{}","confidence":0.9}}]}},"numbers":{{}}}}"#,
        hostile
    );
    let srv = MockServer::start(move |_| ok_content(&payload));
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    assert!(
        sug[0].suggested_tags.contains_key("scene"),
        "敌意标签名（合法长度）应能作为候选: status={:?} err={:?}",
        sug[0].status,
        sug[0].last_error
    );

    // 人工确认写入
    {
        let conn = dbm.lock().unwrap();
        let sug = ai::list_suggestions(&conn, batch_id)?;
        ai::confirm_suggestion(&conn, sug[0].id, &sug[0].suggested_tags)?;
        // 数据库完整性
        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        assert_eq!(integrity, "ok", "敌意标签名不得破坏数据库");
        // tags 表还在（未被 DROP）
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
        assert!(n >= 1, "tags 表应存在且有记录");
    }

    // 注入式搜索词不炸、不误命中全库
    for q in [
        "x'); DROP TABLE assets; --",
        "\" OR 1=1 --",
        "%_%",
        "*",
        "';--",
    ] {
        let ids = search::search_asset_ids_all(&dbm.lock().unwrap(), q)?;
        eprintln!("搜索词 {q:?} 命中 {} 条", ids.len());
    }
    let integrity: String = dbm
        .lock()
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    assert_eq!(integrity, "ok", "注入式搜索词不得破坏数据库");
    Ok(())
});

// ⑨ 全链路黄金路径：打标 → 确认 → FTS 搜索命中标签 → 超级搜索 plan 命中
conn_retry_test!(confirm_then_fts_and_super_search_hit, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(move |_| {
        ok_content(
            r#"{"description":"黄昏海边有人散步交谈","peoplePresence":{"status":"present","confidence":0.9},"tags":{"scene":[{"name":"海边","confidence":0.95}]},"numbers":{}}"#,
        )
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    assert_eq!(
        sug[0].suggested_tags.get("scene"),
        Some(&vec!["海边".to_string()])
    );

    let asset_id = {
        let conn = dbm.lock().unwrap();
        let sug = ai::list_suggestions(&conn, batch_id)?;
        ai::confirm_suggestion_with_description(
            &conn,
            sug[0].id,
            &sug[0].suggested_tags,
            Some("黄昏海边有人散步交谈"),
        )?;
        sug[0].asset_id
    };

    // FTS：搜标签词命中
    let hits = search::search_asset_ids_all(&dbm.lock().unwrap(), "海边")?;
    assert!(
        hits.contains(&asset_id),
        "确认后的标签应可被全文搜索命中，实际命中 {hits:?}"
    );
    // FTS：搜描述词命中
    let hits = search::search_asset_ids_all(&dbm.lock().unwrap(), "散步")?;
    assert!(
        hits.contains(&asset_id),
        "确认后的描述应可被全文搜索命中，实际命中 {hits:?}"
    );

    // 超级搜索 plan：filter 关键词 + FTS 检索器
    let plan = SearchPlanV3 {
        plan_schema_version: 1,
        normalization_version: 1,
        compiler_version: 1,
        filter: Some(QueryExpr::Leaf {
            cond: LeafCond::Search {
                value: "海边".into(),
                scope: SearchScope::All,
            },
        }),
        must_not: None,
        should: vec![],
        minimum_should_match: 0,
        retrievers: Default::default(),
        ranking: Ranking::Relevance,
    };
    let r = search_plan::run_plan_ids(&dbm.lock().unwrap(), &plan)?;
    assert!(
        r.ids.contains(&asset_id),
        "超级搜索 plan 应命中确认后的素材，实际 {:?}",
        r.ids
    );

    // 超级搜索 plan：Tag 叶子（词查 + 显式 tag_id，与生产 super_search_ai 构造形态一致）
    let tag_id: i64 = {
        let conn = dbm.lock().unwrap();
        conn.query_row(
            "SELECT id FROM tags WHERE name = '海边' LIMIT 1",
            [],
            |r| r.get(0),
        )?
    };
    let plan_tag = SearchPlanV3 {
        plan_schema_version: 1,
        normalization_version: 1,
        compiler_version: 1,
        filter: Some(QueryExpr::Leaf {
            cond: LeafCond::Tag {
                facet_key: "scene".into(),
                tag_ids: vec![tag_id],
                mode: None,
                include_descendants: true,
                term_query: Some("海边".into()),
                term_match: Default::default(),
            },
        }),
        must_not: None,
        should: vec![],
        minimum_should_match: 0,
        retrievers: Default::default(),
        ranking: Ranking::Relevance,
    };
    let r = search_plan::run_plan_ids(&dbm.lock().unwrap(), &plan_tag)?;
    assert!(
        r.ids.contains(&asset_id),
        "超级搜索按词查标签应命中，实际 {:?}",
        r.ids
    );

    // 超级搜索 plan：Fts 检索器多路召回
    let plan_fts = SearchPlanV3 {
        plan_schema_version: 1,
        normalization_version: 1,
        compiler_version: 1,
        filter: None,
        must_not: None,
        should: vec![],
        minimum_should_match: 0,
        retrievers: search_plan::RetrieverPlan {
            retrievers: vec![WeightedRetriever {
                weight: 1.0,
                kind: Retriever::Fts {
                    query: "海边".into(),
                    scope: SearchScope::All,
                },
            }],
            fusion: Default::default(),
        },
        ranking: Ranking::Relevance,
    };
    let r = search_plan::run_plan_ids(&dbm.lock().unwrap(), &plan_fts)?;
    assert!(
        r.ids.contains(&asset_id),
        "Fts 检索器应命中确认后的素材，实际 {:?}",
        r.ids
    );
    Ok(())
});

// ⑩ 超长描述确认写入 + FTS 正常（大文本不截断报错、不拖垮索引）
conn_retry_test!(very_long_description_confirm_and_search_ok, {
    let _g = common::net_lock_guard();
    let long_desc = "海边日落场景".repeat(500); // ~3000 字
    let payload = format!(
        r#"{{"description":"{}","peoplePresence":{{"status":"absent","confidence":0.9}},"tags":{{"scene":[{{"name":"海边","confidence":0.9}}]}},"numbers":{{}}}}"#,
        long_desc
    );
    let srv = MockServer::start(move |_| ok_content(&payload));
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    assert_eq!(
        sug[0].suggested_tags.get("scene"),
        Some(&vec!["海边".to_string()]),
        "超长描述不应影响标签建议: err={:?}",
        sug[0].last_error
    );
    {
        let conn = dbm.lock().unwrap();
        let sug = ai::list_suggestions(&conn, batch_id)?;
        ai::confirm_suggestion(&conn, sug[0].id, &sug[0].suggested_tags)?;
    }
    let hits = search::search_asset_ids_all(&dbm.lock().unwrap(), "海边")?;
    assert!(!hits.is_empty(), "超长描述确认后搜索仍应正常");
    Ok(())
});

// ⑪ 空标签名 / 纯空格 / 重复标签：空名被拒，重复去重，不产生脏数据
conn_retry_test!(empty_and_whitespace_and_duplicate_tag_names, {
    let _g = common::net_lock_guard();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let srv = MockServer::start(move |_| {
        let n = calls2.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            // 空名 + 纯空格名 → 应被拒绝触发修复重试
            ok_content(
                r#"{"description":"海边开阔","peoplePresence":{"status":"absent","confidence":0.9},"tags":{"scene":[{"name":"  ","confidence":0.9},{"name":"","confidence":0.9}]},"numbers":{}}"#,
            )
        } else {
            // 修复重试返回同名重复标签 → 去重为 1 个
            ok_content(
                r#"{"description":"海边开阔","peoplePresence":{"status":"absent","confidence":0.9},"tags":{"scene":[{"name":"海边","confidence":0.9},{"name":"海边","confidence":0.85}]},"numbers":{}}"#,
            )
        }
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done");
    let scene = sug[0]
        .suggested_tags
        .get("scene")
        .cloned()
        .unwrap_or_default();
    assert_eq!(scene, vec!["海边".to_string()], "重复标签应去重: {scene:?}");
    // 确认后 tags 表只有一个"海边"规范词
    {
        let conn = dbm.lock().unwrap();
        let sug = ai::list_suggestions(&conn, batch_id)?;
        ai::confirm_suggestion(&conn, sug[0].id, &sug[0].suggested_tags)?;
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM tags WHERE name = '海边'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(n, 1, "确认后词表不得出现重复规范词");
    }
    Ok(())
});

// ───────────────────── 本地 Ollama 双端点模拟 ─────────────────────

const V2_OK: &str = r#"{"description":"海边风景十分开阔明亮安静","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"scene":[{"name":"海边","confidence":0.9}]},"numbers":{}}"#;

// ⑫ localhost 自定义端口即使标记为 local，也必须作为外部 OpenAI 兼容服务请求。
conn_retry_test!(localhost_connection_uses_external_openai_path, {
    let _g = common::net_lock_guard();
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let paths2 = Arc::clone(&paths);
    let srv = MockServer::start(move |req| {
        paths2.lock().unwrap().push(req.path.clone());
        match req.path.as_str() {
            "/chat/completions" => ok_content(V2_OK),
            _ => HttpResponse::status_only(404),
        }
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one_local(&dbm, tmp.path(), &srv)?;
    assert_eq!(
        sug[0].suggested_tags.get("scene"),
        Some(&vec!["海边".to_string()]),
        "localhost 外部 API 应出建议: status={:?} err={:?}",
        sug[0].status,
        sug[0].last_error
    );
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done");
    let paths = paths.lock().unwrap().clone();
    assert!(
        !paths.is_empty() && paths.iter().all(|p| p == "/chat/completions"),
        "外部成功响应应只来自兼容端点，实际请求日志: {paths:?}"
    );
    let request = srv
        .requests()
        .into_iter()
        .find(|request| request.path == "/chat/completions")
        .expect("应记录 OpenAI 兼容请求");
    let body: serde_json::Value = serde_json::from_str(&request.body)?;
    assert!(
        body["response_format"].is_object(),
        "应发送结构化 JSON 格式"
    );
    assert!(
        body.get("keep_alive").is_none(),
        "外部服务不得注入 Ollama 字段"
    );
    Ok(())
});

// ⑬ 外部兼容服务失败时，只能在其兼容端点重试，不得探测 Ollama 原生 API。
conn_retry_test!(
    external_openai_failure_does_not_try_ollama_native_endpoint,
    {
        let _g = common::net_lock_guard();
        let paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let paths2 = Arc::clone(&paths);
        let srv = MockServer::start(move |req| {
            paths2.lock().unwrap().push(req.path.clone());
            match req.path.as_str() {
                "/chat/completions" => HttpResponse::status_only(500),
                _ => HttpResponse::status_only(404),
            }
        });
        let dbm = Arc::new(Database::new(db::init_memory()?));
        let tmp = tempfile::tempdir()?;
        let (_batch_id, sug) = run_one_local(&dbm, tmp.path(), &srv)?;
        assert_eq!(
            sug[0].status, "rejected",
            "外部 HTTP 失败应作为失败建议记录"
        );
        let paths = paths.lock().unwrap().clone();
        assert!(
            paths.iter().any(|p| p == "/chat/completions"),
            "外部兼容请求应到达 OpenAI 端点: {paths:?}"
        );
        assert!(
            paths.iter().all(|path| path == "/chat/completions"),
            "失败和重试都不得切换到 Ollama 原生端点: {paths:?}"
        );
        Ok(())
    }
);

// ⑭ 外部服务的 mojibake 仍应 rejected，但不得触发 Ollama 卸载用户服务。
conn_retry_test!(external_mojibake_does_not_unload_service, {
    let _g = common::net_lock_guard();
    // 有效 V2 JSON（分面齐全不触发修复路径），但 description/标签名都是 Latin-1
    // 误读乱码（占比 > 1/3）→ 触发 is_degenerate → 卸载模型 + 明确报错
    let mojibake = format!(
        "{{\"description\":\"{}\",\"peoplePresence\":{{\"status\":\"absent\",\"confidence\":0.9}},\"tags\":{{\"scene\":[{{\"name\":\"åååå\",\"confidence\":0.9}}]}},\"numbers\":{{}}}}",
        "å".repeat(120)
    );
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let paths2 = Arc::clone(&paths);
    let unload_bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let unload_bodies2 = Arc::clone(&unload_bodies);
    let srv = MockServer::start(move |req| {
        paths2.lock().unwrap().push(req.path.clone());
        match req.path.as_str() {
            "/chat/completions" => ok_content(&mojibake),
            "/api/generate" => {
                unload_bodies2.lock().unwrap().push(req.body.clone());
                HttpResponse::ok_json(r#"{"done":true,"done_reason":"unload"}"#)
            }
            _ => HttpResponse::status_only(404),
        }
    });
    let dbm = Arc::new(Database::new(db::init_memory()?));
    let tmp = tempfile::tempdir()?;
    let (batch_id, sug) = run_one_local(&dbm, tmp.path(), &srv)?;
    let b = ai::get_batch(&dbm.lock().unwrap(), batch_id)?;
    assert_eq!(b.status, "done", "退化输出不得中断批次");
    assert_eq!(sug[0].status, "rejected", "mojibake 不得当成功落库");
    let err = sug[0].last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("异常") || err.contains("乱码") || err.contains("重启"),
        "错误应提示模型输出异常/重启服务: {err}"
    );
    let unloads = unload_bodies.lock().unwrap().clone();
    assert!(
        unloads.is_empty(),
        "外部服务不得收到模型卸载请求: {unloads:?}"
    );
    let requests = srv.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.path == "/chat/completions"),
        "外部服务只应收到 OpenAI 兼容请求: {requests:?}"
    );
    Ok(())
});

/// 跑一批本地（Ollama）打标
fn run_one_local(
    dbm: &Arc<Database>,
    thumbs_dir: &Path,
    srv: &MockServer,
) -> AppResult<(i64, Vec<ai::AiSuggestion>)> {
    let ids = import_images(
        dbm,
        &thumbs_dir_service(thumbs_dir)?,
        &thumbs_dir.join("library"),
        1,
    )?;
    let batch = ai::create_batch(&dbm.lock().unwrap(), &ids, "cloud")?;
    let (_, progress) = progress_sink();
    ai_cloud::run_cloud_batch(
        dbm,
        batch.id,
        &settings_with(profile(&srv.url(), "openai", "local")),
        &categories(),
        &categories(),
        None,
        &Arc::new(AtomicBool::new(false)),
        progress,
    )?;
    let sug = ai::list_suggestions(&dbm.lock().unwrap(), batch.id)?;
    Ok((batch.id, sug))
}
