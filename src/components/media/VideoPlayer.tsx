/**
 * VideoPlayer（指导书 §4.4/§4.5 + FB3-03 §5.2 + FB5-02 §5）：统一视频播放器。
 *  状态机：loading → ready → playing/paused；autoplayBlocked（自动播放被拒）；
 *          error（解码失败，携带 MediaError.code）；proxying（上层正在生成兼容代理）。
 *  功能：播放/暂停、±5 秒、进度条（自定义三层 + buffered）、当前/总时长、倍速 0.5/1/1.5/2、
 *        静音、音量、沉浸全屏（统一由 ViewerPage 状态机驱动，播放器不再请求第二套 fullscreen）。
 * FB5-02（§5.1）DOM 结构：VideoPlayer relative → video（h-full 铺满舞台）
 *   → loading/error/proxy overlay（absolute）→ VideoControls（absolute bottom 悬浮，不参与视频高度计算）。
 *  控制可见性（§5.2）：播放中 1800ms 无操作自动隐藏；暂停/loading 结束未播放/autoplayBlocked 常显；
 *  focus-within 常显；pointermove 只在 hidden→visible 时 setState，随后重置计时器。
 *  键盘作用域（焦点在播放器根节点）：
 *   ArrowLeft/Right = seek ±5s；Space/K = 播放/暂停；M = 静音；F = 沉浸全屏。
 *  事件执行后 stopPropagation()；仅在执行 seek/play 快捷键时 preventDefault()。
 * FB5-01（§4.4）：immersive/onToggleImmersive 来自父级；F 键与全屏按钮都进入 Viewer 统一沉浸。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { Loader2, Play } from "lucide-react";
import VideoControls, { fmtTime } from "@/components/media/VideoControls";

export type VideoPlayerStatus =
  | "loading"
  | "ready"
  | "playing"
  | "paused"
  | "autoplayBlocked"
  | "error"
  | "proxying";

/** §6.2 视频测量快照：canPlayType / 错误码 / 网络与就绪状态 / 关键事件时间。 */
export interface VideoMetrics {
  canPlayType: Record<string, boolean>;
  errorCode?: number;
  networkState?: number;
  readyState?: number;
  loadedmetadataMs?: number;
  canplayMs?: number;
  stalled?: boolean;
  waiting?: boolean;
}

interface VideoPlayerProps {
  src: string;
  fileName: string;
  className?: string;
  /** FB5-01（§4.4）：统一沉浸状态（ViewerPage 状态机），播放器不再有自己的 fullscreen */
  immersive?: boolean;
  /** FB5-01（§4.4）：进入/退出沉浸（F 键与全屏按钮调用） */
  onToggleImmersive?: () => void;
  onMetrics?: (m: VideoMetrics) => void;
  /** 原文件播放失败回调（携带 MediaError.code），供上层决定是否生成兼容代理。 */
  onError?: (code?: number) => void;
  /** 上层正在生成兼容代理（显示处理中 + 取消入口）。 */
  proxying?: boolean;
  /** 代理失败原因（显示重试入口）。 */
  proxyError?: string | null;
  onCancelProxy?: () => void;
  onRetryProxy?: () => void;
}

function errorText(code?: number): string {
  switch (code) {
    case 1:
      return "播放被中止";
    case 2:
      return "发生网络错误";
    case 3:
      return "解码失败（编码可能不受支持）";
    case 4:
      return "格式不受支持";
    default:
      return "无法播放该视频";
  }
}

/** 播放中自动隐藏控制条的静默时长（§3.4） */
const HIDE_CONTROLS_MS = 1800;

