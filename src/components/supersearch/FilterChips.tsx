/** 超级搜索条件芯片（FB5-05 §9.6.1 + §3.7）：三区共用一个 chip 行。
 *  - 必须区（plan.filter，expr 为派生视图）→ 组标签「必须」，删除走 removeAtZonePath("filter", path)；
 *  - 排除区（plan.mustNot）→ 组标签「排除」，删除走 removeAtZonePath("mustNot", path)（§3.10 统一标签）；
 *  - 优先区（plan.should）→ 组标签「优先」，按 index 单条移除；
 *  - 排序 chip 独立（setSort）；「清除全部」调用 clearConditions 同时清 plan 与兼容扁平筛选；
 *  - 无 plan（纯手动条件旧链路）退回扁平 query 渲染，仍可逐项删除。 */
import { useShallow } from "zustand/react/shallow";
import { useSuperSearchStore } from "@/stores/superSearchStore";
import { useTagStore } from "@/stores/tagStore";
import { useNumericDomainStore } from "@/stores/numericDomainStore";
import type { MetadataFilter } from "@/types/asset";
import type { LeafCond } from "@/types/queryExpr";
import type { ResolvedTag } from "@/types/superSearch";
import type { TagNode } from "@/types/tag";
import {
  flattenExprForDisplay,
  type ExprChipModel,
} from "@/utils/queryExprUtils";

const LABELS: Record<string, string> = {
  file_ext: "格式", mime_type: "MIME", width: "宽", height: "高",
  resolution: "像素总量", aspect_ratio: "宽高比", file_size: "文件大小",
  duration_ms: "视频时长", taken_at: "拍摄时间", created_at: "入库时间",
  modified_at: "修改时间", camera: "相机", lens: "镜头", iso: "ISO",
  aperture: "光圈", shutter: "快门", focal: "焦距", video_codec: "视频编码",
  audio_codec: "音频编码", folder: "文件夹",
  // U-3：色板关系表（前三色 chip 可读展示）
  palette_top3: "前三色", palette_dominant: "主色", palette_any: "任一色",
};

/** W3：分面显示名优先读 tagStore.facets（自建分面自动显示中文名）；
 *  FACET_NAMES 只是系统分面在 store 未加载时的兜底。 */
const FACET_NAMES: Record<string, string> = {
  subject: "主体对象", scene: "场景/地点", purpose: "用途",
  color: "色彩", composition: "构图/视角", lighting: "光线/时间", people: "人物属性",
  technical: "可用性/技术特征", custom: "自定义",
};

const OP_TEXT: Record<string, string> = { gt: ">", gte: "≥", lt: "<", lte: "≤", eq: "=", contains: "含", between: "", in: "∈" };

function fmtValue(v: string | number) { return String(v); }

function flattenTagNames(nodes: TagNode[], out: ResolvedTag[] = []): ResolvedTag[] {
  for (const node of nodes) {
    out.push({
      tagId: node.tag.id,
      text: node.tag.name,
      facetKey: node.tag.facetKey,
      path: node.tag.path,
    });
    flattenTagNames(node.children, out);
  }
  return out;
}

function metaLabel(f: MetadataFilter): string {
  const name = LABELS[f.key] ?? f.key;
  // U-3：palette_top3 eq + min → 「前三色含 红（占 ≥50%）」
  if (f.key === "palette_top3" && f.op === "eq" && typeof f.value === "string" && typeof f.min === "number" && f.min > 0) {
    return `${name}含 ${f.value}（占 ≥${Math.round(f.min * 100)}%）`;
  }
  if (f.op === "between") return `${name} ${fmtValue(f.min ?? "")}–${fmtValue(f.max ?? "")}`;
  if (f.op === "in") return `${name} ∈ ${(f.values ?? []).map(fmtValue).join("|")}`;
  if (f.op === "eq" && f.value !== undefined) return `${name} = ${fmtValue(f.value)}`;
  return `${name} ${OP_TEXT[f.op] ?? f.op} ${f.value !== undefined ? fmtValue(f.value) : ""}`;
}

