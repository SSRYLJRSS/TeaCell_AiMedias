/**
 * 「移动到分库」弹窗（A4-02）。
 *
 * 三条硬约束：
 * 1. 目标只能是**当前总仓内**的分库：列表来自后端 `list_move_targets`（跳过保留目录
 *    与链接/目录联接），引用由 `library_epoch` 派生，总仓重新核验后旧引用自然失效。
 *    前端不构造路径，也不允许用户手输目录。
 * 2. 移动不是重新入库：素材 ID、标签、评分与历史全部保留，只改文件位置与路径记录。
 * 3. 完成后必须刷新目录分面与当前结果视图；部分失败时明确保留未移动项的选中态，
 *    不能让它悄悄消失。
 */
import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import Modal from "@/components/common/Modal";
import Button from "@/components/common/Button";
import { listMoveTargets, moveAssets, type MoveItem, type MoveReport, type MoveTarget } from "@/api/move";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useSelectionStore } from "@/stores/selectionStore";

interface MoveDialogProps {
  open: boolean;
  onClose: () => void;
}

/** 逐文件失败原因；未知码原样回显，便于排查。 */
const ERROR_LABEL: Record<string, string> = {
  TARGET_EXISTS: "目标位置已存在同名文件",
  TARGET_OCCUPIED: "目标文件被其他程序占用",
  SOURCE_MISSING: "源文件已不在原位置",
  SOURCE_CHANGED: "源路径已不是普通文件",
  LIBRARY_CHANGED: "总仓身份已变化，请重新核验",
  PERMISSION_DENIED: "没有写入权限",
  CANCELLED: "已取消，未开始移动",
  IO_ERROR: "读写失败",
};

export function moveErrorLabel(item: MoveItem): string {
  const code = item.errorCode ?? "IO_ERROR";
  return ERROR_LABEL[code] ?? code;
}

/** 失败项最多列 5 条，其余折叠成计数。 */
export function failedMoves(report: MoveReport, limit = 5): MoveItem[] {
  return report.items.filter((item) => item.state === "failed").slice(0, limit);
}

/**
 * 效果不明的项：journal 停在 `moved` —— 文件已经搬过去，但数据库事务/证据没有提交
 * （`record_unknown` 只写操作项、不改 journal）。既不能算成功，也不能当普通失败让
 * 用户「重试就好」，必须单独提示核对。
 */
export function unknownMoves(report: MoveReport, limit = 5): MoveItem[] {
  return report.items.filter((item) => item.state === "moved").slice(0, limit);
}

export default function MoveDialog({ open, onClose }: MoveDialogProps) {
  const { selected, clear, setAll } = useSelectionStore(
    useShallow((s) => ({
      selected: s.selected,
      clear: s.clear,
      setAll: s.setAll,
    })),
  );
  const [targets, setTargets] = useState<MoveTarget[]>([]);
  const [targetRef, setTargetRef] = useState<string>("");
  const [report, setReport] = useState<MoveReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    setReport(null);
    setError(null);
    setTargetRef("");
    setLoading(true);
    let alive = true;
    void listMoveTargets()
      .then((list) => {
        if (!alive) return;
        setTargets(list);
      })
      .catch((e) => {
        if (!alive) return;
        setTargets([]);
        setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [open]);

  const run = async () => {
    if (!targetRef) return;
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      const result = await moveAssets(Array.from(selected), targetRef);
      setReport(result);
      // 目录分面与当前结果视图必须重算：文件夹归属变了
      await Promise.all([
        useLibraryStore.getState().refresh(),
        useMetadataStore.getState().refresh(),
      ]);
      // 全部成功才清空选择；部分失败或效果不明都保留未移动项，用户能继续核对处理
      const unmoved = result.failed > 0 || result.skipped > 0 || result.unknown > 0;
      if (unmoved) {
        const kept = result.items
          .filter((item) => item.state !== "committed")
          .map((item) => item.assetId);
        if (kept.length > 0) setAll(kept);
        else clear();
      } else {
        clear();
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const canRun = Boolean(targetRef) && !busy && selected.size > 0;
  const chosen = targets.find((t) => t.collectionRef === targetRef);

  return (
    <Modal
      open={open}
      title={`移动 ${selected.size} 项素材到分库`}
      onClose={busy ? () => undefined : onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            关闭
          </Button>
          <Button variant="primary" disabled={!canRun} onClick={run}>
            {busy ? "移动中…" : "开始移动"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <label className="text-xs text-[var(--color-text-secondary)]" htmlFor="move-target">
            目标分库
          </label>
          <select
            id="move-target"
            aria-label="目标分库"
            value={targetRef}
            disabled={busy || loading}
            onChange={(e) => setTargetRef(e.target.value)}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none"
          >
            <option value="">{loading ? "正在读取分库…" : "请选择分库…"}</option>
            {targets.map((target) => (
              <option key={target.collectionRef} value={target.collectionRef}>
                {target.label}
              </option>
            ))}
          </select>
        </div>
        <p className="text-xs text-[var(--color-text-secondary)]">
          移动只改变文件在总仓内的位置：素材 ID、标签、评分与历史都保留。目标分库里已有同名文件时会自动改用
          「名称(1)」，不会覆盖。
        </p>
        {chosen && (
          <p className="text-xs text-[var(--color-text-secondary)]" data-testid="move-destination">
            将移动到：{chosen.label}
          </p>
        )}
        {report && (
          <div className="flex flex-col gap-1 text-xs" data-testid="move-summary">
            <p className="text-[var(--color-text)]">
              {report.moved > 0
                ? `已移动 ${report.moved} 个文件到 ${report.collectionPath || "总仓根目录"}`
                : "没有文件被移动"}
              {report.skipped > 0 ? `，${report.skipped} 个未开始（已取消）` : ""}
              {report.failed > 0 ? `，${report.failed} 个失败（仍留在原位置）` : ""}
              {report.unknown > 0
                ? `，${report.unknown} 项结果不明（文件可能已移动但记录未更新，需人工核对）`
                : ""}
            </p>
            {failedMoves(report).map((item) => (
              <p key={item.itemId} className="text-[var(--color-danger)]">
                未移动：{item.fileName} —— {moveErrorLabel(item)}
              </p>
            ))}
            {report.failed > failedMoves(report).length && (
              <p className="text-[var(--color-danger)]">
                另有 {report.failed - failedMoves(report).length} 个文件未移动
              </p>
            )}
            {unknownMoves(report).map((item) => (
              <p key={item.itemId} className="text-[var(--color-status)]">
                需核对：{item.fileName}
              </p>
            ))}
            {report.unknown > unknownMoves(report).length && (
              <p className="text-[var(--color-status)]">
                另有 {report.unknown - unknownMoves(report).length} 项待核对
              </p>
            )}
          </div>
        )}
        {error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
      </div>
    </Modal>
  );
}
