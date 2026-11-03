/** 「待审计划」面板的打开状态（A5-02）。
 *
 * 存在的理由：外部客户端（MCP）准备出来的计划不属于任何一个本地会话。
 * `src-tauri/src/services/conversations.rs::ensure_plan_linkable` 只接受 gui/pi
 * 来源，mcp 计划在结构上无法挂进会话，所以助手页的 `linkedPlans` 永远看不到
 * 它们。A5-02 要求「外部准备的计划出现在茶馆可信界面并由用户批准或拒绝」，
 * 因此需要一个不依赖会话的入口。
 *
 * 这里只存界面状态：计划内容一律以后端 `list_pending_plans` 为准，不在前端
 * 缓存副本，也不做任何本地「已批准」推断——批准永远由后端 approve_plan 落库。
 * 面板每次打开都重新挂载，因此读取时机由面板自己的 effect 决定，不放在
 * openPanel 里（否则「面板没渲染也去读列表」，读失败还无处显示）。
 */
import { create } from "zustand";
import { listPendingPlans } from "@/api/plans";
import type { PlanPreview } from "@/types/plans";

interface PendingPlansState {
  open: boolean;
  plans: PlanPreview[];
  loading: boolean;
  error: string | null;
  openPanel: () => void;
  closePanel: () => void;
  reload: () => Promise<void>;
}

export const usePendingPlansStore = create<PendingPlansState>((set) => ({
  open: false,
  plans: [],
  loading: false,
  error: null,

  openPanel: () => set({ open: true, error: null }),

  closePanel: () => set({ open: false, error: null }),

  reload: async () => {
    set({ loading: true, error: null });
    try {
      const plans = await listPendingPlans();
      set({ plans, loading: false });
    } catch (error) {
      // 读取失败时清空列表：留一份旧列表会让人以为「这些还等着审批」，
      // 而那些计划可能已经过期或被别处处理掉了。
      set({
        plans: [],
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  },
}));
