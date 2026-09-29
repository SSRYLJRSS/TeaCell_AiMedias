/** 网格卡片（指导书 §2.2/§6.1）：缩略图 + 角标 + 选中态 + 文件名 + 单击/双击/右键。
 *  FB2-01/02（§9）：卡格比例由 appearance.grid.cellAspect 决定（决策 4：一个设置管素材库与入库两页），
 *  缩略图填充方式由 appearance.grid.cellFit 经 resolveFit 解析（cover/contain/smart，绝不拉伸）。
 *  FB3-01（§3.2）：色条恒定槽位——开关打开时卡片永远预留色条高度（与 rowHeight 同用 HEIGHT_PX），
 *  palette 未到达渲染空槽；视频 hover 仅在卡片内受控播放短片段；双击进入 Viewer。 */
import { memo, useCallback, useContext, useMemo } from "react";
import clsx from "clsx";
import Thumbnail from "./Thumbnail";
import AssetCardVideoLayer from "./AssetCardVideoLayer";
import ColorStrip, { HEIGHT_PX, toPaletteSegments, type PaletteSegment } from "@/components/library/ColorStrip";
import { isVideoAsset } from "@/utils/assetKind";
import { useAppearance } from "@/hooks/useAppearance";
import { useHoverIntent } from "@/hooks/useHoverIntent";
import { GridScrollingContext } from "@/components/library/GridScrollContext";
import { ASPECT_CSS, ASPECT_RATIO, resolveFit } from "@/utils/cellFit";
import type { Asset } from "@/types/asset";

interface AssetCardProps {
  asset: Asset;
  index: number;
  selected: boolean;
  thumbSize: number;
  onSelect: (asset: Asset, index: number, e: React.MouseEvent) => void;
  onPreview: (asset: Asset) => void;
  onContextMenu: (asset: Asset, index: number, e: React.MouseEvent) => void;
  /** FB2-08（§14.9）：点击主色段以同色系搜索；不传则色条主色段不可点 */
  onSearchDominant?: (segment: PaletteSegment) => void;
}

function formatDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

/** RAW 系扩展名（与后端 mime.rs is_raw_ext 同源） */
const RAW_EXTS = new Set([
  "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng",
  "raf", "orf", "rw2", "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff",
  "kdc", "dcr", "mos", "mef", "erf",
]);

/** 格式角标（Phase 2 F05）：RAW/TIFF/HEIC 特殊格式标注，常见格式不打扰 */
function formatBadge(ext: string): string | null {
  const e = ext.toLowerCase();
  if (e === "tif" || e === "tiff") return "TIFF";
  if (e === "heic" || e === "heif") return "HEIC";
  if (RAW_EXTS.has(e)) return "RAW";
  return null;
}

