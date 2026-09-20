/**
 * FB6 需求一：AI 打标页内进度块（当前批次区块专用）。
 *  - 唯一 role="progressbar"：复用 <ProgressBar>，total<=0 或 starting 阶段走不确定态（不出 NaN%）；
 *  - LED 滚动提示不是第二条进度条：20px 固定高度、单行省略，只有运行/启动阶段滚动，
 *    取消/完成/失败显示静态最终状态（动画随之停止）；
 *  - 完整文案由 aria-live="polite" 的节点提供；运行中该节点 sr-only（滚动视觉副本 aria-hidden，
 *    字符不被拆成不可读的无障碍文本），prefers-reduced-motion 下静态节点转可见、滚动副本隐藏；
 *  - 纯 CSS @keyframes，无 setInterval / requestAnimationFrame，卸载无残留计时器。
 */
import clsx from "clsx";
import ProgressBar from "@/components/common/ProgressBar";
import { displayBasename } from "@/utils/pathDisplay";
import type { AiTaggingUiState } from "@/types/ai";

export function aiLedText(state: AiTaggingUiState, currentAssetName?: string | null): string | null {
  switch (state.phase) {
    case "idle":
      return null;
    case "starting":
      return "正在连接 AI 服务，请稍候 · 不会卡住";
    case "running": {
      const { processed, total } = state;
      if (!(total > 0)) return "正在分析素材 · 请勿关闭窗口";
      const at = Math.min(processed + 1, total);
      const target = currentAssetName ? `「${currentAssetName}」` : "";
      return `正在分析${target}第 ${at} / ${total} 张素材 · 请勿关闭窗口`;
    }
    case "cancelling":
      return "取消已受理，当前图片完成后停止";
    case "done":
      return `打标结束 · 已处理 ${state.processed} / ${state.total}`;
    case "error":
      return `打标失败：${state.message}`;
  }
}

/** 只有 starting/running 需要滚动（确定性文案动画）；其余阶段静态展示。 */
function isScrollingPhase(state: AiTaggingUiState): boolean {
  return state.phase === "starting" || state.phase === "running";
}

/** 由 assetPath 取文件名（页面层使用，避免组件内重复拆分逻辑）。
 *  X-18：分隔符按平台判定，Unix 反斜杠是合法文件名字符，不能一律当分隔符。 */
export function assetFileLabel(path: string): string {
  return displayBasename(path);
}

export default function AiTaggingProgress({
  state,
  currentAssetName,
  className,
}: {
  state: AiTaggingUiState;
  /** 当前正在分析的素材展示名（由调用方按 currentAssetId 从 suggestions 查找） */
  currentAssetName?: string | null;
  className?: string;
}) {
  if (state.phase === "idle") return null;
  const indeterminate =
    state.phase === "starting" ||
    ((state.phase === "running" || state.phase === "cancelling") && !(state.total > 0));
  const value =
    state.phase === "running" || state.phase === "cancelling" || state.phase === "done" || state.phase === "error"
      ? Math.min(state.processed, Math.max(0, state.total)) / Math.max(1, state.total)
      : 0;
  const text = aiLedText(state, currentAssetName);
  const scrolling = isScrollingPhase(state);
  const failed = state.phase === "error";

  return (
    <div className={className}>
      <ProgressBar value={value} indeterminate={indeterminate} />
      {text && (
        <div className="mt-1 h-5 overflow-hidden" data-testid="ai-led-hint">
          {/* 完整静态文案：始终 aria-live 播报；滚动阶段 sr-only（视觉由滚动副本承担），
              reduced-motion 下经 .ai-marquee-static 转为可见。 */}
          <p
            aria-live="polite"
            className={clsx(
              "ai-marquee-static text-[10px] leading-5",
              scrolling && "sr-only",
              failed ? "text-[var(--color-danger)]" : "text-[var(--color-text-secondary)]",
            )}
          >
            {text}
          </p>
          {scrolling && (
            <div className="ai-marquee-track" aria-hidden="true">
              <span className="pr-12">{text}</span>
              <span className="pr-12">{text}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
