/** W4 分面管理面板（整体重写）：两组列表 + 弹窗化编辑/新建/删除。
 *  - 两组列表：「AI 自动打标分类」/「手工填写分类」+ 底部折叠「已停用的分类」（Q2：系统分面不可删，停用的折叠只给恢复）
 *  - 拖拽手柄跨组拖动 = 改 input_mode；组内拖动 = reorder
 *  - 列表行只显示 4 项：名称 / key（小字）/ 规则摘要 / 操作按钮；详情进弹窗
 *  - 编辑弹窗 6 字段一个保存通道（update_tag_facet 单事务；替代旧的「基本规则即时写 + AI 行为进草稿」双通道）
 *  - 新建弹窗 2 个必填（名称 + 标签描述）；key 自动 slugify，CJK 生成空串时明确提示
 *  - 删除确认弹窗：精确影响数字 + 输入分类名确认 + 三按钮（取消 / 停用替代 / 确认删除）
 *  - 「画面摘要（一句话描述）」：属于 AI 自动打标组，但不参与精确筛选；
 *    与分面条目同形态展示，点开可查看已有摘要并编辑提示词。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import TagManageDialog from "@/components/dialogs/TagManageDialog";
import {
  convertFacetKind,
  deactivateTagFacet,
  deleteTagFacet,
  getTagFacetImpact,
  listAllTagFacets,
  reorderTagFacets,
  restoreTagFacet,
  saveTagFacet,
  type ConversionReport,
  type FacetDeleteReport,
} from "@/api/tags";
import type { TagFacet, TagFacetImpact } from "@/types/tag";

const APP_TO_OPTIONS: { value: "all" | "image" | "video"; label: string }[] = [
  { value: "all", label: "全部素材" },
  { value: "image", label: "只图片" },
  { value: "video", label: "只视频" },
];

function parseOptionalFiniteNumber(raw: string, label: string): { value: number | null; error?: string } {
  if (raw.trim() === "") return { value: null };
  const value = Number(raw);
  if (!Number.isFinite(value)) return { value: null, error: `${label}必须是有限数字` };
  return { value };
}

type NumberFacetSettings = { min: number | null; max: number | null; step: number; decimals: number };
type NumberFacetSettingsResult = NumberFacetSettings | { error: string };

function parseNumberFacetSettings(minRaw: string, maxRaw: string, stepRaw: string, decimalsRaw: string): NumberFacetSettingsResult {
  const min = parseOptionalFiniteNumber(minRaw, "数值下限");
  const max = parseOptionalFiniteNumber(maxRaw, "数值上限");
  if (min.error) return { error: min.error };
  if (max.error) return { error: max.error };
  if (min.value != null && max.value != null && min.value > max.value) return { error: "数值下限不能大于上限" };
  const step = parseOptionalFiniteNumber(stepRaw, "数值步进");
  if (step.error) return { error: step.error };
  if (step.value == null || step.value <= 0) return { error: "数值步进必须大于 0" };
  const decimals = Number(decimalsRaw);
  if (!Number.isSafeInteger(decimals) || decimals < 0 || decimals > 10) return { error: "小数位数必须是 0 到 10 的整数" };
  return { min: min.value, max: max.value, step: step.value, decimals };
}

const TAG_ROW_CLASS =
  "flex min-h-12 w-full items-center gap-3 px-4 py-2.5 transition-colors hover:bg-[var(--color-surface)]";

function slugify(s: string): string {
  return s
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/^[0-9]+/, "")
    .replace(/^_+|_+$/g, "")
    .slice(0, 64);
}

/** P1-1：中文（或纯符号）名称 slugify 出空串时给一个合法随机兜底 key。
 *  后端 key 约束：小写 snake_case 2–64 位，仅字母/数字/下划线，字母开头。 */
function genFacetFallbackKey(): string {
  const rand = Math.random().toString(36).slice(2, 8) || "1a2b3c";
  return `facet_${rand}`;
}

function ruleSummary(f: TagFacet): string {
  // V24（Phase 7-7）：数值分面摘要 —— 类型优先于选数规则
  if (f.facetKind === "number") {
    const range = f.numMin != null || f.numMax != null ? ` ${f.numMin ?? "-∞"}–${f.numMax ?? "∞"}` : "";
    const unit = f.numUnit ? ` · 单位 ${f.numUnit}` : "";
    return `数值型 ·${range}${unit}`;
  }
  const mode = f.selectionMode === "single" ? "单选" : f.maxItems ? `可多选 ≤${f.maxItems}` : "可多选不限";
  const applies = APP_TO_OPTIONS.find((o) => o.value === f.appliesTo)?.label ?? "全部";
  return f.appliesTo === "all" ? mode : `${mode} · ${applies}`;
}

