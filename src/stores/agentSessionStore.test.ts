/**
 * agentSessionStore 测试（v2 A3-04）。
 *
 * 验收口径：
 * - 增量按 (hostInstanceId, runId, sequence) 去重，同帧重放不重复追加文本；
 * - 旧 hostInstanceId 的迟到 delta 不污染当前流式状态，也不让终态退回 streaming；
 * - 双会话互不串消息，新会话必须是空白；
 * - 「停止回答」只取消 run，不动已入队的业务任务。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  applyFrame,
  runtimeOf,
  useAgentSessionStore,
  type ConversationRuntime,
} from "@/stores/agentSessionStore";
import {
  closeAgentConversation,
  openAgentConversation,
  sendAgentMessage,
  startAgentHost,
  onAgentHostFrame,
  type AgentHostStatus,
  type AgentProtocolFrame,
} from "@/api/agent";
import { getConversation, getConversationWork } from "@/api/conversations";
import { AppError } from "@/api/client";

vi.mock("@/api/agent", () => ({
  startAgentHost: vi.fn(),
  openAgentConversation: vi.fn(),
  sendAgentMessage: vi.fn(),
  cancelAgentRun: vi.fn(),
  closeAgentConversation: vi.fn(),
  onAgentHostFrame: vi.fn(),
  AGENT_HOST_EVENT: "agent-host-event",
}));

vi.mock("@/api/conversations", () => ({
  getConversation: vi.fn(),
  getConversationWork: vi.fn(),
  linkConversation: vi.fn(),
}));

vi.mock("@/api/plans", () => ({
  listPendingPlans: vi.fn(),
  approvePlan: vi.fn(),
  getOperation: vi.fn(),
}));

vi.mock("@/api/operations", () => ({
  cancelOperation: vi.fn(),
  resumeOperation: vi.fn(),
  listOperationRecovery: vi.fn(),
}));

const conversation = "conversation_aaaaaaaaaaaaaaaa";
const runId = "run_bbbbbbbbbbbbbbbb";
const HOST_INSTANCE = "instance_aaaaaaaaaaaaaaaa";

function emptyRuntime(): ConversationRuntime {
  return {
    messages: [],
    status: "idle",
    activeRunId: null,
    stoppingRunId: null,
    lastSequence: {},
    openMode: null,
    // 默认认为会话已在当前 Host 实例上打开：send() 会先 ensureOpen，
    // 不预置就会去调用 open 并依赖 mock 的返回值。
    openedOn: HOST_INSTANCE,
    error: null,
  };
}

/** agent_host_start 的成功返回值（测试用稳定实例 id）。 */
function hostInfo(overrides: Partial<AgentHostStatus> = {}): AgentHostStatus {
  return {
    hostInstanceId: HOST_INSTANCE,
    protocolVersion: 1,
    sdkVersion: "0.0.0-test",
    recoveredStrandedHost: false,
    modelNetworkEnabled: true,
    ...overrides,
  };
}

/** 把 Host 置为 ready，让 send() 的 ensureOpen 走「已打开」短路分支。 */
function seedReadyHost(): void {
  useAgentSessionStore.setState({
    host: {
      state: "ready",
      instanceId: HOST_INSTANCE,
      info: {
        hostInstanceId: HOST_INSTANCE,
        protocolVersion: 1,
        sdkVersion: "0.0.0-test",
        recoveredStrandedHost: false,
        modelNetworkEnabled: true,
      },
      error: null,
    },
  });
}

function frame(
  overrides: Partial<AgentProtocolFrame> & Pick<AgentProtocolFrame, "sequence" | "type">,
): AgentProtocolFrame {
  return {
    protocol: 1,
    instance: "instance_aaaaaaaaaaaaaaaa",
    conversationId: conversation,
    runId,
    ...overrides,
  };
}

const options = { hostInstanceId: "instance_aaaaaaaaaaaaaaaa", nowUnixMs: 1_000 };

