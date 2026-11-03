/**
 * 导出弹窗（指导书 §6.8 / A4-01）：只做「复制导出」。
 *
 * 三条硬约束写在这里而非藏在后端：
 * 1. 目标目录必须经 `pickDirectoryGrant("export_destination", …)` 授权，前端不再
 *    传裸路径——否则命令层会以 `UNGRANTED_DESTINATION_REJECTED` 拒绝。
 * 2. 库内原文件、原路径与标签一律保留；「移动到目录」入口已移除，库外移动被后端
 *    明确拒绝，因此这里没有任何 mode 选择。
 * 3. 完成摘要必须同时给出目标位置与失败项：只有已落盘且核验字节数成功的文件才算
 *    复制成功，失败项逐条列出原因，不写「全部成功」的模糊话。
 */
import { useEffect, useState } from "react";
import Modal from "@/components/common/Modal";
import Button from "@/components/common/Button";
import ProgressBar from "@/components/common/ProgressBar";
import {
  exportCsvManifest,
  exportLocalFiles,
  exportReport,
  onExportProgress,
  type ExportLayout,
} from "@/api/export";
import { pickDirectoryGrant, type DirectoryGrant } from "@/api/authorization";
import { useTauriEvent } from "@/hooks/hooks";
import { useSelectionStore } from "@/stores/selectionStore";
import type { ExportItem, ExportReport } from "@/types/export";

interface ExportDialogProps {
  open: boolean;
  onClose: () => void;
}

/** 逐文件失败原因的展示文案；未知码原样回显，便于排查。 */
const ERROR_LABEL: Record<string, string> = {
  TARGET_EXISTS: "目标目录同名文件过多",
  PERMISSION_DENIED: "目标目录拒绝写入",
  DISK_FULL: "目标磁盘空间不足",
  SOURCE_MISSING: "源文件已不在原位置",
  CANCELLED: "已取消，未开始复制",
  // A4-01：后端逐项复验发现目标子目录越界，或已被换成链接/重解析点，此时只拒绝
  // 这一个文件、其余照常复制。缺这条文案会把原始代码 `PATH_REJECTED` 摆给用户。
  PATH_REJECTED: "目标子目录不安全，该文件未写入",
  IO_ERROR: "读写失败",
};

export function exportErrorLabel(item: ExportItem): string {
  const code = item.errorCode ?? "IO_ERROR";
  const base = ERROR_LABEL[code] ?? code;
  return item.errorMessage ? `${base}（${item.errorMessage}）` : base;
}

/** 失败项最多列 5 条，其余折叠成计数——弹窗不该被 500 行错误铺满。 */
export function failedItems(report: ExportReport, limit = 5): ExportItem[] {
  return report.items.filter((item) => item.state === "failed").slice(0, limit);
}

export default function ExportDialog({ open, onClose }: ExportDialogProps) {
  const selected = useSelectionStore((s) => s.selected);
  const [grant, setGrant] = useState<DirectoryGrant | null>(null);
  /** R-26：子目录组织 + CSV 清单 */
  const [layout, setLayout] = useState<ExportLayout>("flat");
  const [withCsv, setWithCsv] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [report, setReport] = useState<ExportReport | null>(null);
  const [csvPath, setCsvPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useTauriEvent(
    () =>
      onExportProgress((p) => {
        setProgress({ done: p.done, total: p.total });
      }),
    [],
  );

  useEffect(() => {
    if (open) {
      setLayout("flat");
      setWithCsv(false);
      setProgress(null);
      setReport(null);
      setCsvPath(null);
      setError(null);
    }
  }, [open]);

  const chooseDir = async () => {
    setError(null);
    try {
      const picked = await pickDirectoryGrant("export_destination", false);
      if (picked) setGrant(picked);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const run = async () => {
    if (!grant?.directoryRef) return;
    setBusy(true);
    setError(null);
    setReport(null);
    setCsvPath(null);
    try {
      const task = await exportLocalFiles(
        Array.from(selected),
        grant.directoryRef,
        "copy",
        layout,
      );
      const full = await exportReport(task.id);
      setReport(full);
      // R-26：CSV 清单只在至少有一个文件落盘时生成，且同样走目录授权
      if (withCsv && full.counts.copied > 0) {
        try {
          setCsvPath(await exportCsvManifest(Array.from(selected), grant.directoryRef));
        } catch (ce) {
          setError(`清单生成失败：${ce instanceof Error ? ce.message : String(ce)}`);
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const destLabel = grant?.displayPath ?? grant?.displayName ?? "";
  const canRun = Boolean(grant?.directoryRef) && !busy && selected.size > 0;

  return (
    <Modal
      open={open}
      title={`复制导出 ${selected.size} 项素材`}
      onClose={busy ? () => undefined : onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            关闭
          </Button>
          <Button variant="primary" disabled={!canRun} onClick={run}>
            {busy ? "复制中…" : "开始复制导出"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <div className="flex items-center gap-2">
          <input
            readOnly
            value={destLabel}
            placeholder="选择目标目录…"
            aria-label="目标目录"
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none"
          />
          <Button onClick={chooseDir}>浏览…</Button>
        </div>
        <p className="text-xs text-[var(--color-text-secondary)]">
          复制导出只写出副本：库内原文件、原路径与标签保持不变。目标目录里已有同名文件时会自动改用
          「名称(1)」，不会覆盖。
        </p>
        {/* R-26：目录组织 + CSV 清单 */}
        <div className="flex items-center gap-3 text-sm">
          <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">目录组织</span>
          <select
            aria-label="目录组织"
            value={layout}
            onChange={(e) => setLayout(e.target.value as ExportLayout)}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm outline-none"
          >
            <option value="flat">平铺（不分目录）</option>
            <option value="by_tag">按标签分目录</option>
            <option value="by_date">按拍摄月份分目录</option>
          </select>
          <label className="ml-auto flex cursor-pointer items-center gap-1.5 text-xs text-[var(--color-text-secondary)]">
            <input type="checkbox" checked={withCsv} onChange={(e) => setWithCsv(e.target.checked)} />
            附 CSV 清单（文件名/标签/EXIF）
          </label>
        </div>
        {progress && (
          <div className="flex flex-col gap-1">
            <ProgressBar value={progress.total ? progress.done / progress.total : 0} />
            <span className="text-xs text-[var(--color-text-secondary)]">
              {progress.done}/{progress.total}
            </span>
          </div>
        )}
        {report && (
          <div className="flex flex-col gap-1 text-xs" data-testid="export-summary">
            <p className="text-[var(--color-text)]">
              {report.counts.copied > 0
                ? `已复制 ${report.counts.copied} 个文件到 ${report.task.destDir ?? destLabel}`
                : "没有文件被复制"}
              {report.counts.skipped > 0 ? `，${report.counts.skipped} 个未开始（已取消）` : ""}
              {report.counts.failed > 0 ? `，${report.counts.failed} 个失败` : ""}
            </p>
            {report.task.warning && (
              <p className="text-[var(--color-text-secondary)]">{report.task.warning}</p>
            )}
            {failedItems(report).map((item) => (
              <p key={item.id} className="text-[var(--color-danger)]">
                失败：{item.fileName} —— {exportErrorLabel(item)}
              </p>
            ))}
            {report.counts.failed > failedItems(report).length && (
              <p className="text-[var(--color-danger)]">
                另有 {report.counts.failed - failedItems(report).length} 个文件失败
              </p>
            )}
            {csvPath && <p className="text-[var(--color-text-secondary)]">清单：{csvPath}</p>}
          </div>
        )}
        {!report && error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
        {report && error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
      </div>
    </Modal>
  );
}
