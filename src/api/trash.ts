/** 回收站与恢复（对应 commands/trash_cmd.rs，A4-03）
 *
 *  恢复走「计划 → GUI 批准 → 逐项核验」，所以返回值是逐项结果而不是一个数字：
 *  文件缺失、已被替换、已不属于当前总仓的项必须单独报出来，不能被计成成功。
 */
import { invoke } from "./client";

/** 回收站条目：含移入时间与预计自动清理时间（与后端同一公式计算）。 */
export interface TrashEntry {
  assetId: number;
  fileName: string;
  filePath: string;
  deletedAt: number;
  expiresAt: number;
  retentionDays: number;
}

/** 恢复失败码。未知码按原样回显，便于对着日志排查。 */
export type RestoreErrorCode =
  | "FILE_MISSING"
  | "FILE_CHANGED"
  | "OUTSIDE_LIBRARY"
  | "LIBRARY_CHANGED"
  | "NOT_IN_TRASH"
  | "PERMISSION_DENIED"
  | "INTERRUPTED";

export interface RestoreItem {
  itemId: string;
  assetId: number;
  fileName: string;
  /** succeeded / failed / outcome_unknown */
  state: string;
  errorCode: string | null;
  detail: string | null;
}

export interface RestoreReport {
  operationRef: string;
  restored: number;
  failed: number;
  /**
   * 效果不确定的项数：记录可能已经回到库中，也可能还留在回收站。
   * 必须提示用户核对，既不能算成功，也不能当普通失败让他原样重试。
   */
  unknown: number;
  skipped: number;
  cancelled: boolean;
  items: RestoreItem[];
}

/** 回收站清单（最近移入在前）。 */
export function listTrash(): Promise<TrashEntry[]> {
  return invoke<TrashEntry[]>("list_trash");
}

/** 指定素材的回收站信息（界面在选中项旁显示预计清理时间）。 */
export function trashRetention(ids: number[]): Promise<TrashEntry[]> {
  return invoke<TrashEntry[]>("trash_retention", { ids });
}

/** 从回收站恢复：返回逐项结果，调用方须按 `state` 区分已恢复与失败。 */
export function restoreAssets(ids: number[]): Promise<RestoreReport> {
  return invoke<RestoreReport>("restore_assets", { assetIds: ids });
}

const RESTORE_ERROR_TEXT: Record<string, string> = {
  FILE_MISSING: "文件已不在原位置",
  FILE_CHANGED: "原位置已被其他文件替换",
  OUTSIDE_LIBRARY: "不在当前总仓内",
  LIBRARY_CHANGED: "总仓或定位证据已变化",
  NOT_IN_TRASH: "已被其他操作处理",
  PERMISSION_DENIED: "没有读取权限",
  INTERRUPTED: "上次恢复被中断",
};

/** 把失败码翻成可读原因；未知码原样回显。 */
export function restoreErrorLabel(item: RestoreItem): string {
  if (!item.errorCode) return item.detail ?? "恢复失败";
  const known = RESTORE_ERROR_TEXT[item.errorCode];
  const reason = known ?? item.errorCode;
  return item.detail ? `${reason}：${item.detail}` : reason;
}

/** 只列前若干条失败项，其余折叠成计数（摘要不刷屏）。 */
export function failedRestores(report: RestoreReport, limit = 5): RestoreItem[] {
  return report.items
    .filter((item) => item.state === "failed")
    .slice(0, limit);
}

/** 效果不确定的恢复项：需要用户核对回收站与素材库，不能按失败重试。 */
export function unknownRestores(report: RestoreReport, limit = 5): RestoreItem[] {
  return report.items
    .filter((item) => item.state === "outcome_unknown")
    .slice(0, limit);
}

// ---------------------------------------------------------------------------
// A4-04：永久清空
// ---------------------------------------------------------------------------

/** 永久删除的逐项失败码。与恢复的码表不同：这里还多了「证明不了归属」。 */
export type EmptyErrorCode =
  | "FILE_MISSING"
  | "FILE_CHANGED"
  | "NOT_IN_TRASH"
  | "PERMISSION_DENIED"
  | "OWNERSHIP_UNVERIFIED"
  | "INTERRUPTED"
  | "IO_ERROR";

export interface EmptyItem {
  itemId: string;
  assetId: number;
  fileName: string;
  /** succeeded / failed / outcome_unknown */
  state: string;
  errorCode: string | null;
  detail: string | null;
}

/** 一次永久清空的结果。
 *
 *  `missing` 是「文件本来就不在」，`unknown` 是「证明不了删对了」或「文件删了
 *  但记录没同步」——两者都不能算成功，界面必须分开显示。
 */
export interface EmptyReport {
  operationRef: string;
  removed: number;
  failed: number;
  missing: number;
  unknown: number;
  skipped: number;
  cancelled: boolean;
  items: EmptyItem[];
}

/** 永久清空选中的回收站素材（连同原始文件，不可恢复）。
 *
 *  后端要求这次确认来自独立审批（`ReviewKind::PermanentTrash`），普通计划确认
 *  不能替代；调用方必须先让用户单独确认「永久清空这 N 项」。
 */
export function emptyTrash(ids: number[]): Promise<EmptyReport> {
  return invoke<EmptyReport>("empty_trash", { assetIds: ids });
}

const EMPTY_ERROR_TEXT: Record<string, string> = {
  FILE_MISSING: "文件本来就不在原位置，记录已保留",
  FILE_CHANGED: "原位置已不是原来的那个文件，未删除",
  NOT_IN_TRASH: "已被其他操作恢复或清理",
  PERMISSION_DENIED: "没有删除权限，文件仍在原位置",
  OWNERSHIP_UNVERIFIED: "定位证据已失效，未删除",
  INTERRUPTED: "上次执行被中断，需人工核对",
  IO_ERROR: "删除文件时出错",
};

/** 把永久删除的失败码翻成可读原因；未知码原样回显。 */
export function emptyErrorLabel(item: EmptyItem): string {
  if (!item.errorCode) return item.detail ?? "清空失败";
  const known = EMPTY_ERROR_TEXT[item.errorCode];
  const reason = known ?? item.errorCode;
  return item.detail ? `${reason}：${item.detail}` : reason;
}

/** 未成功的项：`outcome_unknown` 也在这里，因为用户必须看到它。 */
export function failedEmpties(report: EmptyReport, limit = 5): EmptyItem[] {
  return report.items.filter((item) => item.state !== "succeeded").slice(0, limit);
}
