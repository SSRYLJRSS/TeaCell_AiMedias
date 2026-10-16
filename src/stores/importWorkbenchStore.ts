import { create } from "zustand";
import { cancelOperation, resumeOperation } from "@/api/operations";
import {
  cancelPrepareImport,
  confirmImport,
  getImportPlan,
  getImportPlanPreviewPaths,
  getImportResult,
  inspectImport,
  newImportTaskId,
  prepareImport,
} from "@/api/import";
import { AppError } from "@/api/client";
import { getOperation } from "@/api/plans";
import {
  createConversation,
  linkConversation,
  listConversations,
} from "@/api/conversations";
import { useConversationStore } from "@/stores/conversationStore";
import { trackImportOperation } from "@/stores/taskStore";
import {
  EMPTY_IMPORT_WORKBENCH_DRAFT,
  isImportWorkbenchDraft,
  type ImportWorkbenchDraft,
  type ImportResultView,
  type PreparedImportPlan,
} from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

const MANUAL_WORKBENCH_REF_KEY = "chabaosucai.import-workbench-conversation.v1";
const MAX_PENDING_PLANS = 100;
const MAX_RECENT_OPERATIONS = 50;
const MAX_DRAFT_WARNING_BYTES = 12 * 1024;
const DEFINITELY_REJECTED_SUBMISSION_CODES = new Set([
  "INVALID_ARG",
  "INVALID_INPUT",
  "NOT_FOUND",
  "UNAUTHORIZED",
  "GRANT_REVOKED",
  "REFERENCE_EXPIRED",
  "LIBRARY_CHANGED",
  "LIBRARY_OFFLINE",
  "PLAN_EXPIRED",
  "PLAN_CHANGED",
  "IDEMPOTENCY_CONFLICT",
  "UNSUPPORTED_CAPABILITY",
]);

interface PreparedFilesResult {
  plan: PreparedImportPlan | null;
  warnings: string[];
  taskId: string;
}

interface ImportWorkbenchState {
  initialized: boolean;
  loading: boolean;
  preparing: boolean;
  submitting: boolean;
  preparingTaskId: string | null;
  error: string | null;
  conversationRef: string | null;
  draft: ImportWorkbenchDraft;
  plans: Record<string, PreparedImportPlan>;
  /** Session-only paths for trusted thumbnail/open actions; drafts persist refs only. */
  planPaths: Record<string, Record<string, string>>;
  planErrors: Record<string, string>;
  operations: Record<string, OperationSnapshot>;
  operationResults: Record<string, ImportResultView>;
  operationResultErrors: Record<string, string>;
  operationResultLoading: Record<string, boolean>;
  initialize: () => Promise<void>;
  switchConversation: (conversationRef: string | null) => Promise<void>;
  updateOptions: (collection: string, renamePattern: string) => Promise<void>;
  prepareFiles: (paths: string[], taskId?: string) => Promise<PreparedFilesResult>;
  prepareDirectory: (sourceRef: string) => Promise<PreparedImportPlan>;
  cancelPreparation: () => Promise<void>;
  removeItem: (planRef: string, itemRef: string) => Promise<void>;
  removeUnsupported: (planRef: string) => Promise<void>;
  repreparePlan: (
    planRef: string,
    options?: { collection: string; renamePattern: string },
    excludedItemRefs?: string[],
  ) => Promise<string>;
  clearPendingPlans: () => Promise<void>;
  submitPlan: (planRef: string) => Promise<OperationSnapshot>;
  handoffPlan: (planRef: string) => Promise<void>;
  handoffOperation: (operationRef: string) => Promise<void>;
  refreshOperations: () => Promise<void>;
  loadOperationResult: (operationRef: string, operationRevision?: number) => Promise<void>;
  cancel: (operationRef: string) => Promise<void>;
  resume: (operationRef: string) => Promise<void>;
  clearError: () => void;
}

let initializeInFlight: Promise<void> | null = null;
let projectionRevision = 0;
const operationResultLoads = new Map<string, Promise<void>>();
const operationResultRevisions = new Map<string, number>();

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function readManualConversationRef(): string | null {
  try {
    return typeof window === "undefined"
      ? null
      : window.localStorage.getItem(MANUAL_WORKBENCH_REF_KEY);
  } catch {
    return null;
  }
}

