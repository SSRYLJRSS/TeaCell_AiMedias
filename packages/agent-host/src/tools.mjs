import { Type } from 'typebox';

// `typebox` is pinned transitively by @earendil-works/pi-coding-agent 1.0.4
// (the same import its built-in tools use) and ships with the Host resource.
// Do not declare or upgrade it separately: the Pi tool schema and this Host
// must validate arguments with the same TypeBox version.

/**
 * Model-facing tool definitions. Names and capability ids are derived from the
 * frozen contract (docs/contracts/agent-v1.json, `tea_` + capability id) and the
 * Rust host validates the declared list exactly; adding a tool here without the
 * matching Rust adapter fails the handshake instead of silently degrading.
 *
 * The list below mirrors `SUPPORTED_PI_CAPABILITIES` in
 * src-tauri/src/services/agent_runtime/capabilities.rs. It deliberately omits
 * `tea_plans_approve`, `tea_operations_resume` and `tea_trash_empty_*`: those
 * are GUI-only entries and the model must never be able to approve, resume or
 * permanently destroy. `imports.*` also has no adapter yet (explicit gap; see
 * the Rust constant's doc comment and A3.md item 4).
 */

const RESOURCE_REF = Type.String({
  description: '后端返回的引用标识（以 resource_ / plan_ / selection_ 等前缀开头的不可读字符串），必须原样回传',
  minLength: 16,
  maxLength: 128,
  pattern: '^[A-Za-z0-9_-]+$',
});

const SEARCH_PARAMETERS = Type.Object({
  text: Type.String({
    description: '要在本地素材库中搜索的关键词；传空字符串表示列出当前素材库的全部素材',
    maxLength: 4096,
  }),
}, { additionalProperties: false });

const OPERATION_PARAMETERS = Type.Object({
  operationId: RESOURCE_REF,
}, { additionalProperties: false });

const SELECTION_PREPARE_PARAMETERS = Type.Object({
  selectionRef: RESOURCE_REF,
}, { additionalProperties: false });

const SUBMIT_PARAMETERS = Type.Object({
  planId: RESOURCE_REF,
  idempotencyKey: RESOURCE_REF,
}, { additionalProperties: false });

const EXPORT_PREPARE_PARAMETERS = Type.Object({
  selectionRef: RESOURCE_REF,
  destinationRef: RESOURCE_REF,
  layout: Type.Union([
    Type.Literal('flat'),
    Type.Literal('by_tag'),
    Type.Literal('by_date'),
  ], { description: '平铺 / 按标签分目录 / 按日期分目录' }),
}, { additionalProperties: false });

const MOVE_PREPARE_PARAMETERS = Type.Object({
  selectionRef: RESOURCE_REF,
  collectionRef: RESOURCE_REF,
}, { additionalProperties: false });

const TAGGING_PREPARE_PARAMETERS = Type.Object({
  selectionRef: RESOURCE_REF,
  serviceRef: RESOURCE_REF,
  instructions: Type.String({
    description: '对本次打标的自然语言要求，例如「重点识别茶类与器型」；不能改变预算、分类表和模型',
    maxLength: 8192,
  }),
}, { additionalProperties: false });

const WORKFLOW_LOAD_PARAMETERS = Type.Object({
  workflowRef: RESOURCE_REF,
  version: Type.Integer({ minimum: 1, maximum: 4294967295 }),
}, { additionalProperties: false });

const WORKFLOW_PREPARE_PARAMETERS = Type.Object({
  workflowRef: RESOURCE_REF,
  version: Type.Integer({ minimum: 1, maximum: 4294967295 }),
  inputs: Type.Object({
    sourceRef: Type.Optional(RESOURCE_REF),
    selectionRef: Type.Optional(RESOURCE_REF),
    collectionRef: Type.Optional(RESOURCE_REF),
    destinationRef: Type.Optional(RESOURCE_REF),
    serviceRef: Type.Optional(RESOURCE_REF),
    taggingInstructions: Type.Optional(Type.String({ maxLength: 8192 })),
    renamePattern: Type.Optional(Type.String({ maxLength: 256 })),
  }, { additionalProperties: false }),
}, { additionalProperties: false });

