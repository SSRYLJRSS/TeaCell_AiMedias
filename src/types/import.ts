/** Import command/event DTOs serialized by Rust with camelCase field names. */
export type ImportPhase =
  | "queued"
  | "scanning"
  | "checking"
  | "hashing"
  | "processing"
  | "previewing"
  | "review"
  | "failed"
  | "cancelled"
  | "done";

export interface ImportProgress {
  taskId: string;
  operationId?: string;
  phase: ImportPhase;
  phaseCurrent: number;
  phaseTotal: number | null;
  file?: string;
  imported: number;
  duplicates: number;
  failed: number;
  message?: string;
}

export interface ImportPlanItem {
  path: string;
  /** UI-only identity fields added when a path-free prepared plan is projected into the list. */
  planRef?: string;
  itemRef?: string;
  displayName?: string;
  kind: "image" | "video";
  size: number;
  previewStatus: "ready" | "limited" | "unsupported";
  previewMessage?: string;
}

export interface ImportPlan {
  items: ImportPlanItem[];
  images: number;
  videos: number;
  totalSize: number;
  warnings: string[];
}

export interface ImportPlanCandidateView {
  itemRef: string;
  displayName: string;
  kind: "image" | "video";
  size: number;
  modifiedAt: number;
  previewStatus: "ready" | "limited" | "unsupported";
  previewMessage?: string;
  renamePreview: string;
  excluded: boolean;
}

export interface PreparedImportPlan {
  planRef: string;
  sourceRef?: string;
  targetCollection?: string;
  renamePattern?: string;
  itemCount: number;
  readyCount: number;
  limitedCount: number;
  unsupportedCount: number;
  totalSize: number;
  expiresAtUnixSeconds: number;
  items: ImportPlanCandidateView[];
}

export interface ImportPlanRequest {
  /** Authorized directory source; mutually exclusive with explicit file paths. */
  sourceRef?: string;
  paths?: string[];
  collection?: string;
  renamePattern?: string;
  /** Rebuild the exact stored candidate set, optionally excluding item refs. */
  basePlanRef?: string;
  excludedItemRefs?: string[];
}

export interface ImportPlanVerification {
  planRef: string;
  staleItemRefs: string[];
  libraryChanged: boolean;
}

export interface ImportResultView {
  imported: number;
  duplicates: number;
  failed: number;
  failures: ImportFailureView[];
}

/** Path-free exception projection for the owning import workbench. */
export interface ImportFailureView {
  itemRef: string;
  displayName: string;
  state: "failed" | "outcome_unknown";
  errorCode?: string;
}

/** Front-end pointer projection only; candidate data and paths remain in Rust. */
export interface ImportWorkbenchDraft {
  schemaVersion: 1;
  collection: string;
  renamePattern: string;
  warnings: string[];
  pendingPlanRefs: Array<{ planRef: string; idempotencyKey?: string }>;
  recentOperationRefs: string[];
}

export function isImportWorkbenchDraft(value: unknown): value is ImportWorkbenchDraft {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<ImportWorkbenchDraft>;
  return candidate.schemaVersion === 1
    && typeof candidate.collection === "string"
    && typeof candidate.renamePattern === "string"
    && (candidate.warnings === undefined || Array.isArray(candidate.warnings) && candidate.warnings.every((entry) => typeof entry === "string"))
    && Array.isArray(candidate.pendingPlanRefs)
    && candidate.pendingPlanRefs.every((entry) =>
      Boolean(entry)
      && typeof entry.planRef === "string"
      && (entry.idempotencyKey === undefined || typeof entry.idempotencyKey === "string"),
    )
    && Array.isArray(candidate.recentOperationRefs)
    && candidate.recentOperationRefs.every((entry) => typeof entry === "string");
}

export const EMPTY_IMPORT_WORKBENCH_DRAFT: ImportWorkbenchDraft = {
  schemaVersion: 1,
  collection: "",
  renamePattern: "",
  warnings: [],
  pendingPlanRefs: [],
  recentOperationRefs: [],
};
