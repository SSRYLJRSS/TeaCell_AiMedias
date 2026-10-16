/** AI 打标（批次管理 + 确认流） */
import type { PlanPreview } from "@/types/plans";

/** 批次执行类型；auto 仅用于旧 IPC 兼容，实际云端/本地由 tagging 用途绑定解析。 */
export type AiMode = "cloud" | "local" | "manual" | "auto";

export interface AiBatch {
  id: number;
  /** interrupted：应用重启/中断时遗留的 processing 批次（可一键续跑，阶段 5 §8.2）
   *  undone：批次撤销已完成（D-4，不再显示可点击撤销） */
  status: "pending" | "processing" | "done" | "cancelled" | "interrupted" | "undone";
  mode: AiMode;
  total: number;
  processed: number;
  confirmed: number;
  createdAt: number;
}

/** 分类标签：{ 分类名: [标签...] }（PRD 5.5） */
export type CategorizedTags = Record<string, string[]>;

export interface AiSuggestion {
  id: number;
  batchId: number;
  assetId: number;
  assetPath: string;
  /** B-3/B-4：素材 MIME（前端判断批次是否含视频是否需要提示开启视频打标） */
  mimeType: string | null;
  suggestedTags: CategorizedTags;
  status: "pending" | "confirmed" | "rejected" | "modified";
  confirmedTags: CategorizedTags;
  /** 单条打标失败原因（v6：失败详情落库，前端展示） */
  lastError: string | null;
  createdAt: number;
  // FB5-05（§7.6）：一句话描述。AI 建议值 / 审核后确认值 / 素材当前值。
  suggestedDescription?: string;
  confirmedDescription?: string | null;
  currentDescription?: string;
}

export interface AiSuggestionItem {
  id: number;
  suggestionId: number;
  assetId: number;
  facetKey: string;
  rawName: string;
  normalizedName: string;
  tagId: number | null;
  confidence: number | null;
  decision: "pending" | "accepted" | "modified" | "rejected";
  decisionReason: string | null;
  createdAt: number;
  /** V24（Phase 7-4）：tag | number —— 数值建议项 */
  itemKind?: "tag" | "number";
  /** itemKind='number' 时为确认值；歧义项为 null（需人工填数） */
  numValue?: number | null;
}

/** FB6 需求一：AI 打标页内进度 UI 状态（由 aiStore 数据派生；进度事件只经页面内唯一
 *  onAiProgress 订阅写入 aiStore，本类型不再触发第二个事件监听）。 */
export type AiTaggingUiState =
  | { phase: "idle" }
  | { phase: "starting"; total: number }
  | { phase: "running"; processed: number; total: number; currentAssetId?: number }
  | { phase: "cancelling"; processed: number; total: number }
  | { phase: "done"; processed: number; total: number }
  | { phase: "error"; message: string; processed: number; total: number };

export interface ModelStatus {
  tier: "light" | "standard";
  state: "not_installed" | "downloading" | "ready" | "error";
  /** 0..1（downloading 时） */
  progress: number;
  sizeMb: number;
  localPath: string | null;
}

/** Rust serde 响应 DTO：可信计划提交后待启动的打标操作。 */
export interface TaggingOperation {
  operationId: string;
  stepId: string;
  batchId: number;
  requestLimit: number;
  requestsAttempted: number;
}

/** Rust serde 响应 DTO：打标计划预览及可选的已关联操作。 */
export interface TaggingPreparation {
  requiresApproval: boolean;
  summary: string;
  preview: PlanPreview | null;
  operation: TaggingOperation | null;
}