export default function FacetManagePanel() {
  const [facets, setFacets] = useState<TagFacet[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showInactive, setShowInactive] = useState(false);
  /** W4 弹窗状态：编辑 / 新建（默认落哪组）/ 删除 / 分类词条 */
  const [editing, setEditing] = useState<TagFacet | null>(null);
  const [creatingGroup, setCreatingGroup] = useState<"ai" | "manual" | null>(null);
  const [deleting, setDeleting] = useState<TagFacet | null>(null);
  const [deleteImpact, setDeleteImpact] = useState<TagFacetImpact | null>(null);
  const [deleteReport, setDeleteReport] = useState<FacetDeleteReport | null>(null);
  const [deleteConfirmName, setDeleteConfirmName] = useState("");
  const [termsFacet, setTermsFacet] = useState<TagFacet | null>(null);
  /** 拖拽中：跨组 = 改 input_mode；组内 = reorder */
  const [dragKey, setDragKey] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setFacets(await listAllTagFacets());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (fn: () => Promise<unknown>, okMsg?: string) => {
    setError(null);
    setNotice(null);
    try {
      await fn();
      await refresh();
      if (okMsg) setNotice(okMsg);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // custom（AI 未知分类的兜底桶，resolve_facet_key 分支③）与 color（V16 起由算法主色
  // 替代，属机器可读属性，色条 + 文件属性面板承担展示）不在分面管理 UI 展示——界面干净，
  // 后端兜底链路不受影响（隐藏 ≠ 删除）。
  const visibleFacets = useMemo(() => facets.filter((f) => f.key !== "custom" && f.key !== "color"), [facets]);
  const active = useMemo(() => visibleFacets.filter((f) => f.status === "active"), [visibleFacets]);
  const aiGroup = useMemo(() => active.filter((f) => f.inputMode === "ai_and_manual"), [active]);
  const manualGroup = useMemo(() => active.filter((f) => f.inputMode === "manual_only"), [active]);
  const inactive = useMemo(() => visibleFacets.filter((f) => f.status !== "active"), [visibleFacets]);

  /** 跨组拖动 = 改 input_mode（与其它分面字段在单事务内保存） */
  const moveToGroup = (facet: TagFacet, group: "ai" | "manual") => {
    if (facet.isSystem && group !== "ai") {
      setNotice("内置 AI 分类固定保留在 AI 自动打标分组");
      return;
    }
    const nextMode = group === "ai" ? "ai_and_manual" : "manual_only";
    if (facet.inputMode === nextMode) return;
    void run(
      () => saveTagFacet({
        key: facet.key,
        displayName: facet.displayName,
        description: facet.description,
        inputMode: nextMode,
        selectionMode: facet.selectionMode,
        maxItems: facet.maxItems,
        appliesTo: facet.appliesTo,
        facetKind: facet.facetKind ?? "tag",
        numMin: facet.numMin ?? null,
        numMax: facet.numMax ?? null,
        numUnit: facet.numUnit ?? "",
        numDecimals: facet.numDecimals ?? 0,
        numStep: facet.numStep ?? 1,
      }),
      `「${facet.displayName}」已移到${group === "ai" ? " AI 自动打标" : "手工填写"}分组`,
    );
  };

  /** 组内拖动 = reorder（本组按新序插入，其它组保持不变） */
  const reorderInGroup = async (draggedKey: string, targetKey: string, groupKeys: string[]) => {
    if (draggedKey === targetKey) return;
    const keys = [...groupKeys];
    const from = keys.indexOf(draggedKey);
    const to = keys.indexOf(targetKey);
    if (from < 0 || to < 0) return;
    keys.splice(to, 0, keys.splice(from, 1)[0]);
    // 全量顺序：遍历原 facets，遇到本组第一个成员时替换为整组新序
    const merged: string[] = [];
    let inserted = false;
    for (const f of facets) {
      if (keys.includes(f.key)) {
        if (!inserted) {
          merged.push(...keys);
          inserted = true;
        }
      } else {
        merged.push(f.key);
      }
    }
    await run(() => reorderTagFacets(merged));
  };

  const onDropToGroup = (group: "ai" | "manual", targetKey?: string) => {
    if (!dragKey) return;
    const facet = facets.find((f) => f.key === dragKey);
    setDragKey(null);
    if (!facet || facet.status !== "active") return;
    const groupList = group === "ai" ? aiGroup : manualGroup;
    if (targetKey && groupList.some((f) => f.key === dragKey)) {
      void reorderInGroup(dragKey, targetKey, groupList.map((f) => f.key));
    } else {
      moveToGroup(facet, group);
    }
  };

  /** 打开删除确认：先取精确影响数字 */
  const openDelete = async (facet: TagFacet) => {
    setDeleting(facet);
    setDeleteConfirmName("");
    setDeleteReport(null);
    try {
      setDeleteImpact(await getTagFacetImpact(facet.key));
    } catch {
      setDeleteImpact(null);
    }
  };

  const confirmDelete = async () => {
    if (!deleting) return;
    try {
      const report = await deleteTagFacet(deleting.key);
      setDeleteReport(report);
      setDeleting(null);
      await refresh();
      setNotice(`已删除「${facetName(deleting)}」`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const facetName = (f: TagFacet) => f.displayName;

  const renderRow = (f: TagFacet, group: "ai" | "manual") => (
    <li
      key={f.key}
      draggable
      onDragStart={() => setDragKey(f.key)}
      onDragEnd={() => setDragKey(null)}
      onDragOver={(e) => e.preventDefault()}
      onDrop={() => onDropToGroup(group, f.key)}
      className={clsx(
        TAG_ROW_CLASS,
        dragKey === f.key && "opacity-50",
      )}
    >
      <span className="cursor-grab select-none text-[var(--color-text-tertiary)]" title="拖动可调整分组或顺序" aria-hidden="true">⠿</span>
      <span className="min-w-0 flex-1" title={f.description}>
        <span className="block truncate text-sm text-[var(--color-text)]">{f.displayName}</span>
        <code className="mt-0.5 block truncate text-[11px] text-[var(--color-text-tertiary)]">{f.key}</code>
      </span>
      {f.isSystem && <span className="shrink-0 text-[11px] text-[var(--color-text-secondary)]">系统</span>}
      <span className="shrink-0 text-[11px] text-[var(--color-text-secondary)]">{ruleSummary(f)}</span>
      <span className="flex shrink-0 items-center gap-1">
        <button type="button" onClick={() => setEditing(f)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]">编辑</button>
        <button type="button" onClick={() => setTermsFacet(f)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]">词条</button>
        <button type="button" onClick={() => run(() => deactivateTagFacet(f.key), `已停用「${f.displayName}」；历史素材关联已保留`)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]">停用</button>
        {!f.isSystem && (
          <button type="button" onClick={() => void openDelete(f)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-danger)]">删除</button>
        )}
      </span>
    </li>
  );

  const renderGroup = (title: string, hint: string, group: "ai" | "manual", list: TagFacet[]) => (
    <section
      onDragOver={(e) => e.preventDefault()}
      onDrop={() => onDropToGroup(group)}
      className="border-t border-[var(--color-border)]"
    >
      <div className="flex items-center justify-between gap-6 px-4 py-3">
        <div className="min-w-0">
          <h2 className="text-sm text-[var(--color-text)]">{title}</h2>
          <p className="mt-0.5 text-xs leading-5 text-[var(--color-text-secondary)]">{hint}</p>
        </div>
        <Button onClick={() => setCreatingGroup(group)}>+ 新增分类</Button>
      </div>
      <ul className="divide-y divide-[var(--color-border)] rounded-lg border border-[var(--color-border)]">
        {list.map((f) => renderRow(f, group))}
        {list.length === 0 && (
          <li className="px-4 py-4 text-center text-xs text-[var(--color-text-tertiary)]">
            暂无分类，可使用上方「+ 新增分类」添加
          </li>
        )}
      </ul>
    </section>
  );

  const renderAiGroup = () => (
    <section onDragOver={(e) => e.preventDefault()} onDrop={() => onDropToGroup("ai")}>
      <div className="mb-2 flex items-start justify-between gap-6">
        <div className="min-w-0">
          <h2 className="text-sm text-[var(--color-text)]">AI 自动打标分类</h2>
          <p className="mt-0.5 text-xs leading-5 text-[var(--color-text-secondary)]">内置分类可停用；用户创建的分类可编辑、停用或删除。</p>
        </div>
        <Button onClick={() => setCreatingGroup("ai")}>+ 新增分类</Button>
      </div>
      <ul className="divide-y divide-[var(--color-border)] rounded-lg border border-[var(--color-border)]">
        {aiGroup.map((f) => renderRow(f, "ai"))}
        <li className={TAG_ROW_CLASS}>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm text-[var(--color-text)]">画面摘要（一句话描述）</span>
            <code className="mt-0.5 block text-[11px] text-[var(--color-text-tertiary)]">description</code>
          </span>
          <span className="shrink-0 text-[11px] text-[var(--color-text-secondary)]">系统</span>
          <span className="shrink-0 text-[11px] text-[var(--color-text-secondary)]">固定生成 · 不属于分类筛选</span>
        </li>
        {aiGroup.length === 0 && (
          <li className="px-4 py-4 text-center text-xs text-[var(--color-text-tertiary)]">
            暂无 AI 打标分类，可新增或恢复已停用分类
          </li>
        )}
      </ul>
    </section>
  );

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-[var(--color-text-secondary)]">
        <span>内置 AI 分类固定在本组；用户分类可切换分组。分类说明定义业务含义，英文标识创建后不可修改。</span>
        {error && <span className="text-[var(--color-danger)]">{error}</span>}
        {notice && <span>{notice}</span>}
      </div>

      {loading ? (
        <p className="border-t border-[var(--color-border)] px-4 py-3 text-xs text-[var(--color-text-secondary)]">加载分类…</p>
      ) : (
        <>
          {renderAiGroup()}
          {renderGroup("手工填写分类", "不由 AI 判断，需手动填写；填写后可用于搜索和筛选。", "manual", manualGroup)}

          {/* 已停用分面保留历史关联；自建分类仍可删除。 */}
          {inactive.length > 0 && (
            <section className="border-t border-[var(--color-border)]">
              <button
                type="button"
                onClick={() => setShowInactive((v) => !v)}
                className="flex w-full items-center gap-1 px-4 py-3 text-left text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
              >
                已停用的分类（{inactive.length}）{showInactive ? "▾" : "▸"}
              </button>
              {showInactive && (
                <ul className="divide-y divide-[var(--color-border)] rounded-lg border border-[var(--color-border)]">
                  {inactive.map((f) => (
                    <li key={f.key} className="flex min-h-12 items-center gap-3 px-4 py-2.5 opacity-75 hover:bg-[var(--color-surface)]">
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm text-[var(--color-text-secondary)] line-through">{f.displayName}</span>
                        <code className="mt-0.5 block truncate text-[11px] text-[var(--color-text-tertiary)]">{f.key}</code>
                      </span>
                      <span className="shrink-0 text-[11px] text-[var(--color-text-secondary)]">{ruleSummary(f)}</span>
                      <button type="button" onClick={() => run(() => restoreTagFacet(f.key), `已恢复「${f.displayName}」`)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]">恢复</button>
                      {!f.isSystem && <button type="button" onClick={() => void openDelete(f)} className="rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-danger)]">删除</button>}
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}

        </>
      )}

      {/* W4-2 编辑弹窗（6 字段一个保存通道） */}
      <EditFacetDialog
        facet={editing}
        onClose={() => {
          setEditing(null);
        }}
        onSaved={(msg) => {
          setNotice(msg);
          void refresh();
        }}
        onError={setError}
      />

      {/* W4-3 新建弹窗（2 个必填；默认落点组） */}
      <CreateFacetDialog
        group={creatingGroup}
        onClose={() => setCreatingGroup(null)}
        onCreated={(msg) => { setNotice(msg); setCreatingGroup(null); void refresh(); }}
      />

      {/* W4-4 删除确认弹窗 */}
      <Modal
        open={deleting != null}
        title={deleting ? `删除分类「${deleting.displayName}」` : "删除分类"}
        onClose={() => setDeleting(null)}
        footer={
          <>
            <Button onClick={() => setDeleting(null)}>取消</Button>
            <Button
              variant="danger"
              disabled={!deleting || deleteConfirmName.trim() !== deleting.displayName}
              onClick={() => void confirmDelete()}
            >
              确认删除
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-2 text-sm">
          <p className="text-[var(--color-danger)]">此操作不可恢复。素材文件本身不会被删除。</p>
          {deleteImpact && (
            <ul className="rounded-md bg-[var(--color-surface)] p-2 text-xs text-[var(--color-text-secondary)]">
              <li>将删除 {deleteImpact.tagCount} 个标签</li>
              <li>解除 {deleteImpact.assetCount} 个素材的关联</li>
              <li>清除 {deleteImpact.aiSuggestionItemCount} 条 AI 候选记录、{deleteImpact.tagOpCount} 条操作流水</li>
              {deleteImpact.aliasCount > 0 && <li>删除 {deleteImpact.aliasCount} 条别名</li>}
              {(deleteImpact.numberCount ?? 0) > 0 && <li>删除 {deleteImpact.numberCount} 条数值（级联，不可恢复）</li>}
            </ul>
          )}
          <p className="text-xs text-[var(--color-text-secondary)]">输入分类名称以确认删除。删除分类不会删除素材文件。</p>
          {deleting && (
            <label className="flex flex-col gap-1 text-xs">
              输入分类名「{deleting.displayName}」确认：
              <input className="ui-control px-2 py-1 text-sm" value={deleteConfirmName} onChange={(e) => setDeleteConfirmName(e.target.value)} placeholder={deleting.displayName} />
            </label>
          )}
          {deleteReport && (
            <p className="text-xs text-[var(--color-success)]">
              已删除：{deleteReport.tagsDeleted} 个标签、{deleteReport.unlinked} 条素材关联。
            </p>
          )}
        </div>
      </Modal>

      {/* 分类词条二级编辑器（保留） */}
      <TagManageDialog
        open={termsFacet != null}
        onClose={() => setTermsFacet(null)}
        title={termsFacet ? `分类词条：${termsFacet.displayName}` : "分类词条"}
      />
    </div>
  );
}

/** 编辑弹窗：全部分面字段通过单个事务保存。 */
function EditFacetDialog({ facet, onClose, onSaved, onError }: {
  facet: TagFacet | null;
  onClose: () => void;
  onSaved: (msg: string) => void;
  onError: (msg: string) => void;
}) {
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [inputMode, setInputMode] = useState<"ai_and_manual" | "manual_only">("ai_and_manual");
  const [selectionMode, setSelectionMode] = useState<"single" | "multi">("multi");
  const [maxItems, setMaxItems] = useState("");
  const [appliesTo, setAppliesTo] = useState<"all" | "image" | "video">("all");
  const [saving, setSaving] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  // V24（Phase 7-7）：数值分面配置编辑 + tag→number 转换预览入口
  const [numMin, setNumMin] = useState("");
  const [numMax, setNumMax] = useState("");
  const [numUnit, setNumUnit] = useState("");
  const [numStep, setNumStep] = useState("1");
  const [numDecimals, setNumDecimals] = useState("0");
  const [convertOpen, setConvertOpen] = useState(false);

  useEffect(() => {
    if (facet) {
      setDisplayName(facet.displayName);
      setDescription(facet.description);
      setInputMode(facet.inputMode);
      setSelectionMode(facet.selectionMode);
      setMaxItems(facet.maxItems ? String(facet.maxItems) : "");
      setAppliesTo(facet.appliesTo);
      setShowAdvanced(false);
      setNumMin(facet.numMin != null ? String(facet.numMin) : "");
      setNumMax(facet.numMax != null ? String(facet.numMax) : "");
      setNumUnit(facet.numUnit ?? "");
      setNumStep(facet.numStep ? String(facet.numStep) : "1");
      setNumDecimals(String(facet.numDecimals ?? 0));
      setConvertOpen(false);
    }
  }, [facet]);

  const submit = async () => {
    if (!facet) return;
    if (selectionMode === "multi" && maxItems.trim() !== "") {
      const parsedMax = Number(maxItems);
      if (!Number.isSafeInteger(parsedMax) || parsedMax < 1) {
        onError("多选上限必须是正整数，或留空表示不限");
        return;
      }
    }
    const numberSettings = facet.facetKind === "number"
      ? parseNumberFacetSettings(numMin, numMax, numStep, numDecimals)
      : null;
    if (numberSettings && "error" in numberSettings) {
      onError(numberSettings.error);
      return;
    }
    setSaving(true);
    try {
      await saveTagFacet({
        key: facet.key,
        displayName: displayName.trim(),
        description,
        inputMode,
        selectionMode,
        maxItems: selectionMode === "single" ? 1 : maxItems.trim() ? Number(maxItems) : null,
        appliesTo,
        facetKind: facet.facetKind ?? "tag",
        numMin: numberSettings ? numberSettings.min : null,
        numMax: numberSettings ? numberSettings.max : null,
        numUnit: facet.facetKind === "number" ? numUnit.trim() : "",
        numDecimals: numberSettings ? numberSettings.decimals : 0,
        numStep: numberSettings ? numberSettings.step : 1,
      });
      onSaved(`已保存「${displayName.trim()}」`);
      onClose();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={facet != null}
      title={facet ? `编辑分类：${facet.displayName}` : "编辑分类"}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={saving || !displayName.trim()} onClick={() => void submit()}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      {facet && (
        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 text-xs">
            分类名称
            <input className="ui-control px-2 py-1.5 text-sm" value={displayName} onChange={(e) => setDisplayName(e.target.value)} aria-label="分类名称" />
          </label>
          <label className="flex flex-col gap-1 text-xs">
            给 AI 的分类说明
            <textarea
              className="ui-control min-h-20 px-2 py-1.5 text-sm"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              aria-label="给 AI 的分类说明"
              placeholder="如「人物服装的主色调」"
            />
            <span className="text-[10px] leading-4 text-[var(--color-text-tertiary)]">内容会原样发送给 AI，作为此分类唯一的业务语义说明；已有标签只作参考，数量与类型限制由上方控件决定。</span>
          </label>
          {inputMode === "ai_and_manual" && !description.trim() && (
            <p className="text-xs text-[var(--color-status-warning,--color-text-secondary)]">
              此分类参与 AI 打标，但说明为空。请补充说明，或切换为“只手工填写”；空说明分类会阻止新打标批次启动。
            </p>
          )}
          <fieldset className="flex flex-col gap-1 text-xs">
            <legend className="mb-0.5">打标方式</legend>
            <label className="flex items-center gap-1.5"><input type="radio" disabled={facet.isSystem} checked={inputMode === "ai_and_manual"} onChange={() => setInputMode("ai_and_manual")} />AI 自动打标（也可手工填写）</label>
            <label className="flex items-center gap-1.5"><input type="radio" disabled={facet.isSystem} checked={inputMode === "manual_only"} onChange={() => setInputMode("manual_only")} />只手工填写</label>
            {facet.isSystem && <span className="text-[10px] leading-4 text-[var(--color-text-tertiary)]">内置分类固定参与 AI 打标；分类说明及显式数量规则仍可编辑。</span>}
          </fieldset>
          {facet.facetKind === "number" ? (
            // V24：数值分面 —— 规则区替换为数值配置（不可改回标签型，§6.6 规则 9）
            <fieldset className="flex flex-col gap-1 text-xs">
              <legend className="mb-0.5">数值设置</legend>
              <div className="flex flex-wrap items-center gap-2">
                <label className="flex items-center gap-1">下限
                  <input className="ui-control w-16 px-1 py-0.5" type="number" value={numMin} onChange={(e) => setNumMin(e.target.value)} placeholder="不限" aria-label="数值下限" />
                </label>
                <label className="flex items-center gap-1">上限
                  <input className="ui-control w-16 px-1 py-0.5" type="number" value={numMax} onChange={(e) => setNumMax(e.target.value)} placeholder="不限" aria-label="数值上限" />
                </label>
                <label className="flex items-center gap-1">单位
                  <input className="ui-control w-14 px-1 py-0.5" value={numUnit} onChange={(e) => setNumUnit(e.target.value)} aria-label="数值单位" />
                </label>
                <label className="flex items-center gap-1">步进
                  <input className="ui-control w-14 px-1 py-0.5" type="number" step="any" min="0" value={numStep} onChange={(e) => setNumStep(e.target.value)} aria-label="数值步进" />
                </label>
                <label className="flex items-center gap-1">小数位
                  <input className="ui-control w-14 px-1 py-0.5" type="number" min="0" max="10" step="1" value={numDecimals} onChange={(e) => setNumDecimals(e.target.value)} aria-label="数值小数位" />
                </label>
              </div>
              <span className="text-[10px] text-[var(--color-text-tertiary)]">数值类型创建后不可改回标签类型；已确认的数值不受影响</span>
            </fieldset>
          ) : (
            <fieldset className="flex flex-col gap-1 text-xs">
              <legend className="mb-0.5">可选数量</legend>
              <label className="flex items-center gap-1.5"><input type="radio" checked={selectionMode === "single"} onChange={() => setSelectionMode("single")} />单选</label>
              <label className="flex items-center gap-1.5">
                <input type="radio" checked={selectionMode === "multi"} onChange={() => setSelectionMode("multi")} />多选，最多
                <input className="ui-control w-16 px-1 py-0.5" type="number" min={1} disabled={selectionMode === "single"} value={maxItems} onChange={(e) => setMaxItems(e.target.value)} placeholder="不限" aria-label="多选上限" />
                个（留空表示不限）
              </label>
            </fieldset>
          )}
          <div>
            <button type="button" onClick={() => setShowAdvanced((v) => !v)} className="text-[11px] text-[var(--color-text-secondary)] hover:text-[var(--color-text)]">
              ▸ 高级{showAdvanced ? "（收起）" : ""}
            </button>
            {showAdvanced && (
              <div className="mt-2 flex flex-col gap-2">
                <label className="flex items-center gap-2 text-xs">
                  适用于
                  <select className="ui-control rounded px-1 py-0.5 text-xs" value={appliesTo} onChange={(e) => setAppliesTo(e.target.value as typeof appliesTo)} aria-label="适用于">
                    {APP_TO_OPTIONS.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
                  </select>
                </label>
                <label className="flex items-center gap-2 text-xs text-[var(--color-text-tertiary)]">
                  英文标识（不可修改）：<code>{facet.key}</code>
                </label>
                {/* V24（Phase 7-7）：tag → number 转换（先 dry-run 预览，不自动裁决） */}
                <div className="flex flex-col gap-1">
                  <button
                    type="button"
                    className="self-start rounded border border-[var(--color-border)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)]"
                    onClick={() => setConvertOpen(true)}
                  >
                    转换为数值型…
                  </button>
                  <span className="text-[10px] text-[var(--color-text-tertiary)]">
                    从标签名称中识别数值，例如「5人」→ 5。此操作不可撤销，请先确认预览结果。
                  </span>
                </div>
              </div>
            )}
          </div>
          <ConvertPreviewDialog facetKey={facet.key} open={convertOpen} onClose={() => setConvertOpen(false)} onDone={(msg) => { onSaved(msg); onClose(); }} onError={onError} />
        </div>
      )}
    </Modal>
  );
}

/** V24（Phase 7-7）：tag → number 转换预览对话框 —— dry-run 报告全部展示；
 *  冲突/歧义未清零时后端拒绝执行（不自动裁决，铁律 5），执行按钮只在这两类为空时可用。 */
function ConvertPreviewDialog({ facetKey, open, onClose, onDone, onError }: {
  facetKey: string;
  open: boolean;
  onClose: () => void;
  onDone: (msg: string) => void;
  onError: (msg: string) => void;
}) {
  const [report, setReport] = useState<ConversionReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [executing, setExecuting] = useState(false);

  useEffect(() => {
    if (!open) {
      setReport(null);
      return;
    }
    let alive = true;
    setLoading(true);
    convertFacetKind(facetKey, true)
      .then((r) => { if (alive) setReport(r); })
      .catch((e) => { if (alive) onError(e instanceof Error ? e.message : String(e)); onClose(); })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, facetKey]);

  const execute = async () => {
    setExecuting(true);
    try {
      await convertFacetKind(facetKey, false);
      onDone("已转换为数值型");
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setExecuting(false);
    }
  };

  const blocked = (report?.conflicts.length ?? 0) > 0 || (report?.ambiguous.length ?? 0) > 0;

  return (
    <Modal
      open={open}
      title="转换为数值型 · 预览报告"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={loading || executing || blocked} onClick={() => void execute()}>
            {executing ? "转换中…" : blocked ? "存在冲突/歧义，先处理" : "确认转换"}
          </Button>
        </>
      }
    >
      {loading || !report ? (
        <p className="text-xs text-[var(--color-text-tertiary)]">正在识别标签中的数值…</p>
      ) : (
        <div className="flex max-h-96 flex-col gap-2 overflow-y-auto text-xs">
          <p className="text-[var(--color-text-secondary)]">
            可解析 {report.parsed.length} 条 · 歧义 {report.ambiguous.length} 条 · 不可解析 {report.unparseable.length} 条 · 冲突 {report.conflicts.length} 处
          </p>
          {report.parsed.length > 0 && (
            <div>
              <p className="font-medium">将写入的数值</p>
              <ul className="ml-4 list-disc">{report.parsed.map((p) => <li key={p.tagId}>「{p.name}」→ {p.value}</li>)}</ul>
            </div>
          )}
          {report.ambiguous.length > 0 && (
            <div className="text-[var(--color-status-warning,--color-text)]">
              <p className="font-medium">含义不明确的标签（请先重命名或手动填写数值）</p>
              <ul className="ml-4 list-disc">{report.ambiguous.map((a) => <li key={a.tagId}>「{a.name}」：{a.reason}</li>)}</ul>
            </div>
          )}
          {report.unparseable.length > 0 && (
            <div>
              <p className="font-medium">无法识别数值的标签（转换后将停用，不会删除）</p>
              <ul className="ml-4 list-disc">{report.unparseable.map((u) => <li key={u.tagId}>「{u.name}」（{u.assetCount} 张素材）</li>)}</ul>
            </div>
          )}
          {report.conflicts.length > 0 && (
            <div className="text-[var(--color-danger)]">
              <p className="font-medium">同一素材存在多个数值（需先在标签管理页合并或重命名）</p>
              <ul className="ml-4 list-disc">
                {report.conflicts.slice(0, 20).map((c) => (
                  <li key={c.assetId}>素材 #{c.assetId}：{c.candidates.map(([, v, rs]) => `${v}（${rs}）`).join(" / ")}</li>
                ))}
              </ul>
            </div>
          )}
          <p className="text-[10px] text-[var(--color-text-tertiary)]">
            层级丢失 {report.hierarchyLoss} 条 · 别名丢弃 {report.aliasLoss} 条 · 待确认的 AI 建议将拒绝 {report.pendingRejected} 条。原标签会保留为已停用状态，不会直接删除数据。
          </p>
        </div>
      )}
    </Modal>
  );
}

/** W4-3 新建弹窗：2 个必填（名称 + 标签描述）；key 自动生成。
 *  P1-1：纯中文名 slugify 出空串不再硬报错 —— 自动补一个合法随机标识（facet_xxx），
 *  用户仍可改；只有显式清空了自己输入过的标识才报错。 */
function CreateFacetDialog({ group, onClose, onCreated }: {
  group: "ai" | "manual" | null;
  onClose: () => void;
  onCreated: (msg: string) => void;
}) {
  const [displayName, setDisplayName] = useState("");
  const [key, setKey] = useState("");
  const [keyTouched, setKeyTouched] = useState(false);
  // 每次打开弹窗生成一次兜底 key，避免输入中文名时每敲一字都变
  const [fallbackKey, setFallbackKey] = useState(() => genFacetFallbackKey());
  const [description, setDescription] = useState("");
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // V24（Phase 7-7）：类型第一项 —— 标签 / 数值；数值带值域配置（§6.7：类型在创建时定）
  const [facetKind, setFacetKindSel] = useState<"tag" | "number">("tag");
  const [numMin, setNumMin] = useState("");
  const [numMax, setNumMax] = useState("");
  const [numUnit, setNumUnit] = useState("");
  const [numStep, setNumStep] = useState("1");
  const [numDecimals, setNumDecimals] = useState("0");

  useEffect(() => {
    if (group != null) {
      setDisplayName("");
      setKey("");
      setKeyTouched(false);
      setDescription("");
      setErr(null);
      setFallbackKey(genFacetFallbackKey());
      setFacetKindSel("tag");
      setNumMin("");
      setNumMax("");
      setNumUnit("");
      setNumStep("1");
      setNumDecimals("0");
    }
  }, [group]);

  // 用户没碰英文标识时：名称能 slugify 就用 slugify 结果，空（纯中文/符号）用随机兜底
  const effectiveKey = keyTouched ? key : slugify(displayName) || fallbackKey;
  // 只有「显式输入过又被清空」才拦（自动生成路径永远非空，不再硬报错）
  const keyEmpty = keyTouched && effectiveKey.trim() === "";
  const autoGenerated = !keyTouched;

  const submit = async () => {
    setErr(null);
    if (!displayName.trim()) return setErr("请填写分类名称");
    if (group === "ai" && !description.trim()) return setErr("请填写给 AI 的分类说明，或选择手工填写分类");
    if (keyEmpty) return setErr("英文标识不能为空");
    const numberSettings = facetKind === "number"
      ? parseNumberFacetSettings(numMin, numMax, numStep, numDecimals)
      : null;
    if (numberSettings && "error" in numberSettings) return setErr(numberSettings.error);
    setSaving(true);
    try {
      await saveTagFacet({
        key: effectiveKey,
        displayName: displayName.trim(),
        description,
        inputMode: group === "manual" ? "manual_only" : "ai_and_manual",
        selectionMode: "multi",
        maxItems: null,
        appliesTo: "all",
        facetKind,
        numMin: numberSettings ? numberSettings.min : null,
        numMax: numberSettings ? numberSettings.max : null,
        numUnit: facetKind === "number" ? numUnit.trim() : "",
        numDecimals: numberSettings ? numberSettings.decimals : 0,
        numStep: numberSettings ? numberSettings.step : 1,
      });
      onCreated(`已创建「${displayName.trim()}」`);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={group != null}
      title={`新增分类${group === "ai" ? "（AI 自动打标）" : group === "manual" ? "（手工填写）" : ""}`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={saving} onClick={() => void submit()}>{saving ? "创建中…" : "创建"}</Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {/* V24（Phase 7-7）：类型第一项（Cloudinary/Notion 惯例：类型在创建时定） */}
        <fieldset className="flex flex-col gap-1 text-xs">
          <legend className="mb-0.5">类型</legend>
          <label className="flex items-center gap-1.5"><input type="radio" checked={facetKind === "tag"} onChange={() => setFacetKindSel("tag")} />标签（从词表选词）</label>
          <label className="flex items-center gap-1.5">
            <input type="radio" checked={facetKind === "number"} onChange={() => setFacetKindSel("number")} />数值（如人数）
          </label>
          {facetKind === "number" && (
            <div className="mt-1 flex flex-wrap items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
              <label className="flex items-center gap-1 text-xs">下限
                <input className="ui-control w-16 px-1 py-0.5" type="number" value={numMin} onChange={(e) => setNumMin(e.target.value)} placeholder="不限" aria-label="数值下限" />
              </label>
              <label className="flex items-center gap-1 text-xs">上限
                <input className="ui-control w-16 px-1 py-0.5" type="number" value={numMax} onChange={(e) => setNumMax(e.target.value)} placeholder="不限" aria-label="数值上限" />
              </label>
              <label className="flex items-center gap-1 text-xs">单位
                <input className="ui-control w-14 px-1 py-0.5" value={numUnit} onChange={(e) => setNumUnit(e.target.value)} placeholder="人" aria-label="数值单位" />
              </label>
              <label className="flex items-center gap-1 text-xs">步进
                <input className="ui-control w-14 px-1 py-0.5" type="number" step="any" min="0" value={numStep} onChange={(e) => setNumStep(e.target.value)} aria-label="数值步进" />
              </label>
              <label className="flex items-center gap-1 text-xs">小数位
                <input className="ui-control w-14 px-1 py-0.5" type="number" min="0" max="10" step="1" value={numDecimals} onChange={(e) => setNumDecimals(e.target.value)} aria-label="数值小数位" />
              </label>
              <span className="text-[10px] text-[var(--color-text-tertiary)]">AI 和手动填写的数值都会限制在此范围内</span>
            </div>
          )}
        </fieldset>
        <label className="flex flex-col gap-1 text-xs">
          分类名称（必填）
          <input className="ui-control px-2 py-1.5 text-sm" placeholder={facetKind === "number" ? "如「人数」" : "如「人物服装颜色」"} value={displayName} onChange={(e) => setDisplayName(e.target.value)} aria-label="分类名称" />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          给 AI 的分类说明{group === "ai" ? "（必填）" : "（手工分类可留空）"}
          <textarea className="ui-control min-h-20 px-2 py-1.5 text-sm" placeholder={facetKind === "number" ? "描述 AI 应从画面识别的数值含义" : "描述此分类收录什么内容，例如允许的标签范围或组织方式"} value={description} onChange={(e) => setDescription(e.target.value)} aria-label="给 AI 的分类说明" />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          英文标识
          <input
            className="ui-control px-2 py-1.5 text-sm"
            placeholder={keyEmpty ? "请输入英文标识，如 clothing_color" : ""}
            value={effectiveKey}
            onChange={(e) => { setKey(slugify(e.target.value)); setKeyTouched(true); }}
            aria-label="英文标识"
          />
          {autoGenerated ? (
            <span className="text-[10px] text-[var(--color-text-tertiary)]">
              {slugify(displayName) ? "根据名称自动生成，可修改" : "当前名称无法生成英文标识，已自动生成，可修改"} · 创建后不可修改
            </span>
          ) : (
            <span className="text-[10px] text-[var(--color-text-tertiary)]">⚠ 创建后不可修改</span>
          )}
        </label>
        {err && <p className="text-xs text-[var(--color-danger)]">{err}</p>}
      </div>
    </Modal>
  );
}
