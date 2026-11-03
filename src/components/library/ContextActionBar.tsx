/** 选中操作条（PRD v2.8）：顶栏内联，统一文字按钮风格，无特殊色框。
 *
 * M-14b（QA_PLAYBOOK U-04）：U-04 把「删除/打标/收藏/导出」列为必须在
 * 【网格右键、工具栏、查看器、批量操作栏】四处行为一致的动作，并写明「不一致即 A 类缺陷」。
 * 恢复评级收藏后右键与查看器都有了入口，唯独批量操作栏没有，所以这里补「收藏 / 取消收藏」，
 * 与另外两处共用 `src/api/assets.ts` 的同一对命令（取反语义也一致）。
 */
import { useCallback, useState } from "react";
import { Copy, FolderInput, Star, Tag, Trash2, UserRoundPlus, X } from "lucide-react";
import Button from "@/components/common/Button";
import { useShallow } from "zustand/react/shallow";
import { setFavorite } from "@/api/assets";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";

interface Props {
  onTag: () => void;
  onExport: () => void;
  /**
   * A4-01：导出已统一为「复制」，库外移动停用（后端对 `mode="move"` 明确拒绝）。
   * 库内分库移动是 A4-02 的能力，接口先留成可选：不传就不渲染入口，
   * 避免出现一个点了只会报错的按钮。
   */
  onMove?: () => void;
  onDelete: () => void;
  /** A3-08：把当前选择固定成快照交给助手（只读交接，不移动/不导出） */
  onHandoff?: () => void;
}

export default function ContextActionBar({ onTag, onExport, onMove, onDelete, onHandoff }: Props) {
  const { selected, truncated, selectionTotal, clear } = useSelectionStore(
    useShallow((s) => ({ selected: s.selected, truncated: s.truncated, selectionTotal: s.selectionTotal, clear: s.clear })),
  );
  const [favError, setFavError] = useState<string | null>(null);
  const [favBusy, setFavBusy] = useState(false);

  const favorite = useCallback(() => {
    const ids = Array.from(useSelectionStore.getState().selected);
    if (ids.length === 0) return;
    const items = useLibraryStore.getState().items;
    const previous = new Map(ids.map((id) => [id, items.find((a) => a.id === id)?.favorite ?? 0]));
    const next = ids.some((id) => (previous.get(id) ?? 0) === 0) ? 1 : 0;
    setFavError(null);
    setFavBusy(true);
    useLibraryStore.getState().patchLocal(ids, { favorite: next });
    void setFavorite(ids, next === 1)
      .catch((error: unknown) => {
        // 失败逐项回滚，不让星标停在没写进库的状态
        for (const [id, value] of previous) useLibraryStore.getState().patchLocal([id], { favorite: value });
        setFavError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => setFavBusy(false));
  }, []);

  if (selected.size === 0) return null;

  // §4.6：截断集合上导出/批量打标二次确认；删除直接禁用（不可逆 + 集合不完整）
  const guard = (action: () => void, noun: string) => () => {
    if (truncated) {
      const ok = window.confirm(`将${noun}已选中的 ${selected.size} 张（共 ${selectionTotal} 张匹配）。继续？`);
      if (!ok) return;
    }
    action();
  };

  return (
    <div className="flex min-w-0 items-center gap-0.5">
      {/* §4.6：截断时选择栏恒显示「已选 N / total」 */}
      <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">
        {truncated ? `已选中 ${selected.size} / ${selectionTotal} 项` : `已选中 ${selected.size} 项`}
      </span>
      <span className="mx-1 h-4 w-px shrink-0 bg-[var(--color-border)]" />

      {/* 选中态要压进 960px 的最小窗口：批量动作收成图标按钮，文字留给读屏和 title。
          之前 7 个字面按钮 + 搜索框 ≈1200px，会把右侧排序/项数整块挤出窗口。 */}
      <Button className="size-7 min-h-0 px-0" title="打标" onClick={guard(onTag, "批量打标")}>
        <Tag size={15} aria-hidden="true" />
        <span className="sr-only">打标</span>
      </Button>
      {onHandoff && (
        <Button className="size-7 min-h-0 px-0" title="交给助手" onClick={onHandoff}>
          <UserRoundPlus size={15} aria-hidden="true" />
          <span className="sr-only">交给助手</span>
        </Button>
      )}

      <Button className="size-7 min-h-0 px-0" title="复制导出" onClick={guard(onExport, "导出")}>
        <Copy size={15} aria-hidden="true" />
        <span className="sr-only">复制导出</span>
      </Button>
      {/* M-14b：U-04 要求收藏在批量操作栏也有一致入口；截断集合同样走二次确认 */}
      <Button className="size-7 min-h-0 px-0" title="收藏 / 取消收藏" onClick={guard(favorite, "收藏")} disabled={favBusy}>
        <Star size={15} aria-hidden="true" />
        <span className="sr-only">收藏 / 取消收藏</span>
      </Button>
      {onMove && (
        <Button className="size-7 min-h-0 px-0" title="移动到…" onClick={guard(onMove, "移动")}>
          <FolderInput size={15} aria-hidden="true" />
          <span className="sr-only">移动到…</span>
        </Button>
      )}
      <Button
        className="size-7 min-h-0 px-0"
        title={truncated ? "当前结果超过 100000 张，请先收窄条件再删除" : "删除"}
        disabled={truncated}
        onClick={onDelete}
      >
        <Trash2 size={15} aria-hidden="true" />
        <span className="sr-only">删除</span>
      </Button>
      <Button className="size-7 min-h-0 px-0" title="取消选择" onClick={clear}>
        <X size={15} aria-hidden="true" />
        <span className="sr-only">取消</span>
      </Button>

      {favError && (
        <span role="alert" className="min-w-0 shrink truncate text-xs text-[var(--color-danger)]" title={favError}>
          收藏没有保存：{favError}
        </span>
      )}
    </div>
  );
}
