/** AI 打标页（PRD v2.5 工作台 2.0）：左数据栏 + 右侧四段式（大图/EXIF 行/胶片条/分类标签面板）
 *  FB6 需求一：页内进度唯一化——「当前批次」区块的 AiTaggingProgress 是唯一 AI 进度 UI；
 *  全局任务条中的「AI 打标中」胶囊已从 taskStore 移除（taskStore 只订阅入库/导出）。 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { useShallow } from "zustand/react/shallow";
import Button from "@/components/common/Button";
import AiTaggingProgress, { assetFileLabel } from "@/components/ai/AiTaggingProgress";
import SuggestionReview from "@/components/ai/SuggestionReview";
import { aiPrepareBatch, aiSubmitTaggingPlan, onAiProgress } from "@/api/ai";
import { approvePlan } from "@/api/plans";
import type { PlanPreview } from "@/types/plans";
import type { TaggingOperation } from "@/types/ai";
import {
  getAiUsageBindings,
  listAiConnections,
  setAiUsageBinding,
  type AiConnection,
} from "@/api/connections";
import { recentTagOps, undoTagBatch } from "@/api/tags";
import { useTauriEvent } from "@/hooks/hooks";
import { useAiStore } from "@/stores/aiStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useTagStore } from "@/stores/tagStore";
import { computeAiStats, estimateRequests } from "@/utils/aiStats";
import type { AiTaggingUiState } from "@/types/ai";
import type { TagOp } from "@/types/asset";

export default function AiTaggingPage() {
  const {
    batches, currentBatchId, suggestions, running, cancelling, error, pendingAssetIds, lastProgressAssetId, runProgress,
  } = useAiStore(
    useShallow((s) => ({
      batches: s.batches,
      currentBatchId: s.currentBatchId,
      suggestions: s.suggestions,
      running: s.running,
      cancelling: s.cancelling,
      error: s.error,
      pendingAssetIds: s.pendingAssetIds,
      lastProgressAssetId: s.lastProgressAssetId,
      runProgress: s.runProgress,
    })),
  );
  const {
    refreshBatches, openBatch, createBatch, startTagging, setError, cancel, confirmAll, patchProgress,
  } = useAiStore(
    useShallow((s) => ({
      refreshBatches: s.refreshBatches,
      openBatch: s.openBatch,
      createBatch: s.createBatch,
      startTagging: s.startTagging,
      setError: s.setError,
      cancel: s.cancel,
      confirmAll: s.confirmAll,
      patchProgress: s.patchProgress,
    })),
  );
  const refreshLibrary = useLibraryStore((s) => s.refresh);
  const tagFacets = useTagStore((s) => s.facets);
  const refreshTags = useTagStore((s) => s.refresh);
  const { settings, loaded, load } = useSettingsStore(
    useShallow((s) => ({ settings: s.settings, loaded: s.loaded, load: s.load })),
  );

  useEffect(() => {
    if (tagFacets.length === 0) void refreshTags();
  }, [tagFacets.length, refreshTags]);

  // 打标范围（v2.11）：全部 / 仅前 N 张
  const [scopeAll, setScopeAll] = useState(true);
  const [scopeN, setScopeN] = useState("10");
  const [preparingTaggingPlan, setPreparingTaggingPlan] = useState(false);

  useEffect(() => {
    if (!loaded) void load();
  }, [loaded, load]);

  // 素材库选好跳过来 → 只自动建批次展示图片，AI 由左栏「开始打标」手动启动。
  useEffect(() => {
    if (pendingAssetIds.length > 0 && !running) void createBatch();
  }, [pendingAssetIds, running, createBatch]);

  // 当前模型以 ai_connections + tagging 用途绑定为唯一数据源；
  // settings.ai.profiles 只是 V15 迁移前的历史输入，不再参与打标页展示或切换。
  const [aiConnections, setAiConnections] = useState<AiConnection[]>([]);
  const [taggingConnectionId, setTaggingConnectionId] = useState<string | null>(null);
  const [connectionsLoaded, setConnectionsLoaded] = useState(false);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [bindingSaving, setBindingSaving] = useState(false);
  const activeConnection =
    aiConnections.find((connection) => connection.id === taggingConnectionId) ?? null;

  const loadTaggingConnection = useCallback(async () => {
    setConnectionsLoaded(false);
    setConnectionError(null);
    try {
      const [connections, bindings] = await Promise.all([
        listAiConnections(),
        getAiUsageBindings(),
      ]);
      setAiConnections(connections);
      setTaggingConnectionId(bindings.tagging);
    } catch (e) {
      setConnectionError(e instanceof Error ? e.message : String(e));
    } finally {
      setConnectionsLoaded(true);
    }
  }, []);

  useEffect(() => {
    void loadTaggingConnection();
  }, [loadTaggingConnection]);

  // 页面内只切换“此功能使用的服务”，服务名称/地址/模型/密钥仍在设置页统一维护。
  const switchConnection = useCallback(async (id: string) => {
    if (!id) return;
    setBindingSaving(true);
    setConnectionError(null);
    try {
      await setAiUsageBinding("tagging", id);
      setTaggingConnectionId(id);
    } catch (e) {
      setConnectionError(e instanceof Error ? e.message : String(e));
    } finally {
      setBindingSaving(false);
    }
  }, []);

  const [reviewIdx, setReviewIdx] = useState(0);

  useEffect(() => {
    void refreshBatches();
  }, [refreshBatches]);

  // 进度事件只有一个订阅入口；按 batchId 归属本次运行，运行计数不覆盖批次累计值。
  useTauriEvent(
    () =>
      onAiProgress((p) => {
        patchProgress(p);
      }),
    [],
  );

  const current = batches.find((b) => b.id === currentBatchId) ?? null;
  // FB6 需求一：派生页内进度 UI 状态（AiTaggingUiState）。
  // 收尾快照只在 running 翻转为 false 的一刻记录（完成/取消/失败显示静态最终状态，不继续滚动）。
  const [aiFinal, setAiFinal] = useState<{ status: string | null; error: string | null; processed: number; total: number } | null>(null);
  const prevRunning = useRef(false);
  useEffect(() => {
    const completedRun = runProgress?.batchId === currentBatchId ? runProgress : null;
    if (prevRunning.current && !running && current && completedRun) {
      setAiFinal({
        status: current.status,
        error,
        processed: completedRun.processed,
        total: completedRun.total,
      });
    }
    prevRunning.current = running;
    // 收尾快照取本次执行计数；批次总量另行展示。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);
  // 切批次：清空上一次的收尾快照，避免旧批次的终态串台。
  useEffect(() => {
    setAiFinal(null);
  }, [currentBatchId]);

  const selectedRun = runProgress?.batchId === currentBatchId ? runProgress : null;
  const runProcessed = selectedRun?.processed ?? 0;
  const runTotal = selectedRun?.total ?? 0;
  const aiState: AiTaggingUiState = running
    ? cancelling
      ? { phase: "cancelling", processed: runProcessed, total: runTotal }
      : runTotal > 0
        ? { phase: "running", processed: runProcessed, total: runTotal, currentAssetId: lastProgressAssetId ?? undefined }
        : { phase: "starting", total: 0 }
    : aiFinal?.error
      ? { phase: "error", message: aiFinal.error, processed: aiFinal.processed, total: aiFinal.total }
      : aiFinal && (aiFinal.status === "done" || aiFinal.status === "cancelled")
        ? { phase: "done", processed: aiFinal.processed, total: aiFinal.total }
        : { phase: "idle" };

  /** LED 提示的当前素材名：按 lastProgressAssetId 从 suggestions 查；找不到只显示计数 */
  const aiCurrentName = useMemo(() => {
    if (lastProgressAssetId == null) return null;
    const hit = suggestions.find((s) => s.assetId === lastProgressAssetId);
    return hit ? assetFileLabel(hit.assetPath) : null;
  }, [suggestions, lastProgressAssetId]);

  // B-3：当前批次是否含视频 + 视频 AI 打标是否开启（前端提示，后端仍保留最终校验）
  const batchHasVideo = useMemo(
    () => suggestions.some((s) => (s.mimeType ?? "").startsWith("video/") || s.assetPath.match(/\.(mp4|mov|avi|mkv|webm|m4v)$/i) != null),
    [suggestions],
  );
  const videoTaggingOn = settings?.ai.videoTagging ?? false;
  // 视频模式只在设置页维护；这里读取已保存值，用于请求次数预估和后端执行前提示。
  const videoMode = settings?.ai.videoTaggingMode === "frames" ? "frames" : "cover";
  const videoFrameCount = settings?.ai.videoFrameCount ?? 3;
  // FB-03 §9.5：区分「设置未加载」与「真未开启」，避免加载失败误报
  const settingsUnloaded = settings === null;
  // B-1 批次统计语义：待生成 / 待确认 / 已确认 / 失败 分开，不再用「处理中」混淆多种状态。
  // 「全部确认」必须用「待确认建议」数量（computeAiStats.awaitingConfirmation），不能用待生成数。
  const stats = useMemo(() => computeAiStats(suggestions), [suggestions]);

  const startBatch = useCallback(async (limit?: number) => {
    if (currentBatchId == null || running || preparingTaggingPlan) return;
    setPreparingTaggingPlan(true);
    setError(null);
    try {
      const preparation = await aiPrepareBatch(currentBatchId, limit);
      let operation: TaggingOperation | null = preparation.operation;
      if (preparation.requiresApproval) {
        const preview: PlanPreview | null = preparation.preview;
        if (!preview) throw new Error("AI 打标计划缺少审批预览");
        if (!window.confirm(`${preparation.summary}\n\n批准后将创建待启动操作。联网只会在你确认此窗口后开始。`)) {
          return;
        }
        const approval = await approvePlan(preview.planId, preview.reviewRef);
        if (approval.reviewStatus !== "approved") {
          throw new Error("AI 打标计划未获批准");
        }
        operation = await aiSubmitTaggingPlan(
          preview.planId,
          `tagging_submit_${preview.planId}`,
        );
      }
      if (!operation) throw new Error("AI 打标计划未生成可启动操作");
      await startTagging(operation, limit);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPreparingTaggingPlan(false);
    }
  }, [currentBatchId, preparingTaggingPlan, running, setError, startTagging]);
  // 过片走全量建议（胶片条含已确认/已拒绝，状态角标区分）。
  // 审核交互（标签编辑、批量确认、数值项）已抽到 SuggestionReview，与助手右栏共用。
  const goto = useCallback(
    (next: number) => setReviewIdx(Math.max(0, Math.min(next, suggestions.length - 1))),
    [suggestions.length],
  );

  const onConfirmAll = async () => {
    await confirmAll();
    await Promise.all([refreshLibrary(), refreshTags()]);
  };

  // B-3：视频批次但视频打标未开启时跳转到设置页开启
  const openSettings = useCallback(() => {
    window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }));
  }, []);

  // R-25 最近打标流水 + 批次撤销（两击确认防误触）
  const [recentOps, setRecentOps] = useState<TagOp[]>([]);
  const [undoArmed, setUndoArmed] = useState<number | null>(null);
  const loadRecent = useCallback(async () => {
    try {
      setRecentOps(await recentTagOps(50));
    } catch {
      /* 流水拉取失败不阻塞主流程 */
    }
  }, []);
  useEffect(() => {
    void loadRecent();
  }, [loadRecent]);

  const afterWrite = async () => {
    await Promise.all([refreshLibrary(), refreshTags()]);
    void loadRecent(); // R-25：确认后刷新最近打标流水
  };

  const undoBatch = async (batchId: number) => {
    if (undoArmed !== batchId) {
      setUndoArmed(batchId);
      return;
    }
    setUndoArmed(null);
    await undoTagBatch(batchId);
    await Promise.all([refreshLibrary(), refreshTags()]);
    await loadRecent();
  };



  return (
    <div className="flex h-full bg-[var(--color-bg)]">
      {/* 左侧数据栏 */}
      {/* 左侧数据栏。pb-32 = 底部安全区（M-11 第五次真机补证：占位高度 = 底栏 56 + 入口 48 + 间距 8 = 112，原 pb-32=96 在 1200×800/DPI 下仍差 20px，打标页最后一行实测被压 108×28px²，故抬到 pb-32=128）：收起态悬浮助手入口固定在左下，
          会盖住历史批次列表最底部约 65px 的可点行。安全区放在 aside 上而不是滚动内容里，
          这样任何滚动位置（含默认位置）都不会有行落到入口底下。 */}
      <aside
        className="flex w-[252px] shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-bg)] pb-32"
        data-testid="tagging-side-panel"
      >
        {/* M-11 真机补证（960×640 打开批次）：aside 的 pb-32 只在内容不溢出时才等于「安全区」。
            打开批次后这一整块（服务信息 + 当前批次 + 统计 + 提示）高达约 690px，超过
            「左栏高度 − 安全区」的可用高度，flex 子项会直接顶穿 padding 落到悬浮入口底下。
            所以非滚动的顶部区块本身必须可收缩、可滚动，才能保证不越过安全区。 */}
        <div className="min-h-0 shrink overflow-y-auto" data-testid="tagging-side-header">
        <div className="border-b border-[var(--color-border)] px-4 py-3.5">
          <div className="flex min-w-0 items-center gap-2" aria-live="polite">
            <span
              className={clsx(
                "size-1.5 shrink-0 rounded-full",
                connectionsLoaded && activeConnection
                  ? "bg-[var(--color-success)]"
                  : "bg-[var(--color-border-strong)]",
              )}
              aria-hidden="true"
            />
            <span className="min-w-0 flex-1 truncate text-xs font-medium text-[var(--color-text)]">
              {!connectionsLoaded
                ? "正在读取打标服务…"
                : activeConnection
                  ? activeConnection.name || "未命名服务"
                  : "未选择打标服务"}
            </span>
            {activeConnection && (
              <span className="shrink-0 text-[10px] text-[var(--color-text-tertiary)]">
                {activeConnection.deployment === "local" ? "本机" : "在线"}
              </span>
            )}
          </div>
          {/* R-44：打标工作台不区分 AI/手动模式。没有打标服务不是错误状态
              （ARCHITECTURE §663/§677），手工填写这条路必须照常可用，所以文案不能只说"去设置"。 */}
          {connectionsLoaded && !activeConnection && (
            <p
              className="mt-1 pl-3.5 text-[10px] leading-4 text-[var(--color-text-secondary)]"
              data-testid="tagging-no-service-hint"
            >
              还没选打标服务，可以先直接手工填写标签；要生成 AI 建议再
              <button
                type="button"
                onClick={openSettings}
                className="mx-0.5 text-[var(--color-accent)] underline decoration-dotted"
              >
                去设置选择
              </button>
              。
            </p>
          )}
          {/* AI 批次启动区：不在运行中就常显「开始打标」（打完也保留，可续跑剩余 pending；v2.12） */}
          {current && !running && (
            <div className="mt-3 flex flex-col gap-2">
              <label className="flex min-h-7 cursor-pointer items-center gap-2 text-xs text-[var(--color-text-secondary)]">
                <input
                  type="radio"
                  checked={scopeAll}
                  onChange={() => setScopeAll(true)}
                  className="shrink-0 accent-[var(--color-accent)]"
                />
                打标全部（{current.total} 张）
              </label>
              <label className="flex min-h-7 cursor-pointer items-center gap-2 text-xs text-[var(--color-text-secondary)]">
                <input
                  type="radio"
                  checked={!scopeAll}
                  onChange={() => setScopeAll(false)}
                  className="shrink-0 accent-[var(--color-accent)]"
                />
                仅打标前
                <input
                  value={scopeN}
                  onChange={(e) => {
                    setScopeN(e.target.value.replace(/\D/g, ""));
                    setScopeAll(false);
                  }}
                  onFocus={() => setScopeAll(false)}
                  inputMode="numeric"
                  className="ui-control w-14 px-1.5 py-1 text-center text-xs"
                />
                张
              </label>
              {/* FB2-07（§13.6）：成本前置告知 —— 预估请求次数随批次规模/模式/帧数实时变化 */}
              <p className="text-[10px] leading-4 text-[var(--color-text-secondary)]">
                本批次约 {estimateRequests(stats, videoMode, videoFrameCount)} 次请求
                {stats.videoCount > 0 && `（含 ${stats.videoCount} 个视频 × ${videoMode === "frames" ? videoFrameCount : "封面"}）`}
              </p>
              <Button
                variant="primary"
                className="mt-1 w-full"
                disabled={
                  preparingTaggingPlan || running || (!scopeAll && (!scopeN || parseInt(scopeN, 10) < 1)) || (batchHasVideo && !videoTaggingOn)
                }
                onClick={() => {
                  // M-07/PRD R-44：没有打标服务时 AI 入口要「先去设置」，而不是点下去报一个红字错误。
                  if (connectionsLoaded && !activeConnection) {
                    openSettings();
                    return;
                  }
                  void startBatch(scopeAll ? undefined : parseInt(scopeN, 10));
                }}
              >
                {preparingTaggingPlan
                  ? "正在准备打标计划…"
                  : batchHasVideo && !videoTaggingOn
                    ? "先开启视频 AI 打标"
                    : connectionsLoaded && !activeConnection
                      ? "先去设置选择打标服务"
                      : "开始打标"}
              </Button>
              {/* R35：批准前就要说清楚发的是什么画质。默认只发压缩预览，
                  想要更高清必须另行确认——不能等用户花完钱才发现发的是缩略图，
                  也不能偷偷按原始尺寸发出去。 */}
              <p
                className="mt-1 text-[10px] leading-4 text-[var(--color-text-tertiary)]"
                data-testid="tagging-egress-clarity"
              >
                外发画质：图片统一转成 JPEG 预览，最长边 1024；需要更高清的画面会另行提示并单独确认，不会自动改尺寸发送。
              </p>
            </div>
          )}
          {running && (
            <Button
              className="mt-3 w-full"
              disabled={cancelling}
              onClick={() => void cancel()}
            >
              {cancelling ? "已请求取消…" : "取消"}
            </Button>
          )}
          <p className="mt-3 text-[11px] leading-4 text-[var(--color-text-tertiary)]">
            {running
              ? cancelling
                ? "取消已受理，当前图片完成后停止" // P2-01：300s 单请求超时不可打断，诚实告知
                : "正在生成标签建议…"
              : current
                ? current?.status === "pending"
                  ? "批次已就绪，图片已载入——点「开始打标」生成建议，也可直接手工填写"
                  : "点「开始打标」可继续处理剩余未打标项"
                : "在素材库选中素材后，点顶栏「打标」直达本页"}
          </p>
        </div>

        <div className="border-b border-[var(--color-border)] px-4 py-3.5">
          <h3 className="ui-section-title mb-2.5">当前模型</h3>
          {!connectionsLoaded ? (
            <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
              正在读取打标服务…
            </p>
          ) : aiConnections.length === 0 ? (
            <div className="flex items-center gap-2">
              <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
                还没有 AI 服务，去「设置 → 服务管理」添加
              </p>
              <button
                type="button"
                onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}
                className="rounded-md bg-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent-text)]"
              >
                去设置添加
              </button>
            </div>
          ) : (
            <div className="flex flex-col gap-2">
              <select
                aria-label="AI 打标服务"
                value={taggingConnectionId ?? ""}
                disabled={bindingSaving}
                onChange={(e) => void switchConnection(e.target.value)}
                className="ui-control w-full px-3 py-2 text-sm"
              >
                <option value="" disabled>请选择服务</option>
                {aiConnections.map((connection) => (
                  <option key={connection.id} value={connection.id}>
                    {connection.name || "未命名"}
                  </option>
                ))}
              </select>
              <div className="min-w-0 text-[11px] leading-4 text-[var(--color-text-secondary)]">
                {activeConnection ? (
                  <>
                    <div className="truncate" title={activeConnection.baseUrl}>
                      {activeConnection.deployment === "local" ? "本机服务" : "在线服务"} · {activeConnection.baseUrl}
                    </div>
                    <div className="truncate" title={activeConnection.model}>
                      模型：{activeConnection.model || "未设置"}
                    </div>
                  </>
                ) : (
                  <span>当前用途尚未绑定服务，请选择一个服务。</span>
                )}
              </div>
            </div>
          )}
        </div>

        {current && (
          <div className="border-b border-[var(--color-border)] px-4 py-3.5">
            <div className="mb-2.5 flex items-center justify-between">
              <h3 className="ui-section-title">当前批次</h3>
              <span className="text-[11px] text-[var(--color-text-tertiary)]">#{current.id} · 共 {suggestions.length} 张</span>
            </div>
            {/* B-3：批次含视频但视频打标未开启——明确提示并提供打开设置入口（前端预检 §9.3；后端保留最终校验） */}
            {batchHasVideo && !videoTaggingOn && !running && (
              <div className="mb-2 flex items-center gap-2 rounded-md border border-[var(--color-status)] bg-[var(--color-status-soft)] px-2.5 py-2 text-xs">
                <span className="min-w-0 flex-1 leading-4 text-[var(--color-status)]">
                  {settingsUnloaded
                    ? "设置尚未加载，无法判断「视频 AI 打标」是否开启；请稍后重试或先到设置页保存配置。"
                    : "视频 AI 打标未开启。请打开\"设置 → AI 设置 → 自动打标 → 视频 AI 打标\"，保存后重新开始批次。"}
                </span>
                <button
                  onClick={openSettings}
                  className="shrink-0 rounded-md border border-[var(--color-status)] px-2 py-1 font-medium text-[var(--color-status)] transition-colors hover:bg-[var(--color-status-soft)]"
                >
                  打开设置
                </button>
              </div>
            )}
            <div className="grid grid-cols-5 gap-1 rounded-[var(--radius-control)] bg-[var(--color-surface)] p-2 text-center">
              <div><strong className="block text-base font-medium text-[var(--color-text)]">{stats.total}</strong><span className="text-[10px] text-[var(--color-text-secondary)]">总数</span></div>
              <div><strong className="block text-base font-medium text-[var(--color-text-secondary)]">{stats.awaitingGeneration}</strong><span className="text-[10px] text-[var(--color-text-secondary)]">待生成</span></div>
              <div><strong className="block text-base font-medium text-[var(--color-status)]">{stats.awaitingConfirmation}</strong><span className="text-[10px] text-[var(--color-text-secondary)]">待确认</span></div>
              <div><strong className="block text-base font-medium text-[var(--color-text)]">{stats.confirmed}</strong><span className="text-[10px] text-[var(--color-text-secondary)]">已确认</span></div>
              <div><strong className="block text-base font-medium text-[var(--color-danger)]">{stats.failed}</strong><span className="text-[10px] text-[var(--color-text-secondary)]">失败</span></div>
            </div>
            {/* FB6 需求一：唯一页内进度条 + LED 滚动提示（starting 立即出现，不等后端事件） */}
            {aiState.phase !== "idle" && (
              <div className="mt-2">
                <AiTaggingProgress state={aiState} currentAssetName={aiCurrentName} />
                {current && selectedRun && (
                  <p className="mt-1 text-[10px] text-[var(--color-text-secondary)]">
                    {runTotal > 0
                      ? `本次执行 ${runProcessed}/${runTotal} · 批次已处理 ${Math.min(selectedRun.batchProcessedAtStart + runProcessed, current.total)}/${current.total}`
                      : `正在准备本次任务${selectedRun.requestedLimit == null ? "" : `（最多 ${selectedRun.requestedLimit} 张）`} · 批次范围 ${current.total} 张`}
                  </p>
                )}
              </div>
            )}
            {stats.awaitingConfirmation > 0 && !running && (
              <Button className="mt-2 w-full" variant="primary" onClick={() => void onConfirmAll()}>
                全部确认（{stats.awaitingConfirmation}）
              </Button>
            )}
            {/* M-04 真机补证：二次启动一定确认不了任何东西（后端只确认 pending），
                此时按「已确认 N」如实说清楚，而不是让用户对着一个点了没反应的「开始打标」猜。 */}
            {!running
              && !preparingTaggingPlan
              && stats.awaitingGeneration === 0
              && stats.awaitingConfirmation === 0
              && stats.failed === 0
              && stats.confirmed === stats.total
              && stats.total > 0
              && (
                <p className="mt-2 text-[10px] leading-4 text-[var(--color-text-tertiary)]" data-testid="tagging-all-reviewed">
                  本批次 {stats.total} 张都已确认写入，再点「开始打标」不会改动任何素材；要重新打标请先在素材库重新挑图。
                </p>
              )}
            {!running && stats.awaitingConfirmation === 0 && stats.awaitingGeneration === 0 && stats.failed === 0 && stats.total > 0 && stats.confirmed !== stats.total && (
              <p className="mt-2 text-[10px] leading-4 text-[var(--color-text-tertiary)]">
                当前没有待确认的建议；可逐张编辑标签后「确认写入」。
              </p>
            )}
          </div>
        )}
        </div>

        {(batches.length > 0 || recentOps.length > 0) && (
          // 底部安全区由 aside（tagging-side-panel）的 pb-32 承担：放在这里只会让
          // 「滚到最底」时多出一段空白，默认滚动位置的行仍会被入口压住（M-11 真机复验）。
          <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2.5" data-testid="tagging-batch-scroll">
            {batches.length > 0 && (
              <>
                <h3 className="ui-section-title mb-2 px-2">
                  历史批次
                </h3>
                {batches.map((b) => {
                  const statusLabel: Record<string, string> = {
                    pending: "待执行", processing: "进行中", done: "已完成", cancelled: "已取消", interrupted: "已中断", undone: "已撤销",
                  };
                  const canResume = b.status === "interrupted" || (b.status !== "processing" && b.status !== "undone" && b.processed < b.total);
                  // D-4：只有 done/cancelled 且存在可撤销写入的批次显示「撤销」；undone 不再显示
                  const canUndo = (b.status === "done" || b.status === "cancelled") && b.confirmed > 0;
                  return (
                  <div key={b.id} className="flex items-center gap-1">
                    <button
                      onClick={() => {
                        setReviewIdx(0);
                        void openBatch(b.id);
                      }}
                      data-active={b.id === currentBatchId}
                      className="ui-nav-item min-w-0 flex-1 px-2.5 py-2 text-left text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
                    >
                      #{b.id} · {b.confirmed}/{b.total}
                      <span className="mt-0.5 flex items-center gap-1.5 text-[10px] text-[var(--color-text-tertiary)]">
                        {new Date(b.createdAt).toLocaleDateString()}
                        <span className={clsx(
                          "rounded px-1 py-0.5 text-[9px]",
                          b.status === "interrupted"
                            ? "bg-[var(--color-status-soft)] text-[var(--color-status)]"
                            : "bg-[var(--color-surface)] text-[var(--color-text-tertiary)]",
                        )}>
                          {statusLabel[b.status] ?? b.status}
                        </span>
                      </span>
                    </button>
                    {canResume && !running && !preparingTaggingPlan && (
                      <button
                        onClick={() => void startBatch()}
                        title="继续执行剩余未打标项"
                        className="shrink-0 rounded px-1.5 py-1 text-[10px] font-medium text-[var(--color-status)] transition-colors hover:bg-[var(--color-surface)]"
                      >
                        继续
                      </button>
                    )}
                    {/* R-25：AI 批次撤销（两击确认；D-4：undone 不再显示可点击撤销） */}
                    {canUndo && (
                      <button
                        onClick={() => void undoBatch(b.id)}
                        title="撤销本批次已确认的标签（再点一次确认）"
                        className={clsx(
                          "shrink-0 rounded px-1.5 py-1 text-[10px] transition-colors",
                          undoArmed === b.id
                            ? "bg-[var(--color-danger)] text-white"
                            : "text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-danger)]",
                        )}
                      >
                        {undoArmed === b.id ? "确认?" : "撤销"}
                      </button>
                    )}
                  </div>
                  );
                })}
              </>
            )}

            {/* R-25 最近打标：挂/摘流水，AI 来源带角标 */}
            {recentOps.length > 0 && (
              <>
                <h3 className="ui-section-title mt-4 mb-2 px-2">
                  最近打标
                </h3>
                {recentOps.map((o) => (
                  <div key={o.id} className="flex min-h-8 items-center gap-1 rounded-md px-2.5 py-1 text-[11px] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)]">
                    <span className={o.op === "add" ? "text-[var(--color-text)]" : "text-[var(--color-danger)]"}>
                      {o.op === "add" ? "＋" : "－"}
                    </span>
                    <span className="shrink-0 text-[var(--color-text)]">{o.tagName}</span>
                    <span className="min-w-0 flex-1 truncate opacity-70" title={o.assetName}>{o.assetName}</span>
                    {o.actor !== "manual" && (
                      <span className="shrink-0 rounded bg-[var(--color-status-soft)] px-1.5 py-0.5 text-[9px] text-[var(--color-status)]">AI</span>
                    )}
                  </div>
                ))}
              </>
            )}
          </div>
        )}
      </aside>

      {/* 右侧工作流：大图 → 胶片条 → EXIF → 标签 → 操作。
          与助手右侧工作台共用 SuggestionReview，避免两处行为漂移（A3-07 第 3 条）。 */}
      <div className="flex min-w-0 flex-1 flex-col">
        {/* M-14：错误必须能被辅助技术立刻感知（role="alert"），否则键盘/读屏用户只看到界面没反应。 */}
        {error && (
          <p role="alert" className="px-4 pt-2 text-xs text-[var(--color-danger)]">
            {error}
          </p>
        )}
        {connectionError && (
          <p role="alert" className="px-4 pt-2 text-xs text-[var(--color-danger)]">
            服务切换失败：{connectionError}
          </p>
        )}

        <SuggestionReview
          suggestions={suggestions}
          index={reviewIdx}
          onGoto={goto}
          emptyHint={
            running
              ? "正在生成标签建议…"
              : suggestions.length > 0
                ? "本批次已全部处理完毕"
                : "还没有打标批次——去素材库选中素材，点顶部「打标」"
          }
          onAfterWrite={afterWrite}
        />
      </div>
    </div>
  );
}
