/**
 * 设置运行时归一化（指导书 A-2）：后端设置 JSON 可能缺失字段（旧库 / 部分返回 / 未来字段增减），
 * 前端不能直接访问 draft.ai.xxx 而崩溃白屏。
 * 本函数用类型守卫对 `unknown` 输入提供安全默认值，产生一份可安全渲染的 Settings。
 *
 * 注意：本函数只负责「前端运行时安全」，不取代 Rust 端迁移 —— 迁移仍是单一事实源。
 * 对未知字段尽量保留，不无故丢弃未来配置。
 */
import type {
  AiSettings,
  ApiProfile,
  Appearance,
  CellAspect,
  ColorStripAppearance,
  CustomSource,
  GridAppearance,
  HoverPreviewAppearance,
  Settings,
  TagCategory,
} from "@/types/settings";
import { CELL_STEPS } from "@/types/settings";

export const DEFAULT_MODEL = "qwen-vl-plus";
/** FB3-07：云端每批处理数量默认 30（与 Rust default_batch_limit 一致；运行时范围 [10,50]） */
export const DEFAULT_BATCH_LIMIT = 30;
/** FB3-07：云端每批处理数量运行时范围（与 ai_cloud.rs 执行层 clamp 一致） */
export const BATCH_LIMIT_MIN = 10;
export const BATCH_LIMIT_MAX = 50;
/** 本机模型默认采用更小的处理轮次，降低小模型长时间连续执行的压力。 */
export const DEFAULT_LOCAL_BATCH_LIMIT = 5;
export const LOCAL_BATCH_LIMIT_MIN = 1;
export const LOCAL_BATCH_LIMIT_MAX = 20;
export const DEFAULT_CACHE_MB = 2048;
export const DEFAULT_TRASH_RETENTION_DAYS = 30;
// ── A4 置信度策略默认（与 Rust db/settings.rs 的 default_* 逐字对应）──
export const DEFAULT_CONF_MIN_SUGGEST = 0.3;

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function asStr(v: unknown, fallback: string): string {
  return typeof v === "string" ? v : fallback;
}