describe("applyFrame", () => {
  it("同一 sequence 重放不会重复追加文本", () => {
    const first = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "你好" }), options);
    expect(first.runtime.messages).toHaveLength(1);
    expect(first.runtime.messages[0].text).toBe("你好");

    const replay = applyFrame(first.runtime, frame({ sequence: 1, type: "delta", text: "你好" }), options);
    expect(replay.runtime).toBe(first.runtime);
    expect(replay.runtime.messages[0].text).toBe("你好");
  });

  it("sequence 递增的增量落到同一条助手消息上", () => {
    let runtime = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "正在" }), options).runtime;
    runtime = applyFrame(runtime, frame({ sequence: 2, type: "delta", text: "读取" }), options).runtime;
    expect(runtime.messages).toHaveLength(1);
    expect(runtime.messages[0].text).toBe("正在读取");
    expect(runtime.status).toBe("streaming");
  });

  it("终态之后到达的旧 sequence 不会把状态退回 streaming", () => {
    let runtime = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "完成" }), options).runtime;
    runtime = applyFrame(runtime, frame({ sequence: 2, type: "settled", aborted: false }), options).runtime;
    expect(runtime.status).toBe("ready");
    expect(runtime.messages[0].status).toBe("complete");

    const late = applyFrame(runtime, frame({ sequence: 1, type: "delta", text: "补" }), options);
    expect(late.runtime).toBe(runtime);
    expect(late.runtime.status).toBe("ready");
    expect(late.runtime.messages[0].text).toBe("完成");
  });

  it("旧 host 实例的迟到帧不改变当前状态，只提示一次异常", () => {
    const runtime = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "新实例" }), options).runtime;
    const stale = applyFrame(
      runtime,
      frame({ sequence: 9, type: "delta", text: "旧实例", instance: "instance_oldoldoldoldoldold" }),
      options,
    );
    expect(stale.runtime).toBe(runtime);
    expect(stale.runtime.messages[0].text).toBe("新实例");
    expect(stale.anomaly?.code).toBe("STALE_HOST_INSTANCE");
  });

  it("中止的 settled 标记为已停止，并清空活动 run", () => {
    let runtime = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "半句" }), options).runtime;
    runtime = applyFrame(runtime, frame({ sequence: 2, type: "settled", aborted: true }), options).runtime;
    expect(runtime.messages[0].status).toBe("aborted");
    expect(runtime.activeRunId).toBeNull();
    expect(runtime.stoppingRunId).toBeNull();
  });

  it("error 帧用固定中文文案并单列异常", () => {
    const runtime = applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "半" }), options).runtime;
    const failed = applyFrame(
      runtime,
      frame({ sequence: 2, type: "error", code: "SERVICE_UNAVAILABLE" }),
      options,
    );
    expect(failed.runtime.messages[0].status).toBe("failed");
    expect(failed.anomaly?.code).toBe("SERVICE_UNAVAILABLE");
    expect(failed.runtime.error).toBeTruthy();
  });

  it("未知错误码不臆造文案，回落到通用提示", () => {
    const failed = applyFrame(
      emptyRuntime(),
      frame({ sequence: 1, type: "error", code: "SOMETHING_NEW" }),
      options,
    );
    expect(failed.anomaly?.code).toBe("SOMETHING_NEW");
    expect(failed.runtime.error).toBe("本轮回答未能完成，请查看诊断信息后重试。");
  });

  it("RUN_FAILED 带网关类别时给出可行动的中文原因，而不是通用话术", () => {
    // A3-06 缺陷 E：网关失败曾只显示 fallback「本轮回答未能完成」，
    // 用户看不出「本机服务没启动」还是「密钥被拒」。
    const failed = applyFrame(
      emptyRuntime(),
      frame({
        sequence: 1,
        type: "error",
        code: "RUN_FAILED",
        gatewayError: { code: "UPSTREAM_UNAVAILABLE" },
      }),
      options,
    );
    expect(failed.runtime.error).toBe(
      "连不上这个 AI 服务（本机服务未启动，或网络不可达）。请检查服务后重试。",
    );
    expect(failed.runtime.error).not.toBe("本轮回答未能完成，请查看诊断信息后重试。");
    expect(failed.runtime.messages).toHaveLength(0);
    expect(failed.runtime.status).toBe("idle");
  });

  it("RUN_FAILED 不带网关类别时仍回落到固定错误码表", () => {
    const failed = applyFrame(
      emptyRuntime(),
      frame({ sequence: 1, type: "error", code: "RUN_FAILED" }),
      options,
    );
    expect(failed.runtime.error).toBe("本轮回答未能完成，请查看诊断信息后重试。");
  });

  it("已知协议错误码不被网关映射覆盖", () => {
    // 网关类别只能改 RUN_FAILED 的文案，不能改写其它固定类别。
    const failed = applyFrame(
      emptyRuntime(),
      frame({
        sequence: 1,
        type: "error",
        code: "GRANT_REVOKED",
        gatewayError: { code: "UPSTREAM_UNAVAILABLE" },
      }),
      options,
    );
    expect(failed.runtime.error).toBe("资源授权已撤销，请重新授权。");
  });

  it("Host 流中断时把流式消息落定为已中断，并释放活动 run", () => {
    // A3-04：Host 死掉时读线程只发这条 UI 状态帧（不是协议帧）。若不处理，
    // 界面会永远停在「正在回答」，而 send() 的 activeRunId 守卫会挡住下一次发送。
    const runtime = applyFrame(
      emptyRuntime(),
      frame({ sequence: 1, type: "delta", text: "写到一半" }),
      options,
    ).runtime;
    expect(runtime.status).toBe("streaming");
    expect(runtime.activeRunId).toBe(runId);

    const interrupted = applyFrame(
      runtime,
      { type: "host.status", instance: "instance_aaaaaaaaaaaaaaaa", state: "interrupted", reason: "host_exited", conversationId: conversation },
      options,
    );
    expect(interrupted.runtime.messages[0]).toMatchObject({ text: "写到一半", status: "aborted" });
    expect(interrupted.runtime.activeRunId).toBeNull();
    expect(interrupted.runtime.stoppingRunId).toBeNull();
    expect(interrupted.runtime.status).not.toBe("streaming");
    expect(interrupted.runtime.error).toContain("Agent Host");
    expect(interrupted.anomaly?.code).toBe("HOST_INTERRUPTED");
  });
});

