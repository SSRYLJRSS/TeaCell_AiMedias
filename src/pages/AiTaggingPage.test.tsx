/**
 * FB6 需求一：AI 打标页内进度唯一化集成测试。
 *  - 点击开始（startBatch）后立即出现页内进度条（starting 相位，不等后端事件）；
 *  - 进度事件（唯一 onAiProgress 订阅）驱动百分比与当前素材名；
 *  - 取消/完成显示静态最终状态；
 *  - 底部全局任务条不出现「AI 打标中」胶囊（taskStore 已移除 AI 订阅），入库/导出不受影响。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import AiTaggingPage from "@/pages/AiTaggingPage";
import { aiPrepareBatch, aiStartTagging, aiSubmitTaggingPlan } from "@/api/ai";
import { approvePlan } from "@/api/plans";
import BottomBar from "@/components/layout/BottomBar";
import { useAiStore } from "@/stores/aiStore";
import { useTaskStore } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { AiBatch, AiSuggestion } from "@/types/ai";
import type { AiConnection } from "@/api/connections";
import type { Settings } from "@/types/settings";
import type { PlanPreview } from "@/types/plans";
import type { TaggingOperation, TaggingPreparation } from "@/types/ai";

const mocks = vi.hoisted(() => ({
  onAiProgress: vi.fn(),
  listAiConnections: vi.fn(),
  getAiUsageBindings: vi.fn(),
  setAiUsageBinding: vi.fn(),
  aiPrepareBatch: vi.fn(),
  aiSubmitTaggingPlan: vi.fn(),
  approvePlan: vi.fn(),
  aiListBatches: vi.fn().mockResolvedValue([]),
}));

vi.mock("@/api/ai", () => ({
  aiApplyTags: vi.fn(),
  aiCancelBatch: vi.fn(),
  aiConfirmAll: vi.fn(),
  aiConfirmSuggestion: vi.fn(),
  aiCreateBatch: vi.fn(),
  aiListBatches: mocks.aiListBatches,
  aiListSuggestions: vi.fn().mockResolvedValue([]),
  aiListSuggestionItems: vi.fn().mockResolvedValue([]),
  aiDecideSuggestionItem: vi.fn().mockResolvedValue(undefined),
  aiPrepareBatch: mocks.aiPrepareBatch,
  aiSubmitTaggingPlan: mocks.aiSubmitTaggingPlan,
  aiStartTagging: vi.fn(),
  aiRejectSuggestion: vi.fn(),
  aiRestoreSuggestion: vi.fn(),
  onAiProgress: mocks.onAiProgress,
}));
vi.mock("@/api/plans", () => ({ approvePlan: mocks.approvePlan }));
vi.mock("@/api/tags", () => ({
  recentTagOps: vi.fn().mockResolvedValue([]),
  undoTagBatch: vi.fn(),
  listTags: vi.fn().mockResolvedValue([]),
  listTagFacets: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue(null),
  toFileUrl: (p: string) => `asset://${p}`,
}));
vi.mock("@/api/assets", () => ({
  getAsset: vi.fn().mockResolvedValue({
    id: 101, filePath: "d:/lib/x.jpg", fileName: "x.jpg", fileExt: "jpg", fileSize: 1,
    mimeType: "image/jpeg", tags: [], createdAt: 1, modifiedAt: 1,
  }),
}));
vi.mock("@/api/import", () => ({
  onImportProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
}));
vi.mock("@/api/export", () => ({
  onExportProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
}));
vi.mock("@/api/connections", () => ({
  listAiConnections: mocks.listAiConnections,
  getAiUsageBindings: mocks.getAiUsageBindings,
  setAiUsageBinding: mocks.setAiUsageBinding,
}));

const mkBatch = (over: Partial<AiBatch> = {}): AiBatch => ({
  id: 1,
  status: "pending",
  mode: "cloud",
  total: 120,
  processed: 0,
  confirmed: 0,
  createdAt: 1,
  ...over,
});

const mkSuggestion = (id: number, assetId: number): AiSuggestion => ({
  id,
  batchId: 1,
  assetId,
  assetPath: `d:/lib/photo${assetId}.jpg`,
  mimeType: "image/jpeg",
  suggestedTags: {},
  status: "pending",
  confirmedTags: {},
  lastError: null,
  createdAt: id,
});

const mkRunProgress = (over: Partial<NonNullable<ReturnType<typeof useAiStore.getState>["runProgress"]>> = {}) => ({
  batchId: 1,
  processed: 0,
  total: 120,
  currentAssetId: null,
  batchProcessedAtStart: 0,
  requestedLimit: null,
  ...over,
});

/** 一条已启用、凭据就绪的云端连接：只有绑定到 tagging 用途才算「有打标服务」。 */
const mkConnection = (over: Partial<AiConnection> = {}): AiConnection => ({
  id: "cloud-1",
  name: "Agnes AI",
  deployment: "cloud",
  protocol: "openai_chat",
  baseUrl: "https://apihub.agnes-ai.com/v1",
  model: "agnes-2.5-flash",
  maxConcurrency: 0,
  requestsPerMinute: 0,
  requestsPerHour: 0,
  hasKey: true,
  credentialStatus: "configured",
  enabled: true,
  ...over,
});

