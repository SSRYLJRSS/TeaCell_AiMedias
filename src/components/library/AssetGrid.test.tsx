/**
 * AssetGrid 交互测试：单击选中/再击取消、Ctrl/Shift 组合、右键菜单与选中语义
 * （对应 M7 BAT-001~008 / LIB-018；jsdom 下补齐 ResizeObserver 与尺寸 mock 支撑虚拟网格）
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { listAssetIds, listAssets, setFavorite, setRating } from "@/api/assets";
import AssetGrid from "@/components/library/AssetGrid";
import GridToolbar from "@/components/library/GridToolbar";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSettingsStore, DEFAULT_APPEARANCE, DEFAULT_NAVIGATION } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";
import type { Asset } from "@/types/asset";

// ── mock API 层 ──
vi.mock("@/api/assets", () => ({
  listAssets: vi.fn(),
  listAssetIds: vi.fn(),
  getAssetUrls: vi.fn().mockResolvedValue([]),
  revealInFolder: vi.fn().mockResolvedValue(undefined),
  setRating: vi.fn().mockResolvedValue(1),
  setFavorite: vi.fn().mockResolvedValue(1),
}));
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://thumb/hd.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));

// ── jsdom 补齐：虚拟滚动需要尺寸 + ResizeObserver ──
let observerInstances: MockResizeObserver[] = [];
class MockResizeObserver {
  cb: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
    observerInstances.push(this);
  }
  observe() {
    // 立即用固定宽度回报一次，让网格算出列数并渲染首屏卡片
    this.cb(
      [{ contentRect: { width: 800, height: 600 } } as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
  }
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal("ResizeObserver", MockResizeObserver);

/** 手动模拟窗口宽度变化，让虚拟网格重测列数（§13.1 列数变化稳定性测试用） */
function resizeTo(width: number) {
  for (const o of observerInstances) {
    o.cb(
      [{ contentRect: { width, height: 600 } } as ResizeObserverEntry],
      o as unknown as ResizeObserver,
    );
  }
}

// Thumbnail 用 IntersectionObserver 触发高清生成；jsdom 缺失，补最小桩
vi.stubGlobal("IntersectionObserver", class {
  cb: IntersectionObserverCallback;
  constructor(cb: IntersectionObserverCallback) {
    this.cb = cb;
  }
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
});

// jsdom 无布局引擎：给元素打固定尺寸桩，@tanstack/react-virtual 才能渲染可见行
Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
  configurable: true,
  value: () => ({
    width: 800,
    height: 600,
    top: 0,
    left: 0,
    right: 800,
    bottom: 600,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  }),
});

const mkAsset = (id: number, over: Partial<Asset> = {}): Asset => ({
  id,
  filePath: `d:/lib/a${id}.jpg`,
  fileName: `a${id}.jpg`,
  fileExt: "jpg",
  fileSize: 100,
  mimeType: "image/jpeg",
  width: 800,
  height: 600,
  durationMs: null,
  videoCodec: null,
  audioCodec: null,
  takenAt: null,
  createdAt: id,
  modifiedAt: id,
  hash: null,
  placeholderPath: `thumb${id}.jpg`,
  hdThumbnailPath: null,
  camera: null,
  lens: null,
  iso: null,
  aperture: null,
  shutter: null,
  focal: null,
  tags: [],
  ...over,
});

function renderGrid(preview: (asset: Asset) => void = vi.fn()) {
  return render(
    <AssetGrid
      onPreview={preview}
      onTag={vi.fn()}
      onExport={vi.fn()}
      onMove={vi.fn()}
      onDelete={vi.fn()}
    />,
  );
}

/** 按文件名找卡片 DOM 节点 */
function cardByName(name: string): HTMLElement {
  const img = screen.getAllByAltText(name)[0];
  // 卡片是 img 的祖先 role=button
  return img.closest('[role="button"]') as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  observerInstances = [];
  // 全选/反选走 fetchAllIds → listAssetIds；给默认返回值（全选 1,2）
  vi.mocked(listAssetIds).mockResolvedValue([1, 2]);
  useLibraryStore.setState({
    items: [],
    viewItems: [],
    total: 0,
    loading: false,
    error: null,
    filter: {
      assetType: "all",
      untaggedOnly: false,
      tagId: null,
      search: "",
      sortBy: "created_at",
      sortDir: "desc",
      trashOnly: false,
    },
  });
  useSelectionStore.setState({ selected: new Set(), anchorIndex: null });
  (useLibraryStore.getState() as unknown as { items: Asset[]; viewItems: Asset[]; total: number }).items = [];
  (useLibraryStore.getState() as unknown as { viewItems: Asset[] }).viewItems = [];
});

