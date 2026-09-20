/**
 * FB6 需求一：AI 打标页内进度唯一化集成测试。
 *  - 点击开始（startBatch）后立即出现页内进度条（starting 相位，不等后端事件）；
 *  - 进度事件（唯一 onAiProgress 订阅）驱动百分比与当前素材名；
 *  - 取消/完成显示静态最终状态；
 *  - 底部全局任务条不出现「AI 打标中」胶囊（taskStore 已移除 AI 订阅），入库/导出不受影响。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import AiTaggingPage from "@/pages/AiTaggingPage";
import BottomBar from "@/components/layout/BottomBar";
import { useAiStore } from "@/stores/aiStore";
import { useTaskStore } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { AiBatch, AiSuggestion } from "@/types/ai";
import type { Settings } from "@/types/settings";

const mocks = vi.hoisted(() => ({
  onAiProgress: vi.fn(),
  listAiConnections: vi.fn(),
  getAiUsageBindings: vi.fn(),
  setAiUsageBinding: vi.fn(),
}));

vi.mock("@/api/ai", () => ({
  aiApplyTags: vi.fn(),
  aiCancelBatch: vi.fn(),
  aiConfirmAll: vi.fn(),
  aiConfirmSuggestion: vi.fn(),
  aiCreateBatch: vi.fn(),
  aiListBatches: vi.fn().mockResolvedValue([]),
  aiListSuggestions: vi.fn().mockResolvedValue([]),
  aiListSuggestionItems: vi.fn().mockResolvedValue([]),
  aiDecideSuggestionItem: vi.fn().mockResolvedValue(undefined),
  aiRejectSuggestion: vi.fn(),
  aiRestoreSuggestion: vi.fn(),
  aiStartBatch: vi.fn(),
  onAiProgress: mocks.onAiProgress,
}));
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
  mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: null });
  mocks.setAiUsageBinding.mockResolvedValue(undefined);
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
    pendingAssetIds: [],
  });
});

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
        name: "agenes",
        deployment: "cloud",
        protocol: "openai_chat",
        baseUrl: "https://api.agnes-ai.cn/v1",
        model: "agnes-3.0-flash",
        hasKey: true,
        credentialStatus: "configured",
        enabled: true,
      },
    ]);
    mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: "cloud-1" });
    useSettingsStore.setState({ settings: legacySettings, loaded: true });

    render(<AiTaggingPage />);

    expect(await screen.findByRole("combobox", { name: "AI 打标服务" })).toHaveValue("cloud-1");
    expect(screen.getByText("模型：agnes-3.0-flash")).toBeInTheDocument();
    expect(screen.queryByText("默认配置")).not.toBeInTheDocument();
    expect(screen.queryByText("mimo-v2.5")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI 打标" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "手动模式" })).not.toBeInTheDocument();
  });

  it("视频批次只读取设置页模式，不再出现第二套封面/抽帧控件", () => {
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
  });

  it("running 即立即出现页内进度条（starting 相位），不等后端事件", () => {
    useAiStore.setState({ batches: [mkBatch()], currentBatchId: 1, suggestions: [mkSuggestion(1, 101)], running: true });
    render(<AiTaggingPage />);
    const bars = screen.getAllByRole("progressbar");
    expect(bars).toHaveLength(1);
    expect(bars[0]).not.toHaveAttribute("aria-valuenow"); // starting 不确定条
    expect(screen.getAllByText("正在连接 AI 服务，请稍候 · 不会卡住").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("正在生成建议 0/120")).toBeInTheDocument();
  });

  it("进度事件驱动百分比与当前素材名；全页只有当前批次这一条进度条", () => {
    useAiStore.setState({
      batches: [mkBatch()],
      currentBatchId: 1,
      suggestions: [mkSuggestion(1, 101), mkSuggestion(2, 142)],
      running: true,
    });
    render(
      <>
        <AiTaggingPage />
        <BottomBar current="ai" onNavigate={() => {}} />
      </>,
    );
    act(() => {
      progressHandler?.({ batchId: 1, processed: 3, total: 120, currentAssetId: 142 });
    });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "3");
    expect(screen.getAllByText(/「photo142\.jpg」/).length).toBeGreaterThanOrEqual(1);
    // 底部居中不出现「AI 打标中」胶囊：taskStore 已无 AI 订阅，进度事件不产生任务
    expect(screen.queryByText("AI 打标中")).not.toBeInTheDocument();
    expect(useTaskStore.getState().tasks).toHaveLength(0);
    expect(useAiStore.getState().lastProgressAssetId).toBe(142);
  });

  it("取消中显示取消文案；完成后显示静态最终状态", () => {
    useAiStore.setState({ batches: [mkBatch({ processed: 5 })], currentBatchId: 1, running: true, cancelling: true });
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
      });
    });
    rerender(<AiTaggingPage />);
    expect(screen.getByText("打标结束 · 已处理 120 / 120")).toBeInTheDocument();
    expect(document.querySelector(".ai-marquee-track")).toBeNull(); // 滚动动画停止
  });

  it("失败显示错误文案（带原因）", () => {
    useAiStore.setState({ batches: [mkBatch({ processed: 3 })], currentBatchId: 1, running: true });
    const { rerender } = render(<AiTaggingPage />);
    act(() => {
      useAiStore.setState({ running: false, error: "网络超时" });
    });
    rerender(<AiTaggingPage />);
    expect(screen.getByText("打标失败：网络超时")).toBeInTheDocument();
  });

  it("切批次后旧批次的终态不残留（回到 idle 不渲染进度块）", () => {
    useAiStore.setState({ batches: [mkBatch({ status: "done", processed: 120 })], currentBatchId: 1, running: true });
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
  });
});
