/** 总仓迁移 / 重新定位向导（A4-05，A4-06 会接进来做设置页入口）。
 *
 * 卡片约束（`plans/v2/execution/A4.md:77-93`、`12-implementation-contracts.md:68-72`）：
 * - 两条**互不替代**的入口：软件把整个总仓搬过去 vs 用户自己搬好了、这里只核验；
 * - **不用文本框改路径直接提交**：目标只能由原生选择器产生，且必须先预检再允许「开始」；
 * - 迁移期间界面必须真实展示可恢复阶段，`switched`/`cleaning` 不是失败；
 * - 源目录**不在核验前删除**，本应用也不自动删。残留只报告。
 *
 * 这里不做任何路径判断或校验：预检、容量、嵌套、清单全部由后端权威推导，
 * 前端只负责把结果如实展示出来。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { open as pickDir } from "@tauri-apps/plugin-dialog";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import {
  cancelLibraryMigration,
  failedMigrationItems,
  isResumableMigration,
  libraryMaintenanceStatus,
  libraryTaskImpact,
  MAINTENANCE_EVENT,
  MIGRATION_PHASE_TEXT,
  migrationItemErrorLabel,
  preflightLibraryMigration,
  resumeLibraryMigration,
  startLibraryMigration,
  type LibraryMigrationKind,
  type LibraryMigrationPreflight,
  type LibraryMigrationReport,
  type MigrationTaskImpact,
} from "@/api/libraryMaintenance";

/** 通知顶栏横幅刷新维护态。迁移开始与结束是仅有的两个变化点。 */
function broadcastMaintenanceChange(): void {
  window.dispatchEvent(new Event(MAINTENANCE_EVENT));
}

/** 字节数转人话；与后端 `format_bytes` 同口径（GiB/MiB 二进制单位）。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} ${units[unit]}` : `${value.toFixed(1)} ${units[unit]}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const KIND_TITLE: Record<LibraryMigrationKind, string> = {
  migrate: "让软件迁移总仓",
  relocate: "我已经搬好了，重新定位",
};

const KIND_HINT: Record<LibraryMigrationKind, string> = {
  migrate:
    "软件把已登记的素材复制到新位置、逐个校验后再切换。源目录在核验通过后仍然完整保留，本流程不会删除它。",
  relocate:
    "您已经把整个总仓搬到新位置，这里只核验并切换定位。不会复制、不会清空重导，素材 ID、标签、评分与历史关联全部保留。",
};

/** 结果区的「下一步」提示。卡片要求成功和部分失败都有明确下一步。 */
function nextStepHint(report: LibraryMigrationReport, failedCount: number): string {
  switch (report.phase) {
    case "completed":
      return failedCount > 0
        ? "总仓位置已切换，但有文件没通过核验。请在素材库里按「定位未核验」筛选，逐项重新核验后再处置旧目录。"
        : "总仓位置已切换完成。原位置的文件与目录都还在，本应用没有删除它们；确认新位置正常后您可以自行处置旧目录。";
    case "failed":
      return "总仓位置没有改变，源目录仍然完整可用。请按上面的原因处理后重新检查并开始；已复制的文件会在重试时复用，不必从头再来。";
    case "aborted":
      return "本次迁移已取消，总仓位置没有改变。您可以重新检查后再次开始。";
    case "switched":
    case "cleaning":
      return "定位已经切换，只剩收尾没做完。请点「继续收尾」，不要新建一次迁移。";
    default:
      return "迁移仍在进行中：正在复制的那一个文件会写完再停，不会留下半截文件。";
  }
}

export interface MigrationWizardProps {
  open: boolean;
  onClose: () => void;
}

