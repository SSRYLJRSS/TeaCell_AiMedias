/**
 * LibraryPage 全页集成测试：真实组件链（SideBar + GridToolbar + AssetGrid + 弹窗组）
 * 单击卡片后观察选中态是否被其他组件意外清除（用户报告「单击自动取消选择」）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import LibraryPage from "@/pages/LibraryPage";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import { useConversationStore } from "@/stores/conversationStore";
import type { Asset, AssetPage } from "@/types/asset";

// ── mock API 层 ──
const mocks = vi.hoisted(() => ({
  listAssets: vi.fn(),
  listAssetIds: vi.fn(),
  listMetadataFacets: vi.fn(),
  getAssetUrls: vi.fn(),
  revealInFolder: vi.fn(),
  trashRestore: vi.fn(),
  getThumbnailUrl: vi.fn(),
  toFileUrl: vi.fn(),
  snapshotSelection: vi.fn(),
  summarySelection: vi.fn(),
  pageSelection: vi.fn(),
  createConversation: vi.fn(),
  linkConversation: vi.fn(),
  getConversation: vi.fn(),
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
// A4-03：回收站恢复已改走逐项结果通道（@/api/trash），旧 trashRestore 不再被 GridToolbar 调用。
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
vi.mock("@/api/tags", () => ({
  removeTags: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/api/video", () => ({
  ensureVideoProxy: vi.fn(),
  cancelVideoProxy: vi.fn().mockResolvedValue(undefined),
  toProxyFileUrl: (p: string) => `asset://proxy/${p}`,
}));
// A3-08：交接弹窗与结果视图都走这两个真实 API，测试里换成可控桩。
vi.mock("@/api/selections", () => ({
  snapshotSelection: mocks.snapshotSelection,
  summarySelection: mocks.summarySelection,
  pageSelection: mocks.pageSelection,
}));
vi.mock("@/api/conversations", () => ({
  createConversation: mocks.createConversation,
  listConversations: vi.fn().mockResolvedValue([]),
  getConversation: mocks.getConversation,
  renameConversation: vi.fn(),
  setConversationArchived: vi.fn(),
  deleteConversation: vi.fn(),
  saveConversationDraft: vi.fn(),
  attachConversationAsset: vi.fn(),
  linkConversation: mocks.linkConversation,
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://${p}`,
}));

// ── jsdom 布局/观测器桩（虚拟滚动 + Thumbnail 依赖） ──
vi.stubGlobal("ResizeObserver", class {
  cb: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
  }
  observe() {
    this.cb(
      [{ contentRect: { width: 800, height: 600 } } as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
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

const mkAsset = (id: number): Asset => ({
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
});

const pageOf = (items: Asset[]): AssetPage => ({ items, total: items.length, hasMore: false });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.listAssets.mockResolvedValue(pageOf([mkAsset(1), mkAsset(2), mkAsset(3)]));
  mocks.listAssetIds.mockResolvedValue([1, 2, 3]);
  mocks.listMetadataFacets.mockResolvedValue([]);
  mocks.getAssetUrls.mockResolvedValue([]);
  mocks.revealInFolder.mockResolvedValue(undefined);
  mocks.trashRestore.mockResolvedValue(undefined);
  mocks.getThumbnailUrl.mockResolvedValue("asset://hd.webp");
  mocks.toFileUrl.mockImplementation((p: string) => `asset://${p}`);
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
  });
  useSelectionStore.setState({ selected: new Set(), anchorIndex: null });
  useLibraryStore.setState({ viewerOpen: false, gridScrollTops: {} });
});

describe("LibraryPage 全页（单击选中不被自动清除）", () => {
  it("单击卡片后选中态保持（等待超过双击保护窗口与防抖周期）", async () => {
    render(<LibraryPage />);
    // 等待 LibraryPage 挂载 effect 的 refresh 完成并渲染卡片
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    const card = screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement;

    fireEvent.click(card);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(1)).toBe(true));

    // 观察 1.2s：双击窗口(250ms) + 搜索防抖(300ms) + refresh 周期均远超
    await new Promise((r) => setTimeout(r, 1200));
    expect(useSelectionStore.getState().selected.has(1)).toBe(true);
    expect(card.getAttribute("aria-selected")).toBe("true");
  });

  it("选中后触发 loadMore/refresh 等数据刷新，选中不被清", async () => {
    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a2.jpg").length).toBeGreaterThan(0));
    const card = screen.getAllByAltText("a2.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.click(card);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(2)).toBe(true));

    // 模拟后端数据刷新（refresh 再次返回新数组）——store 更新会直接驱动已挂载的网格，需 act
    mocks.listAssets.mockResolvedValue(pageOf([mkAsset(2), mkAsset(3), mkAsset(4)]));
    await act(async () => {
      await useLibraryStore.getState().refresh();
    });
    // 刷新后的异步网格重排发生在下一轮宏任务，等待期必须包在 act 内，否则丢失批处理告警。
    await act(async () => {
      await new Promise((r) => setTimeout(r, 500));
    });
    expect(useSelectionStore.getState().selected.has(2)).toBe(true);
  });

  it("选中后点侧栏标签/类型筛选——按 B09 设计清选（确认行为符合预期而非幽灵取消）", async () => {
    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    const card = screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.click(card);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(1)).toBe(true));

    // 切「视频」类型筛选 → B09：筛选变更清空选中（预期行为，非幽灵取消）
    fireEvent.click(screen.getByText("视频"));
    await waitFor(() => expect(useSelectionStore.getState().selected.size).toBe(0));
  });
});

describe("LibraryPage §7.2 Viewer 页面级替换（互斥）", () => {
  it("双击进入 Viewer：库页侧栏/工具栏卸载，Viewer 独占内容区", async () => {
    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    // 库页元素存在于渲染树
    expect(screen.getByText("回收站")).toBeInTheDocument();

    const card = screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.doubleClick(card);

    // Viewer 打开：库页筛选/侧栏/工具栏不在渲染树
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    expect(screen.queryByText("回收站")).not.toBeInTheDocument();
    expect(screen.queryByText("全部")).not.toBeInTheDocument();
    // Viewer 独占：网格卡片（aria-selected 标记）不再渲染；胶片条按钮无此标记
    expect(document.querySelectorAll("[aria-selected]")).toHaveLength(0);
    // viewerOpen 同步（App 据此隐藏 BottomBar）
    expect(useLibraryStore.getState().viewerOpen).toBe(true);
  });

  it("关闭 Viewer：库页上下文恢复（侧栏/网格回渲染）", async () => {
    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    const card = screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement;
    fireEvent.doubleClick(card);
    await waitFor(() => expect(screen.getByRole("button", { name: "关闭查看器" })).toBeInTheDocument());
    expect(useLibraryStore.getState().viewerOpen).toBe(true);

    // 关闭 Viewer（Esc 或关闭按钮）
    fireEvent.click(screen.getByRole("button", { name: "关闭查看器" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "关闭查看器" })).not.toBeInTheDocument());
    expect(useLibraryStore.getState().viewerOpen).toBe(false);
    // 库页元素恢复
    expect(screen.getByText("回收站")).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
  });
});

describe("LibraryPage A3-08 交接与本次结果视图", () => {
  const seedTwo = () => {
    mocks.listAssets.mockResolvedValue(pageOf([mkAsset(1), mkAsset(2)]));
    mocks.listAssetIds.mockResolvedValue([1, 2]);
  };

  it("「交给助手」先确认再固定快照，并留在库页只留结果入口；点开可退出回原浏览上下文", async () => {
    seedTwo();
    mocks.snapshotSelection.mockResolvedValue({
      selectionRef: "selection_aaaaaaaaaaaaaaaa",
      total: 2,
      truncated: false,
    });
    mocks.createConversation.mockResolvedValue({
      conversationRef: "conversation_cccccccccccccccc",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
    });
    mocks.linkConversation.mockResolvedValue(undefined);
    mocks.getConversation.mockResolvedValue({
      conversationRef: "conversation_cccccccccccccccc",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
      drafts: [],
      attachments: [],
      links: [{ kind: "selection", reference: "selection_aaaaaaaaaaaaaaaa" }],
    });
    mocks.pageSelection.mockResolvedValue({
      selectionRef: "selection_aaaaaaaaaaaaaaaa",
      total: 2,
      truncated: false,
      snapshot: 2,
      available: 2,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(1), mkAsset(2)],
    });

    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    fireEvent.click(screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(1)).toBe(true));

    fireEvent.click(screen.getByRole("button", { name: "交给助手" }));
    // 确认弹窗先说明交的是什么，再固定集合
    await waitFor(() => expect(screen.getByRole("dialog", { name: "交给助手" })).toBeInTheDocument());
    expect(screen.getByText(/交接是固定快照/)).toBeInTheDocument();
    fireEvent.click(within(screen.getByRole("dialog", { name: "交给助手" })).getByRole("button", { name: "交给助手" }));

    await waitFor(() =>
      expect(mocks.snapshotSelection).toHaveBeenCalledWith("素材库中的选择", [1], 1),
    );
    expect(mocks.linkConversation).toHaveBeenCalledWith(
      "conversation_cccccccccccccccc",
      "selection",
      "selection_aaaaaaaaaaaaaaaa",
    );

    // A3-08：交接成功不抢屏 —— 仍留在库页，只出现一条「已交给助手」入口
    const notice = await screen.findByRole("status", { name: "已交给助手" });
    expect(notice).toHaveTextContent("交接的 1 项");
    expect(screen.queryByTestId("selection-result-view")).not.toBeInTheDocument();
    expect(screen.getByText("回收站")).toBeInTheDocument();
    expect(useSelectionResultStore.getState().target).toBeNull();

    // 用户点「查看本次结果」才进结果视图（与 Viewer 同一种页面级替换）
    fireEvent.click(within(notice).getByRole("button", { name: "查看本次结果" }));
    await waitFor(() => expect(screen.getByTestId("selection-result-view")).toBeInTheDocument());
    expect(screen.queryByText("回收站")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByText(/共 2 项/)).toBeInTheDocument());
    expect(useSelectionResultStore.getState().target?.selectionRef).toBe("selection_aaaaaaaaaaaaaaaa");

    fireEvent.click(screen.getByRole("button", { name: "退出结果视图" }));
    await waitFor(() => expect(screen.queryByTestId("selection-result-view")).not.toBeInTheDocument());
    expect(useSelectionResultStore.getState().target).toBeNull();
    // 退出后原筛选/选择上下文不变
    expect(screen.getByText("回收站")).toBeInTheDocument();
    expect(useSelectionStore.getState().selected.has(1)).toBe(true);
  });

  it("交接提示可以关掉，且不会因此重新提交或改写集合", async () => {
    seedTwo();
    mocks.snapshotSelection.mockResolvedValue({
      selectionRef: "selection_eeeeeeeeeeeeeeee",
      total: 1,
      truncated: false,
    });
    mocks.createConversation.mockResolvedValue({
      conversationRef: "conversation_eeeeeeeeeeeeeeee",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
    });
    mocks.linkConversation.mockResolvedValue(undefined);
    mocks.getConversation.mockResolvedValue({
      conversationRef: "conversation_eeeeeeeeeeeeeeee",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
      drafts: [],
      attachments: [],
      links: [],
    });

    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    fireEvent.click(screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(1)).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "交给助手" }));
    await waitFor(() => expect(screen.getByRole("dialog", { name: "交给助手" })).toBeInTheDocument());
    fireEvent.click(within(screen.getByRole("dialog", { name: "交给助手" })).getByRole("button", { name: "交给助手" }));

    const notice = await screen.findByRole("status", { name: "已交给助手" });
    fireEvent.click(within(notice).getByRole("button", { name: "关闭交接提示" }));
    await waitFor(() =>
      expect(screen.queryByRole("status", { name: "已交给助手" })).not.toBeInTheDocument(),
    );
    // 关提示不是撤销交接：快照只提交过一次，后端目标也没被动过
    expect(mocks.snapshotSelection).toHaveBeenCalledTimes(1);
    expect(mocks.linkConversation).toHaveBeenCalledTimes(1);
    expect(useSelectionResultStore.getState().target).toBeNull();
  });

  it("全选被截断时弹窗明说只交接已加载部分，并且不会声称是全库", async () => {
    seedTwo();
    mocks.snapshotSelection.mockResolvedValue({
      selectionRef: "selection_bbbbbbbbbbbbbbbb",
      total: 2,
      truncated: true,
    });
    mocks.createConversation.mockResolvedValue({
      conversationRef: "conversation_dddddddddddddddd",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
    });
    mocks.getConversation.mockResolvedValue({
      conversationRef: "conversation_dddddddddddddddd",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
      drafts: [],
      attachments: [],
      links: [],
    });
    mocks.pageSelection.mockResolvedValue({
      selectionRef: "selection_bbbbbbbbbbbbbbbb",
      total: 9000,
      truncated: true,
      snapshot: 2,
      available: 2,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(1), mkAsset(2)],
    });

    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    fireEvent.click(screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement);
    // 模拟「全选」时后端已截断：只选中 2 项但总匹配 9000（store 已挂到页面上，需 act）
    act(() => {
      useSelectionStore.setState({ truncated: true, selectionTotal: 9000 });
    });

    fireEvent.click(screen.getByRole("button", { name: "交给助手" }));
    await waitFor(() => expect(screen.getByRole("dialog", { name: "交给助手" })).toBeInTheDocument());
    expect(screen.getByText(/当前条件共匹配 9000 项，但只加载了 1 项/)).toBeInTheDocument();

    fireEvent.click(within(screen.getByRole("dialog", { name: "交给助手" })).getByRole("button", { name: "交给助手" }));
    // 交给后端的是「已加载的 id + 真实匹配总数」，由后端记录 truncated
    await waitFor(() =>
      expect(mocks.snapshotSelection).toHaveBeenCalledWith("素材库中的选择", [1], 9000),
    );
  });

  it("切到别的会话后回到结果视图，会明说这份快照不是当前会话交的", async () => {
    seedTwo();
    mocks.snapshotSelection.mockResolvedValue({
      selectionRef: "selection_ffffffffffffffff",
      total: 1,
      truncated: false,
    });
    mocks.createConversation.mockResolvedValue({
      conversationRef: "conversation_ffffffffffffffff",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
    });
    mocks.linkConversation.mockResolvedValue(undefined);
    mocks.getConversation.mockResolvedValue({
      conversationRef: "conversation_ffffffffffffffff",
      title: "素材库交接",
      status: "active",
      createdAtUnixSeconds: 1,
      updatedAtUnixSeconds: 1,
      drafts: [],
      attachments: [],
      links: [],
    });
    mocks.pageSelection.mockResolvedValue({
      selectionRef: "selection_ffffffffffffffff",
      total: 1,
      truncated: false,
      snapshot: 1,
      available: 1,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(1)],
    });

    render(<LibraryPage />);
    await waitFor(() => expect(screen.getAllByAltText("a1.jpg")).not.toHaveLength(0));
    fireEvent.click(screen.getAllByAltText("a1.jpg")[0].closest('[role="button"]') as HTMLElement);
    await waitFor(() => expect(useSelectionStore.getState().selected.has(1)).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "交给助手" }));
    await waitFor(() => expect(screen.getByRole("dialog", { name: "交给助手" })).toBeInTheDocument());
    fireEvent.click(within(screen.getByRole("dialog", { name: "交给助手" })).getByRole("button", { name: "交给助手" }));
    const notice = await screen.findByRole("status", { name: "已交给助手" });
    fireEvent.click(within(notice).getByRole("button", { name: "查看本次结果" }));
    await waitFor(() => expect(screen.getByTestId("selection-result-view")).toBeInTheDocument());
    // 同一会话：不出跨会话标注
    expect(screen.queryByText(/来自另一个助手会话/)).not.toBeInTheDocument();

    // 用户切到另一个会话：结果视图仍是上一份快照，但必须说明来源不同
    act(() => {
      useConversationStore.setState({ activeRef: "conversation_other" });
    });
    await waitFor(() =>
      expect(screen.getByText(/来自另一个助手会话的交接，不是当前会话的素材集合/)).toBeInTheDocument(),
    );
    // 标注只是说明事实，不擅自把用户踢出结果视图
    expect(screen.getByTestId("selection-result-view")).toBeInTheDocument();
  });
});
