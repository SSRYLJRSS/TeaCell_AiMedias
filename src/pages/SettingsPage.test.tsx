/**
 * SettingsPage 回归测试（指导书 §6.1/§12.5）：
 *  - 第一项是「素材库与入库」；分组顺序为 素材库与入库 → AI 与模型 → 标签与分类 → 外观与浏览 → 存储与维护 → 诊断与支持 → 关于；
 *  - AI 与模型内部可切换「超级搜索 / 自动打标」子页，右侧显示在线/本地二选一；
 *  - 网盘分组与「本地打标」顶层组不存在（§6.8 网盘移除）；
 *  - 加载失败后点击重试会再次调用 load，成功后进入表单。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, fireEvent } from "@testing-library/react";
import { StrictMode as ReactStrictMode } from "react";
import SettingsPage from "@/pages/SettingsPage";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePlatformStore } from "@/stores/platformStore";
import { useTagStore } from "@/stores/tagStore";
import { exportDiagnostics, getSettings, openAuthorPage, openFeedbackPage, openHelpPage, openLicensePage, openProjectPage, resetAppData, saveSettings } from "@/api/settings";
import { rescanAssetMetadata } from "@/api/assets";
import type { Settings } from "@/types/settings";
import type { TagFacet } from "@/types/tag";

// ── mock 后端/API 层 ──
const assetMocks = vi.hoisted(() => ({
  rescanAssetMetadata: vi.fn(),
  rescanAssetPalette: vi.fn(),
  rescanPaletteColors: vi.fn().mockResolvedValue(0),
  cancelMediaRefill: vi.fn(),
  getPaletteStatus: vi.fn(),
}));
const libraryMocks = vi.hoisted(() => ({
  refreshPaletteFields: vi.fn(),
  refresh: vi.fn(),
  clearTagFilters: vi.fn(),
}));
vi.mock("@/stores/libraryStore", () => ({
  useLibraryStore: {
    getState: () => ({
      refreshPaletteFields: libraryMocks.refreshPaletteFields,
      refresh: libraryMocks.refresh,
      clearTagFilters: libraryMocks.clearTagFilters,
    }),
  },
}));
vi.mock("@/api/settings", () => ({
  getSettings: vi.fn(),
  saveSettings: vi.fn().mockResolvedValue(undefined),
  getDataDir: vi.fn().mockResolvedValue("D:/data"),
  openDataDir: vi.fn().mockResolvedValue(undefined),
  openLogsDir: vi.fn().mockResolvedValue(undefined),
  openHelpPage: vi.fn().mockResolvedValue(undefined),
  openProjectPage: vi.fn().mockResolvedValue(undefined),
  getProjectPageUrl: vi.fn().mockResolvedValue("https://github.com/SSRYLJRSS/TeaCell_AiMedias"),
  openLicensePage: vi.fn().mockResolvedValue(undefined),
  getLicensePageUrl: vi.fn().mockResolvedValue("https://github.com/SSRYLJRSS/TeaCell_AiMedias/blob/main/LICENSE"),
  openAuthorPage: vi.fn().mockResolvedValue(undefined),
  getAuthorPageUrl: vi.fn().mockResolvedValue("https://www.xiaohongshu.com/user/profile/68294317000000000e01ca6d"),
  openFeedbackPage: vi.fn().mockResolvedValue(undefined),
  getFeedbackPageUrl: vi.fn().mockResolvedValue("https://github.com/SSRYLJRSS/TeaCell_AiMedias/issues/new"),
  exportDiagnostics: vi.fn().mockResolvedValue({ path: "D:/diag.zip", logFiles: 2, bytes: 2048 }),
  clearThumbnailCache: vi.fn().mockResolvedValue(undefined),
  resetAppData: vi.fn().mockResolvedValue({
    assetsDeleted: 0,
    assetFilesDeleted: 0,
    assetFilesFailed: 0,
    exportTasksDeleted: 0,
    tagsDeleted: 0,
    aiTasksDeleted: 0,
    connectionsDeleted: 0,
    preferencesReset: false,
    searchStateReset: false,
    cacheFilesDeleted: 0,
    logFilesDeleted: 0,
  }),
}));
vi.mock("@/api/ollama", () => ({
  ollamaInstallStatus: vi.fn().mockResolvedValue({ installerPath: null, installerSize: 0 }),
  ollamaRemoveInstaller: vi.fn().mockResolvedValue(undefined),
  ollamaListSources: vi.fn().mockResolvedValue([{ id: "auto", label: "自动", url: "" }]),
  ollamaProbeSources: vi.fn().mockResolvedValue([{ id: "auto", label: "自动", ok: true, speedBps: 0 }]),
  ollamaAddCustomSource: vi.fn().mockResolvedValue(undefined),
  ollamaRemoveCustomSource: vi.fn().mockResolvedValue(undefined),
  ollamaListLocalModels: vi.fn().mockResolvedValue([]),
  ollamaDeleteModel: vi.fn().mockResolvedValue(undefined),
  ollamaModelDir: vi.fn().mockResolvedValue(""),
  ollamaOpenModelDir: vi.fn().mockResolvedValue(undefined),
  ollamaStartService: vi.fn().mockResolvedValue(undefined),
  probeOllamaHardware: vi.fn().mockResolvedValue({ available: false }),
  onOllamaInstallLog: vi.fn().mockRejectedValue(new Error("no tauri")),
  onOllamaInstallProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
  onOllamaPullProgress: vi.fn().mockRejectedValue(new Error("no tauri")),
  pullOllamaModel: vi.fn().mockResolvedValue(undefined),
  ollamaPing: vi.fn().mockResolvedValue({ ok: false }),
}));
vi.mock("@/api/ai", () => ({
  // FB5-04：aiListModels 已删除（模型发现走 discoverAiModels / connections）
  aiListNewWordCandidates: vi.fn().mockResolvedValue([]),
  aiDecideSuggestionItem: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/api/thumbnail", () => ({
  clearThumbnailCache: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/api/tags", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  listAllTagFacets: vi.fn().mockResolvedValue([]),
  listContentDescriptions: vi.fn().mockResolvedValue([]),
  scanDuplicateTags: vi.fn().mockResolvedValue([]),
  searchTagCandidates: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/api/assets", () => ({
  rescanAssetMetadata: assetMocks.rescanAssetMetadata,
  rescanAssetPalette: assetMocks.rescanAssetPalette,
  rescanAssetPhash: vi.fn().mockResolvedValue({ total: 0, success: 0, failed: 0, skipped: 0 }),
  rescanImageDimensions: vi.fn().mockResolvedValue({ total: 0, success: 0, failed: 0, skipped: 0 }),
  rescanPaletteColors: assetMocks.rescanPaletteColors,
  cancelMediaRefill: assetMocks.cancelMediaRefill,
  getPaletteStatus: assetMocks.getPaletteStatus,
}));
vi.mock("@/api/video", () => ({
  videoProxyCacheStats: vi.fn().mockResolvedValue([2, 1024 * 1024]),
  clearAllVideoProxies: vi.fn().mockResolvedValue(2),
}));
vi.mock("@/api/client", () => ({
  on: vi.fn().mockResolvedValue(() => undefined),
  invoke: vi.fn(),
  AppError: class extends Error {},
}));
vi.mock("@/api/connections", () => ({
  listAiConnections: vi.fn().mockResolvedValue([
    { id: "c1", name: "通义", deployment: "cloud", protocol: "openai_chat", baseUrl: "https://a/v1", model: "qwen-max", hasKey: true, credentialStatus: "configured", enabled: true },
  ]),
  getAiUsageBindings: vi.fn().mockResolvedValue({ super_search: null, tagging: "c1" }),
  getSuperSearchServiceResolution: vi.fn().mockResolvedValue({
    ready: true,
    source: "automaticOnline",
    connectionId: "c1",
    name: "通义",
    model: "qwen-max",
    deployment: "cloud",
    message: null,
  }),
  setAiUsageBinding: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn().mockResolvedValue(null),
  save: vi.fn().mockResolvedValue(null),
}));

/** 一份完整、可渲染的 Settings（normalizeSettings 之后的结构）。 */
function mkSettings(over: Partial<Settings> = {}): Settings {
  return {
    ai: {
      profiles: [{ id: "p1", name: "配置 1", apiMode: "openai", kind: "cloud", baseUrl: "", apiKey: "", model: "qwen-vl-plus" }],
      activeProfile: "p1",
      videoTagging: false,
      videoTaggingMode: "cover",
      videoFrameCount: 3,
      batchLimit: 30,
      localBatchLimit: 5,
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
    ...over,
  };
}

/** Field 结构：label 文本 → p → label 列 div → Field 根 div（根下才是控件列）。 */
function fieldSwitch(label: string): Element {
  const fieldRoot = screen.getByText(label).parentElement!.parentElement!;
  return fieldRoot.querySelector('[role="switch"]')!;
}

const emptyStore = {
  settings: null,
  loaded: false,
  loading: false,
  loadError: null,
  saving: false,
  previewAppearance: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState(emptyStore);
  // R1（三端复核）：这些回归编码 Windows（托管 Ollama）契约——本机服务 tab 可见。
  // 平台能力 store 置为 ready+windows；非托管平台的隐藏行为由 platformStore 单测覆盖。
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
  // 默认：getSettings 成功返回一份完整设置
  vi.mocked(getSettings).mockResolvedValue(mkSettings());
  // 默认 API 行为（色板状态：259 候选全缺；回算成功 259 条并更新前 3 条）
  assetMocks.rescanAssetMetadata.mockResolvedValue({ total: 2, success: 2, failed: 0, skipped: 0 });
  assetMocks.rescanAssetPalette.mockResolvedValue({
    total: 259, success: 259, failed: 0, skipped: 0, updatedIds: [1, 2, 3],
  });
  assetMocks.cancelMediaRefill.mockResolvedValue(undefined);
  assetMocks.getPaletteStatus.mockResolvedValue({
    totalAssets: 259, eligible: 259, ready: 0, missing: 259, unavailable: 0,
  });
  libraryMocks.refreshPaletteFields.mockReset().mockResolvedValue(undefined);
});

