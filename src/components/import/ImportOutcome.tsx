/** 本次入库结果摘要（独立入库页右栏）。
 *
 * 为什么需要它：清单提交后 `plans` 会被清空，右栏如果只剩一个巨大的虚线拖拽框，
 * 用户看到的就是“入库完了，但什么都没了”。左栏负责实时进度，右栏负责**结果**：
 * 新入库 / 重复 / 失败、入库位置、失败与重复明细、可执行的下一步。
 *
 * 边界：
 * - 不伪造结果：计数只来自 `ImportResultView`；结果还没读回来时显示“正在读取结果”。
 * - 不重复画进度条：运行中只给一句摘要，详细阶段仍在左栏「入库进度」。
 * - 不直接 invoke：所有动作由页面/hook 传入。
 */
import clsx from "clsx";
import { formatPathForDisplay } from "@/utils/pathDisplay";
import {
  AlertTriangle,
  CheckCircle2,
  CircleSlash,
  FolderOpen,
  Loader2,
  RotateCcw,
  XCircle,
} from "lucide-react";
import type { ImportResultView } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";

type Tone = "running" | "done" | "warning" | "error" | "idle";

function statusOf(operation: OperationSnapshot): { tone: Tone; title: string } {
  switch (operation.state) {
    case "queued":
      return { tone: "running", title: "已排队" };
    case "running":
      return { tone: "running", title: "正在入库" };
    case "succeeded":
      return { tone: "done", title: "入库完成" };
    case "partially_succeeded":
      return { tone: "warning", title: "部分完成" };
    case "failed":
      return { tone: "error", title: "入库失败" };
    case "cancelled":
      return { tone: "idle", title: "已取消" };
    case "interrupted":
      return { tone: "warning", title: "已中断" };
    case "outcome_unknown":
      return { tone: "warning", title: "结果待核对" };
  }
}

function subtitleOf(operation: OperationSnapshot): string {
  switch (operation.state) {
    case "queued":
      return "等待前面的任务结束后开始。";
    case "running":
      return "详细阶段和当前文件见左侧「入库进度」。";
    case "succeeded":
      return "文件已写入素材库。";
    case "partially_succeeded":
      return "部分文件没有入库，请查看下面的明细。";
    case "failed":
      return "本次没有文件成功入库，请查看下面的明细。";
    case "cancelled":
      return "已取消；还没处理的文件不会继续。";
    case "interrupted":
      return "上次入库被中断，未完成的项可以继续。";
    case "outcome_unknown":
      return "上一次执行结果未确认，请核对后再决定是否继续。";
  }
}

function StatusIcon({ tone }: { tone: Tone }) {
  const className = "mt-0.5 size-5 shrink-0";
  if (tone === "running") {
    return <Loader2 aria-hidden="true" className={clsx(className, "animate-spin text-[var(--color-text-secondary)]")} />;
  }
  if (tone === "done") {
    return <CheckCircle2 aria-hidden="true" className={clsx(className, "text-[var(--color-success)]")} />;
  }
  if (tone === "error") {
    return <XCircle aria-hidden="true" className={clsx(className, "text-[var(--color-danger)]")} />;
  }
  if (tone === "warning") {
    return <AlertTriangle aria-hidden="true" className={clsx(className, "text-[var(--color-status)]")} />;
  }
  return <CircleSlash aria-hidden="true" className={clsx(className, "text-[var(--color-text-tertiary)]")} />;
}

export interface ImportOutcomeProps {
  operation: OperationSnapshot;
  result?: ImportResultView;
  resultError?: string;
  resultLoading: boolean;
  collection: string;
  libraryRoot: string;
  onRetryResult: () => void;
  onOpenLibrary: () => void;
  onResume: () => void;
  onCancel: () => void;
  onHandoff?: () => void;
}

