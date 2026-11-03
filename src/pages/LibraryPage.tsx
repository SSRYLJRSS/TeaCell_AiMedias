/** 素材库页：顶栏筛选 + 左侧任务栏 + 虚拟网格 + 上下文操作条 + 弹窗组（PRD 5.2/5.3）。
 *  §7.2：Viewer 与库页互斥——preview 非空时整体返回 ViewerPage，
 *  GridToolbar/SideBar/AssetGrid 完全卸载；关闭后恢复筛选/滚动/选中上下文（store 持有）。
 *  A3-08：交接给助手与「本次结果视图」——结果视图是只读快照浏览，不建分库。
 *  交接成功后**不抢屏**：留在库页，只留一条「已交给助手 · 查看本次结果」提示；
 *  用户点了才进结果视图，退出后回到原浏览上下文（筛选/选择/滚动都不动）。 */
import { useEffect, useState } from "react";
import GridToolbar from "@/components/library/GridToolbar";
import SideBar from "@/components/library/SideBar";
import AssetGrid from "@/components/library/AssetGrid";
import DeleteDialog from "@/components/dialogs/DeleteDialog";
import DupDialog from "@/components/dialogs/DupDialog";
import ExportDialog from "@/components/dialogs/ExportDialog";
import MoveDialog from "@/components/dialogs/MoveDialog";
import TagAssignDialog from "@/components/dialogs/TagAssignDialog";
import ViewerPage from "@/components/library/ViewerPage";
import HandoffDialog from "@/components/library/HandoffDialog";
import SelectionResultView from "@/components/library/SelectionResultView";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import { useAiStore } from "@/stores/aiStore";
import { useConversationStore } from "@/stores/conversationStore";
import type { Asset } from "@/types/asset";

type DialogKey = "delete" | "purge" | "export" | "move" | "tags" | "dedup" | "handoff" | null;

