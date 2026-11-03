import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import FloatingAssistant from "@/components/assistant/FloatingAssistant";
import { useAgentSessionStore } from "@/stores/agentSessionStore";
import { useAssistantFocusStore } from "@/stores/assistantFocusStore";
import { useConversationStore } from "@/stores/conversationStore";

const mocks = vi.hoisted(() => ({
  listConversations: vi.fn(),
  getConversation: vi.fn(),
  getConversationWork: vi.fn(),
  createConversation: vi.fn(),
  saveConversationDraft: vi.fn(),
  listPendingPlans: vi.fn(),
  getOperation: vi.fn(),
  openAgentConversation: vi.fn(),
  closeAgentConversation: vi.fn(),
  sendAgentMessage: vi.fn(),
  cancelAgentRun: vi.fn(),
  startAgentHost: vi.fn(),
  onAgentHostFrame: vi.fn(),
  listAiConnections: vi.fn(),
  getAiUsageBindings: vi.fn(),
  setAiUsageBinding: vi.fn(),
}));

vi.mock("@/api/conversations", async () => {
  const actual = await vi.importActual<typeof import("@/api/conversations")>("@/api/conversations");
  return {
    ...actual,
    listConversations: mocks.listConversations,
    getConversation: mocks.getConversation,
    getConversationWork: mocks.getConversationWork,
    createConversation: mocks.createConversation,
    saveConversationDraft: mocks.saveConversationDraft,
  };
});

vi.mock("@/api/plans", async () => {
  const actual = await vi.importActual<typeof import("@/api/plans")>("@/api/plans");
  return {
    ...actual,
    listPendingPlans: mocks.listPendingPlans,
    getOperation: mocks.getOperation,
  };
});

vi.mock("@/api/agent", () => ({
  startAgentHost: mocks.startAgentHost,
  openAgentConversation: mocks.openAgentConversation,
  sendAgentMessage: mocks.sendAgentMessage,
  cancelAgentRun: mocks.cancelAgentRun,
  closeAgentConversation: mocks.closeAgentConversation,
  onAgentHostFrame: mocks.onAgentHostFrame,
  AGENT_HOST_EVENT: "agent-host-event",
}));

vi.mock("@/api/connections", () => ({
  listAiConnections: mocks.listAiConnections,
  getAiUsageBindings: mocks.getAiUsageBindings,
  setAiUsageBinding: mocks.setAiUsageBinding,
  getSuperSearchServiceResolution: vi.fn(),
  testAiConnection: vi.fn(),
  discoverAiModels: vi.fn(),
  saveAiConnection: vi.fn(),
  deleteAiConnection: vi.fn(),
}));

const conversationRef = "conversation_aaaaaaaaaaaaaaaa";

function summary(ref = conversationRef) {
  return {
    conversationRef: ref,
    title: "最近的对话",
    status: "active" as const,
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 2,
  };
}

