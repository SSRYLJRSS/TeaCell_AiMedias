//! 设置：SQLite 键值表存储，整体 JSON 读写（架构 §5.6：首版本地明文）

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

/// 内部固定的机器协议补充；不承载可配置的分类业务语义。
pub const DEFAULT_TAGGING_PROMPT: &str = "严格遵守本次请求提供的分类、分类 key、选项数量与数值配置；仅输出规定 JSON 结构，不新增字段；分类业务含义以各分类说明为准；不得输出请求未提供的分类 key。";

/// AI 分面配置（P1B：tag_facets 是唯一事实源，设置只保存 facetKey/hint/enabledForAi/displayName）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFacetConfig {
    pub facet_key: String,
    #[serde(default)]
    pub hint: String,
    /// 是否参与 AI 打标与 AI 搜索提示词
    #[serde(default = "default_enabled_for_ai")]
    pub enabled_for_ai: bool,
    /// 可选本地化显示名；为空时用 tag_facets.display_name
    #[serde(default)]
    pub display_name: Option<String>,
    /// 是否显示在人工打标工作台（独立于 enabled_for_ai）。
    /// None = 未显式设置（前端按 WORKBENCH_DEFAULT_KEYS 决定默认显示；缺省不序列化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_in_workbench: Option<bool>,
}

fn default_enabled_for_ai() -> bool {
    true
}

/// 一套 API 配置档案（一个中转站/服务商）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProfile {
    pub id: String,
    pub name: String,
    /// 接口协议模式：openai（/chat/completions）| anthropic（/messages）
    #[serde(default = "default_api_mode")]
    pub api_mode: String,
    /// 部署类型（P3-01a）：cloud（云端服务商）| local（本机 OpenAI 兼容服务，如 Ollama/LM Studio）；
    /// serde 默认 cloud，旧数据零感知
    #[serde(default = "default_profile_kind")]
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// 在线连接限流；0 表示不限。Windows 应用托管 Ollama 忽略这些值。
    #[serde(default)]
    pub max_concurrency: i64,
    #[serde(default)]
    pub requests_per_minute: i64,
    #[serde(default)]
    pub requests_per_hour: i64,
}

