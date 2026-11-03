/** 已记住的目录授权面板（A3-06）。
 *
 * 背景：`pickDirectoryGrant(purpose, remembered=true)` 会让授权跨会话保留，
 * 用户必须有一个地方能看到并撤销它（PRD 第 9 节：外部目录默认仅本次任务读取，
 * 可记住并在设置中撤销）。
 *
 * 约束：
 * - 只列「记住的」授权（后端 `list_directory_grants` 只返回 remembered 项），
 *   本次任务的一次性授权不在这里出现，避免用户以为需要手动清理；
 * - 必须显示授权范围：导入来源永远只有读取；只有导出目标才带写入/导出。
 *   「记住读取」不等于入库、写入或外发许可；
 * - 撤销后下一次请求立即受阻，不在前端做任何「先允许一次」的缓冲。
 */
import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import {
  listDirectoryGrants,
  revokeAllDirectoryGrants,
  revokeDirectoryGrant,
  type DirectoryGrant,
} from "@/api/authorization";

const PURPOSE_LABEL: Record<DirectoryGrant["purpose"], string> = {
  import_source: "导入来源（只读）",
  export_destination: "导出目标（写入 / 导出）",
};

/** 把 access 位翻成人话；不用「安全」这类词代替真实权限。 */
export function accessLabel(grant: DirectoryGrant): string {
  const parts: string[] = [];
  if (grant.access.read) parts.push("读取");
  if (grant.access.write) parts.push("写入");
  if (grant.access.export) parts.push("导出");
  return parts.length > 0 ? parts.join(" · ") : "无可访问范围";
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

export default function DirectoryGrantsPanel() {
  const [grants, setGrants] = useState<DirectoryGrant[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [confirmRevokeAll, setConfirmRevokeAll] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await listDirectoryGrants();
      setGrants(Array.isArray(list) ? list : []);
    } catch (cause) {
      setError(`读取目录授权失败：${errorMessage(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const revokeOne = async (grant: DirectoryGrant) => {
    setRevoking(grant.grantRef);
    setError(null);
    setNotice(null);
    try {
      await revokeDirectoryGrant(grant.grantRef);
      setNotice(`已撤销「${grant.displayName}」的访问权限；下一次请求会重新询问。`);
      await refresh();
    } catch (cause) {
      setError(`撤销失败：${errorMessage(cause)}`);
    } finally {
      setRevoking(null);
    }
  };

  const revokeAll = async () => {
    setRevoking("__all__");
    setError(null);
    setNotice(null);
    try {
      await revokeAllDirectoryGrants();
      setConfirmRevokeAll(false);
      setNotice("已撤销全部记住的目录授权；下一次请求会重新询问。");
      await refresh();
    } catch (cause) {
      setError(`撤销失败：${errorMessage(cause)}`);
    } finally {
      setRevoking(null);
    }
  };

  return (
    <div className="flex flex-col gap-3 px-4 py-3">
      <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
        记住的目录可以在后续任务中继续读取，但记住读取不等于入库、写入或外发给 AI 服务的许可。
        撤销后，下一次使用该目录会重新询问。
      </p>

      {notice && (
        <p role="status" className="text-xs text-[var(--color-success)]">
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="text-xs text-[var(--color-danger)]">
          {error}
        </p>
      )}

      {loading && grants === null ? (
        <p className="text-xs text-[var(--color-text-secondary)]">正在读取目录授权…</p>
      ) : grants && grants.length === 0 ? (
        <p className="text-xs text-[var(--color-text-secondary)]">
          还没有记住的目录。选择目录时勾选「记住」才会出现在这里。
        </p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
          {(grants ?? []).map((grant) => (
            <li key={grant.grantRef} className="flex items-start gap-3 px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="truncate text-sm text-[var(--color-text)]" title={grant.displayPath}>
                    {grant.displayName || grant.displayPath}
                  </span>
                  <span className="rounded-full border border-[var(--color-border)] px-1.5 py-0.5 text-[10px] text-[var(--color-text-secondary)]">
                    {PURPOSE_LABEL[grant.purpose]}
                  </span>
                  {!grant.available && (
                    <span
                      className="rounded-full border border-[var(--color-status)] px-1.5 py-0.5 text-[10px] text-[var(--color-status)]"
                      title="目录已被移动、删除或更换了总库，需要重新选择"
                    >
                      目录已不可用
                    </span>
                  )}
                </div>
                <p className="mt-1 truncate text-[11px] text-[var(--color-text-tertiary)]" title={grant.displayPath}>
                  {grant.displayPath}
                </p>
                <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">
                  权限：{accessLabel(grant)}
                  {grant.recursive ? "（含子目录）" : "（仅当前目录）"}
                  {typeof grant.expiresAtUnixSeconds === "number"
                    ? `；本次会话授权，${new Date(grant.expiresAtUnixSeconds * 1000).toLocaleString()} 失效`
                    : "；长期有效，直到你撤销"}
                </p>
              </div>
              <Button
                variant="danger"
                disabled={revoking !== null}
                onClick={() => void revokeOne(grant)}
              >
                {revoking === grant.grantRef ? "撤销中…" : "撤销"}
              </Button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button disabled={loading} onClick={() => void refresh()}>
          {loading ? "刷新中…" : "刷新"}
        </Button>
        <Button
          variant="danger"
          disabled={loading || revoking !== null || !grants || grants.length === 0}
          onClick={() => setConfirmRevokeAll(true)}
        >
          撤销全部
        </Button>
      </div>

      <Modal
        open={confirmRevokeAll}
        title="撤销全部目录授权"
        onClose={() => setConfirmRevokeAll(false)}
        footer={
          <>
            <Button disabled={revoking !== null} onClick={() => setConfirmRevokeAll(false)}>
              取消
            </Button>
            <Button variant="danger" disabled={revoking !== null} onClick={() => void revokeAll()}>
              {revoking === "__all__" ? "撤销中…" : "确认撤销"}
            </Button>
          </>
        }
      >
        <p className={clsx("text-sm leading-6 text-[var(--color-text-secondary)]")}>
          将撤销全部 {grants?.length ?? 0} 个记住的目录授权，但不会删除任何素材或已导入的文件。
          之后需要再次使用这些目录时，应用会重新询问。
        </p>
      </Modal>
    </div>
  );
}
