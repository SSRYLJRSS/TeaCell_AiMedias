/** 顶栏（PRD v2.7）：搜索框 + 选中操作条（并入）+ 排序下拉（R-21）+ 总数
 *  类型筛选已移入左侧栏「类型」区；回收站模式（R-22）渲染恢复/彻底删除操作条
 */
import { useState } from "react";
import clsx from "clsx";
import { Grid3x3, Grid2x2, Square } from "lucide-react";
import SearchInput from "@/components/common/SearchInput";
import Button from "@/components/common/Button";
import ContextActionBar from "@/components/library/ContextActionBar";
import SelectedFilterTags from "@/components/library/SelectedFilterTags";
import { useShallow } from "zustand/react/shallow";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useAppearance } from "@/hooks/useAppearance";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  failedRestores,
  restoreAssets,
  restoreErrorLabel,
  unknownRestores,
  type RestoreReport,
} from "@/api/trash";

interface GridToolbarProps {
  onTag: () => void;
  onExport: () => void;
  /** A4-01：库外移动已停用；库内分库移动属 A4-02，暂不渲染入口 */
  onMove?: () => void;
  onDelete: () => void;
  /** W0-7：查找重复入口（hash 精确去重，扫描按钮在库非回收站区） */
  onDedup?: () => void;
  /** R-22：回收站模式下的「彻底删除」（弹窗由 LibraryPage 托管） */
  onPurge: () => void;
  /** A3-08：把当前选择固定成快照交给助手 */
  onHandoff?: () => void;
}

/** R-21 排序选项（与后端 AssetFilter.sortBy 对齐）；W2-8 补「评级」 */
const SORT_OPTIONS: { value: "created_at" | "taken_at" | "size" | "resolution" | "name" | "modified_at" | "rating"; label: string }[] = [
  { value: "created_at", label: "入库时间" },
  { value: "taken_at", label: "拍摄时间" },
  { value: "modified_at", label: "修改时间" },
  { value: "name", label: "文件名" },
  { value: "size", label: "文件大小" },
  { value: "resolution", label: "分辨率" },
  { value: "rating", label: "评级" },
];

/** FB2-01 三态大小入口：小/中/大 → 档位 1 / 3 / 5（与滚轮、Ctrl+± 状态同步） */
const SIZE_STEPS = [
  { step: 1, label: "小", Icon: Grid3x3, title: "格子小" },
  { step: 3, label: "中", Icon: Grid2x2, title: "格子中" },
  { step: 5, label: "大", Icon: Square, title: "格子大" },
];