const mkSettings = (): Settings => ({
  ai: {
    profiles: [],
    activeProfile: "",
    videoTagging: false,
    videoTaggingMode: "cover",
    videoFrameCount: 3,
    batchLimit: 500,
    ollamaSourceId: "auto",
    systemPromptTagging: "",
    systemPromptSearch: "",
    confidenceMinSuggest: 0.3,
  },
  theme: "system",
  logLevel: "info",
  thumbnailCacheMb: 2048,
  tagCategories: [],
  libraryRoot: "",
  trashRetentionDays: 30,
  customDownloadSources: [],
  modelDownloadProxy: "",
  appearance: {
    grid: { libraryCellStep: 3, importCellStep: 1, cellAspect: "1:1", cellFit: "cover", matchDominantColor: false },
    hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
    colorStrip: { enabled: true, showInLibraryGrid: false, showInViewer: true, showInImportGrid: false, height: "normal", mode: "ratio", count: 6 },
    kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
  },
  navigation: {
    shortcuts: [
      { id: "import", visible: true },
      { id: "library", visible: true },
      { id: "ai", visible: true },
      { id: "treasure", visible: true },
    ],
    settingsPinned: false,
  },
});

/** 捕获 onAiProgress 的 handler，手动派发进度事件 */
let progressHandler: ((p: { batchId: number; processed: number; total: number; currentAssetId: number }) => void) | null = null;

beforeEach(() => {
  vi.clearAllMocks();
  progressHandler = null;
  mocks.onAiProgress.mockImplementation((handler) => {
    progressHandler = handler;
    return Promise.resolve(() => undefined);
  });
  mocks.listAiConnections.mockResolvedValue([]);
  mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: null, agent: null });
  mocks.setAiUsageBinding.mockResolvedValue(undefined);
  mocks.aiListBatches.mockResolvedValue([]);
  useSettingsStore.setState({ settings: mkSettings(), loaded: true, previewAppearance: null });
  useTaskStore.setState({ tasks: [] });
  useAiStore.setState({
    batches: [],
    currentBatchId: null,
    suggestions: [],
    running: false,
    cancelling: false,
    error: null,
    lastProgressAssetId: null,
    runProgress: null,
    pendingAssetIds: [],
  });
});

/** 页面挂载后会异步读取 AI 连接与用途绑定；断言后必须 flush 在 act 内，否则更新落在测试之外。 */
async function flushPage() {
  await act(async () => {
    for (let i = 0; i < 3; i += 1) {
      await new Promise((r) => setTimeout(r, 0));
    }
  });
}

