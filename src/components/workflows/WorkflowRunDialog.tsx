/** 用一次「本次输入」把常用流程实例化成新计划（A4-08）。
 *
 * 这个弹窗的全部意义在于：常用流程**不是**可以一键重跑的执行授权。
 * 因此这里有三条不写成注释就一定会被「优化」掉的行为：
 *
 * 1. **不继承任何旧值。** 每次打开都从空白开始（模板只保存「这里要填什么」），
 *    输入一变就丢弃上一份预览 —— 否则「确认」会批准一份用旧素材生成的计划。
 * 2. **不能由流程代批的步骤要说在前面。** `requiresSeparateAction` 的步骤（例如
 *    AI 打标、入库、导出）仍要用户在自己的界面单独确认，文案直接点名第几步、去哪儿。
 * 3. **来源目录不代选。** 2.0 里流程不替用户在入库页挑目录；出现 `sourceRef`
 *    就只读提示并禁用确认，宁可让用户回入库页，也不要凭空造一个目录引用。
 *
 * `selectionRef` 没有存在 store 里，所以在准备计划的那一刻才用 `snapshotSelection`
 * 固定「当前选择」——固定之后用户再改选，也不会改写这份计划对应的集合。
 */
import { useCallback, useEffect, useState } from "react";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import { pickDirectoryGrant, type DirectoryGrant } from "@/api/authorization";
import { listAiConnections, type AiConnection } from "@/api/connections";
import { listMoveTargets, type MoveTarget } from "@/api/move";
import { snapshotSelection } from "@/api/selections";
import { versionLabel, type WorkflowView } from "@/api/workflows";
import {
  cancelWorkflowInstance,
  confirmBlockedReason,
  errorMessage,
  fetchWorkflowRunReport,
  isRunFinished,
  isWorkflowInputKey,
  prepareWorkflowInstance,
  requiresSeparateNotice,
  runPhaseLabel,
  runSummaryLines,
  runWorkflowInstance,
  stepStateLabel,
  type WorkflowInputKey,
  type WorkflowInstanceInputs,
  type WorkflowInstancePreviewView,
  type WorkflowRunReport,
  type WorkflowRunStart,
} from "@/api/workflowRun";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";

interface WorkflowRunDialogProps {
  open: boolean;
  onClose: () => void;
  view: WorkflowView;
}

type Busy = "prepare" | "run" | "cancel" | null;

type Values = Partial<Record<WorkflowInputKey, string>>;

/** 运行期轮询间隔。太快会白白压住 IPC，太慢会让「正在执行」看起来像卡住。 */
const POLL_INTERVAL_MS = 800;

/** 快照名要能看出是「当时条件下的选择」，和交接弹窗保持同一说法。 */
function describeQuery(search: string): string {
  const trimmed = search.trim();
  return trimmed ? `搜索「${trimmed}」的结果` : "素材库中的选择";
}

/**
 * 收拢成后端要的输入：只带这次真的问到、且填了值的键。
 *
 * 空串一律当「没填」而不是「填了空」——后端按 Option 收，`null` 与省略等价，
 * 但不该出现的键就干脆不出现，免得将来某个键变必填时前端悄悄塞个空串。
 */
export function buildInputs(
  view: WorkflowView,
  values: Values,
  selectionRef: string | null,
): WorkflowInstanceInputs {
  const inputs: WorkflowInstanceInputs = {};
  for (const input of view.runtimeInputs) {
    if (!isWorkflowInputKey(input.key)) continue;
    if (input.key === "selectionRef") {
      if (selectionRef) inputs.selectionRef = selectionRef;
      continue;
    }
    const raw = (values[input.key] ?? "").trim();
    if (raw.length === 0) continue;
    inputs[input.key] = raw;
  }
  return inputs;
}

/** 预览区一行步骤：摘要序号 + 名称 + 是否需要单独确认。 */
export function stepPreviewLine(step: WorkflowInstancePreviewView["steps"][number]): string {
  const counts =
    step.itemCount > 0 ? `（${step.itemCount} 项）` : "";
  return `第 ${step.ordinal} 步「${step.label}」${counts}`;
}