function saveManualConversationRef(conversationRef: string | null): void {
  try {
    if (typeof window === "undefined") return;
    if (conversationRef) window.localStorage.setItem(MANUAL_WORKBENCH_REF_KEY, conversationRef);
    else window.localStorage.removeItem(MANUAL_WORKBENCH_REF_KEY);
  } catch {
    // This is only a pointer; the authoritative draft remains in SQLite.
  }
}

function parseDraft(value: unknown): ImportWorkbenchDraft {
  return isImportWorkbenchDraft(value)
    ? {
        ...value,
        warnings: safeWarningEntries(value.warnings?.length ?? 0),
        pendingPlanRefs: value.pendingPlanRefs.slice(0, MAX_PENDING_PLANS),
        recentOperationRefs: value.recentOperationRefs.slice(0, MAX_RECENT_OPERATIONS),
      }
    : { ...EMPTY_IMPORT_WORKBENCH_DRAFT };
}

function safeWarningEntries(count: number): string[] {
  return Array.from({ length: Math.min(count, 500) }, (_, index) =>
    `扫描提示 ${index + 1}：有文件无法读取或已跳过`,
  );
}

function mergeWarnings(previous: string[], incoming: string[]): string[] {
  const encoder = new TextEncoder();
  let count = Math.min(previous.length + incoming.length, 500);
  let result = safeWarningEntries(count);
  while (result.length > 0 && encoder.encode(JSON.stringify(result)).length > MAX_DRAFT_WARNING_BYTES) {
    count -= 1;
    result = safeWarningEntries(count);
  }
  return result;
}

const draftSaveTails = new Map<string, Promise<void>>();

async function ensureConversation(): Promise<string> {
  const conversationStore = useConversationStore.getState();
  if (conversationStore.activeRef) return conversationStore.activeRef;

  const rememberedRef = readManualConversationRef();
  if (rememberedRef) {
    const conversations = await listConversations();
    if (conversations.some((item) => item.conversationRef === rememberedRef)) {
      await useConversationStore.getState().select(rememberedRef);
      return rememberedRef;
    }
    saveManualConversationRef(null);
  }

  const created = await createConversation("手动入库工作台");
  saveManualConversationRef(created.conversationRef);
  await useConversationStore.getState().select(created.conversationRef);
  return created.conversationRef;
}

async function persistDraft(
  conversationRef: string,
  draft: ImportWorkbenchDraft,
): Promise<void> {
  const previous = draftSaveTails.get(conversationRef) ?? Promise.resolve();
  const next = previous.catch(() => undefined).then(async () => {
    await useConversationStore.getState().saveDraft(
      conversationRef,
      "import",
      draft as unknown as Record<string, unknown>,
    );
  });
  draftSaveTails.set(conversationRef, next);
  try {
    await next;
  } finally {
    if (draftSaveTails.get(conversationRef) === next) draftSaveTails.delete(conversationRef);
  }
}

function ownedDraft(conversationRef: string): ImportWorkbenchDraft {
  const state = useImportWorkbenchStore.getState();
  return state.conversationRef === conversationRef
    ? state.draft
    : parseDraft(useConversationStore.getState().draftFor(conversationRef, "import"));
}

function isActiveWorkbench(conversationRef: string): boolean {
  const state = useImportWorkbenchStore.getState();
  return !state.loading
    && state.conversationRef === conversationRef
    && useConversationStore.getState().activeRef === conversationRef;
}

async function preparationOwner(): Promise<string> {
  const state = useImportWorkbenchStore.getState();
  if (state.loading || state.conversationRef !== useConversationStore.getState().activeRef) {
    throw new Error("当前会话正在切换，请等入库工作台加载完成后再准备计划。");
  }
  const ownerRef = state.conversationRef ?? await ensureConversation();
  // A newly created manual conversation has no projection yet.
  if (!useImportWorkbenchStore.getState().conversationRef
    && useConversationStore.getState().activeRef === ownerRef) {
    useImportWorkbenchStore.setState({ conversationRef: ownerRef });
  }
  return ownerRef;
}

