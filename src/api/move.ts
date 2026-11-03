/**
 * 库内分库移动（A4-02，对应 commands/move_cmd.rs）。
 *
 * 与导出不同，这里不涉及目录授权：分库是**当前总仓内的相对目录**，引用由总仓
 * `library_epoch` 派生——总仓重新核验后旧引用自然失效，而不是解析到别的目录。
 * 移动只改文件位置与路径记录：素材 ID、标签、评分与历史全部保留。
 */
import { invoke } from "./client";

/** 一个可选的分库目标。`relativePath` 为空串表示总仓根目录。 */
export interface MoveTarget {
  collectionRef: string;
  relativePath: string;
  label: string;
}

/** 逐文件结果状态（move_journal 五态）。 */
export type MoveItemState = "intent" | "moved" | "committed" | "failed" | "rolled_back";

export type MoveItemErrorCode =
  | "TARGET_EXISTS"
  | "TARGET_OCCUPIED"
  | "SOURCE_MISSING"
  | "SOURCE_CHANGED"
  | "LIBRARY_CHANGED"
  | "PERMISSION_DENIED"
  | "CANCELLED"
  | "IO_ERROR";

export interface MoveItem {
  itemId: string;
  assetId: number;
  fileName: string;
  state: MoveItemState;
  errorCode: MoveItemErrorCode | null;
  sourcePath: string;
  targetPath: string;
}

/** 一次移动的完整结果：目标分库、计数与逐文件明细。 */
export interface MoveReport {
  operationRef: string;
  collectionPath: string;
  moved: number;
  failed: number;
  /** 效果可能已发生但证据不完整：必须提示核对，不能当普通失败处理。 */
  unknown: number;
  skipped: number;
  cancelled: boolean;
  items: MoveItem[];
}

/** 总仓内可分库目标（最多 200 个，跳过保留目录与链接）。 */
export function listMoveTargets(): Promise<MoveTarget[]> {
  return invoke<MoveTarget[]>("list_move_targets");
}

/**
 * 把选中素材移动到指定分库。
 *
 * 后端会重新核验源文件仍在原位置、目标仍在当前总仓内，并为每个文件写移动日志；
 * 单个文件失败只影响该项，不会让整批静默成功。
 */
export function moveAssets(assetIds: number[], collectionRef: string): Promise<MoveReport> {
  return invoke<MoveReport>("move_assets", { assetIds, collectionRef });
}