export default function WorkflowRunDialog({ open, onClose, view }: WorkflowRunDialogProps) {
  const selected = useSelectionStore((s) => s.selected);
  const truncated = useSelectionStore((s) => s.truncated);
  const selectionTotal = useSelectionStore((s) => s.selectionTotal);
  const search = useLibraryStore((s) => s.filter.search);

  const [values, setValues] = useState<Values>({});
  const [grant, setGrant] = useState<DirectoryGrant | null>(null);
  const [targets, setTargets] = useState<MoveTarget[]>([]);
  const [connections, setConnections] = useState<AiConnection[]>([]);
  const [loadingLists, setLoadingLists] = useState(false);
  const [preview, setPreview] = useState<WorkflowInstancePreviewView | null>(null);
  const [start, setStart] = useState<WorkflowRunStart | null>(null);
  const [live, setLive] = useState<WorkflowRunReport | null>(null);
  const [liveError, setLiveError] = useState<string | null>(null);
  const [report, setReport] = useState<WorkflowRunReport | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);

  const keys = new Set(view.runtimeInputs.map((input) => input.key));
  const needsSelection = keys.has("selectionRef");
  const needsSource = keys.has("sourceRef");
  const needsCollection = keys.has("collectionRef");
  const needsService = keys.has("serviceRef");

  const chosenCount = selected.size;
  // 截断集合上「已选 N」不等于「范围就是 N」：与交接弹窗一致，用当时的匹配总数说明。
  const effectiveCount = truncated ? selectionTotal : chosenCount;

  /** 改任何输入都必须作废预览：旧预览对应的是一份旧输入的计划。 */
  const setValue = useCallback((key: WorkflowInputKey, value: string) => {
    setValues((current) => ({ ...current, [key]: value }));
    setPreview(null);
    setError(null);
  }, []);

  // 每次打开都从空白开始：上一次填过的目录、服务、素材范围一律不沿用。
  useEffect(() => {
    if (!open) return;
    setValues({});
    setGrant(null);
    setTargets([]);
    setConnections([]);
    setPreview(null);
    setReport(null);
    setStart(null);
    setLive(null);
    setLiveError(null);
    setCancelled(false);
    setError(null);
    setBusy(null);
  }, [open, view]);

  // 下拉选项按需加载：不涉及分库/服务时不必为一次准备计划多打两个命令。
  useEffect(() => {
    if (!open) return;
    // 两个下拉都不需要时直接返回。原先这里仍会 `setLoadingLists(false)` 并通过
    // `Promise.all([])` 在下一个微任务里再落一次状态 —— 对一次没有异步工作的渲染
    // 是纯粹的状态抖动，也是测试里 act 警告的来源之一。
    if (!needsCollection && !needsService) return;
    let alive = true;
    setLoadingLists(true);
    const jobs: Promise<void>[] = [];
    if (needsCollection) {
      jobs.push(
        listMoveTargets()
          .then((list) => {
            if (!alive) return;
            setTargets(list);
            // 默认选「总仓根目录」：模板里不能带旧分库，但宁可给一个确定的默认也不留空。
            const root = list.find((item) => item.relativePath === "");
            if (root) {
              setValues((current) =>
                current.collectionRef ? current : { ...current, collectionRef: root.collectionRef },
              );
            }
          })
          .catch((cause: unknown) => {
            if (!alive) return;
            setTargets([]);
            setError(`读取分库失败：${errorMessage(cause)}`);
          }),
      );
    }
    if (needsService) {
      jobs.push(
        listAiConnections()
          .then((list) => {
            if (alive) setConnections(list);
          })
          .catch((cause: unknown) => {
            if (!alive) return;
            setConnections([]);
            setError(`读取 AI 服务失败：${errorMessage(cause)}`);
          }),
      );
    }
    void Promise.all(jobs).finally(() => {
      if (alive) setLoadingLists(false);
    });
    return () => {
      alive = false;
    };
  }, [open, view, needsCollection, needsService]);

  const chooseDestination = async () => {
    setError(null);
    try {
      const picked = await pickDirectoryGrant("export_destination", false);
      if (!picked) return;
      setGrant(picked);
      setValue("destinationRef", picked.directoryRef ?? picked.grantRef);
    } catch (cause) {
      setError(`选择导出目标失败：${errorMessage(cause)}`);
    }
  };

  const blocked = confirmBlockedReason(
    { runtimeInputs: view.runtimeInputs, steps: view.steps },
    values,
    effectiveCount,
  );
  const destinationLabel = grant?.displayPath ?? grant?.displayName ?? "";
  const separateNotice = preview ? requiresSeparateNotice(preview) : null;

  const prepare = async () => {
    if (busy) return;
    setBusy("prepare");
    setError(null);
    setReport(null);
    try {
      let selectionRef: string | null = null;
      if (needsSelection) {
        // 只在准备计划这一刻固定集合：之后再改选不会改写这份计划。
        const snapshot = await snapshotSelection(
          describeQuery(search),
          Array.from(selected),
          effectiveCount,
        );
        selectionRef = snapshot.selectionRef;
      }
      const prepared = await prepareWorkflowInstance(
        view.workflowRef,
        view.version,
        buildInputs(view, values, selectionRef),
      );
      setPreview(prepared);
    } catch (cause) {
      setError(`准备计划失败：${errorMessage(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const run = async () => {
    if (!preview || busy || start) return;
    setBusy("run");
    setError(null);
    setReport(null);
    setCancelled(false);
    try {
      // 受理即返回：拿到的只是一张回执，不是终态。之后靠轮询读真实进度。
      const accepted = await runWorkflowInstance(preview.planId);
      setStart(accepted);
      if (isRunFinished(accepted.operationState)) {
        // 幂等命中一条已经结束的任务（例如用户重开同一份计划查看上次结果）。
        setReport(await fetchWorkflowRunReport(preview.planId));
      }
    } catch (cause) {
      setError(`执行失败：${errorMessage(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const cancelRun = async () => {
    if (!start || busy || cancelled) return;
    setBusy("cancel");
    setError(null);
    try {
      await cancelWorkflowInstance(start.operationId);
      setCancelled(true);
    } catch (cause) {
      setError(`取消失败：${errorMessage(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const close = () => onClose();

  // 运行期轮询。停止条件只有一个：数据库给出的状态已经终态。
  // 「看起来没反应」不是停止条件 —— 否则任务还在跑、界面却已经放弃观察，
  // 用户会以为它失败了，然后再点一次。
  const planId = start?.planId ?? null;
  const running = start !== null && report === null;
  useEffect(() => {
    if (!open || !planId || !running) return;
    let alive = true;
    const tick = () => {
      void fetchWorkflowRunReport(planId)
        .then((current) => {
          if (!alive) return;
          if (isRunFinished(current.operationState)) {
            setReport(current);
            setLive(null);
          } else {
            setLive(current);
          }
        })
        .catch((cause) => {
          // 单次轮询失败不改写结论：任务可能仍在跑，只是这一问没问到。
          if (alive) setLiveError(errorMessage(cause));
        });
    };
    tick();
    const timer = window.setInterval(tick, POLL_INTERVAL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [open, planId, running]);

  // 打开时重置已经在上面那个 effect 里做了；关闭后把运行态也清掉，避免下一次
  // 打开时一闪而过地显示上一轮的回执。
  useEffect(() => {
    if (open) return;
    setStart(null);
    setLive(null);
    setLiveError(null);
    setReport(null);
  }, [open]);

  const renderInput = (key: string, label: string, hint: string) => {
    if (!isWorkflowInputKey(key)) {
      return (
        <p key={key} className="text-xs text-[var(--color-status)]">
          「{label}」目前无法在界面上填写（{hint}），请联系维护者。
        </p>
      );
    }
    switch (key) {
      case "selectionRef":
        return (
          <div key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <p className="text-sm text-[var(--color-text)]" data-testid="workflow-run-selection">
              {chosenCount === 0
                ? "先去素材库选择素材"
                : `已在素材库选中 ${chosenCount} 项${truncated ? `（当前条件共 ${selectionTotal} 项，只加载了 ${chosenCount} 项）` : ""}`}
            </p>
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              {hint}·准备计划时会固定成快照，之后改选不会改写这份计划。
            </p>
          </div>
        );
      case "sourceRef":
        return (
          <div key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <p className="text-sm text-[var(--color-status)]">请先在入库页选择来源目录</p>
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              来源目录不构成授权，流程也不会替您挑；请在入库页选好目录后再从那里开始。
            </p>
          </div>
        );
      case "destinationRef":
        return (
          <div key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <div className="flex items-center gap-2">
              <input
                readOnly
                aria-label={`${label}目录`}
                value={destinationLabel}
                placeholder="还没有选择目标目录"
                className="ui-control min-w-0 flex-1 px-2 py-1.5 text-sm"
              />
              <Button className="min-h-0 px-2 py-1 text-[11px]" disabled={busy !== null} onClick={() => void chooseDestination()}>
                选择导出目标目录
              </Button>
            </div>
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              {hint}·目录授权每次单独给，模板不保存上次的目录。
            </p>
          </div>
        );
      case "collectionRef":
        return (
          <label key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <select
              aria-label={label}
              className="ui-control px-2 py-1.5 text-sm"
              value={values.collectionRef ?? ""}
              disabled={busy !== null || loadingLists}
              onChange={(event) => setValue("collectionRef", event.currentTarget.value)}
            >
              <option value="">{loadingLists ? "正在读取分库…" : "请选择分库…"}</option>
              {targets.map((target) => (
                <option key={target.collectionRef} value={target.collectionRef}>
                  {target.label}
                </option>
              ))}
            </select>
            <span className="text-[11px] text-[var(--color-text-tertiary)]">{hint}</span>
          </label>
        );
      case "serviceRef":
        return (
          <label key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <select
              aria-label={label}
              className="ui-control px-2 py-1.5 text-sm"
              value={values.serviceRef ?? ""}
              disabled={busy !== null || loadingLists}
              onChange={(event) => setValue("serviceRef", event.currentTarget.value)}
            >
              <option value="">
                {loadingLists
                  ? "正在读取 AI 服务…"
                  : connections.length === 0
                    ? "需要先在设置里配置 AI 服务"
                    : "请选择 AI 服务…"}
              </option>
              {connections.map((connection) => (
                <option key={connection.id} value={connection.id}>
                  {connection.name}
                  {connection.credentialStatus === "configured" ? "" : "（凭据不可用）"}
                </option>
              ))}
            </select>
            <span className="text-[11px] text-[var(--color-text-tertiary)]">
              {hint}·每次按当前绑定重新核实，缺失时先补齐再开始。
            </span>
          </label>
        );
      case "taggingInstructions":
        return (
          <label key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <textarea
              aria-label={label}
              className="ui-control min-h-[56px] w-full px-2 py-1.5 text-sm"
              value={values.taggingInstructions ?? ""}
              disabled={busy !== null}
              onChange={(event) => setValue("taggingInstructions", event.currentTarget.value)}
            />
            <span className="text-[11px] text-[var(--color-text-tertiary)]">{hint}</span>
          </label>
        );
      case "renamePattern":
        return (
          <label key={key} className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">{label}</span>
            <input
              aria-label={label}
              className="ui-control w-full px-2 py-1.5 text-sm"
              value={values.renamePattern ?? ""}
              disabled={busy !== null}
              onChange={(event) => setValue("renamePattern", event.currentTarget.value)}
            />
            <span className="text-[11px] text-[var(--color-text-tertiary)]">{hint}</span>
          </label>
        );
    }
  };

  return (
    <Modal
      open={open}
      title={`使用常用流程：${view.name}（${versionLabel(view)}）`}
      onClose={close}
      wide
      footer={
        <>
          <Button onClick={close}>关闭</Button>
          <Button
            className="min-h-0 px-2 py-1 text-[11px]"
            disabled={busy !== null || report !== null || blocked !== null}
            onClick={() => void prepare()}
          >
            {busy === "prepare" ? "正在准备…" : "准备计划"}
          </Button>
          <Button
            variant="primary"
            disabled={busy !== null || preview === null || report !== null || start !== null || blocked !== null}
            onClick={() => void run()}
          >
            {busy === "run" ? "正在受理…" : "确认并开始"}
          </Button>
        </>
      }
    >
      <div className="flex max-h-[70vh] flex-col gap-3 overflow-y-auto">
        <p className="text-[11px] text-[var(--color-text-secondary)]">
          常用流程只保存步骤与参数定义。下面这些输入属于这一次：每次都要现场重选，
          模板不会沿用上一次的素材、目录或服务。
        </p>

        <section className="flex flex-col gap-2">
          <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">本次输入</h3>
          {view.runtimeInputs.length === 0 ? (
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              这个流程不需要额外输入；素材范围与目录仍会按每次重新解析。
            </p>
          ) : (
            view.runtimeInputs.map((input) => renderInput(input.key, input.label, input.hint))
          )}
          {needsSource && (
            <p className="text-[11px] text-[var(--color-status)]">
              含来源目录的流程请在入库页选好目录后再开始，这里不会替您代选。
            </p>
          )}
          {blocked && (
            <p role="alert" className="text-xs text-[var(--color-danger)]">
              暂时不能开始：{blocked}
            </p>
          )}
        </section>

        {preview && (
          <section className="flex flex-col gap-2" data-testid="workflow-run-preview">
            <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">本次计划预览</h3>
            <p className="text-sm leading-5 text-[var(--color-text)]">{preview.summary}</p>
            <ol className="flex flex-col gap-1">
              {preview.steps.map((step) => (
                <li
                  key={`${step.capability}-${step.ordinal}`}
                  className={
                    step.requiresSeparateAction
                      ? "rounded-[var(--radius-control)] border border-[var(--color-status)] px-2 py-1 text-[11px] text-[var(--color-status)]"
                      : "px-2 py-1 text-[11px] text-[var(--color-text-secondary)]"
                  }
                >
                  {stepPreviewLine(step)}
                  {step.requiresSeparateAction && "（需要您单独确认）"}
                  {step.note ? `：${step.note}` : ""}
                </li>
              ))}
            </ol>
            {separateNotice && (
              <p className="rounded-[var(--radius-control)] border border-[var(--color-status)] px-2 py-1 text-[11px] text-[var(--color-status)]">
                {separateNotice}
              </p>
            )}
            {preview.warnings.length > 0 && (
              <ul className="list-inside list-disc text-[11px] text-[var(--color-status)]">
                {preview.warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
            )}
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              确认后才会开始执行；这份计划只对应上面这些输入。
            </p>
          </section>
        )}

        {running && (
          <section className="flex flex-col gap-2" data-testid="workflow-run-progress">
            <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">正在运行</h3>
            <p className="text-xs text-[var(--color-status)]">
              {runPhaseLabel(live?.operationState ?? start?.operationState ?? "queued")}
              {start?.replayed && "（这次是重新查看同一份计划，没有新建任务）"}
            </p>
            {live && live.steps.length > 0 && (
              <ul className="flex flex-col gap-1 text-[11px] text-[var(--color-text-secondary)]">
                {live.steps.map((step) => (
                  <li key={`${step.capability}-${step.ordinal}`}>
                    第 {step.ordinal} 步「{step.label}」：{stepStateLabel(step.state)}（
                    {step.done} / {step.total} 项）
                  </li>
                ))}
              </ul>
            )}
            {liveError && (
              <p className="text-[11px] text-[var(--color-status)]">
                这一问没有问到进度（{liveError}），任务本身仍在进行，稍后会自动重试读取。
              </p>
            )}
            <p className="text-[11px] text-[var(--color-text-tertiary)]">
              取消只会停止还没开始的步骤，已经完成的文件效果保持原样，不会回滚。
            </p>
            <div>
              <Button
                variant="danger"
                className="min-h-0 px-2 py-1 text-[11px]"
                disabled={busy !== null || cancelled}
                onClick={() => void cancelRun()}
              >
                {cancelled ? "已请求取消" : busy === "cancel" ? "正在取消…" : "取消任务"}
              </Button>
            </div>
          </section>
        )}

        {report && (
          <section className="flex flex-col gap-2">
            <h3 className="text-xs font-medium text-[var(--color-text-secondary)]">执行结果</h3>
            <div className="flex flex-col gap-1 text-xs" data-testid="workflow-run-summary">
              {runSummaryLines(report).map((line) => (
                <p key={line} className="text-[var(--color-text)]">
                  {line}
                </p>
              ))}
            </div>
            <p className="text-[11px] text-[var(--color-text-secondary)]">{report.summary}</p>

            {report.operationState === "partially_succeeded" && (
              <p className="rounded-[var(--radius-control)] border border-[var(--color-status)] px-2 py-1 text-[11px] text-[var(--color-status)]">
                部分步骤未完成，未完成的步骤保持原样，不会自动重试。
              </p>
            )}
            {report.operationState === "outcome_unknown" && (
              <p className="rounded-[var(--radius-control)] border border-[var(--color-danger)] px-2 py-1 text-[11px] text-[var(--color-danger)]">
                部分步骤结果未知，请到任务记录里核对后再决定是否重试。
              </p>
            )}
          </section>
        )}

        {error && (
          <p role="alert" className="text-xs text-[var(--color-danger)]">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
