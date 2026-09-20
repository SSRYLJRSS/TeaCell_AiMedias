/** 打标工作台（PRD v2.11）：大图（底部居中悬浮导航条）+ EXIF 行 + 分面标签面板
 *  导航：←/→ 键、悬浮条按钮、跳转到第 N 张
 *  指导书阶段 6 §9.2/§9.4：分面来自稳定 WorkbenchFacet[]（tag_facets 唯一事实源），
 *  不再读 tagCategories；系统分面永远显示；标签 key 为稳定 facetKey。
 *  按钮规范：主 CTA 黑色实心（确认写入），其余幽灵文字按钮。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import Button from "@/components/common/Button";
import TagChip from "@/components/library/TagChip";
import FacetTagInput from "@/components/ai/FacetTagInput";
import { getAsset } from "@/api/assets";
import { getThumbnailUrl, toFileUrl } from "@/api/thumbnail";
import { displayBasename } from "@/utils/pathDisplay";
import type { AiSuggestion, CategorizedTags } from "@/types/ai";
import type { Asset } from "@/types/asset";
import type { WorkbenchFacet } from "@/types/tag";

type ImgStage = "hd" | "ph" | "orig";

interface WorkbenchProps {
  suggestion: AiSuggestion;
  /** W3-4：稳定分面两组 ——「AI 识别」在前、「需要你填」在后（buildWorkbenchFacets 产物） */
  aiGroup: WorkbenchFacet[];
  manualGroup: WorkbenchFacet[];
  tags: CategorizedTags;
  onTagsChange: (t: CategorizedTags) => void;
  /** 一句话描述（审核编辑区顶部；AI 目标 12–30 字符，手工最多 30） */
  description: string;
  onDescriptionChange: (v: string) => void;
  index: number; // 0 基
  total: number;
  filmstrip?: ReactNode;
  onGoto: (i: number) => void;
  onConfirm: () => Promise<void>;
  onReject: () => Promise<void>;
  onRestore: () => Promise<void>;
  /** V24（Phase 7-4）：数值建议项（itemKind='number'）—— 采纳写 asset_facet_numbers；
   *  歧义项（numValue=null）提示人工确认，系统绝不自动取值（不变量 11） */
  numberItems?: WorkbenchNumberItem[];
  onDecideNumberItem?: (itemId: number, decision: "accepted" | "rejected") => Promise<void>;
}

export interface WorkbenchNumberItem {
  id: number;
  facetKey: string;
  displayName: string;
  numValue: number | null;
  decision: string;
  decisionReason: string | null;
}

function exifLine(a: Asset | null): string {
  if (!a) return "";
  const parts: string[] = [];
  if (a.camera) parts.push(a.camera);
  if (a.lens) parts.push(a.lens);
  if (a.aperture != null) parts.push(`f/${a.aperture}`);
  if (a.shutter) parts.push(`${a.shutter}s`);
  if (a.iso != null) parts.push(`ISO${a.iso}`);
  if (a.focal != null) parts.push(`${a.focal}mm`);
  if (a.takenAt != null) parts.push(new Date(a.takenAt).toLocaleString("zh-CN", { hour12: false }));
  return parts.join(" · ");
}

