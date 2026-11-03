import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import clsx from "clsx";
import { Maximize2, SquarePen, X } from "lucide-react";
import Composer from "@/components/assistant/Composer";
import MessageList from "@/components/assistant/MessageList";
import { capabilityLabel } from "@/components/assistant/AgentCards";
import { runtimeOf, useAgentSessionStore } from "@/stores/agentSessionStore";
import { useAssistantFocusStore } from "@/stores/assistantFocusStore";
import { useConversationStore } from "@/stores/conversationStore";
import { logger } from "@/utils/logger";

/**
 * A3-09（PRD 第 8 节）：应用内悬浮助手。
 *
 * 边界：
 * - 只是应用内的一层浮层，不创建系统桌面常驻窗，也不做形象编辑器。
 * - 聊天数据只有一份（`conversationStore` 的会话 + `agentSessionStore` 的运行时），
 *   这里既不复制消息，也不另建一套订阅。
 * - 计划在浮层里只显示摘要与「查看并确认」，审批动作一律回完整助手页执行；
 *   浮层不提供「批准」「全部确认」「清空」这类会改变业务状态的按钮。
 * - 关掉入口（设置里关开关或点收起）只影响显示，不取消任何已入队的任务。
 */

const LOGO_SIZE = 48;
const EDGE_MARGIN = 16;
const PANEL_GAP = 12;
const PANEL_MAX_WIDTH = 380;
const PANEL_MAX_HEIGHT = 520;
const STORAGE_KEY = "chabaosucai.floating-assistant.v2";
/**
 * v1 存的是「绝对像素坐标」，换窗口尺寸或改 DPI 之后既不是贴底也不是用户放的地方，
 * 于是长期浮在页面中部压住左栏内容（M-11 真机第 4 次复现：入库页「入库进度」标签被盖住）。
 * v2 起改为记录「距可落位区域底边的距离」，坐标随视口重新换算；旧键直接丢弃，不再读取。
 */
const LEGACY_STORAGE_KEY = "chabaosucai.floating-assistant.v1";
/**
 * 底栏是 `inset-x-0 bottom-0 h-14` 的固定层（BottomBar §导航），所以「贴底」不能贴到
 * y = 窗口高度，否则整个入口会被导航条盖住。真机 960×640 下曾经出现「百宝箱」被入口
 * 压住（M-11 要求底栏五入口与中央 Logo 不重叠），故把可落位区域的底边抬到导航条之上。
 */
const BOTTOM_BAR_SELECTOR = '[data-bottom-bar]';

/** 浮层头部的图标按钮：浮层很窄，动作只留图标，文字走 aria-label/title。 */
const FLOAT_ICON_BUTTON =
  "flex size-7 items-center justify-center rounded-[var(--radius-item)] text-[var(--color-text-tertiary)] " +
  "transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]";

interface Position {
  x: number;
  y: number;
}

function viewport(): { width: number; height: number } {
  if (typeof window === "undefined") return { width: 1280, height: 800 };
  return { width: window.innerWidth, height: window.innerHeight };
}

/** 可落位区域的高度：从导航条上边缘往上算，拿不到导航条时退回整个窗口。 */
function usableHeight(height: number): number {
  if (typeof document === "undefined") return height;
  const bar = document.querySelector<HTMLElement>(BOTTOM_BAR_SELECTOR);
  if (!bar) return height;
  const top = bar.getBoundingClientRect().top;
  // 导航条尚未布局（高度为 0）时不能把整块可视区吃掉。
  return top > 0 && top < height ? top : height;
}

/**
 * 收起态默认停在左下（底栏之上）。
 *
 * 真机 M-04 复现：入口停在右下中部时，圆心正好压在打标页右栏「确认写入」上——用户在
 * 最后一步点确认，点到的是入口。收起态只是入口，没有必须占右侧的理由，所以默认左侧；
 * 用户主动拖到右边仍然允许（那是明确意图），只是不再默认占着右下角的主操作区。
 */
function defaultPositionFor(height: number): Position {
  // 重新计算默认位置时也要受窗口范围约束：窗口缩到比「贴底 + 边距」还小的时候，
  // 原始算式会算出越界值（240 高的窗口得到 184 > 176），入口就露到窗口外（A3-09）。
  // 保留「距底栏 8px」的首选间距，只在放不下时退回 clamp 的上界。
  const preferred = usableHeight(height) - LOGO_SIZE - 8;
  const limit = Math.max(EDGE_MARGIN, height - LOGO_SIZE - EDGE_MARGIN);
  return { x: EDGE_MARGIN, y: Math.max(EDGE_MARGIN, Math.min(preferred, limit)) };
}

