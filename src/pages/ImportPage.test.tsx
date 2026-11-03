import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import ImportPage from "@/pages/ImportPage";
import { useSettingsStore } from "@/stores/settingsStore";
import { trackImportOperation, upsertImport, useTaskStore } from "@/stores/taskStore";
import { useImportWorkbenchStore } from "@/stores/importWorkbenchStore";
import { useConversationStore } from "@/stores/conversationStore";
import type { PreparedImportPlan } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

import { actRender } from "@/test/render";

/** 把编辑设置触发的异步回写收进 act，避免「not wrapped in act」告警。 */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const mocks = vi.hoisted(() => ({
  open: vi.fn(),
  newImportTaskId: vi.fn(() => "task-ui"),
  openFileExternal: vi.fn(),
  getImportResult: vi.fn().mockResolvedValue({ sourceStepRef: "step-1", imported: 0, duplicateItems: [], duplicates: 0, failed: 0, failures: [] }),
  renderNamePreview: vi.fn(),
  pickDirectoryGrant: vi.fn(),
  listDirectoryGrants: vi.fn(),
  reissueDirectoryGrant: vi.fn(),
  registerImportDropHandler: vi.fn(() => vi.fn()),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open }));
vi.mock("@/api/authorization", () => ({
  pickDirectoryGrant: mocks.pickDirectoryGrant,
  listDirectoryGrants: mocks.listDirectoryGrants,
  reissueDirectoryGrant: mocks.reissueDirectoryGrant,
}));
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
/** 草稿 store 的真实 updateOptions；R22 用它证明设置改动确实落在同一份草稿上。 */
const originalUpdateOptions = useImportWorkbenchStore.getState().updateOptions;
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
  mocks.listDirectoryGrants.mockResolvedValue([]);
  mocks.reissueDirectoryGrant.mockResolvedValue(null);
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
  it("保留空库引导和文件选择入口", async () => {
    await actRender(<ImportPage />);
    expect(screen.getByText("把图片 / 视频拖到这里")).toBeInTheDocument();
    expect(screen.getByText("选择文件…")).toBeInTheDocument();
    expect(screen.getByText("选择文件夹…")).toBeInTheDocument();
    expect(mocks.registerImportDropHandler).toHaveBeenCalledTimes(1);
  });

  /**
   * M-11 真机补证（960×640，真实安装包）：收起态悬浮入口停在左下，压在左栏的「入库进度」
   * 区块上（「入库完成 / 已处理 7/7」被盖住）。进度读数被盖住就失去意义。安全区放在左栏
   * 容器上而不是进度区块的可视内容里，避免内边距把读数本身挤进被遮挡区域。
   *
   * 主区已用 pb-14 让开底栏，所以这里只需再让开悬浮入口本身（48px 圆钮 + 8px 间距）；
   * 原来的 pb-32 把底栏算了两遍，进度块被顶到半空。
   */
  it("左栏容器留出悬浮入口安全区，入库进度不被入口压住（M-11 最小窗口）", async () => {
    await actRender(<ImportPage />);
    const panel = screen.getByTestId("import-side-panel");
    expect(panel.className).toContain("pb-16");
    expect(panel.className).not.toContain("pb-32");
    // 进度区块仍要存在且是左栏的子节点——安全区不能靠隐藏进度块来「解决」遮挡。
    const progress = screen.getByTestId("import-progress-panel");
    expect(panel.contains(progress)).toBe(true);
  });

  // R19：拖入先作附件——不预检、不入库、不外发，点附件旁「导入」才进入检查清单。
  describe("R19 拖入先作附件", () => {
    // 页面挂载时把自己的处理器登记给全局拖入分发器；mock 的签名是 () => void，
    // 真实签名收 payload，所以这里显式按真实契约取参数，避免用 as 糊过去。
    function dropHandler(): (payload: unknown) => void {
      const calls = mocks.registerImportDropHandler.mock.calls as unknown as Array<[(payload: unknown) => void]>;
      const call = calls.at(-1);
      expect(call).toBeDefined();
      return call![0];
    }

    it("拖入只留下附件条目，不触发预检也不入库", async () => {
      await actRender(<ImportPage />);

      act(() => dropHandler()({ type: "drop", paths: ["D:/照片/a.jpg", "D:/照片/b.mp4"] }));

      const panel = await screen.findByRole("region", { name: "待导入附件" });
      expect(panel).toHaveTextContent("已拖入 2 个附件");
      expect(screen.getByText("a.jpg")).toBeInTheDocument();
      expect(screen.getByText("b.mp4")).toBeInTheDocument();
      // 关键负向：拖入本身既没准备计划，也没提交任何操作。
      expect(actions.prepareFiles).not.toHaveBeenCalled();
      expect(actions.prepareDirectory).not.toHaveBeenCalled();
      expect(actions.submitPlan).not.toHaveBeenCalled();
      // 清单仍是空的，用户没有被动进入入库流程。
      expect(screen.getByText("把图片 / 视频拖到这里")).toBeInTheDocument();
    });

    it("点附件旁「导入」才进入预检，并只把这一项送去检查", async () => {
      await actRender(<ImportPage />);
      act(() => dropHandler()({ type: "drop", paths: ["D:/照片/a.jpg", "D:/照片/b.jpg"] }));

      const importButtons = await screen.findAllByRole("button", { name: "导入" });
      expect(importButtons).toHaveLength(2);
      fireEvent.click(importButtons[1]);

      await waitFor(() => expect(actions.prepareFiles).toHaveBeenCalledTimes(1));
      expect(actions.prepareFiles.mock.calls[0][0]).toEqual(["D:/照片/b.jpg"]);
      // 送进预检的那一项从附件里移除，剩下没点的还在等用户决定。
      await waitFor(() => expect(screen.queryByText("b.jpg")).not.toBeInTheDocument());
      expect(screen.getByText("a.jpg")).toBeInTheDocument();
      expect(actions.submitPlan).not.toHaveBeenCalled();
    });

    it("「移除」把附件丢掉，既不入库也不预检", async () => {
      await actRender(<ImportPage />);
      act(() => dropHandler()({ type: "drop", paths: ["D:/照片/a.jpg"] }));

      fireEvent.click(await screen.findByRole("button", { name: "移除" }));

      await waitFor(() => expect(screen.queryByRole("region", { name: "待导入附件" })).not.toBeInTheDocument());
      expect(actions.prepareFiles).not.toHaveBeenCalled();
      expect(actions.submitPlan).not.toHaveBeenCalled();
    });

    it("重复拖入同一路径不会重复列出", async () => {
      await actRender(<ImportPage />);
      act(() => dropHandler()({ type: "drop", paths: ["D:/照片/a.jpg"] }));
      act(() => dropHandler()({ type: "drop", paths: ["D:/照片/a.jpg", "D:/照片/c.jpg"] }));

      expect(await screen.findByText("已拖入 2 个附件；还没检查、也没入库，点「导入」才开始检查。")).toBeInTheDocument();
      expect(screen.getAllByRole("button", { name: "导入" })).toHaveLength(2);
    });
  });

  it("页面重开或当前会话切换后加载对应入库草稿", async () => {
    useConversationStore.setState({ activeRef: "conversation-next" });
    await actRender(<ImportPage />);

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
    await actRender(<ImportPage />);

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

  it("会话投影加载期间禁用分库和改名编辑", async () => {
    useConversationStore.setState({ activeRef: "conversation-next" });
    await actRender(<ImportPage />);

    expect(screen.getByPlaceholderText("如：旅行")).toBeDisabled();
    expect(screen.getByRole("button", { name: "分库" })).toBeDisabled();
  });

  // R22：独立入库页是「右侧或独立页都能改设置」的另一侧，改动写回同一份草稿计划。
  it("独立入库页也能改目标分库与改名模板，写回同一份计划状态", async () => {
    const plan = preparedPlan();
    resetWorkbench(plan);
    // 命名预览是异步接口；只有在这个用例里才会真正触发（模板从空变为有 token）。
    mocks.renderNamePreview.mockImplementation(async (pattern: string) => pattern);
    // 用 store 的真实 updateOptions：设置改动必须真的落到同一份草稿，而不是测试自造副本。
    const writeOptions = vi.fn(originalUpdateOptions);
    useImportWorkbenchStore.setState({ updateOptions: writeOptions });
    useConversationStore.setState({ saveDraft: vi.fn().mockResolvedValue(undefined) });
    const plansBefore = useImportWorkbenchStore.getState().plans;
    await actRender(<ImportPage />);

    // 独立页上分库与改名都可编辑（与右栏同一组入口）。
    expect(screen.getByPlaceholderText("如：旅行")).toBeEnabled();
    fireEvent.change(screen.getByPlaceholderText("如：旅行"), { target: { value: "旅行" } });
    await settle();
    expect(writeOptions).toHaveBeenNthCalledWith(1, "旅行", "");
    expect(screen.getByPlaceholderText("如：旅行")).toHaveValue("旅行");

    fireEvent.click(screen.getByRole("button", { name: "日期" }));
    await settle();
    expect(writeOptions).toHaveBeenNthCalledWith(2, "旅行", "{日期}");
    expect(screen.getByText("{日期}")).toBeInTheDocument();

    // 两次设置改动后仍只有原来那一份待入库计划：计划表还是同一个对象，没有重新准备。
    expect(writeOptions).toHaveBeenCalledTimes(2);
    const after = useImportWorkbenchStore.getState();
    expect(after.draft.collection).toBe("旅行");
    expect(after.draft.renamePattern).toBe("{日期}");
    expect(after.plans).toBe(plansBefore);
    expect(after.draft.pendingPlanRefs.map((entry) => entry.planRef)).toEqual(["plan-ui"]);
    expect(Object.keys(after.plans)).toEqual(["plan-ui"]);
    expect(actions.prepareFiles).not.toHaveBeenCalled();
    expect(actions.prepareDirectory).not.toHaveBeenCalled();
    expect(actions.clearPendingPlans).not.toHaveBeenCalled();
  });

  it("选择文件后只准备固定草稿计划", async () => {
    mocks.open.mockResolvedValue(["D:/incoming/a.jpg"]);
    const plan = preparedPlan();
    actions.prepareFiles.mockResolvedValue({ plan, warnings: [], taskId: "task-ui" });
    await actRender(<ImportPage />);

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
    await actRender(<ImportPage />);

    fireEvent.click(screen.getByText("选择文件…"));

    await waitFor(() => expect(screen.getByText("已处理 1/1 · 失败 0")).toBeInTheDocument());
  });

  it("新任务替换全局完成条后仍用 operation 的终态快照解除工作台忙碌", async () => {
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

    await actRender(<ImportPage />);

    // 完成后右栏给结果摘要，而不是留下一块空白。
    const outcome = screen.getByLabelText("本次入库结果");
    expect(outcome.textContent).toContain("入库完成");
    expect(outcome.textContent).toContain("已处理 1/1 · 失败 0");
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
    await actRender(<ImportPage />);

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
    await actRender(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "重试并核对受理" }));

    await waitFor(() => expect(actions.submitPlan).toHaveBeenCalledWith("plan-uncertain"));
  });

  it("明确提交待入库计划，交接动作不调用提交", async () => {
    const plan = preparedPlan();
    resetWorkbench(plan);
    await actRender(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "交给助手（待确认）" }));
    await waitFor(() => expect(actions.handoffPlan).toHaveBeenCalledWith("plan-ui"));
    expect(actions.submitPlan).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "开始入库（1）" }));
    await waitFor(() => expect(actions.submitPlan).toHaveBeenCalledWith("plan-ui"));
  });

  it("恢复草稿时要求剔除不支持项后才能提交", async () => {
    const plan = preparedPlan("unsupported");
    resetWorkbench(plan);
    await actRender(<ImportPage />);

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
    await actRender(<ImportPage />);

    fireEvent.click(screen.getByRole("button", { name: "取消入库" }));
    await waitFor(() => expect(actions.cancel).toHaveBeenCalledWith("op-ui-exact"));
  });

  it("恢复已完成操作的异常文件名与结构化错误码", async () => {
    const plan = preparedPlan();
    const operation = { ...runningOperation(), state: "partially_succeeded" as const, phase: "complete" as const, failed: 1, canCancel: false };
    resetWorkbench(plan, operation);
    useImportWorkbenchStore.setState({ operationResults: {
      [operation.operationId]: {
        sourceStepRef: "step-import-1",
        imported: 1,
        duplicateItems: [],
        duplicates: 2,
        failed: 1,
        failures: [{ itemRef: "failed-item", displayName: "broken.jpg", state: "failed", errorCode: "FILE_UNREADABLE" }],
      },
    } });
    await actRender(<ImportPage />);

    expect(screen.getByText("新入库 1 · 重复 2 · 失败 1")).toBeInTheDocument();
    fireEvent.click(screen.getByText(/查看异常明细/));
    expect(screen.getByText(/broken.jpg · 失败 · FILE_UNREADABLE/)).toBeInTheDocument();
  });

  it("重复项要点名，不能只报一个数字（M-03 真机补证）", async () => {
    const plan = preparedPlan();
    const operation = { ...runningOperation(), state: "succeeded" as const, phase: "complete" as const, canCancel: false };
    resetWorkbench(plan, operation);
    useImportWorkbenchStore.setState({ operationResults: {
      [operation.operationId]: {
        sourceStepRef: "step-import-1",
        imported: 3,
        duplicateItems: [
          { itemRef: "dup-1", displayName: "散图-01-导入页截图.jpg" },
          { itemRef: "dup-2", displayName: "散图-02-素材库截图.jpg" },
        ],
        duplicates: 2,
        failed: 0,
        failures: [],
      },
    } });
    await actRender(<ImportPage />);

    // 清单里两个同内容文件是并列显示的，光看「重复 2」不知道是哪两个，所以要能展开点名。
    const panel = screen.getByTestId("import-duplicates");
    expect(panel).toHaveTextContent("查看重复项（2）");
    fireEvent.click(screen.getByText(/查看重复项/));
    expect(panel).toHaveTextContent("散图-01-导入页截图.jpg");
    expect(panel).toHaveTextContent("散图-02-素材库截图.jpg");
  });

  it("没有重复项时不渲染空的重复明细（避免给出假提示）", async () => {
    const plan = preparedPlan();
    const operation = { ...runningOperation(), state: "succeeded" as const, phase: "complete" as const, canCancel: false };
    resetWorkbench(plan, operation);
    useImportWorkbenchStore.setState({ operationResults: {
      [operation.operationId]: {
        sourceStepRef: "step-import-1",
        imported: 3,
        duplicateItems: [],
        duplicates: 0,
        failed: 0,
        failures: [],
      },
    } });
    await actRender(<ImportPage />);

    expect(screen.queryByTestId("import-duplicates")).toBeNull();
  });

  it("结果明细读取失败时提供精确操作的重试入口", async () => {
    const operation = { ...runningOperation(), state: "succeeded" as const, phase: "complete" as const, canCancel: false };
    resetWorkbench(undefined, operation);
    useImportWorkbenchStore.setState({ operationResultErrors: { [operation.operationId]: "结果查询暂时失败" } });
    await actRender(<ImportPage />);

    expect(screen.getByRole("alert")).toHaveTextContent("结果明细读取失败：结果查询暂时失败");
    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "重试读取结果" }));
    await waitFor(() => expect(actions.loadOperationResult).toHaveBeenCalledTimes(2));
    expect(actions.loadOperationResult).toHaveBeenLastCalledWith(operation.operationId, operation.revision);
  });

  it("未设置素材总库时提示去设置且不触发文件选择器", async () => {
    useSettingsStore.setState({ settings: { libraryRoot: "" } as never });
    await actRender(<ImportPage />);
    fireEvent.click(screen.getByRole("button", { name: "前往设置" }));
    fireEvent.click(screen.getByText("选择文件…"));
    expect(screen.getAllByText(/请先设置素材总库位置/).length).toBeGreaterThan(0);
    expect(mocks.open).not.toHaveBeenCalled();
  });

  it("选择文件夹默认只授权本次读取，勾选「记住」才跨会话保留", async () => {
    mocks.pickDirectoryGrant.mockResolvedValue({
      grantRef: "grant-ui",
      directoryRef: "directory-ui",
      displayName: "下载",
      displayPath: "D:/下载",
      purpose: "import_source",
      recursive: true,
      remembered: false,
      available: true,
      access: { read: true, write: false, export: false },
    });
    actions.prepareDirectory.mockResolvedValue({ planRef: "plan-ui", items: [] });
    await actRender(<ImportPage />);

    // 默认未勾选：本次任务专用，不写入记住的授权。
    fireEvent.click(screen.getByText("选择文件夹…"));
    await waitFor(() =>
      expect(mocks.pickDirectoryGrant).toHaveBeenCalledWith("import_source", false),
    );

    fireEvent.click(screen.getByLabelText(/记住这个目录/));
    fireEvent.click(screen.getByText("选择文件夹…"));
    await waitFor(() =>
      expect(mocks.pickDirectoryGrant).toHaveBeenLastCalledWith("import_source", true),
    );
  });

  it("已记住的目录可直接复用：换发新句柄后走同一条 prepareDirectory 路径", async () => {
    mocks.listDirectoryGrants.mockResolvedValue([
      {
        grantRef: "grant-remembered",
        displayName: "下载素材",
        displayPath: "D:/素材/下载",
        purpose: "import_source",
        recursive: true,
        remembered: true,
        available: true,
        access: { read: true, write: false, export: false },
      },
    ]);
    // 记住的授权持 grant 不持句柄，必须换发才能读目录；复用旧句柄会因会话/实例不符被拒。
    mocks.reissueDirectoryGrant.mockResolvedValue({
      grantRef: "grant-remembered",
      directoryRef: "directory-reissued",
      displayName: "下载素材",
      displayPath: "D:/素材/下载",
      purpose: "import_source",
      recursive: true,
      remembered: true,
      available: true,
      access: { read: true, write: false, export: false },
    });
    actions.prepareDirectory.mockResolvedValue({ planRef: "plan-remembered", items: [] });
    await actRender(<ImportPage />);

    const entry = await screen.findByRole("button", { name: "下载素材" });
    fireEvent.click(entry);
    await waitFor(() =>
      expect(mocks.reissueDirectoryGrant).toHaveBeenCalledWith("grant-remembered"),
    );
    await waitFor(() =>
      expect(actions.prepareDirectory).toHaveBeenCalledWith("directory-reissued"),
    );
    // 复用已记住的目录绝不能顺手再弹一次原生选择器。
    expect(mocks.pickDirectoryGrant).not.toHaveBeenCalled();
  });

  it("已不可用的记住目录如实标注且不可点击，不伪造可用", async () => {
    mocks.listDirectoryGrants.mockResolvedValue([
      {
        grantRef: "grant-gone",
        displayName: "旧目录",
        displayPath: "E:/已删除",
        purpose: "import_source",
        recursive: true,
        remembered: true,
        available: false,
        access: { read: true, write: false, export: false },
      },
      {
        // 导出目标不属于导入来源，不得出现在入库页的「已记住的目录」里。
        grantRef: "grant-export",
        displayName: "导出目标",
        displayPath: "F:/导出",
        purpose: "export_destination",
        recursive: true,
        remembered: true,
        available: true,
        access: { read: true, write: true, export: true },
      },
    ]);
    await actRender(<ImportPage />);

    const unavailable = await screen.findByRole("button", { name: "旧目录（已不可用）" });
    expect(unavailable).toBeDisabled();
    expect(unavailable).toHaveAttribute("title", expect.stringContaining("重新选择"));
    expect(screen.queryByRole("button", { name: "导出目标" })).not.toBeInTheDocument();
    fireEvent.click(unavailable);
    expect(mocks.reissueDirectoryGrant).not.toHaveBeenCalled();
  });

  it("记住的目录换发失败时给出可执行提示，不静默当成成功", async () => {
    mocks.listDirectoryGrants.mockResolvedValue([
      {
        grantRef: "grant-stale",
        displayName: "已移动目录",
        displayPath: "D:/旧位置",
        purpose: "import_source",
        recursive: true,
        remembered: true,
        available: true,
        access: { read: true, write: false, export: false },
      },
    ]);
    mocks.reissueDirectoryGrant.mockRejectedValue(new Error("REFERENCE_EXPIRED"));
    await actRender(<ImportPage />);

    fireEvent.click(await screen.findByRole("button", { name: "已移动目录" }));
    await waitFor(() => expect(mocks.reissueDirectoryGrant).toHaveBeenCalled());
    expect(actions.prepareDirectory).not.toHaveBeenCalled();
  });
});
