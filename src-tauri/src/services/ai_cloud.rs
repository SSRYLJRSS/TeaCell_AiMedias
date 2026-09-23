//! 云端 AI 打标服务（T05a，R-06）：OpenAI 兼容视觉 API（通义千问 VL / 智谱等）
//! - 同步 blocking 风格（与 importer/export_local 一致，command 层包事件）
//! - 图片优先用高清缩略图（省流量），失败回退占位图/原图
//! - 逐条建议写回 + 批次计数 + 进度回调 + 取消

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::ai::{self, CategorizedTags};
use crate::db::tag_facets::FacetPromptContext;
use crate::db::{
    assets,
    settings::{AiSettings, ApiProfile},
};
use crate::services::ai_rate_limit::{self, AiRateLimiter};
use crate::services::imaging::{self, AiImagePayload};
use crate::services::ollama_runtime;

/// A2：提示词版本（手工维护常量）—— 改提示词时必须递增，随 request_config 一起落库溯源。
pub const PROMPT_VERSION: &str = "tagging-v2.1-2026-09";
pub const ANALYSIS_SCHEMA_VERSION: i64 = 2;
pub const MIN_DESCRIPTION_CHARS: usize = 12;
pub const MAX_DESCRIPTION_CHARS: usize = 30;
pub const DEFAULT_CONFIDENCE_MIN_SUGGEST: f64 = 0.30;

/// 产品层的打标传输边界：只有 Windows 上由应用管理的默认 Ollama 才属于“本地打标”。
/// 其他 localhost、局域网地址或自定义 Ollama 都按外部 API 处理，不触发 Ollama 生命周期
/// 和扩展字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisionTransport {
    ManagedWindowsOllama,
    ExternalOpenAi,
    ExternalAnthropic,
}

pub fn is_managed_ollama_profile(profile: &ApiProfile) -> bool {
    cfg!(target_os = "windows")
        && profile.api_mode == "openai"
        && profile.is_local()
        && ollama_runtime::is_managed_ollama_base(&profile.base_url)
}

fn vision_transport(profile: &ApiProfile) -> VisionTransport {
    if profile.api_mode == "anthropic" {
        VisionTransport::ExternalAnthropic
    } else if is_managed_ollama_profile(profile) {
        VisionTransport::ManagedWindowsOllama
    } else {
        VisionTransport::ExternalOpenAi
    }
}

fn vision_transport_name(transport: VisionTransport) -> &'static str {
    match transport {
        VisionTransport::ManagedWindowsOllama => "managed_windows_ollama",
        VisionTransport::ExternalOpenAi => "external_openai_compatible",
        VisionTransport::ExternalAnthropic => "external_anthropic",
    }
}

/// A2：请求配置的稳定序列化（递归按键排序）后 sha256 前 16 位 hex。
/// 同输入同 hash、改任一项则变（request_config_hash_is_stable 守护）。
pub fn stable_config_hash(v: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    // 稳定序列化：对象键排序（serde_json 在 preserve_order 特性下保持插入序，
    // 不做显式排序的话同内容不同构建序会得到不同 hash）
    fn canon(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                let mut sorted: Vec<(String, serde_json::Value)> =
                    m.iter().map(|(k, val)| (k.clone(), canon(val))).collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(canon).collect()),
            other => other.clone(),
        }
    }
    let canonical = serde_json::to_string(&canon(v)).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let out = hasher.finalize();
    out.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A2：批次请求配置（溯源用，六项）—— promptVersion / systemPrompt / facets /
/// topTagsSnapshot / modelParams / mediaKind / imagePreprocess。model = 激活档案模型。
#[allow(clippy::too_many_arguments)]
pub fn build_batch_request_config(
    system_prompt: &str,
    facets: &[FacetPromptContext],
    top_tags: &[(String, String)],
    model: &str,
    media_kind: &str,
    max_tokens: i64,
    is_local: bool,
    json_tier: TextJsonTier,
    min_confidence: f64,
) -> serde_json::Value {
    build_batch_request_config_with_image_max(
        system_prompt,
        facets,
        top_tags,
        model,
        media_kind,
        max_tokens,
        is_local,
        json_tier,
        min_confidence,
        imaging::AI_IMAGE_MAX_PX,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_batch_request_config_with_image_max(
    system_prompt: &str,
    facets: &[FacetPromptContext],
    top_tags: &[(String, String)],
    model: &str,
    media_kind: &str,
    max_tokens: i64,
    managed_ollama: bool,
    json_tier: TextJsonTier,
    min_confidence: f64,
    image_max_px: u32,
) -> serde_json::Value {
    let facet_items: Vec<serde_json::Value> = facets
        .iter()
        .map(|f| {
            serde_json::json!({
                "key": f.key,
                "displayName": f.display_name,
                "description": f.description,
                "selectionMode": f.selection_mode,
                "maxItems": f.max_items,
                "appliesTo": "all",
                // V24（§6.4）：数值分面配置随批次溯源
                "facetKind": f.facet_kind,
                "numMin": f.num_min,
                "numMax": f.num_max,
                "numUnit": f.num_unit,
            })
        })
        .collect();
    let tag_items: Vec<serde_json::Value> = top_tags
        .iter()
        .map(|(facet, words)| serde_json::json!({ "facet": facet, "words": words }))
        .collect();
    // modelParams 记录影响结果的请求参数；本地档案还会关闭思考模式，避免思考 token
    // 吃满输出预算后只返回 message.thinking、把最终 content 留空。
    let tier = match json_tier {
        TextJsonTier::Structured => "structured",
        TextJsonTier::JsonObject => "json_object",
        TextJsonTier::Plain => "plain",
    };
    let mut model_params = serde_json::json!({
        "model": model,
        "maxTokens": max_tokens,
        "jsonTier": tier
    });
    if managed_ollama {
        model_params["keepAlive"] = serde_json::json!(KEEP_ALIVE_IDLE);
        model_params["thinkingDisabled"] = serde_json::json!(true);
    }
    serde_json::json!({
        "promptVersion": PROMPT_VERSION,
        "systemPrompt": system_prompt,
        "facets": facet_items,
        "topTagsSnapshot": tag_items,
        "modelParams": model_params,
        "minConfidence": min_confidence,
        "mediaKind": media_kind,
        "imagePreprocess": {
            "maxPx": image_max_px,
            "fallbackMaxPx": imaging::AI_IMAGE_FALLBACK_MAX_PX,
            "format": "jpeg",
            "quality": imaging::AI_JPEG_QUALITY,
        },
    })
}
use crate::error::{AppError, AppResult};

/// 从已明确包含 `HTTP <三位状态码>` 的错误文本提取状态码。
/// 只在请求层真实拿到响应状态时使用，不从错误类型反推。
fn http_status_from_message(message: &str) -> Option<u16> {
    let lower = message.to_ascii_lowercase();
    let start = lower.find("http ")? + "http ".len();
    let digits: String = lower[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.len() != 3 {
        return None;
    }
    let status = digits.parse::<u16>().ok()?;
    (100..=599).contains(&status).then_some(status)
}

fn is_ai_rate_limited(error: &AppError) -> bool {
    error.code() == "AI_RATE_LIMITED"
}

/// HTTP 状态错误只保留服务名、状态码和稳定建议，不回显响应体，
/// 避免把模型原文或凭据带进结构化日志。
#[cfg(test)]
fn ai_http_status_error(service: &str, status: u16) -> AppError {
    ai_http_status_error_with_body(service, status, "")
}

/// 从服务商错误 JSON 中提取有限长度的公开错误说明。
/// 默认只保留上下文/格式类诊断；限流错误另外允许保留服务商的 RPM/配额提示，
/// 但仍会脱敏并截断，避免模型原文、凭据或代理诊断信息进入用户错误和日志。
fn safe_http_detail(body: &str) -> Option<String> {
    safe_http_detail_with_plain_text(body, false)
}

fn safe_http_rate_limit_detail(body: &str) -> Option<String> {
    safe_http_detail_with_plain_text(body, true)
}

fn safe_http_detail_with_plain_text(body: &str, allow_plain_text: bool) -> Option<String> {
    let detail = if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let candidate = value
            .pointer("/error/message")
            .or_else(|| value.pointer("/error/code"))
            .or_else(|| value.get("message"))
            .or_else(|| value.get("error"))?;
        candidate
            .as_str()
            .map(str::to_string)
            .or_else(|| candidate.as_object().map(|_| candidate.to_string()))?
    } else {
        let text = body.trim();
        let lower = text.to_ascii_lowercase();
        if text.is_empty()
            || text.len() > 240
            || (!allow_plain_text
                && !(lower.contains("context")
                    || lower.contains("token")
                    || lower.contains("prompt is too long")
                    || lower.contains("input too long")
                    || lower.contains("上下文")
                    || lower.contains("令牌")))
        {
            return None;
        }
        text.to_string()
    };
    let detail = crate::observability::redact_secrets(detail.trim());
    let detail = detail.trim();
    if detail.is_empty() || detail.len() > 240 {
        return None;
    }
    let lower = detail.to_ascii_lowercase();
    if lower.contains("api_key")
        || lower.contains("authorization")
        || lower.contains("bearer ")
        || lower.contains("sk-")
    {
        return None;
    }
    Some(detail.to_string())
}

fn ai_http_status_error_with_body(service: &str, status: u16, body: &str) -> AppError {
    let message = match status {
        401 | 403 => format!("{service}鉴权失败（HTTP {status}），请检查 API Key 和访问权限"),
        408 => format!("{service}请求超时（HTTP {status}）"),
        429 => format!("{service}请求过于频繁（HTTP {status}），请稍后重试"),
        500..=599 => format!("{service}服务异常（HTTP {status}），请稍后重试"),
        _ => format!("{service}返回 HTTP {status}"),
    };
    let detail = if status == 429 {
        safe_http_rate_limit_detail(body)
    } else {
        safe_http_detail(body)
    };
    let message = detail
        .map(|detail| format!("{message}：{detail}"))
        .unwrap_or(message);
    match status {
        401 | 403 => AppError::unauthorized(message),
        408 => AppError::timeout(message),
        429 => AppError::ai_rate_limited(message),
        _ => AppError::internal(message),
    }
}

fn vision_transport_error(service: &str, error: reqwest::Error, cfg: &ApiProfile) -> AppError {
    if error.is_timeout() {
        return AppError::timeout(format!("{service}请求超时，请检查网络或服务状态"));
    }
    if is_managed_ollama_profile(cfg) {
        AppError::internal(format!(
            "无法连接应用管理的 Ollama {}：请确认本地服务已启动: {error}",
            cfg.base_url
        ))
    } else {
        AppError::internal(format!("{service}请求失败: {error}"))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProgress {
    pub batch_id: i64,
    pub processed: i64,
    pub total: i64,
    pub current_asset_id: i64,
}

/// FB5-05（§7.4）+ A1：AI 分析结果 = 一句话描述 + 分类标签 + 强类型提议。
/// 描述为空但标签非空 / 标签为空但描述非空，都算一次有效分析。
#[derive(Debug, Clone, Default)]
pub struct MediaAnalysis {
    /// 画面内容的一句话中文描述（已规范化，最多 30 Unicode 字符；可为空）
    pub description: String,
    /// 人物存在状态与模型置信度。
    pub people_presence: ai::PeoplePresence,
    pub tags: CategorizedTags,
    /// A1：typed 提议（facet_key/raw_name/confidence）——与 tags 对齐（同序、同裁剪）。
    /// 纯字符串回退时 confidence=None。
    pub proposals: Vec<ai::TagProposal>,
    /// V24（§6.3④）：数值分面提议（平行字段；当前解析层暂不产出，链路预留）
    pub numbers: Vec<ai::NumberProposal>,
    /// A2：解析层告警（未知分面 key 等），随 AnalysisResult 的 analysis_json 一并落库溯源。
    pub warnings: Vec<String>,
    /// 模型在 tags 对象中显式返回过的稳定分面 key；空数组也计入。
    /// 用于区分“不适用所以返回 []”与“模型漏掉了整个分面”。
    pub responded_tag_facets: std::collections::BTreeSet<String>,
    /// A2：该次请求的模型原始返回（逐字存储，不做任何清洗/剥围栏）
    pub raw_response: String,
    /// A2：该次请求的配置 JSON（溯源；批次级另存 request_config_json）
    pub request_config_json: Option<String>,
    /// A2：AnalysisResult 的序列化（desc + proposals + warnings）
    pub analysis_json: Option<String>,
    /// 低于最低置信度而被拦截的标签数。
    pub blocked_low_confidence: usize,
}

/// 按分面组装提示词（P1B + C-3）：使用稳定英文 facetKey 作为 JSON 键，中文显示名仅作说明；
/// 避免模型返回中文分类名导致归类不稳定。
/// FB5-05（§7.4）：同时要求输出 description（一句话描述，目标 12–30 字，规则见下）。
fn build_system_prompt() -> String {
    let mut sys = String::from(
        "你是图片素材打标助手。仅依据画面中清晰可见的内容，生成画面摘要和分类标签。\n",
    );
    sys.push_str(
        "不要推测素材用途、授权情况、商业价值、可用性、质量评价，以及无法确认的人物身份或地点。\n",
    );
    sys.push_str("输出要求：\n");
    sys.push_str("- 只返回一个 JSON 对象，不要解释或代码围栏；\n");
    sys.push_str(
        "- 结构：{\"description\":\"画面摘要\",\"peoplePresence\":{\"status\":\"present|absent|unknown\",\"confidence\":0.9},\"tags\":{\"分面key\":[{\"name\":\"标签\",\"confidence\":0.9}]},\"numbers\":{}}。\n",
    );
    sys.push_str("画面摘要：\n");
    sys.push_str("- 用一句自然、具体的中文描述主要对象、正在进行的动作或状态、所处环境，严格写 12–30 个字符；\n");
    sys.push_str("- 不得只写「女子湖边」「城市建筑」这类关键词串；信息不足时也要把可确认的对象、状态和环境组成完整一句；\n");
    sys.push_str("- 不以「这是一张」「这张图片展示」开头，不写建议，不堆砌标签；\n");
    sys.push_str("- 摘要内容不得重复放入 tags 数组。\n");
    sys.push_str("分类标签：\n");
    sys.push_str("- tags 对象必须逐项包含用户列出的全部非数值分面 key；不适用或无法判断也必须显式写空数组 []，禁止省略 key；\n");
    sys.push_str(
        "- 每个标签为中文 2–6 字（如「人」「海边」「逆光」），并给出 0–1 的 confidence；\n",
    );
    sys.push_str("- 只标注能够从画面确认的内容；其他分面无法确认时返回空数组，不要猜测；\n");
    sys.push_str("- peoplePresence 必须先判断画面是否有人：present=有人，absent=确认无人，unknown=无法确认；\n");
    sys.push_str("- peoplePresence=absent 时 people 标签只写「无人」；present 时禁止写「无人、未知、人物」，只写可观察属性；unknown 时 people 只写「未知」；\n");
    sys.push_str("- 若包含 subject 分面，填写最具代表性的可见对象；人物统一写「人」，禁止在 subject 写男子、女子、行人、男孩、女孩、老人、人数或穿着；\n");
    sys.push_str(
        "- subject 通常输出 2–3 个清晰主体，只有一个明确主体时只写一个，不得为了凑数编造；\n",
    );
    sys.push_str("- 若包含 scene 分面，只写空间、环境和地点；通常输出 2–3 个清晰维度，不得把树木、水面、楼梯等主体物当作场景；\n");
    sys.push_str("- 若包含 people 分面，按人数档位、性别、年龄段、穿着和动作分别输出原子标签；男女同框分别写，老少同框分别写，禁止输出「年轻女子」这类复合词；\n");
    sys.push_str("- 人数档位固定：0 无人、1 单人、2 双人、3–10 多人、超过 10 或无法准确计数为人群；人数不确定时不猜档位；\n");
    sys.push_str("- 一个标签只归入一个分面；\n");
    sys.push_str("- 优先使用已有候选词；候选不足时可新增简短原子标签，新词将进入「其他」分组等待人工确认；\n");
    sys.push_str(
        "- 多值如实输出：一张图既是「海边」又是「日落」时，scene 里两个都写，不要只挑一个；\n",
    );
    sys.push_str("- 用户给出候选词时，含义相同必须用已有词，不要造近义词（已有「海边」就不要写「海滨」）。\n");
    sys.push_str("- 标签必须使用 {\"name\":\"标签\",\"confidence\":0.9}，不得输出裸字符串。\n");
    // V24（§6.4）：数值分面输出协议 —— numbers 对象，值为原文（字符串或数字都接受）。
    // 歧义表达（范围/约数）如实输出，由解析层判定（绝不取首个数字）。
    sys.push_str("- 数值分类输出到 \"numbers\" 对象：{\"numbers\": {\"分面key\": \"原文\"}}；");
    sys.push_str("原文如实写（如 \"5\" 或 \"5人\"），画不出数字的分面不要出现在 numbers 里。\n");
    sys
}

/// W5a（a2/a3/a5）：user 段 —— 每分面拼 description + 规则 + Top-N 候选词 + 真实 few-shot。
/// top_tags 来自 W2-9 top_tags_per_facet（按使用次数降序，高频词优先 → 标签收敛）。
pub fn build_user_prompt(facets: &[FacetPromptContext], top_tags: &[(String, String)]) -> String {
    // V24（§6.4）：数值分面独立成段 —— 不发词表（无候选词可给），发「输出一个数字 + 值域 + 单位」
    let number_facets: Vec<&FacetPromptContext> =
        facets.iter().filter(|f| f.facet_kind == "number").collect();
    let tag_facets: Vec<&FacetPromptContext> = facets
        .iter()
        .filter(|f| f.facet_kind != "number" && f.key != "custom")
        .collect();
    let mut user = String::from("请为这张图片打标。可用的分类（key 为英文标识）：\n");
    for c in &tag_facets {
        let rule = if c.selection_mode == "single" {
            "单选，最多 1 个".to_string()
        } else {
            match c.max_items {
                Some(n) => format!("可多选，最多 {n} 个"),
                None => "可多选，数量不限".to_string(),
            }
        };
        let facet_rule = match c.key.as_str() {
            "people" => {
                "；按原子属性输出：人数档位、性别、年龄段、穿着、动作；男女同框分别写，老少同框分别写；禁止复合词；只有确认完全无人物时才输出 [\"无人\"]，无法判断时输出 [\"未知\"]；present 时禁止输出无人、未知、人物"
            }
            "subject" => {
                "；人物统一写「人」，禁止写男子、女子、行人、男孩、女孩、老人、人数或穿着；优先选择 2–3 个最具代表性的可见对象，确实只有一个时才写一个"
            }
            "scene" => {
                "；只写空间、环境和地点，多为 2–3 个清晰维度；树木、水面、楼梯等主体物不得当场景"
            }
            _ => "",
        };
        user.push_str(&format!(
            "- {}（key: {}，{rule}）{}{}\n",
            c.display_name,
            c.key,
            if c.description.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", c.description)
            },
            facet_rule,
        ));
    }
    let required_keys: Vec<&str> = facets
        .iter()
        .filter(|f| f.facet_kind != "number" && f.key != "custom")
        .map(|f| f.key.as_str())
        .collect();
    if !required_keys.is_empty() {
        user.push_str(&format!(
            "\ntags 必须完整包含这些 key（允许值为 []，但不得漏 key）：{}\n",
            required_keys.join(", ")
        ));
    }
    if !number_facets.is_empty() {
        user.push_str("\n数值分类（输出到 numbers 对象，不进 tags）：\n");
        for c in &number_facets {
            let range = match (c.num_min, c.num_max) {
                (Some(lo), Some(hi)) => format!("{lo}–{hi} 的"),
                (Some(lo), None) => format!("不小于 {lo} 的"),
                (None, Some(hi)) => format!("不大于 {hi} 的"),
                (None, None) => String::new(),
            };
            let unit = if c.num_unit.trim().is_empty() {
                String::new()
            } else {
                format!("，单位：{}", c.num_unit)
            };
            user.push_str(&format!(
                "- {}（key: {}）：输出一个{}数字{unit}；画面中数不出来就不输出该 key，不要猜{}\n",
                c.display_name,
                c.key,
                range,
                if c.description.trim().is_empty() {
                    String::new()
                } else {
                    format!("（{}）", c.description)
                },
            ));
        }
    }
    if !top_tags.is_empty() {
        user.push_str("\n已有标签候选词（含义相同就用已有的词，不要造近义词）：\n");
        for (facet, words) in top_tags {
            user.push_str(&format!("- {facet}: {words}\n"));
        }
    }
    // R3-2：a3 真实 few-shot —— 不输出「示例词」占位符，而是完整「画面 → JSON」对。
    // 优先覆盖 subject/scene/people，重点示范人物识别与属性必须自洽。
    let mut ex_keys: Vec<&str> = Vec::new();
    for preferred in ["subject", "scene", "people"] {
        if ex_keys.len() < 3 && tag_facets.iter().any(|f| f.key == preferred) {
            ex_keys.push(preferred);
        }
    }
    for facet in &tag_facets {
        if ex_keys.len() >= 3 {
            break;
        }
        if !ex_keys.contains(&facet.key.as_str()) {
            ex_keys.push(facet.key.as_str());
        }
    }
    if !ex_keys.is_empty() {
        let real_word = |key: &str, alt: bool| -> Option<String> {
            let table: &[(&str, &str, &str)] = &[
                ("scene", "海边", "城市"),
                ("subject", "人", "人"),
                ("people", "多人", "单人"),
                ("lighting", "夜景", "白天"),
            ];
            table
                .iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, a, b)| (if alt { *b } else { *a }).to_string())
        };
        let tags_json = |alt: bool| -> String {
            // 示例 A：黄昏海边，树下多人 → scene=海边、subject=树、people=多人…
            // 示例 B：清晨城市建筑，单人 → 对应 alt 词。画面没有的分面输出空数组。
            let pairs: Vec<String> = ex_keys
                .iter()
                .map(|k| {
                    let w = real_word(k, alt).unwrap_or_default();
                    format!(
                        "\"{k}\": {}",
                        if w.is_empty() {
                            "[]".to_string()
                        } else {
                            format!("[{{\"name\":\"{w}\",\"confidence\":0.9}}]")
                        }
                    )
                })
                .collect();
            format!("{{{}}}", pairs.join(", "))
        };
        user.push_str(
            "\n输出示例（真实输入→输出对，结构参考；tags 只含该图真实可观察到的分类）：\n",
        );
        user.push_str(&format!(
            "示例 A：输入「黄昏的海边，树下有一群人散步」→ 输出 {{\"description\":\"一群人在黄昏海边散步交谈\",\"peoplePresence\":{{\"status\":\"present\",\"confidence\":0.96}},\"tags\":{}}}\n",
            tags_json(false)
        ));
        user.push_str(&format!(
            "示例 B：输入「清晨的城市建筑，天空晴朗，只有一个行人」→ 输出 {{\"description\":\"行人独自走过清晨的城市街道\",\"peoplePresence\":{{\"status\":\"present\",\"confidence\":0.94}},\"tags\":{}}}\n",
            tags_json(true)
        ));
        if tag_facets.iter().any(|f| f.key == "people") {
            user.push_str(
                "人物一致性示例：description「女子站在湖边树下抬头张望」时，peoplePresence 必须是 present，subject 必须写「人」，people 输出「女性」等原子属性（人数已知时再写单人/双人/多人/人群），绝不能写无人。\n",
            );
        }
    } else {
        user.push_str(
            "\n输出示例：{\"description\":\"黄昏海边有人散步\",\"peoplePresence\":{\"status\":\"present\",\"confidence\":0.9},\"tags\":{}}\n",
        );
    }
    user
}

/// W5a（a7）：动态 max_tokens —— 固定 500 在分面多时会把 JSON 截断 → 解析失败 → 整条 rejected。
fn dynamic_max_tokens(facet_count: usize) -> i64 {
    (500 + 180 * facet_count as i64).clamp(800, 3200)
}

/// 打标视觉请求的强类型协议。模型必须输出对象标签、人物状态和分面 key。
fn tagging_schema(facets: &[FacetPromptContext]) -> serde_json::Value {
    let mut tag_properties = serde_json::Map::new();
    let mut required_tags = Vec::new();
    for facet in facets
        .iter()
        .filter(|f| f.facet_kind != "number" && f.key != "custom")
    {
        let mut item = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "minLength": 1, "maxLength": 12 },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
                },
                "required": ["name", "confidence"],
                "additionalProperties": false
            }
        });
        let cap = if facet.selection_mode == "single" {
            Some(1)
        } else {
            facet.max_items
        };
        if let Some(cap) = cap {
            item["maxItems"] = serde_json::json!(cap.max(0));
        }
        tag_properties.insert(facet.key.clone(), item);
        required_tags.push(serde_json::Value::String(facet.key.clone()));
    }

    let mut number_properties = serde_json::Map::new();
    for facet in facets.iter().filter(|f| f.facet_kind == "number") {
        number_properties.insert(
            facet.key.clone(),
            serde_json::json!({
                "description": facet.description,
                "anyOf": [
                    { "type": "string" },
                    { "type": "number" }
                ]
            }),
        );
    }

    serde_json::json!({
        "type": "object",
        "properties": {
            "description": {
                "type": "string",
                "minLength": MIN_DESCRIPTION_CHARS,
                "maxLength": MAX_DESCRIPTION_CHARS
            },
            "peoplePresence": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["present", "absent", "unknown"] },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
                },
                "required": ["status", "confidence"],
                "additionalProperties": false
            },
            "tags": {
                "type": "object",
                "properties": tag_properties,
                "required": required_tags,
                "additionalProperties": false
            },
            "numbers": {
                "type": "object",
                "properties": number_properties,
                "additionalProperties": false
            }
        },
        "required": ["description", "peoplePresence", "tags"],
        "additionalProperties": false
    })
}

