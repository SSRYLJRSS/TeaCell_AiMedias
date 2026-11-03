/** 助手会话的运行时投影（v2 A3-04）。
 *
 * 边界（协议 §5、架构 §9.15）：
 * - Pi JSONL 是唯一上下文源；本 store 只在内存里投影「当前进程」的流式消息，
 *   不持久化消息、不伪造历史。重开应用后消息为空，但计划/任务卡一律回读
 *   真实快照（getConversationWork / getOperation），不把模型话术当收据。
 * - 增量按 (hostInstanceId, runId, sequence) 去重：同帧重放不重复追加文本，
 *   旧 hostInstanceId 的迟到帧不得污染当前流式状态，也不得把终态退回 streaming。
 * - 「停止回答」= 取消当前 run；「取消任务」= 取消已入队 operation，两者分开。
 * - 计划/任务归属由后端裁决（`getConversationWork`：显式链接 ∪ `plans.session_id`），
 *   前端不再按「这段时间新增了哪些计划」猜测，也无法指定要认领哪些计划。
 */
import { create } from "zustand";
import {
  cancelAgentRun,
  closeAgentConversation,
  onAgentHostFrame,
  openAgentConversation,
  sendAgentMessage,
  startAgentHost,
  type AgentHostFrame,
  type AgentHostStatus,
  type AgentOpenMode,
  type AgentRunDispatch,
} from "@/api/agent";
import { AppError } from "@/api/client";
import {
  DEFAULT_AGENT_PERMISSION,
  normalizeAgentPermission,
  type AgentPermission,
} from "@/constants/agentPermission";
import { getConversation, getConversationWork } from "@/api/conversations";
import { approvePlan, getOperation, rejectPlan } from "@/api/plans";
import {
  cancelOperation,
  resumeOperation,
  startOperation,
} from "@/api/operations";
import type { OperationSnapshot, PlanPreview } from "@/types/plans";
import {
  AgentErrorCodeValues,
  agentErrorMessages,
  type AgentErrorCode,
} from "@/types/agent.generated";
import { logger } from "@/utils/logger";

export type AgentMessageStatus = "streaming" | "complete" | "aborted" | "failed";

export interface AgentMessage {
  /** 稳定 id：同一 run 的增量始终落到同一条消息上，避免重复气泡。 */
  id: string;
  role: "user" | "assistant";
  text: string;
  runId: string;
  status: AgentMessageStatus;
  errorCode?: string;
}

/** 重要异常单列：不混进消息气泡，也不会被后续正常消息覆盖。 */
export interface AgentAnomaly {
  id: string;
  conversationRef: string;
  runId?: string;
  code: string;
  message: string;
  atUnixMs: number;
}

export type AgentHostState =
  | "idle"
  | "starting"
  | "ready"
  | "interrupted"
  /** 资源缺失或平台不支持：重试无用，必须换环境/重装。 */
  | "unavailable"
  | "error";
export type AgentRuntimeStatus = "idle" | "opening" | "ready" | "streaming" | "unknown";

export interface ConversationRuntime {
  messages: AgentMessage[];
  status: AgentRuntimeStatus;
  activeRunId: string | null;
  /** 正在请求停止的 run（按钮据此禁用，避免重复点击）。 */
  stoppingRunId: string | null;
  /** "instance|runId" → 已应用的最大 sequence。 */
  lastSequence: Record<string, number>;
  /**
   * runId → 已到达的终态（settled/error）。
   *
   * 为什么单列：「本 run 是否结束」不能用「当前有没有活动 run」或「状态是不是
   * ready/idle」推断 —— ACK 先到、帧还没来时这两者都成立。而 settled 先到且
   * 没有 delta 时又不产生助手消息，光看消息状态也判不出来。这里只记录协议明确
   * 的终态事实，供 send 做「本 run 是否已经收敛」的对账。
   *
   * 可选字段：旧快照/测试字面量没有它时按「无终态」处理。
   */
  finishedRuns?: Record<string, AgentMessageStatus>;
  openMode: AgentOpenMode | null;
  /**
   * 这条运行时是在哪个 Host 实例上打开的。
   *
   * Host 重启后实例 id 会变，旧会话在 Host 侧已经不存在；沿用旧记录直接发送
   * 只会得到 CONVERSATION_NOT_OPEN。可选字段：旧快照没有它时按「未打开」处理。
   */
  openedOn?: string | null;
  /**
   * 这条运行时是按哪一档权限打开的（A3-10）。
   *
   * Rust 只在 `conversation.open` 时裁剪工具表，中途换档会留下旧表，所以档位
   * 变化必须关闭后重开；这里记住「当时的档位」，供 `ensureOpen` 判断要不要重开。
   * 可选字段：旧快照没有它时按「未记录」处理，`ensureOpen` 会保守重开。
   */
  openedPermission?: AgentPermission | null;
  error: string | null;
}

export interface AgentHostView {
  state: AgentHostState;
  /** 当前 Host 实例 id；用于丢弃旧实例的迟到帧。 */
  instanceId: string | null;
  info: AgentHostStatus | null;
  error: string | null;
}

