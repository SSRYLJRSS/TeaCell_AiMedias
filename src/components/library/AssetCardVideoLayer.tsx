/** FB2-03 视频悬停原位播放层（素材库用）。放独立组件，不把播放逻辑塞进 AssetCard。
 *  素材库是「已入库」资源，播放失败可放心调 ensureVideoProxy 生成 H.264 代理后重试；
 *  代理未就绪期间静默保持封面（hover 预览不值得弹 loading，§12.5）。
 *  回归护栏：只渲染 absolute inset-0 <video>，不制造 fixed 浮层，不新增 requestThumbnail（§12.3）。 */
import { memo, useCallback, useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { toFileUrl } from "@/api/thumbnail";
import { ensureVideoProxy, toProxyFileUrl } from "@/api/video";
import { useHoverPreviewPlayback } from "@/hooks/useHoverPreviewPlayback";
import { acquireVideoSlot } from "@/utils/videoSlot";
import { usePlatformStore, selectPreferredVideoProxy } from "@/stores/platformStore";
import type { Asset } from "@/types/asset";

interface AssetCardVideoLayerProps {
  asset: Asset;
  previewSeconds: number;
}

export default memo(function AssetCardVideoLayer({ asset, previewSeconds }: AssetCardVideoLayerProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  // 初始用原文件 URL；播放失败（常见 HEVC/不兼容编码）→ 换代理（每张只尝试一次）。
  // 代理变体从平台 store 统一取（Linux 用 WebM，其余 MP4），不硬编码。
  const proxyVariant = usePlatformStore(selectPreferredVideoProxy);
  // P1-03：平台能力就绪前不发起 ensure——否则 Linux 会在能力返回前先按默认 h264_mp4 请求，
  // 生成/复用错误变体。未就绪时静默保持封面即可。
  const platformReady = usePlatformStore((s) => s.status === "ready");
  const [src, setSrc] = useState<string>(() => toFileUrl(asset.filePath));
  const [failed, setFailed] = useState(false);
  // P1-03：以 asset + variant 为键复位「已尝试」；variant 变化（如能力从默认恢复为 Linux WebM）
  // 必须重置，允许按正确变体重新请求。generation 递增用于丢弃旧变体的迟到响应。
  const proxyAttempted = useRef(false);
  const requestGeneration = useRef(0);

  useEffect(() => {
    proxyAttempted.current = false;
    requestGeneration.current += 1; // 作废此前变体/素材的在途响应
    setFailed(false);
    setSrc(toFileUrl(asset.filePath));
  }, [asset.id, asset.filePath, proxyVariant]);

  // 挂载即抢占全局唯一位；卸载让出
  useEffect(() => {
    return acquireVideoSlot(`lib:${asset.id}`, () => halt(videoRef.current));
  }, [asset.id]);

  const onFail = useCallback(() => {
    setFailed(true);
    // 平台能力未就绪前不请求代理，避免按错误默认变体生成/复用。
    if (!platformReady || proxyAttempted.current) return;
    proxyAttempted.current = true;
    const generation = ++requestGeneration.current;
    const requestedVariant = proxyVariant;
    void ensureVideoProxy(asset.id, proxyVariant)
      .then((p) => {
        // P1-03：素材/变体已变或有更新请求 → 丢弃旧响应，避免 H.264 响应覆盖 WebM 状态。
        if (generation !== requestGeneration.current) return;
        if (p.variant !== requestedVariant) return;
        if (p.status === "ready" && p.path) {
          setSrc(toProxyFileUrl(p.path));
          setFailed(false);
        }
        // 未就绪：静默保持封面（不弹加载态，见 §12.5）
      })
      .catch(() => undefined);
  }, [asset.id, proxyVariant, platformReady]);

  useHoverPreviewPlayback(videoRef, { previewSeconds, onFailed: onFail });

  return (
    <video
      ref={videoRef}
      src={src}
      autoPlay
      muted
      playsInline
      preload="metadata"
      aria-hidden="true"
      onError={() => setFailed(true)}
      onCanPlay={() => setFailed(false)}
      className={clsx(
        "absolute inset-0 h-full w-full object-cover transition-opacity duration-100",
        failed && "pointer-events-none opacity-0",
      )}
    />
  );
});

/** 让被抢占方立即静默停下播放（清 src + load 触发主动释放语义）。 */
function halt(v: HTMLVideoElement | null): void {
  if (!v) return;
  v.pause();
  v.removeAttribute("src");
  void v.load();
}