describe("AssetGrid 单击选择语义", () => {
  it("单击未选中卡片 → 选中它；再击已选中卡片 → 取消（E-1 切换语义）", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    renderGrid();
    const card1 = cardByName("a1.jpg");
    expect(card1).toBeTruthy();

    // 普通单击 = 交给 store 决定加/删
    fireEvent.click(card1);
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([1]);
    expect(card1.getAttribute("aria-selected")).toBe("true");

    // 再单击另一张 → 追加（都选上）
    const card2 = cardByName("a2.jpg");
    fireEvent.click(card2);
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 2]);

    // 单击已选中的卡片 → 取消该素材（不再是「保持不变」）
    fireEvent.click(card1);
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([2]);
  });

  it("选中集有跨筛选残留时单击追加：不误清已有选中", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    // 预置「选中了视图外的 id 9」的跨筛选残留（B09 场景）
    useSelectionStore.setState({ selected: new Set([9]), anchorIndex: null });
    renderGrid();

    // 单击未选中卡片 a1 → 追加选中，残留不被误清
    fireEvent.click(cardByName("a1.jpg"));
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 9]);
  });

  it("Ctrl+单击加选/再击减选切换", async () => {
    const assets = [mkAsset(1), mkAsset(2), mkAsset(3)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 3 });
    renderGrid();
    fireEvent.click(cardByName("a1.jpg"));
    fireEvent.click(cardByName("a2.jpg"), { ctrlKey: true });
    fireEvent.click(cardByName("a3.jpg"), { ctrlKey: true });
    const sel = Array.from(useSelectionStore.getState().selected).sort();
    expect(sel).toEqual([1, 2, 3]);
    // Ctrl 再击已选卡片 → 从多选移除
    fireEvent.click(cardByName("a2.jpg"), { ctrlKey: true });
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 3]);
  });

  it("右键未选中卡片 → 加入选中并开菜单（不影响已有选中）", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    renderGrid();
    fireEvent.click(cardByName("a1.jpg")); // 先选中 a1

    fireEvent.contextMenu(cardByName("a2.jpg"));
    // 右键行为：a2 加入选中，a1 保留（F19 追加语义）
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 2]);
    // 菜单出现且包含对选中集有意义的条目
    await waitFor(() => expect(screen.getByText("复制导出")).toBeInTheDocument());
  });

  it("右键已选中卡片 → 选中集不变（允许多选右键操作）", async () => {
    const assets = [mkAsset(1), mkAsset(2), mkAsset(3)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 3 });
    useSelectionStore.setState({ selected: new Set([1, 2]), anchorIndex: null });
    renderGrid();

    fireEvent.contextMenu(cardByName("a1.jpg"));
    const sel = Array.from(useSelectionStore.getState().selected).sort();
    expect(sel).toEqual([1, 2]); // 多选保留，未被反选/清空
  });

  it("右键素材菜单提供评级与收藏入口，选中后可批量设星（M-14b / PRD R-16 / J-03）", async () => {
    // 原用例断言「右键菜单不再提供评级功能」，那是 683628a 误删后的状态；
    // PRD R-16 与 QA_PLAYBOOK J-01/J-03 都把评级列为 2.0 必备能力，这里改为钉住恢复后的入口。
    const assets = [mkAsset(1)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 1 });
    renderGrid();

    fireEvent.contextMenu(cardByName("a1.jpg"));
    await waitFor(() => expect(screen.getByText("复制导出")).toBeInTheDocument());
    expect(screen.getByText("评级")).toBeInTheDocument();
    expect(screen.getByText("收藏 / 取消收藏")).toBeInTheDocument();
  });

  it("右键评级子菜单批量设星写库并同步星标，取消收藏也只改选中项（J-01 / J-03）", async () => {
    const assets = [mkAsset(1), mkAsset(2), mkAsset(3)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 3 });
    renderGrid();

    // 选中 a1、a2 后右键打开菜单，走「评级」二级菜单
    fireEvent.click(cardByName("a1.jpg"));
    fireEvent.contextMenu(cardByName("a2.jpg"));
    await waitFor(() => expect(screen.getByText("评级")).toBeInTheDocument());
    fireEvent.mouseEnter(screen.getByText("评级"));
    const fourStars = await screen.findByText("★★★★");
    fireEvent.click(fourStars);

    await waitFor(() => expect(setRating).toHaveBeenCalledWith([1, 2], 4));
    // 本地回写：两张都变成 4 星，未选中的 a3 不受影响
    const byId = () => Object.fromEntries(useLibraryStore.getState().items.map((a) => [a.id, a.rating ?? 0]));
    await waitFor(() => expect(byId()).toEqual({ 1: 4, 2: 4, 3: 0 }));
  });

  it("右键「收藏 / 取消收藏」批量写入，全部已收藏时再次点击是取消（可预期取反）", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    renderGrid();

    fireEvent.click(cardByName("a1.jpg"));
    fireEvent.contextMenu(cardByName("a2.jpg"));
    await waitFor(() => expect(screen.getByText("收藏 / 取消收藏")).toBeInTheDocument());
    fireEvent.click(screen.getByText("收藏 / 取消收藏"));
    await waitFor(() => expect(setFavorite).toHaveBeenCalledWith([1, 2], true));

    // 再次打开菜单点同一项：两张都已收藏 → 整体取消
    fireEvent.contextMenu(cardByName("a1.jpg"));
    await waitFor(() => expect(screen.getByText("收藏 / 取消收藏")).toBeInTheDocument());
    fireEvent.click(screen.getByText("收藏 / 取消收藏"));
    await waitFor(() => expect(setFavorite).toHaveBeenLastCalledWith([1, 2], false));
  });

  it("右键收藏写库失败时回滚星标并用 role=alert 提示，与查看器和批量操作栏口径一致（U-04）", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    vi.mocked(setFavorite).mockRejectedValueOnce(new Error("数据库忙"));
    renderGrid();

    fireEvent.click(cardByName("a1.jpg"));
    fireEvent.contextMenu(cardByName("a2.jpg"));
    await waitFor(() => expect(screen.getByText("收藏 / 取消收藏")).toBeInTheDocument());
    fireEvent.click(screen.getByText("收藏 / 取消收藏"));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("收藏没有保存");
    expect(alert.textContent).toContain("数据库忙");
    // 回滚：两张都回到未收藏，不假装已经保存
    const byId = () => Object.fromEntries(useLibraryStore.getState().items.map((a) => [a.id, a.favorite ?? 0]));
    await waitFor(() => expect(byId()).toEqual({ 1: 0, 2: 0 }));
  });

  it("右键菜单打开期间 Ctrl+A/Ctrl+I 不生效（防误触反选全选）", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    renderGrid();
    fireEvent.click(cardByName("a1.jpg"));
    fireEvent.contextMenu(cardByName("a2.jpg")); // 打开菜单（a2 追加进选中）
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 2]);

    // 菜单打开时按 Ctrl+A / Ctrl+I：选择集不得变化
    fireEvent.keyDown(window, { key: "a", ctrlKey: true });
    fireEvent.keyDown(window, { key: "i", ctrlKey: true });
    expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 2]);
    // 关闭菜单后快捷键恢复
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText("复制导出")).toBeNull());
    fireEvent.keyDown(window, { key: "a", ctrlKey: true });
    await waitFor(() =>
      expect(Array.from(useSelectionStore.getState().selected).sort()).toEqual([1, 2]),
    );
  });
});

