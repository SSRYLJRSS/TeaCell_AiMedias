/** A3-08：交接确认。
 *
 *  交接必须**先让用户看清交的是什么**：条数、是否截断、以及截断时只包含已加载的
 *  那一部分。这个弹窗不做任何隐式扩展 —— 它不会「顺便带上全库」，也不会把当前页
 *  之外的 id 补进来（那些 id 前端根本没有）。
 *
 *  真正的集合固定在 Rust 侧（`snapshot_selection`）：这里传的是用户此刻选中的 id
 *  列表与当时的匹配总数，后端据此记录 truncated。之后素材库里的选择再怎么变，
 *  这份快照都不会被改写。 */
import { useState } from "react";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import { snapshotSelection } from "@/api/selections";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useConversationStore } from "@/stores/conversationStore";
import { logger } from "@/utils/logger";

interface Props {
  open: boolean;
  onClose: () => void;
  /** 交接成功后由页面决定停在原处还是跳转。参数一次带全，
   *  避免页面再自己拼一份 query/条数（那样会和后端快照对不上）。 */
  onHandedOff: (result: HandoffResult) => void;
}

/** 交接结果：会话引用 + 快照引用 + 后端确认的快照口径。 */
export interface HandoffResult {
  conversationRef: string;
  selectionRef: string;
  /** 结果视图标题使用的固定描述，来自后端已持久化的 query */
  query: string;
  /** 后端确认的快照项数（截断时等于当时匹配总数） */
  snapshot: number;
}

/** 用当时条件给快照起个可读的名字，供结果视图标题与助手卡片展示。 */
function describeQuery(search: string): string {
  const trimmed = search.trim();
  return trimmed ? `搜索「${trimmed}」的结果` : "素材库中的选择";
}

export default function HandoffDialog({ open, onClose, onHandedOff }: Props) {
  const selected = useSelectionStore((s) => s.selected);
  const truncated = useSelectionStore((s) => s.truncated);
  const selectionTotal = useSelectionStore((s) => s.selectionTotal);
  const search = useLibraryStore((s) => s.filter.search);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const count = selected.size;
  const total = truncated ? selectionTotal : count;

  const confirm = async () => {
    if (count === 0 || busy) return;
    setBusy(true);
    setError(null);
    try {
      const query = describeQuery(search);
      const view = await snapshotSelection(query, Array.from(selected), total);
      const store = useConversationStore.getState();
      // 有活动会话就交给它，否则新建一个：交接必须落到一个确定的会话上，
      // 否则助手面板无法「按当前会话」展示这份集合。
      // A3-08：归档会话不算「可用于交接的会话」——它已从默认列表里隐去，
      // 把快照挂上去等于交接进了一个用户看不见的地方。此时新建一个。
      const archivedActive =
        store.active?.status === "archived" ||
        store.conversations.find((item) => item.conversationRef === store.activeRef)?.status ===
          "archived";
      const conversationRef =
        store.activeRef && !archivedActive ? store.activeRef : await store.create("素材库交接");
      await store.link(conversationRef, "selection", view.selectionRef);
      await store.select(conversationRef);
      // 标题与来源一律用后端确认的口径：query 是后端行里存下来的那一份，
      // 前端不再自己拼第二份；条数用真正进了快照的项数（= 提交的 id 数），
      // 而不是截断前的匹配总数 —— 否则会写成「交接的 9000 项」却只有 200 项。
      onHandedOff({
        conversationRef,
        selectionRef: view.selectionRef,
        query,
        snapshot: count,
      });
    } catch (cause) {
      logger.error("交接素材集合失败", {
        error: cause instanceof Error ? cause.message : String(cause),
      });
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="交给助手"
      onClose={() => {
        if (busy) return;
        setError(null);
        onClose();
      }}
      footer={
        <>
          <Button disabled={busy} onClick={() => { setError(null); onClose(); }}>
            取消
          </Button>
          <Button variant="primary" disabled={busy || count === 0} onClick={() => void confirm()}>
            {busy ? "正在固定集合…" : "交给助手"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3 text-sm text-[var(--color-text-secondary)]">
        <p>
          将把 <span className="font-medium text-[var(--color-text)]">{count}</span> 项素材交给助手。
          交接是固定快照：之后你在素材库里改选、换筛选，都不会改变这份集合。
        </p>
        {truncated && (
          <p className="rounded-[var(--radius-control)] border border-[var(--color-status)] px-3 py-2 text-xs text-[var(--color-status)]">
            当前条件共匹配 {selectionTotal} 项，但只加载了 {count} 项。这次交接只包含这 {count} 项；
            需要其余素材时请先收窄条件再次交接。
          </p>
        )}
        <p className="text-xs text-[var(--color-text-tertiary)]">
          交接不会移动、导出或删除任何文件，也不会自动开始打标。助手拿到的是这些素材的引用；
          任何执行仍需要你在任务卡上确认。
        </p>
        {error && (
          <p role="alert" className="text-xs text-[var(--color-danger)]">
            交接失败：{error}
          </p>
        )}
      </div>
    </Modal>
  );
}
