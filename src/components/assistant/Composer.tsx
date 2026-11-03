/** 助手输入区（A3-04 / A3-06 服务与外发提示）。
 *
 * 约束：
 * - 明确显示当前 Agent 服务与模型；未绑定服务时不禁用输入区（手动流程仍可用），
 *   但要把「还没有绑定服务」说清楚，而不是静默失败；
 * - 「本机 / 在线」必须由**地址**推出，不能采信 `deployment` 声明（A3-06 停下条件）；
 * - 外发摘要只描述真实发生的外发：助手通道当前**只发文字**，不发图片内容，
 *   因此不得宣称「发送压缩 JPEG」或「可提供原始清晰度」（见冻结契约
 *   plans/v2/12-implementation-contracts.md §「消息只接受文本」）；
 * - 本机服务不需要 API 密钥，`credentialStatus === "missing"` 对它不是错误；
 * - 附件来自本会话已关联的素材，available=false 必须显式提示，不能静默重新授权；
 * - Enter 发送 / Shift+Enter 换行；回答进行中禁用发送，并单独提供「停止回答」；
 * - 草稿按会话分桶落盘（`assistant` 草稿种类），切换会话不会把半句话串到别的会话。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { ArrowUp, BookOpen, Info, Square } from "lucide-react";
import Button from "@/components/common/Button";
import {
  getAiUsageBindings,
  listAiConnections,
  setAiUsageBinding,
  type AiConnection,
} from "@/api/connections";
import type { ConversationAttachment } from "@/api/conversations";
import { useConversationStore } from "@/stores/conversationStore";
import { useAgentSessionStore } from "@/stores/agentSessionStore";
import PermissionPicker from "@/components/assistant/PermissionPicker";
import ServicePicker, { type ServiceOption } from "@/components/assistant/ServicePicker";
import CommandPalette from "@/components/assistant/CommandPalette";
import {
  filterPaletteItems,
  shouldOpenPalette,
  slashToken,
  type PaletteItem,
} from "@/components/assistant/skills/commands";
import ComposerMetrics, { type ComposerUsage } from "@/components/assistant/ComposerMetrics";
import { type AgentPermission } from "@/constants/agentPermission";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { BUILTIN_SKILLS } from "@/data/skills/catalog";
import { injectionText } from "@/data/skills/schema";
import type { SkillDefinition } from "@/data/skills/types";
import { classifyEndpoint, deploymentMatchesEndpoint } from "@/utils/endpointScope";
import { logger } from "@/utils/logger";

const MAX_PROMPT_BYTES = 4096;
const DRAFT_SAVE_DELAY_MS = 600;

/** Rust 侧按 UTF-8 字节限制（`MAX_PROMPT_BYTES`），不是按字符数。
 *  中文一个字符 3 字节，旧实现按 4096 字符放行，用户会在发送后收到 INVALID_ARG。
 *  这里改为同一口径，平时不显示计数，只在接近上限时提醒。 */
