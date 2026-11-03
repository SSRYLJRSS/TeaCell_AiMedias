/**
 * App 启动骨架 + Viewer BottomBar 互斥测试（指导书 §3.2/§7.3 方案 A）：
 *  - 设置未加载时显示 StartupSkeleton（不显示页面内容，不显示空白）；
 *  - 设置加载完成后渲染页面；
 *  - viewerOpen=true 时全局 BottomBar 隐藏（Viewer 自带胶片条，避免双重导航），TitleBar 保留。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import App from "@/App";
import { getHelpPageUrl, openHelpPage, saveSettings } from "@/api/settings";
import { invoke } from "@/api/client";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePlatformStore } from "@/stores/platformStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import type { Settings } from "@/types/settings";

function mkSettings(): Settings {
  return {
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
    tutorialPromptHandled: true,
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
  };
}

// 与 LibraryPage 测试相同的 mock 集（页面渲染需要）
const mocks = vi.hoisted(() => ({
  listAssets: vi.fn(),
  listAssetIds: vi.fn(),
  listMetadataFacets: vi.fn(),
  getAssetUrls: vi.fn(),
  revealInFolder: vi.fn(),
  trashRestore: vi.fn(),
  getThumbnailUrl: vi.fn(),
  toFileUrl: vi.fn(),
}));
vi.mock("@/api/assets", () => ({
  listAssets: mocks.listAssets,
  listAssetIds: mocks.listAssetIds,
  listMetadataFacets: mocks.listMetadataFacets,
  getAssetUrls: mocks.getAssetUrls,
  revealInFolder: mocks.revealInFolder,
  trashRestore: mocks.trashRestore,
}));
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: mocks.getThumbnailUrl,
  toFileUrl: mocks.toFileUrl,
}));
// A4-03：恢复走逐项结果通道，测试里换成可控桩（回收站列表在渲染时不会被调用）。
vi.mock("@/api/trash", () => ({
  listTrash: vi.fn().mockResolvedValue([]),
  trashRetention: vi.fn().mockResolvedValue([]),
  restoreAssets: vi.fn().mockResolvedValue({
    operationRef: "operation_test",
    restored: 0,
    failed: 0,
    unknown: 0,
    skipped: 0,
    cancelled: false,
    items: [],
  }),
  restoreErrorLabel: () => "",
  failedRestores: () => [],
}));
vi.mock("@/api/settings", () => ({
  getSettings: vi.fn().mockResolvedValue({
    ai: { profiles: [], activeProfile: "", videoTagging: false, batchLimit: 500, systemPromptTagging: "", systemPromptSearch: "", ollamaSourceId: "auto", confidenceMinSuggest: 0.3 },
    theme: "system",
    thumbnailCacheMb: 2048,
    tagCategories: [],
    libraryRoot: "",
    trashRetentionDays: 30,
    customDownloadSources: [],
      modelDownloadProxy: "",
      tutorialPromptHandled: true,
  }),
  saveSettings: vi.fn().mockResolvedValue(undefined),
  openHelpPage: vi.fn().mockResolvedValue(undefined),
  getHelpPageUrl: vi.fn().mockResolvedValue("https://help.example/tutorial"),
}));
vi.mock("@/api/import", () => ({
  onImportProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
}));
vi.mock("@/api/export", () => ({
  onExportProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
}));
vi.mock("@/api/ai", () => ({
  onAiProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
}));
// FB4-03：@/api/client 的 on() 用于 App 级 palette://updated 订阅（测试里捕获 handler 手动触发）
const clientMocks = vi.hoisted(() => ({
  on: vi.fn(),
}));
const platformMocks = vi.hoisted(() => ({
  getPlatformCapabilities: vi.fn(),
}));
vi.mock("@/api/client", () => ({
  on: clientMocks.on,
  invoke: vi.fn(),
}));
vi.mock("@/api/platform", () => ({
  getPlatformCapabilities: platformMocks.getPlatformCapabilities,
}));
// A3-06：首次引导要显示 AI 服务概况；测试里给出空服务列表与空绑定。
vi.mock("@/api/connections", () => ({
  listAiConnections: vi.fn().mockResolvedValue([]),
  getAiUsageBindings: vi.fn().mockResolvedValue({ super_search: null, tagging: null, agent: null }),
  setAiUsageBinding: vi.fn().mockResolvedValue(undefined),
  getSuperSearchServiceResolution: vi.fn().mockResolvedValue(null),
}));
// A3-06：首次引导可选择总库目录；测试里不让原生选择器打开。
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn().mockResolvedValue(null),
}));

vi.stubGlobal("ResizeObserver", class {
  cb: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
  }
  observe() {
    this.cb([{ contentRect: { width: 800, height: 600 } } as ResizeObserverEntry], this as unknown as ResizeObserver);
  }
  unobserve() {}
  disconnect() {}
});
vi.stubGlobal("IntersectionObserver", class {
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
});
Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
  configurable: true,
  value: () => ({
    width: 800, height: 600, top: 0, left: 0, right: 800, bottom: 600, x: 0, y: 0, toJSON: () => ({}),
  }),
});

const emptySettingsStore = {
  settings: null,
  loaded: false,
  loading: false,
  loadError: null,
  saving: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState(emptySettingsStore);
  usePlatformStore.setState({
    status: "ready",
    error: null,
    capabilities: {
      schemaVersion: 1,
      os: "windows",
      arch: "x86_64",
      managedOllama: true,
      preferredVideoProxy: "h264_mp4",
      nativeWindowControls: false,
      primaryModifier: "ctrl",
      libraryTransferVersion: null,
    },
  });
  useLibraryStore.setState({
    items: [],
    total: 0,
    loading: false,
    error: null,
    filter: {
      assetType: "all",
      untaggedOnly: false,
      tagId: null,
      facetFilters: [],
      excludeTagIds: [],
      metadataFilters: [],
      search: "",
      sortBy: "created_at",
      sortDir: "desc",
      trashOnly: false,
    },
    viewerOpen: false,
    gridScrollTops: {},
  });
  mocks.listAssets.mockResolvedValue({ items: [], total: 0, hasMore: false });
  mocks.listAssetIds.mockResolvedValue([]);
  mocks.listMetadataFacets.mockResolvedValue([]);
  mocks.getAssetUrls.mockResolvedValue([]);
  mocks.revealInFolder.mockResolvedValue(undefined);
  mocks.trashRestore.mockResolvedValue(undefined);
  mocks.getThumbnailUrl.mockResolvedValue("asset://hd.webp");
  mocks.toFileUrl.mockImplementation((p: string) => `asset://${p}`);
  // 默认：on() 不捕获（订阅失败路径由 useTauriEvent 兜底）
  clientMocks.on.mockReset().mockRejectedValue(new Error("no tauri"));
  platformMocks.getPlatformCapabilities.mockResolvedValue({
    schemaVersion: 1,
    os: "windows",
    arch: "x86_64",
    managedOllama: true,
    preferredVideoProxy: "h264_mp4",
    nativeWindowControls: false,
    primaryModifier: "ctrl",
    libraryTransferVersion: null,
  });
});

/**
 * 首屏启动期会串起「设置 → 平台能力 → 素材库首屏」多层异步回写。
 * 断言完初始状态后必须把这条链 flush 在 act 内，否则更新落在测试之外，成为 act 噪声。
 */
