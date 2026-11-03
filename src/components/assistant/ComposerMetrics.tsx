/** 输入卡片下方的一行统计（A3-04 输入区改版）。
 *
 * 三段排布，从左到右信息密度递增：
 * - 关联素材数：永远可算，来自 `attachments.length`；
 * - 本会话累计消耗与缓存命中率：来自 Agent 网关回传的 usage；
 * - 上下文窗口占用：环形进度 + 百分比。
 *
 * 后两段没有数据源时**整块不渲染**：不显示「—」、不显示 0%，也不做估算。
 * `docs/UI_DESIGN_SYSTEM.md` §7 明确禁止「将不确定进度伪装成百分比」，
 * 而用量、缓存命中只有上游 usage 才知道真实值。
 */

export interface ComposerUsage {
  /** 本会话累计输入 + 输出 token。 */
  totalTokens: number;
  /** 命中上游缓存的输入 token 占比，0–1。 */
  cacheHitRatio: number;
  /** 已占用的上下文窗口比例，0–1。 */
  contextRatio: number;
}

const RING_SIZE = 16;
const RING_RADIUS = 6.5;
const RING_STROKE = 2;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

/** 11px 的字号放不下 5 位以上数字，大数折算成 k/M；不四舍五入到失真。 */
export function formatTokens(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "0";
  if (value < 1000) return String(Math.round(value));
  if (value < 1_000_000) {
    const thousands = value / 1000;
    return `${thousands < 100 ? thousands.toFixed(1) : Math.round(thousands)}k`;
  }
  const millions = value / 1_000_000;
  return `${millions < 100 ? millions.toFixed(1) : Math.round(millions)}M`;
}

/** 比例先夹到 0–1，再取整成百分比；环和文字用同一个数，不出现「环 23% 文字 24%」。 */
function percentOf(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0;
  return Math.round(Math.min(Math.max(ratio, 0), 1) * 100);
}

function ContextRing({ percent }: { percent: number }) {
  const offset = RING_CIRCUMFERENCE * (1 - percent / 100);
  return (
    <svg
      className="cmp-ring"
      width={RING_SIZE}
      height={RING_SIZE}
      viewBox={`0 0 ${RING_SIZE} ${RING_SIZE}`}
      aria-hidden="true"
      focusable="false"
    >
      <circle
        className="cmp-ring__track"
        cx={RING_SIZE / 2}
        cy={RING_SIZE / 2}
        r={RING_RADIUS}
        strokeWidth={RING_STROKE}
      />
      <circle
        className="cmp-ring__value"
        cx={RING_SIZE / 2}
        cy={RING_SIZE / 2}
        r={RING_RADIUS}
        strokeWidth={RING_STROKE}
        strokeDasharray={RING_CIRCUMFERENCE}
        strokeDashoffset={offset}
      />
    </svg>
  );
}

export default function ComposerMetrics({
  attachmentCount,
  usage,
}: {
  attachmentCount: number;
  /** 缺省表示上游还没回传用量：此时只显示素材数，不猜数字。 */
  usage?: ComposerUsage | null;
}) {
  const totalTokens = usage ? formatTokens(usage.totalTokens) : null;
  const cacheHit = usage ? percentOf(usage.cacheHitRatio) : null;
  const context = usage ? percentOf(usage.contextRatio) : null;

  return (
    <div className="cmp-metrics">
      <span>{attachmentCount} 个素材</span>
      {usage && cacheHit !== null && context !== null && (
        <>
          <span className="cmp-metrics__usage">
            <span>{totalTokens} tok</span>
            <span className="cmp-metrics__sep" aria-hidden="true">
              ·
            </span>
            <span>缓存命中 {cacheHit}%</span>
          </span>
          <span className="cmp-metrics__ctx" title={`上下文窗口已占用 ${context}%`}>
            <ContextRing percent={context} />
            <span>{context}%</span>
          </span>
        </>
      )}
    </div>
  );
}
