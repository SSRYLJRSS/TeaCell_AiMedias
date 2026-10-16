import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import ImportPage from "@/pages/ImportPage";
import { useSettingsStore } from "@/stores/settingsStore";
import { trackImportOperation, upsertImport, useTaskStore } from "@/stores/taskStore";
import { useImportWorkbenchStore } from "@/stores/importWorkbenchStore";
import { useConversationStore } from "@/stores/conversationStore";
import type { PreparedImportPlan } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

const mocks = vi.hoisted(() => ({
  open: vi.fn(),
  newImportTaskId: vi.fn(() => "task-ui"),
  openFileExternal: vi.fn(),
  getImportResult: vi.fn().mockResolvedValue({ imported: 0, duplicates: 0, failed: 0, failures: [] }),
  renderNamePreview: vi.fn(),
  pickDirectoryGrant: vi.fn(),
  registerImportDropHandler: vi.fn(() => vi.fn()),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open }));
vi.mock("@/api/authorization", () => ({ pickDirectoryGrant: mocks.pickDirectoryGrant }));
vi.mock("@/utils/importDropDispatcher", () => ({ registerImportDropHandler: mocks.registerImportDropHandler }));
vi.mock("@/api/import", () => ({
  newImportTaskId: mocks.newImportTaskId,
  openFileExternal: mocks.openFileExternal,
  getImportResult: mocks.getImportResult,
  renderNamePreview: mocks.renderNamePreview,
  inspectImport: vi.fn(),
  onImportProgress: vi.fn().mockResolvedValue(() => {}),
  prepareImport: vi.fn(),
  getImportPlan: vi.fn(),
  getImportPlanPreviewPaths: vi.fn(),
  confirmImport: vi.fn(),
  cancelPrepareImport: vi.fn(),
  verifyImportPlan: vi.fn(),
}));
vi.mock("@/api/plans", () => ({ getOperation: vi.fn() }));
vi.mock("@/api/operations", () => ({ cancelOperation: vi.fn(), resumeOperation: vi.fn() }));
vi.mock("@/api/conversations", () => ({
  createConversation: vi.fn(), listConversations: vi.fn(), getConversation: vi.fn(),
  saveConversationDraft: vi.fn(), linkConversation: vi.fn(),
}));
vi.mock("@/api/preview", () => ({ getPreviewUrl: vi.fn().mockResolvedValue(null) }));

function preparedPlan(previewStatus: "ready" | "limited" | "unsupported" = "ready"): PreparedImportPlan {
  return {
    planRef: "plan-ui",
    itemCount: 1,
    readyCount: previewStatus === "ready" ? 1 : 0,
    limitedCount: previewStatus === "limited" ? 1 : 0,
    unsupportedCount: previewStatus === "unsupported" ? 1 : 0,
    totalSize: 2048,
    expiresAtUnixSeconds: 2_000_000_000,
    items: [{
      itemRef: "item-ui",
      displayName: "sample.jpg",
      kind: "image",
      size: 2048,
      modifiedAt: 1,
      previewStatus,
      previewMessage: previewStatus === "unsupported" ? "格式不支持" : undefined,
      renamePreview: "sample.jpg",
      excluded: false,
    }],
  };
}

function runningOperation(): OperationSnapshot {
  return {
    schemaVersion: 1,
    operationId: "op-ui-exact",
    library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
    revision: 1,
    state: "running",
    phase: "executing",
    completed: 0,
    failed: 0,
    total: 1,
    canCancel: true,
    canResume: false,
  };
}

const originalLoadSettings = useSettingsStore.getState().load;
let actions: {
  initialize: ReturnType<typeof vi.fn>;
  switchConversation: ReturnType<typeof vi.fn>;
  refreshOperations: ReturnType<typeof vi.fn>;
  loadOperationResult: ReturnType<typeof vi.fn>;
  prepareFiles: ReturnType<typeof vi.fn>;
  prepareDirectory: ReturnType<typeof vi.fn>;
  updateOptions: ReturnType<typeof vi.fn>;
  removeItem: ReturnType<typeof vi.fn>;
  removeUnsupported: ReturnType<typeof vi.fn>;
  clearPendingPlans: ReturnType<typeof vi.fn>;
  submitPlan: ReturnType<typeof vi.fn>;
  handoffPlan: ReturnType<typeof vi.fn>;
  handoffOperation: ReturnType<typeof vi.fn>;
  cancel: ReturnType<typeof vi.fn>;
  resume: ReturnType<typeof vi.fn>;
  clearError: ReturnType<typeof vi.fn>;
};

