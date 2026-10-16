//! Ollama 一键部署服务层集成测试（TEST_STRATEGY M6 / G4）
//! 覆盖 ping 存活检测三态、pull 模型 NDJSON 流（成功/错误行/提前结束/取消/非200）。
//! HTTP mock 用 tests/common 的同步 mock（选型说明见 common/mod.rs 文件头）。
//! 网络类用例统一持 common::NET_LOCK 串行执行，消除 Windows 并行偶发连接失败。
//! 运行：cargo test --test ollama_service_integration

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use teacell_ai_media_manager_lib::error::AppResult;
use teacell_ai_media_manager_lib::services::ollama_setup::{self, PullProgress};

use common::{HttpResponse, MockServer};

/// Windows 本地回环瞬态连接失败（见 ai_service_integration.rs 的 conn_retry_test 说明，
/// 实测失败率 ~13-19%）的用例级重试外壳：仅当失败特征是连接层错误时重建 mock 重跑，
/// 业务断言失败不重试（不掩盖真错）。
macro_rules! conn_retry_test {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            let attempt = || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
            for n in 0..=2u32 {
                match attempt() {
                    Ok(()) => return,
                    Err(payload) => {
                        let msg = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        // ping 把连接失败降级为 fail 态（不带连接字样），
                        // "assertion failed: st.running" 即连接层抖动的表现形态
                        let is_conn = msg.contains("error sending request")
                            || msg.contains("连接本地服务失败")
                            || msg.contains("127.0.0.1")
                            || msg.contains("assertion failed: st.running")
                            || msg.contains("错误应含状态码")
                            || msg.contains("应失败");
                        if is_conn && n < 2 {
                            eprintln!(
                                "[conn-retry {}] 连接层错误，重建 mock 重跑: {}",
                                n + 1,
                                &msg.chars().take(160).collect::<String>()
                            );
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

fn tags_body(models: &[&str]) -> String {
    let arr: Vec<String> = models
        .iter()
        .map(|m| format!(r#"{{"name":"{m}"}}"#))
        .collect();
    format!(r#"{{"models":[{}]}}"#, arr.join(","))
}

fn pull_sink() -> (Arc<Mutex<Vec<PullProgress>>>, impl Fn(PullProgress)) {
    let sink: Arc<Mutex<Vec<PullProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink2 = Arc::clone(&sink);
    (sink, move |p| sink2.lock().unwrap().push(p))
}

// ───────────────────────── 用例 ─────────────────────────

// ping：/api/tags 正常 → running=true、is_ollama=true、模型列表解析
conn_retry_test!(ping_detects_ollama_with_models, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|req| {
        assert_eq!(req.path, "/api/tags");
        HttpResponse::ok_json(&tags_body(&["llava:latest", "qwen2.5vl:7b"]))
    });
    let st = ollama_setup::ping(&srv.url());
    assert!(st.running);
    assert!(st.is_ollama);
    assert_eq!(
        st.models,
        vec!["llava:latest".to_string(), "qwen2.5vl:7b".to_string()]
    );
});

// ping：tags 404 但 version 200 → 兼容服务（LM Studio 等），running=true、is_ollama=false
conn_retry_test!(ping_falls_back_to_version_heuristic, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|req| match req.path.as_str() {
        "/api/tags" => HttpResponse::status_only(404),
        "/api/version" => HttpResponse::ok_json(r#"{"version":"0.1.2"}"#),
        _ => HttpResponse::status_only(404),
    });
    let st = ollama_setup::ping(&srv.url());
    assert!(st.running);
    assert!(!st.is_ollama);
    assert!(st.models.is_empty());
});

/// ping：连接拒绝 → running=false（不报错，返回 fail 态）
#[test]
fn ping_connection_refused_returns_not_running() {
    let _g = common::net_lock_guard();
    let st = ollama_setup::ping("http://127.0.0.1:1");
    assert!(!st.running);
    assert!(!st.is_ollama);
}

// pull：NDJSON 流式进度 → 状态序列完整、最终 done，请求体带模型名 + stream:true，
// 且 base_url 带 /v1 时请求打到 api_root 剥离后的 /api/pull
conn_retry_test!(pull_streams_progress_until_success, {
    let run = || -> AppResult<()> {
        let _g = common::net_lock_guard();
        let srv = MockServer::start(|req| {
            assert_eq!(req.path, "/api/pull");
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
            assert_eq!(body["name"], "qwen2.5vl:3b");
            assert_eq!(body["stream"], true);
            HttpResponse::ok_json(
                "{\"status\":\"pulling manifest\"}\n\
             {\"status\":\"downloading digest\",\"total\":1000,\"completed\":250}\n\
             {\"status\":\"downloading digest\",\"total\":1000,\"completed\":1000}\n\
             {\"status\":\"verifying sha256 digest\"}\n\
             {\"status\":\"success\"}\n",
            )
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (sink, progress) = pull_sink();
        // 带 /v1 的档案 base_url：应剥离后请求 /api/pull
        ollama_setup::pull(
            &format!("{}/v1", srv.url()),
            "qwen2.5vl:3b",
            &cancel,
            progress,
        )?;

        let events = sink.lock().unwrap().clone();
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].status, "pulling manifest");
        assert!(!events[0].done);
        assert_eq!(events[2].completed, 1000);
        let last = events.last().unwrap();
        assert!(last.done);
        assert_eq!(last.status, "success");
        assert!(last.error.is_none());
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].path, "/api/pull",
            "应剥离开放兼容层 /v1 直连 Ollama 原生 API"
        );
        Ok(())
    };
    run().unwrap();
});

// pull：模型不存在 → error 行 → Err 带服务端错误文案
conn_retry_test!(pull_error_line_fails_with_server_message, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| {
        HttpResponse::ok_json(
            "{\"status\":\"pulling manifest\"}\n{\"error\":\"file does not exist\"}\n",
        )
    });
    let r = ollama_setup::pull(
        &srv.url(),
        "nope:latest",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let err = r.expect_err("应失败").to_string();
    assert!(
        err.contains("file does not exist"),
        "错误应透传服务端信息: {err}"
    );
});

// pull：流提前结束（只有 downloading 无 success）→ Err「拉取流提前结束」
conn_retry_test!(pull_stream_ends_without_success_fails, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| {
        HttpResponse::ok_json(
            "{\"status\":\"downloading digest\",\"total\":1000,\"completed\":500}\n",
        )
    });
    let r = ollama_setup::pull(
        &srv.url(),
        "m:1b",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let err = r.expect_err("应失败").to_string();
    assert!(
        err.contains("提前结束") || err.contains("未完整下载"),
        "错误信息应说明未完整下载: {err}"
    );
});

// pull：取消 → Err「拉取已取消」，不 panic
conn_retry_test!(pull_cancelled_fails_with_message, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| {
        HttpResponse::ok_json(
            "{\"status\":\"pulling manifest\"}\n{\"status\":\"downloading digest\"}\n",
        )
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_ref = Arc::clone(&cancel);
    let progress = move |_p: PullProgress| {
        cancel_ref.store(true, Ordering::Relaxed);
    };
    let r = ollama_setup::pull(&srv.url(), "m:1b", &cancel, progress);
    let err = r.expect_err("应失败").to_string();
    assert!(err.contains("已取消"), "错误应含取消提示: {err}");
});

// pull：非 200 → Err「拉取请求失败（状态码）」
conn_retry_test!(pull_non_200_status_fails, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| HttpResponse::status_only(404));
    let r = ollama_setup::pull(
        &srv.url(),
        "m:1b",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let err = r.expect_err("应失败").to_string();
    assert!(err.contains("404"), "错误应含状态码: {err}");
});

