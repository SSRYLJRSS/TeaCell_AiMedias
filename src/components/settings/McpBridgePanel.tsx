/** 本机外部接入（MCP）面板（A5-01）。
 *
 * 背景：外部 MCP 客户端通过 packages/mcp-bridge 进程连到已运行的茶馆宿主，用户需要一个
 * 可信界面来启用/关闭本机接入、看运行状态、创建/改名/改范围/撤销配对连接。
 *
 * 约束（docs/contracts/mcp-bridge-v1.md §1、§4.1、§7、§8）：
 * - 桥接进程没有数据库连接、没有文件写执行权、没有网络访问，只做协议转换；
 * - 新建连接默认只勾查询类；写入类必须由用户显式勾选；
 * - `plans.approve` / `operations.resume` / `trash.empty.*` 永不出现在外部工具表里，
 *   勾选 `assets.trash.submit` 只代表「可以提交」，不代表「有批准权」；
 * - 撤销立即生效，已入队任务不回滚；
 * - 默认关闭，关闭时端点文件立即删除并断开所有会话。
 */
import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import Modal from "@/components/common/Modal";
import {
  accessLabel,
  connectionStatusLabel,
  createMcpPairing,
  DEFAULT_MCP_CAPABILITIES,
  mcpBridgeStatus,
  revokeMcpPairing,
  setMcpBridgeEnabled,
  TRASH_SUBMIT_CAPABILITY,
  updateMcpPairing,
  type McpBridgeStatusView,
  type McpPairingView,
} from "@/api/mcp";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 按可发布集合的顺序重排能力，交给后端的数组才是稳定的。 */
function orderCapabilities(capabilities: string[], status: McpBridgeStatusView | null): string[] {
  const publishable = status?.publishable ?? [];
  const known = publishable.map((item) => item.capability);
  const ordered = known.filter((capability) => capabilities.includes(capability));
  const extra = capabilities.filter((capability) => !known.includes(capability));
  return [...ordered, ...extra];
}

function sameCapabilities(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const setB = new Set(b);
  return a.every((capability) => setB.has(capability));
}

/** 用后端返回的单条连接替换列表里的同 id 项；不存在则追加。 */
function upsertConnection(
  status: McpBridgeStatusView | null,
  view: McpPairingView,
): McpBridgeStatusView | null {
  if (!status) return status;
  const existing = status.connections ?? [];
  const found = existing.some((item) => item.connectionId === view.connectionId);
  return {
    ...status,
    connections: found
      ? existing.map((item) => (item.connectionId === view.connectionId ? view : item))
      : [...existing, view],
  };
}

