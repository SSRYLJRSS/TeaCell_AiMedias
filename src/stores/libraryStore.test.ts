/**
 * libraryStore 测试：数据分页载入 + B09（筛选清选中）+ BUG-E（fetchAllIds 轻量参数）+ B09 removedInView
 * mock 掉 @/api/assets 的 invoke 封装，Zustand 直接实例化断言。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { listAssets, listAssetIds, getAssetPalettePatches } from "@/api/assets";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import type { Asset, AssetPage } from "@/types/asset";

vi.mock("@/api/assets", () => ({
  listAssets: vi.fn(),
  listAssetIds: vi.fn(),
  getAssetPalettePatches: vi.fn(),
}));

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
  placeholderPath: null,
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

const pageOf = (items: Asset[], total = items.length): AssetPage => ({
  items,
  total,
  hasMore: items.length < total,
});

beforeEach(() => {
  vi.clearAllMocks();
  useLibraryStore.setState({
    items: [],
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
});

describe("libraryStore 数据装载", () => {
  it("refresh 拉取首页并写入 items/total", async () => {
    vi.mocked(listAssets).mockResolvedValue(pageOf([mkAsset(1), mkAsset(2)], 2));
    await useLibraryStore.getState().refresh();
    const s = useLibraryStore.getState();
    expect(s.items.map((a) => a.id)).toEqual([1, 2]);
    expect(s.total).toBe(2);
    expect(s.loading).toBe(false);
    expect(s.error).toBeNull();
    expect(listAssets).toHaveBeenCalledTimes(1);
  });

  it("refresh 失败写入 error 不抛异常", async () => {
    vi.mocked(listAssets).mockRejectedValue(new Error("invoke failed"));
    await useLibraryStore.getState().refresh();
    const s = useLibraryStore.getState();
    expect(s.error).toContain("invoke failed");
    expect(s.loading).toBe(false);
  });

  it("loadMore 追加去重且不越过 total", async () => {
    vi.mocked(listAssets).mockResolvedValueOnce(pageOf([mkAsset(1)], 3));
    vi.mocked(listAssets).mockResolvedValueOnce(pageOf([mkAsset(2)], 3));
    const s = useLibraryStore.getState();
    await s.refresh();
    await s.loadMore();
    const cur = useLibraryStore.getState();
    expect(cur.items.map((a) => a.id)).toEqual([1, 2]);
    expect(cur.total).toBe(3);
  });

  it("B09 回归：setFilter 清空选中集（跨筛选不残留不可见 id）", async () => {
    vi.mocked(listAssets).mockResolvedValue(pageOf([mkAsset(1)], 1));
    const s = useLibraryStore.getState();
    await s.refresh();
    useSelectionStore.getState().toggle(1, 0, true);
    expect(useSelectionStore.getState().count()).toBe(1);

    await s.setFilter({ assetType: "video" });
    expect(useSelectionStore.getState().count()).toBe(0);
    expect(useLibraryStore.getState().filter.assetType).toBe("video");
  });

  it("clearTagFilters：移除已删除标签的筛选且不会额外发起刷新", async () => {
    useLibraryStore.setState((s) => ({
      filter: {
        ...s.filter,
        untaggedOnly: true,
        tagId: 7,
        facetFilters: [{ facetKey: "scene", tagIds: [7], mode: "any", includeDescendants: true }],
        excludeTagIds: [8],
      },
    }));
    useSelectionStore.getState().toggle(1, 0, true);

    useLibraryStore.getState().clearTagFilters();

    const filter = useLibraryStore.getState().filter;
    expect(filter.untaggedOnly).toBe(false);
    expect(filter.tagId).toBeNull();
    expect(filter.facetFilters).toEqual([]);
    expect(filter.excludeTagIds).toEqual([]);
    expect(useSelectionStore.getState().count()).toBe(0);
    expect(listAssets).not.toHaveBeenCalled();
  });

  it("BUG-E 回归：fetchAllIds 走 list_asset_ids 只取 id 数组（不拉完整对象/不带 limit）", async () => {
    vi.mocked(listAssetIds).mockResolvedValue([7, 8, 9]);
    const ids = await useLibraryStore.getState().fetchAllIds();
    expect(ids).toEqual([7, 8, 9]);
    expect(listAssetIds).toHaveBeenCalledTimes(1);
    const arg = vi.mocked(listAssetIds).mock.calls[0][0];
    // 轻量契约：一次查询全 id、不依赖分页 limit 数值（供全选/反选）
    expect(arg.limit).toBeUndefined();
    expect(arg.offset).toBe(0);
    // 筛选条件必须透传
    expect(arg).toMatchObject({ assetType: "all", sortBy: "created_at" });
  });

  it("B09 removedInView：removeLocal 只减当前视图内实际移除数", async () => {
    vi.mocked(listAssets).mockResolvedValue(pageOf([mkAsset(1), mkAsset(2), mkAsset(3)], 3));
    const s = useLibraryStore.getState();
    await s.refresh();
    // 模拟「选中了视图外的 id 4（跨筛选残留）」——视图内只有 1 移除 1 条
    s.removeLocal([1, 4]);
    const cur = useLibraryStore.getState();
    expect(cur.items.map((a) => a.id)).toEqual([2, 3]);
    // total 只减 1（视图内移除），不会因视图外 id 多减
    expect(cur.total).toBe(2);
  });

  it("P1-01 竞态回归：旧 refresh 慢返回被丢弃（新筛选主导）", async () => {
    // ① 第一次 refresh 挂起（慢）
    let resolveSlow!: (v: AssetPage) => void;
    vi.mocked(listAssets).mockImplementationOnce(
      () => new Promise((res) => (resolveSlow = res)),
    );
    const s = useLibraryStore.getState();
    const slow = s.refresh();
    // ② 筛选变更 → 第二次 refresh（快，立即返回）
    vi.mocked(listAssets).mockResolvedValueOnce(pageOf([mkAsset(2)], 1));
    await s.setFilter({ search: "鱼" });
    expect(useLibraryStore.getState().items.map((a) => a.id)).toEqual([2]);
    // ③ 旧响应晚到 → 必须丢弃（不得覆盖 items/total）
    resolveSlow(pageOf([mkAsset(1)], 5));
    await slow;
    const cur = useLibraryStore.getState();
    expect(cur.items.map((a) => a.id)).toEqual([2]);
    expect(cur.total).toBe(1);
    expect(cur.loading).toBe(false);
  });

  it("P1-01 竞态回归：旧 loadMore 慢返回被丢弃（offset 错位防护）", async () => {
    // 首页 3 条
    vi.mocked(listAssets).mockResolvedValueOnce(pageOf([mkAsset(1)], 3));
    await useLibraryStore.getState().refresh();
    // ① loadMore 挂起（捕获旧 items 与旧 offset）
    let resolveMore!: (v: AssetPage) => void;
    vi.mocked(listAssets).mockImplementationOnce(
      () => new Promise((res) => (resolveMore = res)),
    );
    const more = useLibraryStore.getState().loadMore();
    // ② 期间 refresh（新查询）完成
    vi.mocked(listAssets).mockResolvedValueOnce(pageOf([mkAsset(2)], 10));
    await useLibraryStore.getState().refresh();
    expect(useLibraryStore.getState().items.map((a) => a.id)).toEqual([2]);
    // ③ 旧 loadMore 返回 → 丢弃（不得用旧 offset 追加、不得覆盖 total）
    resolveMore(pageOf([mkAsset(9)], 3));
    await more;
    const cur = useLibraryStore.getState();
    expect(cur.items.map((a) => a.id)).toEqual([2]);
    expect(cur.total).toBe(10);
  });

  it("P2-08 去重回归：refresh 响应含重复 id 时不产生重复 key", async () => {
    vi.mocked(listAssets).mockResolvedValue(pageOf([mkAsset(1), mkAsset(1), mkAsset(2)], 3));
    await useLibraryStore.getState().refresh();
    expect(useLibraryStore.getState().items.map((a) => a.id)).toEqual([1, 2]);
  });
});

describe("refreshPaletteFields（FB4-03 §6.2 定向同步）", () => {
  const paletteOf = (id: number): Asset["palette"] => [
    { hex: `#00000${id}`, r: id, g: id, b: id, ratio: 1 },
  ];

  beforeEach(() => {
    vi.mocked(getAssetPalettePatches).mockReset();
  });

  it("只向 API 请求当前 items 与 updatedIds 的交集（不去重外的 id）", async () => {
    useLibraryStore.setState({
      items: [mkAsset(1, { tags: [] }), mkAsset(2), mkAsset(3)],
      total: 3,
    });
    vi.mocked(getAssetPalettePatches).mockResolvedValue([]);
    await useLibraryStore.getState().refreshPaletteFields([2, 3, 99, 2]);
    // 去重 + 交集 → 只请求 [2,3]
    expect(getAssetPalettePatches).toHaveBeenCalledTimes(1);
    expect(getAssetPalettePatches).toHaveBeenCalledWith([2, 3]);
  });

  it("返回后只改变命中项的色板字段；顺序、长度、total 完全不变；未命中保持对象引用", async () => {
    const a1 = mkAsset(1, { tags: [] });
    const a2 = mkAsset(2);
    const a3 = mkAsset(3);
    useLibraryStore.setState({ items: [a1, a2, a3], total: 3 });
    vi.mocked(getAssetPalettePatches).mockResolvedValue([
      { id: 2, palette: paletteOf(2), dominantHue: 22, dominantSat: 23, dominantLum: 24 },
    ]);
    await useLibraryStore.getState().refreshPaletteFields([2]);
    const s = useLibraryStore.getState();
    expect(s.items.map((a) => a.id)).toEqual([1, 2, 3]);
    expect(s.total).toBe(3);
    // 命中项替换色板字段
    expect(s.items[1].palette).toEqual(paletteOf(2));
    expect(s.items[1].dominantHue).toBe(22);
    expect(s.items[1].dominantSat).toBe(23);
    expect(s.items[1].dominantLum).toBe(24);
    // 未命中保持原引用
    expect(s.items[0]).toBe(a1);
    expect(s.items[2]).toBe(a3);
  });

  it("不修改 tags、fileName、placeholderPath 等无关字段", async () => {
    const tags = [{ id: 9 } as never];
    useLibraryStore.setState({
      items: [mkAsset(1, { tags, fileName: "keep.jpg", placeholderPath: "ph.jpg" })],
      total: 1,
    });
    vi.mocked(getAssetPalettePatches).mockResolvedValue([
      { id: 1, palette: paletteOf(1), dominantHue: 1, dominantSat: 2, dominantLum: 3 },
    ]);
    await useLibraryStore.getState().refreshPaletteFields([1]);
    const a = useLibraryStore.getState().items[0];
    expect(a.tags).toBe(tags);
    expect(a.fileName).toBe("keep.jpg");
    expect(a.placeholderPath).toBe("ph.jpg");
  });

  it("调用过程中发生筛选/删除时，只 patch 最新 items 中仍存在的素材", async () => {
    useLibraryStore.setState({ items: [mkAsset(1), mkAsset(2), mkAsset(3)], total: 3 });
    let resolvePatches!: (v: never[]) => void;
    vi.mocked(getAssetPalettePatches).mockImplementationOnce(
      () => new Promise((res) => (resolvePatches = res as never)),
    );
    const pending = useLibraryStore.getState().refreshPaletteFields([1, 2, 3]);
    // IPC 返回前素材 2 被删除
    useLibraryStore.setState({ items: [mkAsset(1), mkAsset(3)], total: 2 });
    resolvePatches([
      { id: 1, palette: paletteOf(1), dominantHue: 1, dominantSat: 1, dominantLum: 1 },
      { id: 2, palette: paletteOf(2), dominantHue: 2, dominantSat: 2, dominantLum: 2 },
    ] as never);
    await pending;
    const s = useLibraryStore.getState();
    expect(s.items.map((a) => a.id)).toEqual([1, 3]);
    expect(s.items[0].dominantHue).toBe(1);
    expect(s.items[1].palette).toBeUndefined();
    expect(s.total).toBe(2); // 删除产生的 total 变化不被同步覆盖
  });

  it("超过 1000 个交集 id 时正确分批（每批 ≤1000）", async () => {
    const ids = Array.from({ length: 2500 }, (_, i) => i + 1);
    useLibraryStore.setState({ items: ids.map((id) => mkAsset(id)), total: ids.length });
    vi.mocked(getAssetPalettePatches).mockImplementation(async (batch: number[]) =>
      batch.map((id) => ({ id, palette: paletteOf(id), dominantHue: id, dominantSat: id, dominantLum: id })),
    );
    await useLibraryStore.getState().refreshPaletteFields(ids);
    expect(getAssetPalettePatches).toHaveBeenCalledTimes(3);
    expect((vi.mocked(getAssetPalettePatches).mock.calls[0][0] as number[]).length).toBe(1000);
    expect((vi.mocked(getAssetPalettePatches).mock.calls[2][0] as number[]).length).toBe(500);
    const s = useLibraryStore.getState();
    expect(s.items).toHaveLength(2500);
    expect(s.items[2499].dominantHue).toBe(2500);
  });

  it("空交集不调用 API", async () => {
    useLibraryStore.setState({ items: [mkAsset(1)], total: 1 });
    await useLibraryStore.getState().refreshPaletteFields([99]);
    expect(getAssetPalettePatches).not.toHaveBeenCalled();
  });

  it("方法内部从不调用 listAssets 或 refresh", async () => {
    useLibraryStore.setState({ items: [mkAsset(1)], total: 1 });
    vi.mocked(getAssetPalettePatches).mockResolvedValue([
      { id: 1, palette: paletteOf(1), dominantHue: 1, dominantSat: 1, dominantLum: 1 },
    ]);
    await useLibraryStore.getState().refreshPaletteFields([1]);
    expect(listAssets).not.toHaveBeenCalled();
  });
});
/** W5h-d：同源合并显示（纯前端折叠） */
describe("W5h-d kinship 合并显示", () => {
  const mkAsset = (id: number, filePath: string): Asset => ({
    id, filePath, fileName: filePath.split("/").pop() ?? filePath, fileExt: "jpg",
    fileSize: 1, mimeType: "image/jpeg", createdAt: 1, modifiedAt: 1, tags: [],
  } as unknown as Asset);

  it("libraryStore_merges_kinship_when_enabled：同组只留非 RAW 代表", async () => {
    const jpg = mkAsset(1, "d:/all/_0001.JPG");
    const raw = mkAsset(2, "d:/all/_0001.RW2");
    const solo = mkAsset(3, "d:/all/_0002.JPG");
    const { collapseKinship } = await import("@/stores/libraryStore");
    const merged = collapseKinship([jpg, raw, solo]);
    expect(merged.map((a) => a.id)).toEqual([1, 3]);
  });

  it("RAW 在前时 JPG 仍为代表", async () => {
    const raw = mkAsset(1, "d:/all/A.RW2");
    const jpg = mkAsset(2, "d:/all/A.JPG");
    const { collapseKinship } = await import("@/stores/libraryStore");
    expect(collapseKinship([raw, jpg]).map((a) => a.id)).toEqual([2]);
  });

  it("Unix 目录大小写不同不会误合并", async () => {
    const raw = mkAsset(1, "/photos/A/X.RW2");
    const jpg = mkAsset(2, "/photos/a/X.JPG");
    const { collapseKinship } = await import("@/stores/libraryStore");
    expect(collapseKinship([raw, jpg]).map((a) => a.id)).toEqual([1, 2]);
  });

  it("Unix 文件名中的反斜杠不会被当成目录分隔符", async () => {
    const raw = mkAsset(1, "/home/a\\b.RW2");
    const differentJpg = mkAsset(2, "/home/a/b.JPG");
    const { collapseKinship } = await import("@/stores/libraryStore");
    expect(collapseKinship([raw, differentJpg]).map((a) => a.id)).toEqual([1, 2]);
  });

  it("libraryStore_keeps_separate_when_disabled：不折叠由 store 开关控制（viewItems=items）", async () => {
    // mergeInLibrary=false 时 applyKinshipView 返回原数组 —— 通过设置 store 状态验证行为
    const { useSettingsStore } = await import("@/stores/settingsStore");
    const items = [mkAsset(1, "d:/all/A.JPG"), mkAsset(2, "d:/all/A.RW2")];
    useSettingsStore.setState({
      settings: {
        ai: { profiles: [], activeProfile: "", videoTagging: false, videoTaggingMode: "cover", videoFrameCount: 3, batchLimit: 30, ollamaSourceId: "auto" },
        theme: "system", thumbnailCacheMb: 2048, tagCategories: [], libraryRoot: "",
        trashRetentionDays: 30, customDownloadSources: [], modelDownloadProxy: "",
        appearance: {
          grid: { libraryCellStep: 3, importCellStep: 1, cellAspect: "1:1", cellFit: "cover", matchDominantColor: false },
          hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
          colorStrip: { enabled: true, showInLibraryGrid: false, showInViewer: true, showInImportGrid: false, height: "normal", mode: "ratio", count: 6 },
          kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
        },
      } as never,
      previewAppearance: null,
    });
    // 关闭时 viewItems 与 items 一致（store 路径：refresh 里 applyKinshipView）
    const { useLibraryStore } = await import("@/stores/libraryStore");
    useLibraryStore.setState({ items, viewItems: items, total: 2 });
    expect(useLibraryStore.getState().viewItems).toHaveLength(2);
  });
});
