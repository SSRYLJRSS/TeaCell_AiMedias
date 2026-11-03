/** 统一浮层定位原语（WF-1 P0-4）。
 *
 * 解决的问题：权限选择器曾写死向上（bottom-full）、模型选择器写死向下（top-full），
 * 同一输入区两种方向，看起来像两套东西。这里把「朝哪边弹」交给空间决定：
 * 下方够放就向下，否则向上；水平方向贴边时自动回收到视口内。
 *
 * WP2：新增 `preferSide`。输入区贴屏幕底部，调用方通常希望**优先向上**（preferSide="top"）：
 * 上方空间 >=160px 时向上并夹住 maxHeight（min(maxHeight, spaceAbove-8)），
 * 否则回落向下，避免出现悬空/越界的小条。
 *
 * 只负责定位，不接管键盘/焦点/外部点击——那些仍由调用方各自持有，
 * 以免改动既有 role/aria 语义与测试契约。浮层挂在调用方的 relative 包裹层内，
 * 因此 `rootRef.contains(target)` 一类外部点击判定依旧成立。
 */
import { useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import clsx from "clsx";

export interface PickerPopoverProps {
  open: boolean;
  /** 触发器元素：用于测量方向与回收水平位置。 */
  triggerRef: RefObject<HTMLElement | null>;
  children: ReactNode;
  /** 期望宽度（px）；最终会被视口宽度夹住。 */
  width?: number;
  /** 最大高度（px），超出内部滚动。 */
  maxHeight?: number;
  /** 优先方向：默认 auto-flip；top 优先向上（空间不足回落 bottom）。 */
  preferSide?: "top" | "bottom";
  className?: string;
  /** 供调用方持有面板 ref（如外部点击判定里的 menuRef）。 */
  panelRef?: RefObject<HTMLDivElement | null>;
}

export default function PickerPopover({
  open,
  triggerRef,
  children,
  width = 280,
  maxHeight = 320,
  preferSide,
  className,
  panelRef,
}: PickerPopoverProps) {
  const innerRef = useRef<HTMLDivElement | null>(null);
  const [placement, setPlacement] = useState<{
    side: "top" | "bottom";
    offsetX: number;
    maxHeight: number;
  }>({ side: "bottom", offsetX: 0, maxHeight });

  useLayoutEffect(() => {
    if (!open) return;
    const trigger = triggerRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const viewportW = window.innerWidth || 1024;
    const viewportH = window.innerHeight || 768;
    const spaceBelow = viewportH - rect.bottom - 8;
    const spaceAbove = rect.top - 8;
    const need = Math.min(maxHeight, 280);

    let side: "top" | "bottom";
    if (preferSide === "top") {
      // 输入区贴近屏幕底部：优先向上。上方明显不够时回落向下。
      side = spaceAbove >= 160 ? "top" : "bottom";
    } else if (preferSide === "bottom") {
      side = "bottom";
    } else {
      side = spaceBelow >= need || spaceBelow >= spaceAbove ? "bottom" : "top";
    }
    // 无论向上还是向下，都按该侧可用空间夹住高度，避免任一侧溢出窗口。
    const available = (side === "top" ? spaceAbove : spaceBelow) - 8;
    const resolvedMax = Math.max(120, Math.min(maxHeight, available));

    const w = Math.min(width, viewportW - 16);
    let offsetX = 0;
    if (rect.left + w > viewportW - 8) offsetX = viewportW - 8 - w - rect.left;
    if (rect.left + offsetX < 8) offsetX = 8 - rect.left;
    setPlacement({ side, offsetX, maxHeight: resolvedMax });
  }, [open, maxHeight, width, preferSide, triggerRef]);

  if (!open) return null;

  const style: CSSProperties = {
    width: Math.min(width, 560),
    maxHeight: placement.maxHeight,
    maxWidth: "calc(100vw - 16px)",
    left: placement.offsetX,
    ...(placement.side === "bottom" ? { top: "calc(100% + 6px)" } : { bottom: "calc(100% + 6px)" }),
  };

  return (
    <div
      ref={(node) => {
        innerRef.current = node;
        if (panelRef) panelRef.current = node;
      }}
      data-side={placement.side}
      className={clsx("picker-popover", className)}
      style={style}
    >
      {children}
    </div>
  );
}
