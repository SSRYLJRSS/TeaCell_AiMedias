/** 助手消息区里挂在会话上的「计划卡 / 任务卡」（A3-04）。
 *
 * 约束：
 * - 卡片内容一律来自真实快照（PlanPreview / OperationSnapshot），模型话术不算收据；
 * - 批准按钮只提交 `planId + reviewRef`，reviewRef 必须来自该计划本身；
 * - 「停止回答」属于消息区；卡片上的「取消任务」作用于已入队 operation，两者分开；
 * - 同一 operation 的进度按 revision 单调合并（由 store 保证），卡片不自行累加。
 */
import clsx from "clsx";
import { useEffect, useState } from "react";
import Button from "@/components/common/Button";
import ProgressBar from "@/components/common/ProgressBar";
import { AppError } from "@/api/client";
import { summarySelection, type SelectionSummary } from "@/api/selections";
import { useAgentSessionStore, readableAgentError } from "@/stores/agentSessionStore";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { agentErrorMessages } from "@/types/agent.generated";
import type { OperationSnapshot, PlanPreview } from "@/types/plans";

const CAPABILITY_LABEL: Record<string, string> = {
  "assets.search": "搜索素材",
  "imports.prepare": "准备导入",
  "imports.submit": "执行导入",
  "tagging.prepare": "准备打标",
  "tagging.submit": "执行打标",
  // A4-01：导出只有「复制」这一种语义，标签必须直说复制，
  // 否则用户在计划卡上会以为库内原文件会被搬走。
  "exports.prepare": "准备复制导出",
  "exports.submit": "复制导出",
  "assets.move.prepare": "准备移动",
  "assets.move.submit": "执行移动",
  "assets.trash.prepare": "准备移入回收站",
  "assets.trash.submit": "移入回收站",
  "assets.restore.prepare": "准备恢复",
  "assets.restore.submit": "执行恢复",
  "trash.empty.prepare": "准备清空回收站",
  "trash.empty.submit": "清空回收站",
  "workflows.load": "读取流程",
  "workflows.prepare": "准备流程",
  "workflows.submit": "执行流程",
};

export function capabilityLabel(capability: string): string {
  return CAPABILITY_LABEL[capability] ?? capability;
}

const OPERATION_LABEL: Record<OperationSnapshot["state"], string> = {
  queued: "排队中",
  running: "执行中",
  succeeded: "已完成",
  partially_succeeded: "部分完成",
  failed: "失败",
  cancelled: "已取消",
  interrupted: "已中断，可恢复",
  outcome_unknown: "结果待核对",
};

const PHASE_LABEL: Record<OperationSnapshot["phase"], string> = {
  ready: "准备执行",
  executing: "执行中",
  waiting_review: "等待审核",
  waiting_tags: "等待打标",
  cancelling: "取消中",
  reconciling: "核对中",
  complete: "已结束",
};

function isTerminal(snapshot: OperationSnapshot): boolean {
  return snapshot.state !== "queued" && snapshot.state !== "running";
}

function expiresIn(secondsFromUnix: number, nowMs: number): string {
  const remain = secondsFromUnix * 1000 - nowMs;
  if (remain <= 0) return "已过期";
  const minutes = Math.ceil(remain / 60_000);
  if (minutes < 60) return `${minutes} 分钟后过期`;
  return `${Math.ceil(minutes / 60)} 小时后过期`;
}

/** 计划来源文案：外部接入的客户端要显示它在设置里的名字，其余两类给固定说法。 */
export function planOriginLabel(plan: PlanPreview): string {
  switch (plan.originKind) {
    case "mcp":
      return plan.originLabel
        ? `外部客户端：${plan.originLabel}`
        : "外部客户端（已不在配对列表中）";
    case "pi":
      return "内置助手";
    default:
      return "本机界面";
  }
}

/** `scope.targetRef` 在不同能力里含义不同，标签不能一律写成「目标」。
 *
 * A4-01：导出必须让人看出「复制到哪里」。ref 本身是不透明令牌，可读路径
 * 由后端写进 summary；这里只负责把这一行命名正确，不假装 ref 是路径。
 */
export function planTargetLabel(capability: string): string {
  if (capability.startsWith("exports.")) return "复制到：已授权目录";
  if (capability.startsWith("imports.")) return "入库到：目标分库";
  if (capability.startsWith("assets.move.")) return "移动到：库内目标位置";
  return "目标：已授权范围";
}

