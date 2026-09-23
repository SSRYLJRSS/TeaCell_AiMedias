import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import SuperSearchPage from "@/pages/SuperSearchPage";
import { useSuperSearchStore } from "@/stores/superSearchStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSettingsStore, DEFAULT_APPEARANCE } from "@/stores/settingsStore";
import type { Asset } from "@/types/asset";
import { useLibraryStore } from "@/stores/libraryStore";

// 让 rAF 同步执行：滚动方向 hook 依赖它（jsdom 无真实 rAF 时钟）。
vi.spyOn(global, "requestAnimationFrame").mockImplementation((cb) => {
  cb(0);
  return 0;
});
vi.spyOn(global, "cancelAnimationFrame").mockImplementation(() => {});

vi.mock("@/api/assets", () => ({
  listAssets: vi.fn().mockResolvedValue({ items: [], total: 0, hasMore: false }),
  listAssetIds: vi.fn().mockResolvedValue([]),
  getAssetUrls: vi.fn().mockResolvedValue([]),
  revealInFolder: vi.fn().mockResolvedValue(undefined),
  listMetadataFacets: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/api/superSearch", () => ({
  aiParseSearchQuery: vi.fn().mockResolvedValue({
    intent: { groups: [], exclusions: [], sortBy: null, sortDir: null },
    expr: null,
    sortBy: "created_at",
    sortDir: "desc",
    explanation: "",
    warnings: [],
    resolvedTags: [],
  }),
  queryToFilter: vi.fn(),
  listSuperAssets: vi.fn().mockResolvedValue({ items: [], total: 0, hasMore: false }),
  listSuperAssetIds: vi.fn().mockResolvedValue([]),
  diagnoseSearchPlan: vi.fn().mockResolvedValue({ leaves: [], should: [] }),
}));
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://thumb/hd.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));
vi.mock("@/api/tags", () => ({
  listTags: vi.fn().mockResolvedValue([]),
  listTagFacets: vi.fn().mockResolvedValue([]),
  removeTags: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/api/video", () => ({
  ensureVideoProxy: vi.fn(),
  cancelVideoProxy: vi.fn().mockResolvedValue(undefined),
  toProxyFileUrl: (p: string) => `asset://proxy/${p}`,
}));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (p: string) => `asset://${p}` }));

class MockResizeObserver {
  cb: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) { this.cb = cb; }
  observe() { this.cb([{ contentRect: { width: 800, height: 600 } } as ResizeObserverEntry], this as unknown as ResizeObserver); }
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal("ResizeObserver", MockResizeObserver);
vi.stubGlobal("IntersectionObserver", class {
  cb: IntersectionObserverCallback;
  constructor(cb: IntersectionObserverCallback) { this.cb = cb; }
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() { return []; }
});
Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, value: 600 });
Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, value: 800 });
Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
  configurable: true,
  value: () => ({ width: 800, height: 600, top: 0, left: 0, right: 800, bottom: 600, x: 0, y: 0, toJSON: () => ({}) }),
});

/** 结果卡片用的最小 Asset（色条测试需要 palette） */
const mkAsset = (id: number, palette?: Asset["palette"]): Asset =>
  ({
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
    createdAt: 1,
    modifiedAt: 1,
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
    palette,
  }) as Asset;

beforeEach(() => {
  vi.clearAllMocks();
  useSelectionStore.setState({ selected: new Set(), anchorIndex: null });
  useSettingsStore.setState({ settings: null, previewAppearance: null });
  useSuperSearchStore.setState({
    query: { search: "", assetType: "all", untaggedOnly: false, facetFilters: [], excludeTagIds: [], metadataFilters: [], sortBy: "created_at", sortDir: "desc" },
    expr: undefined,
    items: [],
    total: 0,
    loading: false,
    error: null,
    aiError: null,
    aiInput: "",
    aiLoading: false,
    aiExplanation: null,
    warnings: [],
  });
});

