/**
 * 全局静态平台能力缓存。
 * loading/error 时选择器保守返回，不把未知目标猜成 Windows；代际值防止迟到响应覆盖重试结果。
 */
import { create } from "zustand";
import { getPlatformCapabilities } from "@/api/platform";
import type { PlatformCapabilities, PreferredVideoProxy } from "@/types/platform";

type PlatformStatus = "idle" | "loading" | "ready" | "error";

interface PlatformState {
  status: PlatformStatus;
  capabilities: PlatformCapabilities | null;
  error: string | null;
  loadGeneration: number;
  load: () => Promise<void>;
}

export const usePlatformStore = create<PlatformState>((set, get) => ({
  status: "idle",
  capabilities: null,
  error: null,
  loadGeneration: 0,
  load: async () => {
    const current = get();
    if (current.status === "ready" || current.status === "loading") return;
    const generation = current.loadGeneration + 1;
    set({ status: "loading", error: null, loadGeneration: generation });
    try {
      const capabilities = await getPlatformCapabilities();
      if (get().loadGeneration !== generation) return;
      set({ status: "ready", capabilities, error: null });
    } catch (error) {
      if (get().loadGeneration !== generation) return;
      set({
        status: "error",
        error: error instanceof Error ? error.message : String(error),
      });
    }
  },
}));

export function selectManagedOllama(state: PlatformState): boolean {
  return state.status === "ready" && state.capabilities?.managedOllama === true;
}

/** 未就绪时使用最通用的 MP4，且不阻止 UI 继续启动。 */
export function selectPreferredVideoProxy(state: PlatformState): PreferredVideoProxy {
  return state.status === "ready" ? (state.capabilities?.preferredVideoProxy ?? "h264_mp4") : "h264_mp4";
}
