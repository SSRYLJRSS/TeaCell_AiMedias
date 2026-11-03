/** 常用流程面板的打开状态（A4-07）。
 *
 * 面板本身只渲染一次（挂在助手页），入口有两处：
 * 1. 助手页头部的「常用流程」——浏览/查找/编辑/停用；
 * 2. 任务卡上的「保存为常用流程」——从这次已执行的业务步骤生成草稿再编辑。
 *
 * 所以「谁打开、带什么草稿」需要一个共享状态，而不是把回调层层往下传。
 * 这里只存界面状态：流程定义一律以后端返回为准，不在 store 里缓存副本。
 */
import { create } from "zustand";
import { draftWorkflowFromOperation, type WorkflowDraft } from "@/api/workflows";

interface WorkflowPanelState {
  open: boolean;
  /** 从一次操作生成的待保存草稿；为 null 时是纯粹的「浏览/管理」模式。 */
  draft: WorkflowDraft | null;
  /** 正在生成草稿的来源操作，用于按钮上的进行中文案。 */
  draftingFrom: string | null;
  error: string | null;
  openPanel: () => void;
  /** 从已执行的操作生成草稿并直接进入保存模式。 */
  openFromOperation: (operationId: string) => Promise<void>;
  closePanel: () => void;
  clearDraft: () => void;
}

export const useWorkflowPanelStore = create<WorkflowPanelState>((set) => ({
  open: false,
  draft: null,
  draftingFrom: null,
  error: null,

  openPanel: () => set({ open: true, draft: null, error: null }),

  openFromOperation: async (operationId) => {
    set({ draftingFrom: operationId, error: null });
    try {
      const draft = await draftWorkflowFromOperation(operationId);
      set({ draft, open: true });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      // 失败也要把面板打开：让用户看到原因，而不是点了按钮没有任何反应。
      set({ open: true, draft: null, error: message });
    } finally {
      set({ draftingFrom: null });
    }
  },

  closePanel: () => set({ open: false, draft: null, error: null }),
  clearDraft: () => set({ draft: null }),
}));
