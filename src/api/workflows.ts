/** 个人常用流程（对应 commands/workflows_cmd.rs，A4-07）
 *
 *  保存的是「允许的步骤 + 参数定义」，不是执行审批、原路径凭证或历史密钥。
 *  所以这里没有任何字段能承载素材 ID、真实路径、批准引用或凭据：后端用
 *  `deny_unknown_fields` + 受控能力枚举把它们拒之门外，前端也不提供输入框。
 */
import { invoke } from "./client";

/** 步骤目录项：唯一被允许出现在常用流程里的动作及其运行时输入。 */
export interface WorkflowStepParameter {
  key: string;
  label: string;
  required: boolean;
  description: string;
}

export interface WorkflowStepCatalogEntry {
  capability: string;
  label: string;
  /** 危险或不可逆步骤的固定提示（例如永久清空回收站）。 */
  warning: string | null;
  parameters: WorkflowStepParameter[];
}

export interface WorkflowParameterView {
  key: string;
  label: string;
  required: boolean;
  description: string;
}

export interface WorkflowStepView {
  capability: string;
  label: string;
  dependsOn: number | null;
  /** ordinary / permanent_trash */
  reviewKind: string;
  warning: string | null;
}

export interface WorkflowRuntimeInput {
  key: string;
  label: string;
  required: boolean;
  hint: string;
}

export interface WorkflowView {
  workflowRef: string;
  version: number;
  name: string;
  description: string;
  archived: boolean;
  updatedAt: number;
  parameters: WorkflowParameterView[];
  steps: WorkflowStepView[];
  /** 这个流程会申请哪些能力，按顺序的中文名。 */
  requires: string[];
  /** 复用时仍然必须现场选择的输入。 */
  runtimeInputs: WorkflowRuntimeInput[];
  /** 存储的步骤已经不能按原样执行时的说明。 */
  needsAttention: string | null;
}

export interface WorkflowStepInput {
  capability: string;
  dependsOn?: number | null;
}

export interface WorkflowParameterInput {
  key: string;
  required: boolean;
  description: string;
}

export interface WorkflowDraftInput {
  name: string;
  description: string;
  steps: WorkflowStepInput[];
  parameters: WorkflowParameterInput[];
}

export interface WorkflowExcludedStep {
  capability: string;
  reason: string;
}

export interface WorkflowDraft {
  sourceOperationRef: string | null;
  name: string;
  description: string;
  steps: WorkflowStepInput[];
  parameters: WorkflowParameterView[];
  requires: string[];
  runtimeInputs: WorkflowRuntimeInput[];
  notes: string[];
  /** 无法合法表达的步骤：给原因，绝不偷偷生成脚本。 */
  excluded: WorkflowExcludedStep[];
}

export interface WorkflowInstantiation {
  workflowRef: string;
  version: number;
  name: string;
}

/** 唯一的步骤白名单。编辑器只从这里取可选项。 */
export function workflowStepCatalog(): Promise<WorkflowStepCatalogEntry[]> {
  return invoke("workflow_step_catalog");
}

export function listWorkflows(
  includeArchived = false,
  nameQuery?: string,
): Promise<WorkflowView[]> {
  return invoke("list_workflows", {
    includeArchived,
    nameQuery: nameQuery?.trim() ? nameQuery.trim() : null,
  });
}

export function getWorkflow(workflowRef: string, version: number): Promise<WorkflowView> {
  return invoke("get_workflow", { workflowRef, version });
}

export function draftWorkflowFromOperation(operationId: string): Promise<WorkflowDraft> {
  return invoke("draft_workflow_from_operation", { operationId });
}

export function createWorkflow(draft: WorkflowDraftInput): Promise<WorkflowView> {
  return invoke("create_workflow", { draft });
}

export function updateWorkflow(
  workflowRef: string,
  expectedVersion: number,
  draft: WorkflowDraftInput,
): Promise<WorkflowView> {
  return invoke("update_workflow", { workflowRef, expectedVersion, draft });
}

export function renameWorkflow(workflowRef: string, name: string): Promise<WorkflowView> {
  return invoke("rename_workflow", { workflowRef, name });
}

export function setWorkflowArchived(
  workflowRef: string,
  archived: boolean,
): Promise<WorkflowView> {
  return invoke("set_workflow_archived", { workflowRef, archived });
}

export function workflowForPlan(planId: string): Promise<WorkflowInstantiation | null> {
  return invoke("workflow_for_plan", { planId });
}

/** 版本号展示：编辑产生新版本，旧版本仍然可读。 */
export function versionLabel(view: WorkflowView): string {
  return `第 ${view.version} 版`;
}

/** 管理列表里的一句话摘要：做什么 + 还要现场选什么。 */
export function workflowSummaryLine(view: WorkflowView): string {
  const steps = view.requires.length > 0 ? view.requires.join(" → ") : "没有步骤";
  const inputs = view.runtimeInputs.filter((input) => input.required).length;
  return inputs > 0 ? `${steps} · 复用时还要选 ${inputs} 项输入` : `${steps} · 复用时无需再选输入`;
}

/** 无法合法表达时的统一说法：说明原因，而不是偷偷生成脚本。 */
export function excludedNotice(draft: WorkflowDraft): string | null {
  if (draft.excluded.length === 0) return null;
  const names = draft.excluded.map((entry) => entry.capability).join("、");
  return `以下步骤无法保存为常用流程：${names}。${draft.excluded[0].reason}`;
}