describe("SuperSearchPage", () => {
  it("FB6 需求五：无顶栏返回按钮和重复标题；搜索框上方唯一「超级搜索」标识", () => {
    render(<SuperSearchPage />);
    // 不存在顶栏返回按钮
    expect(screen.queryByText("← 返回")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /返回/ })).not.toBeInTheDocument();
    // 「超级搜索」标识存在且唯一（AiSearchBar 内的 h1，非输入框 placeholder）
    const titles = screen.getAllByText("超级搜索");
    expect(titles).toHaveLength(1);
    expect(titles[0].tagName).toBe("H1");
    const searchbox = screen.getByRole("searchbox");
    expect(searchbox.getAttribute("placeholder")).not.toContain("超级搜索");
    // 搜索框在标识下方（同一容器内 h1 位于 form 之前）
    expect(titles[0].compareDocumentPosition(searchbox) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByRole("region", { name: "条件公式" })).toBeInTheDocument();
    // 结果计数保留在摘要行（U-5 之后「加分项」区标题也含「项」字，计数须为「数字 + 项」形态）
    expect(screen.getByText(/^\d+ 项$/)).toBeInTheDocument();
    // 新增条件统一从三个区域进入，摘要行不再重复放一个快速添加加号。
    expect(screen.queryByRole("button", { name: "快速添加条件" })).not.toBeInTheDocument();
  });

  it("小窗口布局：页面封住文档溢出，条件区可独立滚动且结果区保留最低高度", () => {
    const { container } = render(<SuperSearchPage />);
    const page = container.firstElementChild as HTMLElement;
    const filterZone = container.querySelector("[data-filter-zone]") as HTMLElement;
    const resultScroll = screen.getByTestId("super-search-scroll");

    expect(page.className).toContain("min-h-0");
    expect(page.className).toContain("overflow-hidden");
    expect(filterZone.className).toContain("overflow-y-auto");
    expect(filterZone.className).toContain("overscroll-contain");
    expect(filterZone.className).not.toContain("shrink-0");
    expect(resultScroll.className).toContain("min-h-40");
    expect(resultScroll.className).toContain("overscroll-contain");
  });

  function scrollTo(top: number) {
    const el = screen.getByTestId("super-search-scroll") as HTMLElement;
    Object.defineProperty(el, "scrollTop", { configurable: true, value: top, writable: true });
    fireEvent.scroll(el);
  }

  it("FB-06：下滚收起详细条件，上滚恢复（FB2-06 改为 grid-template-rows 折叠，面板常驻 DOM）", () => {
    render(<SuperSearchPage />);
    const panel = document.getElementById("super-search-filters") as HTMLElement;
    expect(panel).toBeInTheDocument();
    expect(panel.style.gridTemplateRows).toBe("1fr"); // 初始展开
    expect(panel.getAttribute("aria-hidden")).toBe("false");
    // 下滚超过阈值（scrollTop>=48 且累计>=24）→ 收起
    scrollTo(100);
    expect(panel.style.gridTemplateRows).toBe("0fr");
    expect(panel.getAttribute("aria-hidden")).toBe("true");
    // 上滚（回到顶部区 minScrollTop）→ 恢复展开
    scrollTo(0);
    expect(panel.style.gridTemplateRows).toBe("1fr");
    expect(panel.getAttribute("aria-hidden")).toBe("false");
  });

  it("FB2-06：顶部区（scrollTop < 48）恒为展开态", () => {
    render(<SuperSearchPage />);
    scrollTo(30); // 低于 minScrollTop，即使下滚也保持展开
    const panel = document.getElementById("super-search-filters") as HTMLElement;
    expect(panel.style.gridTemplateRows).toBe("1fr");
  });

  it("FB2-06：focus 搜索框强制展开、focus 结果卡片不展开", () => {
    render(<SuperSearchPage />);
    scrollTo(100); // 收起
    const panel = document.getElementById("super-search-filters") as HTMLElement;
    expect(panel.style.gridTemplateRows).toBe("0fr");
    // focus 搜索框（在 header data-filter-zone 内）→ 强制展开
    fireEvent.focus(screen.getByRole("searchbox") as HTMLElement);
    expect(panel.style.gridTemplateRows).toBe("1fr");
  });

  // FB2-08（FX-09）：结果卡片的色条主色段可点，写入同色系条件
  it("点结果卡片主色段 → 写入 hue±15 / sat±25 条件；再点另一色系替换而非叠加", () => {
    useSettingsStore.setState({
      settings: { appearance: { ...DEFAULT_APPEARANCE, colorStrip: { ...DEFAULT_APPEARANCE.colorStrip, enabled: true, showInLibraryGrid: true } } } as never,
      previewAppearance: null,
    });
    useSuperSearchStore.setState({
      items: [mkAsset(1, [{ hex: "#1b6ad2", r: 27, g: 106, b: 210, ratio: 0.7 }])],
      total: 1,
    });
    render(<SuperSearchPage />);

    const dominant = screen.getByRole("button", { name: /^搜索.+系素材$/ });
    fireEvent.click(dominant);
    const filters = useSuperSearchStore.getState().query.metadataFilters;
    expect(filters.map((f) => f.key)).toEqual(["dominant_hue", "dominant_sat"]);

    // 换一个色系：同 key 替换（叠加两个不相交 hue 区间会 AND 成空集）
    useSuperSearchStore.setState({
      items: [mkAsset(2, [{ hex: "#d21b1b", r: 210, g: 27, b: 27, ratio: 0.8 }])],
    });
    fireEvent.click(screen.getByRole("button", { name: /^搜索.+系素材$/ }));
    const next = useSuperSearchStore.getState().query.metadataFilters;
    expect(next.filter((f) => f.key === "dominant_hue")).toHaveLength(1);
  });

  describe("FB5-03 中央 Chevron 披露按钮（§3.5/§13.4）", () => {
    it("披露按钮位于中央独立行，只显示 Chevron 图标（无旧文字按钮）", () => {      render(<SuperSearchPage />);
      // 旧文字按钮不复存在
      expect(screen.queryByText(/展开详细条件 ⤵|收起 ⤴/)).not.toBeInTheDocument();
      // 中央披露按钮：仅图标（aria-label 驱动查询）
      expect(screen.getByRole("button", { name: "收起详细条件" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "展开详细条件" })).not.toBeInTheDocument();
      // 按钮位于宽屏内容容器内的独立行
      const btn = screen.getByRole("button", { name: "收起详细条件" });
      expect(btn.closest(".mx-auto.w-full.max-w-\\[1440px\\]")).not.toBeNull();
      // 行高 24px
      const row = btn.closest(".h-6") as HTMLElement;
      expect(row).not.toBeNull();
    });

    it("aria-expanded 与面板状态同步；点击切换 Chevron 方向", () => {
      render(<SuperSearchPage />);
      const btn = screen.getByRole("button", { name: "收起详细条件" });
      expect(btn.getAttribute("aria-expanded")).toBe("true");
      const panel = document.getElementById("super-search-filters") as HTMLElement;
      expect(panel.style.gridTemplateRows).toBe("1fr");

      fireEvent.click(btn);
      expect(screen.getByRole("button", { name: "展开详细条件" }).getAttribute("aria-expanded")).toBe("false");
      expect(panel.style.gridTemplateRows).toBe("0fr");
      expect(panel.getAttribute("aria-hidden")).toBe("true");

      fireEvent.click(screen.getByRole("button", { name: "展开详细条件" }));
      expect(screen.getByRole("button", { name: "收起详细条件" }).getAttribute("aria-expanded")).toBe("true");
      expect(panel.style.gridTemplateRows).toBe("1fr");
    });

    it("现有滚动收起/顶部展开/focus 展开逻辑全部保留", () => {
      render(<SuperSearchPage />);
      const panel = document.getElementById("super-search-filters") as HTMLElement;
      // 下滚收起
      scrollTo(100);
      expect(panel.style.gridTemplateRows).toBe("0fr");
      // focus 搜索框强制展开
      fireEvent.focus(screen.getByRole("searchbox") as HTMLElement);
      expect(panel.style.gridTemplateRows).toBe("1fr");
      // 上滚回顶部保持展开
      scrollTo(0);
      expect(panel.style.gridTemplateRows).toBe("1fr");
    });
  });
});

