import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import BottomBar from "@/components/layout/BottomBar";
import { useTaskStore } from "@/stores/taskStore";

describe("BottomBar 超级搜索入口提示", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTaskStore.setState({ tasks: [] });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("悬停素材库 0.5 秒后显示提示，移开后立即关闭", () => {
    render(<BottomBar current="library" onNavigate={() => undefined} onOpenSuperSearch={() => undefined} />);
    const library = screen.getByRole("button", { name: "素材库" });

    fireEvent.mouseEnter(library);
    act(() => vi.advanceTimersByTime(499));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByRole("tooltip")).toHaveTextContent("双击进入超级搜索");

    fireEvent.mouseLeave(library);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("不足 0.5 秒便移开时不显示提示；双击仍进入超级搜索", () => {
    const onNavigate = vi.fn();
    const onOpenSuperSearch = vi.fn();
    render(<BottomBar current="library" onNavigate={onNavigate} onOpenSuperSearch={onOpenSuperSearch} />);
    const library = screen.getByRole("button", { name: "素材库" });

    fireEvent.mouseEnter(library);
    act(() => vi.advanceTimersByTime(300));
    fireEvent.mouseLeave(library);
    act(() => vi.advanceTimersByTime(300));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();

    fireEvent.click(library);
    fireEvent.doubleClick(library);
    act(() => vi.advanceTimersByTime(230));
    expect(onNavigate).not.toHaveBeenCalled();
    expect(onOpenSuperSearch).toHaveBeenCalledTimes(1);
  });

  it("键盘聚焦 0.5 秒后显示提示，按钮名称保持稳定，失焦后关闭", () => {
    render(<BottomBar current="library" onNavigate={() => undefined} onOpenSuperSearch={() => undefined} />);
    const library = screen.getByRole("button", { name: "素材库" });

    fireEvent.focus(library);
    act(() => vi.advanceTimersByTime(500));
    expect(screen.getByRole("tooltip")).toHaveTextContent("双击进入超级搜索");
    expect(screen.getByRole("button", { name: "素材库" })).toHaveAttribute(
      "aria-describedby",
      "super-search-entry-hint",
    );

    fireEvent.blur(library);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it.each(["import", "library", "ai", "settings", "superSearch"] as const)(
    "%s 页面都不在底栏重复展示入库任务",
    (current) => {
      useTaskStore.setState({
        tasks: [{
          id: "t1",
          kind: "import",
          label: "入库",
          overall: 0.5,
          detail: "正在入库",
        }],
      });

      render(<BottomBar current={current} onNavigate={() => undefined} />);
      expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
      expect(screen.queryByText("正在入库")).not.toBeInTheDocument();
    },
  );

  it("导出任务仍在底栏全局展示", () => {
    useTaskStore.setState({
      tasks: [{
        id: "export",
        kind: "export",
        label: "导出中",
        overall: 0.5,
        detail: "5/10",
      }],
    });

    render(<BottomBar current="settings" onNavigate={() => undefined} />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
    expect(screen.getByText("导出中")).toBeInTheDocument();
  });
});
