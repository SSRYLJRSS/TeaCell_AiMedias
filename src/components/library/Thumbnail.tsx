/** 双层缩略图：占位图立即显示，进入可见区后按需生成高清（PRD R-01）。
 *  §9.2/§9.3：高清 URL 走模块级缓存（虚拟滚动重挂载不复位、不重复淡入）；
 *  组件用 generation 做代际保护，卸载/切张后旧 Promise 不更新当前素材。
 */
import { memo, useEffect, useRef, useState } from "react";
import { toFileUrl } from "@/api/thumbnail";
import { getCachedThumbnail, requestThumbnail } from "@/utils/thumbnailCache";
import { markStartup } from "@/utils/startupMarks";

/** §4.1：首个高清缩略图 ready 标记（模块级，跨组件只打一次） */
let firstThumbMarked = false;

interface ThumbnailProps {
  assetId: number;
  placeholderPath: string | null;
  alt: string;
  /** FB2-01 请求尺寸（由格子档位决定；分级为 512/1024 保证缓存命中） */
  size: number;
  /** FB2-02 填充方式（已由调用方经 resolveFit 解析为二态） */
  fit: "cover" | "contain";
}

export default memo(function Thumbnail({ assetId, placeholderPath, alt, size, fit }: ThumbnailProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  // 初始状态直接读缓存：命中 ready → 立即显示（hdReady 为 true，跳过「从 0 到 1」淡入）
  const cached = getCachedThumbnail(assetId, "hd", size);
  const [hdUrl, setHdUrl] = useState<string | null>(cached?.status === "ready" ? cached.url ?? null : null);
  const [hdReady, setHdReady] = useState<boolean>(cached?.status === "ready");
  const [placeholderUrl, setPlaceholderUrl] = useState<string | null>(
    placeholderPath ? toFileUrl(placeholderPath) : null,
  );
  const [placeholderFailed, setPlaceholderFailed] = useState(false); // B27：占位图加载失败标记

  // §9.3：代际保护——每次 assetId/size 变化或卸载都推进 generation，旧 Promise 不得写状态/缓存
  const gen = useRef(0);

  // 占位图路径后到时补转 URL（入库刚完成的行）
  useEffect(() => {
    if (!placeholderUrl && placeholderPath) setPlaceholderUrl(toFileUrl(placeholderPath));
  }, [placeholderPath, placeholderUrl]);

  // B27：占位图加载失败 → 触发 hd 生成（原逻辑 hd 只在 hdUrl 为 null 时触发，现补充占位图失败也触发）
  useEffect(() => {
    if (placeholderFailed && !hdUrl) {
      const my = ++gen.current;
      requestThumbnail(assetId, "hd", size)
        .then((url) => {
          if (gen.current !== my) return;
          setHdUrl(url);
          setHdReady(true);
        })
        .catch(() => undefined);
    }
  }, [placeholderFailed, hdUrl, assetId, size]);

  // 可见区触发高清生成（IntersectionObserver；requestThumbnail 内部对同一 asset+size 去重）
  useEffect(() => {
    const el = rootRef.current;
    if (!el || hdUrl) return;
    const my = ++gen.current;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          requestThumbnail(assetId, "hd", size)
            .then((url) => {
              if (gen.current !== my) return;
              setHdUrl(url);
              setHdReady(true);
            })
            .catch(() => undefined); // 高清失败保留占位图
        }
      },
      { rootMargin: "200px" }, // 提前一屏预生成，滚动无感
    );
    io.observe(el);
    return () => {
      io.disconnect();
      // gen 是"代际"守卫：cleanup 里自增即作废本效果发起的在途请求（.then 内以 gen.current !== my 拦截）。
      // 这里刻意读写 gen.current 本身，不适用"复制到局部变量"的告警建议。
      // eslint-disable-next-line react-hooks/exhaustive-deps
      gen.current++;
    };
  }, [assetId, hdUrl, size]);

  // §4.1：首个高清缩略图 ready 打点（模块级只打一次，首屏性能观测）
  return (
    <div ref={rootRef} className="absolute inset-0 bg-[var(--color-surface)]">
      {placeholderUrl && !placeholderFailed ? (
        <img
          src={placeholderUrl}
          alt={alt}
          draggable={false}
          onError={() => setPlaceholderFailed(true)} // B27：占位图失败回退到 pulse 占位
          className={`absolute inset-0 h-full w-full ${fit === "contain" ? "object-contain" : "object-cover"}`}
        />
      ) : (
        <div className="absolute inset-0 animate-pulse bg-[var(--color-border)]" />
      )}
      {hdUrl && (
        <img
          src={hdUrl}
          alt=""
          aria-hidden
          draggable={false}
          onLoad={() => {
            setHdReady(true);
            if (!firstThumbMarked) {
              firstThumbMarked = true;
              markStartup("first_thumbnail_ready");
            }
          }}
          className={`absolute inset-0 h-full w-full ${fit === "contain" ? "object-contain" : "object-cover"} transition-opacity duration-300`}
          style={{ opacity: hdReady ? 1 : 0 }}
        />
      )}
    </div>
  );
});
