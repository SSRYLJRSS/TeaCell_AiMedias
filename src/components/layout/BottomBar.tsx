import { useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import ProgressBar from "@/components/common/ProgressBar";
import assistantLogo from "@/assets/icon-logo.png";
import { useTaskStore } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useDoubleAction } from "@/hooks/useDoubleAction";
import { defaultNavigationShortcuts } from "@/utils/normalizeSettings";
import type { TaskItem } from "@/stores/taskStore";

/** 底栏可承载的内置目的地（PRD 第 8 节受控目录，不接受任意 URL/命令）。 */
export type TabKey = "import" | "library" | "ai" | "treasure";

/** 「设置」是系统入口而非快捷项：只能由 `navigation.settingsPinned` 固定到底栏。 */
export type BottomBarTarget = TabKey | "settings";

/** 受控内置功能目录：id → 文案。缺项即「不认识」，渲染时直接跳过。 */
const SHORTCUT_CATALOG: Record<TabKey, string> = {
  import: "入库",
  library: "素材库",
  ai: "打标",
  treasure: "百宝箱",
};

const SUPER_SEARCH_HINT_DELAY_MS = 500;
const SETTINGS_LABEL = "设置";

function isKnownShortcut(id: string): id is TabKey {
  return Object.prototype.hasOwnProperty.call(SHORTCUT_CATALOG, id);
}

interface BottomBarProps {
  current: TabKey | "settings" | "superSearch" | "assistant";
  onNavigate: (target: BottomBarTarget) => void;
  onOpenSuperSearch?: () => void;
  /** 中央 Logo 是助手入口（PRD 第 8 节）：首次进入新建对话，有历史恢复最近对话。 */
  onOpenAssistant?: () => void;
}

interface BarEntry {
  key: BottomBarTarget;
  label: string;
}

/** 达芬奇式底栏：纯文字按钮、整体居中、无图标无副标题（PRD R-13）。
 *  A3-05：入口来自 `settings.navigation.shortcuts`（受控内置目录，用户可增删/隐藏/排序），
 *  「设置」仅在 `settingsPinned` 时出现；中央 Logo 用三列栅格保持窗口正中央，
 *  与左右入口数量无关。未知旧 id 一律跳过，不会渲染成死按钮，也不会清掉功能或数据。
 *  M3-04：左上全局任务层（入库/导出进度聚合）。FB6 需求一：AI 打标不再进全局任务条，
 *  页内进度由 AiTaggingPage 唯一承担；taskStore 负责订阅事件，本组件只读渲染。
 *  P2.3：素材库按钮支持双击进入超级搜索（单击延时导航，双击取消） */
export default function BottomBar({
  current,
  onNavigate,
  onOpenSuperSearch,
  onOpenAssistant,
}: BottomBarProps) {
  const tasks = useTaskStore((s) => s.tasks);
  const navigation = useSettingsStore((s) => s.settings?.navigation ?? null);
  const [showSuperSearchHint, setShowSuperSearchHint] = useState(false);
  const hintTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const visibleTasks = tasks.filter((task) => {
    if (task.kind !== "import") return true;
    if (current === "import") return false;
    return !task.done;
  });

  // 设置未加载时用默认目录兜底；加载后以落库顺序/可见性为准。
  const entries = useMemo<BarEntry[]>(() => {
    const shortcuts = navigation?.shortcuts ?? defaultNavigationShortcuts();
    const visible: BarEntry[] = [];
    for (const shortcut of shortcuts) {
      if (!shortcut.visible) continue;
      if (!isKnownShortcut(shortcut.id)) continue;
      visible.push({ key: shortcut.id, label: SHORTCUT_CATALOG[shortcut.id] });
    }
    if (navigation?.settingsPinned) visible.push({ key: "settings", label: SETTINGS_LABEL });
    return visible;
  }, [navigation]);

  // 中央 Logo 固定窗口正中央：入口按数量对半分到左右两列，Logo 独占中间列。
  const splitAt = Math.ceil(entries.length / 2);
  const leftEntries = entries.slice(0, splitAt);
  const rightEntries = entries.slice(splitAt);

  // 当前页为超级搜索时，底栏仍高亮「素材库」；其余按原映射
  const activeTab = (key: BottomBarTarget): boolean =>
    current === key || (key === "library" && current === "superSearch");

  // 素材库按钮的单击/双击互斥
  const doubleAction = useDoubleAction(
    () => onNavigate("library"),
    () => onOpenSuperSearch?.(),
  );

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

      <nav
        aria-label="主导航"
        data-bottom-bar=""
        className="fixed inset-x-0 bottom-0 z-30 grid h-14 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center border-t border-[var(--color-border)] bg-[var(--color-bg)]/96 px-4 backdrop-blur"
      >
        <div className="flex min-w-0 items-center justify-end gap-4 sm:gap-8">
          {leftEntries.map((entry) => renderEntry(entry))}
        </div>

        {/* 中央 Logo 固定在窗口正中央，作为助手入口（PRD 第 8 节）。
            A3-05 实机反馈：36px 圆 + 24px 图比两侧文字还小，完全不像主入口；
            这里放大到 48px 圆 + 40px 图，仍保持在 56px 底栏内不越界。 */}
        <button
          type="button"
          onClick={() => onOpenAssistant?.()}
          aria-label="助手"
          aria-current={current === "assistant" ? "page" : undefined}
          className={clsx(
            "relative mx-2 flex size-12 shrink-0 items-center justify-center rounded-full transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]",
            current === "assistant"
              ? "bg-[var(--color-surface-hover)] ring-1 ring-[var(--color-border-strong)]"
              : "hover:bg-[var(--color-surface-hover)]",
          )}
        >
          <img src={assistantLogo} alt="" aria-hidden="true" className="size-10 rounded-full object-contain" />
        </button>

        <div className="flex min-w-0 items-center justify-start gap-4 sm:gap-8">
          {rightEntries.map((entry) => renderEntry(entry))}
        </div>
      </nav>
    </>
  );

  function renderEntry(entry: BarEntry) {
    const isLibrary = entry.key === "library";
    const clickProps = isLibrary ? doubleAction : {};
    return (
      <button
        key={entry.key}
        type="button"
        onClick={() => onNavigate(entry.key)}
        {...clickProps}
        onMouseEnter={isLibrary ? openHintAfterDelay : undefined}
        onMouseLeave={isLibrary ? closeHint : undefined}
        onFocus={isLibrary ? openHintAfterDelay : undefined}
        onBlur={isLibrary ? closeHint : undefined}
        aria-label={entry.label}
        aria-current={current === entry.key ? "page" : undefined}
        aria-describedby={isLibrary && showSuperSearchHint ? "super-search-entry-hint" : undefined}
        className={clsx(
          "relative rounded-[var(--radius-item)] px-2 py-1 text-sm tracking-[0.18em] transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]",
          activeTab(entry.key)
            ? "font-medium text-[var(--color-text)] after:absolute after:inset-x-2 after:-bottom-1 after:h-0.5 after:rounded-full after:bg-[var(--color-status)]"
            : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
        )}
      >
        {entry.label}
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
  }
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
            failed ? "text-[var(--color-danger)]" : "text-[var(--color-text)]",
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
      {failed && <p className="mt-1 truncate text-[10px] text-[var(--color-danger)]">{task.error}</p>}
    </div>
  );
}
