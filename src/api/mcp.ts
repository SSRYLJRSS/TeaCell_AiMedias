/** 本机外部接入（MCP 桥接）设置面（A5-01，对应 docs/contracts/mcp-bridge-v1.md）。
 *
 * 边界：这里只做「可信设置界面」需要的读写——启用/关闭、运行状态、配对连接的
 * 创建/改名/改范围/撤销。批准计划、永久清空回收站都不经本协议，也不在这个模块里。
 * 字段名与 Rust `#[serde(rename_all = "camelCase")]` 对齐，禁止在前端自造字段。
 */
import { invoke } from "./client";

/** MCP 可发布集合里的一项能力（列表本身就是可发布集合，publishable 当前恒 true）。 */
export type McpCapabilityView = {
  capability: string;
  toolName: string;
  /** query | prepare | submit | cancel */
  access: string;
  description: string;
  publishable: boolean;
};

/** 一条已配对的本机接入连接；`connectionId` 是用户要填进 MCP 客户端配置的唯一标识。 */
export type McpPairingView = {
  connectionId: string;
  label: string;
  principalId: string;
  capabilities: string[];
  createdAt: number;
  updatedAt: number;
  revokedAt: number | null;
  lastUsedAt: number | null;
  revoked: boolean;
};

export type McpBridgeStatusView = {
  enabled: boolean;
  running: boolean;
  platformSupported: boolean;
  instance: string | null;
  pipeName: string | null;
  endpointPath: string | null;
  sessionCount: number;
  maxSessions: number;
  connections: McpPairingView[];
  publishable: McpCapabilityView[];
};

/** 新建连接时默认只勾查询类；写入类必须由用户显式勾选。 */
export const DEFAULT_MCP_CAPABILITIES: readonly string[] = ["assets.search", "operations.get"];

/** 写入类里最容易被误读的一项：勾上它只代表「可以提交」，不代表「有批准权」。 */
export const TRASH_SUBMIT_CAPABILITY = "assets.trash.submit";

export function mcpBridgeStatus(): Promise<McpBridgeStatusView> {
  return invoke<McpBridgeStatusView>("mcp_bridge_status");
}

export function setMcpBridgeEnabled(enabled: boolean): Promise<McpBridgeStatusView> {
  return invoke<McpBridgeStatusView>("set_mcp_bridge_enabled", { enabled });
}

export function createMcpPairing(label: string, capabilities: string[]): Promise<McpPairingView> {
  return invoke<McpPairingView>("create_mcp_pairing", { label, capabilities });
}

export function updateMcpPairing(
  connectionId: string,
  label: string,
  capabilities: string[],
): Promise<McpPairingView> {
  return invoke<McpPairingView>("update_mcp_pairing", { connectionId, label, capabilities });
}

export function revokeMcpPairing(connectionId: string): Promise<McpPairingView> {
  return invoke<McpPairingView>("revoke_mcp_pairing", { connectionId });
}

/** 访问类别翻成人话；未知值原样回显，不擅自归类成「安全」。 */
export function accessLabel(access: string): string {
  switch (access) {
    case "query":
      return "查询";
    case "prepare":
      return "准备计划";
    case "submit":
      return "提交已批准计划";
    case "cancel":
      return "取消任务";
    default:
      return access;
  }
}

function formatLocalTime(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString();
}

/** 连接的可见状态：撤销优先于「最近使用」，避免把历史当成可用连接。 */
export function connectionStatusLabel(view: McpPairingView): string {
  if (view.revoked) return "已撤销";
  if (view.lastUsedAt !== null) return `最近使用：${formatLocalTime(view.lastUsedAt)}`;
  return "尚未使用";
}