export default function VideoPlayer({
  src,
  fileName,
  className,
  immersive = false,
  onToggleImmersive,
  onMetrics,
  onError,
  proxying = false,
  proxyError = null,
  onCancelProxy,
  onRetryProxy,
}: VideoPlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<VideoPlayerStatus>("loading");
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [bufferedEnd, setBufferedEnd] = useState(0);
  const [rate, setRate] = useState(1);
  const [muted, setMuted] = useState(true);
  const [volume, setVolume] = useState(1);
  const timeRef = useRef({ loadedmetadata: 0, canplay: 0 });
  const lastErrorCode = useRef<number | undefined>(undefined);

  // FB5-02（§5.2）：控制可见性 —— 播放中无操作自动隐藏，暂停/未播放/焦点内常显
  const [controlsActive, setControlsActive] = useState(true);
  const [focusWithin, setFocusWithin] = useState(false);
  const hideTimerRef = useRef<number | null>(null);

  const clearHideTimer = useCallback(() => {
    if (hideTimerRef.current != null) {
      window.clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
  }, []);

  /** 显示控制条并（仅播放中）安排自动隐藏计时器 */
  const showControlsTemporarily = useCallback(() => {
    setControlsActive(true);
    clearHideTimer();
    if (status === "playing") {
      hideTimerRef.current = window.setTimeout(() => setControlsActive(false), HIDE_CONTROLS_MS);
    }
  }, [status, clearHideTimer]);

  // 卸载 / src 变化：清自动隐藏计时器
  useEffect(() => clearHideTimer, [clearHideTimer]);

  // 进入播放：立即安排自动隐藏（§5.2）；离开播放清计时器
  useEffect(() => {
    if (status !== "playing") {
      clearHideTimer();
      return;
    }
    if (!controlsActive) return;
    hideTimerRef.current = window.setTimeout(() => setControlsActive(false), HIDE_CONTROLS_MS);
    return clearHideTimer;
  }, [status, controlsActive, clearHideTimer]);

  const controlsVisible = status !== "playing" || controlsActive || focusWithin;

  const canPlayType = useCallback(() => {
    const v = videoRef.current;
    if (!v) return {};
    const probes = [
      "video/mp4",
      'video/mp4; codecs="avc1.42E01E"',
      'video/webm; codecs="vp9"',
      "video/quicktime",
      "video/x-matroska",
      'video/mp4; codecs="hev1"',
    ];
    const out: Record<string, boolean> = {};
    for (const t of probes) out[t] = v.canPlayType(t) !== "";
    return out;
  }, []);

  const emitMetrics = useCallback(
    (patch: Partial<VideoMetrics> = {}) => {
      if (!onMetrics) return;
      const v = videoRef.current;
      onMetrics({
        canPlayType: canPlayType(),
        errorCode: v?.error?.code,
        networkState: v?.networkState,
        readyState: v?.readyState,
        loadedmetadataMs: timeRef.current.loadedmetadata || undefined,
        canplayMs: timeRef.current.canplay || undefined,
        ...patch,
      });
    },
    [onMetrics, canPlayType],
  );

  // 尝试自动播放（原 mute 自动播放；被拒 → autoplayBlocked 而非静默吞错）
  const tryAutoplay = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    const p = v.play();
    // 某些环境（jsdom/旧 WebView）play() 同步返回 undefined：无 Promise 可等，视为已发起
    if (!p || typeof p.then !== "function") {
      setStatus("playing");
      return;
    }
    p.then(() => {
      setStatus((s) => (s === "autoplayBlocked" ? s : "playing"));
    }).catch((e: unknown) => {
      const name = e instanceof DOMException ? e.name : "";
      if (name === "NotAllowedError" || name === "AbortError") {
        setStatus("autoplayBlocked");
      } else {
        setStatus("paused");
      }
    });
  }, []);

  const togglePlay = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) {
      const p = v.play();
      if (!p || typeof p.then !== "function") {
        setStatus("playing");
        return;
      }
      p.then(() => setStatus("playing")).catch(() => setStatus("autoplayBlocked"));
    } else {
      v.pause();
      setStatus("paused");
    }
  }, []);

  const seekDelta = useCallback((delta: number) => {
    const v = videoRef.current;
    if (!v) return;
    v.currentTime = Math.max(0, Math.min(Number.isFinite(v.duration) ? v.duration : v.currentTime, v.currentTime + delta));
  }, []);

  const toggleMute = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    v.muted = !v.muted;
    setMuted(v.muted);
  }, []);

  const changeVolume = useCallback(
    (next: number) => {
      const v = videoRef.current;
      if (!v) return;
      const val = Math.max(0, Math.min(1, next));
      v.volume = val;
      setVolume(val);
      if (val > 0 && v.muted) {
        v.muted = false;
        setMuted(false);
      }
    },
    [],
  );

  // §4.4 键盘作用域：焦点在播放器根节点时接管快捷键；执行后 stopPropagation，
  // 只有 seek/play 快捷键 preventDefault（防止页面级 ←→ 切素材、Space 滚动）。
  // FB5-01（§4.4）：F = 进入统一沉浸（不再 requestFullscreen）。
  const onRootKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t instanceof HTMLInputElement || t instanceof HTMLSelectElement || t instanceof HTMLTextAreaElement) return;
      const handled = ["ArrowLeft", "ArrowRight", " ", "k", "K", "m", "M", "f", "F"].includes(e.key);
      if (!handled) return;
      // 快捷键统一不回传查看器（播放器优先级高于页面切片）
      e.stopPropagation();
      switch (e.key) {
        case "ArrowLeft":
          e.preventDefault();
          seekDelta(-5);
          break;
        case "ArrowRight":
          e.preventDefault();
          seekDelta(5);
          break;
        case " ":
        case "k":
        case "K":
          e.preventDefault();
          togglePlay();
          break;
        case "m":
        case "M":
          toggleMute();
          break;
        case "f":
        case "F":
          onToggleImmersive?.();
          break;
      }
    },
    [seekDelta, togglePlay, toggleMute, onToggleImmersive],
  );

  // 鼠标移动：只有 hidden -> visible 时 setState，随后重置计时器（§5.2 防高频重渲染）
  const onRootPointerMove = useCallback(() => {
    if (status !== "playing") return; // 非播放中不运行自动隐藏
    if (!controlsActive) {
      showControlsTemporarily();
    } else {
      // 已可见：只重置计时器，不触发 setState
      clearHideTimer();
      hideTimerRef.current = window.setTimeout(() => setControlsActive(false), HIDE_CONTROLS_MS);
    }
  }, [status, controlsActive, showControlsTemporarily, clearHideTimer]);

  const onRootFocus = useCallback(() => {
    setFocusWithin(true);
    showControlsTemporarily();
  }, [showControlsTemporarily]);

  const onRootBlur = useCallback(
    (e: React.FocusEvent) => {
      const next = e.relatedTarget as HTMLElement | null;
      if (next && boxRef.current?.contains(next)) return; // 焦点仍在播放器内
      setFocusWithin(false);
    },
    [],
  );

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onErr = () => {
      const code = v.error?.code;
      lastErrorCode.current = code;
      setStatus("error");
      emitMetrics({ errorCode: code });
      onError?.(code);
    };
    const onLoaded = () => {
      setDuration(Number.isFinite(v.duration) ? v.duration : 0);
      timeRef.current.loadedmetadata = performance.now();
      emitMetrics({ loadedmetadataMs: timeRef.current.loadedmetadata });
      setStatus((s) => (s === "proxying" ? s : "ready"));
    };
    const onCanplay = () => {
      timeRef.current.canplay = performance.now();
      emitMetrics({ canplayMs: timeRef.current.canplay });
    };
    const onStalled = () => emitMetrics({ stalled: true });
    const onWaiting = () => emitMetrics({ waiting: true });
    const onTime = () => setCurrentTime(v.currentTime);
    const onPlay = () => setStatus("playing");
    const onPause = () => setStatus((s) => (s === "playing" || s === "ready" ? "paused" : s));
    const onEnded = () => setStatus("paused");
    // FB5-02（§5.3）：progress 读取最后一个 buffered 区间 → bufferedEnd
    const onProgress = () => {
      const d = v.duration;
      if (!Number.isFinite(d) || d <= 0) {
        setBufferedEnd(0);
        return;
      }
      const t = v.buffered.length > 0 ? v.buffered.end(v.buffered.length - 1) : 0;
      setBufferedEnd(Number.isFinite(t) ? Math.min(t, d) : 0);
    };
    v.addEventListener("error", onErr);
    v.addEventListener("loadedmetadata", onLoaded);
    v.addEventListener("canplay", onCanplay);
    v.addEventListener("stalled", onStalled);
    v.addEventListener("waiting", onWaiting);
    v.addEventListener("timeupdate", onTime);
    v.addEventListener("play", onPlay);
    v.addEventListener("pause", onPause);
    v.addEventListener("ended", onEnded);
    v.addEventListener("progress", onProgress);
    // mount 时上报一次支持矩阵 + 初始状态
    emitMetrics();
    return () => {
      v.removeEventListener("error", onErr);
      v.removeEventListener("loadedmetadata", onLoaded);
      v.removeEventListener("canplay", onCanplay);
      v.removeEventListener("stalled", onStalled);
      v.removeEventListener("waiting", onWaiting);
      v.removeEventListener("timeupdate", onTime);
      v.removeEventListener("play", onPlay);
      v.removeEventListener("pause", onPause);
      v.removeEventListener("ended", onEnded);
      v.removeEventListener("progress", onProgress);
    };
  }, [emitMetrics, onError]);

  // src 变化：重置状态并重新加载，切换素材不残留上一段错误/时间
  useEffect(() => {
    setStatus("loading");
    setCurrentTime(0);
    setDuration(0);
    setBufferedEnd(0);
    lastErrorCode.current = undefined;
    setControlsActive(true);
    const v = videoRef.current;
    if (v) {
      try {
        v.load();
      } catch {
        /* 忽略 */
      }
    }
  }, [src]);

  // 自动播放：muted 首帧尝试（loadedmetadata 后再 play，稳妥）
  useEffect(() => {
    if (status !== "ready") return;
    const timer = window.setTimeout(() => tryAutoplay(), 0);
    return () => window.clearTimeout(timer);
  }, [status, tryAutoplay]);

  const durationKnown = Number.isFinite(duration) && duration > 0;
  const durationLabel = durationKnown ? fmtTime(duration) : "--:--";

  return (
    <div
      ref={boxRef}
      tabIndex={0}
      role="group"
      aria-label="视频播放器"
      data-player-root
      onKeyDown={onRootKeyDown}
      onPointerMove={onRootPointerMove}
      onFocus={onRootFocus}
      onBlur={onRootBlur}
      className={clsx(
        "relative h-full w-full min-h-0 min-w-0 bg-black/95 outline-none focus-visible:ring-1 focus-visible:ring-[var(--color-status)]",
        className,
      )}
    >
      {/* 视频铺满舞台；object-contain 不拉伸（FB5-02：不再 flex-col 给控制条留高度） */}
      <video
        ref={videoRef}
        src={src}
        autoPlay
        muted
        playsInline
        onClick={(e) => {
          e.stopPropagation();
          togglePlay();
        }}
        className="h-full w-full object-contain"
      />

      {/* 加载中 */}
      {status === "loading" && !proxying && (
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/60">
          <Loader2 size={20} className="animate-spin text-[var(--color-text-secondary)]" aria-hidden="true" />
          <p className="text-xs text-[var(--color-text-secondary)]">正在加载视频…</p>
        </div>
      )}

      {/* 自动播放被拒：点击播放，不静默吞错 */}
      {status === "autoplayBlocked" && !proxying && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            togglePlay();
          }}
          aria-label="点击播放"
          className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/50 text-[var(--color-text)] transition-colors hover:bg-black/40"
        >
          <Play size={28} strokeWidth={1.75} aria-hidden="true" />
          <span className="text-sm">点击播放</span>
        </button>
      )}

      {/* 解码失败 */}
      {status === "error" && !proxying && !proxyError && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/70 px-4 text-center">
          <p className="text-sm text-[var(--color-danger)]">⚠ 播放失败</p>
          {/* M-13：失败态也要能 hover 看到完整文件名，否则长中文名截断后无法确认是哪个文件 */}
          <p className="max-w-full truncate text-xs text-[var(--color-text-secondary)]" title={fileName}>{fileName}</p>
          <p className="text-xs text-[var(--color-text-secondary)]">{errorText(lastErrorCode.current)}</p>
          <p className="text-[10px] text-[var(--color-text-tertiary)]">错误码 {lastErrorCode.current ?? "--"}</p>
        </div>
      )}

      {/* 正在生成兼容代理 */}
      {proxying && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/70 px-4 text-center">
          <Loader2 size={20} className="animate-spin text-[var(--color-text-secondary)]" aria-hidden="true" />
          <p className="text-xs text-[var(--color-text-secondary)]">正在生成兼容代理（H.264）…</p>
          {onCancelProxy && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onCancelProxy();
              }}
              className="rounded bg-[var(--color-surface-hover)] px-2 py-1 text-xs text-[var(--color-text)]"
            >
              取消
            </button>
          )}
        </div>
      )}

      {/* 代理失败：原因 + 重试 */}
      {proxyError && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/70 px-4 text-center">
          <p className="max-w-full truncate text-xs text-[var(--color-danger)]">{proxyError}</p>
          <p className="text-xs text-[var(--color-text-secondary)]">
            建议：安装 HEVC 视频扩展，或改用 H.264/AAC 编码的原文件后重新打开。
          </p>
          {onRetryProxy && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onRetryProxy();
              }}
              className="rounded bg-[var(--color-surface-hover)] px-2 py-1 text-xs text-[var(--color-text)]"
            >
              重试生成兼容代理
            </button>
          )}
        </div>
      )}

      {/* 悬浮控制条：播放中无操作自动隐藏；暂停/加载/错误时按 controlsVisible 常显（§5.2）。
          overlay 显示时不运行自动隐藏（controlsVisible 已因 status!=="playing" 保持 true） */}
      <div
        className="absolute inset-x-0 bottom-0 flex justify-center pb-4 transition-opacity duration-200"
        style={{ opacity: controlsVisible ? 1 : 0, pointerEvents: controlsVisible ? "auto" : "none" }}
        aria-hidden={!controlsVisible}
      >
          <VideoControls
            playing={status === "playing"}
            currentTime={currentTime}
            duration={duration}
            durationKnown={durationKnown}
            bufferedEnd={bufferedEnd}
            rate={rate}
            muted={muted}
            volume={volume}
            immersive={immersive}
            onTogglePlay={togglePlay}
            onSeekDelta={seekDelta}
            onSeekTo={(t) => {
              const v = videoRef.current;
              if (v) v.currentTime = t;
              setCurrentTime(t);
            }}
            onRate={(r) => {
              const v = videoRef.current;
              if (v) v.playbackRate = r;
              setRate(r);
            }}
            onToggleMute={toggleMute}
            onVolume={changeVolume}
            onToggleImmersive={() => onToggleImmersive?.()}
          />
      </div>

      {/* 无障碍时间信息（duration 未知时提示加载文案） */}
      {!durationKnown && status !== "loading" && (
        <div className="sr-only">{durationLabel}</div>
      )}
    </div>
  );
}