describe("U-7③ 空结果归零条件", () => {
  it("0 结果时列出归零条件（诊断叶子），单条移除走 removeExprAtPath", async () => {
    const { diagnoseSearchPlan } = await import("@/api/superSearch");
    const expr = {
      op: "and" as const,
      children: [
        { op: "leaf" as const, cond: { type: "search" as const, value: "海边" } },
        { op: "leaf" as const, cond: { type: "search" as const, value: "霓虹" } },
      ],
    };
    // §3.7：条件经 setExpr 写入 plan.filter（plan 唯一事实源），expr 是其派生视图
    useSuperSearchStore.getState().setExpr(expr);
    useSuperSearchStore.setState({ planRevision: 0, items: [], total: 0, loading: false });
    vi.mocked(diagnoseSearchPlan).mockResolvedValue({
      leaves: [
        { zone: "filter", path: [0], planRevision: 0, label: "海边", selfCount: 106, resultCount: 0, countWithoutLeaf: 106, delta: 106 },
        { zone: "filter", path: [1], planRevision: 0, label: "霓虹", selfCount: 0, resultCount: 0, countWithoutLeaf: 20, delta: 20 },
      ],
      should: [],
      warnings: [],
    });
    render(<SuperSearchPage />);
    await waitFor(() => expect(screen.getByText(/以下条件把结果砍到 0/)).toBeInTheDocument(), { timeout: 2000 });
    expect(screen.getByRole("button", { name: "移除归零条件 海边" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "移除归零条件 海边" }));
    // 归零条件在 expr（plan.filter 派生视图）中消失（removeAtZonePath("filter", path) 摘除叶子并 normalize）
    expect(useSuperSearchStore.getState().expr).toEqual({
      op: "leaf",
      cond: { type: "search", value: "霓虹" },
    });
    expect(useSuperSearchStore.getState().plan?.filter).toEqual({
      op: "leaf",
      cond: { type: "search", value: "霓虹" },
    });
  });
});

