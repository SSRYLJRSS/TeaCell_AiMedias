//! 稳定标签分面：分面 key 是协议和数据边界，显示名称可以本地化。
//! 生命周期（指导书 §12.2）：创建 → active → 停用(inactive) → 恢复；key 创建后不可改。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// AI 打标/搜索共享的 FacetPromptContext：稳定 key + 人类可读信息 + 数据库规则
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetPromptContext {
    pub key: String,
    pub display_name: String,
    /// V20 合表后：description 同时承载「给人的说明」与「给 AI 的 hint」（W2-1 删独立 hint）
    pub description: String,
    pub selection_mode: String,
    pub max_items: Option<i64>,
    /// V24（§6.4）：tag | number —— 数值分面不发词表，发「输出一个 min–max 的数字」
    #[serde(default)]
    pub facet_kind: String,
    #[serde(default)]
    pub num_min: Option<f64>,
    #[serde(default)]
    pub num_max: Option<f64>,
    #[serde(default)]
    pub num_unit: String,
    #[serde(default)]
    pub num_decimals: i64,
    #[serde(default = "default_prompt_num_step")]
    pub num_step: f64,
}

fn default_prompt_num_step() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagFacet {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub selection_mode: String,
    pub max_items: Option<i64>,
    pub sort_order: i64,
    pub is_system: bool,
    pub status: String,
    pub applies_to: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// V22a（F1-b）：`input_mode` 改为**只读派生值** —— 序列化时按
    /// `cfg_ai_assignable` 计算（`ai_and_manual` / `manual_only`），不再可写。
    /// 配置的真实事实源是 `cfg_ai_assignable`（F1-a 能力矩阵列）。
    pub input_mode: String,
    // ── V22a 能力矩阵配置列（记录用户意图；生命周期状态永不覆盖它们）──
    pub cfg_visible_in_navigation: bool,
    pub cfg_manual_assignable: bool,
    pub cfg_ai_assignable: bool,
    pub cfg_searchable: bool,
    /// 扩展预留（未来数字型/日期型分面；'tag' | 'number'）
    pub facet_kind: String,
    // ── V24（§6.3①）：数值分面配置五列（facet_kind='number' 时生效）──
    /// 数值下界（NULL = 不限）
    pub num_min: Option<f64>,
    /// 数值上界（NULL = 不限）
    pub num_max: Option<f64>,
    /// 展示后缀（「人」「mm」）
    pub num_unit: String,
    /// 小数位
    pub num_decimals: i64,
    /// 步进
    pub num_step: f64,
}

/// 创建或编辑分类的一次 IPC 请求；同一 DTO 覆盖普通分类和数值分类。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetSaveInput {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub input_mode: String,
    pub selection_mode: String,
    pub max_items: Option<i64>,
    pub applies_to: String,
    pub facet_kind: String,
    pub num_min: Option<f64>,
    pub num_max: Option<f64>,
    pub num_unit: String,
    pub num_decimals: i64,
    pub num_step: f64,
}

/// F1-b：有效值派生（SQL 侧四个常量，唯一声明处）。
/// 注意 EFF_SEARCH 不看 status —— 停用分面的标签仍可用于词条候选和结构化条件；全文 FTS 仍按生命周期退出。
pub const EFF_VISIBLE: &str = "f.status = 'active' AND f.cfg_visible_in_navigation = 1";
pub const EFF_MANUAL: &str = "f.status = 'active' AND f.cfg_manual_assignable = 1";
pub const EFF_AI: &str = "f.status = 'active' AND f.cfg_ai_assignable = 1";
pub const EFF_SEARCH: &str = "f.cfg_searchable = 1";

/// F1-b：Rust 侧同一规则的有效值派生。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetEffective {
    pub visible: bool,
    pub manual: bool,
    pub ai: bool,
    pub searchable: bool,
}

impl TagFacet {
    /// F1-b：单点声明有效值（与 SQL 常量 EFF_* 同规则；cross-test 用 48 组合守护）。
    pub fn effective(&self) -> FacetEffective {
        let alive = self.status == "active";
        FacetEffective {
            visible: alive && self.cfg_visible_in_navigation,
            manual: alive && self.cfg_manual_assignable,
            ai: alive && self.cfg_ai_assignable,
            searchable: self.cfg_searchable,
        }
    }

    /// F1-b：input_mode 只读派生（cfg_ai_assignable 的事实源）。
    pub fn derived_input_mode(&self) -> &'static str {
        if self.cfg_ai_assignable {
            "ai_and_manual"
        } else {
            "manual_only"
        }
    }
}

/// 停用/治理前的引用与影响范围（指导书 §12.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetImpact {
    pub tag_count: i64,
    pub asset_count: i64,
    /// V20 合表后 aiFacetConfigs 恒为 0（W2-4：改报 suggestion items / tag_ops 计数）
    pub ai_suggestion_item_count: i64,
    pub tag_op_count: i64,
    /// R3-3：别名计数 —— delete_facet 会删 tag_aliases（:403），影响报告须含它
    pub alias_count: i64,
    /// V24（§6.5）：数值行计数 —— 删除分面会级联删 asset_facet_numbers
    pub number_count: i64,
}

/// 校验稳定 key：小写 snake_case，2–64 字符，只允许字母/数字/下划线，不以数字开头（指导书 §12.3）。
pub fn validate_key(key: &str) -> AppResult<String> {
    let k = key.trim().to_lowercase();
    let valid = k.len() >= 2
        && k.len() <= 64
        && k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && k.as_bytes()[0].is_ascii_lowercase();
    if !valid {
        return Err(AppError::msg(
            "分面 key 需为小写 snake_case，长度 2–64，仅字母/数字/下划线，且不以数字开头",
        ));
    }
    Ok(k)
}