export default function MigrationWizard({ open, onClose }: MigrationWizardProps) {
  const [kind, setKind] = useState<LibraryMigrationKind>("migrate");
  const [targetRoot, setTargetRoot] = useState("");
  const [preflight, setPreflight] = useState<LibraryMigrationPreflight | null>(null);
  const [report, setReport] = useState<LibraryMigrationReport | null>(null);
  const [running, setRunning] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [impact, setImpact] = useState<MigrationTaskImpact | null>(null);
  // 维护门的持有者可能不在这个窗口里（另一个窗口在迁仓、或上次的迁移还没收尾）。
  // 只读它、不试图抢占：A4-05 之后 `enter` 是排他的，抢占必然失败，与其让用户
  // 撞一条红色报错，不如现在就说清「在等谁、等到什么时候、之后点哪里」。
  const [maintenance, setMaintenance] = useState<{ active: boolean; reason: string | null }>({
    active: false,
    reason: null,
  });
  const [checking, setChecking] = useState(false);

  /** 读一次真实维护态。读不到时保持上一次已知状态，不假装「没有维护」。 */
  const loadStatus = useCallback(async () => {
    try {
      const status = await libraryMaintenanceStatus();
      setMaintenance({ active: status.active, reason: status.reason });
      return status;
    } catch {
      return null;
    }
  }, []);

  // 打开时读一次真实维护态：可能有上次没结束的迁移等着收尾。
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    void (async () => {
      try {
        // 影响面与维护态一起查：卡片第 1 条要求向导展示「任务影响」，而且
        // 用户不必先选目录就该知道「现在能不能开始」。
        const [status, taskImpact] = await Promise.all([
          libraryMaintenanceStatus(),
          libraryTaskImpact(),
        ]);
        if (cancelled) return;
        setImpact(taskImpact);
        setMaintenance({ active: status.active, reason: status.reason });
        if (status.migration) {
          setReport(status.migration);
          setKind(status.migration.kind);
          setTargetRoot(status.migration.targetRoot);
        }
      } catch (cause) {
        if (!cancelled) setError(`读取维护状态失败：${errorMessage(cause)}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open]);

  // 维护态变化时立刻跟上：别的窗口迁仓结束时横幅会刷新，这张卡片也要消失，
  // 否则用户会盯着一个「请等一下」的提示，而它其实已经结束了。
  useEffect(() => {
    if (!open) return;
    const onChange = () => void loadStatus();
    window.addEventListener(MAINTENANCE_EVENT, onChange);
    return () => window.removeEventListener(MAINTENANCE_EVENT, onChange);
  }, [open, loadStatus]);

  // 切换操作类型或位置后，旧预检立刻作废——不能拿着另一份计划去按「开始」。
  const resetPlan = useCallback(() => {
    setPreflight(null);
    setNotice(null);
    setError(null);
  }, []);

  const chooseTarget = async () => {
    const picked = await pickDir({ directory: true, title: "选择新的总仓位置" });
    if (typeof picked !== "string") return;
    setTargetRoot(picked);
    resetPlan();
  };

  const runPreflight = async () => {
    if (!targetRoot) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await preflightLibraryMigration(targetRoot, kind);
      setPreflight(result);
      // 预检带回的影响面比打开向导时那次更新，用它覆盖。
      setImpact(result.impact);
      setNotice(
        result.warnings.length > 0
          ? "预检完成，但有需要你先确认的提示（见下方）。"
          : "预检通过，可以开始。",
      );
    } catch (cause) {
      setPreflight(null);
      // 维护门被别的任务持有（另一个窗口在迁仓、或上次的迁移还没收尾）时，
      // 后端拒绝的原因是「现在还不能开始」，不是「这个目标不行」。以真实维护态
      // 为准分派：是维护就说清在等谁，别让用户对着红色报错猜自己填错了什么。
      const current = await loadStatus();
      if (current?.active === true) {
        setError(null);
        setNotice(null);
      } else {
        setError(`预检未通过：${errorMessage(cause)}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    if (!preflight) return;
    setRunning(true);
    setError(null);
    setNotice(null);
    // 立刻广播一次：横幅要在复制开始时就出现，而不是等这轮结束。
    broadcastMaintenanceChange();
    try {
      const result = await startLibraryMigration(preflight.targetRoot, preflight.kind);
      setReport(result);
      setNotice(
        result.phase === "completed"
          ? "总仓已切换完成。源目录仍然完整保留，请您自行确认后再处置。"
          : MIGRATION_PHASE_TEXT[result.phase],
      );
    } catch (cause) {
      // 失败后刷新真实阶段与维护态：可能是 copying 被中断（需要用户重试或收尾），
      // 也可能是维护门此刻仍在别的任务手里（那就属于「等一等」，不是「出错了」）。
      const status = await loadStatus();
      if (status) setReport(status.migration);
      if (status?.active === true) {
        setError(null);
        setNotice(null);
      } else {
        setError(`迁移未完成：${errorMessage(cause)}`);
      }
    } finally {
      setRunning(false);
      broadcastMaintenanceChange();
    }
  };

  const cancel = async () => {
    setCancelling(true);
    setError(null);
    try {
      const requested = await cancelLibraryMigration(targetRoot);
      setNotice(
        requested
          ? "已请求停止：正在复制的那一个文件会写完再停，不会留下半截文件。"
          : "当前没有正在执行本次目标的迁移。",
      );
    } catch (cause) {
      setError(`停止失败：${errorMessage(cause)}`);
    } finally {
      setCancelling(false);
    }
  };

  const resume = async () => {
    setBusy(true);
    setError(null);
    try {
      const handled = await resumeLibraryMigration();
      const status = await loadStatus();
      if (status) setReport(status.migration);
      setNotice(
        handled.length > 0
          ? `已收尾 ${handled.length} 次未完成的迁移。`
          : "没有需要收尾的迁移。",
      );
    } catch (cause) {
      setError(`收尾失败：${errorMessage(cause)}`);
    } finally {
      setBusy(false);
      broadcastMaintenanceChange();
    }
  };

  /** 维护结束后不必关窗重开：这里的按钮就是「等到之后接下来做什么」。 */
  const recheckMaintenance = async () => {
    setChecking(true);
    setError(null);
    try {
      const [status, taskImpact] = await Promise.all([loadStatus(), libraryTaskImpact()]);
      if (status) setReport(status.migration);
      setImpact(taskImpact);
      if (status?.active === true) {
        setNotice(`总仓仍在维护中：${status.reason ?? "总仓维护进行中"}。`);
      } else {
        setNotice("现在没有其他任务在维护总仓，可以重新检查并开始。");
      }
    } catch (cause) {
      setError(`读取维护状态失败：${errorMessage(cause)}`);
    } finally {
      setChecking(false);
    }
  };

  const failed = useMemo(() => failedMigrationItems(report), [report]);
  const resumable = isResumableMigration(report);
  // 有未结束的任务时不允许开始：由用户自己先结束或取消那些任务，本应用不会
  // 静默取消它们。
  const blocked = impact?.blocking === true;
  // 已进行到定位切换之后的报告不允许再点「开始」：那会变成新建一次迁移。
  const needsFreshRun =
    !report || report.phase === "failed" || report.phase === "aborted";
  // 关掉窗口再打开、或重启后读回的仍是「正在复制」：那说明这次迁移还没停，
  // 界面要给出停止入口而不是只显示一条开始按钮被禁的死路。
  const inProgress =
    !running && (report?.phase === "preparing" || report?.phase === "copying");
  // 维护门被别的任务持有时（A4-05 之后 `enter` 排他），「检查」和「开始」都必然
  // 被后端拒绝。前端先把自己的按钮停掉，让用户看到的是可等待的说明。
  const heldByOther = maintenance.active && !running && !inProgress;
  const canStart =
    !busy && !running && !!preflight && !blocked && !resumable && needsFreshRun && !heldByOther;

  return (
    <Modal
      open={open}
      title="总仓位置"
      wide
      onClose={running ? () => undefined : onClose}
      footer={
        <>
          <Button disabled={running} onClick={onClose}>
            关闭
          </Button>
          {resumable && (
            <Button variant="primary" disabled={busy} onClick={() => void resume()}>
              继续收尾
            </Button>
          )}
          {running || inProgress ? (
            <Button variant="danger" disabled={cancelling} onClick={() => void cancel()}>
              {cancelling ? "停止中…" : "停止"}
            </Button>
          ) : (
            <Button variant="primary" disabled={!canStart} onClick={() => void start()}>
              开始迁移
            </Button>
          )}
        </>
      }
    >
      <div className="flex flex-col gap-4">
        {/* 操作类型：两条命令互不替代 */}
        <fieldset className="flex flex-col gap-2" disabled={running}>
          <legend className="text-xs font-medium text-[var(--color-text-secondary)]">
            选择要做的事
          </legend>
          {(["migrate", "relocate"] as const).map((option) => (
            <label
              key={option}
              className="flex cursor-pointer items-start gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2"
            >
              <input
                type="radio"
                name="library-migration-kind"
                className="mt-1"
                checked={kind === option}
                onChange={() => {
                  setKind(option);
                  resetPlan();
                }}
              />
              <span className="min-w-0">
                <span className="block text-sm text-[var(--color-text)]">
                  {KIND_TITLE[option]}
                </span>
                <span className="mt-0.5 block text-xs leading-5 text-[var(--color-text-secondary)]">
                  {KIND_HINT[option]}
                </span>
              </span>
            </label>
          ))}
        </fieldset>

        {/* 目标位置：只能由原生选择器产生，没有可手写路径的输入框 */}
        <div className="flex flex-col gap-2">
          <span className="text-xs font-medium text-[var(--color-text-secondary)]">新位置</span>
          <div className="flex flex-wrap items-center gap-2">
            <span
              className="min-w-0 flex-1 truncate rounded-[var(--radius-control)] border border-[var(--color-border)] px-2 py-1.5 text-sm text-[var(--color-text-secondary)]"
              title={targetRoot || undefined}
              data-testid="migration-target-root"
            >
              {targetRoot || "尚未选择"}
            </span>
            <Button disabled={running || busy} onClick={() => void chooseTarget()}>
              选择目录…
            </Button>
            <Button disabled={running || busy || heldByOther || !targetRoot} onClick={() => void runPreflight()}>
              {busy ? "检查中…" : "检查"}
            </Button>
          </div>
        </div>

        {notice && (
          <p role="status" className="text-xs leading-5 text-[var(--color-text-secondary)]">
            {notice}
          </p>
        )}
        {error && (
          <p role="alert" className="text-xs leading-5 text-[var(--color-danger)]">
            {error}
          </p>
        )}

        {/* 另一个任务正在维护：这是「等一等」，不是「你填错了」。
            A4-05 之后维护门排他，抢占一定失败；所以这里只说明在等什么、
            以及等到之后点哪里，而不是把用户丢进一条 CONFLICT 报错。 */}
        {heldByOther && (
          <section
            aria-label="维护占用"
            data-testid="migration-maintenance-hold"
            className="flex flex-col gap-1 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2"
          >
            <p className="text-xs font-medium text-[var(--color-status)]">
              另一个任务正在维护总仓
            </p>
            <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
              {maintenance.reason ?? "总仓维护进行中"}。这是可等待的状态：本窗口不会抢占它，
              检查与开始会在它结束后恢复可用。
            </p>
            <div>
              <Button disabled={checking || busy} onClick={() => void recheckMaintenance()}>
                {checking ? "检查中…" : "重新检查维护状态"}
              </Button>
            </div>
          </section>
        )}

        {/* 任务影响：卡片第 1 条明文要求，且它决定「开始」是否可用 */}
        {impact && (
          <section
            aria-label="任务影响"
            data-testid="migration-impact"
            className="flex flex-col gap-1 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2"
          >
            <p className="text-xs font-medium text-[var(--color-text-secondary)]">任务影响</p>
            <ul className="flex flex-col gap-1 text-xs leading-5">
              {impact.messages.map((message) => (
                <li
                  key={message}
                  className={
                    impact.blocking && message !== impact.messages[impact.messages.length - 1]
                      ? "text-[var(--color-danger)]"
                      : "text-[var(--color-text-secondary)]"
                  }
                >
                  {message}
                </li>
              ))}
            </ul>
          </section>
        )}

        {/* 预检结果：必须先看到这些再允许开始 */}
        {preflight && !report && (
          <section
            aria-label="迁移预检"
            className="flex flex-col gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2"
          >
            <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
              <dt className="text-[var(--color-text-secondary)]">当前位置</dt>
              <dd className="truncate text-[var(--color-text)]" title={preflight.sourceRoot}>
                {preflight.sourceRoot}
              </dd>
              <dt className="text-[var(--color-text-secondary)]">新位置</dt>
              <dd className="truncate text-[var(--color-text)]" title={preflight.targetRoot}>
                {preflight.targetRoot}
              </dd>
              <dt className="text-[var(--color-text-secondary)]">素材数量</dt>
              <dd className="text-[var(--color-text)]">{preflight.totalItems} 个文件</dd>
              <dt className="text-[var(--color-text-secondary)]">预计占用</dt>
              <dd className="text-[var(--color-text)]">{formatBytes(preflight.totalBytes)}</dd>
              <dt className="text-[var(--color-text-secondary)]">目标剩余空间</dt>
              <dd className="text-[var(--color-text)]">
                {/* null 表示无法预检，不能说成「足够」 */}
                {preflight.targetFreeBytes === null
                  ? "当前平台无法预检，请自行确认"
                  : formatBytes(preflight.targetFreeBytes)}
              </dd>
            </dl>
            {preflight.notes.length > 0 && (
              <ul className="list-disc pl-5 text-xs leading-5 text-[var(--color-text-secondary)]">
                {preflight.notes.map((note) => (
                  <li key={note}>{note}</li>
                ))}
              </ul>
            )}
            {preflight.warnings.length > 0 && (
              <ul
                data-testid="migration-warnings"
                className="list-disc pl-5 text-xs leading-5 text-[var(--color-status)]"
              >
                {preflight.warnings.map((warning) => (
                  <li key={`${warning.code}:${warning.message}`}>
                    {warning.message}（{warning.code}）
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}

        {/* 执行结果 / 可恢复阶段 */}
        {report && (
          <section
            aria-label="迁移结果"
            data-testid="migration-report"
            className="flex flex-col gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2"
          >
            <p className="text-sm text-[var(--color-text)]">
              阶段：{MIGRATION_PHASE_TEXT[report.phase]}
            </p>
            <p className="text-xs text-[var(--color-text-secondary)]">
              {report.verifiedItems} / {report.totalItems} 个文件已核验
              {report.failedItems > 0 ? `，${report.failedItems} 个未通过` : ""}
            </p>
            {report.errorMessage && (
              <p className="text-xs leading-5 text-[var(--color-danger)]">
                {report.errorMessage}
                {report.errorCode ? `（${report.errorCode}）` : ""}
              </p>
            )}
            {failed.length > 0 && (
              <ul className="flex flex-col gap-1 text-xs leading-5">
                {failed.map((item) => (
                  <li key={item.itemId} className="text-[var(--color-danger)]">
                    {item.relativePath || item.itemId} · {migrationItemErrorLabel(item)}
                  </li>
                ))}
              </ul>
            )}
            {report.phase === "completed" && failed.length === 0 && (
              <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
                原位置的文件与目录都还在，本应用没有删除它们。确认新位置一切正常后，您可以自行处置旧目录。
              </p>
            )}
            {/* 成功和部分失败都要给出明确下一步 */}
            {report.phase !== "copying" && report.phase !== "preparing" && (
              <p
                data-testid="migration-next-step"
                className="text-xs leading-5 text-[var(--color-text-secondary)]"
              >
                {nextStepHint(report, failed.length)}
              </p>
            )}
          </section>
        )}
      </div>
    </Modal>
  );
}

/** 维护横幅：维护期间显示在应用顶部，说明为什么写入被暂停。
 *
 * 读操作不受影响（卡片要求「读和旧会话继续」，只停写入与自动清理）。
 *
 * 不轮询：迁移只在向导里开始和结束，向导会广播 `library-maintenance-changed`。
 * 轮询会让每个测试都要处理定时器，而维护态本来就不是「随时可能变」的东西。
 */
export function LibraryMaintenanceBanner() {
  const [reason, setReason] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const status = await libraryMaintenanceStatus();
        if (!cancelled) setReason(status.active ? status.reason ?? "总仓维护进行中" : null);
      } catch {
        // 读不到维护态时不假装正常，也不阻塞界面：保持上一次已知状态。
      }
    };
    void load();
    const onChange = () => void load();
    window.addEventListener(MAINTENANCE_EVENT, onChange);
    return () => {
      cancelled = true;
      window.removeEventListener(MAINTENANCE_EVENT, onChange);
    };
  }, []);

  if (!reason) return null;
  return (
    <div
      role="status"
      data-testid="library-maintenance-banner"
      className="border-b border-[var(--color-border)] bg-[var(--color-surface)] px-4 py-1.5 text-xs text-[var(--color-text-secondary)]"
    >
      总仓维护中：{reason}。素材可以浏览，新的写入操作会等维护结束后再进行。
    </div>
  );
}
