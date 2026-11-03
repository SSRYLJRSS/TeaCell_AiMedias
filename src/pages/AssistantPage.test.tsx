/**
 * AssistantPage 测试（v2 A3-04）。
 *
 * 验收口径：
 * - 双会话隔离：切换会话后只看到该会话自己的消息；
 * - 空白新会话：没有任何消息，不继承占位历史；
 * - 迟到的旧 Host 实例事件只提示异常，不污染当前消息；
 * - 「停止回答」按钮只出现在流式中，且点击后只取消 run。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import AssistantPage from "@/pages/AssistantPage";
import { NARROW_ASSISTANT_QUERY } from "@/hooks/useMediaQuery";
import { useConversationStore } from "@/stores/conversationStore";
import { useAgentSessionStore } from "@/stores/agentSessionStore";
import { useAssistantFocusStore } from "@/stores/assistantFocusStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import type { ConversationSummary, ConversationView } from "@/api/conversations";
import type { OperationSnapshot } from "@/types/plans";
import { BUILTIN_SKILLS } from "@/data/skills/catalog";

const mocks = vi.hoisted(() => ({
  listConversations: vi.fn(),
  createConversation: vi.fn(),
  getConversation: vi.fn(),
  renameConversation: vi.fn(),
  setConversationArchived: vi.fn(),
  deleteConversation: vi.fn(),
  saveConversationDraft: vi.fn(),
  attachConversationAsset: vi.fn(),
  linkConversation: vi.fn(),
  getConversationWork: vi.fn(),
  startAgentHost: vi.fn(),
  openAgentConversation: vi.fn(),
  sendAgentMessage: vi.fn(),
  cancelAgentRun: vi.fn(),
  closeAgentConversation: vi.fn(),
  onAgentHostFrame: vi.fn(),
  listPendingPlans: vi.fn(),
  approvePlan: vi.fn(),
  rejectPlan: vi.fn(),
  listWorkflows: vi.fn(),
  workflowStepCatalog: vi.fn(),
  getOperation: vi.fn(),
  cancelOperation: vi.fn(),
  resumeOperation: vi.fn(),
  startOperation: vi.fn(),
  listOperationRecovery: vi.fn(),
  getAiUsageBindings: vi.fn(),
  listAiConnections: vi.fn(),
  setAiUsageBinding: vi.fn(),
}));

vi.mock("@/api/conversations", () => ({
  listConversations: mocks.listConversations,
  createConversation: mocks.createConversation,
  getConversation: mocks.getConversation,
  renameConversation: mocks.renameConversation,
  setConversationArchived: mocks.setConversationArchived,
  deleteConversation: mocks.deleteConversation,
  saveConversationDraft: mocks.saveConversationDraft,
  attachConversationAsset: mocks.attachConversationAsset,
  linkConversation: mocks.linkConversation,
  getConversationWork: mocks.getConversationWork,
}));

vi.mock("@/api/agent", () => ({
  startAgentHost: mocks.startAgentHost,
  openAgentConversation: mocks.openAgentConversation,
  sendAgentMessage: mocks.sendAgentMessage,
  cancelAgentRun: mocks.cancelAgentRun,
  closeAgentConversation: mocks.closeAgentConversation,
  onAgentHostFrame: mocks.onAgentHostFrame,
  AGENT_HOST_EVENT: "agent-host-event",
}));

vi.mock("@/api/plans", () => ({
  listPendingPlans: mocks.listPendingPlans,
  approvePlan: mocks.approvePlan,
  rejectPlan: mocks.rejectPlan,
  getOperation: mocks.getOperation,
}));

// 助手页把「常用流程」嵌成右侧栏选项卡，挂载即读列表与步骤目录；
// 这里只替换这两个只读查询，其余（版本文案等纯函数）走真实实现。
vi.mock("@/api/workflows", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  listWorkflows: mocks.listWorkflows,
  workflowStepCatalog: mocks.workflowStepCatalog,
}));

vi.mock("@/api/operations", () => ({
  cancelOperation: mocks.cancelOperation,
  resumeOperation: mocks.resumeOperation,
  startOperation: mocks.startOperation,
  listOperationRecovery: mocks.listOperationRecovery,
}));

vi.mock("@/api/connections", () => ({
  getAiUsageBindings: mocks.getAiUsageBindings,
  listAiConnections: mocks.listAiConnections,
  setAiUsageBinding: mocks.setAiUsageBinding,
}));

const refA = "conversation_aaaaaaaaaaaaaaaa";
const refB = "conversation_bbbbbbbbbbbbbbbb";

const summary = (ref: string, title: string): ConversationSummary => ({
  conversationRef: ref,
  title,
  status: "active",
  createdAtUnixSeconds: 1,
  updatedAtUnixSeconds: 2,
});

function viewOf(ref: string, title: string): ConversationView {
  return {
    conversationRef: ref,
    title,
    status: "active",
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 2,
    drafts: [],
    attachments: [],
    links: [],
  };
}

/** 点击后的异步链（create→select→open→hydrate/refresh）落在测试之外就是 act 噪声，收尾 flush 干净。 */
async function flushAssistant() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

