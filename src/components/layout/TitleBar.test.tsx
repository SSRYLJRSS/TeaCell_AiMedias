/**
 * TitleBar 标题栏测试（PRD 第 8 节 / A3-05）：
 *  - 左侧是工作状态区 + 显式「设置」入口（不渲染品牌标志与软件名）；
 *  - 状态区接 taskStore 的真实任务投影，缓慢轮播、悬停暂停、可手动滚动；
 *  - 右侧只显示最小化、最大化/还原、关闭三个按钮；
 *  - 设置入口与窗口控制按钮不携带 data-tauri-drag-region（点击即拖拽的回归门禁）；
 *  - 点击「设置」派发 app:navigate=settings；
 *  - 非 Tauri 环境（jsdom）不抛异常。
 */
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";
import TitleBar from "@/components/layout/TitleBar";
import { WORK_STATUS_ROTATE_MS } from "@/components/layout/WorkStatus";
import { useTaskStore } from "@/stores/taskStore";
import type { TaskItem } from "@/stores/taskStore";

const windowMocks = vi.hoisted(() => ({
  getCurrentWindow: vi.fn(),
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn(),
  onResized: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: windowMocks.getCurrentWindow,
}));

function task(over: Partial<TaskItem>): TaskItem {
  return { id: "t1", kind: "import", label: "入库", overall: 0.5, detail: "正在入库", ...over };
}

beforeEach(() => {
  vi.resetAllMocks();
  windowMocks.minimize.mockResolvedValue(undefined);
  windowMocks.toggleMaximize.mockResolvedValue(undefined);
  windowMocks.close.mockResolvedValue(undefined);
  windowMocks.isMaximized.mockResolvedValue(false);
  windowMocks.onResized.mockResolvedValue(() => {});
  windowMocks.getCurrentWindow.mockReturnValue({
    minimize: windowMocks.minimize,
    toggleMaximize: windowMocks.toggleMaximize,
    close: windowMocks.close,
    isMaximized: windowMocks.isMaximized,
    onResized: windowMocks.onResized,
  });
  useTaskStore.setState({ tasks: [] });
});