function view(ref = conversationRef) {
  return {
    conversationRef: ref,
    title: "最近的对话",
    status: "active" as const,
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 2,
    drafts: [],
    attachments: [],
    links: [],
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  window.localStorage.clear();
  useAgentSessionStore.getState().reset();
  useAssistantFocusStore.getState().clear();
  useConversationStore.setState({
    conversations: [],
    activeRef: null,
    active: null,
    draftsByConversation: {},
    loading: false,
    error: null,
  });

  mocks.listConversations.mockResolvedValue([summary()]);
  mocks.getConversation.mockResolvedValue(view());
  mocks.createConversation.mockResolvedValue(view());
  mocks.saveConversationDraft.mockResolvedValue({
    kind: "assistant",
    payload: { text: "" },
    revision: 1,
    updatedAtUnixSeconds: 3,
  });
  mocks.listPendingPlans.mockResolvedValue([]);
  mocks.getConversationWork.mockResolvedValue({ plans: [], operations: [] });
  mocks.startAgentHost.mockResolvedValue({
    hostInstanceId: "host_instance_0000000000000001",
    protocolVersion: 1,
    sdkVersion: "0.0.0",
    recoveredStrandedHost: false,
    modelNetworkEnabled: false,
  });
  mocks.openAgentConversation.mockResolvedValue({
    hostInstanceId: "host_instance_0000000000000001",
    conversationRef,
    sessionId: "session_aaaaaaaaaaaaaaaa",
  });
  mocks.closeAgentConversation.mockResolvedValue(undefined);
  mocks.sendAgentMessage.mockResolvedValue({ runId: "run_aaaaaaaaaaaaaaaa", state: "accepted" });
  mocks.cancelAgentRun.mockResolvedValue(undefined);
    mocks.onAgentHostFrame.mockResolvedValue(() => undefined);
  mocks.listAiConnections.mockResolvedValue([]);
  mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: null, agent: null });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("A3-09 应用内悬浮助手", () => {
  it("默认只显示小 Logo；点开后继续最近的对话，不新建第三套聊天数据", async () => {
    render(<FloatingAssistant />);

    const logo = screen.getByTestId("floating-assistant-logo");
    expect(logo).toHaveAttribute("aria-label", "打开助手浮窗");
    expect(screen.queryByTestId("floating-assistant-panel")).toBeNull();

    await act(async () => {
      fireEvent.click(logo);
    });

    expect(screen.getByTestId("floating-assistant-panel")).toBeTruthy();
    // 复用同一份会话：读取的是 conversations API 里最近的活动会话，而不是新造一个。
    expect(mocks.listConversations).toHaveBeenCalled();
    expect(mocks.openAgentConversation).toHaveBeenCalledWith(conversationRef, "resume", "standard");
    expect(useConversationStore.getState().activeRef).toBe(conversationRef);
  });

  it("没有对话时提示新建，不伪造会话", async () => {
    mocks.listConversations.mockResolvedValue([]);
    render(<FloatingAssistant />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("floating-assistant-logo"));
    });

    expect(mocks.openAgentConversation).not.toHaveBeenCalled();
    // 新建入口已收进右上角图标按钮，文案跟着指向它。
    expect(screen.getByText(/还没有对话，点右上角新建/)).toBeTruthy();
  });

  it("Esc 收起浮层并把焦点还给 Logo", async () => {
    render(<FloatingAssistant />);
    const logo = screen.getByTestId("floating-assistant-logo");

    await act(async () => {
      fireEvent.click(logo);
    });
    expect(screen.getByTestId("floating-assistant-panel")).toBeTruthy();

    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });

    expect(screen.queryByTestId("floating-assistant-panel")).toBeNull();
    expect(document.activeElement).toBe(logo);
  });

  it("首次挂载不抢走页面焦点", async () => {
    // 浮层在每次离开助手页时都会重新挂载；挂载就 focus 会把焦点从搜索框等位置
    // 抢到悬浮 Logo 上（A3-09「处理键盘焦点」）。
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    expect(document.activeElement).toBe(input);

    render(<FloatingAssistant />);

    expect(document.activeElement).toBe(input);
    input.remove();
  });

  it("有模态弹窗打开时 Esc 只关弹窗，不连带收起浮层", async () => {
    render(<FloatingAssistant />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("floating-assistant-logo"));
    });
    expect(screen.getByTestId("floating-assistant-panel")).toBeTruthy();

    // Modal 是 aria-modal="true" 的遮罩层：Esc 属于它。
    const modal = document.createElement("div");
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    document.body.appendChild(modal);

    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });

    expect(screen.queryByTestId("floating-assistant-panel")).toBeTruthy();
    modal.remove();

    // 模态关掉后 Esc 又归浮层。
    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });
    expect(screen.queryByTestId("floating-assistant-panel")).toBeNull();
  });

  it("「待确认计划」只列待确认项，不把已批准的计划当待确认", async () => {
    const base = {
      schemaVersion: 1,
      planId: "plan_bbbbbbbbbbbbbbbb",
      capability: "assets.trash.submit",
      reviewKind: "single" as const,
      itemCount: 2,
      summary: "结案",
      reviewRef: "review_bbbbbbbbbbbbbbbb",
      expiresAtUnixSeconds: 9_999_999_999,
      scope: {},
      budget: { maxRequests: 1, maxTokens: 0, maxSeconds: 60, maxItems: 2 },
    };
    const pending = { ...base, planId: "plan_pppppppppppppppp", reviewStatus: "pending" as const };
    const approved = { ...base, planId: "plan_qqqqqqqqqqqqqqqq", reviewStatus: "approved" as const };
    mocks.getConversationWork.mockResolvedValue({ plans: [pending, approved], operations: [] });
    mocks.getConversation.mockResolvedValue({
      ...view(),
      links: [
        { kind: "plan", reference: pending.planId },
        { kind: "plan", reference: approved.planId },
      ],
    });

    render(<FloatingAssistant />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("floating-assistant-logo"));
    });

    const plans = await screen.findByLabelText("待确认计划");
    expect(within(plans).getAllByRole("button", { name: "查看并确认" })).toHaveLength(1);
    expect(within(plans).getAllByText(/2 项 · 结案/)).toHaveLength(1);
    expect(useAssistantFocusStore.getState().planId).toBeNull();
  });

  it("「查看并确认」把要定位的计划交给助手页，不只做一次裸导航", async () => {
    const plan = {
      schemaVersion: 1,
      planId: "plan_cccccccccccccccc",
      capability: "assets.trash.submit",
      reviewStatus: "pending" as const,
      reviewKind: "single" as const,
      itemCount: 3,
      summary: "把 3 项素材移入回收站",
      reviewRef: "review_cccccccccccccccc",
      expiresAtUnixSeconds: 9_999_999_999,
      scope: {},
      budget: { maxRequests: 1, maxTokens: 0, maxSeconds: 60, maxItems: 3 },
    };
    mocks.getConversationWork.mockResolvedValue({ plans: [plan], operations: [] });
    mocks.getConversation.mockResolvedValue({
      ...view(),
      links: [{ kind: "plan", reference: plan.planId }],
    });

    render(<FloatingAssistant />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("floating-assistant-logo"));
    });

    const plans = await screen.findByLabelText("待确认计划");
    // 摘要要能看出这是哪一类计划，而不是只有 N 项。
    expect(within(plans).getByText(/移入回收站 · 3 项/)).toBeTruthy();

    await act(async () => {
      fireEvent.click(within(plans).getByRole("button", { name: "查看并确认" }));
    });

    expect(useAssistantFocusStore.getState().planId).toBe(plan.planId);
  });

  it("计划在浮层只显示摘要与「查看并确认」，不提供批准入口", async () => {
    // 计划必须来自后端权威归属（`getConversationWork`：显式链接 ∪ 该会话自己产生的
    // 计划），而不是测试直接往 store 里塞：浮层打开时会 hydrate，用后端回读的结果
    // 覆盖归属。前端既不按时间差猜，也无法指定要认领哪些计划。
    const plan = {
      schemaVersion: 1,
      planId: "plan_aaaaaaaaaaaaaaaa",
      capability: "imports.submit",
      reviewStatus: "pending" as const,
      reviewKind: "single" as const,
      itemCount: 3,
      summary: "把 3 项素材移入回收站",
      reviewRef: "review_aaaaaaaaaaaaaaaa",
      expiresAtUnixSeconds: 9_999_999_999,
      scope: {},
      budget: { maxRequests: 1, maxTokens: 0, maxSeconds: 60, maxItems: 3 },
    };
    mocks.getConversationWork.mockResolvedValue({ plans: [plan], operations: [] });
    mocks.getConversation.mockResolvedValue({
      ...view(),
      links: [{ kind: "plan", reference: plan.planId }],
    });

    const navigate = vi.fn();
    window.addEventListener("app:navigate", navigate as EventListener);
    render(<FloatingAssistant />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("floating-assistant-logo"));
    });

    const plans = await screen.findByLabelText("待确认计划");
    expect(within(plans).getByText(/3 项 · 把 3 项素材移入回收站/)).toBeTruthy();
    expect(within(plans).getByRole("button", { name: "查看并确认" })).toBeTruthy();
    // 浮层不能直接批准复杂计划（A3-09 验收）。
    expect(within(plans).queryByRole("button", { name: /批准/ })).toBeNull();

    await act(async () => {
      fireEvent.click(within(plans).getByRole("button", { name: "查看并确认" }));
    });

    expect(navigate).toHaveBeenCalled();
    const detail = (navigate.mock.calls[0][0] as CustomEvent).detail;
    expect(detail).toBe("assistant");
    window.removeEventListener("app:navigate", navigate as EventListener);
  });

  it("拖动 Logo 松手后吸附到最近的水平边缘，左半区吸左、右半区吸右", async () => {
    // 默认窗口是 1024×768；先固定下来，避免受前序用例的缩窗影响。
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 768 });

    render(<FloatingAssistant />);
    const logo = screen.getByTestId("floating-assistant-logo");
    // 初始位置落在左下角（x = 16，y = 768-48-8，再被底栏硬约束抬到 704）。
    expect(logo.style.left).toBe("16px");
    expect(logo.style.top).toBe("704px");

    // 抓住 Logo 中心（偏移 24/24）拖到左半区松手：吸到左边 16px，垂直保持松手处的 352px。
    await act(async () => {
      fireEvent.pointerDown(logo, { button: 0, clientX: 40, clientY: 728, pointerId: 1 });
    });
    await act(async () => {
      fireEvent.pointerMove(logo, { clientX: 100, clientY: 376, pointerId: 1 });
    });
    await act(async () => {
      fireEvent.pointerUp(logo, { clientX: 100, clientY: 376, pointerId: 1 });
    });

    expect(logo.style.left).toBe("16px");
    expect(logo.style.top).toBe("352px");

    // 继续拖到右半区松手：吸附到右侧对称位置 1024-48-16=960，垂直变成 470。
    await act(async () => {
      fireEvent.pointerDown(logo, { button: 0, clientX: 40, clientY: 376, pointerId: 2 });
    });
    await act(async () => {
      fireEvent.pointerMove(logo, { clientX: 924, clientY: 494, pointerId: 2 });
    });
    await act(async () => {
      fireEvent.pointerUp(logo, { clientX: 924, clientY: 494, pointerId: 2 });
    });

    expect(logo.style.left).toBe("960px");
    expect(logo.style.top).toBe("470px");

    // 吸附后的位置要写进本地存储：下次打开还在同一条边。
    expect(
      JSON.parse(window.localStorage.getItem("chabaosucai.floating-assistant.v2") ?? "{}"),
    ).toMatchObject({ x: 960, bottomGap: 250 });
  });

  it("缩窗后重新夹紧位置，不把入口留在窗口外", async () => {
    render(<FloatingAssistant />);
    const logo = screen.getByTestId("floating-assistant-logo");
    expect(logo.style.left).not.toBe("");

    // 先把窗口改成很小再触发 resize：Logo 必须仍在可视范围内。
    await act(async () => {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 320 });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 240 });
      fireEvent(window, new Event("resize"));
    });

    const left = Number.parseInt(logo.style.left, 10);
    const top = Number.parseInt(logo.style.top, 10);
    expect(left).toBeGreaterThanOrEqual(16);
    expect(left).toBeLessThanOrEqual(320 - 48 - 16);
    expect(top).toBeGreaterThanOrEqual(16);
    expect(top).toBeLessThanOrEqual(240 - 48 - 16);
  });

  it("入口不落到底栏上：底部导航占位时贴底也停在导航条之上（M-11 底栏不重叠）", async () => {
    // 真机 960×640 曾经出现过入口压在「百宝箱」上的情形：底栏是固定层，
    // 只要把入口夹到 window.innerHeight 就会盖住导航。
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 960 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 640 });
    const bar = document.createElement("nav");
    bar.setAttribute("data-bottom-bar", "");
    bar.getBoundingClientRect = () =>
      ({ top: 584, bottom: 640, left: 0, right: 960, width: 960, height: 56, x: 0, y: 584, toJSON: () => ({}) }) as DOMRect;
    document.body.appendChild(bar);

    try {
      render(<FloatingAssistant />);
      const logo = screen.getByTestId("floating-assistant-logo");
      const height = Number.parseInt(logo.style.height, 10) || 48;
      const top = Number.parseInt(logo.style.top, 10);
      // 入口底边必须落在导航条上边缘（584）之上，不能在底栏的 56px 里。
      expect(top + height).toBeLessThanOrEqual(584);
      expect(top).toBeGreaterThanOrEqual(16);
    } finally {
      bar.remove();
    }
  });

  it("收起状态的小圆点夹在底栏左侧上面，不压住页面右下角的主操作（M-04 确认写入不重叠）", async () => {
    // 真机打标页 1200×800 复现：「确认写入」按钮（右侧操作区最右端）与停在右中部的入口
    // 圆心几乎重合，点确认就变成点入口。收起态只是入口，没有必须占宽的理由，
    // 因此固定停在底栏左上角，把右下角的主 CTA 整块让出来。
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    const bar = document.createElement("nav");
    bar.setAttribute("data-bottom-bar", "");
    bar.getBoundingClientRect = () =>
      ({ top: 736, bottom: 800, left: 0, right: 1200, width: 1200, height: 56, x: 0, y: 736, toJSON: () => ({}) }) as DOMRect;
    document.body.appendChild(bar);

    try {
      render(<FloatingAssistant />);
      const logo = screen.getByTestId("floating-assistant-logo");
      const height = Number.parseInt(logo.style.height, 10) || 48;
      const left = Number.parseInt(logo.style.left, 10);
      const top = Number.parseInt(logo.style.top, 10);
      // 左侧、导航条之上：右下角的确认/写入类主按钮不会被它盖住。
      expect(left).toBeLessThanOrEqual(1200 / 2);
      expect(top + height).toBeLessThanOrEqual(736);
    } finally {
      bar.remove();
    }
  });

  it("没被拖动过时缩放窗口仍贴回左下，不会停在旧视口的中间压住左栏内容（M-11 真机复现）", async () => {
    // 真机路径：960×640（CSS）下入口贴在底栏左上方 y≈645；随后把窗口放大到 1200×800，
    // 旧的默认 y 会原样保留下来，于是入口浮在页面中部，压住打标页左栏的批次行。
    const bar = document.createElement("nav");
    bar.setAttribute("data-bottom-bar", "");
    const setBar = (top: number, bottom: number) => {
      bar.getBoundingClientRect = () =>
        ({ top, bottom, left: 0, right: 4000, width: 4000, height: bottom - top, x: 0, y: top, toJSON: () => ({}) }) as DOMRect;
    };
    document.body.appendChild(bar);
    window.localStorage.removeItem("chabaosucai.floating-assistant.v2");

    try {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 960 });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 640 });
      setBar(584, 640);
      render(<FloatingAssistant />);
      const logo = screen.getByTestId("floating-assistant-logo");
      expect(logo.style.top).toBe("528px");

      // 放大窗口：底栏上边缘跟着下移到 736。
      await act(async () => {
        Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
        Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
        setBar(736, 800);
        fireEvent(window, new Event("resize"));
      });

      const height = Number.parseInt(logo.style.height, 10) || 48;
      const top = Number.parseInt(logo.style.top, 10);
      // 必须重新贴到底栏上沿（736-48-8=680），而不是留在 528 这种旧视口的中间值。
      expect(top).toBe(680);
      expect(top + height).toBeLessThanOrEqual(736);
    } finally {
      bar.remove();
      window.localStorage.removeItem("chabaosucai.floating-assistant.v2");
    }
  });

  it("用户拖动摆放过的位置在缩窗后保留，不被重新贴回左下（尊重用户意图）", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    const bar = document.createElement("nav");
    bar.setAttribute("data-bottom-bar", "");
    bar.getBoundingClientRect = () =>
      ({ top: 736, bottom: 800, left: 0, right: 1200, width: 1200, height: 56, x: 0, y: 736, toJSON: () => ({}) }) as DOMRect;
    document.body.appendChild(bar);
    window.localStorage.removeItem("chabaosucai.floating-assistant.v2");

    render(<FloatingAssistant />);
    const logo = screen.getByTestId("floating-assistant-logo");
    // 默认停在底栏上方 y = 736-48-8 = 680，抓其中心即偏移 24/24。
    expect(logo.style.top).toBe("680px");

    // 拖到右上并松手：写入本地记录，视为用户主动摆放。
    await act(async () => {
      fireEvent.pointerDown(logo, { button: 0, clientX: 40, clientY: 704, pointerId: 1 });
    });
    await act(async () => {
      fireEvent.pointerMove(logo, { clientX: 900, clientY: 200, pointerId: 1 });
    });
    await act(async () => {
      fireEvent.pointerUp(logo, { clientX: 900, clientY: 200, pointerId: 1 });
    });
    const placedTop = Number.parseInt(logo.style.top, 10);
    expect(placedTop).toBe(176);

    // 缩窗再放大：位置记在本地，夹紧到窗口内但不再贴回左下。
    await act(async () => {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 640 });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 480 });
      fireEvent(window, new Event("resize"));
    });
    await act(async () => {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
      fireEvent(window, new Event("resize"));
    });

    try {
      const top = Number.parseInt(logo.style.top, 10);
      // 用户摆过的位置必须留意：不能等于「贴回左下」的 680。
      expect(top).toBe(176);
    } finally {
      bar.remove();
      window.localStorage.removeItem("chabaosucai.floating-assistant.v2");
    }
  });
  it("旧版绝对坐标记录被丢弃，入口不会浮到页面中部压住左栏内容（M-11 真机第 4 次复现）", async () => {
    // 真机现象：入库页左栏底部「入库进度」标签被收起态入口盖住（UIA 量得重叠 7236 px²）。
    // 根因是 v1 存的是上一视口的绝对坐标（y=455），clamp 只保证不出窗口，于是入口停在页面中部。
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    const bar = document.createElement("nav");
    bar.setAttribute("data-bottom-bar", "");
    bar.getBoundingClientRect = () =>
      ({ top: 736, bottom: 800, left: 0, right: 1200, width: 1200, height: 56, x: 0, y: 736, toJSON: () => ({}) }) as DOMRect;
    document.body.appendChild(bar);
    window.localStorage.removeItem("chabaosucai.floating-assistant.v2");
    window.localStorage.setItem("chabaosucai.floating-assistant.v1", JSON.stringify({ x: 16, y: 455 }));

    try {
      render(<FloatingAssistant />);
      const logo = screen.getByTestId("floating-assistant-logo");
      // 贴回底栏上方，而不是沿用旧的 455。
      expect(logo.style.top).toBe("680px");
      // 旧键顺手清掉，避免下次启动再被读到。
      expect(window.localStorage.getItem("chabaosucai.floating-assistant.v1")).toBeNull();
    } finally {
      bar.remove();
      window.localStorage.removeItem("chabaosucai.floating-assistant.v1");
    }
  });
});
