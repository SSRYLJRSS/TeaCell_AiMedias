/** 删除确认弹窗：双策略选择，「删除文件」需二次确认（PRD 5.4-4）
 *
 *  A4-04：回收站的「彻底删除」（`purgeOnly`）不再走 `delete_assets(delete_file)`，
 *  改走 `emptyTrash`。原因：
 *
 *  1. 永久删除必须是一次独立审批（`ReviewKind::PermanentTrash`），普通计划确认
 *     不能替代；后端 `submit_empty` 会拒绝非永久审核类型的计划。
 *  2. 结果必须按逐项报告：`removed` / `failed` / `missing` / `unknown` 四档不能
 *     被压成一个数字，否则「文件本来就不在」和「证明不了删对了」会被当成成功。
 *
 *  在库素材的「删除文件」仍是手动独立确认，且**不**开放给 Agent。
 */
import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import Modal from "@/components/common/Modal";
import Button from "@/components/common/Button";
import { deleteAssets, type DeleteStrategy } from "@/api/assets";
import {
  emptyTrash,
  emptyErrorLabel,
  failedEmpties,
  type EmptyReport,
} from "@/api/trash";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";

interface DeleteDialogProps {
  open: boolean;
  onClose: () => void;
  /** R-22 回收站「彻底删除」模式：锁定永久删除，不提供软删选项 */
  purgeOnly?: boolean;
}

export default function DeleteDialog({ open, onClose, purgeOnly = false }: DeleteDialogProps) {
  const { selected, clear } = useSelectionStore(useShallow((s) => ({ selected: s.selected, clear: s.clear })));
  const removeLocal = useLibraryStore((s) => s.removeLocal);
  const [strategy, setStrategy] = useState<DeleteStrategy>("remove_from_library");
  const [confirmingFile, setConfirmingFile] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [purgeReport, setPurgeReport] = useState<EmptyReport | null>(null);

  const ids = Array.from(selected);
  const effectiveStrategy: DeleteStrategy = purgeOnly ? "delete_file" : strategy;

  const close = () => {
    setStrategy("remove_from_library");
    setConfirmingFile(false);
    setError(null);
    setPurgeReport(null);
    onClose();
  };

  /** 回收站的彻底删除：逐项结果，未成功的项留在选择里便于核对。 */
  const runPurge = async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await emptyTrash(ids);
      const removedIds = report.items
        .filter((item) => item.state === "succeeded")
        .map((item) => item.assetId);
      if (removedIds.length > 0) removeLocal(removedIds);
      const left = ids.filter((id) => !removedIds.includes(id));
      if (left.length > 0) {
        // 没删掉的项必须保留选中：用户还要看见它们并决定怎么办。
        useSelectionStore.getState().setAll(left);
      } else {
        clear();
      }
      setPurgeReport(report);
      if (left.length === 0) close();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await deleteAssets(ids, effectiveStrategy);
      // B03：磁盘删除失败的不从库删——只移除成功的，提示失败数
      const successIds = ids.filter((id) => !result.failedFiles.includes(id));
      if (successIds.length > 0) removeLocal(successIds);
      clear();
      if (result.failedFiles.length > 0) {
        // 部分失败：提示用户，不关闭弹窗（失败的 id 仍在库中，可关闭占用后重试）
        setError(
          `${result.failedFiles.length} 个文件删除失败（可能被占用），已从库中移除其余 ${result.deleted} 项`,
        );
      } else {
        close();
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onConfirm = () => {
    if (effectiveStrategy === "delete_file" && !confirmingFile) {
      setConfirmingFile(true);
      return;
    }
    void (purgeOnly ? runPurge() : run());
  };

  return (
    <Modal open={open} title={purgeOnly ? `永久清空 ${ids.length} 项素材` : `删除 ${ids.length} 项素材`} onClose={close}
      footer={
        <>
          <Button onClick={close}>取消</Button>
          <Button variant="danger" disabled={busy || ids.length === 0} onClick={onConfirm}>
            {confirmingFile
              ? purgeOnly
                ? `永久清空这 ${ids.length} 项`
                : "确认永久删除"
              : busy
                ? purgeOnly
                  ? "清空中…"
                  : "删除中…"
                : "确认删除"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {purgeOnly ? (
          <p className="text-[var(--color-text)]">
            将从回收站永久清空所选素材，<span className="text-[var(--color-danger)]">连同原始文件，不可恢复</span>。
            文件已不在原位置或无法确认归属的项不会被删除，会单独列出来。
          </p>
        ) : (
          <>
            <label className="flex cursor-pointer items-start gap-2">
              <input
                type="radio"
                name="del-strategy"
                checked={strategy === "remove_from_library"}
                onChange={() => {
                  setStrategy("remove_from_library");
                  setConfirmingFile(false);
                }}
                className="mt-1"
              />
              <span>
                <span className="block text-[var(--color-text)]">移入回收站（保留原文件）</span>
                <span className="block text-xs text-[var(--color-text-secondary)]">可在回收站恢复；超期后自动清理（见设置）</span>
              </span>
            </label>
            <label className="flex cursor-pointer items-start gap-2">
              <input
                type="radio"
                name="del-strategy"
                checked={strategy === "delete_file"}
                onChange={() => setStrategy("delete_file")}
                className="mt-1"
              />
              <span>
                <span className="block text-[var(--color-danger)]">删除文件（不可恢复）</span>
                <span className="block text-xs text-[var(--color-text-secondary)]">连同磁盘上的原始文件一起删除</span>
              </span>
            </label>
          </>
        )}
        {confirmingFile && (
          <p className="rounded bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-danger)]">
            二次确认：即将永久删除 {ids.length} 个原始文件，此操作不可恢复！
          </p>
        )}
        {purgeReport && (
          <div
            data-testid="purge-summary"
            className="rounded bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-text)]"
          >
            <p className="font-medium">
              已永久删除 {purgeReport.removed} 项
              {purgeReport.failed > 0 ? `，${purgeReport.failed} 项失败（文件仍在）` : ""}
              {purgeReport.missing > 0 ? `，${purgeReport.missing} 项文件本就不在原位置` : ""}
              {purgeReport.unknown > 0 ? `，${purgeReport.unknown} 项结果不明（需人工核对）` : ""}
              {purgeReport.skipped > 0 ? `，${purgeReport.skipped} 项未处理${
                purgeReport.cancelled ? "（已取消）" : ""
              }` : ""}
            </p>
            {failedEmpties(purgeReport).map((item) => (
              <p key={item.itemId} className="mt-1 text-[var(--color-danger)]">
                {item.fileName} · {emptyErrorLabel(item)}
              </p>
            ))}
          </div>
        )}
        {error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
      </div>
    </Modal>
  );
}
