//! 超级搜索 AI 解析批量压测（一次性工具，不进主程序）。
//! 复用后端真实函数：连真实素材库 → 读 super_search 用途绑定的模型 → 逐条发真实请求
//! → degrade 解析 → build expr/plan → validate，采集每条状态与完整产出。
//! 运行：$env:SS_OUT="<绝对路径.json>"; cargo run --example ss_batch
//! 可选：SS_CASES="15,19,24" 只跑指定 1-based 序号；SS_SLEEP=1500 每条间隔毫秒。
use bagertea_ai_media_v2_lib::db::ai_connections;
use bagertea_ai_media_v2_lib::db::query_expr::{validate_expr, LeafCond, QueryExpr};
use bagertea_ai_media_v2_lib::db::{settings, tag_facets};
use bagertea_ai_media_v2_lib::services::credentials;
use bagertea_ai_media_v2_lib::services::super_search_ai as ss;
use rusqlite::Connection;
use serde::Serialize;
use std::time::Instant;

// 30 条：覆盖单标签 / 多标签且 / 或 / 排除 / 优先 / 素材类型 / 文件大小 / 日期 /
// 视频时长 / 分辨率 / 横竖 / GPS / 颜色 / 文件名 / 未打标 / 超长复合 / 口语化。
const QUERIES: &[&str] = &[
    "草地的照片",
    "有桥的素材",
    "拍了行人的",
    "现代风格的建筑",
    "城市里的建筑",
    "河边的树",
    "白天拍的公园全景",
    "阴天的单人中景",
    "草地或者绿地",
    "桥或者河边",
    "建筑但不要行人",
    "白天的公园，不要树",
    "城市街景，最好有行人",
    "草地，优先近景，尽量自然光",
    "视频素材",
    "草地的图片",
    "大于5MB的素材",
    "10MB到50MB之间的视频",
    "小于2MB的图片",
    "2025年拍的照片",
    "今年拍的白天素材",
    "时长10秒以上的视频",
    "5到30秒的短视频",
    "4K分辨率的素材",
    "横构图的照片",
    "有拍摄定位的照片",
    "偏绿色调的图片",
    "文件名包含IMG的素材",
    "没有打标签的素材",
    "2025年白天在城市拍的建筑全景照片，大于3MB，不要行人，最好有自然光",
];

#[derive(Default, Serialize)]
struct Counts {
    leaf: i32,
    tag: i32,
    exclude_tag: i32,
    metadata: i32,
    search: i32,
    asset_type: i32,
    untagged: i32,
    facet: i32,
    and: i32,
    or: i32,
    not: i32,
}

fn walk(e: &QueryExpr, c: &mut Counts) {
    match e {
        QueryExpr::And { children } => {
            c.and += 1;
            children.iter().for_each(|x| walk(x, c));
        }
        QueryExpr::Or { children } => {
            c.or += 1;
            children.iter().for_each(|x| walk(x, c));
        }
        QueryExpr::Not { child } => {
            c.not += 1;
            walk(child, c);
        }
        QueryExpr::Leaf { cond } => {
            c.leaf += 1;
            match cond {
                LeafCond::Tag { .. } => c.tag += 1,
                LeafCond::ExcludeTag { .. } => c.exclude_tag += 1,
                LeafCond::Metadata { .. } => c.metadata += 1,
                LeafCond::Search { .. } => c.search += 1,
                LeafCond::AssetType { .. } => c.asset_type += 1,
                LeafCond::Untagged => c.untagged += 1,
                LeafCond::FacetHasAny { .. }
                | LeafCond::FacetMissing { .. }
                | LeafCond::FacetNumber { .. } => c.facet += 1,
            }
        }
    }
}

#[derive(Serialize)]
struct Row {
    idx: usize,
    query: String,
    ok: bool,
    status: String,
    ms: u128,
    counts: Counts,
    should: usize,
    min_should: u32,
    warnings: Vec<String>,
    error: Option<String>,
    expr: Option<serde_json::Value>,
}

type ParseResult = (Option<QueryExpr>, usize, u32, Vec<String>, String, Counts);

