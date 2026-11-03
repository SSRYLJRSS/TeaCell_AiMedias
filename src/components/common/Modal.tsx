import { useEffect, useRef } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";

interface ModalProps {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** 宽版（预览等场景） */
  wide?: boolean;
}

/** 可在弹窗内参与 Tab 循环的元素选择器（与浏览器默认可聚焦行为一致的子集）。 */
const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[role="checkbox"]:not([aria-disabled="true"])',
  '[tabindex]:not([tabindex="-1"]):not([disabled])',
].join(",");

/** 收集弹窗内可聚焦元素。
 *
 *  不用 `offsetParent` / `getClientRects()` 判可见性：jsdom 不做布局计算，两者
 *  恒为空，会让 Tab 循环在单元测试里失效（真机上也会把 `position: fixed` 后代
 *  误判成不可见）。改用不影响布局的标记：`hidden` 属性与 `aria-hidden`。
 */
function focusableIn(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) =>
      !element.hidden &&
      element.closest("[hidden]") === null &&
      element.closest('[aria-hidden="true"]') === null,
  );
}

/** 基础模态：遮罩点击/Esc 关闭（T04 弹窗统一基于它）
 *  Esc 走 window 捕获阶段监听：不依赖弹窗内部焦点，打开期间必然可靠关闭；
 *  stopPropagation 拦截后不再冒泡到 window 气泡阶段的其它 Esc 监听
 *  （右键菜单/useEscape），避免重复触发，关闭/卸载自动清理
 *
 *  M-14：键盘焦点闭环（`docs/UI_DESIGN_SYSTEM.md` §9「Dialog 打开后焦点进入，
 *  关闭后返回触发点」）：
 *  1. 打开时把焦点移进弹窗；内部元素已自己 `autoFocus` 时不抢它的焦点。
 *  2. Tab / Shift+Tab 在弹窗内循环，不让焦点跑到底层页面（遮罩外的世界对
 *     键盘用户不可见，焦点跑出去等于按了看不见的按钮）。
 *  3. 关闭时把焦点还给打开它的那个元素；该元素已从 DOM 移除时退回 body，
 *     不强行聚焦到别处。
 *
 *  注意：初始焦点落在容器本身而不是第一个按钮上，所以「危险操作」按钮
 *  （footer 里第二个）永远不会成为默认焦点，M-14 明确要求这一点。
 */
export default function Modal({ open, title, onClose, children, footer, wide }: ModalProps) {
  // 最新回调引用：避免父组件内联 onClose 变化导致监听反复拆装
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });

  const dialogRef = useRef<HTMLDivElement>(null);
  /** 打开弹窗前持有焦点的元素：关闭后要还给它 */
  const triggerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      closeRef.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    triggerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // 弹窗内部已经有人主动 autoFocus（如标签管理的新建输入框）时不抢焦点：
    // 那是有意为之的输入起点，抢走会让用户多点一次。
    if (!dialog.contains(document.activeElement)) {
      dialog.focus({ preventScroll: true });
    }
    return () => {
      const trigger = triggerRef.current;
      triggerRef.current = null;
      if (trigger && trigger.isConnected) {
        trigger.focus({ preventScroll: true });
      }
    };
  }, [open]);

  /** Tab 循环：焦点到两端时手动折返，避免键盘用户跑到遮罩背后的页面。 */
  const onDialogKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Tab") return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const items = focusableIn(dialog);
    if (items.length === 0) {
      // 没有任何可聚焦控件时把焦点留在容器上，不要让 Tab 跳出去。
      event.preventDefault();
      dialog.focus({ preventScroll: true });
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (!event.shiftKey && (active === last || !dialog.contains(active))) {
      event.preventDefault();
      first.focus();
      return;
    }
    if (event.shiftKey && (active === first || active === dialog || !dialog.contains(active))) {
      event.preventDefault();
      last.focus();
    }
  };

  if (!open) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/30"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onKeyDown={onDialogKeyDown}
        className={
          (wide
            ? "w-[860px] max-w-[92vw] rounded-lg bg-[var(--color-bg)] border border-[var(--color-border)] shadow-xl"
            : "w-[420px] max-w-[90vw] rounded-lg bg-[var(--color-bg)] border border-[var(--color-border)] shadow-xl") +
          // 最小窗口 960×640 下，长内容（迁移向导 / 导出失败清单 / 删除确认）会把
          // 底部按钮顶出视口且无法滚动；这里限高，正文独立滚动，页头页脚固定。
          " flex max-h-[calc(100vh-2rem)] flex-col"
        }
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 px-5 py-3 border-b border-[var(--color-border)] text-sm font-medium">{title}</div>
        <div data-testid="modal-body" className="min-h-0 flex-1 overflow-y-auto px-5 py-4 text-sm">{children}</div>
        {footer && <div className="shrink-0 px-5 py-3 border-t border-[var(--color-border)] flex justify-end gap-2">{footer}</div>}
      </div>
    </div>
  );
}
