//! Ollama 一键配置（方案 A2）：存活检测 / 硬件探针 / 模型推荐 / 一键拉取
//! 纯逻辑层：不碰 AppHandle/DB，可单测；进度走回调（与 ai_cloud/export_local 同模式）。
//! 关键事实：档案 base_url 是 OpenAI 兼容层（…/v1），Ollama 原生 API（/api/tags、/api/pull）
//! 挂根路径，调用前一律经 api_root() 剥离 /v1。

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{AppError, AppResult};

/// 存活检测结果（running=false 不代表报错，只是没探测到服务）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    pub running: bool,
    /// 已安装模型名列表（/api/tags）
    pub models: Vec<String>,
    /// 判定为 Ollama 本体（false = 活着但不是 Ollama，如 LM Studio，隐藏拉取区）
    pub is_ollama: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub name: Option<String>,
    pub vram_gb: Option<f32>,
    /// nvidia-smi | unknown（探不到不猜测，不用 AdapterRAM——32 位上限 4GB）
    pub source: String,
}

impl GpuInfo {
    fn unknown() -> Self {
        Self {
            name: None,
            vram_gb: None,
            source: "unknown".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRec {
    pub name: String,
    pub recommended: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    pub model: String,
    /// Ollama 阶段文案（pulling manifest / downloading … / verifying sha256 / success）
    pub status: String,
    pub total: u64,
    pub completed: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// 从档案 base_url 推导 Ollama 原生 API 根地址：剥离尾部 /v1 与斜杠
pub fn api_root(base_url: &str) -> String {
    let t = base_url.trim().trim_end_matches('/');
    t.strip_suffix("/v1")
        .or_else(|| t.strip_suffix("/V1"))
        .unwrap_or(t)
        .to_string()
}

/// 存活检测：GET {root}/api/tags（2s 超时）；连不上 → running=false 不报错。
/// tags 失败但 /api/version 活着 → 非 Ollama 的本地服务（is_ollama=false）
pub fn ping(base_url: &str) -> OllamaStatus {
    let fail = || OllamaStatus {
        running: false,
        models: vec![],
        is_ollama: false,
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return fail(),
    };
    let root = api_root(base_url);
    if let Ok(resp) = client.get(format!("{root}/api/tags")).send() {
        if resp.status().is_success() {
            if let Ok(v) = resp.json::<serde_json::Value>() {
                let models = v
                    .get("models")
                    .and_then(|m| m.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                m.get("name").and_then(|n| n.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                return OllamaStatus {
                    running: true,
                    models,
                    is_ollama: true,
                };
            }
        }
    }
    // tags 不通：version 启发式（LM Studio 等兼容服务兜底识别）
    if let Ok(resp) = client.get(format!("{root}/api/version")).send() {
        if resp.status().is_success() {
            return OllamaStatus {
                running: true,
                models: vec![],
                is_ollama: false,
            };
        }
    }
    fail()
}

/// 显存探针：nvidia-smi 优先（可靠），失败 → unknown 不猜测
pub fn probe_gpu() -> GpuInfo {
    let out = crate::utils::process::output_with_timeout(
        crate::utils::process::background_command("nvidia-smi").args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ]),
        std::time::Duration::from_secs(5),
    );
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = s.lines().next() {
                let mut parts = line.split(',');
                let name = parts
                    .next()
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty());
                let vram_gb = parts
                    .next()
                    .and_then(|x| x.trim().parse::<f32>().ok())
                    .map(|mb| mb / 1024.0);
                if name.is_some() || vram_gb.is_some() {
                    return GpuInfo {
                        name,
                        vram_gb,
                        source: "nvidia-smi".into(),
                    };
                }
            }
            GpuInfo::unknown()
        }
        _ => GpuInfo::unknown(),
    }
}

/// 推荐档位（纯函数）——按显存给出「推荐 + 数个备选」的多模态模型候选，
/// 打标场景需要视觉模型；手动输入框可拉官方库任意模型
pub fn recommend(vram_gb: Option<f32>) -> Vec<ModelRec> {
    // Qwen3.5 is the current small multimodal family in the Ollama registry.
    // Keep the default below the detected VRAM ceiling because Ollama also
    // needs room for the runtime and the model's KV cache.
    const Q08: &str = "qwen3.5:0.8b";
    const Q2: &str = "qwen3.5:2b";
    const Q4: &str = "qwen3.5:4b";
    const Q9: &str = "qwen3.5:9b";
    const MOON: &str = "moondream:2b";
    const MINI: &str = "minicpm-v:8b";
    const GEMMA3_12: &str = "gemma3:12b";
    let rec = |name: &str, note: &str| ModelRec {
        name: name.into(),
        recommended: true,
        note: note.into(),
    };
    let alt = |name: &str, note: &str| ModelRec {
        name: name.into(),
        recommended: false,
        note: note.into(),
    };
    match vram_gb {
        // ≥12GB：9b 主推；更大的 Qwen3.5 变体不再作为本地小模型推荐，
        // 避免把 20GB+ 的模型误导给单卡用户。
        Some(v) if v >= 12.0 => vec![
            rec(Q9, "显存充足，中文与视觉效果更稳"),
            alt(Q4, "更轻量，适合批量打标"),
            alt(GEMMA3_12, "多模态新锐"),
        ],
        // 6–12GB：4b 主推；9b 需要给运行时和 KV cache 留足空间，放在备选。
        Some(v) if v >= 6.0 => vec![
            rec(Q4, "显存适中，最新小型多模态模型"),
            alt(Q2, "更轻，速度更快"),
            alt(Q9, "显存允许时可试，较慢"),
            alt(MINI, "多模态小模型"),
        ],
        // <6GB：优先 2b，避免下载后因显存不足频繁回退到 CPU。
        Some(_) => vec![
            rec(Q2, "显存偏紧，轻量且可运行"),
            alt(Q08, "更轻，速度更快"),
            alt(MOON, "纯 CPU 更友好"),
        ],
        // 未知显存：默认轻量档 + 诚实预期
        None => vec![
            rec(
                Q2,
                "未探测到显存，默认轻量档；纯 CPU 可跑但慢，批量建议插电",
            ),
            alt(Q08, "更轻，速度更快"),
            alt(MOON, "更轻，纯 CPU 更友好"),
        ],
    }
}

/// 解析 /api/pull 流式响应的一行 JSON（错误行含 error 字段；success 行为终态）
pub fn parse_pull_line(line: &str, model: &str) -> PullProgress {
    let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
    let base = |status: &str, total: u64, completed: u64, done: bool, error: Option<String>| {
        PullProgress {
            model: model.to_string(),
            status: status.to_string(),
            total,
            completed,
            done,
            error,
        }
    };
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return base("error", 0, 0, true, Some(err.to_string()));
    }
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let total = v.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
    let completed = v.get("completed").and_then(|t| t.as_u64()).unwrap_or(0);
    let done = status == "success";
    PullProgress {
        model: model.to_string(),
        status,
        total,
        completed,
        done,
        error: None,
    }
}

