/** 全局任务条状态（阶段 1 契约，见指导书 §4.5/§4.6）：只由 taskStore 驱动。
 *  入库任务按 taskId 隔离（旧任务事件不污染新任务）；后端只发阶段进度，前端按权重计算整体展示进度。
 *  入库完成（done）后保留最终状态，直到下一次新入库开始；失败保留文字与状态（不纯红）。 */
import { create } from "zustand";
import { onImportProgress, type ImportPhase, type ImportProgress } from "@/api/import";
import { onExportProgress } from "@/api/export";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { logger } from "@/utils/logger";

export type TaskKind = "import" | "export";

export interface ImportTaskProgress {
  phase: ImportPhase;
  phaseCurrent: number;
  phaseTotal: number | null;
  file?: string;
  imported: number;
  duplicates: number;
  failed: number;
}

export interface TaskItem {
  /** 唯一键：入库用 taskId；导出/AI 用 kind */
  id: string;
  kind: TaskKind;
  label: string;
  /** 0..1 整体展示进度；null = 不确定进度（前端显示相位文案 + 不确定条） */
  overall: number | null;
  /** 当前阶段/当前位置文案，如「正在生成快速预览」「12/120」 */
  detail?: string;
  /** 失败/取消文案（有失败项时显示文字与状态符号） */
  error?: string | null;
  /** 已入完成停留期 */
  done?: boolean;
  /** 入库专用明细；导出任务不设置 */
  importProgress?: ImportTaskProgress;
}

interface TaskState {
  tasks: TaskItem[];
}

/** 阶段权重（指导书 §4.5）：只作 UI 展示决策，不写入 Rust，也不作为业务完成条件。 */
const IMPORT_PHASE_WEIGHTS: Partial<Record<ImportPhase, number>> = {
  scanning: 0.05,
  hashing: 0.25,
  processing: 0.5,
  previewing: 0.2,
};
const IMPORT_PHASE_ORDER: ImportPhase[] = ["scanning", "hashing", "processing", "previewing"];

const PHASE_LABELS: Record<ImportPhase, string> = {
  queued: "排队中",
  scanning: "正在扫描目录",
  checking: "正在检查文件与缩略图",
  hashing: "正在计算指纹",
  processing: "正在入库",
  previewing: "正在生成快速预览",
  review: "检查完成，等待确认入库",
  failed: "文件检查或入库失败",
  cancelled: "操作已取消",
  done: "已完成",
};

export function importPhaseLabel(phase: ImportPhase): string {
  return PHASE_LABELS[phase] ?? phase;
}

/** 由阶段进度计算整体展示进度；phaseTotal 未知时返回 null（不确定进度，不伪造百分比）。 */
export function importOverall(phase: ImportPhase, current: number, total: number | null): number | null {
  if (phase === "done") return 1;
  if (!(phase in IMPORT_PHASE_WEIGHTS)) return null;
  const idx = IMPORT_PHASE_ORDER.indexOf(phase);
  let acc = 0;
  for (let i = 0; i < idx; i++) acc += IMPORT_PHASE_WEIGHTS[IMPORT_PHASE_ORDER[i]]!;
  if (total == null || total <= 0) return null;
  const w = IMPORT_PHASE_WEIGHTS[phase]!;
  return Math.min(1, acc + w * (current / total));
}

/** 展示明细：尽量给出「当前文件 / 计数」文本；无则给阶段文案。 */
function importDetail(p: ImportProgress): string {
  const phase = PHASE_LABELS[p.phase] ?? p.phase;
  if (p.file) return `${phase} · ${p.file}`;
  if (p.phaseTotal != null && p.phaseTotal > 0) return `${p.phaseCurrent}/${p.phaseTotal}`;
  return phase;
}

export const useTaskStore = create<TaskState>(() => ({
  tasks: [],
}));

/** 完成条停留时长（毫秒）：让用户看到 100% 后再消失。 */
const LINGER_MS = 1500;
const lingerTimers = new Map<string, ReturnType<typeof setTimeout>>();

function scheduleLinger(id: string) {
  const prev = lingerTimers.get(id);
  if (prev) clearTimeout(prev);
  lingerTimers.set(
    id,
    setTimeout(() => {
      useTaskStore.setState((s) => ({ tasks: s.tasks.filter((t) => t.id !== id) }));
      lingerTimers.delete(id);
    }, LINGER_MS),
  );
}

