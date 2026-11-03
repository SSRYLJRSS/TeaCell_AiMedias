/**
 * skill/v1 的纯逻辑：校验、L1 索引预算、注入文本、编译成 WorkflowDraft、沉淀合成。
 *
 * 本文件不含任何 UI，也不依赖后端；Rust 侧的 STEP_CATALOG 只在前端镜像一份用于
 * 编译期封闭性校验（真正的守门仍在 Rust `services/workflows.rs`）。
 */
import type {
  SkillCapture,
  SkillCompileResult,
  SkillDefinition,
  SkillExcludedStep,
  SkillParam,
  SkillStep,
  SkillSummary,
  SkillValidationResult,
} from "./types";
import type {
  WorkflowParameterInput,
  WorkflowStepInput,
} from "./types";

export const SKILL_SCHEMA_VERSION = 1 as const;
/** L2 正文预算。 */
export const MAX_SKILL_BODY_BYTES = 4096;
/** L1 索引总预算（所有技能概要之和）。 */
export const MAX_SKILL_INDEX_BYTES = 4096;
export const MAX_SKILL_STEPS = 8;
export const MAX_SKILL_PARAMS = 7;
export const MAX_SKILL_NAME_BYTES = 256;
export const MAX_SKILL_DESCRIPTION_BYTES = 1024;
export const SKILL_ID_PATTERN = /^[a-z0-9-]{1,64}$/;

/** 唯一允许出现在 Workflow 参数里的键（与 Rust STEP_CATALOG 的 wire 拼写一致）。 */
export const ALLOWED_PARAM_KEYS = [
  "sourceRef",
  "selectionRef",
  "collectionRef",
  "destinationRef",
  "serviceRef",
  "taggingInstructions",
  "renamePattern",
] as const;
export type AllowedParamKey = (typeof ALLOWED_PARAM_KEYS)[number];

export interface StepCatalogEntry {
  capability: string;
  label: string;
  params: readonly AllowedParamKey[];
  warning: string | null;
}

/**
 * STEP_CATALOG 的前端镜像：仅 7 个 submit 能力可作为流程步骤。
 * `workflows.submit` / `trash.empty.submit` 的界面动作不在此表（避免递归或绕过审批）。
 */
export const STEP_CATALOG_CAPABILITIES: Record<string, StepCatalogEntry> = {
  "imports.submit": {
    capability: "imports.submit",
    label: "入库",
    params: ["sourceRef", "collectionRef", "renamePattern"],
    warning: null,
  },
  "tagging.submit": {
    capability: "tagging.submit",
    label: "AI 打标",
    params: ["selectionRef", "serviceRef", "taggingInstructions"],
    warning: "正式标签仍要经可信审核界面逐条确认",
  },
  "exports.submit": {
    capability: "exports.submit",
    label: "复制导出",
    params: ["selectionRef", "destinationRef"],
    warning: "只复制，不移动也不删除原素材",
  },
  "assets.move.submit": {
    capability: "assets.move.submit",
    label: "库内移动",
    params: ["selectionRef", "collectionRef"],
    warning: null,
  },
  "assets.trash.submit": {
    capability: "assets.trash.submit",
    label: "移入回收站",
    params: ["selectionRef"],
    warning: "移入回收站可恢复，但恢复仍要单独批准",
  },
  "assets.restore.submit": {
    capability: "assets.restore.submit",
    label: "恢复素材",
    params: ["selectionRef"],
    warning: null,
  },
  "trash.empty.submit": {
    capability: "trash.empty.submit",
    label: "永久清空回收站",
    params: ["selectionRef"],
    warning: "永久删除，永远需要单独确认，且只能在界面执行",
  },
};

/** 参数键的默认展示信息（沉淀时自动推导用）。 */
const DEFAULT_PARAM_META: Record<
  AllowedParamKey,
  { label: string; type: SkillParam["type"]; required: boolean; description: string; example?: string }