export default function LibraryPage() {
  const refresh = useLibraryStore((s) => s.refresh);
  const error = useLibraryStore((s) => s.error);
  const selected = useSelectionStore((s) => s.selected);
  const result = useSelectionResultStore((s) => s.target);
  const closeResult = useSelectionResultStore((s) => s.close);
  /** A3-08：结果视图要能说清「这份快照是不是当前会话交的」。订阅而不是
   *  渲染期 getState()，否则切换会话后这一行不会重算。 */
  const activeConversationRef = useConversationStore((s) => s.activeRef);
  const [dialog, setDialog] = useState<DialogKey>(null);
  const [preview, setPreview] = useState<Asset | null>(null);
  /** A3-08：交接成功后只在这里留一条可关闭的提示，由用户决定何时去看结果。
   *  以前这里直接 `open()`，把整页换成结果视图 —— 既与自己的注释矛盾，也把
   *  用户正在看的筛选/滚动上下文顶掉了。 */
  const [handedOff, setHandedOff] = useState<{
    selectionRef: string;
    title: string;
    query: string;
    conversationRef: string;
  } | null>(null);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** §7.3 方案 A：打开/关闭 Viewer 同步全局 viewerOpen（App 据此隐藏 BottomBar） */
  const openViewer = (asset: Asset) => {
    setPreview(asset);
    useLibraryStore.getState().setViewerOpen(true);
  };
  const closeViewer = () => {
    setPreview(null);
    useLibraryStore.getState().setViewerOpen(false);
  };
  // 兜底：查看器打开期间经标题栏齿轮等跨页离开时，复位全局 viewerOpen，避免 BottomBar 被永久隐藏
  useEffect(() => () => useLibraryStore.getState().setViewerOpen(false), []);

  // §7.2 互斥：Viewer 打开时整体替换库页内容（库页工具栏/侧栏/网格全部卸载）
  if (preview) {
    return <ViewerPage asset={preview} onClose={closeViewer} />;
  }

  // A3-08：结果视图也整体替换库页内容，保证退出时浏览上下文原样恢复。
  if (result) {
    // 结果视图是全局单槽：在 A 会话打开后切到 B 会话，这里仍会拿着 A 的快照。
    // 不标注的话用户会把它当成 B 会话的交接结果。只声明事实，不自动切会话。
    const fromOtherConversation = Boolean(
      result.conversationRef && result.conversationRef !== activeConversationRef,
    );
    return (
      <SelectionResultView
        selectionRef={result.selectionRef}
        title={result.title}
        query={result.query}
        fromOtherConversation={fromOtherConversation}
        onExit={closeResult}
        onPreview={openViewer}
      />
    );
  }

  // 顶栏与右键菜单共用同一组批量动作（v2.8）
  // 打标统一跳转打标工作台；AI 建议和手工填写使用同一批次。
  const actions = {
    onTag: () => {
      useAiStore.getState().setPendingAssets(Array.from(selected));
      window.dispatchEvent(new CustomEvent("app:navigate", { detail: "ai" }));
    },
    onExport: () => {
      // A4-01：只有一个入口——「复制导出」，目标目录在弹窗里走目录授权。
      setDialog("export");
    },
    // A4-02：库内分库移动（不涉及目录授权，目标来自总仓内的分库清单）
    onMove: () => setDialog("move"),
    onDelete: () => setDialog("delete"),
    onDedup: () => setDialog("dedup"),
    // R-22：回收站「彻底删除」（锁定 delete_file 策略）
    onPurge: () => setDialog("purge"),
    // A3-08：先弹确认（条数/截断），再固定快照；不移动、不导出、不自动打标。
    onHandoff: () => setDialog("handoff"),
  };

  return (
    <div className="relative flex h-full flex-col">
      <GridToolbar {...actions} />
      <div className="flex min-h-0 flex-1">
        <SideBar />
        <AssetGrid onPreview={openViewer} {...actions} />
      </div>

      {error && (
        <div className="absolute right-3 bottom-3 rounded-md bg-[var(--color-danger)] px-3 py-2 text-xs text-white shadow-lg">
          {error}
        </div>
      )}

      <DeleteDialog open={dialog === "delete"} onClose={() => setDialog(null)} />
      <DeleteDialog open={dialog === "purge"} purgeOnly onClose={() => setDialog(null)} />
      <DupDialog open={dialog === "dedup"} onClose={() => setDialog(null)} />
      <ExportDialog open={dialog === "export" && selected.size > 0} onClose={() => setDialog(null)} />
      <MoveDialog open={dialog === "move" && selected.size > 0} onClose={() => setDialog(null)} />
      <TagAssignDialog open={dialog === "tags"} onClose={() => setDialog(null)} />
      <HandoffDialog
        open={dialog === "handoff"}
        onClose={() => setDialog(null)}
        onHandedOff={(result) => {
          setDialog(null);
          // A3-08：交接后停在素材库，只留一条「本次结果」的入口，不打断当前浏览。
          // 条数与来源都用回调里后端确认的值，前端不再各拼一份。
          setHandedOff({
            selectionRef: result.selectionRef,
            title: `交接的 ${result.snapshot} 项`,
            query: result.query,
            conversationRef: result.conversationRef,
          });
        }}
      />

      {handedOff && (
        <div
          role="status"
          aria-label="已交给助手"
          className="absolute right-3 bottom-14 flex items-center gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-text-secondary)] shadow-lg"
        >
          <span>
            已交给助手「{handedOff.title}」（
            {handedOff.query}）
          </span>
          <button
            type="button"
            className="rounded-[var(--radius-control)] px-2 py-0.5 font-medium text-[var(--color-status)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
            onClick={() => {
              useSelectionResultStore.getState().open(handedOff);
              setHandedOff(null);
            }}
          >
            查看本次结果
          </button>
          <button
            type="button"
            aria-label="关闭交接提示"
            className="px-1 text-[var(--color-text-tertiary)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)]"
            onClick={() => setHandedOff(null)}
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}
