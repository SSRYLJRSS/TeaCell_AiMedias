/** 查看器（指导书 §4.1 + FB3-04 + FB5-01 §4.1）：ViewerPage 只负责当前素材、上一张/下一张、打开/关闭、组合子组件与错误边界。
 *  布局由 ViewerShell 承载：工具条（固定）→ 左属性栏 + 右媒体舞台 → 底部胶片条。
 *  FB5-01（§4.1）：沉浸浏览状态机 ImmersiveMode = "off" | "native" | "fallback"。
 *   - native：viewerRoot.requestFullscreen() 成功，Fullscreen API 覆盖整个显示器；
 *   - fallback：Fullscreen API 不可用/被拒时，createPortal 把沉浸层挂到 document.body（fixed inset-0 z-[100]），
 *     覆盖整个应用窗口，并让应用根节点 inert 防止 Tab 聚焦底层控件；
 *   - 只认 document.fullscreenElement === viewerRootRef.current 为 native 沉浸；播放器不再有独立 fullscreen。
 *  Escape 优先级：native（浏览器消费 Esc 退出全屏，fullscreenchange 同步回 off）> fallback（退出沉浸）
 *  > 关闭查看器（第二次 Esc）。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import clsx from "clsx";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useShallow } from "zustand/react/shallow";
import ViewerShell from "@/components/viewer/ViewerShell";
import ViewerToolbar from "@/components/viewer/ViewerToolbar";
import ViewerInfoSidebar from "@/components/viewer/ViewerInfoSidebar";
import ViewerFilmstrip from "@/components/viewer/ViewerFilmstrip";
import ViewerTagBar from "@/components/viewer/ViewerTagBar";
import MediaViewport from "@/components/viewer/MediaViewport";
import VideoPlayer from "@/components/media/VideoPlayer";
import ColorStrip, { toPaletteSegments } from "@/components/library/ColorStrip";
import TagAssignDialog from "@/components/dialogs/TagAssignDialog";
import { getThumbnailUrl, toFileUrl } from "@/api/thumbnail";
import { removeTags } from "@/api/tags";
import { ensureVideoProxy, cancelVideoProxy, toProxyFileUrl } from "@/api/video";
import { useLibraryStore } from "@/stores/libraryStore";
import { usePlatformStore, selectPreferredVideoProxy } from "@/stores/platformStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useAppearance } from "@/hooks/useAppearance";
import { isEditableTarget, isInsidePlayer, isViewerNativeFullscreen, requestFullscreenSafe } from "@/utils/shortcuts";
import { isVideoAsset } from "@/utils/assetKind";
import type { Asset } from "@/types/asset";

interface ViewerPageProps {
  asset: Asset;
  onClose: () => void;
  /**
   * 过片数据集（胶片条 / 上下张 / 位置计数 / 近尾自动加载）。
   * 缺省绑定素材库列表（libraryStore）；超级搜索必须传「当前搜索结果集」——
   * 否则查看器会错用素材库全量列表（搜索只命中 6 项、详情却显示 x/118 的联动 bug）。
   */
  listItems?: Asset[];
  listTotal?: number;
  onListLoadMore?: () => void | Promise<void>;
  /** 本地补丁（查看器内删标签 / 信息栏刷新回写当前列表）。缺省写素材库 store；超搜传自身 store。 */
  patchListItem?: (ids: number[], patch: Partial<Asset>) => void;
}

/** FB5-01（§4.1）：沉浸浏览状态。off=普通查看器；native=Fullscreen API；fallback=应用内覆盖层。 */
type ImmersiveMode = "off" | "native" | "fallback";