/**
 * One tool = one frozen capability. `execute` only forwards the model arguments
 * plus the capability id; the Rust host injects the schema version, the caller
 * identity and every authority field, and re-validates the payload.
 */
function defineTool(capabilityId, definition) {
  return {
    name: `tea_${capabilityId.replaceAll('.', '_')}`,
    executionMode: 'parallel',
    ...definition,
  };
}

export function createPiTools({ capabilityRpc }) {
  if (!capabilityRpc || typeof capabilityRpc.exchange !== 'function') {
    throw new TypeError('CAPABILITY_TRANSPORT_UNAVAILABLE');
  }
  const tools = [
    defineTool('assets.search', {
      label: '搜索素材',
      description: '按关键词搜索本地素材库（只读）。返回本页素材的引用与名称、结果总数和代表完整结果集的 selectionRef；后续准备计划必须使用该引用，而不是重新猜测范围。',
      parameters: SEARCH_PARAMETERS,
    }),
    defineTool('tagging.prepare', {
      label: '准备打标计划',
      description: '把 selectionRef 代表的结果集整理成一份待审的打标计划，绑定到用户当前已绑定的打标服务。不发起任何外发、不产生费用；计划必须由用户在界面批准后才能提交。',
      parameters: TAGGING_PREPARE_PARAMETERS,
    }),
    defineTool('tagging.submit', {
      label: '提交打标计划',
      description: '提交一份已由用户批准的打标计划。提交后只会冻结批次并停在「待开始」，真正的模型外发仍需用户在打标界面手动开始，你无法代替用户开始。',
      parameters: SUBMIT_PARAMETERS,
    }),
    defineTool('exports.prepare', {
      label: '准备导出计划',
      description: '把 selectionRef 整理成一份导出到已授权目录的待审计划。不写任何文件，计划必须由用户批准。若用户还没在界面里授权目标目录，这里会返回未授权类错误，此时应提示用户先去导出界面选择目录。',
      parameters: EXPORT_PREPARE_PARAMETERS,
    }),
    defineTool('exports.submit', {
      label: '提交导出计划',
      description: '提交一份已由用户批准的导出计划。复制仍在导出界面（或常用流程）里执行，提交本身只建立操作记录。',
      parameters: SUBMIT_PARAMETERS,
    }),
    defineTool('assets.move.prepare', {
      label: '准备库内移动',
      description: '把 selectionRef 整理成一份移到库内某个集合目录的待审计划（同库移动，不是复制）。计划必须由用户批准。',
      parameters: MOVE_PREPARE_PARAMETERS,
    }),
    defineTool('assets.move.submit', {
      label: '提交库内移动',
      description: '提交一份已由用户批准的库内移动计划。移动会重新核对源文件身份，源文件被改动时会被拒绝。',
      parameters: SUBMIT_PARAMETERS,
    }),
    defineTool('assets.trash.prepare', {
      label: '准备移入回收站',
      description: '把 selectionRef 代表的结果集整理成一份待审计划（不删除任何素材）。计划必须在界面中由用户批准，之后才能用 tea_assets_trash_submit 提交；模型无法自行批准。',
      parameters: SELECTION_PREPARE_PARAMETERS,
    }),
    defineTool('assets.trash.submit', {
      label: '提交回收站计划',
      description: '提交一份已由用户在界面批准的计划（软删入回收站，可恢复）。未批准、已过期或已被改动的计划会被拒绝；同一幂等键重复提交只产生一次效果。',
      parameters: SUBMIT_PARAMETERS,
    }),
    defineTool('assets.restore.prepare', {
      label: '准备从回收站恢复',
      description: '把 selectionRef 代表的结果集整理成一份恢复计划（从回收站还原到原位置）。计划必须由用户批准，永久删除不在此工具范围内。',
      parameters: SELECTION_PREPARE_PARAMETERS,
    }),
    defineTool('assets.restore.submit', {
      label: '提交恢复计划',
      description: '提交一份已由用户批准的恢复计划，把素材从回收站还原回原位置。',
      parameters: SUBMIT_PARAMETERS,
    }),
    defineTool('operations.get', {
      label: '查询操作进度',
      description: '按 operationId 查询当前会话自己提交的操作进度（只读）。只能看到自己创建的操作；不会返回其他会话或其他主体的操作。',
      parameters: OPERATION_PARAMETERS,
    }),
    defineTool('operations.cancel', {
      label: '取消操作',
      description: '按 operationId 请求取消当前会话自己提交的操作。只取消尚未完成的剩余项，已经完成的项不会回滚；需要用户批准的效果仍由 GUI 决定。',
      parameters: OPERATION_PARAMETERS,
    }),
    defineTool('workflows.load', {
      label: '读取常用流程',
      description: '读取一个已保存的常用流程版本（只读），返回它的名称、说明、参数和步骤能力。用它确认流程再决定是否实例化。',
      parameters: WORKFLOW_LOAD_PARAMETERS,
    }),
    defineTool('workflows.prepare', {
      label: '准备流程实例',
      description: '用给定流程版本和运行时输入生成一份待审的流程实例计划。只是冻结步骤范围，不执行任何步骤；需要用户批准的步骤仍会在各自界面单独确认。',
      parameters: WORKFLOW_PREPARE_PARAMETERS,
    }),
    defineTool('workflows.submit', {
      label: '提交流程实例',
      description: '提交一份已由用户批准的流程实例计划。执行由流程运行器驱动，提交本身不直接改文件。',
      parameters: SUBMIT_PARAMETERS,
    }),
  ];
  return tools.map(tool => ({
    ...tool,
    async execute(_toolCallId, params, signal) {
      const capabilityId = tool.name.slice('tea_'.length).replaceAll('_', '.');
      return exchange(capabilityRpc, capabilityId, params, signal);
    },
  }));
}

