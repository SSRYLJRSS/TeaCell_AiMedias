/** 会话索引与持久草稿的前端投影。
 *
 * 关键约束（v2 A1-06）：草稿按稳定 conversationRef 分桶保存，切换/交替编辑两个会话
 * 不会互相串清单；重开或切页时按 ref 重新拉取投影重建，而不是依赖「最近一个」全局 ID。
 * Pi transcript 仍是唯一上下文源，本 store 只投影宿主元数据。
 */
import { create } from "zustand";
import {
  attachConversationAsset,
  createConversation,
  deleteConversation,
  getConversation,
  linkConversation,
  listConversations,
  renameConversation,
  saveConversationDraft,
  setConversationArchived,
  type ConversationAttachment,
  type ConversationDraft,
  type ConversationLinkKind,
  type ConversationSummary,
  type ConversationView,
  type DraftKind,
} from "@/api/conversations";
import { logger } from "@/utils/logger";

export type DraftMap = Partial<Record<DraftKind, unknown>>;

interface ConversationState {
  conversations: ConversationSummary[];
  activeRef: string | null;
  active: ConversationView | null;
  /** conversationRef → (draftKind → payload)，保证两个会话的草稿互相隔离。 */
  draftsByConversation: Record<string, DraftMap>;
  loading: boolean;
  error: string | null;
  refresh: (includeArchived?: boolean, query?: string) => Promise<void>;
  create: (title: string) => Promise<string>;
  select: (conversationRef: string) => Promise<void>;
  saveDraft: (
    conversationRef: string,
    kind: DraftKind,
    payload: Record<string, unknown>,
  ) => Promise<ConversationDraft>;
  rename: (conversationRef: string, title: string) => Promise<void>;
  setArchived: (conversationRef: string, archived: boolean) => Promise<void>;
  remove: (conversationRef: string) => Promise<void>;
  attachAsset: (
    conversationRef: string,
    assetId: number,
    displayName: string,
  ) => Promise<ConversationAttachment>;
  link: (
    conversationRef: string,
    kind: ConversationLinkKind,
    reference: string,
  ) => Promise<void>;
  draftFor: (conversationRef: string, kind: DraftKind) => unknown;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

let selectionRevision = 0;
const draftWriteRevisions = new Map<string, Map<DraftKind, number>>();
let draftWriteRevision = 0;

export const useConversationStore = create<ConversationState>((set, get) => ({
  conversations: [],
  activeRef: null,
  active: null,
  draftsByConversation: {},
  loading: false,
  error: null,

  refresh: async (includeArchived = false, query) => {
    set({ loading: true, error: null });
    try {
      const conversations = await listConversations(includeArchived, query);
      set({ conversations, loading: false });
    } catch (error) {
      logger.error("加载会话列表失败", { error: messageOf(error) });
      set({ loading: false, error: messageOf(error) });
    }
  },

  create: async (title) => {
    const summary = await createConversation(title);
    set((s) => ({
      conversations: [summary, ...s.conversations],
      draftsByConversation: {
        ...s.draftsByConversation,
        [summary.conversationRef]: {},
      },
    }));
    return summary.conversationRef;
  },

  select: async (conversationRef) => {
    const revision = ++selectionRevision;
    const writesAtStart = new Map(draftWriteRevisions.get(conversationRef));
    set({ error: null });
    const view = await getConversation(conversationRef);
    if (revision !== selectionRevision) return;
    const drafts: DraftMap = {};
    for (const draft of view.drafts) drafts[draft.kind] = draft.payload;
    set((s) => {
      for (const [kind, writeRevision] of draftWriteRevisions.get(conversationRef) ?? []) {
        if (writesAtStart.get(kind) !== writeRevision) {
          drafts[kind] = s.draftsByConversation[conversationRef]?.[kind];
        }
      }
      return {
        activeRef: conversationRef,
        active: view,
        draftsByConversation: { ...s.draftsByConversation, [conversationRef]: drafts },
      };
    });
  },

  saveDraft: async (conversationRef, kind, payload) => {
    const saved = await saveConversationDraft(conversationRef, kind, payload);
    const writes = draftWriteRevisions.get(conversationRef) ?? new Map<DraftKind, number>();
    writes.set(kind, ++draftWriteRevision);
    draftWriteRevisions.set(conversationRef, writes);
    set((s) => ({
      draftsByConversation: {
        ...s.draftsByConversation,
        [conversationRef]: {
          ...(s.draftsByConversation[conversationRef] ?? {}),
          [kind]: payload,
        },
      },
    }));
    return saved;
  },

  rename: async (conversationRef, title) => {
    const updated = await renameConversation(conversationRef, title);
    set((s) => ({
      conversations: s.conversations.map((item) =>
        item.conversationRef === conversationRef ? updated : item,
      ),
      active:
        s.active && s.active.conversationRef === conversationRef
          ? { ...s.active, title: updated.title }
          : s.active,
    }));
  },

  setArchived: async (conversationRef, archived) => {
    await setConversationArchived(conversationRef, archived);
    await get().refresh();
  },

  remove: async (conversationRef) => {
    await deleteConversation(conversationRef);
    draftWriteRevisions.delete(conversationRef);
    set((s) => {
      const draftsByConversation = { ...s.draftsByConversation };
      delete draftsByConversation[conversationRef];
      return {
        conversations: s.conversations.filter(
          (item) => item.conversationRef !== conversationRef,
        ),
        draftsByConversation,
        activeRef: s.activeRef === conversationRef ? null : s.activeRef,
        active: s.active?.conversationRef === conversationRef ? null : s.active,
      };
    });
  },

  attachAsset: async (conversationRef, assetId, displayName) => {
    const attachment = await attachConversationAsset(
      conversationRef,
      assetId,
      displayName,
    );
    set((s) =>
      s.active && s.active.conversationRef === conversationRef
        ? {
            active: {
              ...s.active,
              attachments: [...s.active.attachments, attachment],
            },
          }
        : {},
    );
    return attachment;
  },

  link: async (conversationRef, kind, reference) => {
    await linkConversation(conversationRef, kind, reference);
    set((s) =>
      s.active && s.active.conversationRef === conversationRef
        ? {
            active: {
              ...s.active,
              links: [...s.active.links, { kind, reference }],
            },
          }
        : {},
    );
  },

  draftFor: (conversationRef, kind) =>
    get().draftsByConversation[conversationRef]?.[kind],
}));
