/**
 * 助手右侧工作台测试（A3-07）。
 *
 * 验收口径：
 * - 没有 AI 也能在右侧完成入库准备（不依赖任何 AI 服务）；
 * - 从已受理导入准备打标必须按逐项成功收据，且重复映射项只能由用户逐个勾选纳入；
 * - 打标仍走可信批准 + 显式启动：用户拒绝审批时绝不提交、绝不启动；
 * - 工作台只跟随自己所属的对话，别的对话不抢右栏。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import AssistantWorkbench from "@/components/assistant/AssistantWorkbench";
import { useAiStore } from "@/stores/aiStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import type { ImportResultView, ImportPlanItem, PreparedImportPlan } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

const mocks = vi.hoisted(() => ({
  prepareFromImport: vi.fn(),
  startTagging: vi.fn(),
  approvePlan: vi.fn(),
  aiSubmitTaggingPlan: vi.fn(),
  refreshLibrary: vi.fn(),
  refreshTags: vi.fn(),
  useImportWorkbench: vi.fn(),
  /** A3-08：右栏预览只允许读计数摘要，这两个是「加载素材行」的入口，必须不被调用。 */
  summarySelection: vi.fn(),
  pageSelection: vi.fn(),
  listAssets: vi.fn(),
}));

/** 把异步回写收进 act：不加这层，挂载/点击后的 promise 落地会报「not wrapped in act」。 */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

vi.mock("@/hooks/useImportWorkbench", () => ({
  useImportWorkbench: mocks.useImportWorkbench,
}));

vi.mock("@/api/plans", () => ({ approvePlan: mocks.approvePlan }));
vi.mock("@/api/ai", () => ({
  aiSubmitTaggingPlan: mocks.aiSubmitTaggingPlan,
  aiListSuggestions: vi.fn().mockResolvedValue([]),
  aiListSuggestionItems: vi.fn().mockResolvedValue([]),
  aiDecideSuggestionItem: vi.fn(),
  aiApplyTags: vi.fn(),
  aiConfirmSuggestion: vi.fn(),
  aiRejectSuggestion: vi.fn(),
  aiRestoreSuggestion: vi.fn(),
}));
vi.mock("@/api/import", () => ({
  openFileExternal: vi.fn(),
  renderNamePreview: vi.fn(async (pattern: string) => pattern),
  newImportTaskId: vi.fn(() => "task_test"),
}));
vi.mock("@/api/preview", () => ({ getPreviewUrl: vi.fn().mockResolvedValue(null) }));
vi.mock("@/api/library", () => ({ listAssets: mocks.listAssets }));
vi.mock("@/api/selections", () => ({
  summarySelection: mocks.summarySelection,
  pageSelection: mocks.pageSelection,
  snapshotSelection: vi.fn(),
}));
vi.mock("@/api/assets", () => ({ getAsset: vi.fn().mockResolvedValue(null) }));
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue(null),
  toFileUrl: (path: string) => path,
}));
vi.mock("@/api/tags", () => ({
  listTagFacets: vi.fn().mockResolvedValue([]),
  recentTagOps: vi.fn().mockResolvedValue([]),
  undoTagBatch: vi.fn(),
}));

const refA = "conversation_aaaaaaaaaaaaaaaa";

function planOf(items: ImportPlanItem[]): PreparedImportPlan {
  return {
    planRef: "plan_aaaaaaaaaaaaaaaaaaaa",
    itemCount: items.length,
    readyCount: items.length,
    limitedCount: 0,
    unsupportedCount: 0,
    totalSize: 2048,
    expiresAtUnixSeconds: 99,
    items: items.map((item, index) => ({
      itemRef: item.itemRef ?? `item_${index}`,
      displayName: item.displayName ?? `f${index}.jpg`,
      kind: item.kind,
      size: item.size,
      modifiedAt: 1,
      previewStatus: item.previewStatus,
      renamePreview: `p${index}.jpg`,
      excluded: false,
    })),
  };
}

