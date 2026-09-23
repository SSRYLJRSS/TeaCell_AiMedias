/** 超级搜索数据与筛选状态（P2 + FB5-05 §9.6/§9.7 + §3.7 状态协议）：
 *  SearchPlanV3 是唯一执行事实源（filter/mustNot/should/ranking 四区），
 *  expr 降级为 plan.filter 的派生只读视图（供 FilterChips / QueryBuilder 等旧代码路径过渡）。
 *  query 扁平字段仅保留 sortBy/sortDir（排序 chip）与旧链路兜底；手动条件经
 *  resolvedQueryToPlan 写回 plan（§3.7 不变式 1：改必须区不清 AI 的优先项）。
 *  planRevision 为单调计数器：每次 plan 变更 +1，在途的异步诊断返回时代次过期即整批丢弃（不变式 9）。
 *  error（数据查询）与 aiError（AI 解析）分离；请求代际防旧响应覆盖新查询。 */
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { listSuperAssets, listSuperAssetIdsByPlan, aiParseSearchQuery } from "@/api/superSearch";
import { useSelectionStore } from "@/stores/selectionStore";
import type { Asset, ResolvedSearchQuery, MetadataFilter } from "@/types/asset";
import type { LeafCond, QueryExpr, TermMatch } from "@/types/queryExpr";
import type { AiApplyMode, ResolvedTag, SearchPlanV3, SearchWarning, ShouldClause, FetchAllIdsResult } from "@/types/superSearch";
import {
  appendToExpr,
  mergeQueryExpr,
  normalizeExpr,
  removeExprAtPath,
  serializeExpr,
  syncQueryFromExpr,
} from "@/utils/queryExprUtils";
import {
  MAX_SHOULD_CLAUSES,
  appendPlanMerge,
  fieldRanking,
  mergeMustNotExpr,
  migratePlanV3 as migratePlanWithNormalization,
  normalizeSearchPlan,
  resolvedQueryToPlan,
} from "@/utils/planUtils";
import type { ExprPath } from "@/utils/queryExprUtils";

const PAGE_SIZE = 200;

/** 默认查询：全库、非回收站、入库时间降序 */
function defaultQuery(): ResolvedSearchQuery {
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

let listRequestSeq = 0;
let aiRequestSeq = 0;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleRefresh(refresh: () => Promise<void>) {
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    void refresh();
  }, 180);
}

function invalidatePendingRequests() {
  listRequestSeq += 1;
}

function invalidateAiRequests() {
  aiRequestSeq += 1;
}

function collectExprLeaves(expr: QueryExpr | undefined | null, predicate: (cond: LeafCond) => boolean): QueryExpr | undefined {
  if (!expr) return undefined;
  if (expr.op === "leaf") return predicate(expr.cond) ? expr : undefined;
  if (expr.op === "not") {
    const child = collectExprLeaves(expr.child, predicate);
    return child ? { op: "not", child } : undefined;
  }
  const children = expr.children
    .map((child) => collectExprLeaves(child, predicate))
    .filter((child): child is QueryExpr => Boolean(child));
  return children.length > 0 ? normalizeExpr({ op: expr.op, children }) : undefined;
}

function collectCondKeys(expr: QueryExpr | undefined | null, predicate: (cond: LeafCond) => boolean): Set<string> {
  const keys = new Set<string>();
  const visit = (node: QueryExpr | undefined | null) => {
    if (!node) return;
    if (node.op === "leaf") {
      if (predicate(node.cond)) keys.add(JSON.stringify(node.cond));
      return;
    }
    if (node.op === "not") {
      visit(node.child);
      return;
    }
    node.children.forEach(visit);
  };
  visit(expr);
  return keys;
}

function removeExprLeaves(expr: QueryExpr | undefined | null, predicate: (cond: LeafCond) => boolean): QueryExpr | undefined {
  if (!expr) return undefined;
  if (expr.op === "leaf") return predicate(expr.cond) ? undefined : expr;
  if (expr.op === "not") {
    const child = removeExprLeaves(expr.child, predicate);
    return child ? { op: "not", child } : undefined;
  }
  const children = expr.children
    .map((child) => removeExprLeaves(child, predicate))
    .filter((child): child is QueryExpr => Boolean(child));
  return children.length > 0 ? normalizeExpr({ op: expr.op, children }) : undefined;
}

function appendExpr(a: QueryExpr | undefined | null, b: QueryExpr | undefined): QueryExpr | undefined {
  if (!a) return b;
  if (!b) return a;
  return normalizeExpr(a.op === "and" ? { op: "and", children: [b, ...a.children] } : { op: "and", children: [b, a] });
}

/**
 * 替换零结果诊断对应的按词查叶子。建议词只替换用户刚刚失败的那一个条件，
 * 保留其他关键词、布尔组和分面条件；找不到源叶子时才退回追加语义。
 */