function promptBytes(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** 云端部署才需要 API 密钥；本机服务不读密钥，缺失属正常状态。 */
function credentialProblem(connection: AiConnection): boolean {
  if (connection.deployment !== "cloud") return false;
  return connection.credentialStatus !== "configured";
}

/** 这个服务是否会让发送必然失败（停用 / 云端缺密钥 / 地址后端不会接受）。 */
function serviceBlocked(connection: AiConnection): boolean {
  return (
    !connection.enabled ||
    credentialProblem(connection) ||
    classifyEndpoint(connection.baseUrl) === "invalid"
  );
}

/** 失败原因要具体到「哪一步不行、去哪里修」，而不是笼统一句「发送会失败」。 */
function blockedReason(connection: AiConnection): string {
  if (!connection.enabled) {
    return `「${connection.name}」已停用，发送会失败。你可以先启用它，或在设置里换一个服务。`;
  }
  if (credentialProblem(connection)) {
    return `「${connection.name}」的密钥不可用，发送会失败。请到设置里重新填写密钥，或换一个服务。`;
  }
  return `「${connection.name}」的接口地址无法识别，发送会失败。请到设置里填写完整的 http(s) 地址。`;
}

export interface AgentModelLabel {
  name: string | null;
  detail: string;
}

/** 草稿载荷是封闭 JSON 对象（A1-06），助手草稿只存一个 `text` 字段。 */
function draftText(payload: unknown): string {
  if (!payload || typeof payload !== "object") return "";
  const value = (payload as { text?: unknown }).text;
  return typeof value === "string" ? value : "";
}

export default function Composer({
  conversationRef,
  attachments,
  streaming,
  stopping,
  stoppable,
  disabled,
  compact = false,
  usage = null,
  onSend,
  onStop,
}: {
  conversationRef: string;
  attachments: ConversationAttachment[];
  streaming: boolean;
  stopping: boolean;
  /**
   * 本轮尚未结束时为 true：可以取消，但不能接着发新提示词。
   * `unknown`（发送结果未确认）也属于未结束 —— Rust 已保留该 run，界面必须给出
   * 精确取消入口，否则用户既不能停也不能安全重发（A3-04/A3-09）。
   * 缺省时沿用 `streaming`，保持既有调用点语义不变。
   */
  stoppable?: boolean;
  disabled: boolean;
  /** 浮层里复用同一张卡片：收起统计行、压缩内边距，把高度留给对话。 */
  compact?: boolean;
  /**
   * 本会话的用量统计。Agent 网关还没回传 usage 时传 null：
   * 统计行只显示素材数，不估算 token、不伪造缓存命中率与上下文占用。
   */
  usage?: ComposerUsage | null;
  onSend: (text: string) => Promise<void> | void;
  onStop: () => Promise<void> | void;
}) {
  const draft = useConversationStore((s) => s.draftsByConversation[conversationRef]?.assistant);
  // 权限档位是会话级运行时开关（A3-10）：直接读 store，避免每个调用点都多传两个
  // props；档位本身不持久化，重开应用回到默认档。
  const permission = useAgentSessionStore((s) => s.permission);
  const setPermission = useAgentSessionStore((s) => s.setPermission);
  const saveDraft = useConversationStore((s) => s.saveDraft);
  /** A4-07：本会话待注入的「再次使用」文本（订阅后才能触发一次 effect）。 */
  const injectedText = useComposerInjectStore((s) => s.pending[conversationRef] ?? null);
  const [text, setText] = useState(() => draftText(draft));
  const [model, setModel] = useState<AgentModelLabel>({ name: null, detail: "正在读取 Agent 服务…" });
  /** A3-06：可选服务列表与当前绑定；只在用户显式切换时才写绑定。 */
  const [connections, setConnections] = useState<AiConnection[]>([]);
  const [bindingId, setBindingId] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 已落盘的草稿文本：只有真正改动才写回，避免切换会话时空写。 */
  const persisted = useRef(draftText(draft));
  /** 本会话通过「插入技能」选中的技能：只作本轮操作参考，不写入任何执行/批准语义。 */
  const [selectedSkill, setSelectedSkill] = useState<SkillDefinition | null>(null);
  /** 本次注入的技能正文片段：移除胶囊时按片段回收，避免误删用户自己写的内容。 */
  const appliedSkillText = useRef("");
  /** 输入框引用：`/` 命令面板需要它的位置做浮层锚点，并把焦点还给输入框。 */
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  /** 输入区（输入框 + `/` 面板）容器引用：用于「点击外部关闭面板」。 */
  const inputAreaRef = useRef<HTMLDivElement | null>(null);
  /** `/` 命令面板：是否展开、当前 token 区间、过滤后的项与高亮下标。 */
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteToken, setPaletteToken] = useState<{ query: string; start: number; end: number } | null>(null);
  const [paletteItems, setPaletteItems] = useState<PaletteItem[]>([]);
  const [paletteIndex, setPaletteIndex] = useState(0);
  /** 切换会话时重置命令面板，避免把上一个会话的 `/` 状态带过来。 */
  useEffect(() => {
    setPaletteOpen(false);
    setPaletteToken(null);
    setPaletteItems([]);
    setPaletteIndex(0);
  }, [conversationRef]);

  useEffect(() => {
    // 切换会话时载入该会话自己的草稿，绝不沿用上一个会话的半句话。
    const next = draftText(useConversationStore.getState().draftsByConversation[conversationRef]?.assistant);
    persisted.current = next;
    setText(next);
    // 已选技能同样按会话隔离：切到别的会话不把上一个会话的技能胶囊带过去。
    appliedSkillText.current = "";
    setSelectedSkill(null);
    setError(null);
  }, [conversationRef]);

  // A4-07：「再次使用常用流程」把「这次还要填什么」放回输入框。
  // 取走即清空，同一会话不会被重复注入；值仍然只是纯文本，用户可自由改写。
  useEffect(() => {
    if (injectedText === null) return;
    const taken = useComposerInjectStore.getState().take(conversationRef);
    if (taken === null) return;
    setText((current) => (current.trim().length > 0 ? `${current}\n\n${taken}` : taken));
    setError(null);
  }, [conversationRef, injectedText]);

  useEffect(() => {
    if (text === persisted.current) return;
    const timer = window.setTimeout(() => {
      persisted.current = text;
      void saveDraft(conversationRef, "assistant", { text }).catch((cause: unknown) => {
        logger.warn("保存助手草稿失败", {
          conversationRef,
          error: cause instanceof Error ? cause.message : String(cause),
        });
      });
    }, DRAFT_SAVE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [text, conversationRef, saveDraft]);

  /** 把绑定 id + 连接列表折算成「模型：X · 本机/在线 · 服务名」的展示。
   *
   * 本机/在线一律由 `baseUrl` 推出（`classifyEndpoint`）；`deployment` 只用来
   * 判断声明与地址是否矛盾，矛盾时必须在同一行说清楚，不能静默采信声明。
   */
  const describeModel = (boundId: string | null, list: AiConnection[]): AgentModelLabel => {
    if (!boundId) return { name: null, detail: "尚未绑定 Agent 服务，请先在设置中选择" };
    const bound = list.find((item) => item.id === boundId);
    if (!bound) return { name: null, detail: "绑定的 Agent 服务已不存在，请重新选择" };
    const scope = classifyEndpoint(bound.baseUrl);
    const where = scope === "loopback" ? "本机" : scope === "remote" ? "在线" : "地址无法识别";
    const flags = [bound.enabled ? "" : "已停用"];
    if (!deploymentMatchesEndpoint(bound.deployment, bound.baseUrl)) {
      flags.push(`设置的部署方式是「${bound.deployment === "local" ? "本机" : "在线"}」，与地址不一致`);
    }
    const suffix = flags.filter(Boolean).join(" · ");
    return {
      name: bound.model || bound.name,
      detail: `${where} · ${bound.name}${suffix ? ` · ${suffix}` : ""}`,
    };
  };

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const [bindings, list] = await Promise.all([getAiUsageBindings(), listAiConnections()]);
        if (cancelled) return;
        const boundId = bindings.agent ?? null;
        setConnections(list);
        setBindingId(boundId);
        setModel(describeModel(boundId, list));
      } catch (cause) {
        if (cancelled) return;
        logger.warn("读取 Agent 服务信息失败", {
          error: cause instanceof Error ? cause.message : String(cause),
        });
        setModel({ name: null, detail: "无法读取 Agent 服务信息" });
      }
    };
    void load();

    // 聊天页切到设置页再切回来不会重建组件，而 Agent 服务绑定就在设置里；
    // 回到窗口时重读一次，否则用户绑好服务回来仍被提示「还没有绑定」。
    const reloadWhenVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    window.addEventListener("focus", reloadWhenVisible);
    document.addEventListener("visibilitychange", reloadWhenVisible);

    return () => {
      cancelled = true;
      window.removeEventListener("focus", reloadWhenVisible);
      document.removeEventListener("visibilitychange", reloadWhenVisible);
    };
  }, []);

  /** A3-06：切换只作用于后续调用；已批准的任务保持原计划服务（后端按计划快照执行）。 */
  const switchService = async (nextId: string) => {
    const next = nextId === "" ? null : nextId;
    setSwitching(true);
    setError(null);
    try {
      await setAiUsageBinding("agent", next);
      setBindingId(next);
      setModel(describeModel(next, connections));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSwitching(false);
    }
  };

  const boundConnection = useMemo(
    () => connections.find((item) => item.id === bindingId) ?? null,
    [connections, bindingId],
  );

  /** 外发摘要的第一句：只说真实外发范围，不宣称发送图片内容。 */
  const outbound = useMemo(() => {
    if (!boundConnection) return "";
    const scope = classifyEndpoint(boundConnection.baseUrl);
    if (scope === "loopback") {
      return `请求会发到本机地址（${boundConnection.name}，模型 ${boundConnection.model}），不会离开这台电脑。`;
    }
    if (scope === "remote") {
      return `文字请求会发到 ${boundConnection.name}（模型 ${boundConnection.model}），这是一个在线地址。`;
    }
    return `「${boundConnection.name}」的接口地址无法识别，发送会失败；请到设置里检查地址。`;
  }, [boundConnection]);

  const unavailableAttachments = useMemo(
    () => attachments.filter((item) => !item.available),
    [attachments],
  );
  /** R34：失败后「切换」是一个明确动作，把用户送到服务选择器，而不是替他换。 */
  const servicePickerRef = useRef<HTMLButtonElement | null>(null);
  const focusServicePicker = () => {
    const picker = servicePickerRef.current;
    if (!picker) return;
    picker.focus();
    picker.scrollIntoView?.({ block: "nearest" });
  };
  const bytes = promptBytes(text);
  const tooLong = bytes > MAX_PROMPT_BYTES;
  /** 只在接近上限时提示，平时不占用界面。 */
  const nearLimit = !tooLong && bytes > MAX_PROMPT_BYTES * 0.85;
  /** 本轮未结束（流式中或结果未确认）时只能取消，不能接着发。 */
  const pendingRun = stoppable ?? streaming;

  /** 服务选择器的候选行：本机/在线由地址推出，停用/缺密钥如实标注（A3-06 / R33）。 */
  const serviceOptions = useMemo<ServiceOption[]>(
    () =>
      connections.map((item) => {
        const scope = classifyEndpoint(item.baseUrl);
        const where = scope === "loopback" ? "本机" : scope === "remote" ? "在线" : "地址无法识别";
        const flags = [item.enabled ? "" : "已停用", credentialProblem(item) ? "密钥不可用" : ""]
          .filter(Boolean)
          .join(" · ");
        return {
          id: item.id,
          title: item.model || item.name,
          description: `${where} · ${item.name}${flags ? ` · ${flags}` : ""}`,
          disabled: !item.enabled,
        };
      }),
    [connections],
  );

  /** `/` 命令面板的数据：只列技能（权限/模型/清空等动作不再混入，各由自己的胶囊负责）。 */
  const paletteAll = useMemo<PaletteItem[]>(
    () =>
      BUILTIN_SKILLS.map((skill) => ({
        id: `skill:${skill.id}`,
        kind: "skill",
        title: `插入技能：${skill.name}`,
        description: skill.whenToUse[0] ?? skill.description,
        keywords: [skill.name, skill.id],
      })),
    [],
  );

  const closePalette = (refocus: boolean) => {
    setPaletteOpen(false);
    setPaletteToken(null);
    if (refocus) textareaRef.current?.focus();
  };

  /** 输入变化时重算 `/` token：只有命中命令/技能才展开，`/home` 之类不误触。 */
  const updatePalette = (value: string, caret: number) => {
    const token = slashToken(value, caret);
    if (!token || !shouldOpenPalette(paletteAll, token.query)) {
      setPaletteOpen(false);
      setPaletteToken(null);
      return;
    }
    setPaletteToken(token);
    setPaletteItems(filterPaletteItems(paletteAll, token.query));
    setPaletteIndex(0);
    setPaletteOpen(true);
  };

  /** 选中技能项：把技能正文注入输入框（面板只剩技能，故只处理 skill）。 */
  const pickPaletteItem = (item: PaletteItem) => {
    const token = paletteToken;
    const base = token ? text.slice(0, token.start) + text.slice(token.end) : text;
    const raw = item.id.slice(item.id.indexOf(":") + 1);
    closePalette(true);
    const skill = BUILTIN_SKILLS.find((entry) => entry.id === raw);
    if (skill) applySkillAt(skill, base);
    else setText(base);
  };

  // 点击「输入区 + `/` 面板」之外的任何地方都收起面板（与其它选择器一致）。
  useEffect(() => {
    if (!paletteOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      const root = inputAreaRef.current;
      if (root && event.target instanceof Node && root.contains(event.target)) return;
      setPaletteOpen(false);
      setPaletteToken(null);
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  }, [paletteOpen]);

  /** WP5-4：发送前把「注定失败」的两种情况直接说清楚，并禁用发送。
   *
   * 修复前这些情况只在点击后由后端返回错误码，用户看不到原因，还会丢掉输入内容。
   * 两种：没绑定服务、绑定的服务本身不可用（停用/缺密钥/地址非法）。
   *
   * 2026-11-01 去掉了第三种「联网开关关着」：外发不再有用户开关，绑定可用即就绪。
   */
  const sendBlockReason = !bindingId
    ? "还没有绑定 Agent 服务。请到「设置 → AI 服务」绑定一个云模型或本机 Ollama 后再发送。"
    : boundConnection && serviceBlocked(boundConnection)
      ? blockedReason(boundConnection)
      : null;
  const sendDisabled =
    pendingRun || disabled || text.trim().length === 0 || tooLong || sendBlockReason !== null;

  const submit = async () => {
    const trimmed = text.trim();
    if (!trimmed || pendingRun || disabled) return;
    if (tooLong) {
      setError(`消息太长（${bytes}/${MAX_PROMPT_BYTES} 字节），请缩短后再发送。`);
      return;
    }
    setError(null);
    try {
      await onSend(trimmed);
      // 发送成功即清空该会话草稿：草稿只承载「还没发出去的内容」。
      persisted.current = "";
      setText("");
      // 技能只服务于「本轮」：发送成功后清掉已选技能与注入片段，避免胶囊残留到下一轮。
      appliedSkillText.current = "";
      setSelectedSkill(null);
      await saveDraft(conversationRef, "assistant", { text: "" }).catch((cause: unknown) => {
        logger.warn("清空助手草稿失败", {
          conversationRef,
          error: cause instanceof Error ? cause.message : String(cause),
        });
      });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  /** 换档失败要就地说明原因（例如回答还在进行），不能静默无效。 */
  const changePermission = async (next: AgentPermission) => {
    try {
      setError(null);
      await setPermission(next);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  /** 「插入技能」：把技能正文作为操作参考追加到本次输入。
   *  只进输入框，不进 system，也不携带任何执行/批准语义——真正的执行仍要准备计划后再确认。 */
  const applySkillAt = (skill: SkillDefinition, base: string) => {
    const snippet = injectionText(skill);
    appliedSkillText.current = snippet;
    setText(base.trim().length > 0 ? `${base}\n\n${snippet}` : snippet);
    setSelectedSkill(skill);
    setError(null);
  };

  /** 移除「已选技能」胶囊：回收它注入的片段，保留用户自己写的内容。 */
  const removeSkill = () => {
    const snippet = appliedSkillText.current;
    if (snippet) {
      setText((current) => {
        if (!current.includes(snippet)) return current;
        return current.replace(snippet, "").replace(/\n{3,}/g, "\n\n").trimEnd();
      });
    }
    appliedSkillText.current = "";
    setSelectedSkill(null);
  };

  /** 外发范围只在需要时通过信息按钮说明，不再常驻一大段文字。 */
  const outboundSummary = boundConnection
    ? `${outbound}${attachments.length > 0 ? ` 本次对话关联 ${attachments.length} 项素材。` : ""}` +
      " 本次只发送你输入的文字和必要的素材名称，不发送图片内容本身。"
    : "";

  return (
    <div className="border-t border-[var(--color-border)] bg-[var(--color-bg)] px-4 pb-2 pt-3">
      <div className="mx-auto flex max-w-[760px] flex-col">
        <div className={clsx("composer-card", compact && "composer-card--compact")}>
          {/* 服务不可用/未绑定时如实说明，并保留输入内容；
              不静默换到别的服务或改走本机（R34）。 */}
          {sendBlockReason && (
            <p role="alert" className="mb-1 text-[11px] leading-5 text-[var(--color-danger)]">
              {sendBlockReason}
            </p>
          )}

          {selectedSkill && (
            <ul className="mb-1.5 flex flex-wrap gap-1.5" aria-label="已选技能">
              <li
                className="flex items-center gap-1 rounded-full border border-[var(--color-status)] px-2 py-0.5 text-[11px] text-[var(--color-status)]"
                data-testid="selected-skill-chip"
              >
                <BookOpen size={11} aria-hidden="true" />
                <span>{selectedSkill.name}</span>
                <button
                  type="button"
                  aria-label={`移除技能 ${selectedSkill.name}`}
                  title="移除已选技能"
                  className="ml-0.5 rounded-full px-1 leading-none transition-colors hover:bg-[var(--color-surface)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
                  onClick={removeSkill}
                >
                  ×
                </button>
              </li>
            </ul>
          )}

          {attachments.length > 0 && (
            <ul className="mb-1.5 flex flex-wrap gap-1.5" aria-label="已关联素材">
              {attachments.map((item) => (
                <li
                  key={item.attachmentRef}
                  className={clsx(
                    "rounded-full border px-2 py-0.5 text-[11px]",
                    item.available
                      ? "border-[var(--color-border)] text-[var(--color-text-secondary)]"
                      : "border-[var(--color-status)] text-[var(--color-status)]",
                  )}
                  title={item.available ? item.displayName : `${item.displayName}（文件缺失或未核验）`}
                >
                  {item.displayName}
                  {!item.available && "（缺失）"}
                </li>
              ))}
            </ul>
          )}
          {unavailableAttachments.length > 0 && (
            <p className="mb-1 text-[11px] text-[var(--color-status)]">
              {unavailableAttachments.length} 个关联素材已缺失或未核验，助手无法直接使用；请重新选择素材。
            </p>
          )}

          {/* 相对定位容器：让 `/` 命令面板（PickerPopover 是 position:absolute）
              的包含块就是「输入框自身」，从而 `bottom:100%+6px` 能紧贴输入框上方，
              不会再向上冒泡到全高祖先、被顶到窗口顶部。 */}
          <div className="relative" ref={inputAreaRef}>
            <textarea
              ref={textareaRef}
            aria-label="输入消息"
            className="ui-control composer-input w-full resize-none text-sm leading-6"
            placeholder={
              streaming
                ? "正在回答…"
                : pendingRun
                  ? "上一轮结果未确认；可先停止回答，确认后再发送"
                  : "描述要处理的素材与目标，例如：把刚导入的照片移入回收站（输入 / 召唤技能）"
            }
            value={text}
            disabled={disabled}
            onChange={(event) => {
              const value = event.currentTarget.value;
              setText(value);
              updatePalette(value, event.currentTarget.selectionStart ?? value.length);
            }}
            onKeyDown={(event) => {
              // 中文输入法组合期间的回车只是确认候选词，不能当发送。
              if (event.nativeEvent.isComposing) return;
              if (paletteOpen) {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  setPaletteIndex((i) => (paletteItems.length ? Math.min(i + 1, paletteItems.length - 1) : 0));
                  return;
                }
                if (event.key === "ArrowUp") {
                  event.preventDefault();
                  setPaletteIndex((i) => Math.max(i - 1, 0));
                  return;
                }
                if (event.key === "Enter") {
                  event.preventDefault();
                  const item = paletteItems[paletteIndex];
                  if (item && !item.disabled) pickPaletteItem(item);
                  return;
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  closePalette(true);
                  return;
                }
                if (event.key === "Tab") {
                  event.preventDefault();
                  closePalette(false);
                  return;
                }
              }
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void submit();
              }
            }}
          />

          <CommandPalette
            open={paletteOpen}
            items={paletteItems}
            activeIndex={paletteIndex}
            anchorRef={textareaRef}
            onPick={pickPaletteItem}
            onHover={setPaletteIndex}
          />
          </div>

          {/* R34：失败后由用户选「重试」或「切换」，不由应用暗中换供应商。 */}
          {error && (
            <div className="mt-1 flex flex-wrap items-center gap-2 text-[11px] text-[var(--color-danger)]">
              <p role="alert" className="min-w-0 flex-1 break-words">{error}</p>
              <Button
                disabled={sendDisabled}
                onClick={() => void submit()}
              >
                重试发送
              </Button>
              {connections.length > 0 && (
                <Button disabled={switching || pendingRun} onClick={focusServicePicker}>
                  切换服务
                </Button>
              )}
            </div>
          )}

          {/* 工具行：权限与模型是同形胶囊，右侧只留发送这一处主位。 */}
          <div className="mt-1.5 flex min-w-0 items-center gap-1.5">
            <PermissionPicker
              value={permission}
              busy={pendingRun || streaming}
              onSelect={(next) => void changePermission(next)}
            />
            {/* 当前模型是次要信息：收起时只显示模型名，需要时才展开选择。
                「本机 / 在线」直接写在描述里，不再另起一行复述。 */}
            {connections.length > 0 ? (
              <ServicePicker
                ref={servicePickerRef}
                options={serviceOptions}
                bindingId={bindingId}
                model={model}
                switching={switching}
                pendingRun={pendingRun}
                onSelect={(id) => void switchService(id)}
              />
            ) : (
              <span
                className="min-w-0 truncate text-[11px] text-[var(--color-text-tertiary)]"
                title={model.detail}
              >
                {model.detail}
              </span>
            )}
            <span className="sr-only" aria-live="polite" data-testid="agent-model-label">
              {model.name ? `模型：${model.name}` : model.detail}
            </span>
            {/* 外发范围必须能被看见和聚焦：保留一个可聚焦的小图标按钮，
                不用「鼠标悬停才命中」的裸 title（可访问性与 A3-06 的披露要求）。 */}
            {boundConnection && (
              <button
                type="button"
                data-testid="agent-outbound-summary"
                aria-label={outboundSummary}
                title={outboundSummary}
                className="flex size-5 shrink-0 items-center justify-center rounded-full text-[var(--color-text-tertiary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text-secondary)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
              >
                <Info size={12} aria-hidden="true" />
                <span className="sr-only">{outboundSummary}</span>
              </button>
            )}
            {nearLimit && (
              <span className="shrink-0 text-[11px] text-[var(--color-status)]" role="status">
                接近长度上限
              </span>
            )}
            <div className="ml-auto flex shrink-0 items-center gap-1.5">
              {pendingRun && (
                <button
                  type="button"
                  disabled={stopping}
                  onClick={() => void onStop()}
                  className="flex items-center gap-1 rounded-[var(--radius-item)] px-2 py-1 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-40"
                >
                  <Square size={11} aria-hidden="true" />
                  {stopping ? "正在停止…" : "停止回答"}
                </button>
              )}
              <button
                type="button"
                aria-label="发送"
                title="发送（Enter）"
                disabled={sendDisabled}
                onClick={() => void submit()}
                className="flex size-8 items-center justify-center rounded-full bg-[var(--color-accent)] text-[var(--color-accent-text)] transition-opacity hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)] disabled:opacity-30"
              >
                <ArrowUp size={16} aria-hidden="true" />
              </button>
            </div>
          </div>
        </div>
        {!compact && <ComposerMetrics attachmentCount={attachments.length} usage={usage} />}
      </div>
    </div>
  );
}
