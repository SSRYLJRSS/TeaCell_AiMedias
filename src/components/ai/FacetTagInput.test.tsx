import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { fireEvent, screen, act } from "@testing-library/react";
import FacetTagInput, { clearRecentTagCache } from "@/components/ai/FacetTagInput";
import { searchTagCandidates, recentTagOps } from "@/api/tags";
import type { TagOp } from "@/types/asset";

import { actRender } from "@/test/render";
/** 受控父级：让输入值随用户输入更新（还原 Workbench 的 editing 状态） */
function Harness({ facetKey, onCommit }: { facetKey: string; onCommit: (name: string) => void }) {
  const [v, setV] = useState("");
  return <FacetTagInput facetKey={facetKey} value={v} onValueChange={setV} onCommit={onCommit} />;
}

const mkCandidate = (over: Record<string, unknown> = {}) => ({
  id: 1,
  name: "海边",
  canonicalName: "海边",
  normalizedName: "海边",
  facetKey: "scene",
  parentId: null,
  status: "active" as const,
  isSystem: false,
  isPreset: false,
  sortOrder: 0,
  assetCount: 0,
  totalCount: 0,
  aliases: [],
  path: "场景/地点 / 海边",
  ...over,
});

const mkOp = (tagName: string, op: "add" | "remove" = "add"): TagOp => ({
  id: 1,
  assetId: 1,
  tagId: 1,
  op,
  actor: "manual",
  batchId: null,
  createdAt: 0,
  tagName,
  assetName: "a.jpg",
});

vi.mock("@/api/tags", () => ({
  searchTagCandidates: vi.fn(),
  recentTagOps: vi.fn().mockResolvedValue([]),
}));

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  clearRecentTagCache();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("FacetTagInput（§9.5 录入顺序）", () => {
  it("输入按当前 facetKey 搜索规范名/别名候选（防抖 200ms）", async () => {
    vi.mocked(searchTagCandidates).mockResolvedValue([mkCandidate() as never]);
    await actRender(<Harness facetKey="scene" onCommit={vi.fn()} />);
    const input = screen.getByPlaceholderText("+ 加标签");
    fireEvent.change(input, { target: { value: "海" } });
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    expect(searchTagCandidates).toHaveBeenCalledWith("scene", "海");
    const options = screen.getAllByRole("option");
    expect(options.length).toBe(1);
    expect(options[0]).toHaveTextContent("海边");
    expect(options[0]).toHaveTextContent("场景/地点 / 海边");
  });

  it("选择候选提交规范名并清空输入", async () => {
    vi.mocked(searchTagCandidates).mockResolvedValue([mkCandidate() as never]);
    const onCommit = vi.fn();
    await actRender(<Harness facetKey="scene" onCommit={onCommit} />);
    const input = screen.getByPlaceholderText("+ 加标签");
    fireEvent.change(input, { target: { value: "海" } });
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    fireEvent.mouseDown(screen.getByRole("option"));
    expect(onCommit).toHaveBeenCalledWith("海边");
    // 提交后输入被清空，会触发「最近使用标签」加载；不等它在测试内落地，setRecent 就落在测试之外（act 警告）。
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("Enter 提交当前输入（明确确认才加入候选）", async () => {
    vi.mocked(searchTagCandidates).mockResolvedValue([]);
    const onCommit = vi.fn();
    await actRender(<Harness facetKey="custom" onCommit={onCommit} />);
    const input = screen.getByPlaceholderText("+ 加标签");
    fireEvent.change(input, { target: { value: "旅行" } });
    await act(async () => {
      vi.advanceTimersByTime(200);
    });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onCommit).toHaveBeenCalledWith("旅行");
    // 同上：清空输入触发的「最近使用标签」加载要在测试内落地。
    await act(async () => {
      await Promise.resolve();
    });
  });

  it("readOnly 时不搜索候选、禁用输入", async () => {
    await actRender(<FacetTagInput facetKey="scene" value="" onValueChange={vi.fn()} onCommit={vi.fn()} readOnly />);
    const input = screen.getByPlaceholderText("+ 加标签");
    expect(input).toBeDisabled();
    expect(searchTagCandidates).not.toHaveBeenCalled();
  });

  it("输入为空且聚焦时展示最近使用标签", async () => {
    vi.mocked(recentTagOps).mockResolvedValue([mkOp("海边"), mkOp("夜景"), mkOp("海边")]);
    const onCommit = vi.fn();
    const { getByPlaceholderText } = await actRender(<Harness facetKey="scene" onCommit={onCommit} />);
    const input = getByPlaceholderText("+ 加标签");
    fireEvent.focus(input);
    await act(async () => {
      await Promise.resolve(); // 等 recentTagOps 解出
    });
    const items = screen.getAllByRole("option");
    // 去重：海边 / 夜景（去掉重复的海边）
    expect(items).toHaveLength(2);
    fireEvent.mouseDown(items[0]);
    expect(onCommit).toHaveBeenCalledWith("海边");
  });
});