/// 一键拉取：POST {root}/api/pull（stream:true），逐行解析进度回调。
/// 长下载不设整体超时，只限连接超时；网络错误/错误行 → Err，不 panic
pub fn pull<F: Fn(PullProgress)>(
    base_url: &str,
    model: &str,
    cancel: &Arc<AtomicBool>,
    progress: F,
) -> AppResult<()> {
    let root = api_root(base_url);
    let started = Instant::now();
    tracing::info!(
        operation = "ollama_pull",
        stage = "start",
        model = %model,
        "开始拉取 Ollama 模型"
    );
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // 连接后总超时兜底：网络挂起（连上但一直不发数据）时 lines() 会无限阻塞，
        // 设 120 分钟墙上限防永久卡死；正常情况下拉完即返回，不会触发
        .timeout(Duration::from_secs(120 * 60))
        .build()
        .map_err(|e| {
            tracing::error!(
                operation = "ollama_pull",
                stage = "client_init_failed",
                error_code = "HTTP",
                duration_ms = started.elapsed().as_millis() as u64,
                "Ollama 拉取客户端初始化失败"
            );
            AppError::msg(format!("HTTP 客户端初始化失败: {e}"))
        })?;
    let resp = client
        .post(format!("{root}/api/pull"))
        .json(&serde_json::json!({ "name": model, "stream": true }))
        .send()
        .map_err(|e| {
            tracing::error!(
                operation = "ollama_pull",
                stage = "connect_failed",
                model = %model,
                error_code = "HTTP",
                duration_ms = started.elapsed().as_millis() as u64,
                "连接 Ollama 拉取接口失败"
            );
            AppError::msg(format!("连接本地服务失败（请确认 Ollama 已启动）: {e}"))
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        tracing::error!(
            operation = "ollama_pull",
            stage = "http_error",
            model = %model,
            status = %status,
            error_code = "HTTP",
            duration_ms = started.elapsed().as_millis() as u64,
            "Ollama 拉取接口返回错误状态"
        );
        return Err(AppError::msg(format!("拉取请求失败（{status}）: {body}")));
    }

    let reader = std::io::BufReader::new(resp);
    let mut succeeded = false;
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            tracing::warn!(
                operation = "ollama_pull",
                stage = "cancelled",
                model = %model,
                error_code = "CANCELLED",
                duration_ms = started.elapsed().as_millis() as u64,
                "Ollama 模型拉取已取消"
            );
            return Err(AppError::cancelled("拉取已取消"));
        }
        let line = line.map_err(|e| {
            tracing::error!(
                operation = "ollama_pull",
                stage = "read_failed",
                model = %model,
                error_code = "IO",
                duration_ms = started.elapsed().as_millis() as u64,
                "读取 Ollama 拉取进度失败"
            );
            AppError::msg(format!("读取拉取进度失败: {e}"))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let p = parse_pull_line(&line, model);
        if p.error.is_some() {
            let err = p.error.clone().unwrap_or_default();
            tracing::warn!(
                operation = "ollama_pull",
                stage = "ollama_error",
                model = %model,
                error_code = "OLLAMA_PULL_ERROR",
                error_chars = err.chars().count(),
                duration_ms = started.elapsed().as_millis() as u64,
                "Ollama 返回拉取错误"
            );
            progress(p);
            return Err(AppError::msg(format!("Ollama 拉取失败: {err}")));
        }
        succeeded = p.done;
        progress(p);
        if succeeded {
            break;
        }
    }
    if !succeeded {
        tracing::warn!(
            operation = "ollama_pull",
            stage = "incomplete_stream",
            model = %model,
            error_code = "OLLAMA_PULL_INCOMPLETE",
            duration_ms = started.elapsed().as_millis() as u64,
            "Ollama 拉取流提前结束"
        );
        return Err(AppError::msg("拉取流提前结束，模型可能未完整下载，请重试"));
    }
    tracing::info!(
        operation = "ollama_pull",
        stage = "done",
        model = %model,
        duration_ms = started.elapsed().as_millis() as u64,
        "Ollama 模型拉取完成"
    );
    Ok(())
}

/// 单个已装模型的元信息（/api/tags 条目；size = 模型占用磁盘字节数）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelInfo {
    pub name: String,
    pub size: u64,
}

