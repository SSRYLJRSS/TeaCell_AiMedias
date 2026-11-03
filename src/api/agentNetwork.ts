/** Agent 网络配置：显式代理（对应 commands/agent_proxy_cmd.rs）。
 *
 * 边界：
 * - 没有「允许联网」开关：绑定服务、服务启用且凭据可用即可直接对话；就绪判定
 *   在后端 `GatewayBroker::preflight`（网络出口 → 服务绑定 → 密钥存在性）。
 * - 代理只支持 http/https 的「主机:端口」，不接受账号密码、路径或查询参数；
 *   严格校验在后端（services::agent_runtime::gateway_http），这里不重复实现。
 */
import { invoke } from "./client";

export interface AgentModelProxyView {
  proxy: string;
}

export function getAgentModelProxy(): Promise<AgentModelProxyView> {
  return invoke<AgentModelProxyView>("get_agent_model_proxy");
}

export function setAgentModelProxy(proxy: string): Promise<AgentModelProxyView> {
  return invoke<AgentModelProxyView>("set_agent_model_proxy", { proxy });
}
