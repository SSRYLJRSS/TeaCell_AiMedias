/** 导出相关命令封装（对应 commands/export_cmd.rs） */
import { invoke, on } from "./client";
import type { ExportReport, ExportTask } from "@/types/export";
import type { UnlistenFn } from "@tauri-apps/api/event";

export interface ExportProgress {
  taskId: number;
  done: number;
  total: number;
}

/** R-26 子目录组织：flat 平铺 | by_tag 按首个标签 | by_date 按拍摄月份 */
export type ExportLayout = "flat" | "by_tag" | "by_date";

/**
 * 本地「复制导出」（A4-01）。
 *
 * `directoryRef` 必须是 `pickDirectoryGrant("export_destination", …)` 返回的授权句柄，
 * 后端不接受任意裸路径；库内原文件、原路径与标签一律保留。`mode` 只允许 `copy`，
 * 传 `move` 会收到明确的后端拒绝错误（库外移动已停用）。
 */
export function exportLocalFiles(
  assetIds: number[],
  directoryRef: string,
  mode: "copy" = "copy",
  layout: ExportLayout = "flat",
): Promise<ExportTask> {
  return invoke<ExportTask>("export_local_files", {
    assetIds,
    directoryRef,
    mode,
    layout,
  });
}

/** 一次导出任务的完整结果：任务状态 + 逐文件计数 + 逐文件明细 */
export function exportReport(taskId: number): Promise<ExportReport> {
  return invoke<ExportReport>("export_report", { taskId });
}

/** R-26 CSV 清单导出：返回清单文件路径（UTF-8 BOM，Excel 直开），目标同样走目录授权 */
export function exportCsvManifest(assetIds: number[], directoryRef: string): Promise<string> {
  return invoke<string>("export_csv_manifest", { assetIds, directoryRef });
}

export function onExportProgress(handler: (p: ExportProgress) => void): Promise<UnlistenFn> {
  return on<ExportProgress>("export://progress", handler);
}