async function exchange(capabilityRpc, capabilityId, params, signal) {
  try {
    const result = await capabilityRpc.exchange(capabilityId, params, { signal });
    return {
      content: [{ type: 'text', text: JSON.stringify(result) }],
      details: result,
    };
  } catch (error) {
    const code = typeof error?.code === 'string' ? error.code : 'SERVICE_UNAVAILABLE';
    return {
      content: [{ type: 'text', text: `工具执行失败：${code}` }],
      details: { code },
      isError: true,
    };
  }
}

/** Exact names this Host registers; the Rust side validates this list on ready. */
export const PI_TOOL_NAMES = Object.freeze([
  'tea_assets_move_prepare',
  'tea_assets_move_submit',
  'tea_assets_restore_prepare',
  'tea_assets_restore_submit',
  'tea_assets_search',
  'tea_assets_trash_prepare',
  'tea_assets_trash_submit',
  'tea_exports_prepare',
  'tea_exports_submit',
  'tea_operations_cancel',
  'tea_operations_get',
  'tea_tagging_prepare',
  'tea_tagging_submit',
  'tea_workflows_load',
  'tea_workflows_prepare',
  'tea_workflows_submit',
]);

/**
 * A3-10: 校验 `conversation.open` 下发的「本次会话注册的工具集合」。
 *
 * Host **不复制**档位表：档位是 Rust 的概念，Host 只接受一个非空、无重复、
 * 且属于注册集合的工具名子集。任何越界取值都失败关闭，而不是回落到全集——
 * 回落等于把「Rust 写错了」静默变成「模型拿到了更多工具」。
 */
export function normalizeSessionToolNames(value) {
  if (!Array.isArray(value) || value.length === 0) throw new Error('INVALID_TOOL_SET');
  const names = [];
  for (const entry of value) {
    if (typeof entry !== 'string' || !PI_TOOL_NAMES.includes(entry)) {
      throw new Error('INVALID_TOOL_SET');
    }
    if (names.includes(entry)) throw new Error('INVALID_TOOL_SET');
    names.push(entry);
  }
  return names;
}
