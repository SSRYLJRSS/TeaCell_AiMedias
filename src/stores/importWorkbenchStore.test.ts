import { beforeEach, describe, expect, it, vi } from "vitest";
import { useImportWorkbenchStore } from "@/stores/importWorkbenchStore";
import { useConversationStore } from "@/stores/conversationStore";
import { AppError } from "@/api/client";
import type { ImportResultView, PreparedImportPlan } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

const mocks = vi.hoisted(() => ({
  inspectImport: vi.fn(),
  cancelPrepareImport: vi.fn(),
  prepareImport: vi.fn(),
  getImportPlan: vi.fn(),
  getImportPlanPreviewPaths: vi.fn(),
  getImportResult: vi.fn(),
  confirmImport: vi.fn(),
  newImportTaskId: vi.fn(),
  getOperation: vi.fn(),
  createConversation: vi.fn(),
  listConversations: vi.fn(),
  getConversation: vi.fn(),
  saveConversationDraft: vi.fn(),
  linkConversation: vi.fn(),
  cancelOperation: vi.fn(),
  resumeOperation: vi.fn(),
  persistedDrafts: new Map<string, Record<string, unknown>>(),
}));

vi.mock("@/api/import", () => ({
  inspectImport: mocks.inspectImport,
  cancelPrepareImport: mocks.cancelPrepareImport,
  prepareImport: mocks.prepareImport,
  getImportPlan: mocks.getImportPlan,
  getImportPlanPreviewPaths: mocks.getImportPlanPreviewPaths,
  getImportResult: mocks.getImportResult,
  confirmImport: mocks.confirmImport,
  newImportTaskId: mocks.newImportTaskId,
}));
vi.mock("@/api/plans", () => ({ getOperation: mocks.getOperation }));
vi.mock("@/api/conversations", () => ({
  createConversation: mocks.createConversation,
  listConversations: mocks.listConversations,
  getConversation: mocks.getConversation,
  saveConversationDraft: mocks.saveConversationDraft,
  linkConversation: mocks.linkConversation,
}));
vi.mock("@/api/operations", () => ({
  cancelOperation: mocks.cancelOperation,
  resumeOperation: mocks.resumeOperation,
}));

function plan(planRef: string, targetCollection = "", renamePattern = ""): PreparedImportPlan {
  return {
    planRef,
    targetCollection,
    renamePattern,
    itemCount: 1,
    readyCount: 1,
    limitedCount: 0,
    unsupportedCount: 0,
    totalSize: 1234,
    expiresAtUnixSeconds: 2_000_000_000,
    items: [{
      itemRef: "item-1",
      displayName: "photo.jpg",
      kind: "image",
      size: 1234,
      modifiedAt: 1,
      previewStatus: "ready",
      renamePreview: "photo.jpg",
      excluded: false,
    }],
  };
}

