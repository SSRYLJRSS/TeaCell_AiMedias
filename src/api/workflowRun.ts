/** 常用流程的「本次实例化」（对应 A4-08，后端命令 prepare/run/cancel_workflow_instance）。
 *
 *  常用流程只是**模板**：每次复用都要按本次重新提供输入、重新准备一份新计划、
 *  再由用户确认执行。这个模块因此只做三件事：
 *  1. 把「本次输入」原样交给后端准备新计划（前端不缓存上一次的值）；
 *  2. 把执行结果翻译成可读的中文，且没有一步允许被说成「全部成功」；
 *  3. 明确标出「必须由用户在自己的界面单独确认」的步骤 —— 流程不代批。
 */
import { AppError, invoke } from "./client";

/** 本次要填的输入。模板里没有这些值，只保存「这里要填什么」。 */
export interface WorkflowInstanceInputs {
  sourceRef?: string | null;
  selectionRef?: string | null;
  collectionRef?: string | null;
  destinationRef?: string | null;
  serviceRef?: string | null;
  taggingInstructions?: string | null;
  renamePattern?: string | null;
}

export interface WorkflowInstanceStepPreview {
  ordinal: number;
  capability: string;
  label: string;
  /** true = 该步必须由用户在自己的界面单独确认，流程不会代批。 */
  requiresSeparateAction: boolean;
  dependsOn: number | null;
  itemCount: number;
  note: string | null;
}

export interface WorkflowInstancePreviewView {
  planId: string;
  reviewRef: string;
  workflowRef: string;
  version: number;
  workflowName: string;
  summary: string;
  expiresAtUnixSeconds: number;
  steps: WorkflowInstanceStepPreview[];
  warnings: string[];
}

export interface WorkflowRunStepResult {
  ordinal: number;
  capability: string;
  label: string;
  state: "succeeded" | "failed" | "skipped" | "cancelled" | "outcome_unknown";
  message: string | null;
  done: number;
  failed: number;
  total: number;
}

export interface WorkflowRunReport {
  planId: string;
  operationId: string;
  operationState:
    | "queued"
    | "running"
    | "succeeded"
    | "partially_succeeded"
    | "failed"
    | "cancelled"
    | "outcome_unknown";
  steps: WorkflowRunStepResult[];
  summary: string;
}

/** 受理回执：「确认并开始」返回的不是终态，而是一个稳定的 operation 句柄。 */
export interface WorkflowRunStart {
  planId: string;
  operationId: string;
  operationState: WorkflowRunReport["operationState"];
  /** true = 同一份计划此前已提交过，这次幂等命中，没有新建长任务。 */
  replayed: boolean;
  canCancel: boolean;
}

/** 受控的运行时输入键：与后端 `WorkflowParameterKey` 一一对应。 */
export const WORKFLOW_INPUT_KEYS = [
  "sourceRef",
  "selectionRef",
  "collectionRef",
  "destinationRef",
  "serviceRef",
  "taggingInstructions",
  "renamePattern",
] as const;

export type WorkflowInputKey = (typeof WORKFLOW_INPUT_KEYS)[number];

export function isWorkflowInputKey(key: string): key is WorkflowInputKey {
  return (WORKFLOW_INPUT_KEYS as readonly string[]).includes(key);
}

export type WorkflowInputValues = Partial<Record<WorkflowInputKey, string | null>>;

/**
 * 为一次「再次使用」准备**新计划**。
 *
 * 传的是本次现场选的输入；后端据此生成 planId 与 reviewRef，旧计划不会被改写。
 */
export function prepareWorkflowInstance(
  workflowRef: string,
  version: number,
  inputs: WorkflowInstanceInputs,
): Promise<WorkflowInstancePreviewView> {
  return invoke<WorkflowInstancePreviewView>("prepare_workflow_instance", {
    workflowRef,
    version,
    inputs,
  });
}

/**
 * 用户点了「确认并开始」：受理并立刻返回稳定句柄，**不等整次执行跑完**。
 *
 * 为什么必须这样：旧实现要等全部步骤跑完才返回 report，前端在那之前拿不到
 * operationId，于是「运行中取消」在物理上不可能 —— 取消按钮只在结果里出现。
 */
export function runWorkflowInstance(planId: string): Promise<WorkflowRunStart> {
  return invoke<WorkflowRunStart>("run_workflow_instance", { planId });
}

/** 轮询一次本次运行的真实状态；进度、取消后的残留项都由它读出。 */
export function fetchWorkflowRunReport(planId: string): Promise<WorkflowRunReport> {
  return invoke<WorkflowRunReport>("workflow_run_instance_report", { planId });
}

/**
 * 这次运行是否已经结束。
 *
 * `queued` / `running` 之外都算结束。为什么要单独成一个函数：轮询必须只由它决定
 * 停止，「结果看起来不对」不是停止条件——否则用户会看到一个永远停在「正在执行」
 * 的对话框，而任务其实早就结束了。
 */
