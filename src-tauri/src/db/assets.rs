//! 素材仓储：CRUD + 分页查询（类型/未打标/标签树/搜索 四路筛选，标签聚合返回）

use rusqlite::{types::Value, Connection, Row};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use super::search_query::MetadataFilter;
use super::search_query::{self};
use super::sql_utils::offset_placeholders;
use super::{search, tags::Tag, tags::FACET_EFFECTIVE};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: i64,
    pub file_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_size: i64,
    pub mime_type: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub taken_at: Option<i64>,
    pub created_at: i64,
    pub modified_at: i64,
    pub hash: Option<String>,
    pub placeholder_path: Option<String>,
    pub hd_thumbnail_path: Option<String>,
    // EXIF 元信息（PRD 5.5，入库自动提取，不进打标流）
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub aperture: Option<f64>,
    pub shutter: Option<String>,
    pub focal: Option<f64>,
    pub tags: Vec<Tag>,
    // 媒体元数据（指导书 §7.3/§7.4，V12 迁移新增列；后端探测为事实源）
    pub media_kind: Option<String>,
    pub container_format: Option<String>,
    pub video_profile: Option<String>,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<i64>,
    pub frame_rate: Option<f64>,
    pub video_bit_rate: Option<i64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub audio_sample_rate: Option<i64>,
    pub audio_channels: Option<i64>,
    pub audio_layout: Option<String>,
    pub rotation: Option<i64>,
    pub media_metadata_json: Option<String>,
    pub metadata_version: Option<i64>,
    pub metadata_scanned_at: Option<i64>,
    pub metadata_error: Option<String>,
    // FB2-08（§14.8）：算法主色。palette 由 palette_json 解析而来（前端不做 JSON.parse）；
    // dominant_* 是索引列，供超级搜索按颜色筛选。
    #[serde(default)]
    pub palette: Option<Vec<PaletteSegmentDto>>,
    pub dominant_hue: Option<i64>,
    pub dominant_sat: Option<i64>,
    pub dominant_lum: Option<i64>,
    // FB5-05（§7.3）：一句话描述（不限字数；素材字段，不进标签树/统计）。
    // 固定追加在 palette 字段之后，避免已有固定列索引错位。
    #[serde(default)]
    pub content_description: String,
    // GPS 定位（V18）：有符号十进制度（北纬东经为正），无定位为 NULL。
    // 追加在 content_description 之后，保持 from_row 既有列索引不变。
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    // V19：收藏/评级/手动旋转/phash（② T13）。追加在末尾（索引 49–52），严禁插中间。
    /// 收藏标记（0/1）
    #[serde(default)]
    pub favorite: i64,
    /// 评级（0–5；0 = 未评级，排序时排在有评级之后）
    #[serde(default)]
    pub rating: i64,
    /// 用户手动旋转（0/90/180/270）。严禁复用 rotation（那是 V12 ffprobe 媒体元数据语义）
    #[serde(default)]
    pub user_rotation: i64,
    /// 感知哈希 dHash 64 位（W5d 相似去重；未计算为 NULL）
    #[serde(default)]
    pub phash: Option<i64>,
}

/// FB2-08：色板单段（与前端 PaletteSegmentDto 同形）。
/// palette_json 在 from_row 里解析成这个结构，前端不做 JSON.parse。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteSegmentDto {
    pub hex: String,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub ratio: f32,
}

/// FB4-03（§5.1）：色板 JSON 单一解析函数 —— 状态统计 / missing 列表 / patch 查询 / from_row
/// 全部复用同一解析语义，禁止各写一份：
/// - `NULL` / 空字符串 / 全空白 / `[]` / 损坏 JSON / 合法但空数组 -> `None`
/// - 合法且非空数组 -> `Some(Vec<PaletteSegmentDto>)`
///   避免「页面说已完成，但实际渲染为空」的语义分叉。
pub(crate) fn parse_palette_json(raw: Option<String>) -> Option<Vec<PaletteSegmentDto>> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Vec<PaletteSegmentDto>>(&raw)
        .ok()
        .filter(|v| !v.is_empty())
}

/// FB4-03（§5.3）：色板状态 DTO。字段定义见指导书 5.3 节：
/// totalAssets = deleted_at IS NULL 全部素材；eligible = 满足候选谓词；
/// ready = 候选中经 parse_palette_json 得到非空色板的数量；missing = eligible - ready；
/// unavailable = totalAssets - eligible。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteStatus {
    pub total_assets: i64,
    pub eligible: i64,
    pub ready: i64,
    pub missing: i64,
    pub unavailable: i64,
}

