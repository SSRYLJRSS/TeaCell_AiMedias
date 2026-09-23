/**
 * 资产属性面板（指导书 §11）：[通用] [图片] [视频] 三个小标签 + 两列字段。
 *  - 图片素材默认「图片」，视频素材默认「视频」，其余默认「通用」；
 *  - 用户手动切换后在本次查看器会话内保留；
 *  - 空值按 §11.5 六态语义区分（不适用/未提供/未读取/读取失败/不可用/空）；
 *  - 原始 ffprobe JSON 用可折叠只读代码块展示，不提供编辑。
 */
import { useEffect, useMemo, useState } from "react";
import clsx from "clsx";
import {
  buildCommonFields,
  buildImageFields,
  buildVideoFields,
  probeState,
} from "@/utils/mediaMeta";
import { isVideoAsset } from "@/utils/assetKind";
import { getAsset, rescanAssetMetadata, rescanAssetPalette } from "@/api/assets";
import type { Asset } from "@/types/asset";
import { useMetadataStore } from "@/stores/metadataStore";

type Tab = "general" | "image" | "video";

const TABS: { key: Tab; label: string }[] = [
  { key: "general", label: "通用" },
  { key: "image", label: "图片" },
  { key: "video", label: "视频" },
];

function defaultTab(a: Asset): Tab {
  if (isVideoAsset(a)) return "video";
  if (!isVideoAsset(a)) return "image";
  return "general";
}

export default function AssetInfoPanel({ asset, onRefreshed }: { asset: Asset; onRefreshed?: (asset: Asset) => void }) {
  const [activeTab, setActiveTab] = useState<Tab>(() => defaultTab(asset));
  const [rescanning, setRescanning] = useState(false);
  const [rescanMsg, setRescanMsg] = useState<string | null>(null);

  // 切张时按素材类型重置默认标签（手动选择在本次会话内保留到切张）。
  // 只按 asset.id 触发：切到不同素材才重置 tab；同一素材的其它字段变化不应重置用户当前 tab。
  useEffect(() => {
    setActiveTab(defaultTab(asset));
    setRescanMsg(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [asset.id]);

  const common = useMemo(() => buildCommonFields(asset), [asset]);
  const image = useMemo(() => buildImageFields(asset), [asset]);
  const video = useMemo(() => buildVideoFields(asset), [asset]);
  const { failed } = probeState(asset);

  const rescan = async () => {
    setRescanning(true);
    setRescanMsg(null);
    try {
      await rescanAssetMetadata([asset.id], "ids");
      // FB2-08（§14.7）：单张重算顺带算色板 —— 失败不影响元数据回填结果（色板是增强信息）。
      // 这里的 catch 是有意的：用户要的状态（媒体属性）已拿到，色板缺失在 UI 上表现为
      // "没有色条"，是可见的；不该让"重新读取媒体属性"整体报错。
      await rescanAssetPalette([asset.id], "ids").catch(() => undefined);
      await useMetadataStore.getState().refresh();
      const fresh = await getAsset(asset.id);
      onRefreshed?.(fresh);
      setRescanMsg("已重新读取媒体属性");
    } catch (e) {
      setRescanMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setRescanning(false);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      {/* 三个小标签 + 重新读取 */}
      <div className="flex items-center justify-between">
        <div className="flex gap-1">
          {TABS.map((t) => (
            <button
              key={t.key}
              onClick={() => setActiveTab(t.key)}
              className={clsx(
                "rounded px-2 py-0.5 text-[11px] transition-colors",
                activeTab === t.key
                  ? "bg-[var(--color-surface)] font-medium text-[var(--color-text)]"
                  : "text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]",
              )}
            >
              {t.label}
            </button>
          ))}
        </div>
        <button
          onClick={() => void rescan()}
          disabled={rescanning}
          className="rounded px-2 py-0.5 text-[11px] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-50"
          title="重新读取该素材的媒体属性"
        >
          {rescanning ? "读取中…" : "重新读取"}
        </button>
      </div>
      {rescanMsg && <p className="text-[11px] text-[var(--color-text-secondary)]">{rescanMsg}</p>}

      {/* 通用字段始终显示 */}
      {activeTab === "general" && (
        <dl className="flex flex-col gap-1.5 text-xs">
          {common.map((f) => (
            <MetaRow key={f.key} label={f.label} value={f.text} />
          ))}
        </dl>
      )}

      {activeTab === "image" &&
        (isVideoAsset(asset) ? (
          <p className="text-xs text-[var(--color-text-secondary)]">此素材为视频，图片属性不适用。</p>
        ) : (
          <dl className="flex flex-col gap-1.5 text-xs">
            {image.map((f) => (
              <MetaRow key={f.key} label={f.label} value={f.text} />
            ))}
          </dl>
        ))}

      {activeTab === "video" &&
        (isVideoAsset(asset) ? (
          <>
            <dl className="flex flex-col gap-1.5 text-xs">
              {video.map((f) => (
                <MetaRow key={f.key} label={f.label} value={f.text} />
              ))}
            </dl>
            {failed && (
              <p className="text-xs text-[var(--color-danger)]">媒体探测失败，某些字段可能缺失。</p>
            )}
          </>
        ) : (
          <p className="text-xs text-[var(--color-text-secondary)]">此素材为图片，视频属性不适用。</p>
        ))}

      {/* 原始 ffprobe JSON：可折叠只读 */}
      {asset.mediaMetadataJson && (
        <details className="mt-1">
          <summary className="cursor-pointer text-[11px] text-[var(--color-text-secondary)]">
            更多媒体元数据（原始 JSON）
          </summary>
          <pre className="mt-1 max-h-48 overflow-auto rounded bg-[var(--color-surface)] p-2 text-[10px] leading-4 whitespace-pre-wrap break-all text-[var(--color-text-secondary)] select-text">
            {asset.mediaMetadataJson}
          </pre>
        </details>
      )}
    </div>
  );
}

function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="w-20 shrink-0 text-[var(--color-text-secondary)]">{label}</dt>
      <dd className="min-w-0 flex-1 break-all text-[var(--color-text)] select-text">{value}</dd>
    </div>
  );
}
