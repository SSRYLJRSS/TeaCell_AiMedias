/** 技能详情：正文 / 参数 / 步骤 / 审批点 / 示例 + 校验与编译状态。 */
import type { ReactNode } from "react";
import Button from "@/components/common/Button";
import {
  compileSkillToWorkflowDraft,
  STEP_CATALOG_CAPABILITIES,
  validateSkillDefinition,
} from "@/data/skills/schema";
import type { SkillDefinition } from "@/data/skills/types";
import {
  ApprovalBadge,
  CategoryBadge,
  KindBadge,
  PermissionBadges,
} from "./SkillBadges";

interface SkillDetailProps {
  skill: SkillDefinition;
  onUse?: (skill: SkillDefinition) => void;
  onBack?: () => void;
}

function capabilityLabel(capability: string | undefined): string {
  if (!capability) return "（无能力）";
  return STEP_CATALOG_CAPABILITIES[capability]?.label ?? capability;
}

export default function SkillDetail({ skill, onUse, onBack }: SkillDetailProps): ReactNode {
  const validation = validateSkillDefinition(skill);
  const compile = skill.kind === "executable" ? compileSkillToWorkflowDraft(skill) : null;

  let compileLine: string | null = null;
  if (skill.kind === "executable") {
    if (skill.executableVia === "gui") {
      compileLine = "界面引导型：按正文在界面操作，不生成流程草稿。";
    } else if (compile) {
      if (compile.ok && compile.draft) {
        compileLine = `可编译为流程：${compile.draft.steps.length} 步 / ${compile.draft.parameters.length} 个参数`;
      } else {
        const reasons = [
          ...compile.errors,
          ...compile.excluded.map((item) => `${item.capability} — ${item.reason}`),
        ];
        compileLine = `暂不可编译：${reasons.join("；")}`;
      }
    }
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between gap-2 border-b border-[var(--color-border)] p-2.5">
        <div className="flex min-w-0 items-center gap-1.5">
          {onBack ? (
            <Button variant="ghost" onClick={onBack} aria-label="返回技能列表">
              返回
            </Button>
          ) : null}
          <span className="truncate font-semibold text-[var(--color-text-primary)]">{skill.name}</span>
        </div>
        {onUse ? (
          <Button variant="primary" onClick={() => onUse(skill)}>
            在对话中使用
          </Button>
        ) : null}
      </div>

      <div className="flex-1 space-y-3 overflow-y-auto p-2.5 text-xs">
        <div className="flex flex-wrap items-center gap-1">
          <KindBadge kind={skill.kind} />
          <CategoryBadge category={skill.category} />
          <PermissionBadges tiers={skill.appliesTo} />
          <span className="text-[var(--color-text-secondary)]">v{skill.version}</span>
        </div>

        <p className="text-[var(--color-text-primary)]">{skill.description}</p>

        <section>
          <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">适用时机</h3>
          <ul className="list-disc space-y-0.5 pl-4 text-[var(--color-text-secondary)]">
            {skill.whenToUse.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </section>

        <section>
          <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">正文</h3>
          <pre className="whitespace-pre-wrap rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-sans text-[var(--color-text-secondary)]">
            {skill.body}
          </pre>
        </section>

        {skill.params.length > 0 ? (
          <section>
            <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">参数</h3>
            <ul className="space-y-1">
              {skill.params.map((param) => (
                <li key={param.key} className="rounded-[var(--radius-control)] border border-[var(--color-border)] p-1.5">
                  <div className="flex items-center gap-1.5">
                    <span className="font-medium text-[var(--color-text-primary)]">{param.label}</span>
                    <code className="text-[11px] text-[var(--color-text-secondary)]">{param.key}</code>
                    <span className="text-[11px] text-[var(--color-text-secondary)]">
                      {param.required ? "必填" : "可选"}
                    </span>
                  </div>
                  <p className="mt-0.5 text-[var(--color-text-secondary)]">{param.description}</p>
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        {skill.steps.length > 0 ? (
          <section>
            <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">步骤</h3>
            <ol className="list-decimal space-y-0.5 pl-4 text-[var(--color-text-secondary)]">
              {skill.steps.map((step) => (
                <li key={step.id}>
                  {step.title}
                  {step.capability ? `（${capabilityLabel(step.capability)}）` : ""}
                </li>
              ))}
            </ol>
          </section>
        ) : null}

        {skill.approvalPoints.length > 0 ? (
          <section>
            <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">审批点</h3>
            <div className="flex flex-wrap gap-1">
              {skill.approvalPoints.map((point) => (
                <ApprovalBadge key={point} point={point} />
              ))}
            </div>
          </section>
        ) : null}

        {skill.examples.length > 0 ? (
          <section>
            <h3 className="mb-1 font-semibold text-[var(--color-text-primary)]">示例</h3>
            <ul className="space-y-1">
              {skill.examples.map((example) => (
                <li key={example.say} className="rounded-[var(--radius-control)] border border-[var(--color-border)] p-1.5">
                  <p className="text-[var(--color-text-primary)]">「{example.say}」</p>
                  <p className="text-[var(--color-text-secondary)]">{example.result}</p>
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        <section className="space-y-1">
          <h3 className="font-semibold text-[var(--color-text-primary)]">校验</h3>
          <p className={validation.valid ? "text-[var(--color-status)]" : "text-[var(--color-danger,#dc2626)]"}>
            {validation.valid ? "校验通过" : validation.errors.join("；")}
          </p>
          {validation.warnings.length > 0 ? (
            <p className="text-[var(--color-warning,#d97706)]">{validation.warnings.join("；")}</p>
          ) : null}
          {compileLine ? <p className="text-[var(--color-text-secondary)]">{compileLine}</p> : null}
        </section>
      </div>
    </div>
  );
}
