/**
 * Skill（技能 / 操作知识手册）前端契约 —— schema 版本 skill/v1。
 *
 * 分层定位（与设计文档 artifacts/assistant-redesign/skill-library.md 一致）：
 * - Skill：可复用的操作知识 / 手册，只"注入对话"或"编译成流程草稿"，不直接执行。
 * - Workflow：由若干 submit 能力组成、有参数模板、需重新绑定资源与批准的固定步骤流程。
 * - Capability：Agent 可调用的原子工具（16 个 tea_* 工具之一）。
 * - Plan：待审计划，是真实运行快照的回读对象。
 *
 * 安全边界（不可协商）：
 * - Skill 不生成 Operation 收据，不持有 planId / approved / reviewRef / receipt / 旧素材授权。
 * - 知识型技能只有正文；可执行技能只负责"编译成 WorkflowDraft"，真正执行仍走
 *   prepare → 用户批准 → submit，批准 ≠ 收据。
 */
import type {
  WorkflowDraftInput,
  WorkflowParameterInput,
  WorkflowStepInput,
} from "@/api/workflows";

/** 权限档位，与 Rust 侧 `AgentPermission` 对齐（Read/Standard/Full）。 */
export type PermissionTier = "Read" | "Standard" | "Full";

/** 技能分类（用于技能库的分组与筛选）。 */
export type SkillCategory =
  | "ingest"
  | "search"
  | "curation"
  | "tagging"
  | "export"
  | "cleanup"
  | "organize"
  | "workflow"
  | "meta";

/** 技能类型：知识型（只注入正文）/ 可执行型（可编译成流程草稿）。 */
export type SkillKind = "knowledge" | "executable";

/** 技能来源状态：内置 / 用户沉淀 / 已归档。 */
export type SkillStatus = "builtin" | "user" | "archived";

/** 可执行技能的落地方式：编译成 Workflow，或引导用户走界面专属动作。 */
export type SkillExecutableVia = "workflow" | "gui";

/** 参数类型，仅用于表单渲染提示；不影响 wire 协议。 */
export type ParamType = "resourceRef" | "text" | "enum" | "number";

/**
 * 审批点：技能步骤执行前会遇到的确认关卡。
 * - plan：普通计划批准（prepare → 批准 → submit）
 * - permanentTrash：永久删除（trash.empty）单独确认，且永远是界面动作
 * - resumeReview：恢复类操作的可恢复性审阅
 * - trustedReview：打标正式标签需在可信审核界面逐条确认
 * - guiManual：纯界面手动动作（导入、清空回收站、组装流程等）
 */
export type ApprovalPoint =
  | "plan"
  | "permanentTrash"
  | "resumeReview"
  | "trustedReview"
  | "guiManual";

/** 技能参数（键必须是 STEP_CATALOG 白名单里的 wire 拼写）。 */
export interface SkillParam {
  key: string;
  label: string;
  type: ParamType;
  required: boolean;
  description: string;
  example?: string;
}

/** 技能步骤。可执行技能的 capability 应是 STEP_CATALOG 中的 submit 能力。 */
export interface SkillStep {
  id: string;
  title: string;
  /** 该步骤调用的能力 id（如 tagging.submit）；界面引导步骤可为占位 id。 */
  capability?: string;
  /** 该步骤使用哪些参数键（必须是该能力支持的参数）。 */
  usesParams?: string[];
  /** 依赖的上一步 id。 */
  dependsOn?: string;
  /** 审阅类型：普通 / 永久删除。 */
  review?: "ordinary" | "permanent_trash";
  /** 该步骤是否需要单独确认（不可与上一步合并批准）。 */
  requiresSeparateAction?: boolean;
}

/** 技能示例话术与期望结果。 */
export interface SkillExample {
  say: string;
  result: string;
}

/** 技能引用（内部链接 / 文档锚点）。 */
export interface SkillReference {
  label: string;
  href: string;
}

/** 完整技能定义（L2 正文层）。 */
export interface SkillDefinition {
  schemaVersion: 1;
  /** 唯一 id，匹配 /^[a-z0-9-]{1,64}$/。 */
  id: string;
  /** 名称（ShortText ≤ 256 字节）。 */
  name: string;
  /** 一句话说明（≤ 1024 字节）。 */
  description: string;
  /** 何时使用（L1 索引会读取）。 */
  whenToUse: string[];
  category: SkillCategory;
  kind: SkillKind;
  /** 适用于哪些权限档。 */
  appliesTo: PermissionTier[];
  version: number;
  status: SkillStatus;
  /** 涉及的工具名（信息性，便于校验与展示）。 */
  requires: string[];
  params: SkillParam[];
  steps: SkillStep[];
  approvalPoints: ApprovalPoint[];
  /** 正文（Markdown，≤ 4096 字节）。 */
  body: string;
  examples: SkillExample[];
  references?: SkillReference[];
  /** 可执行技能必填。 */
  executableVia?: SkillExecutableVia;
  /** 引导锚点（如 gui:import-workbench），供 UI 定位入口。 */
  guidance?: string;
}

/** L1 索引条目：只含概要，避免塞爆上下文。 */
export interface SkillSummary {
  id: string;
  name: string;
  description: string;
  whenToUse: string[];
  category: SkillCategory;
  kind: SkillKind;
  appliesTo: PermissionTier[];
  status: SkillStatus;
}

/** 校验结果。 */
export interface SkillValidationResult {
  valid: boolean;
  errors: string[];
  warnings: string[];
}

/** 编译时被排除的步骤。 */
export interface SkillExcludedStep {
  capability: string;
  reason: string;
}

/** 可执行技能编译成 WorkflowDraft 的结果。 */
export interface SkillCompileResult {
  ok: boolean;
  draft: WorkflowDraftInput | null;
  excluded: SkillExcludedStep[];
  errors: string[];
}

/** 沉淀技能时的录制输入（来自一次成功操作的能力序列）。 */
export interface SkillCapture {
  id: string;
  name: string;
  description: string;
  whenToUse: string[];
  /** 用户在录制阶段勾选的能力 id。 */
  capabilities: string[];
  /** 可选：显式指定参数（缺省由能力自动推导）。 */
  params?: SkillParam[];
}

/** 供组件复用的公开类型别名（避免各组件重复 import api 类型）。 */
export type { WorkflowDraftInput, WorkflowParameterInput, WorkflowStepInput };
