/** 素材相关命令封装（对应 commands/assets_cmd.rs） */
import { invoke } from "./client";
import type { Asset, AssetFilter, AssetPage, DupGroup, MetadataFacet, NumericDomain } from "@/types/asset";

export type DeleteStrategy = "remove_from_library" | "delete_file";

/** B02/B03：删除结果——deleted 为已从库删除数，failedFiles 为磁盘删除失败的 asset id */
export interface DeleteResult {
  deleted: number;
  failedFiles: number[];
}

export function listAssets(filter: AssetFilter): Promise<AssetPage> {
  return invoke<AssetPage>("list_assets", { filter });
}

/** 取当前筛选结果的全部 id（全选/反选/批量操作用；只返回 id 数组，
 *  不拉完整 Asset，不触发 asset 协议放行） */
export function listAssetIds(filter: AssetFilter): Promise<number[]> {
  return invoke<number[]>("list_asset_ids", { filter });
}

/** 文件自身携带的格式、时间、设备和拍摄参数分面。 */
export function listMetadataFacets(): Promise<MetadataFacet[]> {
  return invoke<MetadataFacet[]>("list_metadata_facets");
}

/** Phase 4（§5.3）：数值字段的 NumericDomain 单一事实源（含预设/单位/边界/量纲阈值/运算符）。 */
export function getNumericDomains(): Promise<NumericDomain[]> {
  return invoke<NumericDomain[]>("get_numeric_domains");
}

export function getAsset(id: number): Promise<Asset> {
  return invoke<Asset>("get_asset", { id });
}

/** 删除双策略（PRD R-10）：仅移出库 / 连同原文件删除
 *  B02/B03：返回 DeleteResult，含磁盘删除失败的 id（delete_file 策略下失败的不从库删） */
export function deleteAssets(ids: number[], strategy: DeleteStrategy): Promise<DeleteResult> {
  return invoke<DeleteResult>("delete_assets", { ids, strategy });
}

/** 重复素材扫描（M3-02 R-20）：hash 精确分组 */
export function scanDuplicates(): Promise<DupGroup[]> {
  return invoke<DupGroup[]>("dedup_scan");
}

/** A4-03：回收站恢复已迁到 `@/api/trash` 的 `restoreAssets`（逐项核验文件存在与归属）。 */

/** 取原文件路径（用于系统打开/复制链接等） */
export function getAssetUrls(ids: number[]): Promise<string[]> {
  return invoke<string[]>("get_asset_urls", { ids });
}

/** 在资源管理器中打开素材所在文件夹（右键菜单，B24 后端校验路径归属） */
export function revealInFolder(path: string): Promise<void> {
  return invoke<void>("reveal_in_folder", { path });
}

// ── 媒体元数据回填（指导书 §7.5）──

export interface RescanResult {
  total: number;
  success: number;
  failed: number;
  skipped: number;
}

export interface RefillProgress {
  done: number;
  total: number;
  success: number;
  failed: number;
  skipped: number;
  currentId: number;
}

/** 回填范围：all=全部视频 | missing=仅缺字段 | ids=选中素材 */
export function rescanAssetMetadata(ids: number[], scope: "all" | "missing" | "ids"): Promise<RescanResult> {
  return invoke<RescanResult>("rescan_asset_metadata", { ids, scope });
}

/** FB2-08：算法色板回算（不调用 AI）。范围：all=全部可算素材 | missing=仅缺色板 | ids=选中素材
 *  FB4-03：返回 PaletteRescanResult（含 updatedIds —— 本轮真实写库成功的素材 id）。 */
export function rescanAssetPalette(
  ids: number[],
  scope: "all" | "missing" | "ids",
): Promise<PaletteRescanResult> {
  return invoke<PaletteRescanResult>("rescan_asset_palette", { ids, scope });
}

export function cancelMediaRefill(): Promise<void> {
  return invoke<void>("cancel_media_refill");
}

// ── FB4-03：色板状态与定向同步（§6.1）──

/** 色板状态：totalAssets = 全部未删除素材；eligible = 可生成（候选）；ready = 已有有效色板；
 *  missing = 缺/损坏（可生成但未完成）；unavailable = 暂不可生成。 */
export interface PaletteStatus {
  totalAssets: number;
  eligible: number;
  ready: number;
  missing: number;
  unavailable: number;
}

/** 色板补丁：只同步色板相关字段（id + palette + dominant_*），不携带其他素材字段。 */
export interface AssetPalettePatch {
  id: number;
  palette: Asset["palette"];
  dominantHue: number | null;
  dominantSat: number | null;
  dominantLum: number | null;
}

/** 色板回算结果：在 RescanResult 之上增加 updatedIds（本轮真实写库成功的素材 id）。 */
export interface PaletteRescanResult extends RescanResult {
  updatedIds: number[];
}

/** 查询色板状态（设置页状态行 / 生成按钮状态）。 */
export function getPaletteStatus(): Promise<PaletteStatus> {
  return invoke<PaletteStatus>("get_palette_status");
}

/** 按 id 定向读取色板补丁（单次 ≤1000，前端负责分批；只返回存在的 id）。 */
export function getAssetPalettePatches(ids: number[]): Promise<AssetPalettePatch[]> {
  return invoke<AssetPalettePatch[]>("get_asset_palette_patches", { ids });
}

/** W2-8：批量收藏/取消收藏（返回受影响行数）。M-14b 恢复：曾随重构被删。 */
export function setFavorite(ids: number[], favorite: boolean): Promise<number> {
  return invoke<number>("set_favorite", { ids, favorite });
}

/** W2-8：批量设评级（rating 0 = 清除；返回受影响行数） */
export function setRating(ids: number[], rating: number): Promise<number> {
  return invoke<number>("set_rating", { ids, rating });
}

/** W5d：感知相似扫描（dHash 汉明 ≤ threshold；excludeKinship 排除同源 RAW+JPG） */
export function scanDuplicatesSimilar(threshold: number, excludeKinship: boolean): Promise<DupGroup[]> {
  return invoke<DupGroup[]>("dedup_scan_similar", { threshold, excludeKinship });
}

/** W5d：感知哈希存量回填（missing = 只补缺；all = 全部图片；ids = 指定 id） */
export function rescanAssetPhash(ids: number[] | null, scope: "all" | "missing" | "ids"): Promise<{ total: number; success: number; failed: number; skipped: number }> {
  return invoke("rescan_asset_phash", { ids, scope });
}

/** R1-2：图片宽高存量回填（RAW 分辨率修复；命令已注册但此前前端不可达） */
export function rescanImageDimensions(ids: number[] | null, scope: "all" | "missing" | "ids"): Promise<{ total: number; success: number; failed: number; skipped: number }> {
  return invoke("rescan_image_dimensions", { ids, scope });
}

/** R1-2（真库缺表回填）：从 palette_json 全量重建 asset_palette_colors（不解码图片，毫秒级，幂等）。
 *  命令已注册（media_cmd.rs rescan_palette_colors）但此前前端不可达 —— 色板关系表空时
 *  「前三色包含红」这类筛选永远 0 结果，导入素材后点一次即可补齐。返回写入的行数。 */
export function rescanPaletteColors(): Promise<number> {
  return invoke<number>("rescan_palette_colors");
}
