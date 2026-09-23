/**
 * AiConnectionManager（指导书 §6.2/§6.3）：AI 连接档案管理。
 *  - 列出连接档案（含密钥配置状态：已配置/未配置，不回显完整 key）；
 *  - 新增/编辑：名称、接口协议（高级）、服务地址、模型、API 密钥（保存到系统凭据 keyring）；
 *  - 删除（同时清理用途绑定与系统凭据）；
 *  - 编辑凭据：传入新 apiKey 才写 keyring；留空保留原密钥。
 */
import { useCallback, useEffect, useState } from "react";
import clsx from "clsx";
import Button from "@/components/common/Button";
import ModelCombobox from "@/components/common/ModelCombobox";
import {
  deleteAiConnection,
  discoverAiModels,
  listAiConnections,
  saveAiConnection,
  testAiConnection,
  type AiConnection,
  type AiDeployment,
  type AiProtocol,
} from "@/api/connections";
import { ollamaListLocalModels } from "@/api/ollama";

interface Props {
  /** 当前服务位置：本机页只管理应用托管的 Windows Ollama，其他连接统一归在线服务 */
  deployment: AiDeployment;
  notify: (msg: string) => void;
  fail: (msg: string) => void;
  /** 连接变化后刷新用途绑定列表 */
  onChanged?: () => void;
}

const EMPTY_FORM = {
  name: "",
  deployment: "cloud" as AiDeployment,
  protocol: "openai_chat" as AiProtocol,
  baseUrl: "",
  model: "",
  apiKey: "",
  maxConcurrency: "",
  requestsPerMinute: "",
  requestsPerHour: "",
};

