/** 自定义标题栏（无边框窗口）：设置入口 + 工作状态区，中间拖拽区，右侧窗口控制。
 *  PRD 第 8 节 / A3-05：
 *   - 左上角先显示「设置」文字入口，右侧才是小型真实工作状态区（缓慢轮播、悬停暂停、可手动滚动）；
 *   - 不再放品牌图标：图标位置让给「设置」，避免一个装饰 logo 占掉唯一的功能入口；
 *   - 状态区与「设置」入口都不带 data-tauri-drag-region（点击不触发拖拽）；
 *   - 窗口控制仍是独立 WindowControls，只保留最小化/最大化(还原)/关闭三个按钮；
 *   - 保留非 Tauri 环境异常降级逻辑。
 *  tauri.conf.json 需保持 decorations: false；窗口命令需 core:window:allow-minimize / toggle-maximize / close。
 */
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Maximize2, Minimize2, X } from "lucide-react";
import WorkStatus from "@/components/layout/WorkStatus";

/** 窗口控制组：最小化 / 最大化(还原) / 关闭。与拖拽区隔离（按钮无 data-tauri-drag-region）。 */
export function WindowControls() {
  const [maximized, setMaximized] = useState(false);

  // 在 Tauri 环境中监听窗口最大化状态，用于在「最大化/还原」图标之间切换
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    const setup = async () => {
      try {
        const w = getCurrentWindow();
        setMaximized(await w.isMaximized());
        const fn = await w.onResized(() => {
          void w
            .isMaximized()
            .then((m) => {
              if (active) setMaximized(m);
            })
            .catch(() => {});
        });
        if (!active) fn();
        else unlisten = fn;
      } catch {
        // 非 Tauri 环境（测试/纯浏览器预览）：忽略窗口控制
      }
    };
    void setup();
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  const win = () => {
    try {
      return getCurrentWindow();
    } catch {
      return null;
    }
  };

  const minimize = () => {
    const w = win();
    if (w) void w.minimize().catch(() => {});
  };
  const toggleMaximize = () => {
    const w = win();
    if (w) void w.toggleMaximize().catch(() => {});
  };
  const close = () => {
    const w = win();
    if (w) void w.close().catch(() => {});
  };

  return (
    <div role="group" aria-label="窗口控制" className="flex items-stretch">
      <button
        type="button"
        onClick={minimize}
        aria-label="最小化"
        title="最小化"
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-status)]"
      >
        <Minus size={16} strokeWidth={1.75} aria-hidden="true" />
      </button>
      <button
        type="button"
        onClick={toggleMaximize}
        aria-label={maximized ? "还原" : "最大化"}
        title={maximized ? "还原" : "最大化"}
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-status)]"
      >
        {maximized ? (
          <Minimize2 size={16} strokeWidth={1.75} aria-hidden="true" />
        ) : (
          <Maximize2 size={16} strokeWidth={1.75} aria-hidden="true" />
        )}
      </button>
      <button
        type="button"
        onClick={close}
        aria-label="关闭"
        title="关闭"
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-danger)] hover:text-[var(--color-danger-contrast)] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-status)]"
      >
        <X size={16} strokeWidth={1.75} aria-hidden="true" />
      </button>
    </div>
  );
}

export default function TitleBar() {
  return (
    <header
      data-tauri-drag-region
      className="relative flex h-10 shrink-0 select-none items-stretch border-b border-[var(--color-border)] bg-[var(--color-bg)]"
    >
      {/* 左侧：设置入口 → 工作状态区（PRD 第 8 节）。图标位置让给「设置」，状态在其右侧。 */}
      <div data-tauri-drag-region className="flex min-w-0 items-center gap-2 pl-2 pr-1">
        <button
          type="button"
          onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}
          aria-label="设置"
          title="设置"
          className="shrink-0 rounded-[var(--radius-item)] px-1.5 py-1 text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[var(--color-border-strong)]"
        >
          设置
        </button>
        <span aria-hidden="true" className="h-3 w-px shrink-0 bg-[var(--color-border)]" />
        <WorkStatus />
      </div>

      {/* 中间空白拖拽区 */}
      <div data-tauri-drag-region className="min-w-0 flex-1" />

      {/* 右侧：窗口控制（只保留最小化/最大化/关闭） */}
      <WindowControls />
    </header>
  );
}