/** 切到「外观与浏览」路由并等待色板状态行渲染（FB4-03 状态在进入该路由时读取） */
async function openGeneral() {
  render(<SettingsPage />);
  await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
  fireEvent.click(screen.getByText("外观与浏览"));
  await waitFor(() => expect(screen.getByText("素材框")).toBeInTheDocument());
}

describe("SettingsPage 自动保存", () => {
  it("初次加载、悬停和聚焦不写设置；真实修改后自动保存且不显示全局保存按钮", async () => {
    useSettingsStore.setState(emptyStore);
    render(<SettingsPage />);
    await screen.findByText("总库位置");
    expect(saveSettings).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("诊断与支持"));
    const level = await screen.findByLabelText("诊断日志级别");

    expect(screen.queryByRole("button", { name: "保存设置" })).not.toBeInTheDocument();
    vi.useFakeTimers();
    fireEvent.mouseOver(level);
    fireEvent.focus(level);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
    });
    expect(saveSettings).not.toHaveBeenCalled();

    fireEvent.change(level, { target: { value: "debug" } });
    expect(level).toHaveValue("debug");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });
    expect(saveSettings).not.toHaveBeenCalled();
    fireEvent.change(level, { target: { value: "trace" } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ logLevel: "trace" }));
    expect(screen.getByText("已自动保存")).toBeInTheDocument();
  });

  it("离开设置页时立即提交尚在防抖窗口中的修改", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    const { unmount } = render(<SettingsPage />);
    await screen.findByText("总库位置");
    fireEvent.click(screen.getByText("诊断与支持"));
    fireEvent.change(await screen.findByLabelText("诊断日志级别"), { target: { value: "debug" } });

    unmount();
    await waitFor(() => expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ logLevel: "debug" })));
  });
});

