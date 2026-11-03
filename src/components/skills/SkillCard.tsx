/** 技能库列表中的单条技能卡片。 */
import clsx from "clsx";
import type { ReactNode } from "react";
import type { SkillSummary } from "@/data/skills/types";
import { CategoryBadge, KindBadge, PermissionBadges } from "./SkillBadges";

interface SkillCardProps {
  skill: SkillSummary;
  selected?: boolean;
  disabled?: boolean;
  onSelect: () => void;
  onToggleDisabled?: () => void;
}

export default function SkillCard({
  skill,
  selected = false,
  disabled = false,
  onSelect,
  onToggleDisabled,
}: SkillCardProps): ReactNode {
  return (
    <li
      className={clsx(
        "flex items-start gap-1.5 rounded-[var(--radius-control)] border p-2 transition-colors",
        selected
          ? "border-[var(--color-status)] bg-[var(--color-surface)]"
          : "border-[var(--color-border)] hover:border-[var(--color-status)]",
        disabled && "opacity-60",
      )}
    >
      <button
        type="button"
        className="min-w-0 flex-1 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-status)]"
        onClick={onSelect}
        aria-pressed={selected}
      >
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="truncate text-sm font-medium text-[var(--color-text-primary)]">{skill.name}</span>
          <KindBadge kind={skill.kind} />
        </div>
        <p className="mt-0.5 line-clamp-2 text-xs text-[var(--color-text-secondary)]">{skill.description}</p>
        <div className="mt-1 flex flex-wrap items-center gap-1">
          <CategoryBadge category={skill.category} />
          <PermissionBadges tiers={skill.appliesTo} />
          {disabled ? <span className="text-[11px] text-[var(--color-text-secondary)]">已停用</span> : null}
        </div>
      </button>
      {onToggleDisabled ? (
        <button
          type="button"
          className="shrink-0 self-start rounded-[var(--radius-control)] px-1.5 py-1 text-[11px] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-status)]"
          onClick={onToggleDisabled}
          aria-label={disabled ? `启用技能 ${skill.name}` : `停用技能 ${skill.name}`}
        >
          {disabled ? "启用" : "停用"}
        </button>
      ) : null}
    </li>
  );
}
