/** Manual import workbench: durable drafts point at fixed plans and exact operations. */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import ProgressBar from "@/components/common/ProgressBar";
import PendingList, { formatSize } from "@/components/import/PendingList";
import RenameBuilder from "@/components/import/RenameBuilder";
import { displayBasename } from "@/utils/pathDisplay";
import { newImportTaskId, openFileExternal } from "@/api/import";
import type { ImportPlanItem } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";
import { pickDirectoryGrant } from "@/api/authorization";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useImportWorkbenchStore } from "@/stores/importWorkbenchStore";
import { useConversationStore } from "@/stores/conversationStore";
import { importPhaseLabel, useTaskStore, type TaskItem } from "@/stores/taskStore";
import { registerImportDropHandler } from "@/utils/importDropDispatcher";

const FILE_FILTERS = [{
  name: "图片与视频",
  extensions: [
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tga", "tif", "tiff", "heic", "heif",
    "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf",
    "rw2", "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff", "kdc", "dcr", "mos", "mef", "erf",
    "mp4", "mov", "avi", "mkv", "webm", "m4v", "mts", "m2ts",
  ],
}];

async function ensureImportLibraryRoot(setError: (message: string) => void): Promise<boolean> {
  let settings = useSettingsStore.getState();
  if (!settings.loaded || settings.loadError) {
    await settings.load();
    settings = useSettingsStore.getState();
  }
  if (settings.loadError) {
    setError(`设置读取失败：${settings.loadError}`);
    return false;
  }
  if (!settings.settings?.libraryRoot.trim()) {
    setError("请先设置素材总库位置，再选择或导入文件。");
    return false;
  }
  return true;
}

function waitForNextFrame(): Promise<void> {
  return new Promise((resolve) => {
    if (typeof window.requestAnimationFrame === "function") window.requestAnimationFrame(() => resolve());
    else window.setTimeout(resolve, 0);
  });
}

function operationLabel(operation: OperationSnapshot): string {
  switch (operation.state) {
    case "queued": return "排队中";
    case "running": return operation.phase === "cancelling" ? "取消中" : "正在入库";
    case "succeeded": return "已完成";
    case "partially_succeeded": return "部分完成";
    case "failed": return "失败";
    case "cancelled": return "已取消";
    case "interrupted": return "已中断，可恢复";
    case "outcome_unknown": return "结果待核对";
  }
}

type WorkbenchItem = ImportPlanItem & { planRef: string; itemRef: string; displayName: string };

