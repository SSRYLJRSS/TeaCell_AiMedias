/** AI 连接档案命令封装（对应 commands/ai_connections_cmd.rs，指导书 §6.3） */
import { invoke } from "./client";

export type AiDeployment = "cloud" | "local";
/** §6.4 协议：apiMode → protocol 迁移后的取值 */
export type AiProtocol = "openai_chat" | "anthropic_messages";

/** 凭据三态（B3）：configured=已配置且可读；missing=未配置；unavailable=系统密钥服务不可用/锁定。 */
export type CredentialStatus = "configured" | "missing" | "unavailable";

export interface AiConnection {
  id: string;
  name: string;
  deployment: AiDeployment;
  protocol: AiProtocol;
  baseUrl: string;
  model: string;
  maxConcurrency: number;
  requestsPerMinute: number;
  requestsPerHour: number;
  /** 兼容旧 UI：等价 credentialStatus === "configured"。UI 三态请以 credentialStatus 为准。 */
  hasKey: boolean;
  /** B3 三态凭据状态。 */
  credentialStatus: CredentialStatus;
  /** 仅 unavailable 时的可执行文案（不含密钥值）。 */
  credentialMessage?: string | null;
  enabled: boolean;
}

export type AiUsage = "super_search" | "tagging";

export interface SuperSearchServiceResolution {
  ready: boolean;
  source: "explicitBinding" | "automaticOnline";
  connectionId: string | null;
  name: string | null;
  model: string | null;
  deployment: AiDeployment | null;
  message: string | null;
}

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
  maxConcurrency?: number;
  requestsPerMinute?: number;
  requestsPerHour?: number;
}): Promise<AiConnection> {
  return invoke<AiConnection>("save_ai_connection", {
    id: input.id,
    name: input.name,
    deployment: input.deployment,
    protocol: input.protocol,
    baseUrl: input.baseUrl,
    model: input.model,
    apiKey: input.apiKey,
    maxConcurrency: input.maxConcurrency,
    requestsPerMinute: input.requestsPerMinute,
    requestsPerHour: input.requestsPerHour,
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

/** 返回超级搜索实际解析的服务信息，不暴露 API Key 或服务地址。 */
export function getSuperSearchServiceResolution(): Promise<SuperSearchServiceResolution> {
  return invoke<SuperSearchServiceResolution>("get_super_search_service_resolution");
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
