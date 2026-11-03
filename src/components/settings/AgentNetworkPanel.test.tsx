/**
 * AgentNetworkPanel 测试（v2 修复计划 WP5-1；2026-11-01 去掉联网开关）。
 *
 * 验收口径：
 * - 面板不再提供「允许 Agent 联网」开关：绑定可用即就绪，权限分级只管文件操作；
 * - 首载回显后端返回的代理，而不是本地假设；
 * - 代理保存失败时错误可读、草稿保留，不能静默清空用户刚输入的内容；
 * - Agent 绑定没有「跟随默认」这种降级：未绑定时明确说 Agent 不可用；
 * - 平台不带 Agent 资源时给出提示，而不是让用户以为是开关问题。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import AgentNetworkPanel from "@/components/settings/AgentNetworkPanel";
import { usePlatformStore } from "@/stores/platformStore";
import type { AgentModelProxyView } from "@/api/agentNetwork";
import type { AiConnection } from "@/api/connections";

const mocks = vi.hoisted(() => ({
  getAgentModelProxy: vi.fn(),
  setAgentModelProxy: vi.fn(),
  listAiConnections: vi.fn(),
  getAiUsageBindings: vi.fn(),
  setAiUsageBinding: vi.fn(),
}));

vi.mock("@/api/agentNetwork", () => ({
  getAgentModelProxy: mocks.getAgentModelProxy,
  setAgentModelProxy: mocks.setAgentModelProxy,
}));

vi.mock("@/api/connections", () => ({
  listAiConnections: mocks.listAiConnections,
  getAiUsageBindings: mocks.getAiUsageBindings,
  setAiUsageBinding: mocks.setAiUsageBinding,
}));

const CLOUD: AiConnection = {
  id: "conn_cloud",
  name: "智谱 GLM",
  protocol: "openai_chat",
  deployment: "cloud",
  baseUrl: "https://open.bigmodel.cn/api/paas/v4",
  model: "glm-4-air",
  hasKey: true,
  enabled: true,
  credentialStatus: "configured",
} as AiConnection;

function proxyView(overrides: Partial<AgentModelProxyView> = {}): AgentModelProxyView {
  return { proxy: "", ...overrides };
}

beforeEach(() => {
  vi.clearAllMocks();
  usePlatformStore.setState({ status: "ready", capabilities: null, error: null });
  mocks.getAgentModelProxy.mockResolvedValue(proxyView());
  mocks.listAiConnections.mockResolvedValue([CLOUD]);
  mocks.getAiUsageBindings.mockResolvedValue({ super_search: null, tagging: null, agent: null });
});

describe("AgentNetworkPanel（WP5-1）", () => {
  it("不再渲染「允许 Agent 联网」开关", async () => {
    render(<AgentNetworkPanel notify={vi.fn()} fail={vi.fn()} />);

    await screen.findByLabelText("Agent 代理");
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByText(/允许 Agent 联网/)).not.toBeInTheDocument();
  });

  it("首载回显后端返回的代理，而不是本地假设", async () => {
    mocks.getAgentModelProxy.mockResolvedValue(proxyView({ proxy: "http://127.0.0.1:7890" }));

    render(<AgentNetworkPanel notify={vi.fn()} fail={vi.fn()} />);

    expect(await screen.findByLabelText("Agent 代理")).toHaveValue("http://127.0.0.1:7890");
  });

  it("保存代理走 setAgentModelProxy，并用返回值刷新界面", async () => {
    const notify = vi.fn();
    mocks.setAgentModelProxy.mockResolvedValue(proxyView({ proxy: "http://127.0.0.1:7890" }));

    render(<AgentNetworkPanel notify={notify} fail={vi.fn()} />);
    const input = await screen.findByLabelText("Agent 代理");
    fireEvent.change(input, { target: { value: "http://127.0.0.1:7890" } });
    fireEvent.click(screen.getByRole("button", { name: "保存代理" }));

    await waitFor(() => {
      expect(mocks.setAgentModelProxy).toHaveBeenCalledWith("http://127.0.0.1:7890");
    });
    expect(input).toHaveValue("http://127.0.0.1:7890");
    expect(notify).toHaveBeenCalled();
  });

  it("代理非法时展示后端错误并保留用户输入", async () => {
    const fail = vi.fn();
    mocks.setAgentModelProxy.mockRejectedValue(new Error("代理地址无效：只支持 http:// 或 https://主机:端口"));

    render(<AgentNetworkPanel notify={vi.fn()} fail={fail} />);
    const input = await screen.findByLabelText("Agent 代理");
    fireEvent.change(input, { target: { value: "socks5://127.0.0.1:1080" } });
    fireEvent.click(screen.getByRole("button", { name: "保存代理" }));

    await waitFor(() => {
      expect(fail).toHaveBeenCalledWith(expect.stringContaining("代理地址无效"));
    });
    // 草稿必须留在输入框里可继续修改。
    expect(input).toHaveValue("socks5://127.0.0.1:1080");
  });

  it("Agent 未绑定时说明不可用，且不提供「跟随默认」选项", async () => {
    render(<AgentNetworkPanel notify={vi.fn()} fail={vi.fn()} />);

    const select = await screen.findByLabelText("Agent 绑定的 AI 服务");
    expect(select).toHaveValue("");
    expect(screen.getByText(/还没有绑定服务，Agent 无法对话/)).toBeInTheDocument();
    expect(screen.queryByText(/跟随默认/)).not.toBeInTheDocument();
  });

  it("绑定服务走 agent 用途，成功后回显", async () => {
    mocks.setAiUsageBinding.mockResolvedValue(undefined);

    render(<AgentNetworkPanel notify={vi.fn()} fail={vi.fn()} />);
    const select = await screen.findByLabelText("Agent 绑定的 AI 服务");
    fireEvent.change(select, { target: { value: CLOUD.id } });

    await waitFor(() => {
      expect(mocks.setAiUsageBinding).toHaveBeenCalledWith("agent", CLOUD.id);
    });
    expect(select).toHaveValue(CLOUD.id);
  });

  it("平台不带 Agent 资源时给出提示", async () => {
    usePlatformStore.setState({
      status: "ready",
      capabilities: { agentHost: false } as never,
      error: null,
    });

    render(<AgentNetworkPanel notify={vi.fn()} fail={vi.fn()} />);

    expect(await screen.findByText(/不包含 Agent 运行资源/)).toBeInTheDocument();
    // 设置注定失败时控件锁住，避免用户点出一句 AGENT_HOST_UNAVAILABLE。
    expect(screen.getByLabelText("Agent 代理")).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Agent 绑定的 AI 服务" })).toBeDisabled();
  });
});