impl ApiProfile {
    pub fn is_local(&self) -> bool {
        self.kind == "local"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    /// 多套 API 配置（中转站），打标只走激活那套
    #[serde(default)]
    pub profiles: Vec<ApiProfile>,
    /// 当前激活档案 id
    #[serde(default)]
    pub active_profile: String,
    // ---- 以下为旧版扁平字段：仅作迁移输入，不再序列化 ----
    #[serde(default, skip_serializing)]
    pub api_mode: String,
    #[serde(default, skip_serializing)]
    pub base_url: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
    #[serde(default = "default_model", skip_serializing)]
    pub model: String,
    // W0-6：auto_tagging / local_model_tier 全库零消费点（老 JSON 反序列化兼容保留，
    // 不再序列化；下个大版本再彻底移除）。替代品见 W5g「一键送打标」。
    #[serde(default, skip_serializing)]
    pub auto_tagging: bool,
    #[serde(default)]
    pub video_tagging: bool,
    /// FB2-07：视频打标模式（cover 复用封面 / frames 抽帧）；默认 cover（决策 5）
    #[serde(default = "default_video_tagging_mode")]
    pub video_tagging_mode: String,
    /// FB2-07：frames 模式抽帧数（2~8），默认 3
    #[serde(default = "default_video_frame_count")]
    pub video_frame_count: i64,
    #[serde(default = "default_tier", skip_serializing)]
    pub local_model_tier: String,
    #[serde(default = "default_batch_limit")]
    pub batch_limit: i64,
    /// 本机服务每轮处理数量；独立于在线服务，适合能力较小的本地模型。
    #[serde(default = "default_local_batch_limit")]
    pub local_batch_limit: i64,
    /// 一键安装的下载源偏好（"auto" = 测速选最快；旧数据缺省视为 auto）
    #[serde(default = "default_ollama_source_id")]
    pub ollama_source_id: String,
    /// 旧版全局文本字段：只读兼容并强制归一为内部协议，不再返回/持久化。
    #[serde(default = "default_tagging_prompt", skip_serializing)]
    pub system_prompt_tagging: String,
    /// 旧版覆盖字段：只读兼容，运行时忽略且不再返回/持久化。
    #[serde(default, skip_serializing)]
    pub system_prompt_search: String,
    /// AI 建议最低置信度阈值：confidence < 此值不入库（连 pending 都不进）；默认 0.30
    #[serde(default = "default_conf_min_suggest")]
    pub confidence_min_suggest: f64,
}

impl AiSettings {
    /// 旧版扁平配置迁移：无档案但有 base_url/api_key 时合成「默认配置」
    pub fn normalize(&mut self) {
        if self.profiles.is_empty() && (!self.base_url.is_empty() || !self.api_key.is_empty()) {
            self.profiles.push(ApiProfile {
                id: "default".into(),
                name: "默认配置".into(),
                api_mode: if self.api_mode.is_empty() {
                    default_api_mode()
                } else {
                    self.api_mode.clone()
                },
                kind: default_profile_kind(),
                base_url: self.base_url.clone(),
                api_key: self.api_key.clone(),
                model: self.model.clone(),
                max_concurrency: 0,
                requests_per_minute: 0,
                requests_per_hour: 0,
            });
            self.active_profile = "default".into();
        }
        // FB2-07：视频打标模式与帧数校验
        if self.video_tagging_mode != "frames" {
            self.video_tagging_mode = "cover".into();
        }
        // 旧版本允许自定义全局提示词。新契约只允许编辑分类说明，机器协议固定。
        self.system_prompt_tagging = default_tagging_prompt();
        self.system_prompt_search.clear();
        self.video_frame_count = self.video_frame_count.clamp(2, 8);
        // FB3-07：云端子批大小收敛到运行时实际范围 [10,50]（ai_cloud 执行层 clamp 同值）。
        // 历史 500（v2.5 胶片条方案遗留）运行时永远被 clamp 到 50，用户看到的值永不生效——
        // 读取时归一到 30（新默认），避免「显示 500 实际 50」的假象。
        if self.batch_limit > 50 || self.batch_limit < 10 {
            self.batch_limit = default_batch_limit();
        }
        if self.local_batch_limit > 20 || self.local_batch_limit < 1 {
            self.local_batch_limit = default_local_batch_limit();
        }
    }

    /// 当前激活档案（找不到时回退第一套）
    pub fn active(&self) -> Option<&ApiProfile> {
        self.profiles
            .iter()
            .find(|p| p.id == self.active_profile)
            .or(self.profiles.first())
    }

    /// 当前激活档案 id（回退第一套的地址；供连接迁移/回退判断）
    pub fn active_profile_opt(&self) -> Option<String> {
        if self.active_profile.is_empty() {
            None
        } else {
            Some(self.active_profile.clone())
        }
    }
}

fn default_api_mode() -> String {
    "openai".into()
}
fn default_conf_min_suggest() -> f64 {
    0.30
}
fn default_ollama_source_id() -> String {
    "auto".into()
}
fn default_custom_sources() -> Vec<CustomSource> {
    Vec::new()
}
fn default_profile_kind() -> String {
    "cloud".into()
}
fn default_model() -> String {
    "qwen-vl-plus".into()
}
fn default_tier() -> String {
    "light".into()
}
fn default_video_tagging_mode() -> String {
    "cover".into()
}
fn default_video_frame_count() -> i64 {
    3
}
fn default_batch_limit() -> i64 {
    30 // FB3-07：云端每轮处理数量；运行时 clamp [10,50]，默认 30（旧 500 永不生效已归一）
}
fn default_local_batch_limit() -> i64 {
    5
}

/// 标签分类（PRD 5.5）：分类=父标签；hint 参与 AI 提示词，single 控制单/多选
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCategory {
    pub name: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub single: bool,
    /// 每类标签数量上限（v2.11，写入 AI 提示词）
    #[serde(default = "default_category_max")]
    pub max: i64,
}

fn default_category_max() -> i64 {
    3
}

pub fn default_tag_categories() -> Vec<TagCategory> {
    [
        ("场景", "如公园/街道/室内，选最主要的一个", true),
        ("色彩", "主色、色调与色彩关系，如青橙/暗调/冷调", false),
        ("人物", "人物数量、年龄段、动作姿态，无人物则留空", false),
        ("物体", "画面中的关键物体", false),
        ("构图视角", "如特写/全景/俯拍/对称", true),
        ("光线", "只描述光线方向与质感，如逆光/柔光/黄昏金调", false),
    ]
    .into_iter()
    .map(|(name, hint, single)| TagCategory {
        name: name.into(),
        hint: hint.into(),
        single,
        max: if single { 1 } else { 3 },
    })
    .collect()
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            active_profile: String::new(),
            api_mode: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            model: default_model(),
            auto_tagging: false,
            video_tagging: false,
            video_tagging_mode: default_video_tagging_mode(),
            video_frame_count: default_video_frame_count(),
            local_model_tier: default_tier(),
            batch_limit: default_batch_limit(),
            local_batch_limit: default_local_batch_limit(),
            ollama_source_id: default_ollama_source_id(),
            system_prompt_tagging: DEFAULT_TAGGING_PROMPT.to_string(),
            system_prompt_search: String::new(),
            confidence_min_suggest: 0.30,
        }
    }
}

