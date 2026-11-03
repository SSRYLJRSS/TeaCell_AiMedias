import { lazy, Suspense, useEffect, useRef, useState } from "react";
import BottomBar, { type TabKey } from "@/components/layout/BottomBar";
import TitleBar from "@/components/layout/TitleBar";
import StartupSkeleton from "@/components/common/StartupSkeleton";
import AssistantSkeleton from "@/components/assistant/AssistantSkeleton";
// 助手页刻意保持静态导入：它的重依赖（Markdown/shiki/mermaid/katex）都在组件内部按需
// 动态 import，静态壳本身很轻；而它的首屏必须与其他页共用同一个 `main` 壳层、同步可见
// （见 App 统一视觉体系测试）。其余页面走路由懒加载，避免首屏下载全部页面 chunk。
import AssistantPage from "@/pages/AssistantPage";

// WF-1 P0-2：路由级懒加载。首屏只加载当前页 chunk，切页时才下载其余页面，
// 显著降低首屏 JS 体积与解析时间；`showSkeleton` 仍负责启动期（设置/平台未就绪）。
const ImportPage = lazy(() => import("@/pages/ImportPage"));
const LibraryPage = lazy(() => import("@/pages/LibraryPage"));
const SuperSearchPage = lazy(() => import("@/pages/SuperSearchPage"));
const AiTaggingPage = lazy(() => import("@/pages/AiTaggingPage"));
const SettingsPage = lazy(() => import("@/pages/SettingsPage"));
const TreasurePage = lazy(() => import("@/pages/TreasurePage"));
import PageErrorBoundary from "@/components/common/PageErrorBoundary";
import OnboardingWizard from "@/components/onboarding/OnboardingWizard";
import FloatingAssistant from "@/components/assistant/FloatingAssistant";
import WorkflowPanel from "@/components/workflows/WorkflowPanel";
import PendingPlansPanel from "@/components/plans/PendingPlansPanel";
import { LibraryMaintenanceBanner } from "@/components/settings/MigrationWizard";
import { getHelpPageUrl, openHelpPage } from "@/api/settings";
import { startGlobalTaskWatch } from "@/stores/taskStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import { useAgentSessionStore } from "@/stores/agentSessionStore";
import { selectNativeWindowControls, usePlatformStore } from "@/stores/platformStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { markStartup } from "@/utils/startupMarks";
import { useTauriEvent } from "@/hooks/hooks";
import { on } from "@/api/client";
import { startImportDropDispatcher, stopImportDropDispatcher } from "@/utils/importDropDispatcher";
import { logger } from "@/utils/logger";

type PageKey = TabKey | "settings" | "superSearch" | "assistant";

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

/**
 * WP3-B「极限秒开」：进入应用后延迟这么久再在空闲窗口预热 Agent Host。
 * 刻意晚于首屏 IPC（设置/平台/素材），避免冷启动的 node 拉起与解压抢首屏主线程；
 * 用户在库里停留期间即可完成预热，切到助手页时 Host 通常已 ready。
 */
const HOST_WARMUP_DELAY_MS = 1200;