/** 让助手页直接进入「有活动会话且就绪」的主界面：右侧栏只在主界面渲染，空态页没有。 */
async function activateReadyConversation() {
  const readyRuntime = {
    messages: [],
    status: "ready" as const,
    activeRunId: null,
    stoppingRunId: null,
    lastSequence: {},
    openMode: "resume" as const,
    error: null,
  };
  useAgentSessionStore.setState((s) => ({ runtimes: { ...s.runtimes, [refB]: readyRuntime } }));
  await act(async () => {
    useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
  });
}

/** 把视口 mock 成窄窗，让 `useMediaQuery(NARROW_ASSISTANT_QUERY)` 判真（jsdom 默认没有窄窗）。 */
function stubNarrowViewport() {
  vi.stubGlobal(
    "matchMedia",
    ((query: string) => ({
      matches: query === NARROW_ASSISTANT_QUERY,
      media: query,
      onchange: null,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      addListener: () => undefined,
      removeListener: () => undefined,
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  useConversationStore.setState({
    conversations: [summary(refA, "甲"), summary(refB, "乙")],
    activeRef: null,
    active: null,
    draftsByConversation: {},
    loading: false,
    error: null,
  });
  useAgentSessionStore.getState().reset();
  useAssistantFocusStore.getState().clear();
  // 右侧栏的选项卡与两个面板 store 是模块级单例：上一个用例打开过就会漏到下一个。
  useWorkflowPanelStore.setState({ open: false, draft: null, draftingFrom: null, error: null });
  usePendingPlansStore.setState({ open: false, plans: [], loading: false, error: null });

  mocks.listWorkflows.mockResolvedValue([]);
  mocks.workflowStepCatalog.mockResolvedValue([]);
  mocks.listPendingPlans.mockResolvedValue([]);

  mocks.listConversations.mockResolvedValue([summary(refA, "甲"), summary(refB, "乙")]);
  mocks.getConversation.mockImplementation(async (ref: string) =>
    ref === refA ? viewOf(refA, "甲") : viewOf(refB, "乙"),
  );
  mocks.startAgentHost.mockResolvedValue({
    hostInstanceId: "instance_aaaaaaaaaaaaaaaa",
    protocolVersion: 1,
    sdkVersion: "1.0.4",
    recoveredStrandedHost: false,
    modelNetworkEnabled: false,
  });
  mocks.openAgentConversation.mockResolvedValue({
    hostInstanceId: "instance_aaaaaaaaaaaaaaaa",
    conversationRef: refA,
    sessionId: "session_aaaaaaaaaaaaaaaa",
  });
  mocks.onAgentHostFrame.mockResolvedValue(() => undefined);
  mocks.getConversationWork.mockResolvedValue({ plans: [], operations: [] });
  mocks.listOperationRecovery.mockResolvedValue([]);
  mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: null, agent: null });
  mocks.listAiConnections.mockResolvedValue([]);
});

afterEach(() => {
  // 窄窗用例改的是全局 matchMedia；不清掉会污染后续用例的布局判定。
  vi.unstubAllGlobals();
});

describe("AssistantPage", () => {
  it("没有活动会话时提示选择或新建，不渲染任何消息", async () => {
    // 一个会话都没有：最近恢复无目标，必须保持空白提示，且不得凭空创建会话。
    useConversationStore.setState({ conversations: [], activeRef: null, active: null });
    mocks.listConversations.mockResolvedValue([]);
    render(<AssistantPage />);
    await waitFor(() => {
      expect(screen.getByText(/选择左侧对话，或新建一个对话开始/)).toBeInTheDocument();
    });
    expect(screen.queryByRole("log", { name: "对话消息" })).not.toBeInTheDocument();
    expect(mocks.createConversation).not.toHaveBeenCalled();
    expect(mocks.openAgentConversation).not.toHaveBeenCalled();
  });

  it("进入助手页时自动恢复最近的活动会话，不需要用户先手动点选", async () => {
    render(<AssistantPage />);
    await waitFor(() => {
      expect(mocks.openAgentConversation).toHaveBeenCalledWith(refA, "resume", "standard");
    });
    // 恢复的是列表里最近的活动会话（第一条），并且左右两栏都切到它。
    await waitFor(() => {
      expect(useConversationStore.getState().activeRef).toBe(refA);
    });
    expect(await screen.findByRole("log", { name: "对话消息" })).toBeInTheDocument();
  });

  it("最近恢复失败时给出可读提示，不静默吞掉", async () => {
    mocks.openAgentConversation.mockRejectedValue(
      new Error("Agent Host未运行或已中断；由用户明确启动或恢复后重试"),
    );
    render(<AssistantPage />);
    // 输入区也会显示「未绑定服务」提示，所以这里只看「有任意一条提示提到 Agent Host」。
    await waitFor(() => {
      const alerts = screen.getAllByRole("alert").map((node) => node.textContent ?? "");
      expect(alerts.some((text) => text.includes("Agent Host"))).toBe(true);
    });
  });

  it("空白新会话不显示占位历史，保存位置提示明确", async () => {
    useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refB]: {
          messages: [],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "new",
          error: null,
        },
      },
    }));

    render(<AssistantPage />);
    const log = await screen.findByRole("log", { name: "对话消息" });
    // 会话列表本身有 li；这里只断言消息区一条都没有。
    expect(within(log).queryAllByRole("listitem")).toHaveLength(0);
    expect(within(log).getByText(/你确认后才会执行/)).toBeInTheDocument();
    // 未绑定 Agent 服务时必须说明，而不是静默失败。
    await waitFor(() => {
      expect(screen.getByTestId("agent-model-label")).toHaveTextContent("尚未绑定 Agent 服务");
    });
  });

  it("两个会话各自展示自己的消息，切换后不串内容", async () => {
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refA]: {
          messages: [
            { id: "user_run_a", role: "user", text: "甲的提问", runId: "run_a", status: "complete" },
            { id: "assistant_run_a", role: "assistant", text: "甲的回答", runId: "run_a", status: "complete" },
          ],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
        [refB]: {
          messages: [
            { id: "user_run_b", role: "user", text: "乙的提问", runId: "run_b", status: "complete" },
          ],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
      },
    }));

    await act(async () => {
      useConversationStore.setState({ activeRef: refA, active: viewOf(refA, "甲") });
    });
    const { rerender } = render(<AssistantPage />);
    await waitFor(() => expect(screen.getByText("甲的回答")).toBeInTheDocument());
    expect(screen.queryByText("乙的提问")).not.toBeInTheDocument();

    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
      rerender(<AssistantPage />);
    });
    await waitFor(() => expect(screen.getByText("乙的提问")).toBeInTheDocument());
    expect(screen.queryByText("甲的回答")).not.toBeInTheDocument();
  });

  it("流式回答中显示「停止回答」，点击后只取消 run", async () => {
    mocks.cancelAgentRun.mockResolvedValue(undefined);
    useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refB]: {
          messages: [
            { id: "assistant_run_c", role: "assistant", text: "正在", runId: "run_c", status: "streaming" },
          ],
          status: "streaming",
          activeRunId: "run_c",
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
      },
    }));

    render(<AssistantPage />);
    const stop = await screen.findByRole("button", { name: "停止回答" });
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.cancelAgentRun).toHaveBeenCalledWith(refB, "run_c"));
    // 停止回答不落任何 operation 变更。
    expect(mocks.cancelOperation).not.toHaveBeenCalled();
  });

  it("发送结果未确认时也给出精确取消入口，且不允许接着发", async () => {
    // A3-04/A3-09：ACK 未知时协议要求保留 runId 供 GUI 精确取消。
    // 旧实现把这个 run 丢掉，界面只剩「未知」提示，用户既不能停也不能安全重发。
    mocks.cancelAgentRun.mockResolvedValue(undefined);
    useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refB]: {
          messages: [
            { id: "user_run_u", role: "user", text: "把这几张移入回收站", runId: "run_u", status: "complete" },
          ],
          status: "unknown",
          activeRunId: "run_u",
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
      },
      anomalies: [
        {
          id: "anomaly_test",
          conversationRef: refB,
          runId: "run_u",
          code: "OUTCOME_UNKNOWN",
          message: "本轮发送结果未知，已保留运行标识；请核对后决定是否停止，不要重复发送同一提示词。",
          atUnixMs: 1,
        },
      ],
    }));

    render(<AssistantPage />);
    // 状态行说明结果未确认。
    await waitFor(() =>
      expect(screen.getByText("上一轮发送结果未知，请核对后再决定")).toBeInTheDocument(),
    );
    // 异常单列区保留 runId 级别的提示。
    expect(screen.getByText("结果待核对")).toBeInTheDocument();

    const input = screen.getByLabelText("输入消息");
    fireEvent.change(input, { target: { value: "再试一次" } });
    // 发送按钮必须禁用：结果未确认时不得重放提示词。
    const send = screen.getByRole("button", { name: "发送" });
    expect(send).toBeDisabled();
    expect(send).toHaveAttribute("disabled");

    const stop = await screen.findByRole("button", { name: "停止回答" });
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.cancelAgentRun).toHaveBeenCalledWith(refB, "run_u"));
  });

  it("草稿按会话隔离，切换后不把半句话带到另一个会话", async () => {    useConversationStore.setState({
      draftsByConversation: {
        [refA]: { assistant: { text: "甲还没发的话" } },
        [refB]: {},
      },
    });
    const readyRuntime = () => ({
      messages: [],
      status: "ready" as const,
      activeRunId: null,
      stoppingRunId: null,
      lastSequence: {},
      openMode: "resume" as const,
      error: null,
    });
    useAgentSessionStore.setState((s) => ({
      runtimes: { ...s.runtimes, [refA]: readyRuntime(), [refB]: readyRuntime() },
    }));

    await act(async () => {
      useConversationStore.setState({ activeRef: refA, active: viewOf(refA, "甲") });
    });
    const { rerender } = render(<AssistantPage />);
    const input = await screen.findByLabelText("输入消息");
    await waitFor(() => expect(input).toHaveValue("甲还没发的话"));

    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
      rerender(<AssistantPage />);
    });
    await waitFor(() => expect(screen.getByLabelText("输入消息")).toHaveValue(""));
  });

  it("输入区展示当前服务并可切换；外发摘要如实说明只发文字且不回显密钥", async () => {
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "connection_aaaaaaaaaaaaaaaa",
    });
    mocks.listAiConnections.mockResolvedValue([
      {
        id: "connection_aaaaaaaaaaaaaaaa",
        name: "云端甲",
        deployment: "cloud",
        protocol: "openai_chat",
        baseUrl: "https://api.example/v1",
        model: "gpt-test",
        maxConcurrency: 4,
        requestsPerMinute: 60,
        requestsPerHour: 600,
        hasKey: true,
        credentialStatus: "configured",
        enabled: true,
      },
      {
        id: "connection_bbbbbbbbbbbbbbbb",
        name: "本机乙",
        deployment: "local",
        protocol: "openai_chat",
        baseUrl: "http://localhost:11434/v1",
        model: "qwen3.5:4b",
        maxConcurrency: 1,
        requestsPerMinute: 60,
        requestsPerHour: 600,
        hasKey: false,
        credentialStatus: "missing",
        enabled: true,
      },
    ]);
    mocks.setAiUsageBinding.mockResolvedValue(undefined);

    const readyRuntime = {
      messages: [],
      status: "ready" as const,
      activeRunId: null,
      stoppingRunId: null,
      lastSequence: {},
      openMode: "resume" as const,
      error: null,
    };
    useAgentSessionStore.setState((s) => ({ runtimes: { ...s.runtimes, [refB]: readyRuntime } }));
    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    });

    render(<AssistantPage />);
    // 当前服务：模型名 + 在线/服务名，不含任何密钥值。
    await waitFor(() => {
      expect(screen.getByTestId("agent-model-label")).toHaveTextContent("模型：gpt-test");
    });
    // A3-06：助手通道只发文字，摘要不得宣称发送压缩图片或可提供原始清晰度。
    const outboundCloud = screen.getByTestId("agent-outbound-summary");
    expect(outboundCloud).toHaveTextContent("文字请求会发到 云端甲（模型 gpt-test），这是一个在线地址。");
    expect(outboundCloud).toHaveTextContent("不发送图片内容本身");
    expect(outboundCloud).not.toHaveTextContent(/JPEG|1024px|原始清晰度/);

    fireEvent.click(screen.getByLabelText("Agent 服务"));
    fireEvent.click(screen.getByRole("menuitem", { name: /qwen3\.5/ }));
    await waitFor(() =>
      expect(mocks.setAiUsageBinding).toHaveBeenCalledWith("agent", "connection_bbbbbbbbbbbbbbbb"),
    );
    // 本机服务：由地址（localhost）推出本机，而不是采信 deployment 声明。
    await waitFor(() =>
      expect(screen.getByTestId("agent-outbound-summary")).toHaveTextContent("不会离开这台电脑"),
    );
    expect(screen.getByTestId("agent-outbound-summary")).toHaveTextContent("请求会发到本机地址");
    // 本机服务没有 API 密钥是正常状态，不能报「密钥不可用，发送会失败」。
    expect(screen.queryByText(/密钥不可用/)).not.toBeInTheDocument();
  });

  it("声明为在线但地址是本机时，不输出「不会离开这台电脑」并说明声明与地址不一致", async () => {
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "connection_cccccccccccccccc",
    });
    mocks.listAiConnections.mockResolvedValue([
      {
        id: "connection_cccccccccccccccc",
        name: "错标甲",
        // 声明 cloud，地址却是回环：旧实现会说「发送给云端」，实际不出本机。
        deployment: "cloud",
        protocol: "openai_chat",
        baseUrl: "http://127.0.0.1:3425/v1",
        model: "mimo-test",
        maxConcurrency: 1,
        requestsPerMinute: 60,
        requestsPerHour: 600,
        hasKey: true,
        credentialStatus: "configured",
        enabled: true,
      },
    ]);
    const readyRuntime = {
      messages: [],
      status: "ready" as const,
      activeRunId: null,
      stoppingRunId: null,
      lastSequence: {},
      openMode: "resume" as const,
      error: null,
    };
    useAgentSessionStore.setState((s) => ({ runtimes: { ...s.runtimes, [refB]: readyRuntime } }));
    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    });

    render(<AssistantPage />);
    await waitFor(() => {
      expect(screen.getByTestId("agent-outbound-summary")).toHaveTextContent("请求会发到本机地址");
    });
    // 部署方式声明与地址矛盾必须能查到，不能静默沿用声明：
    // 常态界面只留模型名，完整判定（本机/在线 + 声明矛盾）收进选择器的 title。
    const trigger = screen.getByLabelText("Agent 服务");
    expect(trigger.getAttribute("title")).toContain("与地址不一致");
    expect(trigger.getAttribute("title")).toContain("设置的部署方式是「在线」");
    expect(screen.getByTestId("agent-outbound-summary")).toHaveTextContent("不会离开这台电脑");
  });

  it("云端服务缺密钥时才提示发送会失败，并说明不会自动改用其他服务", async () => {
    mocks.getAiUsageBindings.mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "connection_dddddddddddddddd",
    });
    mocks.listAiConnections.mockResolvedValue([
      {
        id: "connection_dddddddddddddddd",
        name: "云端丁",
        deployment: "cloud",
        protocol: "openai_chat",
        baseUrl: "https://api.example/v1",
        model: "gpt-test",
        maxConcurrency: 4,
        requestsPerMinute: 60,
        requestsPerHour: 600,
        hasKey: false,
        credentialStatus: "missing",
        enabled: true,
      },
    ]);
    const readyRuntime = {
      messages: [],
      status: "ready" as const,
      activeRunId: null,
      stoppingRunId: null,
      lastSequence: {},
      openMode: "resume" as const,
      error: null,
    };
    useAgentSessionStore.setState((s) => ({ runtimes: { ...s.runtimes, [refB]: readyRuntime } }));
    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    });

    render(<AssistantPage />);
    const alert = await screen.findByText(/密钥不可用，发送会失败/);
    // 只留一句可执行的去处；「不会自动换服务」「草稿没丢」这类解释不再占版面。
    expect(alert).toHaveTextContent("设置");
    expect(alert).not.toHaveTextContent("已写的内容不会丢");
  });

  it("工作台按需展开：默认不占宽度，展开后可收起", async () => {
    const readyRuntime = {
      messages: [],
      status: "ready" as const,
      activeRunId: null,
      stoppingRunId: null,
      lastSequence: {},
      openMode: "resume" as const,
      error: null,
    };
    useAgentSessionStore.setState((s) => ({ runtimes: { ...s.runtimes, [refB]: readyRuntime } }));
    await act(async () => {
      useConversationStore.setState({ activeRef: refB, active: viewOf(refB, "乙") });
    });

    render(<AssistantPage />);
    expect(screen.queryByLabelText("右侧工作台")).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "工作台" }));
    expect(await screen.findByLabelText("右侧工作台")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "收起" }));
    await waitFor(() => expect(screen.queryByLabelText("右侧工作台")).not.toBeInTheDocument());
  });

  it("头部三个图标打开同一个右侧栏，只是切到不同选项卡", async () => {
    await activateReadyConversation();
    render(<AssistantPage />);

    // 默认不占宽度
    expect(screen.queryByLabelText("助手侧栏")).not.toBeInTheDocument();

    // 「常用流程」打开的是右侧栏，不是模态框
    fireEvent.click(await screen.findByRole("button", { name: "常用流程" }));
    const panel = await screen.findByLabelText("助手侧栏");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(panel).getByRole("tab", { name: "常用流程" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    // 流程内容真的渲染在侧栏里（不是空壳）：搜索框与新建入口都在
    expect(await within(panel).findByLabelText("查找常用流程")).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "新建" })).toBeInTheDocument();
    expect(mocks.listWorkflows).toHaveBeenCalledTimes(1);

    // 「待审计划」只是把同一个侧栏切到另一个选项卡
    fireEvent.click(screen.getByRole("button", { name: "待审计划" }));
    const switched = await screen.findByLabelText("助手侧栏");
    expect(switched).toBe(panel);
    expect(within(switched).getByRole("tab", { name: "待审计划" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(await within(switched).findByTestId("pending-plans-list")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();

    // 侧栏里自己也能切回「常用流程」，不需要再点头部图标
    fireEvent.click(within(switched).getByRole("tab", { name: "常用流程" }));
    expect(
      await within(screen.getByLabelText("助手侧栏")).findByLabelText("查找常用流程"),
    ).toBeInTheDocument();

    // 再点同一个图标 → 收起，并复位两个面板 store
    fireEvent.click(screen.getByRole("button", { name: "常用流程" }));
    await waitFor(() => expect(screen.queryByLabelText("助手侧栏")).not.toBeInTheDocument());
    expect(useWorkflowPanelStore.getState().open).toBe(false);
    expect(usePendingPlansStore.getState().open).toBe(false);
  });

  it("外部入口（store.open）在助手页落到侧栏选项卡，不另开模态框", async () => {
    await activateReadyConversation();
    render(<AssistantPage />);
    await waitFor(() => expect(screen.getByRole("log", { name: "对话消息" })).toBeInTheDocument());

    // 任务卡上的「保存为常用流程」走的是 workflowPanelStore.open
    await act(async () => {
      useWorkflowPanelStore.setState({ open: true });
    });
    const panel = await screen.findByLabelText("助手侧栏");
    expect(within(panel).getByRole("tab", { name: "常用流程" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(await within(panel).findByLabelText("查找常用流程")).toBeInTheDocument();

    // 待审计划同理
    await act(async () => {
      usePendingPlansStore.setState({ open: true });
    });
    expect(await screen.findByTestId("pending-plans-list")).toBeInTheDocument();
    expect(
      within(screen.getByLabelText("助手侧栏")).getByRole("tab", { name: "待审计划" }),
    ).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("从浮层「查看并确认」进来时定位到同一张计划卡并清掉待定位目标", async () => {
    // A3-09「定位完整助手页同一计划」：只做一次裸导航的话，用户还得在一列
    // 计划里自己找那一条。
    const plan = {
      schemaVersion: 1,
      planId: "plan_zzzzzzzzzzzzzzzz",
      capability: "assets.trash.submit",
      reviewStatus: "pending" as const,
      reviewKind: "single" as const,
      itemCount: 2,
      summary: "把 2 项素材移入回收站",
      reviewRef: "review_zzzzzzzzzzzzzzzz",
      expiresAtUnixSeconds: 9_999_999_999,
      scope: {},
      budget: { maxRequests: 1, maxTokens: 0, maxSeconds: 60, maxItems: 2 },
    };
    const other = { ...plan, planId: "plan_yyyyyyyyyyyyyyyy", summary: "另一条计划" };
    mocks.getConversationWork.mockResolvedValue({ plans: [other, plan], operations: [] });
    mocks.getConversation.mockImplementation(async (ref: string) => ({
      ...viewOf(ref, "甲"),
      links: [
        { kind: "plan", reference: other.planId },
        { kind: "plan", reference: plan.planId },
      ],
    }));
    useAssistantFocusStore.getState().focusPlan(plan.planId);

    render(<AssistantPage />);

    const card = await waitFor(() => {
      const node = document.querySelector<HTMLElement>(`[data-plan-id="${plan.planId}"]`);
      expect(node).not.toBeNull();
      expect(node).toHaveAttribute("data-plan-highlight", "true");
      return node;
    });
    expect(card).toBeTruthy();
    // 只高亮被点名的那一张。
    expect(
      document.querySelector<HTMLElement>(`[data-plan-id="${other.planId}"]`),
    ).not.toHaveAttribute("data-plan-highlight");
    // 目标被消费，下次进页面不会又冒出高亮。
    expect(useAssistantFocusStore.getState().planId).toBeNull();
  });

  it("点「新建对话」建出空白新会话，不继承上一个会话的草稿与待执行清单", async () => {
    // R10「启动恢复最近对话并可新建，新会话内容为空」：既有「能新建」的正向证据，
    // 也要证明新会话不夹带上一个会话的草稿和待审计划。
    const refC = "conversation_cccccccccccccccc";
    const oldPlan = {
      schemaVersion: 1,
      planId: "plan_oooooooooooooooo",
      capability: "assets.trash.submit",
      reviewStatus: "pending",
      reviewKind: "single",
      itemCount: 1,
      summary: "把 1 项素材移入回收站",
      reviewRef: "review_oooooooooooooooo",
      expiresAtUnixSeconds: 9_999_999_999,
      scope: {},
      budget: { maxRequests: 1, maxTokens: 0, maxSeconds: 60, maxItems: 1 },
    };
    // 上一个会话「甲」带着半句草稿和一张待审计划卡；新会话必须两样都没有。
    mocks.getConversation.mockImplementation(async (ref: string) =>
      ref === refC
        ? viewOf(refC, "新对话")
        : {
            ...viewOf(refA, "甲"),
            drafts: [
              { kind: "assistant", payload: { text: "甲还没发的话" }, revision: 1, updatedAtUnixSeconds: 2 },
            ],
          },
    );
    mocks.getConversationWork.mockImplementation(async (ref: string) =>
      ref === refA ? { plans: [oldPlan], operations: [] } : { plans: [], operations: [] },
    );
    mocks.createConversation.mockResolvedValue(summary(refC, "新对话"));

    render(<AssistantPage />);

    // 先确认恢复出来的「甲」确实带着草稿和一张计划卡，后面的空白才有对照意义。
    await waitFor(() => expect(useConversationStore.getState().activeRef).toBe(refA));
    await waitFor(() => expect(screen.getByLabelText("输入消息")).toHaveValue("甲还没发的话"));
    await waitFor(() =>
      expect(document.querySelector(`[data-plan-id="${oldPlan.planId}"]`)).not.toBeNull(),
    );

    fireEvent.click(screen.getByRole("button", { name: "新建对话" }));

    await waitFor(() => expect(useConversationStore.getState().activeRef).toBe(refC));
    await flushAssistant();

    // 正向：真的调了一次后端新建，并把视图切到新会话。
    expect(mocks.createConversation).toHaveBeenCalledTimes(1);
    expect(mocks.createConversation).toHaveBeenCalledWith("新对话");
    expect(useConversationStore.getState().active?.title).toBe("新对话");
    expect(screen.getByRole("heading", { name: "新对话" })).toBeInTheDocument();
    expect(mocks.openAgentConversation).toHaveBeenCalledWith(refC, "new", "standard");

    // 新会话内容为空：消息数 0，消息区只剩空态提示。
    expect(useAgentSessionStore.getState().runtimes[refC]?.messages ?? []).toHaveLength(0);
    const log = screen.getByRole("log", { name: "对话消息" });
    expect(within(log).queryAllByRole("listitem")).toHaveLength(0);
    expect(within(log).getByText(/你确认后才会执行/)).toBeInTheDocument();

    // 不继承草稿：输入框为空，上一个会话的半句话仍留在它自己名下。
    expect(screen.getByLabelText("输入消息")).toHaveValue("");
    expect(useConversationStore.getState().draftsByConversation[refC]).toEqual({});
    expect(useConversationStore.getState().draftsByConversation[refA]).toEqual({
      assistant: { text: "甲还没发的话" },
    });

    // 不继承待执行清单：新会话没有归属任何计划，页面上也没有残留计划卡。
    expect(useAgentSessionStore.getState().linkedPlans[refC] ?? []).toEqual([]);
    expect(document.querySelector(`[data-plan-id="${oldPlan.planId}"]`)).toBeNull();
    expect(document.querySelectorAll("[data-plan-id]")).toHaveLength(0);
  });

  it("排队中的任务不阻塞聊天：输入框照常可用，也不会被自动启动", async () => {
    // R25 后半句：长写任务默认停在队列里等待开始，这段等待期不能把聊天锁住，
    // 也不能因为页面刷新/重进就把排队任务悄悄跑起来（开始必须由用户点）。
    const queuedId = "operation_cccccccccccccccccccccccccccccccc";
    const queuedSnapshot = {
      operationId: queuedId,
      capability: "assets.trash.submit",
      state: "queued",
      phase: "ready",
      revision: 1,
      completed: 0,
      failed: 0,
      total: 3,
      errorCode: null,
      canCancel: true,
      canResume: false,
      updatedAtUnixMs: 1_700_000_000_000,
    } as unknown as OperationSnapshot;
    // 归属由后端裁决：hydrate 会用 getConversationWork 的结果覆盖 linkedOperations，
    // 所以排队任务必须从后端口径进入，测试里不能用 setState 直接塞。
    mocks.getConversationWork.mockResolvedValue({ plans: [], operations: [queuedSnapshot] });
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refA]: {
          messages: [
            { id: "user_run_q", role: "user", text: "把这三张移进回收站", runId: "run_q", status: "complete" },
            { id: "assistant_run_q", role: "assistant", text: "已准备好计划。", runId: "run_q", status: "complete" },
          ],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
      },
    }));
    useConversationStore.setState({ activeRef: refA, active: viewOf(refA, "甲") });

    render(<AssistantPage />);
    await waitFor(() => expect(screen.getByText("任务 · 排队中")).toBeInTheDocument());
    await flushAssistant();

    // 聊天照常可用：历史消息还在，输入框可用，还能继续发新的提问。
    expect(screen.getByText("已准备好计划。")).toBeInTheDocument();
    const input = screen.getByLabelText("输入消息");
    expect(input).not.toBeDisabled();
    fireEvent.change(input, { target: { value: "顺便看看还有多少张没打标" } });
    expect(input).toHaveValue("顺便看看还有多少张没打标");

    // 只读浏览的前置：任务卡在排队态，页面没有把它当成正在改文件而锁住任何入口。
    expect(screen.getByRole("button", { name: "开始执行" })).toBeInTheDocument();
    // 排队 => 开始执行必须由用户点，页面加载本身绝不能调用后端启动。
    expect(mocks.startOperation).not.toHaveBeenCalled();
  });

  it("窄窗把历史收成浮层：聊天区照常保留，用户点开浮层能选回旧会话", async () => {
    // R14 的「窄窗」半边：历史不再常驻一列，但聊天区必须始终可用，
    // 且用户能自己打开历史浮层、选回旧会话（不靠键盘快捷键或刷新）。
    stubNarrowViewport();
    useAgentSessionStore.setState((s) => ({
      runtimes: {
        ...s.runtimes,
        [refA]: {
          messages: [
            { id: "user_run_a", role: "user", text: "甲的提问", runId: "run_a", status: "complete" },
            { id: "assistant_run_a", role: "assistant", text: "甲的回答", runId: "run_a", status: "complete" },
          ],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
        [refB]: {
          messages: [
            { id: "user_run_b", role: "user", text: "乙的提问", runId: "run_b", status: "complete" },
            { id: "assistant_run_b", role: "assistant", text: "乙的回答", runId: "run_b", status: "complete" },
          ],
          status: "ready",
          activeRunId: null,
          stoppingRunId: null,
          lastSequence: {},
          openMode: "resume",
          error: null,
        },
      },
    }));
    useConversationStore.setState({ activeRef: refA, active: viewOf(refA, "甲") });

    render(<AssistantPage />);
    await waitFor(() => expect(screen.getByText("甲的回答")).toBeInTheDocument());

    // 窄窗默认把历史收起来：侧栏不在 DOM 里，聊天区（消息 + 输入框）仍然渲染。
    expect(screen.queryByLabelText("对话历史")).not.toBeInTheDocument();
    expect(screen.getByLabelText("输入消息")).toBeInTheDocument();
    expect(screen.getByRole("log", { name: "对话消息" })).toBeInTheDocument();

    // 手动恢复入口：点「对话历史」把浮层打开（同时出现遮罩，点遮罩可收起）。
    const toggle = screen.getByRole("button", { name: "打开对话历史" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    const drawer = screen.getByLabelText("对话历史");
    expect(screen.getByRole("button", { name: "关闭对话历史" })).toBeInTheDocument();

    // 在浮层里选回旧会话「乙」：聊天切到乙的内容，浮层随之收起，聊天区没被带走。
    await act(async () => {
      fireEvent.click(within(drawer).getByTitle("乙"));
    });
    await waitFor(() => expect(useConversationStore.getState().activeRef).toBe(refB));
    await flushAssistant();
    expect(screen.queryByLabelText("对话历史")).not.toBeInTheDocument();
    const switched = screen.getByRole("log", { name: "对话消息" });
    expect(within(switched).getByText("乙的回答")).toBeInTheDocument();
    expect(within(switched).queryByText("甲的回答")).not.toBeInTheDocument();

    // 再开一次浮层、点遮罩收起后，聊天内容仍在（收起历史不影响聊天）。
    fireEvent.click(screen.getByRole("button", { name: "打开对话历史" }));
    fireEvent.click(screen.getByRole("button", { name: "关闭对话历史" }));
    expect(screen.queryByLabelText("对话历史")).not.toBeInTheDocument();
    expect(within(screen.getByRole("log", { name: "对话消息" })).getByText("乙的回答")).toBeInTheDocument();
  });

  it("头部「技能库」打开侧栏技能库，选中技能后注入输入框并收起抽屉", async () => {
    await activateReadyConversation();
    render(<AssistantPage />);

    // 头部「技能库」打开同一个侧栏，并切到「技能库」选项卡。
    fireEvent.click(await screen.findByRole("button", { name: "技能库" }));
    const panel = await screen.findByLabelText("助手侧栏");
    expect(within(panel).getByRole("tab", { name: "技能库" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    // 点第一条技能卡片进入详情，再点「在对话中使用」。
    const list = await within(panel).findByRole("list", { name: "技能列表" });
    const firstCard = within(list).getAllByRole("listitem")[0];
    fireEvent.click(within(firstCard).getAllByRole("button")[0]);
    fireEvent.click(await within(panel).findByRole("button", { name: "在对话中使用" }));

    // 抽屉收起，技能正文作为操作参考进入输入框。
    await waitFor(() => expect(screen.queryByLabelText("助手侧栏")).not.toBeInTheDocument());
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    await waitFor(() => expect(textarea.value).toContain(`【技能】${BUILTIN_SKILLS[0].name}`));
  });
});