function replaceFirstTermLeaf(
  expr: QueryExpr,
  sourceTerm: string,
  replacement: string,
  match: TermMatch,
): { expr: QueryExpr; replaced: boolean } {
  if (expr.op === "leaf") {
    const cond = expr.cond;
    if (cond.type === "tag" && cond.tagIds.length === 0 && cond.termQuery?.trim() === sourceTerm) {
      return {
        expr: { op: "leaf", cond: { ...cond, termQuery: replacement, termMatch: match } },
        replaced: true,
      };
    }
    return { expr, replaced: false };
  }
  if (expr.op === "not") {
    const child = replaceFirstTermLeaf(expr.child, sourceTerm, replacement, match);
    return { expr: child.replaced ? { op: "not", child: child.expr } : expr, replaced: child.replaced };
  }
  let replaced = false;
  const children = expr.children.map((child) => {
    if (replaced) return child;
    const next = replaceFirstTermLeaf(child, sourceTerm, replacement, match);
    replaced = next.replaced;
    return next.expr;
  });
  return { expr: replaced ? normalizeExpr({ op: expr.op, children }) ?? expr : expr, replaced };
}

/** 把扁平 query 的局部修改应用到现有 plan，而不是把 AI 的复杂树整棵拍平。 */
function patchExistingPlan(plan: SearchPlanV3, previous: ResolvedSearchQuery, next: ResolvedSearchQuery, patch: Partial<ResolvedSearchQuery>): SearchPlanV3 {
  const rebuilt = resolvedQueryToPlan(next);
  const previousPlan = resolvedQueryToPlan(previous);
  let filter: QueryExpr | null | undefined = plan.filter;
  let mustNot: QueryExpr | null | undefined = plan.mustNot;
  const keys = new Set(Object.keys(patch));
  const filterRules: [string, (cond: LeafCond) => boolean][] = [
    ["search", (cond) => cond.type === "search"],
    ["assetType", (cond) => cond.type === "assetType"],
    ["untaggedOnly", (cond) => cond.type === "untagged"],
    ["facetFilters", (cond) => cond.type === "tag"],
    ["metadataFilters", (cond) => cond.type === "metadata"],
  ];
  for (const [key, matches] of filterRules) {
    if (!keys.has(key)) continue;
    // 只摘除上一版扁平 query 实际贡献的叶子；AI/嵌套条件即使类型相同也继续保留。
    const oldKeys = collectCondKeys(previousPlan.filter, matches);
    filter = removeExprLeaves(filter, (cond) => oldKeys.has(JSON.stringify(cond)));
    filter = appendExpr(filter, collectExprLeaves(rebuilt.filter, matches));
  }
  if (keys.has("excludeTagIds")) {
    const oldKeys = collectCondKeys(previousPlan.mustNot, (cond) => cond.type === "tag" || cond.type === "excludeTag");
    mustNot = removeExprLeaves(mustNot, (cond) => oldKeys.has(JSON.stringify(cond)));
    mustNot = mergeMustNotExpr(mustNot ?? null, rebuilt.mustNot);
  }
  return normalizeSearchPlan({ ...plan, filter: filter ?? null, mustNot: mustNot ?? null });
}

function dedupItems(arr: Asset[]): Asset[] {
  const seen = new Set<number>();
  const out: Asset[] = [];
  for (const a of arr) {
    if (!seen.has(a.id)) {
      seen.add(a.id);
      out.push(a);
    }
  }
  return out;
}

/** 深度比较两个查询是否相等（用于「查询变化清空选中」判断） */
function queryEqual(a: ResolvedSearchQuery, b: ResolvedSearchQuery): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}


/** 从当前状态起一个最小 plan（手动条件首次加分 / 纯 mustNot 时的依托） */
function minimalPlanFrom(cur: { expr?: QueryExpr; query: ResolvedSearchQuery }): SearchPlanV3 {
  return {
    planSchemaVersion: 3,
    normalizationVersion: 1,
    compilerVersion: 1,
    filter: cur.expr ?? null,
    mustNot: null,
    should: [],
    minimumShouldMatch: 0,
    retrievers: { retrievers: [], fusion: "rrf" },
    ranking: fieldRanking(cur.query.sortBy ?? "created_at", cur.query.sortDir ?? "desc"),
  };
}

/** 取表达式中 path 处的子树（moveConditionBetweenZones 用） */
function nodeAtPath(expr: QueryExpr, path: ExprPath): QueryExpr | undefined {
  let node: QueryExpr | undefined = expr;
  for (const idx of path) {
    if (!node || node.op === "leaf" || node.op === "not") return undefined;
    node = node.children[idx];
  }
  return node;
}

/** §3.7 不变式 3：三区全空时 plan 才置 null */
function maybeNullPlan(plan: SearchPlanV3): SearchPlanV3 | null {
  if (plan.filter == null && plan.mustNot == null && plan.should.length === 0) return null;
  return plan;
}

