/** 常用流程的编辑表单（A4-07）。
 *
 * 这里只编辑「允许的步骤 + 参数定义」。表单里没有、也不允许出现素材 ID、
 * 真实路径、批准引用或凭据：后端 `WorkflowDraftInput` 是 `deny_unknown_fields`，
 * 多带一个 `script` 或 `apiKey` 字段会被直接拒绝，前端也不提供这类输入框。
 *
 * 步骤只能从 `workflow_step_catalog` 选的封闭目录里挑，保存路径会用同一目录
 * 二次校验，所以界面即使拿到过期的选项也存不进去。
 */
import { useMemo, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import type {
  WorkflowDraft,
  WorkflowDraftInput,
  WorkflowStepCatalogEntry,
} from "@/api/workflows";

/** 编辑期用的参数行：比保存载荷多一个中文名（只用于展示，不落库）。 */
export type WorkflowParameterDraft = WorkflowDraftInput["parameters"][number] & { label: string };

/** 从目录里派生「当前步骤组合真正会消费的参数」。 */
export function parametersForSteps(
  catalog: WorkflowStepCatalogEntry[],
  capabilities: string[],
): WorkflowParameterDraft[] {
  const seen = new Map<string, WorkflowParameterDraft>();
  for (const capability of capabilities) {
    const entry = catalog.find((item) => item.capability === capability);
    if (!entry) continue;
    for (const parameter of entry.parameters) {
      const existing = seen.get(parameter.key);
      // 任一使用它的步骤要求必填，就按必填展示：宁可多问一次，也不要静默留空。
      seen.set(parameter.key, {
        key: parameter.key,
        label: existing?.label || parameter.label,
        required: (existing?.required ?? false) || parameter.required,
        description: existing?.description || parameter.description,
      });
    }
  }
  return [...seen.values()];
}

export interface WorkflowDraftFormProps {
  catalog: WorkflowStepCatalogEntry[];
  /** 初始草稿：来自一次操作，或来自某个已有版本。 */
  initial: WorkflowDraftInput & { excluded?: WorkflowDraft["excluded"]; notes?: string[] };
  submitLabel: string;
  busy: boolean;
  error: string | null;
  onSubmit: (draft: WorkflowDraftInput) => void | Promise<void>;
  onCancel: () => void;
}

export default function WorkflowDraftForm({
  catalog,
  initial,
  submitLabel,
  busy,
  error,
  onSubmit,
  onCancel,
}: WorkflowDraftFormProps) {
  const [name, setName] = useState(initial.name);
  const [description, setDescription] = useState(initial.description);
  const [steps, setSteps] = useState(initial.steps.map((step) => ({ ...step })));
  const [drafts, setDrafts] = useState<Record<string, { required: boolean; description: string }>>(
    () => {
      const next: Record<string, { required: boolean; description: string }> = {};
      for (const parameter of initial.parameters) {
        next[parameter.key] = { required: parameter.required, description: parameter.description };
      }
      return next;
    },
  );

  const capabilities = steps.map((step) => step.capability);
  const available = useMemo(() => parametersForSteps(catalog, capabilities), [catalog, capabilities]);
  const excluded = initial.excluded ?? [];

  const addStep = () => {
    const used = new Set(capabilities);
    const next = catalog.find((entry) => !used.has(entry.capability));
    if (!next) return;
    setSteps((current) => [...current, { capability: next.capability, dependsOn: null }]);
  };

  const move = (index: number, delta: number) => {
    setSteps((current) => {
      const target = index + delta;
      if (target < 0 || target >= current.length) return current;
      const next = [...current];
      const [item] = next.splice(index, 1);
      next.splice(target, 0, item);
      // 位置变了，向后依赖的序号也得跟着改，否则保存时会被判成「依赖指向后面」。
      const remap = (value: number | null | undefined, from: number, to: number) => {
        if (value == null) return value;
        if (value === from) return to;
        return value;
      };
      return next.map((step, position) => ({
        ...step,
        dependsOn: remap(remap(step.dependsOn, index, target), position, position),
      }));
    });
  };

  const update = (index: number, patch: Partial<{ capability: string; dependsOn: number | null }>) => {
    setSteps((current) =>
      current.map((step, position) => (position === index ? { ...step, ...patch } : step)),
    );
  };

  const remove = (index: number) => {
    setSteps((current) => current.filter((_, position) => position !== index));
  };

  const submit = () => {
    void onSubmit({
      name: name.trim(),
      description: description.trim(),
      steps: steps.map((step) => ({
        capability: step.capability,
        dependsOn: step.dependsOn ?? null,
      })),
      parameters: available.map((parameter) => ({
        key: parameter.key,
        required: drafts[parameter.key]?.required ?? parameter.required,
        description: (drafts[parameter.key]?.description ?? parameter.description).trim(),
      })),
    });
  };

  return (
    <div className="flex flex-col gap-3">
      {excluded.length > 0 && (
        <div className="rounded-[var(--radius-control)] border border-[var(--color-status)] px-3 py-2">
          <p className="text-xs text-[var(--color-status)]">
            以下步骤无法保存为常用流程，已排除而不是改写成脚本：
          </p>
          <ul className="mt-1 list-inside list-disc text-[11px] text-[var(--color-text-secondary)]">
            {excluded.map((entry) => (
              <li key={entry.capability}>
                {entry.capability}：{entry.reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      {(initial.notes ?? []).length > 0 && (
        <ul className="list-inside list-disc text-[11px] text-[var(--color-text-secondary)]">
          {(initial.notes ?? []).map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      )}

      <label className="flex flex-col gap-1">
        <span className="text-xs text-[var(--color-text-secondary)]">流程名称</span>
        <input
          aria-label="流程名称"
          className="ui-control w-full px-2 py-1.5 text-sm"
          value={name}
          maxLength={120}
          onChange={(event) => setName(event.currentTarget.value)}
        />
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-xs text-[var(--color-text-secondary)]">说明（可选）</span>
        <textarea
          aria-label="流程说明"
          className="ui-control min-h-[56px] w-full px-2 py-1.5 text-sm"
          value={description}
          maxLength={4000}
          onChange={(event) => setDescription(event.currentTarget.value)}
        />
      </label>

      <section>
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">
            步骤（最多 {8} 个）
          </h3>
          <Button
            className="min-h-0 px-2 py-0.5 text-[11px]"
            disabled={busy || steps.length >= 8}
            onClick={addStep}
          >
            添加步骤
          </Button>
        </div>
        {steps.length === 0 ? (
          <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">
            还没有步骤。常用流程至少要有一个受控动作才能保存。
          </p>
        ) : (
          <ol className="mt-1 divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
            {steps.map((step, index) => {
              const entry = catalog.find((item) => item.capability === step.capability);
              return (
                <li key={`${step.capability}-${index}`} className="flex flex-wrap items-center gap-2 px-3 py-2">
                  <span className="text-[11px] text-[var(--color-text-tertiary)]">{index + 1}.</span>
                  <select
                    aria-label={`第 ${index + 1} 步动作`}
                    className="ui-control px-1.5 py-0.5 text-[11px]"
                    value={step.capability}
                    disabled={busy}
                    onChange={(event) => update(index, { capability: event.currentTarget.value })}
                  >
                    {catalog.map((option) => (
                      <option key={option.capability} value={option.capability}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                  {index > 0 && (
                    <label className="flex items-center gap-1 text-[11px] text-[var(--color-text-secondary)]">
                      依赖
                      <select
                        aria-label={`第 ${index + 1} 步依赖`}
                        className="ui-control px-1.5 py-0.5 text-[11px]"
                        value={step.dependsOn ?? ""}
                        disabled={busy}
                        onChange={(event) =>
                          update(index, {
                            dependsOn:
                              event.currentTarget.value === ""
                                ? null
                                : Number(event.currentTarget.value),
                          })
                        }
                      >
                        <option value="">不依赖</option>
                        {steps.slice(0, index).map((previous, position) => (
                          <option key={`${previous.capability}-${position}`} value={position}>
                            第 {position + 1} 步
                          </option>
                        ))}
                      </select>
                    </label>
                  )}
                  <span className="ml-auto flex items-center gap-1">
                    <Button
                      className="min-h-0 px-2 py-0.5 text-[11px]"
                      disabled={busy || index === 0}
                      onClick={() => move(index, -1)}
                    >
                      上移
                    </Button>
                    <Button
                      className="min-h-0 px-2 py-0.5 text-[11px]"
                      disabled={busy || index === steps.length - 1}
                      onClick={() => move(index, 1)}
                    >
                      下移
                    </Button>
                    <Button
                      variant="danger"
                      className="min-h-0 px-2 py-0.5 text-[11px]"
                      disabled={busy}
                      onClick={() => remove(index)}
                    >
                      移除
                    </Button>
                  </span>
                  {entry?.warning && (
                    <p className="w-full text-[11px] text-[var(--color-status)]">{entry.warning}</p>
                  )}
                </li>
              );
            })}
          </ol>
        )}
      </section>

      <section>
        <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">
          参数定义（模板只保存「要填什么」，不保存本次的具体值）
        </h3>
        {available.length === 0 ? (
          <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">
            当前步骤不需要额外参数；复用时仍会在新的审批里重新解析素材与目录。
          </p>
        ) : (
          <ul className="mt-1 divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
            {available.map((parameter) => (
              <li key={parameter.key} className="flex flex-col gap-1 px-3 py-2">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm text-[var(--color-text)]">{parameter.label}</span>
                  <label className="flex items-center gap-1 text-[11px] text-[var(--color-text-secondary)]">
                    <input
                      type="checkbox"
                      aria-label={`${parameter.label} 必填`}
                      checked={drafts[parameter.key]?.required ?? parameter.required}
                      disabled={busy}
                      onChange={(event) =>
                        setDrafts((current) => ({
                          ...current,
                          [parameter.key]: {
                            required: event.currentTarget.checked,
                            description:
                              current[parameter.key]?.description ?? parameter.description,
                          },
                        }))
                      }
                    />
                    必填
                  </label>
                </div>
                <input
                  aria-label={`${parameter.label} 说明`}
                  className="ui-control w-full px-2 py-1 text-[11px]"
                  value={drafts[parameter.key]?.description ?? parameter.description}
                  maxLength={256}
                  disabled={busy}
                  onChange={(event) =>
                    setDrafts((current) => ({
                      ...current,
                      [parameter.key]: {
                        required: current[parameter.key]?.required ?? parameter.required,
                        description: event.currentTarget.value,
                      },
                    }))
                  }
                />
              </li>
            ))}
          </ul>
        )}
      </section>

      {error && (
        <p role="alert" className="text-xs text-[var(--color-danger)]">
          {error}
        </p>
      )}

      <div className="flex items-center justify-end gap-2">
        <Button disabled={busy} onClick={onCancel}>
          取消
        </Button>
        <Button
          variant="primary"
          className={clsx(busy && "opacity-60")}
          disabled={busy || name.trim().length === 0 || steps.length === 0}
          onClick={submit}
        >
          {busy ? "正在保存…" : submitLabel}
        </Button>
      </div>
    </div>
  );
}