function asNum(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function asBool(v: unknown, fallback: boolean): boolean {
  return typeof v === "boolean" ? v : fallback;
}

function normalizeProfile(p: unknown): ApiProfile {
  const r = isRecord(p) ? p : {};
  const apiMode = asStr(r.apiMode, "openai");
  const kind = asStr(r.kind, "cloud");
  return {
    id: asStr(r.id, ""),
    name: asStr(r.name, ""),
    apiMode: apiMode === "anthropic" ? "anthropic" : "openai",
    kind: kind === "local" ? "local" : "cloud",
    baseUrl: asStr(r.baseUrl, ""),
    apiKey: asStr(r.apiKey, ""),
    model: asStr(r.model, DEFAULT_MODEL),
  };
}

function normalizeAi(raw: unknown): AiSettings {
  const r = isRecord(raw) ? raw : {};
  const profiles: ApiProfile[] = Array.isArray(r.profiles)
    ? r.profiles.map(normalizeProfile).filter((p) => p.id !== "")
    : [];
  return {
    profiles,
    activeProfile: asStr(r.activeProfile, ""),
    // W0-6：autoTagging/localModelTier 已删（后端零消费；老 JSON 里的值直接忽略）
    videoTagging: asBool(r.videoTagging, false),
    // FB3-07：批大小收敛到 [10,50]（云端执行层实际范围）；越界值（含历史 500/0）归一到默认 30，
    // 与 Rust 端 AiSettings::normalize 的行为一致（不做区间钳制——0 不应变成 10 这种「看似有效」的值）
    batchLimit: (() => {
      const n = Math.round(asNum(r.batchLimit, DEFAULT_BATCH_LIMIT));
      return n >= BATCH_LIMIT_MIN && n <= BATCH_LIMIT_MAX ? n : DEFAULT_BATCH_LIMIT;
    })(),
    localBatchLimit: (() => {
      const n = Math.round(asNum(r.localBatchLimit, DEFAULT_LOCAL_BATCH_LIMIT));
      return n >= LOCAL_BATCH_LIMIT_MIN && n <= LOCAL_BATCH_LIMIT_MAX ? n : DEFAULT_LOCAL_BATCH_LIMIT;
    })(),
    ollamaSourceId: asStr(r.ollamaSourceId, "auto"),
    videoTaggingMode: asEnum(r.videoTaggingMode, ["cover", "frames"] as const, "cover"),
    videoFrameCount: clampInt(r.videoFrameCount, 2, 8, 3),
    // 所有 AI 标签都进入人工确认；这里仅保留入库前的最低置信度阈值。
    confidenceMinSuggest: (() => {
      const n = asNum(r.confidenceMinSuggest, DEFAULT_CONF_MIN_SUGGEST);
      return Number.isFinite(n) && n >= 0 && n <= 1 ? n : DEFAULT_CONF_MIN_SUGGEST;
    })(),
  };
}

function normalizeCategory(c: unknown): TagCategory {
  const r = isRecord(c) ? c : {};
  const single = asBool(r.single, false);
  return {
    name: asStr(r.name, ""),
    hint: asStr(r.hint, ""),
    single,
    max: Math.max(1, Math.round(asNum(r.max, single ? 1 : 3))),
  };
}


function normalizeCustomSource(c: unknown): CustomSource {
  const r = isRecord(c) ? c : {};
  return { id: asStr(r.id, ""), label: asStr(r.label, ""), url: asStr(r.url, "") };
}

/** FB2-08/§8.3：枚举守卫——值必须在白名单内，否则回落 fallback */
function asEnum<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === "string" && (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
}

/** FB2-01/§8.3：整数钳制守卫 */
function clampInt(v: unknown, min: number, max: number, fallback: number): number {
  const n = Math.round(asNum(v, fallback));
  return Math.max(min, Math.min(max, n));
}

const CELL_ASPECTS: readonly CellAspect[] = ["1:1", "4:3", "3:2", "16:9", "3:4", "2:3", "9:16"];

function normalizeGrid(raw: unknown): GridAppearance {
  const r = isRecord(raw) ? raw : {};
  return {
    libraryCellStep: clampInt(r.libraryCellStep, 0, CELL_STEPS.length - 1, 3),
    importCellStep: clampInt(r.importCellStep, 0, CELL_STEPS.length - 1, 1),
    cellAspect: asEnum(r.cellAspect, CELL_ASPECTS, "4:3"),
    cellFit: asEnum(r.cellFit, ["cover", "contain", "smart"] as const, "cover"),
    matchDominantColor: asBool(r.matchDominantColor, false),
  };
}

function normalizeHoverPreview(raw: unknown): HoverPreviewAppearance {
  const r = isRecord(raw) ? raw : {};
  return {
    enabled: asBool(r.enabled, true),
    previewSeconds: clampInt(r.previewSeconds, 2, 10, 3),
    inLibraryGrid: asBool(r.inLibraryGrid, true),
  };
}

function normalizeColorStrip(raw: unknown): ColorStripAppearance {
  const r = isRecord(raw) ? raw : {};
  const height = asEnum(r.height, ["thin", "normal", "thick"] as const, "normal");
  const mode = asEnum(r.mode, ["ratio", "equal"] as const, "ratio");
  // FB2-08：count 只允许 {4,6,8}；非法值（含越界 99）回落默认 6，不做区间钳制
  const rc = r.count;
  const count = rc === 4 || rc === 8 ? rc : 6;
  return {
    enabled: asBool(r.enabled, true),
    showInLibraryGrid: asBool(r.showInLibraryGrid, false),
    showInViewer: asBool(r.showInViewer, true),
    showInImportGrid: asBool(r.showInImportGrid, false),
    height,
    mode,
    count,
  };
}

function normalizeAppearance(raw: unknown): Appearance {
  const r = isRecord(raw) ? raw : {};
  const k = isRecord(r.kinship) ? r.kinship : {};
  return {
    grid: normalizeGrid(r.grid),
    hoverPreview: normalizeHoverPreview(r.hoverPreview),
    colorStrip: normalizeColorStrip(r.colorStrip),
    // W5h：同源文件组（缺省 = 后端默认：同步开、合并显示关）
    kinship: {
      syncTagsToSiblings: asBool(k.syncTagsToSiblings, true),
      mergeInLibrary: asBool(k.mergeInLibrary, false),
    },
  };
}

/** 对一份 `unknown` 设置做运行时归一化，返回可安全渲染的完整 Settings。 */
export function normalizeSettings(raw: unknown): Settings {
  const r = isRecord(raw) ? raw : {};
  const theme = asStr(r.theme, "system");
  const ai = normalizeAi(r.ai);
  // 若用户显式设置了激活档案但不匹配任何配置，回退为空（SettingsPage 会回退到第一套）
  if (ai.activeProfile && ai.profiles.length > 0 && !ai.profiles.some((p) => p.id === ai.activeProfile)) {
    ai.activeProfile = "";
  }
  // F8：aiFacetConfigs 已删（V20 合表 + 类型层清理），后端不再返回此字段
  return {
    ai,
    theme: theme === "light" || theme === "dark" ? theme : "system",
    logLevel: asEnum(r.logLevel, ["info", "debug", "trace"] as const, "info"),
    thumbnailCacheMb: Math.max(0, Math.round(asNum(r.thumbnailCacheMb, DEFAULT_CACHE_MB))),
    tagCategories: Array.isArray(r.tagCategories) ? r.tagCategories.map(normalizeCategory) : [],
    libraryRoot: asStr(r.libraryRoot, ""),
    trashRetentionDays: Math.max(0, Math.round(asNum(r.trashRetentionDays, DEFAULT_TRASH_RETENTION_DAYS))),
    customDownloadSources: Array.isArray(r.customDownloadSources)
      ? r.customDownloadSources.map(normalizeCustomSource).filter((s) => s.id !== "")
      : [],
    modelDownloadProxy: asStr(r.modelDownloadProxy, ""),
    tutorialPromptHandled: asBool(r.tutorialPromptHandled, false),
    appearance: normalizeAppearance(r.appearance),
  };
}