function defaultPosition(): Position {
  return defaultPositionFor(viewport().height);
}

/** 位置必须始终落在窗口内且不压住底栏：缩窗后不能把按钮留在屏幕外（A3-09 验收）。 */
function clampPosition(p: Position, width: number, height: number): Position {
  const maxX = Math.max(EDGE_MARGIN, width - LOGO_SIZE - EDGE_MARGIN);
  const maxY = Math.max(EDGE_MARGIN, usableHeight(height) - LOGO_SIZE - EDGE_MARGIN);
  return {
    x: Math.min(Math.max(EDGE_MARGIN, p.x), maxX),
    y: Math.min(Math.max(EDGE_MARGIN, p.y), maxY),
  };
}

/** 松手后吸附到最近的水平边缘（左/右），垂直位置保持。 */
function snapPosition(p: Position, width: number, height: number): Position {
  const clamped = clampPosition(p, width, height);
  const toLeft = clamped.x;
  const toRight = width - LOGO_SIZE - clamped.x;
  return {
    x: toLeft <= toRight ? EDGE_MARGIN : Math.max(EDGE_MARGIN, width - LOGO_SIZE - EDGE_MARGIN),
    y: clamped.y,
  };
}

function readPosition(): Position {
  if (typeof window === "undefined") return defaultPosition();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      // 旧版本只存过绝对坐标，语义已经失效：清掉，避免它在换尺寸后继续压住内容。
      window.localStorage.removeItem(LEGACY_STORAGE_KEY);
      return defaultPosition();
    }
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return defaultPosition();
    const { x, bottomGap } = parsed as { x?: unknown; bottomGap?: unknown };
    if (typeof x !== "number" || typeof bottomGap !== "number") return defaultPosition();
    // 记住的是「离可落位区域底边多远」，所以换窗口尺寸不会让入口浮到页面中部。
    const y = usableHeight(window.innerHeight) - LOGO_SIZE - bottomGap;
    return clampPosition({ x, y }, window.innerWidth, window.innerHeight);
  } catch {
    // 本地记录损坏只影响位置记忆，不能影响助手本身是否可用。
    return defaultPosition();
  }
}

function writePosition(p: Position) {
  try {
    const bottomGap = usableHeight(window.innerHeight) - (p.y + LOGO_SIZE);
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({ x: p.x, bottomGap }));
  } catch {
    /* 无本地存储时仅失去位置记忆 */
  }
}

/**
 * 是否已有「用户主动摆放」的位置。
 *
 * M-11 真机（2026-10-27，960×640 与 1200×800 两档）复现：入口被停在上一次的绝对坐标上。
 * 默认位置是按当时视口算出来的像素值，视口一变它就既不是「贴底」也不是「用户放的地方」，
 * 而是浮在页面中部压住左栏的批次行——缩窗再放大的走查路径上必然出现。
 * 所以只有用户真的拖过（有本地记录）才沿用绝对坐标，否则每次视口变化都重新贴左下。
 */
function hasStoredPosition(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(STORAGE_KEY) !== null;
  } catch {
    return false;
  }
}

function panelRect(anchor: Position, width: number, height: number) {
  // 尺寸必须真的塞进窗口：旧实现用 Math.max(260, width-32) 取「最小宽度」，
  // 窗口比 292px 还窄时面板会宽出窗口（Math.min 之后仍然大于可用宽度），
  // 卡片要求「尺寸/位置受窗口范围约束」。这里改成先把可用空间算出来再取小。
  const usableWidth = Math.max(120, width - EDGE_MARGIN * 2);
  const usableHeight = Math.max(120, height - EDGE_MARGIN * 2);
  const panelWidth = Math.min(PANEL_MAX_WIDTH, usableWidth);
  const panelHeight = Math.min(PANEL_MAX_HEIGHT, usableHeight);
  const onRight = anchor.x + LOGO_SIZE / 2 > width / 2;
  const rawX = onRight ? anchor.x - panelWidth - PANEL_GAP : anchor.x + LOGO_SIZE + PANEL_GAP;
  const maxX = Math.max(EDGE_MARGIN, width - panelWidth - EDGE_MARGIN);
  const x = Math.min(Math.max(EDGE_MARGIN, rawX), maxX);
  const rawY = anchor.y - panelHeight + LOGO_SIZE;
  const maxY = Math.max(EDGE_MARGIN, height - panelHeight - EDGE_MARGIN);
  const y = Math.min(Math.max(EDGE_MARGIN, rawY), maxY);
  return { x, y, width: panelWidth, height: panelHeight };
}

