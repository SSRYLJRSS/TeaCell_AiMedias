import { useEffect, useRef, useState } from "react";
import clsx from "clsx";
import ProgressBar from "@/components/common/ProgressBar";
import { useTaskStore } from "@/stores/taskStore";
import { useDoubleAction } from "@/hooks/useDoubleAction";
import type { TaskItem } from "@/stores/taskStore";

export type TabKey = "import" | "library" | "ai";

const TABS: { key: TabKey; label: string }[] = [
  { key: "import", label: "入库" },
  { key: "library", label: "素材库" },
  { key: "ai", label: "打标" },
];

const SUPER_SEARCH_HINT_DELAY_MS = 500;

interface BottomBarProps {
  current: TabKey | "settings" | "superSearch";
  onNavigate: (tab: TabKey) => void;
  onOpenSuperSearch?: () => void;
}

/** 达芬奇式底栏：3 个纯文字按钮、整体居中、无图标无副标题（PRD R-13）
 *  全局任务层只展示导出任务。入库与 AI 打标各自由对应页面唯一展示进度，避免切页后出现残留进度条；
 *  taskStore 负责订阅事件，本组件只读渲染。
 *  P2.3：素材库按钮支持双击进入超级搜索（单击延时导航，双击取消） */
export default function BottomBar({ current, onNavigate, onOpenSuperSearch }: BottomBarProps) {
  const tasks = useTaskStore((s) => s.tasks);
  const [showSuperSearchHint, setShowSuperSearchHint] = useState(false);
  const hintTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const visibleTasks = tasks.filter((task) => task.kind !== "import");

  // 当前页为超级搜索时，底栏仍高亮「素材库」；其余按原映射
  const activeTab = (key: TabKey): boolean =>
    current === key || (key === "library" && current === "superSearch");

  // 素材库按钮的单击/双击互斥
  const doubleAction = useDoubleAction(
    () => onNavigate("library"),
    () => onOpenSuperSearch?.(),
  );

  const clickProps = (tab: TabKey) => (tab === "library" ? doubleAction : {});
  const clearHintTimer = () => {
    if (hintTimer.current) clearTimeout(hintTimer.current);
    hintTimer.current = null;
  };
  const openHintAfterDelay = () => {
    clearHintTimer();
    hintTimer.current = setTimeout(() => {
      hintTimer.current = null;
      setShowSuperSearchHint(true);
    }, SUPER_SEARCH_HINT_DELAY_MS);
  };
  const closeHint = () => {
    clearHintTimer();
    setShowSuperSearchHint(false);
  };
  useEffect(() => () => clearHintTimer(), []);

  return (
    <>
      {/* 全局任务层：只由 taskStore 驱动，位于底栏上方（距底栏上边缘 8px），不遮导航 */}
      {visibleTasks.length > 0 && (
        <div className="fixed inset-x-0 bottom-14 z-20 flex flex-col items-center gap-1 px-4 pb-2">
          {visibleTasks.map((t) => (
            <TaskRow key={t.id} task={t} />
          ))}
        </div>
      )}

      <nav className="fixed inset-x-0 bottom-0 z-30 flex h-14 items-center justify-center gap-10 border-t border-[var(--color-border)] bg-[var(--color-bg)]/96 backdrop-blur">
      {TABS.map((tab) => {
        const isLibrary = tab.key === "library";
        return (
          <button
            key={tab.key}
            onClick={() => onNavigate(tab.key)}
            {...clickProps(tab.key)}
            onMouseEnter={isLibrary ? openHintAfterDelay : undefined}
            onMouseLeave={isLibrary ? closeHint : undefined}
            onFocus={isLibrary ? openHintAfterDelay : undefined}
            onBlur={isLibrary ? closeHint : undefined}
            aria-label={isLibrary ? tab.label : undefined}
            aria-describedby={isLibrary && showSuperSearchHint ? "super-search-entry-hint" : undefined}
            className={clsx(
              "relative px-2 py-1 text-sm tracking-[0.18em] transition-colors",
              activeTab(tab.key)
                ? "font-medium text-[var(--color-text)] after:absolute after:inset-x-2 after:-bottom-1 after:h-0.5 after:rounded-full after:bg-[var(--color-status)]"
                : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
            )}
          >
            {tab.label}
            {isLibrary && showSuperSearchHint && (
              <span
                id="super-search-entry-hint"
                role="tooltip"
                className="pointer-events-none absolute bottom-full left-1/2 mb-2 -translate-x-1/2 whitespace-nowrap rounded-[var(--radius-item)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-2 py-1 text-[11px] font-normal tracking-normal text-[var(--color-text-secondary)] shadow-[var(--shadow-soft)]"
              >
                双击进入超级搜索
              </span>
            )}
          </button>
        );
      })}
      </nav>
    </>
  );
}

/** 单条任务摘要：标签 + 整体进度 + 明细/失败文案；不确定进度（overall=null）显示不定进度条。
 *  失败不用纯红背景，用文字 + 状态符号表达。 */
function TaskRow({ task }: { task: TaskItem }) {
  const indeterminate = task.overall == null && !task.done;
  const failed = Boolean(task.error);
  return (
    <div className="w-[min(560px,90vw)] rounded-md border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-3 py-1.5 shadow-sm">
      <div className="flex items-center gap-2">
        <span
          className={clsx(
            "shrink-0 text-xs font-medium",
            failed ? "text-[var(--color-status)]" : "text-[var(--color-text)]",
          )}
          aria-live="polite"
        >
          {failed ? "⚠" : ""}
          {task.label}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-[var(--color-text-secondary)]" title={task.detail}>
          {task.detail}
        </span>
      </div>
      {indeterminate ? (
        <div className="mt-1 h-1 w-full overflow-hidden rounded-full bg-[var(--color-border)]">
          <div className="h-full w-full animate-pulse bg-[var(--color-status)]" />
        </div>
      ) : (
        <ProgressBar value={task.overall ?? 0} className="mt-1 h-1" />
      )}
      {failed && <p className="mt-1 truncate text-[10px] text-[var(--color-status)]">{task.error}</p>}
    </div>
  );
}
