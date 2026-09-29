/** 自定义标题栏（无边框窗口）：左侧 logo 设置入口，中间拖拽区，右侧窗口控制（最小化/最大化/关闭）。
 *  指导书 §3.3 施工要求：
 *   - 左侧不渲染“茶馆”名称文字，点击 logo 进入设置；
 *   - logo 设置入口与窗口控制按钮都不带 data-tauri-drag-region（点击不触发拖拽）；
 *   - 窗口控制拆分为独立 WindowControls 组件，只保留最小化/最大化(还原)/关闭三个按钮；
 *   - 保留现有非 Tauri 环境异常降级逻辑。
 *   tauri.conf.json 需保持 decorations: false；窗口命令需 core:window:allow-minimize / toggle-maximize / close。
 */
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Maximize2, Minimize2, X } from "lucide-react";
import appLogo from "@/assets/icon-logo.png";

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
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
      >
        <Minus size={16} strokeWidth={1.75} aria-hidden="true" />
      </button>
      <button
        type="button"
        onClick={toggleMaximize}
        aria-label={maximized ? "还原" : "最大化"}
        title={maximized ? "还原" : "最大化"}
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
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
        className="flex w-11 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[#e81123] hover:text-white"
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
      {/* 左侧：logo 同时作为设置入口（无文字软件名、无独立齿轮按钮） */}
      <div data-tauri-drag-region className="flex items-center pr-1">
        <button
          type="button"
          onClick={() => window.dispatchEvent(new CustomEvent("app:navigate", { detail: "settings" }))}
          aria-label="设置"
          title="设置"
          className="flex size-10 items-center justify-center text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        >
          <img
            src={appLogo}
            alt=""
            className="pointer-events-none h-6 w-6 select-none"
            draggable={false}
          />
        </button>
      </div>

      {/* 中间空白拖拽区 */}
      <div data-tauri-drag-region className="min-w-0 flex-1" />

      {/* 右侧：窗口控制（只保留最小化/最大化/关闭） */}
      <WindowControls />
    </header>
  );
}
