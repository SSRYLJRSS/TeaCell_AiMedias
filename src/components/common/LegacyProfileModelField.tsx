/**
 * LegacyProfileModelField（FB5-04 §3.6/§13.5）：AiTaggingPage 的「当前模型」可输入 combobox。
 *  - 输入来自 settings JSON 中的旧档案（apiKey 在 JSON 里，非 keyring）——经显式字段走 discover_ai_models 的 legacy 路径；
 *  - 映射：apiMode openai → protocol openai_chat；anthropic → anthropic_messages；
 *    kind local → deployment local 作为旧数据输入；后端仍会按 Windows 托管 Ollama
 *    的严格边界重新判定，其他 localhost/局域网服务按外部服务处理。
 */
import ModelCombobox from "@/components/common/ModelCombobox";
import { discoverAiModels, type AiDeployment, type AiProtocol } from "@/api/connections";
import type { ApiMode, ProfileKind } from "@/types/settings";

interface LegacyProfileModelFieldProps {
  apiMode: ApiMode;
  kind?: ProfileKind;
  baseUrl: string;
  apiKey: string;
  model: string;
  onModelChange: (v: string) => void;
}

export function apiModeToProtocol(apiMode: ApiMode): AiProtocol {
  return apiMode === "anthropic" ? "anthropic_messages" : "openai_chat";
}

export default function LegacyProfileModelField({ apiMode, kind, baseUrl, apiKey, model, onModelChange }: LegacyProfileModelFieldProps) {
  const deployment: AiDeployment = kind === "local" ? "local" : "cloud";
  const protocol = apiModeToProtocol(apiMode);
  return (
    <ModelCombobox
      value={model}
      onChange={onModelChange}
      label="模型名称"
      onDiscover={() => discoverAiModels({ deployment, protocol, baseUrl, apiKey })}
    />
  );
}
