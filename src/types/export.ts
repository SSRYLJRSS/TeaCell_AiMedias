/** 导出任务（指导书 §6.8：网盘已从正常 UI 移除，仅保留本地文件导出目标）。 */

export type ExportTarget = "local";

export interface ExportTask {
  id: number;
  target: ExportTarget;
  status: "pending" | "running" | "done" | "failed" | "cancelled";
  total: number;
  done: number;
  destDir: string | null;
  shareUrl: string | null;
  error: string | null;
  createdAt: number;
  /** A4-01：部分成功时的软提示（「N 个文件复制失败，其余 M 个已完成」） */
  warning: string | null;
}

/** 逐文件结果状态（A4-01）：copied 已落盘并核验 / failed 复制失败 / skipped 取消未开始 */
export type ExportItemState = "copied" | "failed" | "skipped";

/**
 * 逐文件失败归因。`target_exists` 只在同名候选耗尽（超过 999 个同名）时出现；
 * 常规重名会自动追加 (1)(2)…，绝不覆盖目标目录里已有的文件。
 *
 * `PATH_REJECTED`（A4-01）：授权的目标目录在复制过程中被换成链接/重解析点，
 * 或派生出的子目录名越出授权范围——后端逐项复验后拒绝写入这一项。
 */
export type ExportItemErrorCode =
  | "TARGET_EXISTS"
  | "PERMISSION_DENIED"
  | "DISK_FULL"
  | "SOURCE_MISSING"
  | "CANCELLED"
  | "PATH_REJECTED"
  | "IO_ERROR";

export interface ExportItem {
  id: number;
  taskId: number;
  ordinal: number;
  assetId: number | null;
  fileName: string;
  destPath: string | null;
  state: ExportItemState;
  errorCode: ExportItemErrorCode | null;
  errorMessage: string | null;
  createdAt: number;
}

export interface ExportCounts {
  copied: number;
  failed: number;
  skipped: number;
}

/** 一次导出任务的完整结果；完成摘要要同时给出目标位置与失败项。 */
export interface ExportReport {
  task: ExportTask;
  counts: ExportCounts;
  items: ExportItem[];
}