async function loadConversationProjection(conversationRef: string): Promise<void> {
  const revision = ++projectionRevision;
  const superseded = () => {
    if (revision !== projectionRevision) return true;
    if (useConversationStore.getState().activeRef !== conversationRef) {
      // Release this load so the page effect can follow the new activeRef.
      useImportWorkbenchStore.setState({ initialized: true, loading: false });
      return true;
    }
    return false;
  };
  // The assistant may already have selected this conversation. Re-selecting it here
  // would let a slow projection load overwrite a newer activeRef selection.
  if (useConversationStore.getState().activeRef !== conversationRef) {
    await useConversationStore.getState().select(conversationRef);
  }
  if (superseded()) return;
  const conversationStore = useConversationStore.getState();
  const rawDraft = conversationStore.draftFor(conversationRef, "import");
  const draft = parseDraft(rawDraft);
  if (rawDraft !== undefined && !isImportWorkbenchDraft(rawDraft)) {
    useImportWorkbenchStore.setState({
      initialized: true,
      loading: false,
      conversationRef,
      error: "入库草稿版本无法识别，已保留原记录；请导出或处理后再继续。",
      draft: { ...EMPTY_IMPORT_WORKBENCH_DRAFT },
      plans: {},
      planPaths: {},
      planErrors: {},
      operations: {},
      operationResults: {},
      operationResultErrors: {},
      operationResultLoading: {},
    });
    return;
  }
  if (isImportWorkbenchDraft(rawDraft)
    && JSON.stringify(rawDraft.warnings ?? []) !== JSON.stringify(draft.warnings)) {
    await persistDraft(conversationRef, draft);
  }

  const planResults = await Promise.all(draft.pendingPlanRefs.map(async ({ planRef }) => {
    try {
      const [plan, paths] = await Promise.all([
        getImportPlan(planRef),
        getImportPlanPreviewPaths(planRef),
      ]);
      return { kind: "ok" as const, planRef, plan, paths };
    } catch (error) {
      return { kind: "error" as const, planRef, error: errorMessage(error) };
    }
  }));
  const operationResults = await Promise.all(draft.recentOperationRefs.map(async (operationRef) => {
    try {
      return { kind: "ok" as const, operationRef, operation: await getOperation(operationRef) };
    } catch (error) {
      return { kind: "error" as const, operationRef, error: errorMessage(error) };
    }
  }));
  const plans: Record<string, PreparedImportPlan> = {};
  const planPaths: Record<string, Record<string, string>> = {};
  const planErrors: Record<string, string> = {};
  for (const result of planResults) {
    if (result.kind === "ok") {
      plans[result.planRef] = result.plan;
      planPaths[result.planRef] = result.paths;
    }
    else planErrors[result.planRef] = result.error;
  }
  const operations: Record<string, OperationSnapshot> = {};
  for (const result of operationResults) {
    if (result.kind === "ok") {
      operations[result.operationRef] = result.operation;
      trackImportOperation(result.operation);
    }
  }
  if (superseded()) return;
  useImportWorkbenchStore.setState({
    initialized: true,
    loading: false,
    conversationRef,
    draft,
    plans,
    planPaths,
    planErrors,
    operations,
    operationResults: {},
    operationResultErrors: {},
    operationResultLoading: {},
    error: null,
  });
  for (const operation of Object.values(operations)) {
    if (operation.state !== "queued" && operation.state !== "running") {
      void useImportWorkbenchStore.getState().loadOperationResult(operation.operationId);
    }
  }
}