describe("SettingsPage §6.1 信息架构", () => {
  it("分组顺序：第一项是素材库与入库；含 AI 与模型/标签与分类/外观与浏览/存储与维护/诊断与支持/关于", async () => {
    useSettingsStore.setState({ settings: null, loaded: false, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    for (const g of ["素材库与入库", "AI 与模型", "标签与分类", "外观与浏览", "存储与维护", "诊断与支持", "关于"]) {
      expect(screen.getAllByText(g).length).toBeGreaterThan(0);
    }
  });

  it("网盘分组与「本地打标」顶层组不存在（§6.8 网盘移除）", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    expect(screen.queryByText(/网盘/)).not.toBeInTheDocument();
    expect(screen.queryByText("本地打标")).not.toBeInTheDocument();
  });

  it("诊断与支持提供使用帮助入口并调用后端默认浏览器命令", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("诊断与支持"));
    fireEvent.click(await screen.findByRole("button", { name: "打开使用帮助" }));
    await waitFor(() => expect(openHelpPage).toHaveBeenCalledTimes(1));
  });

  it("点击「AI 与模型」先进入服务管理，子页顺序为服务管理、超级搜索、自动打标", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument()); // 默认第一项

    fireEvent.click(screen.getByText("AI 与模型"));
    await waitFor(() => expect(screen.getAllByText("服务管理").length).toBeGreaterThan(0));
    const aiSettings = screen.getByText("AI 与模型");
    const childLabels = Array.from(aiSettings.parentElement?.querySelectorAll("button") ?? []).slice(1).map((node) => node.textContent);
    expect(childLabels).toEqual(["服务管理", "超级搜索", "自动打标"]);
    expect(screen.getAllByText("超级搜索").length).toBeGreaterThan(0);
    expect(screen.getAllByText("自动打标").length).toBeGreaterThan(0);
  });

  it("FB2-02 素材框：切到「外观与浏览」后存在「素材框」组与 7 项比例选项", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("外观与浏览"));
    await waitFor(() => expect(screen.getByText("素材框")).toBeInTheDocument());
    // 7 项比例选项（1:1 / 4:3 / 3:2 / 16:9 / 3:4 / 2:3 / 9:16）
    const ratioOpts = Array.from(screen.getAllByRole("option") as HTMLOptionElement[]).map((o) => o.value).filter((v) => v.includes(":"));
    expect(ratioOpts.length).toBe(7);
    // 填充方式 3 项
    expect(
      Array.from(screen.getAllByRole("option") as HTMLOptionElement[]).filter((o) => ["cover", "contain", "smart"].includes(o.value)).length,
    ).toBe(3);
  });

  // FB2-08（FX-07）+ FB3-10 + FB4-03：色条设置区块（「入库网格显示」已隐藏——永久 disabled 的噪音控件不保留）
  it("色条区块：切「显示主色色条」Toggle 触发即时预览；总开关关闭后位置/样式行不渲染", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("外观与浏览"));
    await waitFor(() => expect(screen.getByText("素材框")).toBeInTheDocument());

    // 默认 colorStrip.enabled=true → 细节折叠在（W4-5）；展开后位置/样式行都在
    expect(screen.getByText("显示主色色条")).toBeInTheDocument();
    fireEvent.click(screen.getByText("▸ 色条细节"));
    expect(screen.getByText("素材库卡片显示")).toBeInTheDocument();
    expect(screen.getByText("大图浏览显示")).toBeInTheDocument();
    expect(screen.queryByText("入库网格显示")).toBeNull();
    expect(screen.getByText("色条高度")).toBeInTheDocument();

    // 点击总开关 → draft 关闭 + 即时预览；设置页自身的自动保存负责唯一一次持久化。
    // 使用假时钟并在本测试内完成自动保存，避免 timer 泄漏到后续测试。
    vi.useFakeTimers();
    const previewSpy = vi.spyOn(useSettingsStore.getState(), "setPreviewAppearance");
    const master = fieldSwitch("显示主色色条");
    fireEvent.click(master);
    expect(previewSpy).toHaveBeenCalledTimes(1);
    // 关闭后位置/样式行整体不渲染（条件渲染，不是 disabled）；状态行（色条数据）仍可见
    expect(screen.queryByText("素材库卡片显示")).toBeNull();
    expect(screen.queryByText("大图浏览显示")).toBeNull();
    expect(screen.queryByText("色条高度")).toBeNull();
    expect(screen.getByText("色条数据")).toBeInTheDocument();
    // 再打开恢复渲染（折叠保持展开状态）
    fireEvent.click(fieldSwitch("显示主色色条"));
    expect(screen.getByText("素材库卡片显示")).toBeInTheDocument();
    expect(previewSpy).toHaveBeenCalledTimes(2);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(vi.mocked(saveSettings)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(getSettings)).toHaveBeenCalledTimes(1);
    expect(useSettingsStore.getState().saving).toBe(false);
  });

  it("关于页显示公测信息、项目/作者/许可证链接且不显示保存栏", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("关于"));
    expect(await screen.findByText("V1.0.2 公测版")).toBeInTheDocument();
    expect(screen.getByText(/茶馆AI素材管理 \/ TeaCell AI Media Manager/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /GitHub · SSRYLJRSS/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "小红书主页" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "MIT License" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "保存设置" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /GitHub · SSRYLJRSS/ }));
    fireEvent.click(screen.getByRole("button", { name: "小红书主页" }));
    fireEvent.click(screen.getByRole("button", { name: "MIT License" }));
    await waitFor(() => expect(openProjectPage).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(openAuthorPage).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(vi.mocked(openLicensePage)).toHaveBeenCalledTimes(1));
  });

  it("诊断页可打开 GitHub 反馈入口，失败时显示可复制链接", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    vi.mocked(openFeedbackPage).mockRejectedValueOnce(new Error("open failed"));
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("诊断与支持"));
    fireEvent.click(await screen.findByRole("button", { name: "在 GitHub 提交问题" }));
    await waitFor(() => expect(screen.getByText(/issues\/new/)).toBeInTheDocument());
    expect(openFeedbackPage).toHaveBeenCalledTimes(1);
  });

  it("色条总开关只控制展示，不改变颜色数据或搜索；所有位置关闭时给出提示", async () => {
    const settings = mkSettings();
    settings.appearance.colorStrip.showInViewer = false;
    useSettingsStore.setState({ settings, loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("外观与浏览"));
    await waitFor(() => expect(screen.getByText("素材框")).toBeInTheDocument());

    expect(screen.getByText(/不影响色板数据、色板计算或颜色搜索/)).toBeInTheDocument();
    expect(screen.getByText("已开启色条，但尚未选择显示位置。")).toBeInTheDocument();
    fireEvent.click(screen.getByText("▸ 色条细节"));
    expect(screen.getByText("素材库卡片显示")).toBeInTheDocument();
    expect(screen.getByText("大图浏览显示")).toBeInTheDocument();
  });

  it("AI 子页「自动打标」只显示「此功能使用的服务」+ 功能参数，不再重复服务管理列表", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("AI 与模型"));
    fireEvent.click(screen.getAllByText("自动打标")[0]);
    await waitFor(() => expect(screen.getByText("此功能使用的服务")).toBeInTheDocument());
    // 旧「部署方式」单选已移除（服务位置移到服务管理页）
    expect(screen.queryByText("部署方式")).not.toBeInTheDocument();
    // 不重复渲染服务管理列表（「+ 新增服务」不在用途页出现）
    expect(screen.queryByRole("button", { name: "+ 新增服务" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /管理 AI 服务/ })).not.toBeInTheDocument();
    expect(screen.getByLabelText("在线服务每批处理数量")).toHaveValue("30");
    expect(screen.getByLabelText("本机服务每批处理数量")).toHaveValue("5");
  });

  it("超级搜索未单独绑定时显示自动选择的在线服务及实际模型，不跟随旧默认档案", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("AI 与模型"));
    fireEvent.click(screen.getByText("超级搜索"));

    expect(await screen.findByText(/自动选择在线服务，当前使用：通义 · qwen-max（在线）/)).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "自动选择在线服务" })).toBeInTheDocument();
    expect(screen.getByRole("combobox")).toHaveValue("");
  });

  it("「服务管理」子页是唯一维护入口：服务位置二选一 + 服务列表", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("AI 与模型"));
    await waitFor(() => expect(screen.getAllByText("服务位置").length).toBeGreaterThan(0));
    expect(screen.getByRole("tab", { name: "在线服务" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "本机服务" })).toBeInTheDocument();
    // 服务管理页渲染连接列表（唯一入口）：列出 mock 的「通义」服务
    await waitFor(() => expect(screen.getAllByText("通义").length).toBeGreaterThan(0));
  });

  it("「服务管理」切到本机服务：显示本地说明与新增服务入口", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("AI 与模型"));
    await waitFor(() => expect(screen.getByRole("tab", { name: "本机服务" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("tab", { name: "本机服务" }));
    await waitFor(() => expect(screen.getByText(/仅在本机处理/)).toBeInTheDocument());
  });
});