export default memo(function AssetCard({ asset, index, selected, thumbSize, onSelect, onPreview, onContextMenu, onSearchDominant }: AssetCardProps) {
  const { grid, hoverPreview, colorStrip } = useAppearance();
  const isScrolling = useContext(GridScrollingContext);
  const handleClick = useCallback(
    (e: React.MouseEvent) => onSelect(asset, index, e),
    [asset, index, onSelect],
  );
  const handleDoubleClick = useCallback(() => onPreview(asset), [asset, onPreview]);
  const handleContextMenu = useCallback(
    (e: React.MouseEvent) => onContextMenu(asset, index, e),
    [asset, index, onContextMenu],
  );

  // B-4：视频类型统一按 MIME 判断（导入时长读取失败时 durationMs 为 null 也能识别为视频）
  const isVideo = isVideoAsset(asset);
  const badge = formatBadge(asset.fileExt);
  // W5b：评级派生值（rating 0–5）
  const ratingStars = asset.rating && asset.rating > 0 ? "★".repeat(Math.min(asset.rating, 5)) : "";

  // FB2-02：容器比例 + 内容填充（smart 需 contentAspect）
  const aspectCSS = ASPECT_CSS[grid.cellAspect] ?? ASPECT_CSS["4:3"];
  const [cw, ch] = ASPECT_RATIO[grid.cellAspect] ?? ASPECT_RATIO["4:3"];
  const contentAspect =
    typeof asset.width === "number" &&
    typeof asset.height === "number" &&
    asset.height > 0 &&
    asset.width > 0
      ? asset.width / asset.height
      : null;
  const containerAspect = cw / ch;
  const fit = resolveFit(grid.cellFit, contentAspect, containerAspect);

  // FB2-03：视频 hover 原位预览。滚动抑制通过动态护栏读取，不能把 ref
  // 在 render 时快照成过期布尔值，否则滚动结束后指针停在卡片上也不会恢复。
  const previewEnabled = isVideo && hoverPreview.enabled && hoverPreview.inLibraryGrid;
  const { active: hoverActive, triggerProps } = useHoverIntent({
    disabled: !previewEnabled,
    canActivate: () => !isScrolling(),
  });
  // 只在 intent 激活（enter 300ms 后）出现
  const showVideoLayer = isVideo && previewEnabled && hoverActive;

  // FB2-08（§5.2）：只在色条确定要渲染时才把 palette 转 segments（不做模块级缓存——
  // 色板随回算变化，模块级缓存会让用户回算后看到旧色）；memo 依赖 asset.palette。
  const stripOn = colorStrip.enabled && colorStrip.showInLibraryGrid;
  const paletteSegments = useMemo(
    () => (stripOn ? toPaletteSegments(asset.palette) : []),
    [stripOn, asset.palette],
  );
  // FB6 需求三：色条实际可见时媒体框底部直角、色条只留底边圆角 → 两者无缝拼接无白缝；
  // 色条关闭/空 palette 时媒体卡片保持原有整体圆角（空 palette 不渲染色条内容，几何由恒定槽位保证）。
  // FB6（白线修复）：媒体框的 inset 描边会在底边画 1px hairline（深色主题为白 8%），紧贴色条顶边
  //形成「白线」。色条可见时把描边/选中 ring/hover 全部上移到卡片外层（媒体框+色条作为一个整体
  // 描边，接缝处无线）；色条不可见时维持原结构（ring 在媒体框上）。
  const stripVisible = stripOn && (asset.palette?.length ?? 0) > 0;
  const ringClasses = selected
    ? "ring-2 ring-[var(--color-accent)]"
    : "ring-1 ring-inset ring-[var(--color-hairline)] hover:ring-[var(--color-border-strong)]";

  return (
    <div
      className={clsx(
        "relative",
        stripVisible &&
          "group overflow-hidden rounded-md ring-offset-2 ring-offset-[var(--color-bg)] transition-shadow",
        stripVisible && ringClasses,
      )}
    >
      <div
        role="button"
        tabIndex={0}
        aria-selected={selected}
        onClick={handleClick}
        onDoubleClick={handleDoubleClick}
        onContextMenu={handleContextMenu}
        {...triggerProps}
        style={{ aspectRatio: aspectCSS, borderRadius: stripVisible ? "var(--radius-item) var(--radius-item) 0 0" : undefined }}
        className={clsx(
          "relative cursor-pointer overflow-hidden outline-none select-none",
          // 色条不可见时：媒体框自带 group + 描边（原结构）；色条可见时 group/描边在外层 wrapper
          !stripVisible && "group rounded-md",
          !stripVisible && "ring-offset-2 ring-offset-[var(--color-bg)] transition-shadow",
          !stripVisible && ringClasses,
          stripVisible && "group rounded-b-none",
        )}
      >
        <Thumbnail assetId={asset.id} placeholderPath={asset.placeholderPath} alt={asset.fileName} size={thumbSize} fit={fit} />

        {/* FB2-03：视频 hover 原位播放层（absolute inset-0，不是 fixed；卡片内如此结构） */}
        {showVideoLayer && <AssetCardVideoLayer asset={asset} previewSeconds={hoverPreview.previewSeconds} />}

        {/* 视频角标（时长缺失时仅按视频识别，不显示时长数字） */}
        {isVideo && asset.durationMs != null && (
          <span className="absolute right-1 bottom-1 rounded bg-black/60 px-1 text-[10px] leading-4 text-white">
            {formatDuration(asset.durationMs)}
          </span>
        )}

        {/* 格式角标（RAW/TIFF/HEIC） */}
        {badge && (
          <span className="absolute right-1 top-1 rounded bg-black/60 px-1 text-[10px] leading-4 text-white">
            {badge}
          </span>
        )}

        {/* W5b（§W5b）：评级角标（仅 1–5 星显示；0 = 无评级） */}
        {ratingStars !== "" && (
          <span className="absolute bottom-1 left-1 z-10 rounded bg-black/60 px-1 text-[10px] leading-4 text-[#f5c542]">
            {ratingStars}
          </span>
        )}

        {/* 选中勾选 */}
        {selected && (
          <span className="absolute top-1 left-1 flex h-4 w-4 items-center justify-center rounded-full bg-[var(--color-accent)] text-[10px] text-[var(--color-accent-text)]">
            ✓
          </span>
        )}

        {/* hover 文件名（§3.3：只允许透明度变化，不创建媒体层） */}
        <div className="absolute inset-x-0 bottom-0 truncate bg-gradient-to-t from-black/60 to-transparent px-1.5 pt-4 pb-1 text-[11px] text-white opacity-0 transition-opacity group-hover:opacity-100">
          {asset.fileName}
        </div>
      </div>

      {/* FB3-01（§3.2）：色条恒定槽位 —— 开关打开时永远渲染固定高度 wrapper（HEIGHT_PX 与
          AssetGridView.rowHeight 同一常量），palette 未到达时留空槽不显示假色带。
          色板异步补齐只更新内容，不改卡片外框几何 → Virtualizer 行高不再累计错位。 */}
      {stripOn ? (
        <div style={{ height: HEIGHT_PX[colorStrip.height] }} aria-hidden={!asset.palette?.length}>
          {asset.palette?.length ? (
            <ColorStrip
              palette={paletteSegments}
              mode={colorStrip.mode}
              height={colorStrip.height}
              count={colorStrip.count}
              rounded
              onSearchDominant={onSearchDominant}
            />
          ) : null}
        </div>
      ) : null}
    </div>
  );
});
