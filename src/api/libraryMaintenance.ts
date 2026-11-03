/**
 * 总仓维护（A4-05，对应 commands/library_maintenance_cmd.rs）。
 *
 * 两条**互不替代**的命令：
 *
 * - `migrate`：软件把整个总仓复制到新位置并切换。源目录不在核验前删除，也不由
 *   本应用自动删除——残留由 `MigrationReport.residue` 报告，处置需用户在可信界面
 *   明示（A0-05 冻结的协议）。
 * - `relocate`：用户自己把总仓搬好了，这里只**核验并切换定位**。不复制、不清空重导，
 *   素材 ID、标签、评分与历史关联全部保留。
 *
 * 两个都不可被 Agent 调用：它们不在能力表里。
 */
import { invoke } from "./client";

export type LibraryMigrationKind = "migrate" | "relocate";

/** 迁移阶段。`switched` 是分水岭：定位已换，源仍完整。 */
export type LibraryMigrationPhase =
  | "preparing"
  | "copying"
  | "switched"
  | "cleaning"
  | "completed"
  | "failed"
  | "aborted";

export type LibraryMigrationWarningCode = "INSUFFICIENT_SPACE" | (string & {});

export interface LibraryMigrationWarning {
  code: LibraryMigrationWarningCode;
  message: string;
}

/**
 * 「开始迁移会影响到哪些任务」的投影（A4-06）。
 *
 * 语义要点：维护期**不会静默取消任何任务**。`blocking` 为真时「开始迁移」是禁用的，
 * 用户必须自己先结束或取消那些任务——这样「迁移把别人的打标结果弄坏」不可能由一次
 * 点击产生。
 */
export interface MigrationCapabilityCount {
  capability: string;
  count: number;
}

export interface MigrationTaskImpact {
  /** 当前总仓代次下没有终态的操作（排队 / 执行中 / 被中断 / 结果不明）。 */
  runningOperations: number;
  runningByCapability: MigrationCapabilityCount[];
  pendingAiBatches: number;
  importRunning: boolean;
  refillRunning: boolean;
  runningExports: number;
  blocking: boolean;
  /** 逐条中文说明，界面直接展示，不自行拼句。 */
  messages: string[];
}

/**
 * 预检结果。界面必须先展示它、再允许「开始」——卡片禁止用文本框改路径直接提交。
 */
export interface LibraryMigrationPreflight {
  runId: string;
  kind: LibraryMigrationKind;
  libraryId: string;
  sourceRoot: string;
  targetRoot: string;
  totalItems: number;
  totalBytes: number;
  /** 目标剩余空间。`null` 表示当前平台无法预检（不是「足够」）。 */
  targetFreeBytes: number | null;
  /** 固定说明，例如「源目录不会被自动删除」。 */
  notes: string[];
  warnings: LibraryMigrationWarning[];
  impact: MigrationTaskImpact;
}

export type LibraryMigrationItemState = "pending" | "copied" | "verified" | "failed" | "skipped";

export type LibraryMigrationItemErrorCode =
  | "TARGET_NOT_EMPTY"
  | "TARGET_INSIDE_SOURCE"
  | "TARGET_IS_LINK"
  | "SOURCE_MISSING"
  | "SOURCE_CHANGED"
  | "INSUFFICIENT_SPACE"
  | "WRONG_LIBRARY"
  | "FILE_MISSING"
  | "CHECKSUM_MISMATCH"
  | "PERMISSION_DENIED"
  | "LIBRARY_CHANGED"
  | "INTERRUPTED"
  | "IO_ERROR";

export interface LibraryMigrationItem {
  itemId: string;
  relativePath: string;
  byteSize: number;
  state: LibraryMigrationItemState;
  errorCode: LibraryMigrationItemErrorCode | null;
}

/** 一次迁移的完整结果。失败项单独给出，不会被静默重试。 */
export interface LibraryMigrationReport {
  runId: string;
  kind: LibraryMigrationKind;
  phase: LibraryMigrationPhase;
  sourceRoot: string;
  targetRoot: string;
  totalItems: number;
  verifiedItems: number;
  failedItems: number;
  errorCode: LibraryMigrationItemErrorCode | null;
  errorMessage: string | null;
  /** 失败项。存在任一项时定位一定没有切换。 */
  failed: LibraryMigrationItem[];
  items: LibraryMigrationItem[];
}

/** 维护态投影：界面据此显示横幅并禁写。 */
export interface LibraryMaintenanceStatus {
  active: boolean;
  reason: string | null;
  /** 未结束的迁移；非空表示用户应当「继续」而不是重新开始。 */
  migration: LibraryMigrationReport | null;
}

