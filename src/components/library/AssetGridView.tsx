/** 通用素材网格（P2.4）：受控组件，不 import useLibraryStore。
 *  虚拟滚动、选中/批量操作、右键菜单全部在此；AssetGrid 只是普通素材库的薄封装。
 *  FB2-01/02（§9）：格子档位由 appearance.grid 驱动 + 统一比例 + 填充方式；Alt/Ctrl/Cmd+滚轮与
 *  Ctrl/Cmd+± 增减档位；滚动抑制窗下不激活 hover 预览（FB2-03 护栏，经 GridScrollingContext 传递）。 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useShallow } from "zustand/react/shallow";
import AssetCard from "./AssetCard";
import ContextMenu, { type MenuEntry } from "@/components/common/ContextMenu";
import { getAssetUrls, revealInFolder } from "@/api/assets";
import { openFileExternal } from "@/api/import";
import { useElementSize, useEscape } from "@/hooks/hooks";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useAppearance } from "@/hooks/useAppearance";
import { useSettingsStore } from "@/stores/settingsStore";
import { GridScrollingContext } from "@/components/library/GridScrollContext";
import { CELL_STEPS } from "@/types/settings";
import { ASPECT_RATIO } from "@/utils/cellFit";
import { thumbSizeForCell } from "@/utils/thumbSize";
import { HEIGHT_PX, type PaletteSegment } from "@/components/library/ColorStrip";
import type { Asset } from "@/types/asset";
import type { FetchAllIdsResult } from "@/types/superSearch";

export interface AssetGridViewProps extends LibraryGridActions {
  items: Asset[];
  total: number;
  loading: boolean;
  loadMore: () => void;
  /** §4.6（B2/B8）：全选 ID 一路到底 —— FetchAllIdsResult（= PlanIdsResult，不降级成 number[]） */
  fetchAllIds: () => Promise<FetchAllIdsResult>;
  /** 查询条件代次；全选请求返回时若条件已变更，丢弃旧 ID 集合。 */
  selectionRevision?: number | string;
  onPreview: (asset: Asset) => void;
  /** §12（FB-06）：外部主滚动容器。缺省时内部自建可滚容器（普通库页）；
   *  提供时虚拟滚动使用外部容器，内部不再 overflow 自身。 */
  scrollElementRef?: React.RefObject<HTMLElement | null>;
  /** FB2-06（§7.4 方案 D）：滚动位置隔离键。素材库传 "library"、超级搜索传 "superSearch"；
   *  null / 省略 = 不保存不恢复。修复两页滚动位置互相污染。 */
  scrollRestoreKey?: string | null;
  /** FB2-08（§14.9）：点击卡片色条主色段以同色系搜索（LibraryPage/SuperSearchPage 提供） */
  onSearchDominant?: (segment: PaletteSegment) => void;
  /** R0-5：空结果态用哪个筛选源。素材库缺省读 useLibraryStore.filter；
   *  超级搜索传自身的条件态，否则会误显示「素材库还是空的 / 去导入素材」。 */
  hasActiveFilter?: boolean;
  /** R0-5：清除筛选按钮的回调。素材库缺省清 useLibraryStore.filter；
   *  超级搜索传 clearConditions（否则按钮对超搜条件完全无效）。 */
  onClearFilter?: () => void;
  /** U-7③：空结果时的归零条件列表（来自 C-2 诊断）—— 每条可单点移除 */
  zeroingActions?: { key: string; label: string; onRemove: () => void }[];
}

/** 批量操作入口（顶栏与右键菜单共用） */
export interface LibraryGridActions {
  onTag: () => void;
  onExport: () => void;
  onMove: () => void;
  onDelete: () => void;
}

const GAP = 8;
/** FB2-03：滚动停止后多少毫秒视为「静止」，可重新允许 hover 预览 */
const SCROLL_SUPPRESS_MS = 150;

