/** A3-08：素材库「本次结果视图」的打开状态。
 *
 *  结果视图只展示一份**固定快照**（交接那一刻的 id 集合），不是新的筛选条件，
 *  也不创建分库：退出后回到用户原来的浏览上下文（筛选、选择、滚动都不动）。
 *  因此这里只存「看哪一份快照」，不存素材、不存分页游标 —— 那些由结果视图
 *  组件按 selectionRef 自己读，避免与素材库的领域状态互相污染。 */
import { create } from "zustand";

export interface SelectionResultTarget {
  /** 后端签发的不可变快照引用 */
  selectionRef: string;
  /** 结果视图标题（如「助手交接的 12 项」） */
  title: string;
  /** 交接时的查询描述，仅用于说明来源 */
  query: string;
  /** A3-08：这份快照属于哪个会话。
   *
   *  target 是全局单槽（结果视图与素材库互斥，只可能看一份），但「属于哪个会话」
   *  必须记下来：在 A 会话打开结果视图后切到 B 会话，再回到库页仍会看到 A 的快照，
   *  没有来源标注的话用户会以为这是 B 会话的交接。有了它就能明确说出「来自哪个会话」，
   *  并在会话对不上时给出说明，而不是默默展示另一个会话的集合。 */
  conversationRef?: string;
}

interface SelectionResultState {
  target: SelectionResultTarget | null;
  /** selectionRef → 结果视图的浏览位置（已加载页数 + 滚动位置）。
   *
   *  结果视图在打开 Viewer 时会被整体卸载（与素材库同一种页面级替换）。如果
   *  这些只存在组件里，预览下一张再回来就会跳回顶部、并把已加载的后续页丢掉 ——
   *  交接来的批次往往很长，这属于「离开/返回丢了上下文」。因此按快照引用存在
   *  组件外：回来时据此重新读到同一页，再恢复滚动位置。 */
  progressBySelection: Record<string, SelectionProgress>;
  open: (target: SelectionResultTarget) => void;
  close: () => void;
  rememberProgress: (selectionRef: string, progress: SelectionProgress) => void;
}

/** 结果视图的浏览位置。`loadedPages` 至少为 1（首屏）。 */
export interface SelectionProgress {
  loadedPages: number;
  scrollTop: number;
}

export const useSelectionResultStore = create<SelectionResultState>((set) => ({
  target: null,
  progressBySelection: {},
  open: (target) => set({ target }),
  close: () => set({ target: null }),
  rememberProgress: (selectionRef, progress) =>
    set((state) => ({
      progressBySelection: { ...state.progressBySelection, [selectionRef]: progress },
    })),
}));
