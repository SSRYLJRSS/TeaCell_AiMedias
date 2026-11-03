/** 统一选择器触发器（WP2）。
 *
 * 三个底部胶囊（插入技能 / 权限 / 模型）共用同一外壳：图标 + 主文本 + 统一的
 * **向下箭头**，高度/圆角沿用 `.cmp-pill`（26px 胶囊，见 docs/UI_DESIGN_SYSTEM.md §4）。
 * 箭头方向由组件固定，不再受原生 select 的操作系统渲染影响——这正是用户抱怨
 * 「一个向上一个向下」的根因。
 *
 * 只负责外观与 aria 开关；开合状态、键盘、外部点击仍由各选择器自己持有。
 */
import { forwardRef, type ReactNode } from "react";
import clsx from "clsx";
import { ChevronDown } from "lucide-react";

export interface PickerTriggerProps {
  /** 左侧图标（12–13px 语义图标）。 */
  icon: ReactNode;
  /** 收起态可见文本（会被 truncate）。 */
  label: ReactNode;
  /** 无障碍名字（可与可见文本不同，例如「会话权限：可执行改动」）。 */
  ariaLabel: string;
  title?: string;
  /** 是否展开，仅用于 aria-expanded。 */
  open?: boolean;
  /** 有弹层时设 true（默认 menu），没有则 false。 */
  haspopup?: "menu" | "listbox" | false;
  disabled?: boolean;
  onClick: () => void;
  className?: string;
  labelClassName?: string;
  testId?: string;
}

const PickerTrigger = forwardRef<HTMLButtonElement, PickerTriggerProps>(function PickerTrigger(
  {
    icon,
    label,
    ariaLabel,
    title,
    open = false,
    haspopup = "menu",
    disabled = false,
    onClick,
    className,
    labelClassName,
    testId,
  },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={ariaLabel}
      title={title}
      aria-haspopup={haspopup || undefined}
      aria-expanded={haspopup ? open : undefined}
      disabled={disabled}
      onClick={onClick}
      data-testid={testId}
      className={clsx(
        "cmp-pill min-w-0 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]",
        className,
      )}
    >
      <span className="flex shrink-0 items-center justify-center" aria-hidden="true">
        {icon}
      </span>
      <span className={clsx("truncate", labelClassName)}>{label}</span>
      <ChevronDown size={11} aria-hidden="true" className="shrink-0" />
    </button>
  );
});

export default PickerTrigger;
