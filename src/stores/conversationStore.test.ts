/**
 * conversationStore 测试（v2 A1-06）：两个会话交替编辑不串清单；
 * 按稳定 conversationRef 重建投影；删除只移除本地投影。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  createConversation,
  deleteConversation,
  getConversation,
  saveConversationDraft,
  type ConversationSummary,
  type ConversationView,
} from "@/api/conversations";
import { useConversationStore } from "@/stores/conversationStore";

vi.mock("@/api/conversations", () => ({
  createConversation: vi.fn(),
  listConversations: vi.fn(),
  getConversation: vi.fn(),
  renameConversation: vi.fn(),
  setConversationArchived: vi.fn(),
  deleteConversation: vi.fn(),
  saveConversationDraft: vi.fn(),
  attachConversationAsset: vi.fn(),
  linkConversation: vi.fn(),
}));

const refA = "conversation_aaaaaaaaaaaaaaaa";
const refB = "conversation_bbbbbbbbbbbbbbbb";

const summary = (ref: string, title: string): ConversationSummary => ({
  conversationRef: ref,
  title,
  status: "active",
  createdAtUnixSeconds: 1,
  updatedAtUnixSeconds: 2,
});

function viewOf(ref: string, title: string, draft?: Record<string, unknown>): ConversationView {
  return {
    conversationRef: ref,
    title,
    status: "active",
    createdAtUnixSeconds: 1,
    updatedAtUnixSeconds: 2,
    drafts: draft
      ? [{ kind: "import", payload: draft, revision: 1, updatedAtUnixSeconds: 2 }]
      : [],
    attachments: [],
    links: [],
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  useConversationStore.setState({
    conversations: [],
    activeRef: null,
    active: null,
    draftsByConversation: {},
    loading: false,
    error: null,
  });
});

it("较早会话请求迟到时不覆盖后来选择的会话", async () => {
  let finishA!: (view: ConversationView) => void;
  vi.mocked(getConversation).mockImplementationOnce(() => new Promise((resolve) => { finishA = resolve; }));
  const selectingA = useConversationStore.getState().select(refA);
  vi.mocked(getConversation).mockResolvedValueOnce(viewOf(refB, "B"));
  await useConversationStore.getState().select(refB);
  finishA(viewOf(refA, "A"));
  await selectingA;
  expect(useConversationStore.getState().activeRef).toBe(refB);
  expect(useConversationStore.getState().active?.title).toBe("B");
});

it("旧读取返回时保留读取期间已成功保存的草稿", async () => {
  let finishRead!: (view: ConversationView) => void;
  vi.mocked(getConversation).mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }));
  const reading = useConversationStore.getState().select(refA);
  vi.mocked(saveConversationDraft).mockImplementationOnce(async (_ref, kind, payload) => ({ kind, payload, revision: 2, updatedAtUnixSeconds: 3 }));
  await useConversationStore.getState().saveDraft(refA, "import", { pendingPlanRefs: [{ planRef: "new-plan" }] });
  finishRead(viewOf(refA, "A", { pendingPlanRefs: [] }));
  await reading;
  expect(useConversationStore.getState().draftFor(refA, "import")).toEqual({ pendingPlanRefs: [{ planRef: "new-plan" }] });
});

describe("conversationStore", () => {
  it("两个会话交替编辑不串清单", async () => {
    vi.mocked(createConversation)
      .mockResolvedValueOnce(summary(refA, "甲"))
      .mockResolvedValueOnce(summary(refB, "乙"));
    vi.mocked(saveConversationDraft).mockImplementation(async (_ref, kind, payload) => ({
      kind,
      payload,
      revision: 1,
      updatedAtUnixSeconds: 3,
    }));

    const store = useConversationStore.getState();
    await store.create("甲");
    await store.create("乙");
    await useConversationStore
      .getState()
      .saveDraft(refA, "import", { files: ["a.jpg"] });
    await useConversationStore
      .getState()
      .saveDraft(refB, "import", { files: ["b.jpg", "c.jpg"] });
    // 回到会话甲继续编辑，不读取会话乙的草稿。
    await useConversationStore
      .getState()
      .saveDraft(refA, "import", { files: ["a.jpg", "d.jpg"] });

    expect(useConversationStore.getState().draftFor(refA, "import")).toEqual({
      files: ["a.jpg", "d.jpg"],
    });
    expect(useConversationStore.getState().draftFor(refB, "import")).toEqual({
      files: ["b.jpg", "c.jpg"],
    });
  });

  it("按稳定 ref 重建投影：选中会话时拉取其自身草稿", async () => {
    vi.mocked(getConversation).mockImplementation(async (ref) =>
      ref === refA ? viewOf(refA, "甲", { files: ["a.jpg"] }) : viewOf(refB, "乙"),
    );

    await useConversationStore.getState().select(refB);
    expect(useConversationStore.getState().activeRef).toBe(refB);
    expect(useConversationStore.getState().draftFor(refB, "import")).toBeUndefined();

    await useConversationStore.getState().select(refA);
    expect(useConversationStore.getState().activeRef).toBe(refA);
    expect(useConversationStore.getState().draftFor(refA, "import")).toEqual({
      files: ["a.jpg"],
    });
  });

  it("删除会话只移除本地投影，不清空其它会话草稿", async () => {
    useConversationStore.setState({
      conversations: [summary(refA, "甲"), summary(refB, "乙")],
      draftsByConversation: { [refA]: { import: { files: ["a.jpg"] } }, [refB]: {} },
    });
    vi.mocked(deleteConversation).mockResolvedValue(undefined);

    await useConversationStore.getState().remove(refA);
    const state = useConversationStore.getState();
    expect(state.conversations.map((c) => c.conversationRef)).toEqual([refB]);
    expect(state.draftsByConversation[refA]).toBeUndefined();
    expect(state.draftsByConversation[refB]).toEqual({});
  });
});