describe("S5 5-4 零结果相近词建议", () => {
  it("执行 warning 建议渲染为可点 chip，点击才把按词查加进必须区（不变量 11）", async () => {
    const { listSuperAssets } = await import("@/api/superSearch");
    const expr = {
      op: "leaf" as const,
      cond: { type: "tag" as const, facetKey: "scene", tagIds: [], mode: "any" as const, includeDescendants: true, termQuery: "森材", termMatch: "alias" as const },
    };
    // 后端返回带零结果建议的 warnings（refresh 响应会写入 executionWarnings —— 真实数据流）
    vi.mocked(listSuperAssets).mockResolvedValue({
      items: [],
      total: 0,
      hasMore: false,
      warnings: [{ source: "plan", zone: "filter", message: "词查「森材」没有命中。试试相近的词：森林、丛林" }],
    });
    useSuperSearchStore.getState().setExpr(expr);
    render(<SuperSearchPage />);
    const chip = await screen.findByRole("button", { name: /试试「森林」/ });
    // 未点击前：条件保持原样
    const before = useSuperSearchStore.getState().plan?.filter;
    expect(before).toEqual(expr);
    // 点击建议 → 只替换对应失败的词查 leaf，其他条件保持不变
    fireEvent.click(chip);
    const after = useSuperSearchStore.getState().plan?.filter;
    expect(after).toEqual({
      op: "leaf",
      cond: { type: "tag", facetKey: "scene", tagIds: [], mode: "any", includeDescendants: true, termQuery: "森林", termMatch: "fuzzy" },
    });
    // 第二个建议也在（丛林），可继续点
    expect(screen.getByRole("button", { name: /试试「丛林」/ })).toBeInTheDocument();
  });
});