export default function AiConnectionManager({ deployment, notify, fail, onChanged }: Props) {
  const [conns, setConns] = useState<AiConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<Record<string, string | null>>({});
  const [localModels, setLocalModels] = useState<string[] | null>(null);

  const visible = conns.filter((c) => {
    if (deployment === "local") return isManagedOllamaConnection(c);
    // 旧数据中可能把 LM Studio/其它 localhost 保存成 local；运行时按外部服务处理，
    // 在线页继续展示，避免用户失去编辑入口。
    return !isManagedOllamaConnection(c);
  });

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setConns(await listAiConnections());
      if (deployment === "local") {
        try {
          const models = await ollamaListLocalModels("http://localhost:11434/v1");
          setLocalModels(models.map((model) => model.name));
        } catch {
          // 服务未运行时不把连接误标成坏配置；引擎区域会展示真实运行态。
          setLocalModels(null);
        }
      } else {
        setLocalModels(null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [deployment]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const beginEdit = (c: AiConnection) => {
    setEditingId(c.id);
    setForm({
      name: c.name,
      deployment: deployment === "local" ? "local" : "cloud",
      protocol: c.protocol,
      baseUrl: c.baseUrl,
      model: c.model,
      apiKey: "", // 不回显完整 key；留空 = 保留
      maxConcurrency: c.maxConcurrency ? String(c.maxConcurrency) : "",
      requestsPerMinute: c.requestsPerMinute ? String(c.requestsPerMinute) : "",
      requestsPerHour: c.requestsPerHour ? String(c.requestsPerHour) : "",
    });
  };

  const beginAdd = () => {
    setEditingId("__new__");
    setForm({ ...EMPTY_FORM, deployment });
  };

  const cancelEdit = () => {
    setEditingId(null);
    setForm(EMPTY_FORM);
  };

  const save = async () => {
    if (!form.name.trim()) return fail("请填写服务名称");
    if (!form.baseUrl.trim()) return fail("请填写服务地址");
    const maxConcurrency = parseLimit(form.maxConcurrency, "最大并发数", fail);
    const requestsPerMinute = parseLimit(form.requestsPerMinute, "每分钟请求数", fail);
    const requestsPerHour = parseLimit(form.requestsPerHour, "每小时请求数", fail);
    if (maxConcurrency === null || requestsPerMinute === null || requestsPerHour === null) return;
    setSaving(true);
    setError(null);
    try {
      const id = editingId === "__new__" ? crypto.randomUUID() : editingId!;
      await saveAiConnection({
        id,
        name: form.name.trim(),
        deployment: form.deployment,
        protocol: form.protocol,
        baseUrl: form.baseUrl.trim(),
        model: form.model.trim(),
        // 只有用户输入了新 key 才写 keyring；空 = 保留原密钥
        apiKey: form.apiKey.trim() ? form.apiKey.trim() : null,
        maxConcurrency,
        requestsPerMinute,
        requestsPerHour,
      });
      await refresh();
      cancelEdit();
      onChanged?.();
      notify(editingId === "__new__" ? "服务已添加" : "服务已保存");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (c: AiConnection) => {
    if (!window.confirm(`删除服务「${c.name}」？将同时清除系统凭据与使用该服务的功能设置。`)) return;
    setError(null);
    try {
      await deleteAiConnection(c.id);
      await refresh();
      onChanged?.();
      notify("服务已删除");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // FB3-08：连接测试走后端（密钥从 keyring 读取；按 OpenAI/Anthropic/本地协议分支测试）。
  // 前端只传 connection_id，不再自己拼 /models 请求——旧实现传空 key 必然 401。
  const test = async (c: AiConnection) => {
    setTestingId(c.id);
    setTestResult((s) => ({ ...s, [c.id]: null }));
    try {
      const r = await testAiConnection(c.id);
      const label = r.ok ? "连接成功" : "连接失败";
      const latency = r.latencyMs > 0 ? `，耗时 ${r.latencyMs}ms` : "";
      setTestResult((s) => ({
        ...s,
        [c.id]: `${label}：${r.message}${latency}`,
      }));
    } catch (e) {
      setTestResult((s) => ({ ...s, [c.id]: `连接失败：${e instanceof Error ? e.message : String(e)}` }));
    } finally {
      setTestingId(null);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <p className="text-sm text-[var(--color-text)]">AI 服务</p>
        {deployment === "cloud" && (
          <Button onClick={beginAdd} disabled={!!editingId}>{editingId ? "请先完成当前编辑" : "+ 新增服务"}</Button>
        )}
      </div>
      <p className="text-[11px] leading-4 text-[var(--color-text-secondary)]">
        服务地址、模型和密钥在此统一管理。API 密钥保存在系统凭据中，不会完整显示。
      </p>

      {error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}

      {loading ? (
        <p className="text-xs text-[var(--color-text-secondary)]">正在加载服务…</p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)] border-t border-[var(--color-border)]">
          {visible.map((c) =>
            editingId === c.id ? (
              <li key={c.id} className="flex flex-col gap-1.5 py-3">
                <EditFields form={form} setForm={setForm} connectionId={c.id} lockedLocal={deployment === "local"} />
                <div className="flex items-center gap-2">
                  <Button variant="primary" disabled={saving} onClick={() => void save()}>
                    {saving ? "保存中…" : "保存"}
                  </Button>
                  <Button disabled={saving} onClick={cancelEdit}>取消</Button>
                  <span className="text-[10px] text-[var(--color-text-tertiary)]">API 密钥留空时保留原密钥</span>
                </div>
              </li>
            ) : (
              <li key={c.id} className="flex items-center gap-2 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-sm text-[var(--color-text)]">{c.name}</span>
                    <span className="shrink-0 rounded bg-[var(--color-surface-hover)] px-1 text-[10px]">
                      {deployment === "local" ? "本机 Ollama" : "在线服务"}
                    </span>
                    <span className="shrink-0 rounded bg-[var(--color-surface-hover)] px-1 text-[10px] text-[var(--color-text-secondary)]">
                      {c.protocol === "anthropic_messages" ? "Anthropic" : "OpenAI 兼容"}
                    </span>
                    {deployment === "local" && localModels && !hasLocalModel(localModels, c.model) && (
                      <span className="shrink-0 rounded bg-[var(--color-status-soft)] px-1 text-[10px] text-[var(--color-status)]">
                        模型未安装
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 truncate text-[10px] text-[var(--color-text-secondary)]">
                    {c.baseUrl || "未填地址"} · {c.model || "未选模型"} · {c.hasKey ? "密钥已配置" : "密钥未配置"}
                  </div>
                  {deployment === "cloud" && (
                    <div className="mt-0.5 text-[10px] text-[var(--color-text-tertiary)]">
                      请求限制：{formatLimit(c.maxConcurrency)} 并发 · {formatLimit(c.requestsPerMinute)} RPM · {formatLimit(c.requestsPerHour)} 次/小时
                    </div>
                  )}
                  {testResult[c.id] && (
                    <div className={clsx("text-[10px]", testResult[c.id]?.startsWith("连接失败") ? "text-[var(--color-danger)]" : "text-[var(--color-status)]")}>
                      {testResult[c.id]}
                    </div>
                  )}
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <button
                    onClick={() => void test(c)}
                    disabled={testingId === c.id}
                    className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)] disabled:opacity-50"
                  >
                    {testingId === c.id ? "测试中…" : "测试连接"}
                  </button>
                  <button
                    onClick={() => beginEdit(c)}
                    className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)]"
                  >
                    编辑
                  </button>
                  <button
                    onClick={() => void remove(c)}
                    className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)] hover:text-red-500"
                  >
                    删除
                  </button>
                </div>
              </li>
            ),
          )}
          {visible.length === 0 && !editingId && (
            <li className="px-2 py-3 text-center text-xs text-[var(--color-text-secondary)]">
              {deployment === "cloud"
                ? "暂无在线服务，可点击「+ 新增服务」创建。localhost 或局域网地址也请在这里添加。"
                : "暂无本机 Ollama 服务，请在上方本机引擎区域安装、启动或选择模型。"}
            </li>
          )}
          {editingId === "__new__" && (
            <li className="flex flex-col gap-1.5 py-3">
              <EditFields form={form} setForm={setForm} connectionId={null} />
              <div className="flex items-center gap-2">
                <Button variant="primary" disabled={saving} onClick={() => void save()}>
                  {saving ? "保存中…" : "创建"}
                </Button>
                <Button disabled={saving} onClick={cancelEdit}>取消</Button>
              </div>
            </li>
          )}
        </ul>
      )}
    </div>
  );
}

function hasLocalModel(models: string[], model: string): boolean {
  const normalized = model.trim();
  return (
    !!normalized &&
    models.some(
      (name) => name === normalized || name === `${normalized}:latest` || name.startsWith(`${normalized}:`),
    )
  );
}

function formatLimit(value: number | undefined): string {
  return value && value > 0 ? String(value) : "不限";
}

function parseLimit(value: string, label: string, fail: (msg: string) => void): number | null {
  const trimmed = value.trim();
  if (!trimmed) return 0;
  const parsed = Number(trimmed);
  if (!Number.isInteger(parsed) || parsed < 0) {
    fail(`${label}必须是非负整数，留空表示不限`);
    return null;
  }
  return parsed;
}

function isManagedOllamaConnection(connection: AiConnection): boolean {
  const root = connection.baseUrl.trim().replace(/\/+$/, "").replace(/\/v1$/i, "").toLowerCase();
  return (
    connection.deployment === "local" &&
    connection.protocol === "openai_chat" &&
    (root === "http://localhost:11434" || root === "http://127.0.0.1:11434")
  );
}

function EditFields({
  form,
  setForm,
  connectionId,
  lockedLocal,
}: {
  form: typeof EMPTY_FORM;
  setForm: (f: typeof EMPTY_FORM) => void;
  /** FB5-04：编辑中的连接档案 id（__new__ 为 null → legacy 显式字段路径） */
  connectionId: string | null;
  lockedLocal?: boolean;
}) {
  const inputCls = "ui-control rounded-md px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)]";
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <label className="w-16 shrink-0 text-xs text-[var(--color-text-secondary)]">服务名称</label>
        <input className={clsx(inputCls, "flex-1")} value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="如「通义官方」" />
      </div>
      <div className="flex items-center gap-2">
        <label className="w-16 shrink-0 text-xs text-[var(--color-text-secondary)]">API 格式（高级）</label>
        <select
          className={clsx(inputCls, "flex-1")}
          value={form.protocol}
          onChange={(e) => setForm({ ...form, protocol: e.target.value as AiProtocol })}
          disabled={lockedLocal}
        >
          <option value="openai_chat">OpenAI 兼容（/chat/completions）</option>
          <option value="anthropic_messages">Anthropic Messages（/messages）</option>
        </select>
      </div>
      <div className="flex items-center gap-2">
        <label className="w-16 shrink-0 text-xs text-[var(--color-text-secondary)]">服务地址</label>
        <input className={clsx(inputCls, "flex-1")} value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://api.example.com/v1" readOnly={lockedLocal} />
      </div>
      {/* FB5-04（§3.6）：模型字段 = 可输入 combobox。草稿 key 优先于 keyring（连接已保存时），
          新连接走 legacy 显式字段路径（无 connectionId）。 */}
      <div className="flex items-center gap-2">
        <label className="w-16 shrink-0 text-xs text-[var(--color-text-secondary)]">模型名称</label>
        <ModelCombobox
          value={form.model}
          onChange={(model) => setForm({ ...form, model })}
          label="模型名称"
          onDiscover={() =>
            discoverAiModels({
              connectionId: connectionId ?? undefined,
              deployment: form.deployment,
              protocol: form.protocol,
              baseUrl: form.baseUrl,
              apiKey: form.apiKey.trim() || undefined,
            })
          }
        />
      </div>
      <div className="flex items-center gap-2">
        <label className="w-16 shrink-0 text-xs text-[var(--color-text-secondary)]">API 密钥</label>
        <input
          type="password"
          className={clsx(inputCls, "flex-1")}
          value={form.apiKey}
          onChange={(e) => setForm({ ...form, apiKey: e.target.value })}
          placeholder={lockedLocal ? "本机 Ollama 不需要 API 密钥" : "sk-…（留空保留原密钥）"}
          autoComplete="new-password"
          disabled={lockedLocal}
        />
      </div>
      {!lockedLocal && (
        <div className="mt-1 border-t border-[var(--color-border)] pt-2">
          <div className="mb-1 text-xs text-[var(--color-text-secondary)]">请求限制</div>
          <div className="mb-1 text-[10px] leading-4 text-[var(--color-text-tertiary)]">
            用于保护免费或有额度的在线 API；留空或填 0 表示不限。打标和 AI 搜索的实际请求会计入，普通失败会重试一次，服务返回 429 限流时会立即停止批次。Agnes 免费模型可先填并发 1、RPM 16～18。
          </div>
          <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-3">
            <label className="flex items-center gap-1.5 text-[11px] text-[var(--color-text-secondary)]">
              并发数
              <input
                type="number"
                min={0}
                max={128}
                step={1}
                className={clsx(inputCls, "min-w-0 flex-1")}
                value={form.maxConcurrency}
                onChange={(e) => setForm({ ...form, maxConcurrency: e.target.value })}
                placeholder="不限"
                aria-label="最大并发数"
              />
            </label>
            <label className="flex items-center gap-1.5 text-[11px] text-[var(--color-text-secondary)]">
              RPM（次/分钟）
              <input
                type="number"
                min={0}
                max={1000000}
                step={1}
                className={clsx(inputCls, "min-w-0 flex-1")}
                value={form.requestsPerMinute}
                onChange={(e) => setForm({ ...form, requestsPerMinute: e.target.value })}
                placeholder="不限"
                aria-label="每分钟请求数"
              />
            </label>
            <label className="flex items-center gap-1.5 text-[11px] text-[var(--color-text-secondary)]">
              次/小时
              <input
                type="number"
                min={0}
                max={10000000}
                step={1}
                className={clsx(inputCls, "min-w-0 flex-1")}
                value={form.requestsPerHour}
                onChange={(e) => setForm({ ...form, requestsPerHour: e.target.value })}
                placeholder="不限"
                aria-label="每小时请求数"
              />
            </label>
          </div>
        </div>
      )}
    </div>
  );
}
