/** 入库进度与操作状态文案（A3-07 抽出共用）。
 *
 * 独立入库页（`src/pages/ImportPage.tsx`）和助手右侧工作台
 * （`src/components/assistant/AssistantWorkbench.tsx`）必须用**同一套**状态文案展示
 * 同一个 operation；否则同一任务在两处会显示不同的「正在入库 / 取消中」措辞，
 * 用户无法判断是不是两个任务。因此文案与进度面板集中在这里，两处只负责布局。
 *
 * 本组件不 invoke，也不持有状态：进度来自 `taskStore` 的 `TaskItem` 投影。
 */
import clsx from "clsx";
import ProgressBar from "@/components/common/ProgressBar";
import { importPhaseLabel, type TaskItem } from "@/stores/taskStore";
import type { OperationSnapshot } from "@/types/plans";

export function operationLabel(operation: OperationSnapshot): string {
  switch (operation.state) {
    case "queued": return "排队中";
    case "running": return operation.phase === "cancelling" ? "取消中" : "正在入库";
    case "succeeded": return "已完成";
    case "partially_succeeded": return "部分完成";
    case "failed": return "失败";
    case "cancelled": return "已取消";
    case "interrupted": return "已中断，可恢复";
    case "outcome_unknown": return "结果待核对";
  }
}

/**
 * 已终态但没有可用百分比的任务，不能继续显示「准备中」。
 *
 * `overall` 只在拿到总量或成功结束时才有值；`partially_succeeded` 和
 * `outcome_unknown` 这类终态可能既没有总量也没成功，于是会落进 `percent == null`。
 * 那里如果直接写「准备中」，用户看到的是一个已经结束、结果还不明的任务仍在转圈，
 * 无法判断它到底停没停。所以终态优先给出真实状态，只有真正在跑的才说「准备中」。
 */
function terminalLabel(task: TaskItem): string | null {
  if (!task.done) return null;
  const snapshot = task.operationSnapshot;
  if (!snapshot) return null;
  if (snapshot.state === "succeeded") return null;
  return operationLabel(snapshot);
}

export default function ImportProgressPanel({ task, starting }: { task?: TaskItem; starting: boolean }) {
  const progress = task?.importProgress;
  const idle = !task && !starting;
  const indeterminate = starting || Boolean(task && task.overall == null && !task.done);
  const percent = task?.overall == null ? null : Math.round(task.overall * 100);
  const stateText = task?.operationSnapshot ? task.detail : undefined;
  const phase = progress ? importPhaseLabel(progress.phase) : stateText ?? "正在检查文件";
  const phaseText = task?.detail?.startsWith("取消中") ? task.detail : phase;
  const hasErrors = Boolean(progress && progress.failed > 0) || Boolean(task?.operationSnapshot?.failed);
  const taskError = task?.error && !hasErrors ? task.error : null;
  const finished = task ? terminalLabel(task) : null;
  // 空闲时不能画一条 0% 的进度条：它看起来像「已经开始了但卡在 0」。
  // 只有真正有任务（或正在扫描）时才渲染进度条。
  const showBar = starting || Boolean(task && (!task.done || percent != null));

  return (
    <section aria-label="入库进度" className="min-w-0" data-state={idle ? "idle" : task?.done ? "done" : "running"}>
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-xs font-medium text-[var(--color-text)]">入库进度</h3>
        <span className={clsx("shrink-0 text-[11px]", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text-secondary)]")}>
          {idle ? "等待入库" : finished ?? (indeterminate || percent == null ? "准备中" : `${percent}%`)}
        </span>
      </div>
      {showBar && <ProgressBar value={task?.overall ?? 0} indeterminate={indeterminate} className="mt-2" />}
      {!idle && <p className={clsx("mt-2 text-xs", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text)]")} aria-live="polite">{phaseText}</p>}
      {progress?.file && <p className="mt-1 truncate text-[11px] text-[var(--color-text-secondary)]" title={progress.file}>{progress.file}</p>}
      {progress?.phaseTotal != null && progress.phaseTotal > 0 && (
        <p className="mt-1 text-[11px] text-[var(--color-text-tertiary)]">当前阶段 {progress.phaseCurrent}/{progress.phaseTotal}</p>
      )}
      {task?.operationSnapshot && <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">
        已处理 {task.operationSnapshot.completed}/{task.operationSnapshot.total ?? "?"} · 失败 {task.operationSnapshot.failed}
      </p>}
      {progress && (progress.phase === "previewing" || progress.phase === "done") && <div className="mt-2 flex flex-wrap gap-x-2 gap-y-1 text-[11px] text-[var(--color-text-secondary)]">
        <span>成功 {progress.imported}</span><span>重复 {progress.duplicates}</span><span>失败 {progress.failed}</span>
      </div>}
      {taskError && <p className="mt-1 text-[11px] text-[var(--color-status)]">{taskError}</p>}
    </section>
  );
}
