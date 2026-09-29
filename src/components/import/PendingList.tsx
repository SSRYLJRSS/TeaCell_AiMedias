/** 待入库清单（PRD v2.6）：列表/缩略图网格双视图，右上角图标切换
 *  性能：缩略图 IntersectionObserver 懒加载 + 模块级请求缓存，大清单不卡
 *  §10（FB-04）：视频在卡片内原位播放（不恢复素材库 hover；AssetCard 无媒体层冻结不变）。
 *  PendingItem 结构：relative 容器 + absolute inset-0 封面 + 仅 active 挂载的视频层 inset-0 + 角标 + 文件名。
 *  播放规则：hover intent 300ms（250~350 区间）后挂载；muted/playsInline/preload=metadata；
 *  同时最多 1 个视频；离开/卸载/入库开始时 pause+清 src+清监听；失败退回封面并提示双击打开；
 *  双击打开原文件（系统默认应用）；视频控件阻止冒泡。
 */
import { useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getPreviewUrl } from "@/api/preview";
import { useHoverIntent } from "@/hooks/useHoverIntent";
import { useDoubleAction } from "@/hooks/useDoubleAction";
import { useHoverPreviewPlayback } from "@/hooks/useHoverPreviewPlayback";
import { acquireVideoSlot } from "@/utils/videoSlot";
import { displayBasename } from "@/utils/pathDisplay";
import { useAppearance } from "@/hooks/useAppearance";
import { CELL_STEPS } from "@/types/settings";
import { currentAppearance, useSettingsStore } from "@/stores/settingsStore";
import { ASPECT_CSS, ASPECT_RATIO, resolveFit } from "@/utils/cellFit";
import type { ImportPlan, ImportPlanItem } from "@/api/import";

type ViewMode = "list" | "grid";

export function formatSize(bytes: number): string {
  if (bytes >= 1 << 30) return `${(bytes / (1 << 30)).toFixed(1)} GB`;
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

function fileName(path: string): string {
  // X-18：分隔符按平台判定，Unix 反斜杠是合法文件名字符
  return displayBasename(path);
}

/** 懒加载缩略图：进入视口才请求，淡入过渡；失败显示类型占位。
 *  §9.2 类冲突修复：基类不含定位（relative），定位由调用方经 className 给（absolute inset-0）。 */
function LazyThumb({ path, kind, className, fit = "cover" }: { path: string; kind: string; className?: string; fit?: "cover" | "contain" }) {
  const ref = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ob = new IntersectionObserver(
      (es) => {
        if (es[0].isIntersecting) {
          setVisible(true);
          ob.disconnect();
        }
      },
      { rootMargin: "200px" },
    );
    ob.observe(el);
    return () => ob.disconnect();
  }, []);

  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    void getPreviewUrl(path).then((u) => {
      if (cancelled) return;
      if (u) setUrl(u);
      else setFailed(true);
    });
    return () => {
      cancelled = true;
    };
  }, [visible, path]);

  return (
    <div ref={ref} className={clsx("overflow-hidden bg-[var(--color-surface)]", className)}>
      {url && (
        <img
          src={url}
          alt=""
          onLoad={() => setLoaded(true)}
          className={clsx(
            "h-full w-full transition-opacity duration-300",
            fit === "contain" ? "object-contain" : "object-cover",
            loaded ? "opacity-100" : "opacity-0",
          )}
        />
      )}
      {(!url || failed) && (
        <div className="absolute inset-0 flex items-center justify-center text-[10px] text-[var(--color-text-secondary)]">
          {url === null && !failed ? "" : kind === "video" ? "视频" : "图片"}
        </div>
      )}
    </div>
  );
}

/** §10 卡片内视频层：absolute inset-0 铺满卡片；仅 active 时由父级挂载。
 *  muted/playsInline/preload=metadata；失败退回封面并提示双击打开；离开/卸载清 src。
 *  FB2-03：播放语义抽到共用 useHoverPreviewPlayback（播「前几秒」不上结尾）；单实例槽用全局 videoSlot。 */
