/** Phase 1/2 执行契约 §4.1a / §4.8：ResolvedSearchQuery → SearchPlanV3 与 AI「追加模式」合并。
 *  这些是纯函数 —— 与后端 db/search_plan.rs 的 validate/prune/compile 语义逐条对齐，
 *  契约测试（35 条中约 7 条 TS 侧）直接对它们断言。 */
import type { ResolvedSearchQuery } from "@/types/asset";
import type { LeafCond, QueryExpr } from "@/types/queryExpr";
import type {
  PlanWarningSource,
  Ranking,
  SearchPlanV3,
  SearchWarning,
  ShouldClause,
} from "@/types/superSearch";
import { metadataFilterOf, normalizeExprMetadata, normalizeMetadataCond } from "@/utils/queryExprUtils";

/** S6：版本常量（与后端 db/search_plan.rs 单点声明对齐）。 */
export const PLAN_SCHEMA_VERSION = 3;
export const NORMALIZATION_VERSION = 1;
export const COMPILER_VERSION = 1;
/** S1：加分条数上限。 */
export const MAX_SHOULD_CLAUSES = 12;

const SHOULD_WEIGHTS = [2.0, 1.0, 0.5] as const;

/** 新版优先区契约：权重由稳定的可见位置生成，UI 不编辑权重。 */
export function weightForShouldPosition(index: number, length: number): number {
  if (length <= 0) return SHOULD_WEIGHTS[2];
  const first = Math.ceil(length / 3);
  const second = Math.ceil((length * 2) / 3);
  return index < first ? SHOULD_WEIGHTS[0] : index < second ? SHOULD_WEIGHTS[1] : SHOULD_WEIGHTS[2];
}

/** 按优先区顺序截断并重算三档权重；不修改输入数组或 clause。 */
export function normalizeShouldByPosition(should: ShouldClause[]): ShouldClause[] {
  // 不完整的手工草稿不得进入可执行计划。QueryBuilder 仍保留草稿行，
  // 但 store / hydrate 的边界必须把历史残留的空条件挡在执行链外。
  const canonical = should.map((clause) => ({ ...clause, cond: normalizeMetadataCond(clause.cond) }));
  const kept = canonical.filter((clause) => isCompleteLeafCond(clause.cond)).slice(0, MAX_SHOULD_CLAUSES);
  return kept.map((clause, index) => ({ ...clause, weight: weightForShouldPosition(index, kept.length) }));
}

/** SearchPlanV3 的叶子完整性判定，与 QueryBuilder 的草稿收敛规则保持一致。 */
export function isCompleteLeafCond(cond: LeafCond, circularKeys?: ReadonlySet<string>): boolean {
  if (cond.type === "tag") return cond.tagIds.length > 0 || Boolean(cond.termQuery?.trim());
  if (cond.type === "excludeTag") return cond.tagIds.length > 0;
  if (cond.type === "search") return cond.value.trim().length > 0;
  if (cond.type === "facetNumber") {
    if (!Number.isFinite(cond.value)) return false;
    if (cond.op === "between") {
      return cond.maxValue != null && Number.isFinite(cond.maxValue) && cond.value <= cond.maxValue;
    }
    return true;
  }
  if (cond.type !== "metadata") return true;
  const filter = metadataFilterOf(cond);
  if (!filter) return false;
  if (filter.op === "between") {
    if (filter.min === undefined || filter.max === undefined) return false;
    const lo = typeof filter.min === "number" ? filter.min : Number(filter.min);
    const hi = typeof filter.max === "number" ? filter.max : Number(filter.max);
    // Store-side normalization may not have loaded numeric domains yet. In that
    // case preserve an inverted range and let the backend's single domain source
    // decide (notably dominant_hue's wrap-around interval); the builder passes a
    // concrete set and can remain conservative while editing.
    if (Number.isFinite(lo) && Number.isFinite(hi) && lo > hi && circularKeys && !circularKeys.has(filter.key)) return false;
    return true;
  }
  if (filter.op === "in") return Boolean(filter.values?.length);
  return filter.value !== undefined && filter.value !== "";
}