export default function FloatingAssistant() {
  const [position, setPosition] = useState<Position>(readPosition);
  const [size, setSize] = useState(viewport);
  const [open, setOpen] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const logoRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const dragOffset = useRef({ dx: 0, dy: 0 });
  const moved = useRef(false);

  const activeRef = useConversationStore((s) => s.activeRef);
  const active = useConversationStore((s) => s.active);
  const select = useConversationStore((s) => s.select);
  const create = useConversationStore((s) => s.create);
  const refresh = useConversationStore((s) => s.refresh);

  const host = useAgentSessionStore((s) => s.host);
  const runtimes = useAgentSessionStore((s) => s.runtimes);
  const plans = useAgentSessionStore((s) => s.plans);
  const linkedPlans = useAgentSessionStore((s) => s.linkedPlans);
  const subscribe = useAgentSessionStore((s) => s.subscribe);
  const openConversation = useAgentSessionStore((s) => s.open);
  const send = useAgentSessionStore((s) => s.send);
  const stop = useAgentSessionStore((s) => s.stop);
  const recoverHost = useAgentSessionStore((s) => s.recoverHost);

  const runtime = activeRef ? runtimeOf(runtimes, activeRef) : null;

  // 与完整助手页共享同一份订阅：subscribe 内部已做幂等守卫，浮层不新建第二套。
  useEffect(() => {
    void subscribe().catch((cause: unknown) => {
      logger.warn("悬浮助手事件订阅失败", {
        error: cause instanceof Error ? cause.message : String(cause),
      });
    });
  }, [subscribe]);

  useEffect(() => {
    const onResize = () => {
      const next = viewport();
      setSize(next);
      // 有底部锚定记录时按「距可落位区域底边多远」换算回新视口，摆过的位置仍贴在同一条边上；
      // 没被用户摆放过就一律重新贴左下——否则绝对坐标会和视口脱钩，放大窗口后入口
      // 停在页面中部压住左栏内容（M-11 真机第 4/5 次复现）。
      setPosition(() => (hasStoredPosition() ? readPosition() : defaultPositionFor(next.height)));
    };
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // 打开后把焦点移进浮层；关闭后回到 Logo，保证键盘可达。
  // 只在「展开状态真的变了」时移动焦点：首次挂载时 open 就是 false，若无条件
  // 执行就会在页面刚渲染时把焦点从搜索框等位置抢到悬浮 Logo 上（每次切回茶馆
  // 页面都会重挂载，等于反复抢焦点）。
  const openedOnce = useRef(false);
  useEffect(() => {
    if (!openedOnce.current) {
      openedOnce.current = true;
      return;
    }
    if (open) {
      panelRef.current?.focus();
    } else {
      logoRef.current?.focus({ preventScroll: true });
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      // 浮层是 aria-modal="false" 的非模态浮层：真正打开着模态弹窗时，Esc 属于
      // 那一层（Modal 走 window 捕获阶段监听，同一节点上的多个监听都会执行），
      // 不能连带把浮层也收起。
      if (document.querySelector('[aria-modal="true"]')) return;
      event.stopPropagation();
      setOpen(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  const startDrag = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      if (event.button !== 0) return;
      moved.current = false;
      dragOffset.current = { dx: event.clientX - position.x, dy: event.clientY - position.y };
      setDragging(true);
      event.currentTarget.setPointerCapture?.(event.pointerId);
    },
    [position.x, position.y],
  );

  const onPointerMove = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      if (!dragging) return;
      const next = clampPosition(
        { x: event.clientX - dragOffset.current.dx, y: event.clientY - dragOffset.current.dy },
        size.width,
        size.height,
      );
      if (Math.abs(next.x - position.x) > 2 || Math.abs(next.y - position.y) > 2) moved.current = true;
      setPosition(next);
    },
    [dragging, position.x, position.y, size.height, size.width],
  );

  const endDrag = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      if (!dragging) return;
      setDragging(false);
      event.currentTarget.releasePointerCapture?.(event.pointerId);
      const snapped = snapPosition(position, size.width, size.height);
      setPosition(snapped);
      writePosition(snapped);
    },
    [dragging, position, size.height, size.width],
  );

  const continueLatest = useCallback(async () => {
    setNotice(null);
    try {
      await refresh();
      const store = useConversationStore.getState();
      const target =
        store.activeRef ?? store.conversations.find((item) => item.status === "active")?.conversationRef;
      if (!target) {
        setNotice("还没有对话，点右上角新建。");
        return;
      }
      if (store.activeRef !== target) await select(target);
      await openConversation(target);
    } catch (cause) {
      setNotice(cause instanceof Error ? cause.message : String(cause));
      logger.warn("悬浮助手打开最近对话失败", {
        error: cause instanceof Error ? cause.message : String(cause),
      });
    }
  }, [openConversation, refresh, select]);

  const toggle = useCallback(() => {
    setOpen((current) => {
      const next = !current;
      if (next) void continueLatest();
      return next;
    });
  }, [continueLatest]);

  /** 中断 → recover=true；启动失败 → 普通重试。两者都不能让用户去重启应用。 */
  const recoverHostAction = useCallback(async () => {
    setNotice(null);
    try {
      await recoverHost();
    } catch (cause) {
      setNotice(cause instanceof Error ? cause.message : String(cause));
    }
  }, [recoverHost]);

  const hostBlocked =
    host.state === "starting" ||
    host.state === "error" ||
    host.state === "interrupted" ||
    host.state === "unavailable";

  const startNew = useCallback(async () => {
    setNotice(null);
    try {
      const ref = await create("新对话");
      await select(ref);
      await openConversation(ref, "new");
    } catch (cause) {
      setNotice(cause instanceof Error ? cause.message : String(cause));
    }
  }, [create, openConversation, select]);

  // 浮层只给摘要；审批必须回到完整助手页。
  // 只列**待确认**的计划：`linkedPlans` 里还留着已批准/已过期/已消费的计划用于
  // 历史卡片，但这些计划的「查看并确认」在浮层里等于让用户去找一个已经不需要
  // 确认的东西，且与「待确认计划」这个标签自相矛盾。
  const visiblePlans = useMemo(() => {
    if (!activeRef) return [];
    return (linkedPlans[activeRef] ?? [])
      .map((ref) => plans[ref])
      .filter((plan) => plan !== undefined && plan.reviewStatus === "pending");
  }, [activeRef, linkedPlans, plans]);

  const rect = panelRect(position, size.width, size.height);
  const streaming = runtime?.status === "streaming";
  /** 结果未确认也算「本轮未结束」：可以精确取消，但不能接着发。 */
  const pendingRun = streaming || runtime?.status === "unknown";

  const focusPlan = useAssistantFocusStore((s) => s.focusPlan);

  const openFullPage = useCallback(
    (planId?: string) => {
      if (activeRef) void openConversation(activeRef);
      // 目标页在同一次导航里才挂载，事件监听来不及接收，所以把「要定位哪张卡」
      // 落成待消费状态，由助手页挂载后读取（A3-09「定位完整助手页同一计划」）。
      if (planId) focusPlan(planId);
      window.dispatchEvent(new CustomEvent("app:navigate", { detail: "assistant" }));
    },
    [activeRef, focusPlan, openConversation],
  );

  return (
    <>
      {open && (
        <div
          ref={panelRef}
          role="dialog"
          aria-modal="false"
          aria-label="悬浮助手"
          tabIndex={-1}
          data-testid="floating-assistant-panel"
          className="fixed z-40 flex flex-col overflow-hidden rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-bg)] shadow-lg outline-none"
          style={{ left: rect.x, top: rect.y, width: rect.width, height: rect.height }}
        >
          <header className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-2">
            <span
              aria-hidden="true"
              className={clsx(
                "size-1.5 shrink-0 rounded-full",
                streaming || pendingRun ? "bg-[var(--color-status)]" : "bg-[var(--color-text-tertiary)]",
              )}
            />
            <h2 className="min-w-0 truncate text-sm font-medium text-[var(--color-text)]">
              {active?.title ?? "助手"}
            </h2>
            <p className="shrink-0 text-[11px] text-[var(--color-text-tertiary)]">
              {streaming ? "正在回答" : pendingRun ? "结果待核对" : "就绪"}
            </p>
            <div className="ml-auto flex shrink-0 items-center gap-0.5">
              <button
                type="button"
                aria-label="新建对话"
                title="新建对话"
                className={FLOAT_ICON_BUTTON}
                onClick={() => void startNew()}
              >
                <SquarePen size={14} aria-hidden="true" />
              </button>
              <button
                type="button"
                aria-label="完整页面"
                title="在完整助手页打开"
                className={FLOAT_ICON_BUTTON}
                onClick={() => openFullPage()}
              >
                <Maximize2 size={14} aria-hidden="true" />
              </button>
              <button
                type="button"
                aria-label="收起悬浮助手"
                title="收起"
                className={FLOAT_ICON_BUTTON}
                onClick={() => setOpen(false)}
              >
                <X size={14} aria-hidden="true" />
              </button>
            </div>
          </header>

          {(notice ||
            host.state === "error" ||
            host.state === "interrupted" ||
            host.state === "unavailable" ||
            runtime?.error) && (
            <div
              role="alert"
              className="flex flex-wrap items-center gap-2 border-b border-[var(--color-border)] px-3 py-1.5 text-[11px] text-[var(--color-danger)]"
            >
              <span className="min-w-0 flex-1 break-words">
                {notice ??
                  runtime?.error ??
                  host.error ??
                  (host.state === "interrupted" ? "Agent 服务上次异常中断" : "Agent 服务未就绪")}
              </span>
              {host.state === "interrupted" && (
                <button
                  type="button"
                  className="ui-control shrink-0 rounded-md px-2 py-0.5 text-[11px]"
                  onClick={() => void recoverHostAction()}
                >
                  恢复服务
                </button>
              )}
              {host.state === "error" && (
                <button
                  type="button"
                  className="ui-control shrink-0 rounded-md px-2 py-0.5 text-[11px]"
                  onClick={() => void recoverHostAction()}
                >
                  重试
                </button>
              )}
            </div>
          )}

          {visiblePlans.length > 0 && (
            <section
              aria-label="待确认计划"
              className="max-h-[35%] overflow-y-auto border-b border-[var(--color-border)] px-3 py-2"
            >
              {visiblePlans.map((plan) => (
                <div key={plan.planId} className="flex flex-col gap-1 py-1" data-float-plan-id={plan.planId}>
                  <p className="text-[11px] leading-5 text-[var(--color-text-secondary)]">
                    {capabilityLabel(plan.capability)} · {plan.itemCount} 项 · {plan.summary}
                  </p>
                  {/* 浮层不提供批准入口：复杂计划必须在完整助手页核对后再确认。 */}
                  <button
                    type="button"
                    className="ui-control self-start rounded-md px-2 py-0.5 text-[11px] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
                    onClick={() => openFullPage(plan.planId)}
                  >
                    查看并确认
                  </button>
                </div>
              ))}
            </section>
          )}

          {activeRef && runtime ? (
            <>
              <MessageList
                messages={runtime.messages}
                emptyHint={"直接说要做什么；计划确认在完整助手页。"}
              />
              <Composer
                conversationRef={activeRef}
                attachments={active?.attachments ?? []}
                streaming={runtime.status === "streaming"}
                stoppable={runtime.status === "streaming" || runtime.status === "unknown"}
                stopping={runtime.stoppingRunId !== null}
                disabled={hostBlocked}
                /* 浮层高度只有 520px：同一张卡片去掉统计行、收窄内边距。 */
                compact
                onSend={async (text) => {
                  await send(activeRef, text);
                }}
                onStop={() => stop(activeRef)}
              />
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center px-4 text-center text-xs leading-5 text-[var(--color-text-tertiary)]">
              {notice ? "点右上角新建开始对话。" : "正在打开最近的对话…"}
            </div>
          )}
        </div>
      )}

      <button
        ref={logoRef}
        type="button"
        aria-label="打开助手浮窗"
        aria-expanded={open}
        data-testid="floating-assistant-logo"
        onPointerDown={startDrag}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onClick={() => {
          if (moved.current) {
            moved.current = false;
            return;
          }
          toggle();
        }}
        className={
          "fixed z-40 flex items-center justify-center rounded-full border border-[var(--color-border)] " +
          "bg-[var(--color-surface)] text-sm font-medium text-[var(--color-text)] shadow-lg " +
          "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)] " +
          (dragging ? "cursor-grabbing" : "cursor-grab")
        }
        style={{ left: position.x, top: position.y, width: LOGO_SIZE, height: LOGO_SIZE }}
        title="助手：拖动可吸附边缘"
      >
        茶
      </button>
    </>
  );
}
