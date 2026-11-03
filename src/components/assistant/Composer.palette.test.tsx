/**
 * `/` 命令面板与输入法守卫（WP2）：
 * - 输入 `/` 在输入框上方弹出面板，面板只列技能，选中后把技能正文注入输入框；
 * - 行首 `/home`、`/etc` 这类路径不误触面板；
 * - Esc 关闭面板并把焦点还给输入框；
 * - 中文输入法组合期间的回车不发送。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import Composer from "@/components/assistant/Composer";
import { BUILTIN_SKILLS } from "@/data/skills/catalog";
import { getAiUsageBindings, listAiConnections, type AiConnection } from "@/api/connections";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { useConversationStore } from "@/stores/conversationStore";

vi.mock("@/api/connections", () => ({
  getAiUsageBindings: vi.fn().mockResolvedValue({}),
  listAiConnections: vi.fn().mockResolvedValue([]),
  setAiUsageBinding: vi.fn(),
}));

function connection(over: Partial<AiConnection> = {}): AiConnection {
  return {
    id: "conn_cloud",
    name: "茶馆云",
    deployment: "cloud",
    protocol: "openai_chat",
    baseUrl: "https://api.example.com/v1",
    model: "tea-large",
    maxConcurrency: 2,
    requestsPerMinute: 30,
    requestsPerHour: 600,
    hasKey: true,
    credentialStatus: "configured",
    enabled: true,
    ...over,
  };
}

/** 模拟「把光标放到末尾并输入」，让 onChange 的 selectionStart 落在末尾。 */
function typeValue(el: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(el, value);
  el.setSelectionRange(value.length, value.length);
  fireEvent.input(el);
}

beforeEach(() => {
  useComposerInjectStore.setState({ pending: {} });
  useConversationStore.setState({ draftsByConversation: {} });
  vi.mocked(listAiConnections).mockResolvedValue([connection()]);
  vi.mocked(getAiUsageBindings).mockResolvedValue({ super_search: null, tagging: null, agent: "conn_cloud" });
});

function renderComposer(onSend = vi.fn()) {
  render(
    <Composer
      conversationRef="conv_a"
      attachments={[]}
      streaming={false}
      stopping={false}
      disabled={false}
      onSend={onSend}
      onStop={vi.fn()}
    />,
  );
  return onSend;
}

describe("Composer / 命令面板", () => {
  it("输入 / 弹出面板，面板只列技能，选中后把技能注入输入框", async () => {
    renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    typeValue(textarea, "/");
    expect(await screen.findByTestId("command-palette")).toBeInTheDocument();

    // 面板里不再出现权限 / 切换模型等非技能项（注意：存在名为「权限档位说明」的技能，
    // 所以这里用旧的「权限：」「切换模型：」前缀精确匹配，而不是宽泛地匹配「权限」二字）。
    expect(screen.queryByRole("menuitem", { name: /权限：/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /切换模型：/ })).not.toBeInTheDocument();

    // 正向结构断言：面板里每一行都是技能项，且数量与技能目录一致（将来混入清空/继续/停止等会被拦）。
    const rows = screen.getAllByRole("menuitem");
    expect(rows.length).toBe(BUILTIN_SKILLS.length);
    expect(rows.every((row) => (row.textContent ?? "").includes("插入技能："))).toBe(true);

    const firstSkill = BUILTIN_SKILLS[0];
    fireEvent.click(await screen.findByRole("menuitem", { name: new RegExp(firstSkill.name) }));
    await waitFor(() => expect(textarea.value).toContain(`【技能】${firstSkill.name}`));
    expect(screen.queryByTestId("command-palette")).not.toBeInTheDocument();
  });

  it("行首 /home、/etc 不误触命令面板", async () => {
    renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    typeValue(textarea, "/home");
    expect(screen.queryByTestId("command-palette")).not.toBeInTheDocument();
    typeValue(textarea, "/etc");
    expect(screen.queryByTestId("command-palette")).not.toBeInTheDocument();
  });

  it("Esc 关闭面板并把焦点还给输入框", async () => {
    renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    typeValue(textarea, "/");
    expect(await screen.findByTestId("command-palette")).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("command-palette")).not.toBeInTheDocument());
    expect(textarea).toHaveFocus();
  });

  it("命令面板挂在输入框上方的相对容器内，不会被顶到窗口顶部", async () => {
    renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    typeValue(textarea, "/");
    const menu = await screen.findByTestId("command-palette");
    const popover = menu.parentElement as HTMLElement;
    const wrapper = popover.parentElement as HTMLElement;
    expect(popover.className).toContain("picker-popover");
    // 包含块必须是输入框所在的 relative 容器，而不是全高祖先（否则会冒泡到窗口顶部）。
    expect(wrapper.className).toContain("relative");
    expect(wrapper.contains(textarea)).toBe(true);
    // 宽度受视口夹住，避免横向溢出。
    expect(popover.style.maxWidth).toContain("100vw");
  });

  it("输入框占位提示含有「输入 / 召唤技能」", () => {
    renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    expect(textarea.getAttribute("placeholder")).toContain("输入 / 召唤技能");
  });
});

describe("Composer 输入法守卫", () => {
  it("输入法组合期间的回车不发送，正常回车才发送", async () => {
    const onSend = renderComposer();
    const textarea = screen.getByLabelText("输入消息") as HTMLTextAreaElement;
    typeValue(textarea, "你好");

    const composing = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    Object.defineProperty(composing, "isComposing", { value: true });
    textarea.dispatchEvent(composing);
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.keyDown(textarea, { key: "Enter" });
    await waitFor(() => expect(onSend).toHaveBeenCalledWith("你好"));
  });
});
