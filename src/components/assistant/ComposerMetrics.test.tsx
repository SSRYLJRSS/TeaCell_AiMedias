/** 输入卡片统计行测试（A3-04 输入区改版）。
 *
 * 验收口径：
 * - 只有素材数可以本地计算，永远显示；
 * - 用量与缓存命中只有上游回传 usage 才知道，没有数据时**整块不渲染**，
 *   不能显示 0%、也不能估算（`docs/UI_DESIGN_SYSTEM.md` §7 禁止把不确定进度
 *   伪装成百分比）；
 * - 环形进度与旁边百分比取同一个整数，不出现「环 23%、文字 24%」。
 */
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import ComposerMetrics, { formatTokens } from "@/components/assistant/ComposerMetrics";

describe("ComposerMetrics", () => {
  it("没有 usage 时只显示素材数，不猜任何数字", () => {
    const { container } = render(<ComposerMetrics attachmentCount={3} />);
    expect(screen.getByText("3 个素材")).toBeInTheDocument();
    expect(screen.queryByText(/tok/)).toBeNull();
    expect(screen.queryByText(/缓存命中/)).toBeNull();
    expect(container.querySelector(".cmp-ring")).toBeNull();
  });

  it("有 usage 时三段齐全，百分比与环形取同一个数", () => {
    const { container } = render(
      <ComposerMetrics
        attachmentCount={12}
        usage={{ totalTokens: 21400, cacheHitRatio: 0.92, contextRatio: 0.23 }}
      />,
    );
    expect(screen.getByText("12 个素材")).toBeInTheDocument();
    expect(screen.getByText("21.4k tok")).toBeInTheDocument();
    expect(screen.getByText("缓存命中 92%")).toBeInTheDocument();
    expect(screen.getByText("23%")).toBeInTheDocument();

    const value = container.querySelector(".cmp-ring__value");
    expect(value).not.toBeNull();
    const circumference = 2 * Math.PI * 6.5;
    expect(Number(value?.getAttribute("stroke-dashoffset"))).toBeCloseTo(circumference * 0.77, 5);
  });

  it("比例越界时夹到 0–100%，不出现负环或超过一圈", () => {
    render(
      <ComposerMetrics
        attachmentCount={0}
        usage={{ totalTokens: 0, cacheHitRatio: 1.4, contextRatio: -0.2 }}
      />,
    );
    expect(screen.getByText("缓存命中 100%")).toBeInTheDocument();
    expect(screen.getByText("0%")).toBeInTheDocument();
  });

  it("token 数折算成 k/M，小数字不补零", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1200)).toBe("1.2k");
    expect(formatTokens(21400)).toBe("21.4k");
    expect(formatTokens(137_000_000)).toBe("137M");
    expect(formatTokens(Number.NaN)).toBe("0");
  });
});
