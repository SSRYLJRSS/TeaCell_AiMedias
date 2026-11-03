/**
 * 助手右侧栏外壳（三个入口共用的抽屉）。
 *
 * 验收口径：
 * - 三个选项卡必须是 `role="tablist"` / `role="tab"`，Testing Library 能按名字点到，
 *   `aria-selected` 跟随当前选项卡；
 * - 宽度两档沿用工作台原有口径（380 / 620），加宽与收起只回调父级，外壳不自己存状态；
 * - 工作台选项卡自带「加宽 / 收起」，外壳让位不再渲染同名控件（同一屏不能有两个「收起」）。
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import AssistantSidePanel from "@/components/assistant/AssistantSidePanel";

const noop = () => undefined;

describe("AssistantSidePanel", () => {
  it("三个选项卡按名字可点，aria-selected 跟随当前选项卡", () => {
    const onTabChange = vi.fn();
    render(
      <AssistantSidePanel
        tab="workflows"
        wide={false}
        onTabChange={onTabChange}
        onToggleWide={noop}
        onClose={noop}
      >
        <p>面板内容</p>
      </AssistantSidePanel>,
    );

    const tablist = screen.getByRole("tablist", { name: "助手侧栏选项卡" });
    expect(within(tablist).getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
      "常用流程",
      "技能库",
      "待审计划",
      "工作台",
    ]);
    expect(screen.getByRole("tab", { name: "常用流程" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("tab", { name: "待审计划" })).toHaveAttribute(
      "aria-selected",
      "false",
    );

    fireEvent.click(screen.getByRole("tab", { name: "待审计划" }));
    expect(onTabChange).toHaveBeenCalledWith("plans");

    // 内容区是 tabpanel，且选项卡真的指向它（不是只有视觉高亮）
    const content = screen.getByRole("tabpanel", { name: "常用流程" });
    expect(content).toHaveTextContent("面板内容");
    expect(screen.getByRole("tab", { name: "常用流程" })).toHaveAttribute(
      "aria-controls",
      content.id,
    );
  });

  it("宽度沿用 380/620，加宽与收起只回调父级", () => {
    const onToggleWide = vi.fn();
    const onClose = vi.fn();
    const { rerender } = render(
      <AssistantSidePanel
        tab="plans"
        wide={false}
        onTabChange={noop}
        onToggleWide={onToggleWide}
        onClose={onClose}
      >
        <p>面板内容</p>
      </AssistantSidePanel>,
    );

    expect(screen.getByLabelText("助手侧栏").className).toContain("w-[380px]");
    fireEvent.click(screen.getByRole("button", { name: "加宽" }));
    expect(onToggleWide).toHaveBeenCalledTimes(1);

    rerender(
      <AssistantSidePanel
        tab="plans"
        wide
        onTabChange={noop}
        onToggleWide={onToggleWide}
        onClose={onClose}
      >
        <p>面板内容</p>
      </AssistantSidePanel>,
    );
    expect(screen.getByLabelText("助手侧栏").className).toContain("w-[620px]");
    fireEvent.click(screen.getByRole("button", { name: "收窄" }));
    expect(onToggleWide).toHaveBeenCalledTimes(2);

    fireEvent.click(screen.getByRole("button", { name: "收起" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("工作台选项卡自带加宽/收起，外壳让位不渲染同名控件", () => {
    render(
      <AssistantSidePanel
        tab="workbench"
        wide={false}
        onTabChange={noop}
        onToggleWide={noop}
        onClose={noop}
      >
        <p>工作台内容</p>
      </AssistantSidePanel>,
    );

    expect(screen.getByRole("tabpanel", { name: "工作台" })).toHaveTextContent("工作台内容");
    expect(screen.queryByRole("button", { name: "收起" })).toBeNull();
    expect(screen.queryByRole("button", { name: "加宽" })).toBeNull();
    // 选项卡仍在：侧栏里可以自己切回别的选项卡
    expect(screen.getByRole("tab", { name: "常用流程" })).toBeInTheDocument();
  });
});