/** 计划卡：显示真实摘要与范围，批准后交给下一次快照回读校准。 */
export function PlanCard({ plan, highlight }: { plan: PlanPreview; highlight?: boolean }) {
  const approve = useAgentSessionStore((s) => s.approvePlanCard);
  const reject = useAgentSessionStore((s) => s.rejectPlanCard);
  const [busy, setBusy] = useState<"approve" | "reject" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    // 过期时间只在展示层刷新；真实过期判定仍由后端 approve_plan 负责。
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);

  const expired = plan.expiresAtUnixSeconds * 1000 <= now;
  const pending = plan.reviewStatus === "pending";

  const run = async (action: "approve" | "reject") => {
    setBusy(action);
    setError(null);
    try {
      if (action === "approve") {
        await approve(plan.planId, plan.reviewRef);
      } else {
        await reject(plan.planId, plan.reviewRef);
      }
    } catch (cause) {
      const code = cause instanceof AppError ? cause.code : "UNKNOWN";
      setError(
        (agentErrorMessages as Record<string, string | undefined>)[code] ??
          (cause instanceof Error ? cause.message : String(cause)),
      );
    } finally {
      setBusy(null);
    }
  };

  return (
    <section
      aria-label={`计划：${capabilityLabel(plan.capability)}`}
      className={clsx(
        "rounded-[var(--radius-control)] border bg-[var(--color-surface-raised)] p-3",
        // A3-09：从浮层「查看并确认」定位过来的那张卡要能被指出来，
        // 否则用户只能在一列计划里自己找。
        highlight ? "border-[var(--color-status)] ring-2 ring-[var(--color-status)]" : "border-[var(--color-border)]",
      )}
      data-plan-id={plan.planId}
      data-plan-highlight={highlight ? "true" : undefined}
    >
      <header className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium text-[var(--color-text)]">
          {capabilityLabel(plan.capability)} · {plan.itemCount} 项
        </h3>
        <span className={clsx("shrink-0 text-[11px]", expired ? "text-[var(--color-status)]" : "text-[var(--color-text-tertiary)]")}>
          {pending ? expiresIn(plan.expiresAtUnixSeconds, now) : reviewStatusLabel(plan.reviewStatus)}
        </span>
      </header>
      <p className="mt-1 text-xs leading-5 text-[var(--color-text-secondary)]">{plan.summary}</p>
      <dl className="mt-2 grid grid-cols-2 gap-x-3 gap-y-1 text-[11px] text-[var(--color-text-tertiary)]">
        <div className="col-span-2 truncate">发起方：{planOriginLabel(plan)}</div>
        {plan.scope.selectionRef && (
          <div className="col-span-2 truncate" title={plan.scope.selectionRef}>
            范围：已选素材集合
          </div>
        )}
        {plan.scope.sourceRef && (
          <div className="col-span-2 truncate" title={plan.scope.sourceRef}>
            来源：已授权目录
          </div>
        )}
        {/* A4-01：导出/导入/移动都会带 targetRef，此前一律不显示，
            审核者看不到文件会去哪儿。具体路径写在后端 summary 里。 */}
        {plan.scope.targetRef && (
          <div className="col-span-2 truncate" title={plan.scope.targetRef}>
            {planTargetLabel(plan.capability)}
          </div>
        )}
        {plan.scope.model && (
          <div className="col-span-2 truncate" title={plan.scope.model}>
            模型：{plan.scope.model}
          </div>
        )}
        <div>预算上限：{plan.budget.maxItems} 项</div>
        {plan.budget.maxRequests > 0 && <div>请求上限：{plan.budget.maxRequests}</div>}
        {plan.budget.monetaryLimit && (
          <div>
            费用上限：
            {(plan.budget.monetaryLimit.amountMicrounits / 1_000_000).toFixed(2)}{" "}
            {plan.budget.monetaryLimit.currency}
          </div>
        )}
      </dl>
      {!pending && plan.reviewStatus === "approved" && (
        <p className="mt-2 text-[11px] text-[var(--color-text-secondary)]">
          已批准。执行仍需在任务卡中由你明确开始。
        </p>
      )}
      {error && <p role="alert" className="mt-2 text-[11px] text-[var(--color-danger)]">{error}</p>}
      {pending && (
        <div className="mt-3 flex items-center gap-2">
          <Button
            variant="primary"
            disabled={busy !== null || expired}
            onClick={() => void run("approve")}
          >
            {busy === "approve" ? "正在确认…" : expired ? "计划已过期" : "确认此计划"}
          </Button>
          <Button
            variant="ghost"
            disabled={busy !== null || expired}
            onClick={() => void run("reject")}
          >
            {busy === "reject" ? "正在拒绝…" : "拒绝"}
          </Button>
          <span className="text-[11px] text-[var(--color-text-tertiary)]">
            仅确认范围，不代替执行
          </span>
        </div>
      )}
    </section>
  );
}

