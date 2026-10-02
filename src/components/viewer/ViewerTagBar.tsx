/** FB4-01 ViewerTagBar：查看器舞台下方、把塌陷空白变成标签展示区（§10）。
 *  按分面分组显示（buildWorkbenchFacets），复用 TagChip。
 *  FB4-01 定稿：展开总高 128px（标题栏 32px + 双栏正文 96px），收起 32px；
 *  折叠动画只切固定高度类 + transition-[height]，不触发 auto 高度级联。
 *  正文为「分面组两栏」grid：每个分面的名称与标签处于同一 grid item，左侧 76px 名称列。
 *  「添加标签」是标题栏右侧独立图标按钮（Plus），不在正文内，避免 button 嵌套 button。
 *  仅承载展示 + 删除；添加仍走容器注入（避免在查看器内再造一个完整打标工作台）。 */
import { memo, useEffect, useMemo, useState } from "react";
import { Plus } from "lucide-react";
import TagChip from "@/components/library/TagChip";
import { useTagStore, buildWorkbenchFacets } from "@/stores/tagStore";
import type { Tag } from "@/types/tag";
import { getFacetNumber } from "@/api/tags";

interface ViewerTagBarProps {
  assetId: number;
  tags: Tag[];
  /** FB5-05（§7.6.1）：一句话描述（素材字段，不限字数）。只读展示，不进标签树/统计。 */
  contentDescription?: string | null;
  onRemoveTag: (tagId: number) => void;
  onAddTag: () => void;
}

export default memo(function ViewerTagBar({ assetId, tags, contentDescription, onRemoveTag, onAddTag }: ViewerTagBarProps) {
  const [collapsed, setCollapsed] = useState(false);
  // 分面唯一事实源 = tag_facets（tagStore.facets），aiFacetConfigs 只覆盖显隐/显示名
  const tagFacets = useTagStore((s) => s.facets);
  const facets = useMemo(
    () => buildWorkbenchFacets(tagFacets).aiGroup,
    [tagFacets],
  );
  // 分组：固定顺序按 facets 出现顺序；未知 facetKey 归「其他」
  const groups = useMemo(() => {
    const byKey = new Map<string, Tag[]>();
    for (const f of facets) byKey.set(f.key, []);
    const other: Tag[] = [];
    for (const t of tags) {
      const list = byKey.get(t.facetKey);
      if (list) list.push(t);
      else other.push(t);
    }
    const out: { key: string; name: string; items: Tag[] }[] = facets
      .filter((f) => (byKey.get(f.key) ?? []).length > 0)
      .map((f) => ({ key: f.key, name: f.displayName, items: byKey.get(f.key)! }));
    if (other.length) out.push({ key: "other", name: "其他", items: other });
    return out;
  }, [tags, facets]);

  // V24（Phase 7-8）：数值分面值展示 —— 该素材的全部数值分面值（手工/AI 确认后落库的）
  const numberFacets = useMemo(() => tagFacets.filter((f) => f.facetKind === "number"), [tagFacets]);
  const [numbers, setNumbers] = useState<{ key: string; name: string; value: number; unit: string }[]>([]);
  useEffect(() => {
    let alive = true;
    void (async () => {
      const out: { key: string; name: string; value: number; unit: string }[] = [];
      for (const f of numberFacets) {
        try {
          const n = await getFacetNumber(assetId, f.key);
          if (n && alive) out.push({ key: f.key, name: f.displayName, value: n.value, unit: f.numUnit ?? "" });
        } catch {
          /* 数值读取失败静默（不阻塞标签展示） */
        }
      }
      if (alive) setNumbers(out);
    })();
    return () => {
      alive = false;
    };
  }, [assetId, numberFacets]);

  // FB5-05（§7.6.1）：描述正文（trim 后为空则不渲染行）
  const description = (contentDescription ?? "").trim();
  const hasDescription = description.length > 0;

  return (
    <div
      className="shrink-0 border-t border-[var(--color-border)] bg-[var(--color-bg)]"
      style={{ height: collapsed ? 32 : 128 }}
    >
      {/* 标题栏（32px）：左为折叠/展开按钮占满剩余宽度，右为独立「添加标签」图标按钮。
          HTML 不允许 button 嵌套 button，故标题栏用 div 承载两个兄弟按钮（FB4-01）。 */}
      <div className="flex h-8 items-center px-4">
        <button
          type="button"
          aria-expanded={!collapsed}
          aria-controls="viewer-tagbar-body"
          onClick={() => setCollapsed((v) => !v)}
          className="flex h-full min-w-0 flex-1 items-center gap-2 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:text-[var(--color-text)]"
        >
          <span className="font-medium">标签</span>
          <span className="text-[var(--color-text-tertiary)]">{tags.length} 项</span>
          <span className="ml-auto select-none">{collapsed ? "展开 ▾" : "收起 ▴"}</span>
        </button>
        <button
          type="button"
          onClick={onAddTag}
          aria-label="添加标签"
          title="添加标签"
          className="ml-1 flex size-7 shrink-0 items-center justify-center rounded-md text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        >
          <Plus size={14} strokeWidth={2} aria-hidden="true" />
        </button>
      </div>
      {!collapsed && (
        <div
          id="viewer-tagbar-body"
          className="grid h-24 grid-cols-2 content-start gap-x-6 gap-y-1 overflow-y-auto overflow-x-hidden px-4"
        >
          {/* FB5-05（§7.6.1）：一句话描述行——正文第一项，col-span-2 横跨两栏；
              普通只读文本（非 TagChip），完整换行；无描述不渲染行。 */}
          {hasDescription && (
            <div className="grid min-w-0 grid-cols-[76px_minmax(0,1fr)] items-start gap-2 break-words">
              <span className="min-w-0 truncate text-[11px] text-[var(--color-text-secondary)]">
                一句话描述
              </span>
              <span className="text-xs leading-5 text-[var(--color-text)]">{description}</span>
            </div>
          )}
          {numbers.length > 0 && (
            <div className="col-span-2 grid min-w-0 grid-cols-[76px_minmax(0,1fr)] items-start gap-2">
              <span className="min-w-0 truncate text-[11px] text-[var(--color-text-secondary)]">数值</span>
              <div className="flex min-w-0 flex-wrap gap-1" data-testid="viewer-facet-numbers">
                {numbers.map((n) => (
                  <span key={n.key} className="inline-flex h-6 items-center rounded-full bg-[var(--color-surface)] px-2 text-xs tabular-nums text-[var(--color-text)]">
                    {n.value}
                    {n.unit}
                  </span>
                ))}
              </div>
            </div>
          )}
          {groups.length === 0 && !hasDescription && numbers.length === 0 ? (
            <div className="col-span-2 flex h-full items-center text-xs text-[var(--color-text-tertiary)]">
              未打标
            </div>
          ) : (
            groups.map((g) => (
              <div
                key={g.key}
                className="grid min-w-0 grid-cols-[76px_minmax(0,1fr)] items-start gap-2"
              >
                <span className="min-w-0 truncate text-[11px] text-[var(--color-text-secondary)]">
                  {g.name}
                </span>
                <div className="flex min-w-0 flex-wrap gap-1">
                  {g.items.map((t) => (
                    <TagChip key={t.id} label={t.name} onRemove={() => onRemoveTag(t.id)} />
                  ))}
                </div>
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
});