describe("AssetGrid 双击预览（F19 语义）", () => {
  it("双击预览：第二次点击按 E-1 切换（取消选中），但仍打开预览", () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    const preview = vi.fn();
    renderGrid(preview);
    const card = cardByName("a1.jpg");

    fireEvent.click(card); // 单击选中 a1（但双击的第二击会 cancel 掉）
    fireEvent.click(card); // 双击的第二击（E-1：已在选中 → 取消）
    fireEvent.dblClick(card); // 打开预览
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([]);
    expect(preview).toHaveBeenCalledTimes(1);
  });

  it("单击已选中的卡片再击 → 取消（E-1 不再「保持不变」）", () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    renderGrid();
    const card = cardByName("a1.jpg");

    fireEvent.click(card);
    fireEvent.click(card); // 再击已选中：取消
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([]);
  });
});

describe("AssetGrid §13.1 列数变化稳定性", () => {
  it("窗口宽度改变（列数变化）后 asset id 与文件名仍对应（不整行 remount 毁状态）", async () => {
    // 造 6 个素材：宽屏 4 列 → 两行，窄屏 2 列 → 三行
    const assets = Array.from({ length: 6 }, (_, i) => mkAsset(i + 1));
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 6 });
    renderGrid();

    // 宽屏（800 → 4 列）：a1..a4 可见
    await waitFor(() => expect(screen.getByAltText("a1.jpg")).toBeInTheDocument());
    expect(screen.getByAltText("a4.jpg")).toBeInTheDocument();

    // 选中 a2 后改窄屏（2 列）→ 列数变化，卡片重排
    fireEvent.click(cardByName("a2.jpg"));
    expect(useSelectionStore.getState().selected.has(2)).toBe(true);
    // ResizeObserver 回调是同步的：直接调用会立刻让网格重算列数并 setState，必须包进 act。
    act(() => resizeTo(340));
    // 立即显式触发重排后（虚拟器按新列数渲染），文件名仍与 id 对应、选中不丢
    await waitFor(() => {
      expect(screen.getByAltText("a1.jpg")).toBeInTheDocument();
      expect(screen.getByAltText("a2.jpg")).toBeInTheDocument();
    });
    expect(useSelectionStore.getState().selected.has(2)).toBe(true);
    // a2 的卡片 aria-selected 仍在（卡片未被整行 key 重置）
    expect(cardByName("a2.jpg").getAttribute("aria-selected")).toBe("true");
  });
});