/** U-5：加分项（should）叶子 → chip 可读文案（tag 名称经 resolvedTags 反查）。 */
function bonusLabel(cond: LeafCond, tagName: Map<number, string>): string {
  const names = (ids: number[]) => ids.map((id) => tagName.get(id) ?? `标签 #${id}`).join("、");
  switch (cond.type) {
    case "search":
      return `关键词：${cond.value}`;
    case "assetType":
      return `类型：${cond.value === "image" ? "图片" : cond.value === "video" ? "视频" : "全部"}`;
    case "untagged":
      return "未打标";
    case "facetHasAny":
      return `「${cond.facetKey}」有任意标签`;
    case "facetMissing":
      return `「${cond.facetKey}」没有标签`;
    case "tag":
      return `标签：${names(cond.tagIds) || (cond.termQuery?.trim() ? `词：${cond.termQuery.trim()}` : "未选择")}`;
    case "excludeTag":
      return `排除：${names(cond.tagIds) || cond.facetKey}`;
    case "metadata":
      return metaLabel(cond.filter);
    case "facetNumber": {
      // V24（Phase 7-8）：数值分面 chip —— 显示名从 NumericDomain 反查（无则退 key）
      const d = useNumericDomainStore.getState().domains.find((x) => x.key === `facet:${cond.facetKey}`);
      const label = d?.label ?? cond.facetKey;
      const opText: Record<string, string> = { eq: "=", gt: ">", gte: "≥", lt: "<", lte: "≤" };
      return cond.op === "between"
        ? `${label} ${cond.value}~${cond.maxValue ?? ""}`
        : `${label} ${opText[cond.op] ?? cond.op} ${cond.value}`;
    }
  }
}