/**
 * 预检：检查目标、容量、嵌套与清单完整性。只读，不写 marker、不复制。
 */
export function preflightLibraryMigration(
  targetRoot: string,
  kind: LibraryMigrationKind,
): Promise<LibraryMigrationPreflight> {
  return invoke<LibraryMigrationPreflight>("preflight_library_migration", {
    targetRoot,
    kind,
  });
}

/**
 * 开始（或继续）一次已确认的迁移。
 *
 * 后端会重新预检并与入参比对；清单在预检后发生变化会直接报 `LIBRARY_CHANGED`，
 * 而不是照着一个过期计划动文件。
 */
export function startLibraryMigration(
  targetRoot: string,
  kind: LibraryMigrationKind,
): Promise<LibraryMigrationReport> {
  return invoke<LibraryMigrationReport>("start_library_migration", { targetRoot, kind });
}

/**
 * 请求停止迁移。返回 `false` 表示当前没有针对该目标的迁移在跑。
 *
 * 只置取消标志：正在写的那一个文件会写完再停；已切换定位后不再接受取消。
 */
export function cancelLibraryMigration(targetRoot: string): Promise<boolean> {
  return invoke<boolean>("cancel_library_migration", { targetRoot });
}

/**
 * 只读地取一次任务影响。向导打开时先查它，用户不必先选目标目录就能知道
 * 「现在能不能开始」。
 */
export function libraryTaskImpact(): Promise<MigrationTaskImpact> {
  return invoke<MigrationTaskImpact>("library_task_impact");
}

/** 当前维护态 + 未结束的迁移。 */
export function libraryMaintenanceStatus(): Promise<LibraryMaintenanceStatus> {
  return invoke<LibraryMaintenanceStatus>("library_maintenance_status");
}

/** 读一次迁移的完整报告。 */
export function libraryMigrationReport(runId: string): Promise<LibraryMigrationReport> {
  return invoke<LibraryMigrationReport>("library_migration_report", { runId });
}

/**
 * 收尾一次被中断的迁移。返回被处理的 runId 列表。
 *
 * 启动对账会自动跑一次；界面按钮用于用户主动重试「已切换但收尾未完成」。
 */
export function resumeLibraryMigration(): Promise<string[]> {
  return invoke<string[]>("resume_library_migration");
}

/** 维护态变化时向导广播的窗口事件；横幅据此刷新，不轮询。 */
export const MAINTENANCE_EVENT = "library-maintenance-changed";

/** 阶段的中文说明。`switched`/`cleaning` 都是**可恢复**状态，不是失败。 */export const MIGRATION_PHASE_TEXT: Record<LibraryMigrationPhase, string> = {
  preparing: "正在核对清单",
  copying: "正在复制并校验",
  switched: "已切换总仓位置，正在重建索引",
  cleaning: "已切换总仓位置，正在收尾",
  completed: "已完成",
  failed: "已中止（总仓位置未改变）",
  aborted: "已取消（总仓位置未改变）",
};

/** 逐文件错误码的可读文案。未知码原样回显，便于用户报障。 */
export const MIGRATION_ITEM_ERROR_TEXT: Record<LibraryMigrationItemErrorCode, string> = {
  TARGET_NOT_EMPTY: "目标目录已有其他内容",
  TARGET_INSIDE_SOURCE: "目标目录位于源目录内部",
  TARGET_IS_LINK: "目标目录是链接或联接",
  SOURCE_MISSING: "源文件已不在原位置",
  SOURCE_CHANGED: "源文件在复制期间被修改",
  INSUFFICIENT_SPACE: "目标磁盘空间不足",
  WRONG_LIBRARY: "目标目录不是当前总仓",
  FILE_MISSING: "目标位置缺少这个文件",
  CHECKSUM_MISMATCH: "复制后的内容校验不一致",
  PERMISSION_DENIED: "没有读写权限",
  LIBRARY_CHANGED: "总仓清单已变化，请重试",
  INTERRUPTED: "执行被中断",
  IO_ERROR: "磁盘读写失败",
};

export function migrationItemErrorLabel(item: LibraryMigrationItem): string {
  if (!item.errorCode) {
    return "未知原因";
  }
  return MIGRATION_ITEM_ERROR_TEXT[item.errorCode] ?? String(item.errorCode);
}

/** 只挑出失败项给人看。 */
export function failedMigrationItems(
  report: LibraryMigrationReport | null,
): LibraryMigrationItem[] {
  return report?.failed ?? [];
}

/** 是否处于「定位已切换，但收尾没做完」的可恢复态。 */
export function isResumableMigration(report: LibraryMigrationReport | null): boolean {
  return report?.phase === "switched" || report?.phase === "cleaning";
}