export default function GridToolbar({ onTag, onExport, onMove, onDelete, onDedup, onPurge, onHandoff }: GridToolbarProps) {
  const { setFilter, total, sortBy, sortDir, trashOnly, removeLocal } = useLibraryStore(
    useShallow((s) => ({
      setFilter: s.setFilter,
      total: s.total,
      sortBy: s.filter.sortBy,
      sortDir: s.filter.sortDir,
      trashOnly: s.filter.trashOnly,
      removeLocal: s.removeLocal,
    })),
  );
  const { selected, truncated, selectionTotal, clear } = useSelectionStore(
    useShallow((s) => ({ selected: s.selected, truncated: s.truncated, selectionTotal: s.selectionTotal, clear: s.clear })),
  );
  const [restoring, setRestoring] = useState(false);
  // A4-03：恢复不是「清空 deleted_at 就算完」，所以必须把失败项原样显示出来。
  const [restoreReport, setRestoreReport] = useState<RestoreReport | null>(null);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const trashRetentionDays = useSettingsStore((s) => s.settings?.trashRetentionDays ?? 30);
  // FB2-01 可发现入口：素材库顶栏三态大小切换（小/中/大 = 档位 1/3/5）
  const { grid } = useAppearance();

  const restore = async () => {
    const ids = Array.from(selected);
    if (ids.length === 0 || restoring) return;
    setRestoring(true);
    setRestoreError(null);
    try {
      const report = await restoreAssets(ids);
      setRestoreReport(report);
      // 只移除真的恢复成功的条目：失败项仍留在回收站，也仍应保持选中。
      const restoredIds = report.items
        .filter((item) => item.state === "succeeded")
        .map((item) => item.assetId);
      if (restoredIds.length > 0) removeLocal(restoredIds);
      // 结果不确定的项也算「未落定」：它们可能仍在回收站，选中态必须保留，
      // 否则用户核对时找不到这些素材。
      const unsettled = report.failed > 0 || report.unknown > 0 || report.skipped > 0;
      if (unsettled) {
        useSelectionStore.getState().setAll(
          report.items.filter((item) => item.state !== "succeeded").map((item) => item.assetId),
        );
      } else {
        clear();
      }
    } catch (error) {
      setRestoreError(error instanceof Error ? error.message : String(error));
    } finally {
      setRestoring(false);
    }
  };

  // FB2-01：顶栏点击三态大小 → 即时预览 + 防抖持久化（与滚轮档位同一管道路径）
  const setSizeStep = (step: number) => {
    if (step === grid.libraryCellStep) return;
    useSettingsStore.getState().commitAppearanceDebounced((a) => ({
      ...a,
      grid: { ...a.grid, libraryCellStep: step },
    }));
  };

  return (
    <div data-testid="grid-toolbar" className="flex h-11 shrink-0 items-center gap-3 border-b border-[var(--color-border)] px-3">
      {/* 设置入口已移至顶部标题栏（指导书 §10.1），此处移除避免重复入口与导航状态分叉 */}
      <SearchInput onSearch={(kw) => setFilter({ search: kw })} />

      {trashOnly ? (
        /* R-22 回收站操作条：恢复 / 彻底删除 */
        selected.size > 0 ? (
          <div className="flex min-w-0 items-center gap-0.5">
            {/* §4.6：截断时选择栏恒显示「已选 N / total」 */}
            <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">
              {truncated ? `已选中 ${selected.size} / ${selectionTotal} 项` : `已选中 ${selected.size} 项`}
            </span>
            <span className="mx-1 h-4 w-px shrink-0 bg-[var(--color-border)]" />
            <Button onClick={() => void restore()} disabled={restoring}>
              {restoring ? "恢复中…" : "恢复"}
            </Button>
            <Button variant="danger" onClick={onPurge}>彻底删除</Button>
            <Button onClick={clear}>取消</Button>
          </div>
        ) : (
          <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">
            {`回收站 · 保留 ${trashRetentionDays} 天后自动清理（可在设置中调整）`}
          </span>
        )
      ) : (
        <ContextActionBar onTag={onTag} onExport={onExport} onMove={onMove} onDelete={onDelete} onHandoff={onHandoff} />
      )}

      {restoreError && (
        <span role="alert" data-testid="restore-error" className="min-w-0 shrink truncate text-xs text-[var(--color-danger)]" title={restoreError}>
          恢复失败：{restoreError}
        </span>
      )}
      {restoreReport &&
        (restoreReport.failed > 0 || restoreReport.unknown > 0 || restoreReport.skipped > 0) && (
          <span data-testid="restore-summary" className="min-w-0 shrink truncate text-xs text-[var(--color-text-secondary)]">
            已恢复 {restoreReport.restored} 项
            {restoreReport.failed > 0 ? `，${restoreReport.failed} 项失败（仍留在回收站）：` : ""}
            {failedRestores(restoreReport).map((item, index) => (
              <span key={item.itemId}>
                {index > 0 ? "；" : ""}
                {item.fileName} · {restoreErrorLabel(item)}
              </span>
            ))}
            {restoreReport.unknown > 0 ? (
              <span className="text-[var(--color-status)]">
                {restoreReport.failed > 0 ? "；" : ""}
                {`${restoreReport.unknown} 项结果不明（可能已恢复，需核对）：`}
                {unknownRestores(restoreReport).map((item, index) => (
                  <span key={item.itemId}>
                    {index > 0 ? "；" : ""}
                    需核对：{item.fileName}
                  </span>
                ))}
              </span>
            ) : (
              ""
            )}
          </span>
        )}

      <div data-testid="toolbar-filters" className="min-w-0 flex-1 overflow-hidden">
        <SelectedFilterTags />
      </div>

      <div data-testid="toolbar-actions" className="flex shrink-0 items-center gap-3">
        {!trashOnly && (
          /* W0-7：恢复去重入口（dedup_scan + DupDialog 已交付但此前不可达）。hash 重复组为 0 时显示「没有重复」是正确结果 */
          onDedup && (
            <button
              type="button"
              onClick={onDedup}
              className="shrink-0 rounded px-1.5 py-0.5 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
              title="扫描内容完全相同的素材（精确 hash 去重）"
            >
              查找重复
            </button>
          )
        )}
        {!trashOnly && (
          <div className="flex items-center gap-0.5">
            <select
              value={sortBy}
              onChange={(e) => setFilter({ sortBy: e.target.value as typeof sortBy })}
              className="rounded border border-[var(--color-border)] bg-[var(--color-bg)] px-1.5 py-0.5 text-xs text-[var(--color-text-secondary)] outline-none hover:text-[var(--color-text)]"
              title="排序方式"
            >
              {SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
            <button
              onClick={() => setFilter({ sortDir: sortDir === "desc" ? "asc" : "desc" })}
              className="rounded px-1.5 py-0.5 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
              title={sortDir === "desc" ? "当前降序，点击切换升序" : "当前升序，点击切换降序"}
            >
              {sortDir === "desc" ? "↓" : "↑"}
            </button>
          </div>
        )}

        <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">{total} 项</span>

        {/* FB2-01 可发现入口：三态大小（小/中/大） */}
        <div className="flex shrink-0 items-center overflow-hidden rounded-md border border-[var(--color-border)]">
          {SIZE_STEPS.map(({ step, label, Icon, title }) => (
            <button
              key={step}
              type="button"
              onClick={() => setSizeStep(step)}
              aria-label={label}
              aria-pressed={grid.libraryCellStep === step}
              title={title}
              data-active={grid.libraryCellStep === step}
              className={clsx(
                "flex h-6 w-7 items-center justify-center text-[var(--color-text-secondary)] transition-colors",
                grid.libraryCellStep === step
                  ? "bg-[var(--color-accent)] text-[var(--color-accent-text)]"
                  : "hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]",
              )}
            >
              <Icon className="h-3.5 w-3.5" aria-hidden />
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