function PendingVideoLayer({ item }: { item: ImportPlanItem }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [vidError, setVidError] = useState(false);
  const [coverUrl, setCoverUrl] = useState<string | null>(null);

  // 封面：失败时展示；不阻塞播放（两者并行，视频就绪即播放）
  useEffect(() => {
    let cancelled = false;
    void getPreviewUrl(item.path).then((u) => {
      if (!cancelled && u) setCoverUrl(u);
    });
    return () => {
      cancelled = true;
    };
  }, [item.path]);

  // 挂载即抢占全局唯一位；卸载让出（模块级视频槽，入库页与素材库共用）
  useEffect(() => {
    const v = videoRef.current;
    return acquireVideoSlot(`pending:${item.path}`, () => {
      if (v) {
        v.pause();
        v.removeAttribute("src");
        void v.load();
      }
    });
  }, [item.path]);

  useHoverPreviewPlayback(videoRef, { previewSeconds: 5, onFailed: () => setVidError(true) });

  if (vidError) {
    return (
      <div className="absolute inset-0 flex flex-col items-center justify-center gap-1 bg-[var(--color-surface)]">
        {coverUrl ? (
          <img src={coverUrl} alt="" className="h-full w-full object-cover" />
        ) : (
          <span className="text-[10px] text-[var(--color-text-secondary)]">视频</span>
        )}
        <span className="absolute bottom-1 rounded bg-black/50 px-1.5 py-0.5 text-[9px] text-white">
          无法播放 · 双击打开
        </span>
      </div>
    );
  }

  return (
    <div className="absolute inset-0 overflow-hidden bg-black" onClick={(e) => e.stopPropagation()}>
      <video
        ref={videoRef}
        src={convertFileSrc(item.path)}
        muted
        playsInline
        preload="metadata"
        className="h-full w-full object-contain"
      />
    </div>
  );
}

interface PendingItemProps {
  item: ImportPlanItem;
  running: boolean;
  onRemove: (path: string) => void;
  onOpenItem: (path: string) => void;
}

/** §10 PendingItem：relative + 封面 absolute inset-0 + 视频层仅 active 挂载 + 角标 + 文件名 */
function PendingItem({ item, running, onRemove, onOpenItem }: PendingItemProps) {
  const hover = useHoverIntent({ disabled: running });
  const cancelHover = hover.cancel;
  // FB2-02（§9.4）：入库网格卡片比例与填充沿用 appearance.grid（决策 4：一个设置管两页）
  const { grid } = useAppearance();
  const aspectCSS = ASPECT_CSS[grid.cellAspect] ?? ASPECT_CSS["4:3"];
  const [cw, ch] = ASPECT_RATIO[grid.cellAspect] ?? ASPECT_RATIO["4:3"];
  const fit = resolveFit(grid.cellFit, null, cw / ch); // 未入库无内容宽高 → smart 退 cover
  const { onClick, onDoubleClick } = useDoubleAction(
    () => {
      /* 单击暂不动作（预览已在 hover 内） */
    },
    () => onOpenItem(item.path),
  );
  // 入库开始：强制释放预览（视频层卸载 + pause）
  useEffect(() => {
    if (running) cancelHover();
  }, [running, cancelHover]);

  const isVideo = item.kind === "video";

  return (
    <div
      className="group relative overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)] transition-all duration-200 hover:-translate-y-0.5 hover:shadow-md"
      style={{ aspectRatio: aspectCSS, background: fit === "contain" ? "var(--color-surface)" : undefined }}
      {...hover.triggerProps}
      onClick={onClick}
      onDoubleClick={onDoubleClick}
    >
      {/* 封面/占位图 absolute inset-0（§10 结构；§9.2 基类不含 relative） */}
      <LazyThumb path={item.path} kind={item.kind} fit={fit} className="absolute inset-0" />
      {/* 视频层 absolute inset-0，仅 active 时挂载（快速扫过不创建实例） */}
      {isVideo && hover.active && <PendingVideoLayer item={item} />}
      {/* 角标 */}
      {isVideo && (
        <span className="absolute top-1 left-1 rounded bg-black/55 px-1 py-0.5 text-[9px] text-white">视频</span>
      )}
      {item.previewStatus === "limited" && !isVideo && (
        <span
          className="absolute top-1 left-1 rounded bg-black/60 px-1 py-0.5 text-[9px] text-[var(--color-status)]"
          title={item.previewMessage}
        >
          可能受限
        </span>
      )}
      {/* 文件名/错误原因 */}
      <div className="absolute inset-x-0 bottom-0 flex items-center justify-between gap-1 bg-gradient-to-t from-black/60 to-transparent px-1.5 pt-3 pb-1">
        <span className="min-w-0 flex-1 truncate text-[11px] text-white" title={item.path}>
          {fileName(item.path)}
        </span>
        <span className="shrink-0 text-[10px] text-white/70">{formatSize(item.size)}</span>
      </div>
      {!running && (
        <button
          aria-label="移除"
          onClick={(e) => {
            e.stopPropagation();
            onRemove(item.path);
          }}
          className="absolute top-1 right-1 hidden h-5 w-5 items-center justify-center rounded-full bg-black/55 text-xs text-white transition-opacity group-hover:flex"
        >
          ×
        </button>
      )}
    </div>
  );
}

