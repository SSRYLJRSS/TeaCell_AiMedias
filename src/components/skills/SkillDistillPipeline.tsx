/**
 * 技能沉淀向导：录制 → 蒸馏 → 校验 → 审阅 → 入库。
 * 只在本地生成技能草稿并做封闭性校验；不生成任何 Operation 收据、不携带旧授权或旧批准。
 */
import { useMemo, useState } from "react";
import clsx from "clsx";
import type { ReactNode } from "react";
import Button from "@/components/common/Button";
import {
  compileSkillToWorkflowDraft,
  composeSkillFromCapture,
  STEP_CATALOG_CAPABILITIES,
  validateSkillDefinition,
} from "@/data/skills/schema";
import type { SkillDefinition } from "@/data/skills/types";
import { ApprovalBadge, CategoryBadge, KindBadge, PermissionBadges } from "./SkillBadges";

const STAGES = ["录制", "蒸馏", "校验", "审阅", "入库"] as const;

function slugify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

function splitLines(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

interface SkillDistillPipelineProps {
  onPublish?: (skill: SkillDefinition) => void;
  onBack?: () => void;
}

export default function SkillDistillPipeline({ onPublish, onBack }: SkillDistillPipelineProps): ReactNode {
  const [stage, setStage] = useState(0);
  const [capabilities, setCapabilities] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [whenToUseText, setWhenToUseText] = useState("");
  const [published, setPublished] = useState(false);

  const capabilityOptions = useMemo(() => Object.values(STEP_CATALOG_CAPABILITIES), []);

  const draft = useMemo<SkillDefinition | null>(() => {
    const trimmedName = name.trim();
    const trimmedDescription = description.trim();
    if (!trimmedName || !trimmedDescription || capabilities.length === 0) return null;
    try {
      return composeSkillFromCapture({
        id: slugify(trimmedName) || "custom-skill",
        name: trimmedName,
        description: trimmedDescription,
        whenToUse: splitLines(whenToUseText),
        capabilities,
      });
    } catch {
      return null;
    }
  }, [name, description, whenToUseText, capabilities]);

  const validation = draft ? validateSkillDefinition(draft) : null;
  const compile = draft && draft.kind === "executable" ? compileSkillToWorkflowDraft(draft) : null;

  const toggleCapability = (capability: string): void => {
    setCapabilities((prev) =>
      prev.includes(capability) ? prev.filter((item) => item !== capability) : [...prev, capability],
    );
  };

  const canGoNext = (): boolean => {
    if (stage === 0) return capabilities.length > 0;
    if (stage === 1) return draft !== null;
    if (stage === 2) return validation?.valid === true;
    if (stage === 3) return validation?.valid === true;
    return false;
  };

  const goNext = (): void => {
    if (stage === 3) setStage(4);
    else setStage((prev) => prev + 1);
  };

  const handlePublish = (): void => {
    if (!draft || !validation?.valid || published) return;
    onPublish?.(draft);
    setPublished(true);
  };

  const renderStage = (): ReactNode => {
    if (stage === 0) {
      return (
        <div className="space-y-2">
          <p className="text-[var(--color-text-secondary)]">
            勾选这次成功操作里实际跑过的步骤（只保留能力序列，不保留具体素材或目录授权）。
          </p>
          <ul className="space-y-1">
            {capabilityOptions.map((option) => (
              <li key={option.capability}>
                <label className="flex items-start gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] p-1.5">
                  <input
                    type="checkbox"
                    className="mt-0.5"
                    checked={capabilities.includes(option.capability)}
                    onChange={() => toggleCapability(option.capability)}
                    aria-label={`包含步骤 ${option.label}`}
                  />
                  <span>
                    <span className="font-medium text-[var(--color-text-primary)]">{option.label}</span>
                    <code className="ml-1 text-[11px] text-[var(--color-text-secondary)]">{option.capability}</code>
                    {option.warning ? (
                      <span className="block text-[11px] text-[var(--color-warning,#d97706)]">{option.warning}</span>
                    ) : null}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      );
    }

    if (stage === 1) {
      return (
        <div className="space-y-2">
          <label className="block space-y-1">
            <span className="font-medium text-[var(--color-text-primary)]">技能名称</span>
            <input
              className="ui-control w-full"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="例如：婚礼素材周更"
              aria-label="技能名称"
            />
          </label>
          <label className="block space-y-1">
            <span className="font-medium text-[var(--color-text-primary)]">一句话说明</span>
            <textarea
              className="ui-control w-full"
              rows={2}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="例如：把婚礼分库的新素材打标后导出到交付目录"
              aria-label="技能说明"
            />
          </label>
          <label className="block space-y-1">
            <span className="font-medium text-[var(--color-text-primary)]">适用时机（每行一条）</span>
            <textarea
              className="ui-control w-full"
              rows={2}
              value={whenToUseText}
              onChange={(event) => setWhenToUseText(event.target.value)}
              placeholder={"每週更新婚礼素材时\n需要批量交付给客户时"}
              aria-label="适用时机"
            />
          </label>
          <p className="text-[11px] text-[var(--color-text-secondary)]">
            参数会根据所选步骤自动推导，可在下一步查看。
          </p>
        </div>
      );
    }

    if (stage === 2) {
      const reasons = [
        ...(validation?.errors ?? []),
        ...(compile?.excluded.map((item) => `${item.capability} — ${item.reason}`) ?? []),
      ];
      const valid = validation?.valid === true;
      return (
        <div className="space-y-2">
          <p className={valid ? "text-[var(--color-status)]" : "text-[var(--color-danger,#dc2626)]"}>
            {valid ? "封闭性校验通过：步骤与参数都在白名单内。" : "校验未通过，请返回修正。"}
          </p>
          {reasons.length > 0 ? (
            <ul className="list-disc space-y-0.5 pl-4 text-[var(--color-danger,#dc2626)]">
              {reasons.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          ) : null}
          {validation && validation.warnings.length > 0 ? (
            <ul className="list-disc space-y-0.5 pl-4 text-[var(--color-warning,#d97706)]">
              {validation.warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
          ) : null}
          {compile?.ok && compile.draft ? (
            <p className="text-[var(--color-text-secondary)]">
              可编译为流程：{compile.draft.steps.length} 步 / {compile.draft.parameters.length} 个参数。
            </p>
          ) : null}
        </div>
      );
    }

    if (stage === 3 && draft) {
      return (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-1">
            <KindBadge kind={draft.kind} />
            <CategoryBadge category={draft.category} />
            <PermissionBadges tiers={draft.appliesTo} />
          </div>
          <p className="font-medium text-[var(--color-text-primary)]">{draft.name}</p>
          <p className="text-[var(--color-text-secondary)]">{draft.description}</p>
          <div>
            <h3 className="font-semibold text-[var(--color-text-primary)]">步骤</h3>
            <ol className="list-decimal space-y-0.5 pl-4 text-[var(--color-text-secondary)]">
              {draft.steps.map((step) => (
                <li key={step.id}>
                  {step.title}
                  {step.capability ? `（${step.capability}）` : ""}
                </li>
              ))}
            </ol>
          </div>
          <div>
            <h3 className="font-semibold text-[var(--color-text-primary)]">参数</h3>
            <ul className="list-disc space-y-0.5 pl-4 text-[var(--color-text-secondary)]">
              {draft.params.map((param) => (
                <li key={param.key}>
                  {param.label}
                  <code className="ml-1 text-[11px]">{param.key}</code>
                </li>
              ))}
            </ul>
          </div>
          <div className="flex flex-wrap gap-1">
            {draft.approvalPoints.map((point) => (
              <ApprovalBadge key={point} point={point} />
            ))}
          </div>
        </div>
      );
    }

    return (
      <div className="space-y-2">
        {published ? (
          <p className="text-[var(--color-status)]">已加入本地技能库草稿（仅本会话可见，待审阅后再正式入库）。</p>
        ) : (
          <>
            <p className="text-[var(--color-text-secondary)]">
              确认后加入本地技能库草稿。真正入库前仍建议人工审阅正文与参数模板。
            </p>
            <Button variant="primary" onClick={handlePublish}>
              加入本地技能库
            </Button>
          </>
        )}
        {onBack ? (
          <Button variant="ghost" onClick={onBack}>
            返回技能库
          </Button>
        ) : null}
      </div>
    );
  };

  return (
    <section className="flex h-full flex-col bg-[var(--color-surface)] text-sm" aria-label="技能沉淀向导">
      <div className="flex items-center justify-between gap-2 border-b border-[var(--color-border)] p-2.5">
        <h2 className="font-semibold text-[var(--color-text-primary)]">沉淀技能</h2>
        {onBack ? (
          <Button variant="ghost" onClick={onBack} aria-label="关闭沉淀向导">
            关闭
          </Button>
        ) : null}
      </div>

      <ol className="flex flex-wrap gap-1 border-b border-[var(--color-border)] p-2.5" aria-label="沉淀阶段">
        {STAGES.map((title, index) => (
          <li
            key={title}
            className={clsx(
              "rounded-full border px-2 py-0.5 text-[11px]",
              index === stage
                ? "border-[var(--color-status)] text-[var(--color-status)]"
                : "border-[var(--color-border)] text-[var(--color-text-secondary)]",
            )}
          >
            {index + 1}. {title}
          </li>
        ))}
      </ol>

      <div className="flex-1 overflow-y-auto p-2.5 text-xs">{renderStage()}</div>

      {stage < 4 ? (
        <div className="flex items-center justify-between gap-2 border-t border-[var(--color-border)] p-2.5">
          <Button variant="ghost" onClick={() => setStage((prev) => Math.max(0, prev - 1))} disabled={stage === 0}>
            上一步
          </Button>
          <Button variant="primary" onClick={goNext} disabled={!canGoNext()}>
            {stage === 3 ? "去入库" : "下一步"}
          </Button>
        </div>
      ) : null}
    </section>
  );
}
