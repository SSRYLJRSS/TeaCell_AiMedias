/** 常用流程的管理面板（A4-07）。
 *
 * 三件事必须在这个界面里说清楚，而不是靠帮助文档：
 * 1. 保存的是「允许的步骤 + 参数定义」——不是这次的素材、目录授权、批准或密钥；
 * 2. 保存前告知会申请哪些能力，以及复用时仍然必须现场选择的输入；
 * 3. 无法合法表达的历史步骤给出原因（由后端 `excluded` 提供），绝不偷偷生成脚本。
 *
 * 「再次使用」只把「这次还要填什么」的清单交回输入框，真正执行仍要重新准备计划
 * 并经用户确认（实例化与统一审批属 A4-08，这里不假装已经能一键重跑）。
 * 「用本次输入开始」是 A4-08 的入口：它打开实例化弹窗，让用户把本次输入填进去再
 * 准备新计划——所以它同样不是「重跑上一次」，两个按钮都不携带旧的素材或授权。
 */
import { useCallback, useEffect, useState, type ReactNode } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import WorkflowDraftForm from "@/components/workflows/WorkflowDraftForm";
import WorkflowRunDialog from "@/components/workflows/WorkflowRunDialog";
import {
  createWorkflow,
  listWorkflows,
  renameWorkflow,
  setWorkflowArchived,
  updateWorkflow,
  versionLabel,
  workflowStepCatalog,
  workflowSummaryLine,
  type WorkflowDraft,
  type WorkflowDraftInput,
  type WorkflowStepCatalogEntry,
  type WorkflowView,
} from "@/api/workflows";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useConversationStore } from "@/stores/conversationStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";

type Mode = "browse" | "create" | "edit";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 已有版本 → 编辑表单的初始值。只带定义，不带任何执行期数据。 */
export function draftInputFromView(view: WorkflowView): WorkflowDraftInput {
  return {
    name: view.name,
    description: view.description,
    steps: view.steps.map((step) => ({
      capability: step.capability,
      dependsOn: step.dependsOn ?? null,
    })),
    parameters: view.parameters.map((parameter) => ({
      key: parameter.key,
      required: parameter.required,
      description: parameter.description,
    })),
  };
}

/** 「再次使用」交回输入框的文本：只描述还要选什么，不携带旧素材/旧授权。 */
export function reusePrompt(view: WorkflowView): string {
  const steps = view.requires.length > 0 ? view.requires.join(" → ") : "（没有步骤）";
  const inputs =
    view.runtimeInputs.length > 0
      ? view.runtimeInputs
          .map((input) => `- ${input.label}${input.required ? "（必填）" : ""}：${input.hint}`)
          .join("\n")
      : "- 无额外输入";
  return [
    `请按常用流程「${view.name}」（${versionLabel(view)}）准备一次新的计划。`,
    `步骤：${steps}`,
    "这次还要我提供的输入：",
    inputs,
    "请只准备计划给我确认，不要直接执行；素材、目录和授权都要按本次重新选择。",
  ].join("\n");
}

interface WorkflowPanelProps {
  /** 作为助手右侧栏的一个选项卡渲染：不渲染 Modal，标题栏保留，关闭动作交给父级外壳。 */
  embedded?: boolean;
  /** 关闭动作。默认收起 store 里的面板；嵌进助手侧栏时由外壳传入，
   *  否则「再次使用」之后面板关闭了、侧栏还留在原地。 */
  onClose?: () => void;
}

/** 面板外壳：默认仍是模态框（行为与改动前一致）；`embedded` 时换成侧栏里的普通容器。
 *
 *  定义在模块作用域而不是渲染函数里，避免每次渲染生成新组件类型导致内容整体重挂载
 *  （输入框焦点、滚动位置都会丢）。
 */
