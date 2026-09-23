/**
 * ViewerFilmstrip（FB4-01 定稿 §4.3）：底部胶片条 —— 固定高度单行缩略图带。
 *  外层固定 80px（h-20）；中间缩略图区为单行 flex（gap-1），固定 56x56px 缩略图。
 *  素材多时只横向滚动（overflow-x:auto + overflow-y:hidden），绝不出第二行或纵向滚动。
 *  上一张/下一张按钮固定左右（保持 size-9）；缩略图区 min-w-0 防按钮挤压。
 *  点击缩略图、左右按钮、键盘左右键都定位同一素材（onJump/onPrev/onNext 由上层绑定）。
 */
import { useEffect, useState } from "react";
import clsx from "clsx";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { toFileUrl } from "@/api/thumbnail";
import { isVideoAsset } from "@/utils/assetKind";
import { requestThumbnail } from "@/utils/thumbnailCache";
import type { Asset } from "@/types/asset";

interface ViewerFilmstripProps {
  items: Asset[];
  currentId: number;
  onJump: (index: number) => void;
  onPrev: () => void;
  onNext: () => void;
}

/** 缩略图固定 56px（FB4-01：单行带硬性尺寸） */
const THUMB = 56;
const PLACEHOLDER_SIZE = 256;

function FilmstripThumbnail({ asset }: { asset: Asset }) {
  const [src, setSrc] = useState<string | null>(() => (
    asset.placeholderPath ? toFileUrl(asset.placeholderPath) : null
  ));

  useEffect(() => {
    let cancelled = false;
    setSrc(asset.placeholderPath ? toFileUrl(asset.placeholderPath) : null);
    // 通过后端取一次“确保有效”的占位图：旧版本写入的通用灰块会在这里被修复，
    // 新路径也能让 WebView 立即避开旧 URL 缓存。
    requestThumbnail(asset.id, "placeholder", PLACEHOLDER_SIZE)
      .then((url) => {
        if (!cancelled) setSrc(url);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [asset.id, asset.placeholderPath]);

  if (!src) {
    return (
      <span className="flex h-full w-full items-center justify-center bg-[var(--color-surface)] text-[9px] text-[var(--color-text-secondary)]">
        {isVideoAsset(asset) ? "视频" : "图片"}
      </span>
    );
  }
  return <img src={src} alt="" className="h-full w-full object-cover" />;
}

export default function ViewerFilmstrip({ items, currentId, onJump, onPrev, onNext }: ViewerFilmstripProps) {
  return (
    <div
      data-filmstrip
      className="flex h-20 shrink-0 items-center gap-1.5 border-t border-[var(--color-border)] px-2 py-2"
    >
      <button
        type="button"
        onClick={onPrev}
        aria-label="上一张"
        title="上一张（←）"
        className="flex size-9 shrink-0 items-center justify-center self-center rounded-md text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-30"
        disabled={items.length === 0}
      >
        <ChevronLeft size={18} strokeWidth={2} aria-hidden="true" />
      </button>

      {/* 单行缩略图区：只横向滚动，无纵向滚动、无第二行（FB4-01） */}
      <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto overflow-y-hidden">
        {items.map((a, i) => (
          <button
            key={a.id}
            type="button"
            onClick={() => onJump(i)}
            aria-label={`第 ${i + 1} 张：${a.fileName}`}
            className={clsx(
              "size-14 shrink-0 overflow-hidden rounded border-2 transition-all",
              a.id === currentId
                ? "border-[var(--color-accent)]"
                : "border-transparent opacity-70 hover:opacity-100",
            )}
            style={{ width: THUMB, height: THUMB }}
          >
            <FilmstripThumbnail asset={a} />
          </button>
        ))}
      </div>

      <button
        type="button"
        onClick={onNext}
        aria-label="下一张"
        title="下一张（→）"
        className="flex size-9 shrink-0 items-center justify-center self-center rounded-md text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-30"
        disabled={items.length === 0}
      >
        <ChevronRight size={18} strokeWidth={2} aria-hidden="true" />
      </button>
    </div>
  );
}
