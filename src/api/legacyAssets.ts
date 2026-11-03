import { invoke } from "./client";

export interface LegacyHashMatch {
  assetId: number;
  fileName: string;
  favorite: number;
  rating: number;
  contentSha256: string;
  tags: string[];
}

export interface LegacyAsset {
  assetId: number;
  filePath: string;
  fileName: string;
  fileSize: number;
  mimeType: string;
  modifiedAt: number;
  hash: string | null;
  status: string;
  reason: string;
  favorite: number;
  rating: number;
  activeResolutionId: string | null;
  activeResolutionAction: "copy_back" | "unlink" | null;
  activeResolutionState: string | null;
  resolutionErrorCode: string | null;
  matchingAssets: LegacyHashMatch[];
}

export interface LegacyAssetPage {
  libraryHealth: string;
  canResolve: boolean;
  rootPath: string | null;
  items: LegacyAsset[];
  nextAfterAssetId: number | null;
}

export interface LegacyResolutionPreview {
  resolutionId: string;
  asset: LegacyAsset;
  action: "copy_back" | "unlink";
  matchingAssets: LegacyHashMatch[];
  keepSeparateRequired: boolean;
  expiresAt: number;
}

export interface LegacyResolutionResult {
  resolutionId: string;
  assetId: number;
  action: "copy_back" | "unlink";
  state: string;
  targetPath: string | null;
  matchingAssets: LegacyHashMatch[];
  errorCode: string | null;
  expiresAt: number;
}

export function listLegacyAssets(afterAssetId = 0, limit = 50): Promise<LegacyAssetPage> {
  return invoke("list_legacy_assets", { afterAssetId, limit });
}

export function listUnlinkedLegacyAssets(afterAssetId = 0, limit = 50): Promise<LegacyAsset[]> {
  return invoke("list_unlinked_legacy_assets", { afterAssetId, limit });
}

export function prepareLegacyUnlink(assetId: number): Promise<LegacyResolutionPreview> {
  return invoke("prepare_legacy_unlink", { assetId });
}

export function prepareLegacyCopyBack(assetId: number): Promise<LegacyResolutionPreview | null> {
  return invoke("prepare_legacy_copy_back", { assetId });
}

export function cancelLegacyResolution(resolutionId: string): Promise<void> {
  return invoke("cancel_legacy_asset_resolution", { resolutionId });
}

export function confirmLegacyResolution(resolutionId: string): Promise<LegacyResolutionResult> {
  return invoke("confirm_legacy_asset_resolution", { resolutionId });
}

export function reopenUnlinkedLegacyAsset(assetId: number): Promise<void> {
  return invoke("reopen_unlinked_legacy_asset", { assetId });
}

/** 批量处置的单条结论。`skipped` 与 `failed` 必须分开显示：跳过是「没轮到」，
 * 失败是「检查没通过」，两者的下一步不同。 */
export interface LegacyBatchEntry {
  assetId: number;
  /** prepared / completed / unlinked / outcome_unknown / failed / skipped */
  state: string;
  resolutionId: string | null;
  errorCode: string | null;
  message: string | null;
}

export interface LegacyBatchReport {
  action: string;
  prepared: number;
  failed: number;
  skipped: number;
  /** 用户在最前面的原生选择框里取消：什么都没做，不该报失败。 */
  cancelled: boolean;
  entries: LegacyBatchEntry[];
}

export function prepareLegacyUnlinkBatch(assetIds: number[]): Promise<LegacyBatchReport> {
  return invoke("prepare_legacy_unlink_batch", { assetIds });
}

/** 一次原生多选后逐条复核；未选中的记录返回 `skipped`。 */
export function prepareLegacyCopyBackBatch(assetIds: number[]): Promise<LegacyBatchReport> {
  return invoke("prepare_legacy_copy_back_batch", { assetIds });
}

export function confirmLegacyResolutionsBatch(
  resolutions: string[],
): Promise<LegacyBatchReport> {
  return invoke("confirm_legacy_resolutions_batch", { resolutions });
}

/** 批量报告里需要用户处理的条目（失败与结果不明两类）。 */
export function batchEntriesNeedingAttention(report: LegacyBatchReport): LegacyBatchEntry[] {
  return report.entries.filter(
    (entry) => entry.state === "failed" || entry.state === "outcome_unknown",
  );
}

/** 批量报告的一句话结论。分开报数量，不把跳过说成完成。 */
export function batchHeadline(report: LegacyBatchReport): string {
  if (report.cancelled) return "已取消，没有处置任何记录。";
  const verb = report.action === "unlink" ? "解除关联" : "复制回仓";
  const parts = [`已准备 ${report.prepared} 条${verb}`];
  if (report.failed > 0) parts.push(`${report.failed} 条未通过检查`);
  if (report.skipped > 0) parts.push(`${report.skipped} 条未轮到处置`);
  return `${parts.join("，")}。`;
}

/** 收据状态码 → 中文。未知码原样回显，不猜。 */
export const LEGACY_OUTCOME_TEXT: Record<string, string> = {
  prepared: "等待确认",
  copying: "复制中断，可继续核验",
  published: "文件已发布，等待登记",
  completed: "已复制回仓",
  unlinked: "已解除关联",
  outcome_unknown: "结果待人工核对",
  failed: "未通过检查",
  skipped: "未轮到处置",
};

export function legacyOutcomeLabel(state: string): string {
  return LEGACY_OUTCOME_TEXT[state] ?? state;
}
