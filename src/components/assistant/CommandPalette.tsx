/** `/` 命令面板（WP2）。
 *
 * 在输入框**上方**弹出，样式与计划选择器一致（深色圆角、行=图标+命令名+右侧灰描述）。
 * 触发、过滤、键盘（↑↓Enter Esc Tab）都在 Composer 里完成；这里只负责把结果画出来，
 * 因此是一个纯展示组件，便于测试和复用。
 */
import { BookOpen } from "lucide-react";
import PickerPopover from "@/components/common/PickerPopover";
import PickerMenuRow from "@/components/common/PickerMenuRow";
import type { PaletteItem } from "@/components/assistant/skills/commands";
import type { RefObject } from "react";

/** 面板只列技能，统一用技能图标。 */
function iconFor() {
  return <BookOpen size={12} />;
}

export default function CommandPalette({
  open,
  items,
  activeIndex,
  anchorRef,
  onPick,
  onHover,
}: {
  open: boolean;
  items: PaletteItem[];
  activeIndex: number;
  anchorRef: RefObject<HTMLElement | null>;
  onPick: (item: PaletteItem) => void;
  onHover?: (index: number) => void;
}) {
  return (
    <PickerPopover open={open} triggerRef={anchorRef} width={440} maxHeight={320} preferSide="top">
      <div role="menu" aria-label="命令面板" data-testid="command-palette">
        {items.length === 0 ? (
          <div className="px-2 py-3 text-center text-[11px] text-[var(--color-text-tertiary)]">
            没有匹配的命令
          </div>
        ) : (
          items.map((item, index) => (
            <PickerMenuRow
              key={item.id}
              icon={iconFor()}
              title={
                <span className="flex items-center gap-1">
                  <span className="text-[var(--color-text-tertiary)]">/</span>
                  {item.title}
                </span>
              }
              description={item.description}
              disabled={item.disabled}
              className={index === activeIndex ? "bg-[var(--color-surface-hover)]" : undefined}
              onSelect={() => onPick(item)}
              {...(onHover ? { onMouseEnter: () => onHover(index) } : {})}
            />
          ))
        )}
      </div>
    </PickerPopover>
  );
}
