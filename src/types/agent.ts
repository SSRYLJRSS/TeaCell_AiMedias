/** Wire shapes only. References must be resolved and authorized by the host. */
export type ResourceRef = string;

export type { CapabilityId, OperationState, OperationPhase, PlanReviewStatus, AgentErrorCode } from './agent.generated';
export type { SearchAssetsInput, PrepareImportInput, PrepareTaggingInput, PrepareExportInput, PrepareMoveInput, PrepareSelectionInput, ApprovePlanInput, LoadWorkflowInput, PrepareWorkflowInput, WorkflowInputs } from './agent.records.generated';
export type { SubmitPlanInput, OperationInput, LibraryScope } from './agent.records.generated';
export type { CapabilityInputs, CapabilityOutputs, AssetSummary, SearchPage, PlanPreview, OperationSnapshot, ApprovalResult, WorkflowSummary } from './agent.records.generated';
export type { WorkflowParameter, WorkflowStepSummary, WorkflowPlanPreview, SubmissionResult, PlanScopeSummary, BudgetSummary, MoneyLimit } from './agent.records.generated';