/// C-1：从 palette_json 全量重建 asset_palette_colors（回填命令的数据层）。
/// 单事务：清空 → 遍历非空 palette（rank = 数组下标）→ rgb→桶插行。
/// 幂等可重复执行；返回写入行数。
pub fn rescan_palette_colors(conn: &Connection) -> AppResult<i64> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM asset_palette_colors", [])?;
    let rows: Vec<(i64, Option<String>)> = {
        let mut stmt =
            tx.prepare("SELECT id, palette_json FROM assets WHERE deleted_at IS NULL")?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        r
    };
    let mut written: i64 = 0;
    let mut stmt = tx.prepare(
        "INSERT INTO asset_palette_colors (asset_id, rank, color_bucket, ratio)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (id, raw) in rows {
        let Some(segments) = parse_palette_json(raw) else {
            continue;
        };
        for (rank, seg) in segments.into_iter().enumerate() {
            let (bucket, _name) = crate::db::palette_bucket::bucket_of_rgb(seg.r, seg.g, seg.b);
            stmt.execute(rusqlite::params![id, rank as i64, bucket, seg.ratio as f64])?;
            written += 1;
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok(written)
}

const PRIMARY_COLOR_MIN_RATIO: f64 = 0.10;

fn primary_color_items(conn: &Connection) -> AppResult<Vec<MetadataFacetItem>> {
    let mut stmt = conn.prepare(
        "SELECT color_bucket, COUNT(*) AS count
           FROM (
             SELECT asset_id, color_bucket
               FROM asset_palette_colors
              WHERE rank < 3
              GROUP BY asset_id, color_bucket
             HAVING SUM(ratio) >= ?1
           )
          GROUP BY color_bucket
          ORDER BY count DESC, color_bucket",
    )?;
    let rows = stmt.query_map([PRIMARY_COLOR_MIN_RATIO], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (bucket, count) = row?;
        if let Some(name) = crate::db::palette_bucket::bucket_name(bucket) {
            items.push(MetadataFacetItem {
                value: name.to_string(),
                label: name.to_string(),
                count,
            });
        }
    }
    Ok(items)
}

/// FB4-03（§5.5）：轻量色板补丁 —— 只同步色板相关字段，不返回文件路径/标签/缩略图等无关字段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPalettePatch {
    pub id: i64,
    pub palette: Option<Vec<PaletteSegmentDto>>,
    pub dominant_hue: Option<i64>,
    pub dominant_sat: Option<i64>,
    pub dominant_lum: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub imported: i64,
    pub failed: i64,
    pub duplicates: i64,
    pub errors: Vec<String>,
    /// 扫描阶段无法读取的路径等非致命警告；不得混入 errors 冒充单文件失败。
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetTagFilter {
    pub facet_key: String,
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default = "default_true")]
    pub include_descendants: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataFacetItem {
    pub value: String,
    pub label: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataFacet {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub items: Vec<MetadataFacetItem>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetFilter {
    /// "all" | "image" | "video"（None / "all" = 全部）
    pub asset_type: Option<String>,
    #[serde(default)]
    pub untagged_only: bool,
    /// 父标签连带子标签（递归 CTE 处理）
    pub tag_id: Option<i64>,
    /// 多标签筛选（R-21，与 tag_id 二选一，优先 tag_ids）
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    /// 多标签组合模式：any（默认）| all（EXISTS 逐标签，防 JOIN 行数爆炸）
    #[serde(default)]
    pub tags_mode: Option<String>,
    /// 新分面协议：同一项内部 any/all，不同分面项之间 AND。
    #[serde(default)]
    pub facet_filters: Vec<FacetTagFilter>,
    /// 明确排除的标签；默认同时排除其后代。
    #[serde(default)]
    pub exclude_tag_ids: Vec<i64>,
    /// 文件自身携带的元数据分面；同组 values 为 OR，不同 key 之间为 AND。
    #[serde(default)]
    pub metadata_filters: Vec<MetadataFilter>,
    /// 搜索关键词（FTS5 / ≤2 字 LIKE 兜底，见 db/search.rs）
    pub search: Option<String>,
    /// 排序字段（R-21）：created_at（默认）| taken_at | size | resolution；缺值排最后
    #[serde(default)]
    pub sort_by: Option<String>,
    /// 排序方向：desc（默认）| asc
    #[serde(default)]
    pub sort_dir: Option<String>,
    /// true = 查回收站（deleted_at 非空）；默认查在库（R-22）
    #[serde(default)]
    pub trash_only: bool,
    /// 布尔表达式树（P4 query_expr）：表达式构建器产物；存在时优先走表达式编译，
    /// 与扁平字段二选一（两者互斥，若同时存在以 expr 为准）。
    #[serde(default)]
    pub expr: Option<super::query_expr::QueryExpr>,
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    200
}

/// 手写 Default：与 serde 反序列化默认值保持一致（派生 Default 会给 limit=0）
impl Default for AssetFilter {
    fn default() -> Self {
        Self {
            asset_type: None,
            untagged_only: false,
            tag_id: None,
            tag_ids: Vec::new(),
            tags_mode: None,
            facet_filters: Vec::new(),
            exclude_tag_ids: Vec::new(),
            metadata_filters: Vec::new(),
            search: None,
            sort_by: None,
            sort_dir: None,
            trash_only: false,
            expr: None,
            offset: 0,
            limit: default_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPage {
    pub items: Vec<Asset>,
    pub total: i64,
    pub has_more: bool,
    /// R2-1：本次查询编译层 warning（剔除/停用/降级），前端结果区上方黄字
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub(crate) const COLUMNS: &str =
    "id, file_path, file_name, file_ext, file_size, mime_type, width, height, \
                       duration_ms, video_codec, audio_codec, taken_at, created_at, modified_at, \
                       hash, placeholder_path, hd_thumbnail_path, \
                       camera, lens, iso, aperture, shutter, focal, \
                       media_kind, container_format, video_profile, pixel_format, bit_depth, frame_rate, \
                       video_bit_rate, color_range, color_space, color_transfer, color_primaries, \
                       audio_sample_rate, audio_channels, audio_layout, rotation, \
                       media_metadata_json, metadata_version, metadata_scanned_at, metadata_error, \
                       palette_json, dominant_hue, dominant_sat, dominant_lum, \
                       content_description, latitude, longitude,                        favorite, rating, user_rotation, phash";

pub(crate) fn from_row(row: &Row) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: row.get(0)?,
        file_path: row.get(1)?,
        file_name: row.get(2)?,
        file_ext: row.get(3)?,
        file_size: row.get(4)?,
        mime_type: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        duration_ms: row.get(8)?,
        video_codec: row.get(9)?,
        audio_codec: row.get(10)?,
        taken_at: row.get(11)?,
        created_at: row.get(12)?,
        modified_at: row.get(13)?,
        hash: row.get(14)?,
        placeholder_path: row.get(15)?,
        hd_thumbnail_path: row.get(16)?,
        camera: row.get(17)?,
        lens: row.get(18)?,
        iso: row.get(19)?,
        aperture: row.get(20)?,
        shutter: row.get(21)?,
        focal: row.get(22)?,
        media_kind: row.get(23)?,
        container_format: row.get(24)?,
        video_profile: row.get(25)?,
        pixel_format: row.get(26)?,
        bit_depth: row.get(27)?,
        frame_rate: row.get(28)?,
        video_bit_rate: row.get(29)?,
        color_range: row.get(30)?,
        color_space: row.get(31)?,
        color_transfer: row.get(32)?,
        color_primaries: row.get(33)?,
        audio_sample_rate: row.get(34)?,
        audio_channels: row.get(35)?,
        audio_layout: row.get(36)?,
        rotation: row.get(37)?,
        media_metadata_json: row.get(38)?,
        metadata_version: row.get(39)?,
        metadata_scanned_at: row.get(40)?,
        metadata_error: row.get(41)?,
        // palette_json（列 42）解析为 DTO；损坏/空 JSON → None（安静降级，不让一行坏数据毁掉整页查询）
        palette: parse_palette_json(row.get::<_, Option<String>>(42)?),
        dominant_hue: row.get(43)?,
        dominant_sat: row.get(44)?,
        dominant_lum: row.get(45)?,
        content_description: row.get(46)?,
        latitude: row.get(47)?,
        longitude: row.get(48)?,
        favorite: row.get(49)?,
        rating: row.get(50)?,
        user_rotation: row.get(51)?,
        phash: row.get(52)?,
        tags: Vec::new(),
    })
}

/// 组装 WHERE 子句与位置参数（?1.. 顺序与返回参数一致）
/// search_pred 为库内编译好的搜索谓词（来自 search::build_search_predicate），
/// 在数据库内与其他条件组合，不再回传大 ID 列表。
fn build_where(
    conn: &Connection,
    filter: &AssetFilter,
    search_pred: Option<&search::SearchPredicate>,
    warnings: &mut Vec<String>,
) -> AppResult<(String, Vec<Value>)> {
    let mut cond = String::from("1=1");
    let mut params: Vec<Value> = Vec::new();
    // 回收站隔离（R-22）：默认只看不在回收站的
    if filter.trash_only {
        cond.push_str(" AND a.deleted_at IS NOT NULL");
    } else {
        cond.push_str(" AND a.deleted_at IS NULL");
    }
    // 布尔表达式树分支（P4 query_expr）：有 expr 时以表达式为准，忽略扁平字段。
    // 此处仅追加 expr 编译片段；回收站隔离已在上方作为基础条件。
    if let Some(expr) = &filter.expr {
        // R2-1：编译 warning（标签剔除/分面停用等）回传 DTO，前端黄字展示
        match super::query_expr::compile_expr_with(conn, expr, warnings) {
            Ok((sql, p)) => {
                if !sql.trim().is_empty() {
                    cond.push_str(&format!(" AND ({sql})"));
                    params.extend(p);
                }
                return Ok((cond, params));
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
    match filter.asset_type.as_deref() {
        Some("image") => cond.push_str(" AND a.mime_type LIKE 'image/%'"),
        Some("video") => cond.push_str(" AND a.mime_type LIKE 'video/%'"),
        _ => {}
    }
    if filter.untagged_only {
        cond.push_str(" AND NOT EXISTS (SELECT 1 FROM asset_tags at WHERE at.asset_id = a.id)");
    }
    if let Some(tid) = filter.tag_id {
        params.push(tid.into());
        // F1-d：后代递归 CTE 加 d < 12 上限
        cond.push_str(&format!(
            " AND a.id IN (SELECT asset_id FROM asset_tags WHERE tag_id IN (
                WITH RECURSIVE sub(id, d) AS (
                  SELECT ?{}, 0 UNION ALL
                  SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
                   WHERE s.d < 12
                ) SELECT id FROM sub))",
            params.len()
        ));
    }
    // 多标签筛选（R-21）：any = 单 CTE 多 seed；all = 逐标签 EXISTS（防 JOIN 行数爆炸）
    // F1-d：所有后代递归 CTE 加 d < 12 上限（防环死循环）
    if !filter.tag_ids.is_empty() {
        let all_mode = filter.tags_mode.as_deref() == Some("all");
        if all_mode {
            for &tid in &filter.tag_ids {
                params.push(tid.into());
                cond.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM asset_tags at2 WHERE at2.asset_id = a.id AND at2.tag_id IN (
                        WITH RECURSIVE sub(id, d) AS (
                          SELECT ?{}, 0 UNION ALL
                          SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
                           WHERE s.d < 12
                        ) SELECT id FROM sub))",
                    params.len()
                ));
            }
        } else {
            let mut seeds = String::new();
            for &tid in &filter.tag_ids {
                params.push(tid.into());
                if !seeds.is_empty() {
                    seeds.push_str(" UNION ALL");
                }
                seeds.push_str(&format!(" SELECT ?{}, 0", params.len()));
            }
            cond.push_str(&format!(
                " AND a.id IN (SELECT asset_id FROM asset_tags WHERE tag_id IN (
                    WITH RECURSIVE sub(id, d) AS (
                      {seeds} UNION ALL
                      SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id = s.id
                       WHERE s.d < 12
                    ) SELECT id FROM sub))"
            ));
        }
    }
    for facet in &filter.facet_filters {
        if facet.tag_ids.is_empty() {
            continue;
        }
        let all_mode = facet.mode.as_deref() == Some("all");
        let descendant = facet.include_descendants;
        let append_one = |cond: &mut String, params: &mut Vec<Value>, tid: i64| {
            params.push(tid.into());
            if descendant {
                cond.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id = a.id AND atf.tag_id IN (
                        WITH RECURSIVE sub(id, d) AS (SELECT ?{}, 0 UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                        SELECT id FROM sub))", params.len()
                ));
            } else {
                cond.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id = a.id AND atf.tag_id = ?{})",
                    params.len()
                ));
            }
        };
        if all_mode {
            for &tid in &facet.tag_ids {
                append_one(&mut cond, &mut params, tid);
            }
        } else if descendant {
            let mut seeds = String::new();
            for &tid in &facet.tag_ids {
                params.push(tid.into());
                if !seeds.is_empty() {
                    seeds.push_str(" UNION ALL");
                }
                seeds.push_str(&format!(" SELECT ?{}, 0", params.len()));
            }
            cond.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id IN (
                    WITH RECURSIVE sub(id, d) AS ({seeds} UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                    SELECT id FROM sub))"
            ));
        } else {
            let placeholders = facet
                .tag_ids
                .iter()
                .map(|tid| {
                    params.push((*tid).into());
                    format!("?{}", params.len())
                })
                .collect::<Vec<_>>()
                .join(",");
            cond.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM asset_tags atf WHERE atf.asset_id=a.id AND atf.tag_id IN ({placeholders}))"
            ));
        }
    }
    for &tid in &filter.exclude_tag_ids {
        params.push(tid.into());
        // F1-d：后代递归 CTE 加 d < 12 上限
        cond.push_str(&format!(
            " AND NOT EXISTS (SELECT 1 FROM asset_tags ate WHERE ate.asset_id=a.id AND ate.tag_id IN (
                WITH RECURSIVE sub(id, d) AS (SELECT ?{}, 0 UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN sub s ON t.parent_id=s.id WHERE s.d < 12)
                SELECT id FROM sub))", params.len()
        ));
    }
    // 元数据比较：按白名单 key/op 编译，全部参数绑定，非法条件即报错（build_where 被上层校验兜底）
    if !filter.metadata_filters.is_empty() {
        match search_query::compile_metadata_all(&filter.metadata_filters) {
            Ok(Some((meta_sql, meta_params))) => {
                if !meta_sql.is_empty() {
                    cond.push_str(&format!(" AND ({meta_sql})"));
                    params.extend(meta_params);
                }
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        // R2-2：量纲人话（不阻断执行 —— 手填 100 字节合法，但把「静默 0 结果」变「0 结果 + 一句提示」）
        for f in &filter.metadata_filters {
            for w in search_query::dimension_warnings(f) {
                warnings.push(w);
            }
        }
    }
    if let Some(pred) = search_pred {
        if !pred.sql.is_empty() {
            // 把谓词中的占位符偏移到全局参数索引
            let shifted = offset_placeholders(&pred.sql, params.len());
            cond.push_str(&format!(" AND ({shifted})"));
            params.extend(pred.params.iter().cloned());
        }
    }
    Ok((cond, params))
}

/// 排序子句（R-21）：taken_at/resolution 缺值排最后；尾缀 a.id DESC 稳定分页
fn order_by(filter: &AssetFilter) -> String {
    let dir = if filter.sort_dir.as_deref() == Some("asc") {
        "ASC"
    } else {
        "DESC"
    };
    let expr = match filter.sort_by.as_deref() {
        Some("taken_at") => format!("CASE WHEN a.taken_at IS NULL THEN 1 ELSE 0 END ASC, a.taken_at {dir}"),
        Some("size") => format!("a.file_size {dir}"),
        Some("resolution") => format!(
            "CASE WHEN a.width IS NULL OR a.height IS NULL THEN 1 ELSE 0 END ASC, (a.width * a.height) {dir}"
        ),
        Some("name") => format!("a.file_name {dir}"),
        // W2-8：未评级（rating=0）排最后，其余按评级排序
        Some("rating") => format!(
            "CASE WHEN a.rating = 0 THEN 1 ELSE 0 END ASC, a.rating {dir}"
        ),
        Some("modified_at") => format!("a.modified_at {dir}"),
        _ => format!("a.created_at {dir}"),
    };
    format!("{expr}, a.id DESC")
}

/// 编译筛选条件中的搜索谓词（库内组合）；无搜索返回 None。
/// FB5-05（§8.3）：普通素材库搜索显式传 SearchScope::All（默认范围 = 三列）。
fn build_search_predicate(
    conn: &Connection,
    filter: &AssetFilter,
) -> AppResult<Option<search::SearchPredicate>> {
    match &filter.search {
        Some(q) if !q.trim().is_empty() => {
            search::build_search_predicate(conn, q, super::query_expr::SearchScope::All)
        }
        _ => Ok(None),
    }
}

// S0：排序白名单单一事实源（db/search_query.rs ALL_SORT_KEYS）——不再本地维护一份
const VALID_SORT: &[&str] = super::search_query::ALL_SORT_KEYS;

impl AssetFilter {
    /// 参数校验：非法 key/op/值/数量/排序/分页一律返回 AppError，不静默忽略。
    /// list / list_ids 入口处调用；非法条件直接拒绝整次查询。
    pub fn validate(&self) -> AppResult<()> {
        if let Some(t) = &self.asset_type {
            if !matches!(t.as_str(), "image" | "video" | "all") {
                return Err(AppError::msg(format!("非法 assetType：{t}")));
            }
        }
        if let Some(m) = &self.tags_mode {
            if !matches!(m.as_str(), "any" | "all") {
                return Err(AppError::msg(format!("非法 tagsMode：{m}")));
            }
        }
        if let Some(dir) = &self.sort_dir {
            if !matches!(dir.as_str(), "asc" | "desc") {
                return Err(AppError::msg(format!("非法 sortDir：{dir}")));
            }
        }
        if let Some(sb) = &self.sort_by {
            if !VALID_SORT.contains(&sb.as_str()) {
                return Err(AppError::msg(format!("非法排序字段：{sb}")));
            }
        }
        // 分页：沿用 B19 钳制语义（list 内 clamp limit 到 [1,1000]、offset ≥0），
        // validate 不拒绝——前端可能传 0/极值，保持既有行为。
        // 标签与排除标签数量上限
        if self.tag_ids.len() > 100
            || self.exclude_tag_ids.len() > 100
            || self.facet_filters.len() > 100
        {
            return Err(AppError::msg("标签条件数量超出上限"));
        }
        for facet in &self.facet_filters {
            if facet.tag_ids.is_empty() {
                return Err(AppError::msg("分面标签条件不能为空"));
            }
            if let Some(m) = &facet.mode {
                if !matches!(m.as_str(), "any" | "all") {
                    return Err(AppError::msg(format!("非法分面 mode：{m}")));
                }
            }
        }
        // 元数据：白名单 key/op/值类型/数量
        search_query::validate_metadata(&self.metadata_filters)?;
        // 布尔表达式树：深度/节点/叶子合法性
        if let Some(expr) = &self.expr {
            super::query_expr::validate_expr(expr)?;
        }
        Ok(())
    }
}

/// 只返回当前筛选结果的 id 数组（BUG-E：全选/反选/批量操作无需完整 Asset 对象）。
/// 复用 build_where 与搜索逻辑，仅 SELECT a.id，ORDER BY 与 list() 一致。
pub fn list_ids(conn: &Connection, filter: &AssetFilter) -> AppResult<Vec<i64>> {
    filter.validate()?;
    let search_pred = build_search_predicate(conn, filter)?;
    let mut warnings = Vec::new();
    let (cond, params) = build_where(conn, filter, search_pred.as_ref(), &mut warnings)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id FROM assets a WHERE {cond} ORDER BY {} LIMIT 100000", // B19：上限 100000（全选用，放宽但防滥用）
        order_by(filter)
    ))?;
    let ids = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            r.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

