/** 标签建议审核视图（A3-07）：大图 + 胶片条 + 分类标签面板 + 确认/拒绝/恢复。
 *
 * 设计边界（plans/v2/execution/A3.md A3-07 第 3 条）：
 * - 「复用标签建议切图、编辑、批量确认」——独立打标页与助手右侧工作台必须用同一份实现，
 *   否则两处会出现「同一条建议两种行为」；
 * - 正式确认只走可信 API（aiConfirmSuggestion / aiRejectSuggestion / aiRestoreSuggestion），
 *   组件本身不 invoke、不拼计划、不写库；
 * - 防旧建议覆盖新编辑：切张或建议已终态时重置 draftEdited，只有在用户没编辑过时才回填
 *   建议值；确认后回载的新建议不会把用户正在编辑的内容盖掉。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Filmstrip from "@/components/ai/Filmstrip";
import Workbench, { type WorkbenchNumberItem } from "@/components/ai/Workbench";
import {
  aiApplyTags,
  aiDecideSuggestionItem,
  aiListSuggestionItems,
} from "@/api/ai";
import { useAiStore } from "@/stores/aiStore";
import { buildWorkbenchFacets, normalizeTagKeys, useTagStore } from "@/stores/tagStore";
import { pickReviewDescription } from "@/utils/reviewDescription";
import type { AiSuggestion, CategorizedTags } from "@/types/ai";

export interface SuggestionReviewProps {
  suggestions: AiSuggestion[];
  index: number;
  onGoto: (index: number) => void;
  /** 无当前建议时的说明文案（三态：生成中 / 全部处理完 / 还没有批次） */
  emptyHint: string;
  /** 确认/套用后刷新素材库与标签（由页面注入，组件不自己决定刷新范围） */
  onAfterWrite: () => void | Promise<void>;
}

