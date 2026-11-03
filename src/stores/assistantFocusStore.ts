import { create } from "zustand";

/**
 * A3-09：浮层「查看并确认」要定位到完整助手页的**同一计划**。
 *
 * 只用一次导航事件是不够的：点击发生在浮层里，目标页（AssistantPage）在同一
 * 次 tick 才挂载，事件监听还没注册，计划 ID 会丢。所以把「要定位哪张计划卡」
 * 落成一个待消费的状态：浮层写入，助手页挂载后读取并消费。
 *
 * 边界：这里只存一个 ID，不存计划内容、不存收据；计划本身仍只来自
 * `agentSessionStore.plans`（后端 PlanPreview 回读）。
 */
interface AssistantFocusState {
  planId: string | null;
  focusPlan: (planId: string) => void;
  clear: () => void;
}

export const useAssistantFocusStore = create<AssistantFocusState>((set) => ({
  planId: null,
  focusPlan: (planId) => set({ planId }),
  clear: () => set({ planId: null }),
}));
