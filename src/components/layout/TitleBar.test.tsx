/**
 * TitleBar 标题栏测试（指导书 §3.3 / §12.1）：
 *  - 左侧 logo 本身作为设置入口，不再显示独立齿轮按钮；
 *  - 软件名不显示（删除「茶包素材 BagerTea V2」文本）；
 *  - 右侧只显示最小化、最大化/还原、关闭三个按钮；
 *  - logo 设置入口/窗口控制按钮不携带 data-tauri-drag-region（点击即拖拽的回归门禁）；
 *  - 点击 logo 设置入口派发 app:navigate=settings；
 *  - 非 Tauri 环境（jsdom）不抛异常。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import TitleBar from "@/components/layout/TitleBar";
import { usePlatformStore } from "@/stores/platformStore";
import type { PlatformCapabilities } from "@/types/platform";

afterEach(() => {
  // 复位平台能力，避免跨用例串状态（默认 idle → 自绘控制）
  usePlatformStore.setState({ status: "idle", capabilities: null, error: null });
});

describe("TitleBar（指导书 §3.3）", () => {
  it("软件名不显示，logo 本身作为设置入口并提供悬停提示", () => {
    const { container } = render(<TitleBar />);
    expect(screen.queryByText(/茶包素材 BagerTea V2/)).not.toBeInTheDocument();
    expect(screen.queryByText(/BagerTea V2/i)).not.toBeInTheDocument();

    const settings = screen.getByRole("button", { name: "设置" });
    expect(settings).toHaveAttribute("title", "设置");
    expect(settings.querySelector("img")).not.toBeNull();
    expect(container.querySelector("svg.lucide-settings")).toBeNull();
  });

  it("窗口控制只显示三个按钮：最小化/最大化或还原/关闭", () => {
    render(<TitleBar />);
    const minBtn = screen.getByRole("button", { name: "最小化" });
    const maxBtn = screen.getByRole("button", { name: /最大化|还原/ });
    const closeBtn = screen.getByRole("button", { name: "关闭" });
    expect(minBtn).toBeInTheDocument();
    expect(maxBtn).toBeInTheDocument();
    expect(closeBtn).toBeInTheDocument();
    // logo 设置入口 + 三个窗口控制 = 恰好 4 个按钮，无多余入口；四个标签互不相同
    const labels = screen.getAllByRole("button").map((b) => b.getAttribute("aria-label"));
    expect(labels).toHaveLength(4);
    expect(new Set(labels).size).toBe(4);
    // 窗口控制顺序为 最小化 → 最大化/还原 → 关闭
    expect(minBtn.compareDocumentPosition(maxBtn) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(maxBtn.compareDocumentPosition(closeBtn) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
  });

  it("logo 设置入口在左侧（位于窗口控制之前）且无 data-tauri-drag-region", () => {
    render(<TitleBar />);
    const settings = screen.getByRole("button", { name: "设置" });
    const close = screen.getByRole("button", { name: "关闭" });
    // logo 设置入口不携带拖拽标记
    expect(settings.getAttribute("data-tauri-drag-region")).toBeNull();
    // 设置位于关闭按钮之前（同排左侧）
    expect((settings.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0).toBe(true);
    // 窗口控制按钮也不携带拖拽标记
    expect(screen.getByRole("button", { name: "最小化" }).getAttribute("data-tauri-drag-region")).toBeNull();
    expect(close.getAttribute("data-tauri-drag-region")).toBeNull();
  });

  it("点击 logo 设置入口派发 app:navigate=settings", () => {
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
    expect(() => render(<TitleBar />)).not.toThrow();
  });

  // 三端统一自绘标题栏（指导书 §6.3）：Windows/macOS/Linux 三种 fixture 下均渲染窗口控制。
  const fixtures: { os: PlatformCapabilities["os"]; arch: PlatformCapabilities["arch"]; managedOllama: boolean; preferredVideoProxy: PlatformCapabilities["preferredVideoProxy"] }[] = [
    { os: "windows", arch: "x86_64", managedOllama: true, preferredVideoProxy: "h264_mp4" },
    { os: "macos", arch: "aarch64", managedOllama: false, preferredVideoProxy: "h264_mp4" },
    { os: "linux", arch: "x86_64", managedOllama: false, preferredVideoProxy: "vp8_webm" },
  ];

  for (const f of fixtures) {
    it(`${f.os}：始终渲染自绘窗口控制（三端一致）`, () => {
      usePlatformStore.setState({
        status: "ready",
        error: null,
        capabilities: { schemaVersion: 1, ...f },
      });
      render(<TitleBar />);
      expect(screen.getByRole("button", { name: "最小化" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /最大化|还原/ })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "关闭" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    });
  }
});