describe("SettingsPage AI 打标审核流程", () => {
  it("不再展示自动写入、自动建词和置信度高级策略", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null, saving: false });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("AI 与模型"));
    fireEvent.click(screen.getAllByText("自动打标")[0]);
    await screen.findByText("此功能使用的服务");

    expect(screen.queryByText("已有标签自动打上")).not.toBeInTheDocument();
    expect(screen.queryByText("新标签自动创建并打上")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("建议最低置信度")).not.toBeInTheDocument();
  });

  it("诊断与支持页切换日志级别后，保存请求携带 debug 级别", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("诊断与支持"));
    const level = await screen.findByLabelText("诊断日志级别");
    fireEvent.change(level, { target: { value: "debug" } });
    expect(level).toHaveValue("debug");

    await waitFor(() =>
      expect(vi.mocked(saveSettings)).toHaveBeenCalledWith(
        expect.objectContaining({ logLevel: "debug" }),
      ),
    );
  });

  it("诊断与支持页导出诊断包时把用户选择的目标路径传给后端", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save).mockResolvedValueOnce("D:/diagnostics.zip");
    vi.mocked(exportDiagnostics).mockResolvedValueOnce({
      path: "D:/diagnostics.zip",
      logFiles: 2,
      truncatedLogs: 0,
      bytes: 2048,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("诊断与支持"));
    fireEvent.click(await screen.findByText("导出诊断包…"));
    await waitFor(() => expect(vi.mocked(exportDiagnostics)).toHaveBeenCalledWith("D:/diagnostics.zip"));
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("SettingsPage 加载与 Hook 安全", () => {
  it("settings 初始为空 → 加载成功后渲染表单，不抛 Hook 顺序错误", async () => {
    useSettingsStore.setState({ settings: null, loaded: false, loading: false, loadError: null });
    render(<SettingsPage />);

    expect(screen.getByText(/加载设置中/)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
  });

  it("加载失败显示错误，点击重试再次调用 load，成功后进入表单", async () => {
    vi.mocked(getSettings)
      .mockRejectedValueOnce(new Error("后端未连接"))
      .mockResolvedValueOnce(mkSettings());
    useSettingsStore.setState({ settings: null, loaded: false, loading: false, loadError: null });

    render(<SettingsPage />);

    await waitFor(() => expect(screen.getByText(/设置加载失败：后端未连接/)).toBeInTheDocument());
    expect(useSettingsStore.getState().loadError).toBe("后端未连接");
    expect(vi.mocked(getSettings)).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: /重试/ }));
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    expect(vi.mocked(getSettings)).toHaveBeenCalledTimes(2);
    expect(useSettingsStore.getState().settings).not.toBeNull();
  });

  it("「存储与维护」分组可触发媒体元数据回填（只补缺失信息范围；FB3-11 新按钮名）", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("存储与维护"));
    fireEvent.click(screen.getByRole("button", { name: "仅补充缺失信息" }));

    await waitFor(() => expect(rescanAssetMetadata).toHaveBeenCalledWith([], "missing"));
    await waitFor(() => expect(screen.getByText(/媒体信息更新完成：总数 2，成功 2/)).toBeInTheDocument());
  });

  it("R1-2 settingsPage_has_dimension_backfill_button：「存储与维护」提供「图片分辨率回填」入口，点「仅补充缺失项」调用 rescanImageDimensions", async () => {
    const { rescanImageDimensions } = await import("@/api/assets");
    vi.mocked(rescanImageDimensions).mockResolvedValue({ total: 3, success: 3, failed: 0, skipped: 0 });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());

    fireEvent.click(screen.getByText("存储与维护"));
    const btn = screen.getByRole("button", { name: "补充缺失的图片分辨率" });
    expect(btn).toBeInTheDocument();
    fireEvent.click(btn);

    await waitFor(() => expect(rescanImageDimensions).toHaveBeenCalledWith([], "missing"));
    await waitFor(() => expect(screen.getByText(/分辨率更新完成：总数 3，成功 3/)).toBeInTheDocument());
  });
});