fn facet_from_row(r: &rusqlite::Row) -> rusqlite::Result<TagFacet> {
    // FACET_COLS 顺序（追加末尾，铁律 2）：
    // 0 key / 1 display_name / 2 description / 3 selection_mode / 4 max_items
    // 5 sort_order / 6 is_system / 7 status / 8 applies_to / 9 created_at / 10 updated_at
    // 11 cfg_visible_in_navigation / 12 cfg_manual_assignable
    // 13 cfg_ai_assignable / 14 cfg_searchable / 15 facet_kind
    let cfg_ai: bool = r.get::<_, i64>(13)? != 0;
    Ok(TagFacet {
        key: r.get(0)?,
        display_name: r.get(1)?,
        description: r.get(2)?,
        selection_mode: r.get(3)?,
        max_items: r.get(4)?,
        sort_order: r.get(5)?,
        is_system: r.get::<_, i64>(6)? != 0,
        status: r.get(7)?,
        applies_to: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        // input_mode 已由 DB 列降级为只读派生：按 cfg_ai_assignable 计算
        input_mode: if cfg_ai {
            "ai_and_manual"
        } else {
            "manual_only"
        }
        .to_string(),
        cfg_visible_in_navigation: r.get::<_, i64>(11)? != 0,
        cfg_manual_assignable: r.get::<_, i64>(12)? != 0,
        cfg_ai_assignable: cfg_ai,
        cfg_searchable: r.get::<_, i64>(14)? != 0,
        facet_kind: r.get(15)?,
        // V24（铁律 2：追加末尾，索引 16–20 与 FACET_COLS 一一对应）
        num_min: r.get(16)?,
        num_max: r.get(17)?,
        num_unit: r.get(18)?,
        num_decimals: r.get(19)?,
        num_step: r.get(20)?,
    })
}

const FACET_COLS: &str =
    "key, display_name, description, selection_mode, max_items, sort_order, is_system, status, applies_to, created_at, updated_at, cfg_visible_in_navigation, cfg_manual_assignable, cfg_ai_assignable, cfg_searchable, facet_kind, num_min, num_max, num_unit, num_decimals, num_step";

/// 用户可见且可停用的内置 AI 分类。key 是固定协议，分类说明仍允许编辑。
const CORE_SYSTEM_AI_FACETS: &[&str] = &["subject", "scene", "composition", "lighting", "people"];

fn is_core_system_ai_facet(key: &str) -> bool {
    CORE_SYSTEM_AI_FACETS.contains(&key)
}

fn is_internal_facet(key: &str) -> bool {
    matches!(key, "color" | "custom")
}

fn is_reserved_facet_key(key: &str) -> bool {
    key == "description" || is_internal_facet(key) || is_core_system_ai_facet(key)
}

/// 系统分面种子清单（migrate_v8 与 reset 后重建共用；key 顺序即 sort_order）。
/// purpose/technical 已由后续迁移移出内置默认；color 是内部兼容 key，补种后置 inactive。
const SYSTEM_FACETS: &[(&str, &str, &str, i64, i64)] = &[
    (
        "subject",
        "主体对象",
        "画面中被重点呈现的人、动物、物体；人物统一标「人」，不写性别、年龄和穿着",
        3,
        10,
    ),
    (
        "scene",
        "场景/地点",
        "画面发生的空间、环境和地点；多值如实输出，不写主体物品",
        3,
        20,
    ),
    ("color", "色彩", "主色、色调与色彩关系", 3, 50),
    ("composition", "构图/视角", "景别、视角和构图关系", 4, 60),
    ("lighting", "光线/时间", "光线方向、质感和时间氛围", 3, 70),
    (
        "people",
        "人物属性",
        "人物状态、人数档位、性别、年龄段、穿着和动作；多人时分别输出可观察属性",
        8,
        80,
    ),
    (
        "custom",
        "自定义",
        "用户自定义且暂未归入固定分面的标签",
        0,
        100,
    ),
];

/// 幂等补种系统分面（INSERT OR IGNORE：已存在行不动，包括用户改过的 display_name 与停用态）。
/// 使用场景：① V8 迁移建库；② 重置标签数据后重建系统分面；③ 启动自愈兜底。
pub fn seed_system_facets(conn: &Connection) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    for (key, name, description, max_items, sort_order) in SYSTEM_FACETS {
        conn.execute(
            "INSERT OR IGNORE INTO tag_facets
             (key, display_name, description, selection_mode, max_items, sort_order, is_system, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'multi', NULLIF(?4, 0), ?5, 1, 'active', ?6, ?6)",
            params![key, name, description, max_items, sort_order, now],
        )?;
    }
    Ok(())
}

/// 空表自愈：tag_facets 一行都没有（历史重置标签路径清空后未补种）时重建系统分面。
/// color 补种后立即置回 inactive（V16 语义：颜色由算法主色呈现，AI 侧已摘除）。
/// 只在完全空表时触发，不影响任何已有分面（含用户自建）。
pub fn seed_system_facets_if_empty(conn: &Connection) -> AppResult<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tag_facets", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }
    seed_system_facets(conn)?;
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "UPDATE tag_facets SET status = 'inactive', updated_at = ?1 WHERE key = 'color'",
        params![now],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> AppResult<Vec<TagFacet>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FACET_COLS} FROM tag_facets
          WHERE status = 'active' ORDER BY sort_order, key"
    ))?;
    let rows = stmt
        .query_map([], facet_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 列出全部（含 inactive），供设置页分面管理展示。
pub fn list_all(conn: &Connection) -> AppResult<Vec<TagFacet>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FACET_COLS} FROM tag_facets ORDER BY sort_order, key"
    ))?;
    let rows = stmt
        .query_map([], facet_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, key: &str) -> AppResult<TagFacet> {
    Ok(conn.query_row(
        &format!("SELECT {FACET_COLS} FROM tag_facets WHERE key = ?1"),
        [key],
        facet_from_row,
    )?)
}

