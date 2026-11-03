/** 助手右侧工作台（A3-07）。
 *
 * 设计边界（plans/v2/execution/A3.md A3-07、实现契约 §4.4）：
 * - 这里只是「同一批领域状态的另一个视图」：入库状态全部来自 importWorkbenchStore
 *   （经 useImportWorkbench 共享），打标状态全部来自 aiStore，绝不复制一份领域状态，
 *   因此在独立入库页与右侧工作台之间不会出现「两处各自提交/确认」。
 * - 附件先留附件：面板不把 Composer 里的附件自动转成入库对象；只有用户在本面板里
 *   显式选文件/选目录，才进入待入库清单。
 * - 从已受理导入准备打标必须按「逐项成功收据」：operation 的 step 引用由后端
 *   `ImportResultView.sourceStepRef` 给出；`duplicate_mapped` 项必须由用户在这里
 *   逐个勾选后才传给 `prepareFromImport`，禁止从数量或目录推断对象。
 * - 打标仍走 A2-04：准备只产出待审批计划，必须用户确认审批文案后显式批准，
 *   再显式启动；未启动不联网。
 * - 正式确认（标签写入、入库提交）只走可信 API（aiStore / importWorkbenchStore），
 *   本组件不 invoke。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import PendingList, { formatSize } from "@/components/import/PendingList";
import RenameBuilder from "@/components/import/RenameBuilder";
import ImportProgressPanel, { operationLabel } from "@/components/import/ImportProgressPanel";
import SuggestionReview from "@/components/ai/SuggestionReview";
import AiTaggingProgress, { assetFileLabel } from "@/components/ai/AiTaggingProgress";
import { openFileExternal } from "@/api/import";
import { aiSubmitTaggingPlan } from "@/api/ai";
import { approvePlan } from "@/api/plans";
import { summarySelection, type SelectionSummary } from "@/api/selections";
import { displayBasename } from "@/utils/pathDisplay";
import { computeAiStats } from "@/utils/aiStats";
import { useImportWorkbench } from "@/hooks/useImportWorkbench";
import { useAiStore } from "@/stores/aiStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import { useTagStore } from "@/stores/tagStore";
import type { AiTaggingUiState, TaggingOperation } from "@/types/ai";

export type AssistantWorkbenchTab = "import" | "review";

interface AssistantWorkbenchProps {
  conversationRef: string;
  tab: AssistantWorkbenchTab;
  wide: boolean;
  /** A3-08：本对话交接过的素材集合快照（只读，先在右栏预览，再打开结果视图）。 */
  selections?: string[];
  onTabChange: (tab: AssistantWorkbenchTab) => void;
  onToggleWide: () => void;
  onClose: () => void;
}

