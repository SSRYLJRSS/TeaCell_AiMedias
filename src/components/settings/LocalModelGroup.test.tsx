/** W7-2：LocalModelGroup 基础渲染（A3 核心 UI，858 行补最小覆盖）：
 *  未检测到引擎时展示一键安装入口。 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import LocalModelGroup from "@/components/settings/LocalModelGroup";
import type { Settings } from "@/types/settings";
import { ollamaInstallStatus, ollamaStartService } from "@/api/ollama";
import { usePlatformStore } from "@/stores/platformStore";

vi.mock("@/api/ollama", () => ({
  ollamaInstallStatus: vi.fn().mockResolvedValue({ installed: false, running: false, version: null, ownership: "none" }),
  probeOllamaHardware: vi.fn().mockResolvedValue({ gpu: { name: null, vramGb: null, source: "unknown" }, recommendations: [] }),
  ollamaRuntimeStatus: vi.fn().mockResolvedValue({ ownership: null, lastActivityAt: 0 }),
  ollamaStartService: vi.fn().mockResolvedValue({ ownership: { kind: "appOwned", detail: { pid: 1, startedAt: 1 } }, lastActivityAt: 1 }),
  ollamaStopService: vi.fn().mockResolvedValue({ before: null, stopped: false, after: { ownership: null, lastActivityAt: 0 } }),
  ollamaListLocalModels: vi.fn().mockResolvedValue([]),
  ollamaDeleteModel: vi.fn(),
  ollamaListSources: vi.fn().mockResolvedValue([]),
  ollamaInstallProbe: vi.fn().mockResolvedValue({ ok: false }),
  installOllama: vi.fn(),
  cancelOllamaInstall: vi.fn(),
  onOllamaInstallProgress: vi.fn().mockResolvedValue(() => {}),
  onOllamaInstallLog: vi.fn().mockResolvedValue(() => {}),
  pullOllamaModel: vi.fn(),
  onOllamaPullProgress: vi.fn().mockResolvedValue(() => {}),
  ollamaModelList: vi.fn().mockResolvedValue([]),
  deleteOllamaModel: vi.fn(),
  ollamaModelDir: vi.fn().mockResolvedValue(""),
  ollamaSetSource: vi.fn(),
  ollamaTestSource: vi.fn(),
  ollamaRemoveInstaller: vi.fn(),
  ollamaInstallerInfo: vi.fn().mockResolvedValue(null),
  ollamaStop: vi.fn(),
  onOllamaRuntimeEvent: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock("@/hooks/useOllama", () => ({
  useOllamaPull: () => ({ pullState: null, pullBusy: false, pull: vi.fn() }),
}));

const draft = {
  ai: {
    baseUrl: "",
    apiKey: "",
    model: "",
    profileMode: "cloud",
    localBaseUrl: "http://localhost:11434",
    localModel: "qwen3.5:4b",
    ollamaSourceId: "auto",
    ollamaCustomSources: [],
  },
} as unknown as Settings;

const noop = () => {};

describe("LocalModelGroup", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePlatformStore.setState({
      status: "ready",
      capabilities: { schemaVersion: 1, os: "windows", arch: "x86_64", managedOllama: true, preferredVideoProxy: "h264_mp4" },
      error: null,
      loadGeneration: 0,
    });
  });

  it("未安装 Ollama 时渲染一键安装入口", async () => {
    render(
      <LocalModelGroup
        draft={draft}
        onPatchAi={noop}
        onPatchSettings={noop}
        notify={noop}
        fail={noop}
      />,
    );
    expect(await screen.findByText("未检测到 Ollama 本地引擎")).toBeTruthy();
    expect(screen.getByText(/一键安装 Ollama/)).toBeTruthy();
  });

  it("已安装但未运行时启动命令立即返回，并持续显示复检状态", async () => {
    vi.mocked(ollamaInstallStatus).mockResolvedValue({
      installed: true,
      running: false,
      version: "0.34.1",
      exePath: "C:/Ollama/ollama.exe",
      models: [],
      installerPath: null,
      installerSize: 0,
    });
    render(
      <LocalModelGroup
        draft={draft}
        onPatchAi={noop}
        onPatchSettings={noop}
        notify={noop}
        fail={noop}
      />,
    );

    await waitFor(() => expect(screen.getByRole("button", { name: "启动服务并复检" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "启动服务并复检" }));
    await waitFor(() => expect(ollamaStartService).toHaveBeenCalledTimes(1));
    expect(await screen.findByText(/正在启动并复检/)).toBeInTheDocument();
  });
});
