/** 胶片条（PRD 5.3 v2.5）：本次带入素材横排缩略图（虚拟化，避免数千素材常驻重型 DOM，指导书 §8.4）
 *  - 状态角标：待打标(灰) / 已建议(主题色) / 已确认(黑) / 已拒绝(暗)
 *  - 点击切当前张；Ctrl+点击 多选（批量套用用）
 */
import clsx from "clsx";
import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { getThumbnailUrl, toFileUrl } from "@/api/thumbnail";
import { displayBasename } from "@/utils/pathDisplay";
import type { AiSuggestion } from "@/types/ai";

interface FilmstripProps {
  suggestions: AiSuggestion[];
  currentId: number | null;
  selectedIds: Set<number>;
  onPick: (s: AiSuggestion, ctrl: boolean) => void;
}

const THUMB_W = 72;
const GAP = 8;

/** 单个缩略图：占位图 → 原图回退链 */
function Thumb({ s, active, selected, onPick }: { s: AiSuggestion; active: boolean; selected: boolean; onPick: (ctrl: boolean) => void }) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getThumbnailUrl(s.assetId, "placeholder")
      .then((u) => !cancelled && setUrl(u))
      .catch(() => !cancelled && setFailed(true));
    return () => {
      cancelled = true;
    };
  }, [s.assetId]);

  const dot =
    s.status === "confirmed"
      ? "bg-[var(--color-text)]"
      : s.status === "rejected"
        ? "bg-[var(--color-border)]"
        : Object.keys(s.suggestedTags).length > 0
          ? "bg-[var(--color-status)]"
          : "bg-[var(--color-text-secondary)] opacity-40";

  return (
    <button
      onClick={(e) => onPick(e.ctrlKey || e.metaKey)}
      className={clsx(
        "relative h-14 shrink-0 overflow-hidden rounded-[5px] border-2 bg-[var(--color-surface)] transition-[border-color,opacity]",
        "w-full",
        active
          ? "border-[var(--color-accent)] opacity-100"
          : selected
            ? "border-[var(--color-status)] opacity-100"
            : "border-transparent opacity-70 hover:opacity-100",
      )}
      title={displayBasename(s.assetPath)}
    >
      {url && !failed ? (
        <img
          src={url}
          alt=""
          className="h-full w-full object-cover"
          onError={() => {
            if (url !== toFileUrl(s.assetPath)) setUrl(toFileUrl(s.assetPath));
            else setFailed(true);
          }}
        />
      ) : (
        <div className="h-full w-full bg-[var(--color-surface)]" />
      )}
      <span className={clsx("absolute right-1 bottom-1 h-2 w-2 rounded-full border border-white/70", dot)} />
    </button>
  );
}

export default function Filmstrip({ suggestions, currentId, selectedIds, onPick }: FilmstripProps) {
  const parentRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: suggestions.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => THUMB_W,
    horizontal: true,
    overscan: 8,
  });

  if (suggestions.length === 0) return null;

  return (
    <div
      ref={parentRef}
      className="flex shrink-0 overflow-x-auto border-y border-[var(--color-border)] bg-[var(--color-bg)] px-4 py-3"
      role="list"
      aria-label="素材胶片条"
    >
      <div style={{ height: 56, width: virtualizer.getTotalSize(), position: "relative" }}>
        {virtualizer.getVirtualItems().map((vi) => {
          const s = suggestions[vi.index];
          return (
            <div
              key={s.id}
              style={{
                position: "absolute",
                top: 0,
                left: vi.start,
                width: THUMB_W - GAP,
                marginRight: GAP,
              }}
            >
              <Thumb
                s={s}
                active={s.id === currentId}
                selected={selectedIds.has(s.assetId)}
                onPick={(ctrl) => onPick(s, ctrl)}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
