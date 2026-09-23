/** Phase 1/2 契约测试（TS 侧 · §4.1a / §4.8 / §4.4）—— 冻结执行契约的可检验定义。 */
import { describe, expect, it } from "vitest";
import type { QueryExpr } from "@/types/queryExpr";
import type { LeafCond } from "@/types/queryExpr";
import type { ResolvedSearchQuery } from "@/types/asset";
import type { SearchPlanV3, ShouldClause } from "@/types/superSearch";
import { appendPlanMerge, MAX_SHOULD_CLAUSES, migratePlanV3, normalizeSearchPlan, normalizeShouldByPosition, PLAN_SCHEMA_VERSION, reorderShould, resolvedQueryToPlan, weightForShouldPosition } from "./planUtils";

function baseQuery(): ResolvedSearchQuery {
  return {
    search: "",
    assetType: "all",
    untaggedOnly: false,
    facetFilters: [],
    excludeTagIds: [],
    metadataFilters: [],
    sortBy: "created_at",
    sortDir: "desc",
  };
}

function tagCond(facetKey: string, tagIds: number[], extra?: Partial<LeafCond & { type: "tag" }>): LeafCond {
  return {
    type: "tag",
    facetKey,
    tagIds,
    mode: extra?.mode ?? "any",
    includeDescendants: extra?.includeDescendants ?? true,
  };
}

function tagLeaf(facetKey: string, tagIds: number[], extra?: Partial<LeafCond & { type: "tag" }>): QueryExpr {
  return { op: "leaf", cond: tagCond(facetKey, tagIds, extra) };
}

/** 把 filter/mustNot 顶层叶子摊平（leaf → 自身；and/or → 各自的 children），便于断言。 */
function exprLeaves(e: QueryExpr | null): LeafCond[] {
  if (!e) return [];
  const children = e.op === "and" || e.op === "or" ? e.children : [e];
  return children.filter((c): c is { op: "leaf"; cond: LeafCond } => c.op === "leaf").map((c) => c.cond);
}

function soleCond(e: QueryExpr | null): LeafCond | null {
  if (!e || e.op !== "leaf") return null;
  return e.cond;
}

function opOf(e: QueryExpr | null): string {
  return e ? e.op : "null";
}

function planWith(filter: QueryExpr | null, mustNot: QueryExpr | null, should: ShouldClause[] = [], ranking: SearchPlanV3["ranking"] = { type: "relevance" }): SearchPlanV3 {
  return {
    planSchemaVersion: PLAN_SCHEMA_VERSION,
    normalizationVersion: 1,
    compilerVersion: 1,
    filter,
    mustNot,
    should,
    minimumShouldMatch: 0,
    retrievers: { retrievers: [], fusion: "rrf" },
    ranking,
  };
}

describe("§4.1a resolvedQueryToPlan", () => {
  it("covers_all_flat_fields：六个扁平字段逐个出现在 plan 里", () => {
    const q: ResolvedSearchQuery = {
      ...baseQuery(),
      search: "海边 ",
      assetType: "video",
      untaggedOnly: true,
      facetFilters: [{ facetKey: "scene", tagIds: [7], mode: "any", includeDescendants: true }],
      metadataFilters: [
        { key: "file_size", op: "gte", value: 5_242_880, values: undefined, min: undefined, max: undefined },
      ],
    };
    const plan = resolvedQueryToPlan(q);
    const conds = exprLeaves(plan.filter);
    expect(conds.some((c) => c.type === "search" && (c as { value: string }).value === "海边")).toBe(true);
    expect(conds.some((c) => c.type === "assetType" && (c as { value: string }).value === "video")).toBe(true);
    expect(conds.some((c) => c.type === "untagged")).toBe(true);
    expect(conds.some((c) => c.type === "tag" && (c as { tagIds: number[] }).tagIds.includes(7))).toBe(true);
    expect(conds.some((c) => c.type === "metadata" && (c as { filter: { op: string } }).filter.op === "gte")).toBe(true);
    expect(plan.ranking).toEqual({ type: "field", key: "created_at", dir: "desc" });
    // 空查询 → filter/mustNot 均为空，但 plan 仍可执行（全库列表）
    const empty = resolvedQueryToPlan({ ...baseQuery(), search: "   " });
    expect(empty.filter).toBeNull();
    expect(empty.mustNot).toBeNull();
  });

  it("exclude_tag_ids_become_positive_tag_in_must_not：排除是正向 Tag 进 mustNot（不是 excludeTag）", () => {
    const plan = resolvedQueryToPlan({ ...baseQuery(), search: "人像", excludeTagIds: [11, 22] });
    const filterConds = exprLeaves(plan.filter);
    expect(filterConds.some((c) => c.type === "excludeTag")).toBe(false);
    expect(opOf(plan.mustNot)).toBe("or");
    const leaves = exprLeaves(plan.mustNot);
    expect(leaves).toHaveLength(2);
    for (const l of leaves) {
      expect(l.type).toBe("tag");
      expect((l as { mode?: string }).mode).toBe("any");
      expect((l as { includeDescendants?: boolean }).includeDescendants).toBe(true);
    }
    expect((leaves[0] as { tagIds: number[] }).tagIds).toEqual([11]);
    expect((leaves[1] as { tagIds: number[] }).tagIds).toEqual([22]);
  });

  it("facet_filter_mode_and_descendants_survive：mode=all + includeDescendants=false 往返不丢", () => {
    const plan = resolvedQueryToPlan({
      ...baseQuery(),
      facetFilters: [{ facetKey: "purpose", tagIds: [3], mode: "all", includeDescendants: false }],
    });
    const cond = soleCond(plan.filter) as { type: "tag"; mode: string; includeDescendants: boolean };
    expect(cond.type).toBe("tag");
    expect(cond.mode).toBe("all");
    expect(cond.includeDescendants).toBe(false);
  });
});

