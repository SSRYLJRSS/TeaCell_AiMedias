import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import BottomBar from "@/components/layout/BottomBar";
import { useTaskStore } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { defaultNavigationShortcuts } from "@/utils/normalizeSettings";
import type { Navigation } from "@/types/settings";

function setNavigation(navigation: Navigation) {
  // 包在 act 里：设置变更会同步驱动已挂载底栏的重渲染
  act(() => {
    useSettingsStore.setState({ settings: { navigation } as never });
  });
}

describe("BottomBar 超级搜索入口提示", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTaskStore.setState({ tasks: [] });
    useSettingsStore.setState({ settings: null });
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

  it("入库页隐藏重复的入库任务，其他页面保留全局进度", () => {
    useTaskStore.setState({
      tasks: [{
        id: "t1",
        kind: "import",
        label: "入库",
        overall: 0.5,
        detail: "正在入库",
      }],
    });

    const { rerender } = render(
      <BottomBar current="import" onNavigate={() => undefined} />,
    );
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();

    rerender(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
  });

  it("离开入库页后只兜底展示运行中任务，完成态留在入库侧栏", () => {
    useTaskStore.setState({
      tasks: [{
        id: "t-done",
        kind: "import",
        label: "入库",
        overall: 1,
        detail: "已完成",
        done: true,
      }],
    });

    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });
});