/// 用户自定义下载源（改造方案：即时落库资产，独立于 draft）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomSource {
    pub id: String,
    pub label: String,
    pub url: String,
}

/// FB2-01/02：素材网格外观（档位/比例/填充/匹配主色）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GridAppearance {
    /// 素材库格子档位（CELL_STEPS 下标），默认 3（=190px）
    #[serde(default = "default_library_cell_step")]
    pub library_cell_step: i64,
    /// 入库网格格子档位，默认 1（=120px）
    #[serde(default = "default_import_cell_step")]
    pub import_cell_step: i64,
    /// 统一容器比例（决策 4），默认 "4:3"；已有合法比例不迁移。
    #[serde(default = "default_cell_aspect")]
    pub cell_aspect: String,
    /// 填充方式 cover|contain|smart，默认 "cover"
    #[serde(default = "default_cell_fit")]
    pub cell_fit: String,
    /// contain 留边是否填该素材主色，默认 false
    #[serde(default)]
    pub match_dominant_color: bool,
}

fn default_library_cell_step() -> i64 {
    3
}
fn default_import_cell_step() -> i64 {
    1
}
fn default_cell_aspect() -> String {
    "4:3".into()
}

fn default_tagging_prompt() -> String {
    DEFAULT_TAGGING_PROMPT.to_string()
}
fn default_cell_fit() -> String {
    "cover".into()
}

/// FB2-03：悬停预览（入库页 + 素材库）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverPreviewAppearance {
    /// 总开关，默认 true（bool 需显式 default，避免误为 false）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 预览时长（秒）2~10，默认 3
    #[serde(default = "default_preview_seconds")]
    pub preview_seconds: i64,
    /// 素材库网格是否启用，默认 true
    #[serde(default = "default_true")]
    pub in_library_grid: bool,
}

fn default_true() -> bool {
    true
}
fn default_preview_seconds() -> i64 {
    3
}

/// FB2-08：算法色条显示外观
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorStripAppearance {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub show_in_library_grid: bool,
    #[serde(default = "default_true")]
    pub show_in_viewer: bool,
    #[serde(default)]
    pub show_in_import_grid: bool,
    #[serde(default = "default_strip_height")]
    pub height: String,
    #[serde(default = "default_strip_mode")]
    pub mode: String,
    #[serde(default = "default_strip_count")]
    pub count: i64,
}

fn default_strip_height() -> String {
    "normal".into()
}
fn default_strip_mode() -> String {
    "ratio".into()
}
fn default_strip_count() -> i64 {
    6
}

/// 外观/交互子对象（批次 2 与批次 3/4 共用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    #[serde(default)]
    pub grid: GridAppearance,
    #[serde(default)]
    pub hover_preview: HoverPreviewAppearance,
    #[serde(default)]
    pub color_strip: ColorStripAppearance,
    /// W5h：同源文件组（RAW+JPG）。sync_tags_to_siblings 默认开（打标层算同一张照片，
    /// 请求量 410→205）；merge_in_library 默认关（素材库独立显示，用户定案）。
    #[serde(default)]
    pub kinship: KinshipAppearance,
}