function operation(state: OperationSnapshot["state"] = "succeeded"): OperationSnapshot {
  return {
    schemaVersion: 1,
    operationId: "op-1",
    library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
    revision: 1,
    state,
    phase: state === "succeeded" ? "complete" : "executing",
    completed: 1,
    failed: 0,
    total: 1,
    canCancel: state === "running",
    canResume: state === "interrupted",
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.persistedDrafts.clear();
  mocks.newImportTaskId.mockReturnValue("task-stage");
  mocks.createConversation.mockResolvedValue({
    conversationRef: "conversation-1",
    title: "手动入库工作台",
    status: "active",
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 1,
  });
  mocks.listConversations.mockResolvedValue([]);
  mocks.getConversation.mockImplementation(async (conversationRef: string) => ({
    conversationRef,
    title: "手动入库工作台",
    status: "active",
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 1,
    attachments: [],
    links: [],
    drafts: mocks.persistedDrafts.has(conversationRef)
      ? [{ kind: "import", payload: mocks.persistedDrafts.get(conversationRef)!, revision: 1, updatedAtUnixSeconds: 1 }]
      : [],
  }));
  mocks.saveConversationDraft.mockImplementation(async (conversationRef: string, _kind: string, payload: Record<string, unknown>) => {
    mocks.persistedDrafts.set(conversationRef, payload);
    return { kind: "import", payload, revision: 1, updatedAtUnixSeconds: 1 };
  });
  mocks.inspectImport.mockResolvedValue({
    items: [{ path: "D:/private/photo.jpg", kind: "image", size: 1234, previewStatus: "ready" }],
    images: 1,
    videos: 0,
    totalSize: 1234,
    warnings: ["D:/private/skipped.jpg: 无法读取; secondary path E:\\private\\secret.jpg"],
  });
  mocks.cancelPrepareImport.mockResolvedValue(undefined);
  mocks.prepareImport.mockResolvedValue(plan("plan-1"));
  mocks.getImportPlan.mockImplementation(async (planRef: string) => plan(planRef));
  mocks.getImportPlanPreviewPaths.mockResolvedValue({ "item-1": "D:/private/photo.jpg" });
  mocks.getImportResult.mockResolvedValue({ sourceStepRef: "step-1", imported: 0, duplicateItems: [], duplicates: 0, failed: 0, failures: [] });
  mocks.getOperation.mockResolvedValue(operation());
  mocks.cancelOperation.mockResolvedValue(operation("cancelled"));
  mocks.resumeOperation.mockResolvedValue(operation("running"));
  mocks.linkConversation.mockResolvedValue(undefined);
  mocks.confirmImport.mockResolvedValue({ operation: operation(), replayed: false });

  useConversationStore.setState({
    conversations: [], activeRef: null, active: null, draftsByConversation: {}, loading: false, error: null,
  });
  useImportWorkbenchStore.setState({
    initialized: false,
    loading: false,
    preparing: false,
    preparingTaskId: null,
    submitting: false,
    error: null,
    conversationRef: null,
    draft: { schemaVersion: 1, collection: "", renamePattern: "", warnings: [], pendingPlanRefs: [], recentOperationRefs: [] },
    plans: {},
    planPaths: {},
    planErrors: {},
    operations: {},
    operationResults: {},
    operationResultErrors: {},
    operationResultLoading: {},
  });
});

