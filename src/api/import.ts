/** 入库命令封装（对应 commands/import_cmd.rs）；响应 DTO 位于 src/types/import.ts。 */
import { invoke, on } from "./client";
import type { SubmissionResult } from "@/types/agent";
import type {
  ImportResultView,
  ImportPlan,
  ImportPlanRequest,
  ImportPlanVerification,
  ImportProgress,
  PreparedImportPlan,
} from "@/types/import";
import type { UnlistenFn } from "@tauri-apps/api/event";

export function newImportTaskId(): string {
  return `import-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export function renderNamePreview(
  template: string,
  collection: string,
  originalStem: string,
  sequence = 1,
): Promise<string> {
  return invoke<string>("preview_rename", {
    template,
    collection,
    origStem: originalStem,
    seq: sequence,
  });
}

/** Read-only scan before fixed-plan preparation; cancellation uses the same task ID. */
export function inspectImport(paths: string[], taskId: string): Promise<ImportPlan> {
  return invoke<ImportPlan>("inspect_import", { paths, taskId });
}

export function onImportProgress(handler: (progress: ImportProgress) => void): Promise<UnlistenFn> {
  return on<ImportProgress>("import://progress", handler);
}

export function openFileExternal(path: string): Promise<void> {
  return invoke<void>("open_file_external", { path });
}

/** 固定候选范围；只准备，不复制文件或受理操作。 */
export function prepareImport(
  request: ImportPlanRequest,
  taskId: string = crypto.randomUUID(),
): Promise<PreparedImportPlan> {
  return invoke<PreparedImportPlan>("prepare_import", {
    taskId,
    request: {
      sourceRef: request.sourceRef,
      paths: request.paths ?? [],
      collection: request.collection,
      renamePattern: request.renamePattern,
      basePlanRef: request.basePlanRef,
      excludedItemRefs: request.excludedItemRefs ?? [],
    },
  });
}

export function cancelPrepareImport(taskId: string): Promise<void> {
  return invoke<void>("cancel_prepare_import", { taskId });
}

export function getImportPlan(planRef: string): Promise<PreparedImportPlan> {
  return invoke<PreparedImportPlan>("get_import_plan", { planRef });
}

/** Ephemeral paths for trusted GUI preview actions; never persist these in a workbench draft. */
export function getImportPlanPreviewPaths(planRef: string): Promise<Record<string, string>> {
  return invoke<Record<string, string>>("get_import_plan_preview_paths", { planRef });
}

export function getImportResult(operationRef: string): Promise<ImportResultView> {
  return invoke<ImportResultView>("get_import_result", { operationRef });
}

export function verifyImportPlan(planRef: string): Promise<ImportPlanVerification> {
  return invoke<ImportPlanVerification>("verify_import_plan", { planRef });
}

/** The trusted GUI click authorizes and durably accepts this exact plan. */
export function confirmImport(planRef: string, idempotencyKey: string): Promise<SubmissionResult> {
  return invoke<SubmissionResult>("confirm_import", { planRef, idempotencyKey });
}