/// W5h：同源文件组设置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KinshipAppearance {
    /// 打标层：给一张打标自动同步到同源文件；建批时同源去重（默认 true）
    #[serde(default = "default_true")]
    pub sync_tags_to_siblings: bool,
    /// 浏览层：素材库合并显示（每组的代表 + 「+RAW」角标；默认 false 独立显示）
    #[serde(default)]
    pub merge_in_library: bool,
}

impl Default for KinshipAppearance {
    fn default() -> Self {
        Self {
            sync_tags_to_siblings: true,
            merge_in_library: false,
        }
    }
}

impl Default for GridAppearance {
    fn default() -> Self {
        Self {
            library_cell_step: default_library_cell_step(),
            import_cell_step: default_import_cell_step(),
            cell_aspect: default_cell_aspect(),
            cell_fit: default_cell_fit(),
            match_dominant_color: false,
        }
    }
}
impl Default for HoverPreviewAppearance {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            preview_seconds: default_preview_seconds(),
            in_library_grid: default_true(),
        }
    }
}
impl Default for ColorStripAppearance {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            show_in_library_grid: false,
            show_in_viewer: default_true(),
            show_in_import_grid: false,
            height: default_strip_height(),
            mode: default_strip_mode(),
            count: default_strip_count(),
        }
    }
}

/// 读取侧：校验并修复 appearance 各字段（非法值回退默认）。在 get_settings 返回前调用。
pub fn normalize_appearance(s: &mut Settings) {
    let a = &mut s.appearance;
    let g = &mut a.grid;
    g.library_cell_step = g.library_cell_step.clamp(0, 7);
    g.import_cell_step = g.import_cell_step.clamp(0, 7);
    if !["1:1", "4:3", "3:2", "16:9", "3:4", "2:3", "9:16"].contains(&g.cell_aspect.as_str()) {
        g.cell_aspect = default_cell_aspect();
    }
    if !["cover", "contain", "smart"].contains(&g.cell_fit.as_str()) {
        g.cell_fit = default_cell_fit();
    }
    let h = &mut a.hover_preview;
    h.preview_seconds = h.preview_seconds.clamp(2, 10);
    let cs = &mut a.color_strip;
    if ![4, 6, 8].contains(&cs.count) {
        cs.count = default_strip_count();
    }
    if !["thin", "normal", "thick"].contains(&cs.height.as_str()) {
        cs.height = default_strip_height();
    }
    if !["ratio", "equal"].contains(&cs.mode.as_str()) {
        cs.mode = default_strip_mode();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub ai: AiSettings,
    #[serde(default = "default_theme")]
    pub theme: String, // system|light|dark
    /// 用户可调整的文件日志级别：info|debug|trace
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_cache_mb")]
    pub thumbnail_cache_mb: i64,
    /// 标签分类（PRD 5.5，设置页可管理）
    /// F8：唯一读者是 migrate_v20（把中文分类迁为稳定分面），新代码禁止读/写此字段。
    #[serde(default, skip_serializing)]
    pub tag_categories: Vec<TagCategory>,
    /// AI 分面配置（P1B 唯一事实源，facet_key 稳定不可修改）
    /// V20 后语义已搬进 tag_facets.input_mode/cfg_*：本字段只作旧 JSON 反序列化输入。
    /// F8：唯一读者是 migrate_v20（回填），新代码禁止读/写此字段。
    #[serde(default, skip_serializing)]
    pub ai_facet_configs: Vec<AiFacetConfig>,
    /// 总库位置（R-32）；空 = 原位索引模式
    #[serde(default)]
    pub library_root: String,
    /// 回收站保留天数（R-22）；启动时清理超期项，0 = 不自动清理
    #[serde(default = "default_trash_retention_days")]
    pub trash_retention_days: i64,
    /// Ollama 一键下载的自定义源（即时落库；旧数据缺省空）
    #[serde(default = "default_custom_sources")]
    pub custom_download_sources: Vec<CustomSource>,
    /// Ollama 模型下载代理（改造方案·加速项 A：拉起 serve 时注入 HTTPS_PROXY；空 = 不用代理）
    #[serde(default)]
    pub model_download_proxy: String,
    /// FB2-01/02/03/08：外观与交互设置
    #[serde(default)]
    pub appearance: Appearance,
    /// 首次启动教程提示是否已处理。新配置及旧设置文件缺字段时均默认未处理。
    #[serde(default = "default_tutorial_prompt_handled")]
    pub tutorial_prompt_handled: bool,
}

fn default_tutorial_prompt_handled() -> bool {
    false
}

fn default_theme() -> String {
    "system".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_cache_mb() -> i64 {
    2048
}
fn default_trash_retention_days() -> i64 {
    30
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ai: AiSettings::default(),
            theme: default_theme(),
            log_level: default_log_level(),
            thumbnail_cache_mb: default_cache_mb(),
            tag_categories: Vec::new(),
            ai_facet_configs: Vec::new(),
            library_root: String::new(),
            trash_retention_days: default_trash_retention_days(),
            custom_download_sources: default_custom_sources(),
            model_download_proxy: String::new(),
            appearance: Appearance::default(),
            tutorial_prompt_handled: false,
        }
    }
}