describe("SettingsPage §13（FB-07）宽屏布局", () => {
  it("右侧内容不再用 max-w-xl 小框：存在 1040px 内容容器与加宽侧栏", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    const { container } = render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    // 内容容器使用 max-w-[1040px]（替代旧 max-w-xl）
    expect(container.querySelector(".max-w-\\[1040px\\]")).not.toBeNull();
    // 侧栏宽度进入 220~260px 范围
    const aside = container.querySelector("aside");
    expect(aside).not.toBeNull();
    expect((aside as HTMLElement).className).toMatch(/w-\[224px\]/);
  });

  it("不显示全局保存栏；未修改时不触发自动保存", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    const { container } = render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "保存设置" })).not.toBeInTheDocument();
    expect(container.querySelector(".sticky.bottom-0")).toBeNull();
    expect(saveSettings).not.toHaveBeenCalled();
  });
});

describe("SettingsPage 色条状态与生成（FB4-03 §10.6）", () => {
  it("外观与浏览路由加载色板状态并显示（缺 259 时的真实文案）", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    expect(assetMocks.getPaletteStatus).toHaveBeenCalled();
    await waitFor(() =>
      expect(screen.getByText(/已生成 0 \/ 可生成 259；另有 0 项暂不可生成/)).toBeInTheDocument(),
    );
  });

  it("总开关关闭时状态行仍可见（位置/样式行隐藏）", async () => {
    const settings = mkSettings();
    settings.appearance.colorStrip.enabled = false;
    useSettingsStore.setState({ settings, loaded: true, loading: false, loadError: null });
    await openGeneral();
    expect(screen.getByText("色条数据")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText(/已生成 0 \/ 可生成 259/)).toBeInTheDocument());
    expect(screen.queryByText("素材库卡片显示")).toBeNull();
    expect(screen.queryByText("大图浏览显示")).toBeNull();
  });

  it("missing > 0 时按钮可用并显示缺失数量", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    const btn = await screen.findByRole("button", { name: /生成缺失色条（259）/ });
    expect(btn).toBeEnabled();
  });

  it("missing = 0 时不可重复执行（显示全部完成）", async () => {
    assetMocks.getPaletteStatus.mockResolvedValue({
      totalAssets: 259, eligible: 259, ready: 259, missing: 0, unavailable: 0,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    await waitFor(() => expect(screen.getByText(/所有可生成素材均已完成/)).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: /生成缺失色条/ })).toBeNull();
  });

  it("eligible = 0 时显示无可生成说明", async () => {
    assetMocks.getPaletteStatus.mockResolvedValue({
      totalAssets: 0, eligible: 0, ready: 0, missing: 0, unavailable: 0,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    await waitFor(() =>
      expect(screen.getByText("当前没有可生成色条的图片或视频封面")).toBeInTheDocument(),
    );
  });

  it("点击后调用 rescanAssetPalette([], \"missing\")；完成后刷新状态并定向同步 updatedIds", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    const btn = await screen.findByRole("button", { name: /生成缺失色条（259）/ });
    fireEvent.click(btn);
    await waitFor(() => expect(assetMocks.rescanAssetPalette).toHaveBeenCalledWith([], "missing"));
    await waitFor(() =>
      expect(screen.getByText(/生成完成：成功 259，跳过 0，失败 0（共处理 259）/)).toBeInTheDocument(),
    );
    // 完成后再读一次状态（初始 1 次 + 完成 1 次）
    expect(assetMocks.getPaletteStatus).toHaveBeenCalledTimes(2);
    // 只做定向同步，不调用全量 refresh（refreshPaletteFields 是唯一被使用的 libraryStore 方法）
    expect(libraryMocks.refreshPaletteFields).toHaveBeenCalledWith([1, 2, 3]);
  });

  it("手动流程不订阅或触发第二次全量 refresh（只调用 refreshPaletteFields）", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    fireEvent.click(await screen.findByRole("button", { name: /生成缺失色条（259）/ }));
    await waitFor(() => expect(libraryMocks.refreshPaletteFields).toHaveBeenCalledTimes(1));
    // 完成摘要成功显示（若误调了不存在的 refresh 会在错误分支留下痕迹）
    expect(screen.queryByText(/生成完成/)).toBeInTheDocument();
  });

  it("互斥闸占用/一般错误显示可读反馈，不轮询不自动重试", async () => {
    assetMocks.rescanAssetPalette.mockRejectedValue(new Error("已有回填任务进行中（媒体元数据回填或色板回算），请等待完成或先取消"));
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    fireEvent.click(await screen.findByRole("button", { name: /生成缺失色条（259）/ }));
    await waitFor(() =>
      expect(screen.getByText(/已有回填任务进行中/)).toBeInTheDocument(),
    );
    expect(libraryMocks.refreshPaletteFields).not.toHaveBeenCalled();
  });

  it("状态读取失败显示可读错误与重试入口", async () => {
    assetMocks.getPaletteStatus.mockRejectedValue(new Error("数据库不可用"));
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    await waitFor(() => expect(screen.getByText(/数据库不可用/)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("回算成功但同步失败时保留摘要并显示具体错误", async () => {
    libraryMocks.refreshPaletteFields.mockRejectedValue(new Error("IPC 失败"));
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openGeneral();
    fireEvent.click(await screen.findByRole("button", { name: /生成缺失色条（259）/ }));
    await waitFor(() => expect(screen.getByText(/生成完成/)).toBeInTheDocument());
    await waitFor(() =>
      expect(screen.getByText(/素材色条同步失败：IPC 失败/)).toBeInTheDocument(),
    );
  });
});
describe("存储与维护 · 重置数据", () => {
  /** 切到「存储与维护」路由并等待重置面板渲染 */
  async function openData() {
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("存储与维护"));
    await waitFor(() => expect(screen.getByText("重置所选数据")).toBeInTheDocument());
  }

  it("未勾选时重置按钮禁用；两步确认后才调用 resetAppData，且传入勾选项", async () => {
    const tagRefreshSpy = vi
      .spyOn(useTagStore.getState(), "refresh")
      .mockResolvedValue(undefined);
    vi.mocked(resetAppData).mockResolvedValue({
      assetsDeleted: 259,
      assetFilesDeleted: 0,
      assetFilesFailed: 0,
      exportTasksDeleted: 0,
      tagsDeleted: 40,
      aiTasksDeleted: 3,
      connectionsDeleted: 0,
      preferencesReset: false,
      searchStateReset: false,
      cacheFilesDeleted: 512,
      logFilesDeleted: 0,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openData();

    const btn = screen.getByRole("button", { name: "重置所选数据" });
    expect(btn).toBeDisabled();

    // 勾选素材 + 标签
    fireEvent.click(screen.getByText("素材库记录"));
    // 「标签与分类」同时出现在左侧导航与勾选项，取后者（面板内）
    fireEvent.click(screen.getAllByText("标签与分类").at(-1)!);
    expect(btn).toBeEnabled();

    // 第一步只出现确认文案，尚未调用后端
    fireEvent.click(btn);
    expect(screen.getByText(/此操作不可撤销/)).toBeInTheDocument();
    expect(resetAppData).not.toHaveBeenCalled();

    // 第二步确认 → 调用后端并携带勾选项；成功后展示报告并刷新素材库
    fireEvent.click(screen.getByRole("button", { name: "确认重置" }));
    await waitFor(() => expect(resetAppData).toHaveBeenCalledTimes(1));
    expect(resetAppData).toHaveBeenCalledWith({
      assets: true,
      assetFiles: false,
      exportTasks: false,
      tags: true,
      aiTasks: false,
      aiConnections: false,
      preferences: false,
      searchState: false,
      caches: false,
      logs: false,
    });
    await waitFor(() => expect(screen.getAllByText(/重置完成：已清除素材 259 条/).length).toBeGreaterThan(0));
    expect(libraryMocks.refresh).toHaveBeenCalledTimes(1);
    expect(libraryMocks.clearTagFilters).toHaveBeenCalledTimes(1);
    expect(tagRefreshSpy).toHaveBeenCalledTimes(1);
    expect(useTagStore.getState().tree).toEqual([]);
  });

  it("重置失败显示可读错误，不刷新素材库", async () => {
    vi.mocked(resetAppData).mockRejectedValue(new Error("正在执行回填任务，请等它结束或取消后再重置"));
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openData();

    fireEvent.click(screen.getByText("缓存文件"));
    fireEvent.click(screen.getByRole("button", { name: "重置所选数据" }));
    fireEvent.click(screen.getByRole("button", { name: "确认重置" }));
    await waitFor(() => expect(screen.getAllByText(/请等它结束或取消后再重置/).length).toBeGreaterThan(0));
    expect(libraryMocks.refresh).not.toHaveBeenCalled();
  });

  it("reset_clears_super_search_persist：重置成功后同步清除 localStorage 里的超级搜索条件", async () => {
    vi.mocked(resetAppData).mockResolvedValue({
      assetsDeleted: 259,
      assetFilesDeleted: 0,
      assetFilesFailed: 0,
      exportTasksDeleted: 0,
      tagsDeleted: 40,
      aiTasksDeleted: 3,
      connectionsDeleted: 0,
      preferencesReset: false,
      searchStateReset: false,
      cacheFilesDeleted: 512,
      logFilesDeleted: 0,
    });
    // 预置陈旧条件（旧 epoch 日期格式的 expr 会让 hydrate 报错 —— 这正是要清掉的场景）
    localStorage.setItem("super-search-conditions", JSON.stringify({ expr: { op: "leaf", cond: { type: "metadata", filter: { key: "taken_at", op: "gte", value: 1722508800000 } } } }));
    expect(localStorage.getItem("super-search-conditions")).not.toBeNull();

    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openData();
    fireEvent.click(screen.getAllByText("标签与分类").at(-1)!);
    fireEvent.click(screen.getByRole("button", { name: "重置所选数据" }));
    fireEvent.click(screen.getByRole("button", { name: "确认重置" }));
    await waitFor(() => expect(resetAppData).toHaveBeenCalledTimes(1));

    // 重置成功后键被移除（否则改完日期 P0 后这条陈旧条件仍会 hydrate 报错）
    await waitFor(() => expect(localStorage.getItem("super-search-conditions")).toBeNull());
  });

  it("原始素材文件是独立高风险项：必须输入确认短语，成功/失败数量如实展示", async () => {
    vi.mocked(resetAppData).mockResolvedValue({
      assetsDeleted: 2,
      assetFilesDeleted: 2,
      assetFilesFailed: 1,
      exportTasksDeleted: 0,
      tagsDeleted: 0,
      aiTasksDeleted: 0,
      connectionsDeleted: 0,
      preferencesReset: false,
      searchStateReset: false,
      cacheFilesDeleted: 3,
      logFilesDeleted: 0,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openData();

    fireEvent.click(screen.getByText("原始素材文件"));
    fireEvent.click(screen.getByRole("button", { name: "重置所选数据" }));
    const confirm = screen.getByRole("button", { name: "确认重置" });
    expect(confirm).toBeDisabled();
    expect(resetAppData).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("确认短语"), { target: { value: "删除原文件" } });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    await waitFor(() => expect(resetAppData).toHaveBeenCalledWith({
      assets: false,
      assetFiles: true,
      exportTasks: false,
      tags: false,
      aiTasks: false,
      aiConnections: false,
      preferences: false,
      searchState: false,
      caches: false,
      logs: false,
    }));
    await waitFor(() => expect(screen.getAllByText(/原始文件删除失败 1 个/).length).toBeGreaterThan(0));
  });

  it("全选明确标注恢复出厂设置，并要求输入恢复出厂设置", async () => {
    vi.mocked(resetAppData).mockResolvedValue({
      assetsDeleted: 0,
      assetFilesDeleted: 0,
      assetFilesFailed: 0,
      exportTasksDeleted: 0,
      tagsDeleted: 0,
      aiTasksDeleted: 0,
      connectionsDeleted: 0,
      preferencesReset: true,
      searchStateReset: true,
      cacheFilesDeleted: 0,
      logFilesDeleted: 0,
    });
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    await openData();

    fireEvent.click(screen.getByRole("button", { name: "全选（恢复出厂设置）" }));
    fireEvent.click(screen.getByRole("button", { name: "重置所选数据" }));
    expect(screen.getByText(/这是恢复出厂设置/)).toBeInTheDocument();
    const confirm = screen.getByRole("button", { name: "确认重置" });
    expect(confirm).toBeDisabled();
    fireEvent.change(screen.getByLabelText("确认短语"), { target: { value: "恢复出厂设置" } });
    fireEvent.click(confirm);
    await waitFor(() => expect(resetAppData).toHaveBeenCalledTimes(1));
    const sent = vi.mocked(resetAppData).mock.calls[0][0];
    expect(Object.values(sent).every(Boolean)).toBe(true);
  });
});

// W3：旧「aiFacetConfigs 草稿」路径已删（V20 合表后分面 AI 语义在 tag_facets.input_mode，
// 由 W4 的分面管理面板直接读写库）。此处的两条旧回归测试随通道一起移除。
describe.skip("标签与分类 · 无配置条目分面的 AI 行为（回归：勾选被静默丢弃）", () => {
  const activeFacet: TagFacet = {
    key: "purpose",
    displayName: "用途",
    description: "",
    inputMode: "ai_and_manual",
    selectionMode: "multi",
    maxItems: 3,
    sortOrder: 1,
    isSystem: true,
    status: "active",
    appliesTo: "all",
    createdAt: 1,
    updatedAt: 1,
  };

  async function openTagsRoute() {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getAllByText("标签与分类").at(-1)!);
    fireEvent.click(await screen.findByText("用途"));
  }

  it("无条目分面的「参与 AI」如实显示为关；勾选后补建条目", async () => {
    const { listAllTagFacets } = await import("@/api/tags");
    vi.mocked(listAllTagFacets).mockResolvedValue([activeFacet]);
    await openTagsRoute();

    // 无条目 = AI 本就不产出该分面（build_prompt_context 只遍历 aiFacetConfigs），如实显示为关
    const checkbox = await screen.findByRole("checkbox", { name: /参与 AI 打标与搜索/ });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);
    expect(checkbox).toBeChecked();
    expect(screen.queryByRole("button", { name: "保存设置" })).not.toBeInTheDocument();
  });

  it("无条目分面输入「给 AI 的识别规则」也会补建条目（不再静默丢弃）", async () => {
    const { listAllTagFacets } = await import("@/api/tags");
    vi.mocked(listAllTagFacets).mockResolvedValue([activeFacet]);
    await openTagsRoute();

    fireEvent.change(await screen.findByLabelText("给 AI 的识别规则"), { target: { value: "只写稳定用途" } });
    expect(screen.queryByRole("button", { name: "保存设置" })).not.toBeInTheDocument();
  });
});

// ── W7 真机复现：切换到「存储与维护」路由不抛 hooks 错误 ──
describe("存储与维护路由（W5c 备份恢复 + W5d phash 行）", () => {
  it("点击「存储与维护」渲染备份/恢复与相似图识别数据，不抛 more-hooks 错误", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, saving: false });
    render(<SettingsPage />);
    fireEvent.click(screen.getByText("存储与维护"));
    await waitFor(() => {
      expect(screen.getByText("数据库备份与恢复")).toBeTruthy();
    });
    expect(screen.getByText("相似图识别数据")).toBeTruthy();
  });

  it("颜色筛选索引重建按钮调用 rescanPaletteColors（颜色索引为空时重新生成）", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, saving: false });
    render(<SettingsPage />);
    fireEvent.click(screen.getByText("存储与维护"));
    await waitFor(() => expect(screen.getByText("颜色筛选索引重建")).toBeInTheDocument());

    assetMocks.rescanPaletteColors.mockResolvedValue(410);
    fireEvent.click(screen.getByRole("button", { name: "重建颜色索引" }));
    await waitFor(() => expect(assetMocks.rescanPaletteColors).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.getByText(/色板关系表已重建：写入 410 条/)).toBeInTheDocument());
  });
});

