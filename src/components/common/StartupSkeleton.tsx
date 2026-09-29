/**
 * StartupSkeleton（指导书 阶段 1 §3.2/§5.3）：首屏过渡骨架。
 * - 背景色与主题 `--color-bg` 一致，不显示纯白闪屏；
 * - 文案不承诺百分比（无真实进度时用不定进度条）；
 * - 本组件纯展示：不触发数据库、缩略图或网络请求；
 * - 非 Tauri 环境（纯浏览器预览/测试）也不抛异常。
 */
import appLogo from "@/assets/icon-logo.png";

export default function StartupSkeleton() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 bg-[var(--color-bg)]">
      <img src={appLogo} alt="" className="h-10 w-10 select-none" draggable={false} />
      <p className="text-sm font-medium text-[var(--color-text)]">茶馆</p>
      <p className="text-xs text-[var(--color-text-secondary)]">正在准备素材库</p>
      {/* 不定进度条：CSS 关键帧左右移动，不承诺百分比 */}
      <div className="mt-1 h-1 w-40 overflow-hidden rounded-full bg-[var(--color-surface-hover)]">
        <div className="startup-indeterminate h-full w-1/3 rounded-full bg-[var(--color-status)]" />
      </div>
      <p className="mt-1 text-[11px] text-[var(--color-text-secondary)]">首次启动可能需要一点时间</p>
    </div>
  );
}
