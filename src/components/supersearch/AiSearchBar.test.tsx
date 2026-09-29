/** W7-2：AiSearchBar 三态展示（W6-5）—— full 蓝字 / partial 黄字警告 / keyword 黄字兜底 */
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import AiSearchBar from "@/components/supersearch/AiSearchBar";
import { useSuperSearchStore } from "@/stores/superSearchStore";

vi.mock("@/api/superSearch", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/superSearch")>();
  return { ...actual, onAiSearchProgress: vi.fn().mockResolvedValue(() => undefined) };
});

const renderBar = () =>
  render(<AiSearchBar onSubmit={vi.fn()} />);

describe("AiSearchBar 三态", () => {
  beforeEach(() => {
    useSuperSearchStore.setState({
      aiInput: "海边",
      aiExplanation: null,
      warnings: [],
      parseStatus: null,
      aiError: null,
      aiRequestId: null,
      aiPhase: null,
      aiStartedAt: null,
      aiElapsedMs: 0,
      aiCancelPending: false,
    });
  });

  it("full：显示解释，无警告", () => {
    useSuperSearchStore.setState({
      aiExplanation: "筛选「海边」",
      warnings: [],
      parseStatus: "full",
    });
    renderBar();
    expect(screen.getByText(/AI 已转换为下方条件/)).toBeTruthy();
    expect(screen.queryByText(/部分条件|按关键词搜索/)).toBeNull();
  });

  it("partial：黄字警告 + 部分理解文案", () => {
    useSuperSearchStore.setState({
      aiExplanation: "筛选「海边」",
      warnings: ["已忽略无效的元数据条件"],
      parseStatus: "partial",
    });
    renderBar();
    expect(screen.getByText(/部分条件未能准确理解/)).toBeTruthy();
    expect(screen.getByText("已忽略无效的元数据条件")).toBeTruthy();
  });

  it("keyword：按关键词搜索文案，不显示红字", () => {
    useSuperSearchStore.setState({
      aiExplanation: "按关键词搜索",
      warnings: ["未能理解搜索条件，已按关键词搜索。"],
      parseStatus: "keyword",
    });
    renderBar();
    expect(screen.getByText(/已按关键词搜索：海边/)).toBeTruthy();
  });

  it("aiError：请求失败显示红字、保留条件提示和按原文搜索按钮", () => {
    useSuperSearchStore.setState({ aiError: "云端请求失败: 401" });
    renderBar();
    expect(screen.getByText(/本次未应用；当前条件与结果保持不变/)).toBeTruthy();
    expect(screen.getByText("按原文搜索")).toBeTruthy();
  });

  it("等待模型时显示阶段、耗时和可操作的取消控件", () => {
    useSuperSearchStore.setState({ aiLoading: true, aiRequestId: "req-1", aiPhase: "requesting", aiStartedAt: Date.now() - 5200, aiElapsedMs: 5200 });
    const cancel = vi.fn();
    useSuperSearchStore.setState({ cancelAiSearch: cancel });
    renderBar();
    expect(screen.getByRole("status").textContent).toContain("正在请求并等待模型响应");
    expect(screen.getByRole("status").textContent).toContain("5 秒");
    screen.getByRole("button", { name: "取消解析" }).click();
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("取消等待期间说明当前请求正在结束", () => {
    useSuperSearchStore.setState({ aiRequestId: "req-2", aiPhase: "cancelling", aiCancelPending: true, aiElapsedMs: 8000 });
    renderBar();
    expect(screen.getByRole("status").textContent).toContain("已停止接收结果，正在结束当前请求");
    expect(screen.getByRole("button", { name: "正在停止" })).toBeTruthy();
  });
});