// list_models：/api/tags 返回 name+size → 结构化列表；base_url 带 /v1 剥离到根路径
conn_retry_test!(list_models_returns_name_and_size, {
    let run = || -> AppResult<()> {
        let _g = common::net_lock_guard();
        let srv = MockServer::start(|req| {
            assert_eq!(req.path, "/api/tags");
            HttpResponse::ok_json(
                r#"{"models":[{"name":"llava:latest","size":1234567890},{"name":"qwen2.5vl:7b","size":4096000000}]}"#,
            )
        });
        let list = ollama_setup::list_models(&format!("{}/v1", srv.url()))?;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "llava:latest");
        assert_eq!(list[0].size, 1234567890);
        assert_eq!(list[1].name, "qwen2.5vl:7b");
        assert_eq!(list[1].size, 4096000000);
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].path, "/api/tags",
            "应剥离开放兼容层 /v1 直连原生 API"
        );
        Ok(())
    };
    run().unwrap();
});

// list_models：非 200 → Err 透传状态码
conn_retry_test!(list_models_non_200_fails, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| HttpResponse::status_only(500));
    let err = ollama_setup::list_models(&srv.url())
        .expect_err("应失败")
        .to_string();
    assert!(err.contains("500"), "错误应含状态码: {err}");
});

// delete_model：DELETE /api/delete 带模型名 → 成功；base_url 带 /v1 剥离到根路径
// handler 内不 assert（panic 会重置连接表现为 send error），统一在测试体查请求日志
conn_retry_test!(delete_model_sends_delete_with_name, {
    let run = || -> AppResult<()> {
        let _g = common::net_lock_guard();
        let srv = MockServer::start(|req| {
            assert!(req.path.starts_with("/api/delete"));
            HttpResponse::ok_json("{}")
        });
        ollama_setup::delete_model(&format!("{}/v1", srv.url()), "qwen2.5vl:3b")?;
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].method, "DELETE",
            "应使用 DELETE 方法，实际: {:?}",
            reqs[0].method
        );
        assert_eq!(
            reqs[0].path, "/api/delete",
            "应剥离开放兼容层 /v1 直连原生 API"
        );
        let body: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(body["name"], "qwen2.5vl:3b");
        Ok(())
    };
    run().unwrap();
});

// delete_model：非 200 → Err 透传状态码与服务端文案（如模型不存在）
conn_retry_test!(delete_model_non_200_fails_with_server_message, {
    let _g = common::net_lock_guard();
    let srv = MockServer::start(|_| HttpResponse {
        status: 404,
        content_type: "application/json",
        body: r#"{"error":"model 'nope:latest' not found"}"#.to_string(),
    });
    let err = ollama_setup::delete_model(&srv.url(), "nope:latest")
        .expect_err("应失败")
        .to_string();
    assert!(err.contains("404"), "错误应含状态码: {err}");
    assert!(err.contains("not found"), "错误应透传服务端文案: {err}");
});