async function flushBoot() {
  await act(async () => {
    for (let i = 0; i < 3; i += 1) {
      await new Promise((r) => setTimeout(r, 0));
    }
  });
}

describe("App 启动骨架（§3.2）", () => {
  it("设置未加载时先显示 StartupSkeleton（TitleBar 仍在）", async () => {
    useSettingsStore.setState(emptySettingsStore); // loaded=false → skeleton
    render(<App />);
    expect(screen.getByText("正在准备素材库")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument(); // TitleBar 保留
    await flushBoot();
  });

  it("新安装首次启动进入可跳过的引导向导，并将选择持久化为已处理", async () => {
    useSettingsStore.setState({
      ...emptySettingsStore,
      settings: { ...mkSettings(), tutorialPromptHandled: false },
      loaded: true,
    });
    render(<App />);
    // A3-06：首次引导是可跳过的多步向导，第一步引导总库位置。
    expect(await screen.findByRole("dialog", { name: "欢迎使用茶馆" })).toBeInTheDocument();
    expect(screen.getByLabelText("总库位置")).toHaveValue("");
    fireEvent.click(screen.getByRole("button", { name: "跳过引导" }));
    await waitFor(() =>
      expect(saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({ tutorialPromptHandled: true }),
      ),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "欢迎使用茶馆" })).not.toBeInTheDocument(),
    );
  });

  it("跳过引导后手动入口照常可用：底栏还能打开入库，也能回到素材库", async () => {
    // R32「跳过不妨碍手动功能」：跳过只关掉引导，不能连带关掉入库/素材库这些手动入口。
    useSettingsStore.setState({
      ...emptySettingsStore,
      settings: { ...mkSettings(), tutorialPromptHandled: false },
      loaded: true,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "跳过引导" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "欢迎使用茶馆" })).not.toBeInTheDocument(),
    );
    // 引导关掉后底栏仍在（没有被遮罩或引导层挡住）。
    expect(screen.getByRole("navigation", { name: "主导航" })).toBeInTheDocument();

    // 一次导航进独立入库工作台：入口可点、目标页面内容真的渲染出来。
    fireEvent.click(screen.getByRole("button", { name: "入库" }));
    expect(await screen.findByText("待入库清单")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "入库" })).toHaveAttribute("aria-current", "page");
    await flushBoot();

    // 同一条底栏回素材库：素材库页面的工具条重新出现。
    fireEvent.click(screen.getByRole("button", { name: "素材库" }));
    await waitFor(() => expect(screen.getByTestId("toolbar-filters")).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "素材库" })).toHaveAttribute("aria-current", "page");
    await flushBoot();
  });

  it("首次引导最后一步说明未配置 AI 也不影响手动流程", async () => {
    useSettingsStore.setState({
      ...emptySettingsStore,
      settings: { ...mkSettings(), tutorialPromptHandled: false },
      loaded: true,
    });
    render(<App />);
    await screen.findByRole("dialog", { name: "欢迎使用茶馆" });
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText(/不配置也能用入库、素材库和手动标签/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText(/执行必须由你在卡片上确认/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "以后再看" })).toBeInTheDocument();
  });

  it("默认浏览器打开失败时展示可复制的使用帮助地址", async () => {
    vi.mocked(openHelpPage).mockRejectedValueOnce(new Error("browser unavailable"));
    vi.mocked(getHelpPageUrl).mockResolvedValueOnce("https://help.example/tutorial");
    useSettingsStore.setState({
      ...emptySettingsStore,
      settings: { ...mkSettings(), tutorialPromptHandled: false },
      loaded: true,
    });
    render(<App />);
    await screen.findByRole("dialog", { name: "欢迎使用茶馆" });
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    fireEvent.click(screen.getByRole("button", { name: "查看新手教程" }));
    expect(await screen.findByDisplayValue("https://help.example/tutorial")).toBeInTheDocument();
    expect(screen.getByText(/无法自动打开新手教程/)).toBeInTheDocument();
  });

  it("引导选择持久化失败时如实提示，仍允许继续使用且下次可再次邀请", async () => {
    vi.mocked(saveSettings).mockRejectedValueOnce(new Error("设置文件不可写"));
    useSettingsStore.setState({
      ...emptySettingsStore,
      settings: { ...mkSettings(), tutorialPromptHandled: false },
      loaded: true,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "跳过引导" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("设置文件不可写");
    fireEvent.click(screen.getByRole("button", { name: "继续使用" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "欢迎使用茶馆" })).not.toBeInTheDocument(),
    );
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ tutorialPromptHandled: true }));
  });

  it("全局阻止 WebView 原生右键菜单", async () => {
    useSettingsStore.setState({ ...emptySettingsStore, settings: mkSettings(), loaded: true });
    render(<App />);
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
    await flushBoot();
  });

  it("启动时加载平台能力，供平台门控与路径显示共享使用", async () => {
    useSettingsStore.setState({ ...emptySettingsStore, settings: mkSettings(), loaded: true });
    usePlatformStore.setState({ status: "idle", capabilities: null, error: null });
    render(<App />);
    await waitFor(() => expect(platformMocks.getPlatformCapabilities).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(usePlatformStore.getState().capabilities?.managedOllama).toBe(true),
    );
  });

  it("平台能力加载失败时展示重试并保持安全降级", async () => {
    useSettingsStore.setState({ ...emptySettingsStore, settings: mkSettings(), loaded: true });
    usePlatformStore.setState({ status: "idle", capabilities: null, error: null });
    platformMocks.getPlatformCapabilities
      .mockRejectedValueOnce(new Error("platform unavailable"))
      .mockResolvedValueOnce({
        schemaVersion: 1,
        os: "windows",
        arch: "x86_64",
        managedOllama: true,
        preferredVideoProxy: "h264_mp4",
        nativeWindowControls: false,
        primaryModifier: "ctrl",
        libraryTransferVersion: null,
      });
    render(<App />);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("平台专属功能已安全禁用");
    expect(usePlatformStore.getState().capabilities).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(platformMocks.getPlatformCapabilities).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(usePlatformStore.getState().capabilities?.managedOllama).toBe(true),
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("macOS 与无边框 Tauri 配置一致：报告自绘控件并继续显示可访问标题栏", async () => {
    useSettingsStore.setState({ ...emptySettingsStore, settings: mkSettings(), loaded: true });
    usePlatformStore.setState({
      status: "ready",
      error: null,
      capabilities: {
        schemaVersion: 1,
        os: "macos",
        arch: "aarch64",
        managedOllama: false,
        preferredVideoProxy: "h264_mp4",
        nativeWindowControls: false,
        primaryModifier: "meta",
        libraryTransferVersion: null,
      },
    });
    render(<App />);
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "窗口控制" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "最小化" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /最大化|还原/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭" })).toBeInTheDocument();
    await flushBoot();
  });
});

