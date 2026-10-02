/** 素材类型（与 Rust db/assets.rs 对齐，camelCase 序列化） */

export interface Asset {
  id: number;
  filePath: string;
  fileName: string;
  fileExt: string;
  fileSize: number;
  mimeType: string;
  width: number | null;
  height: number | null;
  durationMs: number | null;
  videoCodec: string | null;
  audioCodec: string | null;
  takenAt: number | null;
  createdAt: number;
  modifiedAt: number;
  hash: string | null;
  placeholderPath: string | null;
  hdThumbnailPath: string | null;
  // EXIF 元信息（PRD 5.5，入库自动提取）
  camera: string | null;
  lens: string | null;
  iso: number | null;
  aperture: number | null;
  shutter: string | null;
  focal: number | null;
  // 媒体元数据（指导书 §7.3/§7.4，后端探测为事实源；V12 迁移新增列）
  mediaKind?: string | null;
  containerFormat?: string | null;
  videoProfile?: string | null;
  pixelFormat?: string | null;
  bitDepth?: number | null;
  frameRate?: number | null;
  videoBitRate?: number | null;
  colorRange?: string | null;
  colorSpace?: string | null;
  colorTransfer?: string | null;
  colorPrimaries?: string | null;
  audioSampleRate?: number | null;
  audioChannels?: number | null;
  audioLayout?: string | null;
  rotation?: number | null;
  mediaMetadataJson?: string | null;
  metadataVersion?: number | null;
  metadataScannedAt?: number | null;
  metadataError?: string | null;
  // FB2-08：算法主色（§14.8）。palette 为色条分段；dominant_* 供检索索引。
  palette?: PaletteSegmentDto[] | null;
  dominantHue?: number | null;
  dominantSat?: number | null;
  dominantLum?: number | null;
  // FB5-05（§7.3）：一句话描述（不限字数，素材字段，不进标签树/统计）
  contentDescription?: string;
  // V18：GPS 定位（有符号十进制度，北纬东经为正；无定位为 null）
  latitude?: number | null;
  longitude?: number | null;
  // V19（W3-1）：评级/手动旋转/phash（可选 + 默认值兼容旧响应）
  /** 评级 0–5（0 = 未评级） */
  rating?: number;
  /** 用户手动旋转（0/90/180/270；与 ffprobe 的 rotation 语义分离） */
  userRotation?: number;
  /** 感知哈希 dHash 64 位（W5d 相似去重；未计算为 null） */
  phash?: number | null;
  tags: import("./tag").Tag[];
}

/** FB2-08：色板单段（占比例降序）。hex 用于渲染色带；hue/sat/lum 用于中文色名。 */
export interface PaletteSegmentDto {
  hex: string;
  r: number;
  g: number;
  b: number;
  ratio: number;
}

export type AssetType = "all" | "image" | "video";

export interface AssetFilter {
  assetType?: AssetType;
  untaggedOnly?: boolean;
  tagId?: number;
  /** 多标签筛选（R-21，与 tagId 二选一） */
  tagIds?: number[];
  /** any（默认）| all（同时含全部标签） */
  tagsMode?: "any" | "all";
  facetFilters?: FacetTagFilter[];
  excludeTagIds?: number[];
  metadataFilters?: MetadataFilter[];
  search?: string;
  /** 排序字段（R-21）：createdAt（默认）| takenAt | modifiedAt | name | size | resolution */
  sortBy?: "created_at" | "taken_at" | "modified_at" | "name" | "size" | "resolution" | "rating";
  /** desc（默认）| asc */
  sortDir?: "desc" | "asc";
  /** true = 查回收站（R-22） */
  trashOnly?: boolean;
  /** 布尔表达式树（P4 queryExpr）：存在时后端优先走表达式编译 */
  expr?: import("./queryExpr").QueryExpr;
  offset?: number;
  limit?: number;
}

export interface FacetTagFilter {
  facetKey: string;
  tagIds: number[];
  mode: "any" | "all";
  includeDescendants: boolean;
}

export type MetadataFilterKey =
  | "folder"
  | "taken_month"
  | "camera"
  | "lens"
  | "iso"
  | "aperture"
  | "shutter"
  | "focal"
  | "file_ext"
  | "mime_type"
  | "width"
  | "height"
  | "resolution"
  | "aspect_ratio"
  | "file_size"
  | "duration_ms"
  | "taken_at"
  | "created_at"
  | "modified_at"
  | "video_codec"
  | "audio_codec"
  // FB2-08：算法主色（dominant_color）可检索维度
  | "dominant_hue"
  | "dominant_sat"
  | "dominant_lum"
  // C-1/U-3：色板关系表（值 = 折叠色名 eq/in；eq + min = 占比阈值）。UI 只暴露 palette_top3。
  | "palette_dominant"
  | "palette_top3"
  | "palette_any"
  // V18：GPS 定位（带符号十进制度）与定位有无分面（值域 yes/no）
  | "latitude"
  | "rating"
  | "longitude"
  | "has_location";

