/**
 * 技能库面板：浏览 / 搜索 / 分类·权限·类型筛选 / 启用停用 / 查看详情。
 * 自包含（不依赖 store），可独立渲染与单测，后续再接入助手侧栏与 @提及。
 */
import { useMemo, useState } from "react";
import clsx from "clsx";
import type { ReactNode } from "react";
import Button from "@/components/common/Button";
import { BUILTIN_SKILLS } from "@/data/skills/catalog";
import { toSummary } from "@/data/skills/schema";
import type {
  PermissionTier,
  SkillCategory,
  SkillDefinition,
  SkillKind,
  SkillSummary,
} from "@/data/skills/types";
import SkillCard from "./SkillCard";
import SkillDetail from "./SkillDetail";
import { CATEGORY_LABELS, PERMISSION_LABELS } from "./SkillBadges";

type CategoryFilter = "all" | SkillCategory;
type TierFilter = "all" | PermissionTier;
type KindFilter = "all" | SkillKind;

interface SkillLibraryPanelProps {
  skills?: SkillDefinition[];
  embedded?: boolean;
  onUse?: (skill: SkillDefinition) => void;
  onClose?: () => void;
  onOpenDistill?: () => void;
}

export default function SkillLibraryPanel({
  skills = BUILTIN_SKILLS,
  embedded = false,
  onUse,
  onClose,
  onOpenDistill,
}: SkillLibraryPanelProps): ReactNode {
  const [keyword, setKeyword] = useState("");
  const [category, setCategory] = useState<CategoryFilter>("all");
  const [tier, setTier] = useState<TierFilter>("all");
  const [kind, setKind] = useState<KindFilter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [disabledIds, setDisabledIds] = useState<Set<string>>(() => new Set());

  const summaries = useMemo<SkillSummary[]>(() => skills.map(toSummary), [skills]);
  const categories = useMemo<SkillCategory[]>(
    () => Array.from(new Set(summaries.map((item) => item.category))),
    [summaries],
  );

  const filtered = useMemo<SkillSummary[]>(() => {
    const kw = keyword.trim().toLowerCase();
    return summaries.filter((item) => {
      if (category !== "all" && item.category !== category) return false;
      if (kind !== "all" && item.kind !== kind) return false;
      if (tier !== "all" && !item.appliesTo.includes(tier)) return false;
      if (!kw) return true;
      const haystack = [item.name, item.description, ...item.whenToUse, item.id].join(" ").toLowerCase();
      return haystack.includes(kw);
    });
  }, [summaries, keyword, category, tier, kind]);

  const selected = selectedId ? skills.find((item) => item.id === selectedId) ?? null : null;

  const toggleDisabled = (id: string): void => {
    setDisabledIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <section
      className={clsx(
        "flex h-full flex-col bg-[var(--color-surface)] text-sm",
        !embedded && "rounded-[var(--radius-control)] border border-[var(--color-border)]",
      )}
      aria-label="技能库"
    >
      <div className="flex items-center justify-between gap-2 border-b border-[var(--color-border)] p-2.5">
        <div className="flex items-center gap-2">
          <h2 className="font-semibold text-[var(--color-text-primary)]">技能库</h2>
          <span className="text-xs text-[var(--color-text-secondary)]">
            {filtered.length}/{summaries.length}
          </span>
        </div>
        <div className="flex items-center gap-1">
          {onOpenDistill ? (
            <Button variant="ghost" onClick={onOpenDistill}>
              沉淀技能
            </Button>
          ) : null}
          {onClose ? (
            <Button variant="ghost" onClick={onClose} aria-label="关闭技能库">
              关闭
            </Button>
          ) : null}
        </div>
      </div>

      {selected ? (
        <SkillDetail skill={selected} onUse={onUse} onBack={() => setSelectedId(null)} />
      ) : (
        <>
          <div className="space-y-1.5 border-b border-[var(--color-border)] p-2.5">
            <input
              className="ui-control w-full"
              placeholder="搜索技能名称、说明、适用时机"
              value={keyword}
              onChange={(event) => setKeyword(event.target.value)}
              aria-label="搜索技能"
            />
            <div className="flex flex-wrap gap-1.5">
              <select
                className="ui-control"
                value={category}
                onChange={(event) => setCategory(event.target.value as CategoryFilter)}
                aria-label="按分类筛选"
              >
                <option value="all">全部分类</option>
                {categories.map((item) => (
                  <option key={item} value={item}>
                    {CATEGORY_LABELS[item]}
                  </option>
                ))}
              </select>
              <select
                className="ui-control"
                value={tier}
                onChange={(event) => setTier(event.target.value as TierFilter)}
                aria-label="按权限筛选"
              >
                <option value="all">全部权限</option>
                {(Object.keys(PERMISSION_LABELS) as PermissionTier[]).map((item) => (
                  <option key={item} value={item}>
                    {PERMISSION_LABELS[item]}
                  </option>
                ))}
              </select>
              <select
                className="ui-control"
                value={kind}
                onChange={(event) => setKind(event.target.value as KindFilter)}
                aria-label="按类型筛选"
              >
                <option value="all">全部类型</option>
                <option value="knowledge">知识型</option>
                <option value="executable">可执行</option>
              </select>
            </div>
          </div>

          <div className="flex-1 overflow-y-auto p-2">
            {filtered.length === 0 ? (
              <p className="p-4 text-center text-xs text-[var(--color-text-secondary)]">没有匹配的技能</p>
            ) : (
              <ul className="space-y-1.5" aria-label="技能列表">
                {filtered.map((item) => (
                  <SkillCard
                    key={item.id}
                    skill={item}
                    selected={item.id === selectedId}
                    disabled={disabledIds.has(item.id)}
                    onSelect={() => setSelectedId(item.id)}
                    onToggleDisabled={() => toggleDisabled(item.id)}
                  />
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </section>
  );
}