describe("App §7.3 方案 A（Viewer 打开隐藏 BottomBar）", () => {
  const loadedStore = {
    settings: mkSettings(),
    loaded: true,
    loading: false,
    loadError: null,
    saving: false,
  };

  it("viewerOpen=true：BottomBar 隐藏、TitleBar 保留", async () => {
    useSettingsStore.setState(loadedStore);
    useLibraryStore.setState({ viewerOpen: true });
    render(<App />);
    // BottomBar 的「素材库/入库/打标」导航不渲染
    expect(screen.queryByText("素材库")).not.toBeInTheDocument();
    expect(screen.queryByText("入库")).not.toBeInTheDocument();
    expect(screen.queryByText("打标")).not.toBeInTheDocument();
    // TitleBar 始终保留
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    await flushBoot();
  });

  it("viewerOpen=false：BottomBar 正常显示", async () => {
    useSettingsStore.setState(loadedStore);
    render(<App />);
    expect(screen.getByText("素材库")).toBeInTheDocument();
    expect(screen.getByText("入库")).toBeInTheDocument();
    expect(screen.getByText("打标")).toBeInTheDocument();
    await flushBoot();
  });
});

describe("App A3-09 悬浮助手入口（PRD 第 8 节）", () => {
  const loadedStore = {
    settings: mkSettings(),
    loaded: true,
    loading: false,
    loadError: null,
    saving: false,
  };

  it("默认显示应用内悬浮助手入口", async () => {
    useSettingsStore.setState(loadedStore);
    render(<App />);
    expect(screen.getByTestId("floating-assistant-logo")).toBeInTheDocument();
    await flushBoot();
  });

  it("设置里关掉入口后不渲染，且不影响页面与底栏", async () => {
    useSettingsStore.setState({
      ...loadedStore,
      settings: { ...mkSettings(), floatingAssistant: false },
    });
    render(<App />);
    expect(screen.queryByTestId("floating-assistant-logo")).toBeNull();
    expect(screen.getByText("素材库")).toBeInTheDocument();
    await flushBoot();
  });

  it("打开大图浏览时隐藏浮层入口，避免遮挡全屏预览", async () => {
    useSettingsStore.setState(loadedStore);
    useLibraryStore.setState({ viewerOpen: true });
    render(<App />);
    expect(screen.queryByTestId("floating-assistant-logo")).toBeNull();
    await flushBoot();
  });
});