/** §3.7：plan（filter/mustNot/should）是 resolvedTags 名称映射的引用集（比 expr 更全） */
function filterResolvedTagsByPlan(tags: ResolvedTag[], plan: SearchPlanV3 | null): ResolvedTag[] {
  if (!plan) return [];
  const ids = new Set<number>();
  const collectExpr = (e?: QueryExpr | null) => {
    if (!e) return;
    const walk = (n: QueryExpr) => {
      if (n.op === "leaf") {
        const c = n.cond;
        if (c.type === "tag" || c.type === "excludeTag") c.tagIds.forEach((id) => ids.add(id));
      } else if (n.op === "not") walk(n.child);
      else n.children.forEach(walk);
    };
    walk(e);
  };
  collectExpr(plan.filter);
  collectExpr(plan.mustNot);
  for (const sc of plan.should) {
    if (sc.cond.type === "tag" || sc.cond.type === "excludeTag") sc.cond.tagIds.forEach((id) => ids.add(id));
  }
  return tags.filter((t) => ids.has(t.tagId));
}

/** FB5-05（§9.6）：append 模式按 tagId 合并旧映射与新映射 */
function mergeResolvedTags(a: ResolvedTag[], b: ResolvedTag[]): ResolvedTag[] {
  const byId = new Map<number, ResolvedTag>();
  for (const t of [...a, ...b]) byId.set(t.tagId, t);
  return Array.from(byId.values());
}

/** B7：plan 路径 warnings 归一（后端 SearchWarning[] 与旧链路 string[] 兼容） */
function normalizeWarnings(w?: (string | SearchWarning)[]): SearchWarning[] {
  return (w ?? []).map((x) => (typeof x === "string" ? { source: "plan", message: x } : x));
}

/** 从 AI 解析产物兜底构造 plan（后端未返回 plan 时：filter=expr 的最小 plan） */
function planFromParseResult(
  expr: QueryExpr | null | undefined,
  sortBy: ResolvedSearchQuery["sortBy"],
  sortDir: "desc" | "asc",
): SearchPlanV3 {
  return {
    planSchemaVersion: 3,
    normalizationVersion: 1,
    compilerVersion: 1,
    filter: expr ?? null,
    mustNot: null,
    should: [],
    minimumShouldMatch: 0,
    retrievers: { retrievers: [], fusion: "rrf" },
    ranking: fieldRanking(sortBy ?? "created_at", sortDir ?? "desc"),
  };
}

export interface SuperSearchState {
  query: ResolvedSearchQuery;
  /** §3.7：plan.filter 的派生只读视图（供旧代码路径过渡；不再作为第二事实源） */
  expr?: QueryExpr;
  /** §3.7：唯一事实源 —— 四区 + 排序 + 版本号；三区全空时 null（不变式 3） */
  plan?: SearchPlanV3 | null;
  /** §3.7 不变式 9：plan 代次计数器，每次 plan 变更 +1 */
  planRevision: number;
  items: Asset[];
  total: number;
  loading: boolean;
  /** 数据查询错误（refresh/loadMore） */
  error: string | null;
  /** AI 解析错误（§9.7：与 error 分离，失败不触发 refresh、不丢当前条件） */
  aiError: string | null;
  aiInput: string;
  aiLoading: boolean;
  aiExplanation: string | null;
  warnings: string[];
  /** B7：执行层 warning 双通道之一 —— 生命周期跟每次 refresh（请求开始清空、响应写入）。
   *  source="ai"（AI 解析）存 warnings；source="plan"（执行/剔除）存本通道，两者互不清空。 */
  executionWarnings: SearchWarning[];
  /** W6-5：AI 解析三态（完全理解 / 部分理解 / 按关键词搜索） */
  parseStatus: "full" | "partial" | "keyword" | null;
  /** AI 已解析标签（tagId→名称/分面），供 chips 可读展示 */
  resolvedTags: ResolvedTag[];