function resetWorkbench(plan?: PreparedImportPlan, operation?: OperationSnapshot) {
  actions = {
    initialize: vi.fn().mockResolvedValue(undefined),
    switchConversation: vi.fn().mockResolvedValue(undefined),
    refreshOperations: vi.fn().mockResolvedValue(undefined),
    loadOperationResult: vi.fn().mockResolvedValue(undefined),
    prepareFiles: vi.fn().mockResolvedValue({ plan: null, warnings: [], taskId: "task-ui" }),
    prepareDirectory: vi.fn().mockResolvedValue(plan ?? preparedPlan()),
    updateOptions: vi.fn().mockResolvedValue(undefined),
    removeItem: vi.fn().mockResolvedValue(undefined),
    removeUnsupported: vi.fn().mockResolvedValue(undefined),
    clearPendingPlans: vi.fn().mockResolvedValue(undefined),
    submitPlan: vi.fn().mockResolvedValue(operation ?? runningOperation()),
    handoffPlan: vi.fn().mockResolvedValue(undefined),
    handoffOperation: vi.fn().mockResolvedValue(undefined),
    cancel: vi.fn().mockResolvedValue(undefined),
    resume: vi.fn().mockResolvedValue(undefined),
    clearError: vi.fn(),
  };
  useImportWorkbenchStore.setState({
    initialized: true,
    loading: false,
    preparing: false,
    submitting: false,
    error: null,
    conversationRef: "conversation-ui",
    draft: {
      schemaVersion: 1,
      collection: "",
      renamePattern: "",
      warnings: [],
      pendingPlanRefs: plan ? [{ planRef: plan.planRef }] : [],
      recentOperationRefs: operation ? [operation.operationId] : [],
    },
    plans: plan ? { [plan.planRef]: plan } : {},
    planPaths: plan ? { [plan.planRef]: { [plan.items[0].itemRef]: "D:/sample.jpg" } } : {},
    planErrors: {},
    operations: operation ? { [operation.operationId]: operation } : {},
    operationResults: {},
    operationResultErrors: {},
    operationResultLoading: {},
    ...actions,
  } as never);
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.open.mockResolvedValue(null);
  mocks.pickDirectoryGrant.mockResolvedValue(null);
  useSettingsStore.setState({
    load: originalLoadSettings,
    loaded: true,
    loading: false,
    loadError: null,
    settings: { libraryRoot: "D:/library" } as never,
  });
  useTaskStore.setState({ tasks: [], operationSnapshots: {} });
  useConversationStore.setState({
    conversations: [],
    activeRef: "conversation-ui",
    active: null,
    draftsByConversation: {},
    loading: false,
    error: null,
  });
  resetWorkbench();
});