/// 解析 /api/tags 响应体 → 模型元信息列表（纯函数，可单测；缺 size 按 0）
pub fn parse_tags_body(body: &str) -> Vec<LocalModelInfo> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let name = m.get("name").and_then(|n| n.as_str())?.to_string();
                    let size = m.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                    Some(LocalModelInfo { name, size })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 列出已安装模型（含占用大小）：GET {root}/api/tags（5s 超时）
pub fn list_models(base_url: &str) -> AppResult<Vec<LocalModelInfo>> {
    let root = api_root(base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| AppError::msg(format!("HTTP 客户端初始化失败: {e}")))?;
    let resp = client
        .get(format!("{root}/api/tags"))
        .send()
        .map_err(|e| AppError::msg(format!("连接本地服务失败（请确认 Ollama 已启动）: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(AppError::msg(format!(
            "读取模型列表失败（{status}）: {body}"
        )));
    }
    let body = resp
        .text()
        .map_err(|e| AppError::msg(format!("读取模型列表失败: {e}")))?;
    Ok(parse_tags_body(&body))
}

/// 删除已下载模型：DELETE {root}/api/delete（Ollama 原生 API，释放磁盘空间）。
/// 模型名不合法（空）直接拒绝；服务端错误原样透传（含模型不存在等）
pub fn delete_model(base_url: &str, model: &str) -> AppResult<()> {
    if model.trim().is_empty() {
        return Err(AppError::msg("未指定模型名"));
    }
    let root = api_root(base_url);
    let client = reqwest::blocking::Client::builder()
        // 大模型删除（含 blob 清理）可能稍久，给足上限
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::msg(format!("HTTP 客户端初始化失败: {e}")))?;
    let resp = client
        .delete(format!("{root}/api/delete"))
        .json(&serde_json::json!({ "name": model }))
        .send()
        .map_err(|e| AppError::msg(format!("连接本地服务失败（请确认 Ollama 已启动）: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(AppError::msg(format!("删除模型失败（{status}）: {body}")));
    }
    Ok(())
}

/// 探测本地模型存储目录：OLLAMA_MODELS 环境变量优先，其次默认 ~/.ollama/models；
/// 目录不存在（或用户主目录未知）时返回 Err，前端据此隐藏「打开文件夹」入口
pub fn model_dir() -> AppResult<String> {
    if let Some(dir) = std::env::var_os("OLLAMA_MODELS") {
        let dir = std::path::PathBuf::from(dir);
        if !dir.as_os_str().is_empty() && dir.is_dir() {
            return encode_existing_model_dir(&dir);
        }
    }
    let home = dirs::home_dir().ok_or_else(|| AppError::msg("未找到用户主目录"))?;
    let p = home.join(".ollama").join("models");
    encode_existing_model_dir(&p)
}

fn encode_existing_model_dir(path: &std::path::Path) -> AppResult<String> {
    if !path.is_dir() {
        return Err(AppError::msg(
            "未找到模型存储目录（OLLAMA_MODELS 或默认 ~/.ollama/models）",
        ));
    }
    crate::utils::path::encode_native_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_conversion_preserves_existing_path_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("model store");
        std::fs::create_dir(&models).unwrap();
        assert_eq!(
            encode_existing_model_dir(&models).unwrap(),
            models.to_str().unwrap()
        );
    }

    // This test creates an invalid-UTF-8 filesystem entry; macOS rejects that path with EILSEQ.
    #[cfg(target_os = "linux")]
    #[test]
    fn model_dir_conversion_rejects_non_utf8_path_instead_of_rewriting_it() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join(std::ffi::OsStr::from_bytes(b"model-\xff"));
        std::fs::create_dir(&models).unwrap();
        let error = encode_existing_model_dir(&models).unwrap_err();
        assert!(error.to_string().contains("不是有效 UTF-8"));
    }

    #[test]
    fn api_root_strips_v1_and_slashes() {
        assert_eq!(
            api_root("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        assert_eq!(
            api_root("http://localhost:11434/v1/"),
            "http://localhost:11434"
        );
        assert_eq!(api_root("http://localhost:11434"), "http://localhost:11434");
        assert_eq!(
            api_root("  http://127.0.0.1:11434/V1 "),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn recommend_tiers() {
        // ≥12GB：9b 推荐 + 备选
        let r = recommend(Some(16.0));
        assert!(r[0].recommended && r[0].name == "qwen3.5:9b");
        assert!(r.len() >= 3);
        // 6–12GB：4b 推荐 + 数个备选
        let r = recommend(Some(8.0));
        assert!(r[0].recommended && r[0].name == "qwen3.5:4b");
        assert_eq!(r.len(), 4);
        // <6GB：2b 推荐 + 2 个轻量备选
        let r = recommend(Some(2.0));
        assert_eq!(r.len(), 3);
        assert!(r[0].name == "qwen3.5:2b" && r[0].recommended);
        // 未知：默认 2b 诚实预期 + 轻量备选
        let r = recommend(None);
        assert_eq!(r.len(), 3);
        assert!(r[0].name == "qwen3.5:2b" && r[0].note.contains("CPU"));
        assert!(r.iter().any(|m| m.name.starts_with("moondream")));
    }

    #[test]
    fn parse_pull_line_variants() {
        let p = parse_pull_line(
            r#"{"status":"downloading digest","total":1000,"completed":250}"#,
            "qwen3.5:4b",
        );
        assert_eq!(p.completed, 250);
        assert_eq!(p.total, 1000);
        assert!(!p.done && p.error.is_none());

        let p = parse_pull_line(r#"{"status":"success"}"#, "m");
        assert!(p.done && p.error.is_none());

        let p = parse_pull_line(r#"{"error":"file does not exist"}"#, "m");
        assert!(p.done && p.error.as_deref() == Some("file does not exist"));

        // 非法行不 panic
        let p = parse_pull_line("not json", "m");
        assert!(!p.done && p.error.is_none());
    }

    #[test]
    fn parse_tags_body_parses_name_and_size() {
        let body =
            r#"{"models":[{"name":"llava:latest","size":1234567890},{"name":"qwen3.5:9b"}]}"#;
        let list = parse_tags_body(body);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "llava:latest");
        assert_eq!(list[0].size, 1234567890);
        assert_eq!(list[1].name, "qwen3.5:9b");
        assert_eq!(list[1].size, 0, "缺 size 字段按 0 处理");
    }

    #[test]
    fn parse_tags_body_invalid_or_foreign_returns_empty() {
        // 非法 JSON 不 panic → 空
        assert!(parse_tags_body("not json").is_empty());
        // 空模型数组 → 空
        assert!(parse_tags_body(r#"{"models":[]}"#).is_empty());
        // 非 Ollama 响应（无 models 字段）→ 空
        assert!(parse_tags_body("{}").is_empty());
        // 条目缺 name → 跳过
        let list = parse_tags_body(r#"{"models":[{"size":1},{"name":"a","size":2}]}"#);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "a");
    }

    #[test]
    fn delete_model_rejects_empty_name() {
        let err = delete_model("http://localhost:11434", "  ").expect_err("空模型名应拒绝");
        assert!(err.to_string().contains("未指定模型名"));
    }
}