describe("importWorkbenchStore", () => {
  it.each(["files", "directory", "reprepare", "submit"] as const)("%s 完成时只更新发起会话的草稿", async (action) => {
    await useConversationStore.getState().select("conversation-a");
    await useImportWorkbenchStore.getState().switchConversation("conversation-a");
    mocks.inspectImport.mockResolvedValue({ items: [{ path: "D:/a.jpg" }], warnings: [] });
    await useImportWorkbenchStore.getState().prepareDirectory("source-a");
    await useImportWorkbenchStore.getState().updateOptions("collection-a", "");
    let finish!: () => void;
    const waiting = new Promise<void>((resolve) => { finish = resolve; });
    if (action === "submit") {
      // Keep submission from implicitly re-preparing before reaching its await.
      useImportWorkbenchStore.setState({ plans: { "plan-1": plan("plan-1", "collection-a") } });
      mocks.confirmImport.mockImplementationOnce(async () => {
        await waiting;
        return { operation: operation(), replayed: false };
      });
    } else {
      mocks.prepareImport.mockImplementationOnce(async () => {
        await waiting;
        return plan("plan-a-next", "collection-a");
      });
    }
    const store = useImportWorkbenchStore.getState();
    const pending = action === "files" ? store.prepareFiles(["D:/a.jpg"])
      : action === "directory" ? store.prepareDirectory("source-a")
        : action === "reprepare" ? store.repreparePlan("plan-1")
          : store.submitPlan("plan-1");
    await vi.waitFor(() => expect(action === "submit" ? mocks.confirmImport : mocks.prepareImport)
      .toHaveBeenCalledTimes(action === "submit" ? 1 : 2));
    await useConversationStore.getState().select("conversation-b");
    await useImportWorkbenchStore.getState().switchConversation("conversation-b");
    await useImportWorkbenchStore.getState().updateOptions("collection-b", "");
    const bDraft = structuredClone(mocks.persistedDrafts.get("conversation-b"));
    finish();
    await pending;
    expect(mocks.persistedDrafts.get("conversation-b")).toEqual(bDraft);
    expect(useImportWorkbenchStore.getState().conversationRef).toBe("conversation-b");
    expect(useImportWorkbenchStore.getState().draft.collection).toBe("collection-b");
    const aDraft = mocks.persistedDrafts.get("conversation-a")!;
    expect(aDraft.collection).toBe("collection-a");
    expect(aDraft.pendingPlanRefs).toEqual(action === "submit" ? []
      : action === "reprepare" ? [{ planRef: "plan-a-next" }]
        : [{ planRef: "plan-1" }, { planRef: "plan-a-next" }]);
    if (action === "submit") expect(aDraft.recentOperationRefs).toEqual(["op-1"]);
  });

  it("并发读取同一操作结果时合并为一个请求", async () => {
    let finishRead!: (result: ImportResultView) => void;
    mocks.getImportResult.mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }));
    const first = useImportWorkbenchStore.getState().loadOperationResult("op-1");
    const second = useImportWorkbenchStore.getState().loadOperationResult("op-1");
    await Promise.resolve();

    expect(mocks.getImportResult).toHaveBeenCalledTimes(1);
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-1"]).toBe(true);
    finishRead({ sourceStepRef: "step-1", imported: 1, duplicateItems: [], duplicates: 0, failed: 0, failures: [] });
    await Promise.all([first, second]);

    expect(useImportWorkbenchStore.getState().operationResultLoading["op-1"]).toBeUndefined();
    expect(useImportWorkbenchStore.getState().operationResults["op-1"].imported).toBe(1);
  });

  it("恢复后的新终态结果不会被旧读取覆盖", async () => {
    let finishOldRead!: (result: ImportResultView) => void;
    const oldResult: ImportResultView = {
      sourceStepRef: "step-1",
      imported: 1,
      duplicateItems: [],
      duplicates: 0,
      failed: 1,
      failures: [],
    };
    const newResult = { sourceStepRef: "step-1", imported: 2, duplicateItems: [], duplicates: 0, failed: 0, failures: [] };
    let readCount = 0;
    mocks.getImportResult.mockImplementation(() => {
      readCount += 1;
      if (readCount === 1) return new Promise((resolve) => { finishOldRead = resolve; });
      return Promise.resolve(newResult);
    });

    const oldRead = useImportWorkbenchStore.getState().loadOperationResult("op-race", 2);
    await Promise.resolve();
    const newRead = useImportWorkbenchStore.getState().loadOperationResult("op-race", 4);
    await Promise.resolve();
    finishOldRead(oldResult);
    await Promise.all([oldRead, newRead]);

    expect(readCount).toBe(2);
    expect(useImportWorkbenchStore.getState().operationResults["op-race"]).toEqual(newResult);
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-race"]).toBeUndefined();
  });

  it("旧 revision 的迟到失败不会清除新结果读取状态", async () => {
    let failOldRead!: (error: Error) => void;
    let finishNewRead!: (result: ImportResultView) => void;
    const newResult = { sourceStepRef: "step-1", imported: 2, duplicateItems: [], duplicates: 0, failed: 0, failures: [] as [] };
    let readCount = 0;
    mocks.getImportResult.mockImplementation(() => {
      readCount += 1;
      if (readCount === 1) {
        return new Promise((_, reject) => { failOldRead = reject; });
      }
      return new Promise((resolve) => { finishNewRead = resolve; });
    });

    const oldRead = useImportWorkbenchStore.getState().loadOperationResult("op-race-failure", 3);
    await Promise.resolve();
    const newRead = useImportWorkbenchStore.getState().loadOperationResult("op-race-failure", 5);
    await Promise.resolve();
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-race-failure"]).toBe(true);

    failOldRead(new Error("旧结果读取失败"));
    await oldRead;
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-race-failure"]).toBe(true);
    expect(useImportWorkbenchStore.getState().operationResultErrors["op-race-failure"]).toBeUndefined();

    finishNewRead(newResult);
    await newRead;
    expect(useImportWorkbenchStore.getState().operationResults["op-race-failure"]).toEqual(newResult);
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-race-failure"]).toBeUndefined();
  });

  it("结果读取失败后保留按操作重试的错误，并在成功重试后清除", async () => {
    const expectedResult = { sourceStepRef: "step-1", imported: 1, duplicateItems: [], duplicates: 0, failed: 0, failures: [] };
    mocks.getImportResult
      .mockRejectedValueOnce(new Error("结果查询暂时失败"))
      .mockResolvedValueOnce(expectedResult);

    await useImportWorkbenchStore.getState().loadOperationResult("op-1");
    expect(useImportWorkbenchStore.getState().operationResultErrors["op-1"]).toBe("结果查询暂时失败");
    expect(useImportWorkbenchStore.getState().operationResultLoading["op-1"]).toBeUndefined();

    await useImportWorkbenchStore.getState().loadOperationResult("op-1");
    expect(useImportWorkbenchStore.getState().operationResultErrors["op-1"]).toBeUndefined();
    expect(useImportWorkbenchStore.getState().operationResults["op-1"]).toEqual(expectedResult);
    expect(mocks.getImportResult).toHaveBeenCalledTimes(2);
  });

  it("读取函数同步抛错后也能清理并重试", async () => {
    mocks.getImportResult
      .mockImplementationOnce(() => { throw new Error("同步读取错误"); })
      .mockResolvedValueOnce({ sourceStepRef: "step-1", imported: 0, duplicateItems: [], duplicates: 1, failed: 0, failures: [] });

    await useImportWorkbenchStore.getState().loadOperationResult("op-1");
    expect(useImportWorkbenchStore.getState().operationResultErrors["op-1"]).toBe("同步读取错误");

    await useImportWorkbenchStore.getState().loadOperationResult("op-1");
    expect(useImportWorkbenchStore.getState().operationResultErrors["op-1"]).toBeUndefined();
    expect(useImportWorkbenchStore.getState().operationResults["op-1"].duplicates).toBe(1);
  });

  it("把候选持久化为计划引用，恢复清单和异常提示时不保存本机路径", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    const state = useImportWorkbenchStore.getState();
    const stored = mocks.persistedDrafts.get("conversation-1")!;

    expect(state.draft.pendingPlanRefs).toEqual([{ planRef: "plan-1" }]);
    expect(state.planPaths["plan-1"]["item-1"]).toBe("D:/private/photo.jpg");
    expect(state.draft.warnings).toEqual(["扫描提示 1：有文件无法读取或已跳过"]);
    expect(JSON.stringify(stored)).not.toContain("D:/private");

    await state.switchConversation(null);
    await useImportWorkbenchStore.getState().switchConversation("conversation-1");
    expect(useImportWorkbenchStore.getState().plans["plan-1"].items[0].displayName).toBe("photo.jpg");
    expect(useImportWorkbenchStore.getState().planPaths["plan-1"]["item-1"]).toBe("D:/private/photo.jpg");
  });

  it("慢速加载旧会话投影时不反选覆盖较新的活动会话", async () => {
    mocks.persistedDrafts.set("conversation-b", {
      schemaVersion: 1,
      collection: "",
      renamePattern: "",
      warnings: [],
      pendingPlanRefs: [{ planRef: "plan-b" }],
      recentOperationRefs: [],
    });
    await useConversationStore.getState().select("conversation-b");

    let finishPlan!: (value: PreparedImportPlan) => void;
    mocks.getImportPlan.mockImplementationOnce(() => new Promise((resolve) => { finishPlan = resolve; }));
    const conversationReadCount = mocks.getConversation.mock.calls.length;
    const loadingB = useImportWorkbenchStore.getState().switchConversation("conversation-b");
    await Promise.resolve();
    expect(useImportWorkbenchStore.getState().loading).toBe(true);
    expect(mocks.getConversation).toHaveBeenCalledTimes(conversationReadCount);

    await useConversationStore.getState().select("conversation-c");
    finishPlan(plan("plan-b"));
    await loadingB;

    expect(useConversationStore.getState().activeRef).toBe("conversation-c");
    expect(useImportWorkbenchStore.getState().loading).toBe(false);
    await useImportWorkbenchStore.getState().switchConversation("conversation-c");
    expect(useImportWorkbenchStore.getState().conversationRef).toBe("conversation-c");
    expect(useConversationStore.getState().activeRef).toBe("conversation-c");
  });

  it("首次初始化被会话切换打断后释放加载态让页面跟进", async () => {
    mocks.persistedDrafts.set("conversation-a", {
      schemaVersion: 1, collection: "A", renamePattern: "", warnings: [],
      pendingPlanRefs: [{ planRef: "plan-a" }], recentOperationRefs: [],
    });
    await useConversationStore.getState().select("conversation-a");
    let finishPlan!: (value: PreparedImportPlan) => void;
    mocks.getImportPlan.mockImplementationOnce(() => new Promise((resolve) => { finishPlan = resolve; }));
    const initializing = useImportWorkbenchStore.getState().initialize();
    await vi.waitFor(() => expect(finishPlan).toBeDefined());
    await useConversationStore.getState().select("conversation-b");
    finishPlan(plan("plan-a"));
    await initializing;
    expect(useImportWorkbenchStore.getState().initialized).toBe(true);
    expect(useImportWorkbenchStore.getState().loading).toBe(false);
    await useImportWorkbenchStore.getState().switchConversation("conversation-b");
    expect(useImportWorkbenchStore.getState().conversationRef).toBe("conversation-b");
  });

  it("文件扫描期间切换会话后仍使用原选项并只保存原会话提示", async () => {
    await useConversationStore.getState().select("conversation-a");
    await useImportWorkbenchStore.getState().switchConversation("conversation-a");
    await useImportWorkbenchStore.getState().updateOptions("A", "{原名}");
    let finishScan!: () => void;
    mocks.inspectImport.mockImplementationOnce(async () => {
      await new Promise<void>((resolve) => { finishScan = resolve; });
      return { items: [{ path: "D:/a.jpg" }], warnings: ["private path"] };
    });
    const preparing = useImportWorkbenchStore.getState().prepareFiles(["D:/a.jpg"]);
    await vi.waitFor(() => expect(finishScan).toBeDefined());
    await useConversationStore.getState().select("conversation-b");
    await useImportWorkbenchStore.getState().switchConversation("conversation-b");
    await useImportWorkbenchStore.getState().updateOptions("B", "");
    finishScan();
    await preparing;
    expect(mocks.prepareImport).toHaveBeenCalledWith(expect.objectContaining({ collection: "A", renamePattern: "{原名}" }), "task-stage");
    expect(mocks.persistedDrafts.get("conversation-a")?.warnings).toHaveLength(1);
    expect(mocks.persistedDrafts.get("conversation-b")?.warnings).toEqual([]);
    expect(useImportWorkbenchStore.getState().conversationRef).toBe("conversation-b");
  });

  it("会话不匹配或投影加载期间拒绝改写旧会话选项", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    mocks.saveConversationDraft.mockClear();

    useConversationStore.setState({ activeRef: "conversation-b" });
    await expect(useImportWorkbenchStore.getState().updateOptions("错写分类", "{原名}"))
      .rejects.toThrow("当前会话正在切换");
    expect(mocks.saveConversationDraft).not.toHaveBeenCalled();
    expect(useImportWorkbenchStore.getState().draft.collection).toBe("");

    useConversationStore.setState({ activeRef: "conversation-1" });
    useImportWorkbenchStore.setState({ loading: true });
    await expect(useImportWorkbenchStore.getState().updateOptions("仍不可写", "{原名}"))
      .rejects.toThrow("当前会话正在切换");
    expect(mocks.saveConversationDraft).not.toHaveBeenCalled();
    expect(useImportWorkbenchStore.getState().draft.collection).toBe("");
  });

  it("恢复旧草稿时也净化并回写含路径的警告", async () => {
    mocks.persistedDrafts.set("conversation-1", {
      schemaVersion: 1,
      collection: "",
      renamePattern: "",
      warnings: ["D:/private/skipped.jpg: failed at E:\\private\\secondary.jpg"],
      pendingPlanRefs: [],
      recentOperationRefs: [],
    });

    await useImportWorkbenchStore.getState().switchConversation("conversation-1");

    const saved = mocks.persistedDrafts.get("conversation-1")!;
    expect(JSON.stringify(saved)).not.toContain("D:/private");
    expect(JSON.stringify(saved)).not.toContain("E:\\private");
    expect((saved.warnings as string[])[0]).toBe("扫描提示 1：有文件无法读取或已跳过");
  });

  it("并发拖入时只接受一个正在准备的计划", async () => {
    let finishInspection!: (result: Awaited<ReturnType<typeof mocks.inspectImport>>) => void;
    mocks.inspectImport.mockImplementationOnce(() => new Promise((resolve) => { finishInspection = resolve; }));

    const first = useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    await expect(useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]))
      .rejects.toThrow("正在准备另一个入库计划");
    expect(mocks.inspectImport).toHaveBeenCalledTimes(1);
    finishInspection({
      items: [{ path: "D:/private/photo.jpg", kind: "image", size: 1234, previewStatus: "ready" }],
      images: 1,
      videos: 0,
      totalSize: 1234,
      warnings: [],
    });
    await first;
    expect(mocks.prepareImport).toHaveBeenCalledTimes(1);
  });

  it("取消准备只取消当前扫描任务", async () => {
    useImportWorkbenchStore.setState({ preparing: true, preparingTaskId: "task-exact" });
    await useImportWorkbenchStore.getState().cancelPreparation();
    expect(mocks.cancelPrepareImport).toHaveBeenCalledWith("task-exact");
  });

  it("确认响应丢失后用草稿中已持久化的同一幂等键重试", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    mocks.confirmImport
      .mockRejectedValueOnce(new Error("响应超时"))
      .mockResolvedValueOnce({ operation: operation(), replayed: true });

    await expect(useImportWorkbenchStore.getState().submitPlan("plan-1")).rejects.toThrow("响应超时");
    const persistedKey = (mocks.persistedDrafts.get("conversation-1")!.pendingPlanRefs as Array<{ idempotencyKey: string }>)[0].idempotencyKey;
    await useImportWorkbenchStore.getState().submitPlan("plan-1");

    expect(persistedKey).toMatch(/^import_submit_[a-f0-9]+$/);
    expect(mocks.confirmImport).toHaveBeenNthCalledWith(1, "plan-1", persistedKey);
    expect(mocks.confirmImport).toHaveBeenNthCalledWith(2, "plan-1", persistedKey);
    expect(useImportWorkbenchStore.getState().draft.recentOperationRefs).toEqual(["op-1"]);
  });

  it("明确的计划拒绝会释放幂等键，允许重新准备后提交", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    mocks.confirmImport.mockRejectedValueOnce(new AppError("PLAN_CHANGED", "计划已变化"));

    await expect(useImportWorkbenchStore.getState().submitPlan("plan-1")).rejects.toThrow("计划已变化");
    expect(mocks.persistedDrafts.get("conversation-1")!.pendingPlanRefs).toEqual([{ planRef: "plan-1" }]);
    expect(mocks.confirmImport).toHaveBeenCalledTimes(1);
  });

  it("幂等键与其他计划冲突属于明确未受理，可释放当前键", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    mocks.confirmImport.mockRejectedValueOnce(new AppError("IDEMPOTENCY_CONFLICT", "幂等键已被其他计划使用"));

    await expect(useImportWorkbenchStore.getState().submitPlan("plan-1")).rejects.toThrow("幂等键已被其他计划使用");
    expect(mocks.persistedDrafts.get("conversation-1")!.pendingPlanRefs).toEqual([{ planRef: "plan-1" }]);
    expect(mocks.confirmImport).toHaveBeenCalledTimes(1);
  });

  it("受理成功后的关联错误保留原幂等键供对账重试", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    mocks.confirmImport
      .mockResolvedValueOnce({ operation: operation(), replayed: false })
      .mockResolvedValueOnce({ operation: operation(), replayed: true });
    mocks.linkConversation
      .mockRejectedValueOnce(new AppError("NOT_FOUND", "关联暂时失败"))
      .mockResolvedValueOnce(undefined);

    await expect(useImportWorkbenchStore.getState().submitPlan("plan-1")).rejects.toThrow("关联暂时失败");
    const stored = mocks.persistedDrafts.get("conversation-1")!.pendingPlanRefs as Array<{ idempotencyKey: string }>;
    const key = stored[0].idempotencyKey;
    expect(key).toMatch(/^import_submit_[a-f0-9]+$/);
    await useImportWorkbenchStore.getState().submitPlan("plan-1");

    expect(mocks.confirmImport).toHaveBeenNthCalledWith(1, "plan-1", key);
    expect(mocks.confirmImport).toHaveBeenNthCalledWith(2, "plan-1", key);
  });

  it("交接待确认计划只建立引用，不提交或启动入库", async () => {
    await useImportWorkbenchStore.getState().prepareFiles(["D:/private/photo.jpg"]);
    await useImportWorkbenchStore.getState().updateOptions("新分类", "{name}");
    mocks.prepareImport.mockClear();
    await useImportWorkbenchStore.getState().handoffPlan("plan-1");

    expect(mocks.linkConversation).toHaveBeenCalledWith("conversation-1", "plan", "plan-1");
    expect(mocks.confirmImport).not.toHaveBeenCalled();
    expect(mocks.prepareImport).not.toHaveBeenCalled();
  });

  it("取消只调用用户指定的 operationRef", async () => {
    await useImportWorkbenchStore.getState().cancel("op-specific");
    expect(mocks.cancelOperation).toHaveBeenCalledTimes(1);
    expect(mocks.cancelOperation).toHaveBeenCalledWith("op-specific");
  });
});
