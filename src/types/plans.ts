/** Rust serde 响应 DTO：计划审核与操作状态。 */
export type PlanReviewStatus =
  | "pending"
  | "approved"
  | "expired"
  | "invalidated"
  | "consumed";

export type ReviewKind = "ordinary" | "permanent_trash";

export interface LibraryScope {
  libraryId: string;
  libraryEpoch: string;
}

export interface PlanScopeSummary {
  scopeRef: string;
  sourceRef?: string;
  selectionRef?: string;
  targetRef?: string;
  serviceRef?: string;
  serviceRevision?: string;
  model?: string;
}

export interface BudgetSummary {
  maxRequests: number;
  maxTokens: number;
  maxSeconds: number;
  maxItems: number;
  monetaryLimit?: { currency: string; amountMicrounits: number };
}

export interface PlanPreview {
  schemaVersion: number;
  planId: string;
  capability: string;
  library: LibraryScope;
  reviewStatus: PlanReviewStatus;
  reviewKind: ReviewKind;
  itemCount: number;
  summary: string;
  reviewRef: string;
  expiresAtUnixSeconds: number;
  scope: PlanScopeSummary;
  budget: BudgetSummary;
}

export interface ApprovalResult {
  schemaVersion: number;
  planId: string;
  reviewStatus: PlanReviewStatus;
}

export type OperationState =
  | "queued"
  | "running"
  | "succeeded"
  | "partially_succeeded"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "outcome_unknown";

export type OperationPhase =
  | "ready"
  | "executing"
  | "waiting_review"
  | "waiting_tags"
  | "cancelling"
  | "reconciling"
  | "complete";

export interface OperationSnapshot {
  schemaVersion: number;
  operationId: string;
  library: LibraryScope;
  revision: number;
  state: OperationState;
  phase: OperationPhase;
  completed: number;
  failed: number;
  total?: number;
  resultSelectionRef?: string;
  errorCode?: string;
  canCancel: boolean;
  canResume: boolean;
}
