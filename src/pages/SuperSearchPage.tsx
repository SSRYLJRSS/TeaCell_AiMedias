/** 超级搜索页（P4 + §12 FB-06 / FB2-06）：搜索优先——中央大搜索框 + 布尔条件公式构建器 + 结果网格。
 *  FB2-06（§7.2 方案 A）：把「可折叠头部」移出滚动容器——折叠只改 header 高度，不再改变滚动容器
 *  scrollHeight，从根上断掉「卸载 ↔ scrollHeight 钳制」的正反馈环（原先的闪烁根因）。
 *  FB6 需求五：不再有顶栏返回按钮和重复「超级搜索」标题——返回统一由底部「素材库」按钮
 *  单击/双击完成（BottomBar.useDoubleAction）。「超级搜索」标识由 AiSearchBar 在搜索框上方显示。
 *  结构：可收缩且可滚动的 header 区（含搜索区 + grid-template-rows 折叠的详细条件）
 *  → 保留最小可见高度的结果滚动容器（只装虚拟化网格）。页面根节点封住溢出，避免文档层滚动标题栏。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { ChevronDown, ChevronUp } from "lucide-react";
import AiSearchBar from "@/components/supersearch/AiSearchBar";
import QueryBuilder from "@/components/supersearch/QueryBuilder";
import FilterChips from "@/components/supersearch/FilterChips";
import AssetGridView from "@/components/library/AssetGridView";
import type { PaletteSegment } from "@/components/library/ColorStrip";
import DeleteDialog from "@/components/dialogs/DeleteDialog";
import ExportDialog from "@/components/dialogs/ExportDialog";
import TagAssignDialog from "@/components/dialogs/TagAssignDialog";
import ViewerPage from "@/components/library/ViewerPage";
import { useScrollDirection } from "@/hooks/useScrollDirection";
import { useSuperSearchStore } from "@/stores/superSearchStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useAiStore } from "@/stores/aiStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { diagnoseSearchPlan } from "@/api/superSearch";
import type { Asset } from "@/types/asset";
import type { SearchPlanV3 } from "@/types/superSearch";
import { resolvedQueryToPlan } from "@/utils/planUtils";
import { flattenExprForDisplay } from "@/utils/queryExprUtils";
import { dominantFiltersFor } from "@/utils/dominantFilter";

type DialogKey = "delete" | "export" | "tags" | null;

export default function SuperSearchPage() {
  const { refresh, error, total, loading, items, loadMore, fetchAllIds, applyAiSearch, query, setQuery, expr, plan, planRevision, executionWarnings, resolvedTags, clearConditions, removeAtZonePath, applyTermSuggestion } = useSuperSearchStore(
    useShallow((s) => ({
      refresh: s.refresh,
      error: s.error,
      total: s.total,
      loading: s.loading,
      items: s.items,
      loadMore: s.loadMore,
      fetchAllIds: s.fetchAllIds,
      applyAiSearch: s.applyAiSearch,
      query: s.query,
      setQuery: s.setQuery,
      expr: s.expr,
      plan: s.plan,
      planRevision: s.planRevision,
      executionWarnings: s.executionWarnings,
      resolvedTags: s.resolvedTags,
      clearConditions: s.clearConditions,
      removeAtZonePath: s.removeAtZonePath,
      applyTermSuggestion: s.applyTermSuggestion,
    })),
  );
  const selected = useSelectionStore((s) => s.selected);
  const [dialog, setDialog] = useState<DialogKey>(null);
  const [exportMode, setExportMode] = useState<"copy" | "move">("copy");
  const [preview, setPreview] = useState<Asset | null>(null);

  // §7.2/§7.3（与素材库页一致）：打开/关闭 Viewer 同步全局 viewerOpen（App 据此隐藏 BottomBar）。
  // 超搜此前只 setPreview、不同步 viewerOpen，且把 ViewerPage 内联在 flex 列末尾——
  // 头部+结果网格仍占着布局，Viewer 被挤到窗口底部、主图舞台塌陷（详情显示 bug 根因）。
  const openViewer = (asset: Asset) => {
    setPreview(asset);
    useLibraryStore.getState().setViewerOpen(true);
  };
  const closeViewer = () => {
    setPreview(null);
    useLibraryStore.getState().setViewerOpen(false);
  };
  // 兜底：查看器打开期间经标题栏齿轮等跨页离开时，复位全局 viewerOpen，避免 BottomBar 被永久隐藏
  useEffect(() => () => useLibraryStore.getState().setViewerOpen(false), []);
  // U-7③：空结果时列出「把结果砍到 0」的归零条件（C-2 诊断），可单条移除。
  // §3.7 不变式 9：归零条件带 zone + planRevision（两区同下标不混淆；代次过期不渲染删除按钮）
  const [zeroing, setZeroing] = useState<{ zone: "filter" | "mustNot"; path: number[]; label: string }[]>([]);

  // R0-5：超搜自己的空结果判定 —— 有 expr（AI/构建器产物）或扁平 query 非默认，
  // 都算「带条件」，不能显示「素材库还是空的」。清除按钮走 clearConditions。
  const hasActiveFilter = (plan != null && (plan.filter != null || plan.mustNot != null || plan.should.length > 0)) || expr != null || query.search.trim() !== "" || query.assetType !== "all" || query.untaggedOnly || query.facetFilters.length > 0 || query.excludeTagIds.length > 0 || query.metadataFilters.length > 0;

  // §3.9：三区摘要（折叠态常驻）—— 区徽标带条数，点任一展开并滚到对应区；优先区存在时附 B9 排序说明。
  const mustCount = expr ? flattenExprForDisplay(expr, resolvedTags).length : 0;
  const excludeCount = plan?.mustNot ? flattenExprForDisplay(plan.mustNot, resolvedTags).length : 0;
  const shouldCount = plan?.should.length ?? 0;
  const ZONE_TARGET: Record<string, string> = { filter: "qb-zone-filter", should: "qb-zone-should", mustNot: "qb-zone-mustnot" };
  const ZONE_FULL: Record<"filter" | "should" | "mustNot", string> = { filter: "必须满足", should: "优先满足", mustNot: "排除" };
  const zoneSummary: { zone: "filter" | "should" | "mustNot"; label: string; count: number }[] = [
    { zone: "filter" as const, label: "必须", count: mustCount },
    { zone: "should" as const, label: "优先", count: shouldCount },
    { zone: "mustNot" as const, label: "排除", count: excludeCount },
  ].filter((z) => z.count > 0);
  const SORT_NAME: Record<string, string> = {
    created_at: "入库时间", taken_at: "拍摄时间", modified_at: "修改时间",
    file_size: "文件大小", width: "宽度", height: "高度", duration_ms: "视频时长",
  };
  const sortNote = plan && plan.should.length > 0 && plan.ranking?.type === "field"
    ? `按 ${SORT_NAME[plan.ranking.key] ?? plan.ranking.key} ${plan.ranking.dir === "asc" ? "↑" : "↓"} · 优先条件仅决定同值先后`
    : null;


  // U-7③：条件查询 0 结果 → 对当前计划做 AST 诊断，挑出 delta>0 且 result=0 的叶子
  useEffect(() => {
    if (loading || total !== 0 || !hasActiveFilter) {
      setZeroing([]);
      return;
    }
    // §3.7：plan 唯一事实源；空条件（plan null）时由扁平 query 派生
    const diagPlan = (plan ?? resolvedQueryToPlan(query)) as SearchPlanV3;
    if (!diagPlan.filter && !diagPlan.mustNot && diagPlan.should.length === 0) {
      setZeroing([]);
      return;
    }
    const revisionAtRequest = planRevision;
    let alive = true;
    setZeroing([]);
    const timer = window.setTimeout(() => {
      diagnoseSearchPlan(diagPlan, revisionAtRequest)
        .then((d) => {
          if (!alive) return;
          // 不变式 9：诊断返回时代次过期 → 整批丢弃（旧诊断会指着新条件的位置）
          if (d.leaves.some((l) => l.planRevision !== revisionAtRequest)) return;
          setZeroing(d.leaves.filter((l) => l.delta > 0 && l.resultCount === 0).map((l) => ({ zone: l.zone, path: l.path, label: l.label })));
        })
        .catch(() => { if (alive) setZeroing([]); }); // 诊断只读可失败：失败只少展示归零提示
    }, 320);
    return () => { alive = false; window.clearTimeout(timer); };
  }, [loading, total, hasActiveFilter, plan, planRevision, query]);

  // FB2-06（§7.3 方案 C）：非对称阈值 + 顶部区恒展开 + 手动设定抑制窗
  const [chrome, setNode, setChrome] = useScrollDirection({
    collapseThreshold: 24,
    expandThreshold: 12,
    minScrollTop: 48,
    suppressMs: 300,
  });
  const scrollRef = useRef<HTMLElement | null>(null);

  const bindScroll = useCallback(
    (el: HTMLElement | null) => {
      scrollRef.current = el;
      setNode(el);
    },
    [setNode],
  );

  // §7.5 方案 E：仅命中筛选区才强制展开（点结果卡片不展开、不跳动）
  const forceExpand: React.FocusEventHandler = (e) => {
    if (!(e.target as HTMLElement).closest?.("[data-filter-zone]")) return;
    if (chrome !== "expanded") setChrome("expanded");
  };

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (dialog === "export" && selected.size === 0) setDialog(null);
  }, [dialog, selected.size]);

  const actions = {
    onTag: () => {
      useAiStore.getState().setPendingAssets(Array.from(selected));
      window.dispatchEvent(new CustomEvent("app:navigate", { detail: "ai" }));
    },
    onExport: () => {
      setExportMode("copy");
      setDialog("export");
    },
    onMove: () => {
      setExportMode("move");
      setDialog("export");
    },
    onDelete: () => setDialog("delete"),
  };

  // FB2-08（§14.9 / FX-09）：点结果卡片的主色段 → 同色系条件写进查询并刷新。
  // 语义与素材库一致（AssetGrid）：同 key 的旧条件替换而不叠加 ——
  // 两个不相交的 hue 区间 AND 起来恒为空集。
  const onSearchDominant = useCallback(
    (seg: PaletteSegment) => {
      const next = dominantFiltersFor(seg);
      const keys = new Set(next.map((f) => f.key));
      setQuery({
        metadataFilters: [...query.metadataFilters.filter((f) => !keys.has(f.key)), ...next],
        untaggedOnly: false,
      });
    },
    [query.metadataFilters, setQuery],
  );

  // §3.9：展开详细条件并滚到对应区（区徽标点击）；折叠态只改 header 高度，滚动容器 scrollHeight 不变
  const expandToZone = (zone: "filter" | "should" | "mustNot") => {
    setChrome("expanded");
    // 窄窗口的条件编辑器使用页签；先切换到目标区，再执行滚动定位。
    document.getElementById(`qb-tab-${zone}`)?.click();
    window.setTimeout(() => {
      document.getElementById(ZONE_TARGET[zone])?.scrollIntoView?.({ block: "center", behavior: "smooth" } as ScrollIntoViewOptions);
    }, 220); // grid-template-rows 200ms 展开动画结束后再滚
  };
  // §7.2 互斥（同素材库页）：Viewer 打开时整体替换超搜页（搜索头部/结果网格/弹窗全部卸载），
  // 让 ViewerPage 独占整页高度；关闭后由 store 恢复筛选/滚动/选中上下文。
  // 过片数据集显式传「当前搜索结果集」，避免胶片条/上下张错用素材库全量列表。
  if (preview) {
    return (
      <ViewerPage
        asset={preview}
        onClose={closeViewer}
        listItems={items}
        listTotal={total}
        onListLoadMore={loadMore}
      />
    );
  }

  return (
    <div className="relative flex h-full min-h-0 flex-col overflow-hidden">
      {/* FB6 需求五：顶栏已移除（无返回按钮、无重复标题）。结果计数保留在下方摘要行。 */}

      {/* header 区：空间不足时自身滚动；折叠只改 header 高度，不卸载条件面板，
          保留 FB2-06 对 scrollHeight 闪烁回路的修复。 */}
      <div
        data-filter-zone
        onFocusCapture={forceExpand}
        className="min-h-0 shrink overflow-x-hidden overflow-y-auto overscroll-contain"
      >
        {/* 摘要条：搜索框 + chips + 计数（FB5-03 §3.5：文字披露按钮移出，改为中央 Chevron 披露行） */}
        <div className="bg-[var(--color-bg)] px-4 pt-3">
          <div className="mx-auto w-full max-w-[1440px]">
            <AiSearchBar onSubmit={(text) => void applyAiSearch(text)} />
            <div className="mt-1.5 flex items-center gap-2">
              <FilterChips />
              <span className="ml-auto shrink-0 text-[11px] text-[var(--color-text-tertiary)]">{total} 项</span>
            </div>
            {/* §3.9：折叠态保留三区摘要徽标与排序说明；新增条件统一从对应区域进入。 */}
            {(zoneSummary.length > 0 || sortNote) && (
              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1" data-testid="super-search-zone-summary">
                {zoneSummary.map((z) => (
                  <button
                    key={z.zone}
                    type="button"
                    onClick={() => expandToZone(z.zone)}
                    aria-label={`展开到${ZONE_FULL[z.zone]}区（${z.count} 条）`}
                    className="inline-flex h-5 items-center gap-1 rounded-full border border-[var(--color-border)] bg-transparent px-2 text-[11px] text-[var(--color-text-secondary)] hover:border-[var(--color-border-strong)] hover:text-[var(--color-text)]"
                  >
                    <span className="font-medium text-[var(--color-text)]">{z.label}</span>
                    <span>{z.count}</span>
                  </button>
                ))}
                {sortNote && <span className="text-[11px] text-[var(--color-text-tertiary)]">{sortNote}</span>}
              </div>
            )}
            {/* §3.9/B7：执行 warning 黄字栏不随折叠隐藏（executionWarnings；AI 侧 warnings 在 AiSearchBar 常驻区） */}
            {executionWarnings.length > 0 && (
              <div
                role="status"
                className="mt-1 border-l-2 pl-2 text-[11px] leading-5"
                style={{ borderColor: "var(--color-status)", color: "var(--color-text-secondary)" }}
                data-testid="super-search-execution-warnings"
              >
                <p className="text-[var(--color-status)]">有 {executionWarnings.length} 个条件被忽略或调整：</p>
                {executionWarnings.map((w, i) => {
                  // S5 5-4（不变量 11）：零结果建议可点 —— 点了才改条件（applyTermSuggestion）。
                  // 后端文案固定为「词查「X」没有命中。试试相近的词：A、B」。
                  const m = /^(.*)没有命中。试试相近的词：(.+)$/.exec(w.message);
                  const prefix = m ? m[1].replace(/^词查「/, "").replace(/」$/, "") : "";
                  const terms = m ? m[2].split("、").map((s) => s.trim()).filter(Boolean) : [];
                  return (
                    <p key={`${i}-${w.message}`} className="text-[var(--color-status)]">
                      {w.zone ? `[${ZONE_FULL[w.zone]}] ` : ""}{w.message}
                      {terms.length > 0 && (
                        <span className="ml-1 inline-flex flex-wrap items-center gap-1 align-middle">
                          {terms.map((t) => (
                            <button
                              key={t}
                              type="button"
                              data-testid={`term-suggestion-${t}`}
                              title={`点击把原词查条件替换为「${t}」`}
                              onClick={() => applyTermSuggestion(prefix, t, "fuzzy")}
                              className="inline-flex h-5 items-center rounded-full border border-[var(--color-status)] px-1.5 text-[10px] text-[var(--color-status)] hover:bg-[var(--color-surface-hover)]"
                            >
                              试试「{t}」
                            </button>
                          ))}
                        </span>
                      )}
                      {!terms.length && prefix ? null : null}
                    </p>
                  );
                })}
              </div>
            )}
          </div>
        </div>

        {/* 详细条件面板：始终在 DOM，用 grid-template-rows 0fr↔1fr 折叠（§3.5 禁动画 height） */}
        <div
          id="super-search-filters"
          className="grid transition-[grid-template-rows] duration-200 ease-out"
          style={{ gridTemplateRows: chrome === "expanded" ? "1fr" : "0fr" }}
          aria-hidden={chrome !== "expanded"}
        >
          <div
            className="overflow-hidden"
            style={chrome !== "expanded" ? { pointerEvents: "none" } : undefined}
          >
            <div className="mx-auto w-full max-w-[1440px] px-4 pt-3 pb-2">
              <QueryBuilder />
            </div>
          </div>
        </div>

        {/* 唯一的 Chevron 披露行（放在面板下方）：展开时位于三区条件最底部，一键收起看图片；
            收起后面板 0fr 不占位，它自然贴到摘要行下方，作为「展开详细条件」入口。 */}
        <div className="mx-auto w-full max-w-[1440px] px-4">
          <div className="relative flex h-6 items-center justify-center">
            <div className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-[var(--color-border)]" aria-hidden="true" />
            <button
              type="button"
              onClick={() => setChrome(chrome === "expanded" ? "collapsed" : "expanded")}
              aria-expanded={chrome === "expanded"}
              aria-controls="super-search-filters"
              aria-label={chrome === "expanded" ? "收起详细条件" : "展开详细条件"}
              title={chrome === "expanded" ? "收起详细条件" : "展开详细条件"}
              className="relative z-10 flex size-6 items-center justify-center rounded-full bg-[var(--color-bg)] text-[var(--color-text-secondary)] transition-colors hover:text-[var(--color-text)] focus-visible:ring-1 focus-visible:ring-[var(--color-status)]"
            >
              {chrome === "expanded" ? <ChevronUp size={14} strokeWidth={1.75} aria-hidden="true" /> : <ChevronDown size={14} strokeWidth={1.75} aria-hidden="true" />}
            </button>
          </div>
        </div>
      </div>

      {/* 结果滚动容器：保留最低可见高度；结果滚动仍是条件自动收起的监听源。 */}
      <div ref={bindScroll} data-testid="super-search-scroll" className="min-h-40 flex-1 overflow-y-auto overscroll-contain">
        <AssetGridView
          items={items}
          total={total}
          loading={loading}
          loadMore={loadMore}
          fetchAllIds={fetchAllIds}
          selectionRevision={planRevision}
          onPreview={openViewer}
          onSearchDominant={onSearchDominant}
          scrollElementRef={scrollRef}
          scrollRestoreKey="superSearch"
          hasActiveFilter={hasActiveFilter}
          onClearFilter={clearConditions}
          zeroingActions={zeroing.map((z) => ({ key: `${z.zone}:${z.path.join(".")}`, label: z.label, onRemove: () => removeAtZonePath(z.zone, z.path) }))}
          {...actions}
        />
      </div>

      {error && (
        <div className="absolute right-3 bottom-3 rounded-md bg-[var(--color-danger)] px-3 py-2 text-xs text-white shadow-lg">
          {error}
        </div>
      )}

      <DeleteDialog open={dialog === "delete"} onClose={() => setDialog(null)} />
      <ExportDialog open={dialog === "export" && selected.size > 0} initialMode={exportMode} onClose={() => setDialog(null)} />
      <TagAssignDialog open={dialog === "tags"} onClose={() => setDialog(null)} />
    </div>
  );
}
