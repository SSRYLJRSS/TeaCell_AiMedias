/** 助手右侧栏（常用流程 / 待审计划 / 工作台共用的抽屉外壳）。
 *
 * 为什么改成一个抽屉：头部三个图标原本两个开模态框、一个开侧栏，同一块内容出现两种
 * 容器语义——模态框会盖住对话、和「边看对话边改流程」的用法冲突。现在三个入口都打开
 * 这一个侧栏，只是把选项卡切到对应位置，侧栏内部也能自己切换。
 *
 * 边界：
 * - 外壳只管宽度、选项卡切换和关闭；内容仍由各自组件渲染（`WorkflowPanel` /
 *   `PendingPlansPanel` 用 `embedded`，工作台保持原样），不复制它们的领域状态。
 * - 宽度沿用工作台原有口径：380px，加宽 620px。
 * - 工作台自带「加宽 / 收起」按钮，所以它在前台时外壳不再渲染这两个按钮，
 *   避免同一屏出现两个同名控件（加宽状态仍由外壳持有，两边共用同一个值）。
 */
import type { ReactNode } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";

export type AssistantSidePanelTab = "workflows" | "skills" | "plans" | "workbench";

const TABS: ReadonlyArray<{ value: AssistantSidePanelTab; label: string }> = [
  { value: "workflows", label: "常用流程" },
  { value: "skills", label: "技能库" },
  { value: "plans", label: "待审计划" },
  { value: "workbench", label: "工作台" },
];

const CONTENT_ID = "assistant-side-panel-content";

interface AssistantSidePanelProps {
  tab: AssistantSidePanelTab;
  /** 加宽态：380px → 620px，和工作台原来的两档宽度一致。 */
  wide: boolean;
  onTabChange: (tab: AssistantSidePanelTab) => void;
  onToggleWide: () => void;
  onClose: () => void;
  children: ReactNode;
}

export default function AssistantSidePanel({
  tab,
  wide,
  onTabChange,
  onToggleWide,
  onClose,
  children,
}: AssistantSidePanelProps) {
  const current = TABS.find((item) => item.value === tab) ?? TABS[0];
  /** 工作台自己带加宽/收起按钮，这里让位，保证「收起」只有一个。 */
  const ownsControls = tab !== "workbench";

  return (
    <aside
      aria-label="助手侧栏"
      className={clsx(
        // z-40：窄窗的历史浮层是 absolute z-30，侧栏必须压在它上面，否则点侧栏会先
        // 命中浮层遮罩、把两个都关掉。
        "relative z-40 flex shrink-0 flex-col bg-[var(--color-bg)]",
        wide ? "w-[620px]" : "w-[380px]",
        // 工作台自带左边框；其余选项卡由外壳提供，避免左沿出现两条 1px 边线。
        ownsControls && "border-l border-[var(--color-border)]",
      )}
    >
      <header className="flex items-center gap-1 border-b border-[var(--color-border)] px-3 py-2">
        <div
          role="tablist"
          aria-label="助手侧栏选项卡"
          className="flex min-w-0 flex-1 items-center gap-1"
        >
          {TABS.map((item) => (
            <button
              key={item.value}
              type="button"
              role="tab"
              id={`assistant-side-panel-tab-${item.value}`}
              aria-selected={tab === item.value}
              aria-controls={CONTENT_ID}
              onClick={() => onTabChange(item.value)}
              className={clsx(
                "rounded-[var(--radius-item)] px-2 py-1 text-xs transition-colors",
                tab === item.value
                  ? "bg-[var(--color-surface)] font-medium text-[var(--color-text)]"
                  : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
              )}
            >
              {item.label}
            </button>
          ))}
        </div>
        {ownsControls && (
          <>
            <Button
              className="min-h-0 px-2 py-0.5 text-[11px]"
              onClick={onToggleWide}
              title="加宽侧栏"
            >
              {wide ? "收窄" : "加宽"}
            </Button>
            <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={onClose}>
              收起
            </Button>
          </>
        )}
      </header>

      <div
        id={CONTENT_ID}
        role="tabpanel"
        aria-label={current.label}
        className="flex min-h-0 flex-1 flex-col overflow-hidden"
      >
        {children}
      </div>
    </aside>
  );
}
