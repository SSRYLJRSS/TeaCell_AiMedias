//! 标签仓储：CRUD + 递归树 + 连带计数（父标签 = 自身+后代去重素材数）

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

// ═══════════════ F4：可见性三常量（各一个声明处，消费点全部引用） ═══════════════
// ⚠ COALESCE(t.status,'active') 兼容历史 NULL status；EXISTS 分面子查询判有效值。

/// 导航可见（侧栏、标签树、TagAssignDialog）：分面 active 且 cfg_visible_in_navigation=1
pub(crate) const NAV_VISIBLE_TAG: &str =
    "COALESCE(t.status,'active') = 'active' AND EXISTS (SELECT 1 FROM tag_facets f \
      WHERE f.key = COALESCE(t.facet_key,'custom') \
        AND f.status = 'active' AND f.cfg_visible_in_navigation = 1)";

/// 可搜索。⚠ 不看 f.status —— 停用分面的标签仍可搜（F4 语义变更）
pub(crate) const SEARCHABLE_TAG: &str =
    "COALESCE(t.status,'active') = 'active' AND EXISTS (SELECT 1 FROM tag_facets f \
      WHERE f.key = COALESCE(t.facet_key,'custom') AND f.cfg_searchable = 1)";

/// 可进 AI 提示词：分面 active 且 cfg_ai_assignable=1
pub(crate) const AI_ASSIGNABLE_TAG: &str =
    "COALESCE(t.status,'active') = 'active' AND EXISTS (SELECT 1 FROM tag_facets f \
      WHERE f.key = COALESCE(t.facet_key,'custom') \
        AND f.status = 'active' AND f.cfg_ai_assignable = 1)";

/// F4：分面生命周期是否有效（存在且 active，不看 cfg_*）。详情页据此对
/// 「分面已停用/已删除」的标签打「已停用/孤儿」角标（get_asset_tags 不过滤恒显示）。
pub(crate) const FACET_EFFECTIVE: &str = "EXISTS (SELECT 1 FROM tag_facets f \
      WHERE f.key = COALESCE(t.facet_key,'custom') AND f.status = 'active')";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub canonical_name: String,
    pub normalized_name: String,
    pub facet_key: String,
    pub parent_id: Option<i64>,
    pub status: String,
    pub is_system: bool,
    pub is_preset: bool,
    pub sort_order: i64,
    /// 自身直接关联素材数
    pub asset_count: i64,
    /// 自身+后代合计（去重；父标签显示值）
    pub total_count: i64,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// F8：path 仅展示用（侧栏面包屑），禁止用于身份判断、导出或任何持久化/协议语义。
    #[serde(default)]
    pub path: String,
    /// F4：所在分面生命周期是否有效（存在且 active）。UI 打「已停用」角标用。
    #[serde(default)]
    pub facet_effective: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagNode {
    pub tag: Tag,
    pub children: Vec<TagNode>,
}

pub fn normalize_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            'Ａ'..='Ｚ' => ((c as u32 - 'Ａ' as u32) as u8 + b'a') as char,
            'ａ'..='ｚ' => ((c as u32 - 'ａ' as u32) as u8 + b'a') as char,
            '０'..='９' => ((c as u32 - '０' as u32) as u8 + b'0') as char,
            _ => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 读取活动标签的所属分面，用于命令层校验父标签关系。
pub fn active_facet_key(conn: &Connection, id: i64) -> AppResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT facet_key FROM tags WHERE id = ?1 AND status = 'active'",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// 判断标签是否为系统标签。
pub fn is_system(conn: &Connection, id: i64) -> AppResult<bool> {
    Ok(conn
        .query_row(
            "SELECT is_system != 0 FROM tags WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// 子孙 id 集合（含自身）—— 递归 CTE
/// F1-d：WHERE d < 12 防环死循环兜底（环存在时无限递归会被 SQLite 10s timeout 杀）
fn descendant_ids(conn: &Connection, id: i64) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id, d) AS (
           SELECT ?1, 0 UNION ALL
           SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
            WHERE s.d < 12
         ) SELECT id FROM sub",
    )?;
    let ids = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

/// 连带计数：标签及其后代关联的去重素材数
pub fn total_count(conn: &Connection, id: i64) -> AppResult<i64> {
    let n: i64 = conn.query_row(
        "WITH RECURSIVE sub(id, d) AS (
           SELECT ?1, 0 UNION ALL
           SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
            WHERE s.d < 12
         )
         SELECT COUNT(DISTINCT asset_id) FROM asset_tags WHERE tag_id IN (SELECT id FROM sub)",
        [id],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// 全量标签树（百级标签规模，逐标签 CTE 计数毫秒级，架构 §1.4 已论证）
/// F4：可见性收口 NAV_VISIBLE_TAG —— 分面 active + cfg_visible_in_navigation=1 才显示
pub fn list_tree(conn: &Connection) -> AppResult<Vec<TagNode>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.name, COALESCE(t.canonical_name, t.name),
                COALESCE(t.normalized_name, lower(trim(t.name))),
                COALESCE(t.facet_key, 'custom'), t.parent_id,
                COALESCE(t.status, 'active'), COALESCE(t.is_system, 0),
                t.is_preset, t.sort_order,
                (SELECT COUNT(*) FROM asset_tags at WHERE at.tag_id = t.id) AS asset_count,
                {FACET_EFFECTIVE} AS facet_effective
           FROM tags t WHERE {NAV_VISIBLE_TAG}
          ORDER BY t.sort_order, t.id",
    ))?;
    let mut tags: Vec<Tag> = stmt
        .query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                canonical_name: r.get(2)?,
                normalized_name: r.get(3)?,
                facet_key: r.get(4)?,
                parent_id: r.get(5)?,
                status: r.get(6)?,
                is_system: r.get::<_, i64>(7)? != 0,
                is_preset: r.get::<_, i64>(8)? != 0,
                sort_order: r.get(9)?,
                asset_count: r.get(10)?,
                total_count: 0,
                aliases: Vec::new(),
                path: String::new(),
                facet_effective: r.get::<_, i64>(11)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    for t in &mut tags {
        t.total_count = total_count(conn, t.id)?;
        t.aliases = aliases(conn, t.id)?;
        // F1-d：chain 递归加 d < 12 上限（防环死循环；path 仅展示用）
        t.path = conn
            .query_row(
                "WITH RECURSIVE chain(id, name, parent_id, depth) AS (
               SELECT id, name, parent_id, 0 FROM tags WHERE id = ?1
               UNION ALL SELECT t.id, t.name, t.parent_id, c.depth + 1
                 FROM tags t JOIN chain c ON t.id = c.parent_id
                WHERE c.depth < 12
             ) SELECT group_concat(name, ' / ') FROM (SELECT name FROM chain ORDER BY depth DESC)",
                [t.id],
                |r| r.get::<_, Option<String>>(0),
            )?
            .unwrap_or_else(|| t.name.clone());
    }

    // 组树
    fn build(parent: Option<i64>, tags: &[Tag]) -> Vec<TagNode> {
        tags.iter()
            .filter(|t| t.parent_id == parent)
            .map(|t| TagNode {
                tag: t.clone(),
                children: build(Some(t.id), tags),
            })
            .collect()
    }
    Ok(build(None, &tags))
}

pub fn create(conn: &Connection, name: &str, parent_id: Option<i64>) -> AppResult<Tag> {
    create_in_facet(conn, name, parent_id, None)
}

/// F5：在调用方已开事务时（如 ai.rs ai_decide_suggestion_item 内调 find_or_create_canonical）
/// 绝不嵌套开事务；无外层事务时才自己开并 commit，保证多写原子。
fn transactional<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> AppResult<T>,
) -> AppResult<T> {
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    } else {
        f(conn)
    }
}

/// F5：create_tag 命令的落库入口 —— 无 parent 且无 facet 时报错（不再默默落 custom）。
/// 规则收在 db 层单一位置，命令层只做名称校验。
pub fn create_tag_in_facet(
    conn: &Connection,
    name: &str,
    facet_key: Option<&str>,
    parent_id: Option<i64>,
) -> AppResult<Tag> {
    if facet_key.is_none() && parent_id.is_none() {
        return Err(crate::error::AppError::msg(
            "创建标签需要归属分面：请先选择分面再创建",
        ));
    }
    create_in_facet(conn, name, parent_id, facet_key)
}

/// 按 id 取单标签（F5 查重命中返回；列表场景不要用——逐条查询无批量优势）。
fn tag_by_id(conn: &Connection, id: i64) -> AppResult<Tag> {
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.name, COALESCE(t.canonical_name, t.name),
                COALESCE(t.normalized_name, lower(trim(t.name))),
                COALESCE(t.facet_key, 'custom'), t.parent_id,
                COALESCE(t.status, 'active'), COALESCE(t.is_system, 0),
                t.is_preset, t.sort_order, {FACET_EFFECTIVE} AS facet_effective
           FROM tags t WHERE t.id = ?1"
    ))?;
    let mut tag = stmt.query_row([id], |r| {
        Ok(Tag {
            id: r.get(0)?,
            name: r.get(1)?,
            canonical_name: r.get(2)?,
            normalized_name: r.get(3)?,
            facet_key: r.get(4)?,
            parent_id: r.get(5)?,
            status: r.get(6)?,
            is_system: r.get::<_, i64>(7)? != 0,
            is_preset: r.get::<_, i64>(8)? != 0,
            sort_order: r.get(9)?,
            facet_effective: r.get::<_, i64>(10)? != 0,
            asset_count: 0,
            total_count: 0,
            aliases: Vec::new(),
            path: String::new(),
        })
    })?;
    tag.total_count = total_count(conn, id)?;
    hydrate_metadata(conn, &mut tag)?;
    Ok(tag)
}

