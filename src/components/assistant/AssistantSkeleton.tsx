/** 助手页首屏骨架（WF-1 P0-2 / WP1.4）。
 *
 * 路由懒加载或 Agent 服务启动期间先给结构化占位，避免「白屏 → 突然整页出现」。
 * 只表达布局（左历史 / 中对话 / 底部输入），不伪造对话内容或进度百分比。
 *
 * - 默认整页布局，直接作为 `<Suspense fallback>` 使用；
 * - `label`：在对话区顶部显示可见的状态文案（如「Agent 服务正在启动…」）；
 * - `compact`：只渲染对话区，供「真实历史栏已经在旁边」的空态启动占位复用，
 *   避免出现两份并排的占位侧栏。
 */
export default function AssistantSkeleton({
  label,
  compact = false,
}: {
  label?: string;
  compact?: boolean;
}) {
  const center = (
    <div className="flex min-w-0 flex-1 flex-col">
      {label && (
        <p role="status" className="px-4 pt-4 text-xs text-[var(--color-text-tertiary)]">
          {label}
        </p>
      )}
      <div className="flex-1 space-y-4 overflow-hidden px-4 py-6">
        <div className="mx-auto w-full max-w-[760px] space-y-4">
          <div className="ml-auto h-9 w-2/5 rounded-[var(--radius-control)] bg-[var(--color-surface-hover)]" />
          <div className="h-4 w-3/5 rounded bg-[var(--color-surface-hover)]" />
          <div className="h-4 w-4/5 rounded bg-[var(--color-surface-hover)]" />
          <div className="h-4 w-1/2 rounded bg-[var(--color-surface-hover)]" />
        </div>
      </div>
      <div className="p-4">
        <div className="mx-auto h-20 w-full max-w-[760px] rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)]" />
      </div>
    </div>
  );

  if (compact) {
    return (
      <div
        className="flex min-w-0 flex-1 animate-pulse"
        aria-busy="true"
        aria-label={label ?? "正在打开对话"}
      >
        {center}
      </div>
    );
  }

  return (
    <div className="flex flex-1 animate-pulse" aria-busy="true" aria-label={label ?? "正在打开对话"}>
      <div className="flex w-[248px] shrink-0 flex-col gap-2 border-r border-[var(--color-border)] bg-[var(--color-surface)] p-3">
        <div className="h-8 rounded-[var(--radius-control)] bg-[var(--color-surface-hover)]" />
        <div className="h-8 rounded-[var(--radius-control)] bg-[var(--color-surface-hover)]" />
        {Array.from({ length: 6 }).map((_, i) => (
          <div key={i} className="h-6 rounded-[var(--radius-item)] bg-[var(--color-surface-hover)] opacity-70" />
        ))}
      </div>
      {center}
    </div>
  );
}
