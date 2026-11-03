import { invoke } from "@/api/client";
import type { Asset } from "@/types/asset";

/**
 * A3-08：把「当前选择」做成不可变快照。
 *
 * `total` 是当时筛选条件命中的总数，`assetIds` 是实际拿到的 id（可能更少）。
 * 差异由 `truncated` 报告，调用方必须原样展示，不能用数量推断范围。
 */
export interface SelectionView {
  selectionRef: string;
  total: number;
  truncated: boolean;
}

/** 快照计数：不加载素材行就能知道还剩多少可用（交接卡片与结果视图头部共用）。 */
export interface SelectionSummary {
  selectionRef: string;
  total: number;
  truncated: boolean;
  snapshot: number;
  available: number;
  missing: number;
  /** 交接时记录的来源描述，由后端快照行给出；界面不得自己编。 */
  query: string;
}

/** 快照的一页。`available`/`missing` 始终描述整个快照，不随翻页变化。 */
export interface SelectionPage extends SelectionSummary {
  items: Asset[];
  nextOffset?: number;
}

/** 交接前先固定集合；之后素材库里的选择变化不会改写它。 */
export function snapshotSelection(
  query: string,
  assetIds: number[],
  total: number,
): Promise<SelectionView> {
  return invoke<SelectionView>("snapshot_selection", {
    query,
    assetIds,
    total,
  });
}

/** 只读计数，用于在渲染网格前说明「有多少已经不可用」。 */
export function summarySelection(selectionRef: string): Promise<SelectionSummary> {
  return invoke<SelectionSummary>("summary_selection", { selectionRef });
}

/** 读快照的一页（素材库「本次结果视图」与交接卡片共用）。 */
export function pageSelection(
  selectionRef: string,
  offset = 0,
  limit = 200,
): Promise<SelectionPage> {
  return invoke<SelectionPage>("page_selection", {
    selectionRef,
    offset,
    limit,
  });
}