function reviewStatusLabel(status: PlanPreview["reviewStatus"]): string {
  switch (status) {
    case "pending": return "待确认";
    case "approved": return "已确认";
    case "expired": return "已过期";
    case "invalidated": return "已失效";
    case "consumed": return "已执行";
  }
}

/** 任务卡：真实 operation 快照 + 开始/取消/继续，三者语义分开。 */
export function OperationCard({ snapshot }: { snapshot: OperationSnapshot }) {
  const cancelOperation = useAgentSessionStore((s) => s.cancelOperationCard);
  const resumeOperation = useAgentSessionStore((s) => s.resumeOperationCard);
  const startOperation = useAgentSessionStore((s) => s.startOperationCard);
  const saveAsWorkflow = useWorkflowPanelStore((s) => s.openFromOperation);
  const draftingFrom = useWorkflowPanelStore((s) => s.draftingFrom);
  const [busy, setBusy] = useState<"start" | "cancel" | "resume" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const done = isTerminal(snapshot);
  // 待执行 = 已批准、已提交，但还没有任何文件效果。批准与提交只写库；
  // 真正复制/移动/恢复/清空必须由用户在这里点一次。
  const pendingStart = snapshot.state === "queued";
  const total = snapshot.total ?? 0;
  const overall = done
    ? snapshot.state === "succeeded"
      ? 1
      : total > 0
        ? Math.min(1, snapshot.completed / total)
        : 0
    : total > 0
      ? Math.min(1, snapshot.completed / total)
      : null;

  const run = async (kind: "start" | "cancel" | "resume") => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === "cancel") await cancelOperation(snapshot.operationId);
      else if (kind === "resume") await resumeOperation(snapshot.operationId);
      else await startOperation(snapshot.operationId);
    } catch (cause) {
      const code = cause instanceof AppError ? cause.code : "UNKNOWN";
      setError(readableAgentError(code, cause instanceof Error ? cause.message : String(cause)));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section
      aria-label={`任务：${OPERATION_LABEL[snapshot.state]}`}
      className="rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-3"
      data-operation-id={snapshot.operationId}
    >
      <header className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium text-[var(--color-text)]">
          任务 · {OPERATION_LABEL[snapshot.state]}
        </h3>
        <span className="shrink-0 text-[11px] text-[var(--color-text-tertiary)]">
          {PHASE_LABEL[snapshot.phase]}
        </span>
      </header>
      <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
        已完成 {snapshot.completed}
        {snapshot.total != null ? ` / ${snapshot.total}` : ""}
        {snapshot.failed > 0 ? ` · 失败 ${snapshot.failed}` : ""}
      </p>
      {pendingStart ? (
        <p className="mt-2 text-[11px] text-[var(--color-text-secondary)]">
          已提交，等待你开始执行。点「开始执行」后才会真正修改文件。
        </p>
      ) : overall != null ? (
        <ProgressBar value={overall} className="mt-2" />
      ) : (
        <ProgressBar value={0} indeterminate className="mt-2" />
      )}
      {snapshot.errorCode && (
        <p className="mt-2 text-[11px] text-[var(--color-danger)]">
          {readableAgentError(snapshot.errorCode, snapshot.errorCode)}
        </p>
      )}
      {snapshot.state === "outcome_unknown" && (
        <p className="mt-2 text-[11px] text-[var(--color-status)]">
          部分结果需要核实：请先核对恢复摘要，不要重复提交同一任务。
        </p>
      )}
      {error && <p role="alert" className="mt-2 text-[11px] text-[var(--color-danger)]">{error}</p>}
      <div className="mt-3 flex items-center gap-2">
        {pendingStart && (
          <Button variant="primary" disabled={busy !== null} onClick={() => void run("start")}>
            {busy === "start" ? "正在执行…" : "开始执行"}
          </Button>
        )}
        {snapshot.canCancel && (
          <Button disabled={busy !== null} onClick={() => void run("cancel")}>
            {busy === "cancel" ? "正在取消…" : "取消任务"}
          </Button>
        )}
        {snapshot.canResume && (
          <Button variant="primary" disabled={busy !== null} onClick={() => void run("resume")}>
            {busy === "resume" ? "正在继续…" : "继续任务"}
          </Button>
        )}
        {done && !snapshot.canResume && (
          <span className="text-[11px] text-[var(--color-text-tertiary)]">任务已结束</span>
        )}
        {/* A4-07：只把「已理解的业务步骤」存成模板。素材、目录授权与这次的批准不带进模板，
            也会在草稿里把无法合法表达的步骤列出来而不是改写成脚本。 */}
        {done && (
          <Button
            className="ml-auto min-h-0 px-2 py-0.5 text-[11px]"
            disabled={draftingFrom !== null}
            onClick={() => void saveAsWorkflow(snapshot.operationId)}
          >
            {draftingFrom === snapshot.operationId ? "正在生成草稿…" : "保存为常用流程"}
          </Button>
        )}
      </div>
    </section>
  );
}