export default function SuggestionReview({
  suggestions,
  index,
  onGoto,
  emptyHint,
  onAfterWrite,
}: SuggestionReviewProps) {
  const confirmSuggestion = useAiStore((s) => s.confirm);
  const rejectSuggestion = useAiStore((s) => s.reject);
  const restoreSuggestion = useAiStore((s) => s.restore);
  const tagFacets = useTagStore((s) => s.facets);

  const workbenchFacets = useMemo(() => buildWorkbenchFacets(tagFacets), [tagFacets]);

  const idx = Math.min(index, Math.max(0, suggestions.length - 1));
  const currentSuggestion: AiSuggestion | null = suggestions[idx] ?? null;
  const currentSuggestionId = currentSuggestion?.id;

  const [draftTags, setDraftTags] = useState<CategorizedTags>({});
  const draftEdited = useRef(false);
  const draftSuggestionId = useRef<number | null>(null);
  const [numberItems, setNumberItems] = useState<WorkbenchNumberItem[]>([]);

  const loadNumberItems = useCallback(
    async (suggestionId: number): Promise<WorkbenchNumberItem[]> => {
      const items = await aiListSuggestionItems(suggestionId);
      return items
        .filter((item) => item.itemKind === "number")
        .map((item) => ({
          id: item.id,
          facetKey: item.facetKey,
          displayName: tagFacets.find((facet) => facet.key === item.facetKey)?.displayName ?? item.facetKey,
          numValue: item.numValue ?? null,
          decision: item.decision,
          decisionReason: item.decisionReason,
        }));
    },
    [tagFacets],
  );

  useEffect(() => {
    let alive = true;
    if (currentSuggestionId == null) {
      setNumberItems([]);
      return;
    }
    void loadNumberItems(currentSuggestionId)
      .then((items) => {
        if (alive) setNumberItems(items);
      })
      .catch(() => {
        if (alive) setNumberItems([]);
      });
    return () => {
      alive = false;
    };
  }, [currentSuggestionId, loadNumberItems]);

  const decideNumberItem = useCallback(
    async (itemId: number, decision: "accepted" | "rejected") => {
      await aiDecideSuggestionItem(itemId, decision);
      if (currentSuggestionId == null) return;
      try {
        setNumberItems(await loadNumberItems(currentSuggestionId));
      } catch {
        /* 刷新失败静默：下次切换建议时重拉 */
      }
    },
    [currentSuggestionId, loadNumberItems],
  );

  // 切张或建议已终态：丢弃上一张的编辑标记，允许回填新建议
  useEffect(() => {
    const suggestionId = currentSuggestion?.id ?? null;
    const finalized =
      currentSuggestion?.status === "confirmed" || currentSuggestion?.status === "rejected";
    if (draftSuggestionId.current !== suggestionId || finalized) {
      draftSuggestionId.current = suggestionId;
      draftEdited.current = false;
    }
  }, [currentSuggestion?.id, currentSuggestion?.status]);

  useEffect(() => {
    if (!currentSuggestion) return;
    if (draftEdited.current) return;
    const source =
      currentSuggestion.status === "confirmed" && Object.keys(currentSuggestion.confirmedTags).length > 0
        ? currentSuggestion.confirmedTags
        : currentSuggestion.suggestedTags;
    // 依赖含 suggestedTags 内容：批次跑完回载后 id/status 不变但标签已写入，
    // 只依赖 [id, status] 会让当前张停在旧的空 draft，此时点确认会写入空标签。
    setDraftTags(normalizeTagKeys(source, tagFacets.map((facet) => facet.key)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentSuggestion?.id, currentSuggestion?.status, JSON.stringify(currentSuggestion?.suggestedTags)]);

  const [draftDescription, setDraftDescription] = useState("");
  const reviewDescription = currentSuggestion ? pickReviewDescription(currentSuggestion) : null;
  const currentSuggestionStatus = currentSuggestion?.status;
  useEffect(() => {
    if (reviewDescription === null) return;
    if (draftEdited.current) return;
    setDraftDescription(reviewDescription);
  }, [currentSuggestionId, currentSuggestionStatus, reviewDescription]);

  const [selectedAssets, setSelectedAssets] = useState<Set<number>>(new Set());
  const onFilmPick = useCallback(
    (suggestion: AiSuggestion, ctrl: boolean) => {
      if (ctrl) {
        setSelectedAssets((prev) => {
          const next = new Set(prev);
          if (next.has(suggestion.assetId)) next.delete(suggestion.assetId);
          else next.add(suggestion.assetId);
          return next;
        });
        return;
      }
      const target = suggestions.findIndex((item) => item.id === suggestion.id);
      if (target >= 0) onGoto(target);
    },
    [onGoto, suggestions],
  );

  const [applying, setApplying] = useState(false);
  const applyToSelected = async () => {
    if (selectedAssets.size === 0 || Object.keys(draftTags).length === 0) return;
    setApplying(true);
    try {
      await aiApplyTags(Array.from(selectedAssets), draftTags);
      setSelectedAssets(new Set());
      await onAfterWrite();
    } finally {
      setApplying(false);
    }
  };

  if (!currentSuggestion) {
    return (
      <div className="flex flex-1 items-center justify-center px-6 text-center text-sm text-[var(--color-text-secondary)]">
        {emptyHint}
      </div>
    );
  }

  return (
    <Workbench
      suggestion={currentSuggestion}
      aiGroup={workbenchFacets.aiGroup}
      manualGroup={workbenchFacets.manualGroup}
      tags={draftTags}
      onTagsChange={(next) => {
        draftEdited.current = true;
        setDraftTags(next);
      }}
      description={draftDescription}
      onDescriptionChange={(next) => {
        draftEdited.current = true;
        setDraftDescription(next);
      }}
      numberItems={numberItems}
      onDecideNumberItem={decideNumberItem}
      index={idx}
      total={suggestions.length}
      filmstrip={
        <div className="relative shrink-0">
          {selectedAssets.size > 0 && (
            <div className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] bg-[var(--color-bg)] px-4 py-2">
              <span className="text-xs text-[var(--color-text-secondary)]">
                已选 {selectedAssets.size} 张
              </span>
              <button
                onClick={() => void applyToSelected()}
                disabled={applying || Object.keys(draftTags).length === 0}
                className="rounded-md bg-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent-text)] disabled:opacity-50"
              >
                {applying ? "套用中…" : "套用当前标签"}
              </button>
              <button
                onClick={() => setSelectedAssets(new Set())}
                className="rounded-md px-2 py-1 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
              >
                取消
              </button>
            </div>
          )}
          <Filmstrip
            suggestions={suggestions}
            currentId={currentSuggestion.id}
            selectedIds={selectedAssets}
            onPick={onFilmPick}
          />
        </div>
      }
      onGoto={onGoto}
      onConfirm={async () => {
        await confirmSuggestion(currentSuggestion.id, draftTags, draftDescription);
        await onAfterWrite();
      }}
      onReject={async () => {
        await rejectSuggestion(currentSuggestion.id);
      }}
      onRestore={async () => {
        await restoreSuggestion(currentSuggestion.id);
      }}
    />
  );
}