> = {
  sourceRef: {
    label: "来源目录",
    type: "resourceRef",
    required: true,
    description: "要导入的本地文件夹或相机卡目录",
    example: "D:/素材/2024-婚礼",
  },
  selectionRef: {
    label: "素材集合",
    type: "resourceRef",
    required: true,
    description: "本次操作选中的素材集合",
    example: "当前搜索结果",
  },
  collectionRef: {
    label: "目标分库",
    type: "text",
    required: true,
    description: "素材要放入的分库名称",
    example: "婚礼跟拍",
  },
  destinationRef: {
    label: "导出目标",
    type: "resourceRef",
    required: true,
    description: "导出复制的目标目录",
    example: "E:/交付/客户A",
  },
  serviceRef: {
    label: "AI 服务",
    type: "text",
    required: true,
    description: "用于打标的 AI 服务标识",
    example: "local-vision",
  },
  taggingInstructions: {
    label: "打标要求",
    type: "text",
    required: true,
    description: "给 AI 的打标说明",
    example: "按场景和人物数量打标签",
  },
  renamePattern: {
    label: "重命名规则",
    type: "text",
    required: false,
    description: "可选的文件重命名模板",
    example: "{日期}-{序号}",
  },
};

/** UTF-8 字节数（中文按真实字节计）。 */
export function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).length;
}

function truncateChars(value: string, max: number): string {
  const chars = Array.from(value);
  return chars.length <= max ? value : `${chars.slice(0, max).join("")}…`;
}

function isPermissionTier(value: unknown): boolean {
  return value === "Read" || value === "Standard" || value === "Full";
}

/** 生成 L1 概要（丢弃大字段，便于索引预算控制）。 */
export function toSummary(skill: SkillDefinition): SkillSummary {
  return {
    id: skill.id,
    name: truncateChars(skill.name, 24),
    description: truncateChars(skill.description, 48),
    whenToUse: skill.whenToUse.map((item) => truncateChars(item, 32)).slice(0, 2),
    category: skill.category,
    kind: skill.kind,
    appliesTo: [...skill.appliesTo],
    status: skill.status,
  };
}

/**
 * 序列化真正注入上下文的 L1 索引（紧凑行数组，避免对象键名开销）。
 * 每行 = [id, name(≤16字), category, kind, 权限档(斜杠拼接), 一句适用时机(≤18字)]。
 */
export function serializeSkillIndex(skills: SkillDefinition[]): string {
  const rows = skills.map((skill) => [
    skill.id,
    truncateChars(skill.name, 16),
    skill.category,
    skill.kind,
    skill.appliesTo.join("/"),
    truncateChars(skill.whenToUse[0] ?? skill.description, 18),
  ]);
  return JSON.stringify(rows);
}

/** 生成 L1 概要列表（供 UI）；同时校验紧凑索引不超预算，超限直接抛错。 */
export function buildSkillIndex(skills: SkillDefinition[]): SkillSummary[] {
  const bytes = utf8Bytes(serializeSkillIndex(skills));
  if (bytes > MAX_SKILL_INDEX_BYTES) {
    throw new Error(
      `技能 L1 索引 ${bytes}B 超过预算 ${MAX_SKILL_INDEX_BYTES}B，请精简 name/whenToUse`,
    );
  }
  return skills.map(toSummary);
}

/** 预估紧凑索引字节数（不抛错，供 UI 显示）。 */
export function estimateSkillIndexBytes(skills: SkillDefinition[]): number {
  return utf8Bytes(serializeSkillIndex(skills));
}

const FORBIDDEN_KEY_NAMES = new Set([
  "planId",
  "plan_id",
  "approved",
  "reviewRef",
  "receipt",
  "receipts",
  "assetIds",
  "asset_ids",
  "filePaths",
  "file_paths",
  "credentials",
  "credential",
  "script",
  "shell",
  "exec",
]);

const FORBIDDEN_TEXT_PATTERNS: RegExp[] = [
  /\bplanId\b/i,
  /\bapproved\b/i,
  /\breviewRef\b/i,
  /\breceipts?\b/i,
  /\bcredentials?\b/i,
  /\bapi[_-]?key\b/i,
  /\bpassword\b/i,
  /\bsudo\b/i,
  /\bshell\b/i,
  /\bscript\b/i,
];