function PanelFrame({
  embedded,
  onClose,
  children,
}: {
  embedded: boolean;
  onClose: () => void;
  children: ReactNode;
}) {
  if (!embedded) {
    return (
      <Modal open title="常用流程" wide onClose={onClose}>
        {children}
      </Modal>
    );
  }
  // 嵌入时不再自带标题栏：外壳的选项卡已经写着「常用流程」，重复一次只是白占一行。
  return <div className="flex min-h-0 flex-1 flex-col p-3">{children}</div>;
}

export default function WorkflowPanel({ embedded = false, onClose }: WorkflowPanelProps) {
  const closeStorePanel = useWorkflowPanelStore((s) => s.closePanel);
  /** 关闭面板：嵌入侧栏时关的是外壳（父级传入），模态时关的是自己。 */
  const close = onClose ?? closeStorePanel;
  const initialDraft = useWorkflowPanelStore((s) => s.draft);
  const initialError = useWorkflowPanelStore((s) => s.error);
  const clearDraft = useWorkflowPanelStore((s) => s.clearDraft);
  const activeConversation = useConversationStore((s) => s.activeRef);
  const inject = useComposerInjectStore((s) => s.inject);

  const [views, setViews] = useState<WorkflowView[] | null>(null);
  const [catalog, setCatalog] = useState<WorkflowStepCatalogEntry[]>([]);
  const [query, setQuery] = useState("");
  const [includeArchived, setIncludeArchived] = useState(false);
  const [selected, setSelected] = useState<WorkflowView | null>(null);
  const [mode, setMode] = useState<Mode>(initialDraft ? "create" : "browse");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(initialError);
  const [notice, setNotice] = useState<string | null>(
    initialDraft && initialError === null ? "已根据这次任务的业务步骤生成草稿，确认后再保存。" : null,
  );
  const [renameValue, setRenameValue] = useState<string | null>(null);
  const [confirmArchive, setConfirmArchive] = useState<WorkflowView | null>(null);
  /** A4-08：要实例化的流程。null 表示实例化弹窗没开。 */
  const [runTarget, setRunTarget] = useState<WorkflowView | null>(null);

  const refresh = useCallback(
    async (nextQuery: string, nextIncludeArchived: boolean) => {
      const list = await listWorkflows(nextIncludeArchived, nextQuery.trim() || undefined);
      setViews(list);
      setSelected((current) =>
        current ? (list.find((item) => item.workflowRef === current.workflowRef) ?? null) : null,
      );
    },
    [],
  );

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const [list, steps] = await Promise.all([listWorkflows(false), workflowStepCatalog()]);
        if (!alive) return;
        setViews(list);
        setCatalog(Array.isArray(steps) ? steps : []);
      } catch (cause) {
        if (!alive) return;
        setError(`读取常用流程失败：${messageOf(cause)}`);
        setViews([]);
      }
    };
    void load();
    return () => {
      alive = false;
    };
  }, []);

  const reload = async (nextQuery = query, nextArchived = includeArchived) => {
    try {
      await refresh(nextQuery, nextArchived);
    } catch (cause) {
      setError(`刷新列表失败：${messageOf(cause)}`);
    }
  };

  const save = async (input: WorkflowDraftInput) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved =
        mode === "edit" && selected
          ? await updateWorkflow(selected.workflowRef, selected.version, input)
          : await createWorkflow(input);
      setSelected(saved);
      setMode("browse");
      clearDraft();
      setNotice(
        mode === "edit"
          ? `已保存为${versionLabel(saved)}；旧版本快照与其在途任务不受影响。`
          : `已保存「${saved.name}」（${versionLabel(saved)}）。`,
      );
      await reload();
    } catch (cause) {
      setError(`保存失败：${messageOf(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const rename = async (view: WorkflowView) => {
    const next = (renameValue ?? "").trim();
    if (!next) return;
    setBusy(true);
    setError(null);
    try {
      const updated = await renameWorkflow(view.workflowRef, next);
      setSelected(updated);
      setRenameValue(null);
      setNotice("已改名。改名只调整列表显示，不会产生新版本，也不改动在途任务用的快照。");
      await reload();
    } catch (cause) {
      setError(`改名失败：${messageOf(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const archive = async (view: WorkflowView, archived: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const updated = await setWorkflowArchived(view.workflowRef, archived);
      setSelected(updated);
      setConfirmArchive(null);
      setNotice(
        archived
          ? "已停用：列表默认不再显示，历史版本与其在途任务仍然可读。"
          : "已恢复可用。",
      );
      await reload();
    } catch (cause) {
      setError(`操作失败：${messageOf(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const reuse = (view: WorkflowView) => {
    if (!activeConversation) {
      setError("先在左侧选择一个对话，再把这个流程用起来。");
      return;
    }
    inject(activeConversation, reusePrompt(view));
    setNotice("已把「这次还要填什么」放回输入框；执行前仍会重新准备计划并经你确认。");
    close();
  };

  /** A4-08：不注入文本，直接收集本次输入并准备一份新计划。 */
  const startWithInputs = (view: WorkflowView) => {
    if (view.archived) {
      setError("这个流程已停用，不能再开始新的实例；历史版本仍然可读。");
      return;
    }
    setError(null);
    setNotice(null);
    setRunTarget(view);
  };

  const formInitial: WorkflowDraftInput & { excluded?: WorkflowDraft["excluded"]; notes?: string[] } =
    mode === "edit" && selected
      ? draftInputFromView(selected)
      : initialDraft
        ? {
            name: initialDraft.name,
            description: initialDraft.description,
            steps: initialDraft.steps,
            parameters: initialDraft.parameters.map((parameter) => ({
              key: parameter.key,
              required: parameter.required,
              description: parameter.description,
            })),
            excluded: initialDraft.excluded,
            notes: initialDraft.notes,
          }
        : { name: "", description: "", steps: [], parameters: [] };

  return (
    <PanelFrame embedded={embedded} onClose={close}>
      <div className={embedded ? "flex min-h-0 flex-1 flex-col gap-3 overflow-hidden" : "flex max-h-[70vh] gap-4"}>
        <div className={embedded ? "flex max-h-[45vh] min-h-0 shrink-0 flex-col gap-2" : "flex w-[280px] shrink-0 flex-col gap-2"}>
          <div className="flex items-center gap-1">
            <input
              aria-label="查找常用流程"
              className="ui-control min-w-0 flex-1 px-2 py-1 text-xs"
              placeholder="按名称查找"
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void reload();
              }}
            />
            <Button className="min-h-0 px-2 py-1 text-[11px]" onClick={() => void reload()}>
              查找
            </Button>
          </div>
          <label className="flex items-center gap-1 text-[11px] text-[var(--color-text-secondary)]">
            <input
              type="checkbox"
              aria-label="包含已停用流程"
              checked={includeArchived}
              onChange={(event) => {
                const next = event.currentTarget.checked;
                setIncludeArchived(next);
                void reload(query, next);
              }}
            />
            包含已停用
          </label>

          <div className="min-h-0 flex-1 overflow-y-auto rounded-[var(--radius-control)] border border-[var(--color-border)]">
            {views === null ? (
              <p className="px-3 py-2 text-[11px] text-[var(--color-text-secondary)]">
                正在读取常用流程…
              </p>
            ) : views.length === 0 ? (
              <p className="px-3 py-2 text-[11px] text-[var(--color-text-secondary)]">
                还没有常用流程。可以在任务卡上「保存为常用流程」，或点下方的「新建」。
              </p>
            ) : (
              <ul className="divide-y divide-[var(--color-border)]">
                {views.map((view) => (
                  <li key={view.workflowRef}>
                    <button
                      type="button"
                      aria-current={selected?.workflowRef === view.workflowRef ? "true" : undefined}
                      className={clsx(
                        "w-full px-3 py-2 text-left",
                        selected?.workflowRef === view.workflowRef
                          ? "bg-[var(--color-surface-raised)]"
                          : "hover:bg-[var(--color-surface)]",
                      )}
                      onClick={() => {
                        setSelected(view);
                        setMode("browse");
                        setRenameValue(null);
                        setNotice(null);
                      }}
                    >
                      <span className="flex items-center gap-1">
                        <span className="truncate text-sm text-[var(--color-text)]">{view.name}</span>
                        <span className="shrink-0 rounded-full border border-[var(--color-border)] px-1.5 text-[10px] text-[var(--color-text-secondary)]">
                          {versionLabel(view)}
                        </span>
                        {view.archived && (
                          <span className="shrink-0 rounded-full border border-[var(--color-status)] px-1.5 text-[10px] text-[var(--color-status)]">
                            已停用
                          </span>
                        )}
                      </span>
                      <span className="mt-0.5 block truncate text-[11px] text-[var(--color-text-tertiary)]">
                        {workflowSummaryLine(view)}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <Button
            variant="primary"
            className="min-h-0 px-2 py-1 text-[11px]"
            disabled={busy}
            onClick={() => {
              setMode("create");
              setNotice("常用流程只保存「允许的步骤 + 参数定义」，不保存这次的素材、目录授权或密钥。");
            }}
          >
            新建
          </Button>
        </div>

        <div className={embedded ? "min-h-0 flex-1 overflow-y-auto" : "min-w-0 flex-1 overflow-y-auto"}>
          {error && (
            <p role="alert" className="mb-2 text-xs text-[var(--color-danger)]">
              {error}
            </p>
          )}
          {notice && (
            <p role="status" className="mb-2 text-xs text-[var(--color-success)]">
              {notice}
            </p>
          )}

          {mode !== "browse" ? (
            <WorkflowDraftForm
              catalog={catalog}
              initial={formInitial}
              submitLabel={mode === "edit" ? "保存为新版本" : "保存常用流程"}
              busy={busy}
              error={null}
              onSubmit={save}
              onCancel={() => {
                setMode("browse");
                setError(null);
                if (initialDraft) clearDraft();
              }}
            />
          ) : selected ? (
            <div className="flex flex-col gap-3">
              <header className="flex flex-wrap items-center gap-2">
                <h3 className="text-sm font-medium text-[var(--color-text)]">{selected.name}</h3>
                <span className="rounded-full border border-[var(--color-border)] px-1.5 py-0.5 text-[10px] text-[var(--color-text-secondary)]">
                  {versionLabel(selected)}
                </span>
                {selected.archived && (
                  <span className="rounded-full border border-[var(--color-status)] px-1.5 py-0.5 text-[10px] text-[var(--color-status)]">
                    已停用
                  </span>
                )}
              </header>
              {selected.description && (
                <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
                  {selected.description}
                </p>
              )}
              {selected.needsAttention && (
                <p className="rounded-[var(--radius-control)] border border-[var(--color-status)] px-3 py-2 text-[11px] text-[var(--color-status)]">
                  {selected.needsAttention}
                </p>
              )}

              <section>
                <h4 className="text-xs font-medium text-[var(--color-text-secondary)]">
                  会申请的能力（按顺序）
                </h4>
                <ol className="mt-1 list-inside list-decimal text-[11px] text-[var(--color-text-secondary)]">
                  {selected.steps.map((step, index) => (
                    <li key={`${step.capability}-${index}`}>
                      {step.label}
                      {step.dependsOn != null ? `（依赖第 ${step.dependsOn + 1} 步）` : ""}
                      {step.warning ? `：${step.warning}` : ""}
                    </li>
                  ))}
                </ol>
              </section>

              <section>
                <h4 className="text-xs font-medium text-[var(--color-text-secondary)]">
                  复用时仍然要现场选择
                </h4>
                {selected.runtimeInputs.length === 0 ? (
                  <p className="mt-1 text-[11px] text-[var(--color-text-tertiary)]">
                    没有额外输入；素材范围与目录仍会按每次重新解析。
                  </p>
                ) : (
                  <ul className="mt-1 list-inside list-disc text-[11px] text-[var(--color-text-secondary)]">
                    {selected.runtimeInputs.map((input) => (
                      <li key={input.key}>
                        {input.label}
                        {input.required ? "（必填）" : "（可选）"}：{input.hint}
                      </li>
                    ))}
                  </ul>
                )}
              </section>

              {selected.parameters.length > 0 && (
                <section>
                  <h4 className="text-xs font-medium text-[var(--color-text-secondary)]">参数定义</h4>
                  <ul className="mt-1 list-inside list-disc text-[11px] text-[var(--color-text-secondary)]">
                    {selected.parameters.map((parameter) => (
                      <li key={parameter.key}>
                        {parameter.label}
                        {parameter.required ? "（必填）" : "（可选）"}：{parameter.description}
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {renameValue !== null && (
                <div className="flex items-center gap-2">
                  <input
                    aria-label="新的流程名称"
                    className="ui-control min-w-0 flex-1 px-2 py-1 text-xs"
                    value={renameValue}
                    onChange={(event) => setRenameValue(event.currentTarget.value)}
                  />
                  <Button
                    className="min-h-0 px-2 py-1 text-[11px]"
                    disabled={busy || renameValue.trim().length === 0}
                    onClick={() => void rename(selected)}
                  >
                    确认改名
                  </Button>
                  <Button className="min-h-0 px-2 py-1 text-[11px]" onClick={() => setRenameValue(null)}>
                    取消
                  </Button>
                </div>
              )}

              <div className="flex flex-wrap items-center gap-2">
                <Button
                  variant="primary"
                  className="min-h-0 px-2 py-1 text-[11px]"
                  disabled={busy || selected.archived}
                  onClick={() => reuse(selected)}
                >
                  再次使用
                </Button>
                <Button
                  variant="primary"
                  className="min-h-0 px-2 py-1 text-[11px]"
                  disabled={busy || selected.archived}
                  onClick={() => startWithInputs(selected)}
                >
                  用本次输入开始
                </Button>
                <Button
                  className="min-h-0 px-2 py-1 text-[11px]"
                  disabled={busy}
                  onClick={() => {
                    setMode("edit");
                    setNotice(
                      "编辑会保存为新版本；旧版本快照与在途任务使用的定义不会被改动。",
                    );
                  }}
                >
                  编辑
                </Button>
                <Button
                  className="min-h-0 px-2 py-1 text-[11px]"
                  disabled={busy}
                  onClick={() => setRenameValue(selected.name)}
                >
                  改名
                </Button>
                <Button
                  variant={selected.archived ? "ghost" : "danger"}
                  className="min-h-0 px-2 py-1 text-[11px]"
                  disabled={busy}
                  onClick={() =>
                    selected.archived
                      ? void archive(selected, false)
                      : setConfirmArchive(selected)
                  }
                >
                  {selected.archived ? "恢复可用" : "停用"}
                </Button>
              </div>
            </div>
          ) : (
            <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
              左侧选择一条常用流程查看它需要的步骤、能力和运行时输入；也可以用「新建」从零定义。
              删除或改名的操作不会改动正在运行的任务使用的版本快照。
            </p>
          )}
        </div>
      </div>

      <Modal
        open={confirmArchive !== null}
        title="停用常用流程"
        onClose={() => setConfirmArchive(null)}
        footer={
          <>
            <Button disabled={busy} onClick={() => setConfirmArchive(null)}>
              取消
            </Button>
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => confirmArchive && void archive(confirmArchive, true)}
            >
              停用
            </Button>
          </>
        }
      >
        <p className="text-sm leading-6 text-[var(--color-text-secondary)]">
          停用「{confirmArchive?.name}」后列表默认不再显示它，也不会产生新版本；
          已经用它准备好的计划和正在运行的任务保持原样，仍可读历史版本。
        </p>
      </Modal>

      {/* A4-08：实例化弹窗盖在面板之上；它自己负责每次从空白开始收集本次输入。 */}
      {runTarget && (
        <WorkflowRunDialog
          open={runTarget !== null}
          onClose={() => setRunTarget(null)}
          view={runTarget}
        />
      )}
    </PanelFrame>
  );
}
