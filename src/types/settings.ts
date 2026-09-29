/** 设置（网盘账号已在指导书 §6.8 移除，云账号类型不再暴露） */

export type ApiMode = "openai" | "anthropic";

/** 部署类型（P3-01a）：cloud 云端服务商 | local 本机 OpenAI 兼容服务（Ollama/LM Studio） */
export type ProfileKind = "cloud" | "local";

/** 一套 API 配置档案（一个中转站/服务商） */
export interface ApiProfile {
  id: string;
  name: string;
  apiMode: ApiMode;
  /** 部署类型；旧数据缺省视为 cloud */
  kind?: ProfileKind;
  baseUrl: string;
  apiKey: string;
  model: string;
}

export interface AiSettings {
  /** 多套 API 配置（中转站），打标只走激活那套 */
  profiles: ApiProfile[];
  /** 当前激活档案 id */
  activeProfile: string;
  /** W0-6：autoTagging/localModelTier 已删（后端零消费，skip_serializing 只读兼容） */
  videoTagging: boolean;
  /** FB2-07：视频打标模式（决策 5：默认首帧/封面） */
  videoTaggingMode: "cover" | "frames";
  /** FB2-07：frames 模式抽帧数（2~8） */
  videoFrameCount: number;
  /** 在线服务每轮处理数量（10~50） */
  batchLimit: number;
  /** 本机服务每轮处理数量（1~20） */
  localBatchLimit?: number;
  /** @deprecated 旧配置读取兼容字段；normalizeSettings 会丢弃，应用不再保存或使用。 */
  systemPromptTagging?: string;
  /** @deprecated 旧配置读取兼容字段；normalizeSettings 会丢弃，应用不再保存或使用。 */
  systemPromptSearch?: string;
  /** 一键安装的下载源偏好（"auto" = 测速选最快；旧数据缺省视为 auto） */
  ollamaSourceId: string;
  /** AI 建议最低置信度阈值：confidence < 此值不入库（连 pending 都不进）；默认 0.30 */
  confidenceMinSuggest: number;
}

/** 标签分类（PRD 5.5，已弃用）：旧 name/机器协议，仅作迁移输入 */
export interface TagCategory {
  name: string;
  hint: string;
  /** 单选（最多 1 个标签） */
  single: boolean;
  /** 每类标签数量上限（多选时生效） */
  max: number;
}

/** 用户自定义下载源（即时落库；改造方案） */
export interface CustomSource {
  id: string;
  label: string;
  url: string;
}

export interface Settings {
  ai: AiSettings;
  theme: "system" | "light" | "dark";
  /** 文件日志级别；默认 info，排障时可临时切到 debug/trace */
  logLevel: "info" | "debug" | "trace";
  thumbnailCacheMb: number;
  /** 标签分类（已弃用）：仅作迁移输入，后端不再序列化 */
  tagCategories: TagCategory[];
  /** 总库位置（R-32）；空 = 原位索引模式 */
  libraryRoot: string;
  /** 回收站保留天数（R-22）；0 = 不自动清理 */
  trashRetentionDays: number;
  /** Ollama 一键下载的自定义源（即时落库；旧数据缺省空） */
  customDownloadSources: CustomSource[];
  /** Ollama 模型下载代理（拉起 serve 时注入 HTTPS_PROXY；空 = 不用代理） */
  modelDownloadProxy: string;
  /** 教程邀请已处理；旧设置缺少字段时由归一层按未处理处理。 */
  tutorialPromptHandled?: boolean;
  /** FB2-01/02/03/08：外观与交互设置（素材网格档位/比例、悬停预览、色条） */
  appearance: Appearance;
}

/** FB2-01 格子尺寸档位表（约 1.25× 等比）。索引存入设置，不存像素值——
 *  这样以后调整档位表不会让老配置落到非法像素值上。 */
export const CELL_STEPS = [96, 120, 150, 190, 240, 300, 380, 480] as const;

export type CellAspect = "1:1" | "4:3" | "3:2" | "16:9" | "3:4" | "2:3" | "9:16";
export type CellFit = "cover" | "contain" | "smart";
export type StripHeight = "thin" | "normal" | "thick";
export type StripMode = "ratio" | "equal";

export interface GridAppearance {
  /** FB2-01 素材库格子档位（CELL_SIZES 下标），默认 3（=190px） */
  libraryCellStep: number;
  /** FB2-01 入库网格格子档位，默认 1（=120px） */
  importCellStep: number;
  /** FB2-02 统一容器比例（决策 4：一个设置管两页），默认 "4:3" */
  cellAspect: CellAspect;
  /** FB2-02 填充方式（不提供拉伸/形变），默认 "cover" */
  cellFit: CellFit;
  /** FB2-02 contain 留边是否填该素材主色（依赖 FB2-08 色板），默认 false */
  matchDominantColor: boolean;
}

export interface HoverPreviewAppearance {
  /** FB2-03 悬停自动播放总开关，默认 true */
  enabled: boolean;
  /** FB2-03 预览时长（秒），2~10，默认 3 */
  previewSeconds: number;
  /** FB2-03 是否在素材库网格也启用（入库页由 enabled 单独控制），默认 true */
  inLibraryGrid: boolean;
}

export interface ColorStripAppearance {
  enabled: boolean; // 默认 true
  showInLibraryGrid: boolean; // 默认 false
  showInViewer: boolean; // 默认 true
  showInImportGrid: boolean; // 默认 false
  height: StripHeight; // 默认 "normal"
  mode: StripMode; // 默认 "ratio"
  count: 4 | 6 | 8; // 默认 6
}

/** W5h：同源文件组（RAW+JPG）设置。syncTagsToSiblings 默认开（打标算同一张照片）；
 *  mergeInLibrary 默认关（素材库独立显示，用户定案 2026-08-31）。 */
export interface KinshipAppearance {
  syncTagsToSiblings: boolean;
  mergeInLibrary: boolean;
}

export interface Appearance {
  grid: GridAppearance;
  hoverPreview: HoverPreviewAppearance;
  colorStrip: ColorStripAppearance;
  kinship: KinshipAppearance;
}
