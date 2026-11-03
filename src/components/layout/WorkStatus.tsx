/**
 * 左上角小型工作状态区（PRD 第 8 节 / A3-05）。
 *
 * 行为事实：
 *  - 状态只来自 `taskStore` 的真实任务投影（入库/导出/操作快照），不靠计时器模拟进度；
 *  - 多条时缓慢轮播，鼠标悬停或键盘聚焦即暂停；
 *  - 支持手动滚动（滚轮 / 方向键 / PageUp·PageDown）切换，不需先点开面板；
 *  - 尊重 `prefers-reduced-motion`：减少动画时默认不自动轮播，但仍可手动切换。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { useTaskStore } from "@/stores/taskStore";
import type { TaskItem } from "@/stores/taskStore";

/** 轮播间隔：足够慢，避免抢注意力（PRD「缓慢自动轮播」）。 */
export const WORK_STATUS_ROTATE_MS = 6000;
/** 手动滚动后暂停自动轮播的时长，避免刚看到就被切走。 */
export const WORK_STATUS_MANUAL_HOLD_MS = 8000;

export type WorkStatusTone = "running" | "done" | "failed";

export interface WorkStatusEntry {
  id: string;
  label: string;
  /** 状态短文案：进行中 / 已完成 / 未完成 / 失败 */
  state: string;
  tone: WorkStatusTone;
  detail?: string;
}

const TONE_LABEL: Record<WorkStatusTone, string> = {
  running: "进行中",
  done: "已完成",
  failed: "未完成",
};

/** 把任务列表投影成状态条目：进行中 → 未完成 → 已完成，各段内保持原有顺序。 */
export function projectWorkStatus(tasks: TaskItem[]): WorkStatusEntry[] {
  const running: WorkStatusEntry[] = [];
  const failed: WorkStatusEntry[] = [];
  const done: WorkStatusEntry[] = [];
  for (const task of tasks) {
    const tone: WorkStatusTone = task.error ? "failed" : task.done ? "done" : "running";
    const entry: WorkStatusEntry = {
      id: task.id,
      label: task.label,
      state: TONE_LABEL[tone],
      tone,
      detail: task.detail ?? task.error ?? undefined,
    };
    if (tone === "running") running.push(entry);
    else if (tone === "failed") failed.push(entry);
    else done.push(entry);
  }
  return [...running, ...failed, ...done];
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}

/**
 * 把轮播游标夹回合法范围。
 *
 * 任务列表可能在两次渲染之间缩短（另一条任务完成被移除），此时 state 里的游标
 * 仍指向已消失的条目。渲染期必须夹取，「当前条目」和「序号」都走这一个函数，
 * 否则会出现“显示第 2 条却写着 3/2”这种自相矛盾的画面。
 */
export function clampWorkStatusIndex(index: number, length: number): number {
  if (length <= 0) return 0;
  if (!Number.isFinite(index) || index < 0) return 0;
  return Math.min(Math.trunc(index), length - 1);
}

export default function WorkStatus() {
  const tasks = useTaskStore((s) => s.tasks);
  const entries = useMemo(() => projectWorkStatus(tasks), [tasks]);
  const [index, setIndex] = useState(0);
  const [paused, setPaused] = useState(false);
  const holdUntil = useRef(0);
  const reducedMotion = useMemo(prefersReducedMotion, []);

  // 列表缩短时把游标夹回合法范围，避免指向已消失的任务
  useEffect(() => {
    setIndex((i) => clampWorkStatusIndex(i, entries.length));
  }, [entries.length]);

  const step = useCallback(
    (delta: number) => {
      if (entries.length <= 1) return;
      setIndex((i) => ((i + delta) % entries.length + entries.length) % entries.length);
      holdUntil.current = Date.now() + WORK_STATUS_MANUAL_HOLD_MS;
    },
    [entries.length],
  );

  // 缓慢轮播：暂停、只有一条、或减少动画时不启动计时器
  useEffect(() => {
    if (paused || reducedMotion || entries.length <= 1) return;
    const timer = setInterval(() => {
      // 手动滚动后的保持期内不自动切换
      if (Date.now() < holdUntil.current) return;
      setIndex((i) => (i + 1) % entries.length);
    }, WORK_STATUS_ROTATE_MS);
    return () => clearInterval(timer);
  }, [paused, reducedMotion, entries.length]);

  if (entries.length === 0) {
    return (
      <div
        className="flex min-w-0 items-center gap-1.5 text-xs text-[var(--color-text-tertiary)]"
        data-testid="work-status"
      >
        <span className="size-1.5 shrink-0 rounded-full bg-[var(--color-border-strong)]" aria-hidden="true" />
        <span className="truncate">暂无进行中的任务</span>
      </div>
    );
  }

  // 任务列表可能在两次渲染之间缩短（例如另一条任务完成被移除），
  // 渲染时先夹取，避免出现 “3/2” 这种越界序号。
  const currentIndex = clampWorkStatusIndex(index, entries.length);
  const current = entries[currentIndex];
  const toneColor =
    current.tone === "failed"
      ? "text-[var(--color-danger)]"
      : current.tone === "done"
        ? "text-[var(--color-text-tertiary)]"
        : "text-[var(--color-text-secondary)]";

  return (
    <div
      data-testid="work-status"
      role="status"
      aria-live="off"
      aria-label="工作状态"
      tabIndex={0}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocus={() => setPaused(true)}
      onBlur={() => setPaused(false)}
      onWheel={(event) => {
        if (entries.length <= 1) return;
        step(event.deltaY >= 0 ? 1 : -1);
      }}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown" || event.key === "ArrowRight" || event.key === "PageDown") {
          event.preventDefault();
          step(1);
        } else if (event.key === "ArrowUp" || event.key === "ArrowLeft" || event.key === "PageUp") {
          event.preventDefault();
          step(-1);
        }
      }}
      className="flex min-w-0 max-w-[min(360px,42vw)] items-center gap-1.5 rounded-[var(--radius-item)] px-1.5 py-1 text-xs transition-colors hover:bg-[var(--color-surface)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[var(--color-border-strong)]"
    >
      <span
        aria-hidden="true"
        className={clsx(
          "size-1.5 shrink-0 rounded-full",
          current.tone === "failed"
            ? "bg-[var(--color-danger)]"
            : current.tone === "done"
              ? "bg-[var(--color-border-strong)]"
              : "animate-pulse bg-[var(--color-status)]",
        )}
      />
      <span className="shrink-0 text-[var(--color-text-tertiary)]">{current.state}</span>
      <span className={clsx("min-w-0 truncate", toneColor)} title={current.detail ?? current.label}>
        {current.label}
      </span>
      {entries.length > 1 && (
        <span className="shrink-0 text-[10px] tabular-nums text-[var(--color-text-tertiary)]">
          {currentIndex + 1}/{entries.length}
        </span>
      )}
    </div>
  );
}
