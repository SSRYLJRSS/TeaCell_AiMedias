/** 素材库数据与筛选状态（虚拟网格数据源） */
import { create } from "zustand";
import { listAssets, listAssetIds, getAssetPalettePatches, type AssetPalettePatch } from "@/api/assets";
import { useSettingsStore } from "@/stores/settingsStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { markStartup } from "@/utils/startupMarks";
import type { Asset, AssetFilter, AssetType, FacetTagFilter, MetadataFilter } from "@/types/asset";

const PAGE_SIZE = 200;

/** W5h-d：同源合并显示的纯前端折叠 —— 每组（同目录同主干名 + 一 RAW 一非 RAW）只留代表（非 RAW 优先）。
 *  不动后端查询：total/分页/分面计数语义全部保持（计划书 §W5h-d 关键取舍）。
 *  is_raw 判定用扩展名白名单（与后端 utils::mime::is_raw_ext 同源）。 */
const RAW_EXTS = new Set(["raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "iiq", "3fr", "erf", "srw", "kdc", "dcr", "mos", "mef", "x3f", "rwl"]);

function kinshipKeyOf(a: Asset): { key: string; isRaw: boolean } {
  // 与后端 services/kinship.rs::kinship_key 对齐：
  // - 只按 `/` 切目录；Windows 入库路径已由后端 normalize_path 统一为 `/`；
  // - Unix 的 `\\` 是合法文件名字符，不能当目录分隔符；
  // - 目录大小写保留，只折叠 stem，避免 Linux `/A` 与 `/a` 被错误合并。
  const p = a.filePath ?? "";
  const slash = p.lastIndexOf("/");
  const dir = slash >= 0 ? p.slice(0, slash) : "";
  const name = slash >= 0 ? p.slice(slash + 1) : p;
  const dot = name.lastIndexOf(".");
  const stem = dot >= 0 ? name.slice(0, dot) : name;
  const ext = dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
  return { key: `${dir}/${stem.toLowerCase()}`, isRaw: RAW_EXTS.has(ext) };
}

export function collapseKinship(items: Asset[]): Asset[] {
  const best = new Map<string, Asset>();
  for (const a of items) {
    const { key, isRaw } = kinshipKeyOf(a);
    const cur = best.get(key);
    if (!cur) {
      best.set(key, a);
      continue;
    }
    // 非 RAW 优先做代表（JPG 解码快、有内嵌预览）
    const curRaw = kinshipKeyOf(cur).isRaw;
    if (curRaw && !isRaw) best.set(key, a);
  }
  // 保持原顺序
  const visible = new Set(best.values());
  return items.filter((a) => visible.has(a));
}

/** 当前是否开启合并显示（读 settingsStore；默认关） */
function kinshipMergeEnabled(): boolean {
  try {
    return useSettingsStore.getState().settings?.appearance.kinship.mergeInLibrary ?? false;
  } catch {
    return false;
  }
}

/** 应用（或不应用）合并显示，产出 viewItems */
function applyKinshipView(items: Asset[]): Asset[] {
  return kinshipMergeEnabled() ? collapseKinship(items) : items;
}

/** 请求代际计数（模块级，跨 set 调用共享）：refresh/loadMore 响应回写前校验，
 *  过期请求（筛选已变更/已有新请求发出）直接丢弃，防异步响应覆盖竞态（P1-01） */
let requestSeq = 0;

/** 按 id 去重（保持原顺序）：refresh/loadMore 统一走这里，防后端 offset 分页
 *  在数据变动时返回重复 id → React 重复 key 警告（P2-08） */
function dedupItems(arr: Asset[]): Asset[] {
  const seen = new Set<number>();
  const out: Asset[] = [];
  for (const a of arr) {
    if (!seen.has(a.id)) {
      seen.add(a.id);
      out.push(a);
    }
  }
  return out;
}

export interface LibraryFilter {
  assetType: AssetType;
  untaggedOnly: boolean;
  tagId: number | null;
  facetFilters?: FacetTagFilter[];
  excludeTagIds?: number[];
  metadataFilters?: MetadataFilter[];
  search: string;
  /** R-21 排序：created_at（默认）| taken_at | modified_at | name | size | resolution */
  sortBy: "created_at" | "taken_at" | "modified_at" | "name" | "size" | "resolution";
  sortDir: "desc" | "asc";
  /** R-22：true = 回收站视图 */
  trashOnly: boolean;
}

/** LibraryFilter → 后端 AssetFilter（统一出口，refresh/loadMore/fetchAllIds 共用） */
function toApiFilter(f: LibraryFilter, offset: number, limit?: number): AssetFilter {
  return {
    assetType: f.assetType,
    untaggedOnly: f.untaggedOnly,
    tagId: f.tagId ?? undefined,
    facetFilters: f.facetFilters ?? [],
    excludeTagIds: f.excludeTagIds ?? [],
    metadataFilters: f.metadataFilters ?? [],
    search: f.search || undefined,
    sortBy: f.sortBy,
    sortDir: f.sortDir,
    trashOnly: f.trashOnly,
    offset,
    limit,
  };
}

interface LibraryState {
  items: Asset[];
  /** W5h-d：合并显示开启时的「可见列表」（每组只留代表）；关闭时与 items 相同 */
  viewItems: Asset[];
  total: number;
  loading: boolean;
  error: string | null;
  filter: LibraryFilter;
  setFilter: (patch: Partial<LibraryFilter>) => void;
  /** 删除标签后移除已失效的标签筛选，不主动刷新（调用方可与数据刷新合并） */
  clearTagFilters: () => void;
  refresh: () => Promise<void>;
  loadMore: () => Promise<void>;
  /** 删除/打标后局部摘除，避免整页重载 */
  removeLocal: (ids: number[]) => void;
  patchLocal: (ids: number[], patch: Partial<Asset>) => void;
  /**
   * FB4-03（§6.2）：色板字段定向同步 —— 色板回算/导入后置完成后只合并命中素材的
   * 色板字段（palette / dominantHue / dominantSat / dominantLum），
   * 不调用 listAssets / refresh()，绝不重置分页、排序、选择、Viewer 或滚动位置。
   * 算法：去重 updatedIds → 与调用时当前 items 求交集（空交集立即返回，不发 IPC）→
   * 每批 ≤1000 调 getAssetPalettePatches → 函数式 set 在提交瞬间基于最新 s.items 按 id 合并。
   */
  refreshPaletteFields: (updatedIds: number[]) => Promise<void>;
  /** 取当前筛选结果的全部 id（全选/反选/批量操作用；一次查询只取 id 数组） */
  fetchAllIds: () => Promise<number[]>;
  /** Viewer 是否打开（§7.3 方案 A：App 据此隐藏全局 BottomBar，与 Viewer 互斥） */
  viewerOpen: boolean;
  setViewerOpen: (open: boolean) => void;
  /** 网格滚动位置（FB2-06 §7.4 方案 D：按键存储，防止素材库与超级搜索互相污染；
   *  Viewer 关闭后恢复对应页面的滚动上下文；AssetGridView 滚动时写入） */
  gridScrollTops: Record<string, number>;
  setGridScrollTop: (key: string, top: number) => void;
  getGridScrollTop: (key: string) => number;
}

export const useLibraryStore = create<LibraryState>((set, get) => ({
  items: [],
  viewItems: [],
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

  setFilter: (patch) => {
    // F18：筛选值未变化时 no-op（不刷新、不清选）。
    // 根因：GridToolbar 重渲染导致 SearchInput 的 onSearch 引用变化、防抖定时器重启，
    // 会把空串/相同值再次提交给 setFilter → 触发 B09 clear() → 「单击选中后自动取消」。
    // 任何调用源（搜索/排序/标签/类型/回收站）提交相同筛选都不应产生副作用。
    const prev = get().filter;
    const next = { ...prev, ...patch };
    if (
      prev.assetType === next.assetType &&
      prev.untaggedOnly === next.untaggedOnly &&
      prev.tagId === next.tagId &&
      JSON.stringify(prev.facetFilters ?? []) === JSON.stringify(next.facetFilters ?? []) &&
      JSON.stringify(prev.excludeTagIds ?? []) === JSON.stringify(next.excludeTagIds ?? []) &&
      JSON.stringify(prev.metadataFilters ?? []) === JSON.stringify(next.metadataFilters ?? []) &&
      prev.search === next.search &&
      prev.sortBy === next.sortBy &&
      prev.sortDir === next.sortDir &&
      prev.trashOnly === next.trashOnly
    ) {
      return;
    }
    set({ filter: next });
    useSelectionStore.getState().clear(); // B09：筛选变更清空选中，避免跨筛选残留不可见 id
    void get().refresh();
  },

  clearTagFilters: () => {
    const prev = get().filter;
    const unchanged =
      prev.untaggedOnly === false &&
      prev.tagId === null &&
      (prev.facetFilters?.length ?? 0) === 0 &&
      (prev.excludeTagIds?.length ?? 0) === 0;
    if (unchanged) return;
    set({
      filter: {
        ...prev,
        untaggedOnly: false,
        tagId: null,
        facetFilters: [],
        excludeTagIds: [],
      },
    });
    useSelectionStore.getState().clear();
  },

  refresh: async () => {
    const seq = ++requestSeq;
    const f = get().filter;
    set({ loading: true, error: null });
    try {
      const page = await listAssets(toApiFilter(f, 0, PAGE_SIZE));
      // P1-01：响应返回时筛选可能已变更/更新请求已发出——过期响应直接丢弃，
      // 不写 items/total、不碰 loading（loading 归最新请求管）
      if (seq !== requestSeq) return;
      const items = dedupItems(page.items);
      set({ items, viewItems: applyKinshipView(items), total: page.total, loading: false });
      markStartup("library_ready"); // §4.1：素材列表 ready 打点（首次成功刷新即首屏 ready）
    } catch (e) {
      if (seq !== requestSeq) return;
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  loadMore: async () => {
    const { items, total, loading, filter } = get();
    if (loading || items.length >= total) return;
    const seq = ++requestSeq;
    set({ loading: true });
    try {
      // P1-01：用旧 items.length 做 offset 的请求在筛选变更后 offset 必然错位，
      // 若期间已有新请求（refresh/loadMore），旧响应必须丢弃
      const page = await listAssets(toApiFilter(filter, items.length, PAGE_SIZE));
      if (seq !== requestSeq) return;
      const known = new Set(items.map((a) => a.id));
      const next = [...items, ...page.items.filter((a) => !known.has(a.id))];
      set({
        items: next,
        viewItems: applyKinshipView(next),
        total: page.total,
        loading: false,
      });
    } catch (e) {
      if (seq !== requestSeq) return;
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  fetchAllIds: async () => {
    const { filter } = get();
    // 走 list_asset_ids：只取 id 数组，不拉完整 Asset、不依赖 total/limit
    return listAssetIds(toApiFilter(filter, 0));
  },

  removeLocal: (ids) => {
    const gone = new Set(ids);
    set((s) => {
      // B09：只减当前视图内实际移除的数量，避免选中含跨筛选 id 时 total 多减
      const removedInView = s.items.filter((a) => gone.has(a.id)).length;
      const items = s.items.filter((a) => !gone.has(a.id));
      return {
        items,
        viewItems: applyKinshipView(items),
        total: Math.max(0, s.total - removedInView),
      };
    });
  },

  patchLocal: (ids, patch) => {
    const hit = new Set(ids);
    set((s) => {
      const items = s.items.map((a) => (hit.has(a.id) ? { ...a, ...patch } : a));
      return { items, viewItems: applyKinshipView(items) };
    });
  },

  refreshPaletteFields: async (updatedIds) => {
    // 1) 去重
    const ids = Array.from(new Set(updatedIds));
    if (ids.length === 0) return;
    // 2) 与调用时当前 items 求交集；空交集立即返回，不发 IPC
    const currentIds = new Set(get().items.map((a) => a.id));
    const intersect = ids.filter((id) => currentIds.has(id));
    if (intersect.length === 0) return;
    // 3) 每批最多 1000 个 id 定向读取补丁
    const patches: AssetPalettePatch[] = [];
    for (let i = 0; i < intersect.length; i += 1000) {
      const batch = await getAssetPalettePatches(intersect.slice(i, i + 1000));
      patches.push(...batch);
    }
    if (patches.length === 0) return;
    // 4) 提交瞬间基于最新 s.items 合并（期间翻页/过滤/删除只影响仍存在的 id）
    set((s) => {
      const byId = new Map(patches.map((p) => [p.id, p]));
      if (byId.size === 0) return s;
      return {
        items: s.items.map((a) => {
          const p = byId.get(a.id);
          // 未命中保持原引用；命中只替换色板相关字段（不先清空再填充，避免色条闪烁）
          if (!p) return a;
          return {
            ...a,
            palette: p.palette,
            dominantHue: p.dominantHue,
            dominantSat: p.dominantSat,
            dominantLum: p.dominantLum,
          };
        }),
      };
    });
  },

  viewerOpen: false,
  setViewerOpen: (open) => {
    // 关闭 Viewer 时恢复网格滚动（组件写回 gridScrollTop；此处负责清零状态）
    set({ viewerOpen: open, ...(open ? {} : {}) });
  },

  gridScrollTops: {},
  setGridScrollTop: (key, top) => set((s) => ({ gridScrollTops: { ...s.gridScrollTops, [key]: top } })),
  getGridScrollTop: (key) => get().gridScrollTops[key] ?? 0,
}));
