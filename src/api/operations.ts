/** 操作取消/恢复与启动恢复摘要（对应 commands/operations_cmd.rs）。 */
import { invoke } from "./client";
import type { OperationSnapshot } from "@/types/plans";

export interface RecoverySummary {
  operationRef: string;
  pendingSteps: number;
  runningSteps: number;
  unknownItems: number;
  requiresResume: boolean;
}

export function cancelOperation(
  operationRef: string,
): Promise<OperationSnapshot> {
  return invoke<OperationSnapshot>("cancel_operation", { operationRef });
}

/** 只有可信主界面在用户明确点击继续后调用。 */
export function resumeOperation(
  operationRef: string,
): Promise<OperationSnapshot> {
  return invoke<OperationSnapshot>("resume_operation", { operationRef });
}

export function listOperationRecovery(): Promise<RecoverySummary[]> {
  return invoke<RecoverySummary[]>("list_operation_recovery");
}