describe("AiTaggingPage 进度唯一化（FB6 需求一）", () => {
  it("左栏不重复显示页面标题，直接从当前服务上下文开始", async () => {
    render(<AiTaggingPage />);

    expect(await screen.findByText("未选择打标服务")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "打标" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "当前模型" })).toBeInTheDocument();
  });

  it("当前模型读取 tagging 用途绑定，不展示遗留的 settings.ai.profiles 默认配置", async () => {
    const legacySettings = mkSettings();
    legacySettings.ai.profiles = [
      {
        id: "default",
        name: "默认配置",
        apiMode: "openai",
        kind: "cloud",
        baseUrl: "https://opencode.ai/zen/go/v1",
        apiKey: "",
        model: "mimo-v2.5",
      },
    ];
    legacySettings.ai.activeProfile = "default";
    mocks.listAiConnections.mockResolvedValue([
      {
        id: "cloud-1",
        name: "Agnes AI",
        deployment: "cloud",
        protocol: "openai_chat",
        baseUrl: "https://apihub.agnes-ai.com/v1",
        model: "agnes-2.5-flash",
        hasKey: true,
        credentialStatus: "configured",
        enabled: true,
      },
    ]);
    mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: "cloud-1", agent: null });
    useSettingsStore.setState({ settings: legacySettings, loaded: true });

    render(<AiTaggingPage />);

    expect(await screen.findByRole("combobox", { name: "AI 打标服务" })).toHaveValue("cloud-1");
    expect(screen.getByText("模型：agnes-2.5-flash")).toBeInTheDocument();
    expect(screen.queryByText("默认配置")).not.toBeInTheDocument();
    expect(screen.queryByText("mimo-v2.5")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI 打标" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "手动模式" })).not.toBeInTheDocument();
  });

  it("视频批次只读取设置页模式，不再出现第二套封面/抽帧控件", async () => {
    const video = {
      ...mkSuggestion(1, 101),
      assetPath: "d:/lib/clip.mp4",
      mimeType: "video/mp4",
    };
    useSettingsStore.setState({
      settings: { ...mkSettings(), ai: { ...mkSettings().ai, videoTagging: true, videoTaggingMode: "frames", videoFrameCount: 4 } },
      loaded: true,
    });
    useAiStore.setState({ batches: [mkBatch({ total: 1 })], currentBatchId: 1, suggestions: [video] });

    render(<AiTaggingPage />);

    expect(screen.queryByRole("button", { name: "封面打标" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "抽帧打标" })).not.toBeInTheDocument();
    expect(screen.getByText(/含 1 个视频 × 4/)).toBeInTheDocument();
    await flushPage();
  });

  it("running 即立即出现页内进度条（starting 相位），不等后端事件", async () => {
    useAiStore.setState({ batches: [mkBatch()], currentBatchId: 1, suggestions: [mkSuggestion(1, 101)], running: true, runProgress: mkRunProgress({ total: 0 }) });
    render(<AiTaggingPage />);
    const bars = screen.getAllByRole("progressbar");
    expect(bars).toHaveLength(1);
    expect(bars[0]).not.toHaveAttribute("aria-valuenow"); // starting 不确定条
    expect(screen.getAllByText("正在连接 AI 服务，请稍候 · 不会卡住").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("正在准备本次任务 · 批次范围 120 张")).toBeInTheDocument();
    await flushPage();
  });

  it("进度事件驱动百分比与当前素材名；全页只有当前批次这一条进度条", async () => {
    useAiStore.setState({
      batches: [mkBatch()],
      currentBatchId: 1,
      suggestions: [mkSuggestion(1, 101), mkSuggestion(2, 142)],
      running: true,
      runProgress: mkRunProgress(),
    });
    render(
      <>
        <AiTaggingPage />
        <BottomBar current="ai" onNavigate={() => {}} />
      </>,
    );
    act(() => {
      progressHandler?.({ batchId: 1, processed: 3, total: 10, currentAssetId: 142 });
    });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "30");
    expect(screen.getAllByText(/「photo142\.jpg」/).length).toBeGreaterThanOrEqual(1);
    // 底部居中不出现「AI 打标中」胶囊：taskStore 已无 AI 订阅，进度事件不产生任务
    expect(screen.queryByText("AI 打标中")).not.toBeInTheDocument();
    expect(useTaskStore.getState().tasks).toHaveLength(0);
    expect(useAiStore.getState().lastProgressAssetId).toBe(142);
    expect(screen.getByText("本次执行 3/10 · 批次已处理 3/120")).toBeInTheDocument();
    await flushPage();
  });

  it("取消中显示取消文案；完成后显示静态最终状态", async () => {
    useAiStore.setState({ batches: [mkBatch({ processed: 5 })], currentBatchId: 1, running: true, cancelling: true, runProgress: mkRunProgress({ processed: 5 }) });
    const { rerender } = render(<AiTaggingPage />);
    // 页面状态行与 LED 提示各有一份取消文案；进度条仍唯一
    expect(screen.getAllByText("取消已受理，当前图片完成后停止").length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);

    // 收尾：running 翻转 + 批次 done → 静态最终状态
    act(() => {
      useAiStore.setState({
        running: false,
        cancelling: false,
        batches: [mkBatch({ status: "done", processed: 120 })],
        runProgress: mkRunProgress({ processed: 10, total: 10 }),
      });
    });
    rerender(<AiTaggingPage />);
    expect(screen.getByText("打标结束 · 已处理 10 / 10")).toBeInTheDocument();
    expect(document.querySelector(".ai-marquee-track")).toBeNull(); // 滚动动画停止
    await flushPage();
  });

  it("失败显示错误文案（带原因）", async () => {
    useAiStore.setState({ batches: [mkBatch({ processed: 3 })], currentBatchId: 1, running: true, runProgress: mkRunProgress({ processed: 1 }) });
    const { rerender } = render(<AiTaggingPage />);
    act(() => {
      useAiStore.setState({ running: false, error: "网络超时" });
    });
    rerender(<AiTaggingPage />);
    expect(screen.getByText("打标失败：网络超时")).toBeInTheDocument();
    await flushPage();
  });

  it("页面级错误用 role=alert 播报，键盘与读屏用户不会只看到界面没反应（M-14）", async () => {
    useAiStore.setState({ batches: [mkBatch({ processed: 3 })], currentBatchId: 1, running: false, error: "连接本地模型失败" });
    render(<AiTaggingPage />);
    const alerts = screen.getAllByRole("alert");
    expect(alerts.some((node) => node.textContent?.includes("连接本地模型失败"))).toBe(true);
    await flushPage();
  });

  it("切批次后旧批次的终态不残留（回到 idle 不渲染进度块）", async () => {
    useAiStore.setState({ batches: [mkBatch({ status: "done", processed: 120 })], currentBatchId: 1, running: true, runProgress: mkRunProgress({ processed: 120 }) });
    const { rerender } = render(<AiTaggingPage />);
    act(() => {
      useAiStore.setState({ running: false });
    });
    rerender(<AiTaggingPage />);
    expect(screen.getByText("打标结束 · 已处理 120 / 120")).toBeInTheDocument();
    // 切到另一个批次
    act(() => {
      useAiStore.setState({ batches: [mkBatch({ id: 2, status: "pending" })], currentBatchId: 2 });
    });
    rerender(<AiTaggingPage />);
    expect(screen.queryByText(/打标结束/)).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    await flushPage();
  });
});