describe("AssetGrid FB2-01 档位缩放（Alt/Ctrl+滚轮）", () => {
  function mkSettingsAppearance(): Settings {
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
      appearance: DEFAULT_APPEARANCE,
      navigation: DEFAULT_NAVIGATION,
    };
  }

  it("Alt+滚轮上滚 → 档位 +1 且不越界；即时写入 previewAppearance", () => {
    vi.useFakeTimers();
    useSettingsStore.setState({ settings: mkSettingsAppearance(), previewAppearance: null });
    useLibraryStore.setState({ items: [mkAsset(1), mkAsset(2)], viewItems: [mkAsset(1), mkAsset(2)], total: 2 });
    const { container } = renderGrid();
    const scrollEl = container.querySelector(".overflow-y-auto") as HTMLElement;
    expect(scrollEl).not.toBeNull();

    // 先把时钟推过 60ms 节流窗（performance.now 从 0 起，否则首个 wheel 被节流吞掉）
    act(() => vi.advanceTimersByTime(100));
    // 上滚 +1：档位 3 → 4（推进 rAF 让 handler 内 step 落地）
    fireEvent.wheel(scrollEl, { altKey: true, deltaY: -100 });
    act(() => vi.advanceTimersByTime(16));
    expect(useSettingsStore.getState().previewAppearance?.grid.libraryCellStep).toBe(4);
    vi.useRealTimers();
  });

  it("不按修饰键的滚轮不触发档位变化（无回归）", () => {
    useSettingsStore.setState({ settings: mkSettingsAppearance(), previewAppearance: null });
    useLibraryStore.setState({ items: [mkAsset(1)], viewItems: [mkAsset(1)], total: 1 });
    const { container } = renderGrid();
    const scrollEl = container.querySelector(".overflow-y-auto") as HTMLElement;
    fireEvent.wheel(scrollEl, { deltaY: -100 }); // 无 Alt/Ctrl
    expect(useSettingsStore.getState().previewAppearance).toBeNull();
    // 无修饰键 + Alt 关，行为不变
  });
});
// ═══════════════ §4.6 全选截断安全策略（B2/B8 + Phase 2-16） ═══════════════