export default function ViewerPage({ asset: initial, onClose, listItems, listTotal, onListLoadMore, patchListItem }: ViewerPageProps) {
  const library = useLibraryStore(
    useShallow((s) => ({ items: s.items, total: s.total, loadMore: s.loadMore, patchLocal: s.patchLocal })),
  );
  // 数据集可注入：素材库走默认 libraryStore；超级搜索走 superSearchStore 的结果集
  const items = listItems ?? library.items;
  const total = listTotal ?? library.total;
  const loadMore = onListLoadMore ?? library.loadMore;
  const patchLocal = patchListItem ?? library.patchLocal;
  const [currentId, setCurrentId] = useState(initial.id);
  const current: Asset = useMemo(
    () => items.find((a) => a.id === currentId) ?? initial,
    [items, currentId, initial],
  );
  const index = useMemo(() => items.findIndex((a) => a.id === currentId), [items, currentId]);

  // 图片高清源（代际保护：切张后旧请求不回写新素材）
  const [src, setSrc] = useState<string | null>(null);
  const [entered, setEntered] = useState(false);
  // §7.5 致命错误：主图与兜底都失败时显示「当前素材暂时无法显示」，重试需重新获取 URL/高清图
  const [mediaFatal, setMediaFatal] = useState(false);
  const [reloadNonce, setReloadNonce] = useState(0);

  // 视频源 + 代理状态（§8.3：原文件失败 → 按需生成 H.264/AAC MP4）
  const [videoSrc, setVideoSrc] = useState<string | null>(() => convertFileSrc(initial.filePath));
  const [proxyError, setProxyError] = useState<string | null>(null);
  const [proxying, setProxying] = useState(false);
  const proxyAttempted = useRef(false);
  // P1-03：generation 递增作废旧变体/素材的迟到响应，杜绝 H.264 响应覆盖 WebM 状态。
  const proxyGeneration = useRef(0);
  // 代理变体统一从平台 store 取（Linux 用 WebM，其余 MP4），ensure/status/cancel 共用同一值。
  const proxyVariant = usePlatformStore(selectPreferredVideoProxy);
  // P1-03：平台能力就绪前不发起 ensure，避免按错误默认变体生成/复用。
  const platformReady = usePlatformStore((s) => s.status === "ready");

  // 详情开关：左属性栏显隐（§2.3 工具栏「信息开关」；FB3-04 改名，职责不变）
  const [detailsOpen, setDetailsOpen] = useState(true);
  const [assignOpen, setAssignOpen] = useState(false);
  const prevSelected = useRef<ReadonlySet<number> | null>(null);

  // FB5-01（§4.1）：沉浸浏览状态机。native 依赖 viewerRootRef 进入 Fullscreen API；
  // API 不可用/被拒时 fallback 走 createPortal 覆盖应用窗口（data-viewer-immersive）。
  const viewerRootRef = useRef<HTMLDivElement>(null);
  const [immersiveMode, setImmersiveMode] = useState<ImmersiveMode>("off");
  const immersive = immersiveMode !== "off";

  const toggleImmersive = useCallback(async () => {
    if (immersiveMode === "native") {
      try {
        await document.exitFullscreen();
      } catch {
        /* 系统拒绝退出时也清本地状态（§4.1：失败也要清本地状态） */
      }
      setImmersiveMode("off");
      return;
    }
    if (immersiveMode === "fallback") {
      setImmersiveMode("off"); // 应用内覆盖层 → 退出
      return;
    }
    const ok = await requestFullscreenSafe(viewerRootRef.current);
    setImmersiveMode(ok ? "native" : "fallback");
  }, [immersiveMode]);

  // 系统级退出全屏（Esc 由浏览器接管）→ 只在 viewerRoot 处于全屏时认定 native 沉浸；
  // 其他元素进入全屏不改变 ViewerShell 模式（§4.1）。
  useEffect(() => {
    const onFsChange = () => {
      const nativeActive = isViewerNativeFullscreen(viewerRootRef.current);
      setImmersiveMode((mode) => {
        if (nativeActive) return "native";
        return mode === "native" ? "off" : mode;
      });
    };
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, []);

  // fallback 沉浸：焦点移入沉浸画布，应用根节点（不含 portal）inert；退出/卸载清理 inert（§4.1）。
  useEffect(() => {
    if (immersiveMode !== "fallback") return;
    const canvas = document.querySelector<HTMLElement>("[data-immersive-canvas]");
    canvas?.focus({ preventScroll: true });
    const appRoot = (document.getElementById("root") ?? document.body.firstElementChild) as HTMLElement | null;
    const wasInert = appRoot?.hasAttribute("inert") ?? false;
    appRoot?.setAttribute("inert", "");
    return () => {
      if (appRoot && !wasInert) appRoot.removeAttribute("inert");
    };
  }, [immersiveMode]);

  // Viewer 关闭/切页：native 模式先请求退出全屏，失败也清本地状态（§4.1）
  const handleClose = useCallback(() => {
    if (isViewerNativeFullscreen(viewerRootRef.current)) {
      void document.exitFullscreen().catch(() => undefined);
    }
    setImmersiveMode("off");
    onClose();
  }, [onClose]);

  const openAssign = () => {
    // TagAssignDialog 基于选中集工作：暂存原选中，换为当前单张，关闭时恢复
    prevSelected.current = useSelectionStore.getState().selected;
    useSelectionStore.setState({ selected: new Set([current.id]) });
    setAssignOpen(true);
  };

  const closeAssign = () => {
    setAssignOpen(false);
    if (prevSelected.current) useSelectionStore.setState({ selected: prevSelected.current });
    prevSelected.current = null;
  };

  const removeTag = async (tagId: number) => {
    try {
      await removeTags([current.id], [tagId]);
      patchLocal([current.id], { tags: current.tags.filter((t) => t.id !== tagId) });
    } catch {
      /* 失败保持现状，下次打开抽屉仍可重试 */
    }
  };

  // 进场动画
  useEffect(() => {
    const raf = requestAnimationFrame(() => setEntered(true));
    return () => cancelAnimationFrame(raf);
  }, []);

  // 切张/重试：重置视图（MediaViewport 内部按 assetId 重置）+ 加载高清（失败退原图）
  useEffect(() => {
    setSrc(null);
    setMediaFatal(false);
    let cancelled = false;
    getThumbnailUrl(current.id, "hd", 1920)
      .then((u) => !cancelled && setSrc(u))
      .catch(() => !cancelled && setSrc(toFileUrl(current.filePath)));
    return () => {
      cancelled = true;
    };
  }, [current.id, current.filePath, reloadNonce]);

  // 过片（近尾部自动翻页加载）
  const goto = useCallback(
    (next: number) => {
      if (items.length === 0) return;
      const clamped = Math.max(0, Math.min(next, items.length - 1));
      if (items[clamped]) setCurrentId(items[clamped].id);
      if (clamped >= items.length - 5 && items.length < total) void loadMore();
    },
    [items, total, loadMore],
  );

  // 切张 / 变体变化：重置视频源与代理尝试标记（§8.1 播放决策顺序：原文件优先，失败再走代理）。
  // P1-03：variant 也在依赖内——能力从默认恢复为 Linux WebM 时，取消旧请求并允许按正确变体重来。
  useEffect(() => {
    proxyAttempted.current = false;
    proxyGeneration.current += 1; // 作废在途响应
    setProxyError(null);
    setProxying(false);
    setVideoSrc(convertFileSrc(current.filePath));
  }, [current.id, current.filePath, proxyVariant]);

  // §8.1：原文件播放失败 → 按需生成兼容代理；代理失败给出可解释原因与操作建议
  const handleVideoError = useCallback(async () => {
    // P1-03：平台能力未就绪前不请求代理（避免错误默认变体）。
    if (!platformReady || proxyAttempted.current) return;
    proxyAttempted.current = true;
    const id = current.id;
    const generation = ++proxyGeneration.current;
    const requestedVariant = proxyVariant;
    setProxying(true);
    setProxyError(null);
    try {
      const proxy = await ensureVideoProxy(id, proxyVariant);
      // 素材/变体已变或有更新请求 → 丢弃旧响应，避免覆盖当前变体状态。
      if (generation !== proxyGeneration.current || proxy.variant !== requestedVariant) return;
      if (proxy.status === "ready" && proxy.path) {
        setVideoSrc(toProxyFileUrl(proxy.path));
        setProxyError(null);
      } else {
        setProxyError(proxy.error ?? "视频编码不兼容，且没有可用的兼容代理");
      }
    } catch (e) {
      if (generation !== proxyGeneration.current) return;
      setProxyError(e instanceof Error ? e.message : String(e));
    } finally {
      if (generation === proxyGeneration.current) setProxying(false);
    }
  }, [current.id, proxyVariant, platformReady]);

  const handleCancelProxy = () => {
    // ensure/cancel 使用完全相同的 variant（同一 selector 取值）。
    void cancelVideoProxy(current.id, proxyVariant).catch(() => undefined);
    setProxyError("已取消生成代理");
  };

  // 键盘：←→ 过片，Esc 退出。§4.3 忽略输入框/下拉/文本域/contentEditable/播放器根节点与子节点/模态层
  // FB5-01（§4.1）：Esc 优先级 = native（浏览器消费，fullscreenchange 同步）> fallback（退出沉浸）> 关闭查看器。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target) || isInsidePlayer(e.target)) return;
      if (assignOpen) return; // 模态打开时页面切片不响应
      if (e.key === "Escape") {
        if (document.fullscreenElement) {
          // 原生全屏：Esc 本身会被浏览器消费退出全屏；此处不关闭查看器
          return;
        }
        if (immersiveMode === "fallback") {
          setImmersiveMode("off");
          return;
        }
        onClose();
      } else if (e.key === "ArrowLeft") goto(index - 1);
      else if (e.key === "ArrowRight") goto(index + 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, goto, onClose, assignOpen, immersiveMode]);

  const isVideo = isVideoAsset(current);
  // FB3-10（§12.2）：查看器接入算法主色色条（showInViewer 开关此前无实现）。
  // 放标签栏上方；沉浸时随 tagBar 一起被 ViewerShell 卸载（不占布局）。
  const { colorStrip } = useAppearance();
  const viewerStripOn = colorStrip.enabled && colorStrip.showInViewer && !!current.palette?.length;
  const viewerSegments = useMemo(
    () => (viewerStripOn ? toPaletteSegments(current.palette) : []),
    [viewerStripOn, current.palette],
  );

  const stage = isVideo ? (
    <MediaViewport
      assetId={current.id}
      isVideo
      immersive={immersive}
      onToggleImmersive={() => void toggleImmersive()}
      fileName={current.fileName}
      video={
        <VideoPlayer
          src={videoSrc ?? ""}
          fileName={current.fileName}
          immersive={immersive}
          onToggleImmersive={() => void toggleImmersive()}
          proxying={proxying}
          proxyError={proxyError}
          onCancelProxy={handleCancelProxy}
          onRetryProxy={() => {
            proxyAttempted.current = false;
            setProxyError(null);
            void handleVideoError();
          }}
          onError={() => void handleVideoError()}
        />
      }
    />
  ) : (
    <MediaViewport
      assetId={current.id}
      isVideo={false}
      immersive={immersive}
      onToggleImmersive={() => void toggleImmersive()}
      imageSrc={src}
      imageFallbackUrl={toFileUrl(current.filePath)}
      fileName={current.fileName}
      onImageError={() => {
        // §7.5：高清图失败→回落原文件；兜底也失败→致命错误（重新加载需重新获取 URL/高清图）
        if (src && src === toFileUrl(current.filePath)) {
          setMediaFatal(true);
        } else {
          setSrc(toFileUrl(current.filePath));
        }
      }}
      fatal={mediaFatal}
      onRetryCurrent={() => setReloadNonce((n) => n + 1)}
      onBackToLibrary={handleClose}
    />
  );

  // fallback 沉浸：createPortal 到 document.body，覆盖整个应用窗口（§4.1）。
  if (immersiveMode === "fallback") {
    return createPortal(
      <div
        data-viewer-immersive
        data-immersive-canvas
        tabIndex={-1}
        className="fixed inset-0 z-[100] bg-[#000000]"
      >
        {stage}
      </div>,
      document.body,
    );
  }

  return (
    <div
      ref={viewerRootRef}
      data-viewer-immersive={immersive ? "" : undefined}
      className={clsx("h-full min-h-0 transition-all duration-200 ease-out", entered ? "opacity-100" : "opacity-0")}
    >
      <ViewerShell
        immersive={immersive}
        toolbar={
          <ViewerToolbar
            fileName={current.fileName}
            position={index >= 0 ? `${index + 1} / ${total}` : ""}
            detailsOpen={detailsOpen}
            onToggleDetails={() => setDetailsOpen((v) => !v)}
            immersive={immersive}
            onToggleImmersive={() => void toggleImmersive()}
            onClose={handleClose}
          />
        }
        sidebar={
          detailsOpen && !immersive ? (
            <ViewerInfoSidebar asset={current} onRefreshed={(a) => patchLocal([a.id], a)} />
          ) : null
        }
        tagBar={
          !immersive ? (
            <>
              {viewerStripOn && (
                <div className="shrink-0 px-4 pt-1">
                  <ColorStrip palette={viewerSegments} mode={colorStrip.mode} height={colorStrip.height} count={colorStrip.count} />
                </div>
              )}
              <ViewerTagBar
                assetId={current.id}
                tags={current.tags}
                contentDescription={current.contentDescription}
                onRemoveTag={(tid) => void removeTag(tid)}
                onAddTag={openAssign}
              />
            </>
          ) : null
        }
        stage={stage}
        filmstrip={
          !immersive ? (
            <ViewerFilmstrip
              items={items}
              currentId={currentId}
              onJump={goto}
              onPrev={() => goto(index - 1)}
              onNext={() => goto(index + 1)}
            />
          ) : null
        }
      />

      {!immersive && <TagAssignDialog open={assignOpen} onClose={closeAssign} />}
    </div>
  );
}