describe("AiTaggingPage 一句话描述草稿同步", () => {
  it("同步未编辑建议，保留正在编辑的草稿，并在用户确认后显示最终值", async () => {
    const suggestion = { ...mkSuggestion(1, 101), suggestedDescription: "初始建议" };
    useAiStore.setState({ batches: [mkBatch({ total: 1 })], currentBatchId: 1, suggestions: [suggestion] });
    render(<AiTaggingPage />);

    const input = screen.getByRole("textbox", { name: "一句话描述" });
    expect(input).toHaveValue("初始建议");

    await act(async () => {
      useAiStore.setState({ suggestions: [{ ...suggestion, lastError: "无关字段更新" }] });
      await Promise.resolve();
    });
    expect(screen.getByRole("textbox", { name: "一句话描述" })).toHaveValue("初始建议");

    await act(async () => {
      useAiStore.setState({ suggestions: [{ ...suggestion, suggestedDescription: "更新建议" }] });
      await Promise.resolve();
    });
    expect(screen.getByRole("textbox", { name: "一句话描述" })).toHaveValue("更新建议");

    fireEvent.change(input, { target: { value: "用户正在编辑" } });
    await act(async () => {
      useAiStore.setState({ suggestions: [{ ...suggestion, suggestedDescription: "更晚模型结果" }] });
      await Promise.resolve();
    });
    expect(screen.getByRole("textbox", { name: "一句话描述" })).toHaveValue("用户正在编辑");

    await act(async () => {
      useAiStore.setState({
        suggestions: [{ ...suggestion, lastError: "模型请求失败" }],
      });
      await Promise.resolve();
    });
    expect(useAiStore.getState().suggestions[0]?.status).toBe("pending");
    expect(screen.getByRole("textbox", { name: "一句话描述" })).toHaveValue("用户正在编辑");

    await act(async () => {
      useAiStore.setState({
        suggestions: [{
          ...suggestion,
          status: "confirmed",
          confirmedDescription: "用户最终确认",
        }],
      });
      await Promise.resolve();
    });
    expect(screen.getByText("用户最终确认")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "一句话描述" })).not.toBeInTheDocument();
  });
});

