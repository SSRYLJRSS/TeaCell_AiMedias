/** platformStore 测试（三端复核 R0）：状态机 + 保守选择器。 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  usePlatformStore,
  selectManagedOllama,
  selectPreferredVideoProxy,
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
  preferredVideoProxy: "h264_mp4",
};

const macCaps: PlatformCapabilities = {
  schemaVersion: 1,
  os: "macos",
  arch: "aarch64",
  managedOllama: false,
  preferredVideoProxy: "h264_mp4",
};

const linuxCaps: PlatformCapabilities = {
  schemaVersion: 1,
  os: "linux",
  arch: "x86_64",
  managedOllama: false,
  preferredVideoProxy: "vp8_webm",
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
    expect(selectPreferredVideoProxy(s)).toBe("h264_mp4");
  });

  it("加载失败后 status=error 且选择器保守（不猜 Windows）", async () => {
    vi.mocked(getPlatformCapabilities).mockRejectedValue(new Error("boom"));
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(s.status).toBe("error");
    expect(s.error).toBe("boom");
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectPreferredVideoProxy(s)).toBe("h264_mp4");
  });

  it("未就绪（idle）时选择器全部保守默认", () => {
    const s = usePlatformStore.getState();
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectPreferredVideoProxy(s)).toBe("h264_mp4");
  });

  it("macOS：无托管 Ollama、代理仍为 MP4", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(macCaps);
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectPreferredVideoProxy(s)).toBe("h264_mp4");
  });

  it("Linux：无托管 Ollama、代理为 WebM", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(linuxCaps);
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(selectManagedOllama(s)).toBe(false);
    expect(selectPreferredVideoProxy(s)).toBe("vp8_webm");
  });

  it("ready 后重复 load 不再请求后端（幂等）", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(windowsCaps);
    await usePlatformStore.getState().load();
    await usePlatformStore.getState().load();
    expect(getPlatformCapabilities).toHaveBeenCalledTimes(1);
  });

  it("error 后重试成功：status 回到 ready 并加载能力（P1-02 可恢复）", async () => {
    vi.mocked(getPlatformCapabilities).mockRejectedValueOnce(new Error("boom"));
    await usePlatformStore.getState().load();
    expect(usePlatformStore.getState().status).toBe("error");
    // 重试：error 允许再次发起
    vi.mocked(getPlatformCapabilities).mockResolvedValueOnce(windowsCaps);
    await usePlatformStore.getState().load();
    const s = usePlatformStore.getState();
    expect(s.status).toBe("ready");
    expect(selectManagedOllama(s)).toBe(true);
  });

  it("并发重试：较旧响应晚返回不得覆盖较新状态（P1-02 竞态）", async () => {
    // 第一次 load：一个永不 resolve（模拟迟到的旧请求）；先驱动它进入 loading。
    let resolveStale: ((c: PlatformCapabilities) => void) | undefined;
    vi.mocked(getPlatformCapabilities).mockReturnValueOnce(
      new Promise<PlatformCapabilities>((res) => {
        resolveStale = res;
      }),
    );
    const first = usePlatformStore.getState().load();
    expect(usePlatformStore.getState().status).toBe("loading");

    // 强制回到可再次 load 的状态（模拟组件卸载后 store 被重置为 error/idle），发起新一次 load。
    usePlatformStore.setState({ status: "error", error: "reset", capabilities: null });
    vi.mocked(getPlatformCapabilities).mockResolvedValueOnce(linuxCaps);
    await usePlatformStore.getState().load();
    expect(usePlatformStore.getState().capabilities?.os).toBe("linux");

    // 旧请求现在才 resolve：代际已变，必须被丢弃，不覆盖 linux 结果。
    resolveStale?.(windowsCaps);
    await first;
    expect(usePlatformStore.getState().capabilities?.os).toBe("linux");
  });
});
