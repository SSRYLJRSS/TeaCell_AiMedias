/** Agent 服务面板（v2 修复计划 WP5-1；2026-11-01 去掉联网开关）。
 *
 * 背景：Agent 对话的模型请求由本机 Rust 网关发出。这个面板把两件事显式化：
 *   1. Agent 绑定哪个 AI 服务（未绑定时 Agent 不可用，不悄悄回退到别的用途）；
 *   2. 可选的 http(s) 代理（只作为显式配置项，不读取系统代理）。
 *
 * 没有「允许 Agent 联网」开关：绑定服务、服务启用且凭据可用即可直接对话，
 * 就绪判定在后端 `GatewayBroker::preflight`。权限分级只控制文件操作档位
 * （见 Composer 工具行左端的会话权限胶囊），不是联网门槛。
 *
 * 约束：
 * - 前端只经 src/api 调用 Tauri，不直接 invoke；
 * - 颜色只用 theme.css 语义变量；
 * - 保存成功后立即回读后端返回值，不能用本地草稿反推。
 */
import { useCallback, useEffect, useState } from "react";
import Button from "@/components/common/Button";
import {
  getAgentModelProxy,
  setAgentModelProxy,
  type AgentModelProxyView,
} from "@/api/agentNetwork";
import {
  getAiUsageBindings,
  listAiConnections,
  setAiUsageBinding,
  type AiConnection,
} from "@/api/connections";
import { selectAgentHost, usePlatformStore } from "@/stores/platformStore";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-6">
      <div className="min-w-0 sm:max-w-[52%]">
        <p className="text-sm text-[var(--color-text)]">{label}</p>
        {hint && <p className="mt-0.5 text-xs leading-5 text-[var(--color-text-secondary)]">{hint}</p>}
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-2 sm:justify-end">{children}</div>
    </div>
  );
}

export default function AgentNetworkPanel({
  notify,
  fail,
}: {
  notify: (message: string) => void;
  fail: (message: string) => void;
}) {
  const agentHostSupported = usePlatformStore(selectAgentHost);
  const [view, setView] = useState<AgentModelProxyView | null>(null);
  const [proxyDraft, setProxyDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [connections, setConnections] = useState<AiConnection[]>([]);
  const [binding, setBinding] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [network, conns, binds] = await Promise.all([
        getAgentModelProxy(),
        listAiConnections(),
        getAiUsageBindings(),
      ]);
      setView(network);
      setProxyDraft(network.proxy);
      setConnections(conns);
      setBinding(binds.agent ?? null);
      setLoadError(null);
    } catch (error) {
      // 非 Tauri（单元测试 / 预览）时静默降级，不把面板变成一片红。
      setLoadError(messageOf(error));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const apply = async (proxy: string) => {
    setBusy(true);
    try {
      const next = await setAgentModelProxy(proxy);
      setView(next);
      setProxyDraft(next.proxy);
      notify(next.proxy === "" ? "已清空代理，Agent 直连。" : "已保存 Agent 代理。");
    } catch (error) {
      fail(messageOf(error));
      // 失败不改本地草稿：用户输入的代理地址要继续留在输入框里可修改。
    } finally {
      setBusy(false);
    }
  };

  const onBind = async (connectionId: string) => {
    const next = connectionId === "" ? null : connectionId;
    setBusy(true);
    try {
      await setAiUsageBinding("agent", next);
      setBinding(next);
      notify(next ? "Agent 已绑定此服务" : "Agent 已解绑；绑定后才能对话");
    } catch (error) {
      fail(messageOf(error));
    } finally {
      setBusy(false);
    }
  };

  if (loadError) {
    return (
      <Row label="Agent 服务" hint={`读取 Agent 网络配置失败：${loadError}`}>
        <Button onClick={() => void load()}>重试</Button>
      </Row>
    );
  }
  if (!view) {
    return <Row label="Agent 服务" hint="正在读取网络配置…">{null}</Row>;
  }

  const boundName = connections.find((connection) => connection.id === binding)?.name;

  // 平台/安装包不带 Agent 资源时，这些设置注定启用失败（Rust 会回
  // AGENT_HOST_UNAVAILABLE），与其让用户点出一句报错，不如把控件锁住并把原因写在上面那行。
  const locked = busy || !agentHostSupported;

  return (
    <>
      {!agentHostSupported && (
        <Row
          label="Agent 帮助台"
          hint="当前平台或安装包不包含 Agent 运行资源，下面的设置不会生效。"
        >
          {null}
        </Row>
      )}

      <Row
        label="Agent 绑定服务"
        hint={
          binding
            ? `Agent 只使用「${boundName ?? "已失效的服务"}」；服务停用或删除后 Agent 会提示重新绑定。`
            : "还没有绑定服务，Agent 无法对话。云模型与本地 Ollama 都可绑定。"
        }
      >
        <select
          value={binding ?? ""}
          disabled={locked}
          aria-label="Agent 绑定的 AI 服务"
          onChange={(event) => void onBind(event.target.value)}
          className="ui-control min-w-[16rem] rounded-md px-2 py-1.5 text-sm outline-none"
        >
          {/* Agent 没有「跟随默认档案」这种降级：绑错服务比不绑更容易误导。 */}
          <option value="">未绑定（Agent 不可用）</option>
          {connections.map((connection) => (
            <option key={connection.id} value={connection.id} disabled={!connection.enabled}>
              {connection.name} · {connection.model}（{connection.deployment === "cloud" ? "在线" : "本地"}
              {connection.enabled ? "" : "，已停用"}
              {connection.hasKey ? "，已配置密钥" : "，未配置密钥"}）
            </option>
          ))}
        </select>
      </Row>

      {/* 代理只在显式填写时生效：不读取系统代理，避免「看起来配了其实没走」。 */}
      <Row
        label="代理（可选）"
        hint="只支持 http:// 或 https://主机:端口；留空表示直连。不填则完全不走系统代理。"
      >
        <input
          type="text"
          value={proxyDraft}
          disabled={locked}
          spellCheck={false}
          aria-label="Agent 代理"
          placeholder="http://127.0.0.1:7890"
          onChange={(event) => setProxyDraft(event.target.value)}
          className="ui-control w-64 rounded-md px-2 py-1.5 text-sm outline-none"
        />
        <Button
          disabled={locked || proxyDraft.trim() === view.proxy}
          onClick={() => void apply(proxyDraft)}
        >
          保存代理
        </Button>
      </Row>
    </>
  );
}
