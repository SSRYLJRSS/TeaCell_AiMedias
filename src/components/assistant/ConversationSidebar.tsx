/** 助手页左侧历史列表（A3-04 / WF-1 P1 前端 IA）。
 *
 * 约束：
 * - 会话按稳定 conversationRef 分桶，绝不依赖「最近一个」全局 ID；
 * - 搜索、改名、归档/恢复、删除都经 conversationStore（后端命令是唯一事实源）；
 * - 删除前先看活动任务：后端在存在未完成 operation 时返回 RESOURCE_CONFLICT，
 *   界面必须把原因说清楚并指向「任务卡」，而不是假装删除成功；
 * - 置顶与时间分组（今天/昨天/最近 7 天/更早）是纯前端过渡：先落 localStorage，
 *   待后端会话索引增加 pinned/workspace 字段后再迁移（提案 D6）。
 */
import { useEffect, useMemo, useState, type ReactNode } from "react";
import clsx from "clsx";
import { Archive, ArchiveRestore, Pencil, Pin, PinOff, Plus, Trash2 } from "lucide-react";
import Button from "@/components/common/Button";
import SearchInput from "@/components/common/SearchInput";
import Modal from "@/components/common/Modal";
import { AppError } from "@/api/client";
import { useConversationStore } from "@/stores/conversationStore";
import { useAgentSessionStore, runtimeOf } from "@/stores/agentSessionStore";
import type { ConversationSummary } from "@/api/conversations";

