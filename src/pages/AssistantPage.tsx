/** 助手页（v2 A3-04）：左侧历史 + 右侧对话/计划卡/任务卡。
 *
 * 设计边界（plans/v2/execution/A3.md A3-04、架构 §9.15）：
 * - 会话索引/标题/草稿/附件/链接全部走 conversationStore（A1-06 已交付）；
 * - 消息只来自当前进程的 Host 事件流，重启后为空；恢复会话时只加载
 *   「记录 + 真实快照」，未完成业务效果先显示恢复摘要，用户明确继续才续跑；
 * - 计划卡/任务卡一律回读 PlanPreview 与 OperationSnapshot，绝不把模型文本当收据；
 * - 「停止回答」（取消 run）与「取消任务」（取消 operation）是两个独立动作；
 * - 新建会话必须是空白，不继承任何其他会话的消息或附件。
 */
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import clsx from "clsx";
import { BookOpen, ClipboardCheck, PanelLeft, PanelRight, Workflow } from "lucide-react";
import Button from "@/components/common/Button";
import PageErrorBoundary from "@/components/common/PageErrorBoundary";
import AnomalyNotice from "@/components/assistant/AnomalyNotice";
import AssistantWorkbench, { type AssistantWorkbenchTab } from "@/components/assistant/AssistantWorkbench";
import AssistantSidePanel, { type AssistantSidePanelTab } from "@/components/assistant/AssistantSidePanel";
import PendingPlansPanel from "@/components/plans/PendingPlansPanel";
import WorkflowPanel from "@/components/workflows/WorkflowPanel";
import SkillLibraryPanel from "@/components/skills/SkillLibraryPanel";
import SkillDistillPipeline from "@/components/skills/SkillDistillPipeline";
import { injectionText } from "@/data/skills/schema";
import type { SkillDefinition } from "@/data/skills/types";
import Composer from "@/components/assistant/Composer";
import ConversationSidebar from "@/components/assistant/ConversationSidebar";
import AssistantSkeleton from "@/components/assistant/AssistantSkeleton";
import MessageList from "@/components/assistant/MessageList";
import { OperationCard, PlanCard, SelectionCard } from "@/components/assistant/AgentCards";
import { listOperationRecovery, resumeOperation, type RecoverySummary } from "@/api/operations";
import { useAgentSessionStore, runtimeOf } from "@/stores/agentSessionStore";
import { useAssistantFocusStore } from "@/stores/assistantFocusStore";
import { useConversationStore } from "@/stores/conversationStore";
import { markStartup } from "@/utils/startupMarks";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import { useComposerInjectStore } from "@/stores/composerInjectStore";
import { NARROW_ASSISTANT_QUERY, useMediaQuery } from "@/hooks/useMediaQuery";
import { logger } from "@/utils/logger";

const EMPTY_HINT = "描述要处理的素材和目标。助手会先给出计划，你确认后才会执行。";

/** 会话头部的小图标动作：文字提示走 title/aria-label，给对话内容留出宽度。 */
function HeaderAction({
  label,
  active,
  expanded,
  onClick,
  children,
}: {
  label: string;
  active?: boolean;
  /** 用于控制抽屉类面板的开合状态；普通动作不传。 */
  expanded?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-expanded={expanded}
      aria-pressed={expanded == null ? active : undefined}
      onClick={onClick}
      className={clsx(
        "flex size-8 shrink-0 items-center justify-center rounded-[var(--radius-item)] transition-colors",
        active
          ? "bg-[var(--color-surface)] text-[var(--color-text)]"
          : "text-[var(--color-text-tertiary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]",
      )}
    >
      {children}
    </button>
  );
}

