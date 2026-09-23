import { useEffect, useRef, useState } from "react";
import BottomBar, { type TabKey } from "@/components/layout/BottomBar";
import TitleBar from "@/components/layout/TitleBar";
import StartupSkeleton from "@/components/common/StartupSkeleton";
import ImportPage from "@/pages/ImportPage";
import LibraryPage from "@/pages/LibraryPage";
import SuperSearchPage from "@/pages/SuperSearchPage";
import AiTaggingPage from "@/pages/AiTaggingPage";
import SettingsPage from "@/pages/SettingsPage";
import PageErrorBoundary from "@/components/common/PageErrorBoundary";
import { startGlobalTaskWatch } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePlatformStore } from "@/stores/platformStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { markStartup } from "@/utils/startupMarks";
import { useTauriEvent } from "@/hooks/hooks";
import { on } from "@/api/client";

type PageKey = TabKey | "settings" | "superSearch";

/** FB4-03（§6.5）：导入后置色板完成事件（只允许 import 来源发送；手动回算不发） */
interface PaletteUpdatedEvent {
  source: "import";
  total: number;
  success: number;
  failed: number;
  skipped: number;
  updatedIds: number[];
}

/** 首屏骨架最长等待：设置/素材 IPC 若异常挂起，最多等这么久就放行页面（§3.2 超时兜底） */
const STARTUP_MAX_WAIT_MS = 6000;

/** 路由骨架：4 页 + 全局底栏（设置入口位于库页顶栏左侧，底栏仅 3 个主入口） */
export default function App() {
  const [page, setPage] = useState<PageKey>("library");
  // 进入设置前的页面，供设置页「返回」恢复（PRD v2.4）
  const [prevPage, setPrevPage] = useState<PageKey>("library");
  const [startupTimedOut, setStartupTimedOut] = useState(false);
  const pageRef = useRef<PageKey>("library");
  useEffect(() => {
    pageRef.current = page;
  }, [page]);

  // 桌面应用只使用自定义右键菜单，阻止 WebView 在其余区域弹出浏览器原生菜单。
  useEffect(() => {
    const preventNativeContextMenu = (event: MouseEvent) => event.preventDefault();
    window.addEventListener("contextmenu", preventNativeContextMenu);
    return () => window.removeEventListener("contextmenu", preventNativeContextMenu);
  }, []);

  // 库页操作区/上下文条的跨页导航（导入、AI 打标、设置）
  useEffect(() => {
    const onNav = (e: Event) => {
      const next = (e as CustomEvent<PageKey>).detail;
      if (needsPrev(next) && pageRef.current !== next) setPrevPage(pageRef.current);
      setPage(next);
    };
    window.addEventListener("app:navigate", onNav);
    return () => window.removeEventListener("app:navigate", onNav);
  }, []);

  // 需要记录进入前页面以便返回的页：设置、超级搜索
  const needsPrev = (next: PageKey) => next === "settings" || next === "superSearch";

  // 导航入口：底栏 + 超级搜索双击
  const navigate = (next: PageKey) => {
    if (needsPrev(next) && pageRef.current !== next) setPrevPage(pageRef.current);
    setPage(next);
  };

  // 任务状态订阅：入库页消费入库进度，底栏全局任务层只展示导出进度（幂等）
  useEffect(() => {
    void startGlobalTaskWatch();
  }, []);

  // R-24：启动即加载设置并应用主题（load 内部调 applyTheme；single-flight 见 settingsStore §5.3）
  const settingsLoaded = useSettingsStore((s) => s.loaded);
  const loadSettings = useSettingsStore((s) => s.load);
  const platformStatus = usePlatformStore((s) => s.status);
  const loadPlatform = usePlatformStore((s) => s.load);
  useEffect(() => {
    markStartup("react_first_render"); // §4.1：React 首帧打点（首次挂载即首帧）
    if (!settingsLoaded) void loadSettings();
  }, [settingsLoaded, loadSettings]);

  // 平台能力与设置并行加载；失败只进入保守降级，不阻塞核心五步闭环。
  useEffect(() => {
    if (platformStatus === "idle") void loadPlatform();
  }, [platformStatus, loadPlatform]);

  // 骨架超时兜底：设置 IPC 异常挂起时不可无限停留在过渡态（§3.2）
  useEffect(() => {
    const t = setTimeout(() => setStartupTimedOut(true), STARTUP_MAX_WAIT_MS);
    return () => clearTimeout(t);
  }, []);

  // §7.3 方案 A：Viewer 打开时隐藏全局 BottomBar（Viewer 自带胶片条，避免双重导航）
  const viewerOpen = useLibraryStore((s) => s.viewerOpen);

  // FB4-03（§6.5）：全局订阅导入后置色板完成事件 → 只做定向同步（refreshPaletteFields），
  // 禁止调用 libraryStore.refresh()（会重置分页/滚动/Viewer 上下文）。
  // App 级监听：用户可能在素材库/查看器/入库页/设置页之间切换，不依赖某个页面是否挂载；
  // 订阅失败由 useTauriEvent 统一记录，不得阻塞应用首屏。
  useTauriEvent(
    () =>
      on<PaletteUpdatedEvent>("palette://updated", (payload) => {
        void useLibraryStore.getState().refreshPaletteFields(payload.updatedIds);
        void useMetadataStore.getState().refresh();
      }),
    [],
  );

  const showSkeleton = !settingsLoaded && !startupTimedOut;

  return (
    <div className="h-full flex flex-col">
      <TitleBar />

      <main className={viewerOpen ? "flex-1 min-h-0" : "flex-1 min-h-0 pb-14"}>
        {showSkeleton ? (
          <StartupSkeleton />
        ) : (
          /* A-1：页面级 Error Boundary——路由页运行时异常不白屏；key 切换让每个页面独立边界 */
          <PageErrorBoundary key={page} onReset={() => setPage(page)} onBack={() => setPage("library")}>
            {page === "import" && <ImportPage />}
            {page === "library" && <LibraryPage />}
            {page === "superSearch" && <SuperSearchPage />}
            {page === "ai" && <AiTaggingPage />}
            {page === "settings" && <SettingsPage onBack={() => setPage(prevPage)} />}
          </PageErrorBoundary>
        )}
      </main>

      {!viewerOpen && (
        <BottomBar current={page} onNavigate={navigate} onOpenSuperSearch={() => navigate("superSearch")} />
      )}
    </div>
  );
}
