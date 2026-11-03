/** 「Agent 服务 / 模型」选择器（WP2）。
 *
 * 取代原来的原生 `<select>`（透明覆盖）——原生下拉无法样式化、方向由系统决定，
 * 与「插入技能」「权限」两颗胶囊一个向下、一个向上的观感割裂正是用户抱怨点。
 *
 * 这里只负责**展示与选择**：候选服务、本机/在线判定、停用/密钥标注由 Composer
 * 算好后通过 `options` 传进来（A3-06 外发披露与 R33/R34 语义仍留在 Composer）。
 * 触发器暴露 ref，供失败后「切换服务」把焦点交给它（R34）。
 */
import { forwardRef, useEffect, useRef, useState } from "react";
import { Sparkles } from "lucide-react";
import PickerPopover from "@/components/common/PickerPopover";
import PickerTrigger from "@/components/common/PickerTrigger";
import PickerMenuRow from "@/components/common/PickerMenuRow";

export interface ServiceOption {
  id: string;
  title: string;
  description: string;
  disabled: boolean;
}

export interface ServiceModelLabel {
  name: string | null;
  detail: string;
}

const ServicePicker = forwardRef<
  HTMLButtonElement,
  {
    options: ServiceOption[];
    bindingId: string | null;
    model: ServiceModelLabel;
    switching: boolean;
    pendingRun: boolean;
    onSelect: (id: string) => void;
  }
>(function ServicePicker({ options, bindingId, model, switching, pendingRun, onSelect }, ref) {
  const [open, setOpen] = useState(false);
  const innerTriggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) innerTriggerRef.current?.focus();
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
      if (menuRef.current?.contains(target) || innerTriggerRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [open]);

  const pick = (id: string) => {
    close(true);
    onSelect(id);
  };

  return (
    <div className="relative shrink-0">
      <PickerTrigger
        ref={(node) => {
          innerTriggerRef.current = node;
          if (typeof ref === "function") ref(node);
          else if (ref) ref.current = node;
        }}
        icon={<Sparkles size={12} />}
        label={model.name || "未绑定服务"}
        ariaLabel="Agent 服务"
        title={model.name ? `${model.name} · ${model.detail}` : model.detail}
        open={open}
        haspopup="menu"
        disabled={switching || pendingRun}
        onClick={() => (open ? close(false) : setOpen(true))}
        labelClassName="max-w-[150px]"
      />
      <PickerPopover open={open} triggerRef={innerTriggerRef} panelRef={menuRef} width={360} maxHeight={320} preferSide="top">
        <div role="menu" aria-label="Agent 服务">
          {options.map((option) => (
            <PickerMenuRow
              key={option.id || "__none__"}
              title={option.title}
              description={option.description}
              selected={option.id === (bindingId ?? "")}
              disabled={option.disabled}
              titleAttr={option.disabled ? `${option.title} 当前不可用` : undefined}
              onSelect={() => pick(option.id)}
            />
          ))}
        </div>
      </PickerPopover>
    </div>
  );
});

export default ServicePicker;