/** A3-08 交接卡：会话里已经交接过的素材集合。
 *
 *  卡片只回读后端 `summary_selection`（快照/可用/不可用三个数），不加载素材行、
 *  不改素材库的选择。点开进入素材库的「本次结果视图」（只读、可退出、不建分库）。 */
export function SelectionCard({
  selectionRef,
  conversationRef,
}: {
  selectionRef: string;
  /** A3-08：这份交接属于哪个会话。结果视图是全局单槽，切会话后回来要能说清来源。 */
  conversationRef?: string;
}) {
  const [summary, setSummary] = useState<SelectionSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const openResult = useSelectionResultStore((s) => s.open);

  useEffect(() => {
    let alive = true;
    setSummary(null);
    setError(null);
    summarySelection(selectionRef)
      .then((next) => {
        if (alive) setSummary(next);
      })
      .catch((cause: unknown) => {
        if (!alive) return;
        const code = cause instanceof AppError ? cause.code : "UNKNOWN";
        setError(readableAgentError(code, cause instanceof Error ? cause.message : String(cause)));
      });
    return () => {
      alive = false;
    };
  }, [selectionRef]);

  return (
    <section
      aria-label="交接的素材集合"
      className="rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-3"
      data-selection-ref={selectionRef}
    >
      <header className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium text-[var(--color-text)]">
          素材集合{summary ? ` · ${summary.snapshot} 项` : ""}
        </h3>
        <span className="shrink-0 text-[11px] text-[var(--color-text-tertiary)]">固定快照</span>
      </header>
      {error ? (
        <p role="alert" className="mt-1 text-[11px] text-[var(--color-danger)]">
          {error}
        </p>
      ) : (
        <p className="mt-1 text-xs leading-5 text-[var(--color-text-secondary)]">
          {summary
            ? `交接时固定为 ${summary.snapshot} 项素材（可用 ${summary.available} 项${
                summary.missing > 0 ? `，${summary.missing} 项已被删除或移出总库` : ""
              }）。之后你在素材库里改选择不会改变这份集合。`
            : "正在读取这份集合…"}
          {summary?.truncated ? " 交接时即已截断：只包含当时加载到的项。" : ""}
        </p>
      )}
      <div className="mt-3 flex items-center gap-2">
        <Button
          disabled={!summary}
          onClick={() => {
            openResult({
              selectionRef,
              title: summary ? `助手交接的 ${summary.snapshot} 项` : "助手交接的素材",
              // A3-08：来源取后端快照行的 query。这里以前硬编码「助手会话」，
              // 于是同一份快照在结果视图里显示成另一种来源。
              query: summary?.query ?? "助手会话",
              conversationRef,
            });
            // 结果视图托管在素材库页：先开快照目标，再切页，避免闪一帧空视图。
            window.dispatchEvent(new CustomEvent("app:navigate", { detail: "library" }));
          }}
        >
          查看本次结果
        </Button>
        <span className="text-[11px] text-[var(--color-text-tertiary)]">只读快照，不创建分库</span>
      </div>
    </section>
  );
}