interface AgentState {
  host: AgentHostView;
  runtimes: Record<string, ConversationRuntime>;
  /** 会话 → 已归属的计划/操作引用（来自 conversation_links，重开后仍可恢复）。 */
  linkedPlans: Record<string, string[]>;
  linkedOperations: Record<string, string[]>;
  /** A3-08：会话 → 已交接的素材集合快照引用（只读集合，不随素材库选择变动）。 */
  linkedSelections: Record<string, string[]>;
  plans: Record<string, PlanPreview>;
  operations: Record<string, OperationSnapshot>;
  anomalies: AgentAnomaly[];
  /**
   * 当前会话权限档位（A3-10）。会话级、只存内存：不落 settings，也不落
   * conversations 表，重开应用即回到默认档，避免宽档被静默沿用。
   */
  permission: AgentPermission;

  /** 切换档位。已打开的会话会先关闭再按新档重开；回答进行中拒绝切换。 */
  setPermission: (next: AgentPermission) => Promise<void>;
  ensureHost: (recover?: boolean) => Promise<AgentHostStatus>;
  /** 用户确认后恢复上次被强杀的 Host；语义等同 ensureHost(true)，只用于显式按钮。 */
  recoverHost: () => Promise<AgentHostStatus>;
  /** 在发送前保证会话在当前 Host 实例上已打开（Host 重启后必须重开）。 */
  ensureOpen: (conversationRef: string) => Promise<void>;
  open: (conversationRef: string, mode?: AgentOpenMode) => Promise<void>;
  /**
   * 发送一轮提示词。
   *
   * 失败一律抛错（带可读中文原因），调用方据此保留草稿并把原因显示在输入区；
   * 只有真正被 Host 接受的发送才正常返回。
   */
  send: (conversationRef: string, text: string) => Promise<void>;
  stop: (conversationRef: string) => Promise<void>;
  close: (conversationRef: string) => Promise<void>;
  subscribe: () => Promise<void>;
  unsubscribe: () => void;
  hydrate: (conversationRef: string) => Promise<void>;
  approvePlanCard: (planId: string, reviewRef: string) => Promise<void>;
  /** 拒绝只由可信主界面提交；拒绝不产生任何写入操作。 */
  rejectPlanCard: (planId: string, reviewRef: string) => Promise<void>;
  cancelOperationCard: (operationRef: string) => Promise<OperationSnapshot>;
  resumeOperationCard: (operationRef: string) => Promise<OperationSnapshot>;
  /**
   * 明确开始执行一个已批准、已提交、仍在待执行状态的操作。
   *
   * 批准与提交不产生文件效果；这一步是唯一让复制/移动/恢复/清空真正发生的入口。
   */
  startOperationCard: (operationRef: string) => Promise<OperationSnapshot>;
  dismissAnomaly: (id: string) => void;
  reset: () => void;
}

/** 稳定空快照：让 zustand 的 Object.is 比较成立，避免每次渲染都换引用。 */
const EMPTY_RUNTIME: ConversationRuntime = Object.freeze({
  messages: [],
  status: "idle",
  activeRunId: null,
  stoppingRunId: null,
  lastSequence: {},
  openMode: null,
  openedOn: null,
  error: null,
}) as ConversationRuntime;

export function runtimeOf(
  runtimes: Record<string, ConversationRuntime>,
  conversationRef: string,
): ConversationRuntime {
  return runtimes[conversationRef] ?? EMPTY_RUNTIME;
}

