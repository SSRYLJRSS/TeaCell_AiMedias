import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSuperSearchStore } from "@/stores/superSearchStore";
import { listSuperAssets, listSuperAssetIdsByPlan } from "@/api/superSearch";
import { useSelectionStore } from "@/stores/selectionStore";
import type { Asset } from "@/types/asset";
import type { LeafCond, QueryExpr } from "@/types/queryExpr";
import type { SearchPlanV3 } from "@/types/superSearch";

vi.mock("@/api/assets", () => ({
  listAssets: vi.fn(),
  listAssetIds: vi.fn(),
}));
vi.mock("@/api/superSearch", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/superSearch")>();
  return {
    ...actual,
    aiParseSearchQuery: vi.fn(),
    listSuperAssets: vi.fn().mockResolvedValue({ items: [], total: 0, hasMore: false, warnings: [] }),
    listSuperAssetIdsByPlan: vi.fn(),
  };
});

const mkAsset = (id: number): Asset => ({
  id, filePath: `d:/p/a${id}.jpg`, fileName: `a${id}.jpg`, fileExt: "jpg",
  fileSize: 100, mimeType: "image/jpeg", width: 800, height: 600,
  durationMs: null, videoCodec: null, audioCodec: null, takenAt: null,
  createdAt: id, modifiedAt: id, hash: null, placeholderPath: null,
  hdThumbnailPath: null, camera: null, lens: null, iso: null, aperture: null,
  shutter: null, focal: null, tags: [],
});

const tagCond = (tagId: number, facetKey = "subject"): LeafCond => ({
  type: "tag", facetKey, tagIds: [tagId], mode: "any", includeDescendants: true,
});

const tagLeaf = (tagId: number, facetKey = "subject"): QueryExpr => ({
  op: "leaf",
  cond: tagCond(tagId, facetKey),
});

const emptyPlan = (filter: QueryExpr | null = null, extra?: Partial<SearchPlanV3>): SearchPlanV3 => ({
  planSchemaVersion: 3, normalizationVersion: 1, compilerVersion: 1,
  filter, mustNot: null, should: [], minimumShouldMatch: 0,
  retrievers: { retrievers: [], fusion: "rrf" },
  ranking: { type: "field", key: "created_at", dir: "desc" },
  ...extra,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listSuperAssets).mockResolvedValue({ items: [], total: 0, hasMore: false, warnings: [] });
  useSelectionStore.setState({ selected: new Set(), anchorIndex: null, truncated: false, selectionTotal: 0 });
  // 恢复真实 refresh：有的测试用 spy 替换了 store.refresh，不恢复会污染后续测试
  const { refresh } = useSuperSearchStore.getInitialState();
  useSuperSearchStore.setState({ planRevision: 0, expr: undefined, plan: null, items: [], total: 0, loading: false, error: null, aiError: null, refresh });
});

