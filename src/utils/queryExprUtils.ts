/** QueryExpr 与 AssetFilter 的互转 + 构建器 JSON 序列化 + FB5-05（§9.6.1）expr 驱动 chips。 */
import type { AssetFilter, FacetTagFilter, MetadataFilter, ResolvedSearchQuery } from "@/types/asset";
import type { LeafCond, QueryExpr } from "@/types/queryExpr";
import type { ResolvedTag } from "@/types/superSearch";

/** FB5-05（§9.6.1）：expr 节点路径（从根逐层取子下标） */
export type ExprPath = number[];

/** FB5-05（§9.6.1）：可展示 chip 模型。group 为 OR/AND 组标签（任一组 N / 同时满足）。 */
export interface ExprChipModel {
  key: string;
  label: string;
  /** 删除时传给 removeExprAtPath 的稳定路径 */
  path: ExprPath;
  group?: string;
}

/** 扁平 AssetFilter → QueryExpr（多 AND + 排除 NOT）。当 expr 已存在时直接返回它。 */
export function buildExprFromFilter(f: AssetFilter): QueryExpr {
  if (f.expr) return f.expr;
  const leaves: QueryExpr[] = [];
  if (f.search) leaves.push(leaf({ type: "search", value: f.search }));
  if (f.assetType && f.assetType !== "all") leaves.push(leaf({ type: "assetType", value: f.assetType }));
  if (f.untaggedOnly) leaves.push(leaf({ type: "untagged" }));
  for (const fct of f.facetFilters ?? []) {
    if (!fct.tagIds.length) continue;
    leaves.push(
      leaf({
        type: "tag",
        facetKey: fct.facetKey,
        tagIds: fct.tagIds,
        mode: fct.mode ?? "any",
        includeDescendants: fct.includeDescendants,
      }),
    );
  }
  for (const tagId of f.excludeTagIds ?? []) {
    leaves.push(leaf({ type: "excludeTag", facetKey: "", tagIds: [tagId] }));
  }
  for (const m of f.metadataFilters ?? []) {
    leaves.push(leaf({ type: "metadata", filter: m }));
  }
  if (leaves.length === 0) return { op: "and", children: [] };
  if (leaves.length === 1) return leaves[0];
  return { op: "and", children: leaves };
}

/** AI/后端返回的执行查询 -> 公式树。空查询不制造后端无法执行的空分组。 */
export function resolvedQueryToExpr(q: ResolvedSearchQuery): QueryExpr | undefined {
  const leaves: QueryExpr[] = [];
  if (q.search.trim()) leaves.push(leaf({ type: "search", value: q.search.trim() }));
  if (q.assetType !== "all") leaves.push(leaf({ type: "assetType", value: q.assetType }));
  if (q.untaggedOnly) leaves.push(leaf({ type: "untagged" }));
  for (const f of q.facetFilters) {
    if (!f.tagIds.length) continue;
    leaves.push(leaf({
      type: "tag",
      facetKey: f.facetKey,
      tagIds: f.tagIds,
      mode: f.mode,
      includeDescendants: f.includeDescendants,
    }));
  }
  for (const tagId of q.excludeTagIds) {
    leaves.push(leaf({ type: "excludeTag", facetKey: "", tagIds: [tagId] }));
  }
  for (const filter of q.metadataFilters) leaves.push(leaf({ type: "metadata", filter }));
  if (leaves.length === 0) return undefined;
  return leaves.length === 1 ? leaves[0] : { op: "and", children: leaves };
}

/** 追加 AI 条件时保留已有布尔结构，并以 AND 组合两个条件组。 */
export function mergeQueryExpr(a?: QueryExpr, b?: QueryExpr): QueryExpr | undefined {
  if (!a) return b;
  if (!b) return a;
  const children = [
    ...(a.op === "and" ? a.children : [a]),
    ...(b.op === "and" ? b.children : [b]),
  ];
  return children.length === 1 ? children[0] : { op: "and", children };
}

/** 跨区移动：把节点追加为目标区根组的一项，按目标根组当前连接词参与
 *  （根组 op 一致直接 append，否则包一层 op 组；调用方 normalizeExpr 拍平同 op 嵌套）。 */
export function appendToExpr(root: QueryExpr | undefined, node: QueryExpr, op: "and" | "or"): QueryExpr {
  if (!root) return node;
  if (root.op === op) return { op, children: [...root.children, node] };
  return { op, children: [root, node] };
}