export default function Workbench({
  suggestion: s,
  aiGroup,
  manualGroup,
  tags,
  onTagsChange,
  description,
  onDescriptionChange,
  index,
  total,
  filmstrip,
  onGoto,
  onConfirm,
  onReject,
  onRestore,
  numberItems,
  onDecideNumberItem,
}: WorkbenchProps) {
  const [stage, setStage] = useState<ImgStage>("hd");
  const [imgUrl, setImgUrl] = useState<string | null>(null);
  const [asset, setAsset] = useState<Asset | null>(null);
  const [editing, setEditing] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [jump, setJump] = useState("");

  const isRejected = s.status === "rejected";
  const readOnly = s.status === "confirmed" || isRejected;

  // 大图：高清优先；img onError 逐层降级
  useEffect(() => {
    let cancelled = false;
    setStage("hd");
    setImgUrl(null);
    getThumbnailUrl(s.assetId, "hd", 1280)
      .then((u) => !cancelled && setImgUrl(u))
      .catch(() => !cancelled && setStage("orig"));
    return () => {
      cancelled = true;
    };
  }, [s.assetId]);

  const onImgError = () => {
    if (stage === "hd") {
      setStage("ph");
      getThumbnailUrl(s.assetId, "placeholder")
        .then(setImgUrl)
        .catch(() => setStage("orig"));
    } else {
      setStage("orig");
      setImgUrl(toFileUrl(s.assetPath));
    }
  };
  useEffect(() => {
    if (stage === "orig") setImgUrl(toFileUrl(s.assetPath));
  }, [stage, s.assetPath]);

  // EXIF 元信息（只读参考行）
  useEffect(() => {
    let cancelled = false;
    getAsset(s.assetId)
      .then((a) => !cancelled && setAsset(a))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [s.assetId]);

  // W3-4：不再补「假分面」—— V20 后标签 key 应全部命中真实分面。
  // 真出现未知 key（分面被删后残留的标签）→ 显示 warning 提示，不静默造面板。
  const orphanKeys = useMemo(() => {
    const known = new Set([...aiGroup, ...manualGroup].map((f) => f.key));
    return Object.keys(tags).filter((k) => !known.has(k));
  }, [aiGroup, manualGroup, tags]);

  const setFacetTags = (key: string, list: string[]) => {
    const next = { ...tags };
    if (list.length === 0) delete next[key];
    else next[key] = list;
    onTagsChange(next);
  };

  /** 添加标签：单选分面选择新标签时替换旧值（§9.4）；maxItems 超限给出明确反馈。
   *  name 由 FacetTagInput 传入（可能为选中候选的规范名，或用户 Enter 的新词）。 */
  const [capHint, setCapHint] = useState<Record<string, string>>({});
  const addTag = (f: WorkbenchFacet, name?: string) => {
    const key = f.key;
    const t = (name ?? editing[key] ?? "").trim();
    if (!t) return;
    const cur = tags[key] ?? [];
    if (cur.includes(t)) {
      setEditing({ ...editing, [key]: "" });
      return;
    }
    // 数量上限：单选恒 1；multi 时 maxItems 为 null = 不限（W3-4 修复：不再回退硬编码 3）
    const single = f.selectionMode === "single";
    const cap = single ? 1 : (f.maxItems ?? null);
    if (!single && cap !== null && cur.length >= cap) {
      setCapHint((m) => ({ ...m, [key]: `已达上限 ${cap} 个` }));
      setTimeout(() => setCapHint((m) => ({ ...m, [key]: "" })), 2500);
      return;
    }
    setFacetTags(key, single ? [t] : [...cur, t]);
    setEditing({ ...editing, [key]: "" });
    setCapHint((m) => ({ ...m, [key]: "" }));
  };

  const totalTags = Object.values(tags).reduce((n, l) => n + l.length, 0);
  const fileName = displayBasename(s.assetPath);
  const exif = exifLine(asset);

  const handleConfirm = useCallback(async () => {
    setBusy(true);
    try {
      await onConfirm();
    } finally {
      setBusy(false);
    }
  }, [onConfirm]);

  const doJump = () => {
    const n = parseInt(jump, 10);
    if (!Number.isNaN(n) && n >= 1 && n <= total) onGoto(n - 1);
    setJump("");
  };

  // 快捷键：←/→ 过片，Enter 确认
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      if (e.key === "ArrowLeft") onGoto(index - 1);
      else if (e.key === "ArrowRight") onGoto(index + 1);
      else if (e.key === "Enter") void handleConfirm();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, onGoto, handleConfirm]); // B33：补依赖数组，避免每次渲染重绑

  // W3-4：单行分面渲染（两组共用；maxItems 为 null = 不限，不回退硬编码 3）
  const renderFacetRow = (f: WorkbenchFacet) => {
    const list = tags[f.key] ?? [];
    const hint = capHint[f.key];
    const single = f.selectionMode === "single";
    const cap = single ? 1 : (f.maxItems ?? null);
    const reached = !single && cap !== null && list.length >= cap;
    return (
      <div key={f.key} className="flex min-h-8 items-start gap-3 border-b border-[var(--color-border)]/70 pb-2">
        <span className="mt-1.5 w-16 shrink-0 truncate text-xs font-medium text-[var(--color-text-secondary)]" title={f.key}>
          {f.displayName}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-1">
            {list.map((t) => (
              <TagChip key={t} label={t} onRemove={readOnly ? undefined : () => setFacetTags(f.key, list.filter((x) => x !== t))} />
            ))}
            {list.length === 0 && f.key === "subject" && (
              <span
                data-testid="subject-unrecognized"
                className="inline-flex h-6 items-center rounded-full border border-dashed border-[var(--color-border)] px-2 text-xs text-[var(--color-text-tertiary)]"
              >
                未识别
              </span>
            )}
            {!readOnly && (
              <FacetTagInput
                facetKey={f.key}
                value={editing[f.key] ?? ""}
                onValueChange={(v) => setEditing({ ...editing, [f.key]: v })}
                onCommit={(n) => addTag(f, n)}
              />
            )}
          </div>
          {!single && reached && (
            <span className="mt-0.5 block text-[10px] text-[var(--color-status)]" data-testid={`max-${f.key}`}>
              已达上限 {cap} 个
            </span>
          )}
          {hint && <span className="mt-0.5 block text-[10px] text-[var(--color-status)]">{hint}</span>}
        </div>
      </div>
    );
  };

  return (
    <>
      {/* ① 大图区 + 底部居中悬浮导航条 */}
      <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-[var(--color-surface)] p-6">
        {imgUrl ? (
          <img src={imgUrl} alt={fileName} onError={onImgError} className="max-h-full max-w-full rounded-[5px] object-contain" />
        ) : (
          <div className="h-32 w-32 animate-pulse rounded bg-[var(--color-border)]" />
        )}
        <span className="absolute top-3 right-4 rounded-md bg-black/55 px-2 py-1 text-[11px] font-medium text-white backdrop-blur-sm">
          {index + 1} / {total}
        </span>
        <span className="absolute top-3 left-4 max-w-[60%] truncate rounded-md bg-black/55 px-2 py-1 text-[11px] font-medium text-white backdrop-blur-sm">
          {fileName}
        </span>

        {/* 悬浮导航条（v2.11）：← 位置/跳转 → */}
        <div className="absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)]/94 px-1.5 py-1 shadow-[var(--shadow-soft)] backdrop-blur">
          <button
            onClick={() => onGoto(index - 1)}
            disabled={index <= 0}
            title="上一张（←）"
            className="flex h-7 w-8 items-center justify-center rounded-md text-sm text-[var(--color-text)] transition-colors hover:bg-[var(--color-surface)] disabled:opacity-30"
          >
            ←
          </button>
          <span className="px-1 text-xs text-[var(--color-text-secondary)]">
            {index + 1} / {total}
          </span>
          <button
            onClick={() => onGoto(index + 1)}
            disabled={index >= total - 1}
            title="下一张（→）"
            className="flex h-7 w-8 items-center justify-center rounded-md text-sm text-[var(--color-text)] transition-colors hover:bg-[var(--color-surface)] disabled:opacity-30"
          >
            →
          </button>
          <span className="mx-0.5 h-4 w-px bg-[var(--color-border)]" />
          <input
            value={jump}
            onChange={(e) => setJump(e.target.value.replace(/\D/g, ""))}
            onKeyDown={(e) => e.key === "Enter" && doJump()}
            placeholder="跳至"
            title="跳转到第 N 张，Enter 确认"
            className="ui-control w-12 px-1.5 py-1 text-center text-xs"
          />
        </div>
      </div>

      {filmstrip}

      {/* ② EXIF 只读参考行 */}
      {exif && (
        <div className="shrink-0 border-b border-[var(--color-border)] px-4 py-2 text-[11px] text-[var(--color-text-secondary)]">
          {exif}
        </div>
      )}

      {/* ②b 打标失败原因（v6：rejected 且带 lastError 时展示，替代“只看得到灰块”混淆） */}
      {isRejected && s.lastError && (
        <div className="shrink-0 border-t border-[var(--color-danger)] px-3 py-2 text-[11px] text-[var(--color-danger)]">
          <p className="font-medium">本张打标失败：</p>
          <p className="mt-0.5 leading-4 break-all">{s.lastError}</p>
        </div>
      )}

      {/* ③ 分类标签面板：标签优先保留操作高度，图片区只占剩余空间 */}
      <div
        data-testid="workbench-facet-panel"
        className="flex h-[clamp(15rem,38vh,22rem)] shrink-0 flex-col overflow-hidden bg-[var(--color-bg)]"
      >
        <div data-testid="workbench-facet-scroll" className="min-h-0 overflow-y-auto px-4 pt-3 pb-2">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="ui-section-title">标签</h3>
            <span className="text-[11px] text-[var(--color-text-tertiary)]">共 {totalTags} 个</span>
          </div>
          {/* V24（Phase 7-4）：数值建议 —— 确认建议时自动落库；歧义项保持待确认 */}
          {numberItems && numberItems.length > 0 && (
            <div className="mb-2 border-b border-[var(--color-border)]/70 pb-2">
              <span className="text-xs font-medium text-[var(--color-text-secondary)]">数值建议</span>
              <div className="mt-1 flex flex-col gap-1">
                {numberItems.map((n) => (
                  <div key={n.id} className="flex min-h-6 flex-wrap items-center gap-2 text-xs" data-testid="workbench-number-item">
                    <span className="font-medium text-[var(--color-text)]">{n.displayName}</span>
                    {n.numValue != null ? (
                      <>
                        <span className="rounded bg-[var(--color-surface)] px-1.5 py-0.5 tabular-nums">{n.numValue}</span>
                        {n.decision === "pending" && !readOnly && onDecideNumberItem ? (
                          <>
                            <button type="button" aria-label={`采纳数值建议 ${n.displayName}`} onClick={() => void onDecideNumberItem(n.id, "accepted")} className="text-[var(--color-status)] hover:underline">采纳</button>
                            <button type="button" aria-label={`拒绝数值建议 ${n.displayName}`} onClick={() => void onDecideNumberItem(n.id, "rejected")} className="text-[var(--color-text-tertiary)] hover:underline">拒绝</button>
                          </>
                        ) : (
                          <span className="text-[10px] text-[var(--color-text-tertiary)]">{n.decision === "accepted" ? "已采纳" : n.decision === "rejected" ? "已拒绝" : ""}</span>
                        )}
                      </>
                    ) : (
                      <span className="text-[10px] text-[var(--color-text-tertiary)]">
                        需人工确认：{n.decisionReason ?? "表达有歧义"}（数值不自动取值；可在看片台手工赋值）
                      </span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}
          {/* 一句话描述。AI 目标 12–30 字；手工单行 input、maxLength 30、字符计数用 JS 字符迭代；
              位于标签分面滚动区顶部（分面滚动时描述保持在编辑区顶部）。 */}
          <div className="mb-2 flex min-h-8 items-center gap-3 border-b border-[var(--color-border)]/70 pb-2">
            <span className="w-16 shrink-0 text-xs font-medium text-[var(--color-text-secondary)]">一句话描述</span>
            <div className="min-w-0 flex-1">
              {readOnly ? (
                <span className="block truncate text-sm text-[var(--color-text)]">
                  {description.trim() || "未生成描述"}
                </span>
              ) : (
                <div className="flex items-center gap-2">
                  <input
                    data-testid="suggestion-description"
                    value={description}
                    onChange={(e) => onDescriptionChange(e.target.value)}
                    maxLength={30}
                    placeholder="如「女子站在湖边树下回头张望」（12–30 字）"
                    aria-label="一句话描述"
                    className="ui-control h-7 min-w-0 flex-1 rounded-md px-2 text-sm outline-none focus:border-[var(--color-accent)]"
                  />
                  <span className="shrink-0 text-[11px] tabular-nums text-[var(--color-text-tertiary)]">
                    {[...description].length}/30
                  </span>
                </div>
              )}
            </div>
          </div>
          {/* W3-4：AI 识别组在前（含分隔标题），需要你填组在后（视觉分隔） */}
          {aiGroup.length > 0 && (
            <p className="mt-1 mb-1 text-[11px] font-semibold text-[var(--color-text-tertiary)]">AI 识别</p>
          )}
          <div className="grid grid-cols-1 gap-x-6 gap-y-2 xl:grid-cols-2">
            {aiGroup.map((f) => renderFacetRow(f))}
          </div>
          {manualGroup.length > 0 && (
            <p className="mt-3 mb-1 border-t border-[var(--color-border)]/70 pt-2 text-[11px] font-semibold text-[var(--color-text-tertiary)]">
              需要你填（不参与 AI 自动打标）
            </p>
          )}
          <div className="grid grid-cols-1 gap-x-6 gap-y-2 xl:grid-cols-2">
            {manualGroup.map((f) => renderFacetRow(f))}
          </div>
          {orphanKeys.length > 0 && (
            <p className="mt-2 text-[11px] text-[var(--color-status)]">
              有 {orphanKeys.length} 个标签属于已删除或未知的分类（{orphanKeys.join("、")}），确认后将归入 custom。
            </p>
          )}
        </div>
        <div data-testid="workbench-confirm-bar" className="flex shrink-0 items-center gap-2 border-t border-[var(--color-border)] bg-[var(--color-bg)] px-4 py-3">
          <span className="text-xs text-[var(--color-text-secondary)]">{readOnly ? "当前结果为只读状态" : "Enter 添加标签，方向键切换图片"}</span>
          <div className="ml-auto flex gap-2">
            {isRejected ? (
              <Button onClick={() => void onRestore()}>恢复（撤销拒绝）</Button>
            ) : s.status === "confirmed" ? (
              <span className="inline-flex min-h-9 items-center rounded-md bg-[var(--color-surface)] px-3 text-xs font-medium text-[var(--color-success)]">
                ✓ 已写入
              </span>
            ) : (
              <>
                <Button disabled={busy} onClick={() => void onReject()}>
                  拒绝
                </Button>
                <Button variant="primary" disabled={busy || (totalTags === 0 && description.trim().length === 0)} onClick={() => void handleConfirm()}>
                  {busy ? "写入中…" : "确认写入"}
                </Button>
              </>
            )}
          </div>
        </div>
      </div>
    </>
  );
}
