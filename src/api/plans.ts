/** 计划审核与操作查询（对应 commands/plans_cmd.rs）。 */
import { invoke } from "./client";
import type { ApprovalResult, OperationSnapshot, PlanPreview } from "@/types/plans";

export function listPendingPlans(): Promise<PlanPreview[]> {
  return invoke<PlanPreview[]>("list_pending_plans");
}

/** 只有可信主界面提交审核；reviewRef 必须来自对应计划。 */
export function approvePlan(
  planRef: string,
  reviewRef: string,
): Promise<ApprovalResult> {
  return invoke<ApprovalResult>("approve_plan", { planRef, reviewRef });
}

/** 拒绝同样只由可信主界面发出；拒绝不产生任何写入操作。 */
export function rejectPlan(
  planRef: string,
  reviewRef: string,
): Promise<ApprovalResult> {
  return invoke<ApprovalResult>("reject_plan", { planRef, reviewRef });
}

export function getOperation(operationRef: string): Promise<OperationSnapshot> {
  return invoke<OperationSnapshot>("get_operation", { operationRef });
}