pub fn create_in_facet(
    conn: &Connection,
    name: &str,
    parent_id: Option<i64>,
    facet_key: Option<&str>,
) -> AppResult<Tag> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::error::AppError::msg("标签名称不能为空"));
    }
    let facet = match facet_key {
        Some(key) => key.to_string(),
        None => parent_id
            .and_then(|id| {
                conn.query_row("SELECT facet_key FROM tags WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .ok()
            })
            .unwrap_or_else(|| "custom".to_string()),
    };
    let normalized = normalize_name(name);
    // F5：根级「新词」创建自动查重（唯一入口 find_by_term，mode=Alias——同名或同义词都归并）。
    // 子标签按 (parent, name) 语义由 find_or_create_child 处理，不进此处全局查重。
    if parent_id.is_none() {
        let lookup = find_by_term(conn, &facet, &normalized, TermMatch::Alias)?;
        if let Some(hit) = lookup.hits.first() {
            if hit.tag_status == "active" {
                return tag_by_id(conn, hit.tag_id);
            }
        }
    }
    // F5：tag_unique_terms 启用时，同事务写 tag_terms 的 canonical 行（事实源写入收口）。
    // 绝不双写：启用时 canonical 唯一事实源是 tag_terms；未启用则完全不碰 tag_terms。
    transactional(conn, |c| {
        c.execute(
            "INSERT INTO tags (name, canonical_name, normalized_name, facet_key, parent_id)
             VALUES (?1, ?1, ?3, ?4, ?2)",
            rusqlite::params![name, parent_id, normalized, facet],
        )?;
        let id = c.last_insert_rowid();
        if crate::db::schema_features::feature_enabled(c, "tag_unique_terms").unwrap_or(false) {
            c.execute(
                "INSERT INTO tag_terms
                 (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
                 VALUES (?1, ?2, ?3, ?3, '', 'canonical', 1, ?4)",
                rusqlite::params![id, facet, normalized, chrono::Utc::now().timestamp_millis()],
            )?;
        }
        let facet_effective = c
            .query_row(
                "SELECT status = 'active' FROM tag_facets WHERE key = ?1",
                [&facet],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false);
        Ok(Tag {
            id,
            name: name.to_string(),
            canonical_name: name.to_string(),
            normalized_name: normalized,
            facet_key: facet,
            parent_id,
            status: "active".to_string(),
            is_system: false,
            is_preset: false,
            sort_order: 0,
            asset_count: 0,
            total_count: 0,
            aliases: Vec::new(),
            path: String::new(),
            facet_effective,
        })
    })
}

const CORE_FACET_KEYS: &[&str] = &["subject", "scene", "people"];

/// 核心分面默认词表：父节点用于浏览和宽泛筛选，叶子供 AI/手工打标。
const CORE_TAXONOMY: &[(&str, &str, &[&str])] = &[
    ("subject", "人物", &["人"]),
    (
        "subject",
        "动物",
        &["猫", "狗", "鸟", "鱼", "宠物", "野生动物"],
    ),
    ("subject", "植物", &["树木", "花卉", "绿植", "农作物"]),
    ("subject", "食物饮品", &["食物", "饮品", "茶", "咖啡"]),
    (
        "subject",
        "器物",
        &["产品", "家具", "器皿", "电子设备", "日用品", "工具"],
    ),
    (
        "subject",
        "建筑设施",
        &["建筑", "楼梯", "桥梁", "道路", "设施"],
    ),
    (
        "subject",
        "交通工具",
        &["汽车", "自行车", "船", "飞机", "列车"],
    ),
    (
        "subject",
        "自然景观",
        &["山", "水体", "天空", "云", "岩石", "雪"],
    ),
    ("subject", "其他", &[]),
    ("scene", "空间类型", &["室内", "户外", "半室内"]),
    (
        "scene",
        "环境类型",
        &["城市", "自然", "乡村", "工业", "商业", "交通"],
    ),
    (
        "scene",
        "场所类型",
        &[
            "公园",
            "街道",
            "海边",
            "湖边",
            "树林",
            "山地",
            "草地",
            "桥梁",
            "餐饮空间",
            "办公空间",
            "商业空间",
            "工业空间",
        ],
    ),
    ("scene", "其他", &[]),
    ("people", "人物状态", &["无人", "未知"]),
    ("people", "人数", &["单人", "双人", "多人", "人群"]),
    ("people", "性别", &["男性", "女性", "性别不明"]),
    (
        "people",
        "年龄段",
        &[
            "婴幼儿",
            "儿童",
            "青少年",
            "青年",
            "中年",
            "老年",
            "年龄不明",
        ],
    ),
    (
        "people",
        "穿着",
        &[
            "现代装",
            "古装",
            "民族服饰",
            "职业装",
            "制服",
            "礼服",
            "运动装",
            "休闲装",
            "泳装",
        ],
    ),
    (
        "people",
        "动作状态",
        &[
            "站立", "坐姿", "行走", "奔跑", "交谈", "工作", "表演", "休息",
        ],
    ),
    ("people", "其他", &[]),
];

/// 默认词表的常见同义说法。保留规范叶子，避免“女子/女性”等碎片。
const CORE_TAXONOMY_ALIASES: &[(&str, &str, &str)] = &[
    ("subject", "人", "人像"),
    ("subject", "树木", "树"),
    ("subject", "花卉", "花朵"),
    ("subject", "水体", "水面"),
    ("subject", "建筑", "建筑物"),
    ("scene", "户外", "室外"),
    ("scene", "街道", "城市街道"),
    ("scene", "街道", "路边"),
    ("scene", "海边", "海滩"),
    ("scene", "海边", "海岸"),
    ("scene", "湖边", "湖畔"),
    ("scene", "山地", "山景"),
    ("scene", "草地", "草坪"),
    ("scene", "工业空间", "工厂"),
    ("scene", "工业空间", "工业区"),
    ("scene", "工业空间", "厂房"),
    ("scene", "餐饮空间", "咖啡店"),
    ("scene", "餐饮空间", "餐厅"),
    ("people", "女性", "女子"),
    ("people", "女性", "女孩"),
    ("people", "女性", "女人"),
    ("people", "男性", "男子"),
    ("people", "男性", "男孩"),
    ("people", "男性", "男人"),
    ("people", "单人", "一个人"),
    ("people", "双人", "两个人"),
    ("people", "双人", "两人"),
    ("people", "多人", "三人以上"),
    ("people", "人群", "大量人群"),
    ("people", "婴幼儿", "婴儿"),
    ("people", "儿童", "孩子"),
    ("people", "青少年", "少年"),
    ("people", "青年", "年轻人"),
    ("people", "青年", "年轻"),
    ("people", "老年", "老人"),
    ("people", "老年", "年老"),
    ("people", "现代装", "现代服装"),
    ("people", "古装", "古代服饰"),
    ("people", "民族服饰", "少数民族服饰"),
    ("people", "职业装", "工作服"),
    ("people", "运动装", "运动服"),
    ("people", "休闲装", "便装"),
    ("people", "站立", "站姿"),
    ("people", "行走", "走路"),
    ("people", "行走", "步行"),
    ("people", "奔跑", "跑步"),
    ("people", "交谈", "交流"),
];

fn find_named_tag(
    conn: &Connection,
    facet_key: &str,
    parent_id: Option<i64>,
    name: &str,
) -> AppResult<Option<i64>> {
    let sql = if parent_id.is_some() {
        "SELECT id FROM tags WHERE facet_key = ?1 AND parent_id = ?2 AND name = ?3 LIMIT 1"
    } else {
        "SELECT id FROM tags WHERE facet_key = ?1 AND parent_id IS NULL AND name = ?2 LIMIT 1"
    };
    let id = if let Some(parent_id) = parent_id {
        conn.query_row(sql, rusqlite::params![facet_key, parent_id, name], |r| {
            r.get(0)
        })
        .optional()?
    } else {
        conn.query_row(sql, rusqlite::params![facet_key, name], |r| r.get(0))
            .optional()?
    };
    Ok(id)
}

fn ensure_named_tag(
    conn: &Connection,
    facet_key: &str,
    parent_id: Option<i64>,
    name: &str,
) -> AppResult<i64> {
    if let Some(id) = find_named_tag(conn, facet_key, parent_id, name)? {
        return Ok(id);
    }
    Ok(create_in_facet(conn, name, parent_id, Some(facet_key))?.id)
}

fn core_other_parent(conn: &Connection, facet_key: &str) -> AppResult<i64> {
    let parent_id = ensure_named_tag(conn, facet_key, None, "其他")?;
    conn.execute(
        "UPDATE tags SET is_system = 1, is_preset = 1 WHERE id = ?1",
        [parent_id],
    )?;
    Ok(parent_id)
}

fn seed_core_taxonomy_inner(conn: &Connection) -> AppResult<()> {
    for (group_index, (facet_key, group_name, leaves)) in CORE_TAXONOMY.iter().enumerate() {
        let parent_id = ensure_named_tag(conn, facet_key, None, group_name)?;
        conn.execute(
            "UPDATE tags SET is_system = 1, is_preset = 1, sort_order = ?1, status = 'active'
              WHERE id = ?2",
            rusqlite::params![(group_index as i64 + 1) * 100, parent_id],
        )?;
        for (leaf_index, leaf) in leaves.iter().enumerate() {
            let leaf_id = ensure_named_tag(conn, facet_key, Some(parent_id), leaf)?;
            conn.execute(
                "UPDATE tags SET is_system = 0, is_preset = 0, sort_order = ?1, status = 'active'
                  WHERE id = ?2",
                rusqlite::params![
                    (group_index as i64 + 1) * 100 + leaf_index as i64 + 1,
                    leaf_id
                ],
            )?;
        }
    }
    for (facet_key, canonical, alias) in CORE_TAXONOMY_ALIASES {
        let tag_id = conn.query_row(
            "SELECT id FROM tags
              WHERE facet_key = ?1 AND name = ?2
              ORDER BY parent_id IS NULL, id
              LIMIT 1",
            rusqlite::params![facet_key, canonical],
            |r| r.get::<_, i64>(0),
        )?;
        add_alias(conn, tag_id, alias, None, "synonym")?;
    }
    Ok(())
}

/// 幂等补齐核心词表别名，覆盖已存在的非空用户库。
///
/// 核心标签只在空库时播种，但别名属于搜索协议的一部分，后续版本新增的
/// 同义词仍需安全地补进存量库。`add_alias` 已按 feature gate 选择唯一事实源。
pub fn ensure_core_taxonomy_aliases(conn: &Connection) -> AppResult<()> {
    transactional(conn, |c| {
        for (facet_key, canonical, alias) in CORE_TAXONOMY_ALIASES {
            let tag_id: Option<i64> = c
                .query_row(
                    "SELECT id FROM tags
                      WHERE facet_key = ?1 AND name = ?2
                      ORDER BY parent_id IS NULL, id
                      LIMIT 1",
                    rusqlite::params![facet_key, canonical],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(tag_id) = tag_id {
                add_alias(c, tag_id, alias, None, "synonym")?;
            }
        }
        Ok(())
    })
}

/// 幂等播种三个核心分面的默认层级词表。
pub fn seed_core_taxonomy(conn: &Connection) -> AppResult<()> {
    transactional(conn, seed_core_taxonomy_inner)
}

/// 仅当标签表没有任何非废弃标签时播种，避免覆盖已有用户词表。
pub fn seed_core_taxonomy_if_empty(conn: &Connection) -> AppResult<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE COALESCE(status, 'active') != 'deprecated'",
        [],
        |r| r.get(0),
    )?;
    if count == 0 {
        seed_core_taxonomy(conn)?;
    }
    Ok(())
}

