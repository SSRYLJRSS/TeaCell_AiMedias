/** queryExprUtils（FB5-05 §9.5.6/§9.6.1）：normalize / removeExprAtPath / flattenExprForDisplay。 */
import { describe, expect, it } from "vitest";
import {
  appendToExpr,
  flattenExprForDisplay,
  normalizeExpr,
  removeExprAtPath,
  mergeQueryExpr,
} from "@/utils/queryExprUtils";
import type { QueryExpr } from "@/types/queryExpr";

const tagLeaf = (facetKey: string, tagIds: number[]): QueryExpr => ({
  op: "leaf",
  cond: { type: "tag", facetKey, tagIds, mode: "any", includeDescendants: true },
});
const searchLeaf = (value: string, scope: "all" | "content" = "all"): QueryExpr => ({
  op: "leaf",
  cond: { type: "search", value, scope },
});

describe("normalizeExpr（§9.5.6）", () => {
  it("单子节点组折叠", () => {
    expect(normalizeExpr({ op: "and", children: [searchLeaf("海边")] })).toEqual(searchLeaf("海边"));
  });
  it("空组删除 → undefined", () => {
    expect(normalizeExpr({ op: "and", children: [] })).toBeUndefined();
    expect(normalizeExpr({ op: "or", children: [{ op: "and", children: [] }] })).toBeUndefined();
  });
  it("连续相同 AND 扁平化", () => {
    const expr: QueryExpr = {
      op: "and",
      children: [
        { op: "and", children: [searchLeaf("a"), searchLeaf("b")] },
        searchLeaf("c"),
      ],
    };
    expect(normalizeExpr(expr)).toEqual({ op: "and", children: [searchLeaf("a"), searchLeaf("b"), searchLeaf("c")] });
  });
  it("重复 leaf 去重", () => {
    const expr: QueryExpr = { op: "and", children: [searchLeaf("海边"), searchLeaf("海边")] };
    expect(normalizeExpr(expr)).toEqual(searchLeaf("海边"));
  });
});

describe("removeExprAtPath（§9.6.1）", () => {
  it("删除叶子后单子节点组折叠", () => {
    const expr: QueryExpr = { op: "and", children: [searchLeaf("海边"), tagLeaf("subject", [1])] };
    expect(removeExprAtPath(expr, [1])).toEqual(searchLeaf("海边"));
  });
  it("删除 OR 子组保留其余 OR 组", () => {
    const expr: QueryExpr = {
      op: "or",
      children: [
        { op: "and", children: [tagLeaf("lighting", [1])] },
        { op: "and", children: [tagLeaf("lighting", [2])] },
      ],
    };
    const next = removeExprAtPath(expr, [0]);
    expect(next).toEqual(tagLeaf("lighting", [2]));
  });
  it("删除 NOT 整棵清空", () => {
    expect(removeExprAtPath({ op: "not", child: tagLeaf("lighting", [1]) }, [0])).toBeUndefined();
  });
});

describe("appendToExpr（2026-09-06 跨区移动）", () => {
  it("空根 → 直接返回节点", () => {
    expect(appendToExpr(undefined, searchLeaf("海边"), "or")).toEqual(searchLeaf("海边"));
  });
  it("根组 op 一致 → 直接 append 为根组的一项（按目标根组连接词参与）", () => {
    const root: QueryExpr = { op: "or", children: [searchLeaf("海边"), searchLeaf("日落")] };
    expect(appendToExpr(root, searchLeaf("夜景"), "or")).toEqual({
      op: "or",
      children: [searchLeaf("海边"), searchLeaf("日落"), searchLeaf("夜景")],
    });
  });
  it("根组 op 不一致 → 包一层 op 组（normalize 再拍平同 op）", () => {
    const root: QueryExpr = searchLeaf("海边");
    expect(appendToExpr(root, searchLeaf("日落"), "and")).toEqual({
      op: "and",
      children: [searchLeaf("海边"), searchLeaf("日落")],
    });
  });
});

describe("flattenExprForDisplay（§9.6.1）", () => {
  const tags = [
    { facetKey: "lighting", text: "夜景", tagId: 1, path: "" },
    { facetKey: "subject", text: "树", tagId: 2, path: "" },
  ];
  it("AND 顶层叶无组标签；标签名取自 resolvedTags", () => {
    const chips = flattenExprForDisplay({ op: "and", children: [tagLeaf("lighting", [1]), tagLeaf("subject", [2])] }, tags);
    expect(chips.map((c) => c.label)).toEqual(["光线/时间：夜景", "主体对象：树"]);
    expect(chips.every((c) => !c.group)).toBe(true);
  });
  it("OR 根按组标注「任一组 N」", () => {
    const expr: QueryExpr = {
      op: "or",
      children: [
        { op: "and", children: [tagLeaf("lighting", [1])] },
        { op: "and", children: [tagLeaf("subject", [2])] },
      ],
    };
    const chips = flattenExprForDisplay(expr, tags);
    expect(chips.find((c) => c.label.includes("夜景"))?.group).toBe("任一组 1");
    expect(chips.find((c) => c.label.includes("树"))?.group).toBe("任一组 2");
  });
  it("NOT 叶显示「排除：」", () => {
    const chips = flattenExprForDisplay({ op: "not", child: tagLeaf("lighting", [1]) }, tags);
    expect(chips.map((c) => c.label)).toEqual(["排除：夜景"]);
  });
  it("content 搜索显示「内容：」、fileName 显示「文件名：」", () => {
    const chips = flattenExprForDisplay(
      {
        op: "and",
        children: [
          { op: "leaf", cond: { type: "search", value: "银杏树", scope: "content" } },
          { op: "leaf", cond: { type: "search", value: "IMG_1097", scope: "fileName" } },
        ],
      },
      [],
    );
    expect(chips.map((c) => c.label)).toEqual(["内容：银杏树", "文件名：IMG_1097"]);
  });
  it("兼容旧 Rust 返回的扁平 metadata leaf", () => {
    const legacy = {
      op: "leaf",
      cond: { type: "metadata", key: "file_size", op: "gte", value: 5 * 1024 * 1024 },
    } as unknown as QueryExpr;
    expect(flattenExprForDisplay(legacy, []).map((c) => c.label)).toEqual(["文件大小 ≥ 5242880"]);
    expect(normalizeExpr(legacy)).toEqual({
      op: "leaf",
      cond: { type: "metadata", filter: { key: "file_size", op: "gte", value: 5 * 1024 * 1024 } },
    });
  });
});

describe("mergeQueryExpr（§9.6 append）", () => {
  it("两个完整查询组以 AND 合并", () => {
    const a = tagLeaf("lighting", [1]);
    const b = tagLeaf("subject", [2]);
    expect(mergeQueryExpr(a, b)).toEqual({ op: "and", children: [a, b] });
  });
  it("与 undefined 合并返回原组", () => {
    const a = tagLeaf("lighting", [1]);
    expect(mergeQueryExpr(a, undefined)).toEqual(a);
    expect(mergeQueryExpr(undefined, undefined)).toBeUndefined();
  });
});