const snapshot = (overrides: Partial<OperationSnapshot> = {}): OperationSnapshot => ({
  schemaVersion: 1,
  operationId: "operation_aaaaaaaaaaaaaaaa",
  library: { libraryId: "library_aaaaaaaaaaaaaaaa", libraryEpoch: "1" },
  revision: 1,
  state: "succeeded",
  phase: "complete",
  completed: 2,
  failed: 0,
  total: 2,
  canCancel: false,
  canResume: false,
  ...overrides,
});

const result: ImportResultView = {
  sourceStepRef: "step_aaaaaaaaaaaaaaaa",
  imported: 1,
  duplicateItems: [{ itemRef: "item_duplicate", displayName: "dup.jpg" }],
  duplicates: 1,
  failed: 0,
  failures: [],
};

function controller(overrides: Record<string, unknown> = {}) {
  const items: ImportPlanItem[] = [
    { path: "C:/a/one.jpg", kind: "image", size: 1024, previewStatus: "ready", planRef: "plan_aaaaaaaaaaaaaaaaaaaa", itemRef: "item_one", displayName: "one.jpg" },
  ];
  return {
    workbench: {
      conversationRef: refA,
      preparing: false,
      submitting: false,
      loading: false,
      error: null,
      draft: {
        schemaVersion: 1,
        collection: "",
        renamePattern: "{原名}",
        warnings: [],
        pendingPlanRefs: [{ planRef: "plan_aaaaaaaaaaaaaaaaaaaa" }],
        recentOperationRefs: ["operation_aaaaaaaaaaaaaaaa"],
      },
      plans: { plan_aaaaaaaaaaaaaaaaaaaa: planOf(items) },
      operationResults: { operation_aaaaaaaaaaaaaaaa: result },
      operationResultErrors: {},
      updateOptions: vi.fn(),
      clearPendingPlans: vi.fn(),
      cancelPreparation: vi.fn(),
      loadOperationResult: vi.fn(),
    },
    error: null,
    libraryRoot: "D:/library",
    settingsLoaded: true,
    settingsLoadError: null,
    loadSettings: vi.fn(),
    busy: false,
    operationInProgress: false,
    submissionUncertain: false,
    conversationSwitching: false,
    activeOperations: [],
    operationSnapshots: { operation_aaaaaaaaaaaaaaaa: snapshot() },
    activeOperationRef: "operation_aaaaaaaaaaaaaaaa",
    selectedTask: undefined,
    items,
    imageCount: 1,
    videoCount: 0,
    totalSize: 1024,
    unsupportedByPlan: [],
    unsupportedCount: 0,
    showUnsupported: false,
    setShowUnsupported: vi.fn(),
    showWarnings: false,
    setShowWarnings: vi.fn(),
    rememberDirectory: false,
    setRememberDirectory: vi.fn(),
    rememberedGrants: [],
    chooseRememberedFolder: vi.fn(),
    stage: vi.fn(),
    chooseFiles: vi.fn(),
    chooseFolder: vi.fn(),
    removeItem: vi.fn(),
    removeUnsupported: vi.fn(),
    runSafely: vi.fn(async (action: () => Promise<unknown>) => action()),
    submitAll: vi.fn(),
    ...overrides,
  };
}

function renderPanel(overrides: Record<string, unknown> = {}, props: Record<string, unknown> = {}) {
  mocks.useImportWorkbench.mockReturnValue(controller(overrides));
  const onTabChange = vi.fn();
  const onClose = vi.fn();
  const onToggleWide = vi.fn();
  const ui = render(
    <AssistantWorkbench
      conversationRef={refA}
      tab="import"
      wide={false}
      onTabChange={onTabChange}
      onToggleWide={onToggleWide}
      onClose={onClose}
      {...props}
    />,
  );
  return { onTabChange, onClose, onToggleWide, rerender: ui.rerender };
}

/**
 * R13 需要断言「打开结果视图不改写素材库筛选与选择」，
 * 因此先记下模块初始的浏览上下文，每个用例前恢复，避免用例之间互相污染。
 */
const initialLibraryFilter = useLibraryStore.getState().filter;
const initialSelected = useSelectionStore.getState().selected;