/// 读取侧：日志级别只接受三个可选值，非法旧值回退 info。
pub fn normalize_log_level(s: &mut Settings) {
    if !matches!(s.log_level.as_str(), "info" | "debug" | "trace") {
        s.log_level = default_log_level();
    }
}

/// 读取侧：对一份已解析的 Settings 就地新增自定义源（纯逻辑，便于单测/命令复用）
pub fn add_custom_source(s: &mut Settings, src: CustomSource) {
    s.custom_download_sources.push(src);
}

/// 读取侧：按 id 移除自定义源；返回是否移除成功
pub fn remove_custom_source(s: &mut Settings, id: &str) -> bool {
    let before = s.custom_download_sources.len();
    s.custom_download_sources.retain(|c| c.id != id);
    s.custom_download_sources.len() != before
}

const KEY: &str = "app_settings";

/// FB2-08（§14.3）：新播种的分面默认是否参与 AI 打标。
/// color 恒为 false —— 颜色由 `services::palette` 算法给出（可精确计算、快 3~4 个数量级），
/// 让 AI 再猜一遍颜色只会产出与算法主色矛盾的标签。V16 迁移已把存量库的 color 关掉，
/// 这里管的是"之后新播种的配置"（全新库 / 缺 color 的老库），两条路径必须给同一个答案。
fn seeded_ai_enabled(facet_key: &str) -> bool {
    facet_key != "color"
}

/// 把旧的 tag_categories（中文分类名）映射为 ai_facet_configs（稳定 facet_key）。
/// 这是唯一一次迁移：此后业务只读 ai_facet_configs。
fn migrate_tag_categories_to_facets(s: &mut Settings) {
    if s.tag_categories.is_empty() {
        return;
    }
    let mut existing: std::collections::HashSet<String> = s
        .ai_facet_configs
        .iter()
        .map(|c| c.facet_key.clone())
        .collect();
    for cat in &s.tag_categories {
        let facet = super::tag_facets::key_for_legacy_name(&cat.name).to_string();
        if existing.contains(&facet) {
            // 同分面重复：合并 hint（旧 hint 非空则保留）
            if let Some(cfg) = s.ai_facet_configs.iter_mut().find(|c| c.facet_key == facet) {
                if cfg.hint.is_empty() && !cat.hint.is_empty() {
                    cfg.hint = cat.hint.clone();
                }
            }
            continue;
        }
        existing.insert(facet.clone());
        s.ai_facet_configs.push(AiFacetConfig {
            facet_key: facet.clone(),
            hint: cat.hint.clone(),
            enabled_for_ai: seeded_ai_enabled(&facet),
            display_name: None,
            visible_in_workbench: None,
        });
    }
    s.tag_categories.clear();
}

/// 全新/无配置时，用默认分面清单充实 ai_facet_configs（保证 AI 打标有提示词上下文）。
fn normalize_ai_facet_defaults(s: &mut Settings) {
    if s.ai_facet_configs.is_empty() {
        for cat in default_tag_categories() {
            let facet = super::tag_facets::key_for_legacy_name(&cat.name).to_string();
            if s.ai_facet_configs.iter().any(|c| c.facet_key == facet) {
                continue;
            }
            s.ai_facet_configs.push(AiFacetConfig {
                facet_key: facet.clone(),
                hint: cat.hint,
                enabled_for_ai: seeded_ai_enabled(&facet),
                display_name: None,
                visible_in_workbench: None,
            });
        }
    }
}

