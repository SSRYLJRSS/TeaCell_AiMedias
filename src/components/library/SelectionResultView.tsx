/** A3-08：素材库「本次结果视图」。
 *
 *  这是**只读**快照浏览：它按 selectionRef 一页页读后端固定下来的 id 集合，
 *  不是为了替代筛选，也不创建分库。因此：
 *  - 不改筛选、不改素材库的选中集合（退出后浏览上下文原样保留）；
 *  - 原位说明「有多少项已不可用」，绝不静默缩短；
 *  - 数量口径来自后端 `snapshot/available/missing`，与搜索结果协议一致。
 *
 *  批量操作（打标/导出/移动/删除）仍回素材库正常选择里做 —— 结果视图只负责
 *  「这就是刚才交给助手的那批东西」。 */
import { useCallback, useEffect, useRef, useState } from "react";
import Button from "@/components/common/Button";
import AssetCard from "@/components/library/AssetCard";
import { ASPECT_RATIO } from "@/utils/cellFit";
import { thumbSizeForCell } from "@/utils/thumbSize";
import { pageSelection, type SelectionPage } from "@/api/selections";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import { useAppearance } from "@/hooks/useAppearance";
import { CELL_STEPS } from "@/types/settings";
import { logger } from "@/utils/logger";
import type { Asset } from "@/types/asset";

interface Props {
  selectionRef: string;
  title: string;
  query: string;
  onExit: () => void;
  onPreview: (asset: Asset) => void;
  /** 这份快照属于另一个会话（当前活动会话不是它）。
   *
   *  结果视图是全局单槽：在 A 会话打开后切到 B，回到库页仍会看到 A 的快照。
   *  不标注的话用户会以为这是 B 会话的交接，甚至拿它当作 B 的输出继续做决定。
   *  只声明事实，不做「自动切换会话」之类的隐式动作。 */
  fromOtherConversation?: boolean;
}

const PAGE_SIZE = 200;

