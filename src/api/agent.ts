/** Pi Agent Host 会话通道（对应 commands/agent_host_cmd.rs）。
 *
 * 约束（A3-04）：
 * - 调用方不提供文件路径、模型地址或凭据，只传会话引用与文本；
 * - `agent_host_send` 返回 `state:"outcomeUnknown"` 时保留 runId 供精确取消，
 *   调用方不得重放提示词；
 * - Host 事件只是运行期流，不充当持久状态；持久事实来自 operation/plan 快照。
 */
import { invoke, on, type UnlistenFn } from "./client";
import type { AgentPermission } from "@/constants/agentPermission";

/** Host → UI 流的唯一事件名（lib.rs 里 setup 转发 agent_app.emit）。 */
export const AGENT_HOST_EVENT = "agent-host-event";

/** `new` 用于首次建立 Pi 会话文件；`open`/`resume` 要求会话文件已存在。 */
export type AgentOpenMode = "new" | "open" | "resume";

export type AgentRunDispatchState = "accepted" | "outcomeUnknown";

export interface AgentHostStatus {
  hostInstanceId: string;
  protocolVersion: number;
  sdkVersion: string;
  recoveredStrandedHost: boolean;
  modelNetworkEnabled: boolean;
}

export interface AgentConversationStatus {
  hostInstanceId: string;
  conversationRef: string;
  sessionId: string;
}

export interface AgentRunDispatch {
  runId: string;
  state: AgentRunDispatchState;
}

/** 网关安全类别（协议 §6.1 的固定集合；Rust 侧只放行这些值）。
 *
 * 它只说明「模型服务这一跳发生了什么」，不含原始异常、地址、提示词或凭据。
 */
export type GatewayErrorCode =
  | "ROUTE_DENIED"
  | "LEASE_REVOKED"
  | "CONNECTION_CHANGED"
  | "REQUEST_INVALID"
  | "REQUEST_LIMIT"
  | "BUDGET_EXCEEDED"
  | "UPSTREAM_UNAVAILABLE"
  | "UPSTREAM_HTTP"
  | "REDIRECT_DENIED"
  | "STREAM_FAILED"
  | "CANCELLED"
  | "TIMEOUT";

export interface GatewayErrorDetail {
  code: GatewayErrorCode;
  /** 100–599 的 HTTP 状态；只在服务真的回了响应时出现。 */
  status?: number;
}

/** 一轮结束语义。`length` = 因 max_tokens 截断（可续写）；`toolUse` = 等待工具结果；
 *  `aborted` = 用户停止；`error` = 运行失败；`stop` = 正常结束。 */
export type AgentStopReason =
  | "stop"
  | "length"
  | "toolUse"
  | "error"
  | "aborted"
  | "deferred"
  | "pending";

/** provider 上报的 token 用量（仅数字，不伪造）。 */
export interface AgentUsage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
}

/** 思考程度档位（off=不思考）。仅当连接声明支持 reasoning 时下传生效，
 *  未知档位由后端归一化丢弃，不会透传到 provider。 */
export type AgentThinkingLevel =
  | "off"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh";

/** Host 帧：只有 delta/settled/error 会进入 sink（Rust 已过滤迟到帧）。 */
export interface AgentProtocolFrame {
  protocol: number;
  instance: string;
  sequence: number;
  type: "delta" | "settled" | "error";
  conversationId: string;
  runId: string;
  /** delta 专用 */
  text?: string;
  /** settled 专用：被用户停止或运行失败 */
  aborted?: boolean;
  /** settled 专用：本轮结束原因。`length` 表示因 max_tokens 被截断，UI 可提示「因长度停止，可继续」。 */
  stopReason?: AgentStopReason;
  /** settled 专用：provider 原始 finish_reason/stop_reason（有界 ≤64 字符）。 */
  rawStopReason?: string;
  /** settled 专用：provider 上报的 token 用量；provider 未上报时缺省（不伪造 0）。 */
  usage?: AgentUsage;
  /** error 专用：固定错误码分类 */
  code?: string;
  /** error 专用：网关安全类别（仅 RUN_FAILED 可带，A3-06 用于给出具体中文原因）。 */
  gatewayError?: GatewayErrorDetail;
}

/** Host 流结束后的界面状态帧。
 *
 * **不是协议帧**：协议 §7 冻结了 v1 的 delta/settled/error 事件集合，新增帧类型要升级
 * 协议版本，所以这条由 Rust 读线程自己生成、只走 Tauri 事件到达界面，没有 sequence，
 * 也不参与去重。它的作用是让「Host 已中断」立刻可见，而不是等下一次 IPC 失败才发现。
 */
export interface AgentHostStatusFrame {
  type: "host.status";
  instance: string;
  state: "interrupted";
  /** Rust 侧判定 Host 断开的原因码（如 host_exited）。 */
  reason?: string;
  /** 中断时仍有活动回答的会话；没有则为 null。 */
  conversationId?: string | null;
}

export type AgentHostFrame = AgentProtocolFrame | AgentHostStatusFrame;

export function startAgentHost(recoverStrandedHost = false): Promise<AgentHostStatus> {
  return invoke<AgentHostStatus>("agent_host_start", { recoverStrandedHost });
}

export function openAgentConversation(
  conversationRef: string,
  mode: AgentOpenMode,
  permission: AgentPermission,
): Promise<AgentConversationStatus> {
  return invoke<AgentConversationStatus>("agent_host_open_conversation", {
    conversationRef,
    mode,
    permission,
  });
}

export function sendAgentMessage(
  conversationRef: string,
  text: string,
  thinkingLevel?: AgentThinkingLevel,
): Promise<AgentRunDispatch> {
  return invoke<AgentRunDispatch>("agent_host_send", {
    conversationRef,
    text,
    thinkingLevel,
  });
}

export function cancelAgentRun(conversationRef: string, runId: string): Promise<void> {
  return invoke<void>("agent_host_cancel_run", { conversationRef, runId });
}

/** 只读历史项（来自 Pi JSONL 投影，非伪造）。text 为纯文本；stopReason 仅在
 *  助手消息上存在，用于标注「因长度停止」等结束原因。 */
export interface AgentHistoryMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
  stopReason?: AgentStopReason;
}

/** 只读回读一段会话的历史投影。会话未打开时由 Host 只读打开 JSONL 后投影，
 *  不写库、不重放、不产生副作用。 */
export function getAgentConversationHistory(
  conversationRef: string,
): Promise<AgentHistoryMessage[]> {
  return invoke<AgentHistoryMessage[]>("agent_host_conversation_history", {
    conversationRef,
  });
}

export function closeAgentConversation(conversationRef: string): Promise<void> {
  return invoke<void>("agent_host_close_conversation", { conversationRef });
}

export function onAgentHostFrame(
  handler: (frame: AgentHostFrame) => void,
): Promise<UnlistenFn> {
  return on<AgentHostFrame>(AGENT_HOST_EVENT, handler);
}
