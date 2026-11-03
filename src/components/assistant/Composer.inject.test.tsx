/**
 * 助手输入区接收「再次使用常用流程」注入的文本（A4-07），
 * 以及输入框旁的服务选择器只列出可用服务并显示当前选择（R33）。
 *
 * 注入链路：注入内容要落进当前会话的输入框、只取一次、可以被用户继续改写；
 * 发送与草稿行为由 Composer 既有逻辑保证，这里不重复断言。
 * 服务选择器：停用 / 云端缺密钥的服务必须在选项上如实标注，当前绑定要显示在触发器上。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import Composer from "@/components/assistant/Composer";
import {
  listAiConnections,
  getAiUsageBindings,
  setAiUsageBinding,
  type AiConnection,
} from "@/api/connections";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useConversationStore } from "@/stores/conversationStore";

vi.mock("@/api/connections", () => ({
  getAiUsageBindings: vi.fn().mockResolvedValue({}),
  listAiConnections: vi.fn().mockResolvedValue([]),
  setAiUsageBinding: vi.fn(),
}));

beforeEach(() => {
  useComposerInjectStore.setState({ pending: {} });
  useConversationStore.setState({ draftsByConversation: {} });
  // 每个用例自己决定有哪些连接；默认回到「没有连接」，避免上个用例的列表串进来。
  vi.mocked(listAiConnections).mockResolvedValue([]);
  vi.mocked(getAiUsageBindings).mockResolvedValue({ super_search: null, tagging: null, agent: null });
});

function connection(over: Partial<AiConnection> = {}): AiConnection {
  return {
    id: "conn_cloud",
    name: "茶馆云",
    deployment: "cloud",
    protocol: "openai_chat",
    baseUrl: "https://api.example.com/v1",
    model: "tea-large",
    maxConcurrency: 2,
    requestsPerMinute: 30,
    requestsPerHour: 600,
    hasKey: true,
    credentialStatus: "configured",
    enabled: true,
    ...over,
  };
}

function renderComposer(conversationRef: string) {
  return render(
    <Composer
      conversationRef={conversationRef}
      attachments={[]}
      streaming={false}
      stopping={false}
      disabled={false}
      onSend={vi.fn()}
      onStop={vi.fn()}
    />,
  );
}

describe("Composer 接收流程复用注入", () => {
  it("把注入的输入清单放进输入框并只取一次", async () => {
    useComposerInjectStore.getState().inject("conv_a", "请按常用流程「旅行」准备一次新的计划。");
    renderComposer("conv_a");

    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    await waitFor(() => expect(textarea.value).toContain("请按常用流程「旅行」"));
    // 取走即清空：不会因为重渲染反复注入
    expect(useComposerInjectStore.getState().pending).toEqual({});
  });

  it("注入内容追加在已有草稿之后，不覆盖用户已经写的内容", async () => {
    useConversationStore.setState({
      draftsByConversation: { conv_a: { assistant: { text: "先写一句" } } },
    });
    useComposerInjectStore.getState().inject("conv_a", "再补一段");
    renderComposer("conv_a");

    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    await waitFor(() => expect(textarea.value).toContain("再补一段"));
    expect(textarea.value).toContain("先写一句");
    expect(textarea.value.indexOf("先写一句")).toBeLessThan(textarea.value.indexOf("再补一段"));
  });

  it("别的会话的注入不会串到当前输入框", async () => {
    useComposerInjectStore.getState().inject("conv_b", "这是给另一个会话的");
    renderComposer("conv_a");

    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    await waitFor(() => expect(textarea).toBeTruthy());
    expect(textarea.value).toBe("");
    // 给 conv_b 的内容仍然留着，等那个会话打开时再取
    expect(useComposerInjectStore.getState().pending["conv_b"]).toBe("这是给另一个会话的");
  });
});

describe("Composer 的服务选择器（R33）", () => {
  it("停用的服务在选项里标「已停用」且不可选，当前选择显示在触发器上", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      connection(),
      connection({ id: "conn_off", name: "备用云", model: "tea-small", enabled: false }),
    ]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    renderComposer("conv_a");

    const trigger = await screen.findByLabelText("Agent 服务");
    await waitFor(() => expect(trigger).toHaveTextContent("tea-large"));
    expect(trigger.getAttribute("title")).toContain("在线 · 茶馆云");
    // 展开菜单：可用服务正常列出，停用的服务带标注并且切不过去。
    fireEvent.click(trigger);
    const usable = await screen.findByRole("menuitem", { name: /tea-large/ });
    const disabled = screen.getByRole("menuitem", { name: /tea-small/ });
    expect(usable).not.toBeDisabled();
    expect(usable).toHaveTextContent("在线");
    expect(disabled).toBeDisabled();
    expect(disabled).toHaveTextContent("已停用");
    const label = screen.getByTestId("agent-model-label");
    expect(label.textContent).toBe("模型：tea-large");
  });

  it("云端服务缺密钥时选项标「密钥不可用」，并在输入框旁如实说明发送会失败", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      connection({
        id: "conn_nokey",
        name: "云端未配密钥",
        credentialStatus: "missing",
        hasKey: false,
      }),
    ]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_nokey",
    });
    renderComposer("conv_a");

    const trigger = await screen.findByLabelText("Agent 服务");
    fireEvent.click(trigger);
    const current = await screen.findByRole("menuitem", { name: /tea-large/ });
    expect(current).toHaveTextContent("密钥不可用");

    // 不能静默失败：同一屏说明这条服务发送会失败，别整段解释只留一句可执行的去处。
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("「云端未配密钥」的密钥不可用，发送会失败");
    expect(alert.textContent).toContain("设置");
    // 当前选择仍然显示在触发器上，用户看得出正在用哪个服务。
    expect(trigger).toHaveTextContent("tea-large");
  });

  it("本机服务缺密钥不当作错误（只有云端才读密钥）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      connection({
        id: "conn_local",
        name: "本机 Ollama",
        deployment: "local",
        baseUrl: "http://127.0.0.1:11434/v1",
        credentialStatus: "missing",
        hasKey: false,
      }),
    ]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_local",
    });
    renderComposer("conv_a");

    const trigger = await screen.findByLabelText("Agent 服务");
    fireEvent.click(trigger);
    const current = await screen.findByRole("menuitem", { name: /tea-large/ });
    expect(current).toHaveTextContent("本机");
    expect(current).not.toHaveTextContent("密钥不可用");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("切换服务只写绑定并更新当前选择，不碰输入框里的内容", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      connection(),
      connection({ id: "conn_two", name: "二号云", model: "tea-max", baseUrl: "https://api2.example.com/v1" }),
    ]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    renderComposer("conv_a");

    const trigger = await screen.findByLabelText("Agent 服务");
    await waitFor(() => expect(trigger).toHaveTextContent("tea-large"));

    fireEvent.click(trigger);
    fireEvent.click(await screen.findByRole("menuitem", { name: /tea-max/ }));

    await waitFor(() => expect(setAiUsageBinding).toHaveBeenCalledWith("agent", "conn_two"));
    // 切换后触发器与模型标签一起更新，用户看得出当前用的是哪个服务。
    await waitFor(() => expect(trigger).toHaveTextContent("tea-max"));
    expect(screen.getByTestId("agent-model-label").textContent).toBe("模型：tea-max");
    expect(trigger.getAttribute("title")).toContain("在线 · 二号云");
    // 只切换服务，不改写用户已经写的东西。
    expect((screen.getByLabelText("输入消息") as HTMLTextAreaElement).value).toBe("");
  });
});

describe("Composer 失败后由用户决定重试或切换（R34）", () => {
  // R35：附件只是引用，不进外发；有附件也不会自动发送。
  it("有关联附件时不自动外发，并如实说明只发文字、不发图片内容", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([connection()]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(
      <Composer
        conversationRef="conv_a"
        attachments={[
          {
            attachmentRef: "att_1",
            assetId: 101,
            displayName: "a.jpg",
            available: true,
          },
        ]}
        streaming={false}
        stopping={false}
        disabled={false}
        onSend={onSend}
        onStop={vi.fn()}
      />,
    );

    await screen.findByLabelText("Agent 服务");
    // 挂载 + 有附件都不等于外发：一条消息都没发出去。
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByRole("list", { name: "已关联素材" })).toHaveTextContent("a.jpg");
    // 摘要如实说明只发文字，不发图片内容本身。
    const summary = screen.getByTestId("agent-outbound-summary");
    expect(summary).toHaveTextContent("不发送图片内容本身");
    expect(summary).toHaveTextContent("本次对话关联 1 项素材");
  });
  it("发送失败时保留输入内容，并给出「重试发送」与「切换服务」两个明确动作", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([connection()]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    const onSend = vi.fn().mockRejectedValue(new Error("连接超时"));
    render(
      <Composer
        conversationRef="conv_a"
        attachments={[]}
        streaming={false}
        stopping={false}
        disabled={false}
        onSend={onSend}
        onStop={vi.fn()}
      />,
    );
    await screen.findByLabelText("Agent 服务");

    fireEvent.change(screen.getByLabelText("输入消息"), { target: { value: "把刚导入的照片移入回收站" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("连接超时"));
    // 失败不丢用户写的内容：这正是「保留工作」的具体含义。
    expect((screen.getByLabelText("输入消息") as HTMLTextAreaElement).value).toBe("把刚导入的照片移入回收站");
    // 两个动作都是用户可点的显式入口，应用不替他换服务、也不自动重发。
    expect(screen.getByRole("button", { name: "重试发送" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "切换服务" })).toBeEnabled();
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("「重试发送」用同一段内容再发一次，成功后清空输入框", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([connection()]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    const onSend = vi.fn().mockRejectedValueOnce(new Error("连接超时")).mockResolvedValueOnce(undefined);
    render(
      <Composer
        conversationRef="conv_a"
        attachments={[]}
        streaming={false}
        stopping={false}
        disabled={false}
        onSend={onSend}
        onStop={vi.fn()}
      />,
    );
    await screen.findByLabelText("Agent 服务");

    fireEvent.change(screen.getByLabelText("输入消息"), { target: { value: "把刚导入的照片移入回收站" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("连接超时"));

    fireEvent.click(screen.getByRole("button", { name: "重试发送" }));

    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(2));
    expect(onSend).toHaveBeenLastCalledWith("把刚导入的照片移入回收站");
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect((screen.getByLabelText("输入消息") as HTMLTextAreaElement).value).toBe("");
  });

  it("「切换服务」只把焦点交给服务选择器，自己不改绑定", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      connection(),
      connection({ id: "conn_two", name: "二号云", model: "tea-max", baseUrl: "https://api2.example.com/v1" }),
    ]);
    vi.mocked(getAiUsageBindings).mockResolvedValue({
      super_search: null,
      tagging: null,
      agent: "conn_cloud",
    });
    const onSend = vi.fn().mockRejectedValue(new Error("连接超时"));
    render(
      <Composer
        conversationRef="conv_a"
        attachments={[]}
        streaming={false}
        stopping={false}
        disabled={false}
        onSend={onSend}
        onStop={vi.fn()}
      />,
    );
    const trigger = await screen.findByLabelText("Agent 服务");
    vi.mocked(setAiUsageBinding).mockClear();

    fireEvent.change(screen.getByLabelText("输入消息"), { target: { value: "把刚导入的照片移入回收站" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("连接超时"));

    fireEvent.click(screen.getByRole("button", { name: "切换服务" }));

    await waitFor(() => expect(document.activeElement).toBe(trigger));
    // 只是把用户带到选择器，绑定必须等他真的选。
    expect(setAiUsageBinding).not.toHaveBeenCalled();
    expect(trigger).toHaveTextContent("tea-large");
  });
});