function leaf(cond: LeafCond): QueryExpr {
  return { op: "leaf", cond };
}

/**
 * Metadata leaf compatibility at the JS/Rust boundary.
 *
 * The current contract is `{ type: "metadata", filter: MetadataFilter }`, but
 * older Rust builds serialized the MetadataFilter fields directly on `cond`
 * (`{ type: "metadata", key, op, ... }`). Normalize the legacy shape as it
 * enters any expression utility so persisted plans and in-flight responses do
 * not crash the display layer.
 */
type MetadataCondWire = {
  type: "metadata";
  filter?: MetadataFilter;
  key?: unknown;
  op?: unknown;
  value?: unknown;
  values?: unknown;
  min?: unknown;
  max?: unknown;
  unit?: unknown;
};

export function normalizeMetadataCond(cond: LeafCond): LeafCond {
  if (cond.type !== "metadata") return cond;
  const raw = cond as unknown as MetadataCondWire;
  if (raw.filter && typeof raw.filter === "object") return cond;
  if (typeof raw.key !== "string" || typeof raw.op !== "string") return cond;

  const filter: MetadataFilter = {
    key: raw.key as MetadataFilter["key"],
    op: raw.op as MetadataFilter["op"],
  };
  if (raw.value !== undefined) filter.value = raw.value as MetadataFilter["value"];
  if (Array.isArray(raw.values)) filter.values = raw.values as MetadataFilter["values"];
  if (raw.min !== undefined) filter.min = raw.min as MetadataFilter["min"];
  if (raw.max !== undefined) filter.max = raw.max as MetadataFilter["max"];
  if (raw.unit === "KB" || raw.unit === "MB" || raw.unit === "GB") filter.unit = raw.unit;
  return { type: "metadata", filter };
}

/** Return a metadata filter from either the current or legacy wire shape. */
export function metadataFilterOf(cond: LeafCond): MetadataFilter | undefined {
  const normalized = normalizeMetadataCond(cond);
  return normalized.type === "metadata" ? normalized.filter : undefined;
}

/** Recursively canonicalize metadata leaves without changing boolean shape or paths. */
export function normalizeExprMetadata(expr: QueryExpr): QueryExpr {
  if (expr.op === "leaf") return { ...expr, cond: normalizeMetadataCond(expr.cond) };
  if (expr.op === "not") return { ...expr, child: normalizeExprMetadata(expr.child) };
  return { ...expr, children: expr.children.map(normalizeExprMetadata) };
}

/** QueryExpr → AssetFilter 的扁平字段（用于兼容非 expr 链路或回填）。
 *  只提取 AND 顶层的叶子/简单形态；复杂嵌套仅保留 expr 本身。 */
export function queryExprToFilterFields(e: QueryExpr): {
  search?: string;
  assetType?: AssetFilter["assetType"];
  untaggedOnly?: boolean;
  facetFilters?: FacetTagFilter[];
  excludeTagIds?: number[];
  metadataFilters?: MetadataFilter[];
} {
  const children = e.op === "and" ? e.children : [e];
  const out: ReturnType<typeof queryExprToFilterFields> = {};
  const facetFilters: FacetTagFilter[] = [];
  const excludeTagIds: number[] = [];
  const metadataFilters: MetadataFilter[] = [];
  for (const c of children) {
    if (c.op === "leaf") {
      const cond = c.cond;
      if (cond.type === "search") out.search = cond.value;
      else if (cond.type === "assetType") out.assetType = cond.value;
      else if (cond.type === "untagged") out.untaggedOnly = true;
      else if (cond.type === "tag") {
        facetFilters.push({ facetKey: cond.facetKey, tagIds: cond.tagIds, mode: cond.mode ?? "any", includeDescendants: cond.includeDescendants });
      } else if (cond.type === "excludeTag") excludeTagIds.push(...cond.tagIds);
      else if (cond.type === "metadata") {
        const filter = metadataFilterOf(cond);
        if (filter) metadataFilters.push(filter);
      }
    }
  }
  if (facetFilters.length) out.facetFilters = facetFilters;
  if (excludeTagIds.length) out.excludeTagIds = excludeTagIds;
  if (metadataFilters.length) out.metadataFilters = metadataFilters;
  return out;
}