describe("App palette://updated 全局监听（FB4-03 §6.5/§10.7）", () => {
  const loadedStore = {
    settings: mkSettings(),
    loaded: true,
    loading: false,
    loadError: null,
    saving: false,
  };

  it("收到导入色板事件后定向同步色条并刷新颜色分面，不全量刷新素材库", async () => {
    useSettingsStore.setState(loadedStore);
    let handler: ((p: unknown) => void) | undefined;
    clientMocks.on.mockImplementation((_event: string, h: (p: unknown) => void) => {
      handler = h;
      return Promise.resolve(() => undefined);
    });
    render(<App />);
    // 订阅建立（"palette://updated" 事件被监听）
    await waitFor(() =>
      expect(clientMocks.on).toHaveBeenCalledWith("palette://updated", expect.any(Function)),
    );
    // 挂载 spy：当前 store 对象上的 refresh 与 refreshPaletteFields（事件触发时 getState() 返回同一对象）
    const st = useLibraryStore.getState();
    const refreshSpy = vi.spyOn(st, "refresh").mockResolvedValue(undefined);
    const fieldsSpy = vi.spyOn(st, "refreshPaletteFields").mockResolvedValue(undefined);
    const metadataSpy = vi.spyOn(useMetadataStore.getState(), "refresh").mockResolvedValue(undefined);
    // 触发事件（source 固定 import）
    handler!({
      source: "import",
      total: 3,
      success: 3,
      failed: 0,
      skipped: 0,
      updatedIds: [11, 22],
    });
    await waitFor(() => expect(fieldsSpy).toHaveBeenCalledWith([11, 22]));
    expect(metadataSpy).toHaveBeenCalled();
    expect(refreshSpy).not.toHaveBeenCalled();
  });

  it("订阅失败（非 Tauri 环境）不阻塞首屏渲染", async () => {
    useSettingsStore.setState(loadedStore);
    clientMocks.on.mockRejectedValue(new Error("no tauri"));
    render(<App />);
    // 首屏正常（TitleBar 设置按钮在），不抛错
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
  });
});