/** 扫描禁止字段/禁止词；返回人类可读的错误列表。 */
function findForbiddenContent(skill: SkillDefinition): string[] {
  const hits: string[] = [];
  const walk = (node: unknown, path: string): void => {
    if (node === null || node === undefined) return;
    if (Array.isArray(node)) {
      node.forEach((child, index) => walk(child, `${path}[${index}]`));
      return;
    }
    if (typeof node === "object") {
      for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
        if (FORBIDDEN_KEY_NAMES.has(key)) hits.push(`${path}.${key} 是禁止字段`);
        walk(value, `${path}.${key}`);
      }
      return;
    }
    if (typeof node === "string") {
      for (const pattern of FORBIDDEN_TEXT_PATTERNS) {
        if (pattern.test(node)) hits.push(`${path} 含禁止词 ${String(pattern)}`);
      }
    }
  };
  walk(skill, "skill");
  return hits;
}

/** 校验一份技能定义。 */
export function validateSkillDefinition(skill: SkillDefinition): SkillValidationResult {
  const errors: string[] = [];
  const warnings: string[] = [];

  if (skill.schemaVersion !== SKILL_SCHEMA_VERSION) {
    errors.push(`schemaVersion 必须是 ${SKILL_SCHEMA_VERSION}`);
  }
  if (!SKILL_ID_PATTERN.test(skill.id)) {
    errors.push("id 需匹配 /^[a-z0-9-]{1,64}$/");
  }
  if (!skill.name?.trim()) {
    errors.push("name 不能为空");
  } else if (utf8Bytes(skill.name) > MAX_SKILL_NAME_BYTES) {
    errors.push(`name 超过 ${MAX_SKILL_NAME_BYTES} 字节`);
  }
  if (!skill.description?.trim()) {
    errors.push("description 不能为空");
  } else if (utf8Bytes(skill.description) > MAX_SKILL_DESCRIPTION_BYTES) {
    warnings.push(`description 超过 ${MAX_SKILL_DESCRIPTION_BYTES} 字节，建议精简`);
  }
  if (utf8Bytes(skill.body) > MAX_SKILL_BODY_BYTES) {
    errors.push(`body ${utf8Bytes(skill.body)}B 超过 ${MAX_SKILL_BODY_BYTES}B`);
  }
  if (!skill.whenToUse?.length) {
    errors.push("whenToUse 至少一条");
  }
  if (!skill.appliesTo?.length) {
    errors.push("appliesTo 至少一个权限档");
  } else {
    for (const tier of skill.appliesTo) {
      if (!isPermissionTier(tier)) errors.push(`未知权限档 ${String(tier)}`);
    }
  }
  if (skill.params.length > MAX_SKILL_PARAMS) {
    errors.push(`参数超过 ${MAX_SKILL_PARAMS} 个`);
  }
  const seenParamKeys = new Set<string>();
  for (const param of skill.params) {
    if (!ALLOWED_PARAM_KEYS.includes(param.key as AllowedParamKey)) {
      errors.push(`参数键 ${param.key} 不在白名单`);
    }
    if (seenParamKeys.has(param.key)) errors.push(`参数键 ${param.key} 重复`);
    seenParamKeys.add(param.key);
    if (!param.description?.trim()) errors.push(`参数 ${param.key} 缺少说明`);
  }
  if (skill.steps.length > MAX_SKILL_STEPS) {
    errors.push(`步骤超过 ${MAX_SKILL_STEPS} 个`);
  }
  const stepIds = new Set<string>();
  for (const step of skill.steps) {
    if (!step.id?.trim()) errors.push("存在缺 id 的步骤");
    if (stepIds.has(step.id)) errors.push(`步骤 id ${step.id} 重复`);
    stepIds.add(step.id);
  }
  for (const step of skill.steps) {
    if (step.dependsOn && !stepIds.has(step.dependsOn)) {
      errors.push(`步骤 ${step.id} 依赖的 ${step.dependsOn} 不存在`);
    }
    if (step.capability) {
      const entry = STEP_CATALOG_CAPABILITIES[step.capability];
      if (!entry) {
        if (skill.executableVia === "gui") {
          warnings.push(`步骤 ${step.id} 的能力 ${step.capability} 属界面引导，不产生 WorkflowDraft`);
        } else {
          errors.push(`步骤 ${step.id} 的能力 ${step.capability} 不在常用流程步骤白名单`);
        }
      } else {
        for (const key of step.usesParams ?? []) {
          if (!entry.params.includes(key as AllowedParamKey)) {
            errors.push(`步骤 ${step.id} 使用了 ${step.capability} 不支持的参数 ${key}`);
          }
        }
      }
    } else if (skill.kind === "executable") {
      errors.push(`可执行技能步骤 ${step.id} 缺少 capability`);
    }
  }
  if (skill.kind === "executable" && !skill.executableVia) {
    errors.push("可执行技能需声明 executableVia（workflow / gui）");
  }
  if (skill.kind === "knowledge" && skill.steps.length > 0) {
    warnings.push("知识型技能不应包含可执行步骤，步骤将被忽略");
  }

  errors.push(...findForbiddenContent(skill));
  return { valid: errors.length === 0, errors, warnings };
}