describe("agentSessionStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAgentSessionStore.getState().reset();
    vi.mocked(getConversationWork).mockResolvedValue({ plans: [], operations: [] });
    // open() 成功后会 hydrate 任务卡；不 mock 会打印「恢复会话卡片失败」噪声。
    vi.mocked(getConversation).mockResolvedValue({ links: [] } as never);
    // send() 会先 ensureOpen：预置成功的 open 结果，避免落到未加保护的分支。
    vi.mocked(openAgentConversation).mockResolvedValue({
      hostInstanceId: HOST_INSTANCE,
      conversationRef: conversation,
      sessionId: "session_aaaaaaaaaaaaaaaa",
    });
  });

  it("两个会话各自持有消息，新会话是空白", () => {
    const store = useAgentSessionStore.getState();
    const other = "conversation_cccccccccccccccc";

    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversation]: applyFrame(emptyRuntime(), frame({ sequence: 1, type: "delta", text: "甲的回答" }), options)
          .runtime,
      },
    }));

    expect(runtimeOf(useAgentSessionStore.getState().runtimes, conversation).messages[0].text).toBe("甲的回答");
    // 从未打开过的会话拿到稳定空运行时，不继承任何消息。
    expect(runtimeOf(useAgentSessionStore.getState().runtimes, other).messages).toHaveLength(0);
    expect(runtimeOf(useAgentSessionStore.getState().runtimes, other)).toBe(
      runtimeOf(useAgentSessionStore.getState().runtimes, other),
    );
    expect(store.host.state).toBe("idle");
  });

  it("停止回答只取消当前 run，不触碰任务卡", async () => {
    const { cancelAgentRun } = await import("@/api/agent");
    vi.mocked(cancelAgentRun).mockResolvedValue(undefined);
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversation]: { ...emptyRuntime(), status: "streaming", activeRunId: runId },
      },
    }));

    await useAgentSessionStore.getState().stop(conversation);

    expect(cancelAgentRun).toHaveBeenCalledWith(conversation, runId);
    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.stoppingRunId).toBe(runId);
    // 没有任何 operation 被取消：任务卡状态来自后端快照，不在这里兜底。
    expect(useAgentSessionStore.getState().operations).toEqual({});
  });

  it("发送返回前已收到 delta/settled 时不重复助手消息、不回退成卡住的 streaming", async () => {
    seedReadyHost();
    const { sendAgentMessage } = await import("@/api/agent");
    // 构造竞态：sendAgentMessage 挂起，先让本 run 的 delta 与 settled 帧落地，
    // 再 resolve。这正是审计复现的「已回答完又变回正在回答并卡住」的场景。
    let resolveSend!: (value: {
      state: "accepted";
      runId: string;
    }) => void;
    vi.mocked(sendAgentMessage).mockReturnValue(
      new Promise((resolve) => {
        resolveSend = resolve as typeof resolveSend;
      }),
    );

    const pending = useAgentSessionStore.getState().send(conversation, "帮我整理素材");

    // 帧先于 IPC 返回到达：先 delta 追加助手消息，再 settled 落终态。
    useAgentSessionStore.setState((s) => {
      let runtime = applyFrame(
        runtimeOf(s.runtimes, conversation),
        frame({ sequence: 1, type: "delta", text: "已经整理好了" }),
        options,
      ).runtime;
      runtime = applyFrame(runtime, frame({ sequence: 2, type: "settled", aborted: false }), options).runtime;
      return { runtimes: { ...s.runtimes, [conversation]: runtime } };
    });
    const settledBefore = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(settledBefore.status).toBe("ready");
    expect(settledBefore.activeRunId).toBeNull();

    // 现在发送 IPC 才返回 accepted。
    resolveSend({ state: "accepted", runId });
    await pending;

    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    const assistantMessages = runtime.messages.filter((m) => m.role === "assistant");
    // 只有一条助手消息（不重复追加空消息）。
    expect(assistantMessages).toHaveLength(1);
    expect(assistantMessages[0].text).toBe("已经整理好了");
    // 用户消息恰好一条。
    expect(runtime.messages.filter((m) => m.role === "user")).toHaveLength(1);
    // 终态不被回退：仍是 ready，活动 run 保持清空，不会卡在「正在回答」。
    expect(runtime.status).toBe("ready");
    expect(runtime.activeRunId).toBeNull();
    // 后续还能继续发送（没有残留活动 run 阻断）。
    expect(runtime.activeRunId).not.toBe(runId);
  });

  it("ACK 先到、还没有任何帧时保留活动 run，停止能精确取消", async () => {
    seedReadyHost();
    const { sendAgentMessage, cancelAgentRun } = await import("@/api/agent");
    vi.mocked(sendAgentMessage).mockResolvedValue({ state: "accepted", runId });
    vi.mocked(cancelAgentRun).mockResolvedValue(undefined);

    await useAgentSessionStore.getState().send(conversation, "帮我整理素材");

    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    // 关键回归：ACK 到达不等于本轮已结束。旧实现用「activeRunId 为空 + ready」
    // 误判为已收敛，把 runId 丢掉，于是 stop() 直接 return，取消发不出去。
    expect(runtime.status).toBe("streaming");
    expect(runtime.activeRunId).toBe(runId);
    // 用户消息 + 空的流式助手占位各一条。
    expect(runtime.messages.filter((m) => m.role === "user")).toHaveLength(1);
    expect(runtime.messages.filter((m) => m.role === "assistant")).toHaveLength(1);

    await useAgentSessionStore.getState().stop(conversation);
    expect(cancelAgentRun).toHaveBeenCalledTimes(1);
    expect(cancelAgentRun).toHaveBeenCalledWith(conversation, runId);
    // 取消的是「回答」，不是任何业务任务。
    expect(useAgentSessionStore.getState().operations).toEqual({});
  });

  it("ACK 结果未知且还没有帧时保留精确 run、可取消、且不重放提示词", async () => {
    seedReadyHost();
    const { sendAgentMessage, cancelAgentRun } = await import("@/api/agent");
    vi.mocked(sendAgentMessage).mockResolvedValue({ state: "outcomeUnknown", runId });
    vi.mocked(cancelAgentRun).mockResolvedValue(undefined);

    await useAgentSessionStore.getState().send(conversation, "把这几张移入回收站");

    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.status).toBe("unknown");
    // 协议要求：结果未知时保留该 runId 供 GUI 精确调用 conversation.cancel_run。
    expect(runtime.activeRunId).toBe(runId);
    const anomaly = useAgentSessionStore.getState().anomalies.find((item) => item.code === "OUTCOME_UNKNOWN");
    expect(anomaly).toBeDefined();
    expect(anomaly?.runId).toBe(runId);

    await useAgentSessionStore.getState().stop(conversation);
    expect(cancelAgentRun).toHaveBeenCalledWith(conversation, runId);
    // 绝不重放：整个流程只发生过一次 send。
    expect(sendAgentMessage).toHaveBeenCalledTimes(1);
  });

  it("settled 先到且没有 delta 时，迟到的 ACK 不回退终态", async () => {
    seedReadyHost();
    const { sendAgentMessage } = await import("@/api/agent");
    let resolveSend!: (value: { state: "accepted"; runId: string }) => void;
    vi.mocked(sendAgentMessage).mockReturnValue(
      new Promise((resolve) => {
        resolveSend = resolve as typeof resolveSend;
      }),
    );

    const pending = useAgentSessionStore.getState().send(conversation, "帮我看下");

    // 没有任何 delta，直接来 settled：此时没有助手消息可用来判断「已结束」，
    // 旧实现会把它重新投影成 streaming 并卡住。
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversation]: applyFrame(
          runtimeOf(s.runtimes, conversation),
          frame({ sequence: 1, type: "settled", aborted: false }),
          options,
        ).runtime,
      },
    }));

    resolveSend({ state: "accepted", runId });
    await pending;

    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.status).toBe("ready");
    expect(runtime.activeRunId).toBeNull();
  });

  it("别的 run 仍在活动时不抢占它的投影", async () => {
    seedReadyHost();
    const { sendAgentMessage } = await import("@/api/agent");
    vi.mocked(sendAgentMessage).mockResolvedValue({ state: "accepted", runId: "run_dddddddddddddddd" });
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [conversation]: { ...emptyRuntime(), status: "streaming", activeRunId: "run_eeeeeeeeeeeeeeee" },
      },
    }));

    // 守卫会先挡住这次发送：send 必须抛错（调用方据此保留草稿并显示原因），
    // 而不是静默返回 false。
    await expect(useAgentSessionStore.getState().send(conversation, "插一句")).rejects.toThrow(
      /上一轮回答尚未结束/,
    );
    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.activeRunId).toBe("run_eeeeeeeeeeeeeeee");
    expect(runtime.error).toContain("上一轮回答尚未结束");
  });

  it("并发 ensureHost 只启动一次 Host，两个调用者拿到同一个结果", async () => {
    // 侧栏与浮窗可能同时挂载，各自调 ensureHost；没有去重就会撞上 Rust 的
    // AGENT_HOST_STARTING 冲突，用户看到「正在启动」这种不像错误的错误。
    let releaseStart: (info: AgentHostStatus) => void = () => {};
    vi.mocked(startAgentHost).mockImplementation(
      () =>
        new Promise<AgentHostStatus>((resolve) => {
          releaseStart = resolve;
        }),
    );

    const first = useAgentSessionStore.getState().ensureHost();
    const second = useAgentSessionStore.getState().ensureHost();
    await Promise.resolve();
    expect(startAgentHost).toHaveBeenCalledTimes(1);

    releaseStart(hostInfo());
    await expect(first).resolves.toMatchObject({ hostInstanceId: HOST_INSTANCE });
    await expect(second).resolves.toMatchObject({ hostInstanceId: HOST_INSTANCE });
  });

  it("AGENT_HOST_STARTING 自动重试而不是把并发启动当失败", async () => {
    vi.mocked(startAgentHost)
      .mockRejectedValueOnce(new AppError("AGENT_HOST_STARTING", "Agent 服务正在启动"))
      .mockResolvedValue(hostInfo());

    const info = await useAgentSessionStore.getState().ensureHost();

    expect(startAgentHost).toHaveBeenCalledTimes(2);
    expect(info.hostInstanceId).toBe(HOST_INSTANCE);
    expect(useAgentSessionStore.getState().host.state).toBe("ready");
  });

  it("上次被强杀时按 AGENT_HOST_INTERRUPTED 落到 interrupted，不显示成普通错误", async () => {
    vi.mocked(startAgentHost).mockRejectedValue(
      new AppError("AGENT_HOST_INTERRUPTED", "上次是被强制结束的"),
    );

    await expect(useAgentSessionStore.getState().ensureHost()).rejects.toBeInstanceOf(AppError);

    const host = useAgentSessionStore.getState().host;
    expect(host.state).toBe("interrupted");
    expect(host.error).toContain("恢复 Agent 服务");
  });

  it("资源缺失/平台不支持时按 AGENT_HOST_UNAVAILABLE 落到 unavailable", async () => {
    vi.mocked(startAgentHost).mockRejectedValue(
      new AppError("AGENT_HOST_UNAVAILABLE", "Agent 服务在此平台不可用"),
    );

    await expect(useAgentSessionStore.getState().ensureHost()).rejects.toBeInstanceOf(AppError);

    const host = useAgentSessionStore.getState().host;
    expect(host.state).toBe("unavailable");
    expect(host.error).toContain("重装完整版本");
  });

  it("recoverHost 以 recover=true 重启，中断状态被清掉", async () => {
    useAgentSessionStore.setState({
      host: { state: "interrupted", instanceId: null, info: null, error: "上次被强制结束" },
    });
    vi.mocked(startAgentHost).mockResolvedValue(hostInfo());

    await useAgentSessionStore.getState().recoverHost();

    expect(startAgentHost).toHaveBeenCalledWith(true);
    expect(useAgentSessionStore.getState().host.state).toBe("ready");
  });

  it("网络出口未就绪时 send 抛错并写入可行动文案（调用方据此保留草稿）", async () => {
    seedReadyHost();
    // 2026-11-01 去掉了「允许 Agent 联网」开关；这个码现在只表示当前平台/安装包
    // 没有网络出口，文案必须指向支持范围而不是「去设置里打开开关」。
    vi.mocked(sendAgentMessage).mockRejectedValue(
      new AppError("AGENT_MODEL_NETWORK_DISABLED", "Agent 网络出口没有就绪"),
    );

    // send 只抛 Error（调用方拿 message 显示、并保留输入框草稿）。
    await expect(useAgentSessionStore.getState().send(conversation, "你好")).rejects.toThrow(
      /网络出口没有就绪/,
    );

    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.error).toContain("网络出口");
    // 会话本身仍是打开的（只是这一轮没发出去），不能留下活动 run 卡住下一次发送。
    expect(runtime.status).toBe("ready");
    expect(runtime.activeRunId).toBeNull();
    // 不能留下「用户气泡」：草稿必须原样留在输入框里。
    expect(runtime.messages).toHaveLength(0);
  });

  it("Host 换实例后旧的开合记录作废，发送前重新打开会话", async () => {
    const newInstance = "instance_bbbbbbbbbbbbbbbb";
    useAgentSessionStore.setState({
      host: {
        state: "ready",
        instanceId: newInstance,
        info: hostInfo({ hostInstanceId: newInstance }),
        error: null,
      },
      runtimes: { [conversation]: { ...emptyRuntime(), openedOn: HOST_INSTANCE } },
    });
    vi.mocked(openAgentConversation).mockResolvedValue({
      hostInstanceId: newInstance,
      conversationRef: conversation,
      sessionId: "session_bbbbbbbbbbbbbbbb",
    });
    vi.mocked(sendAgentMessage).mockResolvedValue({ state: "accepted", runId });

    await useAgentSessionStore.getState().send(conversation, "继续");

    expect(openAgentConversation).toHaveBeenCalledWith(conversation, "resume", "standard");
    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.openedOn).toBe(newInstance);
  });

  it("只有 AGENT_SESSION_NOT_FOUND 才用 new 重建，其它打开失败照原样抛出", async () => {
    vi.mocked(openAgentConversation)
      .mockRejectedValueOnce(new AppError("AGENT_SESSION_NOT_FOUND", "Host 上没有这个会话"))
      .mockResolvedValueOnce({
        hostInstanceId: HOST_INSTANCE,
        conversationRef: conversation,
        sessionId: "session_aaaaaaaaaaaaaaaa",
      });

    await useAgentSessionStore.getState().open(conversation, "resume");

    expect(openAgentConversation).toHaveBeenLastCalledWith(conversation, "new", "standard");
    expect(runtimeOf(useAgentSessionStore.getState().runtimes, conversation).openMode).toBe("new");

    vi.mocked(openAgentConversation).mockReset();
    vi.mocked(openAgentConversation).mockRejectedValue(
      new AppError("AGENT_CONVERSATION_NOT_OPEN", "会话打不开"),
    );

    await expect(useAgentSessionStore.getState().open(conversation, "resume")).rejects.toMatchObject({
      code: "AGENT_CONVERSATION_NOT_OPEN",
    });
    expect(openAgentConversation).toHaveBeenCalledTimes(1);
  });

  it("换档会关闭并用新档位重开会话，已显示的对话记录保留", async () => {
    seedReadyHost();
    useAgentSessionStore.setState({
      permission: "standard",
      runtimes: {
        [conversation]: {
          ...emptyRuntime(),
          openedPermission: "standard",
          messages: [
            { id: "m1", runId, role: "user", text: "帮我打标", status: "complete" },
            { id: "m2", runId, role: "assistant", text: "好的", status: "complete" },
          ],
        },
      },
    });
    vi.mocked(closeAgentConversation).mockResolvedValue();
    vi.mocked(openAgentConversation).mockResolvedValue({
      hostInstanceId: HOST_INSTANCE,
      conversationRef: conversation,
      sessionId: "session_aaaaaaaaaaaaaaaa",
    });
    vi.mocked(getConversation).mockResolvedValue({ ref: conversation, title: "会话", links: [] } as never);
    vi.mocked(getConversationWork).mockResolvedValue({ plans: [], operations: [] } as never);

    await useAgentSessionStore.getState().setPermission("read");

    expect(useAgentSessionStore.getState().permission).toBe("read");
    // 必须真的先关闭：Host 只在 conversation.open 时按档位裁剪工具表。
    expect(closeAgentConversation).toHaveBeenCalledWith(conversation);
    expect(openAgentConversation).toHaveBeenCalledWith(conversation, "resume", "read");
    const runtime = runtimeOf(useAgentSessionStore.getState().runtimes, conversation);
    expect(runtime.openedPermission).toBe("read");
    // 换档不是换会话：已显示的记录不能因为重开而消失。
    expect(runtime.messages.map((message) => message.text)).toEqual(["帮我打标", "好的"]);
  });

  it("回答进行中拒绝换档，档位保持不变", async () => {
    seedReadyHost();
    useAgentSessionStore.setState({
      permission: "standard",
      runtimes: {
        [conversation]: { ...emptyRuntime(), status: "streaming", activeRunId: runId },
      },
    });

    await expect(useAgentSessionStore.getState().setPermission("full")).rejects.toThrow(
      /回答进行中/,
    );
    expect(useAgentSessionStore.getState().permission).toBe("standard");
    expect(closeAgentConversation).not.toHaveBeenCalled();
  });

  it("档位与已打开的会话不一致时，发送前先按新档位重开", async () => {
    seedReadyHost();
    // 手工造出「打开时是 standard、现在档位是 full」的错配，
    // 模拟档位被别处改动或状态未同步的情况。
    useAgentSessionStore.setState({
      permission: "full",
      runtimes: { [conversation]: { ...emptyRuntime(), openedPermission: "standard" } },
    });
    vi.mocked(openAgentConversation).mockResolvedValue({
      hostInstanceId: HOST_INSTANCE,
      conversationRef: conversation,
      sessionId: "session_aaaaaaaaaaaaaaaa",
    });
    vi.mocked(sendAgentMessage).mockResolvedValue({ state: "accepted", runId });

    await useAgentSessionStore.getState().send(conversation, "继续");

    expect(openAgentConversation).toHaveBeenCalledWith(conversation, "resume", "full");
  });

  it("并发订阅只安装一次 Host 事件监听", async () => {
    useAgentSessionStore.getState().unsubscribe();
    vi.mocked(onAgentHostFrame).mockResolvedValue(() => {});
    vi.mocked(startAgentHost).mockResolvedValue(hostInfo());

    await Promise.all([
      useAgentSessionStore.getState().subscribe(),
      useAgentSessionStore.getState().subscribe(),
    ]);

    expect(onAgentHostFrame).toHaveBeenCalledTimes(1);
    useAgentSessionStore.getState().unsubscribe();
  });
});