describe("App R06 统一视觉体系（助手页与素材库页共用同一壳层）", () => {
  const loadedStore = {
    settings: mkSettings(),
    loaded: true,
    loading: false,
    loadError: null,
    saving: false,
  };

  /**
   * 助手页进页时会读会话索引与恢复摘要（`@/api/conversations`、`@/api/operations` 在本文件未 mock，
   * 走被 mock 的 `@/api/client.invoke`）。给这些只读查询一个空结果，页面才停在正常空态，
   * 而不是落进 PageErrorBoundary 的错误页 —— 否则比对的就不是真实页面壳层了。
   */
  beforeEach(() => {
    const invokeMock = invoke as unknown as { mockImplementation: (fn: (command: string) => Promise<unknown>) => void };
    invokeMock.mockImplementation(async (command) => {
      if (command === "agent_host_start") {
        return {
          hostInstanceId: "test-host-instance",
          protocolVersion: 6,
          sdkVersion: "test",
          recoveredStrandedHost: false,
          modelNetworkEnabled: false,
        };
      }
      if (command === "agent_host_conversation_history") return [];
      return command === "list_conversations" || command === "list_operation_recovery" ? [] : null;
    });
  });

  /** 页面的壳层 = `<main>`；PageErrorBoundary 无异常时直接透传 children，firstElementChild 即页面根容器。 */
  function shell(): HTMLElement {
    const node = document.querySelector("main");
    expect(node).not.toBeNull();
    return node as HTMLElement;
  }

  function pageRoot(container: HTMLElement): HTMLElement {
    const node = container.firstElementChild;
    expect(node).not.toBeNull();
    return node as HTMLElement;
  }

  it("换页只换内容不换壳层：素材库页与助手页挂在同一个 main 上", async () => {
    useSettingsStore.setState(loadedStore);
    render(<App />);
    await flushBoot();

    const libraryShell = shell();
    // 壳层统一承担「撑满剩余高度 + 给底栏留位」；页面自己不再各写一层布局。
    expect(libraryShell.className).toContain("flex-1");
    expect(libraryShell.className).toContain("min-h-0");
    expect(libraryShell.className).toContain("pb-14");
    const shellClassName = libraryShell.className;
    expect(pageRoot(libraryShell).className).toContain("h-full");

    fireEvent.click(screen.getByRole("button", { name: "助手" }));
    await flushBoot();

    // 助手页没有另起一套外壳：仍是同一个 main 节点、同一套 class，当前项由底栏标出。
    expect(screen.queryByText("页面暂时无法显示")).toBeNull();
    expect(screen.getByText(/选择左侧对话，或新建一个对话开始/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "助手" })).toHaveAttribute("aria-current", "page");
    const assistantShell = shell();
    expect(assistantShell).toBe(libraryShell);
    expect(assistantShell.className).toBe(shellClassName);
    expect(pageRoot(assistantShell).className).toContain("h-full");
    await flushBoot();
  });

  it("两类页面容器只用主题语义变量，不写死字面颜色", async () => {
    useSettingsStore.setState(loadedStore);
    render(<App />);
    await flushBoot();

    // 内联样式里的颜色只能来自 CSS 变量；出现 #hex / rgb() / hsl() 就说明绕过了主题。
    const literalColor = /(#[0-9a-f]{3,8}\b|rgba?\(|hsla?\()/i;
    const inlineColors = (root: Element) =>
      [...root.querySelectorAll<HTMLElement>("[style]")]
        .map((node) => node.getAttribute("style") ?? "")
        .filter((style) => /(^|;)\s*(color|background|background-color|border-color)\s*:/i.test(style))
        .filter((style) => literalColor.test(style));
    // 语义变量 class 是统一视觉体系的落点：背景/文字/边框都写 var(--color-*)。
    // 数语义变量而不是写成「大于 0」：写死颜色的断言是「一个都不许有」（下面几处 toContain），
    // 这里要求两类容器都真的带上语义变量，数量本身不能退化成装饰性检查。
    const themeVariableClasses = (root: Element) =>
      root.querySelectorAll('[class*="var(--color-"]').length;
    const libraryPage = pageRoot(shell());
    expect(inlineColors(libraryPage)).toEqual([]);
    expect(themeVariableClasses(libraryPage)).toBeGreaterThan(0);
    // 素材库工具条（排序/尺寸档等）同样只引用主题变量。
    expect(themeVariableClasses(screen.getByTestId("toolbar-actions"))).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "助手" }));
    await flushBoot();

    const assistantPage = pageRoot(shell());
    expect(inlineColors(assistantPage)).toEqual([]);
    expect(themeVariableClasses(assistantPage)).toBeGreaterThan(0);
    // 助手页自己的入口（新建会话）也走语义变量，没有自成一套配色。
    expect(screen.getByTestId("empty-new-conversation").className).toContain(
      "var(--color-accent)",
    );
    await flushBoot();
  });
});