describe("AiTaggingPage 打标计划审批", () => {
  const preview: PlanPreview = {
    schemaVersion: 1,
    planId: "plan-tagging-1",
    capability: "tagging_submit",
    library: { libraryId: "library-1", libraryEpoch: "epoch-1" },
    reviewStatus: "pending",
    reviewKind: "ordinary",
    originKind: "gui",
    itemCount: 1,
    summary: "已冻结 1 个素材的打标范围",
    reviewRef: "review-tagging-1",
    expiresAtUnixSeconds: 1_800_000_000,
    scope: { scopeRef: "scope-tagging-1" },
    budget: { maxRequests: 2, maxTokens: 0, maxSeconds: 3600, maxItems: 1 },
  };
  const operation: TaggingOperation = {
    operationId: "operation-tagging-1",
    stepId: "step-tagging-1",
    batchId: 1,
    requestLimit: 2,
    requestsAttempted: 0,
  };

  beforeEach(() => {
    useAiStore.setState({
      batches: [mkBatch({ total: 1 })],
      currentBatchId: 1,
      suggestions: [mkSuggestion(1, 101)],
    });
    // 打标审批链要求真的绑定了打标服务：没有绑定（R-44 降级态）时按钮会变成「先去设置…」，
    // 那是另一条路径，由下面的降级用例覆盖。
    mocks.listAiConnections.mockResolvedValue([mkConnection()]);
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: "cloud-1",
      agent: null,
    });
    vi.mocked(aiPrepareBatch).mockResolvedValue({
      requiresApproval: true,
      summary: preview.summary,
      preview,
      operation: null,
    } satisfies TaggingPreparation);
    vi.mocked(approvePlan).mockResolvedValue({
      schemaVersion: 1,
      planId: preview.planId,
      reviewStatus: "approved",
    });
    vi.mocked(aiSubmitTaggingPlan).mockResolvedValue(operation);
    vi.mocked(aiStartTagging).mockResolvedValue(mkBatch({ status: "done", processed: 1 }));
  });

  it("页面逐步审批、提交并显式启动打标 operation", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<AiTaggingPage />);
    fireEvent.click(screen.getByRole("button", { name: "开始打标" }));

    await waitFor(() => expect(aiStartTagging).toHaveBeenCalledWith(operation.operationId, operation.stepId));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining(preview.summary));
    expect(approvePlan).toHaveBeenCalledWith(preview.planId, preview.reviewRef);
    expect(aiSubmitTaggingPlan).toHaveBeenCalledWith(preview.planId, `tagging_submit_${preview.planId}`);
  });

  it("用户取消审批时不提交计划也不启动联网步骤", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<AiTaggingPage />);
    fireEvent.click(screen.getByRole("button", { name: "开始打标" }));

    await waitFor(() => expect(aiPrepareBatch).toHaveBeenCalledWith(1, undefined));
    expect(approvePlan).not.toHaveBeenCalled();
    expect(aiSubmitTaggingPlan).not.toHaveBeenCalled();
    expect(aiStartTagging).not.toHaveBeenCalled();
  });

  // R35：既有 Rust 用例证明后端真的按最长边 1024 压 JPEG；这一条证明用户在按「开始打标」
  // 之前就在页面上看到同一口径，而不是点完确认框才知道发的是什么。
  it("批准前如实写出外发清晰度：最长边 1024 的 JPEG，高清要另行确认", async () => {
    render(<AiTaggingPage />);
    const clarity = await screen.findByTestId("tagging-egress-clarity");

    expect(clarity).toHaveTextContent("JPEG");
    expect(clarity).toHaveTextContent("最长边 1024");
    expect(clarity).toHaveTextContent("另行提示并单独确认");
    // 关键负向：不能暗示会自动放高清——那等于绕过单独确认这一步。
    expect(clarity).not.toHaveTextContent("自动发送原图");
    expect(clarity).not.toHaveTextContent("原始尺寸");
    // 说明必须与「开始打标」同屏，且排在按钮之后——它解释的正是按下这一刻会发生什么。
    expect(clarity.previousElementSibling).toHaveTextContent("开始打标");
    // 读到清晰度本身不触发任何准备计划或外发。
    expect(aiPrepareBatch).not.toHaveBeenCalled();
  });

  // R35 的边界：没有当前批次（左栏没有可打标的图）时不渲染启动区，
  // 那条清晰度说明也就不该单独挂在页面上——避免用户在打不了标时读到一句无主的口径。
  it("没有当前批次时不单独渲染外发清晰度说明", async () => {
    useAiStore.setState({ batches: [], currentBatchId: null, suggestions: [] });
    render(<AiTaggingPage />);

    await flushPage();
    expect(screen.queryByTestId("tagging-egress-clarity")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "开始打标" })).not.toBeInTheDocument();
  });

  // M-04 真机补证：批次 #50 的 6 张全部确认后再进页面，左栏原本只说「当前没有待确认的建议」，
  // 用户会以为还能再点一次「开始打标」。后端 ai_confirm_all 只确认 pending，重跑等于白烧请求且什么都不写，
  // 所以这里必须提前说清楚「都已确认写入」。
  it("全部确认后再进页面，如实说明已确认写入、再点开始打标不会改动素材（M-04 真机补证）", async () => {
    const confirmedSuggestion = (id: number, assetId: number) => ({
      ...mkSuggestion(id, assetId),
      suggestedTags: { 主体: ["猫"] },
      status: "confirmed" as const,
      confirmedTags: { 主体: ["猫"] },
    });
    const doneBatch = mkBatch({ status: "done", total: 2, processed: 2, confirmed: 2 });
    useAiStore.setState({
      batches: [doneBatch],
      currentBatchId: 1,
      suggestions: [confirmedSuggestion(1, 101), confirmedSuggestion(2, 142)],
      running: false,
    });
    // 页面挂载会 refreshBatches()，默认 mock 返回 [] 会把种入的批次清掉；这里让它返回同一批。
    mocks.aiListBatches.mockResolvedValue([doneBatch]);
    render(<AiTaggingPage />);

    const reviewed = await screen.findByTestId("tagging-all-reviewed");
    expect(reviewed).toHaveTextContent("都已确认写入");
    expect(reviewed).toHaveTextContent("不会改动任何素材");
    // 关键负向：不能再给出「当前没有待确认的建议」这种会让人误以为可以继续的说法。
    expect(screen.queryByText(/当前没有待确认的建议/)).not.toBeInTheDocument();
    // 读到提示本身不触发重跑。
    expect(aiStartTagging).not.toHaveBeenCalled();
    expect(aiPrepareBatch).not.toHaveBeenCalled();
    await flushPage();
  });

  // 边界：还有待确认的批次不能挂这条提示——否则用户会以为已经确认完了。
  it("仍有待确认建议时不显示「都已确认写入」", async () => {
    useAiStore.setState({
      batches: [mkBatch({ status: "done", total: 2, processed: 2, confirmed: 1 })],
      currentBatchId: 1,
      suggestions: [{ ...mkSuggestion(1, 101), suggestedTags: { 主体: ["猫"] }, status: "confirmed" as const },
        { ...mkSuggestion(2, 142), suggestedTags: { 主体: ["狗"] } }],
      running: false,
    });
    render(<AiTaggingPage />);

    await flushPage();
    expect(screen.queryByTestId("tagging-all-reviewed")).not.toBeInTheDocument();
  });

  /**
   * M-11 真机补证（960×640，真实安装包）：收起态悬浮入口停在左下，压在历史批次列表上，
   * 批次行的「继续 / 撤销」永远点不到。真机复验证明只给列表加内边距不够——默认滚动位置
   * 的批次行仍落在入口底下，所以安全区必须由左栏容器承担。
   */
  it("左栏容器留出悬浮入口安全区，默认滚动位置也不会压住批次行（M-11 最小窗口）", async () => {
    const doneBatch = mkBatch({ status: "done", total: 2, processed: 2, confirmed: 2 });
    useAiStore.setState({ batches: [doneBatch], currentBatchId: 1, suggestions: [], running: false });
    mocks.aiListBatches.mockResolvedValue([doneBatch]);
    render(<AiTaggingPage />);

    const scroller = await screen.findByTestId("tagging-batch-scroll");
    expect(scroller.className).toContain("overflow-y-auto");
    const panel = screen.getByTestId("tagging-side-panel");
    expect(panel.className).toContain("pb-32");
    expect(scroller.className).not.toContain("pb-32");
    await flushPage();
  });

  /**
   * M-11 真机补证（960×640，打开批次后）：aside 的 pb-32 只在内容不溢出时才成立。
   * 真机实测打开 #52 后，服务信息 + 当前批次 + 统计 + 提示合计把左栏顶到 y=734，
   * 越过安全区底边（726→安全区后 695），悬浮入口（y=645）又压住「全部确认/提示」区。
   * 所以非滚动的顶部区块本身必须可收缩、可滚动，不能靠外层 padding 兜底。
   */
  it("打开批次后顶部区块自己在安全区内滚动，不会顶穿悬浮入口安全区（M-11 最小窗口）", async () => {
    const doneBatch = mkBatch({ status: "done", total: 1, processed: 1, confirmed: 1 });
    useAiStore.setState({
      batches: [doneBatch],
      currentBatchId: 1,
      suggestions: [{ ...mkSuggestion(1, 101), suggestedTags: { 主体: ["猫"] }, status: "confirmed" as const }],
      running: false,
    });
    mocks.aiListBatches.mockResolvedValue([doneBatch]);
    render(<AiTaggingPage />);

    // 顶部区块：可收缩 + 自己滚动，才可能在内容很高时仍停在安全区内。
    const header = await screen.findByTestId("tagging-side-header");
    expect(header.className).toContain("min-h-0");
    expect(header.className).toContain("shrink");
    expect(header.className).toContain("overflow-y-auto");
    // 批次的「都已确认写入」提示确实渲染进来了（说明这一块真的很高，不是空壳）。
    expect(screen.getByTestId("tagging-all-reviewed")).toBeInTheDocument();
    // 顶部区块必须在左栏容器内，而且不与批次列表共用同一个滚动容器。
    const panel = screen.getByTestId("tagging-side-panel");
    expect(panel.contains(header)).toBe(true);
    expect(header.contains(screen.getByTestId("tagging-batch-scroll"))).toBe(false);
    expect(panel.className).toContain("pb-32");
    await flushPage();
  });
});

