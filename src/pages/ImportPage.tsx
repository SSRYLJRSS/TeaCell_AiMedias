/** 入库页（PRD v2.6 编排层）：左侧任务栏承载入库进度与操作，右侧为拖拽区/清单。
 *  进度统一消费 taskStore；同一页的底栏不再重复绘制入库任务。 */
import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import { open as pickFiles, open as pickDir } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import ProgressBar from "@/components/common/ProgressBar";
import PendingList, { formatSize } from "@/components/import/PendingList";
import RenameBuilder from "@/components/import/RenameBuilder";
import { displayBasename } from "@/utils/pathDisplay";
import { newImportTaskId, openFileExternal } from "@/api/import";
import {
  cancelImport,
  importFiles,
  inspectImport,
  type ImportPlanItem,
  type ImportPlan,
} from "@/api/import";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  clearActiveImportTask,
  importPhaseLabel,
  latestImportTask,
  markImportCancelling,
  useTaskStore,
  type TaskItem,
} from "@/stores/taskStore";
import type { ImportResult } from "@/types/asset";

// 与后端 utils/mime.rs asset_type_from_ext 白名单同步（选择器过滤，拖拽入口由后端扫描过滤）
const FILE_FILTERS = [
  {
    name: "图片与视频",
    extensions: [
      "jpg", "jpeg", "png", "gif", "webp", "bmp", "tga", "tif", "tiff", "heic", "heif",
      "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng",
      "raf", "orf", "rw2", "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff",
      "kdc", "dcr", "mos", "mef", "erf",
      "mp4", "mov", "avi", "mkv", "webm", "m4v", "mts", "m2ts",
    ],
  },
];

async function ensureImportLibraryRoot(setError: (message: string) => void): Promise<boolean> {
  let settings = useSettingsStore.getState();
  if (!settings.loaded || settings.loadError) {
    await settings.load();
    settings = useSettingsStore.getState();
  }
  if (settings.loadError) {
    setError(`设置读取失败：${settings.loadError}`);
    return false;
  }
  if (!settings.settings?.libraryRoot.trim()) {
    setError("请先在设置中配置并保存总库位置，再选择或导入文件。");
    return false;
  }
  return true;
}

function waitForNextFrame(): Promise<void> {
  return new Promise((resolve) => {
    if (typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(() => resolve());
    } else {
      window.setTimeout(resolve, 0);
    }
  });
}