/// 创建用户分面（is_system=false）。key 校验唯一并规范化为小写；selection_mode/max_items 校验。
pub fn create(
    conn: &Connection,
    key: &str,
    display_name: &str,
    description: &str,
    selection_mode: &str,
    max_items: Option<i64>,
    applies_to: &str,
) -> AppResult<TagFacet> {
    let key = validate_key(key)?;
    let display_name = display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(AppError::msg("显示名不能为空"));
    }
    if is_reserved_facet_key(&key) {
        return Err(AppError::invalid_arg(
            "该分类 key 由系统保留，不能用于新建分类",
        ));
    }
    if selection_mode != "single" && selection_mode != "multi" {
        return Err(AppError::msg("selection_mode 只允许 single | multi"));
    }
    // single 强制 max_items 语义为 1；multi 必须为正整数或 NULL
    let max_items = match selection_mode {
        "single" => Some(1),
        _ => match max_items {
            Some(n) if n >= 1 => Some(n),
            Some(_) => return Err(AppError::msg("多选分面的 max_items 必须为正整数或为空")),
            None => None,
        },
    };
    if applies_to != "all" && applies_to != "image" && applies_to != "video" {
        return Err(AppError::msg("applies_to 只允许 all | image | video"));
    }
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM tag_facets WHERE key = ?1", [&key], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_some() {
        return Err(AppError::msg("分面 key 已存在（创建后不可修改）"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let sort_order: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM tag_facets",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO tag_facets
         (key, display_name, description, selection_mode, max_items, sort_order, is_system, status, applies_to, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'active', ?7, ?8, ?8)",
        params![key, display_name, description, selection_mode, max_items, sort_order, applies_to, now],
    )?;
    // F8：V20 合表后 ai_facet_configs 已死（skip_serializing），不再补种子配置；
    // 参与 AI 语义由 cfg_ai_assignable/input_mode 列承载（新建默认 ai_and_manual）。
    get(conn, &key)
}

/// 重新排序（传入完整的有序 key 列表）。
pub fn reorder(conn: &Connection, ordered_keys: &[String]) -> AppResult<()> {
    // 只更新传入的 key；未传入的不动（幂等）。按索引递增 sort_order。
    for (i, k) in ordered_keys.iter().enumerate() {
        conn.execute(
            "UPDATE tag_facets SET sort_order=?1, updated_at=?2 WHERE key=?3",
            params![
                (i + 1) as i64 * 10,
                chrono::Utc::now().timestamp_millis(),
                k
            ],
        )?;
    }
    Ok(())
}