export default function FilterChips() {
  const { query, expr, plan, resolvedTags, removeAtZonePath, removePlanShould, setQuery, setSort, clearConditions } = useSuperSearchStore(
    useShallow((s) => ({
      query: s.query,
      expr: s.expr,
      plan: s.plan,
      resolvedTags: s.resolvedTags,
      removeAtZonePath: s.removeAtZonePath,
      removePlanShould: s.removePlanShould,
      setQuery: s.setQuery,
      setSort: s.setSort,
      clearConditions: s.clearConditions,
    })),
  );
  const { tagTree, tagFacets } = useTagStore(
    useShallow((s) => ({ tagTree: s.tree, tagFacets: s.facets })),
  );
  // 手动条件不经过 AI，因此不会写入 resolvedTags；把实时标签树作为名称事实源，
  // 再用 AI 返回的 resolvedTags 覆盖同 ID 的显示文本（兼容新建/尚未刷新到树的标签）。
  const namePool = (() => {
    const byId = new Map<number, ResolvedTag>();
    for (const tag of flattenTagNames(tagTree)) byId.set(tag.tagId, tag);
    for (const tag of resolvedTags) byId.set(tag.tagId, tag);
    return Array.from(byId.values());
  })();
  const facetNameByKey = new Map(tagFacets.map((facet) => [facet.key, facet.displayName]));

  type Chip = { key: string; label: string; group?: string; onRemove: () => void };
  const chips: Chip[] = [];

  if (expr) {
    // §3.10：必须区组标签统一「必须」；删除带 zone（§3.7 不变式 6）
    const models: ExprChipModel[] = flattenExprForDisplay(expr, namePool);
    for (const m of models) {
      chips.push({ key: `filter:${m.key}`, label: m.label, group: "必须", onRemove: () => removeAtZonePath("filter", m.path) });
    }
  } else if (!plan) {
    // 纯手动条件链路（无 expr）：扁平 query 渲染，仍逐项删除
    if (query.search) {
      chips.push({ key: "search", label: `关键词：${query.search}`, onRemove: () => setQuery({ search: "" }) });
    }
    if (query.assetType !== "all") {
      chips.push({
        key: "type", label: `类型：${query.assetType === "image" ? "图片" : "视频"}`,
        onRemove: () => setQuery({ assetType: "all" }),
      });
    }
    if (query.untaggedOnly) {
      chips.push({ key: "untagged", label: "未打标", onRemove: () => setQuery({ untaggedOnly: false }) });
    }
    const nameById = new Map<number, { facetKey: string; text: string }>();
    for (const rt of namePool) nameById.set(rt.tagId, { facetKey: rt.facetKey, text: rt.text });
    for (const f of query.facetFilters) {
      for (const tid of f.tagIds) {
        const info = nameById.get(tid);
        const fname = facetNameByKey.get(f.facetKey) ?? FACET_NAMES[f.facetKey] ?? f.facetKey;
        chips.push({
          key: `facet:${f.facetKey}:${tid}`,
          label: info ? `${fname}：${info.text}` : `${fname} · 标签#${tid}`,
          onRemove: () =>
            setQuery({
              facetFilters: query.facetFilters
                .map((g) => (g.facetKey === f.facetKey ? { ...g, tagIds: g.tagIds.filter((x) => x !== tid) } : g))
                .filter((g) => g.tagIds.length > 0 || g.facetKey !== f.facetKey),
            }),
        });
      }
    }
    for (const tid of query.excludeTagIds) {
      const info = nameById.get(tid);
      chips.push({
        key: `exclude:${tid}`,
        label: info ? `排除：${info.text}` : `排除：标签#${tid}`,
        onRemove: () => setQuery({ excludeTagIds: query.excludeTagIds.filter((x) => x !== tid) }),
      });
    }
    for (const m of query.metadataFilters) {
      chips.push({
        key: `meta:${m.key}:${m.op}:${m.value ?? m.min ?? ""}:${m.max ?? ""}`,
        label: metaLabel(m),
        onRemove: () => setQuery({ metadataFilters: query.metadataFilters.filter((x) => x !== m) }),
      });
    }
  }

  // 排除区（plan.mustNot）：§3.10 组标签统一「排除」；删除带 zone（§3.7 不变式 6）
  if (plan?.mustNot) {
    const models: ExprChipModel[] = flattenExprForDisplay(plan.mustNot, namePool);
    for (const m of models) {
      chips.push({ key: `exclude:${m.key}`, label: m.label, group: "排除", onRemove: () => removeAtZonePath("mustNot", m.path) });
    }
  }

  // U-5/§3.10：加分项（should）作为独立「优先」组 chips（按索引单条移除）
  if (plan && plan.should.length > 0) {
    const tagName = new Map(namePool.map((rt) => [rt.tagId, rt.text]));
    plan.should.forEach((sc, i) => {
      chips.push({
        key: `bonus:${i}:${JSON.stringify(sc.cond)}:${sc.weight}`,
        label: bonusLabel(sc.cond, tagName),
        group: "优先",
        onRemove: () => removePlanShould(i),
      });
    });
  }

  // 排序 chip 不属于 expr：独立 setSort
  if (query.sortBy !== "created_at" || query.sortDir !== "desc") {
    chips.push({
      key: "sort", label: `排序：${query.sortBy} ${query.sortDir}`,
      onRemove: () => setSort("created_at", "desc"),
    });
  }

  if (chips.length === 0) return null;

  return (
    <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto py-1" aria-label="筛选条件">
      {chips.map((chip) => (
        <span
          key={chip.key}
          className="inline-flex h-7 shrink-0 items-center gap-1 rounded-full border border-[var(--color-border)] bg-transparent pl-2.5 pr-1 text-xs text-[var(--color-text)]"
        >
          {chip.group && (
            <span className="text-[10px] text-[var(--color-text-tertiary)]">{chip.group}</span>
          )}
          {chip.label}
          <button
            type="button"
            onClick={chip.onRemove}
            className="flex size-5 items-center justify-center rounded-full text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]"
            aria-label={`取消 ${chip.label}`}
            title={`取消 ${chip.label}`}
          >
            ×
          </button>
        </span>
      ))}
      <button
        type="button"
        onClick={() => void clearConditions()}
        className="shrink-0 rounded-full px-2 py-1 text-[11px] text-[var(--color-text-secondary)] hover:text-[var(--color-text)]"
      >
        清除全部
      </button>
    </div>
  );
}