beforeEach(() => {
  vi.clearAllMocks();
  useLibraryStore.setState({ filter: initialLibraryFilter });
  useSelectionStore.setState({ selected: initialSelected });
  useSelectionResultStore.setState({ target: null });
  mocks.listAssets.mockResolvedValue({ items: [], nextCursor: null });
  mocks.summarySelection.mockResolvedValue({
    selectionRef: "selection_aaaaaaaaaaaaaaaa",
    total: 1,
    truncated: false,
    snapshot: 1,
    available: 1,
    missing: 0,
    query: "q",
  });
  mocks.pageSelection.mockResolvedValue({ items: [], nextCursor: null });
  useAiStore.setState({
    batches: [],
    currentBatchId: null,
    suggestions: [],
    running: false,
    cancelling: false,
    error: null,
    runProgress: null,
    lastProgressAssetId: null,
    pendingAssetIds: [],
    preparingFromImport: false,
  });
  mocks.prepareFromImport.mockResolvedValue({
    requiresApproval: true,
    summary: "将把 1 项素材发送给 服务甲（模型 m），图片使用压缩后的 JPEG。",
    preview: { planId: "plan_tag", reviewRef: "review_tag", reviewStatus: "pending" },
    operation: null,
  });
  mocks.approvePlan.mockResolvedValue({ reviewStatus: "approved" });
  mocks.aiSubmitTaggingPlan.mockResolvedValue({
    operationId: "operation_tagging_aaaaaaaa",
    stepId: "step_tagging_aaaaaaaa",
    batchId: 7,
    requestLimit: 10,
    requestsAttempted: 0,
  });
  mocks.refreshLibrary.mockResolvedValue(undefined);
  mocks.refreshTags.mockResolvedValue(undefined);
  useAiStore.setState({
    prepareFromImport: mocks.prepareFromImport,
    startTagging: mocks.startTagging,
  });
});