export default function ImportPage() {
  const workbench = useImportWorkbenchStore();
  const activeConversationRef = useConversationStore((s) => s.activeRef);
  const initializeWorkbench = workbench.initialize;
  const switchWorkbenchConversation = workbench.switchConversation;
  const refreshOperations = workbench.refreshOperations;
  const prepareFiles = workbench.prepareFiles;
  const loadOperationResult = workbench.loadOperationResult;
  const libraryRoot = useSettingsStore((s) => s.settings?.libraryRoot ?? "");
  const settingsLoaded = useSettingsStore((s) => s.loaded);
  const settingsLoadError = useSettingsStore((s) => s.loadError);
  const loadSettings = useSettingsStore((s) => s.load);
  const refreshLibrary = useLibraryStore((s) => s.refresh);
  const [dragOver, setDragOver] = useState(false);
  const [checkingTaskId, setCheckingTaskId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showWarnings, setShowWarnings] = useState(false);
  const [showUnsupported, setShowUnsupported] = useState(false);
  const refreshedOperations = useRef(new Map<string, number>());

  useEffect(() => {
    void initializeWorkbench();
    if (!settingsLoaded) void loadSettings();
  }, [initializeWorkbench, settingsLoaded, loadSettings]);

  useEffect(() => {
    if (!workbench.initialized || workbench.loading
      || activeConversationRef === workbench.conversationRef) return;
    void switchWorkbenchConversation(activeConversationRef);
  }, [
    activeConversationRef,
    switchWorkbenchConversation,
    workbench.conversationRef,
    workbench.initialized,
    workbench.loading,
  ]);

  useEffect(() => {
    void refreshOperations();
  }, [refreshOperations]);

  const taskItems = useTaskStore((s) => s.tasks);
  const taskOperationSnapshots = useTaskStore((s) => s.operationSnapshots);
  const operationSnapshots = useMemo(() => {
    const snapshots: Record<string, OperationSnapshot> = {};
    for (const operationRef of workbench.draft.recentOperationRefs) {
      const snapshot = taskOperationSnapshots[operationRef] ?? workbench.operations[operationRef];
      if (snapshot) snapshots[operationRef] = snapshot;
    }
    return snapshots;
  }, [workbench.draft.recentOperationRefs, workbench.operations, taskOperationSnapshots]);
  const activeOperations = useMemo(() => Object.values(operationSnapshots)
    .filter((operation) => operation.state === "queued" || operation.state === "running"), [operationSnapshots]);
  const submissionUncertain = workbench.draft.pendingPlanRefs.some((entry) => Boolean(entry.idempotencyKey));
  const conversationSwitching = workbench.loading || activeConversationRef !== workbench.conversationRef;
  const operationInProgress = conversationSwitching || workbench.preparing || workbench.submitting || activeOperations.length > 0;
  const busy = operationInProgress || submissionUncertain;

  const items = useMemo<WorkbenchItem[]>(() => Object.entries(workbench.plans).flatMap(([planRef, plan]) =>
    plan.items.filter((item) => !item.excluded).map((item) => ({
      path: workbench.planPaths[planRef]?.[item.itemRef] ?? "",
      planRef,
      itemRef: item.itemRef,
      displayName: item.displayName,
      kind: item.kind,
      size: item.size,
      previewStatus: item.previewStatus,
      previewMessage: item.previewMessage,
    }))), [workbench.plans, workbench.planPaths]);

  const unsupportedByPlan = useMemo(() => Object.entries(workbench.plans)
    .map(([planRef, plan]) => ({
      planRef,
      items: plan.items.filter((item) => !item.excluded && item.previewStatus === "unsupported"),
    }))
    .filter((group) => group.items.length > 0), [workbench.plans]);
  const unsupportedCount = unsupportedByPlan.reduce((sum, group) => sum + group.items.length, 0);
  const imageCount = items.filter((item) => item.kind === "image").length;
  const videoCount = items.length - imageCount;
  const totalSize = items.reduce((sum, item) => sum + item.size, 0);

  useEffect(() => {
    if (unsupportedCount > 0) setShowUnsupported(true);
  }, [unsupportedCount]);

  const stage = useCallback(async (paths: string[]) => {
    if (!paths.length || busy) return;
    setError(null);
    if (!(await ensureImportLibraryRoot(setError))) return;
    const taskId = newImportTaskId();
    setCheckingTaskId(taskId);
    try {
      await waitForNextFrame();
      const result = await prepareFiles(paths, taskId);
      if (!result.plan && result.warnings.length === 0) setError("未发现可入库的图片/视频文件");
      if (result.plan?.items.some((item) => !item.excluded && item.previewStatus === "unsupported")) {
        setShowUnsupported(true);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCheckingTaskId((currentTaskId) => currentTaskId === taskId ? null : currentTaskId);
    }
  }, [busy, prepareFiles]);

  useEffect(() => registerImportDropHandler((payload) => {
    if (payload.type === "over") setDragOver(true);
    else if (payload.type === "leave") setDragOver(false);
    else if (payload.type === "drop") {
      setDragOver(false);
      void stage(payload.paths);
    }
  }), [stage]);

  const chooseFiles = async () => {
    if (busy || !(await ensureImportLibraryRoot(setError))) return;
    try {
      const picked = await pickFiles({ multiple: true, filters: FILE_FILTERS });
      if (Array.isArray(picked)) void stage(picked);
      else if (typeof picked === "string") void stage([picked]);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const chooseFolder = async () => {
    if (busy || !(await ensureImportLibraryRoot(setError))) return;
    try {
      const grant = await pickDirectoryGrant("import_source", false);
      if (!grant) return;
      if (!grant.available || !grant.directoryRef) {
        setError("所选目录授权不可用，请重新选择目录。");
        return;
      }
      const plan = await workbench.prepareDirectory(grant.directoryRef);
      if (plan.items.some((item) => !item.excluded && item.previewStatus === "unsupported")) setShowUnsupported(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const removeItem = async (item: ImportPlanItem) => {
    const candidate = item as WorkbenchItem;
    try {
      await workbench.removeItem(candidate.planRef, candidate.itemRef);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const removeUnsupported = async () => {
    try {
      for (const group of unsupportedByPlan) await workbench.removeUnsupported(group.planRef);
      setShowUnsupported(false);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const runSafely = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const submitAll = async () => {
    if (operationInProgress || unsupportedCount > 0 || workbench.draft.pendingPlanRefs.length === 0) return;
    setError(null);
    for (const { planRef } of workbench.draft.pendingPlanRefs) {
      try {
        await workbench.submitPlan(planRef);
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : String(cause));
        return;
      }
    }
  };

  useEffect(() => {
    for (const operation of Object.values(operationSnapshots)) {
      const terminal = operation.state !== "queued" && operation.state !== "running";
      if (!terminal) {
        refreshedOperations.current.delete(operation.operationId);
      } else if (refreshedOperations.current.get(operation.operationId) !== operation.revision) {
        refreshedOperations.current.set(operation.operationId, operation.revision);
        void refreshLibrary();
        void useMetadataStore.getState().refresh();
        void loadOperationResult(operation.operationId, operation.revision);
      }
    }
  }, [operationSnapshots, refreshLibrary, loadOperationResult]);

  const activeOperationRef = activeOperations[0]?.operationId ?? workbench.draft.recentOperationRefs[0];
  const selectedTask = taskItems.find((task) => task.id === (checkingTaskId ?? activeOperationRef));
  const combinedError = error ?? workbench.error;

  return (
    <div className="flex h-full">
      <Modal
        open={showUnsupported && unsupportedCount > 0}
        title="有文件无法生成缩略图"
        onClose={() => setShowUnsupported(false)}
        footer={(
          <>
            <Button onClick={() => setShowUnsupported(false)}>暂不处理</Button>
            <Button variant="primary" onClick={() => void removeUnsupported()}>剔除并保留可导入项</Button>
          </>
        )}
      >
        <p className="text-sm text-[var(--color-text)]">
          以下 {unsupportedCount} 个文件无法解析入库缩略图，当前版本禁止导入。确认后会从固定计划中剔除。
        </p>
        <ul className="mt-3 max-h-40 space-y-1 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2 text-xs text-[var(--color-text-secondary)]">
          {unsupportedByPlan.flatMap((group) => group.items.map((item) => (
            <li key={`${group.planRef}-${item.itemRef}`} className="break-all" title={item.previewMessage}>{item.displayName}</li>
          )))}
        </ul>
      </Modal>

      <aside className="flex w-[190px] shrink-0 flex-col border-r border-[var(--color-border)]">
        <div className="border-b border-[var(--color-border)] p-3">
          <h3 className="mb-2 text-xs font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">待入库清单</h3>
          <div className="flex flex-col gap-1 text-sm text-[var(--color-text)]">
            <span>图片 {imageCount} 张</span>
            <span>视频 {videoCount} 个</span>
            <span className="text-xs text-[var(--color-text-secondary)]">共 {items.length} 项 · {formatSize(totalSize)}</span>
          </div>
        </div>

        <div className="flex flex-col gap-3 p-3">
          <label className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">
              分库名称{libraryRoot ? "（总库下新建文件夹）" : "（需先在设置里配置总库）"}
            </span>
            <input
              value={workbench.draft.collection}
              onChange={(event) => void runSafely(() => workbench.updateOptions(event.target.value, workbench.draft.renamePattern))}
              disabled={!libraryRoot || workbench.submitting || conversationSwitching}
              placeholder="如：旅行"
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-40"
            />
          </label>
          <RenameBuilder
            value={workbench.draft.renamePattern}
            onChange={(value) => void runSafely(() => workbench.updateOptions(workbench.draft.collection, value))}
            disabled={!libraryRoot || workbench.submitting || conversationSwitching}
            collection={workbench.draft.collection.trim()}
            sampleStem={items[0] ? displayBasename(items[0].displayName).replace(/\.[^.]+$/, "") : ""}
            sampleExt={items[0]?.displayName.split(".").pop() ?? ""}
          />
        </div>

        <div className="mt-auto border-t border-[var(--color-border)] p-3">
          {workbench.preparing ? (
            <Button className="mb-3 w-full" onClick={() => void runSafely(() => workbench.cancelPreparation())}>取消准备</Button>
          ) : items.length > 0 && (
            <div className="mb-3">
              {activeOperations[0]?.canCancel ? (
                <Button className="w-full" disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(activeOperations[0].operationId))}>取消入库</Button>
              ) : operationInProgress ? (
                <Button className="w-full" disabled>正在处理…</Button>
              ) : (
                <>
                  <Button
                    variant="primary"
                    className="w-full"
                    disabled={unsupportedCount > 0 || workbench.draft.pendingPlanRefs.length !== Object.keys(workbench.plans).length}
                    onClick={() => void submitAll()}
                  >
                    开始入库（{items.length}）
                  </Button>
                  <Button
                    className="mt-1 w-full"
                    disabled={submissionUncertain}
                    onClick={() => void runSafely(() => workbench.clearPendingPlans())}
                    title={submissionUncertain ? "受理响应待核对，请重试该计划" : undefined}
                  >清空清单</Button>
                </>
              )}
            </div>
          )}
          {busy && items.length === 0 && activeOperations[0]?.canCancel && (
            <Button className="mb-3 w-full" disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(activeOperations[0].operationId))}>取消入库</Button>
          )}
          <ImportProgressPanel task={selectedTask} starting={workbench.preparing} />
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col p-6">
        {(!settingsLoaded || settingsLoadError || !libraryRoot.trim()) && (
          <div className="mb-3 flex w-full min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-text-secondary)]" role="status">
            <p className="min-w-0 flex-1 break-words leading-5">
              {settingsLoadError ? `设置读取失败：${settingsLoadError}` : !settingsLoaded ? "正在读取设置，请稍候。" : "请先设置素材总库位置，再选择或导入文件。"}
            </p>
            {settingsLoadError ? <Button onClick={() => void loadSettings()}>重试</Button> : settingsLoaded && !libraryRoot.trim() ? (
              <Button className="shrink-0" onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}>前往设置</Button>
            ) : null}
          </div>
        )}

        {combinedError && !workbench.preparing && <p className="mb-3 text-xs text-[var(--color-danger)]" role="alert">{combinedError}</p>}
        {unsupportedCount === 0 && items.some((item) => item.previewStatus === "limited") && (
          <div className="mb-3 w-full min-w-0 rounded-md border border-[var(--color-status)] bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-status)]">
            有 {items.filter((item) => item.previewStatus === "limited").length} 个特殊格式已解析出缩略图，可以导入；但后续高清预览、元数据或 AI 功能可能受限。
          </div>
        )}
        {workbench.draft.warnings.length > 0 && (
          <div className="mb-3 max-w-lg text-xs text-[var(--color-status)]" role="status">
            <p>扫描时跳过或遇到 {workbench.draft.warnings.length} 项，请确认后再入库。</p>
            <button type="button" onClick={() => setShowWarnings((value) => !value)} className="mt-1 text-left underline">
              {showWarnings ? "收起扫描提示" : "查看扫描提示"}
            </button>
            {showWarnings && <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
              {workbench.draft.warnings.map((warning, index) => <li key={`${index}-${warning}`} className="break-all">{warning}</li>)}
            </ul>}
          </div>
        )}

        {workbench.draft.pendingPlanRefs.length > 0 && (
          <div className="mb-3 space-y-1 rounded-md border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-text-secondary)]">
            {workbench.draft.pendingPlanRefs.map(({ planRef, idempotencyKey }) => {
              const plan = workbench.plans[planRef];
              return <div key={planRef} className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate">{workbench.planErrors[planRef] ?? `待确认计划 · ${plan?.itemCount ?? "读取中"} 项`}</span>
                {idempotencyKey && !plan && <Button disabled={workbench.submitting || conversationSwitching} onClick={() => void runSafely(() => workbench.submitPlan(planRef))}>重试并核对受理</Button>}
                {plan && <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.handoffPlan(planRef))}>交给助手（待确认）</Button>}
              </div>;
            })}
          </div>
        )}

        {items.length > 0 ? (
          <PendingList
            items={items}
            running={busy || submissionUncertain}
            onRemove={() => undefined}
            onRemoveCandidate={(item) => void removeItem(item)}
            onAddFiles={() => void chooseFiles()}
            onAddFolder={() => void chooseFolder()}
            onClear={() => void runSafely(() => workbench.clearPendingPlans())}
            onOpenItem={(path) => void openFileExternal(path).catch(() => undefined)}
          />
        ) : (
          <div className={clsx(
            "flex flex-1 flex-col items-center justify-center gap-3 rounded-xl border-2 border-dashed transition-colors",
            dragOver ? "border-[var(--color-accent)] bg-[var(--color-surface)]" : "border-[var(--color-border)]",
          )}>
            <p className="text-base text-[var(--color-text)]">把图片 / 视频拖到这里</p>
            <p className="text-sm text-[var(--color-text-secondary)]">支持文件夹递归，重复文件自动识别；先入清单，手动确认才入库</p>
            {!busy && <div className="flex gap-2">
              <Button variant="primary" onClick={() => void chooseFiles()}>选择文件…</Button>
              <Button onClick={() => void chooseFolder()}>选择文件夹…</Button>
            </div>}
          </div>
        )}

        {workbench.draft.recentOperationRefs.length > 0 && (
          <section className="mt-3 space-y-1" aria-label="最近入库操作">
          {workbench.draft.recentOperationRefs.map((operationRef) => {
              const operation = operationSnapshots[operationRef];
              if (!operation) return null;
              const result = workbench.operationResults[operationRef];
              const resultError = workbench.operationResultErrors[operationRef];
              const resultLoading = workbench.operationResultLoading[operationRef] ?? false;
              const failures = result?.failures ?? [];
              return <div key={operationRef} className="rounded-md border border-[var(--color-border)] px-3 py-2 text-xs">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="min-w-0 flex-1 text-[var(--color-text-secondary)]">
                    {operationLabel(operation)} · 已处理 {operation.completed}/{operation.total ?? "?"} · 失败 {operation.failed}
                  </span>
                  {operation.canCancel && <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(operationRef))}>取消此操作</Button>}
                  {operation.canResume && <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.resume(operationRef))}>继续此操作</Button>}
                  <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.handoffOperation(operationRef))}>交给助手（已受理）</Button>
                </div>
                {result && <p className="mt-1 text-[var(--color-text-secondary)]">新入库 {result.imported} · 重复 {result.duplicates} · 失败 {result.failed}</p>}
                {resultError && <div className="mt-1 flex flex-wrap items-center gap-2 text-[var(--color-status)]" role="alert">
                  <span>结果明细读取失败：{resultError}</span>
                  <Button disabled={resultLoading || conversationSwitching} onClick={() => void loadOperationResult(operationRef, operation.revision)}>
                    {resultLoading ? "正在读取结果…" : "重试读取结果"}
                  </Button>
                </div>}
                {failures.length > 0 && <details className="mt-2 text-[var(--color-status)]">
                  <summary className="cursor-pointer">查看异常明细（{failures.length}）</summary>
                  <ul className="mt-1 max-h-36 space-y-1 overflow-y-auto">
                    {failures.map((failure) => <li key={failure.itemRef} className="break-all">
                      {failure.displayName} · {failure.state === "outcome_unknown" ? "结果未知" : "失败"}{failure.errorCode ? ` · ${failure.errorCode}` : ""}
                    </li>)}
                  </ul>
                </details>}
              </div>;
            })}
          </section>
        )}
      </div>
    </div>
  );
}