fn required_tag_facet_keys(facets: &[FacetPromptContext]) -> Vec<String> {
    facets
        .iter()
        .filter(|f| f.facet_kind != "number" && f.key != "custom")
        .map(|f| f.key.clone())
        .collect()
}

fn missing_tag_facet_keys(analysis: &MediaAnalysis, facets: &[FacetPromptContext]) -> Vec<String> {
    required_tag_facet_keys(facets)
        .into_iter()
        .filter(|key| !analysis.responded_tag_facets.contains(key))
        .collect()
}

/// 退化输出检测（Ollama 长驻状态损坏的已知症状，见 ollama/ollama#8235/#17587）：
/// - 重复单一字符串（@@@@@… / !!!!!…）：采样/分词状态损坏后贪心退化的典型输出；
/// - UTF-8 字节被按 Latin-1 误读的乱码（ä¸æ¯…）：分词器字节错切的典型输出；
///
/// 此类输出重试无意义，应卸载模型重载后重试（社区已验证的恢复手段，#8235 评论）。
fn is_degenerate(content: &str) -> bool {
    let t = content.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() < 6 {
        return false;
    }
    // 全部同字符（@@@@@ / !!!!! …）
    if chars.iter().all(|c| *c == chars[0]) {
        return true;
    }
    // Latin-1 补充区字符占比 >1/3（UTF-8 字节误读为 Latin-1 的乱码特征）
    let latin = chars
        .iter()
        .copied()
        .filter(|c| ('\u{0080}'..='\u{00FF}').contains(c))
        .count();
    latin * 3 > chars.len()
}

/// 本地（Ollama）档案：触发一次模型卸载（keep_alive=0）。
/// 长驻 runner 状态损坏后"卸载重载"即恢复（#8235 作者验证）；失败静默，由上层重试兜底。
/// §8.4：读取响应并记录 done_reason（期望 unload），不再纯 fire-and-forget。
fn unload_ollama_model(cfg: &ApiProfile) {
    // 本地档案 base_url 形如 http://localhost:11434/v1 → 原生端点剥掉 /v1
    let base = cfg.base_url.trim_end_matches('/');
    let native = base.strip_suffix("/v1").unwrap_or(base).to_string();
    let body = serde_json::json!({
        "model": cfg.model,
        "prompt": "",
        "keep_alive": "0",
    });
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client
        .post(format!("{native}/api/generate"))
        .json(&body)
        .send()
    {
        Ok(resp) => {
            if let Ok(v) = resp.json::<serde_json::Value>() {
                let done = v.get("done_reason").and_then(|d| d.as_str()).unwrap_or("?");
                if done == "unload" {
                    tracing::info!("Ollama 模型已卸载（done_reason=unload）：{}", cfg.model);
                } else {
                    tracing::warn!("Ollama 卸载响应 done_reason={done}（期望 unload）");
                }
            }
        }
        Err(e) => {
            tracing::warn!("Ollama 卸载请求失败（签名自愈路径不阻塞）：{e}");
        }
    }
}

/// §8.4：只有应用管理的 Windows Ollama 原生请求才注入 keep_alive。
/// 自定义 localhost/局域网服务即使使用 OpenAI 兼容协议，也不得收到 Ollama 扩展字段。
#[cfg(test)]
fn apply_keep_alive(body: &mut serde_json::Value, managed_ollama: bool) {
    if managed_ollama {
        body["keep_alive"] = serde_json::json!(KEEP_ALIVE_IDLE);
    }
}

/// 本地服务空闲保留时长常量（§8.1 L1 服务级默认；与 ollama_installer::KEEP_ALIVE_IDLE 一致）
pub const KEEP_ALIVE_IDLE: &str = "2m";

/// 本地模型是否支持视觉/图片输入（启发式短名单；§9.3 视频批次预检用）。
/// 判定顺序：含视觉标记（vl/vision/llava/moondream/minicpm-v/gemma3 等）→ 支持；
/// 命中已知纯文本模型段 → 不支持；未知一律按支持处理（避免误拦自定义视觉模型）。
pub fn model_supports_vision(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return true;
    }
    let vision_markers = [
        "vl",
        "vision",
        "llava",
        "moondream",
        "minicpm-v",
        "minicpmv",
        "gemma3",
        "qwen3.5",
        "internvl",
        "intern-vl",
        "qwen2.5-vl",
        "qwen2-vl",
        "glm-4v",
        "cogvlm",
        "bunny",
        "llava-phi",
    ];
    if vision_markers.iter().any(|x| m.contains(x)) {
        return true;
    }
    let text_only_markers = [
        "llama3", "llama-3", "llama2", "llama-2", "deepseek", "mistral", "phi-4", "phi4", "phi-3",
        "phi3", "gemma2", "gemma-2", "qwen3", "qwen-3", "qwen2.5", "qwen2", "qwen-2.5", "qwen-2",
        "kimi", "glm-4-", "gemma-1", "gpt-oss",
    ];
    if text_only_markers.iter().any(|x| m.contains(x)) {
        return false;
    }
    true
}

fn resolve_facet<'a>(
    label: &str,
    facets: &'a [FacetPromptContext],
) -> Option<&'a FacetPromptContext> {
    let label = label.trim();
    facets
        .iter()
        .find(|facet| facet.key == label || facet.display_name.trim() == label)
}

fn replace_facet_values(
    tags: &mut CategorizedTags,
    proposals: &mut Vec<ai::TagProposal>,
    key: &str,
    values: Vec<String>,
) {
    proposals.retain(|proposal| proposal.facet_key != key);
    if values.is_empty() {
        tags.remove(key);
        return;
    }
    tags.insert(key.to_string(), values.clone());
    proposals.extend(values.into_iter().map(|raw_name| ai::TagProposal {
        facet_key: key.to_string(),
        raw_name,
        confidence: None,
    }));
}

fn description_mentions_people(description: &str) -> bool {
    [
        "人", "女子", "女孩", "女性", "男子", "男孩", "男性", "老人", "儿童", "孩子", "人群",
        "行人", "模特", "游客", "人物", "少年", "青年", "中年",
    ]
    .iter()
    .any(|word| description.contains(word))
}

