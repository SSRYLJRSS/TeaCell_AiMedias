/** 标签树形导航：可折叠、父标签显示合计数、点击连带筛选子标签（PRD 5.4-3）
 *  FB6 需求四：顶部统一标题行——左「智能标签」、右「全部展开/全部收起」（不与文件属性共用状态）。
 *  首次进入默认收起全部分面，用户操作后再使用显式折叠状态。 */
import { useEffect, useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import clsx from "clsx";
import { useShallow } from "zustand/react/shallow";
import { flattenVisible, useTagStore } from "@/stores/tagStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useAiStore } from "@/stores/aiStore";

interface TagTreeProps {
  onManage?: () => void;
}

export default function TagTree({ onManage }: TagTreeProps) {
  const { tree, facets, refresh } = useTagStore(
    useShallow((s) => ({
      tree: s.tree,
      facets: s.facets,
      refresh: s.refresh,
    })),
  );
  const { filter, setFilter } = useLibraryStore(useShallow((s) => ({ filter: s.filter, setFilter: s.setFilter })));
  const { total, fetchAllIds } = useLibraryStore(useShallow((s) => ({ total: s.total, fetchAllIds: s.fetchAllIds })));

  // W5g（指导书 §W5g）：一键送打标——把当前「未打标」筛选结果全部送进 AI 打标页。
  // 全部是既有能力组合：fetchAllIds（当前筛选全量 id）→ aiStore.setPendingAssets → app:navigate。
  const [sending, setSending] = useState(false);
  const onSendUntagged = async () => {
    if (sending) return;
    setSending(true);
    try {
      const ids = await fetchAllIds();
      if (ids.length === 0) return;
      useAiStore.getState().setPendingAssets(ids);
      window.dispatchEvent(new CustomEvent("app:navigate", { detail: "ai" }));
    } finally {
      setSending(false);
    }
  };

  useEffect(() => {
    if (tree.length === 0) void refresh();
  }, [tree.length, refresh]);

  // 分面分组与文件属性保持相同的折叠模型：分组标题负责展开/收起，组内仍保留标签树层级。
  const [collapsedFacets, setCollapsedFacets] = useState<ReadonlySet<string> | null>(null);
  const [expandedNodes, setExpandedNodes] = useState<ReadonlySet<number>>(new Set());
  const groups = useMemo(
    () => (facets.length > 0
      ? facets.map((facet) => ({ facet, roots: tree.filter((n) => n.tag.facetKey === facet.key) }))
      : [{ facet: null, roots: tree }])
      .filter(({ roots }) => roots.length > 0),
    [facets, tree],
  );
  const defaultCollapsedFacets = useMemo(
    () => new Set(groups.map(({ facet }) => facet?.key ?? "legacy")),
    [groups],
  );
  const collapsedFacetKeys = collapsedFacets ?? defaultCollapsedFacets;
  const allCollapsed = groups.length > 0 && groups.every(({ facet }) => collapsedFacetKeys.has(facet?.key ?? "legacy"));
  const toggleFacet = (key: string) => {
    setCollapsedFacets((current) => {
      const next = new Set(current ?? defaultCollapsedFacets);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const toggleAllFacets = () => {
    setCollapsedFacets(allCollapsed ? new Set() : new Set(groups.map(({ facet }) => facet?.key ?? "legacy")));
  };
  const toggleNode = (id: number) => {
    setExpandedNodes((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };
  const currentFilters = filter.facetFilters ?? [];
  const excludedIds = filter.excludeTagIds ?? [];
  const [menu, setMenu] = useState<{ tagId: number; x: number; y: number } | null>(null);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  const toggleExcluded = (tagId: number) => {
    const excluded = excludedIds.includes(tagId);
    setFilter({
      excludeTagIds: excluded ? excludedIds.filter((id) => id !== tagId) : [...excludedIds, tagId],
      untaggedOnly: false,
      trashOnly: false,
    });
    setMenu(null);
  };

  return (
    <div className="flex flex-col py-1 text-sm">
      {/* 与文件属性统一：标题行控制全部分组，下面每个分面标题可独立折叠。 */}
      <div className="flex min-h-8 items-center justify-between px-2">
        <h4 className="text-[11px] font-semibold text-[var(--color-text)]">智能标签</h4>
        {groups.length > 0 && (
          <button
            type="button"
            onClick={toggleAllFacets}
            aria-label={allCollapsed ? "全部展开" : "全部收起"}
            title={allCollapsed ? "展开所有智能标签分组" : "收起所有智能标签分组"}
            className="px-1.5 py-1 text-[10px] text-[var(--color-text-secondary)] transition-colors hover:text-[var(--color-text)]"
          >
            {allCollapsed ? "全部展开" : "全部收起"}
          </button>
        )}
      </div>
      {/* 「未打标」归标签区（v2.7 修订）；点标签行筛选，再点已激活标签取消。
          W5g：激活未打标筛选时行尾出现「送去打标」按钮，一键把结果送进 AI 打标页 */}
      <div className="flex items-center gap-1 pr-2">
        <TreeRow
          label="未打标"
          active={filter.untaggedOnly}
          onClick={() => setFilter({ tagId: null, facetFilters: [], excludeTagIds: [], untaggedOnly: true, trashOnly: false })}
        />
        {filter.untaggedOnly && (
          <button
            type="button"
            disabled={sending || total === 0}
            onClick={() => void onSendUntagged()}
            title="把当前未打标的素材全部送进 AI 打标页"
            className="shrink-0 rounded-md px-1.5 py-1 text-[10px] font-medium text-[var(--color-accent)] transition-colors hover:bg-[var(--color-surface)] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {sending ? "整理中…" : `送去打标（${total} 张）`}
          </button>
        )}
      </div>

      {groups.map(({ facet, roots }) => {
        const facetKey = facet?.key ?? "legacy";
        const collapsed = collapsedFacetKeys.has(facetKey);
        const rows = flattenVisible(roots, expandedNodes);
        return (
        <section key={facetKey} className="pt-2">
          <button
            type="button"
            onClick={() => toggleFacet(facetKey)}
            className="flex w-full items-center justify-between px-2 pb-1 text-left"
            aria-label={facet?.displayName ?? "标签"}
            aria-expanded={!collapsed}
          >
            <span className="block text-[11px] font-semibold text-[var(--color-text)]">
              {facet?.displayName ?? "标签"}
            </span>
            <span className="ml-2 text-xs text-[var(--color-text-tertiary)]">
              {collapsed ? "▸" : "▾"}
            </span>
          </button>
          {!collapsed && rows.map(({ node, depth }) => {
        const hasChildren = node.children.length > 0;
        const isOpen = expandedNodes.has(node.tag.id);
        const active = currentFilters.some((f) => f.tagIds.includes(node.tag.id)) && !filter.untaggedOnly;
        const excluded = excludedIds.includes(node.tag.id);
        return (
          <div key={node.tag.id} className="flex items-center" style={{ paddingLeft: depth * 14 }}>
            <button
              aria-label={isOpen ? "折叠" : "展开"}
              onClick={() => hasChildren && toggleNode(node.tag.id)}
              className={clsx(
                "w-4 shrink-0 text-[10px] text-[var(--color-text-secondary)]",
                !hasChildren && "invisible",
              )}
            >
              {isOpen ? "▾" : "▸"}
            </button>
            <TreeRow
              label={node.tag.name}
              count={node.tag.totalCount}
              active={active}
              excluded={excluded}
              onContextMenu={(event) => {
                event.preventDefault();
                setMenu({
                  tagId: node.tag.id,
                  x: Math.min(event.clientX, window.innerWidth - 150),
                  y: Math.min(event.clientY, window.innerHeight - 90),
                });
              }}
              onClick={() => {
                const existing = currentFilters.find((f) => f.facetKey === node.tag.facetKey);
                const ids = existing?.tagIds ?? [];
                const nextIds = active ? ids.filter((id) => id !== node.tag.id) : [...ids, node.tag.id];
                const rest = currentFilters.filter((f) => f.facetKey !== node.tag.facetKey);
                const facetFilters = nextIds.length > 0
                  ? [...rest, {
                      facetKey: node.tag.facetKey,
                      tagIds: nextIds,
                      mode: existing?.mode ?? ("any" as const),
                      includeDescendants: existing?.includeDescendants ?? true,
                    }]
                  : rest;
                setFilter({ tagId: null, facetFilters, untaggedOnly: false, trashOnly: false });
              }}
            />
          </div>
        );
          })}
        </section>
        );
      })}

      {tree.length === 0 && (
        <p className="px-3 py-2 text-xs text-[var(--color-text-secondary)]">暂无标签</p>
      )}

      {menu && (
        <div
          className="fixed z-50 min-w-32 border border-[var(--color-border)] bg-[var(--color-bg)] py-1 text-xs shadow-lg"
          style={{ left: menu.x, top: menu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="block w-full px-3 py-1.5 text-left text-[var(--color-text)] hover:bg-[var(--color-surface)]"
            onClick={() => toggleExcluded(menu.tagId)}
          >
            {excludedIds.includes(menu.tagId) ? "取消排除" : "排除此标签"}
          </button>
          {onManage && (
            <button
              className="block w-full px-3 py-1.5 text-left text-[var(--color-text)] hover:bg-[var(--color-surface)]"
              onClick={() => {
                setMenu(null);
                onManage();
              }}
            >
              管理标签
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function TreeRow({
  label,
  count,
  active,
  excluded = false,
  onClick,
  onContextMenu,
}: {
  label: string;
  count?: number;
  active: boolean;
  excluded?: boolean;
  onClick: () => void;
  onContextMenu?: (event: ReactMouseEvent<HTMLButtonElement>) => void;
}) {
  return (
    <button
      onClick={onClick}
      onContextMenu={onContextMenu}
      // M-13：长中文标签会被 truncate 截断，必须能 hover 看到全量（不撑破布局即可截断，但不能只截断不给全量）
      title={label}
      className={clsx(
        "ui-nav-item ui-nav-list-item flex min-h-8 flex-1 items-center justify-between px-2 py-1.5 text-left text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]",
      )}
      data-active={active}
    >
      <span className={clsx("truncate", excluded && "line-through opacity-60")}>{label}</span>
      {count != null && <span className="ml-2 min-w-6 shrink-0 text-right text-xs text-[var(--color-text-tertiary)]">{count}</span>}
    </button>
  );
}
