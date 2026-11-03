import { useCallback, useEffect, useMemo, useState } from "react";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import {
  batchEntriesNeedingAttention,
  batchHeadline,
  cancelLegacyResolution,
  confirmLegacyResolution,
  confirmLegacyResolutionsBatch,
  legacyOutcomeLabel,
  listLegacyAssets,
  listUnlinkedLegacyAssets,
  prepareLegacyCopyBack,
  prepareLegacyCopyBackBatch,
  prepareLegacyUnlink,
  prepareLegacyUnlinkBatch,
  reopenUnlinkedLegacyAsset,
} from "@/api/legacyAssets";
import type {
  LegacyAsset,
  LegacyAssetPage,
  LegacyBatchReport,
  LegacyHashMatch,
  LegacyResolutionPreview,
} from "@/api/legacyAssets";

type ResolutionSelection = {
  resolutionId: string;
  asset: LegacyAsset;
  action: "copy_back" | "unlink";
  matchingAssets: LegacyHashMatch[];
};

export interface LegacyAssetPanelProps {
  /** 总仓离线/归属不明时给出的可操作出口：打开「总仓位置」向导重新定位。 */
  onRequestRelocation?: () => void;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function statusLabel(status: string): string {
  switch (status) {
    case "external_pending": return "已确认位于总仓外";
    case "missing": return "原文件缺失";
    default: return "归属待核实";
  }
}

function healthLabel(health: string): string {
  switch (health) {
    case "ready": return "总仓已核验";
    case "offline": return "总仓暂不可用";
    case "wrong_root": return "总仓位置或身份不匹配";
    case "unconfigured": return "尚未配置总仓";
    default: return "总仓尚未核验";
  }
}

/** 总仓不可用时该做什么。只说这几种状态，不猜原因。 */
function offlineHint(health: string): string {
  switch (health) {
    case "offline":
      return "总仓目录当前不可访问（可能是移动硬盘未接或目录被移动）。请重新连接，或把整个总仓搬回来后用「总仓位置」里的重新定位核对，再回来处置。逐项处置已全部暂停，记录与原文件都不会被改动。";
    case "wrong_root":
      return "当前总仓目录的位置或身份标记与记录不一致，可能是搬动过、或选错了目录。请用「总仓位置」里的重新定位核对完整总仓后重试；归属未确认前不会自动解除关联。";
    case "unconfigured":
      return "这台电脑还没有可用的总仓。请先在「总仓位置」里选择并核验总仓目录，再回来处置这些记录。";
    default:
      return "总仓尚未通过身份核验，逐项处置暂时不可用。请先完成总仓核验（见「总仓位置」）。";
  }
}

function sizeLabel(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function toSelection(preview: LegacyResolutionPreview): ResolutionSelection {
  return {
    resolutionId: preview.resolutionId,
    asset: preview.asset,
    action: preview.action,
    matchingAssets: preview.matchingAssets,
  };
}

export default function LegacyAssetPanel({ onRequestRelocation }: LegacyAssetPanelProps = {}) {
  const [page, setPage] = useState<LegacyAssetPage | null>(null);
  const [unlinked, setUnlinked] = useState<LegacyAsset[]>([]);
  const [unlinkedCursor, setUnlinkedCursor] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [busyAssetId, setBusyAssetId] = useState<number | null>(null);
  const [busyResolutionId, setBusyResolutionId] = useState<string | null>(null);
  const [selection, setSelection] = useState<ResolutionSelection | null>(null);
  const [selectedIds, setSelectedIds] = useState<number[]>([]);
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchReport, setBatchReport] = useState<LegacyBatchReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [nextPage, unlinkedRows] = await Promise.all([
        listLegacyAssets(0, 50),
        listUnlinkedLegacyAssets(0, 50),
      ]);
      setPage(nextPage);
      setUnlinked(unlinkedRows);
      setUnlinkedCursor(unlinkedRows.length === 50 ? unlinkedRows[unlinkedRows.length - 1]?.assetId ?? null : null);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const loadMore = async () => {
    if (page?.nextAfterAssetId === null || page?.nextAfterAssetId === undefined || loadingMore) return;
    setLoadingMore(true);
    try {
      const nextPage = await listLegacyAssets(page.nextAfterAssetId, 50);
      setPage((current) => current ? {
        ...nextPage,
        items: [...current.items, ...nextPage.items],
      } : nextPage);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setLoadingMore(false);
    }
  };

  const loadMoreUnlinked = async () => {
    if (unlinkedCursor === null || loadingMore) return;
    setLoadingMore(true);
    try {
      const nextRows = await listUnlinkedLegacyAssets(unlinkedCursor, 50);
      setUnlinked((current) => [...current, ...nextRows]);
      setUnlinkedCursor(nextRows.length === 50 ? nextRows[nextRows.length - 1]?.assetId ?? null : null);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setLoadingMore(false);
    }
  };

  const beginUnlink = async (asset: LegacyAsset) => {
    setBusyAssetId(asset.assetId);
    setError(null);
    try {
      const preview = await prepareLegacyUnlink(asset.assetId);
      setSelection(toSelection(preview));
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusyAssetId(null);
    }
  };

  const beginCopyBack = async (asset: LegacyAsset) => {
    setBusyAssetId(asset.assetId);
    setError(null);
    try {
      const preview = await prepareLegacyCopyBack(asset.assetId);
      if (preview) {
        setSelection(toSelection(preview));
        await refresh();
      }
    } catch (cause) {
      setError(errorMessage(cause));
      await refresh();
    } finally {
      setBusyAssetId(null);
    }
  };

  const showActiveResolution = (asset: LegacyAsset) => {
    if (!asset.activeResolutionId || !asset.activeResolutionAction) return;
    setSelection({
      resolutionId: asset.activeResolutionId,
      asset,
      action: asset.activeResolutionAction,
      matchingAssets: asset.matchingAssets,
    });
  };

  const closePreview = async () => {
    if (!selection) return;
    const matchingItem = page?.items.find((item) => item.activeResolutionId === selection.resolutionId);
    if (matchingItem?.activeResolutionState === "prepared") {
      setBusyResolutionId(selection.resolutionId);
      try {
        await cancelLegacyResolution(selection.resolutionId);
      } catch (cause) {
        setError(errorMessage(cause));
      } finally {
        setBusyResolutionId(null);
      }
    }
    setSelection(null);
    await refresh();
  };

  const confirmSelection = async () => {
    if (!selection) return;
    setBusyResolutionId(selection.resolutionId);
    setError(null);
    setNotice(null);
    try {
      const result = await confirmLegacyResolution(selection.resolutionId);
      if (result.state === "completed") {
        setNotice(`素材 ${result.assetId} 已复制回仓，原记录与整理信息已保留。`);
      } else if (result.state === "unlinked") {
        setNotice(`素材 ${result.assetId} 已从待处理清单解除关联，素材记录及原文件均已保留。`);
      } else if (result.state === "outcome_unknown") {
        setNotice(`素材 ${result.assetId} 的回仓结果需要人工核对；清单保留了处置收据。`);
      }
      setSelection(null);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
      await refresh();
    } finally {
      setBusyResolutionId(null);
    }
  };

  const reopen = async (asset: LegacyAsset) => {
    setBusyAssetId(asset.assetId);
    setError(null);
    try {
      await reopenUnlinkedLegacyAsset(asset.assetId);
      setNotice(`素材 ${asset.assetId} 已重新加入待处理清单。`);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusyAssetId(null);
    }
  };

  /** 本页可勾选的行：已确认在总仓外、且没有进行中的处置。 */
  const selectableIds = useMemo(
    () => (page?.items ?? [])
      .filter((item) => item.status === "external_pending" && item.activeResolutionId === null)
      .map((item) => item.assetId),
    [page],
  );
  const selectedSet = useMemo(() => new Set(selectedIds), [selectedIds]);
  const toggleSelected = (assetId: number) => {
    setSelectedIds((current) => current.includes(assetId)
      ? current.filter((id) => id !== assetId)
      : [...current, assetId]);
  };
  const selectAll = () => setSelectedIds(selectableIds);
  const clearSelection = () => setSelectedIds([]);

  const prepareBatch = async (action: "unlink" | "copy_back") => {
    if (selectedIds.length === 0) return;
    setBatchBusy(true);
    setError(null);
    setNotice(null);
    try {
      const report = action === "unlink"
        ? await prepareLegacyUnlinkBatch(selectedIds)
        : await prepareLegacyCopyBackBatch(selectedIds);
      if (report.cancelled) {
        setNotice("已取消，没有处置任何记录。");
        return;
      }
      setBatchReport(report);
      setSelectedIds([]);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
      await refresh();
    } finally {
      setBatchBusy(false);
    }
  };

  const confirmBatch = async () => {
    const resolutions = (batchReport?.entries ?? [])
      .filter((entry) => entry.state === "prepared" && entry.resolutionId)
      .map((entry) => entry.resolutionId as string);
    if (resolutions.length === 0) {
      setBatchReport(null);
      return;
    }
    setBatchBusy(true);
    setError(null);
    try {
      const report = await confirmLegacyResolutionsBatch(resolutions);
      setBatchReport(report);
      setNotice(batchHeadline(report));
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause));
      await refresh();
    } finally {
      setBatchBusy(false);
    }
  };

  const assetName = (assetId: number) =>
    page?.items.find((item) => item.assetId === assetId)?.fileName
    ?? unlinked.find((item) => item.assetId === assetId)?.fileName
    ?? `素材 ${assetId}`;

  const activeState = selection
    ? page?.items.find((item) => item.activeResolutionId === selection.resolutionId)?.activeResolutionState ?? null
    : null;
  const resolutionRunning = selection !== null && busyResolutionId === selection.resolutionId;
  const lockedUnknown = activeState === "outcome_unknown";
  const confirmLabel = selection?.action === "unlink"
    ? "确认解除关联"
    : activeState === "copying" || activeState === "published"
      ? "继续核验并完成回仓"
      : "确认复制回仓";

  const batchPending = (batchReport?.entries ?? []).filter((entry) => entry.state === "prepared");
  const batchTrouble = batchReport ? batchEntriesNeedingAttention(batchReport) : [];
  const batchTitle = batchReport?.action === "unlink" ? "批量解除关联检查结果" : "批量复制回仓检查结果";

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <p className="text-sm font-medium">历史库外记录</p>
          <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
            显示尚未确认托管归属的旧记录。复制回仓会保留资产 ID 与整理信息；解除关联不会删除原文件。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-[var(--color-text-secondary)]" role="status">
            {page ? `${healthLabel(page.libraryHealth)} · ${page.items.length} 条待处理` : "读取中…"}
          </span>
          <Button disabled={loading} onClick={() => void refresh()}>{loading ? "刷新中…" : "刷新"}</Button>
        </div>
      </div>

      {page?.rootPath && (
        <p className="break-all text-xs text-[var(--color-text-secondary)]" title={page.rootPath}>
          当前总仓：{page.rootPath}
        </p>
      )}

      {page && !page.canResolve && (
        <div
          role="alert"
          data-testid="legacy-offline-hint"
          className="space-y-2 rounded-[var(--radius-control)] border border-[var(--color-border)] p-3"
        >
          <p className="text-sm font-medium">{healthLabel(page.libraryHealth)}：逐项处置已暂停</p>
          <p className="text-xs text-[var(--color-text-secondary)]">{offlineHint(page.libraryHealth)}</p>
          {onRequestRelocation && (
            <Button onClick={onRequestRelocation}>打开「总仓位置」向导…</Button>
          )}
        </div>
      )}

      {error && <p role="alert" className="text-sm text-[var(--color-danger)]">{error}</p>}
      {notice && <p role="status" className="text-sm text-[var(--color-status)]">{notice}</p>}

      {selectableIds.length > 0 && page?.canResolve && (
        <div className="flex flex-wrap items-center gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] p-3">
          <span className="text-xs text-[var(--color-text-secondary)]">
            已选 {selectedIds.length} 条 · 本页可处理 {selectableIds.length} 条
          </span>
          <Button disabled={batchBusy} onClick={selectAll}>全选本页</Button>
          <Button disabled={batchBusy || selectedIds.length === 0} onClick={clearSelection}>清除选择</Button>
          <Button
            disabled={batchBusy || selectedIds.length === 0}
            onClick={() => void prepareBatch("copy_back")}
          >批量复制回仓…</Button>
          <Button
            disabled={batchBusy || selectedIds.length === 0}
            onClick={() => void prepareBatch("unlink")}
          >批量解除关联</Button>
        </div>
      )}

      {loading && !page ? (
        <p className="text-sm text-[var(--color-text-secondary)]">正在读取待处理记录…</p>
      ) : page?.items.length ? (
        <div className="divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
          {page.items.map((asset) => {
            const hasActive = asset.activeResolutionId !== null;
            const canResume = hasActive && ["prepared", "copying", "published"].includes(asset.activeResolutionState ?? "");
            const selectable = page.canResolve && asset.status === "external_pending" && !hasActive;
            return (
              <div key={asset.assetId} className="flex flex-wrap items-center justify-between gap-3 p-3">
                <div className="flex min-w-0 flex-1 items-start gap-2">
                  {selectable && (
                    <input
                      type="checkbox"
                      className="mt-1"
                      aria-label={`选择 ${asset.fileName}`}
                      checked={selectedSet.has(asset.assetId)}
                      onChange={() => toggleSelected(asset.assetId)}
                    />
                  )}
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium" title={asset.fileName}>{asset.fileName}</p>
                    <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
                      ID {asset.assetId} · {statusLabel(asset.status)} · {sizeLabel(asset.fileSize)} · 评分 {asset.rating} · {asset.favorite ? "已收藏" : "未收藏"}
                    </p>
                    {asset.activeResolutionState && (
                      <p className="mt-1 text-xs text-[var(--color-text-secondary)]">
                        处置状态：{legacyOutcomeLabel(asset.activeResolutionState)}
                        {asset.resolutionErrorCode ? `（${asset.resolutionErrorCode}）` : ""}
                      </p>
                    )}
                    {asset.status !== "external_pending" && (
                      <p className="mt-1 text-xs text-[var(--color-text-secondary)]">归属未确认前，不能复制回仓或解除关联。</p>
                    )}
                  </div>
                </div>
                <div className="flex flex-wrap gap-1">
                  {canResume && asset.activeResolutionId && (
                    <Button
                      disabled={!page.canResolve || busyAssetId !== null || busyResolutionId !== null}
                      onClick={() => showActiveResolution(asset)}
                    >
                      {asset.activeResolutionState === "prepared" ? "查看并继续" : "恢复处置"}
                    </Button>
                  )}
                  {asset.status === "external_pending" && !hasActive && (
                    <>
                      <Button
                        disabled={!page.canResolve || busyAssetId !== null || busyResolutionId !== null}
                        onClick={() => void beginCopyBack(asset)}
                      >
                        {busyAssetId === asset.assetId ? "检查中…" : "复制回仓"}
                      </Button>
                      <Button
                        disabled={!page.canResolve || busyAssetId !== null || busyResolutionId !== null}
                        onClick={() => void beginUnlink(asset)}
                      >解除关联</Button>
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <p className="rounded-[var(--radius-control)] border border-[var(--color-border)] p-3 text-sm text-[var(--color-text-secondary)]">
          当前没有待处理的历史库外记录。
        </p>
      )}
      {page?.nextAfterAssetId !== null && page?.nextAfterAssetId !== undefined && (
        <Button disabled={loadingMore} onClick={() => void loadMore()}>{loadingMore ? "读取中…" : "加载更多"}</Button>
      )}

      <div className="border-t border-[var(--color-border)] pt-4">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <div>
            <p className="text-sm font-medium">已解除关联</p>
            <p className="mt-1 text-xs text-[var(--color-text-secondary)]">这些记录仍保留在数据库中，可以重新加入待处理清单。</p>
          </div>
          <span className="text-xs text-[var(--color-text-secondary)]">{unlinked.length} 条</span>
        </div>
        {unlinked.length > 0 ? (
          <div className="divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
            {unlinked.map((asset) => (
              <div key={asset.assetId} className="flex flex-wrap items-center justify-between gap-3 p-3">
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium" title={asset.fileName}>{asset.fileName}</p>
                  <p className="mt-1 text-xs text-[var(--color-text-secondary)]">ID {asset.assetId} · {sizeLabel(asset.fileSize)}</p>
                </div>
                <Button
                  disabled={!page?.canResolve || busyAssetId !== null || busyResolutionId !== null}
                  onClick={() => void reopen(asset)}
                >重新打开待处理</Button>
              </div>
            ))}
          </div>
        ) : (
          <p className="text-xs text-[var(--color-text-secondary)]">暂无已解除关联记录。</p>
        )}
        {unlinkedCursor !== null && (
          <div className="mt-2">
            <Button disabled={loadingMore} onClick={() => void loadMoreUnlinked()}>{loadingMore ? "读取中…" : "加载更多已解除记录"}</Button>
          </div>
        )}
      </div>

      <Modal
        open={batchReport !== null}
        title={batchTitle}
        onClose={() => { if (!batchBusy) setBatchReport(null); }}
        wide={batchReport !== null ? true : undefined}
        footer={(
          <>
            <Button disabled={batchBusy} onClick={() => setBatchReport(null)}>关闭</Button>
            <Button
              variant={batchReport?.action === "unlink" ? "danger" : "primary"}
              disabled={batchBusy || batchPending.length === 0}
              onClick={() => void confirmBatch()}
            >
              {batchBusy
                ? "逐条处置中…"
                : batchPending.length === 0
                  ? "没有可确认的条目"
                  : `确认处置这 ${batchPending.length} 条`}
            </Button>
          </>
        )}
      >
        {batchReport && (
          <div className="space-y-3">
            <p className="text-[var(--color-text-secondary)]">{batchHeadline(batchReport)}</p>
            <p className="text-xs text-[var(--color-text-secondary)]">
              每条都有自己的收据；确认后逐条复核源文件，任何一条结果不明都会单独标出，不会连带其他记录。
            </p>
            <ul className="space-y-2" data-testid="legacy-batch-entries">
              {batchReport.entries.map((entry) => (
                <li key={`${entry.assetId}-${entry.state}`} className="rounded border border-[var(--color-border)] p-2">
                  <p className="text-sm">
                    {assetName(entry.assetId)} · ID {entry.assetId} · {legacyOutcomeLabel(entry.state)}
                  </p>
                  {entry.message && (
                    <p className="mt-1 text-xs text-[var(--color-text-secondary)]">{entry.message}</p>
                  )}
                  {entry.errorCode && (
                    <p className="mt-1 text-xs text-[var(--color-text-secondary)]">错误码：{entry.errorCode}</p>
                  )}
                </li>
              ))}
            </ul>
            {batchReport.action === "copy_back" && batchPending.some((entry) => entry.message) && (
              <p className="text-xs text-[var(--color-text-secondary)]">
                上面标出的同哈希情况一律按独立记录复制：本次不会合并标签，也不会走普通重复项跳过流程。
              </p>
            )}
            {batchTrouble.length > 0 && (
              <p role="alert" className="text-sm text-[var(--color-danger)]">
                其中 {batchTrouble.length} 条需要先处理：{batchTrouble.map((entry) => assetName(entry.assetId)).join("、")}。
              </p>
            )}
          </div>
        )}
      </Modal>

      <Modal
        open={selection !== null}
        title={selection?.action === "unlink" ? "确认解除旧记录关联" : "确认复制旧记录回仓"}
        onClose={() => { if (!resolutionRunning) void closePreview(); }}
        wide={selection?.matchingAssets.length ? true : undefined}
        footer={(
          <>
            <Button disabled={resolutionRunning} onClick={() => void closePreview()}>取消</Button>
            <Button
              variant={selection?.action === "unlink" ? "danger" : "primary"}
              disabled={resolutionRunning || lockedUnknown || !page?.canResolve}
              onClick={() => void confirmSelection()}
            >
              {resolutionRunning ? "正在核验…" : lockedUnknown ? "结果待人工核对" : confirmLabel}
            </Button>
          </>
        )}
      >
        {selection && (
          <div className="space-y-3">
            <p className="font-medium">{selection.asset.fileName} · ID {selection.asset.assetId}</p>
            {selection.action === "unlink" ? (
              <p className="text-[var(--color-text-secondary)]">
                确认后只会从待处理清单解除管理关系。不会删除或改写库外原文件、素材记录、标签、收藏、评分或其他关系；之后可从“已解除关联”重新打开。
              </p>
            ) : (
              <p className="text-[var(--color-text-secondary)]">
                将原文件复制到当前总仓，并把这条记录切换到新副本。源文件保持不变；现有素材 ID、标签、收藏、评分和关系会保留。
              </p>
            )}
            {selection.matchingAssets.length > 0 && (
              <div className="space-y-2 rounded-[var(--radius-control)] border border-[var(--color-border)] p-3">
                <p className="font-medium">总仓中存在 {selection.matchingAssets.length} 条完整 SHA-256 相同的素材</p>
                <p className="text-xs text-[var(--color-text-secondary)]">
                  本次仍保留为独立记录，不合并标签，也不会走普通重复项跳过流程。
                </p>
                <ul className="max-h-48 space-y-2 overflow-y-auto">
                  {selection.matchingAssets.map((match) => (
                    <li key={match.assetId} className="rounded border border-[var(--color-border)] p-2">
                      <p className="text-sm">ID {match.assetId} · {match.fileName} · 评分 {match.rating} · {match.favorite ? "已收藏" : "未收藏"}</p>
                      <p className="mt-1 break-words text-xs text-[var(--color-text-secondary)]">
                        标签：{match.tags.length ? match.tags.join("、") : "无"}
                      </p>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {activeState === "copying" || activeState === "published" ? (
              <p className="text-xs text-[var(--color-text-secondary)]">
                已找到持久处置收据。继续前会重新核验源文件、目标文件身份和完整摘要；无法证明归属时会保持待处理。
              </p>
            ) : null}
            {lockedUnknown && (
              <p role="alert" className="text-[var(--color-danger)]">
                文件或收据无法证明当前结果。应用不会覆盖或自动认领目标文件，请先人工核对清单中的收据状态。
              </p>
            )}
          </div>
        )}
      </Modal>
    </div>
  );
}
