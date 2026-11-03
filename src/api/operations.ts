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

/**
 * 明确「开始执行」一个已批准并已提交、还停在待执行状态的操作。
 *
 * 批准与提交只登记计划与操作，不产生任何文件效果；真正的复制/移动/恢复/清空/
 * 流程执行必须由用户在这里点一次。后端会自己还原创建者身份并校验队列队首，
 * 前端不做任何权限猜测——被拒绝就直接把错误给用户看。
 */
export function startOperation(
  operationRef: string,
): Promise<OperationSnapshot> {
  return invoke<OperationSnapshot>("start_operation", { operationRef });
}
