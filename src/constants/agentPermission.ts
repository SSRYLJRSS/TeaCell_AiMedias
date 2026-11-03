/** 助手会话的权限档位（v2 A3-10）。
 *
 * 语义权威在 `plans/v2/12-implementation-contracts.md` §6.2，这里只是把三档的
 * **用户可见名字与解释**放一处，避免输入区、设置页和测试各写一份文案。
 *
 * 边界：
 * - 档位只决定「模型这一轮能拿到哪层工具」，不决定「能不能外发」，也不改变
 *   任何审批语义：无论哪一档，计划都必须由人在主界面批准，删除仍然单独确认。
 * - 档位是会话级、只在内存里生效；不落 `settings`、不落 `conversations`，
 *   重开会话即按默认档重建，避免宽档被静默沿用。
 * - 顺序必须保持「从窄到宽」：选择器按这个顺序渲染，测试也按它断言单调性。
 */

export type AgentPermission = "read" | "standard" | "full";

/** 默认档：让第一次使用的用户不理解安全模型也能安全用。 */
export const DEFAULT_AGENT_PERMISSION: AgentPermission = "standard";

export interface AgentPermissionOption {
  value: AgentPermission;
  label: string;
  /** 一句话说清「这一档能做什么、不能做什么」，不要复述工具名。 */
  summary: string;
}

export const AGENT_PERMISSION_OPTIONS: readonly AgentPermissionOption[] = Object.freeze([
  {
    value: "read",
    label: "仅阅读",
    summary: "只能搜索素材、查看任务和工作流；不会改动任何素材。",
  },
  {
    value: "standard",
    label: "可执行改动",
    summary: "能打标、导出、从回收站恢复；移动和删除不在这一档。",
  },
  {
    value: "full",
    label: "完全权限",
    summary: "额外允许准备移动和删除；执行前仍需你批准，删除仍会单独确认。",
  },
]);

export function isAgentPermission(value: unknown): value is AgentPermission {
  return value === "read" || value === "standard" || value === "full";
}

/** 未知取值一律按默认档收窄，不把陌生字符串当成宽档。 */
export function normalizeAgentPermission(value: unknown): AgentPermission {
  return isAgentPermission(value) ? value : DEFAULT_AGENT_PERMISSION;
}

export function agentPermissionLabel(value: AgentPermission): string {
  return AGENT_PERMISSION_OPTIONS.find((item) => item.value === value)?.label ?? value;
}