/// W2-2 + F8：合并编辑命令 —— 6 字段一个事务（display_name/description/input_mode/
/// selection_mode/max_items/applies_to）。旧的 update_display/update_rules 两个即时写
/// 命令已在 F8 删除（唯一编辑通道是 update_facet）。
/// 部分字段非法时全部不生效（单一保存通道语义）。
/// F1-b：input_mode 降级为只读派生 —— 此处按入参换算写 `cfg_ai_assignable`，
/// 同时回写 input_mode 列保持 DB 内一致（回滚/历史查询可读）。
// 8 参数为分面编辑字段的内聚集合，收进结构体需同步改全部调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn update_facet(
    conn: &Connection,
    key: &str,
    display_name: &str,
    description: &str,
    input_mode: &str,
    selection_mode: &str,
    max_items: Option<i64>,
    applies_to: &str,
) -> AppResult<()> {
    let existing = get(conn, key)?;
    if existing.is_system && !is_core_system_ai_facet(key) {
        return Err(AppError::unsupported(
            "该系统分面由应用内部管理，不能通过分类编辑器修改",
        ));
    }
    if is_core_system_ai_facet(key) && input_mode != "ai_and_manual" {
        return Err(AppError::invalid_arg(
            "内置 AI 分类不能移出 AI 自动打标分组",
        ));
    }
    let display_name = display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(AppError::msg("显示名不能为空"));
    }
    if input_mode != "ai_and_manual" && input_mode != "manual_only" {
        return Err(AppError::msg(
            "input_mode 只允许 ai_and_manual | manual_only",
        ));
    }
    let cfg_ai: i64 = if input_mode == "ai_and_manual" { 1 } else { 0 };
    if selection_mode != "single" && selection_mode != "multi" {
        return Err(AppError::msg("selection_mode 只允许 single | multi"));
    }
    let max_items = match selection_mode {
        "single" => Some(1),
        _ => match max_items {
            Some(n) if n >= 1 => Some(n),
            Some(_) => return Err(AppError::msg("多选分面的 max_items 必须为正整数或为空")),
            None => None,
        },
    };
    if applies_to != "all" && applies_to != "image" && applies_to != "video" {
        return Err(AppError::msg("applies_to 只允许 all | image | video"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE tag_facets SET
            display_name = ?2, description = ?3,
            cfg_ai_assignable = ?4,
            input_mode = CASE WHEN ?4 = 1 THEN 'ai_and_manual' ELSE 'manual_only' END,
            selection_mode = ?5, max_items = ?6, applies_to = ?7, updated_at = ?8
          WHERE key = ?1",
        rusqlite::params![
            key,
            display_name,
            description.trim(),
            cfg_ai,
            selection_mode,
            max_items,
            applies_to,
            now
        ],
    )?;
    if n == 0 {
        return Err(AppError::msg("分面不存在"));
    }
    tx.commit()?;
    Ok(())
}

/// 创建或编辑分面的一次性持久化入口。基础字段、AI 适用状态与数值类型配置
/// 在同一 SQLite 事务内提交，避免前端用多次 IPC 做不可靠的补偿保存。
pub(crate) fn save_facet(conn: &Connection, input: &FacetSaveInput) -> AppResult<TagFacet> {
    let key = input.key.as_str();
    let display_name = input.display_name.trim();
    let description = input.description.as_str();
    let input_mode = input.input_mode.as_str();
    let selection_mode = input.selection_mode.as_str();
    let applies_to = input.applies_to.as_str();
    let facet_kind = input.facet_kind.as_str();
    let num_min = input.num_min;
    let num_max = input.num_max;
    let num_unit = input.num_unit.as_str();
    let num_decimals = input.num_decimals;
    let num_step = input.num_step;
    let max_items = match selection_mode {
        "single" => Some(1),
        _ => input.max_items,
    };
    let tx = conn.unchecked_transaction()?;
    let existing = tx
        .query_row(
            "SELECT is_system, facet_kind FROM tag_facets WHERE key=?1",
            [&key],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let is_new = existing.is_none();
    if is_new {
        if is_reserved_facet_key(key) {
            return Err(AppError::invalid_arg(
                "该分类 key 由系统保留，不能用于新建分类",
            ));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let sort_order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 10 FROM tag_facets",
            [],
            |row| row.get(0),
        )?;
        let (num_min, num_max, num_unit, num_decimals, num_step) = if facet_kind == "number" {
            (num_min, num_max, num_unit, num_decimals, num_step)
        } else {
            (None, None, "", 0, 1.0)
        };
        tx.execute(
            "INSERT INTO tag_facets
             (key, display_name, description, selection_mode, max_items, sort_order, is_system,
              status, applies_to, created_at, updated_at, cfg_ai_assignable, facet_kind,
              num_min, num_max, num_unit, num_decimals, num_step, input_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'active', ?7, ?8, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                key,
                display_name,
                description,
                selection_mode,
                max_items,
                sort_order,
                applies_to,
                now,
                i64::from(input_mode == "ai_and_manual"),
                facet_kind,
                num_min,
                num_max,
                num_unit,
                num_decimals,
                num_step,
                input_mode,
            ],
        )?;
    } else {
        let (is_system, existing_kind) = existing.unwrap();
        if is_system && !is_core_system_ai_facet(key) {
            return Err(AppError::unsupported(
                "该系统分面由应用内部管理，不能通过分类编辑器修改",
            ));
        }
        if is_core_system_ai_facet(key) && input_mode != "ai_and_manual" {
            return Err(AppError::invalid_arg(
                "内置 AI 分类不能移出 AI 自动打标分组",
            ));
        }
        if existing_kind == "number" && facet_kind != "number" {
            return Err(AppError::invalid_arg(
                "数值分面不能改回标签型（连续值无法无损转成离散标签）",
            ));
        }
        if existing_kind == "tag" && facet_kind == "number" {
            let tag_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM tags WHERE facet_key=?1 AND status != 'deprecated'",
                [&key],
                |row| row.get(0),
            )?;
            if tag_count > 0 {
                return Err(AppError::conflict(format!(
                    "分面「{key}」已有 {tag_count} 个标签，请先使用转换预览"
                )));
            }
        }
        let (num_min, num_max, num_unit, num_decimals, num_step) = if facet_kind == "number" {
            (num_min, num_max, num_unit, num_decimals, num_step)
        } else {
            (None, None, "", 0, 1.0)
        };
        let now = chrono::Utc::now().timestamp_millis();
        let changed = tx.execute(
            "UPDATE tag_facets SET display_name=?2, description=?3,
              cfg_ai_assignable=?4,
              input_mode=CASE WHEN ?4=1 THEN 'ai_and_manual' ELSE 'manual_only' END,
              selection_mode=?5, max_items=?6, applies_to=?7, facet_kind=?8,
              num_min=?9, num_max=?10, num_unit=?11, num_decimals=?12, num_step=?13,
              updated_at=?14 WHERE key=?1",
            params![
                key,
                display_name,
                description,
                i64::from(input_mode == "ai_and_manual"),
                selection_mode,
                max_items,
                applies_to,
                facet_kind,
                num_min,
                num_max,
                num_unit,
                num_decimals,
                num_step,
                now,
            ],
        )?;
        if changed == 0 {
            return Err(AppError::not_found("分面不存在"));
        }
    }
    tx.commit()?;
    get(conn, key)
}

/// W2-3：删除报告（数字与 W2-4 get_impact 一致，可互相印证）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetDeleteReport {
    pub tags_deleted: i64,
    pub unlinked: i64,
    pub ops_deleted: i64,
    pub items_deleted: i64,
    /// V24：级联删除的数值行数（asset_facet_numbers）
    pub numbers_deleted: i64,
}

/// W2-3：物理删除分面 + 全级联。删除顺序（不可调换）：
/// ① asset_tags（让 trg_at_ad 跑，刷 FTS）
/// ② tag_ops（防 undo_batch JOIN 到已删 tag）
/// ③ ai_suggestion_items（facet_key 裸 TEXT 无外键）
/// ④ tag_aliases → ⑤ tags → ⑥ tag_facets
/// 系统分面拒绝（Q2：key 有大量代码/文档引用）。ai_suggestions.suggested_tags JSON 保留（AI 原始返回可追溯）。
pub fn delete_facet(conn: &Connection, key: &str) -> AppResult<FacetDeleteReport> {
    let f = get(conn, key)?;
    if f.is_system {
        return Err(AppError::msg("系统分面不能删除（只允许停用）"));
    }
    delete_facet_cascade(conn, key)
}