export default function ImportOutcome({
  operation,
  result,
  resultError,
  resultLoading,
  collection,
  libraryRoot,
  onRetryResult,
  onOpenLibrary,
  onResume,
  onCancel,
  onHandoff,
}: ImportOutcomeProps) {
  const { tone, title } = statusOf(operation);
  const running = operation.state === "queued" || operation.state === "running";
  const target = `${formatPathForDisplay(libraryRoot)}${collection.trim() ? ` / ${collection.trim()}` : ""}`;
  const hasImported = Boolean(result && result.imported > 0);

  return (
    <section
      aria-label="本次入库结果"
      data-state={operation.state}
      className="flex min-w-0 flex-col gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-4 py-3"
    >
      <div className="flex items-start gap-3">
        <StatusIcon tone={tone} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h2 className="text-sm font-medium text-[var(--color-text)]">{title}</h2>
            <span className="text-[11px] text-[var(--color-text-tertiary)]">
              已处理 {operation.completed}/{operation.total ?? "?"} · 失败 {operation.failed}
            </span>
          </div>
          <p className="mt-0.5 text-[11px] leading-5 text-[var(--color-text-secondary)]">{subtitleOf(operation)}</p>

          {result && (
            <p className="mt-2 text-xs text-[var(--color-text-secondary)]">
              {`新入库 ${result.imported} · 重复 ${result.duplicates} · 失败 ${result.failed}`}
            </p>
          )}
          {result && hasImported && (
            <p className="mt-1.5 break-all text-[11px] text-[var(--color-text-tertiary)]" title={target}>
              入库位置：{target}
            </p>
          )}
          {!result && !running && (
            <p className="mt-2 text-[11px] text-[var(--color-text-tertiary)]">
              {resultLoading ? "正在读取结果…" : "结果明细尚未返回。"}
            </p>
          )}
          {resultError && (
            <p role="alert" className="mt-2 flex flex-wrap items-center gap-2 text-[11px] text-[var(--color-status)]">
              结果明细读取失败：{resultError}
              <button
                type="button"
                disabled={resultLoading}
                onClick={onRetryResult}
                className="underline underline-offset-2 disabled:opacity-40"
              >
                {resultLoading ? "正在重试…" : "重试读取结果"}
              </button>
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {operation.canResume && (
            <button
              type="button"
              onClick={onResume}
              className="flex items-center gap-1 rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
            >
              <RotateCcw size={13} aria-hidden="true" />
              继续入库
            </button>
          )}
          {/* 运行中的取消入口归左栏「入库进度」，这里只管终态后仍可取消的情况，避免同一动作出现两次。 */}
          {!running && operation.canCancel && (
            <button
              type="button"
              onClick={onCancel}
              className="flex items-center gap-1 rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-danger)]"
            >
              取消入库
            </button>
          )}
          {!running && hasImported && (
            <button
              type="button"
              onClick={onOpenLibrary}
              className="flex items-center gap-1 rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
            >
              <FolderOpen size={13} aria-hidden="true" />
              打开素材库
            </button>
          )}
          {!running && onHandoff && (
            <button
              type="button"
              onClick={onHandoff}
              className="rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
            >
              交给助手
            </button>
          )}
        </div>
      </div>

      {result && result.duplicateItems.length > 0 && (
        <details className="ml-8 text-[11px] text-[var(--color-text-secondary)]" data-testid="import-duplicates">
          <summary className="cursor-pointer select-none">查看重复项（{result.duplicateItems.length}）</summary>
          <ul className="mt-1 max-h-32 space-y-0.5 overflow-y-auto">
            {result.duplicateItems.map((item) => (
              <li key={item.itemRef} className="break-all">{item.displayName}</li>
            ))}
          </ul>
        </details>
      )}

      {result && result.failures.length > 0 && (
        <details className="ml-8 text-[11px] text-[var(--color-status)]">
          <summary className="cursor-pointer select-none">查看异常明细（{result.failures.length}）</summary>
          <ul className="mt-1 max-h-32 space-y-0.5 overflow-y-auto">
            {result.failures.map((failure) => (
              <li key={failure.itemRef} className="break-all">
                {failure.displayName} · {failure.state === "outcome_unknown" ? "结果未知" : "失败"}
                {failure.errorCode ? ` · ${failure.errorCode}` : ""}
              </li>
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}