describe("§4.8 AI 追加模式合并", () => {
  const A = tagLeaf("scene", [1]);
  const B = tagLeaf("scene", [2]);
  const C = tagLeaf("scene", [3]);

  it("append_and_merges_filter_keeps_inner_or", () => {
    const oldPlan = planWith({ op: "or", children: [A, B] }, null);
    const { plan } = appendPlanMerge(oldPlan, planWith(C, null));
    const f = plan.filter as { op: "and"; children: QueryExpr[] };
    const kids: QueryExpr[] = f.children;
    expect(kids).toHaveLength(2);
    const or = kids.find((k) => k.op === "or") as { children: QueryExpr[] };
    expect(or.children).toHaveLength(2);
    expect(or.children.some((k) => (k as { op: "leaf"; cond: { tagIds: number[] } }).cond.tagIds.includes(1))).toBe(true);
    // C 不被拍平进 or
    expect(kids.some((k) => k.op === "leaf" && (k as { op: "leaf"; cond: { tagIds: number[] } }).cond.tagIds.includes(3))).toBe(true);
  });

  it("append_or_merges_must_not：任一命中即排除，两组排除合并成 or", () => {
    const { plan } = appendPlanMerge(planWith(null, A), planWith(null, B));
    const mn = plan.mustNot as { op: "or"; children: QueryExpr[] };
    expect(mn.op).toBe("or");
    expect(mn.children).toHaveLength(2);
    // 重复排除去重
    const again = appendPlanMerge(planWith(null, A), planWith(null, A));
    expect(again.plan.mustNot).not.toBeNull();
    expect((again.plan.mustNot as { op: "leaf"; cond: LeafCond }).cond.type).toBe("tag");
  });

  it("append_concats_should_and_caps_at_12_by_position", () => {
    const make = (n: number, w: number, base: number) =>
      Array.from({ length: n }, (_, i) => ({
        cond: tagCond("scene", [base + i]),
        weight: w,
        label: `w${w}-${i}`,
        evidence: null,
      }));
    const { plan, warnings } = appendPlanMerge(planWith(null, null, make(7, 0.5, 1)), planWith(null, null, make(8, 2.0, 100)));
    expect(plan.should).toHaveLength(MAX_SHOULD_CLAUSES);
    expect(plan.should.map((s) => s.weight)).toEqual([2, 2, 2, 2, 1, 1, 1, 1, 0.5, 0.5, 0.5, 0.5]);
    expect(warnings.some((w) => w.zone === "should" && w.message.includes("12 条"))).toBe(true);
  });

  it("append_never_changes_user_ranking", () => {
    const user = planWith(A, null, [], { type: "field", key: "taken_at", dir: "asc" });
    const ai = planWith(B, null, [], { type: "relevance" });
    const { plan, warnings } = appendPlanMerge(user, ai);
    expect(plan.ranking).toEqual({ type: "field", key: "taken_at", dir: "asc" });
    expect(warnings.some((w) => w.message.includes("排序方式未被采纳"))).toBe(true);
  });

  it("evidence 随各自 ShouldClause 保留（不拼进 label）", () => {
    const oldPlan = planWith(A, null, [
      { cond: tagCond("scene", [1]), weight: 1, label: "蓝天", evidence: "最好有蓝天" },
    ]);
    const { plan } = appendPlanMerge(oldPlan, planWith(B, null, [{ cond: tagCond("scene", [2]), weight: 2, label: "户外", evidence: "最好是户外" }]));
    expect(plan.should).toHaveLength(2);
    expect(plan.should[0].evidence).toBe("最好有蓝天");
    expect(plan.should[1].evidence).toBe("最好是户外");
    expect(plan.should[0].label).toBe("蓝天");
  });
});