/// 供一次性产品迁移删除已下线的系统分面；普通命令仍必须经过 [`delete_facet`] 的系统保护。
pub(crate) fn delete_facet_cascade(conn: &Connection, key: &str) -> AppResult<FacetDeleteReport> {
    let tx = conn.unchecked_transaction()?;
    // V24（§6.5）：数值级联 —— 必须在同一事务内先删 asset_facet_numbers（铁律 6：先删引用再删主体）
    let numbers_deleted = crate::db::facet_numbers::delete_facet_numbers(&tx, key)?;
    let unlinked = tx.execute(
        "DELETE FROM asset_tags WHERE tag_id IN (SELECT id FROM tags WHERE facet_key = ?1)",
        [key],
    )? as i64;
    let ops_deleted = tx.execute(
        "DELETE FROM tag_ops WHERE tag_id IN (SELECT id FROM tags WHERE facet_key = ?1)",
        [key],
    )? as i64;
    let items_deleted = tx.execute(
        "DELETE FROM ai_suggestion_items WHERE facet_key = ?1",
        [key],
    )? as i64;
    tx.execute(
        "DELETE FROM tag_aliases WHERE tag_id IN (SELECT id FROM tags WHERE facet_key = ?1)",
        [key],
    )?;
    let tags_deleted = tx.execute("DELETE FROM tags WHERE facet_key = ?1", [key])? as i64;
    tx.execute("DELETE FROM tag_facets WHERE key = ?1", [key])?;
    tx.commit()?;
    Ok(FacetDeleteReport {
        tags_deleted,
        unlinked,
        ops_deleted,
        items_deleted,
        numbers_deleted: numbers_deleted as i64,
    })
}