describe("超级搜索 → 详情查看器（联动显示 bug 回归）", () => {
  beforeEach(() => {
    // 素材库列表故意放另一批素材（5 项），用于证明查看器不会错用素材库全量列表
    useLibraryStore.setState({
      items: [mkAsset(201), mkAsset(202), mkAsset(203), mkAsset(204), mkAsset(205)],
      total: 5,
      viewerOpen: false,
    });
  });

  async function renderWithResults() {
    const superItems = [mkAsset(101), mkAsset(102)];
    const { listSuperAssets } = await import("@/api/superSearch");
    vi.mocked(listSuperAssets).mockResolvedValue({ items: superItems, total: 2, hasMore: false });
    useSuperSearchStore.setState({ items: superItems, total: 2, loading: false });
    render(<SuperSearchPage />);
    await waitFor(() => expect(screen.getAllByAltText("a101.jpg").length).toBeGreaterThan(0));
    return superItems;
  }

  it("双击结果卡片：查看器整页替换超搜页、viewerOpen 同步、过片只走搜索结果集（非素材库 5 项）", async () => {
    await renderWithResults();
    expect(screen.getByRole("searchbox")).toBeInTheDocument();

    const card = screen.getAllByAltText("a101.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.doubleClick(card);

    // 互斥挂载：搜索头部卸载、Viewer 出现（旧实现把 Viewer 挤在 flex 列底部，主图塌陷）
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    expect(screen.queryByRole("searchbox")).not.toBeInTheDocument();
    // viewerOpen 同步（App 据此隐藏全局 BottomBar，避免双底栏）
    expect(useLibraryStore.getState().viewerOpen).toBe(true);
    // 位置计数 = 搜索结果集 2 项，而不是素材库全量 5 项（旧实现错显 x/5 的联动错位）
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    expect(screen.queryByText(/\/ 5/)).not.toBeInTheDocument();
    // 胶片条只含搜索结果，不混入素材库列表
    expect(screen.getByRole("button", { name: "第 1 张：a101.jpg" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "第 2 张：a102.jpg" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /a20\d\.jpg/ })).not.toBeInTheDocument();

    // 下一张在搜索结果集内推进；到末尾再点也被钳住，不会跳进素材库列表
    fireEvent.click(screen.getByRole("button", { name: "下一张" }));
    expect(screen.getByText("2 / 2")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "下一张" }));
    expect(screen.getByText("2 / 2")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /a20\d\.jpg/ })).not.toBeInTheDocument();
  });

  it("关闭查看器：回到超搜页且 viewerOpen 复位（全局 BottomBar 恢复）", async () => {
    await renderWithResults();
    const card = screen.getAllByAltText("a101.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.doubleClick(card);
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭查看器" })).toBeInTheDocument());
    expect(useLibraryStore.getState().viewerOpen).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "关闭查看器" }));
    await waitFor(() => expect(screen.getByRole("searchbox")).toBeInTheDocument());
    expect(useLibraryStore.getState().viewerOpen).toBe(false);
  });
});

describe("FB5-03 条件面板唯一披露按钮（位于面板最底部，不再有第二个向上箭头）", () => {
  it("展开态：全页只有一个收起按钮，且位于三区条件（条件公式）之后；点它即收起，再点展开", () => {
    render(<SuperSearchPage />);
    const panel = document.getElementById("super-search-filters") as HTMLElement;
    expect(panel.style.gridTemplateRows).toBe("1fr");
    // 全页只有一个「收起详细条件」按钮（旧实现顶部、底部各一个）
    expect(screen.getAllByRole("button", { name: "收起详细条件" })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "收起详细条件（底部）" })).not.toBeInTheDocument();
    const toggle = screen.getByRole("button", { name: "收起详细条件" });
    // 按钮位于 QueryBuilder（条件公式 region）之后 —— 即面板最底部
    const builder = screen.getByRole("region", { name: "条件公式" });
    expect(toggle.compareDocumentPosition(builder) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();

    fireEvent.click(toggle);
    expect(panel.style.gridTemplateRows).toBe("0fr");
    expect(panel.getAttribute("aria-hidden")).toBe("true");
    // 收起后同一个按钮变成「展开详细条件」（面板 0fr 后它贴到摘要行下方）
    expect(screen.getByRole("button", { name: "展开详细条件" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "展开详细条件" }));
    expect(panel.style.gridTemplateRows).toBe("1fr");
  });
});