describe("ImportPage workbench", () => {
  it("保留空库引导和文件选择入口", () => {
    render(<ImportPage />);
    expect(screen.getByText("把图片 / 视频拖到这里")).toBeInTheDocument();
    expect(screen.getByText("选择文件…")).toBeInTheDocument();
    expect(screen.getByText("选择文件夹…")).toBeInTheDocument();
    expect(mocks.registerImportDropHandler).toHaveBeenCalledTimes(1);
  });

  it("页面重开或当前会话切换后加载对应入库草稿", async () => {
    useConversationStore.setState({ activeRef: "conversation-next" });
    render(<ImportPage />);

    await waitFor(() => expect(actions.switchConversation).toHaveBeenCalledWith("conversation-next"));

    act(() => useConversationStore.setState({ activeRef: "conversation-third" }));
    await waitFor(() => expect(actions.switchConversation).toHaveBeenLastCalledWith("conversation-third"));
  });

  it("正在加载会话时如果活动会话继续变化，会跟进最新选择", async () => {
    let finishLoadingB!: () => void;
    actions.switchConversation.mockImplementationOnce(() => {
      useImportWorkbenchStore.setState({ loading: true });
      return new Promise<void>((resolve) => { finishLoadingB = resolve; });
    });
    render(<ImportPage />);

    act(() => useConversationStore.setState({ activeRef: "conversation-b" }));
    await waitFor(() => expect(actions.switchConversation).toHaveBeenCalledWith("conversation-b"));
    act(() => useConversationStore.setState({ activeRef: "conversation-c" }));
    expect(actions.switchConversation).toHaveBeenCalledTimes(1);

    await act(async () => {
      finishLoadingB();
      await Promise.resolve();
      useImportWorkbenchStore.setState({ loading: false, conversationRef: "conversation-b" });
    });
    await waitFor(() => expect(actions.switchConversation).toHaveBeenLastCalledWith("conversation-c"));
  });

  it("会话投影加载期间禁用分库和改名编辑", () => {
    useConversationStore.setState({ activeRef: "conversation-next" });
    render(<ImportPage />);

    expect(screen.getByPlaceholderText("如：旅行")).toBeDisabled();
    expect(screen.getByRole("button", { name: "分库" })).toBeDisabled();
  });

  it("选择文件后只准备固定草稿计划", async () => {
    mocks.open.mockResolvedValue(["D:/incoming/a.jpg"]);
    const plan = preparedPlan();
    actions.prepareFiles.mockResolvedValue({ plan, warnings: [], taskId: "task-ui" });
    render(<ImportPage />);

    fireEvent.click(screen.getByText("选择文件…"));

    await waitFor(() => expect(actions.prepareFiles).toHaveBeenCalledWith(["D:/incoming/a.jpg"], "task-ui"));
    expect(screen.queryByText("开始入库（1）")).toBeNull();
  });

  it("文件扫描结束后显示最近 operation 进度，不残留扫描任务选择", async () => {
    const completed = { ...runningOperation(), state: "succeeded" as const, phase: "complete" as const, completed: 1, canCancel: false };
    resetWorkbench(undefined, completed);
    useTaskStore.setState({ tasks: [{
      id: completed.operationId,
      kind: "import",
      label: "入库",
      overall: 1,
      detail: "入库完成",
      done: true,
      operationSnapshot: completed,
    }] });
    mocks.open.mockResolvedValue(["D:/incoming/new.jpg"]);
    actions.prepareFiles.mockResolvedValue({ plan: preparedPlan(), warnings: [], taskId: "task-ui" });
    render(<ImportPage />);

    fireEvent.click(screen.getByText("选择文件…"));

    await waitFor(() => expect(screen.getByText("已处理 1/1 · 失败 0")).toBeInTheDocument());
  });

  it("新任务替换全局完成条后仍用 operation 的终态快照解除工作台忙碌", () => {
    const completed = {
      ...runningOperation(),
      state: "succeeded" as const,
      phase: "complete" as const,
      completed: 1,
      canCancel: false,
    };
    resetWorkbench(undefined, runningOperation());
    trackImportOperation(completed);
    upsertImport({
      taskId: "new-import-task",
      operationId: "op-new-import",
      phase: "processing",
      phaseCurrent: 1,
      phaseTotal: 1,
      imported: 0,
      duplicates: 0,
      failed: 0,
    });

    render(<ImportPage />);

    expect(screen.getByLabelText("最近入库操作").textContent).toContain("已完成 · 已处理 1/1 · 失败 0");
    expect(screen.queryByRole("button", { name: "正在处理…" })).toBeNull();
  });

  it("同一 operation 恢复后再次进入终态会重新加载结果", async () => {
    const completed = {
      ...runningOperation(),
      state: "interrupted" as const,
      phase: "complete" as const,
      canCancel: false,
      canResume: true,
    };
    resetWorkbench(undefined, completed);
    useTaskStore.setState({ operationSnapshots: { [completed.operationId]: completed } });
    render(<ImportPage />);

    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(1));
    const resumed = {
      ...completed,
      revision: completed.revision + 1,
      state: "running" as const,
      phase: "executing" as const,
      canResume: false,
    };
    const finishedAgain = {
      ...resumed,
      revision: resumed.revision + 1,
      state: "succeeded" as const,
      phase: "complete" as const,
    };

    act(() => useTaskStore.setState({ operationSnapshots: { [resumed.operationId]: resumed } }));
    act(() => useTaskStore.setState({ operationSnapshots: { [finishedAgain.operationId]: finishedAgain } }));

    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(2));
    expect(actions.loadOperationResult).toHaveBeenLastCalledWith(completed.operationId, finishedAgain.revision);
  });

  it("源预览不可恢复时仍允许用原幂等键核对受理结果", async () => {
    resetWorkbench();
    useImportWorkbenchStore.setState({
      draft: {
        schemaVersion: 1,
        collection: "",
        renamePattern: "",
        warnings: [],
        pendingPlanRefs: [{ planRef: "plan-uncertain", idempotencyKey: "import_submit_original" }],
        recentOperationRefs: [],
      },
      planErrors: { "plan-uncertain": "原始文件或授权不可用，请重新选择来源。" },
    });
    render(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "重试并核对受理" }));

    await waitFor(() => expect(actions.submitPlan).toHaveBeenCalledWith("plan-uncertain"));
  });

  it("明确提交待入库计划，交接动作不调用提交", async () => {
    const plan = preparedPlan();
    resetWorkbench(plan);
    render(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "交给助手（待确认）" }));
    await waitFor(() => expect(actions.handoffPlan).toHaveBeenCalledWith("plan-ui"));
    expect(actions.submitPlan).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "开始入库（1）" }));
    await waitFor(() => expect(actions.submitPlan).toHaveBeenCalledWith("plan-ui"));
  });

  it("恢复草稿时要求剔除不支持项后才能提交", async () => {
    const plan = preparedPlan("unsupported");
    resetWorkbench(plan);
    render(<ImportPage />);

    expect(await screen.findByRole("dialog", { name: "有文件无法生成缩略图" })).toBeInTheDocument();
    expect(screen.getAllByText("sample.jpg").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "开始入库（1）" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "剔除并保留可导入项" }));
    await waitFor(() => expect(actions.removeUnsupported).toHaveBeenCalledWith("plan-ui"));
  });

  it("取消按钮携带当前操作的精确 ID", async () => {
    const plan = preparedPlan();
    const operation = runningOperation();
    resetWorkbench(plan, operation);
    render(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "取消入库" }));
    await waitFor(() => expect(actions.cancel).toHaveBeenCalledWith("op-ui-exact"));
  });

  it("恢复已完成操作的异常文件名与结构化错误码", async () => {
    const plan = preparedPlan();
    const operation = { ...runningOperation(), state: "partially_succeeded" as const, phase: "complete" as const, failed: 1, canCancel: false };
    resetWorkbench(plan, operation);
    useImportWorkbenchStore.setState({ operationResults: {
      [operation.operationId]: {
        imported: 1,
        duplicates: 2,
        failed: 1,
        failures: [{ itemRef: "failed-item", displayName: "broken.jpg", state: "failed", errorCode: "FILE_UNREADABLE" }],
      },
    } });
    render(<ImportPage />);

    expect(screen.getByText("新入库 1 · 重复 2 · 失败 1")).toBeInTheDocument();
    fireEvent.click(screen.getByText(/查看异常明细/));
    expect(screen.getByText(/broken.jpg · 失败 · FILE_UNREADABLE/)).toBeInTheDocument();
  });

  it("结果明细读取失败时提供精确操作的重试入口", async () => {
    const operation = { ...runningOperation(), state: "succeeded" as const, phase: "complete" as const, canCancel: false };
    resetWorkbench(undefined, operation);
    useImportWorkbenchStore.setState({ operationResultErrors: { [operation.operationId]: "结果查询暂时失败" } });
    render(<ImportPage />);

    expect(screen.getByRole("alert")).toHaveTextContent("结果明细读取失败：结果查询暂时失败");
    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "重试读取结果" }));
    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(2));
    expect(actions.loadOperationResult).toHaveBeenLastCalledWith(operation.operationId, operation.revision);
  });

  it("未设置素材总库时提示去设置且不触发文件选择器", async () => {
    useSettingsStore.setState({ settings: { libraryRoot: "" } as never });
    render(<ImportPage />);
    fireEvent.click(screen.getByRole("button", { name: "前往设置" }));
    fireEvent.click(screen.getByText("选择文件…"));
    expect(screen.getAllByText(/请先设置素材总库位置/).length).toBeGreaterThan(0);
    expect(mocks.open).not.toHaveBeenCalled();
  });
});