function paramOrder(key: string): number {
  const index = (ALLOWED_PARAM_KEYS as readonly string[]).indexOf(key);
  return index === -1 ? ALLOWED_PARAM_KEYS.length : index;
}

/**
 * 把可执行技能编译成 WorkflowDraft（与 `src/api/workflows.ts` 的 WorkflowDraftInput 同形）。
 * 只做"产物生成 + 封闭性校验"，不调用后端、不生成收据。
 */
export function compileSkillToWorkflowDraft(skill: SkillDefinition): SkillCompileResult {
  const validation = validateSkillDefinition(skill);
  if (!validation.valid) {
    return { ok: false, draft: null, excluded: [], errors: validation.errors };
  }
  if (skill.kind !== "executable") {
    return { ok: false, draft: null, excluded: [], errors: ["知识型技能不编译为流程，只用于注入对话"] };
  }
  if (skill.executableVia === "gui") {
    return {
      ok: false,
      draft: null,
      excluded: skill.steps.map((step) => ({
        capability: step.capability ?? "gui",
        reason: "该技能是界面引导型，不产生 WorkflowDraft，请按正文在界面操作",
      })),
      errors: [],
    };
  }

  const excluded: SkillExcludedStep[] = [];
  const idToIndex = new Map<string, number>();
  const compiledSteps: WorkflowStepInput[] = [];
  const usedParamKeys = new Set<string>();

  skill.steps.forEach((step) => {
    idToIndex.set(step.id, compiledSteps.length);
  });

  for (const step of skill.steps) {
    if (!step.capability || !STEP_CATALOG_CAPABILITIES[step.capability]) {
      excluded.push({
        capability: step.capability ?? "(none)",
        reason: `能力 ${step.capability ?? "(none)"} 不在常用流程步骤白名单内`,
      });
      continue;
    }
    let dependsOn: number | null = null;
    if (step.dependsOn) {
      const target = idToIndex.get(step.dependsOn);
      if (target === undefined || target >= compiledSteps.length) {
        excluded.push({
          capability: step.capability,
          reason: `依赖 ${step.dependsOn} 无法定位到前序步骤`,
        });
        continue;
      }
      dependsOn = target;
    }
    compiledSteps.push({ capability: step.capability, dependsOn });
    for (const key of step.usesParams ?? []) usedParamKeys.add(key);
  }

  if (excluded.length > 0) {
    return { ok: false, draft: null, excluded, errors: ["存在无法编译的步骤"] };
  }

  const parameters: WorkflowParameterInput[] = skill.params
    .filter((param) => usedParamKeys.has(param.key))
    .sort((a, b) => paramOrder(a.key) - paramOrder(b.key))
    .map((param) => ({
      key: param.key,
      required: param.required,
      description: param.description,
    }));

  return {
    ok: true,
    draft: {
      name: skill.name,
      description: skill.description,
      steps: compiledSteps,
      parameters,
    },
    excluded: [],
    errors: [],
  };
}

