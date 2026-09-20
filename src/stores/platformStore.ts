/**
 * 平台能力 store（三端复核 R0）：App 启动加载一次并缓存；所有页面从这里读取，
 * 禁止各页面自行 invoke 或探测 navigator.platform 决定后端能力。
 *
 * loading/error 时选择器保守返回：不把未知平台猜成 Windows；
 * managedOllama 只有 status==="ready" 且后端显式为 true 时才为 true。
 */
import { create } from "zustand";
import { getPlatformCapabilities } from "@/api/platform";
import type { PlatformCapabilities, PrimaryModifier } from "@/types/platform";

type PlatformStatus = "idle" | "loading" | "ready" | "error";

interface PlatformState {
  status: PlatformStatus;
  capabilities: PlatformCapabilities | null;
  error: string | null;
  /** 幂等加载：ready 后不重复请求；error/idle 允许重试。 */
  load: () => Promise<void>;
}

export const usePlatformStore = create<PlatformState>((set, get) => ({
  status: "idle",
  capabilities: null,
  error: null,
  load: async () => {
    const s = get();
    if (s.status === "ready" || s.status === "loading") return;
    set({ status: "loading", error: null });
    try {
      const capabilities = await getPlatformCapabilities();
      set({ status: "ready", capabilities, error: null });
    } catch (e) {
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

/** 是否使用原生窗口按钮：仅 ready 且后端 true。 */
export function selectNativeWindowControls(s: PlatformState): boolean {
  return s.status === "ready" && s.capabilities?.nativeWindowControls === true;
}

/** 主修饰键：未就绪默认 ctrl（最安全假设）。 */
export function selectPrimaryModifier(s: PlatformState): PrimaryModifier {
  return s.status === "ready" ? (s.capabilities?.primaryModifier ?? "ctrl") : "ctrl";
}
