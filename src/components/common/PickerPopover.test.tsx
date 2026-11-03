/** PickerPopover 定位原语：preferSide="top" 优先向上、空间不足回落向下，宽度受视口夹取。 */
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { useRef } from "react";
import PickerPopover from "@/components/common/PickerPopover";

function rect(top: number, height = 30, left = 100): DOMRect {
  return {
    top,
    bottom: top + height,
    left,
    right: left + 120,
    width: 120,
    height,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

function Harness({ top, preferSide }: { top: number; preferSide?: "top" | "bottom" }) {
  const ref = useRef<HTMLButtonElement | null>(null);
  return (
    <div className="relative">
      <button
        ref={(node) => {
          ref.current = node;
          if (node) node.getBoundingClientRect = () => rect(top);
        }}
      >
        触发器
      </button>
      <PickerPopover open triggerRef={ref} width={440} maxHeight={320} preferSide={preferSide}>
        <div>菜单</div>
      </PickerPopover>
    </div>
  );
}

describe("PickerPopover", () => {
  it("preferSide=top 且上方空间充足时向上弹出", () => {
    const { container } = render(<Harness top={600} preferSide="top" />);
    const pop = container.querySelector(".picker-popover") as HTMLElement;
    expect(pop).toHaveAttribute("data-side", "top");
    // 宽度受视口夹取（避免 1024 窄窗溢出）。
    expect(pop.style.maxWidth).toBe("calc(100vw - 16px)");
  });

  it("preferSide=top 但上方空间不足时回落向下", () => {
    const { container } = render(<Harness top={100} preferSide="top" />);
    const pop = container.querySelector(".picker-popover") as HTMLElement;
    expect(pop).toHaveAttribute("data-side", "bottom");
  });
});