export type MetadataOp =
  | "eq"
  | "in"
  | "contains"
  | "gt"
  | "gte"
  | "lt"
  | "lte"
  | "between";

export type MetadataValue = string | number;

export interface MetadataFilter {
  key: MetadataFilterKey;
  op: MetadataOp;
  /** 单值操作符（eq / contains / gt / gte / lt / lte）使用 */
  value?: MetadataValue;
  /** in 操作符使用 */
  values?: MetadataValue[];
  /** between 操作符使用 */
  min?: MetadataValue;
  max?: MetadataValue;
  /** Phase 4（§4-3）：file_size 的展示单位（KB/MB/GB）—— 提升进条件，between 两侧共用且换源不丢。
   *  后端只消费字节数值，本字段仅在协议里透传，Rust 侧 serde 忽略未知字段。 */
  unit?: "KB" | "MB" | "GB";
}

export interface MetadataFacetItem {
  value: string;
  label: string;
  count: number;
}

/** Phase 4（§5.3）：数值字段的 NumericDomain 单一事实源（get_numeric_domains 命令下发）。
 *  unit = 控件类型标签（raw|bytes|millis|pixels|degrees|percent|stars|custom）；
 *  unitLabel = 单位展示文本（f/、mm、B、px、°、%…，null = 无单位）；
 *  presets = (label, value) 快捷项（16:9、ISO 800、f/2.8…）。 */
export interface NumericDomain {
  key: MetadataFilterKey | `facet:${string}`;
  /** V24：数值分面的显示名（人数）；内置 key 由前端字段表提供，此列缺省 */
  label?: string;
  unit: "raw" | "bytes" | "millis" | "pixels" | "degrees" | "percent" | "stars" | "custom";
  unitLabel?: string | null;
  min?: number | null;
  max?: number | null;
  step: number;
  decimals: number;
  presets: [string, number][];
  /** dominant_hue：允许 min > max（跨 0° 区间） */
  circular: boolean;
  /** R2-2 量纲提示阈值（低于该值提示可能写错单位） */
  suspiciousBelow?: number | null;
  allowedOps: MetadataOp[];
}

export interface MetadataFacet {
  /** 分面展示键（可为 folder/duration/taken_month 等，未必是筛选 key） */
  key: string;
  displayName: string;
  description: string;
  items: MetadataFacetItem[];
}

export interface AssetPage {
  items: Asset[];
  total: number;
  hasMore: boolean;
  /** R2-1：后端编译层剔除/降级 warning（String）；plan 路径用 SearchWarning[]（见 PlanAssetPage） */
  warnings?: string[];
}

/** 分面标签条件（ResolvedSearchQuery 内，tagIds 为后端解析结果） */
export interface ResolvedFacetFilter {
  facetKey: string;
  tagIds: number[];
  /** any（同分面内 OR，默认）| all */
  mode: "any" | "all";
  includeDescendants: boolean;
}

/** 执行对象：后端/前端统一查询协议（P0 contract-v1 §3）。
 *  FB5-05（§9.5）：移除 missingFacetKeys——无法映射的概念已由 content 搜索 leaf/warning 承接；
 *  AI 结果以 QueryExpr 为唯一事实源，本扁平对象仅供手动条件链路使用。 */
export interface ResolvedSearchQuery {
  search: string;
  assetType: AssetType;
  untaggedOnly: boolean;
  facetFilters: ResolvedFacetFilter[];
  excludeTagIds: number[];
  metadataFilters: MetadataFilter[];
  sortBy: "created_at" | "taken_at" | "modified_at" | "name" | "size" | "resolution" | "rating";
  sortDir: "desc" | "asc";
}

export interface ImportResult {
  imported: number;
  failed: number;
  duplicates: number;
  errors: string[];
  /** 扫描阶段无法读取的路径等非致命警告，不改变成功/重复/失败计数。 */
  warnings: string[];
}

/** 重复素材分组（M3-02）：assets 按 created_at 升序，首项最早（保留候选） */
/** 重复/相似分组类型：exact = 字节级相同（sha256）；similar = 感知相似（dHash） */
export type DupGroupKind = "exact" | "similar";

export interface DupGroup {
  hash: string;
  /** W5d：精确重复 or 感知相似（前端据此切换文案） */
  kind: DupGroupKind;
  assets: Asset[];
}

/** 打标操作流水（R-25，对应 db/tag_ops.rs TagOp） */
export interface TagOp {
  id: number;
  assetId: number;
  tagId: number;
  op: "add" | "remove";
  actor: "manual" | "ai_cloud" | "ai_local";
  batchId: number | null;
  createdAt: number;
  tagName: string;
  assetName: string;
}