pub fn list(conn: &Connection, filter: &AssetFilter) -> AppResult<AssetPage> {
    filter.validate()?;
    let search_pred = build_search_predicate(conn, filter)?;
    let mut warnings = Vec::new();
    let (cond, params) = build_where(conn, filter, search_pred.as_ref(), &mut warnings)?;
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM assets a WHERE {cond}"),
        rusqlite::params_from_iter(params.iter()),
        |r| r.get(0),
    )?;

    let mut page_params = params.clone();
    let limit = filter.limit.clamp(1, 1000); // B19：上限 1000，防一次拉全库
    let offset = filter.offset.max(0);
    page_params.push(limit.into());
    page_params.push(offset.into());
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM assets a WHERE {cond} ORDER BY {} LIMIT ?{} OFFSET ?{}",
        order_by(filter),
        params.len() + 1,
        params.len() + 2
    ))?;
    let mut items: Vec<Asset> = stmt
        .query_map(rusqlite::params_from_iter(page_params.iter()), from_row)?
        .collect::<Result<_, _>>()?;

    fill_tags(conn, &mut items)?;
    Ok(AssetPage {
        has_more: offset + (items.len() as i64) < total,
        items,
        total,
        warnings,
    })
}

/// Phase 2：按 id 列表取素材并保持传入顺序 + 回填标签（plan 执行分页用，
/// plan 的排序由 SQL 决定，这里不再 ORDER BY —— 只按 ids 原序重排）。
pub(crate) fn by_ids_ordered(conn: &Connection, ids: &[i64]) -> AppResult<Vec<Asset>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT {COLUMNS} FROM assets a WHERE a.id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let mut items: Vec<Asset> = stmt
        .query_map(rusqlite::params_from_iter(ids.iter()), from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    let pos: std::collections::HashMap<i64, usize> =
        ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    items.sort_by_key(|a| pos.get(&a.id).copied().unwrap_or(usize::MAX));
    fill_tags(conn, &mut items)?;
    Ok(items)
}

