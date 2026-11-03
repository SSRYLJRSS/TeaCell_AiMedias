/** 会话权限档位选择器（v2 A3-10）。
 *
 * 位置与形态由 `plans/v2/12-implementation-contracts.md` §6.2 与冻结的交互方案决定：
 * - 常驻在输入区工具行左侧，紧挨「Agent 服务」，收起时只显示当前档位名；
 * - 向上弹出，不用模态框：这是高频、低风险的切换，弹层不该遮住正在写的提示词；
 * - 升到「完全权限」必须在菜单内二次确认（默认焦点在「取消」），降档不确认；
 * - **不做焦点陷阱**：弹层不是模态，用户随时可以继续在输入框里打字；
 *   `Esc` 关闭并把焦点还给触发器。
 *
 * 无障碍：`role="menu"` + `role="menuitemradio"` + `aria-checked`，让辅助技术
 * 把三档读成一组单选，而不是三个普通按钮。
 */
import { useEffect, useRef, useState } from "react";
import { ShieldAlert } from "lucide-react";
import PickerPopover from "@/components/common/PickerPopover";
import PickerTrigger from "@/components/common/PickerTrigger";
import PickerMenuRow from "@/components/common/PickerMenuRow";
import {
  AGENT_PERMISSION_OPTIONS,
  agentPermissionLabel,
  type AgentPermission,
} from "@/constants/agentPermission";

export default function PermissionPicker({
  value,
  busy = false,
  disabled = false,
  onSelect,
}: {
  value: AgentPermission;
  /** 正在回答：档位与工具表已定型，换档必须先停止。 */
  busy?: boolean;
  disabled?: boolean;
  onSelect: (next: AgentPermission) => void;
}) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  const locked = disabled || busy;
  const label = agentPermissionLabel(value);
  const lockedReason = busy ? "回答进行中，请先停止再切换权限" : "当前无法切换权限";

  const close = (refocus: boolean) => {
    setOpen(false);
    setConfirming(false);
    if (refocus) triggerRef.current?.focus();
  };

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      close(true);
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (menuRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [open]);

  // 二次确认出现时把焦点放在「取消」上：误触 Enter 不应该升到最宽档。
  useEffect(() => {
    if (confirming) cancelRef.current?.focus();
  }, [confirming]);

  const choose = (next: AgentPermission) => {
    if (next === value) {
      close(true);
      return;
    }
    if (next === "full") {
      setConfirming(true);
      return;
    }
    close(true);
    onSelect(next);
  };

  return (
    <div className="relative shrink-0">
      <PickerTrigger
        ref={triggerRef}
        icon={<ShieldAlert size={12} />}
        label={label}
        ariaLabel={`会话权限：${label}`}
        title={locked ? lockedReason : `会话权限：${label}`}
        open={open}
        haspopup="menu"
        disabled={locked}
        onClick={() => (open ? close(false) : setOpen(true))}
        labelClassName="max-w-[72px]"
      />

      <PickerPopover
        open={open}
        triggerRef={triggerRef}
        panelRef={menuRef}
        width={300}
        maxHeight={360}
        preferSide="top"
      >
        <div role="menu" aria-label="会话权限">
          {confirming ? (
            <div className="flex flex-col gap-2 p-2">
              <p className="text-[11px] leading-5 text-[var(--color-text-secondary)]">
                切换到「完全权限」后，助手可以准备移动和删除素材。执行前仍然需要你批准，删除仍会单独确认。
              </p>
              <div className="flex items-center justify-end gap-2">
                <button
                  ref={cancelRef}
                  type="button"
                  className="rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
                  onClick={() => {
                    setConfirming(false);
                    triggerRef.current?.focus();
                  }}
                >
                  取消
                </button>
                <button
                  type="button"
                  className="rounded-[var(--radius-item)] bg-[var(--color-status)] px-2 py-1 text-[11px] text-[var(--color-accent-text)] hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
                  onClick={() => {
                    close(true);
                    onSelect("full");
                  }}
                >
                  确认切换
                </button>
              </div>
            </div>
          ) : (
            <>
              {AGENT_PERMISSION_OPTIONS.map((option) => (
                <PickerMenuRow
                  key={option.value}
                  role="menuitemradio"
                  title={option.label}
                  description={option.summary}
                  selected={option.value === value}
                  onSelect={() => choose(option.value)}
                />
              ))}
              <p className="px-2 pb-1 pt-1.5 text-[11px] leading-4 text-[var(--color-text-tertiary)]">
                切换后本会话会重新打开；无论哪一档，计划都要由你批准。
              </p>
            </>
          )}
        </div>
      </PickerPopover>
    </div>
  );
}
