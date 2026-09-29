/** 素材相关命令封装（commands/assets_cmd.rs）→ 超级搜索查询（P4 表达式构建器） */
import { invoke, on } from "./client";
import { listAssets, listAssetIds } from "./assets";
import type { AssetFilter, AssetPage, ResolvedSearchQuery } from "@/types/asset";
import type { QueryExpr } from "@/types/queryExpr";
import type { AiSearchParseResult, PlanAssetPage, PlanDiagnostics, PlanIdsResult, SearchPlanV3, SearchWarning } from "@/types/superSearch";
import type { UnlistenFn } from "@tauri-apps/api/event";

export type AiSearchPhase = "queued" | "requesting" | "validating" | "completed" | "failed" | "cancelled" | "cancelling";

export interface AiSearchProgress {
  requestId: string;
  phase: AiSearchPhase;
  elapsedMs: number;
  errorCode: string | null;
}

/** 列表页结果：plan 路径 warnings 为 SearchWarning[]，无 plan 旧链路为 string[]（store 归一化）。 */
export type SuperListPage = Omit<AssetPage, "warnings"> & { warnings?: (string | SearchWarning)[] };

/** ResolvedSearchQuery + 可选 expr → 后端 AssetFilter（分页由调用方提供）。
 *  仅无 plan 的旧链路（空条件）使用；有 plan 一律走 list_assets_by_plan（§4.1 单一编译器）。 */
export function queryToFilter(
  q: ResolvedSearchQuery,
  offset: number,
  limit?: number,
  expr?: QueryExpr,
): AssetFilter {
  return {
    assetType: q.assetType,
    untaggedOnly: q.untaggedOnly,
    facetFilters: q.facetFilters.map((f) => ({
      facetKey: f.facetKey,
      tagIds: f.tagIds,
      mode: f.mode,
      includeDescendants: f.includeDescendants,
    })),
    excludeTagIds: q.excludeTagIds,
    metadataFilters: q.metadataFilters,
    search: q.search || undefined,
    sortBy: q.sortBy,
    sortDir: q.sortDir,
    trashOnly: false,
    expr,
    offset,
    limit,
  };
}

/** Phase 2 store 换源（§3.7）：列表执行以 plan 为唯一事实源。
 *  传入 plan → 走 list_assets_by_plan（validate → prune → 执行，warnings 为 SearchWarning[]）；
 *  无 plan（空条件旧链路）→ 回退扁平查询。 */
export function listSuperAssets(
  q: ResolvedSearchQuery,
  offset: number,
  limit?: number,
  plan?: SearchPlanV3,
): Promise<SuperListPage> {
  if (plan) {
    return invoke<PlanAssetPage>("list_assets_by_plan", { plan, offset, limit: limit ?? null });
  }
  return listAssets(queryToFilter(q, offset, limit));
}

export function listSuperAssetIds(q: ResolvedSearchQuery, expr?: QueryExpr): Promise<number[]> {
  return listAssetIds(queryToFilter(q, 0, undefined, expr));
}

/** FB5-05（§9.5）：AI 自然语言 → SearchIntentV3 + 后端生成的 QueryExpr（唯一执行事实源）。
 *  已删除未使用的 currentQuery 参数——append 由前端明确合并 expr。 */
export function aiParseSearchQuery(text: string, requestId: string): Promise<AiSearchParseResult> {
  return invoke<AiSearchParseResult>("ai_parse_search_query", { text, requestId });
}

export function cancelAiSearch(requestId: string): Promise<void> {
  return invoke<void>("cancel_ai_search", { requestId });
}

export function onAiSearchProgress(handler: (progress: AiSearchProgress) => void): Promise<UnlistenFn> {
  return on<AiSearchProgress>("super-search://progress", handler);
}

/** C-2/U-6/§4.5：对 SearchPlanV3 做 AST 命中诊断（validate → prune 后）。
 *  planRevision 为发起时的 plan 代次（§3.7 不变式 9），后端原样回显到每条诊断，
 *  前端据此对在途旧诊断整批丢弃。返回叶子带 zone、加分项带 index、warnings 与列表命令同一批。 */
export function diagnoseSearchPlan(plan: SearchPlanV3, planRevision: number): Promise<PlanDiagnostics> {
  return invoke<PlanDiagnostics>("diagnose_search_plan_cmd", { plan, planRevision });
}

/** Phase 2 §4.1：plan 执行 —— 结果列表（分页）。后端 validate → prune → execute。 */
export function listSuperAssetsByPlan(
  plan: SearchPlanV3,
  offset: number,
  limit?: number,
): Promise<PlanAssetPage> {
  return invoke<PlanAssetPage>("list_assets_by_plan", { plan, offset, limit: limit ?? null });
}

/** Phase 2 §4.1（B2/B8）：plan 全选 ID —— PlanIdsResult 一路到底（不降级成 number[]）。 */
export function listSuperAssetIdsByPlan(plan: SearchPlanV3): Promise<PlanIdsResult> {
  return invoke<PlanIdsResult>("list_asset_ids_by_plan", { plan });
}