describe("AssistantWorkbench", () => {
  it("没有 AI 服务时也能在右侧准备入库", async () => {
    renderPanel();
    expect(screen.getByRole("tab", { name: "入库" })).toBeInTheDocument();
    expect(screen.getByText("one.jpg")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始入库（1）" })).toBeInTheDocument();
    // 未配置任何 AI 服务也应该看到入库设置，而不是被引导去配置 AI。
    expect(screen.getByPlaceholderText("如：旅行")).toBeInTheDocument();
    // 入库页内嵌的命名预览是异步回写的；让它在测试内落地，避免落在测试之外（act 警告）。
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("待确认计划已失效时说明原因并指向「清空」，而不是只把「开始入库」变灰（M-14 可行动提示）", async () => {
    const base = controller();
    mocks.useImportWorkbench.mockReturnValue({
      ...base,
      workbench: {
        ...base.workbench,
        // 草稿里 2 条引用，但只有 1 份计划加载成功：另一份已过期/读取失败。
        draft: {
          ...base.workbench.draft,
          pendingPlanRefs: [
            { planRef: "plan_aaaaaaaaaaaaaaaaaaaa" },
            { planRef: "plan_expiredbbbbbbbbbbbbbb" },
          ],
        },
      },
    });
    render(
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    // 数据条件决定了按钮必须禁用（不能假装能入库），但原因要摆在用户面前。
    expect(screen.getByRole("button", { name: "开始入库（1）" })).toBeDisabled();
    expect(
      screen.getByText(/有 1 份待确认计划已失效或读取失败，暂时不能开始入库/),
    ).toHaveAttribute("role", "alert");
    expect(screen.getByText(/请点「清空」后回到入库页重新选择素材/)).toBeInTheDocument();
    // 命名预览是异步回写的，收进 act，避免警告落在测试之外。
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("重复映射项默认不纳入，勾选后才传给准备动作", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPanel();
    const prepare = screen.getByRole("button", { name: "从这次入库准备打标" });
    fireEvent.click(prepare);
    await waitFor(() => expect(mocks.prepareFromImport).toHaveBeenCalled());
    expect(mocks.prepareFromImport).toHaveBeenLastCalledWith(
      "operation_aaaaaaaaaaaaaaaa",
      "step_aaaaaaaaaaaaaaaa",
      [],
    );

    fireEvent.click(screen.getByRole("checkbox", { name: /dup\.jpg/ }));
    fireEvent.click(prepare);
    await waitFor(() =>
      expect(mocks.prepareFromImport).toHaveBeenLastCalledWith(
        "operation_aaaaaaaaaaaaaaaa",
        "step_aaaaaaaaaaaaaaaa",
        ["item_duplicate"],
      ),
    );
    confirm.mockRestore();
  });

  // R35：默认压缩预览外发（最长边 1024 JPEG），高清另提示。
  it("准备打标前如实写出外发清晰度：压缩预览最长边 1024px，更高清要另外确认", async () => {
    renderPanel();

    const clarity = await screen.findByTestId("tagging-egress-clarity");
    expect(clarity).toHaveTextContent("最长边 1024px");
    expect(clarity).toHaveTextContent("高清");
    expect(clarity).toHaveTextContent("单独确认");
    // 只说不做：这段说明本身不得触发准备、审批或联网。
    expect(mocks.prepareFromImport).not.toHaveBeenCalled();
    expect(mocks.approvePlan).not.toHaveBeenCalled();
    expect(mocks.startTagging).not.toHaveBeenCalled();
  });

  it("用户拒绝审批时不批准、不提交、不启动", async () => {    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "从这次入库准备打标" }));
    await waitFor(() => expect(mocks.prepareFromImport).toHaveBeenCalled());
    expect(confirm).toHaveBeenCalled();
    expect(mocks.approvePlan).not.toHaveBeenCalled();
    expect(mocks.aiSubmitTaggingPlan).not.toHaveBeenCalled();
    expect(mocks.startTagging).not.toHaveBeenCalled();
    confirm.mockRestore();
  });

  it("审批通过后按 批准 → 提交 → 显式启动 顺序执行：先切到标签审核且**不**自动启动打标；再点「开始打标」才真正启动", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const { onTabChange } = renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "从这次入库准备打标" }));
    // 审批链路走完：批准 → 提交 → 切到审核，但**不**启动联网。
    await waitFor(() => expect(mocks.aiSubmitTaggingPlan).toHaveBeenCalled());
    expect(mocks.approvePlan).toHaveBeenCalledWith("plan_tag", "review_tag");
    expect(mocks.aiSubmitTaggingPlan).toHaveBeenCalledWith("plan_tag", "tagging_submit_plan_tag");
    expect(onTabChange).toHaveBeenCalledWith("review");
    // 关键：审批通过后不得自动启动打标（不再「批准即启动」）。
    expect(mocks.startTagging).not.toHaveBeenCalled();
    // 待启动状态出现在面板上，含明确的「开始打标」按钮。
    const startBtn = await screen.findByRole("button", { name: "开始打标" });
    expect(screen.getByText(/打标计划已批准/)).toBeInTheDocument();
    // 用户显式点「开始打标」后才调用 startTagging（真实启动 + 联网）。
    fireEvent.click(startBtn);
    await waitFor(() => expect(mocks.startTagging).toHaveBeenCalledTimes(1));
    expect(mocks.startTagging).toHaveBeenCalledWith(
      expect.objectContaining({ operationId: "operation_tagging_aaaaaaaa", batchId: 7 }),
    );
    confirm.mockRestore();
  });

  it("工作台只跟随自己的对话：草稿属于别的对话时不给操作入口", async () => {
    const other = controller();
    other.workbench = { ...other.workbench, conversationRef: "conversation_bbbbbbbbbbbbbbbb" };
    mocks.useImportWorkbench.mockReturnValue(other);
    render(
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(/正在切换到当前对话的入库草稿/);
    expect(screen.queryByRole("button", { name: /开始入库/ })).not.toBeInTheDocument();
  });

  it("已记住的目录在工作台里可一键复用，失效项如实标注且不可点", async () => {
    const chooseRememberedFolder = vi.fn();
    renderPanel({
      items: [],
      unsupportedByPlan: [],
      rememberedGrants: [
        {
          grantRef: "grant_remembered",
          displayName: "常用素材",
          displayPath: "D:\\常用素材",
          purpose: "import_source",
          recursive: true,
          remembered: true,
          available: true,
          access: { read: true, write: false, export: false },
        },
        {
          grantRef: "grant_gone",
          displayName: "已删除目录",
          displayPath: "E:\\旧目录",
          purpose: "import_source",
          recursive: true,
          remembered: true,
          available: false,
          access: { read: true, write: false, export: false },
        },
      ],
      chooseRememberedFolder,
    });

    const usable = await screen.findByRole("button", { name: "常用素材" });
    fireEvent.click(usable);
    // 记住的授权只持 grant，必须靠换发句柄拿目录，不能凭 grantRef 直接读。
    await waitFor(() => expect(chooseRememberedFolder).toHaveBeenCalledWith("grant_remembered"));

    const gone = screen.getByRole("button", { name: "已删除目录（已不可用）" });
    expect(gone).toBeDisabled();
    expect(gone).toHaveAttribute("title", expect.stringContaining("重新选择"));
    fireEvent.click(gone);
    // 明确失效时不再发起换发，避免把已失效目录当成可用。
    expect(chooseRememberedFolder).toHaveBeenCalledTimes(1);
  });

  it("扫描异常在右栏也可见，不因窄布局丢掉异常明细", async () => {
    const setShowWarnings = vi.fn();
    renderPanel({
      showWarnings: false,
      setShowWarnings,
    });
    // 默认不出现（没有 warnings 时不留空框）。先确认干净数据的初始态。
    expect(screen.queryByText(/扫描时跳过或遇到/)).not.toBeInTheDocument();

    const base = controller({ showWarnings: false, setShowWarnings });
    const withWarnings = {
      ...base,
      workbench: {
        ...base.workbench,
        draft: {
          ...base.workbench.draft,
          warnings: ["跳过 unsupported.heic：缩略图无法生成", "读取失败 broken.jpg"],
        },
      },
    };
    mocks.useImportWorkbench.mockReturnValue(withWarnings);
    render(
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByText("扫描时跳过或遇到 2 项，请确认后再入库。")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "查看扫描提示" }));
    expect(setShowWarnings).toHaveBeenCalled();
    // 两次渲染都内嵌命名预览，异步回写要收在测试内（否则 act 警告）。
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("受理响应待核对时右栏给出「重试并核对受理」，不清空丢幂等键", async () => {
    const submitPlan = vi.fn().mockResolvedValue(undefined);
    const base = controller({ submissionUncertain: true, busy: true });
    const uncertain = {
      ...base,
      workbench: {
        ...base.workbench,
        submitPlan,
        draft: {
          ...base.workbench.draft,
          // plan 还没落地但已有幂等键：正是「提交返回前断了」的形态。
          plans: {},
          pendingPlanRefs: [{ planRef: "plan_uncertain", idempotencyKey: "tagging_submit_plan_uncertain" }],
          recentOperationRefs: [],
        },
      },
      items: [],
      operationSnapshots: {},
    };
    mocks.useImportWorkbench.mockReturnValue(uncertain);
    render(
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByText(/1 份计划的受理响应待核对/)).toBeInTheDocument();
    // 清空按钮必须是禁用的，且说明为什么不能清。
    // 用 title 定位：RenameBuilder 也有一个叫「清空」的按钮，按名字会撞。
    const clear = screen.getByTitle("受理响应待核对，请重试该计划");
    expect(clear).toBeDisabled();
    expect(clear).toHaveTextContent("清空");

    fireEvent.click(screen.getByRole("button", { name: "重试并核对受理" }));
    await waitFor(() => expect(submitPlan).toHaveBeenCalledWith("plan_uncertain"));
  });

  it("入库已经跑起来时右栏能取消，而不是只留一个禁用按钮", async () => {
    const cancel = vi.fn().mockResolvedValue(undefined);
    const base = controller({
      operationInProgress: true,
      busy: true,
      activeOperations: [{ operationId: "operation_running", state: "running", canCancel: true, revision: 2 }],
    });
    const running = {
      ...base,
      workbench: { ...base.workbench, cancel },
    };
    mocks.useImportWorkbench.mockReturnValue(running);
    render(
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    // 有取消能力时必须给「取消入库」，不能再渲染一个无解释的死按钮。
    expect(screen.queryByRole("button", { name: "正在处理…" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "取消入库" }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith("operation_running"));
  });

  it("审核页显示真实打标进度并可批量确认，与独立打标页一致", async () => {
    const confirmAll = vi.fn().mockResolvedValue(undefined);
    useAiStore.setState({
      currentBatchId: 7,
      running: true,
      runProgress: { batchId: 7, processed: 1, total: 3, currentAssetId: 11, batchProcessedAtStart: 0, requestedLimit: 3 },
      suggestions: [
        {
          id: 1,
          batchId: 7,
          assetId: 11,
          assetPath: "C:/a/one.jpg",
          mimeType: "image/jpeg",
          suggestedTags: { 主体: ["猫"] },
          status: "pending",
          confirmedTags: {},
          lastError: null,
          createdAt: 1,
        },
        {
          id: 2,
          batchId: 7,
          assetId: 12,
          assetPath: "C:/a/two.jpg",
          mimeType: "image/jpeg",
          suggestedTags: { 主体: ["狗"] },
          status: "pending",
          confirmedTags: {},
          lastError: null,
          createdAt: 1,
        },
      ],
      confirmAll,
    });
    renderPanel({}, { tab: "review" });

    // 进度必须来自 aiStore 真实计数，不能是假计时器。
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
    // LED 文案是有意的视觉副本（滚动 + sr-only），因此会出现多次。
    expect(screen.getAllByText(/第 2 \/ 3 张素材/).length).toBeGreaterThan(0);
    // 运行中不给「全部确认」，避免和批次任务并发打架。
    expect(screen.queryByRole("button", { name: /全部确认/ })).not.toBeInTheDocument();

    // 组件订阅 aiStore，外部 setState 必须落在 act 内，否则状态回写逃逸出批处理。
    act(() => {
      useAiStore.setState({ running: false });
    });
    const all = await screen.findByRole("button", { name: "全部确认（2）" });
    fireEvent.click(all);
    await waitFor(() => expect(confirmAll).toHaveBeenCalledTimes(1));
  });

  // R13：在助手里看结果不离开聊天。右栏只给数量摘要，点按钮才进可退出的「本次结果视图」。
  it("R13 右栏预览只读数量、不加载素材行，打开结果视图不改写素材库筛选与选择", async () => {
    const selectionRef = "selection_aaaaaaaaaaaaaaaa";
    mocks.summarySelection.mockResolvedValue({
      selectionRef,
      total: 5,
      truncated: false,
      snapshot: 3,
      available: 2,
      missing: 1,
      query: "红色 猫",
    });
    // 素材库当前浏览上下文：打开结果视图前后必须逐字节一样。
    useLibraryStore.setState({
      filter: { ...useLibraryStore.getState().filter, search: "保留的搜索", assetType: "video", tagId: 42 },
    });
    useSelectionStore.setState({ selected: new Set([7, 8]) });
    const filterBefore = useLibraryStore.getState().filter;
    const selectedBefore = useSelectionStore.getState().selected;

    renderPanel({}, { selections: [selectionRef] });

    const preview = await screen.findByLabelText("本次结果预览");
    // 数量摘要：来自后端 SelectionSummary，不是本地 items 长度。
    expect(await screen.findByText("共 3 项 · 可用 2 项 · 1 项已不可用")).toBeInTheDocument();
    // A3-08 的有意设计：预览区只列交接引用，不渲染素材缩略图行。
    // 一条交接引用 = 一个 li，且一个 <img> 都不该出现。
    expect(preview.querySelectorAll("li")).toHaveLength(1);
    expect(preview.querySelectorAll("img")).toHaveLength(0);
    // 也没有任何「加载素材行」的接口调用。
    expect(mocks.pageSelection).not.toHaveBeenCalled();
    expect(mocks.listAssets).not.toHaveBeenCalled();

    const navigations: unknown[] = [];
    const onNavigate = (event: Event) => navigations.push((event as CustomEvent).detail);
    window.addEventListener("app:navigate", onNavigate);
    fireEvent.click(screen.getByRole("button", { name: "查看本次结果" }));
    window.removeEventListener("app:navigate", onNavigate);

    // 点按钮才打开「本次结果视图」，标题/检索词都取自摘要。
    expect(useSelectionResultStore.getState().target).toEqual({
      selectionRef,
      title: "助手交接的 3 项",
      query: "红色 猫",
      conversationRef: refA,
    });
    expect(navigations).toEqual(["library"]);
    // 打开结果视图不改写素材库的筛选与选择（连对象引用都不变）。
    expect(useLibraryStore.getState().filter).toBe(filterBefore);
    expect(useSelectionStore.getState().selected).toBe(selectedBefore);
    await settle();
  });

  // R22：右侧或独立页都能改设置；改的是同一份草稿计划，不是新建第二份。
  it("R22 右栏能改目标分库与改名模板，两处写回同一份草稿计划", async () => {
    const base = controller();
    const draft = { ...base.workbench.draft };
    const updateOptions = vi.fn(async (collection: string, renamePattern: string) => {
      // 真实 store 的语义是「原地改 draft，不新建计划」；这里镜像同一语义。
      draft.collection = collection;
      draft.renamePattern = renamePattern;
    });
    const workbench = { ...base.workbench, draft, updateOptions };
    mocks.useImportWorkbench.mockReturnValue({ ...base, workbench });
    // 每次 rerender 都要新建 element：复用同一个 element 会让 React 走 props 同一性 bailout，
    // 草稿写回后的界面就不会重新渲染。
    const makeElement = () => (
      <AssistantWorkbench
        conversationRef={refA}
        tab="import"
        wide={false}
        onTabChange={vi.fn()}
        onToggleWide={vi.fn()}
        onClose={vi.fn()}
      />
    );
    const ui = render(makeElement());

    // 改目标分库。
    fireEvent.change(screen.getByPlaceholderText("如：旅行"), { target: { value: "旅行" } });
    await settle();
    expect(updateOptions).toHaveBeenNthCalledWith(1, "旅行", "{原名}");
    ui.rerender(makeElement());
    expect(screen.getByPlaceholderText("如：旅行")).toHaveValue("旅行");

    // 改改名模板：RenameBuilder 的 token 直接写回同一份草稿。
    fireEvent.click(screen.getByRole("button", { name: "日期" }));
    await settle();
    expect(updateOptions).toHaveBeenNthCalledWith(2, "旅行", "{原名}_{日期}");
    ui.rerender(makeElement());
    expect(screen.getByText("{原名}_{日期}")).toBeInTheDocument();

    // 两次编辑仍只有一份待入库计划，也没有重新准备出新计划。
    expect(updateOptions).toHaveBeenCalledTimes(2);
    expect(workbench.draft.pendingPlanRefs).toHaveLength(1);
    expect(Object.keys(workbench.plans)).toEqual(["plan_aaaaaaaaaaaaaaaaaaaa"]);
    expect(base.chooseFiles).not.toHaveBeenCalled();
    expect(base.chooseFolder).not.toHaveBeenCalled();
    await settle();
  });

  // R14（工作台可调宽那一半）：加宽/收窄是真实宽度切换，不是纯文案。
  it("R14 加宽后工作台宽度变化，再收窄能回到原宽度", async () => {
    mocks.useImportWorkbench.mockReturnValue(controller());
    function Harness() {
      const [wide, setWide] = useState(false);
      return (
        <AssistantWorkbench
          conversationRef={refA}
          tab="import"
          wide={wide}
          onTabChange={vi.fn()}
          onToggleWide={() => setWide((value) => !value)}
          onClose={vi.fn()}
        />
      );
    }
    render(<Harness />);

    // 默认窄栏：380px，按钮是「加宽」。
    expect(screen.getByLabelText("右侧工作台").className).toContain("w-[380px]");
    expect(screen.getByLabelText("右侧工作台").className).not.toContain("w-[620px]");
    expect(screen.getByRole("button", { name: "加宽" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "加宽" }));
    // 加宽后真实宽度类变化，按钮翻成「收窄」。
    expect(screen.getByLabelText("右侧工作台").className).toContain("w-[620px]");
    expect(screen.getByLabelText("右侧工作台").className).not.toContain("w-[380px]");
    expect(screen.getByRole("button", { name: "收窄" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "收窄" }));
    // 手动恢复得回去，不是单向放大。
    expect(screen.getByLabelText("右侧工作台").className).toContain("w-[380px]");
    expect(screen.queryByRole("button", { name: "收窄" })).not.toBeInTheDocument();
    await settle();
  });
});