fn subject_is_missing(analysis: &MediaAnalysis, facets: &[FacetPromptContext]) -> bool {
    facets.iter().any(|facet| facet.key == "subject")
        && analysis
            .tags
            .get("subject")
            .is_none_or(|values| values.is_empty())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

/// 人物属性按图片级原子标签归一；未知新词保留，交由人工确认。
fn normalize_people_name(raw: &str) -> Vec<String> {
    let value = raw.trim();
    if value.is_empty() || matches!(value, "人物" | "有人") {
        return Vec::new();
    }
    let mapped: &[&str] = match value {
        "男性" | "男子" | "男人" => &["男性"],
        "女性" | "女子" | "女人" => &["女性"],
        "男孩" => &["男性", "儿童"],
        "女孩" => &["女性", "儿童"],
        "男女" => &["男性", "女性"],
        "一个人" | "一人" | "单人" => &["单人"],
        "两个人" | "两人" | "二人" | "双人" => &["双人"],
        "多人" | "三人以上" => &["多人"],
        "人群" | "大量人群" => &["人群"],
        "婴儿" | "婴幼儿" => &["婴幼儿"],
        "儿童" | "孩子" => &["儿童"],
        "少年" | "青少年" => &["青少年"],
        "年轻人" | "青年" => &["青年"],
        "中年" => &["中年"],
        "老人" | "年老" | "老年" => &["老年"],
        "现代服装" | "现代装" => &["现代装"],
        "古代服饰" | "古装" => &["古装"],
        "少数民族服饰" | "民族服饰" => &["民族服饰"],
        "工作服" | "职业装" => &["职业装"],
        "制服" => &["制服"],
        "礼服" => &["礼服"],
        "运动服" | "运动装" => &["运动装"],
        "便装" | "休闲装" => &["休闲装"],
        "泳装" => &["泳装"],
        "站姿" | "站立" => &["站立"],
        "坐着" | "坐姿" => &["坐姿"],
        "走路" | "步行" | "行走" => &["行走"],
        "跑步" | "奔跑" => &["奔跑"],
        "交流" | "交谈" => &["交谈"],
        "工作" => &["工作"],
        "表演" => &["表演"],
        "休息" => &["休息"],
        _ => &[],
    };
    if mapped.is_empty() {
        vec![value.to_string()]
    } else {
        mapped.iter().map(|value| (*value).to_string()).collect()
    }
}

fn is_person_subject(value: &str) -> bool {
    if value == "人" {
        return true;
    }
    const PERSON_TERMS: &[&str] = &[
        "人物", "男子", "女子", "男孩", "女孩", "男性", "女性", "行人", "人群", "老人", "儿童",
        "青年", "中年", "游客", "顾客", "模特", "人像",
    ];
    PERSON_TERMS.iter().any(|term| value.contains(term))
}

fn rewrite_facet_with_proposals(
    analysis: &mut MediaAnalysis,
    key: &str,
    values: Vec<String>,
    proposals: Vec<ai::TagProposal>,
) {
    analysis
        .proposals
        .retain(|proposal| proposal.facet_key != key);
    if values.is_empty() {
        analysis.tags.remove(key);
        return;
    }
    analysis.tags.insert(key.to_string(), values.clone());
    for value in values {
        let confidence = proposals
            .iter()
            .find(|proposal| proposal.raw_name == value)
            .and_then(|proposal| proposal.confidence);
        analysis.proposals.push(ai::TagProposal {
            facet_key: key.to_string(),
            raw_name: value,
            confidence,
        });
    }
}

fn apply_semantic_rules(
    analysis: &mut MediaAnalysis,
    facets: &[FacetPromptContext],
    min_confidence: f64,
) {
    if facets.iter().any(|f| f.key == "subject") {
        let mut values = analysis.tags.remove("subject").unwrap_or_default();
        let mut proposals = analysis
            .proposals
            .iter()
            .filter(|p| p.facet_key == "subject")
            .cloned()
            .collect::<Vec<_>>();
        analysis.proposals.retain(|p| p.facet_key != "subject");

        if matches!(
            analysis.people_presence.status,
            ai::PeoplePresenceStatus::Absent
        ) {
            values.retain(|value| !is_person_subject(value));
            proposals.retain(|proposal| !is_person_subject(&proposal.raw_name));
        } else {
            values = values
                .into_iter()
                .map(|value| {
                    if is_person_subject(&value) {
                        "人".to_string()
                    } else {
                        value
                    }
                })
                .collect();
            proposals = proposals
                .into_iter()
                .map(|mut proposal| {
                    if is_person_subject(&proposal.raw_name) {
                        proposal.raw_name = "人".to_string();
                    }
                    proposal
                })
                .collect();
        }

        let mut unique = Vec::new();
        for value in values {
            push_unique(&mut unique, value);
        }
        if matches!(
            analysis.people_presence.status,
            ai::PeoplePresenceStatus::Present
        ) {
            unique.retain(|value| value != "人");
            unique.insert(0, "人".to_string());
        }
        let cap = facets
            .iter()
            .find(|f| f.key == "subject")
            .and_then(|f| f.max_items)
            .unwrap_or(3)
            .max(0) as usize;
        unique.truncate(cap);

        let mut deduped_proposals = Vec::new();
        for value in &unique {
            let mut proposal = proposals
                .iter()
                .find(|p| p.raw_name == *value)
                .cloned()
                .unwrap_or(ai::TagProposal {
                    facet_key: "subject".to_string(),
                    raw_name: value.clone(),
                    confidence: None,
                });
            if value == "人" && proposal.confidence.is_none() {
                proposal.confidence = Some(analysis.people_presence.confidence);
            }
            proposal.facet_key = "subject".to_string();
            proposal.raw_name = value.clone();
            deduped_proposals.push(proposal);
        }
        rewrite_facet_with_proposals(analysis, "subject", unique, deduped_proposals);
    }

    let has_people_facet = facets.iter().any(|f| f.key == "people");
    if has_people_facet {
        let people_values = analysis.tags.remove("people").unwrap_or_default();
        let people_proposals = analysis
            .proposals
            .iter()
            .filter(|p| p.facet_key == "people")
            .cloned()
            .collect::<Vec<_>>();
        analysis.proposals.retain(|p| p.facet_key != "people");

        let normalized = match analysis.people_presence.status {
            ai::PeoplePresenceStatus::Present => {
                let mut values = Vec::new();
                for value in people_values {
                    for normalized_value in normalize_people_name(&value) {
                        push_unique(&mut values, normalized_value);
                    }
                }
                values.retain(|value| value != "无人" && value != "未知");
                values
            }
            ai::PeoplePresenceStatus::Absent
                if analysis.people_presence.confidence as f64 >= min_confidence
                    && !description_mentions_people(&analysis.description) =>
            {
                vec!["无人".to_string()]
            }
            _ => vec!["未知".to_string()],
        };
        let cap = facets
            .iter()
            .find(|f| f.key == "people")
            .and_then(|f| f.max_items)
            .unwrap_or(8)
            .max(0) as usize;
        let mut normalized = normalized;
        normalized.truncate(cap);
        let mut rebuilt = Vec::new();
        for value in &normalized {
            let confidence = people_proposals
                .iter()
                .find(|p| {
                    p.raw_name == *value
                        || normalize_people_name(&p.raw_name)
                            .iter()
                            .any(|normalized| normalized == value)
                })
                .and_then(|p| p.confidence)
                .or_else(|| {
                    if value == "无人" || value == "未知" {
                        Some(analysis.people_presence.confidence)
                    } else {
                        None
                    }
                });
            rebuilt.push(ai::TagProposal {
                facet_key: "people".to_string(),
                raw_name: value.clone(),
                confidence,
            });
        }
        rewrite_facet_with_proposals(analysis, "people", normalized, rebuilt);
    }
}

/// 打标 V2 协议的唯一解析入口。只接受：
/// {"description":"…","peoplePresence":{"status":"…","confidence":0.9},
///  "tags":{"facetKey":[{"name":"…","confidence":0.9}]},"numbers":{…}}
pub fn parse_media_analysis(
    content: &str,
    facets: &[FacetPromptContext],
    min_confidence: f64,
) -> AppResult<MediaAnalysis> {
    let trimmed = content.trim();
    let json_candidate = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(_) => trimmed,
        Err(_) => match (trimmed.find('{'), trimmed.rfind('}')) {
            (Some(s), Some(e)) if s < e => &trimmed[s..=e],
            _ => trimmed,
        },
    };
    let root: serde_json::Value = serde_json::from_str(json_candidate).map_err(|e| {
        AppError::msg(format!(
            "模型未返回打标 V2 JSON：{e}；原始返回：{}",
            content.chars().take(300).collect::<String>()
        ))
    })?;
    let root_obj = root
        .as_object()
        .ok_or_else(|| AppError::msg("打标 V2 输出必须是 JSON 对象"))?;

    let raw_description = root_obj
        .get("description")
        .and_then(|value| value.as_str())
        .ok_or_else(|| AppError::msg("打标 V2 缺少 description"))?;
    let description = normalize_content_description(raw_description);

    let people = root_obj
        .get("peoplePresence")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::msg("打标 V2 缺少 peoplePresence"))?;
    let people_status = match people.get("status").and_then(|value| value.as_str()) {
        Some("present") => ai::PeoplePresenceStatus::Present,
        Some("absent") => ai::PeoplePresenceStatus::Absent,
        Some("unknown") => ai::PeoplePresenceStatus::Unknown,
        Some(other) => {
            return Err(AppError::msg(format!(
                "非法 peoplePresence.status：{other}"
            )))
        }
        None => return Err(AppError::msg("peoplePresence 缺少 status")),
    };
    let people_confidence = people
        .get("confidence")
        .and_then(|value| value.as_f64())
        .filter(|value| (0.0..=1.0).contains(value))
        .ok_or_else(|| AppError::msg("peoplePresence.confidence 必须是 0–1 的数字"))?
        as f32;

    let raw_tags = root_obj
        .get("tags")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::msg("打标 V2 缺少 tags 对象"))?;
    let mut tags = CategorizedTags::new();
    let mut proposals = Vec::new();
    let mut warnings = Vec::new();
    let mut responded_tag_facets = std::collections::BTreeSet::new();

    for (label, values) in raw_tags {
        let facet = resolve_facet(label, facets)
            .ok_or_else(|| AppError::msg(format!("模型返回了未知分面 key「{label}」")))?;
        if facet.facet_kind == "number" || facet.key == "custom" {
            return Err(AppError::msg(format!(
                "分面「{label}」不允许出现在 tags 中"
            )));
        }
        let values = values
            .as_array()
            .ok_or_else(|| AppError::msg(format!("tags.{label} 必须是数组")))?;
        let mut facet_tags: Vec<String> = Vec::new();
        for value in values {
            let object = value
                .as_object()
                .ok_or_else(|| AppError::msg(format!("tags.{label} 的元素必须是对象")))?;
            let name = object
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= 12)
                .ok_or_else(|| AppError::msg(format!("tags.{label} 的元素缺少合法 name")))?;
            let confidence = object
                .get("confidence")
                .and_then(|value| value.as_f64())
                .filter(|value| (0.0..=1.0).contains(value))
                .ok_or_else(|| AppError::msg(format!("tags.{label}.confidence 必须是 0–1")))?;
            if !facet_tags.iter().any(|existing| existing == name) {
                facet_tags.push(name.to_string());
                proposals.push(ai::TagProposal {
                    facet_key: facet.key.clone(),
                    raw_name: name.to_string(),
                    confidence: Some(confidence as f32),
                });
            }
        }
        let cap = if facet.selection_mode == "single" {
            Some(1)
        } else {
            facet.max_items.map(|value| value.max(0) as usize)
        };
        if let Some(cap) = cap {
            if facet_tags.len() > cap {
                warnings.push(format!("分面「{label}」超过数量上限 {cap}，已截断。"));
                for removed in facet_tags.drain(cap..) {
                    proposals.retain(|proposal| {
                        proposal.facet_key != facet.key || proposal.raw_name != removed
                    });
                }
            }
        }
        responded_tag_facets.insert(facet.key.clone());
        if !facet_tags.is_empty() {
            tags.insert(facet.key.clone(), facet_tags);
        }
    }

    let mut numbers = Vec::new();
    if let Some(raw_numbers) = root_obj.get("numbers").and_then(|value| value.as_object()) {
        for (label, value) in raw_numbers {
            let facet = resolve_facet(label, facets)
                .ok_or_else(|| AppError::msg(format!("numbers 中出现未知分面「{label}」")))?;
            if facet.facet_kind != "number" {
                return Err(AppError::msg(format!("numbers.{label} 不是数值分面")));
            }
            let raw_text = match value {
                serde_json::Value::String(value) => value.trim().to_string(),
                serde_json::Value::Number(value) => value.to_string(),
                _ => return Err(AppError::msg(format!("numbers.{label} 必须是字符串或数字"))),
            };
            if !raw_text.is_empty() {
                numbers.push(ai::NumberProposal {
                    facet_key: facet.key.clone(),
                    raw_text,
                    value: 0.0,
                    confidence: None,
                });
            }
        }
    } else if root_obj.contains_key("numbers") && !root_obj["numbers"].is_null() {
        return Err(AppError::msg("numbers 必须是对象"));
    }

    if tags.is_empty() && description.is_empty() {
        return Err(AppError::msg("打标 V2 未返回可用描述或标签"));
    }

    let mut analysis = MediaAnalysis {
        description,
        people_presence: ai::PeoplePresence {
            status: people_status,
            confidence: people_confidence,
        },
        tags,
        proposals,
        numbers,
        warnings,
        responded_tag_facets,
        raw_response: String::new(),
        request_config_json: None,
        analysis_json: None,
        blocked_low_confidence: 0,
    };
    apply_semantic_rules(&mut analysis, facets, min_confidence);
    Ok(analysis)
}

fn vision_request_should_fallback(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if lower.contains("http 429")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("请求过于频繁")
        || lower.contains("请求频率受限")
    {
        return false;
    }
    let structured_compatibility = lower.contains("response_format")
        || lower.contains("json_schema")
        || lower.contains("json schema")
        || lower.contains("structured output")
        || lower.contains("structured_outputs")
        || lower.contains("tool_choice")
        || ((lower.contains("format") || lower.contains("格式"))
            && (lower.contains("unsupported")
                || lower.contains("not support")
                || lower.contains("不支持")
                || lower.contains("格式")));
    let extension_field_compatibility = (lower.contains("unknown field")
        || lower.contains("unrecognized field")
        || lower.contains("unsupported parameter")
        || lower.contains("invalid parameter"))
        && (lower.contains("think") || lower.contains("format"));
    !is_context_overflow_message(message)
        && !lower.contains("无法连接")
        && !lower.contains("connect")
        && !lower.contains("timed out")
        && !lower.contains("timeout")
        && !lower.contains("401")
        && !lower.contains("403")
        && !lower.contains("unauthorized")
        && !lower.contains("forbidden")
        && !lower.contains("api key")
        && !lower.contains("authentication")
        && !lower.contains("未返回最终内容")
        && !lower.contains("returned empty content")
        && (structured_compatibility || extension_field_compatibility)
}

fn is_context_overflow_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("context limit")
        || lower.contains("prompt is too long")
        || lower.contains("too many tokens")
        || lower.contains("input tokens") && lower.contains("max")
        || lower.contains("上下文") && (lower.contains("超") || lower.contains("过长"))
        || lower.contains("令牌") && (lower.contains("超") || lower.contains("过长"))
}

fn openai_message_content(value: &serde_json::Value, managed_ollama: bool) -> AppResult<String> {
    let message = &value["choices"][0]["message"];
    let content = message["content"]
        .as_str()
        .ok_or_else(|| AppError::msg("服务未返回可选内容（choices 为空或 content 缺失）"))?;
    if !content.trim().is_empty() {
        return Ok(content.to_string());
    }

    let reasoning_len = message
        .get("reasoning")
        .or_else(|| message.get("thinking"))
        .and_then(|item| item.as_str())
        .map(|item| item.chars().count())
        .unwrap_or(0);
    let finish_reason = value["choices"][0]["finish_reason"]
        .as_str()
        .unwrap_or("unknown");
    let action = if managed_ollama {
        "请检查 Ollama 的思考设置或输出 token 限制"
    } else {
        "请检查模型输出设置和 token 限制"
    };
    Err(AppError::msg(format!(
        "服务未返回最终内容（returned empty content；finish_reason={finish_reason}，reasoning_chars={reasoning_len}）。{action}"
    )))
}

#[allow(clippy::too_many_arguments)]
fn openai_vision_request(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    system: &str,
    user: &str,
    b64: &str,
    mime: &str,
    max_tokens: i64,
    tier: TextJsonTier,
    schema: &serde_json::Value,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<String> {
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": user },
                    { "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{b64}") } }
                ]
            }
        ],
        "max_tokens": max_tokens,
        "temperature": 0
    });
    if tier == TextJsonTier::Structured {
        body["response_format"] = serde_json::json!({
            "type": "json_schema",
            "json_schema": { "name": "image_tagging_v2", "strict": true, "schema": schema }
        });
    } else if tier == TextJsonTier::JsonObject {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    let _permit = limiter
        .map(|rate_limiter| rate_limiter.acquire(cancel))
        .transpose()?;
    let response = if cfg.api_key.trim().is_empty() {
        client
            .post(format!(
                "{}/chat/completions",
                cfg.base_url.trim_end_matches('/')
            ))
            .json(&body)
            .send()
    } else {
        client
            .post(format!(
                "{}/chat/completions",
                cfg.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&cfg.api_key)
            .json(&body)
            .send()
    }
    .map_err(|e| vision_transport_error("视觉请求", e, cfg))?;
    let status = response.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let body = response.text().unwrap_or_default();
        return Err(ai_http_status_error_with_body(
            "视觉请求",
            status_code,
            &body,
        ));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| AppError::msg(format!("响应解析失败: {e}")))?;
    openai_message_content(&value, is_managed_ollama_profile(cfg))
}

fn ollama_message_content(value: &serde_json::Value) -> AppResult<String> {
    let message = value
        .get("message")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| AppError::msg("Ollama 返回缺少 message"))?;
    let content = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AppError::msg("Ollama 返回缺少 message.content"))?;
    if !content.trim().is_empty() {
        return Ok(content.to_string());
    }

    let thinking_len = message
        .get("thinking")
        .and_then(serde_json::Value::as_str)
        .map(|item| item.chars().count())
        .unwrap_or(0);
    let done_reason = value
        .get("done_reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let eval_count = value
        .get("eval_count")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    Err(AppError::msg(format!(
        "Ollama 未返回最终内容（done_reason={done_reason}，thinking_chars={thinking_len}，eval_count={eval_count}）。思考输出可能耗尽了生成预算，请升级 Ollama 后重试"
    )))
}

#[allow(clippy::too_many_arguments)]
fn ollama_native_vision_request(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    system: &str,
    user: &str,
    b64: &str,
    max_tokens: i64,
    schema: &serde_json::Value,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<String> {
    let base = cfg.base_url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let body = serde_json::json!({
        "model": cfg.model,
        "stream": false,
        // 打标需要短而确定的 JSON，不消费 token 生成独立思考轨迹。Ollama 对不支持
        // thinking 的模型会忽略 false；支持 thinking 的模型会直接生成最终 content。
        "think": false,
        "format": schema,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user, "images": [b64] }
        ],
        "options": {
            "temperature": 0,
            "num_predict": max_tokens
        },
        "keep_alive": KEEP_ALIVE_IDLE
    });
    let _permit = limiter
        .map(|rate_limiter| rate_limiter.acquire(cancel))
        .transpose()?;
    let response = client
        .post(format!("{root}/api/chat"))
        .json(&body)
        .send()
        .map_err(|e| vision_transport_error("Ollama 视觉请求", e, cfg))?;
    let status = response.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let body = response.text().unwrap_or_default();
        return Err(ai_http_status_error_with_body(
            "Ollama 视觉请求",
            status_code,
            &body,
        ));
    }
    let raw = response
        .text()
        .map_err(|e| AppError::msg(format!("Ollama 响应读取失败: {e}")))?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        AppError::msg(format!(
            "Ollama 响应解析失败: {e}；原始响应：{}",
            raw.chars().take(300).collect::<String>()
        ))
    })?;
    ollama_message_content(&value)
}

