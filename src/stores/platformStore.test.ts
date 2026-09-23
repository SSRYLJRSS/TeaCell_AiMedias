import { beforeEach, describe, expect, it, vi } from "vitest";
import { getPlatformCapabilities } from "@/api/platform";
import {
  selectManagedOllama,
  selectPreferredVideoProxy,
  usePlatformStore,
} from "@/stores/platformStore";
import type { PlatformCapabilities } from "@/types/platform";

vi.mock("@/api/platform", () => ({ getPlatformCapabilities: vi.fn() }));

const windows: PlatformCapabilities = {
  schemaVersion: 1,
  os: "windows",
  arch: "x86_64",
  managedOllama: true,
  preferredVideoProxy: "h264_mp4",
};
const linux: PlatformCapabilities = {
  schemaVersion: 1,
  os: "linux",
  arch: "x86_64",
  managedOllama: false,
  preferredVideoProxy: "vp8_webm",
};

beforeEach(() => {
  vi.clearAllMocks();
  usePlatformStore.setState({ status: "idle", capabilities: null, error: null, loadGeneration: 0 });
});

describe("platformStore", () => {
  it("loads and caches capabilities", async () => {
    vi.mocked(getPlatformCapabilities).mockResolvedValue(windows);
    await usePlatformStore.getState().load();
    expect(selectManagedOllama(usePlatformStore.getState())).toBe(true);
    expect(selectPreferredVideoProxy(usePlatformStore.getState())).toBe("h264_mp4");
    await usePlatformStore.getState().load();
    expect(getPlatformCapabilities).toHaveBeenCalledTimes(1);
  });

  it("uses conservative selectors after failure", async () => {
    vi.mocked(getPlatformCapabilities).mockRejectedValue(new Error("boom"));
    await usePlatformStore.getState().load();
    expect(usePlatformStore.getState().status).toBe("error");
    expect(selectManagedOllama(usePlatformStore.getState())).toBe(false);
    expect(selectPreferredVideoProxy(usePlatformStore.getState())).toBe("h264_mp4");
  });

  it("supports retry after error", async () => {
    vi.mocked(getPlatformCapabilities).mockRejectedValueOnce(new Error("boom")).mockResolvedValueOnce(linux);
    await usePlatformStore.getState().load();
    await usePlatformStore.getState().load();
    expect(usePlatformStore.getState().capabilities?.os).toBe("linux");
  });
});
