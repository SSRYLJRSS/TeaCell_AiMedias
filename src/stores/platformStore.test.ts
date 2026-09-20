/** platformStore 测试（三端复核 R0）：状态机 + 保守选择器。 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  usePlatformStore,
  selectManagedOllama,
  selectNativeWindowControls,
  selectPrimaryModifier,
} from "@/stores/platformStore";
import { getPlatformCapabilities } from "@/api/platform";
import type { PlatformCapabilities } from "@/types/platform";

vi.mock("@/api/platform", () => ({
  getPlatformCapabilities: vi.fn(),
}));

const windowsCaps: PlatformCapabilities = {
  schemaVersion: 1,
  os: "windows",
  arch: "x86_64",
  managedOllama: true,
  nativeWindowControls: false,
  primaryModifier: "ctrl",
  libraryTransferVersion: null,
};

const macCaps: PlatformCapabilities = {
  schemaVersion: 1,
  os: "macos",
  arch: "aarch64",
  managedOllama: false,
  nativeWindowControls: true,
  primaryModifier: "meta",
  libraryTransferVersion: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  usePlatformStore.setState({ status: "idle", capabilities: null, error: null });
});

describe("platformStore（R0）", () => {
  it("加载成功后 status=ready 并缓存能力", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(windowsCaps);
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(s.status).toBe("ready");
    expect(s.capabilities?.os).toBe("windows");
    expect(selectManagedOllama(s)).toBe(true);
  });

  it("加载失败后 status=error 且选择器保守（不猜 Windows）", async () => {
    vi.mocked(getPlatformCapabilities).mockRejectedValue(new Error("boom"));
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(s.status).toBe("error");
    expect(s.error).toBe("boom");
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectNativeWindowControls(s)).toBe(false);
    expect(selectPrimaryModifier(s)).toBe("ctrl");
  });

  it("未就绪（idle）时选择器全部保守默认", () => {
    const s = usePlatformStore.getState();
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectNativeWindowControls(s)).toBe(false);
    expect(selectPrimaryModifier(s)).toBe("ctrl");
  });

  it("macOS：无托管 Ollama、原生窗口按钮、meta 修饰键", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(macCaps);
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectNativeWindowControls(s)).toBe(true);
    expect(selectPrimaryModifier(s)).toBe("meta");
  });

  it("ready 后重复 load 不再请求后端（幂等）", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(windowsCaps);
    await usePlatformStore.getState().load();
    await usePlatformStore.getState().load();
    expect(getPlatformCapabilities).toHaveBeenCalledTimes(1);
  });
});
