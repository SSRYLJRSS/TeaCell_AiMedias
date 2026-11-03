/**
 * PermissionPicker 测试（v2 A3-10）。
 *
 * 验收口径（`plans/v2/12-implementation-contracts.md` §6.2）：
 * - 常驻显示当前档位，展开后三档是**一组单选**（`menuitemradio` + `aria-checked`）；
 * - 升到「完全权限」必须在菜单内二次确认，默认焦点在「取消」——误触 Enter 不能升档；
 * - 降档不确认，但仍然只切换一次；
 * - `Esc` 关闭并把焦点还给触发器；弹层不是模态，不抢输入框焦点；
 * - 回答进行中（`busy`）不允许换档，并把原因写在 title 上，而不是静默禁用。
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import PermissionPicker from "@/components/assistant/PermissionPicker";

describe("PermissionPicker", () => {
  it("收起时只显示当前档位名，并把它写进无障碍名字", () => {
    render(<PermissionPicker value="standard" onSelect={() => {}} />);
    const trigger = screen.getByRole("button", { name: "会话权限：可执行改动" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
  });

  it("展开后三档是一组单选，当前档被标记为已选", () => {
    render(<PermissionPicker value="read" onSelect={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "会话权限：仅阅读" }));
    const items = screen.getAllByRole("menuitemradio");
    expect(items.map((item) => item.textContent)).toEqual([
      expect.stringContaining("仅阅读"),
      expect.stringContaining("可执行改动"),
      expect.stringContaining("完全权限"),
    ]);
    expect(items[0]).toHaveAttribute("aria-checked", "true");
    expect(items[1]).toHaveAttribute("aria-checked", "false");
  });

  it("降档直接生效，不弹二次确认", () => {
    const onSelect = vi.fn();
    render(<PermissionPicker value="full" onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "会话权限：完全权限" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /仅阅读/ }));
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith("read");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("升到完全权限先二次确认，默认焦点在取消，取消不切换", () => {
    const onSelect = vi.fn();
    render(<PermissionPicker value="standard" onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "会话权限：可执行改动" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /完全权限/ }));
    // 还没确认，不能已经切过去。
    expect(onSelect).not.toHaveBeenCalled();
    const cancel = screen.getByRole("button", { name: "取消" });
    expect(cancel).toHaveFocus();
    fireEvent.click(cancel);
    expect(onSelect).not.toHaveBeenCalled();
    // 取消后回到选项列表，而不是把整个弹层关掉。
    expect(screen.getAllByRole("menuitemradio")).toHaveLength(3);
  });

  it("确认后才切换，并把「删除仍单独确认」写进说明", () => {
    const onSelect = vi.fn();
    render(<PermissionPicker value="standard" onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "会话权限：可执行改动" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /完全权限/ }));
    expect(screen.getByText(/删除仍会单独确认/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "确认切换" }));
    expect(onSelect).toHaveBeenCalledWith("full");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("Esc 关闭弹层并把焦点还给触发器", () => {
    render(<PermissionPicker value="standard" onSelect={() => {}} />);
    const trigger = screen.getByRole("button", { name: "会话权限：可执行改动" });
    fireEvent.click(trigger);
    expect(screen.getByRole("menu")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("回答进行中禁用换档，并把原因写进 title", () => {
    render(<PermissionPicker value="standard" busy onSelect={() => {}} />);
    const trigger = screen.getByRole("button", { name: "会话权限：可执行改动" });
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveAttribute("title", "回答进行中，请先停止再切换权限");
    fireEvent.click(trigger);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
});