/** 路由骨架：4 页 + 全局底栏（设置入口位于库页顶栏左侧，底栏仅 3 个主入口） */
export default function App() {
  const [page, setPage] = useState<PageKey>("library");
  // 进入设置前的页面，供设置页「返回」恢复（PRD v2.4）
  const [prevPage, setPrevPage] = useState<PageKey>("library");
  const [startupTimedOut, setStartupTimedOut] = useState(false);
  const [showTutorialPrompt, setShowTutorialPrompt] = useState(false);
  const [tutorialBusy, setTutorialBusy] = useState(false);
  const [tutorialError, setTutorialError] = useState<string | null>(null);
  const [tutorialSaveFailed, setTutorialSaveFailed] = useState(false);
  const [tutorialUrl, setTutorialUrl] = useState("");
  const [tutorialCopied, setTutorialCopied] = useState(false);
  const tutorialPromptChecked = useRef(false);
  const pageRef = useRef<PageKey>("library");
  const workflowPanelOpen = useWorkflowPanelStore((s) => s.open);
  const pendingPlansOpen = usePendingPlansStore((s) => s.open);
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

  // 全局任务条：订阅入库/导出/AI 进度事件（幂等，M3-04）
  useEffect(() => {
    void startGlobalTaskWatch();
  }, []);

  // Native WebView drag/drop is subscribed once here; the mounted import page
  // registers and unregisters its target as navigation changes.
  useEffect(() => {
    void startImportDropDispatcher().catch((error: unknown) => {
      logger.error("导入拖拽监听启动失败", { error: error instanceof Error ? error.message : String(error) });
    });
    return stopImportDropDispatcher;
  }, []);

  // WP3-B「极限秒开」：进入应用并让首屏交互稳定后，在空闲窗口后台预热 Agent Host。
  // 幂等（ensureHost 模块级去重 + Rust 侧多实例保护）、失败静默、不弹横幅、绝不阻塞首屏。
  // 测试环境跳过，避免改变既有 App/助手用例的调用时序与断言。
  useEffect(() => {
    if (typeof import.meta.env !== "undefined" && import.meta.env.MODE === "test") return;
    let cancelled = false;
    const warm = () => {
      if (cancelled) return;
      markStartup("host_warmup_started");
      void useAgentSessionStore
        .getState()
        .ensureHost()
        .then(() => {
          if (!cancelled) markStartup("host_warmup_ready");
        })
        .catch(() => {
          // 非 Tauri / Host 资源缺失时静默；助手页会自行呈现启动中/错误四态。
        });
    };
    const w = window as Window & {
      requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number;
      cancelIdleCallback?: (handle: number) => void;
    };
    let idleHandle: number | null = null;
    const timer = window.setTimeout(() => {
      if (cancelled) return;
      if (typeof w.requestIdleCallback === "function") {
        idleHandle = w.requestIdleCallback(warm, { timeout: 2000 });
      } else {
        warm();
      }
    }, HOST_WARMUP_DELAY_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      if (idleHandle !== null && typeof w.cancelIdleCallback === "function") {
        w.cancelIdleCallback(idleHandle);
      }
    };
  }, []);

  // R-24：启动即加载设置并应用主题（load 内部调 applyTheme；single-flight 见 settingsStore §5.3）
  const settingsLoaded = useSettingsStore((s) => s.loaded);
  const settings = useSettingsStore((s) => s.settings);
  const loadSettings = useSettingsStore((s) => s.load);
  const saveSettings = useSettingsStore((s) => s.save);
  const platformStatus = usePlatformStore((s) => s.status);
  const nativeWindowControls = usePlatformStore(selectNativeWindowControls);
  const loadPlatform = usePlatformStore((s) => s.load);
  useEffect(() => {
    markStartup("react_first_render"); // §4.1：React 首帧打点（首次挂载即首帧）
    if (!settingsLoaded) void loadSettings();
  }, [settingsLoaded, loadSettings]);

  useEffect(() => {
    if (!settingsLoaded || !settings || tutorialPromptChecked.current) return;
    tutorialPromptChecked.current = true;
    if (settings.tutorialPromptHandled === false) setShowTutorialPrompt(true);
  }, [settingsLoaded, settings]);

  const finishTutorialPrompt = async () => {
    const current = useSettingsStore.getState().settings;
    if (!current) return;
    setTutorialBusy(true);
    setTutorialError(null);
    setTutorialSaveFailed(false);
    try {
      await saveSettings({ ...current, tutorialPromptHandled: true });
      setShowTutorialPrompt(false);
    } catch (error) {
      setTutorialError(error instanceof Error ? error.message : String(error));
      setTutorialSaveFailed(true);
    } finally {
      setTutorialBusy(false);
    }
  };

  const continueAfterTutorialSaveFailure = () => {
    setShowTutorialPrompt(false);
    setTutorialError(null);
    setTutorialSaveFailed(false);
  };

  const openTutorial = async () => {
    setTutorialBusy(true);
    setTutorialError(null);
    try {
      await openHelpPage();
      await finishTutorialPrompt();
    } catch (error) {
      setTutorialError(`无法自动打开新手教程：${error instanceof Error ? error.message : String(error)}`);
      try {
        setTutorialUrl(await getHelpPageUrl());
      } catch {
        setTutorialError("无法自动打开教程，也无法读取文档地址。请稍后在设置中手动打开使用帮助。");
      }
      setTutorialBusy(false);
    }
  };

  const copyTutorialUrl = async () => {
    if (!tutorialUrl) return;
    try {
      await navigator.clipboard.writeText(tutorialUrl);
      setTutorialCopied(true);
    } catch {
      setTutorialError("自动复制失败。请选中下方完整地址并复制到浏览器地址栏打开。");
    }
  };

  // 静态平台能力与设置并行加载；状态订阅也让路径显示等纯 UI 选择器在加载后刷新。
  // 失败时 platformStore 保守禁用平台专属操作，并在界面提供显式重试。
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

  const platformResolved = platformStatus === "ready" || platformStatus === "error";
  const showSkeleton = (!settingsLoaded || !platformResolved) && !startupTimedOut;

  return (
    <div className="h-full flex flex-col">
      {!nativeWindowControls && <TitleBar />}

      {platformStatus === "error" && (
        <div
          role="alert"
          className="flex items-center justify-between gap-3 border-b border-[var(--color-danger)] bg-[var(--color-surface)] px-4 py-2 text-xs text-[var(--color-danger)]"
        >
          <span>平台能力加载失败，平台专属功能已安全禁用。请重试或重启应用。</span>
          <button
            type="button"
            className="shrink-0 underline underline-offset-2"
            onClick={() => void loadPlatform()}
          >
            重试
          </button>
        </div>
      )}

      {/* A4-05：维护期间只停写入与自动清理，读取和旧会话继续可用。 */}
      <LibraryMaintenanceBanner />

      <main className={viewerOpen ? "flex-1 min-h-0" : "flex-1 min-h-0 pb-14"}>
        {showSkeleton ? (
          <StartupSkeleton />
        ) : (
          /* A-1：页面级 Error Boundary——路由页运行时异常不白屏；key 切换让每个页面独立边界 */
          <PageErrorBoundary key={page} onReset={() => setPage(page)} onBack={() => setPage("library")}>
            <Suspense fallback={<StartupSkeleton />}>
              {page === "import" && <ImportPage />}
              {page === "library" && <LibraryPage />}
              {page === "superSearch" && <SuperSearchPage />}
              {page === "ai" && <AiTaggingPage />}
              {page === "settings" && <SettingsPage onBack={() => setPage(prevPage)} />}
              {page === "assistant" && (
                <Suspense fallback={<AssistantSkeleton />}>
                  <AssistantPage />
                </Suspense>
              )}
              {page === "treasure" && <TreasurePage />}
            </Suspense>
          </PageErrorBoundary>
        )}
      </main>

      {!viewerOpen && (
        <BottomBar
          current={page}
          onNavigate={navigate}
          onOpenSuperSearch={() => navigate("superSearch")}
          onOpenAssistant={() => navigate("assistant")}
        />
      )}

      {/* A3-09：应用内悬浮助手。查看器打开时隐藏（避免遮挡全屏预览），
          完整助手页里也不显示（入口已在那里）；关掉开关只隐藏，不影响任务。 */}
      {!viewerOpen && page !== "assistant" && settings?.floatingAssistant !== false && (
        <FloatingAssistant />
      )}

      {/* A4-07：常用流程面板。入口有两处（任务卡的「保存为常用流程」与助手页 header），
          面板本身挂在应用层只渲染一次，避免两处各存一份流程列表状态。
          助手页把它嵌进右侧栏（选项卡），所以那一页不渲染这里的模态框，
          否则同一份内容会同时以「模态框 + 侧栏」出现两次。 */}
      {page !== "assistant" && workflowPanelOpen && <WorkflowPanel />}

      {/* A5-02：待审计划面板。外部客户端（MCP）准备的计划不属于任何本地会话，
          `linkedPlans` 结构上装不进它们，所以「外部准备 → 茶馆确认」需要一个
          不依赖会话的入口。入口在助手页头部与 MCP 设置面板，面板挂应用层只渲染一次。
          同样地，助手页里它以右侧栏选项卡的形式出现，那份模态框不重复渲染。 */}
      {page !== "assistant" && pendingPlansOpen && <PendingPlansPanel />}

      <OnboardingWizard
        open={showTutorialPrompt}
        persistError={tutorialSaveFailed ? (tutorialError ?? "无法把「已完成引导」写回设置。") : null}
        onFinish={() => void finishTutorialPrompt()}
        onContinueAnyway={continueAfterTutorialSaveFailure}
        onOpenTutorial={openTutorial}
        tutorialBusy={tutorialBusy}
        tutorialError={tutorialError}
        tutorialUrl={tutorialUrl}
        tutorialCopied={tutorialCopied}
        onCopyTutorialUrl={copyTutorialUrl}
        onOpenSettings={() => {
          navigate("settings");
          setShowTutorialPrompt(false);
        }}
      />
    </div>
  );
}