describe("BottomBar 内置快捷项（PRD 第 8 节 / A3-05）", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
    useSettingsStore.setState({ settings: null });
  });

  function entryLabels(): string[] {
    const nav = screen.getByRole("navigation", { name: "主导航" });
    return within(nav)
      .queryAllByRole("button")
      .filter((b) => b.getAttribute("aria-label") !== "助手")
      .map((b) => b.getAttribute("aria-label") ?? "");
  }

  it("默认五入口顺序为 入库｜素材库｜中央 Logo｜打标｜百宝箱，Logo 独占中间列", () => {
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual(["入库", "素材库", "打标", "百宝箱"]);

    const nav = screen.getByRole("navigation", { name: "主导航" });
    // 三列栅格：1fr | auto | 1fr，Logo 在中间列 → 无论左右入口数量如何都保持窗口正中央
    expect(nav.className).toContain("grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]");
    expect(nav.children).toHaveLength(3);
    const middle = nav.children[1] as HTMLElement;
    expect(middle.tagName).toBe("BUTTON");
    expect(middle).toHaveAttribute("aria-label", "助手");
    expect(nav.children[0].querySelectorAll("button")).toHaveLength(2);
    expect(nav.children[2].querySelectorAll("button")).toHaveLength(2);
  });

  it("隐藏的入口不渲染，但功能目录本身不被删除", () => {
    const shortcuts = defaultNavigationShortcuts().map((s) =>
      s.id === "treasure" ? { ...s, visible: false } : s,
    );
    setNavigation({ shortcuts, settingsPinned: false });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual(["入库", "素材库", "打标"]);
    // 只是隐藏：设置里仍保留这一项，重新打开即可恢复
    expect(useSettingsStore.getState().settings?.navigation.shortcuts).toHaveLength(4);
  });

  it("按用户保存的顺序渲染入口", () => {
    const shortcuts = [
      { id: "ai" as const, visible: true },
      { id: "treasure" as const, visible: true },
      { id: "import" as const, visible: true },
      { id: "library" as const, visible: true },
    ];
    setNavigation({ shortcuts, settingsPinned: false });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual(["打标", "百宝箱", "入库", "素材库"]);
  });

  it("未知旧 id 被安全跳过，不会渲染成死按钮", () => {
    const shortcuts = [
      { id: "import" as const, visible: true },
      { id: "legacy-plugin" as unknown as "ai", visible: true },
      { id: "library" as const, visible: true },
    ];
    setNavigation({ shortcuts, settingsPinned: false });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual(["入库", "素材库"]);
    expect(screen.queryByRole("button", { name: "legacy-plugin" })).not.toBeInTheDocument();
  });

  it("全部入口隐藏时仍保留中央 Logo，底栏不会消失", () => {
    setNavigation({ shortcuts: [], settingsPinned: false });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual([]);
    expect(screen.getByRole("button", { name: "助手" })).toBeInTheDocument();
  });

  it("入口数量从 0 到 5 变化时，中央 Logo 始终独占中间列（R07 验收）", () => {
    const all = defaultNavigationShortcuts();
    // 0..4 个可配置入口（逐项隐藏），第 5 种是 settingsPinned 追加的「设置」
    for (let visibleCount = 0; visibleCount <= 4; visibleCount += 1) {
      const shortcuts = all.map((s, i) => ({ ...s, visible: i < visibleCount }));
      setNavigation({ shortcuts, settingsPinned: false });
      const { unmount } = render(<BottomBar current="library" onNavigate={() => undefined} />);
      const nav = screen.getByRole("navigation", { name: "主导航" });
      expect(nav.className).toContain("grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]");
      expect(nav.children).toHaveLength(3);
      expect((nav.children[1] as HTMLElement).tagName).toBe("BUTTON");
      expect(nav.children[1]).toHaveAttribute("aria-label", "助手");
      expect(entryLabels()).toHaveLength(visibleCount);
      unmount();
    }

    // 5 个入口：4 个快捷项 + 固定的「设置」，Logo 仍在中间列
    setNavigation({ shortcuts: all, settingsPinned: true });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    const nav = screen.getByRole("navigation", { name: "主导航" });
    expect(nav.children).toHaveLength(3);
    expect(nav.children[1]).toHaveAttribute("aria-label", "助手");
    expect(entryLabels()).toHaveLength(5);
  });

  it("「设置」固定项只在 settingsPinned 时出现，且排在可配置入口之后", () => {
    setNavigation({ shortcuts: defaultNavigationShortcuts(), settingsPinned: false });
    const { rerender } = render(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(screen.queryByRole("button", { name: "设置" })).not.toBeInTheDocument();

    setNavigation({ shortcuts: defaultNavigationShortcuts(), settingsPinned: true });
    rerender(<BottomBar current="library" onNavigate={() => undefined} />);
    expect(entryLabels()).toEqual(["入库", "素材库", "打标", "百宝箱", "设置"]);
  });

  it("点击快捷项把目标原样交给 onNavigate（含百宝箱与设置）", () => {
    setNavigation({ shortcuts: defaultNavigationShortcuts(), settingsPinned: true });
    const onNavigate = vi.fn();
    render(<BottomBar current="library" onNavigate={onNavigate} />);
    fireEvent.click(screen.getByRole("button", { name: "百宝箱" }));
    expect(onNavigate).toHaveBeenLastCalledWith("treasure");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    expect(onNavigate).toHaveBeenLastCalledWith("settings");
  });

  it("当前页为百宝箱时对应入口高亮", () => {
    render(<BottomBar current="treasure" onNavigate={() => undefined} />);
    expect(screen.getByRole("button", { name: "百宝箱" })).toHaveAttribute("aria-current", "page");
  });

  it("键盘 Tab 到入口与中央 Logo 时有可见焦点环（不依赖鼠标悬停）", () => {
    setNavigation({ shortcuts: defaultNavigationShortcuts(), settingsPinned: true });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    // 所有可聚焦按钮都是原生 button（Tab 可到达），且带 focus-visible 焦点环
    const buttons = [screen.getByRole("button", { name: "助手" }), ...within(screen.getByRole("navigation", { name: "主导航" })).queryAllByRole("button")];
    // 数量本身有断言：4 个默认快捷项 + 固定「设置」+ 中央 Logo（助手）+ 任务入口，少一个就说明焦点环没机会被检查
    expect(buttons).toHaveLength(7);
    for (const button of buttons) {
      expect(button.tagName).toBe("BUTTON");
      expect(button.className).toContain("focus-visible:outline-2");
      expect(button.className).toContain("var(--color-status)");
      expect(button.className).not.toContain("focus-visible:outline-none");
    }
  });

  it("失败任务文案用危险语义色，不用进度橙", () => {
    setNavigation({ shortcuts: defaultNavigationShortcuts(), settingsPinned: false });
    act(() => {
      useTaskStore.setState({
        tasks: [
          {
            id: "imp",
            kind: "import",
            label: "导入",
            overall: null,
            detail: "处理中",
            error: "源目录不可读",
          } as never,
        ],
      });
    });
    render(<BottomBar current="library" onNavigate={() => undefined} />);
    const failedText = screen.getByText("源目录不可读");
    expect(failedText.className).toContain("var(--color-danger)");
    expect(failedText.className).not.toContain("var(--color-status)");
    act(() => {
      useTaskStore.setState({ tasks: [] });
    });
  });
});
