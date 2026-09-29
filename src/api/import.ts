/** 入库相关命令封装（对应 commands/import_cmd.rs） */
import { invoke, on } from "./client";
import type { ImportResult } from "@/types/asset";
import type { UnlistenFn } from "@tauri-apps/api/event";

/** 入库阶段（阶段 1 契约，见指导书 §4.4）：后端只发阶段进度，前端按权重计算整体进度。 */
export type ImportPhase = "queued" | "scanning" | "checking" | "hashing" | "processing" | "previewing" | "review" | "failed" | "cancelled" | "done";

export interface ImportProgress {
  taskId: string;
  phase: ImportPhase;
  phaseCurrent: number;
  /** 未知时必须显示不确定进度（不伪造百分比） */
  phaseTotal: number | null;
  file?: string;
  imported: number;
  duplicates: number;
  failed: number;
  message?: string;
}

export interface ImportOptions {
  /** 分库名称（总库下新建子文件夹，R-32） */
  collection?: string;
  /** 批量改名模板：{分库} {原名} {日期} {序号} {序号:N}；空 = 不改名 */
  renamePattern?: string;
}

export function newImportTaskId(): string {
  return `import-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export function importFiles(paths: string[], taskId: string, opts: ImportOptions = {}): Promise<ImportResult> {
  return invoke<ImportResult>("import_files", {
    paths,
    taskId,
    collection: opts.collection,
    renamePattern: opts.renamePattern,
  });
}

/** 改名预览：直调后端 render_name（单一事实源，防前后端规则 drift；日期以今天示意） */
export function renderNamePreview(template: string, collection: string, origStem: string, seq = 1): Promise<string> {
  return invoke<string>("preview_rename", { template, collection, origStem, seq });
}

export function cancelImport(): Promise<void> {
  return invoke<void>("cancel_import");
}

export interface ImportPlanItem {
  path: string;
  kind: "image" | "video";
  size: number;
  previewStatus: "ready" | "limited" | "unsupported";
  previewMessage?: string;
}

export interface ImportPlan {
  items: ImportPlanItem[];
  images: number;
  videos: number;
  totalSize: number;
  warnings: string[];
}

/** 扫描路径生成待入库清单（不落库，两段式入库用） */
export function inspectImport(paths: string[], taskId: string): Promise<ImportPlan> {
  return invoke<ImportPlan>("inspect_import", { paths, taskId });
}

export function onImportProgress(handler: (p: ImportProgress) => void): Promise<UnlistenFn> {
  return on<ImportProgress>("import://progress", handler);
}

/** 用系统默认应用打开待入库原文件（§10 双击打开；失败静默由调用方处理） */
export function openFileExternal(path: string): Promise<void> {
  return invoke<void>("open_file_external", { path });
}