  setQuery: (patch: Partial<ResolvedSearchQuery>) => void;
  /** §3.7：设置 plan.filter（必须区）；保留 should/mustNot/ranking（不变式 1） */
  setPlanFilter: (expr: QueryExpr | undefined) => void;
  /** §3.7：设置 plan.mustNot（排除区，区内容为正向条件，计划层统一包 NOT） */
  setPlanMustNot: (expr: QueryExpr | undefined) => void;
  /** P4/兼容旧链路：设置表达式树（内部改写 plan.filter，不再清空整个 plan） */
  setExpr: (expr: QueryExpr | undefined) => void;
  replaceQuery: (query: ResolvedSearchQuery, expr?: QueryExpr) => void;
  setAiInput: (v: string) => void;
  setAiResult: (explanation: string, warnings: string[]) => void;
  clearAiResult: () => void;
  /** FB5-05（§9.6）+ §4.8：AI 解析并应用（replace 采纳 AI 的 plan/ranking；append 按 §4.8 逐字段合并） */
  applyAiSearch: (text: string, mode?: AiApplyMode) => Promise<void>;
  /** §3.7 不变式 6：按区删除单个条件（chips 删除用，禁止走 setQuery） */
  removeAtZonePath: (zone: "filter" | "mustNot" | "should", path: ExprPath | number) => void;
  /** 兼容旧签名：必须区删除（路由到 removeAtZonePath("filter", path)） */
  removeExprAtPath: (path: ExprPath) => void;
  removePlanFilterAt: (path: ExprPath) => void;
  removePlanMustNotAt: (path: ExprPath) => void;
  /** §3.7：把条件从一区移到另一区（filter↔mustNot；按目标区语义 AND/OR 并入） */
  moveConditionBetweenZones: (from: "filter" | "mustNot" | "should", to: "filter" | "mustNot" | "should", path: ExprPath | number) => void;
  /** S5 5-4（不变量 11）：零结果相近词建议 —— 用户点了才替换失败的按词查条件；
   *  系统绝不主动改写语义（未点击前条件保持原样）。 */
  applyTermSuggestion: (sourceTerm: string, term: string, match: TermMatch) => void;
  /** §3.7：清空某一区（不变式 3：三区全空才清 plan） */
  clearZone: (zone: "filter" | "should" | "mustNot") => void;
  /** U-5：加分项（should）编辑 —— 覆盖整个 should 数组与最低命中数（不变式 4 自动收敛） */
  setPlanShould: (should: ShouldClause[], minimumShouldMatch: number) => void;
  /** U-5：按索引移除单条加分项（chips/加分区删除用） */
  removePlanShould: (index: number) => void;
  /** FB5-05（§9.6.1）：排序 chip 独立 action（只改 sortBy/sortDir，经 setQuery 重建 ranking） */
  setSort: (sortBy: ResolvedSearchQuery["sortBy"], sortDir: "desc" | "asc") => void;
  /** FB5-05（§9.6.1）：清除全部——同时清 plan 与兼容扁平筛选 */
  clearConditions: () => void;
  refresh: () => Promise<void>;
  loadMore: () => Promise<void>;
  /** 查看器内删标签/信息栏编辑后的本地回写（语义同 libraryStore.patchLocal，不触发整页刷新） */
  patchLocal: (ids: number[], patch: Partial<Asset>) => void;
  /** B2/B8/§4.6：plan 全选 ID 一路到底 —— FetchAllIdsResult（= PlanIdsResult，不降级成 number[]） */
  fetchAllIds: () => Promise<FetchAllIdsResult>;
  clearQuery: () => void;
}

/** S6：本地保存的 plan 版本迁移（与后端 db/search_plan.rs migrate_plan 对齐的最小版）。
 *  planSchemaVersion / normalizationVersion > 当前（用户降级应用）→ null（丢弃 + 提示）；
 *  ≤ 当前视为可迁移。当前 schema v3 无历史结构差异，恒等返回；未来结构变更在此加分支。 */
export function migratePlanV3(plan: SearchPlanV3): SearchPlanV3 | null {
  return migratePlanWithNormalization(plan);
}

