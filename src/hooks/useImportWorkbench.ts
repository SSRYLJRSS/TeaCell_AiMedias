/** 入库工作台行为层（A3-07）。
 *
 * 为什么抽成 hook：独立入库页（`src/pages/ImportPage.tsx`）和助手右侧工作台
 * （`src/components/assistant/AssistantWorkbench.tsx`）必须操作**同一个**草稿与任务，
 * 否则同一批文件会被两次提交、两次确认。领域状态只存在于
 * `importWorkbenchStore`（按 conversationRef 分桶、写回 SQLite），本 hook 只做
 * 「派生视图 + 组合动作」，两个入口共用即可保证两处显示一致。
 *
 * 边界：
 * - 这里不直接 `invoke`，也不复制任何领域状态；所有写操作都落到 store 或 `@/api/*`；
 * - 拖放注册（`registerImportDropHandler`）留在页面层，因为它是整页手势，不是工作台状态。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import { newImportTaskId } from "@/api/import";
import { listDirectoryGrants, pickDirectoryGrant, reissueDirectoryGrant } from "@/api/authorization";
import type { DirectoryGrant } from "@/api/authorization";
import type { ImportPlanItem } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useImportWorkbenchStore } from "@/stores/importWorkbenchStore";
import { useConversationStore } from "@/stores/conversationStore";
import { useTaskStore } from "@/stores/taskStore";

export const IMPORT_FILE_FILTERS = [{
  name: "图片与视频",
  extensions: [
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tga", "tif", "tiff", "heic", "heif",
    "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf",
    "rw2", "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff", "kdc", "dcr", "mos", "mef", "erf",
    "mp4", "mov", "avi", "mkv", "webm", "m4v", "mts", "m2ts",
  ],
}];

export type ImportWorkbenchItem = ImportPlanItem & {
  planRef: string;
  itemRef: string;
  displayName: string;
};

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

export function useImportWorkbench() {
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

  const [checkingTaskId, setCheckingTaskId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showWarnings, setShowWarnings] = useState(false);
  const [showUnsupported, setShowUnsupported] = useState(false);
  /** A3-06：目录默认只授权本次读取；只有显式勾选才记住，且记住的授权可在设置里撤销。 */
  const [rememberDirectory, setRememberDirectory] = useState(false);
  /** A3-06：已记住的导入来源目录。没有这个入口，「记住这个目录（下次不用重新选择）」
   *  就是空承诺——`list_directory_grants` 返回的项 `directoryRef` 为空，
   *  必须经 `reissue_directory_grant` 换出新句柄才能真正读取。 */
  const [rememberedGrants, setRememberedGrants] = useState<DirectoryGrant[]>([]);
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

  const items = useMemo<ImportWorkbenchItem[]>(() => Object.entries(workbench.plans).flatMap(([planRef, plan]) =>
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

  const chooseFiles = useCallback(async () => {
    if (busy || !(await ensureImportLibraryRoot(setError))) return;
    try {
      const picked = await pickFiles({ multiple: true, filters: IMPORT_FILE_FILTERS });
      if (Array.isArray(picked)) void stage(picked);
      else if (typeof picked === "string") void stage([picked]);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [busy, stage]);

  /** 读取「已记住的导入来源目录」列表；只列本次可用且属于导入来源的授权。 */
  const refreshRememberedGrants = useCallback(async () => {
    try {
      const grants = await listDirectoryGrants();
      setRememberedGrants(grants.filter((grant) => grant.purpose === "import_source"));
    } catch {
      // 列不出来不影响手动选择目录，静默降级为空列表（不伪造可用项）。
      setRememberedGrants([]);
    }
  }, []);

  useEffect(() => {
    void refreshRememberedGrants();
  }, [refreshRememberedGrants]);

  const chooseFolder = useCallback(async () => {
    if (busy || !(await ensureImportLibraryRoot(setError))) return;
    try {
      const grant = await pickDirectoryGrant("import_source", rememberDirectory);
      if (!grant) return;
      if (!grant.available || !grant.directoryRef) {
        setError("所选目录授权不可用，请重新选择目录。");
        return;
      }
      const plan = await workbench.prepareDirectory(grant.directoryRef);
      if (plan.items.some((item) => !item.excluded && item.previewStatus === "unsupported")) setShowUnsupported(true);
      // 勾选「记住」后立即可在「已记住的目录」里看到，不必等下次进页面。
      if (rememberDirectory) void refreshRememberedGrants();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [busy, rememberDirectory, refreshRememberedGrants, workbench]);

  /** 直接使用已记住的目录：换发新句柄后走同一条 prepareDirectory 路径。
   *
   * 为什么必须换发：持久化的记住授权只保留 grant，句柄按会话/实例签发，
   * 所以不能把旧句柄拿来复用，也不能凭 grantRef 直接读目录。
   */
  const chooseRememberedFolder = useCallback(async (grantRef: string) => {
    if (busy || !(await ensureImportLibraryRoot(setError))) return;
    setError(null);
    try {
      const grant = await reissueDirectoryGrant(grantRef);
      if (!grant.available || !grant.directoryRef) {
        setError("这个目录已不可用（可能被移动或删除），请重新选择目录。");
        await refreshRememberedGrants();
        return;
      }
      const plan = await workbench.prepareDirectory(grant.directoryRef);
      if (plan.items.some((item) => !item.excluded && item.previewStatus === "unsupported")) setShowUnsupported(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      await refreshRememberedGrants();
    }
  }, [busy, refreshRememberedGrants, workbench]);

  const removeItem = useCallback(async (item: ImportPlanItem) => {
    const candidate = item as ImportWorkbenchItem;
    try {
      await workbench.removeItem(candidate.planRef, candidate.itemRef);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [workbench]);

  const removeUnsupported = useCallback(async () => {
    try {
      for (const group of unsupportedByPlan) await workbench.removeUnsupported(group.planRef);
      setShowUnsupported(false);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [unsupportedByPlan, workbench]);

  const runSafely = useCallback(async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, []);

  const submitAll = useCallback(async () => {
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
  }, [operationInProgress, unsupportedCount, workbench]);

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

  return {
    workbench,
    /** 传给 PendingList / ImportProgressPanel 的统一错误（页面内错误优先） */
    error: error ?? workbench.error,
    localError: error,
    setError,
    libraryRoot,
    settingsLoaded,
    settingsLoadError,
    loadSettings,
    busy,
    operationInProgress,
    submissionUncertain,
    conversationSwitching,
    activeOperations,
    operationSnapshots,
    activeOperationRef,
    selectedTask,
    items,
    imageCount,
    videoCount,
    totalSize,
    unsupportedByPlan,
    unsupportedCount,
    showUnsupported,
    setShowUnsupported,
    showWarnings,
    setShowWarnings,
    rememberDirectory,
    setRememberDirectory,
    rememberedGrants,
    refreshRememberedGrants,
    stage,
    chooseFiles,
    chooseFolder,
    chooseRememberedFolder,
    removeItem,
    removeUnsupported,
    runSafely,
    submitAll,
  };
}

export type ImportWorkbenchController = ReturnType<typeof useImportWorkbench>;