export default function AssistantWorkbench({
  conversationRef,
  tab,
  wide,
  selections = [],
  onTabChange,
  onToggleWide,
  onClose,
}: AssistantWorkbenchProps) {
  const control = useImportWorkbench();
  const {
    workbench,
    error,
    libraryRoot,
    settingsLoadError,
    busy,
    conversationSwitching,
    submissionUncertain,
    activeOperations,
    operationSnapshots,
    selectedTask,
    items,
    imageCount,
    videoCount,
    totalSize,
    unsupportedCount,
    showUnsupported,
    setShowUnsupported,
    showWarnings,
    setShowWarnings,
    operationInProgress,
    unsupportedByPlan,
    rememberDirectory,
    setRememberDirectory,
    rememberedGrants,
    chooseFiles,
    chooseFolder,
    chooseRememberedFolder,
    removeItem,
    removeUnsupported,
    runSafely,
    submitAll,
  } = control;

  const suggestions = useAiStore((s) => s.suggestions);
  const running = useAiStore((s) => s.running);
  const cancelling = useAiStore((s) => s.cancelling);
  const currentBatchId = useAiStore((s) => s.currentBatchId);
  const runProgress = useAiStore((s) => s.runProgress);
  const lastProgressAssetId = useAiStore((s) => s.lastProgressAssetId);
  const confirmAll = useAiStore((s) => s.confirmAll);

  /** M-14：计划失效或读取失败时，`开始入库` 会因「草稿里的计划数 ≠ 已加载计划数」被静默禁用。
   *  用户只看到灰按钮、看不到原因，也没有下一步——这里补一条可行动的说明（按「清空」重来）。 */
  const stalePlanCount = Math.max(
    0,
    workbench.draft.pendingPlanRefs.length - Object.keys(workbench.plans).length,
  );
  const preparingFromImport = useAiStore((s) => s.preparingFromImport);
  const prepareFromImport = useAiStore((s) => s.prepareFromImport);
  const startTagging = useAiStore((s) => s.startTagging);
  const openBatch = useAiStore((s) => s.openBatch);
  const refreshLibrary = useLibraryStore((s) => s.refresh);
  const refreshTags = useTagStore((s) => s.refresh);

  const [reviewIdx, setReviewIdx] = useState(0);
  /** 每个导入 operation 下用户显式勾选的重复项（itemRef 集合）；默认空 = 全部不纳入。 */
  const [duplicates, setDuplicates] = useState<Record<string, string[]>>({});
  const [prepError, setPrepError] = useState<string | null>(null);
  /** 已批准、等待用户单独点「开始打标」的待启动操作；为空表示当前没有待启动项。 */
  const [readyOperation, setReadyOperation] = useState<TaggingOperation | null>(null);
  /** 正在批准/提交「从这次入库准备打标」这条链路。 */
  const [preparingApproval, setPreparingApproval] = useState(false);
  /** 正在显式启动已批准操作（会联网）。 */
  const [startingTagging, setStartingTagging] = useState(false);

  const goto = useCallback(
    (next: number) => setReviewIdx(Math.max(0, Math.min(next, suggestions.length - 1))),
    [suggestions.length],
  );

  const afterWrite = useCallback(async () => {
    await Promise.all([refreshLibrary(), refreshTags()]);
  }, [refreshLibrary, refreshTags]);

  const toggleDuplicate = (operationRef: string, itemRef: string) => {
    setDuplicates((prev) => {
      const current = prev[operationRef] ?? [];
      const next = current.includes(itemRef)
        ? current.filter((value) => value !== itemRef)
        : [...current, itemRef];
      return { ...prev, [operationRef]: next };
    });
  };

  /** 从某次已受理导入的逐项收据准备打标范围：先审批并把待启动操作暂存下来，
   *  再由用户单独点「开始打标」显式启动（审批与启动是两个独立动作，契约 §4.4）。 */
  const prepareTagging = async (operationRef: string, sourceStepRef: string) => {
    setPrepError(null);
    setPreparingApproval(true);
    try {
      const included = duplicates[operationRef] ?? [];
      const preparation = await prepareFromImport(operationRef, sourceStepRef, included);
      if (!preparation) return; // 错误已写入 aiStore.error
      let operation: TaggingOperation | null = preparation.operation;
      if (preparation.requiresApproval) {
        if (!preparation.preview) {
          setPrepError("AI 打标计划缺少审批预览，未启动。");
          return;
        }
        const approved = window.confirm(
          `${preparation.summary}\n\n批准后将创建待启动操作。联网只会在你点击「开始打标」后开始。`,
        );
        if (!approved) return;
        const approval = await approvePlan(preparation.preview.planId, preparation.preview.reviewRef);
        if (approval.reviewStatus !== "approved") {
          setPrepError("AI 打标计划未获批准，未启动。");
          return;
        }
        operation = await aiSubmitTaggingPlan(
          preparation.preview.planId,
          `tagging_submit_${preparation.preview.planId}`,
        );
      }
      if (!operation) {
        setPrepError("AI 打标计划未生成可启动操作，未启动。");
        return;
      }
      // 关键：此处**不**启动打标，只暂存待启动操作；真正联网在 handleStartTagging。
      // 这样「审批」与「开始打标」保持两个独立动作，未启动不联网。
      setReadyOperation(operation);
      // 提前把本次操作所在批次挂到当前视图，切到审核页就能看到这一批（含空建议占位），
      // 但不会因此启动 AI。openBatch 只读建议，不触发联网。
      await openBatch(operation.batchId);
      onTabChange("review");
    } finally {
      setPreparingApproval(false);
    }
  };

  /** 用户在工作台里显式启动已批准的打标操作（此时才会真正联网）。 */
  const handleStartTagging = async () => {
    if (!readyOperation || startingTagging) return;
    setPrepError(null);
    setStartingTagging(true);
    try {
      await startTagging(readyOperation);
      setReadyOperation(null);
    } finally {
      setStartingTagging(false);
    }
  };

  const recentWithResults = useMemo(
    () =>
      workbench.draft.recentOperationRefs
        .map((ref) => ({
          ref,
          operation: operationSnapshots[ref],
          result: workbench.operationResults[ref],
        }))
        .filter((entry) => entry.operation !== undefined),
    [workbench.draft.recentOperationRefs, workbench.operationResults, operationSnapshots],
  );

  const reviewEmptyHint = running
    ? "正在生成标签建议…"
    : suggestions.length > 0
      ? "本批次已全部处理完毕"
      : "还没有待审核的标签建议。可在「入库」中从成功收据准备打标范围。";

  // A3-07 缺口修复（同步）：右栏此前完全没有「扫描异常」与「受理待核对」两个区块，
  // 而独立入库页都有 —— 同一任务在两处显示的信息不一致，且提交受理不确定时右栏
  // 无从恢复（清空按钮被 busy 禁用但没有重试入口）。这里补齐，绝不复制领域状态。
  const warnings = workbench.draft.warnings;
  const pendingPlanRefs = workbench.draft.pendingPlanRefs;
  const uncertainRefs = pendingPlanRefs.filter((entry) => entry.idempotencyKey);
  const activeOperation = activeOperations[0];

  // A3-07 缺口修复（进度）：在右栏启动打标后此前没有任何进度显示，
  // AiTaggingProgress 却是页面内唯一 AI 进度 UI。这里按 aiStore 派生出同一份状态。
  const run = runProgress?.batchId === currentBatchId ? runProgress : null;
  const aiState: AiTaggingUiState =
    running
      ? cancelling
        ? { phase: "cancelling", processed: run?.processed ?? 0, total: run?.total ?? 0 }
        : (run?.total ?? 0) > 0
          ? { phase: "running", processed: run!.processed, total: run!.total, currentAssetId: lastProgressAssetId ?? undefined }
          : { phase: "starting", total: 0 }
      : { phase: "idle" };
  const currentAssetName = useMemo(() => {
    if (lastProgressAssetId == null) return null;
    const hit = suggestions.find((item) => item.assetId === lastProgressAssetId);
    return hit ? assetFileLabel(hit.assetPath) : null;
  }, [suggestions, lastProgressAssetId]);
  const aiStats = useMemo(() => computeAiStats(suggestions), [suggestions]);

  // A3-07：右栏只跟随它所属的对话。别的对话在后台推进（入库/打标）时，这里既不显示
  // 它的进度，也不允许用它的草稿提交，避免「后台其他对话抢右栏」。
  const boundToPanel = workbench.conversationRef === conversationRef;

  if (!boundToPanel) {
    return (
      <aside
        aria-label="右侧工作台"
        className={clsx(
          "flex shrink-0 flex-col border-l border-[var(--color-border)] bg-[var(--color-bg)]",
          wide ? "w-[620px]" : "w-[380px]",
        )}
      >
        <header className="flex items-center justify-between border-b border-[var(--color-border)] px-3 py-2">
          <h2 className="text-xs font-medium text-[var(--color-text)]">右侧工作台</h2>
          <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={onClose}>
            收起
          </Button>
        </header>
        <p role="status" className="px-3 py-3 text-[11px] leading-5 text-[var(--color-text-secondary)]">
          正在切换到当前对话的入库草稿与打标批次，完成后这里会显示内容。其他对话的任务仍会在后台继续。
        </p>
      </aside>
    );
  }

  return (
    <aside
      aria-label="右侧工作台"
      className={clsx(
        "flex shrink-0 flex-col border-l border-[var(--color-border)] bg-[var(--color-bg)]",
        wide ? "w-[620px]" : "w-[380px]",
      )}
    >
      <header className="flex items-center gap-1 border-b border-[var(--color-border)] px-3 py-2">
        <div className="flex min-w-0 flex-1 items-center gap-1" role="tablist" aria-label="工作台视图">
          {(["import", "review"] as const).map((value) => (
            <button
              key={value}
              type="button"
              role="tab"
              aria-selected={tab === value}
              onClick={() => onTabChange(value)}
              className={clsx(
                "rounded-md px-2 py-1 text-xs transition-colors",
                tab === value
                  ? "bg-[var(--color-surface)] font-medium text-[var(--color-text)]"
                  : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
              )}
            >
              {value === "import" ? "入库" : "标签审核"}
              {value === "review" && suggestions.length > 0 ? `（${suggestions.length}）` : ""}
            </button>
          ))}
        </div>
        <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={onToggleWide} title="加宽工作台">
          {wide ? "收窄" : "加宽"}
        </Button>
        <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={onClose}>
          收起
        </Button>
      </header>

      {tab === "import" ? (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-text-secondary)]">
            <span>图片 {imageCount}</span>
            <span>视频 {videoCount}</span>
            <span>共 {items.length} 项 · {formatSize(totalSize)}</span>
            {conversationSwitching && <span className="text-[var(--color-status)]">正在切换会话…</span>}
          </div>

          {/* A3-08：交接过来的集合先在右侧预览数量，再决定是否打开素材库结果视图。
              这里只读 summary，不加载素材行；打开结果视图不改变素材库的筛选与选择。 */}
          {selections.length > 0 && (
            <section
              aria-label="本次结果预览"
              className="border-b border-[var(--color-border)] px-3 py-2 text-[11px]"
            >
              <p className="text-[var(--color-text-secondary)]">交接的素材集合</p>
              <ul className="mt-1 space-y-1">
                {selections.map((selectionRef) => (
                  <SelectionPreview
                    key={selectionRef}
                    selectionRef={selectionRef}
                    conversationRef={conversationRef ?? undefined}
                  />
                ))}
              </ul>
            </section>
          )}

          <div className="flex flex-col gap-2 border-b border-[var(--color-border)] px-3 py-2">
            <label className="flex flex-col gap-1">
              <span className="text-[11px] text-[var(--color-text-secondary)]">
                分库名称{libraryRoot ? "（总库下新建文件夹）" : "（需先在设置里配置总库）"}
              </span>
              <input
                value={workbench.draft.collection}
                onChange={(event) =>
                  void runSafely(() =>
                    workbench.updateOptions(event.target.value, workbench.draft.renamePattern),
                  )
                }
                disabled={!libraryRoot || workbench.submitting || conversationSwitching}
                placeholder="如：旅行"
                className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-xs outline-none focus:border-[var(--color-accent)] disabled:opacity-40"
              />
            </label>
            <RenameBuilder
              value={workbench.draft.renamePattern}
              onChange={(value) =>
                void runSafely(() => workbench.updateOptions(workbench.draft.collection, value))
              }
              disabled={!libraryRoot || workbench.submitting || conversationSwitching}
              collection={workbench.draft.collection.trim()}
              sampleStem={items[0] ? displayBasename(items[0].displayName).replace(/\.[^.]+$/, "") : ""}
              sampleExt={items[0]?.displayName.split(".").pop() ?? ""}
            />
            {/* A3-06：记住读取不等于入库或外发许可，撤销入口在设置 → 素材库与入库。 */}
            <label className="flex items-start gap-2 text-[11px] text-[var(--color-text-secondary)]">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={rememberDirectory}
                onChange={(event) => setRememberDirectory(event.currentTarget.checked)}
              />
              <span>
                记住这个目录（下次不用重新选择）
                <span className="mt-0.5 block text-[10px] text-[var(--color-text-tertiary)]">
                  只读，可随时在设置里撤销。
                </span>
              </span>
            </label>
          </div>

          {!libraryRoot && (
            <p role="status" className="border-b border-[var(--color-border)] px-3 py-2 text-[11px] leading-5 text-[var(--color-text-secondary)]">
              {settingsLoadError
                ? `设置读取失败：${settingsLoadError}`
                : "请先设置素材总库位置，再选择或导入文件。"}
            </p>
          )}

          {error && !workbench.preparing && (
            <p role="alert" className="border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-danger)]">
              {error}
            </p>
          )}

          {unsupportedCount > 0 && (
            <div className="border-b border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-[11px]">
              <p className="text-[var(--color-status)]">
                有 {unsupportedCount} 个文件无法生成缩略图，当前版本禁止导入。
              </p>
              <div className="mt-1 flex items-center gap-2">
                <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={() => setShowUnsupported(!showUnsupported)}>
                  {showUnsupported ? "收起明细" : "查看明细"}
                </Button>
                <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={() => void removeUnsupported()}>
                  剔除并保留可导入项
                </Button>
              </div>
              {showUnsupported && (
                <ul className="mt-1 max-h-28 space-y-0.5 overflow-y-auto">
                  {unsupportedByPlan.flatMap((group) =>
                    group.items.map((item) => (
                      <li key={`${group.planRef}-${item.itemRef}`} className="break-all text-[var(--color-text-secondary)]" title={item.previewMessage}>
                        {item.displayName}
                      </li>
                    )),
                  )}
                </ul>
              )}
            </div>
          )}

          {/* A3-07：扫描异常明细。独立入库页有「扫描时跳过 N 项」，右栏此前没有，
              同一任务在两处显示的信息就不一致（卡片要求「异常」完整可见）。 */}
          {warnings.length > 0 && (
            <div
              className="border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-status)]"
              role="status"
            >
              <p>扫描时跳过或遇到 {warnings.length} 项，请确认后再入库。</p>
              <button
                type="button"
                onClick={() => setShowWarnings((value) => !value)}
                className="mt-1 text-left underline"
              >
                {showWarnings ? "收起扫描提示" : "查看扫描提示"}
              </button>
              {showWarnings && (
                <ul className="mt-1 max-h-32 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
                  {warnings.map((warning, index) => (
                    <li key={`${index}-${warning}`} className="break-all">
                      {warning}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}

          {/* A3-07：受理待核对。提交返回前连接中断时 pendingPlanRef 会带 idempotencyKey，
              独立入库页给了「重试并核对受理」；右栏此前只有一个被 busy 禁用的清空按钮，
              用户会卡住。这里补同一恢复路径（复用同一个 submitPlan，不复制状态）。 */}
          {uncertainRefs.length > 0 && (
            <div
              className="border-b border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-status)]"
              role="status"
            >
              <p>有 {uncertainRefs.length} 份计划的受理响应待核对，请先重试核对再清空清单。</p>
              <div className="mt-1 flex flex-wrap gap-1.5">
                {uncertainRefs.map(({ planRef }) => (
                  <Button
                    key={planRef}
                    className="min-h-0 px-2 py-0.5 text-[11px]"
                    disabled={workbench.submitting || conversationSwitching}
                    onClick={() => void runSafely(() => workbench.submitPlan(planRef))}
                  >
                    重试并核对受理
                  </Button>
                ))}
              </div>
            </div>
          )}

          <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
            {items.length > 0 ? (
              <PendingList
                items={items}
                running={busy}
                onRemove={() => undefined}
                onRemoveCandidate={(item) => void removeItem(item)}
                onAddFiles={() => void chooseFiles()}
                onAddFolder={() => void chooseFolder()}
                onClear={() => void runSafely(() => workbench.clearPendingPlans())}
                onOpenItem={(path) => void openFileExternal(path).catch(() => undefined)}
              />
            ) : (
              <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-[var(--color-border)] px-3 py-8 text-center">
                <p className="text-xs text-[var(--color-text)]">还没有待入库文件</p>
                <p className="text-[11px] leading-5 text-[var(--color-text-secondary)]">
                  附件只是一份引用，不会被自动入库。选好文件后先入清单，确认设置再提交。
                </p>
                {!busy && (
                  <div className="flex flex-col items-center gap-1.5">
                    <div className="flex gap-2">
                      <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={() => void chooseFiles()}>
                        选择文件…
                      </Button>
                      <Button className="min-h-0 px-2 py-0.5 text-[11px]" onClick={() => void chooseFolder()}>
                        选择文件夹…
                      </Button>
                    </div>
                    {/* A3-06：已记住的目录在这里也必须有直接入口，否则「记住」只是空承诺。 */}
                    {rememberedGrants.length > 0 && (
                      <div className="flex flex-wrap items-center justify-center gap-1" aria-label="已记住的导入目录">
                        <span className="text-[10px] text-[var(--color-text-tertiary)]">已记住的目录：</span>
                        {rememberedGrants.map((grant) => (
                          <button
                            key={grant.grantRef}
                            type="button"
                            disabled={!grant.available}
                            title={
                              grant.available
                                ? `${grant.displayPath}（只读；可在设置里撤销）`
                                : "目录已被移动、删除或更换了总库，需要重新选择"
                            }
                            onClick={() => void chooseRememberedFolder(grant.grantRef)}
                            className="ui-control rounded-full px-2 py-0 text-[11px] disabled:opacity-40"
                          >
                            {grant.displayName}
                            {!grant.available && "（已不可用）"}
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                )}
              </div>
            )}

            {recentWithResults.length > 0 && (
              <section className="mt-3 space-y-2" aria-label="最近入库与打标准备">
                {recentWithResults.map(({ ref, operation, result }) => {
                  if (!operation) return null;
                  const picked = duplicates[ref] ?? [];
                  const failures = result?.failures ?? [];
                  return (
                    <div key={ref} className="rounded-md border border-[var(--color-border)] px-2.5 py-2 text-[11px]">
                      <p className="text-[var(--color-text-secondary)]">
                        {operationLabel(operation)} · 已处理 {operation.completed}/{operation.total ?? "?"} · 失败 {operation.failed}
                      </p>
                      {result && (
                        <p className="mt-0.5 text-[var(--color-text-secondary)]">
                          新入库 {result.imported} · 重复 {result.duplicates} · 失败 {result.failed}
                        </p>
                      )}
                      {failures.length > 0 && (
                        <details className="mt-1 text-[var(--color-status)]">
                          <summary className="cursor-pointer">查看异常明细（{failures.length}）</summary>
                          <ul className="mt-1 max-h-24 space-y-0.5 overflow-y-auto">
                            {failures.map((failure) => (
                              <li key={failure.itemRef} className="break-all">
                                {failure.displayName} · {failure.state === "outcome_unknown" ? "结果未知" : "失败"}
                                {failure.errorCode ? ` · ${failure.errorCode}` : ""}
                              </li>
                            ))}
                          </ul>
                        </details>
                      )}

                      {result && result.sourceStepRef && (
                        <div className="mt-2 border-t border-[var(--color-border)] pt-2">
                          {result.duplicateItems.length > 0 ? (
                            <>
                              <p className="leading-5 text-[var(--color-text-secondary)]">
                                有 {result.duplicateItems.length} 项与素材库中已有素材重复。它们不会自动进入打标范围，
                                需要你逐个勾选确认。
                              </p>
                              <ul className="mt-1 space-y-0.5">
                                {result.duplicateItems.map((item) => (
                                  <li key={item.itemRef}>
                                    <label className="flex items-start gap-2">
                                      <input
                                        type="checkbox"
                                        className="mt-0.5"
                                        checked={picked.includes(item.itemRef)}
                                        onChange={() => toggleDuplicate(ref, item.itemRef)}
                                      />
                                      <span className="min-w-0 flex-1 break-all text-[var(--color-text-secondary)]">
                                        {item.displayName}
                                        <span className="ml-1 text-[var(--color-text-tertiary)]">（映射到已有素材）</span>
                                      </span>
                                    </label>
                                  </li>
                                ))}
                              </ul>
                            </>
                          ) : (
                            <p className="leading-5 text-[var(--color-text-secondary)]">
                              这次入库没有重复映射项，打标范围只包含逐项成功收据里的素材。
                            </p>
                          )}
                          <Button
                            className="mt-1.5 w-full text-[11px]"
                            variant="primary"
                            disabled={preparingFromImport || preparingApproval || running || startingTagging}
                            onClick={() => void prepareTagging(ref, result.sourceStepRef)}
                          >
                            {preparingApproval || preparingFromImport ? "正在准备打标计划…" : "从这次入库准备打标"}
                          </Button>
                          <p className="mt-1 text-[10px] leading-4 text-[var(--color-text-tertiary)]">
                            只会生成待审批计划；批准后还需要点「开始打标」才会启动，未启动不会联网。
                          </p>
                          {/* R35：批准前必须写清发的是什么清晰度，并说明高清要另外确认。 */}
                          <p
                            className="mt-1 text-[10px] leading-4 text-[var(--color-text-tertiary)]"
                            data-testid="tagging-egress-clarity"
                          >
                            图片按压缩预览发送（最长边 1024px 的 JPEG）；需要更高清的画面不在本次范围内，
                            要另外提出并单独确认。视频按设置发送封面或指定帧数。
                          </p>
                          {readyOperation && (
                            <div className="mt-2 rounded-md border border-[var(--color-accent)] px-2 py-1.5">
                              <p className="text-[10px] leading-4 text-[var(--color-text-secondary)]">
                                打标计划已批准，{readyOperation.requestLimit} 次请求待启动。点「开始打标」才会联网生成建议。
                              </p>
                              <Button
                                className="mt-1 w-full text-[11px]"
                                variant="primary"
                                disabled={startingTagging || running}
                                onClick={() => void handleStartTagging()}
                              >
                                {startingTagging ? "正在开始打标…" : "开始打标"}
                              </Button>
                            </div>
                          )}
                          {prepError && (
                            <p role="alert" className="mt-1 text-[var(--color-danger)]">
                              {prepError}
                            </p>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </section>
            )}
          </div>

          <div className="border-t border-[var(--color-border)] px-3 py-2">
            <div className="flex items-center gap-2">
              {workbench.preparing ? (
                <Button className="flex-1 text-[11px]" onClick={() => void runSafely(() => workbench.cancelPreparation())}>
                  取消准备
                </Button>
              ) : activeOperation?.canCancel ? (
                /* A3-07：入库已经在跑时，右栏此前只留一个禁用按钮，用户既不能取消也不懂为什么。
                   独立入库页在这里给的是「取消入库」，两处必须一致。 */
                <Button
                  className="flex-1 text-[11px]"
                  disabled={conversationSwitching}
                  onClick={() => void runSafely(() => workbench.cancel(activeOperation.operationId))}
                >
                  取消入库
                </Button>
              ) : operationInProgress ? (
                <Button className="flex-1 text-[11px]" disabled>
                  正在处理…
                </Button>
              ) : (
                <Button
                  className="flex-1 text-[11px]"
                  variant="primary"
                  disabled={
                    items.length === 0 ||
                    unsupportedCount > 0 ||
                    !!operationInProgressGuard(conversationSwitching, workbench.submitting) ||
                    workbench.draft.pendingPlanRefs.length !== Object.keys(workbench.plans).length
                  }
                  onClick={() => void submitAll()}
                >
                  开始入库（{items.length}）
                </Button>
              )}
              <Button
                className="text-[11px]"
                disabled={items.length === 0 || busy}
                onClick={() => void runSafely(() => workbench.clearPendingPlans())}
                title={submissionUncertain ? "受理响应待核对，请重试该计划" : undefined}
              >
                清空
              </Button>
            </div>
            {stalePlanCount > 0 && (
              <p role="alert" className="mt-1 text-[10px] leading-4 text-[var(--color-danger)]">
                有 {stalePlanCount} 份待确认计划已失效或读取失败，暂时不能开始入库。
                请点「清空」后回到入库页重新选择素材（导入计划 30 分钟内有效）。
              </p>
            )}
            <div className="mt-2">
              <ImportProgressPanel task={selectedTask} starting={workbench.preparing} />
            </div>
            {items.some((item) => item.previewStatus === "limited") && (
              <p className="mt-1 text-[10px] leading-4 text-[var(--color-status)]">
                有特殊格式已解析出缩略图，可以导入；但后续高清预览、元数据或 AI 功能可能受限。
              </p>
            )}
          </div>
        </div>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex items-center justify-between border-b border-[var(--color-border)] px-3 py-2">
            <p className="text-[11px] text-[var(--color-text-secondary)]">
              {suggestions.length > 0 ? `第 ${Math.min(reviewIdx, suggestions.length - 1) + 1} / ${suggestions.length} 张` : "暂无建议"}
            </p>
            <Button
              className="min-h-0 px-2 py-0.5 text-[11px]"
              onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "ai" }))}
            >
              打开独立打标页
            </Button>
          </div>

          {/* A3-07：在右栏启动打标后此前完全没有进度，而 AiTaggingProgress 是页面内唯一
              的 AI 进度 UI。批量确认此前也只存在于独立打标页，右栏缺卡片要求的「批量确认」。 */}
          {aiState.phase !== "idle" && (
            <div className="border-b border-[var(--color-border)] px-3 py-2">
              <AiTaggingProgress state={aiState} currentAssetName={currentAssetName} />
            </div>
          )}
          {aiStats.awaitingConfirmation > 0 && !running && (
            <div className="border-b border-[var(--color-border)] px-3 py-2">
              <Button
                className="w-full min-h-0 py-1 text-[11px]"
                variant="primary"
                onClick={() => void confirmAll()}
              >
                全部确认（{aiStats.awaitingConfirmation}）
              </Button>
            </div>
          )}

          <div className="flex min-h-0 flex-1 flex-col">
            <SuggestionReview
              suggestions={suggestions}
              index={reviewIdx}
              onGoto={goto}
              emptyHint={reviewEmptyHint}
              onAfterWrite={afterWrite}
            />
          </div>
        </div>
      )}
    </aside>
  );
}

/** 入库进行中就不允许再次提交（提交中或正在切会话都算）。 */
function operationInProgressGuard(conversationSwitching: boolean, submitting: boolean): boolean {
  return conversationSwitching || submitting;
}

/** A3-08 右侧预览：只读数量，不加载素材行；点按钮才进素材库的「本次结果视图」。
 *
 *  这里刻意不复用 `SelectionCard`：卡片挂在消息流里（会话级），这份预览在
 *  工作台里（当前面板），两者读的是同一份后端摘要，但展示位置和动作不同。 */
function SelectionPreview({
  selectionRef,
  conversationRef,
}: {
  selectionRef: string;
  conversationRef?: string;
}) {
  const [summary, setSummary] = useState<SelectionSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const openResult = useSelectionResultStore((s) => s.open);

  useEffect(() => {
    let alive = true;
    setSummary(null);
    setError(null);
    summarySelection(selectionRef)
      .then((next) => {
        if (alive) setSummary(next);
      })
      .catch((cause: unknown) => {
        if (!alive) return;
        setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      alive = false;
    };
  }, [selectionRef]);

  return (
    <li className="flex flex-wrap items-center gap-2">
      <span className="min-w-0 flex-1 text-[var(--color-text-secondary)]">
        {error
          ? `读取失败：${error}`
          : summary
            ? `共 ${summary.snapshot} 项 · 可用 ${summary.available} 项${
                summary.missing > 0 ? ` · ${summary.missing} 项已不可用` : ""
              }`
            : "正在读取…"}
      </span>
      <Button
        className="min-h-0 px-2 py-0.5 text-[11px]"
        disabled={!summary}
        onClick={() => {
            openResult({
              selectionRef,
              title: summary ? `助手交接的 ${summary.snapshot} 项` : "助手交接的素材",
              // A3-08：来源只用后端快照行里的 query，界面不另编一份。
              query: summary?.query ?? "助手会话",
              conversationRef,
            });
          // 结果视图托管在素材库页：先设目标再切页，避免闪一帧空视图。
          window.dispatchEvent(new CustomEvent("app:navigate", { detail: "library" }));
        }}
      >
        查看本次结果
      </Button>
    </li>
  );
}
