/**
 * McpBridgePanel 测试（A5-01）。
 *
 * 验收口径（docs/contracts/mcp-bridge-v1.md §1/§4.1/§7/§8）：
 * - 首载必须如实显示运行状态与会话数，未启用时说明不会创建端点文件；
 * - 平台不支持时开关禁用并说明原因，不让用户以为是自己没勾；
 * - 新建连接默认只勾查询类，并在 notice 里回显 connectionId（用户要填进客户端配置）；
 * - 勾选 assets.trash.submit 必须提示「外部客户端只能提交您已在本界面批准的计划」；
 * - 撤销必须二次确认，确认后才调后端并用返回值刷新列表；
 * - 读取失败要给出可读错误，不能静默空列表。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import McpBridgePanel from "@/components/settings/McpBridgePanel";
import { accessLabel, connectionStatusLabel } from "@/api/mcp";
import type { McpBridgeStatusView, McpCapabilityView, McpPairingView } from "@/api/mcp";

const mocks = vi.hoisted(() => ({
  mcpBridgeStatus: vi.fn(),
  setMcpBridgeEnabled: vi.fn(),
  createMcpPairing: vi.fn(),
  updateMcpPairing: vi.fn(),
  revokeMcpPairing: vi.fn(),
}));

vi.mock("@/api/mcp", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mcpBridgeStatus: mocks.mcpBridgeStatus,
  setMcpBridgeEnabled: mocks.setMcpBridgeEnabled,
  createMcpPairing: mocks.createMcpPairing,
  updateMcpPairing: mocks.updateMcpPairing,
  revokeMcpPairing: mocks.revokeMcpPairing,
}));

const PUBLISHABLE: McpCapabilityView[] = [
  {
    capability: "assets.search",
    toolName: "tea_assets_search",
    access: "query",
    description: "按关键词、标签筛选搜索素材",
    publishable: true,
  },
  {
    capability: "assets.trash.prepare",
    toolName: "tea_assets_trash_prepare",
    access: "prepare",
    description: "准备一个移入回收站的计划，不执行删除",
    publishable: true,
  },
  {
    capability: "assets.trash.submit",
    toolName: "tea_assets_trash_submit",
    access: "submit",
    description: "提交一份已批准的计划",
    publishable: true,
  },
  {
    capability: "operations.get",
    toolName: "tea_operations_get",
    access: "query",
    description: "查询任务状态",
    publishable: true,
  },
  {
    capability: "operations.cancel",
    toolName: "tea_operations_cancel",
    access: "cancel",
    description: "取消一个任务",
    publishable: true,
  },
];

const CONNECTION_ID = "mcpconn_0123456789abcdef";

function mkConnection(overrides: Partial<McpPairingView> = {}): McpPairingView {
  return {
    connectionId: CONNECTION_ID,
    label: "编辑器",
    principalId: "principal_aaaaaaaaaaaaaaaa",
    capabilities: ["assets.search", "operations.get"],
    createdAt: 1_800_000_000,
    updatedAt: 1_800_000_000,
    revokedAt: null,
    lastUsedAt: null,
    revoked: false,
    ...overrides,
  };
}

function mkStatus(overrides: Partial<McpBridgeStatusView> = {}): McpBridgeStatusView {
  return {
    enabled: true,
    running: true,
    platformSupported: true,
    instance: "inst_0123456789abcdef",
    pipeName: "teacell-mcp-v1-inst_0123456789abcdef",
    endpointPath: "C:\\Users\\me\\AppData\\Roaming\\TeaCell\\mcp\\endpoint.json",
    sessionCount: 3,
    maxSessions: 8,
    connections: [mkConnection()],
    publishable: PUBLISHABLE,
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.mcpBridgeStatus.mockResolvedValue(mkStatus());
});

describe("McpBridgePanel 状态与开关", () => {
  it("首载渲染运行状态与会话数，并给出端点文件路径", async () => {
    render(<McpBridgePanel />);

    const status = await screen.findByTestId("mcp-bridge-status");
    expect(status).toHaveTextContent("运行中（会话 3/8）");
    expect(status).toHaveTextContent("C:\\Users\\me\\AppData\\Roaming\\TeaCell\\mcp\\endpoint.json");
    expect(mocks.mcpBridgeStatus).toHaveBeenCalledTimes(1);
  });

  it("未启用时说明不会创建端点文件，也不监听管道", async () => {
    mocks.mcpBridgeStatus.mockResolvedValue(
      mkStatus({ enabled: false, running: false, instance: null, pipeName: null, endpointPath: null, sessionCount: 0, connections: [] }),
    );
    render(<McpBridgePanel />);

    const status = await screen.findByTestId("mcp-bridge-status");
    expect(status).toHaveTextContent("未启用：不会创建端点文件，也不会监听管道");
    expect(screen.getByRole("switch", { name: "启用本机 MCP 接入" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });

  it("平台不支持时开关禁用并说明只在 Windows x64 提供", async () => {
    mocks.mcpBridgeStatus.mockResolvedValue(
      mkStatus({ platformSupported: false, enabled: false, running: false }),
    );
    render(<McpBridgePanel />);

    const toggle = await screen.findByRole("switch", { name: "启用本机 MCP 接入" });
    expect(toggle).toBeDisabled();
    expect(screen.getByText("本机 MCP 接入目前只在 Windows x64 提供。")).toBeInTheDocument();

    fireEvent.click(toggle);
    expect(mocks.setMcpBridgeEnabled).not.toHaveBeenCalled();
  });

  it("开关调用 setMcpBridgeEnabled(true) 并用返回值刷新界面", async () => {
    mocks.mcpBridgeStatus.mockResolvedValue(
      mkStatus({ enabled: false, running: false, instance: null, pipeName: null, endpointPath: null, sessionCount: 0 }),
    );
    mocks.setMcpBridgeEnabled.mockResolvedValue(mkStatus({ sessionCount: 0 }));

    render(<McpBridgePanel />);
    const toggle = await screen.findByRole("switch", { name: "启用本机 MCP 接入" });
    expect(toggle).toHaveAttribute("aria-checked", "false");

    fireEvent.click(toggle);

    await waitFor(() => expect(mocks.setMcpBridgeEnabled).toHaveBeenCalledWith(true));
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: "启用本机 MCP 接入" })).toHaveAttribute(
        "aria-checked",
        "true",
      ),
    );
    expect(screen.getByRole("status")).toHaveTextContent("已启用本机 MCP 接入");
    expect(screen.getByTestId("mcp-bridge-status")).toHaveTextContent("运行中（会话 0/8）");
  });
});

describe("McpBridgePanel 连接管理", () => {
  it("创建连接默认只勾查询类，并在 notice 里回显 connectionId", async () => {
    mocks.mcpBridgeStatus.mockResolvedValue(mkStatus({ connections: [] }));
    mocks.createMcpPairing.mockResolvedValue(mkConnection({ label: "我的编辑器" }));

    render(<McpBridgePanel />);
    const input = await screen.findByLabelText("连接名称");
    const createButton = screen.getByRole("button", { name: "创建连接" });
    expect(createButton).toBeDisabled();

    fireEvent.change(input, { target: { value: "我的编辑器" } });
    expect(createButton).toBeEnabled();
    fireEvent.click(createButton);

    await waitFor(() =>
      expect(mocks.createMcpPairing).toHaveBeenCalledWith("我的编辑器", [
        "assets.search",
        "operations.get",
      ]),
    );
    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent(CONNECTION_ID);
    // 新建的连接用返回值进列表，不重新拉整份状态
    expect(await screen.findByText("我的编辑器")).toBeInTheDocument();
    expect(mocks.mcpBridgeStatus).toHaveBeenCalledTimes(1);
  });

  it("勾选 assets.trash.submit 才出现批准权警示，保存传回的 capabilities 含该 id", async () => {
    mocks.updateMcpPairing.mockResolvedValue(
      mkConnection({ capabilities: ["assets.search", "assets.trash.submit", "operations.get"] }),
    );

    render(<McpBridgePanel />);
    const trashSubmit = await screen.findByLabelText("允许 assets.trash.submit");
    expect(screen.queryByText("外部客户端只能提交您已在本界面批准的计划")).toBeNull();

    const saveButton = screen.getByRole("button", { name: "保存权限" });
    expect(saveButton).toBeDisabled();

    fireEvent.click(trashSubmit);

    expect(
      screen.getByText("外部客户端只能提交您已在本界面批准的计划"),
    ).toBeInTheDocument();
    expect(saveButton).toBeEnabled();

    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(mocks.updateMcpPairing).toHaveBeenCalledWith(CONNECTION_ID, "编辑器", [
        "assets.search",
        "assets.trash.submit",
        "operations.get",
      ]),
    );
    expect(await screen.findByRole("status")).toHaveTextContent("已保存「编辑器」的权限范围");
  });

  it("撤销必须二次确认：点撤销不调后端，确认后才调用并用返回值刷新列表", async () => {
    mocks.mcpBridgeStatus
      .mockResolvedValueOnce(mkStatus())
      .mockResolvedValue(mkStatus());
    mocks.revokeMcpPairing.mockResolvedValue(
      mkConnection({ revoked: true, revokedAt: 1_800_000_500, label: "编辑器" }),
    );

    render(<McpBridgePanel />);
    fireEvent.click(await screen.findByRole("button", { name: "撤销" }));

    const dialog = await screen.findByRole("dialog", { name: "撤销本机接入连接" });
    expect(dialog).toHaveTextContent("立即生效");
    expect(dialog).toHaveTextContent("不会因此回滚或重放");
    expect(mocks.revokeMcpPairing).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole("button", { name: "确认撤销" }));

    await waitFor(() => expect(mocks.revokeMcpPairing).toHaveBeenCalledWith(CONNECTION_ID));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "撤销本机接入连接" })).not.toBeInTheDocument(),
    );
    // 撤销后的连接仍然列出（灰化、无操作按钮），让人看出历史
    expect(await screen.findByText("已撤销")).toBeInTheDocument();
    expect(screen.getByText(CONNECTION_ID)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "保存权限" })).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("已撤销连接「编辑器」");
  });

  it("读取失败显示 role=alert 且含可读前缀", async () => {
    mocks.mcpBridgeStatus.mockRejectedValueOnce(new Error("数据库被占用"));

    render(<McpBridgePanel />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("读取本机接入状态失败：数据库被占用");
    expect(screen.getByRole("button", { name: "刷新" })).toBeEnabled();
  });
});

describe("mcp 纯函数", () => {
  it("accessLabel 把四类访问翻成中文，未知值原样回显", () => {
    expect(accessLabel("query")).toBe("查询");
    expect(accessLabel("prepare")).toBe("准备计划");
    expect(accessLabel("submit")).toBe("提交已批准计划");
    expect(accessLabel("cancel")).toBe("取消任务");
    expect(accessLabel("admin")).toBe("admin");
  });

  it("connectionStatusLabel 撤销优先于最近使用，否则回退到尚未使用", () => {
    expect(connectionStatusLabel(mkConnection())).toBe("尚未使用");
    expect(
      connectionStatusLabel(mkConnection({ lastUsedAt: 1_800_000_500 })),
    ).toBe(`最近使用：${new Date(1_800_000_500 * 1000).toLocaleString()}`);
    expect(
      connectionStatusLabel(mkConnection({ revoked: true, revokedAt: 1_800_000_600, lastUsedAt: 1_800_000_500 })),
    ).toBe("已撤销");
  });
});