const PINS_KEY = "assistant.conversationPins";
const SEARCH_DEBOUNCE_MS = 200;

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function loadPins(): string[] {
  try {
    const raw = window.localStorage.getItem(PINS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : null;
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function startOfDay(ts: number): number {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export default function ConversationSidebar() {
  const conversations = useConversationStore((s) => s.conversations);
  const activeRef = useConversationStore((s) => s.activeRef);
  const loading = useConversationStore((s) => s.loading);
  const refresh = useConversationStore((s) => s.refresh);
  const create = useConversationStore((s) => s.create);
  const select = useConversationStore((s) => s.select);
  const rename = useConversationStore((s) => s.rename);
  const setArchived = useConversationStore((s) => s.setArchived);
  const remove = useConversationStore((s) => s.remove);
  const runtimes = useAgentSessionStore((s) => s.runtimes);
  const openRuntime = useAgentSessionStore((s) => s.open);
  const closeRuntime = useAgentSessionStore((s) => s.close);

  const [includeArchived, setIncludeArchived] = useState(false);
  const [queryInput, setQueryInput] = useState("");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<{ ref: string; title: string } | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [pins, setPins] = useState<string[]>(() => loadPins());
  const [archivedOpen, setArchivedOpen] = useState(false);

  // 搜索防抖：输入变化 200ms 后才触发一次后端查询，避免逐键请求。
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(queryInput.trim()), SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [queryInput]);

  useEffect(() => {
    void refresh(includeArchived, query || undefined);
  }, [refresh, includeArchived, query]);

  useEffect(() => {
    try {
      window.localStorage.setItem(PINS_KEY, JSON.stringify(pins));
    } catch {
      /* 存储不可用（隐私模式/配额满）时忽略，置顶仅本次会话生效 */
    }
  }, [pins]);

  const togglePin = (conversationRef: string) => {
    setPins((current) =>
      current.includes(conversationRef)
        ? current.filter((x) => x !== conversationRef)
        : [...current, conversationRef],
    );
  };

  const rows = useMemo(
    () => conversations.filter((item) => includeArchived || item.status === "active"),
    [conversations, includeArchived],
  );

  const groups = useMemo(() => {
    const pinned: ConversationSummary[] = [];
    const buckets: { key: string; label: string; items: ConversationSummary[] }[] = [
      { key: "today", label: "今天", items: [] },
      { key: "yesterday", label: "昨天", items: [] },
      { key: "week", label: "最近 7 天", items: [] },
      { key: "earlier", label: "更早", items: [] },
      { key: "archived", label: "已归档", items: [] },
    ];
    const today = startOfDay(Date.now());
    for (const item of rows) {
      if (pins.includes(item.conversationRef)) {
        pinned.push(item);
        continue;
      }
      if (item.status === "archived") {
        buckets[4].items.push(item);
        continue;
      }
      const days = Math.floor((today - startOfDay(item.updatedAtUnixSeconds * 1000)) / 86_400_000);
      if (days <= 0) buckets[0].items.push(item);
      else if (days === 1) buckets[1].items.push(item);
      else if (days < 7) buckets[2].items.push(item);
      else buckets[3].items.push(item);
    }
    return { pinned, buckets: buckets.filter((b) => b.items.length > 0) };
  }, [rows, pins]);

  const choose = async (conversationRef: string) => {
    setError(null);
    setBusy(true);
    try {
      await select(conversationRef);
      await openRuntime(conversationRef);
    } catch (cause) {
      setError(messageOf(cause));
    } finally {
      setBusy(false);
    }
  };

  const createNew = async () => {
    setError(null);
    setBusy(true);
    try {
      const ref = await create("新对话");
      await select(ref);
      // 新会话必须显式 `new`：空白会话不允许继承任何其他会话的消息与附件。
      await openRuntime(ref, "new");
      void useAgentSessionStore.getState().subscribe();
    } catch (cause) {
      setError(messageOf(cause));
    } finally {
      setBusy(false);
    }
  };

  const submitRename = async () => {
    if (!renaming) return;
    const title = renaming.title.trim();
    if (!title) return;
    try {
      await rename(renaming.ref, title);
      setRenaming(null);
    } catch (cause) {
      setError(messageOf(cause));
    }
  };

  const toggleArchived = async (conversationRef: string, archived: boolean) => {
    try {
      await setArchived(conversationRef, archived);
      if (archived && activeRef === conversationRef) await closeRuntime(conversationRef);
    } catch (cause) {
      setError(messageOf(cause));
    }
  };

  const confirmDelete = async () => {
    if (!deleting) return;
    const target = deleting;
    setDeleting(null);
    try {
      // 后端在存在未完成 operation 时会拒绝；这里也先停掉本地流，避免残留回答。
      const runtime = runtimeOf(runtimes, target);
      if (runtime.activeRunId) await useAgentSessionStore.getState().stop(target);
      await closeRuntime(target);
      await remove(target);
    } catch (cause) {
      if (cause instanceof AppError && cause.code === "RESOURCE_CONFLICT") {
        setError("该对话还有未完成的任务，请先在任务卡中处理或取消后再删除。");
      } else {
        setError(messageOf(cause));
      }
    }
  };

  const renderRow = (item: ConversationSummary) => {
    const runtime = runtimeOf(runtimes, item.conversationRef);
    const active = item.conversationRef === activeRef;
    const pinned = pins.includes(item.conversationRef);
    return (
      <li key={item.conversationRef}>
        <div
          className={clsx(
            "group flex items-center gap-1 rounded-[var(--radius-item)] px-2 py-1.5",
            active ? "bg-[var(--color-surface-raised)]" : "hover:bg-[var(--color-surface-hover)]",
          )}
        >
          <button
            type="button"
            onClick={() => void choose(item.conversationRef)}
            className="min-w-0 flex-1 text-left"
            title={item.title}
          >
            <span className="flex items-center gap-1.5">
              {pinned && <Pin size={10} aria-hidden="true" className="shrink-0 text-[var(--color-status)]" />}
              <span
                className={clsx(
                  "truncate text-sm",
                  active ? "font-medium text-[var(--color-text)]" : "text-[var(--color-text)]",
                )}
              >
                {item.title}
              </span>
              {runtime.status === "streaming" && (
                <span className="shrink-0 text-[10px] text-[var(--color-status)]">回答中</span>
              )}
              {item.status === "archived" && (
                <span className="shrink-0 text-[10px] text-[var(--color-text-tertiary)]">已归档</span>
              )}
            </span>
          </button>
          <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
            <IconAction label={pinned ? `取消置顶 ${item.title}` : `置顶 ${item.title}`} onClick={() => togglePin(item.conversationRef)}>
              {pinned ? <PinOff size={12} aria-hidden="true" /> : <Pin size={12} aria-hidden="true" />}
            </IconAction>
            <IconAction
              label={`重命名 ${item.title}`}
              onClick={() => setRenaming({ ref: item.conversationRef, title: item.title })}
            >
              <Pencil size={12} aria-hidden="true" />
            </IconAction>
            <IconAction
              label={`${item.status === "archived" ? "恢复" : "归档"} ${item.title}`}
              onClick={() => void toggleArchived(item.conversationRef, item.status !== "archived")}
            >
              {item.status === "archived" ? (
                <ArchiveRestore size={12} aria-hidden="true" />
              ) : (
                <Archive size={12} aria-hidden="true" />
              )}
            </IconAction>
            <IconAction label={`删除 ${item.title}`} onClick={() => setDeleting(item.conversationRef)}>
              <Trash2 size={12} aria-hidden="true" />
            </IconAction>
          </div>
        </div>
      </li>
    );
  };

  const renderGroup = (label: string, items: ConversationSummary[], collapsible = false) => {
    if (items.length === 0) return null;
    const open = !collapsible || archivedOpen;
    return (
      <section key={label} className="mb-1">
        {collapsible ? (
          <button
            type="button"
            className="flex w-full items-center gap-1 px-2 py-1 text-[10px] font-semibold tracking-[0.08em] text-[var(--color-text-tertiary)] uppercase hover:text-[var(--color-text-secondary)]"
            aria-expanded={archivedOpen}
            onClick={() => setArchivedOpen((v) => !v)}
          >
            {label} ({items.length})
          </button>
        ) : (
          <p className="px-2 py-1 text-[10px] font-semibold tracking-[0.08em] text-[var(--color-text-tertiary)] uppercase">
            {label}
          </p>
        )}
        {open && <ul className="flex flex-col">{items.map(renderRow)}</ul>}
      </section>
    );
  };

  return (
    <aside
      aria-label="对话历史"
      className="flex w-[248px] shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-surface)]"
    >
      <div className="flex flex-col gap-2 p-3">
        <Button variant="primary" className="gap-1.5" disabled={busy} onClick={() => void createNew()}>
          <Plus size={14} aria-hidden="true" />
          新建对话
        </Button>
        <SearchInput className="w-full" placeholder="搜索对话" onSearch={setQueryInput} />
        <label className="flex items-center gap-1.5 text-[11px] text-[var(--color-text-secondary)]">
          <input
            type="checkbox"
            checked={includeArchived}
            onChange={(event) => setIncludeArchived(event.currentTarget.checked)}
          />
          显示已归档
        </label>
        {error && (
          <p role="alert" className="rounded-[var(--radius-item)] bg-[var(--color-status-soft)] px-2 py-1 text-[11px] text-[var(--color-status)]">
            {error}
          </p>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {loading && rows.length === 0 && (
          <p className="px-2 py-3 text-xs text-[var(--color-text-tertiary)]">正在加载…</p>
        )}
        {!loading && rows.length === 0 && (
          <p className="px-2 py-3 text-xs text-[var(--color-text-tertiary)]">还没有对话，点击「新建对话」开始。</p>
        )}
        {groups.pinned.length > 0 && renderGroup("置顶", groups.pinned)}
        {groups.buckets.map((bucket) => renderGroup(bucket.label, bucket.items, bucket.key === "archived"))}
      </div>

      <Modal
        open={renaming !== null}
        title="重命名对话"
        onClose={() => setRenaming(null)}
        footer={
          <>
            <Button onClick={() => setRenaming(null)}>取消</Button>
            <Button variant="primary" onClick={() => void submitRename()}>
              保存
            </Button>
          </>
        }
      >
        <input
          aria-label="对话标题"
          className="ui-control w-full px-2 py-1.5 text-sm"
          maxLength={80}
          value={renaming?.title ?? ""}
          onChange={(event) =>
            setRenaming((current) => (current ? { ...current, title: event.currentTarget.value } : current))
          }
        />
      </Modal>

      <Modal
        open={deleting !== null}
        title="删除对话"
        onClose={() => setDeleting(null)}
        footer={
          <>
            <Button onClick={() => setDeleting(null)}>取消</Button>
            <Button variant="danger" onClick={() => void confirmDelete()}>
              删除
            </Button>
          </>
        }
      >
        <p className="text-sm text-[var(--color-text-secondary)]">
          删除后该对话不再出现在列表中。已完成的任务记录与素材不受影响；若仍有未完成任务，需要先处理或取消。
        </p>
      </Modal>
    </aside>
  );
}

function IconAction({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className="grid size-6 place-items-center rounded-[var(--radius-item)] text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]"
    >
      {children}
    </button>
  );
}