/* 苹果式简约图标：列表 / 网格 */
const ListIcon = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
    <line x1="1" y1="3" x2="13" y2="3" />
    <line x1="1" y1="7" x2="13" y2="7" />
    <line x1="1" y1="11" x2="13" y2="11" />
  </svg>
);
const GridIcon = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor">
    <rect x="1" y="1" width="5" height="5" rx="1" />
    <rect x="8" y="1" width="5" height="5" rx="1" />
    <rect x="1" y="8" width="5" height="5" rx="1" />
    <rect x="8" y="8" width="5" height="5" rx="1" />
  </svg>
);

interface PendingListProps {
  items: ImportPlan["items"];
  running: boolean;
  onRemove: (path: string) => void;
  /** 添加文件（页面上层调用 API，本组件不直接 invoke） */
  onAddFiles?: () => void;
  /** 添加文件夹（目录选择器） */
  onAddFolder?: () => void;
  onClear?: () => void;
  /** 双击打开原文件（系统默认应用；§10 双击进入） */
  onOpenItem?: (path: string) => void;
  /** 双击打开动作的承诺式实现（如无系统打开能力时跳过；默认 no-op） */
  onOpenItemFallback?: (path: string) => void;
}

export default function PendingList({
  items,
  running,
  onRemove,
  onAddFiles,
  onAddFolder,
  onClear,
  onOpenItem,
  onOpenItemFallback,
}: PendingListProps) {
  const [view, setView] = useState<ViewMode>("list");
  // FB2-01/02：入库网格格宽档位驱动（CELL_STEPS[importCellStep]）
  const { grid } = useAppearance();

  // FB6 需求二：缩略图视图 Alt/Ctrl/Cmd + 滚轮增减卡片档位（复用素材库 CELL_STEPS 档位语义）。
  // 仅 grid 视图挂原生 wheel listener（passive:false）；普通滚轮只滚动；
  // 档位到边界时不 preventDefault（普通滚动继续）；60ms 节流防触控板惯性一次跳多档；
  // 切档前记录视口中心附近的卡片，切档后一帧恢复中心（不跳顶）；入库 running 中不挂载。
  const scrollRef = useRef<HTMLDivElement>(null);
  /** 节流时间戳放 effect 外（组件级）：effect 因档位变化重建时不清零，保证 60ms 真实生效；-Infinity = 从未步进 */
  const lastWheelStepAt = useRef(-Infinity);
  useEffect(() => {
    if (view !== "grid" || running) return;
    const el = scrollRef.current;
    if (!el) return;
    let raf = 0;
    const onWheel = (e: WheelEvent) => {
      if (!e.altKey && !e.ctrlKey && !e.metaKey) return; // 普通滚轮：只滚动
      const target = e.target as HTMLElement | null;
      if (target && target.closest("input, textarea, select")) return; // 输入聚焦时不拦截
      const app = currentAppearance(
        useSettingsStore.getState().settings,
        useSettingsStore.getState().previewAppearance,
      );
      const cur = app.grid.importCellStep;
      const next = Math.max(0, Math.min(CELL_STEPS.length - 1, cur + (e.deltaY < 0 ? 1 : -1)));
      if (next === cur) return; // 边界档位：不阻止默认行为（普通滚动继续）
      const now = performance.now();
      if (now - lastWheelStepAt.current < 60) return; // 节流窗内不改变档位（也不拦截滚动）
      e.preventDefault();
      lastWheelStepAt.current = now;

      // 锚点：切档前视口中心附近的卡片（读 scrollTop/clientHeight + getBoundingClientRect，不读 offsetX/Y）
      const gridEl = el.querySelector<HTMLElement>("[data-pending-grid]");
      let anchor: HTMLElement | null = null;
      if (gridEl) {
        const elRect = el.getBoundingClientRect();
        const centerY = elRect.top + el.clientHeight / 2;
        for (const child of Array.from(gridEl.children) as HTMLElement[]) {
          const r = child.getBoundingClientRect();
          if (r.top <= centerY && r.bottom >= centerY) {
            anchor = child;
            break;
          }
        }
        anchor ??= (gridEl.firstElementChild as HTMLElement) ?? null;
      }

      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        useSettingsStore.getState().commitAppearanceDebounced((a) => ({
          ...a,
          grid: { ...a.grid, importCellStep: next },
        }));
        // 新布局绘制后把锚点卡片回中（等价于素材库 scrollToIndex(center)；jsdom 中 rect 高度为 0 时跳过）
        if (el && gridEl && anchor) {
          requestAnimationFrame(() => {
            const elRect = el.getBoundingClientRect();
            const gRect = gridEl.getBoundingClientRect();
            const aRect = anchor!.getBoundingClientRect();
            if (!aRect.height) return;
            const gridTopInContent = gRect.top - elRect.top + el.scrollTop;
            const anchorCenter = aRect.top - gRect.top + aRect.height / 2;
            el.scrollTop = gridTopInContent + anchorCenter - el.clientHeight / 2;
          });
        }
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      cancelAnimationFrame(raf);
    };
  }, [view, running, grid.importCellStep]);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border border-[var(--color-border)]">
      {/* 固定动作头部：添加文件/文件夹、视图切换、清空。统计只由左侧清单摘要承担。 */}
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-[var(--color-border)] px-2">
        <button
          type="button"
          onClick={onAddFiles}
          disabled={running}
          className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-40"
          title="添加文件"
          aria-label="添加文件"
        >
          + 添加文件
        </button>
        <button
          type="button"
          onClick={onAddFolder}
          disabled={running}
          className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-40"
          title="添加文件夹"
          aria-label="添加文件夹"
        >
          + 添加文件夹
        </button>
        <span className="ml-auto" />
        <div className="flex overflow-hidden rounded-md border border-[var(--color-border)] bg-[var(--color-bg)]">
          {(
            [
              ["list", <ListIcon key="l" />],
              ["grid", <GridIcon key="g" />],
            ] as const
          ).map(([mode, icon]) => (
            <button
              key={mode}
              onClick={() => setView(mode)}
              title={mode === "list" ? "列表视图" : "缩略图视图"}
              aria-label={mode === "list" ? "列表视图" : "缩略图视图"}
              className={clsx(
                "flex h-6 w-8 items-center justify-center transition-colors duration-150",
                view === mode
                  ? "bg-[var(--color-accent)] text-[var(--color-accent-text)]"
                  : "text-[var(--color-text-secondary)] hover:text-[var(--color-text)]",
              )}
            >
              {icon}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={onClear}
          disabled={running}
          className="rounded px-1.5 py-1 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-40"
          title="清空清单"
          aria-label="清空清单"
        >
          清空
        </button>
      </div>

      <div ref={scrollRef} data-testid="pending-grid-scroll" className="min-h-0 flex-1 overflow-y-auto">
        {view === "list" ? (
          <div>
            {items.map((i) => (
              <div
                key={i.path}
                className="flex items-center gap-2.5 border-b border-[var(--color-border)] px-3 py-1.5 text-sm transition-colors duration-150 last:border-b-0 hover:bg-[var(--color-surface)]"
              >
                <span className="shrink-0 rounded bg-[var(--color-surface)] px-1.5 py-0.5 text-[10px] text-[var(--color-text-secondary)]">
                  {i.kind === "video" ? "视频" : "图片"}
                </span>
                <span className="min-w-0 flex-1 truncate text-[var(--color-text)]" title={i.path}>
                  {fileName(i.path)}
                </span>
                <span className="shrink-0 text-xs text-[var(--color-text-secondary)]">{formatSize(i.size)}</span>
                {i.previewStatus === "limited" && (
                  <span className="shrink-0 text-[10px] text-[var(--color-status)]" title={i.previewMessage}>
                    可能受限
                  </span>
                )}
                {!running && (
                  <button
                    aria-label="移除"
                    onClick={() => onRemove(i.path)}
                    className="shrink-0 text-[var(--color-text-secondary)] transition-colors hover:text-[var(--color-danger)]"
                  >
                    ×
                  </button>
                )}
              </div>
            ))}
          </div>
        ) : (
          <div
            data-pending-grid
            className="grid gap-2 p-2"
            style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${CELL_STEPS[grid.importCellStep]}px, 1fr))` }}
          >
            {items.map((i) => (
              <PendingItem
                key={i.path}
                item={i}
                running={running}
                onRemove={onRemove}
                onOpenItem={(p) => {
                  if (onOpenItem) onOpenItem(p);
                  else onOpenItemFallback?.(p);
                }}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