function initialState() {
  return {
    host: { state: "idle", instanceId: null, info: null, error: null } as AgentHostView,
    runtimes: {} as Record<string, ConversationRuntime>,
    linkedPlans: {} as Record<string, string[]>,
    linkedOperations: {} as Record<string, string[]>,
    linkedSelections: {} as Record<string, string[]>,
    plans: {} as Record<string, PlanPreview>,
    operations: {} as Record<string, OperationSnapshot>,
    anomalies: [] as AgentAnomaly[],
    permission: DEFAULT_AGENT_PERMISSION,
  };
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function codeOf(error: unknown): string {
  return error instanceof AppError ? error.code : "UNKNOWN";
}

const KNOWN_AGENT_CODES = new Set<string>(AgentErrorCodeValues);

/** Host 生命周期与绑定类错误的可行动文案。
 *
 * 这些码由 Rust 端在发送前/启动时给出（A3-04 修复），比协议 §6.1 的网关类别更靠近
 * 真正原因：是没绑服务、没存密钥、网络出口未就绪，还是 Host 上次被强杀。界面只说下一步
 * 该做什么，不复述内部细节。
 */
const HOST_OR_BINDING_TEXT: Record<string, string> = {
  AGENT_HOST_INTERRUPTED:
    "上次运行被强制结束了。确认没有正在进行的对话后，点「恢复 Agent 服务」再继续。",
  AGENT_HOST_STARTING: "Agent 服务正在启动，请稍候重试。",
  AGENT_HOST_UNAVAILABLE:
    "Agent 服务未运行，或这个平台/安装包不提供 Agent 能力。请先启动服务；仍失败时请重装完整版本。",
  AGENT_SERVICE_UNBOUND:
    "还没有为 Agent 选择 AI 服务，或原来绑定的服务已经不存在。请在「设置 → AI 服务」里绑定一个服务（可用云模型或本机 Ollama）。",
  AGENT_CREDENTIAL_MISSING:
    "Agent 绑定的 AI 服务还没有保存密钥。请在「设置 → AI 服务」里补充后重试。",
  AGENT_MODEL_NETWORK_DISABLED:
    "Agent 网络出口没有就绪：当前平台或安装包不含 Agent 运行资源。请确认支持范围，或重装完整版本后重试。",
  AGENT_CONVERSATION_NOT_OPEN:
    "会话需要在 Agent 服务里重新打开。已自动重试一次；仍失败请新建对话。",
  AGENT_SESSION_NOT_FOUND:
    "Agent 服务上已不存在这个会话，已按新会话继续（历史计划与任务卡仍在）。",
};

/** Host/绑定类错误优先用可行动文案，其余回落到协议码表或后端原文。 */
export function readableHostError(code: string, fallback: string): string {
  return HOST_OR_BINDING_TEXT[code] ?? fallback;
}

/** 网关失败类别的中文说明（A3-06）。
 *
 * 为什么需要一层：`RUN_FAILED` 只说「这一轮跑失败了」，
 * 而 `agentErrorMessages` 里没有网关类别，界面就只剩一句通用话术。
 * 用户实际需要知道的是「服务连不上 / 被服务拒绝 / 超时 / 被限流」这类可行动原因。
 * 这里只映射协议 §6.1 的固定类别，不透传任何原始异常或地址。
 */
const GATEWAY_FAILURE_TEXT: Record<string, string> = {
  UPSTREAM_UNAVAILABLE: "连不上这个 AI 服务（本机服务未启动，或网络不可达）。请检查服务后重试。",
  UPSTREAM_HTTP: "AI 服务拒绝了这次请求。请检查密钥、服务状态和所选模型后重试。",
  TIMEOUT: "AI 服务在限定时间内没有响应。可以稍后重试，或在设置里换一个更快的服务。",
  STREAM_FAILED: "AI 服务的回答中途断开，已保留收到的内容。可以重试本轮。",
  REQUEST_LIMIT: "请求太频繁，已被本地发送限制挡住。请稍等片刻再发送。",
  BUDGET_EXCEEDED: "本轮已达到批准的预算上限，已停止发送。需要更多预算请重新审核。",
  ROUTE_DENIED: "这个服务或模型不在允许的路由范围内，请换一个受支持的服务。",
  CONNECTION_CHANGED: "服务配置在回答期间发生了变化，本轮已停止。请确认服务后重新发送。",
  LEASE_REVOKED: "服务租约已失效（可能被停用或撤销授权），本轮已停止。请重新选择服务。",
  REQUEST_INVALID: "这次请求的格式不被 AI 服务接受，本轮已停止。可以调整描述后重试。",
  REDIRECT_DENIED: "AI 服务要求跳转到其他地址，已按安全策略拒绝。请检查服务地址。",
  CANCELLED: "本轮已取消，已经收到的内容仍予保留。",
};

/** 失败文案：优先给出网关具体原因，其次才是固定错误码表。 */
export function readableRunFailure(
  code: string,
  gatewayCode: string | undefined,
): string {
  if (code === "RUN_FAILED" && gatewayCode) {
    const specific = GATEWAY_FAILURE_TEXT[gatewayCode];
    if (specific) return specific;
  }
  return readableAgentError(code, "本轮回答未能完成，请查看诊断信息后重试。");
}

/** 协议错误码是固定集合；未知码按「本轮失败」处理，不臆造文案。 */
export function isAgentErrorCode(code: string): code is AgentErrorCode {
  return KNOWN_AGENT_CODES.has(code);
}

export function readableAgentError(code: string, fallback: string): string {
  return isAgentErrorCode(code) ? agentErrorMessages[code] : fallback;
}

/** 把一帧应用到某会话的运行时。判定为迟到/重复时返回原对象与可选异常。 */
export function applyFrame(
  runtime: ConversationRuntime,
  frame: AgentHostFrame,
  options: { hostInstanceId: string | null; nowUnixMs: number },
): { runtime: ConversationRuntime; anomaly?: Omit<AgentAnomaly, "id"> } {
  if (options.hostInstanceId && frame.instance !== options.hostInstanceId) {
    // 旧 Host 实例的迟到帧：不改当前状态，只在流式期间提示一次。
    if (runtime.status !== "streaming") return { runtime };
    return {
      runtime,
      anomaly: {
        conversationRef: frame.conversationId ?? "",
        runId: undefined,
        code: "STALE_HOST_INSTANCE",
        message: "已忽略旧 Agent Host 实例的迟到事件，当前会话状态未改变。",
        atUnixMs: options.nowUnixMs,
      },
    };
  }
  if (frame.type === "host.status") {
    // Host 流已断：保留已收到的文本，把流式消息标为中断，并释放活动 run，
    // 否则界面会一直显示「正在回答」且下一次发送被挡住。
    const messages = runtime.messages.map((item) =>
      item.status === "streaming" ? { ...item, status: "aborted" as const } : item,
    );
    // 活动 run 随 Host 一起死了：记成终态，避免 ACK 迟到时又被当成仍在进行。
    const finishedRuns = runtime.activeRunId
      ? { ...runtime.finishedRuns, [runtime.activeRunId]: "aborted" as AgentMessageStatus }
      : runtime.finishedRuns;
    return {
      runtime: {
        ...runtime,
        messages,
        finishedRuns,
        status: "idle",
        activeRunId: null,
        stoppingRunId: null,
        error: "Agent Host 已中断。已保留收到的内容；核对会话与任务后可恢复或新建对话。",
      },
      anomaly: {
        conversationRef: frame.conversationId ?? "",
        code: "HOST_INTERRUPTED",
        message: "Agent Host 连接已中断，本轮回答未完成。请核对任务卡后决定是否恢复。",
        atUnixMs: options.nowUnixMs,
      },
    };
  }
  const key = `${frame.instance}|${frame.runId}`;
  const last = runtime.lastSequence[key];
  if (frame.sequence !== undefined && last !== undefined && frame.sequence <= last) {
    // 同帧重放：既不重复追加文本，也不让终态退回 streaming。
    return { runtime };
  }
  const next: ConversationRuntime = {
    ...runtime,
    lastSequence:
      frame.sequence === undefined
        ? runtime.lastSequence
        : { ...runtime.lastSequence, [key]: frame.sequence },
  };

  if (frame.type === "delta") {
    const text = frame.text ?? "";
    const existing = next.messages.findIndex(
      (item) => item.role === "assistant" && item.runId === frame.runId,
    );
    // 是否已收到过本轮终态（settled/error 先到、delta 后到的乱序情况）。
    const finishedStatus = next.finishedRuns?.[frame.runId];
    const runFinished = finishedStatus !== undefined;
    if (existing >= 0) {
      const messages = next.messages.map((item, at) =>
        at === existing
          ? {
              ...item,
              text: item.text + text,
              // 迟到增量只补文本：不把已完成的终态退回 streaming，也不重新占住 activeRun，
              // 否则界面会卡在「正在回答」且下一轮发送被挡（WF-1 截断兜底）。
              status: runFinished ? item.status : ("streaming" as const),
            }
          : item,
      );
      return {
        runtime: {
          ...next,
          messages,
          status: runFinished ? next.status : "streaming",
          activeRunId: runFinished ? next.activeRunId : frame.runId,
        },
      };
    }
    // settled 先到、第一条 delta 后到：补建助手消息，但状态沿用已记录的终态。
    if (runFinished) {
      return {
        runtime: {
          ...next,
          messages: [
            ...next.messages,
            {
              id: `assistant_${frame.runId}`,
              role: "assistant",
              text,
              runId: frame.runId,
              status: finishedStatus,
            },
          ],
        },
      };
    }
    return {
      runtime: {
        ...next,
        messages: [
          ...next.messages,
          {
            id: `assistant_${frame.runId}`,
            role: "assistant",
            text,
            runId: frame.runId,
            status: "streaming",
          },
        ],
        status: "streaming",
        activeRunId: frame.runId,
      },
    };
  }

  if (frame.type === "settled") {
    const status: AgentMessageStatus = frame.aborted ? "aborted" : "complete";
    const messages = next.messages.map((item) =>
      item.role === "assistant" && item.runId === frame.runId ? { ...item, status } : item,
    );
    const isCurrent = next.activeRunId === null || next.activeRunId === frame.runId;
    return {
      runtime: {
        ...next,
        messages,
        // settled 是协议定义的 run 终态；即便这一轮没有 delta（没有任何助手消息），
        // 也要记下来，否则 ACK 后到时无从判断本 run 早已结束。
        finishedRuns: { ...next.finishedRuns, [frame.runId]: status },
        status: isCurrent ? "ready" : next.status,
        activeRunId: next.activeRunId === frame.runId ? null : next.activeRunId,
        stoppingRunId: next.stoppingRunId === frame.runId ? null : next.stoppingRunId,
      },
    };
  }

  const code = frame.code ?? "INTERNAL";
  const text = readableRunFailure(code, frame.gatewayError?.code);
  const messages = next.messages.map((item) =>
    item.role === "assistant" && item.runId === frame.runId
      ? { ...item, status: "failed" as const, errorCode: code }
      : item,
  );
  return {
    runtime: {
      ...next,
      messages,
      // error 帧同样结束本轮（协议：运行终态仍以 settled 为准，但 error 之后
      // Rust 也已清掉 active_run，这里不能把它当成「还在跑」）。
      finishedRuns: { ...next.finishedRuns, [frame.runId]: "failed" as AgentMessageStatus },
      status: "idle",
      activeRunId: next.activeRunId === frame.runId ? null : next.activeRunId,
      stoppingRunId: next.stoppingRunId === frame.runId ? null : next.stoppingRunId,
      error: text,
    },
    anomaly: {
      conversationRef: frame.conversationId,
      runId: frame.runId,
      code,
      message: text,
      atUnixMs: options.nowUnixMs,
    },
  };
}

let subscription: (() => void) | null = null;
/** 订阅进行中标记：并发调用只安装一个监听器。 */
let subscribing = false;
/** 启动进行中的去重句柄：并发调用复用同一个 Host 启动结果。 */
let hostStart: Promise<AgentHostStatus> | null = null;
let anomalySequence = 0;

/** AGENT_HOST_STARTING：另一个调用者正在启动，等它结束后重试即可，不算失败。 */
const STARTING_RETRY_INTERVAL_MS = 300;
const STARTING_RETRY_LIMIT = 20;

async function startHostWithRetry(recover: boolean): Promise<AgentHostStatus> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await startAgentHost(recover);
    } catch (error) {
      if (codeOf(error) === "AGENT_HOST_STARTING" && attempt < STARTING_RETRY_LIMIT) {
        await new Promise((resolve) => setTimeout(resolve, STARTING_RETRY_INTERVAL_MS));
        continue;
      }
      throw error;
    }
  }
}

