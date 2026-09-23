/** AI 连接档案命令封装（对应 commands/ai_connections_cmd.rs，指导书 §6.3） */
import { invoke } from "./client";

/** cloud = 外部 API（包含 localhost/局域网兼容服务）；local = Windows 托管 Ollama。 */
export type AiDeployment = "cloud" | "local";
/** §6.4 协议：apiMode → protocol 迁移后的取值 */
export type AiProtocol = "openai_chat" | "anthropic_messages";

export interface AiConnection {
  id: string;
  name: string;
  deployment: AiDeployment;
  protocol: AiProtocol;
  baseUrl: string;
  model: string;
  /** 0/undefined = unlimited; limits are enforced only for external online services. */
  maxConcurrency?: number;
  requestsPerMinute?: number;
  requestsPerHour?: number;
  hasKey: boolean;
  enabled: boolean;
}

export type AiUsage = "super_search" | "tagging";

export function listAiConnections(): Promise<AiConnection[]> {
  return invoke<AiConnection[]>("list_ai_connections");
}

/** api_key 为 Some(非空) 时写入系统凭据（keyring）；None/空串保留原密钥。 */
export function saveAiConnection(input: {
  id: string;
  name: string;
  deployment: AiDeployment;
  protocol: AiProtocol;
  baseUrl: string;
  model: string;
  apiKey?: string | null;
  maxConcurrency?: number | null;
  requestsPerMinute?: number | null;
  requestsPerHour?: number | null;
}): Promise<AiConnection> {
  return invoke<AiConnection>("save_ai_connection", {
    id: input.id,
    name: input.name,
    deployment: input.deployment,
    protocol: input.protocol,
    baseUrl: input.baseUrl,
    model: input.model,
    apiKey: input.apiKey,
    maxConcurrency: input.maxConcurrency ?? null,
    requestsPerMinute: input.requestsPerMinute ?? null,
    requestsPerHour: input.requestsPerHour ?? null,
  });
}

export function deleteAiConnection(id: string): Promise<void> {
  return invoke<void>("delete_ai_connection", { id });
}

/** 绑定用途 → 连接（null/空 = 解绑，回退默认档案）。修改一个用途不影响另一个（§6.2）。 */
export function setAiUsageBinding(usage: AiUsage, connectionId: string | null): Promise<void> {
  return invoke<void>("set_ai_usage_binding", { usage, connectionId });
}

export function getAiUsageBindings(): Promise<Record<AiUsage, string | null>> {
  return invoke<Record<AiUsage, string | null>>("get_ai_usage_bindings");
}

/** FB3-08：连接测试结果（后端从 keyring 取密钥，按协议分支测试；错误信息已脱敏） */
export interface AiConnectionTestResult {
  ok: boolean;
  statusCode: number | null;
  latencyMs: number;
  protocol: string;
  model: string;
  message: string;
}

/** FB3-08：测试指定连接（密钥只在 Rust 侧读取，前端只传 connection_id） */
export function testAiConnection(connectionId: string): Promise<AiConnectionTestResult> {
  return invoke<AiConnectionTestResult>("test_ai_connection", { connectionId });
}

/**
 * FB5-04（§3.6）：连接感知模型发现。
 * - connectionId 提供时：密钥从 keyring 读取；显式 apiKey（编辑中的草稿）优先于 keyring；
 *   地址/协议/部署未显式提供时用档案保存值。
 * - 无 connectionId：走 legacy 显式字段（AiTaggingPage 的 settings profile，apiKey 在 JSON 中）。
 * 错误为分类后的可读文案（401/403、404/405、429、超时、网络等），不含密钥。
 */
export function discoverAiModels(input: {
  connectionId?: string | null;
  deployment?: AiDeployment;
  protocol?: AiProtocol;
  baseUrl?: string;
  apiKey?: string;
}): Promise<string[]> {
  return invoke<string[]>("discover_ai_models", {
    connectionId: input.connectionId ?? null,
    deployment: input.deployment ?? null,
    protocol: input.protocol ?? null,
    baseUrl: input.baseUrl ?? null,
    apiKey: input.apiKey ?? null,
  });
}
