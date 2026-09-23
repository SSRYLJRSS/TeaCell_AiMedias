/** W7-2：LocalModelGroup 基础渲染（A3 核心 UI，858 行补最小覆盖）：
 *  未检测到引擎时展示一键安装入口。 */
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import LocalModelGroup from "@/components/settings/LocalModelGroup";
import { usePlatformStore } from "@/stores/platformStore";
import type { Settings } from "@/types/settings";

vi.mock("@/api/ollama", () => ({
  ollamaInstallStatus: vi.fn().mockResolvedValue({ installed: false, running: false, version: null, ownership: "none" }),
  probeOllamaHardware: vi.fn(),
  ollamaRuntimeStatus: vi.fn().mockResolvedValue({ running: false, ownership: "none" }),
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
  ollamaModelDir: vi.fn(),
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
    // R1（三端复核）：本组件的托管 Ollama 行为编码 Windows 契约；
    // 平台能力 store 置 ready+windows，本机探针 effect 才会运行。
    // 非托管平台的「不触发探针」行为由 platformStore 单测覆盖。
    usePlatformStore.setState({
      status: "ready",
      error: null,
      capabilities: {
        schemaVersion: 1,
        os: "windows",
        arch: "x86_64",
        managedOllama: true,
        preferredVideoProxy: "h264_mp4",
      },
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

  it("非托管平台（macOS）：不触发任何 Ollama 探针命令", async () => {
    const { ollamaInstallStatus, ollamaListSources, ollamaRuntimeStatus } = await import(
      "@/api/ollama"
    );
    usePlatformStore.setState({
      status: "ready",
      error: null,
      capabilities: {
        schemaVersion: 1,
        os: "macos",
        arch: "aarch64",
        managedOllama: false,
        preferredVideoProxy: "h264_mp4",
      },
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
    // 挂载后给 effect 一个时机；不应有任何管理探针发出
    await Promise.resolve();
    expect(ollamaInstallStatus).not.toHaveBeenCalled();
    expect(ollamaListSources).not.toHaveBeenCalled();
    expect(ollamaRuntimeStatus).not.toHaveBeenCalled();
  });
});