describe("TitleBar（PRD 第 8 节 / A3-05）", () => {
  it("软件名不显示，图标位置让给「设置」，工作状态区在设置右侧", () => {
    const { container } = render(<TitleBar />);
    expect(screen.queryByText("茶馆")).not.toBeInTheDocument();

    const settings = screen.getByRole("button", { name: "设置" });
    expect(settings).toHaveAttribute("title", "设置");
    // 左上角不再放品牌图：那个位置直接让给「设置」文字入口。
    expect(settings.textContent).toBe("设置");
    expect(settings.querySelector("img")).toBeNull();
    expect(container.querySelector("header img")).toBeNull();
    expect(container.querySelector("svg.lucide-settings")).toBeNull();
    // 顺序固定：设置 → 工作状态区（PRD 第 8 节要求状态在右侧）。
    const workStatus = screen.getByTestId("work-status");
    expect(settings.compareDocumentPosition(workStatus) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("窗口控制只显示三个按钮：最小化/最大化或还原/关闭", () => {
    render(<TitleBar />);
    const minBtn = screen.getByRole("button", { name: "最小化" });
    const maxBtn = screen.getByRole("button", { name: /最大化|还原/ });
    const closeBtn = screen.getByRole("button", { name: "关闭" });
    expect(minBtn).toBeInTheDocument();
    expect(maxBtn).toBeInTheDocument();
    expect(closeBtn).toBeInTheDocument();
    // 设置入口 + 三个窗口控制 = 恰好 4 个按钮，无多余入口；四个标签互不相同
    const labels = screen.getAllByRole("button").map((b) => b.getAttribute("aria-label"));
    expect(labels).toHaveLength(4);
    expect(new Set(labels).size).toBe(4);
    // 窗口控制顺序为 最小化 → 最大化/还原 → 关闭
    expect(minBtn.compareDocumentPosition(maxBtn) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(maxBtn.compareDocumentPosition(closeBtn) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(screen.getByRole("group", { name: "窗口控制" })).toBeInTheDocument();
  });

  it("自绘窗口按钮调用 Tauri 当前窗口 API", async () => {
    render(<TitleBar />);
    fireEvent.click(screen.getByRole("button", { name: "最小化" }));
    fireEvent.click(screen.getByRole("button", { name: /最大化|还原/ }));
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));

    await waitFor(() => {
      expect(windowMocks.minimize).toHaveBeenCalledTimes(1);
      expect(windowMocks.toggleMaximize).toHaveBeenCalledTimes(1);
      expect(windowMocks.close).toHaveBeenCalledTimes(1);
    });
  });

  it("设置入口在左侧（位于窗口控制之前）且无 data-tauri-drag-region", () => {
    render(<TitleBar />);
    const settings = screen.getByRole("button", { name: "设置" });
    const close = screen.getByRole("button", { name: "关闭" });
    expect(settings.getAttribute("data-tauri-drag-region")).toBeNull();
    expect((settings.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0).toBe(true);
    expect(screen.getByRole("button", { name: "最小化" }).getAttribute("data-tauri-drag-region")).toBeNull();
    expect(close.getAttribute("data-tauri-drag-region")).toBeNull();
  });

  it("窗口控制与设置入口都有可见的键盘焦点环", () => {
    render(<TitleBar />);
    for (const name of ["最小化", "关闭", "设置"]) {
      const button = screen.getByRole("button", { name });
      expect(button.className).toContain("focus-visible:outline");
      expect(button.className).not.toContain("focus-visible:outline-none");
    }
  });

  it("关闭钮悬停用语义危险色，不硬编码 Windows 主题色", () => {
    render(<TitleBar />);
    const close = screen.getByRole("button", { name: "关闭" });
    expect(close.className).toContain("hover:bg-[var(--color-danger)]");
    expect(close.className).toContain("hover:text-[var(--color-danger-contrast)]");
    // 回归门禁：禁止回落到硬编码色值
    expect(close.className).not.toMatch(/#[0-9a-fA-F]{3,8}/);
    expect(close.className).not.toContain("hover:text-white");
  });

  it("布局文件不引入无语义硬编码颜色（AGENTS.md 规则 9 / 卡片停下条件）", () => {
    // 读取源码做静态断言：A3-05 负责的三个布局文件只能通过 theme.css 语义变量上色。
    // 注意：vite 会重写 import.meta.url，这里按仓库根（vitest cwd）解析源码路径。
    const files = ["BottomBar.tsx", "TitleBar.tsx", "WorkStatus.tsx"];
    for (const file of files) {
      const source = readFileSync(join(process.cwd(), "src/components/layout", file), "utf8");
      expect(source, `${file} 不应含十六进制硬编码颜色`).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
      expect(source, `${file} 不应含 rgba()/rgb() 硬编码颜色`).not.toMatch(/\brgba?\(/);
    }
  });

  it("点击设置入口派发 app:navigate=settings", () => {
    const listener = vi.fn();
    window.addEventListener("app:navigate", listener);
    render(<TitleBar />);
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    expect(listener).toHaveBeenCalledTimes(1);
    const detail = (listener.mock.calls[0][0] as CustomEvent).detail;
    expect(detail).toBe("settings");
    window.removeEventListener("app:navigate", listener);
  });

  it("非 Tauri 环境渲染不抛异常（jsdom 无窗口 API）", () => {
    windowMocks.getCurrentWindow.mockImplementationOnce(() => {
      throw new Error("not running in Tauri");
    });
    expect(() => render(<TitleBar />)).not.toThrow();
  });

  it("工作状态区来自真实任务投影：无任务时说明，有任务时显示状态与进度序号", () => {
    const { rerender } = render(<TitleBar />);
    expect(screen.getByTestId("work-status").textContent).toContain("暂无进行中的任务");

    act(() => {
      useTaskStore.setState({ tasks: [task({ id: "t1", label: "导入照片" })] });
    });
    rerender(<TitleBar />);
    const status = screen.getByTestId("work-status");
    expect(status.textContent).toContain("进行中");
    expect(status.textContent).toContain("导入照片");
  });

  it("多条任务缓慢轮播，悬停即暂停", () => {
    vi.useFakeTimers();
    try {
      useTaskStore.setState({
        tasks: [
          task({ id: "t1", label: "任务甲" }),
          task({ id: "t2", label: "任务乙", done: true, overall: 1, detail: "已完成" }),
        ],
      });
      render(<TitleBar />);
      const status = screen.getByTestId("work-status");
      expect(status.textContent).toContain("任务甲");
      expect(status.textContent).toContain("1/2");

      // 缓慢轮播：未到间隔不切换
      act(() => {
        vi.advanceTimersByTime(WORK_STATUS_ROTATE_MS - 1);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("任务甲");

      act(() => {
        vi.advanceTimersByTime(1);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("任务乙");

      // 悬停暂停：再过一整个间隔也不切换
      fireEvent.mouseEnter(screen.getByTestId("work-status"));
      act(() => {
        vi.advanceTimersByTime(WORK_STATUS_ROTATE_MS * 2);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("任务乙");
    } finally {
      vi.useRealTimers();
    }
  });

  it("支持手动滚动切换，不依赖自动轮播", () => {
    useTaskStore.setState({
      tasks: [task({ id: "t1", label: "任务甲" }), task({ id: "t2", label: "任务乙" })],
    });
    render(<TitleBar />);
    expect(screen.getByTestId("work-status").textContent).toContain("任务甲");
    fireEvent.wheel(screen.getByTestId("work-status"), { deltaY: 120 });
    expect(screen.getByTestId("work-status").textContent).toContain("任务乙");
    fireEvent.keyDown(screen.getByTestId("work-status"), { key: "ArrowUp" });
    expect(screen.getByTestId("work-status").textContent).toContain("任务甲");
  });
});
