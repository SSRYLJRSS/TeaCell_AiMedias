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
import { selectPreferredVideoProxy, usePlatformStore } from "@/stores/platformStore";
import type { Asset } from "@/types/asset";

interface AssetCardVideoLayerProps {
  asset: Asset;
  previewSeconds: number;
}

export default memo(function AssetCardVideoLayer({ asset, previewSeconds }: AssetCardVideoLayerProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const proxyVariant = usePlatformStore(selectPreferredVideoProxy);
  const platformReady = usePlatformStore((state) => state.status === "ready");
  // 初始用原文件 URL；播放失败（常见 HEVC/不兼容编码）→ 换 H.264 代理（每张只尝试一次）
  const [src, setSrc] = useState<string>(() => toFileUrl(asset.filePath));
  const [failed, setFailed] = useState(false);
  const proxyAttempted = useRef(false);

  useEffect(() => {
    proxyAttempted.current = false;
    setFailed(false);
    setSrc(toFileUrl(asset.filePath));
  }, [asset.id, asset.filePath, proxyVariant]);

  // 挂载即抢占全局唯一位；卸载让出
  useEffect(() => {
    return acquireVideoSlot(`lib:${asset.id}`, () => halt(videoRef.current));
  }, [asset.id]);

  const onFail = useCallback(() => {
    setFailed(true);
    if (!platformReady || proxyAttempted.current) return;
    proxyAttempted.current = true;
    void ensureVideoProxy(asset.id, proxyVariant).then((p) => {
      if (p.status === "ready" && p.path) {
        setSrc(toProxyFileUrl(p.path));
        setFailed(false);
      }
      // 未就绪：静默保持封面（不弹加载态，见 §12.5）
    }).catch(() => undefined);
  }, [asset.id, platformReady, proxyVariant]);

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