export default function McpBridgePanel() {
  const openPendingPlans = usePendingPlansStore((s) => s.openPanel);
  const [status, setStatus] = useState<McpBridgeStatusView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [working, setWorking] = useState<string | null>(null);
  const [newLabel, setNewLabel] = useState("");
  /** 每条连接尚未保存的能力草稿；没有草稿时以后端返回的值为准。 */
  const [drafts, setDrafts] = useState<Record<string, string[]>>({});
  const [renaming, setRenaming] = useState<{ connectionId: string; label: string } | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<McpPairingView | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const view = await mcpBridgeStatus();
      setStatus(view ?? null);
    } catch (cause) {
      setError(`读取本机接入状态失败：${errorMessage(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const enabled = status?.enabled ?? false;
  const platformSupported = status?.platformSupported ?? true;
  const publishable = status?.publishable ?? [];
  const connections = status?.connections ?? [];

  const toggleEnabled = async () => {
    setWorking("toggle");
    setError(null);
    setNotice(null);
    try {
      const next = await setMcpBridgeEnabled(!enabled);
      setStatus(next ?? status);
      setNotice(
        next?.enabled
          ? "已启用本机 MCP 接入：宿主会创建端点文件并开始监听管道。"
          : "已关闭本机 MCP 接入：端点文件已删除，所有外部会话已断开。",
      );
    } catch (cause) {
      setError(`切换本机 MCP 接入失败：${errorMessage(cause)}`);
    } finally {
      setWorking(null);
    }
  };

  const createConnection = async () => {
    const label = newLabel.trim();
    if (!label) return;
    setWorking("create");
    setError(null);
    setNotice(null);
    try {
      const created = await createMcpPairing(label, [...DEFAULT_MCP_CAPABILITIES]);
      setStatus((prev) => upsertConnection(prev, created));
      setNewLabel("");
      setNotice(
        `已创建连接「${created.label}」，connectionId：${created.connectionId}。请把这个 connectionId 填进 MCP 客户端配置；令牌由桥接进程从端点文件读取，不要写进配置或分享。`,
      );
    } catch (cause) {
      setError(`创建连接失败：${errorMessage(cause)}`);
    } finally {
      setWorking(null);
    }
  };

  const capabilitiesFor = (view: McpPairingView): string[] =>
    drafts[view.connectionId] ?? view.capabilities ?? [];

  const toggleCapability = (view: McpPairingView, capability: string) => {
    const current = capabilitiesFor(view);
    const next = current.includes(capability)
      ? current.filter((item) => item !== capability)
      : [...current, capability];
    setDrafts((prev) => ({ ...prev, [view.connectionId]: next }));
  };

  const saveCapabilities = async (view: McpPairingView) => {
    const next = orderCapabilities(capabilitiesFor(view), status);
    setWorking(`caps:${view.connectionId}`);
    setError(null);
    setNotice(null);
    try {
      const updated = await updateMcpPairing(view.connectionId, view.label, next);
      setStatus((prev) => upsertConnection(prev, updated));
      setDrafts((prev) => {
        const copy = { ...prev };
        delete copy[view.connectionId];
        return copy;
      });
      setNotice(`已保存「${updated.label}」的权限范围；下一帧调用按新范围校验。`);
    } catch (cause) {
      setError(`保存权限范围失败：${errorMessage(cause)}`);
    } finally {
      setWorking(null);
    }
  };

  const commitRename = async (view: McpPairingView) => {
    if (!renaming) return;
    const label = renaming.label.trim();
    if (!label) return;
    setWorking(`rename:${view.connectionId}`);
    setError(null);
    setNotice(null);
    try {
      const updated = await updateMcpPairing(
        view.connectionId,
        label,
        orderCapabilities(capabilitiesFor(view), status),
      );
      setStatus((prev) => upsertConnection(prev, updated));
      setDrafts((prev) => {
        const copy = { ...prev };
        delete copy[view.connectionId];
        return copy;
      });
      setRenaming(null);
      setNotice(`已把连接改名为「${updated.label}」。`);
    } catch (cause) {
      setError(`改名失败：${errorMessage(cause)}`);
    } finally {
      setWorking(null);
    }
  };

  const confirmRevoke = async () => {
    const target = revokeTarget;
    if (!target) return;
    setWorking(`revoke:${target.connectionId}`);
    setError(null);
    setNotice(null);
    try {
      const revoked = await revokeMcpPairing(target.connectionId);
      setStatus((prev) => upsertConnection(prev, revoked));
      setRevokeTarget(null);
      setNotice(
        `已撤销连接「${target.label}」。撤销立即生效：下一次握手会被拒绝，已建立的会话也会在下一帧被拒绝；已进入队列的任务不会回滚。`,
      );
    } catch (cause) {
      setError(`撤销连接失败：${errorMessage(cause)}`);
    } finally {
      setWorking(null);
    }
  };

  return (
    <div className="flex flex-col gap-3 px-4 py-3">
      <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
        外部 MCP 客户端可以通过桥接进程连到正在运行的茶馆。桥接进程没有数据库连接、没有文件写权限、不能发起网络请求；
        计划批准与永久清空只能在本界面完成。本机接入默认关闭，关闭时端点文件立即删除并断开所有会话。
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

      {loading && status === null ? (
        <p className="text-xs text-[var(--color-text-secondary)]">正在读取本机接入状态…</p>
      ) : null}

      {/* 1. 开关 */}
      <div className="flex flex-wrap items-center gap-3">
        <button
          type="button"
          role="switch"
          aria-label="启用本机 MCP 接入"
          aria-checked={enabled}
          disabled={loading || working === "toggle" || !platformSupported}
          onClick={() => void toggleEnabled()}
          className={clsx(
            "inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-status)] disabled:cursor-not-allowed disabled:opacity-40",
            enabled
              ? "border-[var(--color-status)] bg-[var(--color-status)]"
              : "border-[var(--color-border-strong)] bg-[var(--color-surface)]",
          )}
        >
          <span
            className={clsx(
              "block h-4 w-4 rounded-full transition-transform",
              enabled
                ? "translate-x-6 bg-[var(--color-bg)]"
                : "translate-x-1 bg-[var(--color-text-tertiary)]",
            )}
          />
        </button>
        <span className="text-sm text-[var(--color-text)]">
          {enabled ? "本机 MCP 接入已启用" : "本机 MCP 接入已关闭"}
        </span>
        {working === "toggle" && (
          <span className="text-xs text-[var(--color-text-secondary)]">正在切换…</span>
        )}
      </div>

      {!platformSupported && (
        <p className="text-xs text-[var(--color-text-secondary)]">
          本机 MCP 接入目前只在 Windows x64 提供。
        </p>
      )}

      {/* 2. 状态 */}
      <div
        data-testid="mcp-bridge-status"
        className="flex flex-col gap-1 rounded-[var(--radius-control)] border border-[var(--color-border)] px-3 py-2"
      >
        {status && status.enabled ? (
          <>
            <span className="text-xs text-[var(--color-text)]">
              {status.running
                ? `运行中（会话 ${status.sessionCount}/${status.maxSessions}）`
                : "已启用，但当前没有在监听（重启应用后重试）"}
            </span>
            {status.endpointPath && (
              <span className="break-all font-mono text-[11px] text-[var(--color-text-tertiary)]">
                {status.endpointPath}
              </span>
            )}
            {status.pipeName && (
              <span className="break-all font-mono text-[11px] text-[var(--color-text-tertiary)]">
                {status.pipeName}
              </span>
            )}
          </>
        ) : (
          <span className="text-xs text-[var(--color-text-secondary)]">
            未启用：不会创建端点文件，也不会监听管道
          </span>
        )}
      </div>

      {/* 3. 边界提示 */}
      <p className="text-xs leading-5 text-[var(--color-text-tertiary)]">
        桥接进程没有数据库连接、没有文件写权限、不能发起网络请求。计划批准与永久清空只能在本界面完成；
        外部工具表里永远不会出现 plans.approve、operations.resume、trash.empty.*。
      </p>
      {/* A5-02：外部客户端只能准备计划，确认必须回到可信界面。
          这些计划不属于任何本地会话，所以给一个直连「待审计划」面板的入口。 */}
      <div className="flex items-center gap-2">
        <Button
          className="min-h-0 px-2 py-0.5 text-[11px]"
          onClick={() => void openPendingPlans()}
        >
          查看待审计划
        </Button>
        <span className="text-[11px] text-[var(--color-text-tertiary)]">
          外部客户端准备的计划会出现在这里，由你确认或拒绝；拒绝不产生任何写入。
        </span>
      </div>

      {/* 4. 新建连接 */}
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex min-w-0 flex-col gap-1">
          <span className="text-[11px] text-[var(--color-text-secondary)]">连接名称</span>
          <input
            aria-label="连接名称"
            value={newLabel}
            onChange={(event) => setNewLabel(event.target.value)}
            placeholder="例如：编辑器"
            maxLength={64}
            className="min-h-9 w-56 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-2 text-sm text-[var(--color-text)]"
          />
        </label>
        <Button
          variant="primary"
          disabled={working === "create" || newLabel.trim().length === 0}
          onClick={() => void createConnection()}
        >
          {working === "create" ? "创建中…" : "创建连接"}
        </Button>
      </div>
      <p className="text-[11px] text-[var(--color-text-secondary)]">
        新建连接默认只勾选查询类（assets.search、operations.get）；写入类需要你在此显式勾选。
      </p>

      {/* 5. 连接列表 */}
      {connections.length === 0 ? (
        <p className="text-xs text-[var(--color-text-secondary)]">
          还没有本机接入连接。创建后会在这里看到 connectionId 和权限范围。
        </p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-control)] border border-[var(--color-border)]">
          {connections.map((connection) => {
            const capabilities = capabilitiesFor(connection);
            const dirty = !sameCapabilities(capabilities, connection.capabilities ?? []);
            const busy = working === `caps:${connection.connectionId}`;
            const renamingThis = renaming?.connectionId === connection.connectionId;
            return (
              <li
                key={connection.connectionId}
                className={clsx(
                  "flex flex-col gap-2 px-3 py-2",
                  connection.revoked && "opacity-60",
                )}
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span
                    className={clsx(
                      "truncate text-sm",
                      connection.revoked
                        ? "text-[var(--color-text-secondary)] line-through"
                        : "text-[var(--color-text)]",
                    )}
                    title={connection.label}
                  >
                    {connection.label}
                  </span>
                  <span className="rounded-full border border-[var(--color-border)] px-1.5 py-0.5 text-[10px] text-[var(--color-text-secondary)]">
                    {connectionStatusLabel(connection)}
                  </span>
                </div>
                <p className="break-all font-mono text-[11px] text-[var(--color-text-tertiary)]">
                  {connection.connectionId}
                </p>

                {connection.revoked ? (
                  <p className="text-[11px] text-[var(--color-text-secondary)]">
                    该连接已撤销，不能再握手；保留在这里只为让你看出历史。
                  </p>
                ) : (
                  <>
                    <ul className="flex flex-col gap-1">
                      {publishable.map((capability) => (
                        <li key={capability.capability} className="flex flex-col gap-0.5">
                          <label className="flex items-start gap-2 text-[11px] text-[var(--color-text-secondary)]">
                            <input
                              type="checkbox"
                              aria-label={`允许 ${capability.capability}`}
                              checked={capabilities.includes(capability.capability)}
                              disabled={busy}
                              onChange={() => toggleCapability(connection, capability.capability)}
                              className="mt-0.5 h-3.5 w-3.5 accent-[var(--color-status)]"
                            />
                            <span className="min-w-0">
                              <span className="text-[var(--color-text)]">
                                {accessLabel(capability.access)}
                              </span>
                              <span className="ml-1 font-mono text-[10px] text-[var(--color-text-tertiary)]">
                                {capability.capability} · {capability.toolName}
                              </span>
                              <span className="block">{capability.description}</span>
                            </span>
                          </label>
                          {capability.capability === TRASH_SUBMIT_CAPABILITY &&
                            capabilities.includes(TRASH_SUBMIT_CAPABILITY) && (
                              <p className="ml-6 text-[11px] text-[var(--color-status)]">
                                外部客户端只能提交您已在本界面批准的计划
                              </p>
                            )}
                        </li>
                      ))}
                    </ul>

                    {renamingThis ? (
                      <div className="flex flex-wrap items-end gap-2">
                        <label className="flex flex-col gap-1">
                          <span className="text-[11px] text-[var(--color-text-secondary)]">
                            新连接名称
                          </span>
                          <input
                            aria-label="新连接名称"
                            value={renaming.label}
                            maxLength={64}
                            onChange={(event) =>
                              setRenaming({ connectionId: connection.connectionId, label: event.target.value })
                            }
                            className="min-h-9 w-56 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-2 text-sm text-[var(--color-text)]"
                          />
                        </label>
                        <Button
                          variant="primary"
                          disabled={
                            working === `rename:${connection.connectionId}` ||
                            renaming.label.trim().length === 0
                          }
                          onClick={() => void commitRename(connection)}
                        >
                          保存名称
                        </Button>
                        <Button onClick={() => setRenaming(null)}>取消</Button>
                      </div>
                    ) : (
                      <div className="flex flex-wrap items-center gap-2">
                        <Button
                          disabled={!dirty || busy || working !== null}
                          onClick={() => void saveCapabilities(connection)}
                        >
                          {busy ? "保存中…" : "保存权限"}
                        </Button>
                        <Button
                          disabled={working !== null}
                          onClick={() =>
                            setRenaming({
                              connectionId: connection.connectionId,
                              label: connection.label,
                            })
                          }
                        >
                          重命名
                        </Button>
                        <Button
                          variant="danger"
                          disabled={working !== null}
                          onClick={() => setRevokeTarget(connection)}
                        >
                          撤销
                        </Button>
                      </div>
                    )}
                  </>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button disabled={loading} onClick={() => void refresh()}>
          {loading ? "刷新中…" : "刷新"}
        </Button>
      </div>

      <Modal
        open={revokeTarget !== null}
        title="撤销本机接入连接"
        onClose={() => setRevokeTarget(null)}
        footer={
          <>
            <Button
              disabled={working !== null}
              onClick={() => setRevokeTarget(null)}
            >
              取消
            </Button>
            <Button
              variant="danger"
              disabled={working !== null}
              onClick={() => void confirmRevoke()}
            >
              {working?.startsWith("revoke:") ? "撤销中…" : "确认撤销"}
            </Button>
          </>
        }
      >
        <p className="text-sm leading-6 text-[var(--color-text-secondary)]">
          撤销「{revokeTarget?.label ?? ""}」立即生效：下一次握手会被拒绝，已建立的会话也会在下一帧被拒绝。
          已经进入 Operations 队列的写任务不会因此回滚或重放；你仍可以在茶馆界面查看它们。
        </p>
      </Modal>
    </div>
  );
}