export const useImportWorkbenchStore = create<ImportWorkbenchState>((set, get) => ({
  initialized: false,
  loading: false,
  preparing: false,
  preparingTaskId: null,
  submitting: false,
  error: null,
  conversationRef: null,
  draft: { ...EMPTY_IMPORT_WORKBENCH_DRAFT },
  plans: {},
  planPaths: {},
  planErrors: {},
  operations: {},
  operationResults: {},
  operationResultErrors: {},
  operationResultLoading: {},

  initialize: async () => {
    if (get().initialized) return;
    if (initializeInFlight) return initializeInFlight;
    initializeInFlight = (async () => {
      set({ loading: true, error: null });
      try {
        const selectedRef = useConversationStore.getState().activeRef;
        if (selectedRef) {
          await loadConversationProjection(selectedRef);
          return;
        }
        const rememberedRef = readManualConversationRef();
        if (!rememberedRef) {
          set({ initialized: true, loading: false });
          return;
        }
        const conversations = await listConversations();
        if (!conversations.some((item) => item.conversationRef === rememberedRef)) {
          saveManualConversationRef(null);
          set({ initialized: true, loading: false });
          return;
        }
        await loadConversationProjection(rememberedRef);
      } catch (error) {
        set({ loading: false, error: errorMessage(error) });
      } finally {
        initializeInFlight = null;
      }
    })();
    return initializeInFlight;
  },

  switchConversation: async (conversationRef) => {
    if (!conversationRef) {
      projectionRevision += 1;
      set({
        loading: false,
        conversationRef: null,
        draft: { ...EMPTY_IMPORT_WORKBENCH_DRAFT },
        plans: {},
        planPaths: {},
        planErrors: {},
        operations: {},
        operationResults: {},
        operationResultErrors: {},
        operationResultLoading: {},
        error: null,
      });
      return;
    }
    set({ loading: true, error: null });
    try {
      await loadConversationProjection(conversationRef);
    } catch (error) {
      set({ loading: false, error: errorMessage(error) });
    }
  },

  updateOptions: async (collection, renamePattern) => {
    const { conversationRef, draft, loading } = get();
    if (loading || conversationRef !== useConversationStore.getState().activeRef) {
      throw new Error("当前会话正在切换，请等入库工作台加载完成后再修改选项。");
    }
    const next = { ...draft, collection, renamePattern };
    set({ draft: next, error: null });
    if (conversationRef) await persistDraft(conversationRef, next);
  },

  prepareFiles: async (paths, requestedTaskId) => {
    const taskId = requestedTaskId ?? newImportTaskId();
    if (!paths.length) return { plan: null, warnings: [], taskId };
    if (get().preparing) throw new Error("正在准备另一个入库计划，请稍候。");
    set({ preparing: true, preparingTaskId: taskId, error: null });
    try {
      const conversationRef = await preparationOwner();
      const options = ownedDraft(conversationRef);
      const scan = await inspectImport(paths, taskId);
      if (scan.warnings.length) {
        const current = ownedDraft(conversationRef);
        const next = { ...current, warnings: mergeWarnings(current.warnings, scan.warnings) };
        await persistDraft(conversationRef, next);
        if (isActiveWorkbench(conversationRef)) set({ draft: next });
      }
      if (!scan.items.length) return { plan: null, warnings: scan.warnings, taskId };
      if (ownedDraft(conversationRef).pendingPlanRefs.length >= MAX_PENDING_PLANS) {
        throw new Error("待入库计划已达 100 组，请先确认入库或清理草稿。");
      }
      const plan = await prepareImport({
        paths: scan.items.map((item) => item.path),
        collection: options.collection,
        renamePattern: options.renamePattern,
      }, taskId);
      const planPaths = await getImportPlanPreviewPaths(plan.planRef);
      const current = ownedDraft(conversationRef);
      const next = {
        ...current,
        pendingPlanRefs: [...current.pendingPlanRefs, { planRef: plan.planRef }],
      };
      await persistDraft(conversationRef, next);
      if (isActiveWorkbench(conversationRef)) set((state) => ({
        conversationRef,
        draft: next,
        plans: { ...state.plans, [plan.planRef]: plan },
        planPaths: { ...state.planPaths, [plan.planRef]: planPaths },
        preparing: false,
        preparingTaskId: null,
      }));
      return { plan, warnings: scan.warnings, taskId };
    } catch (error) {
      set({ preparing: false, preparingTaskId: null, error: errorMessage(error) });
      throw error;
    } finally {
      set({ preparing: false, preparingTaskId: null });
    }
  },

  prepareDirectory: async (sourceRef) => {
    if (get().preparing) throw new Error("正在准备另一个入库计划，请稍候。");
    if (get().draft.pendingPlanRefs.length >= MAX_PENDING_PLANS) {
      throw new Error("待入库计划已达 100 组，请先确认入库或清理草稿。");
    }
    const taskId = newImportTaskId();
    set({ preparing: true, preparingTaskId: taskId, error: null });
    try {
      const conversationRef = await preparationOwner();
      const draft = ownedDraft(conversationRef);
      const plan = await prepareImport({
        sourceRef,
        collection: draft.collection,
        renamePattern: draft.renamePattern,
      }, taskId);
      const planPaths = await getImportPlanPreviewPaths(plan.planRef);
      const current = ownedDraft(conversationRef);
      const next = {
        ...current,
        pendingPlanRefs: [...current.pendingPlanRefs, { planRef: plan.planRef }],
      };
      await persistDraft(conversationRef, next);
      if (isActiveWorkbench(conversationRef)) set((state) => ({
        conversationRef,
        draft: next,
        plans: { ...state.plans, [plan.planRef]: plan },
        planPaths: { ...state.planPaths, [plan.planRef]: planPaths },
        preparing: false,
        preparingTaskId: null,
      }));
      return plan;
    } catch (error) {
      set({ preparing: false, preparingTaskId: null, error: errorMessage(error) });
      throw error;
    } finally {
      set({ preparing: false, preparingTaskId: null });
    }
  },

  cancelPreparation: async () => {
    const taskId = get().preparingTaskId;
    if (!taskId) return;
    await cancelPrepareImport(taskId);
  },

  removeItem: async (planRef, itemRef) => {
    const plan = get().plans[planRef];
    if (!plan) throw new Error("导入计划已不可用，请重新选择文件。");
    await get().repreparePlan(planRef, {
      collection: plan.targetCollection ?? "",
      renamePattern: plan.renamePattern ?? "",
    }, [itemRef]);
  },

  removeUnsupported: async (planRef) => {
    const plan = get().plans[planRef];
    if (!plan) throw new Error("导入计划已不可用，请重新选择文件。");
    const excludedItemRefs = plan.items
      .filter((item) => item.previewStatus === "unsupported")
      .map((item) => item.itemRef);
    if (!excludedItemRefs.length) return;
    await get().repreparePlan(planRef, {
      collection: plan.targetCollection ?? "",
      renamePattern: plan.renamePattern ?? "",
    }, excludedItemRefs);
  },

  repreparePlan: async (planRef, options, excludedItemRefs = []) => {
    const { draft, plans } = get();
    if (get().preparing) throw new Error("正在准备另一个入库计划，请稍候。");
    const plan = plans[planRef];
    if (!plan) throw new Error("导入计划已不可用，请重新选择文件。");
    if (draft.pendingPlanRefs.find((entry) => entry.planRef === planRef)?.idempotencyKey) {
      throw new Error("该计划正在确认受理，响应确认前不能更改范围。");
    }
    const taskId = newImportTaskId();
    set({ preparing: true, preparingTaskId: taskId, error: null });
    try {
      const ownerRef = await preparationOwner();
      const nextPlan = await prepareImport({
        basePlanRef: planRef,
        excludedItemRefs,
        collection: options?.collection ?? plan.targetCollection,
        renamePattern: options?.renamePattern ?? plan.renamePattern,
      }, taskId);
      const nextPlanPaths = await getImportPlanPreviewPaths(nextPlan.planRef);
      const currentDraft = ownedDraft(ownerRef);
      const nextDraft = {
        ...currentDraft,
        pendingPlanRefs: currentDraft.pendingPlanRefs.map((entry) =>
          entry.planRef === planRef ? { planRef: nextPlan.planRef } : entry,
        ),
      };
      await persistDraft(ownerRef, nextDraft);
      if (isActiveWorkbench(ownerRef)) set((state) => {
        const nextPlans = { ...state.plans, [nextPlan.planRef]: nextPlan };
        delete nextPlans[planRef];
        const planPaths = { ...state.planPaths, [nextPlan.planRef]: nextPlanPaths };
        delete planPaths[planRef];
        const nextPlanErrors = { ...state.planErrors };
        delete nextPlanErrors[planRef];
        return {
          conversationRef: ownerRef,
          draft: nextDraft,
          plans: nextPlans,
          planPaths,
          planErrors: nextPlanErrors,
          preparing: false,
          preparingTaskId: null,
        };
      });
      return nextPlan.planRef;
    } catch (error) {
      set({ preparing: false, preparingTaskId: null, error: errorMessage(error) });
      throw error;
    } finally {
      set({ preparing: false, preparingTaskId: null });
    }
  },

  clearPendingPlans: async () => {
    const { conversationRef, draft } = get();
    if (!conversationRef || !isActiveWorkbench(conversationRef)) {
      throw new Error("当前会话正在切换，请等入库工作台加载完成后再清空草稿。");
    }
    if (draft.pendingPlanRefs.some((entry) => entry.idempotencyKey)) {
      throw new Error("有计划的受理结果尚未确认，请先重试该计划以对账，再清空草稿。");
    }
    const next = { ...draft, pendingPlanRefs: [], warnings: [] };
    await persistDraft(conversationRef, next);
    if (isActiveWorkbench(conversationRef)) set({ draft: next, plans: {}, planPaths: {}, planErrors: {}, error: null });
  },

  submitPlan: async (planRef) => {
    if (get().submitting) throw new Error("正在确认另一个入库计划，请稍候。");
    const ownerRef = await preparationOwner();
    let targetPlanRef = planRef;
    if (!isActiveWorkbench(ownerRef)) throw new Error("会话已切换，请返回原会话确认入库。");
    let entry = ownedDraft(ownerRef).pendingPlanRefs.find((item) => item.planRef === targetPlanRef);
    if (!entry) throw new Error("待入库计划已变化，请刷新后重试。");
    const plan = get().plans[targetPlanRef];
    const options = ownedDraft(ownerRef);
    if (plan && !entry.idempotencyKey && (
      (plan.targetCollection ?? "") !== options.collection
      || (plan.renamePattern ?? "") !== options.renamePattern
    )) {
      targetPlanRef = await get().repreparePlan(targetPlanRef, {
        collection: options.collection,
        renamePattern: options.renamePattern,
      });
      if (!isActiveWorkbench(ownerRef)) throw new Error("会话已切换，计划已保存在原会话，请返回原会话确认。");
      entry = get().draft.pendingPlanRefs.find((item) => item.planRef === targetPlanRef);
      if (!entry) throw new Error("规则已应用，但待入库计划刷新失败，请重试。");
    }
    const idempotencyKey = entry.idempotencyKey ?? `import_submit_${crypto.randomUUID().replaceAll("-", "")}`;
    const keyedDraft = {
      ...ownedDraft(ownerRef),
      pendingPlanRefs: ownedDraft(ownerRef).pendingPlanRefs.map((item) =>
        item.planRef === targetPlanRef ? { planRef: targetPlanRef, idempotencyKey } : item,
      ),
    };
    set({ submitting: true, error: null });
    let submissionAccepted = false;
    try {
      await persistDraft(ownerRef, keyedDraft);
      if (isActiveWorkbench(ownerRef)) set({ draft: keyedDraft });
      const result = await confirmImport(targetPlanRef, idempotencyKey);
      submissionAccepted = true;
      const operation = result.operation;
      await linkConversation(ownerRef, "operation", operation.operationId);
      const currentDraft = ownedDraft(ownerRef);
      const completedDraft = {
        ...currentDraft,
        pendingPlanRefs: currentDraft.pendingPlanRefs.filter((item) => item.planRef !== targetPlanRef),
        recentOperationRefs: [
          operation.operationId,
          ...currentDraft.recentOperationRefs.filter((id) => id !== operation.operationId),
        ].slice(0, MAX_RECENT_OPERATIONS),
      };
      await persistDraft(ownerRef, completedDraft);
      if (isActiveWorkbench(ownerRef)) set((state) => {
        const plans = { ...state.plans };
        delete plans[targetPlanRef];
        const planPaths = { ...state.planPaths };
        delete planPaths[targetPlanRef];
        return {
          draft: completedDraft,
          plans,
          planPaths,
          operations: { ...state.operations, [operation.operationId]: operation },
        };
      });
      trackImportOperation(operation);
      return operation;
    } catch (error) {
      const code = error instanceof AppError ? error.code : null;
      if (!submissionAccepted && code && DEFINITELY_REJECTED_SUBMISSION_CODES.has(code)) {
        const current = ownedDraft(ownerRef);
        const retryableDraft = {
          ...current,
          pendingPlanRefs: current.pendingPlanRefs.map((item) =>
            item.planRef === targetPlanRef ? { planRef: targetPlanRef } : item,
          ),
        };
        try {
          await persistDraft(ownerRef, retryableDraft);
          if (isActiveWorkbench(ownerRef)) set({ draft: retryableDraft, error: errorMessage(error) });
        } catch {
          // Keep the persisted key if it could not be safely cleared.
          set({ error: errorMessage(error) });
        }
      } else {
        set({ error: errorMessage(error) });
      }
      throw error;
    } finally {
      set({ submitting: false });
    }
  },

  handoffPlan: async (planRef) => {
    const ownerRef = get().conversationRef;
    if (!ownerRef) throw new Error("当前没有可交接的入库草稿。");
    if (!isActiveWorkbench(ownerRef)) throw new Error("会话正在切换，请加载完成后再交接。");
    const entry = get().draft.pendingPlanRefs.find((item) => item.planRef === planRef);
    if (!entry) {
      throw new Error("该导入计划已不在当前草稿中。");
    }
    await linkConversation(ownerRef, "plan", planRef);
    if (isActiveWorkbench(ownerRef)) await useConversationStore.getState().select(ownerRef);
  },

  handoffOperation: async (operationRef) => {
    const ownerRef = get().conversationRef;
    if (!ownerRef) throw new Error("当前没有可交接的入库工作区。");
    if (!isActiveWorkbench(ownerRef)) throw new Error("会话正在切换，请加载完成后再交接。");
    if (!get().draft.recentOperationRefs.includes(operationRef)) {
      throw new Error("该操作不属于当前入库工作区。");
    }
    await linkConversation(ownerRef, "operation", operationRef);
    if (isActiveWorkbench(ownerRef)) await useConversationStore.getState().select(ownerRef);
  },

  refreshOperations: async () => {
    const ownerRef = get().conversationRef;
    const refs = get().draft.recentOperationRefs;
    const results = await Promise.all(refs.map(async (operationRef) => {
      try {
        return await getOperation(operationRef);
      } catch {
        return null;
      }
    }));
    const operations: Record<string, OperationSnapshot> = {};
    for (const operation of results) {
      if (!operation) continue;
      operations[operation.operationId] = operation;
      trackImportOperation(operation);
    }
    if (!ownerRef || !isActiveWorkbench(ownerRef)) return;
    set({ operations });
    await Promise.all(Object.values(operations)
      .filter((operation) => operation.state !== "queued" && operation.state !== "running")
      .map((operation) => get().loadOperationResult(operation.operationId, operation.revision)));
  },

  loadOperationResult: async (operationRef, requestedRevision) => {
    const operationRevision = requestedRevision ?? get().operations[operationRef]?.revision ?? 0;
    const loadKey = `${operationRef}\u0000${operationRevision}`;
    const existingLoad = operationResultLoads.get(loadKey);
    if (existingLoad) return existingLoad;
    const latestRevision = operationResultRevisions.get(operationRef);
    if (latestRevision !== undefined && operationRevision < latestRevision) return;
    operationResultRevisions.set(operationRef, operationRevision);

    const load = Promise.resolve().then(async () => {
      if (operationResultRevisions.get(operationRef) === operationRevision) {
        set((state) => {
          const operationResultErrors = { ...state.operationResultErrors };
          delete operationResultErrors[operationRef];
          const operationResults = { ...state.operationResults };
          delete operationResults[operationRef];
          return {
            operationResultErrors,
            operationResults,
            operationResultLoading: { ...state.operationResultLoading, [operationRef]: true },
          };
        });
      }
      try {
        const result = await getImportResult(operationRef);
        if (operationResultRevisions.get(operationRef) === operationRevision) {
          set((state) => {
            const operationResultErrors = { ...state.operationResultErrors };
            delete operationResultErrors[operationRef];
            return {
              operationResults: { ...state.operationResults, [operationRef]: result },
              operationResultErrors,
            };
          });
        }
      } catch (error) {
        if (operationResultRevisions.get(operationRef) === operationRevision) {
          set((state) => ({
            operationResultErrors: { ...state.operationResultErrors, [operationRef]: errorMessage(error) },
          }));
        }
      } finally {
        if (operationResultRevisions.get(operationRef) === operationRevision) {
          set((state) => {
            const operationResultLoading = { ...state.operationResultLoading };
            delete operationResultLoading[operationRef];
            return { operationResultLoading };
          });
        }
        operationResultLoads.delete(loadKey);
      }
    });
    operationResultLoads.set(loadKey, load);
    return load;
  },

  cancel: async (operationRef) => {
    const ownerRef = get().conversationRef;
    const operation = await cancelOperation(operationRef);
    if (ownerRef && isActiveWorkbench(ownerRef)) set((state) => ({ operations: { ...state.operations, [operationRef]: operation } }));
    trackImportOperation(operation);
  },

  resume: async (operationRef) => {
    const ownerRef = get().conversationRef;
    const operation = await resumeOperation(operationRef);
    if (ownerRef && isActiveWorkbench(ownerRef)) set((state) => ({ operations: { ...state.operations, [operationRef]: operation } }));
    trackImportOperation(operation);
  },

  clearError: () => set({ error: null }),
}));