/**
 * 生成注入对话的文本（@技能 或自动推荐时用）。
 * 只作为"操作参考"注入，不写入任何执行/批准语义。
 */
export function injectionText(skill: SkillDefinition): string {
  const lines: string[] = [
    `【技能】${skill.name}`,
    `用途：${skill.description}`,
    "适用时机：",
    ...skill.whenToUse.map((item) => `- ${item}`),
    "",
    skill.body.trim(),
  ];
  if (skill.params.length > 0) {
    lines.push("", "本技能需要你补充的输入（执行前请确认）：");
    for (const param of skill.params) {
      const required = param.required ? "必填" : "可选";
      lines.push(`- ${param.label}（${param.key}，${required}）：${param.description}`);
    }
  }
  lines.push("", "说明：以上只是操作参考。任何实际执行都要先准备计划并经我确认，批准后再提交。");
  return lines.join("\n");
}

function deriveParam(key: string): SkillParam {
  const meta = DEFAULT_PARAM_META[key as AllowedParamKey];
  if (!meta) throw new Error(`未知参数键 ${key}`);
  return { key, label: meta.label, type: meta.type, required: meta.required, description: meta.description, example: meta.example };
}

/**
 * 把一次成功操作的"能力序列"合成为可执行技能草稿（蒸馏阶段用）。
 * 只生成定义与 WorkflowDraft，不携带任何旧素材、旧目录授权或历史批准。
 */
export function composeSkillFromCapture(input: SkillCapture): SkillDefinition {
  const steps: SkillStep[] = [];
  const paramMap = new Map<string, SkillParam>();
  const approvalPoints = new Set<SkillDefinition["approvalPoints"][number]>();

  input.capabilities.forEach((capability, index) => {
    const entry = STEP_CATALOG_CAPABILITIES[capability];
    if (!entry) {
      throw new Error(`能力 ${capability} 不在常用流程步骤白名单内，无法沉淀为技能`);
    }
    const usesParams = [...entry.params];
    for (const key of usesParams) {
      if (!paramMap.has(key)) paramMap.set(key, deriveParam(key));
    }
    approvalPoints.add("plan");
    const isPermanent = capability === "trash.empty.submit";
    const needsReview = capability === "tagging.submit";
    if (isPermanent) approvalPoints.add("permanentTrash");
    if (needsReview) approvalPoints.add("trustedReview");
    steps.push({
      id: `s${index + 1}`,
      title: entry.label,
      capability,
      usesParams,
      dependsOn: index > 0 ? `s${index}` : undefined,
      review: isPermanent ? "permanent_trash" : "ordinary",
      requiresSeparateAction: isPermanent || needsReview,
    });
  });

  const params = [...paramMap.values()].sort((a, b) => paramOrder(a.key) - paramOrder(b.key));
  const bodyLines: string[] = [
    "## 适用",
    input.description,
    "",
    "## 步骤",
    ...steps.map(
      (step, index) => `${index + 1}. ${step.title}（${step.capability}）：先准备计划，经我确认后再提交。`,
    ),
    "",
    "## 边界",
    "- 本技能由一次成功操作蒸馏而来，执行时仍需重新准备计划并经你确认。",
    "- 不携带旧素材、旧目录授权或任何历史批准。",
  ];

  return {
    schemaVersion: SKILL_SCHEMA_VERSION,
    id: input.id,
    name: input.name,
    description: input.description,
    whenToUse: input.whenToUse.length ? input.whenToUse : [input.description],
    category: "workflow",
    kind: "executable",
    appliesTo: ["Standard", "Full"],
    version: 1,
    status: "user",
    requires: input.capabilities.map((capability) => `tea_${capability.replaceAll(".", "_")}`),
    params: input.params ?? params,
    steps,
    approvalPoints: [...approvalPoints],
    body: bodyLines.join("\n"),
    examples: [],
    executableVia: "workflow",
  };
}