export default function AssistantPage() {
  const activeRef = useConversationStore((s) => s.activeRef);
  const active = useConversationStore((s) => s.active);
  const createConversation = useConversationStore((s) => s.create);
  const selectConversation = useConversationStore((s) => s.select);
  const conversationsLoading = useConversationStore((s) => s.loading);
  const runtimes = useAgentSessionStore((s) => s.runtimes);
  const host = useAgentSessionStore((s) => s.host);
  const plans = useAgentSessionStore((s) => s.plans);
  const operations = useAgentSessionStore((s) => s.operations);
  const linkedPlans = useAgentSessionStore((s) => s.linkedPlans);
  const linkedOperations = useAgentSessionStore((s) => s.linkedOperations);
  const linkedSelections = useAgentSessionStore((s) => s.linkedSelections);
  const subscribe = useAgentSessionStore((s) => s.subscribe);
  const ensureHost = useAgentSessionStore((s) => s.ensureHost);
  const recoverHost = useAgentSessionStore((s) => s.recoverHost);
  const send = useAgentSessionStore((s) => s.send);
  const stop = useAgentSessionStore((s) => s.stop);
  const openConversation = useAgentSessionStore((s) => s.open);
  const hydrate = useAgentSessionStore((s) => s.hydrate);
  const workflowPanelRequested = useWorkflowPanelStore((s) => s.open);
  const closeWorkflowPanel = useWorkflowPanelStore((s) => s.closePanel);
  const pendingPlansRequested = usePendingPlansStore((s) => s.open);
  const closePendingPlans = usePendingPlansStore((s) => s.closePanel);

  const [recovery, setRecovery] = useState<RecoverySummary[]>([]);
  const [busyRecovery, setBusyRecovery] = useState<string | null>(null);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
  const [hostError, setHostError] = useState<string | null>(null);
  /** 首帧到 Host 就绪之间为 true：空态改为骨架，避免先闪一帧「选择左侧对话」文案。 */
  const [hostStarting, setHostStarting] = useState(true);
  // A3-07/A4-07/A5-02：右侧只有一个抽屉，头部三个图标只是把同一个抽屉切到不同选项卡
  // （Codex 式右栏）。默认不占宽度；加宽时让出左侧历史，但聊天区始终保留。
  const narrow = useMediaQuery(NARROW_ASSISTANT_QUERY);
  const [panelTab, setPanelTab] = useState<AssistantSidePanelTab | null>(null);
  const [panelWide, setPanelWide] = useState(false);
  /** 技能库选项卡内部两态：浏览库 / 沉淀向导。切走或收起时复位回浏览。 */
  const [skillView, setSkillView] = useState<"library" | "distill">("library");
  const [workbenchTab, setWorkbenchTab] = useState<AssistantWorkbenchTab>("import");
  /** 外部入口（任务卡「保存为常用流程」、其他页面的待审计划）每次请求都让内嵌面板
   *  重新挂载：选项卡已经在它上面时不会重新读 store 里的新草稿/新列表，
   *  重挂载才会按最新状态进入。 */
  const [panelRequestKey, setPanelRequestKey] = useState(0);
  const [historyOpen, setHistoryOpen] = useState(false);
  /** 「最近恢复」只跑一次；用 ref 而不是 activeRef 做取消条件，见下方注释。 */
  const restoredRef = useRef(false);
  // 窄窗下历史改为浮层，避免「侧栏 + 历史 + 聊天」三列把内容挤到不可用宽度。
  const showHistory = narrow ? historyOpen : !(panelTab !== null && panelWide);

  const runtime = activeRef ? runtimeOf(runtimes, activeRef) : null;

  /** 最后一条用户消息：只用于把原问题放回输入框供用户确认（不自动重发）。 */
  const lastUserText = useMemo(() => {
    if (!runtime) return "";
    for (let i = runtime.messages.length - 1; i >= 0; i -= 1) {
      if (runtime.messages[i].role === "user") return runtime.messages[i].text;
    }
    return "";
  }, [runtime]);

  // WF-1 P0-5：区分「宿主中断」与「用户主动停止」。
  // host.status 帧会把流式消息置为 aborted 并写 runtime.error；用户点停止只置 aborted、无 error。
  // 因此：有 error 且有 aborted 助手消息 = 被中断，给「继续」；最后一条 failed = 「重试」。
  const interrupted =
    Boolean(runtime?.error) &&
    Boolean(runtime?.messages.some((m) => m.role === "assistant" && m.status === "aborted"));

  /** 重试/继续不直接重发：把文本注入输入框，让用户在发送前有机会改目标或补充上下文。 */
  const retryLastTurn = () => {
    if (!activeRef) return;
    useComposerInjectStore.getState().inject(activeRef, lastUserText);
  };
  const continueLastTurn = () => {
    if (!activeRef) return;
    useComposerInjectStore.getState().inject(activeRef, "请继续未完成的部分。");
  };

  /** 收起整个抽屉，并复位两个面板 store：外部入口会把 store.open 置真，
   *  只改本地 panelTab 的话，下次进页面（或切到别的页面）会再弹一次同一份内容。 */
  const closeSidePanel = () => {
    setPanelTab(null);
    setSkillView("library");
    closeWorkflowPanel();
    closePendingPlans();
  };

  /** 头部图标：点当前选项卡就收起，点别的就换选项卡——同一个抽屉，不换容器。 */
  const toggleSidePanel = (next: AssistantSidePanelTab) => {
    if (panelTab === next) {
      closeSidePanel();
      return;
    }
    // 每次都从技能库首页进入，避免上次停在沉淀向导里。
    setSkillView("library");
    setPanelTab(next);
  };

  /** 技能库「在对话中使用」：把技能正文作为本次操作参考注入当前会话输入框，并收起抽屉。
   *  只注入参考资料，不生成计划、不写批准/收据语义。 */
  const useSkill = (skill: SkillDefinition) => {
    if (!activeRef) return;
    useComposerInjectStore.getState().inject(activeRef, injectionText(skill));
    closeSidePanel();
  };

  // 外部入口（任务卡上的「保存为常用流程」、设置页/MCP 面板里的待审计划）在助手页
  // 要落到同一个抽屉的对应选项卡上；App 层在助手页不再渲染这两个模态框。
  useEffect(() => {
    if (!workflowPanelRequested) return;
    setPanelTab("workflows");
    setPanelRequestKey((key) => key + 1);
  }, [workflowPanelRequested]);

  useEffect(() => {
    if (!pendingPlansRequested) return;
    setPanelTab("plans");
    setPanelRequestKey((key) => key + 1);
  }, [pendingPlansRequested]);

  useEffect(() => {
    // 事件订阅失败不阻断页面；store 里的 subscription 为空时会自动重试。
    void subscribe().catch((error: unknown) => {
      logger.warn("助手事件订阅失败，稍后重试", {
        error: error instanceof Error ? error.message : String(error),
      });
    });
    return () => {
      // 不在这里 unsubscribe：多个页面/浮层共享同一订阅（A3-09）。
    };
  }, [subscribe]);

  // WP1.4：Host 启动放到后台，首帧立即渲染骨架/外壳，不再让启动结果 gate 首屏。
  // 启动期间空态改为骨架（见下方四态），避免先闪一帧误导性的「选择左侧对话」文案。
  useEffect(() => {
    let cancelled = false;
    setHostStarting(true);
    void (async () => {
      try {
        await ensureHost();
        if (cancelled) return;
        setHostError(null);
      } catch (error) {
        if (cancelled) return;
        setHostError(error instanceof Error ? error.message : String(error));
      } finally {
        if (!cancelled) setHostStarting(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [ensureHost]);

  // WP3-B：助手页启动细粒度埋点（生产构建默认不打印，仅记录到数组，供 before/after 对比）。
  // 只在首次达成时打一次，避免 Host/列表状态抖动重复打点。
  const listReadyRef = useRef(false);
  const sawListLoadingRef = useRef(false);
  const canInputRef = useRef(false);
  useEffect(() => {
    markStartup("assistant_skeleton_visible");
  }, []);
  useEffect(() => {
    if (host.state === "ready") markStartup("assistant_host_ready");
  }, [host.state]);
  useEffect(() => {
    if (listReadyRef.current) return;
    if (conversationsLoading) {
      sawListLoadingRef.current = true;
      return;
    }
    // 必须观测到「真正加载过一次」再置就绪，避免初始 loading=false 就打点。
    if (!sawListLoadingRef.current) return;
    listReadyRef.current = true;
    markStartup("assistant_list_ready");
  }, [conversationsLoading]);
  useEffect(() => {
    if (canInputRef.current) return;
    if (activeRef && runtimeOf(runtimes, activeRef).status === "ready") {
      canInputRef.current = true;
      markStartup("assistant_can_input");
    }
  }, [activeRef, runtimes]);

  useEffect(() => {
    if (!activeRef) return;
    void hydrate(activeRef);
  }, [activeRef, hydrate]);

  useEffect(() => {
    // A3-04「最近恢复」：进入助手页时若没有活动会话，就恢复到最近一个活动会话，
    // 而不是让用户每次都手动点一次。全程只复用 conversationStore 的索引
    // （标题/状态/最近时间都来自后端），一个会话都没有时保持空白提示，
    // 绝不凭空造一个会话或伪造历史消息。
    //
    // 只尝试一次：`select()` 会把 activeRef 设上，若用 activeRef 做取消条件，
    // 这次恢复会被自己的状态更新打断（open 永远不会被调用）。
    if (activeRef || restoredRef.current) return;
    restoredRef.current = true;
    void (async () => {
      try {
        const store = useConversationStore.getState();
        if (store.conversations.length === 0) await store.refresh();
        const now = useConversationStore.getState();
        const target =
          now.activeRef ??
          now.conversations.find((item) => item.status === "active")?.conversationRef;
        if (!target) return;
        // 列表加载与进入会话拆开：select（读会话视图）与 open（起 Host / 开会话）并行，
        // 不再串成 refresh→select→open 一条阻塞链；open 内部对 ensureHost 去重。
        const selecting = now.activeRef !== target ? now.select(target) : Promise.resolve();
        await Promise.all([selecting, openConversation(target)]);
      } catch (error) {
        setHostError(error instanceof Error ? error.message : String(error));
      }
    })();
  }, [activeRef, openConversation]);

  useEffect(() => {
    // 恢复摘要只读真实后端口径，不猜测哪个任务「大概能续跑」。
    void (async () => {
      try {
        setRecovery(await listOperationRecovery());
      } catch (error) {
        logger.warn("读取恢复摘要失败", {
          error: error instanceof Error ? error.message : String(error),
        });
        setRecovery([]);
      }
    })();
  }, [activeRef, operations]);

  const visiblePlans = useMemo(() => {
    if (!activeRef) return [];
    return (linkedPlans[activeRef] ?? []).map((ref) => plans[ref]).filter((plan) => plan !== undefined);
  }, [activeRef, linkedPlans, plans]);

  const visibleOperations = useMemo(() => {
    if (!activeRef) return [];
    return (linkedOperations[activeRef] ?? [])
      .map((ref) => operations[ref])
      .filter((snapshot) => snapshot !== undefined);
  }, [activeRef, linkedOperations, operations]);

  /** A3-08：交接给这份会话的素材集合快照（只读，重开会话仍在）。 */
  const visibleSelections = useMemo(() => {
    if (!activeRef) return [];
    return linkedSelections[activeRef] ?? [];
  }, [activeRef, linkedSelections]);

  // A3-09：浮层里的「查看并确认」要定位到**同一张计划卡**。目标由浮层写入，
  // 这里读取后滚到那张卡并高亮；读取即消费（避免下次进页面又凭空高亮），
  // 高亮本身单独保留几秒，否则清掉目标后这一帧就消失了。
  const focusPlanId = useAssistantFocusStore((s) => s.planId);
  const clearFocusPlan = useAssistantFocusStore((s) => s.clear);
  const planListRef = useRef<HTMLDivElement>(null);
  const focusAttempts = useRef(0);
  const [highlightedPlanId, setHighlightedPlanId] = useState<string | null>(null);

  useEffect(() => {
    if (!focusPlanId) {
      focusAttempts.current = 0;
      return;
    }
    const node = planListRef.current?.querySelector<HTMLElement>(`[data-plan-id="${focusPlanId}"]`);
    if (node) {
      node.scrollIntoView?.({ block: "nearest" });
      setHighlightedPlanId(focusPlanId);
      clearFocusPlan();
      focusAttempts.current = 0;
      return;
    }
    // 计划卡可能还没渲染（归属/计划仍在回读），所以没找到时先不清除目标；
    // 连续多次仍找不到说明这张计划确实不在当前会话，放弃定位而不是无限重试。
    focusAttempts.current += 1;
    if (focusAttempts.current >= 3) {
      clearFocusPlan();
      focusAttempts.current = 0;
    }
  }, [focusPlanId, clearFocusPlan, visiblePlans]);

  useEffect(() => {
    if (!highlightedPlanId) return;
    const timer = setTimeout(() => setHighlightedPlanId(null), 4000);
    return () => clearTimeout(timer);
  }, [highlightedPlanId]);

  const resume = async (operationRef: string) => {
    setBusyRecovery(operationRef);
    setRecoveryError(null);
    try {
      await resumeOperation(operationRef);
      await hydrate(activeRef ?? "");
      setRecovery(await listOperationRecovery());
    } catch (error) {
      setRecoveryError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusyRecovery(null);
    }
  };

  const startNew = async () => {
    setHostError(null);
    try {
      await ensureHost(false);
    } catch (error) {
      setHostError(error instanceof Error ? error.message : String(error));
    }
  };

  /** 空态主 CTA：建一个空白会话并进入（与侧栏「新建对话」同语义，显式按 new 打开）。 */
  const createNewSession = async () => {
    setHostError(null);
    try {
      const ref = await createConversation("新对话");
      await selectConversation(ref);
      await openConversation(ref, "new");
    } catch (error) {
      setHostError(error instanceof Error ? error.message : String(error));
    }
  };

  /** WP5-3：Host 上次被强杀后的唯一恢复入口（recover=true 是 Rust 侧的显式确认）。 */
  const recover = async () => {
    setHostError(null);
    try {
      await recoverHost();
    } catch (error) {
      setHostError(error instanceof Error ? error.message : String(error));
    }
  };

  /** Host 不在 ready 时发送必然失败；这里区分「正在起来」和「起不来」。 */
  const hostBlocked =
    host.state === "starting" ||
    host.state === "error" ||
    host.state === "interrupted" ||
    host.state === "unavailable";

  // 启动中：Host 仍在起来（含首帧），空态先给骨架，不落入误导性的默认文案。
  const isStarting = hostStarting || host.state === "starting";

  if (!activeRef || !runtime) {
    return (
      <div className="flex h-full">
        <ConversationSidebar />
        {isStarting ? (
          <AssistantSkeleton compact label="Agent 服务正在启动…" />
        ) : (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
            <p className="max-w-[420px] text-sm leading-6 text-[var(--color-text-secondary)]">
              {host.state === "interrupted"
                ? "Agent 服务上次异常中断。核对会话与任务后，点「恢复 Agent 服务」继续。"
                : host.state === "unavailable"
                  ? "这个平台或安装包不提供 Agent 能力；其余功能不受影响。"
                  : host.state === "error"
                    ? "Agent 服务启动失败。可以重试；仍失败请查看诊断日志。"
                    : "选择左侧对话，或新建一个对话开始。"}
            </p>
            {hostError && (
              <p role="alert" className="max-w-[480px] text-xs text-[var(--color-danger)]">
                {hostError}
              </p>
            )}
            {host.state === "interrupted" ? (
              <Button variant="primary" onClick={() => void recover()}>
                恢复 Agent 服务
              </Button>
            ) : host.state === "error" || hostError ? (
              <Button variant="primary" onClick={() => void startNew()}>
                重试
              </Button>
            ) : host.state === "unavailable" ? null : (
              <Button
                variant="primary"
                data-testid="empty-new-conversation"
                onClick={() => void createNewSession()}
              >
                新建对话
              </Button>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="relative flex h-full">
      {showHistory &&
        (narrow ? (
          // 窄窗：历史做成浮层，选中会话或点遮罩即收起，聊天区与工作台始终可用。
          <>
            <button
              type="button"
              aria-label="关闭对话历史"
              className="absolute inset-0 z-20 bg-black/20"
              onClick={() => setHistoryOpen(false)}
            />
            <div className="absolute inset-y-0 left-0 z-30 flex shadow-lg" onClick={() => setHistoryOpen(false)}>
              <ConversationSidebar />
            </div>
          </>
        ) : (
          <ConversationSidebar />
        ))}
      <PageErrorBoundary onReset={() => void openConversation(activeRef)} onBack={() => void openConversation(activeRef)}>
        <div className="flex min-w-0 flex-1 flex-col">
          <header className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-2">
            {narrow && (
              <HeaderAction
                label={historyOpen ? "收起对话历史" : "打开对话历史"}
                expanded={historyOpen}
                onClick={() => setHistoryOpen((open) => !open)}
              >
                <PanelLeft size={15} aria-hidden="true" />
              </HeaderAction>
            )}
            <div className="min-w-0 flex-1">
              <h2 className="truncate text-sm font-medium text-[var(--color-text)]">{active?.title ?? "对话"}</h2>
              <p className="truncate text-[11px] text-[var(--color-text-tertiary)]">
                {runtime.status === "streaming"
                  ? "正在回答"
                  : runtime.status === "unknown"
                    ? "上一轮发送结果未知，请核对后再决定"
                    : "就绪"}
              </p>
            </div>
            {/* WP5-3：状态要能直接指出下一步动作，而不是一律「未就绪」。 */}
            {host.state === "interrupted" ? (
              <span className="flex shrink-0 items-center gap-1.5">
                <span
                  className="rounded-full border border-[var(--color-danger)] px-2 py-0.5 text-[10px] text-[var(--color-danger)]"
                  title={host.error ?? "Agent 服务上次被异常结束"}
                >
                  Agent 已中断
                </span>
                <Button onClick={() => void recover()}>恢复服务</Button>
              </span>
            ) : host.state === "unavailable" ? (
              <span
                className="shrink-0 rounded-full border border-[var(--color-danger)] px-2 py-0.5 text-[10px] text-[var(--color-danger)]"
                title={host.error ?? "Agent 服务在此平台不可用"}
              >
                Agent 不可用
              </span>
            ) : host.state === "error" ? (
              <span className="flex shrink-0 items-center gap-1.5">
                <span
                  className="rounded-full border border-[var(--color-danger)] px-2 py-0.5 text-[10px] text-[var(--color-danger)]"
                  title={host.error ?? "Agent 服务启动失败"}
                >
                  Agent 启动失败
                </span>
                <Button onClick={() => void startNew()}>重试</Button>
              </span>
            ) : host.state === "starting" ? (
              <span className="shrink-0 rounded-full border border-[var(--color-status)] px-2 py-0.5 text-[10px] text-[var(--color-status)]">
                正在启动…
              </span>
            ) : host.state !== "ready" ? (
              <span
                className="shrink-0 rounded-full border border-[var(--color-status)] px-2 py-0.5 text-[10px] text-[var(--color-status)]"
                title="Agent Host 未启动"
              >
                Agent 未启动
              </span>
            ) : null}
            <div className="flex shrink-0 items-center gap-0.5">
              {/* 三个图标打开的是同一个右侧栏，只是切到不同选项卡；再点同一个就收起。 */}
              <HeaderAction
                label="常用流程"
                active={panelTab === "workflows"}
                onClick={() => toggleSidePanel("workflows")}
              >
                <Workflow size={15} aria-hidden="true" />
              </HeaderAction>
              {/* 技能库：内置/自建技能手册的浏览、管理、以及「插入到本会话」。 */}
              <HeaderAction
                label="技能库"
                active={panelTab === "skills"}
                onClick={() => toggleSidePanel("skills")}
              >
                <BookOpen size={15} aria-hidden="true" />
              </HeaderAction>
              {/* A5-02：外部客户端准备的计划不属于任何会话，必须有一个不依赖会话的入口。 */}
              <HeaderAction
                label="待审计划"
                active={panelTab === "plans"}
                onClick={() => toggleSidePanel("plans")}
              >
                <ClipboardCheck size={15} aria-hidden="true" />
              </HeaderAction>
              <HeaderAction
                label="工作台"
                active={panelTab === "workbench"}
                onClick={() => toggleSidePanel("workbench")}
              >
                <PanelRight size={15} aria-hidden="true" />
              </HeaderAction>
            </div>
          </header>

          <AnomalyNotice conversationRef={activeRef} />

          {hostError && (
            <p
              role="alert"
              className="border-b border-[var(--color-border)] bg-[var(--color-surface)] px-4 py-2 text-xs text-[var(--color-danger)]"
            >
              {hostError}
            </p>
          )}

          {recovery.length > 0 && (
            <div className="border-b border-[var(--color-border)] bg-[var(--color-surface)] px-4 py-2">
              <p className="text-[11px] text-[var(--color-text-secondary)]">
                有 {recovery.length} 个任务需要恢复确认。继续前请确认目录授权仍然有效。
              </p>
              <ul className="mt-1 flex flex-col gap-1">
                {recovery.map((item) => (
                  <li key={item.operationRef} className="flex items-center gap-2 text-[11px]">
                    <span className="text-[var(--color-text-secondary)]">
                      待执行 {item.pendingSteps} · 执行中 {item.runningSteps} · 待核对 {item.unknownItems}
                    </span>
                    {item.requiresResume && (
                      <Button
                        className="min-h-0 px-2 py-0.5 text-[11px]"
                        disabled={busyRecovery !== null}
                        onClick={() => void resume(item.operationRef)}
                      >
                        {busyRecovery === item.operationRef ? "正在继续…" : "继续任务"}
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
              {recoveryError && (
                <p role="alert" className="mt-1 text-[11px] text-[var(--color-danger)]">
                  {recoveryError}
                </p>
              )}
            </div>
          )}

          <MessageList
            messages={runtime.messages}
            interrupted={interrupted}
            onRetry={retryLastTurn}
            onContinue={continueLastTurn}
            emptyHint={
              runtime.error
                ? `${EMPTY_HINT}\n\n上一轮提示：${runtime.error}`
                : EMPTY_HINT
            }
          />

          {(visiblePlans.length > 0 || visibleOperations.length > 0 || visibleSelections.length > 0) && (
            <div className="max-h-[45%] overflow-y-auto border-t border-[var(--color-border)] px-4 py-3" ref={planListRef}>
              <div className="mx-auto flex max-w-[760px] flex-col gap-2">
                {visibleSelections.map((selectionRef) => (
                  <SelectionCard key={selectionRef} selectionRef={selectionRef} conversationRef={activeRef ?? undefined} />
                ))}
                {visiblePlans.map((plan) => (
                  <PlanCard key={plan.planId} plan={plan} highlight={plan.planId === highlightedPlanId} />
                ))}
                {visibleOperations.map((snapshot) => (
                  <OperationCard key={snapshot.operationId} snapshot={snapshot} />
                ))}
              </div>
            </div>
          )}

          <Composer
            conversationRef={activeRef}
            attachments={active?.attachments ?? []}
            streaming={runtime.status === "streaming"}
            stoppable={runtime.status === "streaming" || runtime.status === "unknown"}
            stopping={runtime.stoppingRunId !== null}
            disabled={hostBlocked}
            onSend={async (text) => {
              await send(activeRef, text);
            }}
            onStop={() => stop(activeRef)}
          />
        </div>
      </PageErrorBoundary>
      {panelTab && (
        <AssistantSidePanel
          tab={panelTab}
          wide={panelWide}
          onTabChange={setPanelTab}
          onToggleWide={() => setPanelWide((current) => !current)}
          onClose={closeSidePanel}
        >
          {panelTab === "workflows" && (
            <WorkflowPanel
              embedded
              key={`workflows-${panelRequestKey}`}
              // 「再次使用」把提示放回输入框后要关掉整个抽屉，而不是只关面板
              onClose={closeSidePanel}
            />
          )}
          {panelTab === "skills" &&
            (skillView === "distill" ? (
              <SkillDistillPipeline
                key={`skills-distill-${panelRequestKey}`}
                onBack={() => setSkillView("library")}
                // 沉淀只生成本地草稿；入库审阅是后续动作，这里先回到技能库。
                onPublish={() => setSkillView("library")}
              />
            ) : (
              <SkillLibraryPanel
                embedded
                key={`skills-${panelRequestKey}`}
                onUse={useSkill}
                onOpenDistill={() => setSkillView("distill")}
                onClose={closeSidePanel}
              />
            ))}
          {panelTab === "plans" && (
            <PendingPlansPanel embedded key={`plans-${panelRequestKey}`} />
          )}
          {panelTab === "workbench" && (
            // 工作台自带 <aside> 宽度与左右边框；外面这层只负责把它的高度撑满，
            // 让内部的 min-h-0/flex-1 滚动区在抽屉里同样生效。
            <div className="flex min-h-0 flex-1">
              <AssistantWorkbench
                conversationRef={activeRef}
                tab={workbenchTab}
                wide={panelWide}
                selections={visibleSelections}
                onTabChange={setWorkbenchTab}
                onToggleWide={() => setPanelWide((current) => !current)}
                onClose={closeSidePanel}
              />
            </div>
          )}
        </AssistantSidePanel>
      )}
    </div>
  );
}
