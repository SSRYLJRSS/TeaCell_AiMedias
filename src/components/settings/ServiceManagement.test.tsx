import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ServiceManagement from "@/components/settings/ServiceManagement";
import { getAiUsageBindings, listAiConnections, saveAiConnection, setAiUsageBinding } from "@/api/connections";
import { usePlatformStore } from "@/stores/platformStore";
import type { PlatformCapabilities } from "@/types/platform";
import type { Settings } from "@/types/settings";

vi.mock("@/components/settings/AiConnectionManager", () => ({
  default: ({ deployment }: { deployment: string }) => (
    <div data-testid="connection-manager">{deployment}</div>
  ),
}));
vi.mock("@/components/settings/LocalModelGroup", () => ({
  default: ({ onModelSelected }: { onModelSelected?: (model: string) => Promise<void> }) => (
    <div data-testid="local-model-group">
      <button type="button" onClick={() => void onModelSelected?.("qwen3.5:4b")}>mock-select-local-model</button>
    </div>
  ),
}));
vi.mock("@/api/connections", () => ({
  getAiUsageBindings: vi.fn(),
  listAiConnections: vi.fn(),
  saveAiConnection: vi.fn(),
  setAiUsageBinding: vi.fn(),
}));

const draft: Settings = {
  ai: {
    profiles: [],
    activeProfile: "",
    videoTagging: false,
    videoTaggingMode: "cover",
    videoFrameCount: 3,
    batchLimit: 20,
    systemPromptTagging: "",
    systemPromptSearch: "",
    ollamaSourceId: "auto",
    confidenceMinSuggest: 0.3,
  },
  theme: "system",
  logLevel: "info",
  thumbnailCacheMb: 2048,
  tagCategories: [],
  libraryRoot: "",
  trashRetentionDays: 30,
  customDownloadSources: [],
  modelDownloadProxy: "",
  appearance: {
    grid: {
      libraryCellStep: 3,
      importCellStep: 1,
      cellAspect: "1:1",
      cellFit: "cover",
      matchDominantColor: false,
    },
    hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
    colorStrip: {
      enabled: true,
      showInLibraryGrid: false,
      showInViewer: true,
      showInImportGrid: false,
      height: "normal",
      mode: "ratio",
      count: 6,
    },
    kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
  },
};

function setPlatform(os: "windows" | "macos" | "linux") {
  const capabilities: PlatformCapabilities = {
    schemaVersion: 1,
    os,
    arch: os === "macos" ? "aarch64" : "x86_64",
    managedOllama: os === "windows",
    preferredVideoProxy: os === "linux" ? "vp8_webm" : "h264_mp4",
    nativeWindowControls: os === "macos",
    primaryModifier: os === "macos" ? "meta" : "ctrl",
    libraryTransferVersion: null,
  };
  usePlatformStore.setState({ status: "ready", capabilities, error: null });
}

describe("ServiceManagement platform policy", () => {
  beforeEach(() => setPlatform("windows"));

  it.each(["macos", "linux"] as const)(
    "%s 明确说明应用内 Ollama 不支持，并只显示在线服务入口",
    (os) => {
      setPlatform(os);
      render(
        <ServiceManagement
          draft={draft}
          onPatchAi={vi.fn()}
          onPatchSettings={vi.fn()}
          notify={vi.fn()}
          fail={vi.fn()}
        />,
      );

      expect(screen.getByRole("note")).toHaveTextContent("当前平台暂不支持应用内安装、启动或管理 Ollama");
      expect(screen.getByRole("tab", { name: "在线服务" })).toBeInTheDocument();
      expect(screen.queryByRole("tab", { name: "本机服务" })).not.toBeInTheDocument();
      expect(screen.queryByTestId("local-model-group")).not.toBeInTheDocument();
      expect(screen.getByTestId("connection-manager")).toHaveTextContent("cloud");
    },
  );

  it("Windows 保留本机服务入口和 Ollama 管理界面", () => {
    render(
      <ServiceManagement
        draft={draft}
        onPatchAi={vi.fn()}
        onPatchSettings={vi.fn()}
        notify={vi.fn()}
        fail={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("tab", { name: "本机服务" }));
    expect(screen.getByTestId("local-model-group")).toBeInTheDocument();
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
  });

  it("模型向导选择后写入实际本机连接，并且只补缺失的打标绑定", async () => {
    vi.mocked(listAiConnections).mockResolvedValue([]);
    vi.mocked(saveAiConnection).mockResolvedValue({
      id: "local-1",
      name: "本机 Ollama",
      deployment: "local",
      protocol: "openai_chat",
      baseUrl: "http://localhost:11434/v1",
      model: "qwen3.5:4b",
      maxConcurrency: 0,
      requestsPerMinute: 0,
      requestsPerHour: 0,
      hasKey: false,
      credentialStatus: "missing",
      enabled: true,
    });
    vi.mocked(getAiUsageBindings).mockResolvedValue({ super_search: null, tagging: null, agent: null });
    vi.mocked(setAiUsageBinding).mockResolvedValue(undefined);

    render(
      <ServiceManagement
        draft={draft}
        onPatchAi={vi.fn()}
        onPatchSettings={vi.fn()}
        notify={vi.fn()}
        fail={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "本机服务" }));
    fireEvent.click(screen.getByRole("button", { name: "mock-select-local-model" }));

    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "本机 Ollama",
          deployment: "local",
          protocol: "openai_chat",
          baseUrl: "http://localhost:11434/v1",
          model: "qwen3.5:4b",
        }),
      ),
    );
    await waitFor(() => expect(setAiUsageBinding).toHaveBeenCalledWith("tagging", "local-1"));
  });

  it("模型向导更新已有本机连接时保留当前有效的打标绑定", async () => {
    vi.clearAllMocks();
    const localConnection = {
      id: "local-1",
      name: "本机 Ollama",
      deployment: "local" as const,
      protocol: "openai_chat" as const,
      baseUrl: "http://localhost:11434/v1",
      model: "old-model",
      maxConcurrency: 0,
      requestsPerMinute: 0,
      requestsPerHour: 0,
      hasKey: false,
      credentialStatus: "missing" as const,
      enabled: true,
    };
    const cloudConnection = {
      ...localConnection,
      id: "cloud-1",
      name: "云端服务",
      deployment: "cloud" as const,
      baseUrl: "https://api.example.test/v1",
      credentialStatus: "configured" as const,
      hasKey: true,
    };
    vi.mocked(listAiConnections).mockResolvedValue([localConnection, cloudConnection]);
    vi.mocked(saveAiConnection).mockResolvedValue({ ...localConnection, model: "qwen3.5:4b" });
    vi.mocked(getAiUsageBindings).mockResolvedValue({ super_search: null, tagging: "cloud-1", agent: null });
    vi.mocked(setAiUsageBinding).mockResolvedValue(undefined);

    render(
      <ServiceManagement
        draft={draft}
        onPatchAi={vi.fn()}
        onPatchSettings={vi.fn()}
        notify={vi.fn()}
        fail={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "本机服务" }));
    fireEvent.click(screen.getByRole("button", { name: "mock-select-local-model" }));

    await waitFor(() =>
      expect(saveAiConnection).toHaveBeenCalledWith(
        expect.objectContaining({ id: "local-1", model: "qwen3.5:4b" }),
      ),
    );
    await waitFor(() => expect(getAiUsageBindings).toHaveBeenCalled());
    expect(setAiUsageBinding).not.toHaveBeenCalled();
  });
});