export default function AssetGridView({
  items,
  total,
  loading,
  loadMore,
  fetchAllIds,
  selectionRevision,
  onPreview,
  onTag,
  onExport,
  onMove,
  onDelete,
  scrollElementRef,
  scrollRestoreKey,
  onSearchDominant,
  hasActiveFilter: hasActiveFilterProp,
  onClearFilter,
  zeroingActions,
}: AssetGridViewProps) {
  const { selected, truncated, selectionTotal, toggle, rangeTo, clear, setAll, invert } = useSelectionStore(
    useShallow((s) => ({
      selected: s.selected,
      truncated: s.truncated,
      selectionTotal: s.selectionTotal,
      toggle: s.toggle,
      rangeTo: s.rangeTo,
      clear: s.clear,
      setAll: s.setAll,
      invert: s.invert,
    })),
  );

  // §4.6 截断策略：全选 → 记录 truncated/total；反选 → 触顶直接禁止（「不在前十万里」≠「不匹配」）
  const selectionRevisionRef = useRef(selectionRevision);
  // 在渲染阶段同步更新，避免一个已 resolve 的 Promise 在 effect 刷新前短暂接受旧结果。
  selectionRevisionRef.current = selectionRevision;
  const selectAll = useCallback(() => {
    const revisionAtRequest = selectionRevision;
    void fetchAllIds().then((r) => {
      if (revisionAtRequest !== undefined && selectionRevisionRef.current !== revisionAtRequest) return;
      setAll(r.ids, { truncated: r.truncated, total: r.total });
    });
  }, [fetchAllIds, selectionRevision, setAll]);
  const invertAll = useCallback(() => {
    const revisionAtRequest = selectionRevision;
    void fetchAllIds().then((r) => {
      if (revisionAtRequest !== undefined && selectionRevisionRef.current !== revisionAtRequest) return;
      if (r.truncated) {
        window.alert("当前结果超过 100000 张，反选在截断集合上没有定义，请先收窄条件。");
        return;
      }
      invert(r.ids, { truncated: false, total: r.total });
    });
  }, [fetchAllIds, invert, selectionRevision]);
  // §4.6：可逆性差的批量操作在截断集合上二次确认；删除/反选直接禁用（见菜单）
  const guardTruncated = useCallback(
    (action: () => void, noun: string) => () => {
      if (truncated) {
        const ok = window.confirm(`将${noun}已选中的 ${selected.size} 张（共 ${selectionTotal} 张匹配）。继续？`);
        if (!ok) return;
      }
      action();
    },
    [truncated, selectionTotal, selected.size],
  );
  const { ref, width } = useElementSize<HTMLDivElement>();

  // FB2-01/02：外观驱动尺寸
  const { grid, colorStrip } = useAppearance();
  const cell = CELL_STEPS[grid.libraryCellStep] ?? CELL_STEPS[3];
  // 冷启动时 WebView2 可能在父级布局完成前短暂回报 width=0。不能用 0 算列数/行高，
  // 否则 Virtualizer 会缓存成「2 列 + 几像素行高」，实际大卡片便逐行重叠。
  const layoutReady = Number.isFinite(width) && width > GAP;
  const columns = layoutReady
    ? Math.max(2, Math.floor((width + GAP) / (cell + GAP)))
    : 1;
  const rowCount = layoutReady ? Math.ceil(items.length / columns) : 0;
  const orderedIds = useMemo(() => items.map((a) => a.id), [items]);
  const cellWidth = layoutReady
    ? Math.max(1, (width - GAP * (columns - 1)) / columns)
    : cell;
  const [rw, rh] = ASPECT_RATIO[grid.cellAspect] ?? ASPECT_RATIO["4:3"];
  // FB2-08（FX-07 隐藏坑）：色条在媒体容器之外，开启后每张卡片实际高度多出 stripPx，
  // 不补进 rowHeight 虚拟滚动会逐行累积错位（滚动时卡片重叠/大片空白）。
  const stripPx =
    colorStrip.enabled && colorStrip.showInLibraryGrid ? HEIGHT_PX[colorStrip.height] : 0;
  const rowHeight = cellWidth * (rh / rw) + stripPx + GAP;
  const thumbSize = thumbSizeForCell(cell);

  // §7.2 / FB2-06：Viewer 关闭后恢复网格滚动位置（按键隔离，scrollRestoreKey 为空时跳过）
  useEffect(() => {
    if (!scrollRestoreKey) return;
    const el = scrollElementRef?.current ?? ref.current;
    const saved = useLibraryStore.getState().getGridScrollTop(scrollRestoreKey);
    if (el && saved > 0) el.scrollTop = saved;
    // 仅挂载时恢复一次（Virtualizer 接管后续滚动）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scrollRestoreKey]);

  // 滚动位置写入 store（Viewer 打开前最后值；滚动容器卸载再恢复）
  useEffect(() => {
    const el = scrollElementRef?.current ?? ref.current;
    if (!el || !scrollRestoreKey) return;
    let raf = 0;
    const onScroll = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        useLibraryStore.getState().setGridScrollTop(scrollRestoreKey, el.scrollTop);
      });
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(raf);
    };
  }, [scrollElementRef, ref, scrollRestoreKey]);

  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollElementRef?.current ?? ref.current,
    estimateSize: () => rowHeight,
    overscan: 3,
    enabled: layoutReady,
  });

  // 比例/格宽变化后必须显式 re-measure（TanStack Virtual 不会因 estimateSize 闭包变化自动重算）
  useEffect(() => {
    if (!layoutReady) return;
    virtualizer.measure();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layoutReady, rowHeight]);

  useEffect(() => {
    const last = virtualizer.getVirtualItems().at(-1);
    if (last && last.index >= rowCount - 2 && items.length < total) void loadMore();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [virtualizer.range, rowCount, items.length, total, loadMore]);

  useEscape(useCallback(() => clear(), [clear]), selected.size > 0);

  const handleSelect = useCallback(
    (asset: Asset, index: number, e: React.MouseEvent) => {
      if (e.shiftKey) {
        rangeTo(index, orderedIds);
      } else {
        // E-1：普通点击/Ctrl/Command 都交给 store 决定添加或删除（additive toggle）。
        toggle(asset.id, index, true);
      }
    },
    [toggle, rangeTo, orderedIds],
  );

  const handlePreview = useCallback((asset: Asset) => onPreview(asset), [onPreview]);

  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  // FB2-03 护栏：滚动中及停止后 SCROLL_SUPPRESS_MS 内，网格不激活 hover 预览。
  const scrollingRef = useRef(false);
  useEffect(() => {
    const el = scrollElementRef?.current ?? ref.current;
    if (!el) return;
    let t = 0;
    const disarm = () => {
      scrollingRef.current = true;
      window.clearTimeout(t);
      t = window.setTimeout(() => {
        scrollingRef.current = false;
      }, SCROLL_SUPPRESS_MS);
    };
    el.addEventListener("scroll", disarm, { passive: true });
    return () => {
      el.removeEventListener("scroll", disarm);
      window.clearTimeout(t);
    };
  }, [ref, scrollElementRef]);
  const isScrolling = useCallback(() => scrollingRef.current, []);

  // FB2-01：档位步进（滚轮/键盘/工具栏共用）。切换前记录中心锚点，切换后尽量回到同一位置。
  const stepCell = useCallback(
    (delta: number) => {
      const cur = grid.libraryCellStep;
      const next = Math.max(0, Math.min(CELL_STEPS.length - 1, cur + delta));
      if (next === cur) return;
      const el = scrollElementRef?.current ?? ref.current;
      const centerY = (el?.scrollTop ?? 0) + (el?.clientHeight ?? 0) / 2;
      const anchorRow = Math.floor(centerY / Math.max(1, rowHeight));
      const anchorIdx = Math.min(Math.max(0, items.length - 1), anchorRow * columns);
      useSettingsStore.getState().commitAppearanceDebounced((a) => ({
        ...a,
        grid: { ...a.grid, libraryCellStep: next },
      }));
      requestAnimationFrame(() => virtualizer.scrollToIndex(anchorIdx, { align: "center" }));
    },
    [grid.libraryCellStep, rowHeight, columns, items.length, scrollElementRef, ref, virtualizer],
  );

  // FB2-01：Alt（用户点名）+ Ctrl/Cmd（行业惯例）滚轮增减档位；阻止 WebView2 页面级缩放。
  // Ctrl+滚轮是可取消 wheel 事件（passive:false + preventDefault）——Leaflet/Mapbox 标准做法。
  useEffect(() => {
    const el = scrollElementRef?.current ?? ref.current;
    if (!el) return;
    let raf = 0;
    let lastStepAt = 0;
    const onWheel = (e: WheelEvent) => {
      if (!e.altKey && !e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      const now = performance.now();
      if (now - lastStepAt < 60) return; // 每档最小间隔，触控板惯性一次几十个 wheel
      lastStepAt = now;
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => stepCell(e.deltaY < 0 ? 1 : -1));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      cancelAnimationFrame(raf);
    };
  }, [stepCell, scrollElementRef, ref]);

  // FB2-01：Ctrl/Cmd + = / - 增减一档（复用既有 keydown 效果，避开 INPUT 与菜单打开态）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (menu) return;
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "a" || e.key === "A") {
        e.preventDefault();
        selectAll();
      } else if (e.key === "i" || e.key === "I") {
        e.preventDefault();
        invertAll();
      } else if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        stepCell(1);
      } else if (e.key === "-" || e.key === "_") {
        e.preventDefault();
        stepCell(-1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectAll, invertAll, menu, stepCell]);

  const handleContextMenu = useCallback(
    (asset: Asset, _index: number, e: React.MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (!selected.has(asset.id)) toggle(asset.id, _index, true);
      setMenu({ x: e.clientX, y: e.clientY });
    },
    [selected, toggle],
  );

  const handleBlankContextMenu = useCallback((e: React.MouseEvent) => {
    if (e.target !== e.currentTarget) return;
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY });
  }, []);

  const copyPaths = useCallback(async () => {
    const paths = await getAssetUrls([...selected]);
    await navigator.clipboard.writeText(paths.join("\n"));
  }, [selected]);

  const revealFirst = useCallback(async () => {
    const first = Array.from(selected)[0];
    if (first == null) return;
    const paths = await getAssetUrls([first]);
    if (paths[0]) await revealInFolder(paths[0]);
  }, [selected]);

  // W5f-f4：用系统默认程序打开（openFileExternal 在 api/import.ts，入库页同款）
  const openFirstExternal = useCallback(async () => {
    const first = Array.from(selected)[0];
    if (first == null) return;
    const paths = await getAssetUrls([first]);
    if (paths[0]) await openFileExternal(paths[0]);
  }, [selected]);

  // W5f-f1：当前是否带任何筛选/搜索条件（区分「库为空」与「筛选无结果」）
  // R0-5：超级搜索传自己的条件态（props），缺省才回退读素材库 store ——
  // 否则超搜 0 结果会误显示「素材库还是空的 / 去导入素材」。
  const storeHasFilter = useLibraryStore((st) => {
    const f = st.filter;
    return (
      f.assetType !== "all" ||
      f.untaggedOnly ||
      f.tagId != null ||
      (f.facetFilters ?? []).length > 0 ||
      (f.excludeTagIds ?? []).length > 0 ||
      (f.metadataFilters ?? []).length > 0 ||
      f.search.trim() !== ""
    );
  });
  const hasActiveFilter = hasActiveFilterProp ?? storeHasFilter;

  const menuEntries = useMemo((): MenuEntry[] => {
    // §4.6：反选在截断集合上禁止（结果没有定义）；全选允许但记 truncated
    const common: MenuEntry[] = [
      { label: "全选", onClick: selectAll },
      { label: "反选", disabled: truncated, title: truncated ? "当前结果超过 100000 张，反选在截断集合上没有定义" : undefined, onClick: invertAll },
    ];
    if (selected.size === 0) return common;
    return [
      { label: "打标", onClick: guardTruncated(onTag, "批量打标") },
      { label: "导出", onClick: guardTruncated(onExport, "导出") },
      { label: "移动到…", onClick: guardTruncated(onMove, "移动") },
      // §4.6：删除（移入回收站）在截断集合上禁止 —— 不可逆 + 集合不完整 = 最坏组合
      {
        label: "删除",
        disabled: truncated,
        title: truncated ? "当前结果超过 100000 张，请先收窄条件再删除" : undefined,
        onClick: onDelete,
      },
      { divider: true },
      { label: "复制路径", onClick: () => void copyPaths() },
      // W5f-f4：用默认程序打开（单选；Lightroom/Capture One 打开 RAW 是高频动作）
      {
        label: "用默认程序打开",
        disabled: selected.size !== 1,
        onClick: () => void openFirstExternal(),
      },
      { label: "打开所在文件夹", disabled: selected.size !== 1, onClick: () => void revealFirst() },
      { divider: true },
      ...common,
      { label: "取消选择", onClick: clear },
    ];
  }, [selected.size, truncated, selectAll, invertAll, guardTruncated, onTag, onExport, onMove, onDelete, copyPaths, revealFirst, openFirstExternal, clear]);

  if (items.length === 0) {
    return (
      <div
        ref={ref}
        className={
          scrollElementRef
            ? "flex min-h-0 flex-1 items-center justify-center text-sm text-[var(--color-text-secondary)]"
            : "flex h-full flex-1 items-center justify-center text-sm text-[var(--color-text-secondary)]"
        }
      >
        {loading ? (
          "加载中…"
        ) : total === 0 ? (
          // W5f-f1：区分「库为空」（引导导入）与「筛选无结果」（清除筛选）
          hasActiveFilter ? (
            <div className="flex flex-col items-center gap-2">
              <span>没有符合条件的素材</span>
              {zeroingActions && zeroingActions.length > 0 && (
                <div className="flex max-w-full flex-wrap items-center justify-center gap-1">
                  <span className="text-[11px] text-[var(--color-text-tertiary)]">以下条件把结果砍到 0：</span>
                  {zeroingActions.map((z) => (
                    <span key={z.key} className="inline-flex h-7 max-w-[260px] items-center gap-1 rounded-full border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 pl-2.5 pr-0.5 text-xs text-[var(--color-danger)]">
                      <span className="truncate">{z.label}</span>
                      <button type="button" onClick={z.onRemove} aria-label={`移除归零条件 ${z.label}`} title="移除这个把结果砍到 0 的条件" className="flex size-5 shrink-0 items-center justify-center rounded-full hover:bg-[var(--color-surface-hover)]">×</button>
                    </span>
                  ))}
                </div>
              )}
              <button
                type="button"
                onClick={() =>
                  onClearFilter
                    ? onClearFilter()
                    : useLibraryStore.getState().setFilter({ assetType: "all", untaggedOnly: false, tagId: null, facetFilters: [], excludeTagIds: [], metadataFilters: [], search: "" })
                }
                className="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
              >
                清除筛选条件
              </button>
            </div>
          ) : (
            <div className="flex flex-col items-center gap-2">
              <span>素材库还是空的</span>
              <span className="text-xs text-[var(--color-text-tertiary)]">先导入一些图片或视频，再配置 AI 打标</span>
              <button
                type="button"
                onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "import" }))}
                className="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-[var(--color-accent-text)]"
              >
                去导入素材
              </button>
            </div>
          )
        ) : (
          "没有匹配的素材"
        )}
      </div>
    );
  }

  return (
    <GridScrollingContext.Provider value={isScrolling}>
      <div
        ref={ref}
        className={
          scrollElementRef
            ? "min-w-0 flex-1 p-2"
            : "h-full min-w-0 flex-1 overflow-y-auto p-2"
        }
        onClick={(e) => {
          if (e.target === e.currentTarget) clear();
        }}
        onContextMenu={handleBlankContextMenu}
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((row) => (
            <div
              key={row.key}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${row.start}px)`,
                display: "grid",
                gridTemplateColumns: `repeat(${columns}, 1fr)`,
                gap: GAP,
                paddingBottom: GAP,
              }}
            >
              {Array.from({ length: columns }, (_, c) => {
                const idx = row.index * columns + c;
                const asset = items[idx];
                if (!asset) return <div key={`ph-${row.index}-${c}`} />;
                return (
                  <AssetCard
                    key={asset.id}
                    asset={asset}
                    index={idx}
                    selected={selected.has(asset.id)}
                    thumbSize={thumbSize}
                    onSelect={handleSelect}
                    onPreview={handlePreview}
                    onContextMenu={handleContextMenu}
                    onSearchDominant={onSearchDominant}
                  />
                );
              })}
            </div>
          ))}
        </div>
        {menu && <ContextMenu x={menu.x} y={menu.y} entries={menuEntries} onClose={() => setMenu(null)} />}
      </div>
    </GridScrollingContext.Provider>
  );
}