#[allow(clippy::too_many_arguments)]
fn anthropic_vision_request(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    system: &str,
    user: &str,
    b64: &str,
    mime: &str,
    max_tokens: i64,
    tier: TextJsonTier,
    schema: &serde_json::Value,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<String> {
    let mut body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": max_tokens,
        "temperature": 0,
        "system": system,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": user },
                { "type": "image", "source": { "type": "base64", "media_type": mime, "data": b64 } }
            ]
        }]
    });
    if tier == TextJsonTier::Structured {
        body["tools"] = serde_json::json!([{
            "name": "emit_image_analysis",
            "description": "返回图片打标 V2 结构化结果。只调用一次。",
            "input_schema": schema
        }]);
        body["tool_choice"] = serde_json::json!({
            "type": "tool",
            "name": "emit_image_analysis"
        });
    }
    let _permit = limiter
        .map(|rate_limiter| rate_limiter.acquire(cancel))
        .transpose()?;
    let response = client
        .post(format!("{}/messages", cfg.base_url.trim_end_matches('/')))
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .map_err(|e| vision_transport_error("Anthropic 视觉请求", e, cfg))?;
    let status = response.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let body = response.text().unwrap_or_default();
        return Err(ai_http_status_error_with_body(
            "Anthropic 视觉请求",
            status_code,
            &body,
        ));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| AppError::msg(format!("Anthropic 响应解析失败: {e}")))?;
    if tier == TextJsonTier::Structured {
        if let Some(tool_input) = extract_anthropic_tool_input(&value) {
            return Ok(tool_input);
        }
    }
    extract_anthropic_text(&value).ok_or_else(|| AppError::msg("Anthropic 返回缺少 text 内容块"))
}

fn apply_people_conflict_guard(
    analysis: &mut MediaAnalysis,
    facets: &[FacetPromptContext],
    min_confidence: f64,
) {
    if analysis.people_presence.status != ai::PeoplePresenceStatus::Absent
        || !description_mentions_people(&analysis.description)
    {
        return;
    }
    analysis.people_presence.status = ai::PeoplePresenceStatus::Unknown;
    analysis.people_presence.confidence = analysis.people_presence.confidence.min(0.49);
    replace_facet_values(
        &mut analysis.tags,
        &mut analysis.proposals,
        "people",
        vec!["未知".to_string()],
    );
    apply_semantic_rules(analysis, facets, min_confidence);
}

#[allow(clippy::too_many_arguments)]
fn request_analysis(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    facets: &[FacetPromptContext],
    top_tags: &[(String, String)],
    image_path: &std::path::Path,
    system_override: &str,
    min_confidence: f64,
    repair_empty_subject: bool,
    tier_cache: &Cell<TextJsonTier>,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<MediaAnalysis> {
    let prepare_image = |max_px: u32| {
        imaging::encode_ai_image(image_path, max_px).ok_or_else(|| {
            let extension = image_path
                .extension()
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("unknown");
            let cause = if image_path.exists() {
                "格式不受支持、文件损坏或对应解码器不可用"
            } else {
                "文件不存在或路径已失效"
            };
            AppError::unsupported(format!("无法解码素材图片（扩展名 .{extension}）：{cause}"))
        })
    };
    let mut image_payload = prepare_image(imaging::AI_IMAGE_MAX_PX)?;
    let mime = "image/jpeg";

    // system prompt 可由用户完整替换；JSON Schema 始终在请求层附加，不能被提示词关闭。
    let system = if system_override.trim().is_empty() {
        build_system_prompt()
    } else {
        system_override.to_string()
    };
    let user = build_user_prompt(facets, top_tags);
    let max_tokens = dynamic_max_tokens(facets.len());
    let schema = tagging_schema(facets);
    let initial_tier = tier_cache.get();

    let transport = vision_transport(cfg);
    let send = |payload: &AiImagePayload, tier: TextJsonTier, prompt: &str| -> AppResult<String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&payload.bytes);
        if transport == VisionTransport::ExternalAnthropic {
            return anthropic_vision_request(
                client, cfg, &system, prompt, &b64, mime, max_tokens, tier, &schema, limiter,
                cancel,
            );
        }
        if transport == VisionTransport::ManagedWindowsOllama && tier == TextJsonTier::Structured {
            match ollama_native_vision_request(
                client, cfg, &system, prompt, &b64, max_tokens, &schema, limiter, cancel,
            ) {
                Ok(content) => return Ok(content),
                Err(native_error) => {
                    if !vision_request_should_fallback(&native_error.to_string()) {
                        return Err(native_error);
                    }
                    tracing::warn!(
                        operation = "ai_tagging",
                        stage = "native_structured_fallback",
                        error = %native_error,
                        "Ollama 原生结构化请求失败，尝试 OpenAI 兼容结构化接口"
                    );
                    return openai_vision_request(
                        client, cfg, &system, prompt, &b64, mime, max_tokens, tier, &schema,
                        limiter, cancel,
                    )
                    .map_err(|compat_error| AppError::msg(format!(
                        "Ollama 原生结构化请求失败：{native_error}；兼容接口回退失败：{compat_error}"
                    )));
                }
            }
        }
        openai_vision_request(
            client, cfg, &system, prompt, &b64, mime, max_tokens, tier, &schema, limiter, cancel,
        )
    };

    let request_with_fallback = |payload: &AiImagePayload,
                                 start: TextJsonTier,
                                 prompt: &str|
     -> AppResult<(TextJsonTier, String)> {
        let tiers: &[TextJsonTier] = match start {
            TextJsonTier::Structured => &[
                TextJsonTier::Structured,
                TextJsonTier::JsonObject,
                TextJsonTier::Plain,
            ],
            TextJsonTier::JsonObject => &[TextJsonTier::JsonObject, TextJsonTier::Plain],
            TextJsonTier::Plain => &[TextJsonTier::Plain],
        };
        let mut last_error = None;
        for tier in tiers {
            match send(payload, *tier, prompt) {
                Ok(content) => {
                    tier_cache.set(*tier);
                    return Ok((*tier, content));
                }
                Err(error) => {
                    let message = error.to_string();
                    if !vision_request_should_fallback(&message) {
                        return Err(error);
                    }
                    last_error = Some(message);
                }
            }
        }
        Err(AppError::msg(format!(
            "视觉结构化请求逐级降级后仍失败：{}",
            last_error.unwrap_or_else(|| "未知错误".to_string())
        )))
    };

    let (mut used_tier, mut content) =
        match request_with_fallback(&image_payload, initial_tier, &user) {
            Ok(result) => result,
            Err(error) if is_context_overflow_message(&error.to_string()) => {
                tracing::warn!(
                    operation = "ai_tagging",
                    stage = "image_context_fallback",
                    initial_max_px = image_payload.max_px,
                    fallback_max_px = imaging::AI_IMAGE_FALLBACK_MAX_PX,
                    error = %error,
                    "模型上下文不足，降低 AI 图片尺寸后重试"
                );
                image_payload = prepare_image(imaging::AI_IMAGE_FALLBACK_MAX_PX)?;
                request_with_fallback(&image_payload, initial_tier, &user)?
            }
            Err(error) => return Err(error),
        };
    let enrich =
        |mut a: MediaAnalysis, used_content: String, tier: TextJsonTier| -> MediaAnalysis {
            a.raw_response = used_content;
            a.request_config_json = Some(
                build_batch_request_config_with_image_max(
                    &system,
                    facets,
                    top_tags,
                    &cfg.model,
                    "image",
                    max_tokens,
                    is_managed_ollama_profile(cfg),
                    tier,
                    min_confidence,
                    image_payload.max_px,
                )
                .to_string(),
            );
            a.analysis_json = Some(
                serde_json::to_string(&ai::AnalysisResult {
                    description: a.description.clone(),
                    people_presence: a.people_presence.clone(),
                    proposals: a.proposals.clone(),
                    numbers: a.numbers.clone(),
                    warnings: a.warnings.clone(),
                })
                .unwrap_or_default(),
            );
            a
        };
    let mut first_error: Option<String> = None;
    let mut analysis = match parse_media_analysis(&content, facets, min_confidence) {
        Ok(analysis) => analysis,
        Err(error) => {
            first_error = Some(error.to_string());
            MediaAnalysis::default()
        }
    };

    let missing_subject = repair_empty_subject
        && !analysis.description.trim().is_empty()
        && subject_is_missing(&analysis, facets);
    let needs_repair = first_error.is_some()
        || !missing_tag_facet_keys(&analysis, facets).is_empty()
        || analysis.description.chars().count() < MIN_DESCRIPTION_CHARS
        || missing_subject
        || (analysis.people_presence.status == ai::PeoplePresenceStatus::Absent
            && description_mentions_people(&analysis.description));

    if needs_repair {
        let problems = if let Some(error) = &first_error {
            error.clone()
        } else {
            let mut items = Vec::new();
            let missing = missing_tag_facet_keys(&analysis, facets);
            if !missing.is_empty() {
                items.push(format!("缺少必需分面：{}", missing.join(", ")));
            }
            if analysis.description.chars().count() < MIN_DESCRIPTION_CHARS {
                items.push(format!(
                    "description 过短，必须为 {MIN_DESCRIPTION_CHARS}–{MAX_DESCRIPTION_CHARS} 个字符"
                ));
            }
            if missing_subject {
                items.push(
                    "subject 为空：description 中有明确主体时必须补全；确实没有可命名对象时可保持空数组"
                        .to_string(),
                );
            }
            if analysis.people_presence.status == ai::PeoplePresenceStatus::Absent
                && description_mentions_people(&analysis.description)
            {
                items.push("description 已明确出现人物，但 peoplePresence 为 absent".to_string());
            }
            items.join("；")
        };
        let repair_prompt = format!(
            "{user}\n\n上一次输出不合格：{problems}\n上一次原始输出：{}\n请重新观察同一张图片，只返回完整的打标 V2 JSON。",
            content.chars().take(1200).collect::<String>()
        );
        match request_with_fallback(&image_payload, used_tier, &repair_prompt) {
            Ok((tier, repaired)) => match parse_media_analysis(&repaired, facets, min_confidence) {
                Ok(mut repaired_analysis) => {
                    let still_missing = missing_tag_facet_keys(&repaired_analysis, facets);
                    if !still_missing.is_empty() {
                        return Err(AppError::msg(format!(
                            "模型修复后仍漏掉必需分面：{}",
                            still_missing.join(", ")
                        )));
                    }
                    if repaired_analysis.people_presence.status == ai::PeoplePresenceStatus::Absent
                        && description_mentions_people(&repaired_analysis.description)
                    {
                        apply_people_conflict_guard(&mut repaired_analysis, facets, min_confidence);
                        repaired_analysis
                            .warnings
                            .push("人物状态与描述冲突，已改为未知，避免误写无人。".to_string());
                    }
                    if repaired_analysis.description.chars().count() < MIN_DESCRIPTION_CHARS {
                        repaired_analysis.warnings.push(format!(
                            "description 修复后仍不足 {MIN_DESCRIPTION_CHARS} 个字符。"
                        ));
                    }
                    if repair_empty_subject
                        && subject_is_missing(&repaired_analysis, facets)
                        && !repaired_analysis.description.trim().is_empty()
                    {
                        repaired_analysis
                            .warnings
                            .push("subject 修复后仍为空：未识别明确主体，已保留空值。".to_string());
                    }
                    content.push_str("\n--- validation repair ---\n");
                    content.push_str(&repaired);
                    used_tier = tier;
                    analysis = repaired_analysis;
                }
                Err(error) if first_error.is_some() => return Err(error),
                Err(error) => {
                    tracing::warn!(
                        operation = "ai_tagging",
                        stage = "repair_parse",
                        error = %error,
                        "打标修复响应仍无法解析，保留首次有效结果"
                    );
                }
            },
            Err(error) if first_error.is_some() => return Err(error),
            Err(error) => tracing::warn!(
                operation = "ai_tagging",
                stage = "repair_request",
                error_code = error.code(),
                error = %error,
                "打标修复请求失败，保留首次有效结果"
            ),
        }
    }

    let still_missing = missing_tag_facet_keys(&analysis, facets);
    if !still_missing.is_empty() {
        return Err(AppError::msg(format!(
            "模型输出仍漏掉必需分面：{}",
            still_missing.join(", ")
        )));
    }
    if analysis.people_presence.status == ai::PeoplePresenceStatus::Absent
        && description_mentions_people(&analysis.description)
    {
        apply_people_conflict_guard(&mut analysis, facets, min_confidence);
        analysis
            .warnings
            .push("人物状态与描述冲突，已改为未知，避免误写无人。".to_string());
    }
    if analysis.description.chars().count() < MIN_DESCRIPTION_CHARS {
        analysis
            .warnings
            .push(format!("description 不足 {MIN_DESCRIPTION_CHARS} 个字符。"));
    }
    if repair_empty_subject
        && !analysis.description.trim().is_empty()
        && subject_is_missing(&analysis, facets)
        && !analysis
            .warnings
            .iter()
            .any(|warning| warning.starts_with("subject "))
    {
        analysis
            .warnings
            .push("subject 为空：未识别明确主体，已保留空值。".to_string());
    }
    if is_degenerate(&content) {
        if is_managed_ollama_profile(cfg) {
            unload_ollama_model(cfg);
        }
        let snippet: String = content.chars().take(120).collect();
        return Err(AppError::msg(format!(
            "模型输出持续异常（原始返回：{snippet}…）：请重启本地模型服务后重试。"
        )));
    }
    Ok(enrich(analysis, content, used_tier))
}

/// 从 Anthropic Messages 响应中取第一个 text 内容块
pub fn extract_anthropic_text(v: &serde_json::Value) -> Option<String> {
    v["content"]
        .as_array()?
        .iter()
        .find(|b| b["type"].as_str() == Some("text"))
        .and_then(|b| b["text"].as_str().map(String::from))
}

/// 从模型列表响应中提取模型 id（宽容解析三种形态，FB5-04 §13.5）：
///  1. OpenAI 兼容：{"data": [{"id": "..."}]}
///  2. 扁平列表：{"models": ["a", "b"]}
///  3. 顶层数组：["a", "b"]
pub fn parse_model_ids(v: &serde_json::Value) -> Vec<String> {
    let collect = |arr: &Vec<serde_json::Value>| -> Vec<String> {
        arr.iter()
            .filter_map(|m| {
                m.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .or_else(|| {
                        m["id"]
                            .as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    })
            })
            .collect()
    };
    if let Some(arr) = v["data"].as_array() {
        return collect(arr);
    }
    if let Some(arr) = v["models"].as_array() {
        return collect(arr);
    }
    if let Some(arr) = v.as_array() {
        return collect(arr);
    }
    Vec::new()
}

/// 一句话描述规范化，最多 30 个 Unicode 字符（不按 UTF-8 bytes）。
/// 顺序：1. trim → 2. 换行/连续空白折叠为单空格 → 3. 去除开头无信息套话
/// （「这是一张/这张图片展示/画面中有」等）→ 4. 取第一个句段并去掉句末 。！？
/// → 5. 按 chars 截断到 30。
/// 描述为空/全是空白时返回空串；调用方不得因此让有效标签整条失败。
pub fn normalize_content_description(raw: &str) -> String {
    const FILLERS: &[&str] = &[
        "这是一张",
        "这是一幅",
        "这张图片展示",
        "这张图片显示",
        "这张图片是",
        "这张照片",
        "图片展示",
        "图片中",
        "画面中有",
        "画面中",
        "画面是",
        "图中是",
        "照片中",
        "图中有",
    ];

    // 1-2. trim + 折叠空白（Unicode 空白统一为单个普通空格）
    let mut folded = String::with_capacity(raw.len());
    let mut prev_space = false;
    for ch in raw.trim().chars() {
        if ch.is_whitespace() {
            if !prev_space {
                folded.push(' ');
                prev_space = true;
            }
        } else {
            folded.push(ch);
            prev_space = false;
        }
    }
    let mut s = folded;

    // 3. 去开头套话（任一命中即停；命中后清掉剩余前导空白）
    for f in FILLERS {
        if let Some(rest) = s.strip_prefix(f) {
            s = rest.trim_start().to_string();
            break;
        }
    }

    // 4. 取第一个完整句段，去掉句末标点
    if let Some(idx) = s.find(&['。', '！', '？', '.', '!', '?'][..]) {
        s = s[..idx].trim_end().to_string();
    }

    // 5. 按 Unicode 字符截断（不按 bytes）
    s.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

/// 结构化输出的降级等级（P3 §9.3）：
/// 第 1 级 = API 级结构化输出（Anthropic tool use / OpenAI json_schema）
/// 第 2 级 = json_object + prompt 内嵌 schema
/// 第 3 级 = 纯 prompt + 宽容 JSON 解析（含截取 {} 片段 + 带错误重试一次）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TextJsonTier {
    Plain,
    JsonObject,
    Structured,
}