/// C-5/V11：存量库补齐独立 color 分面的 AI 配置 —— 老库可能已把「色彩风格」归 style 而缺少 color。
/// 幂等：已有 color 配置则不变；只补默认，不覆盖用户已有的 style hint 或任何配置内容。
pub fn ensure_color_facet_config(conn: &Connection) -> AppResult<()> {
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([KEY])?;
    let Some(row) = rows.next()? else {
        return Ok(());
    };
    let raw: String = row.get(0)?;
    let mut s: Settings = serde_json::from_str(&raw).unwrap_or_default();
    if s.ai_facet_configs.iter().any(|c| c.facet_key == "color") {
        return Ok(());
    }
    s.ai_facet_configs.push(AiFacetConfig {
        facet_key: "color".into(),
        hint: "主色、色调与色彩关系，如青橙/暗调/冷调".into(),
        enabled_for_ai: seeded_ai_enabled("color"),
        display_name: None,
        visible_in_workbench: None,
    });
    save_settings(conn, &s)?;
    Ok(())
}

pub fn get_settings(conn: &Connection) -> AppResult<Settings> {
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([KEY])?;
    if let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let mut s: Settings = serde_json::from_str(&raw).unwrap_or_default();
        // 兼容迁移：旧 tag_categories → ai_facet_configs；全空则用默认分面清单
        migrate_tag_categories_to_facets(&mut s);
        normalize_ai_facet_defaults(&mut s);
        s.ai.normalize();
        normalize_appearance(&mut s);
        normalize_log_level(&mut s);
        return Ok(s);
    }
    let mut d = Settings::default();
    normalize_ai_facet_defaults(&mut d);
    Ok(d)
}

pub fn save_settings(conn: &Connection, s: &Settings) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY, serde_json::to_string(s)?],
    )?;
    Ok(())
}