export default function ImportPage() {
  const refreshLibrary = useLibraryStore((s) => s.refresh);
  const libraryRoot = useSettingsStore((s) => s.settings?.libraryRoot ?? "");
  const settingsLoaded = useSettingsStore((s) => s.loaded);
  const settingsLoadError = useSettingsStore((s) => s.loadError);
  const loadSettings = useSettingsStore((s) => s.load);

  const [plan, setPlan] = useState<ImportPlan | null>(null);
  // W5f-f2：导入失败明细展开状态
  const [showImportErrors, setShowImportErrors] = useState(false);
  const [collection, setCollection] = useState("");
  const [renamePattern, setRenamePattern] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [result, setResult] = useState<ImportResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [scanReview, setScanReview] = useState<ImportPlan | null>(null);
  const [scanWarnings, setScanWarnings] = useState<string[]>([]);
  const [showScanWarnings, setShowScanWarnings] = useState(false);
  const [running, setRunning] = useState(false);
  const [startingMessage, setStartingMessage] = useState("正在准备入库");
  const importTask = useTaskStore((s) => latestImportTask(s.tasks));
  const activeImportTask = importTask && !importTask.done ? importTask : undefined;
  const importBusy = running || Boolean(activeImportTask);

  useEffect(() => {
    if (!settingsLoaded) void loadSettings();
  }, [settingsLoaded, loadSettings]);

  const mergePlan = useCallback((scanned: ImportPlan) => {
    if (scanned.items.length === 0) return;
    setPlan((prev) => {
      if (!prev) return scanned;
      const known = new Set(prev.items.map((item) => item.path));
      const fresh = scanned.items.filter((item) => !known.has(item.path));
      const items = [...prev.items, ...fresh];
      return summarizePlan(items, [...new Set([...prev.warnings, ...scanned.warnings])]);
    });
  }, []);

  /** 选文件/拖文件 → 只生成清单，不入库（PRD v2.4 手动确认）；追加期间保留旧清单 */
  const stage = useCallback(
    async (paths: string[]) => {
      if (paths.length === 0 || importBusy) return;
      setError(null);
      setResult(null);
      if (!(await ensureImportLibraryRoot(setError))) return;
      const taskId = newImportTaskId();
      setStartingMessage("正在检查文件与缩略图");
      setRunning(true);
      try {
        // 让左侧唯一进度区先绘制“检查中”，避免紧接着的 IPC 启动让反馈挤到首批缩略图之后。
        await waitForNextFrame();
        const scanned = await inspectImport(paths, taskId);
        if (scanned.warnings.length > 0) {
          setScanWarnings((prev) => [...new Set([...prev, ...scanned.warnings])]);
        }
        if (scanned.items.length === 0) {
          if (scanned.warnings.length === 0) setError("未发现可入库的图片/视频文件");
          return;
        }
        if (scanned.items.some((item) => item.previewStatus === "unsupported")) {
          // 不把无法生成缩略图的文件静默放入正式队列；用户确认后再合并可用项。
          setScanReview(scanned);
          return;
        }
        mergePlan(scanned);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setRunning(false);
      }
    },
    [importBusy, mergePlan],
  );

  const confirmScanReview = () => {
    if (!scanReview) return;
    const allowed = scanReview.items.filter((item) => item.previewStatus !== "unsupported");
    mergePlan(summarizePlan(allowed, scanReview.warnings));
    setScanReview(null);
    if (allowed.length === 0 && !plan?.items.length) {
      setError("无法解析的文件已剔除，当前没有可导入项");
    }
  };

  const cancelScanReview = () => {
    setScanReview(null);
    setError(null);
  };

  // Tauri 原生拖拽（获取真实文件路径）
  useEffect(() => {
    let un: (() => void) | undefined;
    let cancelled = false;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        if (e.payload.type === "over") setDragOver(true);
        else if (e.payload.type === "leave") setDragOver(false);
        else if (e.payload.type === "drop") {
          setDragOver(false);
          void stage(e.payload.paths);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else un = fn;
      });
    return () => {
      cancelled = true;
      un?.();
    };
  }, [stage]);

  const choose = async () => {
    if (importBusy) return;
    if (!(await ensureImportLibraryRoot(setError))) return;
    const picked = await pickFiles({ multiple: true, filters: FILE_FILTERS });
    if (Array.isArray(picked)) void stage(picked);
    else if (typeof picked === "string") void stage([picked]);
  };

  /** 添加文件夹：目录选择器，同样走 stage(paths) */
  const chooseFolder = async () => {
    if (importBusy) return;
    if (!(await ensureImportLibraryRoot(setError))) return;
    const picked = await pickDir({ directory: true, multiple: true });
    if (picked && Array.isArray(picked)) void stage(picked);
    else if (typeof picked === "string") void stage([picked]);
  };

  const removeItem = (path: string) =>
    setPlan((prev) => {
      if (!prev) return prev;
      const items = prev.items.filter((i) => i.path !== path);
      return summarizePlan(items, prev.warnings);
    });

  const clearPlan = () => {
    if (importBusy) return;
    setPlan(null);
    setScanWarnings([]);
  };

  /** 手动确认入库 */
  const run = async () => {
    if (!plan || plan.items.length === 0 || importBusy) return;
    if (!(await ensureImportLibraryRoot(setError))) return;
    if (plan.items.some((item) => item.previewStatus === "unsupported")) {
      setScanReview(plan);
      return;
    }
    setRunning(true);
    setStartingMessage("正在准备入库");
    setError(null);
    setResult(null);
    try {
      const r = await importFiles(
        plan.items.map((i) => i.path),
        newImportTaskId(),
        { collection: collection.trim() || undefined, renamePattern: renamePattern.trim() || undefined },
      );
      setResult(r);
      setPlan(null);
      setScanWarnings([]);
      void refreshLibrary();
      void useMetadataStore.getState().refresh();
    } catch (e) {
      clearActiveImportTask();
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="flex h-full">
      <Modal
        open={Boolean(scanReview)}
        title="有文件无法生成缩略图"
        onClose={cancelScanReview}
        footer={(
          <>
            <Button onClick={cancelScanReview}>取消添加</Button>
            <Button variant="primary" onClick={confirmScanReview}>剔除并保留可导入项</Button>
          </>
        )}
      >
        <p className="text-sm text-[var(--color-text)]">
          以下 {scanReview?.items.filter((item) => item.previewStatus === "unsupported").length ?? 0} 个文件无法解析入库缩略图，当前版本禁止导入。
          是否将它们从本次队列中剔除，并保留其余可导入文件？
        </p>
        <ul className="mt-3 max-h-40 space-y-1 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2 text-xs text-[var(--color-text-secondary)]">
          {scanReview?.items.filter((item) => item.previewStatus === "unsupported").map((item) => (
            <li key={item.path} className="break-all" title={item.previewMessage}>{item.path}</li>
          ))}
        </ul>
      </Modal>
      {/* 左侧任务栏：统计 + 选项 + 开始/清空（运行中改为取消） */}
      <aside className="flex w-[180px] shrink-0 flex-col border-r border-[var(--color-border)]">
        <div className="border-b border-[var(--color-border)] p-3">
          <h3 className="mb-2 text-xs font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">
            待入库清单
          </h3>
          <div className="flex flex-col gap-1 text-sm text-[var(--color-text)]">
            <span>图片 {plan?.images ?? 0} 张</span>
            <span>视频 {plan?.videos ?? 0} 个</span>
            <span className="text-xs text-[var(--color-text-secondary)]">
              共 {plan?.items.length ?? 0} 项 · {formatSize(plan?.totalSize ?? 0)}
            </span>
          </div>
        </div>

        <div className="flex flex-col gap-3 p-3">
          <label className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-text-secondary)]">
              分库名称{libraryRoot ? "（总库下新建文件夹）" : "（需先在设置里配置总库）"}
            </span>
            <input
              value={collection}
              onChange={(e) => setCollection(e.target.value)}
              disabled={!libraryRoot}
              placeholder="如：旅行"
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-40"
            />
          </label>
          <RenameBuilder
            value={renamePattern}
            onChange={setRenamePattern}
            disabled={!libraryRoot}
            collection={collection.trim()}
            sampleStem={
            plan?.items[0]?.path
              ? displayBasename(plan.items[0].path).replace(/\.[^.]+$/, "")
              : ""
          }
            sampleExt={plan?.items[0]?.path.split(".").pop() ?? ""}
          />
        </div>

        <div className="mt-auto border-t border-[var(--color-border)] p-3">
          {plan && plan.items.length > 0 && (
            <div className="mb-3">
              {importBusy ? (
                <Button
                  className="w-full"
                  onClick={() => {
                    markImportCancelling();
                    void cancelImport();
                  }}
                >
                  取消入库
                </Button>
              ) : (
                <>
                  <Button variant="primary" className="w-full" onClick={() => void run()}>
                    开始入库（{plan.items.length}）
                  </Button>
                  <Button className="mt-1 w-full" onClick={clearPlan}>
                    清空清单
                  </Button>
                </>
              )}
            </div>
          )}
          {importBusy && (!plan || plan.items.length === 0) && (
            <Button
              className="mb-3 w-full"
              onClick={() => {
                markImportCancelling();
                void cancelImport();
              }}
            >
              取消检查
            </Button>
          )}
          <ImportProgressPanel
            task={running && !activeImportTask ? undefined : importTask}
            starting={running && !activeImportTask}
            startingMessage={startingMessage}
          />
        </div>
      </aside>

      {/* 右侧：拖拽区 / 清单 + 失败明细（完成计数只在左侧进度面板展示） */}
      <div className="flex min-w-0 flex-1 flex-col p-6">
        {(!settingsLoaded || settingsLoadError || !libraryRoot.trim()) && (
          <div className="mb-3 flex max-w-2xl flex-wrap items-center gap-2 rounded-md border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-text-secondary)]" role="status">
            <p className="min-w-0 flex-1">
              {!settingsLoaded ? "正在读取设置，请稍候。" : settingsLoadError ? `设置读取失败：${settingsLoadError}` : "请先在设置中配置并保存总库位置，再选择或导入文件。"}
            </p>
            {settingsLoadError ? (
              <Button onClick={() => void loadSettings()}>重试</Button>
            ) : !libraryRoot.trim() ? (
              <Button variant="primary" onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}>前往设置</Button>
            ) : null}
          </div>
        )}
        {/* W5f-f2：导入失败明细 —— 首行摘要 + 可展开全量清单 */}
        {result && result.errors.length > 0 && (
          <div className="mb-3 max-w-lg text-xs text-[var(--color-danger)]">
            <button
              type="button"
              onClick={() => setShowImportErrors((v) => !v)}
              className="text-left"
            >
              {showImportErrors ? "收起失败明细" : "查看失败明细"}
            </button>
            {showImportErrors && (
              <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
                {result.errors.map((e, i) => (
                  <li key={i} className="break-all">{e}</li>
                ))}
              </ul>
            )}
          </div>
        )}
        {scanWarnings.length > 0 && (
          <div className="mb-3 max-w-lg text-xs text-[var(--color-status)]" role="status">
            <p>扫描时跳过或遇到 {scanWarnings.length} 项，请在导入前确认。</p>
            <button
              type="button"
              onClick={() => setShowScanWarnings((v) => !v)}
              className="mt-1 text-left underline"
            >
              {showScanWarnings ? "收起扫描提示" : "查看扫描提示"}
            </button>
            {showScanWarnings && (
              <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
                {scanWarnings.map((warning, i) => (
                  <li key={`${i}-${warning}`} className="break-all">{warning}</li>
                ))}
              </ul>
            )}
          </div>
        )}
        {result && result.warnings.length > 0 && (
          <div className="mb-3 max-w-lg text-xs text-[var(--color-status)]">
            <button
              type="button"
              onClick={() => setShowImportErrors((v) => !v)}
              className="text-left"
            >
              {showImportErrors ? "收起扫描警告" : `查看扫描警告（${result.warnings.length}）`}
            </button>
            {showImportErrors && (
              <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface)] p-2">
                {result.warnings.map((warning, i) => (
                  <li key={i} className="break-all">{warning}</li>
                ))}
              </ul>
            )}
          </div>
        )}
        {error && !running && !error.startsWith("请先在设置中配置并保存总库位置") && <p className="mb-3 text-xs text-[var(--color-danger)]">{error}</p>}

        {plan?.items.some((item) => item.previewStatus === "limited") && (
          <div className="mb-3 w-full min-w-0 rounded-md border border-[var(--color-status)] bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-status)]">
            有 {plan.items.filter((item) => item.previewStatus === "limited").length} 个特殊格式已解析出缩略图，可以导入；但后续高清预览、元数据或 AI 功能可能受限。
          </div>
        )}

        {plan && plan.items.length > 0 ? (
          <PendingList
            items={plan.items}
            running={importBusy}
            onRemove={removeItem}
            onAddFiles={choose}
            onAddFolder={chooseFolder}
            onClear={clearPlan}
            onOpenItem={(p) => void openFileExternal(p).catch(() => undefined)}
          />
        ) : (
          <div
            className={clsx(
              "flex flex-1 flex-col items-center justify-center gap-3 rounded-xl border-2 border-dashed transition-colors",
              dragOver ? "border-[var(--color-accent)] bg-[var(--color-surface)]" : "border-[var(--color-border)]",
            )}
          >
            <p className="text-base text-[var(--color-text)]">把图片 / 视频拖到这里</p>
            <p className="text-sm text-[var(--color-text-secondary)]">
              支持文件夹递归，重复文件自动识别；选中后先入清单，手动确认才入库
            </p>
            {!importBusy && (
              <div className="flex gap-2">
                <Button variant="primary" onClick={choose}>
                  选择文件…
                </Button>
                <Button onClick={chooseFolder}>选择文件夹…</Button>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function summarizePlan(items: ImportPlanItem[], warnings: string[] = []): ImportPlan {
  return {
    items,
    images: items.filter((item) => item.kind === "image").length,
    videos: items.filter((item) => item.kind === "video").length,
    totalSize: items.reduce((sum, item) => sum + item.size, 0),
    warnings,
  };
}

/** 左侧入库进度：阶段、总体百分比、当前文件与真实结果计数都在同一处展示。 */
function ImportProgressPanel({ task, starting, startingMessage }: { task?: TaskItem; starting: boolean; startingMessage: string }) {
  const detail = task?.importProgress;
  const idle = !task && !starting;
  const indeterminate = starting || Boolean(task && task.overall == null && !task.done);
  const percent = task?.overall == null ? null : Math.round(task.overall * 100);
  const phase = detail ? importPhaseLabel(detail.phase) : startingMessage;
  const phaseText = task?.detail?.startsWith("取消中") ? task.detail : phase;
  const showResults = detail?.phase === "previewing" || detail?.phase === "done";
  const phaseProgress =
    detail?.phase === "scanning"
      ? `已发现 ${detail.phaseCurrent} 项`
      : detail?.phase === "checking"
        ? `已检查 ${detail.phaseCurrent}/${detail.phaseTotal ?? "?"} 项`
      : detail?.phaseTotal != null && detail.phaseTotal > 0
        ? `当前阶段 ${detail.phaseCurrent}/${detail.phaseTotal}`
        : null;
  const hasErrors = Boolean(detail && detail.failed > 0);
  const taskError = task?.error && !hasErrors ? task.error : null;

  return (
    <section aria-label="入库进度" className="min-w-0" data-state={idle ? "idle" : task?.done ? "done" : "running"}>
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-xs font-medium text-[var(--color-text)]">入库进度</h3>
        <span className={clsx("shrink-0 text-[11px]", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text-secondary)]")}>
          {idle ? "等待入库" : indeterminate || percent == null ? "准备中" : `${percent}%`}
        </span>
      </div>
      {(!task?.done || percent != null) && <ProgressBar value={task?.overall ?? 0} indeterminate={indeterminate} className="mt-2" />}
      {!idle && (
        <p className={clsx("mt-2 text-xs", hasErrors ? "text-[var(--color-status)]" : "text-[var(--color-text)]")} aria-live="polite">
          {phaseText}
        </p>
      )}
      {detail?.file && (
        <p className="mt-1 truncate text-[11px] text-[var(--color-text-secondary)]" title={detail.file}>
          {detail.file}
        </p>
      )}
      {phaseProgress && <p className="mt-1 text-[11px] text-[var(--color-text-tertiary)]">{phaseProgress}</p>}
      {showResults && detail && (
        <div className="mt-2 flex flex-wrap gap-x-2 gap-y-1 text-[11px] text-[var(--color-text-secondary)]">
          <span>成功 {detail.imported}</span>
          <span>重复 {detail.duplicates}</span>
          <span className={detail.failed > 0 ? "text-[var(--color-danger)]" : undefined}>
            失败 {detail.failed}
          </span>
        </div>
      )}
      {taskError && <p className="mt-1 text-[11px] text-[var(--color-status)]">{taskError}</p>}
    </section>
  );
}