/// 读取 JSON 响应，并把非 2xx 状态映射成稳定的 AI 错误码与安全详情。
fn response_json_or_error(
    response: reqwest::blocking::Response,
    service: &str,
) -> AppResult<serde_json::Value> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| AppError::msg(format!("{service}响应读取失败: {error}")))?;
    if !status.is_success() {
        return Err(ai_http_status_error_with_body(
            service,
            status.as_u16(),
            &body,
        ));
    }
    serde_json::from_str(&body)
        .map_err(|error| AppError::msg(format!("{service}响应解析失败: {error}")))
}

/// 发起一次纯文本 JSON 请求，返回模型文本回复。
/// 统一 OpenAI 兼容与 Anthropic 两种协议；不解析语义，由调用方校验。
/// structured_schema 仅在 Tier::Structured 下使用（OpenAI json_schema 或 Anthropic tool input_schema）。
#[allow(clippy::too_many_arguments)]
fn request_text_raw(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    system: &str,
    user: &str,
    tier: TextJsonTier,
    structured_schema: Option<serde_json::Value>,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<String> {
    let _permit = limiter
        .map(|rate_limiter| rate_limiter.acquire(cancel))
        .transpose()?;
    let conn_err = |e: reqwest::Error| {
        if is_managed_ollama_profile(cfg) {
            AppError::msg(format!(
                "无法连接应用管理的 Ollama {base}：请确认本地服务已启动: {e}",
                base = cfg.base_url
            ))
        } else {
            AppError::msg(format!("外部 AI 服务请求失败: {e}"))
        }
    };
    let base = cfg.base_url.trim_end_matches('/');
    let messages = serde_json::json!([
        { "role": "system", "content": system },
        { "role": "user", "content": user }
    ]);

    let fetch: Box<dyn Fn() -> AppResult<String>> = if cfg.api_mode == "anthropic" {
        Box::new(move || {
            let mut body = serde_json::json!({
                "model": cfg.model,
                "max_tokens": 1024,
                "system": system,
                "messages": [ { "role": "user", "content": user } ]
            });
            // 第 1 级：tool use + 强制 tool_choice（schema 作为工具 input_schema）
            if tier == TextJsonTier::Structured {
                if let Some(sch) = &structured_schema {
                    body["tools"] = serde_json::json!([{
                        "name": "emit_search_intent",
                        "description": "输出自然语言解析后的查询意图。只调用一次，用返回的 JSON 作为最终结果。",
                        "input_schema": sch
                    }]);
                    body["tool_choice"] = serde_json::json!({
                        "type": "tool", "name": "emit_search_intent"
                    });
                }
            }
            let resp = response_json_or_error(
                client
                    .post(format!("{base}/messages"))
                    .header("x-api-key", &cfg.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
                    .send()
                    .map_err(conn_err)?,
                "AI 搜索请求",
            )?;
            // Anthropic tool use：取 tool_use 块的 input 作为结构化结果
            if tier == TextJsonTier::Structured {
                if let Some(tool_input) = extract_anthropic_tool_input(&resp) {
                    return Ok(tool_input);
                }
            }
            extract_anthropic_text(&resp)
                .ok_or_else(|| AppError::msg("Anthropic 返回缺少 text 内容块"))
        })
    } else {
        // OpenAI 兼容：Bearer 鉴权 + /chat/completions；Tier::Structured 用 response_format json_schema
        Box::new(move || {
            let mut body = serde_json::json!({
                "model": cfg.model,
                "messages": messages,
                "max_tokens": 1024
            });
            if tier == TextJsonTier::Structured {
                if let Some(sch) = &structured_schema {
                    body["response_format"] = serde_json::json!({
                        "type": "json_schema",
                        "json_schema": { "name": "search_intent", "strict": true, "schema": sch }
                    });
                }
            } else if tier == TextJsonTier::JsonObject {
                body["response_format"] = serde_json::json!({ "type": "json_object" });
            }
            let resp = if cfg.api_key.trim().is_empty() {
                response_json_or_error(
                    client
                        .post(format!("{base}/chat/completions"))
                        .json(&body)
                        .send()
                        .map_err(conn_err)?,
                    "AI 搜索请求",
                )?
            } else {
                response_json_or_error(
                    client
                        .post(format!("{base}/chat/completions"))
                        .bearer_auth(&cfg.api_key)
                        .json(&body)
                        .send()
                        .map_err(conn_err)?,
                    "AI 搜索请求",
                )?
            };
            resp["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| {
                    let raw = serde_json::to_string(&resp).unwrap_or_default();
                    let snippet: String = raw.chars().take(300).collect();
                    AppError::msg(format!("服务未返回内容。原始响应：{snippet}"))
                })
        })
    };
    fetch()
}

/// 从 Anthropic 响应中提取 tool_use 块的 input（结构化输出第 1 级）
fn extract_anthropic_tool_input(v: &serde_json::Value) -> Option<String> {
    for block in v["content"].as_array()? {
        if block["type"].as_str() == Some("tool_use") {
            if let Some(input) = block["input"].as_object() {
                // 序列化为紧凑 JSON 字符串（保持与 OpenAI content 路径一致）
                return serde_json::to_string(input).ok();
            }
        }
    }
    None
}

/// 三级降级的文本 JSON 请求：第 1 级结构化 → 第 2 级 json_object → 第 3 级纯文本。
/// 每级失败（含 400/unknown field）自动降级；返回 (tier_used, 模型文本)。
/// 最终仍失败返回最后一次错误。profile 级能力缓存由调用方（super_search_ai）维护。
pub fn request_text_json(
    cfg: &ApiProfile,
    system: &str,
    user: &str,
    structured_schema: Option<serde_json::Value>,
    max_structured_tier: TextJsonTier,
) -> AppResult<(TextJsonTier, String)> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| AppError::msg(format!("HTTP 客户端初始化失败: {e}")))?;
    let rate_limiter = if is_managed_ollama_profile(cfg) {
        None
    } else {
        ai_rate_limit::for_connection(
            &cfg.id,
            cfg.max_concurrency,
            cfg.requests_per_minute,
            cfg.requests_per_hour,
        )
    };
    let cancel = AtomicBool::new(false);

    // 第 3 级失败时带错误重试逻辑单独处理；一级一级降级。
    let mut last_err: Option<String> = None;

    let should_fallback = |message: &str| {
        let lower = message.to_ascii_lowercase();
        !lower.contains("无法连接")
            && !lower.contains("connect")
            && !lower.contains("timed out")
            && !lower.contains("timeout")
            && !lower.contains("401")
            && !lower.contains("403")
            && !lower.contains("unauthorized")
            && !lower.contains("forbidden")
            && !lower.contains("api key")
            && !lower.contains("authentication")
    };

    if max_structured_tier >= TextJsonTier::Structured {
        match request_text_raw(
            &client,
            cfg,
            system,
            user,
            TextJsonTier::Structured,
            structured_schema.clone(),
            rate_limiter.as_deref(),
            &cancel,
        ) {
            Ok(t) => return Ok((TextJsonTier::Structured, t)),
            Err(e) => {
                if is_ai_rate_limited(&e) {
                    return Err(e);
                }
                let message = e.to_string();
                if !should_fallback(&message) {
                    return Err(e);
                }
                last_err = Some(message);
            }
        }
    }
    if max_structured_tier >= TextJsonTier::JsonObject {
        match request_text_raw(
            &client,
            cfg,
            system,
            user,
            TextJsonTier::JsonObject,
            None,
            rate_limiter.as_deref(),
            &cancel,
        ) {
            Ok(t) => return Ok((TextJsonTier::JsonObject, t)),
            Err(e) => {
                if is_ai_rate_limited(&e) {
                    return Err(e);
                }
                let message = e.to_string();
                if !should_fallback(&message) {
                    return Err(e);
                }
                last_err = Some(message);
            }
        }
    }
    match request_text_raw(
        &client,
        cfg,
        system,
        user,
        TextJsonTier::Plain,
        None,
        rate_limiter.as_deref(),
        &cancel,
    ) {
        Ok(t) => Ok((TextJsonTier::Plain, t)),
        Err(e) => {
            if is_ai_rate_limited(&e) {
                return Err(e);
            }
            let detail = last_err.unwrap_or_default();
            Err(AppError::msg(format!(
                "AI 请求降级仍失败：{detail}；最后尝试：{e}"
            )))
        }
    }
}

/// 纯文本 JSON 请求的本地退化检测（复用 is_degenerate）
pub fn is_degenerate_text(content: &str) -> bool {
    is_degenerate(content)
}

/// 拼模型列表 URL：去尾斜杠后接 /models；地址已以 /models 结尾时不重复拼接（FB5-04 §13.5）。
pub fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.to_ascii_lowercase().ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models")
    }
}

/// FB5-04（§3.6）：拉取服务商可用模型列表（GET {base_url}/models）。
/// 错误分类为可读提示（不把「无模型列表」误报成服务完全不可用）：
///  - 401/403 → 密钥无效或无模型列表权限
///  - 404/405 → 该服务未提供模型列表，请手动输入（提示可能缺 /v1 前缀，但不自动补）
///  - 429 → 请求过于频繁
///  - 超时/网络 → 连接问题
///  - 200 但无可解析模型 → 同 404 文案
///    模型去重 + 不区分大小写排序；错误与 debug 输出不含 API Key（请求带 key，消息只用 base_url/status）。
pub fn discover_models(
    base_url: &str,
    api_key: &str,
    protocol: &str,
    managed_ollama: bool,
) -> AppResult<Vec<String>> {
    if base_url.trim().is_empty() {
        return Err(AppError::invalid_arg("请先填写服务地址"));
    }
    let url = models_url(base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::internal(format!("HTTP 客户端初始化失败: {e}")))?;
    let mut req = client.get(&url);
    let anthropic = protocol == "anthropic_messages" || protocol == "anthropic";
    if anthropic {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else if !api_key.trim().is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => {
            if e.is_timeout() {
                return Err(AppError::timeout("模型列表请求超时，请检查服务地址或网络"));
            }
            return Err(AppError::msg(if managed_ollama {
                format!("无法连接应用管理的 Ollama {url}：请确认服务已启动")
            } else {
                "无法连接服务，请检查服务地址或网络".to_string()
            }));
        }
    };
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let msg = match code {
            401 | 403 => "密钥无效或无模型列表权限".to_string(),
            404 | 405 => {
                "该服务未提供模型列表，请手动输入。若服务地址缺少 /v1 前缀，可在末尾补全 /v1 后重试。"
                    .to_string()
            }
            429 => "请求过于频繁（HTTP 429），请稍后重试".to_string(),
            _ => format!("模型列表请求失败（HTTP {code}）"),
        };
        return match code {
            401 | 403 => Err(AppError::unauthorized(msg)),
            404 | 405 => Err(AppError::not_found(msg)),
            429 => Err(AppError::ai_rate_limited(msg)),
            _ => Err(AppError::internal(msg)),
        };
    }
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| AppError::msg(format!("模型列表解析失败: {e}")))?;
    let mut models = parse_model_ids(&json);
    if models.is_empty() {
        return Err(AppError::not_found("该服务未提供模型列表，请手动输入"));
    }
    // 去重 + 不区分大小写排序（§3.6）
    models.sort_by_key(|a| a.to_lowercase());
    models.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    Ok(models)
}

// ── FB3-08（§10.2）：连接测试（按协议分支，密钥只在 Rust 侧从 keyring 读取） ──

/// 连接测试结果（camelCase 给前端；错误信息脱敏——不含密钥与完整 URL query）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectionTestResult {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub latency_ms: u64,
    pub protocol: String,
    pub model: String,
    pub message: String,
}

/// 拼测试 URL：base_url 去尾斜杠后接 path（用户已含 /v1 或不含都兼容）
fn join_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// 连接测试主入口（阻塞网络请求，命令层包 spawn_blocking）。
/// protocol: openai_chat | anthropic_messages；托管 Ollama 与外部服务都通过 /models 测试，
/// 但错误文案只对 Windows 默认 Ollama 提示本地服务。
pub fn test_connection(
    base_url: &str,
    api_key: &str,
    protocol: &str,
    model: &str,
    managed_ollama: bool,
) -> AiConnectionTestResult {
    let started = std::time::Instant::now();
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return AiConnectionTestResult {
                ok: false,
                status_code: None,
                latency_ms: started.elapsed().as_millis() as u64,
                protocol: protocol.to_string(),
                model: model.to_string(),
                message: format!("无法初始化网络请求: {e}"),
            }
        }
    };

    let (ok, status_code, message) = match protocol {
        // Anthropic Messages：不假设 /models 可用，用最小 /messages 请求验证鉴权与路径
        "anthropic_messages" => {
            let url = join_url(base_url, "/messages");
            let body = serde_json::json!({
                "model": if model.is_empty() { "claude-3-haiku-20240307" } else { model },
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hi"}],
            });
            let resp = client
                .post(&url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(body.to_string())
                .send();
            match resp {
                Ok(r) => {
                    let code = r.status().as_u16();
                    let ok = r.status().is_success();
                    let msg = if ok {
                        format!("连接成功（Anthropic Messages，HTTP {code}）")
                    } else {
                        anthropic_fail_message(code)
                    };
                    (ok, Some(code), msg)
                }
                Err(e) => (false, None, network_error_message(&e, &url)),
            }
        }
        // OpenAI 兼容 / 本地：GET /models（带 Bearer；本地通常无 Key 不带鉴权头）
        _ => {
            let url = join_url(base_url, "/models");
            let mut req = client.get(&url);
            if !api_key.trim().is_empty() {
                req = req.bearer_auth(api_key);
            }
            match req.send() {
                Ok(r) => {
                    let code = r.status().as_u16();
                    let ok = r.status().is_success();
                    let msg = if ok {
                        // 校验指定模型是否在列表中（有模型名时给出更精确的结论）
                        let listed = r.json::<serde_json::Value>().ok();
                        let models = listed.as_ref().map(parse_model_ids).unwrap_or_default();
                        if !model.is_empty()
                            && !models.is_empty()
                            && !models.iter().any(|m| m == model)
                        {
                            format!("服务可达（HTTP {code}），但模型列表中没有「{model}」。请核对该服务实际可用的模型名。")
                        } else if models.is_empty() {
                            format!("连接成功（HTTP {code}，未返回模型列表）")
                        } else {
                            format!("连接成功（HTTP {code}，共 {} 个模型）", models.len())
                        }
                    } else {
                        openai_fail_message(code, managed_ollama)
                    };
                    let result_ok = ok && (model.is_empty() || !msg.contains("模型列表中没有"));
                    return finish(result_ok, Some(code), msg, started, protocol, model);
                }
                Err(e) => (false, None, network_error_message(&e, &url)),
            }
        }
    };
    finish(ok, status_code, message, started, protocol, model)
}

fn finish(
    ok: bool,
    status_code: Option<u16>,
    message: String,
    started: std::time::Instant,
    protocol: &str,
    model: &str,
) -> AiConnectionTestResult {
    AiConnectionTestResult {
        ok,
        status_code,
        latency_ms: started.elapsed().as_millis() as u64,
        protocol: protocol.to_string(),
        model: model.to_string(),
        message,
    }
}

/// HTTP 层错误 → 可读建议（脱敏：不回显完整 URL/密钥）
fn network_error_message(e: &reqwest::Error, url: &str) -> String {
    if e.is_timeout() {
        return "请求超时（20 秒）。服务可能未启动、地址/端口错误或被防火墙拦截。".to_string();
    }
    if e.is_connect() {
        // DNS/TCP 连接失败：给出地址核对建议，但只提示 host 而非完整 URL
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("");
        return format!("无法连接到 {host}。请核对服务地址是否正确（在线服务通常以 https:// 开头并以 /v1 结尾）、网络是否可达。");
    }
    if e.is_decode() {
        return "服务响应了，但返回内容不是有效的 JSON。请确认地址指向 API 而不是网页。"
            .to_string();
    }
    format!("请求失败: {e}")
}

fn openai_fail_message(code: u16, managed_ollama: bool) -> String {
    match code {
        401 | 403 => {
            "服务可达，但密钥无效或没有权限（401/403）。请检查 API 密钥是否正确、是否过期。"
                .to_string()
        }
        404 => {
            "路径不存在（404）。请检查服务地址是否已包含正确的路径（如 /v1），或去掉多余的路径。"
                .to_string()
        }
        429 => "请求频率受限（429）。服务可达，稍后重试即可。".to_string(),
        _ if managed_ollama => {
            format!("本地应用管理的 Ollama 返回 HTTP {code}。请确认引擎已启动且端口正确。")
        }
        _ => format!("服务返回 HTTP {code}。请核对该服务是否为 OpenAI 兼容接口。"),
    }
}

