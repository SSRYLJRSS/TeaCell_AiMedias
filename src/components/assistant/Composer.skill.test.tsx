/**
 * 助手输入区用 `/` 召唤技能：把技能正文作为本次操作参考注入输入框，并显示可移除胶囊。
 *
 * 「插入技能」胶囊已按用户反馈移除（改为 `输入 / 召唤技能`），因此这里通过 `/` 命令面板
 * 选技能，验收口径不变：
 * - 选择技能后，技能正文与"操作参考"说明进入输入框，不携带执行/批准语义；
 * - 输入区出现"已选技能"胶囊，移除后胶囊消失且注入片段被回收；
 * - 只注入纯文本，不改动草稿保存与发送链路（由 Composer 既有逻辑覆盖）。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import Composer from "@/components/assistant/Composer";
import { BUILTIN_SKILLS } from "@/data/skills/catalog";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useConversationStore } from "@/stores/conversationStore";

vi.mock("@/api/connections", () => ({
  getAiUsageBindings: vi.fn().mockResolvedValue({ super_search: null, tagging: null, agent: null }),
  listAiConnections: vi.fn().mockResolvedValue([]),
  setAiUsageBinding: vi.fn(),
}));

beforeEach(() => {
  useComposerInjectStore.setState({ pending: {} });
  useConversationStore.setState({ draftsByConversation: {} });
});

function renderComposer(conversationRef: string) {
  return render(
    <Composer
      conversationRef={conversationRef}
      attachments={[]}
      streaming={false}
      stopping={false}
      disabled={false}
      onSend={vi.fn()}
      onStop={vi.fn()}
    />,
  );
}

/** 模拟「把光标放到末尾并输入」，让 onChange 的 selectionStart 落在末尾。 */
function typeValue(el: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(el, value);
  el.setSelectionRange(value.length, value.length);
  fireEvent.input(el);
}

const firstSkill = BUILTIN_SKILLS[0];

/** 在输入框里敲一个行首 `/` 唤起面板，选中目标技能。 */
async function pickSkillViaSlash() {
  const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
  typeValue(textarea, "/");
  fireEvent.click(await screen.findByRole("menuitem", { name: new RegExp(firstSkill.name) }));
  return textarea;
}

describe("Composer 用 / 召唤技能", () => {
  it("选择技能后把正文与操作参考注入输入框，并显示已选技能胶囊", async () => {
    renderComposer("conv_a");

    const textarea = await pickSkillViaSlash();
    await waitFor(() => expect(textarea.value).toContain(`【技能】${firstSkill.name}`));
    // 只是操作参考，不写入执行/批准语义。
    expect(textarea.value).toContain("操作参考");
    // 可移除胶囊出现在输入区。
    expect(screen.getByTestId("selected-skill-chip")).toHaveTextContent(firstSkill.name);
  });

  it("移除已选技能胶囊时回收注入片段，不残留技能正文", async () => {
    renderComposer("conv_a");

    const textarea = await pickSkillViaSlash();
    await waitFor(() => expect(textarea.value).toContain(`【技能】${firstSkill.name}`));

    fireEvent.click(screen.getByRole("button", { name: `移除技能 ${firstSkill.name}` }));

    await waitFor(() =>
      expect(screen.queryByTestId("selected-skill-chip")).not.toBeInTheDocument(),
    );
    expect((screen.getByLabelText("输入消息") as HTMLTextAreaElement).value).not.toContain(
      `【技能】${firstSkill.name}`,
    );
  });

  it("保留用户已写内容：注入追加在草稿之后，不覆盖", async () => {
    useConversationStore.setState({
      draftsByConversation: { conv_a: { assistant: { text: "先写一句" } } },
    });
    renderComposer("conv_a");

    const textarea = (await screen.findByLabelText("输入消息")) as HTMLTextAreaElement;
    await waitFor(() => expect(textarea).toHaveValue("先写一句"));

    // 新起一行再敲 `/`，触发行首 token（不覆盖前文）。
    typeValue(textarea, "先写一句\n/");
    fireEvent.click(await screen.findByRole("menuitem", { name: new RegExp(firstSkill.name) }));

    await waitFor(() => expect(textarea.value).toContain("先写一句"));
    expect(textarea.value.indexOf("先写一句")).toBeLessThan(
      textarea.value.indexOf(`【技能】${firstSkill.name}`),
    );
  });
});
