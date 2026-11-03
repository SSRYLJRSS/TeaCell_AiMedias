/** 技能相关的小徽章与中文标签。 */
import clsx from "clsx";
import type { ReactNode } from "react";
import type {
  ApprovalPoint,
  PermissionTier,
  SkillCategory,
  SkillKind,
} from "@/data/skills/types";

export const CATEGORY_LABELS: Record<SkillCategory, string> = {
  ingest: "入库",
  search: "搜索",
  curation: "整理",
  tagging: "打标",
  export: "导出",
  cleanup: "清理",
  organize: "移动",
  workflow: "流程",
  meta: "元信息",
};

export const KIND_LABELS: Record<SkillKind, string> = {
  knowledge: "知识型",
  executable: "可执行",
};

export const PERMISSION_LABELS: Record<PermissionTier, string> = {
  Read: "只读",
  Standard: "标准",
  Full: "完全",
};

export const APPROVAL_LABELS: Record<ApprovalPoint, string> = {
  plan: "计划批准",
  permanentTrash: "永久删除确认",
  resumeReview: "恢复审阅",
  trustedReview: "可信审核",
  guiManual: "界面手动",
};

type BadgeTone = "neutral" | "accent" | "warn" | "muted";

const TONE_CLASS: Record<BadgeTone, string> = {
  neutral: "border-[var(--color-border)] text-[var(--color-text-secondary)]",
  accent: "border-[var(--color-status)] text-[var(--color-status)]",
  warn: "border-[var(--color-warning,#d97706)] text-[var(--color-warning,#d97706)]",
  muted: "border-transparent bg-[var(--color-surface-muted,rgba(0,0,0,0.04))] text-[var(--color-text-secondary)]",
};

export function Badge({
  children,
  tone = "neutral",
  className,
}: {
  children: ReactNode;
  tone?: BadgeTone;
  className?: string;
}): ReactNode {
  return (
    <span
      className={clsx(
        "inline-flex items-center rounded-full border px-1.5 py-0.5 text-[11px] leading-none",
        TONE_CLASS[tone],
        className,
      )}
    >
      {children}
    </span>
  );
}

export function CategoryBadge({ category }: { category: SkillCategory }): ReactNode {
  return <Badge tone="muted">{CATEGORY_LABELS[category]}</Badge>;
}

export function KindBadge({ kind }: { kind: SkillKind }): ReactNode {
  return <Badge tone={kind === "executable" ? "accent" : "neutral"}>{KIND_LABELS[kind]}</Badge>;
}

export function PermissionBadges({ tiers }: { tiers: PermissionTier[] }): ReactNode {
  return (
    <span className="inline-flex flex-wrap gap-1">
      {tiers.map((tier) => (
        <Badge key={tier} tone="neutral">
          {PERMISSION_LABELS[tier]}
        </Badge>
      ))}
    </span>
  );
}

export function ApprovalBadge({ point }: { point: ApprovalPoint }): ReactNode {
  return <Badge tone={point === "permanentTrash" ? "warn" : "neutral"}>{APPROVAL_LABELS[point]}</Badge>;
}
