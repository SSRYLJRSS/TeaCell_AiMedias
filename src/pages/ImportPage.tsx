/** Manual import workbench: durable drafts point at fixed plans and exact operations.
 *
 * A3-07：行为层抽到 `useImportWorkbench`，助手右侧工作台与这里共用同一套派生与动作，
 * 保证同一批文件不会被两个入口分别提交或分别确认。本页只负责整页布局与拖放手势。
 */
import { useEffect, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import PendingList, { formatSize } from "@/components/import/PendingList";
import ImportOutcome from "@/components/import/ImportOutcome";
import RenameBuilder from "@/components/import/RenameBuilder";
import ImportProgressPanel, { operationLabel } from "@/components/import/ImportProgressPanel";
import { displayBasename } from "@/utils/pathDisplay";
import { openFileExternal } from "@/api/import";
import { useImportWorkbench } from "@/hooks/useImportWorkbench";
import { registerImportDropHandler } from "@/utils/importDropDispatcher";

export default function ImportPage() {
  const control = useImportWorkbench();
  const {
    workbench,
    error: combinedError,
    libraryRoot,
    settingsLoaded,
    settingsLoadError,
    loadSettings,
    busy,
    operationInProgress,
    submissionUncertain,
    conversationSwitching,
    activeOperations,
    operationSnapshots,
    selectedTask,
    items,
    imageCount,
    videoCount,
    totalSize,
    unsupportedByPlan,
    unsupportedCount,
    showUnsupported,
    setShowUnsupported,
    showWarnings,
    setShowWarnings,
    rememberDirectory,
    setRememberDirectory,
    rememberedGrants,
    stage,
    chooseFiles,
    chooseFolder,
    chooseRememberedFolder,
    removeItem,
    removeUnsupported,
    runSafely,
    submitAll,
  } = control;
  const [dragOver, setDragOver] = useState(false);
  /** 最近一次入库操作直接在右栏呈现结果；更早的操作收进「历史入库操作」。 */
  const latestOperationRef = workbench.draft.recentOperationRefs[0];
  const latestOperation = latestOperationRef ? operationSnapshots[latestOperationRef] : undefined;
  const olderOperationRefs = workbench.draft.recentOperationRefs.slice(1);
  /** 最近一次入库结果：清单清空后是右栏主体，清单还在时放在列表上方，结果始终可见。 */
  const latestOutcome = latestOperation && latestOperationRef ? (
    <ImportOutcome
      operation={latestOperation}
      result={workbench.operationResults[latestOperationRef]}
      resultError={workbench.operationResultErrors[latestOperationRef]}
      resultLoading={workbench.operationResultLoading[latestOperationRef] ?? false}
      collection={workbench.draft.collection}
      libraryRoot={libraryRoot}
      onRetryResult={() => void workbench.loadOperationResult(latestOperationRef, latestOperation.revision)}
      onOpenLibrary={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "library" }))}
      onResume={() => void runSafely(() => workbench.resume(latestOperationRef))}
      onCancel={() => void runSafely(() => workbench.cancel(latestOperationRef))}
      onHandoff={() => void runSafely(async () => {
        await workbench.handoffOperation(latestOperationRef);
        window.dispatchEvent(new CustomEvent("app:navigate", { detail: "assistant" }));
      })}
    />
  ) : null;
  /** R19：拖入的文件先当附件（只记路径，不预检、不入库、不外发），
   *  用户点附件旁的「导入」才进入检查清单。 */
  const [droppedAttachments, setDroppedAttachments] = useState<string[]>([]);

  // 整页拖放属于页面手势，不是工作台状态，因此留在页面层。
  useEffect(() => registerImportDropHandler((payload) => {
    if (payload.type === "over") setDragOver(true);
    else if (payload.type === "leave") setDragOver(false);
    else if (payload.type === "drop") {
      setDragOver(false);
      // 只排序去重保留用户拖入的原始顺序，不在这里做任何检查或读取。
      setDroppedAttachments((current) => {
        const next = [...current];
        for (const path of payload.paths) if (!next.includes(path)) next.push(path);
        return next;
      });
    }
  }), []);

  /** 把（全部或一个）附件送进预检；成功后从附件列表移除，失败则保留以便重试。 */
  const importAttachments = async (paths: string[]) => {
    if (paths.length === 0) return;
    await stage(paths);
    setDroppedAttachments((current) => current.filter((path) => !paths.includes(path)));
  };
  return (
    <div className="flex h-full">
      <Modal
        open={showUnsupported && unsupportedCount > 0}
        title="有文件无法生成缩略图"
        onClose={() => setShowUnsupported(false)}
        footer={(
          <>
            <Button onClick={() => setShowUnsupported(false)}>暂不处理</Button>
            <Button variant="primary" onClick={() => void removeUnsupported()}>剔除并保留可导入项</Button>
          </>
        )}
      >
        <p className="text-sm text-[var(--color-text)]">
          以下 {unsupportedCount} 个文件无法解析入库缩略图，当前版本禁止导入。确认后会从固定计划中剔除。
        </p>
        <ul className="mt-3 max-h-40 space-y-1 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2 text-xs text-[var(--color-text-secondary)]">
          {unsupportedByPlan.flatMap((group) => group.items.map((item) => (
            <li key={`${group.planRef}-${item.itemRef}`} className="break-all" title={item.previewMessage}>{item.displayName}</li>
          )))}
        </ul>
      </Modal>

      {/* pb-16 = 悬浮助手入口安全区（入口 48 + 间距 8 + 余量 8 = 64）。
          注意 main 已经有 pb-14 让出底栏，这里不能再把底栏 56px 算第二遍：
          原 pb-32=128 等于底栏重复计算，整块「入库进度」被顶到半空。 */}
      <aside
        className="flex w-[208px] shrink-0 flex-col border-r border-[var(--color-border)] pb-16"
        data-testid="import-side-panel"
      >
        <div className="border-b border-[var(--color-border)] p-3">
          <h3 className="mb-2 text-xs font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">待入库清单</h3>
          <div className="flex flex-col gap-1 text-sm text-[var(--color-text)]">
            <span>图片 {imageCount} 张</span>
            <span>视频 {videoCount} 个</span>
            <span className="text-xs text-[var(--color-text-secondary)]">共 {items.length} 项 · {formatSize(totalSize)}</span>
          </div>
        </div>

        <div className="flex flex-col gap-3 p-3">
          <label className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">
              分库名称{libraryRoot ? "（总库下新建文件夹）" : "（需先在设置里配置总库）"}
            </span>
            <input
              value={workbench.draft.collection}
              onChange={(event) => void runSafely(() => workbench.updateOptions(event.target.value, workbench.draft.renamePattern))}
              disabled={!libraryRoot || workbench.submitting || conversationSwitching}
              placeholder="如：旅行"
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-40"
            />
          </label>
          <RenameBuilder
            value={workbench.draft.renamePattern}
            onChange={(value) => void runSafely(() => workbench.updateOptions(workbench.draft.collection, value))}
            disabled={!libraryRoot || workbench.submitting || conversationSwitching}
            collection={workbench.draft.collection.trim()}
            sampleStem={items[0] ? displayBasename(items[0].displayName).replace(/\.[^.]+$/, "") : ""}
            sampleExt={items[0]?.displayName.split(".").pop() ?? ""}
          />
          {/* A3-06：默认只授权本次读取。记住只影响「能不能继续读这个目录」，
              不等于入库、写入或外发许可；撤销入口在设置 → 素材库与入库。 */}
          <label className="flex items-start gap-2 text-xs text-[var(--color-text-secondary)]">
            <input
              type="checkbox"
              className="mt-0.5"
              checked={rememberDirectory}
              onChange={(event) => setRememberDirectory(event.currentTarget.checked)}
            />
            <span>
              记住这个目录（下次不用重新选择）
              <span className="mt-0.5 block text-[11px] text-[var(--color-text-tertiary)]">
                只读，可随时在设置里撤销。
              </span>
            </span>
          </label>
        </div>

        {/* 底部安全区由 aside（import-side-panel）的 pb-32 承担，见上。 */}
        <div className="mt-auto border-t border-[var(--color-border)] p-3" data-testid="import-progress-panel">
          {workbench.preparing ? (
            <Button className="mb-3 w-full" onClick={() => void runSafely(() => workbench.cancelPreparation())}>取消准备</Button>
          ) : items.length > 0 && (
            <div className="mb-3">
              {activeOperations[0]?.canCancel ? (
                <Button className="w-full" disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(activeOperations[0].operationId))}>取消入库</Button>
              ) : operationInProgress ? (
                <Button className="w-full" disabled>正在处理…</Button>
              ) : (
                <>
                  <Button
                    variant="primary"
                    className="w-full"
                    disabled={unsupportedCount > 0 || workbench.draft.pendingPlanRefs.length !== Object.keys(workbench.plans).length}
                    onClick={() => void submitAll()}
                  >
                    开始入库（{items.length}）
                  </Button>
                  <Button
                    className="mt-1 w-full"
                    disabled={submissionUncertain}
                    onClick={() => void runSafely(() => workbench.clearPendingPlans())}
                    title={submissionUncertain ? "受理响应待核对，请重试该计划" : undefined}
                  >清空清单</Button>
                </>
              )}
            </div>
          )}
          {busy && items.length === 0 && activeOperations[0]?.canCancel && (
            <Button className="mb-3 w-full" disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(activeOperations[0].operationId))}>取消入库</Button>
          )}
          <ImportProgressPanel task={selectedTask} starting={workbench.preparing} />
        </div>
      </aside>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col p-6">
        {(!settingsLoaded || settingsLoadError || !libraryRoot.trim()) && (
          <div className="mb-3 flex w-full min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-text-secondary)]" role="status">
            <p className="min-w-0 flex-1 break-words leading-5">
              {settingsLoadError ? `设置读取失败：${settingsLoadError}` : !settingsLoaded ? "正在读取设置，请稍候。" : "请先设置素材总库位置，再选择或导入文件。"}
            </p>
            {settingsLoadError ? <Button onClick={() => void loadSettings()}>重试</Button> : settingsLoaded && !libraryRoot.trim() ? (
              <Button className="shrink-0" onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}>前往设置</Button>
            ) : null}
          </div>
        )}

        {combinedError && !workbench.preparing && <p className="mb-3 text-xs text-[var(--color-danger)]" role="alert">{combinedError}</p>}
        {unsupportedCount === 0 && items.some((item) => item.previewStatus === "limited") && (
          <div className="mb-3 w-full min-w-0 rounded-md border border-[var(--color-status)] bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-status)]">
            有 {items.filter((item) => item.previewStatus === "limited").length} 个特殊格式已解析出缩略图，可以导入；但后续高清预览、元数据或 AI 功能可能受限。
          </div>
        )}
        {workbench.draft.warnings.length > 0 && (
          <div className="mb-3 max-w-lg text-xs text-[var(--color-status)]" role="status">
            <p>扫描时跳过或遇到 {workbench.draft.warnings.length} 项，请确认后再入库。</p>
            <button type="button" onClick={() => setShowWarnings((value) => !value)} className="mt-1 text-left underline">
              {showWarnings ? "收起扫描提示" : "查看扫描提示"}
            </button>
            {showWarnings && <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
              {workbench.draft.warnings.map((warning, index) => <li key={`${index}-${warning}`} className="break-all">{warning}</li>)}
            </ul>}
          </div>
        )}

        {workbench.draft.pendingPlanRefs.length > 0 && (
          <div className="mb-3 space-y-1 rounded-md border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-text-secondary)]">
            <p className="text-[var(--color-text-tertiary)]">
              交给助手的是<strong className="font-medium text-[var(--color-text-secondary)]">计划引用</strong>，不会重新准备或提交；
              助手页可按当前对话查看同一份计划。
            </p>
            {workbench.draft.pendingPlanRefs.map(({ planRef, idempotencyKey }) => {
              const plan = workbench.plans[planRef];
              return <div key={planRef} className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate">{workbench.planErrors[planRef] ?? `待确认计划 · ${plan?.itemCount ?? "读取中"} 项${plan?.targetCollection ? ` · 分库「${plan.targetCollection}」` : ""}${plan?.renamePattern ? ` · 命名 ${plan.renamePattern}` : ""}`}</span>
                {idempotencyKey && !plan && <Button disabled={workbench.submitting || conversationSwitching} onClick={() => void runSafely(() => workbench.submitPlan(planRef))}>重试并核对受理</Button>}
                {plan && <Button disabled={conversationSwitching} onClick={() => void runSafely(async () => {
                  await workbench.handoffPlan(planRef);
                  // A3-08：交接是明确动作——交完直接到助手页看这份计划，避免用户再找一次。
                  window.dispatchEvent(new CustomEvent("app:navigate", { detail: "assistant" }));
                })}>交给助手（待确认）</Button>}
              </div>;
            })}
          </div>
        )}

        {/* R19：拖入的文件先当附件停在这里——不预检、不入库、不外发。
            点附件旁的「导入」才进入检查清单（预检结果仍要再确认才入库）。 */}
        {droppedAttachments.length > 0 && (
          <section
            aria-label="待导入附件"
            className="mb-3 space-y-1 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-xs"
          >
            <div className="flex flex-wrap items-center gap-2">
              <span className="min-w-0 flex-1 text-[var(--color-text-secondary)]">
                已拖入 {droppedAttachments.length} 个附件；还没检查、也没入库，点「导入」才开始检查。
              </span>
              {!busy && (
                <Button
                  variant="primary"
                  disabled={workbench.preparing}
                  onClick={() => void runSafely(() => importAttachments(droppedAttachments))}
                >
                  导入全部附件（{droppedAttachments.length}）
                </Button>
              )}
              {!busy && (
                <Button disabled={workbench.preparing} onClick={() => setDroppedAttachments([])}>
                  清空附件
                </Button>
              )}
            </div>
            <ul className="max-h-40 space-y-1 overflow-y-auto">
              {droppedAttachments.map((path) => (
                <li key={path} className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-[var(--color-text-secondary)]" title={path}>
                    {displayBasename(path)}
                  </span>
                  <Button
                    disabled={busy || workbench.preparing}
                    onClick={() => void runSafely(() => importAttachments([path]))}
                  >
                    导入
                  </Button>
                  <Button
                    disabled={busy || workbench.preparing}
                    onClick={() => setDroppedAttachments((current) => current.filter((item) => item !== path))}
                  >
                    移除
                  </Button>
                </li>
              ))}
            </ul>
          </section>
        )}

        {items.length > 0 ? (
          <div className="flex min-h-0 flex-1 flex-col gap-3">
            {latestOutcome && <div className="shrink-0">{latestOutcome}</div>}
            <PendingList
              items={items}
              running={busy || submissionUncertain}
              onRemove={() => undefined}
              onRemoveCandidate={(item) => void removeItem(item)}
              onAddFiles={() => void chooseFiles()}
              onAddFolder={() => void chooseFolder()}
              onClear={() => void runSafely(() => workbench.clearPendingPlans())}
              onOpenItem={(path) => void openFileExternal(path).catch(() => undefined)}
            />
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
            {latestOutcome}

            <div className={clsx(
              "flex min-h-[180px] flex-1 flex-col items-center justify-center gap-3 rounded-xl border-2 border-dashed px-4 transition-colors",
              dragOver ? "border-[var(--color-accent)] bg-[var(--color-surface)]" : "border-[var(--color-border)]",
            )}>
              <p className="text-base text-[var(--color-text)]">
                {latestOperation ? "继续拖入图片 / 视频" : "把图片 / 视频拖到这里"}
              </p>
              {!latestOperation && (
                <p className="max-w-xl text-center text-sm leading-6 text-[var(--color-text-secondary)]">
                  支持文件夹递归，先入清单再确认；重复文件按内容识别，不会覆盖原素材。
                </p>
              )}
              {!busy && <div className="flex gap-2">
                <Button variant="primary" onClick={() => void chooseFiles()}>选择文件…</Button>
                <Button onClick={() => void chooseFolder()}>选择文件夹…</Button>
              </div>}
              {/* A3-06：已记住的目录必须有直接入口，否则「记住」只是一句空承诺。 */}
              {!busy && rememberedGrants.length > 0 && (
                <div className="flex max-w-md flex-wrap items-center justify-center gap-1.5" aria-label="已记住的导入目录">
                  <span className="text-xs text-[var(--color-text-tertiary)]">已记住的目录：</span>
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
                      className="ui-control rounded-full px-2.5 py-0.5 text-xs disabled:opacity-40"
                    >
                      {grant.displayName}
                      {!grant.available && "（已不可用）"}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {olderOperationRefs.length > 0 && (
          <section className="mt-3 space-y-1" aria-label="历史入库操作">
          {olderOperationRefs.map((operationRef) => {
              const operation = operationSnapshots[operationRef];
              if (!operation) return null;
              const result = workbench.operationResults[operationRef];
              const resultError = workbench.operationResultErrors[operationRef];
              const resultLoading = workbench.operationResultLoading[operationRef] ?? false;
              const failures = result?.failures ?? [];
              return <div key={operationRef} className="rounded-md border border-[var(--color-border)] px-3 py-2 text-xs">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="min-w-0 flex-1 text-[var(--color-text-secondary)]">
                    {operationLabel(operation)} · 已处理 {operation.completed}/{operation.total ?? "?"} · 失败 {operation.failed}
                  </span>
                  {operation.canCancel && <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.cancel(operationRef))}>取消此操作</Button>}
                  {operation.canResume && <Button disabled={conversationSwitching} onClick={() => void runSafely(() => workbench.resume(operationRef))}>继续此操作</Button>}
                  <Button disabled={conversationSwitching} onClick={() => void runSafely(async () => {
                    await workbench.handoffOperation(operationRef);
                    window.dispatchEvent(new CustomEvent("app:navigate", { detail: "assistant" }));
                  })}>交给助手（已受理）</Button>
                </div>
                {result && <p className="mt-1 text-[var(--color-text-secondary)]">新入库 {result.imported} · 重复 {result.duplicates} · 失败 {result.failed}</p>}
                {/* 重复项要和失败一样说得清楚：清单里并列显示时看不出哪些会和库里撞上，
                    入库完成后必须点名，否则用户不知道「重复 1」指的是哪一个。 */}
                {result && result.duplicateItems.length > 0 && <details className="mt-1 text-[var(--color-text-secondary)]" data-testid="import-duplicates">
                  <summary className="cursor-pointer">查看重复项（{result.duplicateItems.length}）</summary>
                  <ul className="mt-1 max-h-36 space-y-1 overflow-y-auto">
                    {result.duplicateItems.map((item) => <li key={item.itemRef}>{item.displayName}</li>)}
                  </ul>
                </details>}
                {resultError && <div className="mt-1 flex flex-wrap items-center gap-2 text-[var(--color-status)]" role="alert">
                  <span>结果明细读取失败：{resultError}</span>
                  <Button disabled={resultLoading || conversationSwitching} onClick={() => void workbench.loadOperationResult(operationRef, operation.revision)}>
                    {resultLoading ? "正在读取结果…" : "重试读取结果"}
                  </Button>
                </div>}
                {failures.length > 0 && <details className="mt-2 text-[var(--color-status)]">
                  <summary className="cursor-pointer">查看异常明细（{failures.length}）</summary>
                  <ul className="mt-1 max-h-36 space-y-1 overflow-y-auto">
                    {failures.map((failure) => <li key={failure.itemRef} className="break-all">
                      {failure.displayName} · {failure.state === "outcome_unknown" ? "结果未知" : "失败"}{failure.errorCode ? ` · ${failure.errorCode}` : ""}
                    </li>)}
                  </ul>
                </details>}
              </div>;
            })}
          </section>
        )}
      </div>
    </div>
  );
}