// M-07 真机走查补证（PRD R-44 / ARCHITECTURE «不配置 AI 不是错误状态»）：
// 没有选中打标服务时，页面既不能把「开始打标」做成点了报红字的死路，
// 也不能让用户以为手工填写不可用——它必须先说清楚「可以先手工填写」，并把 AI 入口指向设置。
describe("AiTaggingPage 未配置打标服务时的降级（M-07 / R-44）", () => {
  it("未绑定打标服务时如实说明可以先手工填写，AI 入口改成「先去设置」而不是报错", async () => {
    mocks.listAiConnections.mockResolvedValue([mkConnection()]);
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: null,
    });
    const manualBatch = mkBatch({ mode: "manual", total: 1 });
    useAiStore.setState({
      batches: [manualBatch],
      currentBatchId: 1,
      suggestions: [mkSuggestion(1, 101)],
      running: false,
    });
    // 页面挂载会 refreshBatches()，默认 mock 返回 [] 会把种入的批次清掉。
    mocks.aiListBatches.mockResolvedValue([manualBatch]);

    render(<AiTaggingPage />);

    const hint = await screen.findByTestId("tagging-no-service-hint");
    expect(hint).toHaveTextContent("可以先直接手工填写");
    expect(hint).not.toHaveTextContent("尚未绑定打标服务，请先在打标页选择");

    // 主按钮此时不是「开始打标」：它会把用户送进一个必然失败的准备流程。
    expect(screen.queryByRole("button", { name: "开始打标" })).not.toBeInTheDocument();
    const toSettings = screen.getByRole("button", { name: "先去设置选择打标服务" });
    fireEvent.click(toSettings);
    // 点它不发起任何打标请求，只是把用户送到设置页。
    expect(aiPrepareBatch).not.toHaveBeenCalled();
    expect(aiStartTagging).not.toHaveBeenCalled();
    await flushPage();
  });

  // 绑定之后必须恢复成真正的启动按钮——降级不能变成永久改文案。
  it("绑定打标服务后主按钮恢复成「开始打标」", async () => {
    mocks.listAiConnections.mockResolvedValue([mkConnection()]);
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: "cloud-1",
      agent: null,
    });
    const boundBatch = mkBatch({ total: 1 });
    useAiStore.setState({
      batches: [boundBatch],
      currentBatchId: 1,
      suggestions: [mkSuggestion(1, 101)],
      running: false,
    });
    mocks.aiListBatches.mockResolvedValue([boundBatch]);

    render(<AiTaggingPage />);

    expect(await screen.findByRole("button", { name: "开始打标" })).toBeInTheDocument();
    expect(screen.queryByTestId("tagging-no-service-hint")).not.toBeInTheDocument();
    await flushPage();
  });
});