fn anthropic_fail_message(code: u16) -> String {
    match code {
        401 => "密钥无效（401）。请检查 Anthropic API 密钥。".to_string(),
        403 => "没有权限（403）。密钥可能无权访问该模型。".to_string(),
        404 => "路径或模型不存在（404）。请检查地址是否以 /v1 结尾、模型名是否可用。".to_string(),
        _ => format!("Anthropic 服务返回 HTTP {code}。"),
    }
}

/// 取用于打标的素材路径。
///
/// 图片优先走原始文件，之后在 request_analysis 内统一解码为 AI JPEG；这样 UI 是否
/// 打开过高清预览不会改变请求内容。视频仍使用已有封面/高清帧作为视觉输入。
fn pick_image(asset: &assets::Asset) -> PathBuf {
    if !asset.mime_type.starts_with("video/") {
        let original = PathBuf::from(&asset.file_path);
        if original.exists() {
            return original;
        }
    }
    if let Some(p) = &asset.hd_thumbnail_path {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }
    if let Some(p) = &asset.placeholder_path {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }
    PathBuf::from(&asset.file_path)
}

/// P3-02：多帧标签频次合并——同分类同标签命中 ≥ ceil(ok/2) 帧才进建议；单帧时阈值 1；
/// 每分类最多保留 5 个（与单帧解析上限一致，防标签体系污染）
pub fn merge_frame_tags(frames: &[CategorizedTags]) -> CategorizedTags {
    let ok = frames.len();
    if ok == 0 {
        return CategorizedTags::new();
    }
    // FB2-07（§13.5②）：阈值随帧数自适应 ceil(n/2)。固定 2 在 n=6 时过松（1/3 帧命中就通过），
    // n=2 时又过严。n=2→1、n=3→2、n=4→2、n=6→3、n=8→4
    let threshold = ok.div_ceil(2);
    let mut counts: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for t in frames {
        for (cat, tags) in t {
            for tag in tags {
                *counts.entry((cat.clone(), tag.clone())).or_default() += 1;
            }
        }
    }
    let mut out = CategorizedTags::new();
    for ((cat, tag), c) in counts {
        if c >= threshold {
            let v = out.entry(cat).or_default();
            if v.len() < 5 {
                v.push(tag);
            }
        }
    }
    out
}

/// 多帧标签置信度合并：只保留达到同一多数阈值的 proposals，并取成功帧的平均值。
fn merge_frame_proposals(
    frames: &[MediaAnalysis],
    merged_tags: &CategorizedTags,
) -> Vec<ai::TagProposal> {
    if frames.is_empty() {
        return Vec::new();
    }
    let threshold = frames.len().div_ceil(2);
    let mut grouped: std::collections::BTreeMap<(String, String), Vec<Option<f32>>> =
        std::collections::BTreeMap::new();
    for analysis in frames {
        for proposal in &analysis.proposals {
            let raw_name = proposal.raw_name.trim();
            if raw_name.is_empty() {
                continue;
            }
            grouped
                .entry((proposal.facet_key.clone(), raw_name.to_string()))
                .or_default()
                .push(proposal.confidence);
        }
    }
    grouped
        .into_iter()
        .filter_map(|((facet_key, raw_name), confidences)| {
            if confidences.len() < threshold
                || !merged_tags
                    .get(&facet_key)
                    .is_some_and(|names| names.iter().any(|name| name == &raw_name))
            {
                return None;
            }
            let values: Vec<f32> = confidences.into_iter().flatten().collect();
            let confidence = if values.is_empty() {
                None
            } else {
                Some(values.iter().sum::<f32>() / values.len() as f32)
            };
            Some(ai::TagProposal {
                facet_key,
                raw_name,
                confidence,
            })
        })
        .collect()
}

