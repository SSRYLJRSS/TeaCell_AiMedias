/**
 * 平台能力 store（三端复核 R0）：App 启动加载一次并缓存；所有页面从这里读取，
 * 禁止各页面自行 invoke 或探测 navigator.platform 决定后端能力。
 *
 * loading/error 时选择器保守返回：不把未知平台猜成 Windows；
 * managedOllama 只有 status==="ready" 且后端显式为 true 时才为 true。
 */
import { create } from "zustand";
import { getPlatformCapabilities } from "@/api/platform";
import type { PlatformCapabilities, PreferredVideoProxy } from "@/types/platform";

type PlatformStatus = "idle" | "loading" | "ready" | "error";

interface PlatformState {
  status: PlatformStatus;
  capabilities: PlatformCapabilities | null;
  error: string | null;
  /** 竞态代际（P1-02）：每次发起 load 自增；只有最新一次的响应允许落地，杜绝旧响应覆盖新状态。 */
  loadGeneration: number;
  /** 幂等加载：ready 后不重复请求；error/idle 允许重试。 */
  load: () => Promise<void>;
}

export const usePlatformStore = create<PlatformState>((set, get) => ({
  status: "idle",
  capabilities: null,
  error: null,
  loadGeneration: 0,
  load: async () => {
    const s = get();
    if (s.status === "ready" || s.status === "loading") return;
    // 递增代际并记下本次请求归属；响应回来时只有仍是最新代际才落地。
    const generation = s.loadGeneration + 1;
    set({ status: "loading", error: null, loadGeneration: generation });
    try {
      const capabilities = await getPlatformCapabilities();
      // P1-02：并发重试 / 组件卸载后较旧响应晚到时，代际已变 → 丢弃，不覆盖较新状态。
      if (get().loadGeneration !== generation) return;
      set({ status: "ready", capabilities, error: null });
    } catch (e) {
      if (get().loadGeneration !== generation) return;
      set({
        status: "error",
        error: e instanceof Error ? e.message : String(e),
      });
    }
  },
}));

/** 托管 Ollama 是否可用：仅 ready 且后端 true。未就绪一律 false（不猜 Windows）。 */
export function selectManagedOllama(s: PlatformState): boolean {
  return s.status === "ready" && s.capabilities?.managedOllama === true;
}

/**
 * 首选视频兼容代理变体：未就绪默认 h264_mp4（最通用假设）。
 * 前端所有 ensure/status/cancel 调用从此 selector 取 variant，不允许组件硬编码。
 */
export function selectPreferredVideoProxy(s: PlatformState): PreferredVideoProxy {
  return s.status === "ready"
    ? (s.capabilities?.preferredVideoProxy ?? "h264_mp4")
    : "h264_mp4";
}