/** SearchPlanV3 的单一归一化边界：min 恒为 0，should 顺序是唯一优先级来源。 */
export function normalizeSearchPlan(plan: SearchPlanV3): SearchPlanV3 {
  return {
    ...plan,
    filter: plan.filter ? normalizeExprMetadata(plan.filter) : null,
    mustNot: plan.mustNot ? normalizeExprMetadata(plan.mustNot) : null,
    should: normalizeShouldByPosition(plan.should),
    minimumShouldMatch: 0,
  };
}

/** 不可变的优先区重排。to 是删除 source 后的最终插入下标。 */
export function reorderShould<T>(items: T[], from: number, to: number): T[] {
  if (from < 0 || from >= items.length || to < 0 || to > items.length || from === to) return items.slice();
  const next = items.slice();
  const [item] = next.splice(from, 1);
  if (item === undefined) return next;
  next.splice(Math.min(to, next.length), 0, item);
  return next;
}

export function fieldRanking(key: string, dir: string): Ranking {
  return { type: "field", key, dir: dir === "asc" ? "asc" : "desc" };
}

function leaf(cond: LeafCond): QueryExpr {
  return { op: "leaf", cond };
}

function andOf(leaves: QueryExpr[]): QueryExpr | null {
  if (leaves.length === 0) return null;
  if (leaves.length === 1) return leaves[0];
  return { op: "and", children: leaves };
}

function orOf(leaves: QueryExpr[]): QueryExpr | null {
  if (leaves.length === 0) return null;
  if (leaves.length === 1) return leaves[0];
  return { op: "or", children: leaves };
}

function defaultPlan(filter: QueryExpr | null, mustNot: QueryExpr | null, ranking: Ranking): SearchPlanV3 {
  return {
    planSchemaVersion: PLAN_SCHEMA_VERSION,
    normalizationVersion: NORMALIZATION_VERSION,
    compilerVersion: COMPILER_VERSION,
    filter,
    mustNot,
    should: [],
    minimumShouldMatch: 0,
    retrievers: { retrievers: [], fusion: "rrf" },
    ranking,
  };
}

/** §4.1a：扁平查询 → SearchPlanV3。
 *  - 正向条件（search/assetType/untagged/facetFilters/metadata）→ plan.filter（多叶子 AND）
 *  - excludeTagIds → mustNot 里的**正向 Tag**（不是 excludeTag）；多条 → Or
 *  - sortBy/sortDir → ranking（B9：字段主键 + score 次级由后端保证）
 */
export function resolvedQueryToPlan(q: ResolvedSearchQuery): SearchPlanV3 {
  const filterLeaves: QueryExpr[] = [];
  if (q.search.trim()) filterLeaves.push(leaf({ type: "search", value: q.search.trim() }));
  if (q.assetType !== "all") filterLeaves.push(leaf({ type: "assetType", value: q.assetType }));
  if (q.untaggedOnly) filterLeaves.push(leaf({ type: "untagged" }));
  for (const f of q.facetFilters) {
    if (f.tagIds.length === 0) continue;
    filterLeaves.push(
      leaf({
        type: "tag",
        facetKey: f.facetKey,
        tagIds: f.tagIds,
        mode: f.mode ?? "any",
        includeDescendants: f.includeDescendants,
      }),
    );
  }
  for (const m of q.metadataFilters) filterLeaves.push(leaf({ type: "metadata", filter: m }));
  // 排除：正向 Tag 进 mustNot（P0 极性：mustNot 只放正向条件，统一由计划层包 NOT）
  const exclusionLeaves = q.excludeTagIds.map((tagId) =>
    leaf({ type: "tag", facetKey: "", tagIds: [tagId], mode: "any", includeDescendants: true }),
  );
  return defaultPlan(
    andOf(filterLeaves),
    orOf(exclusionLeaves),
    fieldRanking(q.sortBy ?? "created_at", q.sortDir ?? "desc"),
  );
}

/** 纯叶子序列化（稳定 key 顺序由构造方保证）。 */
export function serializeCond(c: LeafCond): string {
  return JSON.stringify(c);
}

export function warning(source: PlanWarningSource, message: string, zone?: SearchWarning["zone"]): SearchWarning {
  return { source, message, ...(zone ? { zone } : {}) };
}

function exprKey(child: QueryExpr): string {
  return child.op === "leaf" ? serializeCond(child.cond) : JSON.stringify(child);
}