/// 停用（软停用，保留历史引用与全部 cfg_* 配置）。
/// F7：允许系统分面停用（设置页「已停用折叠区」就是为 color 这类设计的）；
/// 「不能删除」仍是唯一保留项（delete_facet 检查 is_system）。
pub fn deactivate(conn: &Connection, key: &str) -> AppResult<()> {
    let f = get(conn, key)?;
    if f.is_system && !is_core_system_ai_facet(key) {
        return Err(AppError::unsupported("该系统分面由应用内部管理，不能停用"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let n = conn.execute(
        "UPDATE tag_facets SET status='inactive', updated_at=?1 WHERE key=?2 AND status='active'",
        params![now, key],
    )?;
    if n == 0 && f.status != "inactive" {
        return Err(AppError::msg("分面不存在或已停用"));
    }
    Ok(())
}

/// 恢复。
pub fn restore(conn: &Connection, key: &str) -> AppResult<()> {
    let f = get(conn, key)?;
    if f.is_system && !is_core_system_ai_facet(key) {
        return Err(AppError::unsupported("该系统分面由应用内部管理，不能恢复"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let n = conn.execute(
        "UPDATE tag_facets SET status='active', updated_at=?1 WHERE key=?2",
        params![now, key],
    )?;
    if n == 0 {
        return Err(AppError::msg("分面不存在"));
    }
    Ok(())
}

/// 停用前的影响范围（指导书 §12.3）：标签数量、引用素材数量、AI 配置数量。
pub fn get_impact(conn: &Connection, key: &str) -> AppResult<FacetImpact> {
    let tag_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    let asset_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT at.asset_id) FROM asset_tags at
          JOIN tags t ON t.id = at.tag_id WHERE t.facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    // W2-4：ai_suggestion_items.facet_key 是裸 TEXT 无外键，删除时必须清；
    // tag_ops 同理（防 undo_batch JOIN 到已删 tag）。计数与 W2-3 实际删除量一致。
    let ai_suggestion_item_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_suggestion_items WHERE facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    let tag_op_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_ops o
          JOIN tags t ON t.id = o.tag_id WHERE t.facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    // R3-3：别名数（delete_facet 实际会连带删除，报告必须覆盖）
    let alias_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_aliases a
          JOIN tags t ON t.id = a.tag_id WHERE t.facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    // V24（§6.5）：数值行数（delete_facet 同事务级联删除，报告必须覆盖）
    let number_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM asset_facet_numbers WHERE facet_key = ?1",
        [key],
        |r| r.get(0),
    )?;
    Ok(FacetImpact {
        tag_count,
        asset_count,
        ai_suggestion_item_count,
        tag_op_count,
        alias_count,
        number_count,
    })
}

/// W2-10：AI 返回分类 key 的唯一路由入口。三条分支：
/// ① DB 里存在该 key → 原样返回（自建分面走这条）
/// ② 中文旧名表命中且该 key 在 DB → 返回映射结果
/// ③ 都不中 → custom + warning（绝不静默丢进 custom：必须留痕）
///
/// 返回 (facet_key, 旧名映射到的 key)。第二个返回值仅用于日志区分来源。
pub fn resolve_facet_key(conn: &Connection, raw: &str) -> AppResult<(String, String)> {
    let raw = raw.trim();
    let exists = |key: &str| -> bool {
        conn.query_row("SELECT 1 FROM tag_facets WHERE key = ?1", [key], |_| Ok(()))
            .is_ok()
    };
    // ① DB 直存
    if exists(raw) {
        return Ok((raw.to_string(), raw.to_string()));
    }
    // ② 旧名表映射
    let mapped = key_for_legacy_name(raw);
    if mapped != raw && exists(mapped) {
        return Ok((mapped.to_string(), mapped.to_string()));
    }
    // ③ 兜底 custom（含警告：manual_only/停用分面落这里说明 AI 输出了不参与 AI 的分类）
    if mapped != raw || raw != "custom" {
        tracing::warn!(
            "AI 返回未知分面 key「{raw}」，路由到 custom（该分类不存在或不参与 AI 打标）"
        );
    }
    Ok(("custom".to_string(), "custom".to_string()))
}

/// 兼容旧 AI 分类显示名（纯函数：只查中文旧名表，**不再作为路由入口**——
/// W2-10 起分面 key 路由必须走 resolve_facet_key，它会查 DB 让自建分面生效）。
/// 仅保留给：解析历史 CategorizedTags JSON 的中文 key（老数据确实存着中文分类名）。
pub fn key_for_legacy_name(name: &str) -> &'static str {
    match name.trim() {
        "subject" => "subject",
        "scene" => "scene",
        "purpose" => "purpose",
        "color" => "color",
        "composition" => "composition",
        "lighting" => "lighting",
        "people" => "people",
        "technical" => "technical",
        "custom" => "custom",
        "主体" | "主体/对象" | "主体对象" | "物体" => "subject",
        "场景" | "场景/地点" => "scene",
        "用途" | "用途/项目类型" => "purpose",
        "色彩" | "色调" => "color",
        "构图" | "构图视角" | "构图/视角" => "composition",
        "光线" | "时间" | "光线/时间" | "光线/时间氛围" => "lighting",
        "人物" | "人物属性" | "人物/主体属性" => "people",
        "技术" | "可用性/技术特征" => "technical",
        _ => "custom",
    }
}

/// W2-1：tag_facets 是唯一事实源 —— 直接从 DB 读，只取参与 AI 的分面。
/// V20 合表后 settings.aiFacetConfigs 的语义已全部搬进 tag_facets，不再传参。
/// 一次消灭③诊断的四类 bug：两个保存通道、两个显示名、孤儿条目、缺条目静默不参与 AI。
///
/// F4：筛选条件改用 EFF_AI（cfg_ai_assignable 事实源，取代旧 input_mode 列判断），
/// 并按 `applies_to` 消费 media_kind —— 视频专属分面不再污染图片批次提示词：
/// - `media_kind = "all"`：返回全部参与 AI 的分面（超级搜索词典需要跨类型全量）；
/// - `media_kind = "image" | "video"`：只取 `applies_to IN ('all', media_kind)`。
pub fn build_prompt_context(
    conn: &Connection,
    media_kind: &str,
) -> AppResult<Vec<FacetPromptContext>> {
    if !matches!(media_kind, "all" | "image" | "video") {
        return Err(AppError::msg("media_kind 只允许 all | image | video"));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT key, display_name, description, selection_mode, max_items,
                facet_kind, num_min, num_max, num_unit, num_decimals, num_step
           FROM tag_facets f
          WHERE {EFF_AI}
            AND (?1 = 'all' OR f.applies_to = 'all' OR f.applies_to = ?1)
          ORDER BY f.sort_order"
    ))?;
    let out = stmt
        .query_map(params![media_kind], |r| {
            Ok(FacetPromptContext {
                key: r.get(0)?,
                display_name: r.get(1)?,
                description: r.get(2)?,
                selection_mode: r.get(3)?,
                max_items: r.get(4)?,
                facet_kind: r.get(5)?,
                num_min: r.get(6)?,
                num_max: r.get(7)?,
                num_unit: r.get(8)?,
                num_decimals: r.get(9)?,
                num_step: r.get(10)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(out)
}
/// V24（Phase 7-7）：分面类型设置（新建数值分面的第二落点 / 编辑数值配置）。
/// - `number → tag` 直接禁止（§6.6 规则 9：连续值退化成离散标签不可逆）；
/// - `tag → number` 仅当该分面没有任何标签时允许（新分面直建）；
///   已有标签必须走 `convert_facet_kind` 转换预览（不静默丢数据）；
/// - 数值分面上再次调用 = 只调整 num_* 配置。
// 8 参数为分面类型/数值配置的内聚集合，收进结构体需同步改全部调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn set_facet_kind(
    conn: &Connection,
    key: &str,
    kind: &str,
    num_min: Option<f64>,
    num_max: Option<f64>,
    num_unit: &str,
    num_decimals: i64,
    num_step: f64,
) -> AppResult<TagFacet> {
    let f = get(conn, key)?;
    if kind != "tag" && kind != "number" {
        return Err(AppError::msg("facet_kind 只允许 tag | number"));
    }
    if !num_step.is_finite() || num_step <= 0.0 {
        return Err(AppError::msg("步进必须是正数"));
    }
    if kind == "number"
        && (!(0..=10).contains(&num_decimals)
            || num_min.is_some_and(|value| !value.is_finite())
            || num_max.is_some_and(|value| !value.is_finite())
            || matches!((num_min, num_max), (Some(min), Some(max)) if min > max))
    {
        return Err(AppError::invalid_arg("数值范围、精度或步进无效"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    if f.facet_kind == "number" {
        if kind != "number" {
            return Err(AppError::msg(
                "数值分面不能改回标签型（连续值无法无损转成离散标签）",
            ));
        }
        conn.execute(
            "UPDATE tag_facets SET num_min=?1, num_max=?2, num_unit=?3, num_decimals=?4, num_step=?5, updated_at=?6 WHERE key=?7",
            params![num_min, num_max, num_unit, num_decimals, num_step, now, key],
        )?;
    } else if kind == "number" {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tags WHERE facet_key = ?1 AND status != 'deprecated'",
            [key],
            |r| r.get(0),
        )?;
        if n > 0 {
            return Err(AppError::msg(format!(
                "分面「{key}」已有 {n} 个标签 —— 请用「转换为数值型」先看转换预览，不能直接改型"
            )));
        }
        conn.execute(
            "UPDATE tag_facets SET facet_kind='number', num_min=?1, num_max=?2, num_unit=?3, num_decimals=?4, num_step=?5, updated_at=?6 WHERE key=?7",
            params![num_min, num_max, num_unit, num_decimals, num_step, now, key],
        )?;
    }
    get(conn, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn conn() -> Connection {
        init_memory().unwrap()
    }

    #[test]
    fn seed_system_facets_is_idempotent_and_preserves_existing() {
        let c = conn();
        // 全新库（V8 迁移已种）：再种一次不重复、不改已有行
        seed_system_facets(&c).unwrap();
        let n = list_all(&c).unwrap().len();
        seed_system_facets(&c).unwrap();
        assert_eq!(list_all(&c).unwrap().len(), n);
        // 已存在的行（含用户改名）不被覆盖
        c.execute(
            "UPDATE tag_facets SET display_name='我改过的名字' WHERE key='subject'",
            [],
        )
        .unwrap();
        seed_system_facets(&c).unwrap();
        assert_eq!(get(&c, "subject").unwrap().display_name, "我改过的名字");
    }

    #[test]
    fn seed_if_empty_rebuilds_with_color_inactive() {
        let c = conn();
        // 模拟历史「重置标签」清空 tag_facets 且未补种的库
        c.execute("DELETE FROM tag_facets", []).unwrap();
        assert!(list(&c).unwrap().is_empty());
        seed_system_facets_if_empty(&c).unwrap();
        let keys: Vec<String> = list(&c).unwrap().into_iter().map(|f| f.key).collect();
        assert!(keys.contains(&"subject".to_string()));
        assert!(
            !keys.contains(&"color".to_string()),
            "color 应补种为 inactive，不出现在 active 列表"
        );
        assert!(
            !keys.contains(&"style".to_string()),
            "style 已下线，不得重新补种"
        );
        assert_eq!(get(&c, "color").unwrap().status, "inactive");
        for key in ["purpose", "technical"] {
            assert!(get(&c, key).is_err(), "{key} 不应作为系统默认分类出现");
        }
        // 非空表不触发（用户自建分面不被打扰）
        seed_system_facets_if_empty(&c).unwrap();
        assert_eq!(
            list_all(&c)
                .unwrap()
                .iter()
                .filter(|f| !f.is_system)
                .count(),
            0
        );
    }

    #[test]
    fn seed_if_empty_noop_when_facets_exist() {
        let c = conn();
        // 模拟用户只留自建分面的库：不清空、也不强插系统分面
        c.execute("DELETE FROM tag_facets", []).unwrap();
        create(&c, "my_facet", "我的分面", "", "multi", None, "all").unwrap();
        seed_system_facets_if_empty(&c).unwrap();
        let keys: Vec<String> = list_all(&c).unwrap().into_iter().map(|f| f.key).collect();
        assert_eq!(keys, vec!["my_facet".to_string()]);
    }

    #[test]
    fn create_seeds_ai_facet_config_entry() {
        // V20 合表后：新建分面的「参与 AI」语义由 tag_facets.input_mode 承载（列默认
        // ai_and_manual = 参与），settings JSON 侧 ai_facet_configs 已死（skip_serializing）。
        let c = conn();
        create(&c, "my_facet", "我的分面", "", "multi", None, "all").unwrap();
        let mode: String = c
            .query_row(
                "SELECT input_mode FROM tag_facets WHERE key='my_facet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mode, "ai_and_manual", "新建用户分面默认参与 AI");
        // JSON 侧不得残留（旧语义的 seed_ai_config 已被 V20 取代）
        let raw: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='app_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        assert!(
            !raw.contains("my_facet"),
            "ai_facet_configs 已死，不得再写入"
        );
    }

    #[test]
    fn create_rejects_system_and_internal_keys_but_allows_legacy_user_keys() {
        let c = conn();
        for key in [
            "subject",
            "scene",
            "composition",
            "lighting",
            "people",
            "color",
            "custom",
            "description",
        ] {
            assert!(
                create(&c, key, "保留分类", "", "multi", None, "all").is_err(),
                "{key} must remain reserved"
            );
        }
        let purpose = create(&c, "purpose", "用途", "", "multi", None, "all").unwrap();
        let technical = create(&c, "technical", "技术特征", "", "multi", None, "all").unwrap();
        assert!(!purpose.is_system);
        assert!(!technical.is_system);
    }

    #[test]
    fn save_facet_persists_all_fields_for_number_facets() {
        let c = conn();
        let saved = save_facet(
            &c,
            &FacetSaveInput {
                key: "custom_count".into(),
                display_name: "自定义人数".into(),
                description: "画面内的人数".into(),
                input_mode: "manual_only".into(),
                selection_mode: "multi".into(),
                max_items: Some(4),
                applies_to: "all".into(),
                facet_kind: "number".into(),
                num_min: Some(0.0),
                num_max: Some(50.0),
                num_unit: "人".into(),
                num_decimals: 1,
                num_step: 0.5,
            },
        )
        .unwrap();
        assert_eq!(saved.input_mode, "manual_only");
        assert!(!saved.cfg_ai_assignable);
        assert_eq!(saved.facet_kind, "number");
        assert_eq!(saved.num_min, Some(0.0));
        assert_eq!(saved.num_max, Some(50.0));
        assert_eq!(saved.num_decimals, 1);
        assert_eq!(saved.num_step, 0.5);
    }

    #[test]
    fn save_facet_rolls_back_all_fields_when_database_rejects_update() {
        let c = conn();
        let original = create(
            &c,
            "atomic_facet",
            "原名称",
            "原说明",
            "multi",
            Some(3),
            "all",
        )
        .unwrap();
        c.execute_batch(
            "CREATE TRIGGER reject_atomic_facet_update BEFORE UPDATE ON tag_facets
             WHEN NEW.key='atomic_facet' AND NEW.display_name='触发回滚'
             BEGIN SELECT RAISE(ABORT, 'test rollback'); END;",
        )
        .unwrap();

        assert!(save_facet(
            &c,
            &FacetSaveInput {
                key: "atomic_facet".into(),
                display_name: "触发回滚".into(),
                description: "新说明".into(),
                input_mode: "manual_only".into(),
                selection_mode: "multi".into(),
                max_items: Some(8),
                applies_to: "video".into(),
                facet_kind: "number".into(),
                num_min: Some(1.0),
                num_max: Some(9.0),
                num_unit: "项".into(),
                num_decimals: 2,
                num_step: 0.5,
            },
        )
        .is_err());

        let after = get(&c, "atomic_facet").unwrap();
        assert_eq!(after.display_name, original.display_name);
        assert_eq!(after.description, original.description);
        assert_eq!(after.input_mode, original.input_mode);
        assert_eq!(after.max_items, original.max_items);
        assert_eq!(after.applies_to, original.applies_to);
        assert_eq!(after.facet_kind, "tag");
    }

    #[test]
    fn validate_key_enforces_snake_case() {
        assert_eq!(validate_key("clothing_color").unwrap(), "clothing_color");
        assert!(validate_key("镜头语言").is_err());
        assert!(validate_key("1bad").is_err());
        assert!(validate_key("a").is_err()); // 太短
        assert!(validate_key("has space").is_err());
        assert!(validate_key("UPPER").is_ok()); // 自动小写
    }

    #[test]
    fn create_sets_user_facet_and_rejects_duplicate() {
        let c = conn();
        let f = create(
            &c,
            "clothing_color",
            "衣服颜色",
            "描述",
            "multi",
            Some(3),
            "image",
        )
        .unwrap();
        assert_eq!(f.key, "clothing_color");
        assert!(!f.is_system);
        assert_eq!(f.applies_to, "image");
        assert_eq!(f.max_items, Some(3));
        // duplicate key rejected
        assert!(create(&c, "clothing_color", "重复", "", "multi", Some(3), "all").is_err());
        // bad selection mode rejected
        assert!(create(&c, "another", "名", "", "singlex", Some(3), "all").is_err());
    }

    #[test]
    fn single_mode_forces_max_items_one() {
        let c = conn();
        let f = create(&c, "pick_one", "单选", "", "single", Some(5), "all").unwrap();
        assert_eq!(f.selection_mode, "single");
        assert_eq!(f.max_items, Some(1)); // single 强制 1
    }

    #[test]
    fn deactivate_restore_roundtrip_for_user_and_core_system_facets() {
        let c = conn();
        let f = create(&c, "mood", "氛围", "", "multi", None, "all").unwrap();
        deactivate(&c, &f.key).unwrap();
        assert_eq!(get(&c, &f.key).unwrap().status, "inactive");
        // 停用后 list()（active）不含它
        assert!(!list(&c).unwrap().iter().any(|x| x.key == "mood"));
        restore(&c, &f.key).unwrap();
        assert_eq!(get(&c, &f.key).unwrap().status, "active");
        // 五个面向用户的内置 AI 分类允许停用/恢复，但不允许删除。
        let sys = get(&c, "subject").unwrap();
        assert!(sys.is_system);
        deactivate(&c, &sys.key).unwrap();
        assert_eq!(get(&c, &sys.key).unwrap().status, "inactive");
        restore(&c, &sys.key).unwrap();
        assert_eq!(get(&c, &sys.key).unwrap().status, "active", "停用→恢复对称");
        // 系统分面仍不能物理删除
        assert!(delete_facet(&c, &sys.key).is_err());

        // 内部兼容分面不暴露给普通分类管理操作。
        assert!(deactivate(&c, "color").is_err());
        assert!(restore(&c, "color").is_err());
    }

    #[test]
    fn impact_counts_tags_and_assets() {
        let c = conn();
        let f = create(&c, "impact_facet", "影响", "", "multi", None, "all").unwrap();
        // 造一个 tag 挂在分面下（tags 表需要 name/facet_key）
        c.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('红', '红', '红', ?1, 0, 'active', 0)",
            [&f.key],
        )
        .unwrap();
        let impact = get_impact(&c, &f.key).unwrap();
        assert_eq!(impact.tag_count, 1);
        assert_eq!(impact.asset_count, 0);
        assert_eq!(impact.alias_count, 0, "无别名时 alias_count 应为 0");
    }

    /// R3-3：delete_facet 会连带删 tag_aliases，影响报告必须含别名计数。
    #[test]
    fn impact_counts_aliases_in_facet() {
        let c = conn();
        let f = create(
            &c,
            "impact_alias_facet",
            "影响别名",
            "",
            "multi",
            None,
            "all",
        )
        .unwrap();
        let tag_id: i64 = c
            .query_row(
                "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
                 VALUES ('红', '红', '红', ?1, 0, 'active', 0) RETURNING id",
                [&f.key],
                |r| r.get(0),
            )
            .unwrap();
        c.execute(
            "INSERT INTO tag_aliases (tag_id, alias, normalized_alias, is_searchable, created_at)
             VALUES (?1, '赤', '赤', 1, 1)",
            [tag_id],
        )
        .unwrap();
        let impact = get_impact(&c, &f.key).unwrap();
        assert_eq!(impact.alias_count, 1, "分面下别名应计入影响报告");
        assert_eq!(impact.tag_count, 1);
    }

    /// 回归（真机：设置页标签与分类 AI/手工两组全空白）：
    /// list_all 返回的 TagFacet 必须含 input_mode（FACET_COLS 漏列 → 前端 inputMode 恒 undefined → 两组全空）。
    #[test]
    fn list_all_includes_input_mode_for_ui_grouping() {
        let c = conn();
        // 自建一个分面并改成 manual_only（create 默认 ai_and_manual）
        create(
            &c,
            "w7_manual",
            "手工专属",
            "仅手工填写",
            "multi",
            None,
            "all",
        )
        .unwrap();
        update_facet(
            &c,
            "w7_manual",
            "手工专属",
            "仅手工填写",
            "manual_only",
            "multi",
            None,
            "all",
        )
        .unwrap();
        let facets = list_all(&c).unwrap();
        let manual = facets
            .iter()
            .find(|f| f.key == "w7_manual")
            .expect("自建分面应列出");
        assert_eq!(manual.input_mode, "manual_only");
        let scene = facets
            .iter()
            .find(|f| f.key == "scene")
            .expect("系统分面应列出");
        assert_eq!(scene.input_mode, "ai_and_manual");
        // 序列化给前端必须 camelCase inputMode（FacetManagePanel/Workbench 分组依据）
        let json = serde_json::to_value(&facets).unwrap();
        let first = json.as_array().unwrap()[0].as_object().unwrap();
        assert!(
            first.contains_key("inputMode"),
            "序列化 JSON 必须含 inputMode（camelCase）"
        );
    }
}