export const useAgentSessionStore = create<AgentState>((set, get) => ({
  ...initialState(),

  setPermission: async (next) => {
    const permission = normalizeAgentPermission(next);
    const state = get();
    if (state.permission === permission) return;
    // Pi 的工具表在会话创建时定型，中途换档只能关闭后重开；有回答在跑时重开
    // 会吞掉这一轮的输出，所以先拒绝，让用户显式停止。
    const busy = Object.values(state.runtimes).some(
      (runtime) =>
        runtime.status === "opening" ||
        runtime.status === "streaming" ||
        runtime.activeRunId !== null,
    );
    if (busy) throw new Error("回答进行中，请先停止再切换权限");
    set({ permission });
    const opened = Object.keys(state.runtimes).filter(
      (ref) => runtimeOf(state.runtimes, ref).openedOn,
    );
    for (const ref of opened) {
      // 重开会把运行时投影清空，但对话记录只是本进程的显示历史，不能因为
      // 换了权限就整段消失；重开成功后再挂回去。
      const before = runtimeOf(get().runtimes, ref);
      await get().close(ref);
      await get().open(ref, "resume");
      set((current) => ({
        runtimes: {
          ...current.runtimes,
          [ref]: {
            ...runtimeOf(current.runtimes, ref),
            messages: before.messages,
            lastSequence: before.lastSequence,
            finishedRuns: before.finishedRuns,
          },
        },
      }));
    }
  },

  ensureHost: async (recover = false) => {
    const current = get().host;
    if (!recover && current.state === "ready" && current.info) return current.info;
    // 并发去重：两个面板同时挂载时只发一次 agent_host_start，其余调用者等待同一结果。
    if (hostStart) return hostStart;
    const promise = (async () => {
      const wasInterrupted = current.state === "interrupted";
      set({
        host: { state: "starting", instanceId: current.instanceId, info: current.info, error: null },
      });
      try {
        const info = await startHostWithRetry(recover);
        // 新实例上的会话都要重新打开；旧 openedOn 一律作废。
        set((state) => ({
          host: { state: "ready", instanceId: info.hostInstanceId, info, error: null },
          runtimes: Object.fromEntries(
            Object.entries(state.runtimes).map(([ref, runtime]) => [ref, { ...runtime, openedOn: null }]),
          ),
        }));
        return info;
      } catch (error) {
        const code = codeOf(error);
        // 只按错误码分流，不再用「消息里有没有『中断』」猜状态。
        const state: AgentHostState =
          code === "AGENT_HOST_INTERRUPTED"
            ? "interrupted"
            : code === "AGENT_HOST_UNAVAILABLE"
              ? "unavailable"
              : wasInterrupted
                ? "interrupted"
                : "error";
        set({
          host: {
            state,
            instanceId: null,
            info: null,
            error: readableHostError(code, readableAgentError(code, messageOf(error))),
          },
        });
        logger.error("Agent Host 启动失败", { code, error: messageOf(error) });
        throw error;
      }
    })();
    hostStart = promise;
    try {
      return await promise;
    } finally {
      if (hostStart === promise) hostStart = null;
    }
  },

  /** 用户明确确认「上次中断、现在恢复」时的唯一入口（等价于 ensureHost(true)）。 */
  recoverHost: async () => get().ensureHost(true),

  /**
   * 确保某个会话在当前 Host 实例上是打开的。
   *
   * Host 重启后旧会话在 Host 侧已不存在，直接发送会得到 CONVERSATION_NOT_OPEN；
   * 这里按 (会话, Host 实例) 复用打开结果，只补必要的 open。
   */
  ensureOpen: async (conversationRef) => {
    const state = get();
    const runtime = runtimeOf(state.runtimes, conversationRef);
    const ready = state.host.state === "ready" && state.host.instanceId !== null;
    // 档位变化必须重开：Host 只在 conversation.open 时按档位裁剪工具表，
    // 沿用一个按别的档位建好的会话会让模型继续拿着旧工具表。
    const sameTier = runtime.openedPermission === state.permission;
    if (ready && runtime.openedOn && runtime.openedOn === state.host.instanceId && sameTier) {
      return;
    }
    await get().open(conversationRef, runtime.openMode ?? "resume");
  },

  open: async (conversationRef, mode) => {
    set((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversationRef]: { ...runtimeOf(s.runtimes, conversationRef), status: "opening", error: null },
      },
    }));
    try {
      await get().ensureHost();
      // 新建必须 `new`；其余先按 `resume` 恢复已有 Pi 会话，缺文件时退回 `new`。
      const preferred: AgentOpenMode = mode ?? "resume";
      const permission = get().permission;
      let resolvedMode: AgentOpenMode = preferred;
      let opened;
      try {
        opened = await openAgentConversation(conversationRef, preferred, permission);
      } catch (error) {
        // 只有「Host 上确实没有这个会话」才允许用 new 重建。其它错误（文件损坏、
        // 会话被占用、Host 未启动）都是真实状态，静默新建会丢掉本该恢复的上下文。
        if (preferred === "new" || codeOf(error) !== "AGENT_SESSION_NOT_FOUND") throw error;
        opened = await openAgentConversation(conversationRef, "new", permission);
        resolvedMode = "new";
      }
      set((s) => ({
        host: { ...s.host, state: "ready", instanceId: opened.hostInstanceId, error: null },
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            status: "ready",
            openMode: resolvedMode,
            openedPermission: permission,
            // 记下「在哪个 Host 实例上打开的」，供 ensureOpen 判断是否需要重开。
            openedOn: opened.hostInstanceId,
            error: null,
          },
        },
      }));
      await get().hydrate(conversationRef);
    } catch (error) {
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            status: "idle",
            openedOn: null,
            error: readableHostError(codeOf(error), readableAgentError(codeOf(error), messageOf(error))),
          },
        },
      }));
      logger.error("打开 Agent 会话失败", {
        code: codeOf(error),
        conversationRef,
        error: messageOf(error),
      });
      throw error;
    }
  },

  send: async (conversationRef, text) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    const runtime = runtimeOf(get().runtimes, conversationRef);
    if (runtime.activeRunId) {
      const busy = "上一轮回答尚未结束，请先停止回答或等待完成。";
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            error: busy,
          },
        },
      }));
      throw new Error(busy);
    }
    // Host 重启后旧会话在 Host 侧已不存在：先按当前实例重新打开，再发送。
    try {
      await get().ensureOpen(conversationRef);
    } catch (error) {
      const message = readableHostError(
        codeOf(error),
        readableAgentError(codeOf(error), messageOf(error)),
      );
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            status: "idle",
            error: message,
          },
        },
      }));
      throw error instanceof Error ? error : new Error(message);
    }
    let dispatch: AgentRunDispatch;
    try {
      dispatch = await sendAgentMessage(conversationRef, trimmed);
    } catch (error) {
      // 发送被拒（未绑服务、未开联网、缺密钥、Host 未运行…）必须让调用方知道：
      // 之前返回 false 会让输入框以为发成功、把草稿清掉。
      const message = readableHostError(
        codeOf(error),
        readableAgentError(codeOf(error), messageOf(error)),
      );
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            error: message,
          },
        },
      }));
      throw new Error(message);
    }
    const accepted = dispatch.state === "accepted";
    set((s) => {
      const base = runtimeOf(s.runtimes, conversationRef);
      // 发送 IPC 返回前就可能已经有本 run 的帧到达（delta 追加了助手消息、或
      // settled 已把状态收敛为 ready/idle 并清空 activeRunId）。这里做幂等对账：
      // 只补齐缺失的消息，绝不重复追加、绝不把已收敛的终态回退成 streaming，
      // 否则会出现「已回答完却又变回正在回答并卡住」以及重复空助手消息。
      const hasUser = base.messages.some(
        (item) => item.role === "user" && item.runId === dispatch.runId,
      );
      const hasAssistant = base.messages.some(
        (item) => item.role === "assistant" && item.runId === dispatch.runId,
      );
      const messages: AgentMessage[] = [...base.messages];
      if (!hasUser) {
        messages.push({
          id: `user_${dispatch.runId}`,
          role: "user",
          text: trimmed,
          runId: dispatch.runId,
          status: "complete",
        });
      }
      // 帧先到时助手消息可能已存在；此时保持其现状（可能 streaming 或已 settled）。
      if (accepted && !hasAssistant) {
        messages.push({
          id: `assistant_${dispatch.runId}`,
          role: "assistant",
          text: "",
          runId: dispatch.runId,
          status: "streaming",
        });
      }
      // 判断「本 run 是否已经收敛」只能看本 run 自己的证据：协议终态已落地
      // （finishedRuns，覆盖 settled 先到且无 delta 的情形），或它的助手消息已落到
      // complete/aborted/failed。不能用「当前有没有活动 run」或「状态是不是 ready」
      // 来推断 —— ACK 先到、帧还没来时这两者都成立，会把刚发出的 run 误判成已结束，
      // 于是 activeRunId 留空、stop() 没有 run 可取消（A3-04/A3-09 缺陷）。
      const assistant = base.messages.find(
        (item) => item.role === "assistant" && item.runId === dispatch.runId,
      );
      const assistantFinished =
        assistant !== undefined &&
        (assistant.status === "complete" ||
          assistant.status === "aborted" ||
          assistant.status === "failed");
      const settledEarly = base.finishedRuns?.[dispatch.runId] !== undefined || assistantFinished;
      // 别的 run 仍在活动时不抢占它的投影（正常情况下 Rust 已用 RUN_BUSY 拒绝）。
      const foreignActive = !settledEarly && base.activeRunId !== null && base.activeRunId !== dispatch.runId;
      return {
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...base,
            messages,
            status: settledEarly
              ? base.status
              : foreignActive
                ? base.status
                : accepted
                  ? "streaming"
                  : "unknown",
            // outcomeUnknown 同样要写入 runId：Rust 侧已保留 active_run，
            // GUI 只有拿着这个 runId 才能精确取消，且绝不重放提示词。
            activeRunId: settledEarly
              ? base.activeRunId
              : foreignActive
                ? base.activeRunId
                : dispatch.runId,
            error: null,
          },
        },
        anomalies:
          dispatch.state === "outcomeUnknown"
            ? [
                ...s.anomalies,
                {
                  id: `anomaly_${++anomalySequence}`,
                  conversationRef,
                  runId: dispatch.runId,
                  code: "OUTCOME_UNKNOWN",
                  message:
                    "本轮发送结果未知，已保留运行标识；请核对后决定是否停止，不要重复发送同一提示词。",
                  atUnixMs: Date.now(),
                },
              ]
            : s.anomalies,
      };
    });
  },

  stop: async (conversationRef) => {
    const runId = runtimeOf(get().runtimes, conversationRef).activeRunId;
    if (!runId) return;
    set((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversationRef]: {
          ...runtimeOf(s.runtimes, conversationRef),
          stoppingRunId: runId,
          error: null,
        },
      },
    }));
    try {
      // 只停「回答」，不触碰任何已入队的业务任务。
      await cancelAgentRun(conversationRef, runId);
    } catch (error) {
      set((s) => ({
        runtimes: {
          ...s.runtimes,
          [conversationRef]: {
            ...runtimeOf(s.runtimes, conversationRef),
            stoppingRunId: null,
            error: messageOf(error),
          },
        },
      }));
      throw error;
    }
  },

  close: async (conversationRef) => {
    try {
      await closeAgentConversation(conversationRef);
    } catch (error) {
      logger.warn("关闭 Agent 会话失败", {
        conversationRef,
        code: codeOf(error),
        error: messageOf(error),
      });
    }
    set((s) => {
      const runtimes = { ...s.runtimes };
      delete runtimes[conversationRef];
      return { runtimes };
    });
  },

  subscribe: async () => {
    if (subscription || subscribing) return;
    subscribing = true;
    try {
      const unlisten = await onAgentHostFrame((frame) => {
        const state = get();
        if (frame.type === "host.status") {
          // Host 流已断：立刻把 Host 置为中断（界面可提示恢复），并把当时仍在
          // 流式回答的会话逐个落定为「已中断」，避免一直显示「正在回答」。
          // 流式中与「结果未知」两类都随 Host 一起作废：前者在等增量，后者
          // 在等 ACK 对账，Host 没了都不会再有下文。
          const interrupted = Object.entries(state.runtimes).filter(
            ([, runtime]) => runtime.status === "streaming" || runtime.status === "unknown",
          );
          // 所有会话都失去打开的会话（Host 实例已死），必须重开才能继续发送。
          const runtimes: Record<string, ConversationRuntime> = Object.fromEntries(
            Object.entries(state.runtimes).map(([ref, runtime]) => [
              ref,
              { ...runtime, openedOn: null },
            ]),
          );
          const anomalies = [...state.anomalies];
          for (const [conversationRef, runtime] of interrupted) {
            const result = applyFrame(runtime, frame, {
              hostInstanceId: state.host.instanceId,
              nowUnixMs: Date.now(),
            });
            runtimes[conversationRef] = { ...result.runtime, openedOn: null };
            if (result.anomaly) {
              anomalies.push({
                ...result.anomaly,
                conversationRef,
                id: `anomaly_${++anomalySequence}`,
              });
            }
          }
          set({
            host: {
              state: "interrupted",
              instanceId: state.host.instanceId,
              info: state.host.info,
              error: "Agent Host 连接已中断。核对会话与任务后可恢复或新建对话。",
            },
            runtimes,
            anomalies,
          });
          return;
        }
        if (!state.runtimes[frame.conversationId]) return;
        const result = applyFrame(runtimeOf(state.runtimes, frame.conversationId), frame, {
          hostInstanceId: state.host.instanceId,
          nowUnixMs: Date.now(),
        });
        set((s) => ({
          runtimes: { ...s.runtimes, [frame.conversationId]: result.runtime },
          anomalies: result.anomaly
            ? [...s.anomalies, { ...result.anomaly, id: `anomaly_${++anomalySequence}` }]
            : s.anomalies,
        }));
        if (frame.type === "settled") {
          // 结束 ≠ 成功：回读真实计划与任务快照校准卡片。
          void get().hydrate(frame.conversationId);
        }
      });
      subscription = unlisten;
    } catch (error) {
      // 订阅失败必须可重试：不缓存失败结果，也不阻断页面渲染。
      subscription = null;
      logger.error("Agent Host 事件订阅失败", { error: messageOf(error) });
      throw error;
    } finally {
      subscribing = false;
    }
  },

  unsubscribe: () => {
    subscription?.();
    subscription = null;
  },

  hydrate: async (conversationRef) => {
    try {
      const view = await getConversation(conversationRef);
      // A3-04：归属由后端裁决 —— 显式链接 ∪ 该会话自己产生的计划（plans.session_id）。
      // 前端只提供会话引用，既不按时间差猜，也无法指定要认领哪些计划。
      const work = await getConversationWork(conversationRef);
      const linkedPlanRefs = work.plans.map((plan) => plan.planId);
      const plans: Record<string, PlanPreview> = { ...get().plans };
      for (const plan of work.plans) plans[plan.planId] = plan;
      const operations: Record<string, OperationSnapshot> = { ...get().operations };
      for (const operation of work.operations) {
        // 后端已回读真实快照；这里只落地，不推测进度。
        operations[operation.operationId] = operation;
      }
      // 显式链接过、但后端未能返回的任务，仍然逐条回读：引用过期/库变更时保留原有
      // 卡片，并让错误显式可见而不是静默清空。
      const operationRefs = view.links
        .filter((item) => item.kind === "operation")
        .map((item) => item.reference);
      for (const operationRef of operationRefs) {
        if (operations[operationRef]) continue;
        try {
          operations[operationRef] = await getOperation(operationRef);
        } catch (error) {
          logger.warn("读取操作快照失败", { operationRef, error: messageOf(error) });
        }
      }
      // A3-08：交接过的素材集合是只读快照引用，重开会话后仍能打开同一份结果视图。
      const selectionRefs = view.links
        .filter((item) => item.kind === "selection")
        .map((item) => item.reference);
      set((s) => ({
        plans,
        operations,
        linkedPlans: { ...s.linkedPlans, [conversationRef]: linkedPlanRefs },
        linkedOperations: {
          ...s.linkedOperations,
          [conversationRef]: [
            ...new Set([
              ...work.operations.map((operation) => operation.operationId),
              ...operationRefs,
            ]),
          ],
        },
        linkedSelections: { ...s.linkedSelections, [conversationRef]: selectionRefs },
      }));
    } catch (error) {
      logger.warn("恢复会话卡片失败", { conversationRef, error: messageOf(error) });
    }
  },

  approvePlanCard: async (planId, reviewRef) => {
    // 审批只由可信主界面提交；成功后不乐观改写，交给下一次 hydrate 回读。
    await approvePlan(planId, reviewRef);
    const entry = Object.entries(get().linkedPlans).find(([, refs]) => refs.includes(planId));
    if (entry) await get().hydrate(entry[0]);
  },

  rejectPlanCard: async (planId, reviewRef) => {
    // 与审批同一条路径：拒绝后同样回读真实快照，避免界面自行推测新状态。
    await rejectPlan(planId, reviewRef);
    const entry = Object.entries(get().linkedPlans).find(([, refs]) => refs.includes(planId));
    if (entry) await get().hydrate(entry[0]);
  },

  cancelOperationCard: async (operationRef) => {
    const snapshot = await cancelOperation(operationRef);
    set((s) => ({ operations: { ...s.operations, [operationRef]: snapshot } }));
    return snapshot;
  },

  resumeOperationCard: async (operationRef) => {
    const snapshot = await resumeOperation(operationRef);
    set((s) => ({ operations: { ...s.operations, [operationRef]: snapshot } }));
    return snapshot;
  },

  startOperationCard: async (operationRef) => {
    const snapshot = await startOperation(operationRef);
    set((s) => ({ operations: { ...s.operations, [operationRef]: snapshot } }));
    return snapshot;
  },

  dismissAnomaly: (id) => {
    set((s) => ({ anomalies: s.anomalies.filter((item) => item.id !== id) }));
  },

  reset: () => {
    subscription?.();
    subscription = null;
    set({ ...initialState() });
  },
}));
