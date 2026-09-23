/**
 * ServiceManagement（指导书 §8.2）：AI 服务管理的唯一入口。
 * - 「在线服务 / 本机服务」二选一，只显示当前模式的字段与列表；
 * - 服务实体（名称/地址/模型/密钥/协议）只在这里新增/编辑/删除/测试；
 * - 本机服务 Tab 同时承载引擎向导（Ollama 一键安装/模型管理，改造方案 A3）；
 * - 超级搜索/自动打标页面只选择「此功能使用的服务」，不再重复渲染本面板（§8.7）。
 */
import { useState } from "react";
import clsx from "clsx";
import AiConnectionManager from "@/components/settings/AiConnectionManager";
import LocalModelGroup from "@/components/settings/LocalModelGroup";
import {
  getAiUsageBindings,
  listAiConnections,
  saveAiConnection,
  setAiUsageBinding,
} from "@/api/connections";
import type { Settings } from "@/types/settings";
import { selectManagedOllama, usePlatformStore } from "@/stores/platformStore";

const DEFAULT_LOCAL_BASE = "http://localhost:11434/v1";

interface Props {
  draft: Settings;
  onPatchAi: (patch: Partial<Settings["ai"]>) => void;
  onPatchSettings: (patch: Partial<Settings>) => void;
  notify: (msg: string) => void;
  fail: (msg: string) => void;
}

export default function ServiceManagement({ draft, onPatchAi, onPatchSettings, notify, fail }: Props) {
  const managedOllama = usePlatformStore(selectManagedOllama);
  const [deployment, setDeployment] = useState<"cloud" | "local">("cloud");
  const [bindVersion, setBindVersion] = useState(0); // 连接变化后刷新列表
  const effectiveDeployment: "cloud" | "local" = managedOllama ? deployment : "cloud";
  const positions = managedOllama ? (["cloud", "local"] as const) : (["cloud"] as const);

  /**
   * 模型向导和实际用途绑定必须写入同一份 ai_connections 数据。
   * 旧版这里只修改 settings.ai.profiles，批次执行不会读取它，导致“下载并配置”看似成功但仍用旧模型。
   */
  const syncLocalModel = async (model: string) => {
    const connections = await listAiConnections();
    const local = connections.find(
      (connection) =>
        connection.deployment === "local" &&
        connection.protocol === "openai_chat" &&
        connection.baseUrl.replace(/\/+$/, "").replace(/\/v1$/i, "") ===
          DEFAULT_LOCAL_BASE.replace(/\/v1$/, ""),
    );
    const saved = await saveAiConnection(
      local
        ? {
            id: local.id,
            name: local.name,
            deployment: local.deployment,
            protocol: local.protocol,
            baseUrl: local.baseUrl,
            model,
            apiKey: null,
          }
        : {
            id: crypto.randomUUID(),
            name: "本机 Ollama",
            deployment: "local",
            protocol: "openai_chat",
            baseUrl: DEFAULT_LOCAL_BASE,
            model,
            apiKey: null,
          },
    );
    if (!local) {
      const bindings = await getAiUsageBindings();
      const taggingBindingExists =
        !!bindings.tagging && connections.some((connection) => connection.id === bindings.tagging);
      if (!taggingBindingExists) await setAiUsageBinding("tagging", saved.id);
    }
    setBindVersion((v) => v + 1);
  };

  return (
    <div className="flex flex-col">
      {/* 服务位置二选一：在线服务 / 本机服务（§3.6 文案：部署方式 → 服务位置） */}
      <div className="flex items-center gap-4 px-4 py-3">
        <span className="text-xs text-[var(--color-text-secondary)]">服务位置</span>
        <div className="flex items-center gap-1 rounded-md border border-[var(--color-border)] p-0.5">
          {positions.map((d) => (
            <button
              key={d}
              type="button"
              role="tab"
              aria-selected={effectiveDeployment === d}
              onClick={() => setDeployment(d)}
              className={clsx(
                "rounded px-3 py-1 text-sm transition-colors",
                effectiveDeployment === d
                  ? "bg-[var(--color-surface-hover)] font-medium text-[var(--color-text)]"
                  : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
              )}
            >
              {d === "cloud" ? "在线服务" : "本机服务"}
            </button>
          ))}
        </div>
      </div>

      <div className="border-b border-[var(--color-border)] px-4 pb-3">
        <p className="text-[11px] leading-4 text-[var(--color-text-secondary)]">
          {effectiveDeployment === "cloud"
            ? "通过在线服务分析素材；素材会按所选服务发送。服务配置只在这里维护。"
            : "仅在本机处理，不会上传素材。可安装引擎、下载模型，并管理本机服务。"}
        </p>
      </div>

      {!managedOllama && (
        <div className="mx-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2">
          <p className="text-[11px] leading-4 text-[var(--color-text-secondary)]">
            本平台不提供 Ollama 的安装、启动和模型管理。你可以自行安装兼容服务，或连接局域网内其它机器，
            然后在下方表单填写地址和模型。
          </p>
        </div>
      )}

      {effectiveDeployment === "local" && (
        <div className="border-b border-[var(--color-border)]">
          <LocalModelGroup
            draft={draft}
            onPatchAi={onPatchAi}
            onPatchSettings={onPatchSettings}
            onModelSelected={syncLocalModel}
            notify={notify}
            fail={fail}
          />
        </div>
      )}

      <div className="px-4 py-3">
        <AiConnectionManager
          key={`${effectiveDeployment}-${bindVersion}`}
          deployment={effectiveDeployment}
          notify={notify}
          fail={fail}
          onChanged={() => setBindVersion((v) => v + 1)}
        />
      </div>
    </div>
  );
}