export default function SelectionResultView({
  selectionRef,
  title,
  query,
  onExit,
  onPreview,
  fromOtherConversation = false,
}: Props) {
  const { grid } = useAppearance();
  const [page, setPage] = useState<SelectionPage | null>(null);
  const [items, setItems] = useState<Asset[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** 防止上一份快照的迟到分页结果串到新快照里。 */
  const revision = useRef(0);
  const rememberProgress = useSelectionResultStore((s) => s.rememberProgress);
  /** 打开 Viewer 会把这个组件整体卸载；位置存在 store 里，回来时按页数与滚动恢复。 */
  const savedProgress = useRef(useSelectionResultStore.getState().progressBySelection[selectionRef]);
  const loadedPages = useRef(0);
  const scrollTop = useRef(savedProgress.current?.scrollTop ?? 0);
  const scroller = useRef<HTMLDivElement | null>(null);

  const load = useCallback(
    async (offset: number, replace: boolean) => {
      const at = replace ? ++revision.current : revision.current;
      setLoading(true);
      setError(null);
      try {
        const next = await pageSelection(selectionRef, offset, PAGE_SIZE);
        if (at !== revision.current) return;
        setPage(next);
        setItems((prev) => (replace ? next.items : [...prev, ...next.items]));
        loadedPages.current = replace ? 1 : loadedPages.current + 1;
        return next;
      } catch (cause) {
        if (at !== revision.current) return;
        logger.error("读取交接结果失败", {
          error: cause instanceof Error ? cause.message : String(cause),
        });
        setError(cause instanceof Error ? cause.message : String(cause));
        return undefined;
      } finally {
        if (at === revision.current) setLoading(false);
      }
    },
    [selectionRef],
  );

  useEffect(() => {
    setItems([]);
    setPage(null);
    loadedPages.current = 0;
    // 上次看到第几页就重新读到第几页：否则回来只剩首屏，滚动位置也没地方落。
    const targetPages = Math.max(1, savedProgress.current?.loadedPages ?? 1);
    void (async () => {
      let offset = 0;
      for (let index = 0; index < targetPages; index += 1) {
        const next = await load(offset, index === 0);
        if (!next) return;
        if (next.nextOffset == null) return;
        offset = next.nextOffset;
      }
    })();
  }, [load]);

  // 恢复滚动位置：必须在首屏渲染之后。
  useEffect(() => {
    if (items.length === 0 || !scroller.current) return;
    scroller.current.scrollTop = scrollTop.current;
  }, [items.length]);

  // 卸载（含打开 Viewer）时把位置留给下一次。
  useEffect(
    () => () => {
      rememberProgress(selectionRef, {
        loadedPages: Math.max(1, loadedPages.current),
        scrollTop: scroller.current?.scrollTop ?? scrollTop.current,
      });
    },
    [rememberProgress, selectionRef],
  );

  const cell = CELL_STEPS[grid.libraryCellStep] ?? CELL_STEPS[3];
  const thumbSize = thumbSizeForCell(cell);
  const [rw, rh] = ASPECT_RATIO[grid.cellAspect] ?? ASPECT_RATIO["4:3"];

  const missing = page?.missing ?? 0;
  /** A3-08：来源以快照行为准。props 里的 query 只是「打开时的那份说法」，
   *  同一份快照还会从助手卡片开，两处必须显示同一个来源。 */
  const sourceQuery = page?.query ?? query;

  return (
    <div data-testid="selection-result-view" className="flex h-full min-w-0 flex-col">
      <div className="flex h-11 shrink-0 items-center gap-3 border-b border-[var(--color-border)] px-3">
        <span className="min-w-0 flex-1 truncate text-xs text-[var(--color-text-secondary)]">
          <span className="font-medium text-[var(--color-text)]">{title}</span>
          <span className="mx-1.5">·</span>
          来自{sourceQuery}的固定快照
        </span>
        <Button onClick={onExit}>退出结果视图</Button>
      </div>

      <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-text-tertiary)]">
        {page && (
          <>
            <span>共 {page.snapshot} 项</span>
            <span>可用 {page.available} 项</span>
            {missing > 0 && (
              <span className="text-[var(--color-status)]">
                {missing} 项已不可用（被删除或已移出总库），不会显示
              </span>
            )}
            {page.truncated && (
              <span className="text-[var(--color-status)]">交接时即已截断：只包含当时加载到的项</span>
            )}
          </>
        )}
        <span className="ml-auto">这里只读；批量操作请回素材库正常选择</span>
      </div>

      {fromOtherConversation && (
        <div
          role="status"
          className="flex shrink-0 items-center gap-2 border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-status)]"
        >
          这份结果来自另一个助手会话的交接，不是当前会话的素材集合。
        </div>
      )}

      {error && (
        <div role="alert" className="flex shrink-0 items-center gap-2 px-3 py-2 text-xs text-[var(--color-status)]">
          <span className="min-w-0 flex-1">结果读取失败：{error}</span>
          <Button onClick={() => void load(0, true)}>重试</Button>
        </div>
      )}

      <div
        ref={scroller}
        onScroll={(event) => {
          scrollTop.current = event.currentTarget.scrollTop;
        }}
        className="min-h-0 flex-1 overflow-y-auto p-2"
      >
        {items.length === 0 && loading ? (
          <p className="p-4 text-center text-sm text-[var(--color-text-secondary)]">加载中…</p>
        ) : items.length === 0 && error ? (
          // 读取失败不等于「快照里什么都没有」：不能拿空态冒充结论。
          <p className="p-4 text-center text-sm text-[var(--color-text-secondary)]">
            这份快照暂时读不出来，请重试。
          </p>
        ) : items.length === 0 ? (
          <p className="p-4 text-center text-sm text-[var(--color-text-secondary)]">
            这份快照里的素材都已不可用。
          </p>
        ) : (
          <>
            <div
              className="grid gap-2"
              style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${cell}px, 1fr))` }}
            >
              {items.map((asset, index) => (
                <div key={asset.id} style={{ aspectRatio: rw / rh }}>
                  <AssetCard
                    asset={asset}
                    index={index}
                    selected={false}
                    thumbSize={thumbSize}
                    // 只读浏览：点选不改共享选择集合，双击进查看器。
                    onSelect={() => undefined}
                    onPreview={onPreview}
                    onContextMenu={() => undefined}
                  />
                </div>
              ))}
            </div>
            <div className="flex items-center justify-center gap-2 py-3">
              {page?.nextOffset != null ? (
                <Button disabled={loading} onClick={() => void load(page.nextOffset!, false)}>
                  {loading ? "加载中…" : "加载更多"}
                </Button>
              ) : (
                <span className="text-[11px] text-[var(--color-text-tertiary)]">
                  已显示全部 {items.length} 项
                </span>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