export function upsertImport(p: ImportProgress) {
  const overall = importOverall(p.phase, p.phaseCurrent, p.phaseTotal);
  const done = p.phase === "done" || p.phase === "review" || p.phase === "failed" || p.phase === "cancelled";
  useTaskStore.setState((s) => {
    const others = s.tasks.filter(
      (t) => t.id !== p.taskId && !(t.kind === "import" && t.done),
    );
    const task: TaskItem = {
      id: p.taskId,
      kind: "import",
      label: "入库",
      overall,
      detail: p.message && !done ? p.message : importDetail(p),
      // 完成/失败时给出明确文案；失败不纯红，用文字+符号表达
      error: p.phase === "failed" || p.phase === "cancelled"
        ? p.message ?? PHASE_LABELS[p.phase]
        : p.phase === "done" && p.failed > 0
          ? `成功 ${p.imported} · 重复 ${p.duplicates} · 失败 ${p.failed}`
          : null,
      done,
      importProgress: {
        phase: p.phase,
        phaseCurrent: p.phaseCurrent,
        phaseTotal: p.phaseTotal,
        file: p.file,
        imported: p.imported,
        duplicates: p.duplicates,
        failed: p.failed,
      },
    };
    return { tasks: [...others, task] };
  });
}

/** 取最近一条入库任务；页面侧栏只消费 taskStore，不重复订阅后端事件。 */
export function latestImportTask(tasks: TaskItem[]): TaskItem | undefined {
  for (let i = tasks.length - 1; i >= 0; i--) {
    if (tasks[i].kind === "import") return tasks[i];
  }
  return undefined;
}

/** 命令级失败不会收到 done 事件，清理未完成任务，避免页面永久保持“进行中”。 */
export function clearActiveImportTask() {
  useTaskStore.setState((s) => {
    for (let i = s.tasks.length - 1; i >= 0; i--) {
      if (s.tasks[i].kind === "import" && !s.tasks[i].done) {
        return { tasks: s.tasks.filter((_, index) => index !== i) };
      }
    }
    return s;
  });
}

function upsertGeneric(key: TaskKind, label: string, done: number, total: number) {
  useTaskStore.setState((s) => {
    const others = s.tasks.filter((t) => t.id !== key);
    const task: TaskItem = {
      id: key,
      kind: key,
      label,
      overall: total > 0 ? done / total : null,
      detail: total > 0 ? `${done}/${total}` : `${done}`,
      done: total > 0 && done >= total,
    };
    return { tasks: [...others, task] };
  });
  if (total > 0 && done >= total) scheduleLinger(key);
}

/** 用户点击取消后标记最近的进行中入库任务为「取消中」（running → cancelling → cancelled）。 */
export function markImportCancelling() {
  useTaskStore.setState((s) => {
    const tasks = [...s.tasks];
    for (let i = tasks.length - 1; i >= 0; i--) {
      if (tasks[i].kind === "import" && !tasks[i].done) {
        tasks[i] = { ...tasks[i], detail: "取消中…" };
        break;
      }
    }
    return { tasks };
  });
}

let subscribed = false;
const unlisteners: UnlistenFn[] = [];

/** 订阅入库/导出进度事件（幂等，App 挂载时调用一次）。
 *  FB6 需求一：AI 打标进度不再进全局任务条——页面内 AiTaggingPage「当前批次」进度条是唯一
 *  AI 进度 UI，由 aiStore.patchProgress（页面内唯一 onAiProgress 订阅）驱动；本 store 只是
 *  入库/导出的事实订阅方。失败可恢复：部分成功时逐个回收已建立监听，复位 subscribed 允许下次惰性重试。 */
export async function startGlobalTaskWatch(): Promise<void> {
  if (subscribed) return;
  subscribed = true;
  try {
    const results = await Promise.allSettled([
      onImportProgress((p) => upsertImport(p)),
      onExportProgress((p) => upsertGeneric("export", "导出中", p.done, p.total)),
    ]);
    const failures = results.filter((r) => r.status === "rejected").length;
    if (failures > 0) {
      for (const r of results) {
        if (r.status === "fulfilled") unlisteners.push(r.value);
      }
      for (const fn of unlisteners.splice(0)) fn();
      subscribed = false;
      logger.error(`全局任务监听订阅失败 ${failures}/2 个事件，已回收并允许重试`);
    }
  } catch (e) {
    subscribed = false;
    logger.error("全局任务监听订阅异常", { error: e instanceof Error ? e.stack : String(e) });
  }
}