export function isRunFinished(state: string): boolean {
  return (
    state === "succeeded" ||
    state === "partially_succeeded" ||
    state === "failed" ||
    state === "cancelled" ||
    state === "outcome_unknown"
  );
}

/** 运行中的状态文案：排队与执行要分开说，避免「卡住」和「排队」混为一谈。 */
export function runPhaseLabel(state: string): string {
  if (state === "queued") return "已受理，正在排队等待执行";
  if (state === "running") return "正在执行，请稍候";
  return "已受理";
}

/** 取消一次正在执行/结果未知的任务；已完成的步骤保持原样，不做隐式回滚。 */
export function cancelWorkflowInstance(operationId: string): Promise<void> {
  return invoke<void>("cancel_workflow_instance", { operationId });
}

const STEP_STATE_LABELS: Record<WorkflowRunStepResult["state"], string> = {
  succeeded: "已成功",
  failed: "未成功",
  skipped: "已跳过",
  cancelled: "已取消",
  outcome_unknown: "结果未知",
};

/** 逐步骤状态的中文说法。未知取值原样回显，便于排查而不是假装成功。 */
export function stepStateLabel(state: WorkflowRunStepResult["state"]): string {
  return STEP_STATE_LABELS[state] ?? state;
}

/** 预览与结果里的「第 N 步」：后端若给了正数序号就用它，否则退回位置。 */
function stepNumber(step: { ordinal: number }, index: number): number {
  return Number.isFinite(step.ordinal) && step.ordinal > 0 ? step.ordinal : index + 1;
}

/** 每条步骤一行：`第 N 步「label」：状态：message`。 */
export function runSummaryLines(report: WorkflowRunReport): string[] {
  return report.steps.map((step, index) => {
    const head = `第 ${stepNumber(step, index)} 步「${step.label}」：${stepStateLabel(step.state)}`;
    return step.message ? `${head}：${step.message}` : head;
  });
}

/** 这类步骤的审批留在哪个界面 —— 文案要说得出用户该去哪儿，而不是「去别处」。 */
const SEPARATE_ACTION_PLACE: Record<string, string> = {
  "imports.submit": "入库页",
  "tagging.submit": "打标界面",
  "exports.submit": "导出界面",
  "assets.move.submit": "移动界面",
  "assets.trash.submit": "回收站界面",
  "assets.restore.submit": "回收站界面",
  "trash.empty.submit": "回收站界面",
};

/**
 * 必须单独确认的步骤要提前说清楚，而不是等执行时才让用户发现少了什么。
 *
 * 流程能做的只有准备与执行受控步骤；这一步的正式批准仍留在用户自己的界面里，
 * 所以文案必须点名「哪一步、去哪个界面」，否则「已确认」会被误解成整条流程都批了。
 */
export function requiresSeparateNotice(view: WorkflowInstancePreviewView): string | null {
  const sentences: string[] = [];
  view.steps.forEach((step, index) => {
    if (!step.requiresSeparateAction) return;
    const place = SEPARATE_ACTION_PLACE[step.capability] ?? "对应界面";
    sentences.push(
      `第 ${stepNumber(step, index)} 步「${step.label}」需要您在${place}单独确认，流程不会替您批准。`,
    );
  });
  return sentences.length === 0 ? null : sentences.join(" ");
}

/**
 * 确认按钮为什么不能点：返回中文原因，可以点则返回 null。
 *
 * 三类必须拦在前端（后端也会拒，但用户不该靠报错才知道）：
 * 来源目录只能去入库页选；素材库没选中就没有素材范围；AI 打标缺服务不能开始。
 */
export function confirmBlockedReason(
  view: { runtimeInputs: { key: string; label: string; required: boolean }[]; steps: { capability: string }[] },
  values: WorkflowInputValues,
  selectionCount: number,
): string | null {
  const keys = new Set(view.runtimeInputs.map((input) => input.key));
  if (keys.has("sourceRef")) return "请先在入库页选择来源目录";
  if (keys.has("selectionRef") && selectionCount === 0) return "先去素材库选择素材";
  const needsTagging = view.steps.some((step) => step.capability === "tagging.submit");
  const serviceRef = (values.serviceRef ?? "").trim();
  if (needsTagging && serviceRef.length === 0) return "需要先在设置里配置 AI 服务";
  for (const input of view.runtimeInputs) {
    if (!input.required) continue;
    // 来源目录与素材集合由上面的专门规则覆盖（素材集合在准备时才快照）。
    if (input.key === "sourceRef" || input.key === "selectionRef") continue;
    if (!isWorkflowInputKey(input.key)) continue;
    const value = (values[input.key] ?? "").trim();
    if (value.length === 0) return `请先填写「${input.label}」`;
  }
  return null;
}

/** 失败原因的统一兜底：AppError 用自己的 message，其它 Error 转文本。 */
export function errorMessage(error: unknown): string {
  if (error instanceof AppError) return error.message;
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "未知错误";
}