/// P3-02 + FB2-07 + FB5-05（§7.7）：视频抽帧打标（frames 模式）——抽 n 段中点帧逐帧请求后频次合并。
/// 标签按现有频次规则合并；描述取时间上最接近视频中点的成功帧描述（抽帧本身按段中点时间序采样，
/// 成功帧序列的中间帧即近似中点；不为合并描述额外发第二次 AI 请求）。
/// 抽帧/识别全失败返回 Err（单条置 rejected）；所有帧描述为空时保持空描述，不影响标签结果。
// 8 参数为单次视频打标链路的稳定上下文（客户端/配置/分面/素材/帧数/提示词），收进结构体需同步改调用点，收益低。
#[allow(clippy::too_many_arguments)]
fn analyze_video_frames(
    client: &reqwest::blocking::Client,
    cfg: &ApiProfile,
    facets: &[FacetPromptContext],
    top_tags: &[(String, String)],
    asset: &assets::Asset,
    frame_count: usize,
    system_override: &str,
    min_confidence: f64,
    tier_cache: &Cell<TextJsonTier>,
    limiter: Option<&AiRateLimiter>,
    cancel: &AtomicBool,
) -> AppResult<MediaAnalysis> {
    let dir = std::env::temp_dir().join(format!(
        "bagertea_kframes_{}_{}",
        std::process::id(),
        asset.id
    ));
    std::fs::create_dir_all(&dir)?;
    let frames = super::video::extract_keyframes(
        std::path::Path::new(&asset.file_path),
        asset.duration_ms,
        &dir,
        frame_count,
    );
    if frames.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(AppError::msg(
            "视频抽帧失败：未安装 ffmpeg 或编码不支持（可在设置关闭视频打标）",
        ));
    }
    let mut results: Vec<MediaAnalysis> = Vec::new();
    let mut frame_failures: Vec<String> = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        match request_analysis(
            client,
            cfg,
            facets,
            top_tags,
            frame,
            system_override,
            min_confidence,
            false,
            tier_cache,
            limiter,
            cancel,
        ) {
            Ok(analysis) => results.push(analysis),
            Err(error) if is_ai_rate_limited(&error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
            Err(error) => frame_failures.push(format!("第 {} 帧识别失败：{error}", index + 1)),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if results.is_empty() {
        return Err(AppError::msg("视频全部帧 AI 识别失败"));
    }
    let merged = {
        let tag_frames: Vec<CategorizedTags> = results.iter().map(|a| a.tags.clone()).collect();
        merge_frame_tags(&tag_frames)
    };
    if merged.is_empty() && results.iter().all(|a| a.description.is_empty()) {
        return Err(AppError::msg(
            "视频帧标签未达命中阈值（帧数少时需多数帧共同命中）",
        ));
    }
    let proposals = merge_frame_proposals(&results, &merged);
    let mut warnings = Vec::new();
    if !frame_failures.is_empty() {
        warnings.push(format!(
            "视频有 {}/{} 帧识别失败，已按成功帧合并。",
            frame_failures.len(),
            frames.len()
        ));
        warnings.extend(frame_failures);
    }
    // §7.7：描述 = 时间上最接近视频中点的成功帧描述
    let description = results[results.len() / 2].description.clone();
    let people_presence = results[results.len() / 2].people_presence.clone();
    let raw_response = results
        .iter()
        .filter(|analysis| !analysis.raw_response.trim().is_empty())
        .map(|analysis| analysis.raw_response.as_str())
        .collect::<Vec<_>>()
        .join("\n--- frame ---\n");
    let request_config_json = results
        .iter()
        .find_map(|analysis| analysis.request_config_json.clone());
    let analysis_json = serde_json::to_string(&ai::AnalysisResult {
        description: description.clone(),
        people_presence: people_presence.clone(),
        proposals: proposals.clone(),
        numbers: Vec::new(),
        warnings: warnings.clone(),
    })
    .ok();
    Ok(MediaAnalysis {
        description,
        people_presence,
        tags: merged,
        proposals,
        numbers: Vec::new(),
        warnings,
        responded_tag_facets: results
            .iter()
            .flat_map(|a| a.responded_tag_facets.iter().cloned())
            .collect(),
        raw_response,
        request_config_json,
        analysis_json,
        blocked_low_confidence: results
            .iter()
            .map(|analysis| analysis.blocked_low_confidence)
            .sum(),
    })
}

/// 执行批次：逐条「读库 → 网络请求 → 写库」，进度回调 + 取消；
/// 每次 DB 操作短锁即用即放，网络等待期间不持锁，避免阻塞全应用其它 DB 读写
/// 在线与本机模型都按设置进行内存分块，不新增 chunk 表。
/// 单项失败重试前退避（秒）：指数退避首段
const RETRY_SECONDS: u64 = 1;

// 8 参数为云端打标批处理链路的稳定上下文（DB/批次/配置/分面/上限/取消/进度回调），收进结构体需同步改全部调用点，收益低。
#[allow(clippy::too_many_arguments)]
pub fn run_cloud_batch<F: Fn(AiProgress)>(
    db: &Arc<Mutex<Connection>>,
    batch_id: i64,
    cfg: &AiSettings,
    facets: &[FacetPromptContext],
    facets_video: &[FacetPromptContext],
    limit: Option<i64>,
    cancel: &Arc<AtomicBool>,
    progress: F,
) -> AppResult<()> {
    let lock = || db.lock().map_err(|_| AppError::msg("数据库锁中毒"));

    let profile = cfg
        .active()
        .ok_or_else(|| AppError::msg("请先在设置页添加 API 配置（中转站）"))?;
    if profile.base_url.trim().is_empty() {
        return Err(AppError::msg("当前 API 配置缺少 base_url"));
    }
    let managed_ollama = is_managed_ollama_profile(profile);
    // 限流只保护外部在线服务；Windows 应用托管的默认 Ollama 不受在线额度约束。
    // 限流器按连接 id 进程内共享，多个同时运行的批次会共同遵守同一组设置。
    let rate_limiter = if managed_ollama {
        None
    } else {
        ai_rate_limit::for_connection(
            &profile.id,
            profile.max_concurrency,
            profile.requests_per_minute,
            profile.requests_per_hour,
        )
    };
    // 外部 OpenAI 兼容服务可能是 localhost/局域网部署且不需要密钥；认证要求由服务
    // 自己返回 401/403。Anthropic 协议仍要求标准 API Key。
    if profile.api_mode == "anthropic" && profile.api_key.trim().is_empty() {
        return Err(AppError::msg("当前 Anthropic 配置缺少 API Key"));
    }
    let client = reqwest::blocking::Client::builder()
        // 连接 15s：本地服务没起来能快速报错；总超时 300s：
        // 本地大模型（7B+纯 CPU）单张响应可能远超 60s，60s 会误杀慢速打标
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| AppError::msg(format!("HTTP 客户端初始化失败: {e}")))?;

    {
        // 同一短锁内「检查 + 置位」，防止两次并发启动都通过预检
        let conn = lock()?;
        if ai::get_batch(&conn, batch_id)?.status == "processing" {
            return Err(AppError::msg("批次正在执行中"));
        }
        ai::set_batch_status(&conn, batch_id, "processing")?;
    }
    let suggestions = {
        let conn = lock()?;
        ai::list_suggestions(&conn, batch_id)?
    };
    // W5a（a2/a5）：Top-20 候选词（按使用次数降序 → 标签收敛）进 user 段提示词
    let top_tags: Vec<(String, String)> = {
        let conn = lock()?;
        crate::db::tags::top_tags_per_facet(&conn, 60).unwrap_or_default()
    };
    // v2.11：可选只处理前 N 张（其余保持 pending，可再次启动）
    // F15a：只处理真正未分析过的 pending。不能只看 suggested_tags 是否为空：
    // 低置信度全被拦截、纯描述结果都会留下空 tags，但已经有分析结果，不能再次请求。
    let pending: Vec<_> = suggestions
        .into_iter()
        .filter(|s| {
            s.status == "pending"
                && s.suggested_tags.is_empty()
                && s.suggested_description.trim().is_empty()
                && !s.has_analysis
        })
        .collect();
    let todo: Vec<_> = match limit {
        Some(n) => pending.into_iter().take(n.max(0) as usize).collect(),
        None => pending,
    };
    // 指导书 §8.2/§8.3：逻辑批次完整保留（用户所选全部素材都在批内），执行层本地分块。
    // 在线与本机服务分别使用自己的「每轮处理数量」；本机允许更小的轮次以适配小模型。
    // 并发 1；单项失败重试 1 次（指数退避），仍失败置 rejected。
    let chunk_size = if managed_ollama {
        (cfg.local_batch_limit as usize).clamp(1, 20)
    } else {
        (cfg.batch_limit as usize).clamp(10, 50)
    };
    let total = todo.len() as i64;
    // F15b（2026-08-22）：无待打标项（全部已处理/已确认/已拒绝）不再空转 done——
    // 明确报错；并先把批次状态复位，避免留下 processing 僵尸态
    // （命令层 ai_start_batch 已预检「无待打标项」；视频批次三检——开关/ffmpeg/本地视觉模型——
    //   也在命令层预检，此处为直接服务层调用与逐条兜底，见 :833 的 video_tagging 逐条校验）
    if todo.is_empty() {
        let conn = lock()?;
        ai::set_batch_status(&conn, batch_id, "done")?;
        tracing::warn!(
            operation = "ai_tagging",
            batch_id,
            stage = "no_pending",
            "AI 打标批次没有待处理项"
        );
        return Err(AppError::msg("当前没有待打标的建议（已全部处理或确认）"));
    }
    let min_confidence = cfg.confidence_min_suggest.clamp(0.0, 1.0);
    let tier_cache = Cell::new(TextJsonTier::Structured);
    let batch_started = std::time::Instant::now();
    tracing::info!(
        operation = "ai_tagging",
        batch_id,
        model = %profile.model,
        transport = vision_transport_name(vision_transport(profile)),
        managed_ollama,
        total,
        chunk_size,
        "AI 打标批次开始"
    );

    // 单条打标计算（网络请求不持 DB 锁）；失败由调用方决定重试/降级
    // F4：图片条目用 facets（all+image），视频条目用 facets_video（all+video）
    let compute = |asset: &crate::db::assets::Asset| -> AppResult<MediaAnalysis> {
        let is_video = asset.mime_type.starts_with("video/");
        let kind_facets: &[FacetPromptContext] = if is_video { facets_video } else { facets };
        if is_video && !cfg.video_tagging {
            return Err(AppError::msg(
                "视频 AI 打标未开启。请打开\"设置 → AI 设置 → 自动打标 → 视频 AI 打标\"，保存后重新开始批次。",
            ));
        }
        if is_video {
            // FB2-07（§13.4/§13.5）：cover 模式走与图片完全相同的 pick_image+request_analysis 路径
            //（零 ffmpeg、零额外解码、1 次请求）；frames 模式抽 N 段中点帧逐帧识别后合并（§7.7）。
            match cfg.video_tagging_mode.as_str() {
                "frames" => analyze_video_frames(
                    &client,
                    profile,
                    kind_facets,
                    &top_tags,
                    asset,
                    (cfg.video_frame_count as usize).clamp(2, 8),
                    &cfg.system_prompt_tagging,
                    min_confidence,
                    &tier_cache,
                    rate_limiter.as_deref(),
                    cancel,
                ),
                // cover（默认）：复用入库时生成的视频封面，needs 高清图优先
                _ => request_analysis(
                    &client,
                    profile,
                    kind_facets,
                    &top_tags,
                    &pick_image(asset),
                    &cfg.system_prompt_tagging,
                    min_confidence,
                    true,
                    &tier_cache,
                    rate_limiter.as_deref(),
                    cancel,
                ),
            }
        } else {
            // 网络请求（可能耗时数十秒）：不持 DB 锁
            request_analysis(
                &client,
                profile,
                kind_facets,
                &top_tags,
                &pick_image(asset),
                &cfg.system_prompt_tagging,
                min_confidence,
                true,
                &tier_cache,
                rate_limiter.as_deref(),
                cancel,
            )
        }
    };

    let mut processed = 0i64;
    let mut failed_assets = 0u32;
    // W5a（a12）：连续失败计数（成功清零；≥3 熔断）
    let mut consecutive_failures = 0u32;
    // A2：批次级请求溯源只写一次（取本批首个成功请求的配置；视频帧合并不携带配置，等待后续成功项）
    let mut batch_config_written = false;
    // 当前产品统一走人工审核：只拦截低于阈值的标签，其余全部等待用户确认写入。
    let policy = ai::ConfidencePolicy {
        min_suggest: min_confidence,
    };
    let mark_cancelled = |asset_id: i64, processed_count: i64| -> AppResult<()> {
        let conn = lock()?;
        ai::set_batch_status(&conn, batch_id, "cancelled")?;
        tracing::info!(
            operation = "ai_tagging",
            batch_id,
            asset_id,
            stage = "cancelled",
            processed = processed_count,
            total,
            "AI 打标批次已取消"
        );
        Ok(())
    };
    for chunk in todo.chunks(chunk_size) {
        for s in chunk {
            if cancel.load(Ordering::Relaxed) {
                mark_cancelled(s.asset_id, processed)?;
                return Ok(());
            }
            let asset = {
                let conn = lock()?;
                assets::get(&conn, s.asset_id)?
            };
            // 429/额度错误不能重试：继续发送只会放大供应商的限流压力。
            let mut tags = compute(&asset);
            // 限流等待可在网络请求期间收到取消；这条路径必须结束批次而不是把素材误标 rejected。
            if cancel.load(Ordering::Relaxed)
                || matches!(tags.as_ref(), Err(error) if error.code() == "CANCELLED")
            {
                mark_cancelled(s.asset_id, processed)?;
                return Ok(());
            }
            if matches!(tags.as_ref(), Err(error) if is_ai_rate_limited(error)) {
                let error = match tags {
                    Err(error) => error,
                    Ok(_) => unreachable!("限流分支必须来自 Err"),
                };
                let error_message = error.to_string();
                let conn = lock()?;
                // 保留 pending，修正限额后可直接续跑；前端从批次错误和 last_error 都能看到服务商原文。
                ai::set_suggestion_error(&conn, s.id, &error_message)?;
                ai::set_batch_status(&conn, batch_id, "interrupted")?;
                tracing::warn!(
                    operation = "ai_tagging",
                    batch_id,
                    asset_id = s.asset_id,
                    stage = "rate_limited",
                    processed,
                    total,
                    error_code = error.code(),
                    http_status = ?http_status_from_message(&error_message),
                    error = %error_message,
                    "AI 服务返回限流错误，停止批次且不再重试"
                );
                return Err(error);
            }
            if let Err(error) = &tags {
                let error_message = error.to_string();
                tracing::warn!(
                    operation = "ai_tagging",
                    batch_id,
                    asset_id = s.asset_id,
                    stage = "request_retry",
                    attempt = 1,
                    next_attempt = 2,
                    backoff_ms = RETRY_SECONDS * 1000,
                    error_code = error.code(),
                    http_status = ?http_status_from_message(&error_message),
                    error = %error_message,
                    "AI 打标首次请求失败，退避后重试"
                );
                if cancel.load(Ordering::Relaxed) {
                    mark_cancelled(s.asset_id, processed)?;
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(RETRY_SECONDS * 1000));
                tags = compute(&asset);
                if cancel.load(Ordering::Relaxed)
                    || matches!(tags.as_ref(), Err(error) if error.code() == "CANCELLED")
                {
                    mark_cancelled(s.asset_id, processed)?;
                    return Ok(());
                }
                // 重试本身也可能撞上供应商限流；仍按 429 语义立即停批，不能把它
                // 当作普通单条失败 reject 后继续消耗配额。
                if matches!(tags.as_ref(), Err(error) if is_ai_rate_limited(error)) {
                    let error = match tags {
                        Err(error) => error,
                        Ok(_) => unreachable!("限流分支必须来自 Err"),
                    };
                    let error_message = error.to_string();
                    let conn = lock()?;
                    ai::set_suggestion_error(&conn, s.id, &error_message)?;
                    ai::set_batch_status(&conn, batch_id, "interrupted")?;
                    tracing::warn!(
                        operation = "ai_tagging",
                        batch_id,
                        asset_id = s.asset_id,
                        stage = "rate_limited_after_retry",
                        processed,
                        total,
                        error_code = error.code(),
                        http_status = ?http_status_from_message(&error_message),
                        error = %error_message,
                        "AI 服务重试后返回限流错误，停止批次且不再继续请求"
                    );
                    return Err(error);
                }
            }
            {
                let conn = lock()?;
                match tags {
                    Ok(mut a) => {
                        // W5a（a12）：成功清零连续失败计数（单条内的退避重试不计入熔断）
                        consecutive_failures = 0;
                        // typed 写建议：低置信度在 DB 层拦截，其余全部保持 pending。
                        let blocked = ai::set_suggestion_result_policy(
                            &conn,
                            s.id,
                            &a.tags,
                            &a.proposals,
                            &a.description,
                            &policy,
                        )?;
                        a.blocked_low_confidence = blocked;
                        if blocked > 0 {
                            a.warnings
                                .push(format!("已拦截 {blocked} 个低置信度标签。"));
                            a.analysis_json = Some(
                                serde_json::to_string(&ai::AnalysisResult {
                                    description: a.description.clone(),
                                    people_presence: a.people_presence.clone(),
                                    proposals: a.proposals.clone(),
                                    numbers: a.numbers.clone(),
                                    warnings: a.warnings.clone(),
                                })
                                .unwrap_or_default(),
                            );
                        }
                        // V24（§6.4）：数值提议落库（歧义进 pending 待人工填数；越界/无数字丢弃 + warning）
                        let number_warnings =
                            ai::record_number_proposals_for_suggestion(&conn, s.id, &a.numbers)?;
                        for w in number_warnings {
                            tracing::info!(
                                operation = "ai_tagging",
                                batch_id,
                                asset_id = s.asset_id,
                                stage = "number_proposal",
                                warning = %w,
                                "AI 数值提议需要人工确认"
                            );
                        }
                        // 逐字存模型原始返回 + AnalysisResult V2 序列化。
                        ai::set_suggestion_provenance(
                            &conn,
                            s.id,
                            &a.raw_response,
                            a.analysis_json.as_deref().unwrap_or(""),
                            ANALYSIS_SCHEMA_VERSION,
                        )?;
                        // A2：批次级溯源（prompt 版本 + 配置 JSON + 稳定 hash + 档案标识）——只写一次
                        if !batch_config_written {
                            if let Some(rcj) = &a.request_config_json {
                                let cfg_val = serde_json::from_str::<serde_json::Value>(rcj)
                                    .unwrap_or_default();
                                let hash = stable_config_hash(&cfg_val);
                                ai::set_batch_provenance(
                                    &conn,
                                    batch_id,
                                    &profile.model,
                                    None,
                                    &profile.id,
                                    PROMPT_VERSION,
                                    &hash,
                                    rcj,
                                )?;
                                batch_config_written = true;
                            }
                        }
                    }
                    Err(e) => {
                        // 单条失败不阻塞批次：建议置 rejected 并记录空标签与失败原因（v6 详情落库）
                        let error_code = e.code();
                        let err = e.to_string();
                        failed_assets += 1;
                        tracing::warn!(
                            operation = "ai_tagging",
                            batch_id,
                            asset_id = s.asset_id,
                            stage = "final",
                            attempts = 2,
                            error_code,
                            http_status = ?http_status_from_message(&err),
                            error = %err,
                            "AI 打标最终失败"
                        );
                        let _ = ai::set_suggestion_error(&conn, s.id, &err);
                        ai::reject_suggestion(&conn, s.id)?;
                        // W5a（a12）：熔断 —— 连续失败 ≥3 说明是配置/网络级问题
                        //（key 无效/额度耗尽/断网），继续跑只会浪费请求与时间。
                        consecutive_failures += 1;
                        if consecutive_failures >= 3 {
                            ai::set_batch_status(&conn, batch_id, "interrupted")?;
                            tracing::warn!(
                                operation = "ai_tagging",
                                batch_id,
                                asset_id = s.asset_id,
                                stage = "interrupted",
                                processed,
                                total,
                                consecutive_failures,
                                error_code,
                                http_status = ?http_status_from_message(&err),
                                error = %err,
                                "连续失败达到阈值，AI 打标批次已中断"
                            );
                            return Err(AppError::msg(format!(
                                "连续 {consecutive_failures} 条打标失败，已中断批次（大概率是配置或网络问题，最近错误：{err}）。修复后可在打标页继续未完成的条目。"
                            )));
                        }
                    }
                }
                ai::inc_batch_processed(&conn, batch_id)?;
            }
            processed += 1;
            progress(AiProgress {
                batch_id,
                processed,
                total,
                current_asset_id: s.asset_id,
            });
        }
    }

    {
        let conn = lock()?;
        ai::set_batch_status(&conn, batch_id, "done")?;
    }
    tracing::info!(
        operation = "ai_tagging",
        batch_id,
        stage = "batch_done",
        processed,
        total,
        failed_assets,
        duration_ms = batch_started.elapsed().as_millis() as u64,
        "AI 打标批次完成"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{anthropic_fail_message, join_url, models_url, openai_fail_message};
    use super::{apply_keep_alive, extract_anthropic_text, parse_model_ids, KEEP_ALIVE_IDLE};
    use crate::db::settings::ApiProfile;
    use crate::db::tag_facets::FacetPromptContext;

    #[test]
    fn extracts_only_explicit_http_status_codes() {
        assert_eq!(
            super::http_status_from_message("视觉请求请求过于频繁（HTTP 429）"),
            Some(429)
        );
        assert_eq!(
            super::http_status_from_message("Anthropic 服务异常（http 503）"),
            Some(503)
        );
        assert_eq!(
            super::http_status_from_message("HTTP client init failed"),
            None
        );
        assert_eq!(super::http_status_from_message("HTTP 42"), None);
    }

    #[test]
    fn maps_http_status_to_stable_error_codes() {
        let rate_limited = super::ai_http_status_error("视觉请求", 429);
        assert_eq!(rate_limited.code(), "AI_RATE_LIMITED");
        assert!(!rate_limited.to_string().contains("response body"));

        assert_eq!(
            super::ai_http_status_error("视觉请求", 401).code(),
            "UNAUTHORIZED"
        );
        assert_eq!(
            super::ai_http_status_error("视觉请求", 408).code(),
            "TIMEOUT"
        );
        assert_eq!(
            super::ai_http_status_error("视觉请求", 503).code(),
            "INTERNAL"
        );
    }

    #[test]
    fn rate_limit_error_keeps_safe_provider_reason() {
        let error = super::ai_http_status_error_with_body(
            "视觉请求",
            429,
            r#"{"error":{"type":"rate_limit_exceeded","message":"RPM limit exceeded for free users"}}"#,
        );
        assert_eq!(error.code(), "AI_RATE_LIMITED");
        assert!(error.to_string().contains("RPM limit exceeded"));

        let secret = super::ai_http_status_error_with_body(
            "视觉请求",
            429,
            r#"{"error":{"message":"rate limit","api_key":"sk-secret"}}"#,
        );
        assert!(!secret.to_string().contains("sk-secret"));
    }

    #[test]
    fn strict_garbage_is_err() {
        assert!(super::parse_media_analysis("我无法查看这张图片", &[], 0.30).is_err());
    }

    #[test]
    fn strict_valid_object_ok() {
        let facets = vec![FacetPromptContext {
            key: "scene".into(),
            display_name: "场景/地点".into(),
            selection_mode: "multi".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        }];
        let r = super::parse_media_analysis(
            r#"{"description":"公园里树木茂盛","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"scene":[{"name":"公园","confidence":0.9}]}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(r.tags.get("scene").unwrap(), &vec!["公园".to_string()]);
    }

    fn prompt_facets() -> Vec<FacetPromptContext> {
        vec![FacetPromptContext {
            key: "people".into(),
            display_name: "人物属性".into(),
            selection_mode: "multi".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        }]
    }

    #[test]
    fn user_prompt_hides_custom_and_states_people_consistency() {
        let mut facets = prompt_facets();
        facets.push(FacetPromptContext {
            key: "custom".into(),
            display_name: "自定义".into(),
            selection_mode: "multi".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        });
        let prompt = super::build_user_prompt(&facets, &[]);
        assert!(!prompt.contains("key: custom"));
        assert!(prompt.contains("peoplePresence 必须是 present"));
        assert!(prompt.contains("绝不能写无人"));
    }

    // §8.4：本地请求体注入 keep_alive；云端不注入
    #[test]
    fn keep_alive_injected_only_for_local() {
        let mut local = serde_json::json!({ "model": "qwen3.5:4b" });
        apply_keep_alive(&mut local, true);
        assert_eq!(local["keep_alive"], KEEP_ALIVE_IDLE);

        let mut cloud = serde_json::json!({ "model": "gpt-4o" });
        apply_keep_alive(&mut cloud, false);
        assert!(cloud.get("keep_alive").is_none());
    }

    #[test]
    fn fallback_only_accepts_explicit_format_compatibility_errors() {
        assert!(super::vision_request_should_fallback(
            "服务返回 HTTP 400：response_format json_schema unsupported"
        ));
        assert!(!super::vision_request_should_fallback(
            "视觉请求请求过于频繁（HTTP 429）：RPM limit exceeded"
        ));
        assert!(!super::vision_request_should_fallback(
            "服务返回 HTTP 400：context length exceeded"
        ));
        assert!(!super::vision_request_should_fallback(
            "服务返回 HTTP 401：unauthorized"
        ));
        assert!(super::vision_request_should_fallback(
            "Ollama unknown field: think"
        ));
        assert!(super::vision_request_should_fallback("响应格式不支持"));
        assert!(!super::vision_request_should_fallback(
            "model does not support image input"
        ));
    }

    #[test]
    fn context_overflow_is_classified_without_model_name_heuristics() {
        assert!(super::is_context_overflow_message(
            "prompt is too long: context length exceeded"
        ));
        assert!(super::is_context_overflow_message("输入上下文过长"));
        assert!(!super::is_context_overflow_message("invalid image format"));
    }

    #[test]
    fn safe_http_detail_keeps_context_diagnostics_without_secrets() {
        let error = super::ai_http_status_error_with_body(
            "视觉请求",
            400,
            r#"{"error":{"message":"prompt is too long; context length 4197 > 4096"}}"#,
        );
        assert!(super::is_context_overflow_message(&error.to_string()));
        let secret = super::ai_http_status_error_with_body(
            "视觉请求",
            400,
            r#"{"error":{"message":"bad api_key sk-secret"}}"#,
        );
        assert!(!secret.to_string().contains("sk-secret"));
    }

    #[test]
    fn localhost_external_profile_never_becomes_managed_ollama() {
        let profile = ApiProfile {
            id: "external-localhost".into(),
            name: "LM Studio".into(),
            api_mode: "openai".into(),
            kind: "local".into(),
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key: String::new(),
            model: "vision-model".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        };
        assert!(!super::is_managed_ollama_profile(&profile));
        assert_eq!(
            super::vision_transport(&profile),
            super::VisionTransport::ExternalOpenAi
        );
    }

    #[test]
    fn empty_ollama_final_content_reports_thinking_exhaustion() {
        let response = serde_json::json!({
            "message": { "content": "", "thinking": "只生成了思考轨迹" },
            "done": true,
            "done_reason": "length",
            "eval_count": 800
        });
        let error = super::ollama_message_content(&response).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("未返回最终内容"));
        assert!(message.contains("done_reason=length"));
        assert!(message.contains("thinking_chars=8"));
        assert!(message.contains("eval_count=800"));
        assert!(!message.contains("只生成了思考轨迹"), "不得回显思考原文");
    }

    // §9.3：本地模型视觉能力启发式（含视觉标记 → 支持；已知纯文本 → 不支持；未知 → 支持防误拦）
    #[test]
    fn vision_model_heuristic() {
        assert!(super::model_supports_vision("qwen3.5:4b")); // 新版 Qwen3.5 多模态
        assert!(super::model_supports_vision("llava:13b"));
        assert!(super::model_supports_vision("moondream:2b"));
        assert!(super::model_supports_vision("gemma3:4b")); // 多模态
        assert!(!super::model_supports_vision("qwen3:4b")); // 纯文本
        assert!(!super::model_supports_vision("deepseek-r1:7b"));
        assert!(super::model_supports_vision("有些自定义视觉模型")); // 未知按支持，避免误拦
        assert!(super::model_supports_vision("")); // 空模型不拦
    }

    // ── FB3-08：连接测试的纯函数部分（网络路径在真机验收） ──

    #[test]
    fn join_url_trims_trailing_slash() {
        // 用户地址带不带尾斜杠、带不带 /v1 都拼出正确路径
        assert_eq!(
            join_url("https://api.example.com/v1", "/models"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            join_url("https://api.example.com/v1/", "/models"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            join_url("http://localhost:11434/v1", "/models"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            join_url("https://api.example.com", "/messages"),
            "https://api.example.com/messages"
        );
    }

    #[test]
    fn openai_fail_messages_are_actionable() {
        let m401 = openai_fail_message(401, false);
        assert!(m401.contains("密钥"), "401 应指向密钥问题：{m401}");
        let m404 = openai_fail_message(404, false);
        assert!(m404.contains("路径"), "404 应指向路径问题：{m404}");
        let local = openai_fail_message(500, true);
        assert!(
            local.contains("本地"),
            "本地部署的失败信息应指向引擎/端口：{local}"
        );
    }

    #[test]
    fn anthropic_fail_messages_are_actionable() {
        assert!(anthropic_fail_message(401).contains("密钥"));
        assert!(anthropic_fail_message(404).contains("路径"));
    }

    #[test]
    fn anthropic_text_extracted() {
        let v = serde_json::json!({"content": [
            {"type": "thinking", "thinking": "..."},
            {"type": "text", "text": "[\"人像\"]"}
        ]});
        assert_eq!(extract_anthropic_text(&v).as_deref(), Some("[\"人像\"]"));
    }

    #[test]
    fn anthropic_no_text_block_gives_none() {
        assert!(extract_anthropic_text(&serde_json::json!({"content": []})).is_none());
    }

    #[test]
    fn parse_openai_models() {
        let v = serde_json::json!({"object": "list", "data": [
            {"id": "qwen-vl-plus"}, {"id": "qwen-vl-max"}, {"no_id": true}
        ]});
        assert_eq!(parse_model_ids(&v), vec!["qwen-vl-plus", "qwen-vl-max"]);
    }

    #[test]
    fn parse_bad_payload_gives_empty() {
        assert!(parse_model_ids(&serde_json::json!({"error": "unauthorized"})).is_empty());
    }

    // ── FB5-04（§13.5）：模型发现 —— 三种响应形态 / 去重排序 / URL 不重复 /models ──

    #[test]
    fn parse_model_ids_three_shapes() {
        // 1. OpenAI 兼容 {data:[{id}]}
        let data = serde_json::json!({"data": [{"id": "a"}, {"id": "b"}]});
        assert_eq!(parse_model_ids(&data), vec!["a", "b"]);
        // 2. 扁平 {models:["a","b"]}
        let flat = serde_json::json!({"models": ["a", "b"]});
        assert_eq!(parse_model_ids(&flat), vec!["a", "b"]);
        // 3. 顶层数组
        let top = serde_json::json!(["a", "b"]);
        assert_eq!(parse_model_ids(&top), vec!["a", "b"]);
        // 空/坏数据 → 空
        assert!(parse_model_ids(&serde_json::json!({})).is_empty());
        assert!(parse_model_ids(&serde_json::json!([])).is_empty());
    }

    #[test]
    fn models_url_never_duplicates() {
        assert_eq!(
            models_url("https://api.example.com/v1"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            models_url("https://api.example.com/v1/"),
            "https://api.example.com/v1/models"
        );
        // 已以 /models 结尾：不重复拼接（§13.5「URL 不重复 /models」）
        assert_eq!(
            models_url("https://api.example.com/v1/models"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            models_url("https://api.example.com/v1/models/"),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            models_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/models"
        );
    }

    #[test]
    fn discover_models_dedupes_and_sorts_ci() {
        // 无法发真实网络请求：通过 parse_model_ids + 排序/去重纯逻辑等价验证（网络路径在真机验收）
        let raw = serde_json::json!({"data": [
            {"id": "qwen-VL-Max"}, {"id": "qwen-vl-max"}, {"id": "gpt-4.1"}, {"id": "gpt-4.1"}
        ]});
        let mut models = parse_model_ids(&raw);
        models.sort_by_key(|a| a.to_lowercase());
        models.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
        // 不区分大小写排序 + 去重：大小写变体只保留输入顺序中的第一个
        assert_eq!(models, vec!["gpt-4.1", "qwen-VL-Max"]);
    }

    #[test]
    fn merge_frames_keeps_majority_tags() {
        use std::collections::BTreeMap;
        let f = |pairs: &[(&str, &[&str])]| -> super::CategorizedTags {
            let mut m = BTreeMap::new();
            for (k, v) in pairs {
                m.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
            }
            m
        };
        let frames = vec![
            f(&[("场景", &["公园", "街道"])]),
            f(&[("场景", &["公园"]), ("光线", &["逆光"])]),
            f(&[("场景", &["公园", "海边"])]),
        ];
        let r = super::merge_frame_tags(&frames);
        assert_eq!(r.get("场景").unwrap(), &vec!["公园".to_string()]); // 3 帧命中
        assert!(!r.contains_key("光线")); // 仅 1 帧，不达阈值
    }

    #[test]
    fn merge_frames_keeps_proposal_confidence() {
        let frame = |name: &str, confidence: f32| -> super::MediaAnalysis {
            let tags =
                super::CategorizedTags::from([("scene".to_string(), vec![name.to_string()])]);
            super::MediaAnalysis {
                tags,
                proposals: vec![crate::db::ai::TagProposal {
                    facet_key: "scene".into(),
                    raw_name: name.into(),
                    confidence: Some(confidence),
                }],
                ..Default::default()
            }
        };
        let frames = vec![frame("公园", 0.8), frame("公园", 0.6), frame("海边", 0.9)];
        let merged = super::merge_frame_tags(
            &frames
                .iter()
                .map(|analysis| analysis.tags.clone())
                .collect::<Vec<_>>(),
        );
        let proposals = super::merge_frame_proposals(&frames, &merged);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].raw_name, "公园");
        let confidence = proposals[0].confidence.unwrap();
        assert!((confidence - 0.7).abs() < 0.0001);
    }

    #[test]
    fn merge_single_frame_keeps_all() {
        use std::collections::BTreeMap;
        let mut m = BTreeMap::new();
        m.insert("场景".to_string(), vec!["室内".to_string()]);
        let r = super::merge_frame_tags(&[m]);
        assert_eq!(r.get("场景").unwrap(), &vec!["室内".to_string()]);
        assert!(super::merge_frame_tags(&[]).is_empty());
    }

    #[test]
    fn degenerate_repeated_char_detected() {
        assert!(super::is_degenerate("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@"));
        assert!(super::is_degenerate("!!!!!!!!!!!!!!!!!!!!!!!!!!!!"));
        assert!(!super::is_degenerate("@@"));
        assert!(!super::is_degenerate(""));
    }

    #[test]
    fn degenerate_mojibake_detected() {
        // UTF-8 中文被按 Latin-1 误读的典型乱码（ollama 分词器字节错切症状，实测样本）
        assert!(super::is_degenerate("å¯¹ä¸èµ·ï¼ææ æ³å¸®å©æ¨è§£è¯»å¾åå®¹ã"));
        assert!(super::is_degenerate("ä»¥ä¸æ¯æ´çå¥½å¹¶è§æ ¼åçJSONæ ¼å¼ï¼"));
    }

    #[test]
    fn normal_replies_not_degenerate() {
        assert!(!super::is_degenerate("这张图片是一张纯红色的图片。"));
        assert!(!super::is_degenerate("{\"场景\": [\"公园\"]}"));
        assert!(!super::is_degenerate(
            "This image shows a bridge in a city."
        ));
        // 中文标点+ASCII 混排不误报
        assert!(!super::is_degenerate("1 + 1 = 2"));
    }

    // ── FB5-05（§7.5）：一句话描述规范化 ──

    #[test]
    fn normalize_description_trims_whitespace_and_folds() {
        assert_eq!(
            super::normalize_content_description("  夜晚树下多人合影  "),
            "夜晚树下多人合影"
        );
        assert_eq!(
            super::normalize_content_description("夜晚树下\n多人合影"),
            "夜晚树下 多人合影"
        );
        assert_eq!(
            super::normalize_content_description("夜晚树下\t\t多人  合影"),
            "夜晚树下 多人 合影"
        );
        assert_eq!(super::normalize_content_description(""), "");
        assert_eq!(super::normalize_content_description("   "), "");
    }

    // ── FB5-05（§7.4）：MediaAnalysis 解析 ──

    #[test]
    fn parse_media_analysis_new_protocol() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                max_items: Some(3),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people".into(),
                max_items: Some(8),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"这是一张夜晚树下多人合影。","peoplePresence":{"status":"present","confidence":0.95},"tags":{"subject":[{"name":"树","confidence":0.9}],"people":[{"name":"多人","confidence":0.9}]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(a.description, "夜晚树下多人合影");
        assert_eq!(
            a.tags.get("subject").unwrap(),
            &vec!["人".to_string(), "树".to_string()]
        );
        assert_eq!(a.tags.get("people").unwrap(), &vec!["多人".to_string()]);
    }

    #[test]
    fn parse_media_analysis_keeps_people_attrs_atomic() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                max_items: Some(3),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people".into(),
                max_items: Some(8),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"男女老少在公园散步交谈","peoplePresence":{"status":"present","confidence":0.95},"tags":{"subject":[{"name":"女子","confidence":0.9},{"name":"树","confidence":0.9},{"name":"建筑","confidence":0.8}],"people":[{"name":"男女","confidence":0.9},{"name":"老人","confidence":0.9},{"name":"古装","confidence":0.9}]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(
            a.tags.get("subject").unwrap(),
            &vec!["人".to_string(), "树".to_string(), "建筑".to_string()]
        );
        assert_eq!(
            a.tags.get("people").unwrap(),
            &vec![
                "男性".to_string(),
                "女性".to_string(),
                "老年".to_string(),
                "古装".to_string()
            ]
        );
    }

    #[test]
    fn parse_media_analysis_absent_people_removes_human_subject() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                max_items: Some(3),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people".into(),
                max_items: Some(8),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"公园里只有树木和长椅","peoplePresence":{"status":"absent","confidence":0.95},"tags":{"subject":[{"name":"女子","confidence":0.9},{"name":"树","confidence":0.9}],"people":[{"name":"女性","confidence":0.9}]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(a.tags.get("subject").unwrap(), &vec!["树".to_string()]);
        assert_eq!(a.tags.get("people").unwrap(), &vec!["无人".to_string()]);
    }

    #[test]
    fn parse_media_analysis_keeps_new_people_word() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                max_items: Some(3),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people".into(),
                max_items: Some(8),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"赛博朋克少女站在霓虹街道","peoplePresence":{"status":"present","confidence":0.95},"tags":{"subject":[{"name":"人","confidence":0.9}],"people":[{"name":"赛博朋克少女","confidence":0.7}]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(
            a.tags.get("people").unwrap(),
            &vec!["赛博朋克少女".to_string()]
        );
    }

    /// V24（§6.4）：numbers 对象 → NumberProposal（原文原样）；数值分面 key 不得留在 tags。
    #[test]
    fn parse_media_analysis_extracts_numbers_object() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people_count".into(),
                display_name: "人数".into(),
                selection_mode: "single".into(),
                facet_kind: "number".into(),
                num_min: Some(0.0),
                num_max: Some(50.0),
                num_unit: "人".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"树下五人合影","peoplePresence":{"status":"present","confidence":0.9},"tags":{"subject":[{"name":"树","confidence":0.9}]},"numbers":{"people_count":"5人"}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(
            a.numbers.len(),
            1,
            "numbers 提议应收集 1 条：{:?}",
            a.numbers
        );
        assert_eq!(a.numbers[0].facet_key, "people_count");
        assert_eq!(a.numbers[0].raw_text, "5人");
        assert!(
            !a.tags.contains_key("people_count"),
            "数值分面不得留在 tags"
        );
    }

    #[test]
    fn parse_media_analysis_resolves_number_display_name() {
        let facets = vec![FacetPromptContext {
            key: "people_count".into(),
            display_name: "人数".into(),
            selection_mode: "single".into(),
            facet_kind: "number".into(),
            ..Default::default()
        }];
        let a = super::parse_media_analysis(
            r#"{"description":"两人合影","peoplePresence":{"status":"present","confidence":0.9},"tags":{},"numbers":{"人数":"2"}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(a.numbers.len(), 1);
        assert_eq!(a.numbers[0].facet_key, "people_count");
        assert_eq!(a.numbers[0].raw_text, "2");
    }

    #[test]
    fn completeness_distinguishes_empty_array_from_missing_key() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "scene".into(),
                display_name: "场景".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let a = super::parse_media_analysis(
            r#"{"description":"一棵树生长在公园里","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"subject":[{"name":"树","confidence":0.9}],"scene":[]}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert!(a.responded_tag_facets.contains("scene"));
        assert!(super::missing_tag_facet_keys(&a, &facets).is_empty());

        let b = super::parse_media_analysis(
            r#"{"description":"一棵树生长在公园里","peoplePresence":{"status":"unknown","confidence":0.8},"tags":{"subject":[{"name":"树","confidence":0.9}]}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(super::missing_tag_facet_keys(&b, &facets), vec!["scene"]);
    }

    /// V2：数值分面误写进 tags 视为协议错误，触发修复。
    #[test]
    fn parse_media_analysis_number_in_tags_is_rejected() {
        let facets = vec![FacetPromptContext {
            key: "people_count".into(),
            display_name: "人数".into(),
            description: String::new(),
            selection_mode: "single".into(),
            max_items: None,
            facet_kind: "number".into(),
            num_min: None,
            num_max: None,
            num_unit: String::new(),
        }];
        let error = super::parse_media_analysis(
            r#"{"description":"五人合影","peoplePresence":{"status":"present","confidence":0.9},"tags":{"people_count":[{"name":"5","confidence":0.9}]}}"#,
            &facets,
            0.30,
        )
        .unwrap_err();
        assert!(error.to_string().contains("不允许出现在 tags"));
    }

    #[test]
    fn parse_media_analysis_description_only_is_valid() {
        let facets = vec![FacetPromptContext {
            key: "subject".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        }];
        let a = super::parse_media_analysis(
            r#"{"description":"纯红色背景没有其他物体","peoplePresence":{"status":"absent","confidence":0.9},"tags":{"subject":[]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap();
        assert_eq!(a.description, "纯红色背景没有其他物体");
        assert!(a.tags.is_empty());
    }

    #[test]
    fn parse_media_analysis_both_empty_fails() {
        let facets = vec![FacetPromptContext {
            key: "subject".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        }];
        let err = super::parse_media_analysis(
            r#"{"description":"","peoplePresence":{"status":"unknown","confidence":0.3},"tags":{"subject":[]},"numbers":{}}"#,
            &facets,
            0.30,
        )
        .unwrap_err();
        assert!(err.to_string().contains("未返回可用描述或标签"));
        let err2 = super::parse_media_analysis("not json", &facets, 0.30).unwrap_err();
        assert!(err2.to_string().contains("未返回打标 V2 JSON"));
    }

    // FX-02：模型回复包 ```json 围栏（gemma3 等本地模型实测形态）。
    // 此前整串解析失败 → 顶层键 "description" 被当成分面 key，
    // 描述整句落 custom、真实 tags 全丢（线上「只打出 custom 标」第二层根因）。
    #[test]
    fn parse_media_analysis_fenced_json_new_protocol() {
        let facets = vec![
            FacetPromptContext {
                key: "subject".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "scene".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
            FacetPromptContext {
                key: "people".into(),
                facet_kind: "tag".into(),
                ..Default::default()
            },
        ];
        let raw = "```json\n{\"description\":\"女孩独自站在街道旁\",\"peoplePresence\":{\"status\":\"present\",\"confidence\":0.95},\"tags\":{\"scene\":[{\"name\":\"街道\",\"confidence\":0.9}],\"people\":[{\"name\":\"女性\",\"confidence\":0.9}],\"subject\":[{\"name\":\"女孩\",\"confidence\":0.9}]},\"numbers\":{}}\n```";
        let a = super::parse_media_analysis(raw, &facets, 0.30).unwrap();
        assert_eq!(a.description, "女孩独自站在街道旁");
        assert_eq!(a.tags.get("scene").unwrap(), &vec!["街道".to_string()]);
        assert_eq!(a.tags.get("people").unwrap(), &vec!["女性".to_string()]);
    }

    #[test]
    fn parse_media_analysis_fenced_with_prose_around() {
        let facets = vec![FacetPromptContext {
            key: "subject".into(),
            facet_kind: "tag".into(),
            ..Default::default()
        }];
        let raw = "好的，以下是整理好的并符合规格的JSON格式：\n```json\n{\"description\":\"夜晚树下自拍\",\"peoplePresence\":{\"status\":\"present\",\"confidence\":0.9},\"tags\":{\"subject\":[{\"name\":\"树\",\"confidence\":0.9}]},\"numbers\":{}}\n```";
        let a = super::parse_media_analysis(raw, &facets, 0.30).unwrap();
        assert_eq!(a.description, "夜晚树下自拍");
        assert_eq!(
            a.tags.get("subject").unwrap(),
            &vec!["人".to_string(), "树".to_string()]
        );
    }

    #[test]
    fn normalize_description_strips_fillers_and_sentence() {
        // 开头套话去除
        assert_eq!(
            super::normalize_content_description("这是一张夜晚树下多人合影。"),
            "夜晚树下多人合影"
        );
        assert_eq!(
            super::normalize_content_description("这张图片展示夜晚的海滩！"),
            "夜晚的海滩"
        );
        assert_eq!(
            super::normalize_content_description("画面中有树和多人，氛围温馨。"),
            "树和多人，氛围温馨"
        );
        // 只取第一个句段（逗号不在截断范围，但句号截断）
        assert_eq!(
            super::normalize_content_description("夜晚树下多人合影，氛围温馨。整体色调偏冷。"),
            "夜晚树下多人合影，氛围温馨"
        );
        // 无套话/无标点原样保留
        assert_eq!(
            super::normalize_content_description("夜晚树下多人合影"),
            "夜晚树下多人合影"
        );
    }

    #[test]
    fn normalize_description_truncates_to_30_unicode_chars() {
        let long = "一个阳光明媚的海边沙滩上人们正在散步的场景十分美好";
        assert_eq!(long.chars().count(), 25);
        let out = super::normalize_content_description(long);
        assert_eq!(out.chars().count(), 25, "未超过 30 字不得截断：{out}");
        let longer = "一个阳光明媚的海边沙滩上人们正在散步聊天的场景十分美好且热闹非凡";
        assert!(longer.chars().count() > 30);
        let truncated = super::normalize_content_description(longer);
        assert_eq!(truncated.chars().count(), 30);
        // 表情符号（多字节）也不被切坏
        let emoji = "🌅海边日落很美很浪漫的景色";
        let out2 = super::normalize_content_description(emoji);
        assert!(out2.chars().count() <= 30);
        assert!(!out2.is_empty());
    }

    #[test]
    fn system_prompt_limits_inference_and_preserves_json_contract() {
        let prompt = super::build_system_prompt();
        assert!(prompt.contains("不要推测素材用途"));
        assert!(prompt.contains("无法确认时返回空数组"));
        assert!(prompt.contains("12–30 个字符"));
        assert!(prompt.contains("peoplePresence"));
        assert!(prompt.contains("\"description\""));
        assert!(prompt.contains("\"tags\""));
    }
}
