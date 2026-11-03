/**
 * AssistantPage 启动体验测试（WP1.4）。
 *
 * 只覆盖本次改动：
 * - Host 启动中先出骨架 + 可见文案，不闪「选择左侧对话」默认文案；
 * - 空态提供「新建对话」主 CTA，点击建空白会话并进入；
 * - conversationStore.refresh 并发调用共享同一 in-flight Promise（只打一次后端）；
 * - 错误 / 中断两态各有可行动 CTA。
 *
 * 不重复 AssistantPage.test.tsx 里已有的会话隔离/发送/面板等用例。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import AssistantPage from "@/pages/AssistantPage";
import { useConversationStore } from "@/stores/conversationStore";
import { useAgentSessionStore } from "@/stores/agentSessionStore";
import { useAssistantFocusStore } from "@/stores/assistantFocusStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import { AppError } from "@/api/client";
import type { ConversationSummary, ConversationView } from "@/api/conversations";
import { getStartupMarks, resetStartupMarks } from "@/utils/startupMarks";

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

const summary = (ref: string, title: string): ConversationSummary => ({
  conversationRef: ref,
  title,
  status: "active",
  createdAtUnixSeconds: 1,
  updatedAtUnixSeconds: 2,
});

const viewOf = (ref: string, title: string): ConversationView => ({
  conversationRef: ref,
  title,
  status: "active",
  createdAtUnixSeconds: 1,
  updatedAtUnixSeconds: 2,
  drafts: [],
  attachments: [],
  links: [],
});

/** 收尾异步链（refresh/select/open/hydrate），避免 act 噪声。 */
async function flushAssistant() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const hostInfo = {
  hostInstanceId: "instance_aaaaaaaaaaaaaaaa",
  protocolVersion: 1,
  sdkVersion: "1.0.4",
  recoveredStrandedHost: false,
  modelNetworkEnabled: false,
};

beforeEach(async () => {
  vi.clearAllMocks();
  resetStartupMarks();
  useConversationStore.setState({
    conversations: [],
    activeRef: null,
    active: null,
    draftsByConversation: {},
    loading: false,
    error: null,
  });
  useAgentSessionStore.getState().reset();
  useAssistantFocusStore.getState().clear();
  useWorkflowPanelStore.setState({ open: false, draft: null, draftingFrom: null, error: null });
  usePendingPlansStore.setState({ open: false, plans: [], loading: false, error: null });

  mocks.listWorkflows.mockResolvedValue([]);
  mocks.workflowStepCatalog.mockResolvedValue([]);
  mocks.listPendingPlans.mockResolvedValue([]);
  mocks.listConversations.mockResolvedValue([]);
  mocks.getConversation.mockImplementation(async (ref: string) => viewOf(ref, "新对话"));
  mocks.createConversation.mockResolvedValue(summary(refA, "新对话"));
  mocks.startAgentHost.mockResolvedValue(hostInfo);
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

  // 让上一个用例遗留的单飞 Promise 收尾（module 级句柄在 finally 里清空）。
  await Promise.resolve();
  await Promise.resolve();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("AssistantPage · 启动体验（WP1.4）", () => {
  it("Host 启动中先出骨架并显示「Agent 服务正在启动…」，不闪默认空态文案", async () => {
    let resolveHost: (value: typeof hostInfo) => void = () => {};
    mocks.startAgentHost.mockReturnValue(
      new Promise<typeof hostInfo>((resolve) => {
        resolveHost = resolve;
      }),
    );

    render(<AssistantPage />);

    // 首帧即骨架 + 可见启动文案。
    await waitFor(() =>
      expect(screen.getByText("Agent 服务正在启动…")).toBeInTheDocument(),
    );
    expect(document.querySelector('[aria-busy="true"]')).not.toBeNull();
    // 启动期间不得提前显示「选择左侧对话」的默认文案。
    expect(screen.queryByText(/选择左侧对话/)).not.toBeInTheDocument();

    // 收尾：让 Host 就绪，避免悬挂的 Promise 泄漏到后续用例。
    await act(async () => {
      resolveHost(hostInfo);
      await Promise.resolve();
    });
    await flushAssistant();
  });

  it("空态（无会话且 Host 就绪）给出「新建对话」CTA，点击建空白会话并进入", async () => {
    render(<AssistantPage />);

    // 就绪后出现空态文案与主 CTA。
    await waitFor(() =>
      expect(screen.getByText(/选择左侧对话，或新建一个对话开始/)).toBeInTheDocument(),
    );
    const cta = screen.getByTestId("empty-new-conversation");
    expect(cta).toBeInTheDocument();

    fireEvent.click(cta);
    await waitFor(() => expect(mocks.createConversation).toHaveBeenCalledWith("新对话"));
    await waitFor(() => expect(mocks.openAgentConversation).toHaveBeenCalled());
    await flushAssistant();
  });

  it("conversationStore.refresh 并发调用共享同一 in-flight Promise，只打一次后端", async () => {
    let resolveList: (value: ConversationSummary[]) => void = () => {};
    mocks.listConversations.mockReturnValueOnce(
      new Promise<ConversationSummary[]>((resolve) => {
        resolveList = resolve;
      }),
    );

    const store = useConversationStore.getState();
    const first = store.refresh();
    const second = store.refresh();
    // 第二次调用复用同一 Promise，没有再次请求后端。
    expect(mocks.listConversations).toHaveBeenCalledTimes(1);

    resolveList([summary(refA, "甲")]);
    await Promise.all([first, second]);

    expect(useConversationStore.getState().conversations).toHaveLength(1);
    expect(useConversationStore.getState().loading).toBe(false);
  });

  it("Host 启动失败时显示重试 CTA", async () => {
    mocks.startAgentHost.mockRejectedValue(new Error("boom"));

    render(<AssistantPage />);

    await waitFor(() =>
      expect(screen.getByText(/Agent 服务启动失败/)).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    await flushAssistant();
  });

  it("Host 上次被中断时显示「恢复 Agent 服务」CTA", async () => {
    mocks.startAgentHost.mockRejectedValue(
      new AppError("AGENT_HOST_INTERRUPTED", "Agent 服务上次被异常结束"),
    );

    render(<AssistantPage />);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "恢复 Agent 服务" })).toBeInTheDocument(),
    );
    await flushAssistant();
  });

  it("启动埋点：骨架先于 Host 就绪（首帧不被启动结果 gate）", async () => {
    let resolveHost: (value: typeof hostInfo) => void = () => {};
    mocks.startAgentHost.mockReturnValue(
      new Promise<typeof hostInfo>((resolve) => {
        resolveHost = resolve;
      }),
    );

    render(<AssistantPage />);
    await waitFor(() => expect(screen.getByText("Agent 服务正在启动…")).toBeInTheDocument());

    // Host 仍挂起时，骨架打点已落地，Host 就绪打点尚未出现。
    const pending = getStartupMarks().map((entry) => entry.mark);
    expect(pending).toContain("assistant_skeleton_visible");
    expect(pending).not.toContain("assistant_host_ready");

    await act(async () => {
      resolveHost(hostInfo);
      await Promise.resolve();
    });
    await flushAssistant();

    // 就绪后：Host 就绪打点必须晚于骨架打点。
    const marks = getStartupMarks().map((entry) => entry.mark);
    const skeletonAt = marks.indexOf("assistant_skeleton_visible");
    const hostReadyAt = marks.indexOf("assistant_host_ready");
    expect(skeletonAt).toBeGreaterThanOrEqual(0);
    expect(hostReadyAt).toBeGreaterThan(skeletonAt);
  });
});