describe("App 助手页把常用流程/待审计划嵌进右侧栏", () => {
  /**
   * 助手页进页会读会话索引、恢复摘要；嵌进右侧栏的两个面板还会读流程列表/步骤目录与
   * 待审计划。这里统一给只读查询空结果，让页面停在正常空态而不是错误边界。
   */
  beforeEach(() => {
    const invokeMock = invoke as unknown as {
      mockImplementation: (fn: (command: string) => Promise<unknown>) => void;
    };
    invokeMock.mockImplementation(async (command) => {
      if (
        command === "list_conversations" ||
        command === "list_operation_recovery" ||
        command === "list_pending_plans" ||
        command === "list_workflows" ||
        command === "workflow_step_catalog"
      ) {
        return [];
      }
      return null;
    });
  });

  it("素材库页照常渲染全局模态框，助手页改为侧栏且不再重复渲染模态框", async () => {
    useSettingsStore.setState({
      settings: mkSettings(),
      loaded: true,
      loading: false,
      loadError: null,
      saving: false,
    });
    useWorkflowPanelStore.setState({ open: true, draft: null, draftingFrom: null, error: null });
    usePendingPlansStore.setState({ open: true, plans: [], loading: false, error: null });

    render(<App />);
    await flushBoot();

    // 非助手页：外部入口（任务卡「保存为常用流程」、设置页的待审计划）仍靠应用层模态框落地。
    expect(screen.getByRole("dialog", { name: "常用流程" })).toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "待审计划" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "助手" }));
    await flushBoot();

    // 助手页：同一份内容由右侧栏承载，模态框不再渲染——否则会盖住对话，
    // 也正是这次要把两个入口统一成「一个抽屉」的原因。
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
