/** FilterChips（FB5-05 §9.6.1 + §3.7/§3.10）：三区共用一个 chip 行。
 *  - 必须区（plan.filter / expr 派生）组标签「必须」；删除走 removeAtZonePath("filter", path)；
 *  - 排除区（plan.mustNot）组标签「排除」；优先区（plan.should）组标签「优先」；
 *  - 排序 chip 独立 setSort；清除全部走 clearConditions；
 *  - 无 plan（纯手动链路）退回扁平 query 渲染。 */
import { afterEach, describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import FilterChips from "@/components/supersearch/FilterChips";
import { useSuperSearchStore } from "@/stores/superSearchStore";
import { useTagStore } from "@/stores/tagStore";
import type { QueryExpr } from "@/types/queryExpr";
import type { TagNode } from "@/types/tag";

const tagLeaf = (facetKey: string, tagIds: number[]): QueryExpr => ({
  op: "leaf",
  cond: { type: "tag", facetKey, tagIds, mode: "any", includeDescendants: true },
});

const searchLeaf = (value: string, scope: "all" | "content" | "fileName" = "all"): QueryExpr => ({
  op: "leaf",
  cond: { type: "search", value, scope },
});

const treeTag = (id: number, name: string, facetKey: string): TagNode => ({
  tag: {
    id,
    name,
    canonicalName: name,
    normalizedName: name,
    facetKey,
    parentId: null,
    status: "active",
    isSystem: false,
    isPreset: false,
    sortOrder: 0,
    assetCount: 0,
    totalCount: 0,
    aliases: [],
    path: name,
    facetEffective: true,
  },
  children: [],
});

function setExprState(expr: QueryExpr | undefined, resolvedTags: { facetKey: string; text: string; tagId: number; path?: string }[] = []) {
  // §3.7：走真实 store action（setExpr → plan.filter），保证 plan/expr 一致（换源后 expr 是派生视图）
  useSuperSearchStore.getState().setExpr(expr);
  useSuperSearchStore.setState({
    resolvedTags: resolvedTags.map((t) => ({ ...t, path: t.path ?? "" })),
    aiExplanation: null,
    warnings: [],
  });
}

afterEach(() => {
  useTagStore.setState({ tree: [], facets: [] });
});

describe("FilterChips（FB5-05 §9.6.1 expr 驱动）", () => {
  it("AND 树显示可读标签 chips：主体：建筑 + 色彩：红色", () => {
    setExprState(
      { op: "and", children: [tagLeaf("subject", [1]), tagLeaf("color", [2])] },
      [
        { facetKey: "subject", text: "建筑", tagId: 1 },
        { facetKey: "color", text: "红色", tagId: 2 },
      ],
    );
    render(<FilterChips />);
    expect(screen.getByText("主体对象：建筑")).toBeInTheDocument();
    expect(screen.getByText("色彩：红色")).toBeInTheDocument();
  });

  it("OR 根按组显示，组标签统一「必须」（§3.10）", () => {
    setExprState({
      op: "or",
      children: [
        { op: "and", children: [tagLeaf("lighting", [1]), tagLeaf("subject", [2])] },
        { op: "and", children: [tagLeaf("lighting", [3]), tagLeaf("scene", [4])] },
      ],
    }, [
      { facetKey: "lighting", text: "夜间", tagId: 1 },
      { facetKey: "subject", text: "树", tagId: 2 },
      { facetKey: "lighting", text: "白天", tagId: 3 },
      { facetKey: "scene", text: "建筑", tagId: 4 },
    ]);
    render(<FilterChips />);
    // 三区统一标签：全部叶子都是「必须」组
    expect(screen.getAllByText("必须").length).toBeGreaterThanOrEqual(4);
    expect(screen.getAllByText(/夜间|树|白天|建筑/).length).toBeGreaterThanOrEqual(4);
  });

  it("NOT 叶显示「排除：…」", () => {
    setExprState({ op: "not", child: tagLeaf("lighting", [5]) }, [
      { facetKey: "lighting", text: "夜景", tagId: 5 },
    ]);
    render(<FilterChips />);
    expect(screen.getByText("排除：夜景")).toBeInTheDocument();
  });

  it("content 范围搜索 leaf 显示「内容：…」（§9.4）", () => {
    setExprState(searchLeaf("银杏树", "content"));
    render(<FilterChips />);
    expect(screen.getByText("内容：银杏树")).toBeInTheDocument();
  });

  it("删除 chip 走 removeAtZonePath（filter 区）只摘除该节点，保留其余条件", () => {
    setExprState(
      { op: "and", children: [tagLeaf("subject", [1]), tagLeaf("color", [2])] },
      [
        { facetKey: "subject", text: "建筑", tagId: 1 },
        { facetKey: "color", text: "红色", tagId: 2 },
      ],
    );
    render(<FilterChips />);
    fireEvent.click(screen.getByRole("button", { name: "取消 色彩：红色" }));
    const st = useSuperSearchStore.getState();
    expect(st.expr).toEqual(tagLeaf("subject", [1]));
    // 剩余名称映射保留
    expect(st.resolvedTags.map((t) => t.tagId)).toEqual([1]);
  });

  it("删除 NOT 叶后 expr 清空（无剩余条件）", () => {
    setExprState({ op: "not", child: tagLeaf("lighting", [5]) }, [
      { facetKey: "lighting", text: "夜景", tagId: 5 },
    ]);
    render(<FilterChips />);
    fireEvent.click(screen.getByRole("button", { name: "取消 排除：夜景" }));
    expect(useSuperSearchStore.getState().expr).toBeUndefined();
  });

  it("未知 tagId 退回「标签#id」", () => {
    setExprState(tagLeaf("custom", [99]));
    render(<FilterChips />);
    expect(screen.getByText("自定义：标签#99")).toBeInTheDocument();
  });

  it("手动标签从实时标签树解析名称，覆盖必须/排除/优先三个区域", () => {
    useTagStore.setState({
      tree: [treeTag(101, "建筑", "subject"), treeTag(102, "夜景", "lighting"), treeTag(103, "女孩", "subject")],
    });
    const expr = tagLeaf("subject", [101]);
    useSuperSearchStore.getState().setExpr(expr);
    useSuperSearchStore.setState({
      plan: {
        planSchemaVersion: 3,
        normalizationVersion: 1,
        compilerVersion: 1,
        filter: expr,
        mustNot: tagLeaf("lighting", [102]),
        should: [{ cond: { type: "tag", facetKey: "subject", tagIds: [103], mode: "any", includeDescendants: true }, weight: 1, label: "" }],
        minimumShouldMatch: 0,
        retrievers: { retrievers: [] },
        ranking: { type: "field", key: "created_at", dir: "desc" },
      },
    });
    render(<FilterChips />);
    expect(screen.getByText("主体对象：建筑")).toBeInTheDocument();
    expect(screen.getByText("光线/时间：夜景")).toBeInTheDocument();
    expect(screen.getByText("标签：女孩")).toBeInTheDocument();
  });

  it("排序 chip 独立：调用 setSort（不碰 expr）", () => {
    useSuperSearchStore.setState({ query: { ...useSuperSearchStore.getState().query, sortBy: "size", sortDir: "desc" } });
    setExprState(tagLeaf("subject", [1]));
    render(<FilterChips />);
    fireEvent.click(screen.getByRole("button", { name: "取消 排序：size desc" }));
    const st = useSuperSearchStore.getState();
    expect(st.query.sortBy).toBe("created_at");
    expect(st.expr).toEqual(tagLeaf("subject", [1]));
    expect(st.query.sortBy).toBe("created_at");
  });

  it("清除全部调用 clearConditions：expr 与扁平筛选同时清空", () => {
    useSuperSearchStore.setState({ query: { ...useSuperSearchStore.getState().query, search: "海边" } });
    setExprState({ op: "and", children: [searchLeaf("海边"), tagLeaf("subject", [1])] });
    render(<FilterChips />);
    fireEvent.click(screen.getByRole("button", { name: "清除全部" }));
    const st = useSuperSearchStore.getState();
    expect(st.expr).toBeUndefined();
    expect(st.query.search).toBe("");
  });

  it("无 expr 时（纯手动链路）退回扁平 query 渲染，删除仍可用", () => {
    setExprState(undefined);
    useSuperSearchStore.setState({
      query: { ...useSuperSearchStore.getState().query, search: "海边" },
    });
    render(<FilterChips />);
    expect(screen.getByText("关键词：海边")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "取消 关键词：海边" }));
    expect(useSuperSearchStore.getState().query.search).toBe("");
  });
});

describe("U-5 加分 chips", () => {
  it("plan.should 渲染为「优先」组 chip，删除按索引调用 removePlanShould（§3.10）", () => {
    const expr = searchLeaf("海边");
    useSuperSearchStore.getState().setExpr(expr);
    useSuperSearchStore.setState({
      resolvedTags: [{ facetKey: "subject", text: "女孩", tagId: 2, path: "" }],
      plan: {
        planSchemaVersion: 3, normalizationVersion: 1, compilerVersion: 1,
        filter: expr, mustNot: null,
        should: [{ cond: { type: "tag", facetKey: "subject", tagIds: [2], mode: "any", includeDescendants: true }, weight: 1, label: "" }],
        minimumShouldMatch: 0,
        retrievers: { retrievers: [] },
        ranking: { type: "field", key: "created_at", dir: "desc" },
      },
    });
    render(<FilterChips />);
    // chip 组标签「优先」+ 可读文案
    expect(screen.getByText("优先")).toBeInTheDocument();
    const chip = screen.getByText("标签：女孩");
    expect(chip).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "取消 标签：女孩" }));
    const st = useSuperSearchStore.getState();
    expect(st.plan?.should).toHaveLength(0);
    expect(st.expr).toEqual(expr);
  });
});