/// V10 迁移：读 settings JSON → 旧 tag_categories 转 ai_facet_configs → 写回。
/// 幂等：已转（tag_categories 为空）则不变。立即落库，保证重启后无需再转。
pub fn normalize_settings_persist(conn: &Connection) -> AppResult<()> {
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([KEY])?;
    let Some(row) = rows.next()? else {
        return Ok(());
    };
    let raw: String = row.get(0)?;
    let mut s: Settings = serde_json::from_str(&raw).unwrap_or_default();
    // 标记是否需要写回（tag_categories 有值说明未迁移）
    if !s.tag_categories.is_empty() {
        migrate_tag_categories_to_facets(&mut s);
        save_settings(conn, &s)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_flat_config_migrates_to_profile() {
        let mut ai: AiSettings = serde_json::from_str(
            r#"{"baseUrl":"https://a.com/v1","apiKey":"sk-x","model":"m1","apiMode":"anthropic"}"#,
        )
        .unwrap();
        ai.normalize();
        assert_eq!(ai.profiles.len(), 1);
        let p = ai.active().unwrap();
        assert_eq!(p.base_url, "https://a.com/v1");
        assert_eq!(p.api_mode, "anthropic");
        assert_eq!(p.model, "m1");
        assert_eq!(ai.active_profile, "default");
    }

    #[test]
    fn legacy_profile_defaults_to_cloud_kind() {
        let p: ApiProfile = serde_json::from_str(
            r#"{"id":"x","name":"旧档案","baseUrl":"http://a/v1","apiKey":"k","model":"m"}"#,
        )
        .unwrap();
        assert_eq!(p.kind, "cloud");
        assert!(!p.is_local());
    }

    #[test]
    fn empty_config_stays_empty() {
        let mut ai = AiSettings::default();
        ai.normalize();
        assert!(ai.active().is_none());
    }

    #[test]
    fn active_falls_back_to_first() {
        let mut ai = AiSettings::default();
        ai.profiles.push(ApiProfile {
            id: "p1".into(),
            name: "A".into(),
            api_mode: "openai".into(),
            kind: "cloud".into(),
            base_url: "u".into(),
            api_key: "k".into(),
            model: "m".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        });
        ai.active_profile = "not-exist".into();
        assert_eq!(ai.active().unwrap().id, "p1");
    }

    #[test]
    fn legacy_settings_default_new_fields() {
        // 旧数据无新字段 → serde default 兜底
        let s: Settings =
            serde_json::from_str(r#"{"ai":{"profiles":[]},"theme":"system"}"#).unwrap();
        assert_eq!(s.ai.ollama_source_id, "auto");
        assert_eq!(s.log_level, "info");
        assert!(s.custom_download_sources.is_empty());
        assert_eq!(s.model_download_proxy, "");
    }

    #[test]
    fn invalid_saved_log_level_falls_back_to_info() {
        let mut s: Settings = serde_json::from_str(r#"{"logLevel":"verbose"}"#).unwrap();
        normalize_log_level(&mut s);
        assert_eq!(s.log_level, "info");
    }

    #[test]
    fn get_settings_normalizes_persisted_log_level() {
        let conn = crate::db::init_memory().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![KEY, r#"{"logLevel":"verbose"}"#],
        )
        .unwrap();

        let s = get_settings(&conn).unwrap();
        assert_eq!(s.log_level, "info");
    }

    #[test]
    fn custom_source_add_remove_roundtrip() {
        let mut s = Settings::default();
        add_custom_source(
            &mut s,
            CustomSource {
                id: "custom-1".into(),
                label: "NAS".into(),
                url: "https://nas/x".into(),
            },
        );
        assert_eq!(s.custom_download_sources.len(), 1);
        assert!(remove_custom_source(&mut s, "custom-1"));
        assert_eq!(s.custom_download_sources.len(), 0);
        assert!(!remove_custom_source(&mut s, "custom-1"));
    }

    #[test]
    fn legacy_tag_categories_migrate_to_facet_configs() {
        let mut s: Settings = serde_json::from_str(
            r#"{"tagCategories":[{"name":"场景","hint":"如公园/街道","single":true,"max":1},
                {"name":"未知分类","hint":"hint-x","single":false,"max":3}]}"#,
        )
        .unwrap();
        // get_settings 会调用的迁移
        super::migrate_tag_categories_to_facets(&mut s);
        assert!(s.tag_categories.is_empty());
        let scene = s
            .ai_facet_configs
            .iter()
            .find(|c| c.facet_key == "scene")
            .expect("场景应映射到 scene");
        assert_eq!(scene.hint, "如公园/街道");
        assert!(scene.enabled_for_ai);
        // 未知分类 → custom
        let custom = s
            .ai_facet_configs
            .iter()
            .find(|c| c.facet_key == "custom")
            .expect("未知分类应归 custom");
        assert_eq!(custom.hint, "hint-x");
    }

    /// FB2-08（§14.3）：新播种的 color 分面不参与 AI 打标。
    /// V16 只改了存量库的 settings JSON；全新库与"缺 color 的老库"走播种路径，
    /// 两边必须一致，否则设置页显示 color 开着而 AI 实际不产出 color 标签。
    #[test]
    fn seeded_color_facet_is_not_ai_enabled() {
        let mut s = Settings::default();
        super::normalize_ai_facet_defaults(&mut s);
        let color = s
            .ai_facet_configs
            .iter()
            .find(|c| c.facet_key == "color")
            .expect("默认清单应含 color 分面");
        assert!(
            !color.enabled_for_ai,
            "color 由算法主色负责，不得播种成参与 AI 打标"
        );
        // 其他分面不受影响
        let scene = s
            .ai_facet_configs
            .iter()
            .find(|c| c.facet_key == "scene")
            .unwrap();
        assert!(scene.enabled_for_ai);
    }

    #[test]
    fn facet_config_roundtrip_json() {
        let c = AiFacetConfig {
            facet_key: "scene".into(),
            hint: "海边".into(),
            enabled_for_ai: true,
            display_name: Some("场景".into()),
            visible_in_workbench: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: AiFacetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.facet_key, "scene");
        assert_eq!(back.display_name.as_deref(), Some("场景"));
        // 未显式设置 visibleInWorkbench 不序列化（前端按白名单兜底）
        assert!(back.visible_in_workbench.is_none());
        assert!(!json.contains("visibleInWorkbench"));
    }

    #[test]
    fn old_serialization_skips_legacy_tag_categories() {
        let mut s = Settings::default();
        s.tag_categories.push(TagCategory {
            name: "场景".into(),
            hint: String::new(),
            single: false,
            max: 3,
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("tagCategories"),
            "tag_categories 不应再序列化"
        );
    }

    #[test]
    fn appearance_missing_fields_default_to_expectations() {
        // 缺 appearance 字段反序列化 → 全默认；尤其 hover_preview.enabled 默认为 true
        let s: Settings = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(
            s.appearance.grid.library_cell_step,
            default_library_cell_step()
        );
        assert_eq!(s.appearance.grid.cell_aspect, "4:3");
        assert_eq!(s.appearance.grid.cell_fit, "cover");
        assert!(s.appearance.hover_preview.enabled, "hover 默认应开启");
        assert_eq!(s.appearance.hover_preview.preview_seconds, 3);
        assert!(s.appearance.hover_preview.in_library_grid);
        assert_eq!(s.appearance.color_strip.count, 6);
        assert_eq!(s.appearance.color_strip.height, "normal");
        assert!(!s.appearance.color_strip.show_in_library_grid);
        assert!(s.appearance.color_strip.show_in_viewer);
    }

    #[test]
    fn legacy_global_prompts_are_ignored_and_not_serialized() {
        let mut ai: AiSettings = serde_json::from_str(
            r#"{"systemPromptTagging":"override tagging","systemPromptSearch":"override search"}"#,
        )
        .unwrap();
        ai.normalize();
        assert_eq!(ai.system_prompt_tagging, DEFAULT_TAGGING_PROMPT);
        assert!(ai.system_prompt_search.is_empty());
        let encoded = serde_json::to_string(&ai).unwrap();
        assert!(!encoded.contains("systemPromptTagging"));
        assert!(!encoded.contains("systemPromptSearch"));
    }

    #[test]
    fn first_run_tutorial_defaults_to_unhandled_for_new_and_legacy_settings() {
        assert!(!Settings::default().tutorial_prompt_handled);
        let existing: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(!existing.tutorial_prompt_handled);
        let handled: Settings = serde_json::from_str(r#"{"tutorialPromptHandled":true}"#).unwrap();
        assert!(handled.tutorial_prompt_handled);
    }

    #[test]
    fn normalize_appearance_clamps_and_falls_back() {
        let mut s: Settings = serde_json::from_str(
            r#"{"appearance":{"grid":{"libraryCellStep":99,"cellAspect":"oops","cellFit":"stretch"},
                "hoverPreview":{"previewSeconds":99},"colorStrip":{"count":5,"height":"huge"}}}"#,
        )
        .unwrap();
        normalize_appearance(&mut s);
        let g = &s.appearance.grid;
        assert_eq!(g.library_cell_step, 7); // clamp 0..=7
        assert_eq!(g.cell_aspect, "4:3");
        assert_eq!(g.cell_fit, "cover");
        assert_eq!(s.appearance.hover_preview.preview_seconds, 10);
        assert_eq!(s.appearance.color_strip.count, 6);
        assert_eq!(s.appearance.color_strip.height, "normal");
    }

    #[test]
    fn appearance_roundtrip_json_consistent() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.appearance.grid.library_cell_step,
            s.appearance.grid.library_cell_step
        );
        assert_eq!(
            back.appearance.hover_preview.enabled,
            s.appearance.hover_preview.enabled
        );
    }

    #[test]
    fn ai_video_tagging_mode_defaults_and_clamps() {
        let s: Settings = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(s.ai.video_tagging_mode, "cover", "默认封面打标（决策 5）");
        assert_eq!(s.ai.video_frame_count, 3);
        let mut ai = s.ai;
        ai.video_tagging_mode = "bad".into();
        ai.video_frame_count = 99;
        ai.normalize();
        assert_eq!(ai.video_tagging_mode, "cover");
        assert_eq!(ai.video_frame_count, 8);
    }
}