// W5f-f6：搜索条件持久化（localStorage）—— §3.7 不变式 5：只存 plan + sortBy/sortDir，
// 不再存 expr（expr 是 plan.filter 的派生视图）。hydrate 不触发请求；进页走既有代际机制单次查询。
export const useSuperSearchStore = create<SuperSearchState>()(
  persist(
    (set, get) => ({
  query: defaultQuery(),
  expr: undefined,
  plan: null,
  planRevision: 0,
  items: [],
  total: 0,
  loading: false,
  error: null,
  aiError: null,
  aiInput: "",
  aiLoading: false,
  aiExplanation: null,
  warnings: [],
  executionWarnings: [],
  parseStatus: null,
  resolvedTags: [],

  setQuery: (patch) => {
    const prev = get().query;
    const next = { ...prev, ...patch };
    if (queryEqual(prev, next)) return;
    invalidatePendingRequests();
    invalidateAiRequests();
    const curPlan = get().plan;
    // §3.7 不变式 8：排序/方向变化 = 只换 ranking，plan.filter/mustNot/should 原样保留
    // （否则重建会把 AI 的嵌套 filter 树拍平成扁平字段，丢掉结构）。
    const onlySort = Object.keys(patch).every((k) => k === "sortBy" || k === "sortDir");
    if (onlySort && curPlan) {
      const plan = { ...curPlan, ranking: fieldRanking(next.sortBy ?? "created_at", next.sortDir ?? "desc") };
      set({ query: next, plan, expr: plan.filter ?? undefined, planRevision: get().planRevision + 1, aiLoading: false });
      useSelectionStore.getState().clear();
      scheduleRefresh(get().refresh);
      return;
    }
    // §3.7：plan 是唯一事实源。扁平字段修改只替换对应类型的叶子，
    // 保留 AI 生成的复杂布尔树、mustNot 和 should，不再整棵重建覆盖。
    const plan = curPlan
      ? maybeNullPlan(patchExistingPlan(curPlan, prev, next, patch))
      : maybeNullPlan(resolvedQueryToPlan(next));
    const query = syncQueryFromExpr(
      { ...defaultQuery(), sortBy: next.sortBy, sortDir: next.sortDir },
      plan?.filter ?? undefined,
    );
    const resolvedTags = filterResolvedTagsByPlan(get().resolvedTags, plan);
    set({ query, plan, expr: plan?.filter ?? undefined, planRevision: get().planRevision + 1, warnings: [], aiExplanation: null, aiError: null, aiLoading: false, resolvedTags });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  setPlanFilter: (expr) => {
    const cur = get();
    const currentExpr = cur.expr;
    if ((!expr && !currentExpr) || (expr && currentExpr && serializeExpr(expr) === serializeExpr(currentExpr))) return;
    invalidatePendingRequests();
    invalidateAiRequests();
    // §3.7 不变式 1：只动 plan.filter；should/mustNot/ranking 原样保留
    const curPlan = cur.plan;
    const base: SearchPlanV3 = curPlan ?? minimalPlanFrom(cur);
    const plan = maybeNullPlan(normalizeSearchPlan({ ...base, filter: expr ?? null }));
    const query = syncQueryFromExpr(cur.query, expr);
    const resolvedTags = filterResolvedTagsByPlan(cur.resolvedTags, plan);
    set({ query, expr: expr ?? undefined, plan, planRevision: cur.planRevision + 1, warnings: [], aiExplanation: null, aiError: null, aiLoading: false, resolvedTags });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  setPlanMustNot: (expr) => {
    const cur = get();
    const curPlan = cur.plan;
    const curMustNot = curPlan?.mustNot ?? null;
    if ((!expr && !curMustNot) || (expr && curMustNot && serializeExpr(expr) === serializeExpr(curMustNot))) return;
    invalidatePendingRequests();
    invalidateAiRequests();
    // 无 plan 时从当前 filter 起一个最小 plan（纯排除条件也可独立成 plan）
    const base: SearchPlanV3 = curPlan ?? minimalPlanFrom(cur);
    const plan = maybeNullPlan(normalizeSearchPlan({ ...base, mustNot: expr ?? null }));
    const resolvedTags = filterResolvedTagsByPlan(cur.resolvedTags, plan);
    const query = syncQueryFromExpr({ ...defaultQuery(), sortBy: cur.query.sortBy, sortDir: cur.query.sortDir }, plan?.filter ?? undefined);
    set({ query, plan, expr: plan?.filter ?? undefined, planRevision: cur.planRevision + 1, resolvedTags, aiLoading: false });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  setExpr: (expr) => get().setPlanFilter(expr),

  replaceQuery: (query, expr) => {
    const prev = get().query;
    const currentExpr = get().expr;
    const sameExpr = (!expr && !currentExpr) || (expr && currentExpr && serializeExpr(expr) === serializeExpr(currentExpr));
    if (queryEqual(prev, query) && sameExpr) return;
    invalidatePendingRequests();
    invalidateAiRequests();
    // 换源：expr 优先作为 plan.filter；否则扁平条件重建 plan（should 不跨 replace 保留）
    const plan = expr
      ? { ...minimalPlanFrom({ expr, query }), filter: expr, ranking: fieldRanking(query.sortBy ?? "created_at", query.sortDir ?? "desc") }
      : resolvedQueryToPlan(query);
    set({ query, expr: plan.filter ?? undefined, plan, planRevision: get().planRevision + 1, warnings: [], aiExplanation: null, aiError: null, aiLoading: false, resolvedTags: [] });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  setAiInput: (v) => {
    if (v === get().aiInput) return;
    // 输入框是新的用户意图：旧解析即使稍后返回，也不能覆盖正在编辑的文本。
    invalidateAiRequests();
    set({ aiInput: v, aiError: null, aiExplanation: null, warnings: [], parseStatus: null, aiLoading: false });
  },
  setAiResult: (explanation, warnings) =>
    set({ aiExplanation: explanation, warnings, parseStatus: warnings.length > 0 ? "partial" : "full" }),
  clearAiResult: () => {
    invalidateAiRequests();
    set({ aiExplanation: null, warnings: [], parseStatus: null, resolvedTags: [], aiError: null, aiLoading: false });
  },

  applyAiSearch: async (text, mode = "replace") => {
    const aiSeq = ++aiRequestSeq;
    const cur = get().query;
    set({ aiLoading: true, aiError: null });
    try {
      const result = await aiParseSearchQuery(text);
      if (aiSeq !== aiRequestSeq) return;
      // AI 结果即将替换执行计划：让在途列表响应失效，避免旧结果回写覆盖新计划。
      invalidatePendingRequests();
      // §3.7：AI 返回的 expr 摄入时即丢弃（改用 plan 的 filter/mustNot 区，避免排除双算）
      const aiPlan = normalizeSearchPlan(result.plan ?? planFromParseResult(result.expr, result.sortBy, result.sortDir));
      let nextExpr: QueryExpr | undefined;
      let nextPlan: SearchPlanV3 | null;
      let nextQuery: ResolvedSearchQuery;
      let nextResolvedTags: ResolvedTag[];
      let aiWarnings: string[] = result.warnings;
      if (mode === "replace") {
        nextPlan = normalizeSearchPlan(aiPlan);
        nextExpr = aiPlan.filter ?? undefined;
        // query 只同步 sortBy/sortDir，不从复杂 plan 反推扁平条件（§9.6）
        nextQuery = { ...defaultQuery(), sortBy: result.sortBy, sortDir: result.sortDir };
        nextResolvedTags = result.resolvedTags;
      } else {
        // §4.8：append 按表逐字段合并 plan（filter AND / mustNot OR / should 拼接 / ranking+retrievers 保留用户）
        const basePlan = get().plan ?? resolvedQueryToPlan(cur);
        const merged = appendPlanMerge(basePlan, aiPlan);
        nextPlan = normalizeSearchPlan(merged.plan);
        nextExpr = merged.plan.filter ?? undefined;
        nextQuery = cur; // append 不改用户排序（§4.8）
        nextResolvedTags = mergeResolvedTags(get().resolvedTags, result.resolvedTags);
        aiWarnings = [...result.warnings, ...merged.warnings.map((w) => w.message)];
      }
      set({
        query: nextQuery,
        expr: nextExpr,
        plan: nextPlan,
        planRevision: get().planRevision + 1,
        aiExplanation: result.explanation,
        warnings: aiWarnings,
        parseStatus: result.parseStatus,
        resolvedTags: nextResolvedTags,
        aiLoading: false,
      });
      useSelectionStore.getState().clear();
      scheduleRefresh(get().refresh);
    } catch (e) {
      if (aiSeq !== aiRequestSeq) return;
      // §9.7：AI 解析失败只设 aiError；保留当前 query/expr/items/total，不触发 refresh
      set({ aiLoading: false, aiError: e instanceof Error ? e.message : String(e) });
    }
  },

  removeAtZonePath: (zone, path) => {
    if (zone === "should") {
      get().removePlanShould(typeof path === "number" ? path : path[0]);
      return;
    }
    if (typeof path === "number") return;
    if (zone === "filter") get().removePlanFilterAt(path);
    else get().removePlanMustNotAt(path);
  },

  removeExprAtPath: (path) => get().removeAtZonePath("filter", path),

  /** S5 5-4：零结果建议「试试相近的词：X」可点 —— 点击才替换失败的按词查 leaf。
   *  后端建议只是提示（不改写）；此处是用户显式动作的落点。 */
  applyTermSuggestion: (sourceTerm, term, match) => {
    const source = sourceTerm.trim();
    const replacement = term.trim();
    if (!replacement) return;
    const cur = get();
    const current = cur.plan?.filter ?? cur.expr;
    const node: QueryExpr = {
      op: "leaf",
      cond: { type: "tag", facetKey: "", tagIds: [], mode: "any", includeDescendants: true, termQuery: replacement, termMatch: match },
    };
    if (current && source) {
      const replaced = replaceFirstTermLeaf(current, source, replacement, match);
      if (replaced.replaced) {
        cur.setPlanFilter(replaced.expr);
        return;
      }
    }
    // 兼容没有原始失败叶子的旧 warning 或手工点击路径：追加而不覆盖现有条件。
    const merged = mergeQueryExpr(current, node);
    cur.setPlanFilter(merged ? normalizeExpr(merged) : undefined);
  },

  removePlanFilterAt: (path) => {
    const plan = get().plan;
    if (!plan?.filter) return;
    const next = removeExprAtPath(plan.filter, path);
    get().setPlanFilter(next);
  },

  removePlanMustNotAt: (path) => {
    const plan = get().plan;
    if (!plan?.mustNot) return;
    const next = removeExprAtPath(plan.mustNot, path);
    get().setPlanMustNot(next);
  },

  moveConditionBetweenZones: (from, to, path) => {
    if (from === to) return;
    const cur = get();
    const plan = cur.plan;
    if (!plan) return;
    let node: QueryExpr | undefined;
    let filter = plan.filter;
    let mustNot = plan.mustNot;
    let should = plan.should;
    let min = plan.minimumShouldMatch;
    if (to === "should" && from !== "should" && should.length >= MAX_SHOULD_CLAUSES) {
      set((state) => ({
        executionWarnings: [
          ...state.executionWarnings,
          {
            source: "plan",
            zone: "should",
            message: `优先条件已达到上限（${MAX_SHOULD_CLAUSES} 条），未移动该条件。`,
          },
        ],
      }));
      return;
    }
    if (from === "should") {
      // 优先区按 index 取整条加分项（叶子）
      const idx = typeof path === "number" ? path : Number(path[0]);
      const clause = plan.should[idx];
      if (!clause) return;
      node = { op: "leaf", cond: clause.cond };
      should = should.filter((_, i) => i !== idx);
      min = Math.min(min, should.length);
    } else {
      const src = from === "filter" ? plan.filter : plan.mustNot;
      if (!src) return;
      const exprPath = typeof path === "number" ? [path] : path;
      node = nodeAtPath(src, exprPath);
      if (!node) return;
      const srcRemoved = removeExprAtPath(src, exprPath);
      if (from === "filter") filter = srcRemoved ?? null;
      else mustNot = srcRemoved ?? null;
    }
    // 目标区并入：移动项 append 到目标区根组，按目标根组当前连接词参与
    // （filter 根为 OR 时并进 OR、mustNot 根为 AND 时并进 AND，不再写死拼接）；
    // should 拼接（默认一般偏好 1.0；加分项上限 12 由 setPlanShould 语义裁剪）
    if (to === "filter") {
      const op = filter && filter.op === "or" ? "or" : "and";
      filter = normalizeExpr(appendToExpr(filter ?? undefined, node, op)) ?? null;
    } else if (to === "mustNot") {
      const op = mustNot && mustNot.op === "and" ? "and" : "or";
      mustNot = normalizeExpr(appendToExpr(mustNot ?? undefined, node, op)) ?? null;
    } else {
      if (node.op !== "leaf") return; // 加分项只接受叶子（组仍留在原区）
      should = [...should, { cond: node.cond, weight: 1, label: "", evidence: null }].slice(0, 12);
      min = should.length ? min : 0;
    }
    const nextPlan = maybeNullPlan(normalizeSearchPlan({ ...plan, filter, mustNot, should, minimumShouldMatch: min }));
    const resolvedTags = filterResolvedTagsByPlan(cur.resolvedTags, nextPlan);
    invalidateAiRequests();
    const query = syncQueryFromExpr({ ...defaultQuery(), sortBy: cur.query.sortBy, sortDir: cur.query.sortDir }, nextPlan?.filter ?? undefined);
    invalidatePendingRequests();
    set({ query, plan: nextPlan, expr: nextPlan?.filter ?? undefined, planRevision: cur.planRevision + 1, resolvedTags, aiLoading: false });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  clearZone: (zone) => {
    if (zone === "should") {
      const plan = get().plan;
      if (plan && plan.should.length > 0) get().setPlanShould([], 0);
      return;
    }
    if (zone === "filter") get().setPlanFilter(undefined);
    else get().setPlanMustNot(undefined);
  },

  setPlanShould: (should, _minimumShouldMatch) => {
    const cur = get();
    if (!cur.plan && should.length === 0) return; // 没有 plan 也没有加分项：无事可做
    const clamped = 0;
    // U-5：手动条件首次加分时，从当前 expr/query 起一个最小 plan（filter 单源镜像）
    const plan: SearchPlanV3 = cur.plan ?? minimalPlanFrom(cur);
    const next = maybeNullPlan(normalizeSearchPlan({ ...plan, should, minimumShouldMatch: clamped }));
    const query = syncQueryFromExpr({ ...defaultQuery(), sortBy: cur.query.sortBy, sortDir: cur.query.sortDir }, next?.filter ?? undefined);
    const resolvedTags = filterResolvedTagsByPlan(cur.resolvedTags, next);
    invalidatePendingRequests();
    invalidateAiRequests();
    set({ query, plan: next, expr: next?.filter ?? undefined, planRevision: cur.planRevision + 1, resolvedTags, aiLoading: false });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  removePlanShould: (index) => {
    const plan = get().plan;
    if (!plan) return;
    const should = plan.should.filter((_, i) => i !== index);
    get().setPlanShould(should, Math.min(plan.minimumShouldMatch, should.length));
  },

  setSort: (sortBy, sortDir) => {
    const cur = get().query;
    if (cur.sortBy === sortBy && cur.sortDir === sortDir) return;
    get().setQuery({ sortBy, sortDir });
  },

  clearConditions: () => {
    invalidatePendingRequests();
    invalidateAiRequests();
    const def = defaultQuery();
    set({ query: def, expr: undefined, plan: null, planRevision: get().planRevision + 1, warnings: [], aiExplanation: null, aiError: null, aiLoading: false, resolvedTags: [] });
    useSelectionStore.getState().clear();
    scheduleRefresh(get().refresh);
  },

  refresh: async () => {
    const seq = ++listRequestSeq;
    const { query, plan } = get();
    // B7：执行 warning 生命周期 = 每次 refresh（请求开始清空、响应写入）
    set({ loading: true, error: null, executionWarnings: [] });
    try {
      // §3.7：列表执行一律走 plan（空条件时由扁平 query 派生空 plan）—— 单一编译器
      const execPlan = plan ?? resolvedQueryToPlan(query);
      const page = await listSuperAssets(query, 0, PAGE_SIZE, execPlan);
      if (seq !== listRequestSeq) return;
      set({ items: dedupItems(page.items), total: page.total, loading: false, executionWarnings: normalizeWarnings(page.warnings) });
    } catch (e) {
      if (seq !== listRequestSeq) return;
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  loadMore: async () => {
    const { items, total, loading, query, plan } = get();
    if (loading || items.length >= total) return;
    const seq = ++listRequestSeq;
    set({ loading: true });
    try {
      const execPlan = plan ?? resolvedQueryToPlan(query);
      const page = await listSuperAssets(query, items.length, PAGE_SIZE, execPlan);
      if (seq !== listRequestSeq) return;
      const known = new Set(items.map((a) => a.id));
      set({
        items: [...items, ...page.items.filter((a) => !known.has(a.id))],
        total: page.total,
        loading: false,
        executionWarnings: normalizeWarnings(page.warnings),
      });
    } catch (e) {
      if (seq !== listRequestSeq) return;
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  fetchAllIds: async () => {
    const { query, plan } = get();
    // §4.6（B2/B8）：全选 ID 一律走 plan（PlanIdsResult 一路到底，不降级成 number[]）
    const execPlan = plan ?? resolvedQueryToPlan(query);
    const result = await listSuperAssetIdsByPlan(execPlan);
    if (result.warnings.length > 0) {
      set((s) => ({ executionWarnings: [...s.executionWarnings, ...result.warnings] }));
    }
    return result;
  },

  patchLocal: (ids, patch) => {
    const hit = new Set(ids);
    set((s) => ({ items: s.items.map((a) => (hit.has(a.id) ? { ...a, ...patch } : a)) }));
  },

  clearQuery: () => {
    const def = defaultQuery();
    void get().replaceQuery(def, undefined);
  },
}),
{
  name: "super-search-conditions",
  // §3.7 不变式 5：持久化只存 plan + query.sortBy/sortDir（expr 是 plan.filter 的派生视图，不再存）
  version: 2,
  // merge：persisted 的残缺 query（v2 只存 sortBy/sortDir）必须在合并时就归一到完整
  // defaultQuery —— 否则 query.search 等字段为 undefined，首次渲染即崩
  //（SuperSearchPage 的 query.search.trim()；onRehydrateStorage 补全来不及在首帧前生效）。
  merge: (persisted, current) => {
    const p = persisted as { plan?: SearchPlanV3 | null; query?: Partial<ResolvedSearchQuery> } | null;
    const migratedPlan = p?.plan ? migratePlanV3(p.plan) : null;
    const futurePlanWasDiscarded = Boolean(p?.plan && !migratedPlan);
    const futureWarning: SearchWarning = {
      source: "plan",
      message: "保存的搜索条件来自更新版本，已重置搜索条件。",
    };
    return {
      ...current,
      plan: migratedPlan,
      expr: migratedPlan?.filter ?? undefined,
      query: { ...defaultQuery(), ...(p?.query ?? {}) },
      executionWarnings: futurePlanWasDiscarded && !current.executionWarnings.some((w) => w.message === futureWarning.message)
        ? [...current.executionWarnings, futureWarning]
        : current.executionWarnings,
    } as typeof current;
  },
  partialize: (state) => ({
    plan: state.plan,
    query: { sortBy: state.query.sortBy, sortDir: state.query.sortDir },
  }),
  // v2 迁移：丢弃 expr（旧数据补成 plan.filter 以防丢条件）；query 归一为仅排序字段。
  migrate: (persisted) => {
    const p = persisted as {
      expr?: QueryExpr | null;
      query?: { sortBy?: string; sortDir?: string } | null;
      plan?: SearchPlanV3 | null;
    } | null;
    let plan = p?.plan ?? null;
    if (!plan && p?.expr) {
      plan = {
        planSchemaVersion: 3,
        normalizationVersion: 1,
        compilerVersion: 1,
        filter: p.expr,
        mustNot: null,
        should: [],
        minimumShouldMatch: 0,
        retrievers: { retrievers: [], fusion: "rrf" },
        ranking: fieldRanking(p.query?.sortBy ?? "created_at", p.query?.sortDir ?? "desc"),
      };
    }
    // 版本判断统一放在 merge：这样未来版本 plan 仍能被识别并产生一次用户 warning，
    // 不会在 migrate 阶段先变成 null 而丢掉提示。
    return {
      plan,
      query: { sortBy: p?.query?.sortBy ?? "created_at", sortDir: p?.query?.sortDir ?? "desc" },
    } as never;
  },
  // hydrate 只补齐兼容 query/expr；plan 的迁移和未来版本 warning 已在 merge 原子完成。
  onRehydrateStorage: () => (state) => {
    const p = state as { plan?: SearchPlanV3 | null; query?: Partial<ResolvedSearchQuery> } | undefined;
    const def = defaultQuery();
    useSuperSearchStore.setState({
      query: { ...def, sortBy: p?.query?.sortBy ?? def.sortBy, sortDir: p?.query?.sortDir ?? def.sortDir },
    });
    useSuperSearchStore.setState({ expr: p?.plan?.filter ?? undefined });
  },
},
  ),
);

export type { MetadataFilter, FetchAllIdsResult };