/** §4.8：filter 按 AND 合并（保留内部 OR/AND 分组），按序列化去重相同叶子。 */
function andMerge(a: QueryExpr | null, b: QueryExpr | null): QueryExpr | null {
  if (!a) return b;
  if (!b) return a;
  const seen = new Set<string>();
  const children: QueryExpr[] = [];
  for (const child of [...(a.op === "and" ? a.children : [a]), ...(b.op === "and" ? b.children : [b])]) {
    const key = exprKey(child);
    if (seen.has(key)) continue;
    seen.add(key);
    children.push(child);
  }
  return andOf(children);
}

/** §4.8：mustNot 按 OR 合并（任一命中即排除），同样去重。 */
function orMerge(a: QueryExpr | null, b: QueryExpr | null): QueryExpr | null {
  if (!a) return b;
  if (!b) return a;
  const seen = new Set<string>();
  const children: QueryExpr[] = [];
  for (const child of [...(a.op === "or" ? a.children : [a]), ...(b.op === "or" ? b.children : [b])]) {
    const key = exprKey(child);
    if (seen.has(key)) continue;
    seen.add(key);
    children.push(child);
  }
  return orOf(children);
}

/** §4.8：AI「追加模式」合并 —— replace 与 append 只差 ranking/retrievers。 */
export function appendPlanMerge(
  base: SearchPlanV3,
  incoming: SearchPlanV3,
): { plan: SearchPlanV3; warnings: SearchWarning[] } {
  const warnings: SearchWarning[] = [];
  // filter：AND 合并（保留内部 OR）
  const filter = andMerge(base.filter, incoming.filter);
  // mustNot：OR 合并
  const mustNot = orMerge(base.mustNot, incoming.mustNot);
  // should：拼接 + 去重 + 超 12 按当前用户可见顺序保留前 12
  const seen = new Set<string>();
  const merged: ShouldClause[] = [];
  for (const sc of [...base.should, ...incoming.should]) {
    const key = serializeCond(sc.cond);
    if (seen.has(key)) continue;
    seen.add(key);
    merged.push(sc);
  }
  const dropped = Math.max(0, merged.length - MAX_SHOULD_CLAUSES);
  const should = normalizeShouldByPosition(merged);
  if (dropped > 0) {
    warnings.push(
      warning("plan", `加分项超过 ${MAX_SHOULD_CLAUSES} 条，已按当前优先顺序保留前 ${MAX_SHOULD_CLAUSES} 条。`, "should"),
    );
  }
  // minimumShouldMatch：old.should 非空时保持用户值；最后 clamp(0, len)
  const minimumShouldMatch = 0;
  // ranking：append 恒保留用户的；AI 建议不同 → 提示
  if (JSON.stringify(base.ranking) !== JSON.stringify(incoming.ranking)) {
    warnings.push(warning("plan", "AI 建议的排序方式未被采纳（追加模式保留当前排序）。"));
  }
  const plan: SearchPlanV3 = {
    ...defaultPlan(filter, mustNot, base.ranking),
    should,
    minimumShouldMatch,
    // retrievers：append 保留旧的；AI 的不同 → 提示
    retrievers:
      JSON.stringify(base.retrievers) === JSON.stringify(incoming.retrievers)
        ? base.retrievers
        : (warnings.push(warning("plan", "AI 建议的召回方式未被采纳（追加模式保留当前召回）。")), base.retrievers),
    ranking: base.ranking,
  };
  return { plan, warnings };
}

/** §3.7/§4.8：排除区 OR 合并的公开入口（store 的 moveConditionBetweenZones 等使用）。 */
export function mergeMustNotExpr(a: QueryExpr | null, b: QueryExpr | null): QueryExpr | null {
  return orMerge(a, b);
}

/** §4.4：plan 版本迁移（store hydrate 无条件执行；未来版本 → null 丢弃）。 */
export function migratePlanV3(plan: SearchPlanV3): SearchPlanV3 | null {
  if (plan.planSchemaVersion > PLAN_SCHEMA_VERSION || plan.normalizationVersion > NORMALIZATION_VERSION) {
    return null;
  }
  return normalizeSearchPlan({
    ...plan,
    planSchemaVersion: PLAN_SCHEMA_VERSION,
    normalizationVersion: NORMALIZATION_VERSION,
    compilerVersion: COMPILER_VERSION,
  });
}
