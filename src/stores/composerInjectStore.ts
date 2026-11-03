/** 跨组件把一段文字放进某个会话的输入框（A4-07「再次使用」）。
 *
 * 为什么不直接写草稿：助手输入区的文本是组件本地状态，草稿只是它的落盘副本。
 * 「再次使用常用流程」需要把「这次要填哪些输入」的清单交给用户继续编辑，
 * 走这里比每次都给 Composer 加一层 prop 更小、也不会串到别的会话。
 */
import { create } from "zustand";

interface ComposerInjectState {
  /** conversationRef → 待注入文本。取走即清空。 */
  pending: Record<string, string>;
  inject: (conversationRef: string, text: string) => void;
  take: (conversationRef: string) => string | null;
}

export const useComposerInjectStore = create<ComposerInjectState>((set, get) => ({
  pending: {},
  inject: (conversationRef, text) =>
    set((state) => ({ pending: { ...state.pending, [conversationRef]: text } })),
  take: (conversationRef) => {
    const text = get().pending[conversationRef] ?? null;
    if (text === null) return null;
    set((state) => {
      const next = { ...state.pending };
      delete next[conversationRef];
      return { pending: next };
    });
    return text;
  },
}));