describe("§4.4 版本矩阵", () => {
  it("future_plan_discarded_on_hydrate：未来版本 → null（丢弃）", () => {
    const plan = planWith(null, null);
    expect(migratePlanV3({ ...plan, planSchemaVersion: PLAN_SCHEMA_VERSION + 1 })).toBeNull();
  });
  it("persisted_plan_hydrates_and_executes：当前版本往返不丢、版本号对齐当前常量", () => {
    const plan = planWith(tagLeaf("scene", [1]), null, [{ cond: tagCond("scene", [2]), weight: 1, label: "x", evidence: null }]);
    const migrated = migratePlanV3(plan);
    expect(migrated).not.toBeNull();
    expect(migrated!.planSchemaVersion).toBe(PLAN_SCHEMA_VERSION);
    expect(JSON.stringify(migrated!.filter)).toBe(JSON.stringify(plan.filter));
    expect(migrated!.should).toHaveLength(1);
  });
});

describe("优先区位置归一化纯函数", () => {
  it("三分桶覆盖 N=0..13，超过上限按可见顺序截断", () => {
    expect([0, 1, 2, 3, 4, 5, 12, 13].map((n) => Array.from({ length: n }, (_, i) => weightForShouldPosition(i, n)))).toEqual([
      [],
      [2],
      [2, 1],
      [2, 1, 0.5],
      [2, 2, 1, 0.5],
      [2, 2, 1, 1, 0.5],
      [2, 2, 2, 2, 1, 1, 1, 1, 0.5, 0.5, 0.5, 0.5],
      [2, 2, 2, 2, 2, 1, 1, 1, 1, 0.5, 0.5, 0.5, 0.5],
    ]);
    const clauses = Array.from({ length: 13 }, (_, i) => ({ cond: tagCond("scene", [i]), weight: 99, label: `c${i}` }));
    expect(normalizeShouldByPosition(clauses)).toHaveLength(MAX_SHOULD_CLAUSES);
    expect(normalizeShouldByPosition(clauses).map((s) => s.label)).toEqual(Array.from({ length: 12 }, (_, i) => `c${i}`));
  });

  it("覆盖非法旧 weight、固定 min=0、保持输入不可变且幂等", () => {
    const input = Array.from({ length: 3 }, (_, i) => ({ cond: tagCond("scene", [i + 1]), weight: Number.NaN, label: `x${i}`, evidence: i === 1 ? "原文" : null }));
    const plan = planWith(null, null, input);
    plan.minimumShouldMatch = 99;
    const before = JSON.stringify(plan);
    const normalized = normalizeSearchPlan(plan);
    expect(JSON.stringify(plan)).toBe(before);
    expect(normalized.minimumShouldMatch).toBe(0);
    expect(normalized.should.map((s) => s.weight)).toEqual([2, 1, 0.5]);
    expect(normalized.should[1].evidence).toBe("原文");
    expect(normalizeSearchPlan(normalized)).toEqual(normalized);
  });

  it("hydrate legacy flat metadata leaves into the nested wire contract", () => {
    const flat = {
      type: "metadata",
      key: "file_size",
      op: "gte",
      value: 5 * 1024 * 1024,
    } as unknown as LeafCond;
    const plan = planWith({ op: "leaf", cond: flat }, null, [{ cond: flat, weight: 1, label: "文件大小" }]);
    const normalized = normalizeSearchPlan(plan);
    expect(normalized.filter).toEqual({
      op: "leaf",
      cond: { type: "metadata", filter: { key: "file_size", op: "gte", value: 5 * 1024 * 1024 } },
    });
    expect(normalized.should[0]?.cond).toEqual({
      type: "metadata",
      filter: { key: "file_size", op: "gte", value: 5 * 1024 * 1024 },
    });
  });

  it("保留尚未加载 numeric domain 时的色相跨零区间", () => {
    const hue: ShouldClause = {
      cond: { type: "metadata", filter: { key: "dominant_hue", op: "between", min: 345, max: 15 } },
      weight: 1,
      label: "红色",
    };
    expect(normalizeShouldByPosition([hue])).toHaveLength(1);
    expect(normalizeShouldByPosition([hue])[0]?.cond).toEqual(hue.cond);
  });

  it("reorderShould 返回新数组，支持首尾和相同位置", () => {
    const source = ["a", "b", "c"];
    expect(reorderShould(source, 2, 0)).toEqual(["c", "a", "b"]);
    expect(reorderShould(source, 0, 2)).toEqual(["b", "c", "a"]);
    const same = reorderShould(source, 1, 1);
    expect(same).toEqual(source);
    expect(same).not.toBe(source);
    expect(reorderShould(source, -1, 0)).toEqual(source);
    expect(reorderShould(source, 0, 9)).toEqual(source);
  });
});