fn main() {
    let db_path = std::env::var("SS_DB").ok().unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap()
            .join("bagertea_ai_media_v2")
            .join("library.db")
            .to_string_lossy()
            .to_string()
    });
    eprintln!("DB = {db_path}");
    let conn = Connection::open(&db_path).expect("open db");

    let mut settings = settings::get_settings(&conn).expect("settings");
    // §10：db 层只解析不含明文密钥的 profile，keyring 读取在锁外补全（示例为单连接顺序流程）。
    if let Some(bref) =
        ai_connections::usage_profile_ref(&conn, "super_search").expect("usage binding")
    {
        let key_ref = bref.api_key_ref.clone();
        ai_connections::apply_profile_to_cfg(&mut settings.ai, bref.profile);
        if let Some(id) = key_ref {
            let key = credentials::get_api_key(&id)
                .expect("keyring")
                .unwrap_or_default();
            if let Some(active) = settings.ai.profiles.iter_mut().find(|p| p.id == id) {
                active.api_key = key;
            }
        }
    }
    let cfg = settings.ai;
    match cfg.active() {
        Some(p) => eprintln!("model = {} @ {}", p.model, p.base_url),
        None => {
            eprintln!("no active AI profile, abort");
            return;
        }
    }

    let facets = tag_facets::build_prompt_context(&conn, "all").expect("facets");
    let dict = ss::collect_tag_dictionary(&conn, &facets).expect("dict");
    let caps = ss::library_capabilities(&conn).unwrap_or_default();

    // SS_CASES=逗号分隔的 1-based 序号只跑子集；SS_SLEEP=每条间隔毫秒（规避限流）
    let pick: Vec<usize> = std::env::var("SS_CASES")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|x| x.trim().parse::<usize>().ok())
                .collect()
        })
        .unwrap_or_default();
    let sleep_ms: u64 = std::env::var("SS_SLEEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let chosen: Vec<(usize, &&str)> = QUERIES
        .iter()
        .enumerate()
        .filter(|(i, _)| pick.is_empty() || pick.contains(&(i + 1)))
        .collect();
    eprintln!(
        "dict {} / facets {}，本次跑 {} 条，间隔 {sleep_ms}ms\n",
        dict.len(),
        facets.len(),
        chosen.len()
    );

    let mut rows: Vec<Row> = Vec::new();
    for (i, q) in &chosen {
        let (i, q) = (*i, **q);
        let t0 = Instant::now();
        let mut counts = Counts::default();
        let mut row = Row {
            idx: i + 1,
            query: (*q).to_string(),
            ok: false,
            status: "error".into(),
            ms: 0,
            counts: Counts::default(),
            should: 0,
            min_should: 0,
            warnings: Vec::new(),
            error: None,
            expr: None,
        };
        let run = (|| -> Result<ParseResult, String> {
            let (intent, mut w) =
                ss::request_intent(&cfg, q, &facets, &dict, &caps).map_err(|e| e.to_string())?;
            let kw = ss::is_keyword_fallback_v3(&intent, q);
            let v2 = ss::v3_to_v2_view(&intent);
            let (expr, _rt, ew) = ss::build_expr_from_v2(&conn, &v2).map_err(|e| e.to_string())?;
            let (plan, _pr, pw) =
                ss::build_plan_from_v3(&conn, &intent).map_err(|e| e.to_string())?;
            if let Some(e) = &expr {
                validate_expr(e).map_err(|x| x.to_string())?;
            }
            w.extend(ew);
            w.extend(pw);
            if let Some(e) = &expr {
                walk(e, &mut counts);
            }
            let status = if kw {
                "keyword"
            } else if w.is_empty() {
                "full"
            } else {
                "partial"
            };
            Ok((
                expr,
                plan.should.len(),
                plan.minimum_should_match,
                w,
                status.into(),
                std::mem::take(&mut counts),
            ))
        })();
        row.ms = t0.elapsed().as_millis();
        match run {
            Ok((expr, should, msm, w, status, c)) => {
                row.ok = true;
                row.status = status;
                row.should = should;
                row.min_should = msm;
                row.warnings = w;
                row.counts = c;
                row.expr = expr.and_then(|e| serde_json::to_value(e).ok());
            }
            Err(e) => row.error = Some(e),
        }
        eprintln!(
            "[{:>2}/{}] {:7} {:>5}ms leaf={} tag={} meta={} search={} type={} not={} should={} {}",
            row.idx,
            chosen.len(),
            row.status,
            row.ms,
            row.counts.leaf,
            row.counts.tag,
            row.counts.metadata,
            row.counts.search,
            row.counts.asset_type,
            row.counts.not,
            row.should,
            row.error.clone().unwrap_or_default()
        );
        rows.push(row);
        if sleep_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }
    }

    let out = std::env::var("SS_OUT").unwrap_or_else(|_| "ss_batch_result.json".into());
    std::fs::write(&out, serde_json::to_string_pretty(&rows).unwrap()).expect("write result");

    let (mut full, mut partial, mut keyword, mut error) = (0, 0, 0, 0);
    for r in &rows {
        match r.status.as_str() {
            "full" => full += 1,
            "partial" => partial += 1,
            "keyword" => keyword += 1,
            _ => error += 1,
        }
    }
    eprintln!("\n==== full={full} partial={partial} keyword={keyword} error={error} -> {out}");
}