/** 公式树同步回 query 的兼容字段；复杂 OR/NOT 仍以 expr 为真实执行源。 */
export function syncQueryFromExpr(q: ResolvedSearchQuery, expr?: QueryExpr): ResolvedSearchQuery {
  const base: ResolvedSearchQuery = {
    ...q,
    search: "",
    assetType: "all",
    untaggedOnly: false,
    facetFilters: [],
    excludeTagIds: [],
    metadataFilters: [],
  };
  if (!expr) return base;
  return { ...base, ...queryExprToFilterFields(expr) };
}

/** 把 QueryExpr 转成可发送给后端的 AssetFilter.expr 外的扁平字段（expr 本身单独存在）。 */
export function queryExprToAssetFilterFields(e: QueryExpr): Partial<AssetFilter> {
  const f = queryExprToFilterFields(e);
  return { ...f, expr: e };
}

/** 序列化整棵树（JSON 字符串，供 store 比较脏状态） */
export function serializeExpr(e: QueryExpr): string {
  return JSON.stringify(e);
}

/** FB5-05（§9.5.6/§9.6.1）：前端归一化——空组删除、单子节点组折叠、连续相同 AND/OR 扁平化、重复 leaf 去重。 */
export function normalizeExpr(expr: QueryExpr): QueryExpr | undefined {
  if (expr.op === "leaf") return { ...expr, cond: normalizeMetadataCond(expr.cond) };
  if (expr.op === "not") {
    const child = normalizeExpr(expr.child);
    return child ? { op: "not", child } : undefined;
  }
  const isAnd = expr.op === "and";
  const out: QueryExpr[] = [];
  for (const c of expr.children) {
    const norm = normalizeExpr(c);
    if (!norm) continue;
    if (norm.op === "and" && isAnd) out.push(...norm.children);
    else if (norm.op === "or" && !isAnd) out.push(...norm.children);
    else out.push(norm);
  }
  // 重复 leaf 去重
  const seen = new Set<string>();
  const dedup = out.filter((e) => {
    const key = JSON.stringify(e);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  if (dedup.length === 0) return undefined;
  if (dedup.length === 1) return dedup[0];
  return { op: isAnd ? "and" : "or", children: dedup };
}

/** 摘除 path 指向的节点后归一化；返回 undefined 表示整树被清空。 */
export function removeExprAtPath(expr: QueryExpr, path: ExprPath): QueryExpr | undefined {
  const removed = removeAt(expr, path);
  return removed === undefined ? undefined : normalizeExpr(removed);
}

function removeAt(expr: QueryExpr, path: ExprPath): QueryExpr | undefined {
  if (path.length === 0) return undefined;
  if (expr.op === "leaf") return undefined;
  if (expr.op === "not") {
    if (path.length === 1) return undefined; // 删除 NOT 本身
    const child = removeAt(expr.child, path.slice(1));
    return child === undefined ? undefined : { op: "not", child };
  }
  const [head, ...rest] = path;
  const children: QueryExpr[] = [];
  let changed = false;
  expr.children.forEach((c, i) => {
    if (i === head) {
      const removed = removeAt(c, rest);
      if (removed === undefined) {
        changed = true; // 该子节点被删除
        return;
      }
      children.push(removed);
    } else {
      children.push(c);
    }
  });
  return changed ? { ...expr, children } : expr;
}

/** FB5-05（§9.6.1）：递归遍历 expr 生成展示 chips。
 *  AND 组显示「同时满足」；OR 根按组显示「任一组 1 / 任一组 2」；NOT 叶显示「排除：…」。 */
export function flattenExprForDisplay(expr: QueryExpr, resolvedTags: ResolvedTag[]): ExprChipModel[] {
  const nameById = new Map<number, ResolvedTag>();
  for (const rt of resolvedTags) nameById.set(rt.tagId, rt);
  const chips: ExprChipModel[] = [];
  walk(expr, [], undefined, chips, nameById);
  return chips;
}

function walk(
  expr: QueryExpr,
  path: ExprPath,
  group: string | undefined,
  out: ExprChipModel[],
  nameById: Map<number, ResolvedTag>,
): void {
  switch (expr.op) {
    case "and": {
      const hasNested = expr.children.some((c) => c.op !== "leaf");
      const g = hasNested ? "同时满足" : group;
      expr.children.forEach((c, i) => walk(c, [...path, i], g, out, nameById));
      break;
    }
    case "or": {
      // 多层嵌套时组标签补层级序号（顶层「任一组 1」；嵌套「任一组 2.1」= 第 2 项内第 1 组）
      expr.children.forEach((c, i) => {
        const childPath = [...path, i];
        const label = path.length > 0 ? `任一组 ${childPath.map((v) => v + 1).join(".")}` : `任一组 ${i + 1}`;
        walk(c, childPath, label, out, nameById);
      });
      break;
    }
    case "not": {
      // NOT 叶：显示「排除：…」（tag → 名称；search → 排除内容）
      out.push({ key: `not:${path.join(".")}`, label: notLabel(expr.child, nameById), path });
      break;
    }
    case "leaf": {
      const label = leafLabel(expr, nameById);
      out.push({ key: `leaf:${path.join(".")}`, label, path, group });
      break;
    }
  }
}

function notLabel(leaf: QueryExpr, nameById: Map<number, ResolvedTag>): string {
  if (leaf.op !== "leaf") return "排除：条件";
  const cond = leaf.cond;
  if (cond.type === "tag") {
    const names = cond.tagIds.map((id) => nameById.get(id)?.text ?? `标签#${id}`).join("、");
    return `排除：${names}`;
  }
  if (cond.type === "search") return `排除内容：${cond.value}`;
  return `排除：${leafLabel(leaf, nameById)}`;
}

/** W3：系统分面显示名兜底（纯函数无法读 store）；自建分面回退显示 key。 */
const FACET_NAMES: Record<string, string> = {
  subject: "主体对象", scene: "场景/地点", purpose: "用途",
  color: "色彩", composition: "构图/视角", lighting: "光线/时间", people: "人物属性",
  technical: "可用性/技术特征", custom: "自定义",
};

function leafLabel(leaf: QueryExpr, nameById: Map<number, ResolvedTag>): string {
  if (leaf.op !== "leaf") return "条件";
  const cond = leaf.cond;
  switch (cond.type) {
    case "search": {
      const scopeLabel =
        cond.scope === "fileName" ? "文件名" : cond.scope === "description" ? "描述" : cond.scope === "content" ? "内容" : "关键词";
      return `${scopeLabel}：${cond.value}`;
    }
    case "tag": {
      const fname = FACET_NAMES[cond.facetKey] ?? cond.facetKey;
      const info = cond.tagIds.map((id) => nameById.get(id)?.text ?? `标签#${id}`).join("、");
      return `${fname}：${info}`;
    }
    case "excludeTag": {
      const info = cond.tagIds.map((id) => nameById.get(id)?.text ?? `标签#${id}`).join("、");
      return `排除标签：${info}`;
    }
    case "assetType":
      return cond.value === "image" ? "类型：图片" : cond.value === "video" ? "类型：视频" : "类型：全部";
    case "untagged":
      return "未打标";
    case "metadata":
      return metaLabel(metadataFilterOf(cond));
    default:
      return "条件";
  }
}

const LABELS: Record<string, string> = {
  file_ext: "格式", mime_type: "MIME", width: "宽", height: "高",
  resolution: "像素总量", aspect_ratio: "宽高比", file_size: "文件大小",
  duration_ms: "视频时长", taken_at: "拍摄时间", created_at: "入库时间",
  modified_at: "修改时间", camera: "相机", lens: "镜头", iso: "ISO",
  aperture: "光圈", shutter: "快门", focal: "焦距", video_codec: "视频编码",
  audio_codec: "音频编码", folder: "文件夹",
};

const OP_TEXT: Record<string, string> = { gt: ">", gte: "≥", lt: "<", lte: "≤", eq: "=", contains: "含", between: "", in: "∈" };

function metaLabel(f: MetadataFilter | undefined): string {
  if (!f || typeof f !== "object" || typeof f.key !== "string" || typeof f.op !== "string") return "元数据条件";
  const name = LABELS[f.key] ?? f.key;
  if (f.op === "between") return `${name} ${String(f.min ?? "")}–${String(f.max ?? "")}`;
  if (f.op === "in") return `${name} ∈ ${(f.values ?? []).map(String).join("|")}`;
  if (f.op === "eq" && f.value !== undefined) return `${name} = ${String(f.value)}`;
  return `${name} ${OP_TEXT[f.op] ?? f.op} ${f.value !== undefined ? String(f.value) : ""}`;
}