/// 新协议使用的规范标签创建：分面是独立实体，标签直接归属分面。
/// 核心分面的新词优先落到「其他」父类；普通/自建分面保持根级创建。
/// F3-a/F5：查重已收口在 create_in_facet（find_by_term mode=Alias）。
pub fn find_or_create_canonical(conn: &Connection, facet_key: &str, name: &str) -> AppResult<i64> {
    let normalized = normalize_name(name);
    if normalized.is_empty() {
        return Err(crate::error::AppError::msg("标签名称不能为空"));
    }
    if CORE_FACET_KEYS.contains(&facet_key) {
        let lookup = find_by_term(conn, facet_key, &normalized, TermMatch::Alias)?;
        if let Some(hit) = lookup.hits.first() {
            if hit.tag_status == "active" {
                return Ok(hit.tag_id);
            }
        }
        let parent_id = core_other_parent(conn, facet_key)?;
        return find_or_create_child(conn, parent_id, name);
    }
    Ok(create_in_facet(conn, name, None, Some(facet_key))?.id)
}

pub fn update(
    conn: &Connection,
    id: i64,
    name: Option<&str>,
    parent_id: Option<Option<i64>>,
) -> AppResult<()> {
    transactional(conn, |c| {
        if let Some(n) = name {
            let normalized = normalize_name(n);
            c.execute(
                "UPDATE tags SET name = ?1, canonical_name = ?1, normalized_name = ?2 WHERE id = ?3",
                rusqlite::params![n, normalized, id],
            )?;
            // F5：改名同步 tag_terms 的 canonical 行（启用时 tag_terms 是事实源，必须跟着走）
            if crate::db::schema_features::feature_enabled(c, "tag_unique_terms").unwrap_or(false) {
                c.execute(
                    "UPDATE tag_terms SET term = ?1, normalized_term = ?2
                      WHERE tag_id = ?3 AND term_kind = 'canonical'",
                    rusqlite::params![n, normalized, id],
                )?;
            }
        }
        if let Some(pid) = parent_id {
            // ① 改 parent 校验同分面（V22a 触发器同规则兜底——双层防护，应用层给清晰报错）
            if let Some(new_parent) = pid {
                let cur_facet: Option<String> = c
                    .query_row("SELECT facet_key FROM tags WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                let par_facet: Option<String> = c
                    .query_row(
                        "SELECT facet_key FROM tags WHERE id = ?1",
                        [new_parent],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let (Some(cf), Some(pf)) = (cur_facet, par_facet) {
                    if cf != pf {
                        return Err(crate::error::AppError::msg("标签不能挂到其他分面下"));
                    }
                }
            }
            // ③ 防环改由触发器守（V22a trg_tags_no_cycle）——不再自实现 descendant_ids，
            //    避免「应用层防环逻辑自身被环卡死」；触发器 RAISE 回滚本次 UPDATE。
            c.execute(
                "UPDATE tags SET parent_id = ?1 WHERE id = ?2",
                rusqlite::params![pid, id],
            )?;
        }
        Ok(())
    })
}

/// 删除标签：CASCADE 删除子标签与 asset_tags 关联（FTS 由触发器联动）
pub fn delete(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM tags WHERE id = ?1", [id])?;
    Ok(())
}

pub fn update_preserve_alias(
    conn: &Connection,
    id: i64,
    name: Option<&str>,
    parent_id: Option<Option<i64>>,
) -> AppResult<()> {
    let old_name: Option<String> = if name.is_some() {
        Some(conn.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get(0))?)
    } else {
        None
    };
    update(conn, id, name, parent_id)?;
    if let (Some(old), Some(new)) = (old_name, name) {
        if old != new {
            add_alias(conn, id, &old, None, "old_name")?;
        }
    }
    Ok(())
}

pub fn deactivate(conn: &Connection, id: i64) -> AppResult<()> {
    // F1-d：递归 CTE 加 d < 12 上限（防环死循环）
    let changed = conn.execute(
        "WITH RECURSIVE sub(id, d) AS (
           SELECT ?1, 0 UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
            WHERE s.d < 12
         ) UPDATE tags SET status = 'deprecated' WHERE id IN (SELECT id FROM sub)",
        [id],
    )?;
    if changed == 0 {
        return Err(crate::error::AppError::msg("标签不存在"));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagFacetGovernance {
    pub facet_key: String,
    pub tag_count: i64,
    pub active_tag_count: i64,
    pub deprecated_tag_count: i64,
    pub linked_asset_count: i64,
    pub alias_count: i64,
    pub pending_ai_item_count: i64,
}

pub fn governance(conn: &Connection) -> AppResult<Vec<TagFacetGovernance>> {
    let mut stmt = conn.prepare(
        "SELECT f.key,
                COUNT(DISTINCT t.id),
                COUNT(DISTINCT CASE WHEN t.status = 'active' THEN t.id END),
                COUNT(DISTINCT CASE WHEN t.status = 'deprecated' THEN t.id END),
                COUNT(DISTINCT at.asset_id),
                COUNT(DISTINCT ta.id),
                COUNT(DISTINCT CASE WHEN asi.decision = 'pending' THEN asi.id END)
           FROM tag_facets f
           LEFT JOIN tags t ON t.facet_key = f.key
           LEFT JOIN asset_tags at ON at.tag_id = t.id
           LEFT JOIN tag_aliases ta ON ta.tag_id = t.id
           LEFT JOIN ai_suggestion_items asi ON asi.facet_key = f.key
          GROUP BY f.key, f.sort_order
          ORDER BY f.sort_order, f.key",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TagFacetGovernance {
                facet_key: r.get(0)?,
                tag_count: r.get(1)?,
                active_tag_count: r.get(2)?,
                deprecated_tag_count: r.get(3)?,
                linked_asset_count: r.get(4)?,
                alias_count: r.get(5)?,
                pending_ai_item_count: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn add_alias(
    conn: &Connection,
    tag_id: i64,
    alias: &str,
    locale: Option<&str>,
    alias_type: &str,
) -> AppResult<()> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Ok(());
    }
    let normalized = normalize_name(alias);
    let locale = locale.unwrap_or("");
    // F5-d：tag_unique_terms 是唯一开关，绝不双写。
    //   =1：只写 tag_terms（tag_aliases 冻结只读），冲突由 ux_terms 唯一索引自动完成，
    //       应用层把 UNIQUE 错误翻译成人话；
    //   =0：只写 tag_aliases（tag_terms 不读不写），维持原行为。
    if crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false) {
        let facet: String =
            conn.query_row("SELECT facet_key FROM tags WHERE id = ?1", [tag_id], |r| {
                r.get(0)
            })?;
        let result = conn.execute(
            "INSERT INTO tag_terms
             (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
            rusqlite::params![
                tag_id,
                facet,
                normalized,
                alias,
                locale,
                alias_type, // synonym / old_name / translation / typo（与旧表 alias_type 同词）
                chrono::Utc::now().timestamp_millis()
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => {
                // 幂等：同一标签重复加同一个词 → 忽略（对应旧表 INSERT OR IGNORE）
                let self_hit: Option<i64> = conn
                    .query_row(
                        "SELECT 1 FROM tag_terms
                          WHERE tag_id = ?1 AND normalized_term = ?2 AND locale = ?3
                          LIMIT 1",
                        rusqlite::params![tag_id, normalized, locale],
                        |r| r.get(0),
                    )
                    .ok();
                if self_hit.is_some() {
                    return Ok(());
                }
                // ux_terms 已自动拦截冲突；查出占用者给可读消息
                let conflict_owner: Option<String> = conn
                    .query_row(
                        "SELECT t.name FROM tag_terms tt JOIN tags t ON t.id = tt.tag_id
                          WHERE tt.facet_key = ?1 AND tt.normalized_term = ?2 AND tt.tag_id != ?3
                          LIMIT 1",
                        rusqlite::params![facet, normalized, tag_id],
                        |r| r.get(0),
                    )
                    .ok();
                let message = match conflict_owner {
                    Some(owner) => format!("「{alias}」已被分面内「{owner}」占用"),
                    None => format!("「{alias}」已被其他标签占用"),
                };
                Err(crate::error::AppError::msg(message))
            }
            Err(e) => Err(e.into()),
        }
    } else {
        // 旧表路径（F6-c）：冲突按分面 + locale —— 跨分面同名允许
        // （people 用了「一个人」作别名，不影响 subject 再用「一个人」）。
        let conflict: Option<i64> = conn
            .query_row(
                "SELECT ta.tag_id FROM tag_aliases ta
                   JOIN tags t ON t.id = ta.tag_id
                   JOIN tags me ON me.id = ?3
                  WHERE ta.normalized_alias = ?1 AND ta.locale = ?2
                    AND ta.tag_id != ?3 AND t.facet_key = me.facet_key
                  LIMIT 1",
                rusqlite::params![normalized, locale, tag_id],
                |r| r.get(0),
            )
            .ok();
        if conflict.is_some() {
            return Err(crate::error::AppError::msg("别名已绑定到其他规范标签"));
        }
        conn.execute(
            "INSERT OR IGNORE INTO tag_aliases
             (tag_id, alias, normalized_alias, locale, alias_type, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                tag_id,
                alias,
                normalized,
                locale,
                alias_type,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }
}

/// rusqlite UNIQUE 约束失败判定（SQLITE_CONSTRAINT_UNIQUE = 2067）。
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(ferr, _) => {
            ferr.extended_code == 2067 || ferr.code == rusqlite::ErrorCode::ConstraintViolation
        }
        _ => false,
    }
}

pub fn aliases(conn: &Connection, tag_id: i64) -> AppResult<Vec<String>> {
    // P0-4（铁律 9）：tag_unique_terms=1 后 tag_aliases 冻结只读、新词全写 tag_terms ——
    // Tag.aliases 必须同 gate 读 tag_terms，否则 gate 开启后别名列表恒空。
    if crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false) {
        let mut stmt = conn.prepare(
            "SELECT term FROM tag_terms
              WHERE tag_id = ?1 AND term_kind != 'canonical'
              ORDER BY created_at, term",
        )?;
        let rows = stmt.query_map([tag_id], |r| r.get(0))?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut stmt = conn.prepare("SELECT alias FROM tag_aliases WHERE tag_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([tag_id], |r| r.get(0))?;
    let aliases = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(aliases)
}

/// 只返回可用于搜索和 AI 词典的别名。
///
/// `tag_unique_terms` 开启后，`tag_terms` 是唯一事实源；关闭时兼容旧的
/// `tag_aliases`。调用方不应自行选择表，避免新旧库出现不同解析结果。
pub fn searchable_aliases(conn: &Connection, tag_id: i64) -> AppResult<Vec<String>> {
    if crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false) {
        let mut stmt = conn.prepare(
            "SELECT term FROM tag_terms
              WHERE tag_id = ?1 AND term_kind != 'canonical' AND is_searchable = 1
              ORDER BY created_at, term",
        )?;
        let rows = stmt.query_map([tag_id], |r| r.get(0))?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut stmt = conn.prepare(
        "SELECT alias FROM tag_aliases
          WHERE tag_id = ?1 AND is_searchable = 1
          ORDER BY id",
    )?;
    let rows = stmt.query_map([tag_id], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn hydrate_metadata(conn: &Connection, tag: &mut Tag) -> AppResult<()> {
    tag.aliases = aliases(conn, tag.id)?;
    // F1-d：chain 递归加 d < 12 上限（防环死循环；path 仅展示用）
    tag.path = conn
        .query_row(
            "WITH RECURSIVE chain(id, name, parent_id, depth) AS (
               SELECT id, name, parent_id, 0 FROM tags WHERE id = ?1
               UNION ALL SELECT t.id, t.name, t.parent_id, c.depth + 1
                 FROM tags t JOIN chain c ON t.id = c.parent_id
                WHERE c.depth < 12
             ) SELECT group_concat(name, ' / ') FROM (SELECT name FROM chain ORDER BY depth DESC)",
            [tag.id],
            |r| r.get::<_, Option<String>>(0),
        )?
        .unwrap_or_else(|| tag.name.clone());
    Ok(())
}

pub fn list_by_facet(conn: &Connection, facet_key: &str) -> AppResult<Vec<TagNode>> {
    let tree = list_tree(conn)?;
    fn filter(nodes: Vec<TagNode>, facet: &str) -> Vec<TagNode> {
        nodes
            .into_iter()
            .filter_map(|mut node| {
                let children = filter(node.children, facet);
                if node.tag.facet_key == facet || !children.is_empty() {
                    node.children = children;
                    Some(node)
                } else {
                    None
                }
            })
            .collect()
    }
    Ok(filter(tree, facet_key))
}

/// W2-9 + F7：提示词候选词 —— 高频词优先（标签收敛更快），**按分面各取 Top-n**
/// （ROW_NUMBER() OVER PARTITION BY facet_key），不再全库 LIMIT n —— 分面多时靠后的分面
/// 不再拿不到候选词。核心分面优先完整候选词，其他分面仍按高频优先。
/// 标签名 >12 字截断；总输出 5000 字符上限按分面数**均摊配额**，
/// 不再用 break 直接丢弃整个分面（谁被丢不由 facet_key 字母序决定）。
/// 供 W5a 提示词拼入候选词（「含义相同就用已有的词」约束的事实基础）。
pub fn top_tags_per_facet(conn: &Connection, n: usize) -> AppResult<Vec<(String, String)>> {
    // F4：AI_ASSIGNABLE_TAG —— 停用分面的标签不得作为「已有候选词」喂给 AI
    let mut stmt = conn.prepare(&format!(
        "SELECT facet_key, name FROM (
            SELECT t.facet_key, t.name, COUNT(at.asset_id) AS uses,
                   ROW_NUMBER() OVER (
                     PARTITION BY t.facet_key
                     ORDER BY
                       CASE WHEN t.facet_key IN ('subject', 'scene', 'people') THEN 0 ELSE 1 END,
                       CASE WHEN t.facet_key IN ('subject', 'scene', 'people') THEN t.sort_order END,
                       COUNT(at.asset_id) DESC,
                       t.id
                   ) AS rn
              FROM tags t LEFT JOIN asset_tags at ON at.tag_id = t.id
             WHERE {AI_ASSIGNABLE_TAG}
               AND NOT EXISTS (
                 SELECT 1 FROM tags child
                  WHERE child.parent_id = t.id AND COALESCE(child.status, 'active') = 'active'
               )
             GROUP BY t.id
         ) ranked
          WHERE rn <= ?1
          ORDER BY facet_key, rn",
    ))?;
    let rows: Vec<(String, String)> = stmt
        .query_map([n.max(1) as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // 按分面分组聚合成 "facet_key: 词1/词2/..."，超 12 字的词截断；
    // F7：总量 5000 字符上限按分面数均摊（每分面至少保留 1 个词），不再 break 丢整个分面
    const CAP: usize = 5000;
    let mut by_facet: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (facet, name) in rows {
        let short: String = name.chars().take(12).collect();
        by_facet.entry(facet).or_default().push(short);
    }
    if by_facet.is_empty() {
        return Ok(Vec::new());
    }
    let per_facet_cap = (CAP / by_facet.len()).max(1);
    let mut out: Vec<(String, String)> = Vec::new();
    for (facet, words) in by_facet {
        // 贪心凑词直到分面配额；超长首词也保留（每分面至少 1 个词）
        let mut line = String::new();
        for w in &words {
            let cost =
                line.chars().count() + if line.is_empty() { 0 } else { 1 } + w.chars().count();
            if cost > per_facet_cap && !line.is_empty() {
                break;
            }
            if !line.is_empty() {
                line.push('/');
            }
            line.push_str(w);
        }
        out.push((facet, line));
    }
    Ok(out)
}

pub fn search_candidates(
    conn: &Connection,
    facet_key: Option<&str>,
    query: &str,
) -> AppResult<Vec<Tag>> {
    let normalized = normalize_name(query);
    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    let (alias_join, alias_column) = if terms_enabled {
        (
            "LEFT JOIN tag_terms tt ON tt.tag_id = t.id
              AND tt.term_kind != 'canonical' AND tt.is_searchable = 1",
            "tt.normalized_term",
        )
    } else {
        (
            "LEFT JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1",
            "ta.normalized_alias",
        )
    };
    // F4：可搜性收口 SEARCHABLE_TAG（不再内联判 f.status='active'）
    let mut sql = format!(
        "SELECT DISTINCT t.id, t.name, COALESCE(t.canonical_name,t.name),
                COALESCE(t.normalized_name,lower(trim(t.name))), COALESCE(t.facet_key,'custom'),
                t.parent_id, COALESCE(t.status,'active'), COALESCE(t.is_system,0),
                t.is_preset, t.sort_order,
                (SELECT COUNT(*) FROM asset_tags at WHERE at.tag_id=t.id),
                {FACET_EFFECTIVE}
           FROM tags t {alias_join}
          WHERE {SEARCHABLE_TAG}
            AND (COALESCE(t.normalized_name,lower(trim(t.name))) LIKE ?1
              OR {alias_column} LIKE ?1)"
    );
    if facet_key.is_some() {
        sql.push_str(" AND COALESCE(t.facet_key,'custom') = ?2");
    }
    sql.push_str(" ORDER BY t.sort_order, t.id LIMIT 100");
    let pattern = format!("%{normalized}%");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = if let Some(facet) = facet_key {
        stmt.query_map(rusqlite::params![pattern, facet], tag_from_row)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(rusqlite::params![pattern], tag_from_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    for tag in &mut rows {
        tag.total_count = total_count(conn, tag.id)?;
        hydrate_metadata(conn, tag)?;
    }
    Ok(rows)
}

fn tag_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: r.get(0)?,
        name: r.get(1)?,
        canonical_name: r.get(2)?,
        normalized_name: r.get(3)?,
        facet_key: r.get(4)?,
        parent_id: r.get(5)?,
        status: r.get(6)?,
        is_system: r.get::<_, i64>(7)? != 0,
        is_preset: r.get::<_, i64>(8)? != 0,
        sort_order: r.get(9)?,
        asset_count: r.get(10)?,
        total_count: 0,
        aliases: Vec::new(),
        path: String::new(),
        facet_effective: r.get::<_, i64>(11)? != 0,
    })
}

/// 合并标签（M3-01 R-19）：src 的素材关联与子标签全部并入 dst，随后删除 src。
/// 单事务；走 DELETE+INSERT 而非 UPDATE 改挂，保证 FTS 触发器（trg_at_ai/ad）联动。
///
/// F5 + F6-c：tag_unique_terms 启用时，src 的全部词条（canonical + 别名）按关键顺序迁移到 dst：
///   ① 读出源全部 terms → ② DELETE src 的 terms（释放 ux_terms 唯一空间）→
///   ③ 逐个写给 dst（源 canonical 作 synonym —— 合并 = 语义等价永久可搜；改名才写 old_name），
///     撞唯一约束跳过 → ④ src 改名「原名 #<id>」（避开旧 UNIQUE(parent_id,name)）→
///   ⑤ status 置 deprecated。不按此顺序，源的 canonical 与目标既有词条会 UNIQUE constraint failed。
pub fn merge_preserve_alias(conn: &Connection, src_id: i64, dst_id: i64) -> AppResult<()> {
    if src_id == dst_id {
        return Err(crate::error::AppError::msg("不能把标签合并到它自己"));
    }
    // 防环：目标不能是源标签的后代（否则子标签回挂后树结构错乱）
    if descendant_ids(conn, src_id)?.contains(&dst_id) {
        return Err(crate::error::AppError::msg(
            "不能把标签合并到它自己的子标签下",
        ));
    }
    let (src_facet, dst_facet): (String, String) = conn.query_row(
        "SELECT s.facet_key, d.facet_key FROM tags s JOIN tags d ON d.id = ?2 WHERE s.id = ?1",
        rusqlite::params![src_id, dst_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if src_facet != dst_facet {
        return Err(crate::error::AppError::msg("不同分面的标签不能合并"));
    }
    // 预检子标签同名冲突（tags 表 UNIQUE(parent_id, name)）
    let clash: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags a JOIN tags b
           ON a.parent_id = ?1 AND b.parent_id = ?2 AND a.name = b.name",
        rusqlite::params![src_id, dst_id],
        |r| r.get(0),
    )?;
    if clash > 0 {
        return Err(crate::error::AppError::msg(
            "目标标签下已有同名子标签，请先重命名后再合并",
        ));
    }

    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    transactional(conn, |c| {
        let src_name: String =
            c.query_row("SELECT name FROM tags WHERE id = ?1", [src_id], |r| {
                r.get(0)
            })?;
        // ① src 独有素材 → 挂到 dst（INSERT 触发 FTS 更新）
        c.execute(
            "INSERT INTO asset_tags
             (asset_id, tag_id, source, created_at, confidence, confirmation, confirmed_at, confirmed_by, source_batch_id)
             SELECT at.asset_id, ?2, at.source, at.created_at, at.confidence, at.confirmation,
                    at.confirmed_at, at.confirmed_by, at.source_batch_id FROM asset_tags at
              WHERE at.tag_id = ?1
                AND NOT EXISTS (SELECT 1 FROM asset_tags x WHERE x.asset_id = at.asset_id AND x.tag_id = ?2)",
            rusqlite::params![src_id, dst_id],
        )?;
        // ② 删除 src 全部关联（DELETE 触发 FTS 更新；已挂 dst 的素材去重生效）
        c.execute("DELETE FROM asset_tags WHERE tag_id = ?1", [src_id])?;
        // ③ src 的子标签回挂 dst（保留层级）
        c.execute(
            "UPDATE tags SET parent_id = ?2 WHERE parent_id = ?1",
            rusqlite::params![src_id, dst_id],
        )?;
        if terms_enabled {
            // ① terms 顺序迁移（F5 关键）：读 → 删（释放唯一空间）→ 写（撞了跳过）
            struct SrcTerm {
                normalized_term: String,
                term: String,
                locale: String,
                term_kind: String,
                is_searchable: i64,
            }
            let mut stmt = c.prepare(
                "SELECT normalized_term, term, locale, term_kind, is_searchable
                   FROM tag_terms WHERE tag_id = ?1 ORDER BY term_kind, term",
            )?;
            let src_terms: Vec<SrcTerm> = stmt
                .query_map([src_id], |r| {
                    Ok(SrcTerm {
                        normalized_term: r.get(0)?,
                        term: r.get(1)?,
                        locale: r.get(2)?,
                        term_kind: r.get(3)?,
                        is_searchable: r.get(4)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            // ② 释放 src 的唯一空间
            c.execute("DELETE FROM tag_terms WHERE tag_id = ?1", [src_id])?;
            // ③ 写给 dst（F6-c：合并 = 语义等价 → canonical 降级为 synonym，永久可搜；
            //    改名才写 old_name）。撞了（目标已有同词）→ 跳过：目标词优先。
            for t in &src_terms {
                let kind = if t.term_kind == "canonical" {
                    "synonym"
                } else {
                    t.term_kind.as_str()
                };
                let insert = c.execute(
                    "INSERT INTO tag_terms
                     (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        dst_id,
                        dst_facet,
                        t.normalized_term,
                        t.term,
                        t.locale,
                        kind,
                        t.is_searchable,
                        chrono::Utc::now().timestamp_millis()
                    ],
                );
                match insert {
                    Ok(_) => {}
                    // 撞了（目标已有同词）→ 跳过：目标词优先
                    Err(e) if is_unique_violation(&e) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            // ④ src 改名「原名 #<id>」避开旧 UNIQUE(parent_id,name)；⑤ 置 deprecated
            let renamed = format!("原名 #{}", src_id);
            c.execute(
                "UPDATE tags SET name = ?1, canonical_name = ?1, normalized_name = ?2 WHERE id = ?3",
                rusqlite::params![renamed, normalize_name(&renamed), src_id],
            )?;
            c.execute(
                "UPDATE tags SET status = 'deprecated' WHERE id = ?1",
                [src_id],
            )?;
        } else {
            // 旧表路径：④ src 置 deprecated（物理删除语义由 merge() 兼容包装负责）
            c.execute(
                "UPDATE tags SET status = 'deprecated' WHERE id = ?1",
                [src_id],
            )?;
            add_alias(c, dst_id, &src_name, None, "old_name")?;
        }
        Ok(())
    })
}

/// 旧版兼容合并：保持原有“源标签物理删除、旧名称不再命中”的语义。
/// 新界面必须使用 merge_preserve_alias。
pub fn merge(conn: &Connection, src_id: i64, dst_id: i64) -> AppResult<()> {
    let src_name: String =
        conn.query_row("SELECT name FROM tags WHERE id=?1", [src_id], |r| r.get(0))?;
    merge_preserve_alias(conn, src_id, dst_id)?;
    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM tags WHERE id=?1", [src_id])?;
    if terms_enabled {
        // 物理删除语义：dst 上刚挂的 synonym（src canonical 迁移）删掉（旧名称不再命中）
        tx.execute(
            "DELETE FROM tag_terms WHERE tag_id=?1 AND term_kind='synonym' AND normalized_term=?2",
            rusqlite::params![dst_id, normalize_name(&src_name)],
        )?;
    } else {
        tx.execute(
            "DELETE FROM tag_aliases WHERE tag_id=?1 AND alias_type='old_name' AND normalized_alias=?2",
            rusqlite::params![dst_id, normalize_name(&src_name)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// 按名称查找/创建根级标签（AI 打标确认写入用）。
/// F5：查重/落分面已收口在 create_in_facet（根级同名或同义词直接返回已有标签）。
pub fn find_or_create_root(conn: &Connection, name: &str) -> AppResult<i64> {
    Ok(create_in_facet(conn, name, None, Some("custom"))?.id)
}

pub fn find_or_create_facet_root(
    conn: &Connection,
    facet_key: &str,
    display_name: &str,
) -> AppResult<i64> {
    let mut stmt = conn.prepare(
        "SELECT id FROM tags WHERE parent_id IS NULL AND facet_key = ?1
          AND status = 'active' ORDER BY is_system DESC, id LIMIT 1",
    )?;
    let mut rows = stmt.query([facet_key])?;
    if let Some(row) = rows.next()? {
        return Ok(row.get(0)?);
    }
    drop(rows);
    drop(stmt);
    let tag = create_in_facet(conn, display_name, None, Some(facet_key))?;
    conn.execute(
        "UPDATE tags SET is_system = 1, is_preset = 1 WHERE id = ?1",
        [tag.id],
    )?;
    Ok(tag.id)
}

/// 按名称查找/创建子标签（PRD 5.5：AI 分类标签，分类=父标签）
pub fn find_or_create_child(conn: &Connection, parent_id: i64, name: &str) -> AppResult<i64> {
    let mut stmt = conn.prepare("SELECT id FROM tags WHERE name = ?1 AND parent_id = ?2")?;
    let mut rows = stmt.query(rusqlite::params![name, parent_id])?;
    if let Some(row) = rows.next()? {
        return Ok(row.get(0)?);
    }
    drop(rows);
    drop(stmt);
    Ok(create_in_facet(conn, name, Some(parent_id), None)?.id)
}

/// 旧版兼容入口：不再自动播种自由标签。
///
/// 过去这里会创建“人像、风景、美食、街拍……”等标签。当前 AI 使用
/// facet + canonical tag，这些预置词既不是必要词表，也容易和用户自己的
/// 标签重复，因此保留函数名但改为空操作，避免外部旧调用失效。
pub fn seed_presets(conn: &Connection) -> AppResult<()> {
    retire_unused_presets(conn)?;
    Ok(())
}

// ═══════════════ F2-a：V22b 冲突预检（六类，只读） ═══════════════

/// 分面内 term 冲突组（规范名↔规范名 / 规范名↔别名 / 别名↔别名 归一后去重）。
/// 用于设置页展示「「海边」有 2 个条目（关联 12/3 张素材），合并到哪个？」
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TermConflictGroup {
    pub facet_key: String,
    pub term: String,
    pub entries: Vec<TermConflictEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TermConflictEntry {
    /// 标签 id（tag_aliases 的归属标签）
    pub tag_id: i64,
    pub name: String,
    /// canonical | alias
    pub kind: String,
    pub linked_assets: i64,
}

/// 孤儿标签（facet_key 指向不存在分面）—— 建议迁到 custom，需用户确认。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanTag {
    pub id: i64,
    pub name: String,
    pub facet_key: String,
}

/// 跨分面挂父的标签（子标签 facet 与父 facet 不一致）—— 自动断开层级。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossFacetChild {
    pub id: i64,
    pub name: String,
    pub parent_id: i64,
    pub own_facet: String,
    pub parent_facet: String,
}

/// 环边（断开最后一条边即可修复）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleEdge {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}

/// tag_terms.facet_key 与 tags.facet_key 不一致（V22b 首次迁移时为空；后续 apply 时查）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetMismatch {
    pub tag_id: i64,
    pub tag_name: String,
    pub terms_facet: String,
    pub tag_facet: String,
}

/// V22b 预检结果汇总。conflicts.is_empty() 才允许启用 tag_unique_terms 等约束。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagConflictReport {
    pub term_conflicts: Vec<TermConflictGroup>,
    pub orphans: Vec<OrphanTag>,
    pub cross_facet_children: Vec<CrossFacetChild>,
    pub cycle_edges: Vec<CycleEdge>,
    pub over_deep_subtrees: Vec<i64>,
    pub facet_mismatches: Vec<FacetMismatch>,
}

impl TagConflictReport {
    /// 是否零冲突（干净库 = 没有需要人工处理的 term 冲突；自动可修项也并入判断：
    /// V22b 只在「无任何冲突」时直接启用 —— 有自动可修项也应让用户先知情）。
    pub fn is_clean(&self) -> bool {
        self.term_conflicts.is_empty()
            && self.orphans.is_empty()
            && self.cross_facet_children.is_empty()
            && self.cycle_edges.is_empty()
            && self.over_deep_subtrees.is_empty()
            && self.facet_mismatches.is_empty()
    }

    pub fn total(&self) -> usize {
        self.term_conflicts.len()
            + self.orphans.len()
            + self.cross_facet_children.len()
            + self.cycle_edges.len()
            + self.over_deep_subtrees.len()
            + self.facet_mismatches.len()
    }
}

/// F2-a：V22b 前置预检 —— 只读，不修改任何数据。
/// 六类：
///  ① 分面内 term 冲突（规范名↔规范名 / 规范名↔别名 / 别名↔别名，三类都查）
///  ② facet_key 指向不存在分面的孤儿标签
///  ③ 跨分面挂父的标签
///  ④ 环（带深度上限的递归 CTE 探测）
///  ⑤ 超过 8 层的子树
///  ⑥ tag_terms.facet_key 与 tags.facet_key 不一致（首次迁移时为空，后续 apply 时查）
pub fn detect_tag_conflicts(conn: &Connection) -> AppResult<TagConflictReport> {
    // ── ① 分面内 term 冲突：canonical 与可搜别名归一到同一集合，(facet_key, term)
    //    出现 ≥2 个不同标签即冲突（ux_terms 唯一索引的前置检查）──
    let term_conflicts: Vec<TermConflictGroup> = {
        // 冲突组：(facet_key, term) 至少命中 2 个不同标签
        let mut group_stmt = conn.prepare(
            "SELECT facet_key, term FROM (
               SELECT t.facet_key, COALESCE(t.normalized_name, lower(trim(t.name))) AS term, t.id
                 FROM tags t WHERE t.status = 'active'
               UNION ALL
               SELECT t.facet_key, ta.normalized_alias, t.id
                 FROM tag_aliases ta JOIN tags t ON t.id = ta.tag_id
                WHERE t.status = 'active' AND ta.is_searchable = 1
             ) GROUP BY facet_key, term
             HAVING COUNT(DISTINCT id) > 1
             ORDER BY facet_key, term",
        )?;
        let groups: Vec<(String, String)> = group_stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        let mut out = Vec::new();
        for (facet, term) in groups {
            let mut members: Vec<TermConflictEntry> = Vec::new();
            // canonical 来源
            let mut canonical = conn.prepare(
                "SELECT t.id, t.name,
                        (SELECT COUNT(*) FROM asset_tags at2 WHERE at2.tag_id = t.id)
                   FROM tags t WHERE t.status='active' AND t.facet_key=?1
                     AND COALESCE(t.normalized_name, lower(trim(t.name))) = ?2
                   ORDER BY t.id",
            )?;
            let mut rows = canonical.query(rusqlite::params![facet, term])?;
            while let Some(r) = rows.next()? {
                members.push(TermConflictEntry {
                    tag_id: r.get(0)?,
                    name: r.get(1)?,
                    kind: "canonical".into(),
                    linked_assets: r.get(2)?,
                });
            }
            // alias 来源（同一标签若已以 canonical 计入则不重复）
            let mut alias = conn.prepare(
                "SELECT t.id, t.name,
                        (SELECT COUNT(*) FROM asset_tags at2 WHERE at2.tag_id = t.id)
                   FROM tag_aliases ta JOIN tags t ON t.id = ta.tag_id
                  WHERE t.status='active' AND t.facet_key=?1 AND ta.is_searchable=1
                    AND ta.normalized_alias = ?2
                    AND NOT EXISTS (
                      SELECT 1 FROM tags tc WHERE tc.id = t.id AND tc.status='active'
                        AND COALESCE(tc.normalized_name, lower(trim(tc.name))) = ?2)
                   ORDER BY t.id",
            )?;
            let mut rows = alias.query(rusqlite::params![facet, term])?;
            while let Some(r) = rows.next()? {
                members.push(TermConflictEntry {
                    tag_id: r.get(0)?,
                    name: r.get(1)?,
                    kind: "alias".into(),
                    linked_assets: r.get(2)?,
                });
            }
            if members.len() > 1 {
                out.push(TermConflictGroup {
                    facet_key: facet,
                    term,
                    entries: members,
                });
            }
        }
        out
    };

    // ── ② 孤儿标签：facet_key 指向不存在的分面 ──
    let orphans: Vec<OrphanTag> = {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, t.facet_key FROM tags t
              WHERE t.status = 'active'
                AND NOT EXISTS (SELECT 1 FROM tag_facets f WHERE f.key = t.facet_key)
              ORDER BY t.facet_key, t.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(OrphanTag {
                id: r.get(0)?,
                name: r.get(1)?,
                facet_key: r.get(2)?,
            })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // ── ③ 跨分面挂父 ──
    let cross_facet_children: Vec<CrossFacetChild> = {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, t.parent_id, t.facet_key, p.facet_key
               FROM tags t JOIN tags p ON p.id = t.parent_id
              WHERE t.status='active' AND t.facet_key != p.facet_key
              ORDER BY t.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CrossFacetChild {
                id: r.get(0)?,
                name: r.get(1)?,
                parent_id: r.get(2)?,
                own_facet: r.get(3)?,
                parent_facet: r.get(4)?,
            })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // ── ④ 环（深度上限探测）：沿父链 12 步内回到自身即环成员 ──
    let cycle_edges: Vec<CycleEdge> = {
        let mut all = conn.prepare(
            "SELECT id, name, parent_id FROM tags
              WHERE parent_id IS NOT NULL AND status='active'",
        )?;
        let nodes: Vec<(i64, String, Option<i64>)> = all
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        let mut out = Vec::new();
        for (id, name, pid) in nodes {
            let mut cur = id;
            let mut is_cycle = false;
            for _ in 0..12 {
                let parent: Option<Option<i64>> = conn
                    .query_row("SELECT parent_id FROM tags WHERE id=?1", [cur], |r| {
                        r.get(0)
                    })
                    .ok();
                match parent {
                    Some(Some(p)) if p == id => {
                        is_cycle = true;
                        break;
                    }
                    Some(Some(p)) => cur = p,
                    _ => break,
                }
            }
            if is_cycle {
                out.push(CycleEdge {
                    id,
                    name,
                    parent_id: pid,
                });
            }
        }
        out
    };

    // ── ⑤ 深度 ≥ 8 的节点（超深子树成员；从根计 0）──
    let over_deep_subtrees: Vec<i64> = {
        let mut stmt = conn.prepare(
            "WITH RECURSIVE depth(id, d) AS (
               SELECT id, 0 FROM tags WHERE parent_id IS NULL AND status='active'
               UNION ALL
               SELECT t.id, d.d + 1 FROM tags t JOIN depth d ON t.parent_id = d.id
                WHERE t.status='active' AND d.d < 12
             )
             SELECT id FROM depth WHERE d >= 8 ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // ── ⑥ tag_terms.facet_key 与 tags.facet_key 不一致（表不存在 → 空）──
    let facet_mismatches: Vec<FacetMismatch> = {
        let table_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tag_terms'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if table_exists == 0 {
            Vec::new()
        } else {
            let mut stmt = conn.prepare(
                "SELECT tt.tag_id, COALESCE(t.name,''), tt.facet_key, t.facet_key
                   FROM tag_terms tt JOIN tags t ON t.id = tt.tag_id
                  WHERE tt.facet_key != t.facet_key
                  ORDER BY tt.tag_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FacetMismatch {
                    tag_id: r.get(0)?,
                    tag_name: r.get(1)?,
                    terms_facet: r.get(2)?,
                    tag_facet: r.get(3)?,
                })
            })?;
            rows.filter_map(|r| r.ok()).collect()
        }
    };

    Ok(TagConflictReport {
        term_conflicts,
        orphans,
        cross_facet_children,
        cycle_edges,
        over_deep_subtrees,
        facet_mismatches,
    })
}

/// 停用没有任何素材关联的旧预置标签。
/// 已经被用户使用过的预置标签不删除，保留历史搜索和素材关联。
pub fn retire_unused_presets(conn: &Connection) -> AppResult<usize> {
    let changed = conn.execute(
        "UPDATE tags SET status = 'deprecated'
          WHERE is_preset = 1
            AND status = 'active'
            AND NOT (is_system = 1 AND parent_id IS NULL)
            AND NOT EXISTS (SELECT 1 FROM asset_tags at WHERE at.tag_id = tags.id)
            AND NOT EXISTS (SELECT 1 FROM tags child WHERE child.parent_id = tags.id AND child.status = 'active')",
        [],
    )?;
    Ok(changed)
}

// ═══════════════ F3：find_by_term 单一入口 + next_prefix（S5 扩展复用） ═══════════════

/// 词匹配模式（F3 定义；S5 在 LeafCond::Tag 上扩展 term_match 复用同一枚举）。
/// `serde(rename_all)` 对齐前端 camelCase 契约。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TermMatch {
    /// 规范名精确（term_kind='canonical'）
    Exact,
    /// 规范名或任意别名精确（默认）
    #[default]
    Alias,
    /// 前缀（「青」→「青少年」）
    Prefix,
    /// 包含（「人」→「人物」「单人」「一个人」）
    Contains,
    /// 编辑距离 ≤ 1（「森材」→「森林」）
    Fuzzy,
}

/// F3-b：字典序的「下一个前缀」，作范围查询开区间上界。
/// 不变式：所有以 prefix 开头的字符串 s 都满足 prefix <= s < next_prefix(prefix)。
/// ⚠ 不能用 `prefix + '\u{FFFF}'` —— 实测 UTF-8 下 U+1F600（😀）字节序大于 U+FFFF，
///   会漏掉含 emoji 的标签。遍历字符递增并自动跳过 surrogate。
pub fn next_prefix(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let mut cp = last as u32 + 1;
        while cp <= 0x10FFFF {
            if let Some(c) = char::from_u32(cp) {
                let mut out: String = chars.iter().collect();
                out.push(c);
                return Some(out);
            }
            cp += 1;
        }
        // 该字符已到顶 → 丢掉它，对前一个字符继续
    }
    None // 空串或全是 char::MAX → 无上界，调用方只用下界
}

/// 分面内按 term 查标签的**唯一入口**（F3-a）。
/// 唯一索引（ux_terms，tag_unique_terms 启用后）保证最多一行 —— 不再需要 ORDER BY 兜底。
/// 消灭四处重复 SQL 与 `ORDER BY id LIMIT 1`：
///   - find_or_create_canonical（mode=Alias）
///   - ai::set_suggestion_tags 的 tag_id 反查（mode=Alias）
///   - ai::final_pairs 反查（mode=Alias）
///   - search_candidates 保留（模糊候选，见 F4 的 SEARCHABLE_TAG）
///
/// F5-d feature gate：tag_unique_terms=1 时读 tag_terms（事实源），=0 时读旧表
/// （tags + tag_aliases）。分支收在本函数一处，上层不自己判断 feature。
pub fn find_by_term(
    conn: &Connection,
    facet_key: &str,
    normalized: &str,
    mode: TermMatch,
) -> AppResult<TermLookup> {
    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    let mut warnings = Vec::new();
    let mut hits = Vec::new();
    if terms_enabled {
        // tag_terms 事实源：Prefix/Contains/Fuzzy 交给 S5 词扩展（多命中 + 上限 + 文案）
        if matches!(
            mode,
            TermMatch::Prefix | TermMatch::Contains | TermMatch::Fuzzy
        ) {
            let cap = match mode {
                TermMatch::Prefix => PREFIX_EXPAND_CAP,
                TermMatch::Contains => CONTAINS_EXPAND_CAP,
                _ => FUZZY_EXPAND_CAP,
            };
            let (h, w) = expand_term_query(conn, facet_key, normalized, mode, cap)?;
            return Ok(TermLookup {
                hits: h,
                warnings: w,
            });
        }
        // Exact / Alias：精确单点（唯一索引保证最多一行）
        let kind_filter = match mode {
            TermMatch::Exact => "AND term_kind = 'canonical'",
            TermMatch::Alias => "",
            _ => unreachable!(),
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT tag_id, term_kind, term FROM tag_terms
              WHERE facet_key = ?1 AND normalized_term = ?2 {kind_filter}
              ORDER BY term_kind = 'canonical' DESC, term_kind, term LIMIT 1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![facet_key, normalized])?;
        if let Some(r) = rows.next()? {
            let tag_id: i64 = r.get(0)?;
            let term_kind: String = r.get(1)?;
            let matched: String = r.get(2)?;
            let status: String = conn
                .query_row(
                    "SELECT COALESCE(status,'active') FROM tags WHERE id=?1",
                    [tag_id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "active".into());
            if term_kind != "canonical" {
                let canonical: Option<String> = conn
                    .query_row("SELECT name FROM tags WHERE id=?1", [tag_id], |r| r.get(0))
                    .ok();
                warnings.push(
                    canonical
                        .map(|c| format!("「{matched}」已归入「{c}」"))
                        .unwrap_or_else(|| format!("「{matched}」是别名，已归入其规范标签")),
                );
            }
            hits.push(TermHit {
                tag_id,
                term_kind,
                matched_term: matched,
                tag_status: status,
            });
        }
    } else if matches!(
        mode,
        TermMatch::Prefix | TermMatch::Contains | TermMatch::Fuzzy
    ) {
        // F5-d：tag_unique_terms=0 时前缀/包含/纠错匹配不可用（需 tag_terms 事实源 + 区间扫描）
        warnings.push("前缀/包含/纠错匹配需先在设置页启用标签约束".into());
    } else {
        // 旧表（tags + tag_aliases）。Exact 只查 tags（规范名）；Alias 才并别名
        let sql = if matches!(mode, TermMatch::Exact) {
            "SELECT t.id, 'canonical', t.name, COALESCE(t.status,'active')
               FROM tags t
              WHERE t.facet_key=?1 AND t.status='active' AND t.normalized_name=?2
              ORDER BY t.id LIMIT 1"
        } else {
            "SELECT t.id, 'canonical', t.name, COALESCE(t.status,'active')
               FROM tags t
              WHERE t.facet_key=?1 AND t.status='active' AND t.normalized_name=?2
              UNION ALL
             SELECT t.id, 'alias', ta.alias, COALESCE(t.status,'active')
               FROM tag_aliases ta JOIN tags t ON t.id=ta.tag_id
              WHERE t.facet_key=?1 AND t.status='active' AND ta.is_searchable=1
                AND ta.normalized_alias=?2
              ORDER BY 1 LIMIT 1"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params![facet_key, normalized])?;
        if let Some(r) = rows.next()? {
            let tag_id: i64 = r.get(0)?;
            let term_kind: String = r.get(1)?;
            let matched: String = r.get(2)?;
            let status: String = r.get(3)?;
            if term_kind != "canonical" {
                let canonical: Option<String> = conn
                    .query_row("SELECT name FROM tags WHERE id=?1", [tag_id], |r| r.get(0))
                    .ok();
                warnings.push(
                    canonical
                        .map(|c| format!("「{matched}」已归入「{c}」"))
                        .unwrap_or_else(|| format!("「{matched}」是别名，已归入其规范标签")),
                );
            }
            hits.push(TermHit {
                tag_id,
                term_kind,
                matched_term: matched,
                tag_status: status,
            });
        }
    }
    Ok(TermLookup { hits, warnings })
}

/// find_by_term 的返回。
#[derive(Debug, Clone, Default)]
pub struct TermLookup {
    pub hits: Vec<TermHit>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TermHit {
    pub tag_id: i64,
    /// canonical 时无需提示；别名命中要告知用户
    pub term_kind: String,
    pub matched_term: String,
    /// active / deprecated（deprecated 不该出现，但要能诊断）
    pub tag_status: String,
}

// ═══════════════ F6：词表治理 —— 近似匹配（只提示，绝不自动合并） ═══════════════

/// 近似命中的原因（F6-b，按可信度降序）：
/// - `Substring`：一方是另一方子串且长度差 ≤ 2（「一个」→「一个人」）
/// - `Spell`：编辑距离 ≤ 1（「女孩」vs「女人」也可能命中——这正是只提示不合并的原因）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimilarReason {
    Substring,
    Spell,
}

/// 字符级编辑距离（中文按 char 计）。词表规模小（百级），O(m·n) 足够。
pub fn char_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

// ═══════════════ S5：五种匹配模式 —— 词扩展（Prefix / Contains / Fuzzy） ═══════════════

/// S5 词扩展上限：Prefix/Contains 各 10；Fuzzy 5（规范见指导书 S5 规格表）。
pub const PREFIX_EXPAND_CAP: usize = 10;
pub const CONTAINS_EXPAND_CAP: usize = 10;
pub const FUZZY_EXPAND_CAP: usize = 5;

fn like_escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// S5：Prefix 范围查询模板 —— `>= :lo AND < :hi`（走 ux_terms 索引；禁止 `LIKE 'x%'`，
/// 后者实测 5 万行 → SCAN）。facet_key 空 = 跨分面。参数位从 ?1/?2 起（调用方绑定）。
pub(crate) fn term_prefix_sql(facet_key: &str) -> String {
    let facet_pred = if facet_key.is_empty() {
        String::new()
    } else {
        "tt.facet_key = ?1 AND ".to_string()
    };
    let (a, b) = if facet_key.is_empty() { (1, 2) } else { (2, 3) };
    format!(
        "SELECT tt.tag_id, tt.term_kind, tt.term, COALESCE(t.status,'active')
           FROM tag_terms tt JOIN tags t ON t.id = tt.tag_id
          WHERE {facet_pred} tt.normalized_term >= ?{a} AND tt.normalized_term < ?{b}
          ORDER BY length(tt.term) ASC, tt.term, tt.tag_id"
    )
}

/// S5：把「按词查」扩展成一组标签命中（`LeafCond::Tag.term_query` 的唯一扩展入口；
/// find_by_term 的 Prefix/Contains/Fuzzy 也走这里）。
///
/// - `Prefix`：**范围查询** `normalized_term >= :lo AND < next_prefix(:lo)`（走 ux_terms
///   索引；`LIKE 'x%'` 实测不走索引，禁止）→ 名字长度升序。
/// - `Contains`：`LIKE '%' || ? || '%'`（全表扫，输入先转义）→ 名字长度升序。
/// - `Fuzzy`：两级 —— ① 首字符相同或 char 长度差 ≤ 1 缩候选（SQL）；② Rust 编辑距离
///   ≤ 1 → 按距离升序、同距按长度升序，取 cap。
///
/// feature gate：tag_unique_terms=0 时前缀/包含/纠错不可用（返回空 + warning，
/// 与 find_by_term 语义一致）。返回命中 + 给人看的 warning 文案（超 cap / 多命中列举）。
pub fn expand_term_query(
    conn: &Connection,
    facet_key: &str,
    normalized: &str,
    mode: TermMatch,
    cap: usize,
) -> AppResult<(Vec<TermHit>, Vec<String>)> {
    let mut warnings = Vec::new();
    if normalized.is_empty() {
        return Ok((Vec::new(), warnings));
    }
    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    if !terms_enabled {
        warnings.push("前缀/包含/纠错匹配需先在设置页启用标签约束".into());
        return Ok((Vec::new(), warnings));
    }
    let cap = cap.max(1);
    // facet_key 空 = 跨全部分面（AI/搜索框不指定分面时）
    let facet_pred = if facet_key.is_empty() {
        String::new()
    } else {
        "tt.facet_key = ?1 AND ".to_string()
    };
    let base = format!(
        "SELECT tt.tag_id, tt.term_kind, tt.term, COALESCE(t.status,'active')
           FROM tag_terms tt JOIN tags t ON t.id = tt.tag_id
          WHERE {facet_pred}"
    );
    let mut vals: Vec<rusqlite::types::Value> = Vec::new();
    if !facet_key.is_empty() {
        vals.push(facet_key.to_string().into());
    }
    let mut rows_sql: Vec<Vec<(i64, String, String, String)>> = Vec::new();
    match mode {
        TermMatch::Prefix => {
            let lo = normalized.to_string();
            let hi = next_prefix(normalized).unwrap_or_else(|| {
                // 正常中文不会顶到 char 上界；兜底退化为仅下界
                lo.clone()
            });
            vals.push(lo.into());
            vals.push(hi.into());
            let mut stmt = conn.prepare(&term_prefix_sql(facet_key))?;
            rows_sql.push(
                stmt.query_map(rusqlite::params_from_iter(vals.iter()), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect(),
            );
        }
        TermMatch::Contains => {
            vals.push(like_escape(normalized).into());
            let mut stmt = conn.prepare(&format!(
                "{base} normalized_term LIKE '%' || ?{p} || '%' ESCAPE '\\'
                  ORDER BY length(tt.term) ASC, tt.term, tt.tag_id",
                p = vals.len()
            ))?;
            rows_sql.push(
                stmt.query_map(rusqlite::params_from_iter(vals.iter()), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect(),
            );
        }
        TermMatch::Fuzzy => {
            let len = normalized.chars().count() as i64;
            let first = normalized
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default();
            // ① 首字符相同 或 长度差 ≤ 1 → SQL 缩候选（首字符走前缀可索引扫描）
            vals.push(first.clone().into());
            vals.push((len - 1).max(0).into());
            vals.push((len + 1).into());
            let mut stmt = conn.prepare(&format!(
                "{base} (substr(tt.normalized_term,1,1) = ?{fp}
                     OR length(tt.normalized_term) BETWEEN ?{lp} AND ?{hp})",
                fp = vals.len() - 2,
                lp = vals.len() - 1,
                hp = vals.len()
            ))?;
            rows_sql.push(
                stmt.query_map(rusqlite::params_from_iter(vals.iter()), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect(),
            );
        }
        // Exact/Alias 不经扩展（find_by_term 单点处理，最多一行）
        _ => return Ok((Vec::new(), warnings)),
    }
    let loaded = rows_sql.into_iter().flatten().collect::<Vec<_>>();
    let loaded_len = loaded.len();
    if loaded_len == 0 {
        return Ok((Vec::new(), warnings));
    }
    // ② Fuzzy 第二阶段：编辑距离 ≤ 1（char），按距离升序、同距按名字长度升序
    let ordered: Vec<(i64, String, String, String)> = if mode == TermMatch::Fuzzy {
        type ScoredTerm = (usize, usize, (i64, String, String, String));
        let mut scored: Vec<ScoredTerm> = loaded
            .into_iter()
            .filter_map(|row| {
                let d = char_levenshtein(normalized, &row.2);
                (d <= 1).then_some((d, row.2.chars().count(), row))
            })
            .collect();
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2 .2.cmp(&b.2 .2))
                .then(a.2 .0.cmp(&b.2 .0))
        });
        scored.into_iter().map(|(_, _, r)| r).collect()
    } else {
        loaded
    };
    let truncated = ordered.len() > cap;
    let taken = ordered.into_iter().take(cap).collect::<Vec<_>>();
    // 超上限 warning（按原始匹配数，不是 cap 后数）
    if truncated {
        warnings.push(format!(
            "「{normalized}」匹配到 {loaded_len} 个，只用了前 {cap} 个"
        ));
    }
    let mut hits = Vec::with_capacity(taken.len());
    for (tag_id, kind, term, status) in taken {
        hits.push(TermHit {
            tag_id,
            term_kind: kind,
            matched_term: term,
            tag_status: status,
        });
    }
    // 命中多个时把名字列给用户看（避免「不知命中什么」的困惑）
    if hits.len() > 1 && !truncated {
        let names: Vec<&str> = hits.iter().map(|h| h.matched_term.as_str()).collect();
        warnings.push(format!(
            "「{normalized}」匹配到 {} 个：{}",
            hits.len(),
            names.join("、")
        ));
    }
    Ok((hits, warnings))
}

/// F6-b：分面内近似匹配 —— 只提示，绝不自动改写/合并。
/// 三级（调用方已排除精确别名命中）：
///   ② 一方是另一方子串且长度差 ≤ 2 → Substring
///   ③ 编辑距离 ≤ 1（char）→ Spell
/// 候选 = 分面内 active 标签的规范名（feature 启用读 tag_terms canonical，否则读 tags）。
pub fn find_similar_tag(
    conn: &Connection,
    facet_key: &str,
    normalized: &str,
) -> AppResult<Option<(i64, String, SimilarReason)>> {
    if normalized.is_empty() {
        return Ok(None);
    }
    let terms_enabled =
        crate::db::schema_features::feature_enabled(conn, "tag_unique_terms").unwrap_or(false);
    let mut cands: Vec<(i64, String)> = if terms_enabled {
        let mut stmt = conn.prepare(
            "SELECT tt.tag_id, tt.term FROM tag_terms tt
               JOIN tags t ON t.id = tt.tag_id
              WHERE tt.facet_key = ?1 AND tt.term_kind = 'canonical'
                AND COALESCE(t.status,'active') = 'active'",
        )?;
        let rows = stmt
            .query_map([facet_key], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, name FROM tags
              WHERE facet_key = ?1 AND status = 'active'",
        )?;
        let rows = stmt
            .query_map([facet_key], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    // 保持确定性：按 tag_id 排序后比较
    cands.sort_by_key(|(id, _)| *id);
    // best = (priority, tag_id, display_name, reason)
    let mut best: Option<(u8, i64, String, SimilarReason)> = None;
    for (id, name) in &cands {
        let cnorm = normalize_name(name);
        if cnorm.is_empty() || cnorm == normalized {
            continue;
        }
        let len_diff = (cnorm.chars().count() as isize - normalized.chars().count() as isize).abs();
        // ② 子串（长度差 ≤ 2）
        let substring =
            len_diff <= 2 && (cnorm.contains(normalized) || normalized.contains(cnorm.as_str()));
        // ③ 编辑距离 ≤ 1
        let spell = char_levenshtein(&cnorm, normalized) <= 1;
        if substring {
            let replace = match &best {
                None => true,
                Some((p, ..)) => *p <= 2,
            };
            if replace {
                best = Some((2, *id, name.clone(), SimilarReason::Substring));
            }
        } else if spell {
            let replace = match &best {
                None => true,
                Some((p, ..)) => *p <= 1,
            };
            if replace {
                best = Some((1, *id, name.clone(), SimilarReason::Spell));
            }
        }
    }
    Ok(best.map(|(_, id, name, reason)| (id, name, reason)))
}

/// F6-d：疑似重复组 —— 对词表跑一遍近似匹配，按连通分量聚合。
/// 返回可供设置页展示的组（每组给「合并到…」下拉）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub facet_key: String,
    pub members: Vec<DuplicateMember>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateMember {
    pub tag_id: i64,
    pub name: String,
    pub asset_count: i64,
}

/// F6-d：每分面内部，把近似词连边后取连通分量。扫描是「疑似」清单（人工再判），阈值放宽松：
/// 子串差 ≤2 或编辑距离 ≤2 —— 否则「单人/一个人/一个」这类真实重复（距离 2）揪不出来。
/// （find_similar_tag 的实时提示仍用 ≤1：只提示；扫描清单允许多几条待人工合并。）
pub fn scan_duplicate_tags(conn: &Connection) -> AppResult<Vec<DuplicateGroup>> {
    // 每分面的 active 标签（id, name, 关联数）
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.facet_key,
                (SELECT COUNT(*) FROM asset_tags at WHERE at.tag_id = t.id)
           FROM tags t
          WHERE COALESCE(t.status,'active') = 'active'
          ORDER BY t.facet_key, t.id",
    )?;
    let all: Vec<(i64, String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut by_facet: std::collections::BTreeMap<String, Vec<(i64, String, i64)>> =
        Default::default();
    for (id, name, facet, uses) in all {
        by_facet.entry(facet).or_default().push((id, name, uses));
    }
    let mut groups = Vec::new();
    for (facet, tags_in) in by_facet {
        let n = tags_in.len();
        // 邻接（无向）：similar edge
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for i in 0..n {
            for j in (i + 1)..n {
                let (ia, name_a, _) = &tags_in[i];
                let (ib, name_b, _) = &tags_in[j];
                if ia == ib {
                    continue;
                }
                let na = normalize_name(name_a);
                let nb = normalize_name(name_b);
                if na.is_empty() || nb.is_empty() || na == nb {
                    continue;
                }
                let len_diff = (na.chars().count() as isize - nb.chars().count() as isize).abs();
                let similar = (len_diff <= 2
                    && (na.contains(nb.as_str()) || nb.contains(na.as_str())))
                    || char_levenshtein(&na, &nb) <= 2;
                if similar {
                    adj[i].push(j);
                    adj[j].push(i);
                }
            }
        }
        // 连通分量
        let mut seen = vec![false; n];
        for start in 0..n {
            if seen[start] {
                continue;
            }
            let mut comp: Vec<usize> = Vec::new();
            let mut stack = vec![start];
            seen[start] = true;
            while let Some(u) = stack.pop() {
                comp.push(u);
                for &v in &adj[u] {
                    if !seen[v] {
                        seen[v] = true;
                        stack.push(v);
                    }
                }
            }
            if comp.len() < 2 {
                continue;
            }
            let mut members: Vec<DuplicateMember> = comp
                .iter()
                .map(|&idx| {
                    let (id, name, uses) = &tags_in[idx];
                    DuplicateMember {
                        tag_id: *id,
                        name: name.clone(),
                        asset_count: *uses,
                    }
                })
                .collect();
            // 稳定展示：关联数降序 → id 升序
            members.sort_by(|a, b| {
                b.asset_count
                    .cmp(&a.asset_count)
                    .then_with(|| a.tag_id.cmp(&b.tag_id))
            });
            groups.push(DuplicateGroup {
                facet_key: facet.clone(),
                members,
            });
        }
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use crate::db::migrations;
    use crate::db::schema_features;

    fn terms_db() -> Connection {
        let c = init_memory().unwrap();
        migrations::apply_v22b_constraints(&c).unwrap();
        schema_features::set_feature(&c, "tag_unique_terms", true, None).unwrap();
        c
    }
    /// S5：Prefix 必须走索引 —— EXPLAIN QUERY PLAN 含 SEARCH 且不含 SCAN。
    /// 防止有人把范围查询改回 `LIKE 'x%'`（实测 5 万行 → SCAN）。
    #[test]
    fn term_match_prefix_uses_index() {
        let c = terms_db();
        // feature 开时 create_in_facet 已写 canonical 词条（tag_terms），无需手插
        create_in_facet(&c, "海边", None, Some("scene")).unwrap();
        create_in_facet(&c, "海景", None, Some("scene")).unwrap();
        let mut stmt = c
            .prepare(&format!("EXPLAIN QUERY PLAN {}", term_prefix_sql("scene")))
            .unwrap();
        let plan: String = stmt
            .query_row(rusqlite::params!["scene", "海", "鸿"], |r| r.get(3))
            .unwrap();
        assert!(plan.contains("SEARCH"), "前缀范围查询必须走索引：{plan}");
        assert!(!plan.contains("SCAN"), "不得退化全表扫：{plan}");
    }

    /// S5 5-5：SQL 前缀范围与 Rust 字节序判断必须一致（BINARY 排序是 next_prefix
    /// 不变式的前提；含 emoji / BMP 边界词一并进行）。
    #[test]
    fn prefix_range_matches_rust_ordering() {
        let c = terms_db();
        let t1 = create_in_facet(&c, "海边", None, Some("scene")).unwrap();
        let t2 = create_in_facet(&c, "😀", None, Some("scene")).unwrap();
        let t3 = create_in_facet(&c, "a\u{FFFF}", None, Some("scene")).unwrap();
        // 兄弟词（同前缀但区间外）：「海上」> next_prefix("海边")，不应命中前缀「海边」
        create_in_facet(&c, "海上", None, Some("scene")).unwrap();
        for term in ["海边x", "😀😀", "a\u{FFFF}x"] {
            let id = create_in_facet(&c, term, None, Some("scene")).unwrap();
            let _ = id;
        }
        let _ = (t1, t2, t3);
        for prefix in ["海", "海边", "😀", "a\u{FFFF}"] {
            let normalized = normalize_name(prefix);
            let hi = next_prefix(&normalized);
            // SQL 侧：真实范围查询（走 expand_term_query 同一 SQL）
            let cap = PREFIX_EXPAND_CAP;
            let (hits, _w) =
                expand_term_query(&c, "scene", &normalized, TermMatch::Prefix, cap).unwrap();
            // Rust 侧：读全部词，按 str 字节序判断「以 prefix 开头」
            let mut stmt = c
                .prepare("SELECT DISTINCT normalized_term FROM tag_terms WHERE facet_key='scene'")
                .unwrap();
            let all: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            let rust_hits: Vec<&String> =
                all.iter().filter(|t| t.starts_with(&normalized)).collect();
            assert_eq!(
                hits.len(),
                rust_hits.len(),
                "SQL 与 Rust 前缀命中数不一致：prefix={prefix} sql={:?} rust={:?}",
                hits.iter()
                    .map(|h| h.matched_term.as_str())
                    .collect::<Vec<_>>(),
                rust_hits
            );
            // 区间不变式：SQL 每个命中都满足 Rust 的 starts_with
            for h in &hits {
                assert!(
                    h.matched_term.starts_with(&normalized),
                    "SQL 命中「{}」不满足 Rust 前缀判断 {}",
                    h.matched_term,
                    normalized
                );
            }
            // 兄弟词不在区间内：「海上」不属于「海边」前缀
            if prefix == "海边" {
                assert!(
                    !hits.iter().any(|h| h.matched_term.contains("海上")),
                    "兄弟词「海上」不得命中前缀「海边」"
                );
            }
            let _ = hi;
        }
    }

    #[test]
    fn core_alias_backfill_repairs_nonempty_terms_db_idempotently() {
        let c = terms_db();
        let youth = create_in_facet(&c, "青年", None, Some("people")).unwrap();

        // 存量库不会重新播种整套核心词表，但新版本别名必须能补进唯一事实源。
        ensure_core_taxonomy_aliases(&c).unwrap();
        let aliases = searchable_aliases(&c, youth.id).unwrap();
        assert!(aliases.iter().any(|alias| alias == "年轻"));
        assert!(aliases.iter().any(|alias| alias == "年轻人"));

        // 启动时会重复调用；重复补齐不能产生第二份词条。
        ensure_core_taxonomy_aliases(&c).unwrap();
        let alias_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tag_terms
                  WHERE tag_id = ?1 AND term_kind != 'canonical'",
                [youth.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alias_count, 2);
    }
}
