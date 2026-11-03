/** 统一菜单行（WP2）。
 *
 * 参考 DeepSeek Harness 的菜单行：左图标 + 主文本 + 右侧灰色描述 + 末尾选中勾。
 * 刻意保持**无状态**：不抽状态机/键盘/定位，只把一行画对，方便权限、技能、
 * 模型、`/` 命令面板复用同一视觉语言。开合、焦点、外部点击仍由调用方持有，
 * 以免改动既有 role/aria 测试契约。
 */
import clsx from "clsx";
import { Check } from "lucide-react";
import type { ReactNode } from "react";

export interface PickerMenuRowProps {
  icon?: ReactNode;
  title: ReactNode;
  /** 右侧灰色描述（服务名、技能何时用等）。 */
  description?: ReactNode;
  selected?: boolean;
  disabled?: boolean;
  /** 单选组用 menuitemradio（带 aria-checked），普通动作菜单用 menuitem。 */
  role?: "menuitem" | "menuitemradio";
  /** 原生 title 提示（例如停用原因）。 */
  titleAttr?: string;
  onSelect?: () => void;
  onMouseEnter?: () => void;
  className?: string;
}

export default function PickerMenuRow({
  icon,
  title,
  description,
  selected = false,
  disabled = false,
  role = "menuitem",
  titleAttr,
  onSelect,
  onMouseEnter,
  className,
}: PickerMenuRowProps) {
  const isRadio = role === "menuitemradio";
  return (
    <button
      type="button"
      role={role}
      aria-checked={isRadio ? selected : undefined}
      title={titleAttr}
      disabled={disabled}
      onClick={onSelect}
      onMouseEnter={onMouseEnter}
      className={clsx(
        "flex w-full items-center gap-2 rounded-[var(--radius-item)] px-2 py-1.5 text-left transition-colors",
        disabled
          ? "cursor-not-allowed opacity-50"
          : "hover:bg-[var(--color-surface)]",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]",
        className,
      )}
    >
      {icon ? (
        <span className="flex size-4 shrink-0 items-center justify-center text-[var(--color-text-tertiary)]" aria-hidden="true">
          {icon}
        </span>
      ) : null}
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[12px] text-[var(--color-text)]">{title}</span>
        {description ? (
          <span className="block truncate text-[11px] leading-4 text-[var(--color-text-tertiary)]">
            {description}
          </span>
        ) : null}
      </span>
      {selected ? (
        <Check size={12} aria-hidden="true" className="shrink-0 text-[var(--color-accent)]" />
      ) : (
        <span className="size-3 shrink-0" aria-hidden="true" />
      )}
    </button>
  );
}