fn metadata_items(
    conn: &Connection,
    value_expr: &str,
    label_expr: &str,
    present_expr: &str,
) -> AppResult<Vec<MetadataFacetItem>> {
    let sql = format!(
        "SELECT {value_expr} AS value, {label_expr} AS label, COUNT(*) AS count
           FROM assets a
          WHERE a.deleted_at IS NULL AND {present_expr}
          GROUP BY value
          ORDER BY count DESC, label COLLATE NOCASE
          LIMIT 80"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(MetadataFacetItem {
            value: row.get(0)?,
            label: row.get(1)?,
            count: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// 文件属性分面：由导入时读取的文件信息与 EXIF 动态聚合，不写入人工/AI 标签表。
pub fn list_metadata_facets(
    conn: &Connection,
    library_root: Option<&str>,
) -> AppResult<Vec<MetadataFacet>> {
    let normalized_aperture = "rtrim(rtrim(printf('%.2f', a.aperture), '0'), '.')";
    let normalized_focal = "rtrim(rtrim(printf('%.2f', a.focal), '0'), '.')";
    let taken_month = "strftime('%Y-%m', a.taken_at / 1000, 'unixepoch', 'localtime')";
    let mut facets = vec![
        MetadataFacet {
            key: "folder".into(),
            display_name: "所在文件夹".into(),
            description: "入库分库或素材原始目录".into(),
            items: list_folder_items(conn, library_root)?,
        },
        MetadataFacet {
            key: "taken_month".into(),
            display_name: "拍摄时间".into(),
            description: "按照片或视频的拍摄月份".into(),
            items: metadata_items(
                conn,
                taken_month,
                &format!("substr({taken_month}, 1, 4) || '年' || substr({taken_month}, 6, 2) || '月'"),
                "a.taken_at IS NOT NULL",
            )?,
        },
        MetadataFacet {
            key: "camera".into(),
            display_name: "拍摄设备".into(),
            description: "相机或手机型号".into(),
            items: metadata_items(conn, "a.camera", "a.camera", "a.camera IS NOT NULL AND trim(a.camera) != ''")?,
        },
        MetadataFacet {
            key: "lens".into(),
            display_name: "镜头".into(),
            description: "EXIF 中记录的镜头型号".into(),
            items: metadata_items(conn, "a.lens", "a.lens", "a.lens IS NOT NULL AND trim(a.lens) != ''")?,
        },
        MetadataFacet {
            key: "iso".into(),
            display_name: "感光度".into(),
            description: "ISO 拍摄参数".into(),
            items: metadata_items(conn, "CAST(a.iso AS TEXT)", "'ISO ' || a.iso", "a.iso IS NOT NULL")?,
        },
        MetadataFacet {
            key: "aperture".into(),
            display_name: "光圈".into(),
            description: "镜头光圈值".into(),
            items: metadata_items(conn, normalized_aperture, &format!("'f/' || {normalized_aperture}"), "a.aperture IS NOT NULL")?,
        },
        MetadataFacet {
            key: "shutter".into(),
            display_name: "快门".into(),
            description: "曝光时间".into(),
            items: metadata_items(conn, "a.shutter", "a.shutter", "a.shutter IS NOT NULL AND trim(a.shutter) != ''")?,
        },
        MetadataFacet {
            key: "focal".into(),
            display_name: "焦距".into(),
            description: "拍摄焦段".into(),
            items: metadata_items(conn, normalized_focal, &format!("{normalized_focal} || ' mm'"), "a.focal IS NOT NULL")?,
        },
        MetadataFacet {
            key: "file_ext".into(),
            display_name: "文件格式".into(),
            description: "图片或视频的扩展名".into(),
            items: metadata_items(conn, "lower(a.file_ext)", "upper(a.file_ext)", "trim(a.file_ext) != ''")?,
        },
        MetadataFacet {
            key: "resolution".into(),
            display_name: "分辨率".into(),
            description: "文件的像素尺寸".into(),
            items: metadata_items(
                conn,
                "CAST(a.width AS TEXT) || 'x' || CAST(a.height AS TEXT)",
                "CAST(a.width AS TEXT) || ' × ' || CAST(a.height AS TEXT)",
                "a.width IS NOT NULL AND a.height IS NOT NULL",
            )?,
        },
        MetadataFacet {
            key: "file_size".into(),
            display_name: "文件大小".into(),
            description: "适合快速定位大文件".into(),
            items: metadata_items(
                conn,
                "CASE WHEN a.file_size < 1048576 THEN 'lt_1mb' WHEN a.file_size < 10485760 THEN '1_10mb' WHEN a.file_size < 104857600 THEN '10_100mb' ELSE 'gte_100mb' END",
                "CASE WHEN a.file_size < 1048576 THEN '小于 1 MB' WHEN a.file_size < 10485760 THEN '1–10 MB' WHEN a.file_size < 104857600 THEN '10–100 MB' ELSE '大于等于 100 MB' END",
                "a.file_size IS NOT NULL",
            )?,
        },
        MetadataFacet {
            key: "duration".into(),
            display_name: "视频时长".into(),
            description: "仅显示视频素材的时长区间".into(),
            items: metadata_items(
                conn,
                "CASE WHEN a.duration_ms < 10000 THEN 'lt_10s' WHEN a.duration_ms < 60000 THEN '10_60s' WHEN a.duration_ms < 300000 THEN '1_5m' ELSE 'gte_5m' END",
                "CASE WHEN a.duration_ms < 10000 THEN '小于 10 秒' WHEN a.duration_ms < 60000 THEN '10 秒–1 分钟' WHEN a.duration_ms < 300000 THEN '1–5 分钟' ELSE '大于等于 5 分钟' END",
                "a.duration_ms IS NOT NULL",
            )?,
        },
        // W2-8：评级分面（0 = 未评级不展示；点击走 rating eq/gte 数值管道）
        MetadataFacet {
            key: "rating".into(),
            display_name: "评级".into(),
            description: "你给素材打的星级".into(),
            items: metadata_items(
                conn,
                "CAST(a.rating AS TEXT)",
                "CASE a.rating WHEN 1 THEN '★' WHEN 2 THEN '★★' WHEN 3 THEN '★★★' WHEN 4 THEN '★★★★' ELSE '★★★★★' END",
                "a.rating > 0",
            )?,
        },
        MetadataFacet {
            key: "video_codec".into(),
            display_name: "视频编码".into(),
            description: "视频编解码格式".into(),
            items: metadata_items(conn, "lower(a.video_codec)", "upper(a.video_codec)", "a.video_codec IS NOT NULL AND trim(a.video_codec) != ''")?,
        },
        MetadataFacet {
            key: "audio_codec".into(),
            display_name: "音频编码".into(),
            description: "视频中的音频编码格式".into(),
            items: metadata_items(conn, "lower(a.audio_codec)", "upper(a.audio_codec)", "a.audio_codec IS NOT NULL AND trim(a.audio_codec) != ''")?,
        },
        MetadataFacet {
            key: "palette_top3".into(),
            display_name: "主要颜色".into(),
            description: "画面前三色中占比至少 10% 的颜色".into(),
            items: primary_color_items(conn)?,
        },
        MetadataFacet {
            key: "has_location".into(),
            display_name: "定位信息".into(),
            description: "素材是否携带 GPS 定位".into(),
            items: metadata_items(
                conn,
                "CASE WHEN a.latitude IS NOT NULL AND a.longitude IS NOT NULL THEN 'yes' ELSE 'no' END",
                "CASE WHEN a.latitude IS NOT NULL AND a.longitude IS NOT NULL THEN '有定位' ELSE '无定位' END",
                "1",
            )?,
        },
    ];
    facets.retain(|facet| !facet.items.is_empty());
    Ok(facets)
}

fn list_folder_items(
    conn: &Connection,
    library_root: Option<&str>,
) -> AppResult<Vec<MetadataFacetItem>> {
    let mut stmt = conn.prepare("SELECT file_path FROM assets WHERE deleted_at IS NULL")?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let root = library_root
        .map(crate::utils::path::normalize_path)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string());
    let mut counts = BTreeMap::<String, i64>::new();
    for path in paths {
        let normalized = crate::utils::path::normalize_path(&path);
        let Some(parent) = normalized
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
        else {
            continue;
        };
        let folder = parent.trim_end_matches('/').to_string();
        let mut current = Some(folder);
        while let Some(folder) = current {
            let inside_root = root
                .as_ref()
                .map(|value| folder == *value || folder.starts_with(&format!("{value}/")))
                .unwrap_or(true);
            if !inside_root {
                break;
            }
            let label = if let Some(root) = &root {
                if folder == *root {
                    "总库根目录".to_string()
                } else if let Some(relative) = folder.strip_prefix(&format!("{root}/")) {
                    relative.to_string()
                } else {
                    folder.clone()
                }
            } else {
                folder.clone()
            };
            *counts.entry(format!("{folder}\t{label}")).or_default() += 1;
            current = folder
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string());
        }
    }
    let mut items = counts
        .into_iter()
        .map(|(key, count)| {
            let (value, label) = key.split_once('\t').unwrap_or((&key, &key));
            MetadataFacetItem {
                value: value.to_string(),
                label: label.to_string(),
                count,
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    items.truncate(80);
    Ok(items)
}

/// 聚合返回每页素材的标签（一次 IN 查询，Rust 侧归组）
fn fill_tags(conn: &Connection, items: &mut [Asset]) -> AppResult<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids = items
        .iter()
        .map(|a| a.id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT at.asset_id, t.id, t.name, COALESCE(t.canonical_name,t.name),
                COALESCE(t.normalized_name,lower(trim(t.name))), COALESCE(t.facet_key,'custom'),
                t.parent_id, COALESCE(t.status,'active'), COALESCE(t.is_system,0),
                t.is_preset, t.sort_order, {FACET_EFFECTIVE} AS facet_effective
           FROM asset_tags at JOIN tags t ON t.id = at.tag_id
          WHERE at.asset_id IN ({ids}) AND COALESCE(t.status,'active') != 'blocked'
          ORDER BY t.sort_order, t.id"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Tag {
                id: r.get(1)?,
                name: r.get(2)?,
                canonical_name: r.get(3)?,
                normalized_name: r.get(4)?,
                facet_key: r.get(5)?,
                parent_id: r.get(6)?,
                status: r.get(7)?,
                is_system: r.get::<_, i64>(8)? != 0,
                is_preset: r.get::<_, i64>(9)? != 0,
                sort_order: r.get(10)?,
                facet_effective: r.get::<_, i64>(11)? != 0,
                asset_count: 0,
                total_count: 0,
                aliases: Vec::new(),
                path: String::new(),
            },
        ))
    })?;
    for row in rows {
        let (asset_id, mut tag) = row?;
        super::tags::hydrate_metadata(conn, &mut tag)?;
        if let Some(a) = items.iter_mut().find(|a| a.id == asset_id) {
            a.tags.push(tag);
        }
    }
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Asset> {
    let mut asset = conn.query_row(
        &format!("SELECT {COLUMNS} FROM assets WHERE id = ?1"),
        [id],
        from_row,
    )?;
    let mut items = vec![asset.clone()];
    fill_tags(conn, &mut items)?;
    asset.tags = items.pop().map(|a| a.tags).unwrap_or_default();
    Ok(asset)
}