describe("AssetGrid §4.6 截断安全", () => {
  it("筛选变化后丢弃在途的旧全选结果", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    vi.mocked(listAssets).mockResolvedValue({ items: [], total: 0, hasMore: false });

    let resolveIds!: (ids: number[]) => void;
    vi.mocked(listAssetIds).mockReturnValueOnce(
      new Promise((resolve) => {
        resolveIds = resolve;
      }),
    );

    renderGrid();
    fireEvent.keyDown(window, { key: "a", ctrlKey: true });
    await waitFor(() => expect(listAssetIds).toHaveBeenCalledTimes(1));

    act(() => useLibraryStore.getState().setFilter({ search: "新条件" }));
    expect(useSelectionStore.getState().selected.size).toBe(0);

    await act(async () => {
      resolveIds([1, 2]);
      await Promise.resolve();
    });

    expect(useSelectionStore.getState().selected.size).toBe(0);
  });

  it("筛选变化后丢弃在途的旧反选结果", async () => {
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 2 });
    useSelectionStore.setState({ selected: new Set([1]), anchorIndex: 0 });
    vi.mocked(listAssets).mockResolvedValue({ items: [], total: 0, hasMore: false });

    let resolveIds!: (ids: number[]) => void;
    vi.mocked(listAssetIds).mockReturnValueOnce(
      new Promise((resolve) => {
        resolveIds = resolve;
      }),
    );

    renderGrid();
    fireEvent.keyDown(window, { key: "i", ctrlKey: true });
    await waitFor(() => expect(listAssetIds).toHaveBeenCalledTimes(1));

    act(() => useLibraryStore.getState().setFilter({ search: "新条件" }));
    expect(useSelectionStore.getState().selected.size).toBe(0);

    await act(async () => {
      resolveIds([1, 2]);
      await Promise.resolve();
    });

    expect(useSelectionStore.getState().selected.size).toBe(0);
  });

  it("truncated_ids_block_delete_and_invert：触顶 100000 → 反选/删除菜单项禁用（不可逆 + 集合不完整）", async () => {
    // fetchAllIds 返回触顶的 100000 个 id，total=100010（截断前匹配总数）
    const ids = Array.from({ length: 100_000 }, (_, i) => i + 1);
    vi.mocked(listAssetIds).mockResolvedValue(ids);
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 100_010 });
    useSelectionStore.setState({ selected: new Set(), anchorIndex: null, truncated: false, selectionTotal: 0 });
    renderGrid();

    // Ctrl+A 全选 → setAll 记录 truncated + total（粘性）
    fireEvent.keyDown(window, { key: "a", ctrlKey: true });
    await waitFor(() => expect(useSelectionStore.getState().truncated).toBe(true));
    await waitFor(() => expect(useSelectionStore.getState().selectionTotal).toBe(100_010));

    // 右键打开菜单：反选 / 删除 必须禁用（guardTruncated 的导出/打标走二次确认，不在本条断言）
    fireEvent.contextMenu(cardByName("a1.jpg"));
    const invertBtn = screen.getByText("反选").closest("button") as HTMLButtonElement;
    expect(invertBtn.disabled).toBe(true);
    expect(invertBtn.title).toContain("100000");
    const deleteBtn = screen.getByText("删除").closest("button") as HTMLButtonElement;
    expect(deleteBtn.disabled).toBe(true);
    expect(deleteBtn.title).toContain("收窄条件");
  });

  it("truncated_ids_require_confirm_for_export：截断时导出走二次确认（window.confirm 拒绝则不执行）", async () => {
    const ids = Array.from({ length: 100_000 }, (_, i) => i + 1);
    vi.mocked(listAssetIds).mockResolvedValue(ids);
    const onExport = vi.fn();
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const assets = [mkAsset(1), mkAsset(2)];
    useLibraryStore.setState({ items: assets, viewItems: assets, total: 100_010 });
    useSelectionStore.setState({ selected: new Set(), anchorIndex: null, truncated: false, selectionTotal: 0 });
    render(
      <AssetGrid
        onPreview={vi.fn()}
        onTag={vi.fn()}
        onExport={onExport}
        onMove={vi.fn()}
        onDelete={vi.fn()}
      />,
    );
    fireEvent.keyDown(window, { key: "a", ctrlKey: true });
    await waitFor(() => expect(useSelectionStore.getState().truncated).toBe(true));

    fireEvent.contextMenu(cardByName("a1.jpg"));
    fireEvent.click(screen.getByText("复制导出"));
    await waitFor(() => expect(confirmSpy).toHaveBeenCalled());
    expect(confirmSpy.mock.calls[0][0]).toContain("100010");
    expect(onExport).not.toHaveBeenCalled(); // 拒绝确认 → 不执行
  });

  it("selection_bar_shows_partial_count：选择栏恒显示「已选 N / total」，不是只显示 N", async () => {
    // 选择栏文案在 GridToolbar（LibraryPage 渲染），此处直接挂载它验证显示逻辑
    const ids = Array.from({ length: 100_000 }, (_, i) => i + 1);
    vi.mocked(listAssetIds).mockResolvedValue(ids);
    useSelectionStore.setState({
      selected: new Set(ids),
      anchorIndex: null,
      truncated: true,
      selectionTotal: 100_010,
    });
    render(
      <GridToolbar onTag={vi.fn()} onExport={vi.fn()} onMove={vi.fn()} onDelete={vi.fn()} onPurge={vi.fn()} />,
    );
    expect(screen.getByText(/已选中 100000 \/ 100010 项/)).toBeTruthy();
  });
});
