/**
 * AiConnectionManager 测试（指导书 §6.3/§12.5）：
 *  - 列出连接档案（名称/部署/协议/密钥配置状态）；
 *  - 新增连接：写 keyring（saveAiConnection 带 apiKey）；错误校验（名称/地址必填）；
 *  - 编辑：留空 apiKey 不覆盖（null 传给后端）；
 *  - 删除：确认后调用 deleteAiConnection（含清理用途绑定）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import AiConnectionManager from "@/components/settings/AiConnectionManager";
import { deleteAiConnection, discoverAiModels, listAiConnections, saveAiConnection, testAiConnection } from "@/api/connections";
import { openAgnesApiKeyDocs } from "@/api/settings";
import { usePlatformStore } from "@/stores/platformStore";

vi.mock("@/api/connections", () => ({
  listAiConnections: vi.fn(),
  discoverAiModels: vi.fn(),
  deleteAiConnection: vi.fn(),
  saveAiConnection: vi.fn(),
  testAiConnection: vi.fn(),
}));
vi.mock("@/api/settings", () => ({ openAgnesApiKeyDocs: vi.fn().mockResolvedValue(undefined) }));

import type { AiConnection } from "@/api/connections";

// jsdom 无 crypto.randomUUID 默认实现
beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal("crypto", { randomUUID: () => "new-conn-1" });
  // R1（三端复核）：这些用例编码 Windows（托管）契约——按部署过滤本机/在线。
  // 平台 store 置 ready+windows；非托管平台「旧 local 档案仍可见」由 X-05 逻辑另测。
  usePlatformStore.setState({
    status: "ready",
    error: null,
    capabilities: {
      schemaVersion: 1,
      os: "windows",
      arch: "x86_64",
      managedOllama: true,
      preferredVideoProxy: "h264_mp4",
      nativeWindowControls: false,
      primaryModifier: "ctrl",
      libraryTransferVersion: null,
    },
  });
});

const fakeConns: AiConnection[] = [
  { id: "c1", name: "通义", deployment: "cloud", protocol: "openai_chat", baseUrl: "https://a/v1", model: "qwen-max", maxConcurrency: 0, requestsPerMinute: 0, requestsPerHour: 0, hasKey: true, credentialStatus: "configured", enabled: true },
  { id: "c2", name: "本地 Ollama", deployment: "local", protocol: "openai_chat", baseUrl: "http://localhost:11434/v1", model: "llama3.2-vision", maxConcurrency: 0, requestsPerMinute: 0, requestsPerHour: 0, hasKey: false, credentialStatus: "missing", enabled: true },
];

describe("AiConnectionManager（§6.3）", () => {
  it("按部署过滤并列出服务（含密钥状态）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    // local 服务被过滤（在线面板不显示本机服务）
    expect(screen.queryByText("本地 Ollama")).not.toBeInTheDocument();
    expect(screen.getByText(/密钥已配置/)).toBeInTheDocument();
  });

  it("标题与按钮使用用户文案「AI 服务 / + 新增服务」而非「连接档案」", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("AI 服务")).toBeInTheDocument());
    expect(screen.queryByText("连接档案")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "+ 新增服务" })).toBeInTheDocument();
  });

  it("新增服务调用 saveAiConnection 并带 apiKey（写系统凭据）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    vi.mocked(saveAiConnection).mockResolvedValue({ ...fakeConns[0], id: "new-conn-1" });
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "+ 新增服务" })).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "+ 新增服务" }));
    fireEvent.change(screen.getByPlaceholderText(/如「通义官方」/), { target: { value: "智谱" } });
    fireEvent.change(screen.getByPlaceholderText(/api.example.com/), { target: { value: "https://zhipu/v1" } });
    fireEvent.change(screen.getByRole("combobox", { name: "模型名称" }), { target: { value: "glm-4v" } });
    fireEvent.change(screen.getByPlaceholderText(/无需鉴权可留空/), { target: { value: "sk-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          id: "new-conn-1",
          name: "智谱",
          deployment: "cloud",
          baseUrl: "https://zhipu/v1",
          model: "glm-4v",
          apiKey: "sk-secret",
        }),
      ),
    );
  });

  it("在线服务可配置并保存并发、每分钟和每小时请求限制", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    vi.mocked(saveAiConnection).mockResolvedValue({ ...fakeConns[0], id: "new-conn-1" });
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "+ 新增服务" })).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "+ 新增服务" }));
    fireEvent.click(screen.getByText("高级设置"));
    fireEvent.change(screen.getByPlaceholderText(/如「通义官方」/), { target: { value: "限额服务" } });
    fireEvent.change(screen.getByPlaceholderText(/api.example.com/), { target: { value: "https://limits/v1" } });
    fireEvent.change(screen.getByLabelText("最大并发数"), { target: { value: "2" } });
    fireEvent.change(screen.getByLabelText("每分钟请求数"), { target: { value: "10" } });
    fireEvent.change(screen.getByLabelText("每小时请求数"), { target: { value: "100" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(
        expect.objectContaining({ maxConcurrency: 2, requestsPerMinute: 10, requestsPerHour: 100 }),
      ),
    );
  });

  it("编辑时显示已保存的请求限制并保留到保存结果", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([
      { ...fakeConns[0], maxConcurrency: 3, requestsPerMinute: 18, requestsPerHour: 600 },
    ]);
    vi.mocked(saveAiConnection).mockResolvedValue(fakeConns[0]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.click(screen.getByText("高级设置"));
    expect(screen.getByLabelText("最大并发数")).toHaveValue(3);
    expect(screen.getByLabelText("每分钟请求数")).toHaveValue(18);
    expect(screen.getByLabelText("每小时请求数")).toHaveValue(600);
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(
        expect.objectContaining({ id: "c1", maxConcurrency: 3, requestsPerMinute: 18, requestsPerHour: 600 }),
      ),
    );
  });

  it("编辑留空 API 密钥 → apiKey 传 null（不覆盖原密钥）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(saveAiConnection).mockResolvedValue(fakeConns[0]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(expect.objectContaining({ id: "c1", apiKey: null })),
    );
  });

  it("用作者推荐菜单应用 Agnes 预设，并将申请入口放在推荐旁边", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "+ 新增服务" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "+ 新增服务" }));
    fireEvent.click(screen.getByRole("button", { name: /作者推荐/ }));
    fireEvent.click(screen.getByRole("button", { name: /Agnes AI.*兼容 API/ }));

    expect(screen.getByPlaceholderText(/如「通义官方」/)).toHaveValue("Agnes AI（作者推荐）");
    expect(screen.getByPlaceholderText(/api.example.com/)).toHaveValue("https://apihub.agnes-ai.com/v1");
    expect(screen.getByRole("combobox", { name: "模型名称" })).toHaveValue("agnes-2.5-flash");
    fireEvent.click(screen.getByRole("button", { name: /获取 Agnes API Key/ }));
    await waitFor(() => expect(openAgnesApiKeyDocs).toHaveBeenCalledTimes(1));
    expect(screen.getByText("API 格式")).toBeInTheDocument();
    expect(screen.queryByText("API 格式（高级）")).not.toBeInTheDocument();
  });

  it("Agnes 预设会覆盖已有配置时先确认，取消后草稿保持不变", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.click(screen.getByRole("button", { name: /作者推荐/ }));
    fireEvent.click(screen.getByRole("button", { name: /Agnes AI.*兼容 API/ }));

    expect(confirmSpy).toHaveBeenCalledWith(expect.stringContaining("继续吗"));
    expect(screen.getByPlaceholderText(/如「通义官方」/)).toHaveValue("通义");
    expect(screen.getByPlaceholderText(/api.example.com/)).toHaveValue("https://a/v1");
    expect(screen.getByRole("combobox", { name: "模型名称" })).toHaveValue("qwen-max");
    expect(saveAiConnection).not.toHaveBeenCalled();
  });

  it("确认应用预设时只覆盖公开连接字段并保留密钥与高级限流", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.mocked(listAiConnections).mockResolvedValue([
      { ...fakeConns[0], maxConcurrency: 2, requestsPerMinute: 12, requestsPerHour: 300 },
    ]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.change(screen.getByPlaceholderText(/留空保留已保存密钥/), { target: { value: "sk-new-secret" } });
    fireEvent.click(screen.getByText("高级设置"));
    expect(screen.getByLabelText("每分钟请求数")).toHaveValue(12);
    fireEvent.click(screen.getByRole("button", { name: /作者推荐/ }));
    fireEvent.click(screen.getByRole("button", { name: /Agnes AI.*兼容 API/ }));

    expect(screen.getByPlaceholderText(/如「通义官方」/)).toHaveValue("Agnes AI（作者推荐）");
    expect(screen.getByPlaceholderText(/api.example.com/)).toHaveValue("https://apihub.agnes-ai.com/v1");
    expect(screen.getByRole("combobox", { name: "模型名称" })).toHaveValue("agnes-2.5-flash");
    expect(screen.getByPlaceholderText(/留空保留已保存密钥/)).toHaveValue("sk-new-secret");
    expect(screen.getByLabelText("每分钟请求数")).toHaveValue(12);
  });

  it("未填写 API 密钥时模型可手动输入，但不能读取远端模型列表", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "+ 新增服务" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "+ 新增服务" }));

    const modelInput = screen.getByRole("combobox", { name: "模型名称" });
    expect(modelInput).toBeEnabled();
    expect(screen.getByRole("button", { name: "读取模型列表" })).toBeDisabled();
    fireEvent.change(modelInput, { target: { value: "manual-model" } });
    expect(modelInput).toHaveValue("manual-model");
  });

  it("手动编辑模型名不会清空已读取的模型候选", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(discoverAiModels).mockResolvedValue(["qwen-max", "qwen-plus"]);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.click(screen.getByRole("button", { name: "读取模型列表" }));
    await screen.findByRole("option", { name: "qwen-plus" });

    fireEvent.change(screen.getByRole("combobox", { name: "模型名称" }), {
      target: { value: "qwen-plus" },
    });

    expect(screen.getByRole("option", { name: "qwen-max" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "刷新模型列表" })).toBeInTheDocument();
  });

  it("删除需确认并调用 deleteAiConnection", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(deleteAiConnection).mockResolvedValue(undefined);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(confirmSpy).toHaveBeenCalled();
    await waitFor(() => expect(deleteAiConnection).toHaveBeenCalledWith("c1"));
  });

  // FB3-08：测试连接走后端 test_ai_connection（keyring 密钥在 Rust 侧读取），
  // 成功/失败都显示后端结构化信息（脱敏 message + 延迟）
  it("点击测试连接调用 testAiConnection 并显示后端结果（成功含延迟）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(testAiConnection).mockResolvedValue({
      ok: true,
      statusCode: 200,
      latencyMs: 312,
      protocol: "openai_chat",
      model: "qwen-max",
      message: "连接成功（HTTP 200，共 12 个模型）",
    });
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));
    await waitFor(() => expect(testAiConnection).toHaveBeenCalledWith("c1"));
    await waitFor(() =>
      expect(screen.getByText(/连接成功：连接成功（HTTP 200，共 12 个模型），耗时 312ms/)).toBeInTheDocument(),
    );
  });

  it("修改模型后清除旧连接状态，并禁止检测未保存草稿", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(testAiConnection).mockResolvedValue({
      ok: true,
      statusCode: 200,
      latencyMs: 20,
      protocol: "openai_chat",
      model: "qwen-max",
      message: "服务可达，模型列表读取成功",
    });
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));
    await screen.findByText(/连接成功：服务可达/);
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.change(screen.getByRole("combobox", { name: "模型名称" }), { target: { value: "new-model" } });

    expect(screen.getByText("配置已变更，尚未检测")).toBeInTheDocument();
    expect(screen.getByText(/尚未在当前连接配置下验证/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存后检测" })).toBeDisabled();
  });

  it("连接草稿变更后忽略先前检测的迟到响应", async () => {
    let resolveTest!: (result: Awaited<ReturnType<typeof testAiConnection>>) => void;
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(testAiConnection).mockReturnValueOnce(new Promise((resolve) => { resolveTest = resolve; }));
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));
    await waitFor(() => expect(testAiConnection).toHaveBeenCalledWith("c1"));
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    fireEvent.change(screen.getByPlaceholderText(/api.example.com/), { target: { value: "https://new.example/v1" } });
    resolveTest({
      ok: true,
      statusCode: 200,
      latencyMs: 20,
      protocol: "openai_chat",
      model: "qwen-max",
      message: "旧地址检测成功",
    });

    await waitFor(() => expect(screen.getByText("配置已变更，尚未检测")).toBeInTheDocument());
    expect(screen.queryByText(/旧地址检测成功/)).not.toBeInTheDocument();
  });

  it("测试失败显示失败信息（红色路径）", async () => {
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    vi.mocked(testAiConnection).mockResolvedValue({
      ok: false,
      statusCode: 401,
      latencyMs: 150,
      protocol: "openai_chat",
      model: "qwen-max",
      message: "服务可达，但密钥无效或没有权限（401/403）。请检查 API 密钥是否正确、是否过期。",
    });
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));
    await waitFor(() =>
      expect(screen.getByText(/连接失败：服务可达，但密钥无效/)).toBeInTheDocument(),
    );
  });

  // X-05：非托管平台（macOS）没有本机服务 tab，从 Windows 迁移来的旧 local 档案
  // 必须仍在在线服务列表可见/可编辑，不被部署过滤丢失。
  it("非托管平台：旧 deployment=local 档案在在线列表仍可见", async () => {
    usePlatformStore.setState({
      status: "ready",
      error: null,
      capabilities: {
        schemaVersion: 1,
        os: "macos",
        arch: "aarch64",
        managedOllama: false,
        preferredVideoProxy: "h264_mp4",
        nativeWindowControls: true,
        primaryModifier: "meta",
        libraryTransferVersion: null,
      },
    });
    vi.mocked(listAiConnections).mockResolvedValue(fakeConns);
    render(<AiConnectionManager deployment="cloud" notify={vi.fn()} fail={vi.fn()} />);
    // 云档案与旧 local 档案都应出现（不因 deployment=cloud 过滤丢失 local）
    await waitFor(() => expect(screen.getByText("通义")).toBeInTheDocument());
    expect(screen.getByText("本地 Ollama")).toBeInTheDocument();
  });
});