// 8 参数为素材入库字段的内聚集合，收进结构体需同步改全部调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    file_path: &str,
    file_name: &str,
    file_ext: &str,
    file_size: i64,
    mime_type: &str,
    modified_at: i64,
) -> AppResult<i64> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![file_path, file_name, file_ext, file_size, mime_type, now, modified_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete(conn: &Connection, ids: &[i64]) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let n = conn.execute(&format!("DELETE FROM assets WHERE id IN ({list})"), [])?;
    Ok(n as u64)
}

/// R-22 软删入回收站：deleted_at 置当前时间（重复软删不覆盖首次时间）
pub fn soft_delete(conn: &Connection, ids: &[i64]) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let now = chrono::Utc::now().timestamp_millis();
    let n = conn.execute(
        &format!("UPDATE assets SET deleted_at = ?1 WHERE id IN ({list}) AND deleted_at IS NULL"),
        [now],
    )?;
    Ok(n as u64)
}

/// R-22 从回收站恢复：deleted_at 置空
pub fn restore(conn: &Connection, ids: &[i64]) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let n = conn.execute(
        &format!(
            "UPDATE assets SET deleted_at = NULL WHERE id IN ({list}) AND deleted_at IS NOT NULL"
        ),
        [],
    )?;
    Ok(n as u64)
}

/// R-22 查超期回收站项（deleted_at < cutoff），返回 (id, file_path) 供锁外删文件
pub fn list_expired_trash(conn: &Connection, cutoff_ms: i64) -> AppResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_path FROM assets WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
    )?;
    let rows = stmt.query_map([cutoff_ms], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// EXIF 回写补丁（均为可空，提取不到就存 NULL）
#[derive(Debug, Clone, Copy, Default)]
pub struct ExifPatch<'a> {
    pub camera: Option<&'a str>,
    pub lens: Option<&'a str>,
    pub iso: Option<i64>,
    pub aperture: Option<f64>,
    pub shutter: Option<&'a str>,
    pub focal: Option<f64>,
    pub taken_at: Option<i64>,
    /// GPS 定位（带符号十进制度）：COALESCE 只补空，不覆盖已有值（重扫不丢存量定位）
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// EXIF 元信息回写（入库管线阶段二调用）
pub fn set_exif(conn: &Connection, id: i64, ex: &ExifPatch<'_>) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET camera=?1, lens=?2, iso=?3, aperture=?4, shutter=?5, focal=?6,
            taken_at = COALESCE(taken_at, ?7),
            latitude = COALESCE(latitude, ?8),
            longitude = COALESCE(longitude, ?9) WHERE id=?10",
        rusqlite::params![
            ex.camera,
            ex.lens,
            ex.iso,
            ex.aperture,
            ex.shutter,
            ex.focal,
            ex.taken_at,
            ex.latitude,
            ex.longitude,
            id
        ],
    )?;
    Ok(())
}

pub fn find_by_path(conn: &Connection, file_path: &str) -> AppResult<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM assets WHERE file_path = ?1")?;
    let mut rows = stmt.query([file_path])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// 媒体探测回写（指导书 §7.4/§7.5）：结构字段 + metadata_scanned_at / metadata_error。
/// 只回写本次探测结果；error 与 NULL 分属「读取失败」「未读取」，不可混为一谈。
#[derive(Debug, Default, Clone)]
pub struct MediaProbeUpdate {
    pub media_kind: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub container_format: Option<String>,
    pub video_profile: Option<String>,
    pub pixel_format: Option<String>,
    pub frame_rate: Option<f64>,
    pub rotation: Option<i64>,
    pub media_metadata_json: Option<String>,
    pub metadata_version: Option<i64>,
    /// 探测失败原因（可辨识）；None = 成功
    pub error: Option<String>,
}

/// 把一次探测结果回写到 assets（媒体探测协议版本 V12 之后）。
pub fn update_media_metadata(conn: &Connection, id: i64, m: &MediaProbeUpdate) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "UPDATE assets SET media_kind=?1, width=?2, height=?3, duration_ms=?4, video_codec=?5, audio_codec=?6,
            container_format=?7, video_profile=?8, pixel_format=?9, frame_rate=?10, rotation=?11,
            media_metadata_json=?12, metadata_error=?13, metadata_version=?14, metadata_scanned_at=?15
         WHERE id=?16",
        rusqlite::params![
            m.media_kind,
            m.width,
            m.height,
            m.duration_ms,
            m.video_codec,
            m.audio_codec,
            m.container_format,
            m.video_profile,
            m.pixel_format,
            m.frame_rate,
            m.rotation,
            m.media_metadata_json,
            m.error,
            m.metadata_version,
            now,
            id
        ],
    )?;
    Ok(())
}

/// 列出缺少媒体元数据的视频（用于「仅缺字段」回填范围）。视频 = MIME video/* 或已有 duration。
pub fn list_video_ids_needing_metadata(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM assets
          WHERE deleted_at IS NULL
            AND (mime_type LIKE 'video/%' OR duration_ms IS NOT NULL)
            AND (duration_ms IS NULL OR video_codec IS NULL)",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// 列出全部视频的 id（用于「全部视频」回填范围）。
pub fn list_video_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM assets
          WHERE deleted_at IS NULL AND (mime_type LIKE 'video/%' OR duration_ms IS NOT NULL)",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// 列出缺 GPS 定位 / 拍摄时间的素材 id（定位回填 scope=missing）：
/// 图片缺 latitude / taken_at（R0-2：JPG 拍摄时间解析修复前全部为空，需补）；
/// 视频缺 latitude / taken_at（两者任一缺失即需补）。
pub fn list_ids_needing_geo_taken(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM assets
          WHERE deleted_at IS NULL
            AND ((mime_type LIKE 'image/%' AND (latitude IS NULL OR taken_at IS NULL))
              OR (mime_type LIKE 'video/%' AND (latitude IS NULL OR taken_at IS NULL)))",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// 定位回填 scope=all：全部未删除素材（不复用色板候选谓词，避免漏掉无封面视频）。
pub fn list_geo_taken_all_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM assets WHERE deleted_at IS NULL")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// 回填 GPS 定位与拍摄时间（仅补空，不覆盖已有值；老素材无定位保持 NULL）。
/// 注意 COALESCE 参数顺序：已有列值在前，新值在后 —— COALESCE(旧, 新) 才是「只补空」。
/// W2-8：批量评级（0–5；0 = 清除评级）。
pub fn set_rating(conn: &Connection, ids: &[i64], rating: i64) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if !(0..=5).contains(&rating) {
        return Err(AppError::msg("评级只允许 0–5（0 = 清除）"));
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let n = conn.execute(
        &format!("UPDATE assets SET rating = ?1 WHERE id IN ({list})"),
        rusqlite::params![rating],
    )?;
    Ok(n as u64)
}

/// W2-8：批量手动旋转（0/90/180/270；写入 user_rotation，严禁碰 ffprobe 的 rotation）。
pub fn set_user_rotation(conn: &Connection, ids: &[i64], rotation: i64) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ![0, 90, 180, 270].contains(&rotation) {
        return Err(AppError::msg("旋转角只允许 0/90/180/270"));
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let n = conn.execute(
        &format!("UPDATE assets SET user_rotation = ?1 WHERE id IN ({list})"),
        rusqlite::params![rotation],
    )?;
    Ok(n as u64)
}

/// W1-4：图片宽高缺失候选（RAW 无法被 image_dimensions 解码的历史存量）。
pub fn list_ids_needing_dimensions(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM assets
          WHERE deleted_at IS NULL
            AND mime_type LIKE 'image/%'
            AND (width IS NULL OR height IS NULL)",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// W1-4：写回宽高（只补空，不覆盖已有值）。
pub fn set_dimensions(
    conn: &Connection,
    id: i64,
    width: Option<i64>,
    height: Option<i64>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET width = COALESCE(width, ?1),
            height = COALESCE(height, ?2)
          WHERE id = ?3",
        rusqlite::params![width, height, id],
    )?;
    Ok(())
}

pub fn set_geo_taken(
    conn: &Connection,
    id: i64,
    latitude: Option<f64>,
    longitude: Option<f64>,
    taken_at: Option<i64>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET latitude = COALESCE(latitude, ?1),
            longitude = COALESCE(longitude, ?2),
            taken_at = COALESCE(taken_at, ?3)
          WHERE id = ?4",
        rusqlite::params![latitude, longitude, taken_at, id],
    )?;
    Ok(())
}

pub fn set_hash(conn: &Connection, id: i64, hash: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET hash = ?1 WHERE id = ?2",
        rusqlite::params![hash, id],
    )?;
    Ok(())
}