function ImportProgressPanel({ task, starting }: { task?: TaskItem; starting: boolean }) {
  const progress = task?.importProgress;
  const idle = !task && !starting;
  const indeterminate = starting || Boolean(task && task.overall == null && !task.done);
  const percent = task?.overall == null ? null : Math.round(task.overall * 100);
  const stateText = task?.operationSnapshot ? task.detail : undefined;
  const phase = progress ? importPhaseLabel(progress.phase) : stateText ?? "正在检查文件";
  const phaseText = task?.detail?.startsWith("取消中") ? task.detail : phase;
  const hasErrors = Boolean(progress && progress.failed > 0) || Boolean(task?.operationSnapshot?.failed);
  const taskError = task?.error && !hasErrors ? task.error : null;

  return (
    <section aria-label="入库进度" className="min-w-0" data-state={idle ? "idle" : task?.done ? "done" : "running"}>
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-xs font-medium text-[var(--color-text)]">入库进度</h3>
        <span className={clsx("shrink-0 text-[11px]", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text-secondary)]")}>
          {idle ? "等待入库" : indeterminate || percent == null ? "准备中" : `${percent}%`}
        </span>
      </div>
      {(!task?.done || percent != null) && <ProgressBar value={task?.overall ?? 0} indeterminate={indeterminate} className="mt-2" />}
      {!idle && <p className={clsx("mt-2 text-xs", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text)]")} aria-live="polite">{phaseText}</p>}
      {progress?.file && <p className="mt-1 truncate text-[11px] text-[var(--color-text-secondary)]" title={progress.file}>{progress.file}</p>}
      {progress?.phaseTotal != null && progress.phaseTotal > 0 && (
        <p className="mt-1 text-[11px] text-[var(--color-text-tertiary)]">当前阶段 {progress.phaseCurrent}/{progress.phaseTotal}</p>
      )}
      {task?.operationSnapshot && <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">
        已处理 {task.operationSnapshot.completed}/{task.operationSnapshot.total ?? "?"} · 失败 {task.operationSnapshot.failed}
      </p>}
      {progress && (progress.phase === "previewing" || progress.phase === "done") && <div className="mt-2 flex flex-wrap gap-x-2 gap-y-1 text-[11px] text-[var(--color-text-secondary)]">
        <span>成功 {progress.imported}</span><span>重复 {progress.duplicates}</span><span>失败 {progress.failed}</span>
      </div>}
      {taskError && <p className="mt-1 text-[11px] text-[var(--color-status)]">{taskError}</p>}
    </section>
  );
}