// ── StrictMode + 全 route 序列切换（模拟真机双渲染）──
describe("StrictMode 全路由遍历", () => {
  it("按序切换全部 6 个分组不抛 hooks 错误", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, saving: false });
    const { unmount } = render(
      <ReactStrictMode><SettingsPage /></ReactStrictMode>,
    );
    const routes = ["AI 与模型", "标签与分类", "外观与浏览", "存储与维护", "诊断与支持", "关于", "素材库与入库"];
    for (const r of routes) {
      fireEvent.click(screen.getByText(r));
      await waitFor(() => expect(screen.queryByText("加载设置中…")).toBeNull());
    }
    unmount();
  });
});

// ── 标签数据保护由后端自动维护，不在设置页展示 ──
describe("SettingsPage 标签数据保护", () => {
  it("标签设置不展示内部数据保护控件", async () => {
    useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null });
    render(<SettingsPage />);
    await waitFor(() => expect(screen.getByText("总库位置")).toBeInTheDocument());
    fireEvent.click(screen.getByText("标签与分类"));

    await screen.findByText("AI 自动打标分类");
    expect(screen.queryByText("分类设置")).toBeNull();
    expect(screen.queryByText("分类标签")).toBeNull();
    expect(screen.queryByText(/标签数据保护/)).toBeNull();
    expect(screen.queryByRole("button", { name: "检查标签冲突" })).toBeNull();
    expect(screen.queryByRole("button", { name: "启用保护" })).toBeNull();
  });
});