pub fn hash_exists(conn: &Connection, hash: &str) -> AppResult<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM assets WHERE hash = ?1", [hash], |r| {
        r.get(0)
    })?;
    Ok(n > 0)
}

pub fn set_placeholder_path(conn: &Connection, id: i64, path: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET placeholder_path = ?1 WHERE id = ?2",
        rusqlite::params![path, id],
    )?;
    Ok(())
}

/// W5d（§W5d）：写感知哈希（phash 列在 V19 追加；0 是无意义值，不入库）。
pub fn set_phash(conn: &Connection, id: i64, phash: u64) -> AppResult<()> {
    if phash == 0 {
        return Ok(());
    }
    conn.execute(
        "UPDATE assets SET phash = ?1 WHERE id = ?2",
        rusqlite::params![phash as i64, id],
    )?;
    Ok(())
}

/// 写算法色板、主导三维度和多颜色关系索引；同一事务保证色条与颜色筛选同步。
pub fn set_palette(
    conn: &Connection,
    id: i64,
    palette_json: &str,
    version: i64,
    hue: i64,
    sat: i64,
    lum: i64,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    let segments = parse_palette_json(Some(palette_json.to_string()));
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE assets SET palette_json=?1, palette_version=?2, palette_scanned_at=?3,
            dominant_hue=?4, dominant_sat=?5, dominant_lum=?6 WHERE id=?7",
        rusqlite::params![palette_json, version, now, hue, sat, lum, id],
    )?;
    tx.execute("DELETE FROM asset_palette_colors WHERE asset_id=?1", [id])?;
    if let Some(segments) = segments {
        let mut stmt = tx.prepare(
            "INSERT INTO asset_palette_colors (asset_id, rank, color_bucket, ratio)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (rank, segment) in segments.into_iter().enumerate() {
            let (bucket, _) =
                crate::db::palette_bucket::bucket_of_rgb(segment.r, segment.g, segment.b);
            stmt.execute(rusqlite::params![
                id,
                rank as i64,
                bucket,
                segment.ratio as f64
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// FB2-08：色板回算的候选范围。
/// 只取图片，以及有 hd 封面的视频 —— 视频的 placeholder 可能是 write_generic 写的
/// 纯 UI 占位图（暖灰底 + 品牌蓝块，thumbnail.rs），拿它算"主色"会写进一个
/// 与素材无关的 dominant_hue，污染按颜色检索（FX-13）。
const PALETTE_CANDIDATE_PRED: &str = "deleted_at IS NULL AND (\
       mime_type LIKE 'image/%' \
    OR (mime_type LIKE 'video/%' AND hd_thumbnail_path IS NOT NULL))";

/// FB4-03（§5.3）：色板状态统计。只取计数所需字段（候选行仅查询 palette_json，
/// 不构造完整 Asset）；ready 在 Rust 侧逐行调用 parse_palette_json 精确统计，
/// 不能只用 `palette_json IS NOT NULL` 近似（损坏 JSON / 空数组会被误判为已生成）。
pub fn get_palette_status(conn: &Connection) -> AppResult<PaletteStatus> {
    let total_assets: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assets WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT palette_json FROM assets WHERE {PALETTE_CANDIDATE_PRED}"
    ))?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    let mut eligible: i64 = 0;
    let mut ready: i64 = 0;
    for row in rows {
        let raw = row?;
        eligible += 1;
        if parse_palette_json(raw).is_some() {
            ready += 1;
        }
    }
    Ok(PaletteStatus {
        total_assets,
        eligible,
        ready,
        missing: eligible - ready,
        unavailable: total_assets - eligible,
    })
}

/// FB4-03（§5.4）：列出缺少色板的素材 id（用于「仅缺色板」回填范围）。
/// 修正前只判 `palette_json IS NULL`；现在查询候选行的 (id, palette_json) 后用
/// parse_palette_json 过滤 —— NULL / 空字符串 / `[]` / 损坏 JSON 全部纳入 missing，
/// 与状态统计共用同一 parser 与候选谓词，杜绝「状态说 missing>0，回算却 total=0」。
pub fn list_ids_needing_palette(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, palette_json FROM assets WHERE {PALETTE_CANDIDATE_PRED}"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut ids = Vec::new();
    for row in rows {
        let (id, raw) = row?;
        if parse_palette_json(raw).is_none() {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// W5d（§W5d）：列出缺少感知哈希的素材 id（phash 只对图片有意义；视频无 dHash）。
/// 覆盖三个范围：missing（图片 + phash 为空）/ all（全部图片）/ ids（指定 id 中未删的图片）。
pub fn list_ids_needing_phash(conn: &Connection, scope: &str, ids: &[i64]) -> AppResult<Vec<i64>> {
    match scope {
        "missing" => {
            let mut stmt = conn.prepare(
                "SELECT id FROM assets WHERE deleted_at IS NULL AND mime_type LIKE 'image/%' AND phash IS NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        }
        "all" => {
            let mut stmt = conn.prepare(
                "SELECT id FROM assets WHERE deleted_at IS NULL AND mime_type LIKE 'image/%'",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        }
        _ => {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT id FROM assets WHERE id IN ({list}) AND mime_type LIKE 'image/%'"
            ))?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        }
    }
}

/// FB4-03（§5.5）：按 id 查询色板补丁（定向同步用，只返回色板相关字段）。
/// 契约：空 ids 直接返回空数组；单次最多 1000 个 id（超限返回明确错误，前端负责分批）；
/// SQL 参数化；只返回数据库中存在的 id；palette 使用 parse_palette_json；不修改任何记录。
pub fn get_asset_palette_patches(
    conn: &Connection,
    ids: &[i64],
) -> AppResult<Vec<AssetPalettePatch>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > 1000 {
        return Err(AppError::msg("一次最多查询 1000 个素材的色板，请分批"));
    }
    let placeholders = (1..=ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id, palette_json, dominant_hue, dominant_sat, dominant_lum
           FROM assets WHERE id IN ({placeholders})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok(AssetPalettePatch {
            id: r.get(0)?,
            palette: parse_palette_json(r.get(1)?),
            dominant_hue: r.get(2)?,
            dominant_sat: r.get(3)?,
            dominant_lum: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// FB2-08：列出全部可算色板的素材 id（用于「全部」色板回算范围）。
/// 语义是"全部可算色板的素材"而非"全部素材"：与 list_ids_needing_palette 同谓词（FX-13）。
pub fn list_all_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM assets WHERE {PALETTE_CANDIDATE_PRED}"
    ))?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// 列出全部未删除图片，用于图片尺寸回填的 all 范围。
pub fn list_image_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn
        .prepare("SELECT id FROM assets WHERE deleted_at IS NULL AND mime_type LIKE 'image/%'")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

pub fn set_hd_thumbnail_path(conn: &Connection, id: i64, path: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET hd_thumbnail_path = ?1 WHERE id = ?2",
        rusqlite::params![path, id],
    )?;
    Ok(())
}

/// B04：move 导出后同步更新库记录的文件路径与文件名（unique_dest 可能加了后缀）
pub fn update_file_path_and_name(
    conn: &Connection,
    id: i64,
    file_path: &str,
    file_name: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE assets SET file_path = ?1, file_name = ?2 WHERE id = ?3",
        rusqlite::params![file_path, file_name, id],
    )?;
    Ok(())
}

/// B27：清空所有素材的占位图路径（清缓存时回写 DB，避免 DB 指向已删文件）
pub fn clear_all_placeholder_paths(conn: &Connection) -> AppResult<()> {
    conn.execute("UPDATE assets SET placeholder_path = NULL", [])?;
    Ok(())
}

/// B27：清空所有素材的高清缩略图路径（清缓存时回写 DB）
pub fn clear_all_hd_thumbnail_paths(conn: &Connection) -> AppResult<()> {
    conn.execute("UPDATE assets SET hd_thumbnail_path = NULL", [])?;
    Ok(())
}

/// 列出非空的一句话描述（标签与分类设置页展示用；描述走 FTS 模糊搜索，不参与分面精确筛选）。
pub fn list_content_descriptions(
    conn: &Connection,
    limit: i64,
) -> AppResult<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_name, content_description FROM assets
          WHERE deleted_at IS NULL AND content_description IS NOT NULL AND content_description != ''
          ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        crate::db::init_memory().unwrap()
    }

    fn ins(c: &Connection, path: &str, mime: &str) -> i64 {
        insert(c, path, "a", "jpg", 100, mime, 1).unwrap()
    }

    /// FX-13：回算候选不含无封面视频，也不含非图非视频。
    #[test]
    fn palette_candidates_exclude_videos_without_cover() {
        let c = mem();
        let img = ins(&c, "/a.jpg", "image/jpeg");
        let vid_no = ins(&c, "/b.mp4", "video/mp4"); // 无 hd
        let vid_ok = ins(&c, "/c.mp4", "video/mp4");
        set_hd_thumbnail_path(&c, vid_ok, "/hd/c.jpg").unwrap();
        let other = ins(&c, "/d.psd", "application/octet-stream");

        let ids = list_ids_needing_palette(&c).unwrap();
        assert!(ids.contains(&img));
        assert!(ids.contains(&vid_ok));
        assert!(
            !ids.contains(&vid_no),
            "无封面视频不得进回算队列（会拿到 UI 占位图）"
        );
        assert!(!ids.contains(&other), "非图非视频不得进回算队列");
        // list_all_ids 同谓词
        let all = list_all_ids(&c).unwrap();
        assert!(all.contains(&vid_ok));
        assert!(!all.contains(&vid_no));
        assert!(!all.contains(&other));
        // 已算过的不再进 missing
        set_palette(
            &c,
            img,
            r##"[{"hex":"#000000","r":0,"g":0,"b":0,"ratio":1.0}]"##,
            1,
            0,
            0,
            0,
        )
        .unwrap();
        assert!(!list_ids_needing_palette(&c).unwrap().contains(&img));
    }

    /// FX-05：set_palette 写入后，list/get 能把 palette_json 解析成 DTO 数组。
    #[test]
    fn list_returns_parsed_palette() {
        let c = mem();
        let id = ins(&c, "/a.jpg", "image/jpeg");
        let json = r##"[{"hex":"#1b2a3c","r":27,"g":42,"b":60,"ratio":0.31},
                       {"hex":"#e6dfc8","r":230,"g":223,"b":200,"ratio":0.22}]"##;
        set_palette(&c, id, json, 1, 213, 55, 24).unwrap();
        let a = get(&c, id).unwrap();
        let p = a.palette.clone().expect("应解析出色板");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].hex, "#1b2a3c");
        assert_eq!(a.dominant_hue, Some(213));
        // 序列化后前端拿到的键名是 palette，不是 paletteJson
        let v = serde_json::to_value(&a).unwrap();
        assert!(v.get("palette").is_some());
        assert!(v.get("paletteJson").is_none(), "palette_json 不得直接暴露");
    }

    #[test]
    fn set_palette_keeps_color_index_in_sync() {
        let c = mem();
        let id = ins(&c, "/indexed.jpg", "image/jpeg");
        let json = r##"[{"hex":"#e02020","r":224,"g":32,"b":32,"ratio":0.7},{"hex":"#2030a0","r":32,"g":48,"b":160,"ratio":0.2}]"##;
        set_palette(&c, id, json, 1, 0, 0, 0).unwrap();
        let rows: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM asset_palette_colors WHERE asset_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 2);

        let replacement = r##"[{"hex":"#20c040","r":32,"g":192,"b":64,"ratio":0.8}]"##;
        set_palette(&c, id, replacement, 1, 0, 0, 0).unwrap();
        let rows: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM asset_palette_colors WHERE asset_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "重算后旧色板索引必须被替换");
    }

    /// FX-05：损坏 JSON 不得让查询失败，只降级为 None。
    #[test]
    fn broken_palette_json_degrades_to_none() {
        let c = mem();
        let id = ins(&c, "/a.jpg", "image/jpeg");
        set_palette(&c, id, "{not json", 1, 0, 0, 0).unwrap();
        let a = get(&c, id).unwrap();
        assert!(a.palette.is_none());
        assert_eq!(a.dominant_hue, Some(0), "dominant_* 仍应可读");
        // 空数组同样归一为 None（Option 语义 = 未计算）
        set_palette(&c, id, "[]", 1, 0, 0, 0).unwrap();
        let b = get(&c, id).unwrap();
        assert!(b.palette.is_none(), "空数组应归一为 None");
    }

    // ── FB4-03：色板单一解析函数（§10.4）──

    #[test]
    fn parse_palette_json_returns_none_for_empty_or_broken() {
        let valid = r##"[{"hex":"#000000","r":0,"g":0,"b":0,"ratio":1.0}]"##;
        assert!(parse_palette_json(None).is_none(), "NULL -> None");
        assert!(
            parse_palette_json(Some(String::new())).is_none(),
            "空串 -> None"
        );
        assert!(
            parse_palette_json(Some("   \n\t  ".into())).is_none(),
            "全空白 -> None"
        );
        assert!(
            parse_palette_json(Some("[]".into())).is_none(),
            "[] -> None"
        );
        assert!(
            parse_palette_json(Some("{not json".into())).is_none(),
            "损坏 JSON -> None"
        );
        let some = parse_palette_json(Some(valid.to_string()));
        assert!(some.is_some(), "合法非空数组 -> Some");
        assert_eq!(some.unwrap().len(), 1);
    }

    /// 状态五个字段计算正确；非候选素材计入 total/unavailable 不计入 eligible。
    #[test]
    fn palette_status_counts_exactly() {
        let c = mem();
        let img1 = ins(&c, "/a.jpg", "image/jpeg");
        let _img2 = ins(&c, "/b.jpg", "image/jpeg"); // NULL 色板：missing
        let vid_ok = ins(&c, "/c.mp4", "video/mp4");
        set_hd_thumbnail_path(&c, vid_ok, "/hd/c.jpg").unwrap();
        let _vid_no = ins(&c, "/d.mp4", "video/mp4"); // 无封面：非候选
        let _other = ins(&c, "/e.psd", "application/octet-stream"); // 非图非视频：非候选
                                                                    // 删除素材不计入 total（total 只算 deleted_at IS NULL）
        let deleted = ins(&c, "/f.jpg", "image/jpeg");
        soft_delete(&c, &[deleted]).unwrap();

        // img1 已生成；img2 空（NULL）；vid_ok 损坏 JSON
        set_palette(
            &c,
            img1,
            r##"[{"hex":"#000000","r":0,"g":0,"b":0,"ratio":1.0}]"##,
            1,
            0,
            0,
            0,
        )
        .unwrap();
        set_palette(&c, vid_ok, "{broken", 1, 0, 0, 0).unwrap();

        let st = get_palette_status(&c).unwrap();
        // total = 全部未删除 = 5（img1, img2, vid_ok, vid_no, other）
        assert_eq!(st.total_assets, 5);
        // eligible = 候选（img1, img2, vid_ok）= 3
        assert_eq!(st.eligible, 3);
        // ready = 只有 img1 的合法色板 = 1（vid_ok 是损坏 JSON，不算 ready）
        assert_eq!(st.ready, 1);
        assert_eq!(st.missing, 2);
        assert_eq!(st.unavailable, 2);
    }

    /// 损坏 JSON 计入 missing：list_ids_needing_palette 包含 NULL、空串、空数组、损坏 JSON。
    #[test]
    fn missing_includes_broken_and_empty_palette_json() {
        let c = mem();
        let null_id = ins(&c, "/a.jpg", "image/jpeg");
        let empty_id = ins(&c, "/b.jpg", "image/jpeg");
        let arr_id = ins(&c, "/c.jpg", "image/jpeg");
        let broken_id = ins(&c, "/d.jpg", "image/jpeg");
        let ok_id = ins(&c, "/e.jpg", "image/jpeg");
        set_palette(&c, empty_id, "", 1, 0, 0, 0).unwrap();
        set_palette(&c, arr_id, "[]", 1, 0, 0, 0).unwrap();
        set_palette(&c, broken_id, "{nope", 1, 0, 0, 0).unwrap();
        set_palette(
            &c,
            ok_id,
            r##"[{"hex":"#000000","r":0,"g":0,"b":0,"ratio":1.0}]"##,
            1,
            0,
            0,
            0,
        )
        .unwrap();

        let missing = list_ids_needing_palette(&c).unwrap();
        assert!(missing.contains(&null_id), "NULL 应进 missing");
        assert!(missing.contains(&empty_id), "空串应进 missing");
        assert!(missing.contains(&arr_id), "空数组应进 missing");
        assert!(missing.contains(&broken_id), "损坏 JSON 应进 missing");
        assert!(!missing.contains(&ok_id), "有效色板不在 missing");
    }

    /// patch 查询：只返回请求且存在的 id，并正确解析 palette。
    #[test]
    fn palette_patch_returns_only_existing_ids_with_parsed_palette() {
        let c = mem();
        let a = ins(&c, "/a.jpg", "image/jpeg");
        let b = ins(&c, "/b.jpg", "image/jpeg");
        set_palette(
            &c,
            a,
            r##"[{"hex":"#111111","r":17,"g":17,"b":17,"ratio":1.0}]"##,
            1,
            10,
            20,
            30,
        )
        .unwrap();
        // b 保持 NULL
        let patches = get_asset_palette_patches(&c, &[a, 9999, b]).unwrap();
        assert_eq!(patches.len(), 2, "不存在的 id 不返回");
        let pa = patches.iter().find(|p| p.id == a).unwrap();
        assert_eq!(pa.palette.as_ref().unwrap()[0].hex, "#111111");
        assert_eq!(pa.dominant_hue, Some(10));
        assert_eq!(pa.dominant_sat, Some(20));
        assert_eq!(pa.dominant_lum, Some(30));
        let pb = patches.iter().find(|p| p.id == b).unwrap();
        assert!(pb.palette.is_none());
        // 空 ids → 空数组
        assert!(get_asset_palette_patches(&c, &[]).unwrap().is_empty());
    }

    /// patch 查询超过 1000 id 返回错误。
    #[test]
    fn palette_patch_rejects_over_1000_ids() {
        let c = mem();
        let ids: Vec<i64> = (1..=1001).collect();
        let err = get_asset_palette_patches(&c, &ids).unwrap_err();
        assert!(err.to_string().contains("1000"));
    }

    // ── GPS 定位 / 拍摄时间回填辅助 ──

    /// list_ids_needing_geo_taken：图片缺定位 / 缺拍摄时间 / 视频缺定位或缺 taken_at 才入选。
    /// R0-2：图片分支加 taken_at IS NULL —— JPG 解析修复前存量 taken_at 全空必须能入选。
    #[test]
    fn list_ids_needing_geo_taken_selection() {
        let c = mem();
        let img_no_geo = ins(&c, "/i1.jpg", "image/jpeg");
        let img_geo = ins(&c, "/i2.jpg", "image/jpeg");
        let vid_no_taken = ins(&c, "/v1.mp4", "video/mp4");
        let vid_full = ins(&c, "/v2.mp4", "video/mp4");
        set_geo_taken(&c, img_geo, Some(30.25), Some(120.16), None).unwrap();
        set_geo_taken(&c, vid_no_taken, Some(30.25), Some(120.16), None).unwrap();
        set_geo_taken(
            &c,
            vid_full,
            Some(30.25),
            Some(120.16),
            Some(1_710_484_200_000),
        )
        .unwrap();

        let ids = list_ids_needing_geo_taken(&c).unwrap();
        assert!(ids.contains(&img_no_geo), "图片缺定位应入选");
        assert!(
            ids.contains(&img_geo),
            "图片有定位但缺 taken_at 应入选（R0-2）"
        );
        assert!(ids.contains(&vid_no_taken), "视频缺 taken_at 应入选");
        assert!(!ids.contains(&vid_full), "视频定位+时间齐全不入选");
    }

    /// R0-2 新增：图片已有定位但缺 taken_at 也应入选（解析修复前存量全空）。
    #[test]
    fn geo_taken_backfill_selects_image_missing_taken_at() {
        let c = mem();
        let img_no_taken = ins(&c, "/i3.jpg", "image/jpeg");
        let img_full = ins(&c, "/i4.jpg", "image/jpeg");
        set_geo_taken(&c, img_no_taken, Some(30.25), Some(120.16), None).unwrap();
        set_geo_taken(
            &c,
            img_full,
            Some(30.25),
            Some(120.16),
            Some(1_710_484_200_000),
        )
        .unwrap();

        let ids = list_ids_needing_geo_taken(&c).unwrap();
        assert!(
            ids.contains(&img_no_taken),
            "图片有定位但缺 taken_at 应入选（R0-2）"
        );
        assert!(!ids.contains(&img_full), "图片定位+时间齐全不入选");
    }

    /// set_geo_taken 只补空不覆盖：已有值传新值也不变，空值被补上。
    #[test]
    fn set_geo_taken_fills_only_nulls() {
        let c = mem();
        let id = ins(&c, "/a.jpg", "image/jpeg");
        set_geo_taken(&c, id, Some(30.25), Some(120.16), None).unwrap();
        // 再次传入不同值：已有经纬度不得被覆盖；taken_at 仍为空可补。
        set_geo_taken(&c, id, Some(99.0), Some(99.0), Some(1_710_484_200_000)).unwrap();
        let a = get(&c, id).unwrap();
        assert_eq!(a.latitude, Some(30.25), "已有纬度不得被覆盖");
        assert_eq!(a.longitude, Some(120.16), "已有经度不得被覆盖");
        assert_eq!(a.taken_at, Some(1_710_484_200_000), "空 taken_at 应被补上");
    }

    /// 分面：前三色按色系合并并应用 10% 门槛；同一素材可进入多个颜色 + 定位计数。
    #[test]
    fn metadata_facets_include_primary_colors_and_location() {
        let c = mem();
        let red_a = ins(&c, "/r1.jpg", "image/jpeg");
        let red_b = ins(&c, "/r2.jpg", "image/jpeg");
        let green = ins(&c, "/g.jpg", "image/jpeg");
        set_palette(
            &c,
            red_a,
            r##"[{"hex":"#e02020","r":224,"g":32,"b":32,"ratio":0.5},{"hex":"#2030a0","r":32,"g":48,"b":160,"ratio":0.3}]"##,
            1, 0, 0, 0,
        ).unwrap();
        set_palette(
            &c,
            red_b,
            r##"[{"hex":"#e02020","r":224,"g":32,"b":32,"ratio":0.06},{"hex":"#d82828","r":216,"g":40,"b":40,"ratio":0.05}]"##,
            1, 0, 0, 0,
        ).unwrap();
        set_palette(
            &c,
            green,
            r##"[{"hex":"#20c040","r":32,"g":192,"b":64,"ratio":0.09},{"hex":"#e02020","r":224,"g":32,"b":32,"ratio":0.08}]"##,
            1, 0, 0, 0,
        ).unwrap();
        set_geo_taken(&c, red_a, Some(30.25), Some(120.16), None).unwrap();

        let facets = list_metadata_facets(&c, None).unwrap();
        let hue = facets
            .iter()
            .find(|f| f.key == "palette_top3")
            .expect("应有主要颜色分面");
        let get_count = |v: &str| {
            hue.items
                .iter()
                .find(|i| i.value == v)
                .map(|i| i.count)
                .unwrap_or(0)
        };
        assert_eq!(get_count("红"), 2, "同色系 6%+5% 合并后应达到门槛");
        assert_eq!(get_count("天蓝"), 1, "同一素材可以同时进入红、天蓝");
        assert_eq!(get_count("绿"), 0, "低于 10% 的颜色不应进入分面");

        let loc = facets
            .iter()
            .find(|f| f.key == "has_location")
            .expect("应有定位分面");
        let loc_count = |v: &str| {
            loc.items
                .iter()
                .find(|i| i.value == v)
                .map(|i| i.count)
                .unwrap_or(0)
        };
        assert_eq!(loc_count("yes"), 1);
        assert_eq!(loc_count("no"), 2);
    }

    /// W1-1（V19）：新列经 INSERT/SELECT 往返不丢值（COLUMNS 位置映射 + from_row 索引 49–52 对齐）。
    #[test]
    fn asset_roundtrip_includes_new_columns() {
        let c = mem();
        let id = ins(&c, "/a.jpg", "image/jpeg");
        c.execute(
            "UPDATE assets SET favorite=1, rating=5, user_rotation=90, phash=12345678901234567 WHERE id=?1",
            [id],
        )
        .unwrap();
        let a = get(&c, id).unwrap();
        assert_eq!(a.favorite, 1);
        assert_eq!(a.rating, 5);
        assert_eq!(a.user_rotation, 90);
        assert_eq!(a.phash, Some(12345678901234567));
        // 默认值：新插入行四列均为默认
        let id2 = ins(&c, "/b.jpg", "image/jpeg");
        let b = get(&c, id2).unwrap();
        assert_eq!(b.favorite, 0);
        assert_eq!(b.rating, 0);
        assert_eq!(b.user_rotation, 0);
        assert_eq!(b.phash, None);
    }
}