describe("superSearchStore", () => {
  it("默认全库、非回收站、入库时间降序", () => {
    const q = useSuperSearchStore.getState().query;
    expect(q.assetType).toBe("all");
    expect(q.untaggedOnly).toBe(false);
    expect(q.sortBy).toBe("created_at");
    expect(q.sortDir).toBe("desc");
    expect(q.metadataFilters).toEqual([]);
  });

  it("查询变化清空选中并刷新（列表执行走 plan）", async () => {
    vi.mocked(listSuperAssets).mockResolvedValue({ items: [mkAsset(1)], total: 1, hasMore: false, warnings: [] });
    useSelectionStore.setState({ selected: new Set([9]), anchorIndex: null });
    useSuperSearchStore.getState().setQuery({ assetType: "image" });
    expect(useSelectionStore.getState().selected.size).toBe(0);
    await vi.waitFor(() => expect(useSuperSearchStore.getState().items.length).toBe(1));
    // §3.7：扁平条件经 resolvedQueryToPlan 写回 plan（唯一事实源）
    const plan = useSuperSearchStore.getState().plan;
    expect(plan?.filter).toEqual({ op: "leaf", cond: { type: "assetType", value: "image" } });
  });

  it("旧请求不覆盖新查询（代际）", async () => {
    let resolve!: (v: { items: Asset[]; total: number; hasMore: boolean }) => void;
    vi.mocked(listSuperAssets).mockReturnValueOnce(new Promise((r) => { resolve = r; }));
    // 第一次 refresh 挂起
    void useSuperSearchStore.getState().refresh();
    // 立刻改查询触发第二次 refresh（mock 立即返回）
    vi.mocked(listSuperAssets).mockResolvedValueOnce({ items: [mkAsset(2)], total: 1, hasMore: false, warnings: [] });
    useSuperSearchStore.getState().setQuery({ search: "x" });
    await vi.waitFor(() => expect(listSuperAssets).toHaveBeenCalledTimes(2), { timeout: 1000 });
    await vi.waitFor(() => expect(useSuperSearchStore.getState().items[0]?.id).toBe(2));
    // 旧响应回来，不得覆盖
    resolve({ items: [mkAsset(1)], total: 1, hasMore: false });
    await Promise.resolve();
    expect(useSuperSearchStore.getState().items[0].id).toBe(2);
  });

  it("fetchAllIds 走 plan 全选命令并返回 PlanIdsResult（不降级成 number[]）", async () => {
    vi.mocked(listSuperAssetIdsByPlan).mockResolvedValue({ ids: [1, 2, 3], total: 3, truncated: false, warnings: [] });
    useSuperSearchStore.getState().setQuery({ search: "海边" });
    const result = await useSuperSearchStore.getState().fetchAllIds();
    expect(result).toEqual({ ids: [1, 2, 3], total: 3, truncated: false, warnings: [] });
    expect(listSuperAssetIdsByPlan).toHaveBeenCalledWith(expect.objectContaining({ filter: expect.anything() }));
  });

  it("clearQuery 恢复默认", () => {
    useSuperSearchStore.getState().setQuery({ assetType: "video", search: "海边" });
    useSuperSearchStore.getState().clearQuery();
    const q = useSuperSearchStore.getState().query;
    expect(q.assetType).toBe("all");
    expect(q.search).toBe("");
  });

  it("setExpr 写入 plan.filter（唯一事实源）并同步 query 字段", () => {
    const expr: QueryExpr = { op: "leaf", cond: { type: "assetType", value: "video" } };
    useSuperSearchStore.getState().setExpr(expr);
    expect(useSuperSearchStore.getState().query.assetType).toBe("video");
    expect(useSuperSearchStore.getState().plan?.filter).toEqual(expr);
    expect(useSuperSearchStore.getState().expr).toEqual(expr);
    // §3.7：手工改扁平条件重建 plan（expr 是 plan.filter 的派生视图，不再独立存在）
    useSuperSearchStore.getState().setQuery({ search: "海边" });
    const plan = useSuperSearchStore.getState().plan;
    // resolvedQueryToPlan 按固定顺序生成叶子（search 在前），AND 子项顺序即此
    expect(plan?.filter).toEqual({
      op: "and",
      children: [
        { op: "leaf", cond: { type: "search", value: "海边" } },
        { op: "leaf", cond: { type: "assetType", value: "video" } },
      ],
    });
    expect(useSuperSearchStore.getState().expr).toEqual(plan?.filter);
  });

  it("扁平字段编辑只替换旧扁平叶子，保留 AI 的嵌套条件", () => {
    const aiGroup: QueryExpr = { op: "or", children: [tagLeaf(10), tagLeaf(11)] };
    const filter: QueryExpr = { op: "and", children: [tagLeaf(1), aiGroup] };
    useSuperSearchStore.getState().setExpr(filter);
    useSuperSearchStore.getState().setQuery({
      facetFilters: [{ facetKey: "subject", tagIds: [2], mode: "any", includeDescendants: true }],
    });
    const next = useSuperSearchStore.getState().plan?.filter;
    expect(next).toEqual({ op: "and", children: [tagLeaf(2), aiGroup] });
  });

  it("AI replace：使用后端返回的 plan 为唯一事实源，query 只同步排序", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    const expr: QueryExpr = { op: "leaf", cond: { type: "search", value: "海边" } };
    vi.mocked(aiParseSearchQuery).mockResolvedValue({
      intent: {
        groups: [
          { assetType: "all", concepts: [{ text: "海边", role: "scene", facetHint: "scene", confidence: 0.95 }], textTerms: [], metadata: [], preferred: [] },
        ],
        exclusions: [],
        sortBy: null,
        sortDir: null,
      },
      expr,
      plan: emptyPlan(expr),
      sortBy: "created_at",
      sortDir: "desc",
      explanation: "按关键词搜索",
      warnings: [],
      parseStatus: "full",
      resolvedTags: [],
    });
    useSuperSearchStore.getState().setExpr({ op: "leaf", cond: { type: "assetType", value: "image" } });
    await useSuperSearchStore.getState().applyAiSearch("海边");
    const st = useSuperSearchStore.getState();
    expect(st.plan?.filter).toEqual(expr);
    expect(st.expr).toEqual(expr);
    expect(st.query.sortBy).toBe("created_at");
    // 扁平 query 不再从 expr 反推（AI 结果以 plan 为准）
    expect(st.query.search).toBe("");
    // §3.7 不变式 9：plan 变更代次 +1
    expect(st.planRevision).toBeGreaterThan(0);
  });

  it("AI append：与现有 plan 按 §4.8 合并（filter AND），resolvedTags 按 tagId 合并", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    const newExpr = tagLeaf(2);
    vi.mocked(aiParseSearchQuery).mockResolvedValue({
      intent: { groups: [], exclusions: [], sortBy: null, sortDir: null },
      expr: newExpr,
      plan: emptyPlan(newExpr),
      sortBy: "created_at",
      sortDir: "desc",
      explanation: "",
      warnings: [],
      parseStatus: "full",
      resolvedTags: [{ facetKey: "subject", text: "树", tagId: 2, path: "" }],
    });
    const base = tagLeaf(1);
    useSuperSearchStore.getState().setExpr(base);
    useSuperSearchStore.setState({ resolvedTags: [{ facetKey: "subject", text: "建筑", tagId: 1, path: "" }] });
    await useSuperSearchStore.getState().applyAiSearch("树", "append");
    const st = useSuperSearchStore.getState();
    expect(st.expr).toEqual({ op: "and", children: [base, newExpr] });
    expect(st.plan?.filter).toEqual(st.expr);
    expect(st.resolvedTags.map((t) => t.tagId).sort()).toEqual([1, 2]);
  });

  it("AI 解析失败：只设 aiError，保留当前 expr/items，不触发 refresh", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    vi.mocked(listSuperAssets).mockResolvedValue({ items: [mkAsset(1)], total: 1, hasMore: false, warnings: [] });
    vi.mocked(aiParseSearchQuery).mockRejectedValue(new Error("模型未返回可解析的 JSON"));
    const base = tagLeaf(1);
    useSuperSearchStore.getState().setExpr(base);
    useSuperSearchStore.setState({ resolvedTags: [{ facetKey: "subject", text: "建筑", tagId: 1, path: "" }] });
    const refreshSpy = vi.fn();
    useSuperSearchStore.setState({ refresh: refreshSpy });
    await useSuperSearchStore.getState().applyAiSearch("无法解析的句子");
    const st = useSuperSearchStore.getState();
    expect(st.aiError).toContain("模型未返回");
    expect(st.error).toBeNull();
    expect(st.expr).toEqual(base);
    expect(st.resolvedTags.length).toBe(1);
    expect(refreshSpy).not.toHaveBeenCalled();
  });

  it("手动编辑条件会取消旧 AI 请求并解除 loading", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    let resolveAi!: (value: Awaited<ReturnType<typeof aiParseSearchQuery>>) => void;
    vi.mocked(aiParseSearchQuery).mockReturnValueOnce(new Promise((resolve) => { resolveAi = resolve; }));
    const task = useSuperSearchStore.getState().applyAiSearch("最好有蓝天");
    await vi.waitFor(() => expect(useSuperSearchStore.getState().aiLoading).toBe(true));
    useSuperSearchStore.getState().setPlanMustNot(tagLeaf(7));
    expect(useSuperSearchStore.getState().aiLoading).toBe(false);
    resolveAi({} as Awaited<ReturnType<typeof aiParseSearchQuery>>);
    await task;
    expect(useSuperSearchStore.getState().plan?.mustNot).toEqual(tagLeaf(7));
  });

  it("编辑搜索输入时旧 AI 响应不会覆盖新输入", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    let resolveAi!: (value: Awaited<ReturnType<typeof aiParseSearchQuery>>) => void;
    vi.mocked(aiParseSearchQuery).mockReturnValueOnce(new Promise((resolve) => { resolveAi = resolve; }));
    const task = useSuperSearchStore.getState().applyAiSearch("旧条件");
    await vi.waitFor(() => expect(useSuperSearchStore.getState().aiLoading).toBe(true));
    useSuperSearchStore.getState().setAiInput("新条件");
    expect(useSuperSearchStore.getState().aiLoading).toBe(false);
    resolveAi({} as Awaited<ReturnType<typeof aiParseSearchQuery>>);
    await task;
    expect(useSuperSearchStore.getState().aiInput).toBe("新条件");
    expect(useSuperSearchStore.getState().plan).toBeNull();
  });

  it("removeExprAtPath 只摘除 filter 区对应节点", () => {
    const expr: QueryExpr = {
      op: "and",
      children: [
        { op: "leaf", cond: { type: "search", value: "海边" } },
        tagLeaf(1),
      ],
    };
    useSuperSearchStore.getState().setExpr(expr);
    useSuperSearchStore.setState({ resolvedTags: [{ facetKey: "subject", text: "建筑", tagId: 1, path: "" }] });
    useSuperSearchStore.getState().removeExprAtPath([1]);
    expect(useSuperSearchStore.getState().expr).toEqual({ op: "leaf", cond: { type: "search", value: "海边" } });
    // 已不再引用的名称映射被清理
    expect(useSuperSearchStore.getState().resolvedTags).toEqual([]);
  });

  it("clearConditions 同时清 plan 与扁平筛选", () => {
    useSuperSearchStore.getState().setQuery({ search: "海边" });
    useSuperSearchStore.getState().setExpr({ op: "leaf", cond: { type: "assetType", value: "video" } });
    useSuperSearchStore.getState().clearConditions();
    const st = useSuperSearchStore.getState();
    expect(st.expr).toBeUndefined();
    expect(st.plan).toBeNull();
    expect(st.query.search).toBe("");
    expect(st.query.assetType).toBe("all");
  });

  // ═══════════════ §3.7 三区 store API + 九条不变式 ═══════════════

  it("setPlanFilter 只动必须区，保留 AI 的优先项与排除区（不变式 1/2）", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    const filter = tagLeaf(1);
    const plan = emptyPlan(filter, {
      mustNot: { op: "or", children: [tagLeaf(3, "scene")] },
      should: [{ cond: tagCond(2), weight: 1, label: "蓝天（加分项）" }],
      minimumShouldMatch: 1,
    });
    vi.mocked(aiParseSearchQuery).mockResolvedValue({
      intent: { groups: [], exclusions: [], sortBy: null, sortDir: null },
      expr: filter,
      plan,
      sortBy: "created_at", sortDir: "desc", explanation: "", warnings: [],
      parseStatus: "full", resolvedTags: [],
    });
    await useSuperSearchStore.getState().applyAiSearch("标签");
    useSuperSearchStore.getState().setPlanFilter({ op: "leaf", cond: { type: "assetType", value: "image" } });
    const kept = useSuperSearchStore.getState().plan;
    expect(kept?.should?.[0].cond).toEqual(plan.should[0].cond);
    expect(kept?.should?.[0].weight).toBe(2);
    expect(kept?.mustNot).toEqual(plan.mustNot);
    expect(kept?.filter).toEqual({ op: "leaf", cond: { type: "assetType", value: "image" } });
  });

  it("setPlanMustNot 写入排除区；三区全空才清 plan（不变式 3）", () => {
    useSuperSearchStore.getState().setPlanMustNot({ op: "leaf", cond: { type: "search", value: "夜景" } });
    let plan = useSuperSearchStore.getState().plan;
    expect(plan?.mustNot).toEqual({ op: "leaf", cond: { type: "search", value: "夜景" } });
    expect(plan?.filter).toBeNull();
    // 清排除区：filter/mustNot/should 全空 → plan null
    useSuperSearchStore.getState().clearZone("mustNot");
    expect(useSuperSearchStore.getState().plan).toBeNull();
    // 先有必须区，清排除区保留必须区
    useSuperSearchStore.getState().setPlanFilter({ op: "leaf", cond: { type: "search", value: "人物" } });
    useSuperSearchStore.getState().setPlanMustNot({ op: "leaf", cond: { type: "search", value: "夜景" } });
    useSuperSearchStore.getState().clearZone("mustNot");
    plan = useSuperSearchStore.getState().plan;
    expect(plan?.filter).toEqual({ op: "leaf", cond: { type: "search", value: "人物" } });
    expect(plan?.mustNot).toBeNull();
  });

  it("minimumShouldMatch 随 should 长度自动收敛（不变式 4）", () => {
    useSuperSearchStore.getState().setPlanShould([{ cond: { type: "search", value: "a" }, weight: 1, label: "a" }], 5);
    let plan = useSuperSearchStore.getState().plan;
    expect(plan?.minimumShouldMatch).toBe(0);
    // should 清空（且 filter/mustNot 也为空）→ 不变式 3：plan 置 null
    useSuperSearchStore.getState().setPlanShould([], 1);
    plan = useSuperSearchStore.getState().plan;
    expect(plan).toBeNull();
  });

  it("moveConditionBetweenZones 从必须区移到排除区（不变式 6/§3.10）", () => {
    useSuperSearchStore.getState().setExpr({
      op: "and",
      children: [
        { op: "leaf", cond: { type: "search", value: "人物" } },
        { op: "leaf", cond: { type: "search", value: "夜景" } },
      ],
    });
    const revBefore = useSuperSearchStore.getState().planRevision;
    useSuperSearchStore.getState().moveConditionBetweenZones("filter", "mustNot", [1]);
    const plan = useSuperSearchStore.getState().plan;
    expect(plan?.filter).toEqual({ op: "leaf", cond: { type: "search", value: "人物" } });
    expect(plan?.mustNot).toEqual({ op: "leaf", cond: { type: "search", value: "夜景" } });
    expect(useSuperSearchStore.getState().planRevision).toBeGreaterThan(revBefore);
  });

  it("优先区达到 12 条时移动被拒绝，源条件不丢失", () => {
    const source = tagLeaf(99, "scene");
    const should = Array.from({ length: 12 }, (_, i) => ({ cond: tagCond(i + 1), weight: 1, label: `优先${i + 1}` }));
    useSuperSearchStore.getState().setExpr(source);
    useSuperSearchStore.setState({
      plan: emptyPlan(source, { should }),
      executionWarnings: [],
    });
    useSuperSearchStore.getState().moveConditionBetweenZones("filter", "should", []);
    const st = useSuperSearchStore.getState();
    expect(st.plan?.filter).toEqual(source);
    expect(st.plan?.should).toHaveLength(12);
    expect(st.executionWarnings.at(-1)?.message).toContain("未移动该条件");
  });

  it("removeAtZonePath 按区删除（filter 与 mustNot 同下标互不干扰）", () => {
    useSuperSearchStore.getState().setExpr(tagLeaf(1));
    useSuperSearchStore.getState().setPlanMustNot(tagLeaf(2, "scene"));
    useSuperSearchStore.getState().removeAtZonePath("mustNot", [0]);
    const plan = useSuperSearchStore.getState().plan;
    expect(plan?.filter).toEqual(tagLeaf(1));
    expect(plan?.mustNot).toBeNull();
  });

  it("planRevision 每次 plan 变更 +1（不变式 9）", () => {
    useSuperSearchStore.getState().setPlanFilter(tagLeaf(1));
    const r1 = useSuperSearchStore.getState().planRevision;
    useSuperSearchStore.getState().setPlanMustNot(tagLeaf(2, "scene"));
    const r2 = useSuperSearchStore.getState().planRevision;
    useSuperSearchStore.getState().setPlanShould([{ cond: tagCond(3), weight: 1, label: "x" }], 0);
    const r3 = useSuperSearchStore.getState().planRevision;
    expect(r2).toBe(r1 + 1);
    expect(r3).toBe(r2 + 1);
  });

  it("AI append：合并后 plan 精确保留（§4.8），不再清空", async () => {
    const { aiParseSearchQuery } = await import("@/api/superSearch");
    const newExpr = tagLeaf(2);
    vi.mocked(aiParseSearchQuery).mockResolvedValue({
      intent: { groups: [], exclusions: [], sortBy: null, sortDir: null },
      expr: newExpr,
      plan: emptyPlan(newExpr),
      sortBy: "created_at",
      sortDir: "desc",
      explanation: "",
      warnings: [],
      parseStatus: "full",
      resolvedTags: [],
    });
    const base: QueryExpr = { op: "leaf", cond: { type: "search", value: "海边" } };
    useSuperSearchStore.getState().setExpr(base);
    await useSuperSearchStore.getState().applyAiSearch("树", "append");
    const st = useSuperSearchStore.getState();
    expect(st.plan).not.toBeNull();
    expect(st.plan?.filter).toEqual({ op: "and", children: [base, newExpr] });
    expect(st.expr).toEqual(st.plan?.filter);
  });

  it("S5 5-4：applyTermSuggestion 只替换点击建议对应的失败按词查 leaf（不变量 11）", () => {
    useSuperSearchStore.setState({ plan: null, expr: undefined });
    // 未点击前：条件保持原样（后端只出建议不改写）
    const before = useSuperSearchStore.getState().plan;
    expect(before).toBeNull();
    // 用户点击建议「森林」→ 词查 leaf 进 filter
    useSuperSearchStore.getState().applyTermSuggestion("", "森林", "fuzzy");
    const st = useSuperSearchStore.getState();
    expect(st.plan?.filter).toEqual({
      op: "leaf",
      cond: { type: "tag", facetKey: "", tagIds: [], mode: "any", includeDescendants: true, termQuery: "森林", termMatch: "fuzzy" },
    });
    // 找不到原词时才追加，不覆盖已有条件（兼容旧 warning/手工路径）
    useSuperSearchStore.getState().applyTermSuggestion("不存在的原词", "海边", "fuzzy");
    const st2 = useSuperSearchStore.getState();
    expect(st2.plan?.filter?.op).toBe("and");
    const conds = st2.plan?.filter?.op === "and" ? st2.plan.filter.children : [];
    expect(conds).toHaveLength(2);
    // 空词不入条件
    useSuperSearchStore.getState().applyTermSuggestion("", "  ", "fuzzy");
    expect(useSuperSearchStore.getState().plan?.filter).toEqual(st2.plan?.filter);
  });

  it("相近词替换保留其他布尔条件与树结构", () => {
    const failed: QueryExpr = {
      op: "leaf",
      cond: { type: "tag", facetKey: "scene", tagIds: [], mode: "any", includeDescendants: true, termQuery: "森材", termMatch: "alias" },
    };
    const other = tagLeaf(9, "subject");
    useSuperSearchStore.getState().setExpr({ op: "or", children: [{ op: "and", children: [failed, other] }, tagLeaf(10, "scene")] });
    useSuperSearchStore.getState().applyTermSuggestion("森材", "森林", "fuzzy");
    expect(useSuperSearchStore.getState().plan?.filter).toEqual({
      op: "or",
      children: [
        {
          op: "and",
          children: [
            { op: "leaf", cond: { type: "tag", facetKey: "scene", tagIds: [], mode: "any", includeDescendants: true, termQuery: "森林", termMatch: "fuzzy" } },
            other,
          ],
        },
        tagLeaf(10, "scene"),
      ],
    });
  });

  it("S5：prefix/contains 词查 leaf 经 setExpr 写入 plan 并发给后端（prefix_mode_reaches_backend）", async () => {
    vi.mocked(listSuperAssets).mockResolvedValue({ items: [mkAsset(1)], total: 1, hasMore: false, warnings: [] });
    useSuperSearchStore.setState({ plan: null, expr: undefined });
    const leaf: QueryExpr = { op: "leaf", cond: { type: "tag", facetKey: "scene", tagIds: [], mode: "any", includeDescendants: true, termQuery: "青", termMatch: "prefix" } };
    useSuperSearchStore.getState().setExpr(leaf);
    const st = useSuperSearchStore.getState();
    expect(st.plan?.filter).toEqual(leaf);
    // 刷新执行 → 后端收到的 plan.filter 携带 termQuery/termMatch（wire 契约）
    await st.refresh();
    expect(vi.mocked(listSuperAssets)).toHaveBeenCalled();
    const sent = vi.mocked(listSuperAssets).mock.calls.at(-1);
    // listSuperAssets(q, offset, limit, plan) —— plan 是第 4 参（§3.7 列表一律走 plan）
    const sentPlan = sent?.[3] as SearchPlanV3 | undefined;
    const cond = sentPlan?.filter?.op === "leaf" ? sentPlan.filter.cond : undefined;
    expect(cond?.type).toBe("tag");
    expect((cond as { termQuery?: string }).termQuery).toBe("青");
    expect((cond as { termMatch?: string }).termMatch).toBe("prefix");
  });

  it("migratePlanV3：未来 schema 版本丢弃（防用户降级应用破数据），当前版本保留", async () => {
    const { migratePlanV3 } = await import("@/stores/superSearchStore");
    const base: SearchPlanV3 = {
      planSchemaVersion: 3, normalizationVersion: 1, compilerVersion: 1,
      filter: null, mustNot: null, should: [], minimumShouldMatch: 0,
      retrievers: { retrievers: [], fusion: "rrf" },
      ranking: { type: "field", key: "created_at", dir: "desc" },
    };
    expect(migratePlanV3(base)).not.toBeNull();
    expect(migratePlanV3({ ...base, planSchemaVersion: 99 })).toBeNull();
  });

  it("future plan hydrate：丢弃未知版本并保留一次可见 warning", async () => {
    const base: SearchPlanV3 = {
      planSchemaVersion: 99,
      normalizationVersion: 1,
      compilerVersion: 1,
      filter: tagLeaf(77),
      mustNot: null,
      should: [],
      minimumShouldMatch: 0,
      retrievers: { retrievers: [], fusion: "rrf" },
      ranking: { type: "field", key: "created_at", dir: "desc" },
    };
    useSuperSearchStore.setState({ executionWarnings: [], plan: null, expr: undefined });
    localStorage.setItem(
      "super-search-conditions",
      JSON.stringify({ state: { plan: base, query: { sortBy: "created_at", sortDir: "desc" } }, version: 2 }),
    );
    await useSuperSearchStore.persist.rehydrate();
    const st = useSuperSearchStore.getState();
    expect(st.plan).toBeNull();
    expect(st.expr).toBeUndefined();
    expect(st.executionWarnings.filter((w) => w.message.includes("更新版本"))).toHaveLength(1);
    localStorage.removeItem("super-search-conditions");
  });
});
