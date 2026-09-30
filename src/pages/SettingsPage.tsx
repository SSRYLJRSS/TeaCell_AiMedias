/** 设置页（指导书 §2.2/§6.1-§6.7）：
 *  左侧分组导航（含 AI 与模型三个子页）+ 右侧分组内容；配置项修改后自动保存。
 *  IA：素材库与入库 → AI 与模型（服务管理/超级搜索/自动打标）→ 标签与分类 → 外观与浏览 → 存储与维护 → 诊断与支持 → 关于。
 *  AI 子页内使用「在线服务/本地服务」二选一，只渲染当前模式字段。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import clsx from "clsx";
import { useShallow } from "zustand/react/shallow";
import { open as pickDir, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { on } from "@/api/client";
import Button from "@/components/common/Button";
import { displayBasename } from "@/utils/pathDisplay";
import { ollamaInstallStatus, ollamaRemoveInstaller } from "@/api/ollama";
import { backupDb, clearThumbnailCache, exportDiagnostics, getAuthorPageUrl, getDataDir, getFeedbackPageUrl, getLicensePageUrl, getProjectPageUrl, openAuthorPage, openDataDir, openFeedbackPage, openHelpPage, openLicensePage, openLogsDir, openProjectPage, resetAppData, restoreDb, type ResetDataSelection } from "@/api/settings";
import {
  rescanAssetMetadata,
  rescanAssetPalette,
  rescanAssetPhash,
  rescanImageDimensions,
  rescanPaletteColors,
  cancelMediaRefill,
  getPaletteStatus,
  type RefillProgress,
  type PaletteStatus,
} from "@/api/assets";
import { listAiConnections, getAiUsageBindings, getSuperSearchServiceResolution, setAiUsageBinding, type AiConnection, type SuperSearchServiceResolution } from "@/api/connections";
import { videoProxyCacheStats, clearAllVideoProxies } from "@/api/video";
import FacetManagePanel from "@/components/settings/FacetManagePanel";
import ServiceManagement from "@/components/settings/ServiceManagement";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useTagStore } from "@/stores/tagStore";
import { applyTheme, useSettingsStore, DEFAULT_APPEARANCE } from "@/stores/settingsStore";
import { CELL_STEPS } from "@/types/settings";
import type { CellAspect, CellFit, Settings } from "@/types/settings";

/** FB2-01/02：素材框比例与填充可选项（顺序即展示顺序） */
const CELL_ASPECTS: { value: CellAspect; label: string }[] = [
  { value: "1:1", label: "1:1（方形）" },
  { value: "4:3", label: "4:3" },
  { value: "3:2", label: "3:2" },
  { value: "16:9", label: "16:9" },
  { value: "3:4", label: "3:4" },
  { value: "2:3", label: "2:3" },
  { value: "9:16", label: "9:16" },
];
const CELL_FITS: { value: CellFit; label: string }[] = [
  { value: "cover", label: "裁切填满" },
  { value: "contain", label: "完整显示" },
  { value: "smart", label: "智能适应" },
];

/** §6.1 路由状态：必须能表达 AI 的三个子页面（超级搜索 / 自动打标 / 服务管理） */
type SettingsRoute = "library" | "ai.superSearch" | "ai.tagging" | "ai.services" | "tags" | "general" | "data" | "diagnostics" | "about";

/** §6.1 分组顺序 */
const GROUPS: {
  key: "library" | "ai" | "tags" | "general" | "data" | "diagnostics" | "about";
  label: string;
  children?: { key: SettingsRoute; label: string }[];
}[] = [
  { key: "library", label: "素材库与入库" },
  {
    key: "ai",
    label: "AI 与模型",
    children: [
      { key: "ai.services", label: "服务管理" },
      { key: "ai.superSearch", label: "超级搜索" },
      { key: "ai.tagging", label: "自动打标" },
    ],
  },
  { key: "tags", label: "标签与分类" },
  { key: "general", label: "外观与浏览" },
  { key: "data", label: "存储与维护" },
  { key: "diagnostics", label: "诊断与支持" },
  { key: "about", label: "关于" },
];

/** 初始分组：素材库与入库（第一项） */
const DEFAULT_ROUTE: SettingsRoute = "library";
const SETTINGS_AUTOSAVE_DELAY_MS = 500;

export default function SettingsPage({ onBack }: { onBack?: () => void }) {
  const { settings, loaded, loading, load, save, loadError } = useSettingsStore(
    useShallow((s) => ({
      settings: s.settings,
      loaded: s.loaded,
      loading: s.loading,
      load: s.load,
      save: s.save,
      loadError: s.loadError,
    })),
  );
  const [draft, setDraft] = useState<Settings | null>(null);
  const [route, setRoute] = useState<SettingsRoute>(DEFAULT_ROUTE);
  const [dataDir, setDataDir] = useState("");
  const [dataDirError, setDataDirError] = useState(false);
  const [exportingDiagnostics, setExportingDiagnostics] = useState(false);
  const [autosaveStatus, setAutosaveStatus] = useState<"idle" | "pending" | "saving" | "saved" | "error">("idle");
  const [autosaveError, setAutosaveError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [failedExternalUrl, setFailedExternalUrl] = useState<string | null>(null);
  const draftRef = useRef<Settings | null>(null);
  const autosaveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const autosaveVersionRef = useRef(0);
  const autosaveQueueRef = useRef<Promise<void>>(Promise.resolve());
  const persistAutosaveRef = useRef<(snapshot: Settings, version: number | null) => Promise<void>>(() => Promise.resolve());
  const mountedRef = useRef(true);

  const persistAutosave = useCallback((snapshot: Settings, version: number | null) => {
    const operation = autosaveQueueRef.current.catch(() => undefined).then(async () => {
      // 被更新的草稿取代时，跳过尚未开始的旧版本写入。
      if (version !== null && version !== autosaveVersionRef.current) return;
      if (mountedRef.current) {
        setAutosaveStatus("saving");
        setAutosaveError(null);
      }
      await save(snapshot);
      if (mountedRef.current && (version === null || version === autosaveVersionRef.current)) {
        setAutosaveStatus("saved");
      }
    }).catch((cause: unknown) => {
      if (mountedRef.current && (version === null || version === autosaveVersionRef.current)) {
        setAutosaveStatus("error");
        setAutosaveError(cause instanceof Error ? cause.message : String(cause));
      }
    });
    autosaveQueueRef.current = operation;
    return operation;
  }, [save]);
  persistAutosaveRef.current = persistAutosave;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (!autosaveTimerRef.current) return;
      clearTimeout(autosaveTimerRef.current);
      autosaveTimerRef.current = null;
      const pending = draftRef.current;
      const persisted = useSettingsStore.getState().settings;
      if (pending && JSON.stringify(pending) !== JSON.stringify(persisted)) {
        void persistAutosaveRef.current(pending, null);
      }
    };
  }, []);

  // A3：安装包缓存（「存储与维护」分组展示占用/清理）
  const [installerInfo, setInstallerInfo] = useState<{ path: string; size: number } | null>(null);
  const [removingInstaller, setRemovingInstaller] = useState(false);
  // §6.7：视频代理缓存统计（数量/占用）+ 清理
  const [proxyStats, setProxyStats] = useState<{ count: number; bytes: number } | null>(null);
  const [clearingProxies, setClearingProxies] = useState(false);
  // 指导书 §7.5：媒体元数据批量回填（进度 + 取消）
  const [refillResult, setRefillResult] = useState<string | null>(null);
  const [refillProgress, setRefillProgress] = useState<RefillProgress | null>(null);
  const [refilling, setRefilling] = useState(false);
  const refillUnsub = useRef<(() => void) | null>(null);
  useEffect(() => () => refillUnsub.current?.(), []);

  const onRefill = async (scope: "all" | "missing") => {
    setRefilling(true);
    setRefillResult(null);
    setRefillProgress(null);
    // 订阅进度（非 Tauri 环境静默降级）
    on<RefillProgress>("media_refill://progress", (p) => setRefillProgress(p))
      .then((unsub) => {
        refillUnsub.current = unsub;
      })
      .catch(() => undefined);
    try {
      const r = await rescanAssetMetadata([], scope);
      setRefillResult(`媒体信息更新完成：总数 ${r.total}，成功 ${r.success}，失败 ${r.failed}，跳过 ${r.skipped}`);
    } catch (e) {
      setRefillResult(e instanceof Error ? e.message : String(e));
    } finally {
      setRefilling(false);
      refillUnsub.current?.();
      refillUnsub.current = null;
      setRefillProgress(null);
    }
  };
  const onCancelRefill = () => {
    void cancelMediaRefill().catch(() => undefined);
    setRefillResult("正在取消…");
  };

  // FB2-08：算法色板回算（与元数据回填互斥，见 FX-12；进度事件同频道，互斥保证不混淆）
  const [paletteResult, setPaletteResult] = useState<string | null>(null);
  const [paletteProgress, setPaletteProgress] = useState<RefillProgress | null>(null);
  const [paletteRunning, setPaletteRunning] = useState(false);
  const paletteUnsub = useRef<(() => void) | null>(null);
  useEffect(() => () => paletteUnsub.current?.(), []);
  const onRescanPalette = async (scope: "all" | "missing") => {
    setPaletteRunning(true);
    setPaletteResult(null);
    setPaletteProgress(null);
    // 独立订阅，不复用 refillUnsub：两个订阅同时活着时复用 ref 会互相覆盖，导致其中一个泄漏
    on<RefillProgress>("media_refill://progress", (p) => setPaletteProgress(p))
      .then((unsub) => {
        paletteUnsub.current = unsub;
      })
      .catch(() => undefined);
    try {
      const r = await rescanAssetPalette([], scope);
      setPaletteResult(`回算完成：总数 ${r.total}，成功 ${r.success}，跳过 ${r.skipped}，失败 ${r.failed}`);
      await useMetadataStore.getState().refresh();
    } catch (e) {
      setPaletteResult(e instanceof Error ? e.message : String(e));
    } finally {
      setPaletteRunning(false);
      paletteUnsub.current?.();
      paletteUnsub.current = null;
      setPaletteProgress(null);
    }
  };

  // ── W5d（§W5d）：感知哈希存量回填（与色板回算互斥：同一 refill_running 闸）──
  const [phashRunning, setPhashRunning] = useState(false);
  const phashUnsub = useRef<(() => void) | null>(null);
  const [phashProgress, setPhashProgress] = useState<RefillProgress | null>(null);
  const [phashResult, setPhashResult] = useState<string | null>(null);
  useEffect(() => () => phashUnsub.current?.(), []);

  const onRescanPhash = async (scope: "all" | "missing") => {
    setPhashRunning(true);
    setPhashResult(null);
    setPhashProgress(null);
    on<RefillProgress>("media_refill://progress", (p) => setPhashProgress(p))
      .then((unsub) => {
        phashUnsub.current = unsub;
      })
      .catch(() => undefined);
    try {
      const r = await rescanAssetPhash([], scope);
      setPhashResult(`相似图数据生成完成：总数 ${r.total}，成功 ${r.success}，跳过 ${r.skipped}，失败 ${r.failed}`);
    } catch (e) {
      setPhashResult(e instanceof Error ? e.message : String(e));
    } finally {
      setPhashRunning(false);
      phashUnsub.current?.();
      phashUnsub.current = null;
      setPhashProgress(null);
    }
  };

  // ── R1-2：色板关系表重建（rescan_palette_colors：从 palette_json 重灌 asset_palette_colors，
  //  不解码图片毫秒级；表空时「前三色包含红」等筛选恒 0 结果，导入素材后点一次）──
  const [paletteColorsRunning, setPaletteColorsRunning] = useState(false);
  const [paletteColorsResult, setPaletteColorsResult] = useState<string | null>(null);
  const onRescanPaletteColors = async () => {
    if (paletteColorsRunning || paletteRunning || refilling) return;
    setPaletteColorsRunning(true);
    setPaletteColorsResult(null);
    try {
      const n = await rescanPaletteColors();
      setPaletteColorsResult(`色板关系表已重建：写入 ${n} 条（表已满时重复执行返回 0，属正常）`);
      await useMetadataStore.getState().refresh();
    } catch (e) {
      setPaletteColorsResult(e instanceof Error ? e.message : String(e));
    } finally {
      setPaletteColorsRunning(false);
    }
  };

  // ── R1-2：图片宽高存量回填（RAW 分辨率修复；与其它回填共用互斥闸）──
  const [dimRunning, setDimRunning] = useState(false);
  const dimUnsub = useRef<(() => void) | null>(null);
  const [dimProgress, setDimProgress] = useState<RefillProgress | null>(null);
  const [dimResult, setDimResult] = useState<string | null>(null);
  useEffect(() => () => dimUnsub.current?.(), []);

  const onRescanDimensions = async (scope: "all" | "missing") => {
    setDimRunning(true);
    setDimResult(null);
    setDimProgress(null);
    on<RefillProgress>("media_refill://progress", (p) => setDimProgress(p))
      .then((unsub) => {
        dimUnsub.current = unsub;
      })
      .catch(() => undefined);
    try {
      const r = await rescanImageDimensions([], scope);
      setDimResult(`分辨率更新完成：总数 ${r.total}，成功 ${r.success}，跳过 ${r.skipped}，失败 ${r.failed}`);
    } catch (e) {
      setDimResult(e instanceof Error ? e.message : String(e));
    } finally {
      setDimRunning(false);
      dimUnsub.current?.();
      dimUnsub.current = null;
      setDimProgress(null);
    }
  };

  // ── FB4-03（§4.5/§6.3）：色板状态行 + 「生成缺失色条」手动流程 ──
  const [paletteStatus, setPaletteStatus] = useState<PaletteStatus | null>(null);
  const [paletteStatusError, setPaletteStatusError] = useState<string | null>(null);
  const [generatingMissing, setGeneratingMissing] = useState(false);
  const [generateProgress, setGenerateProgress] = useState<RefillProgress | null>(null);
  const [generateResult, setGenerateResult] = useState<string | null>(null);
  /** W4-5：色条细节折叠（默认收起） */
  const [showColorDetails, setShowColorDetails] = useState(false);
  const generateUnsub = useRef<(() => void) | null>(null);
  useEffect(() => () => generateUnsub.current?.(), []);

  /** FB4-03：重新读取色板状态（进入外观与浏览路由时调用；失败保留错误文案 + 重试入口）。 */
  const refreshPaletteStatus = useCallback(async () => {
    try {
      const st = await getPaletteStatus();
      setPaletteStatus(st);
      setPaletteStatusError(null);
    } catch (e) {
      setPaletteStatusError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // 进入外观与浏览路由时读取状态；总开关变化不需要重复触发扫描（§6.3）
  useEffect(() => {
    if (route !== "general") return;
    void refreshPaletteStatus();
  }, [route, refreshPaletteStatus]);

  /** FB4-03：生成缺失色条 —— 手动回算不发 palette://updated 全局事件；
   *  resolve 后不得调用 libraryStore.refresh()；状态刷新与局部同步即使失败也保留摘要并显示具体错误。 */
  const onGenerateMissingPalette = async () => {
    if (generatingMissing || paletteRunning || refilling) return;
    setGeneratingMissing(true);
    setGenerateResult(null);
    setGenerateProgress(null);
    on<RefillProgress>("media_refill://progress", (p) => setGenerateProgress(p))
      .then((unsub) => {
        generateUnsub.current = unsub;
      })
      .catch(() => undefined);
    try {
      const r = await rescanAssetPalette([], "missing");
      setGenerateResult(
        `生成完成：成功 ${r.success}，跳过 ${r.skipped}，失败 ${r.failed}（共处理 ${r.total}）`,
      );
      // 随后重新读取状态 + 定向同步色板字段；任一失败也要保留摘要并给出具体错误
      const errors: string[] = [];
      try {
        await refreshPaletteStatus();
      } catch (e) {
        errors.push(`状态刷新失败：${e instanceof Error ? e.message : String(e)}`);
      }
      try {
        await useLibraryStore.getState().refreshPaletteFields(r.updatedIds);
      } catch (e) {
        errors.push(`素材色条同步失败：${e instanceof Error ? e.message : String(e)}`);
      }
      try {
        await useMetadataStore.getState().refresh();
      } catch (e) {
        errors.push(`主要颜色刷新失败：${e instanceof Error ? e.message : String(e)}`);
      }
      if (errors.length > 0) {
        setPaletteStatusError(errors.join("；"));
      }
    } catch (e) {
      // 互斥闸被占用（FX-12）/ 一般错误：明确展示，不轮询不自动重试
      setGenerateResult(e instanceof Error ? e.message : String(e));
    } finally {
      setGeneratingMissing(false);
      generateUnsub.current?.();
      generateUnsub.current = null;
      setGenerateProgress(null);
    }
  };

  useEffect(() => {
    if (!loaded) void load();
  }, [loaded, load]);

  useEffect(() => {
    if (settings && !draft) {
      const initial = structuredClone(settings);
      const preview = useSettingsStore.getState().previewAppearance;
      if (preview) initial.appearance = structuredClone(preview);
      draftRef.current = initial;
      setDraft(initial);
    }
  }, [settings, draft]);

  useEffect(() => {
    // A-3：数据目录读取失败只显示「暂不可用」，不抛出到页面边界
    getDataDir()
      .then((dir) => {
        setDataDir(dir);
        setDataDirError(false);
      })
      .catch(() => setDataDirError(true));
  }, []);

  // 进入「存储与维护」分组时刷新安装包缓存 + 视频代理缓存统计
  useEffect(() => {
    if (route !== "data") return;
    ollamaInstallStatus()
      .then((s) =>
        s.installerPath ? setInstallerInfo({ path: s.installerPath, size: s.installerSize }) : setInstallerInfo(null),
      )
      .catch(() => setInstallerInfo(null));
    videoProxyCacheStats()
      .then(([count, bytes]) => setProxyStats({ count, bytes }))
      .catch(() => setProxyStats(null));
  }, [route]);

  const onClearVideoProxies = async () => {
    setClearingProxies(true);
    try {
      const removed = await clearAllVideoProxies();
      setNotice(removed > 0 ? `已清理 ${removed} 个视频代理文件` : "视频代理缓存已清理");
      const [count, bytes] = await videoProxyCacheStats();
      setProxyStats({ count, bytes });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setClearingProxies(false);
    }
  };

  const onRemoveInstaller = async () => {
    setRemovingInstaller(true);
    try {
      await ollamaRemoveInstaller();
      setInstallerInfo(null);
      setNotice("Ollama 安装包已删除");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRemovingInstaller(false);
    }
  };

  if (!draft) {
    // P2-03：加载失败不能永久停留在「加载设置中…」——给出错误与重试入口
    if (loadError) {
      return (
        <div className="flex h-full flex-col items-center justify-center gap-3 text-sm">
          <p className="text-[var(--color-danger)]">设置加载失败：{loadError}</p>
          <Button onClick={() => void load()}>重试</Button>
          <p className="text-xs text-[var(--color-text-secondary)]">
            加载成功前设置页不可编辑，避免覆盖真实配置
          </p>
        </div>
      );
    }
    return (
      <div className="flex h-full items-center justify-center text-sm text-[var(--color-text-secondary)]">
        {loading ? "加载设置中…" : "准备加载…"}
      </div>
    );
  }

  const dirty = (next: Settings) => {
    const current = draftRef.current ?? draft;
    if (JSON.stringify(current) === JSON.stringify(next)) return;
    draftRef.current = next;
    setDraft(next);
    setAutosaveStatus("pending");
    setAutosaveError(null);
    const version = ++autosaveVersionRef.current;
    if (autosaveTimerRef.current) clearTimeout(autosaveTimerRef.current);
    autosaveTimerRef.current = setTimeout(() => {
      autosaveTimerRef.current = null;
      const latest = draftRef.current;
      if (latest) void persistAutosaveRef.current(latest, version);
    }, SETTINGS_AUTOSAVE_DELAY_MS);
  };
  const patchAi = (patch: Partial<Settings["ai"]>) => dirty({ ...draft, ai: { ...draft.ai, ...patch } });

  // W3：aiFacetConfigs 草稿路径已删（V20 合表后分面语义在 tag_facets.input_mode，
  // FacetManagePanel 直接读写库；此处不再维护第二份草稿）

  // FB2-01/02（§8.4）：素材框外观 —— draft.appearance 兜底默认；改动同时写 draft 与 previewAppearance（即时预览）
  const draftAppearance = draft.appearance ?? DEFAULT_APPEARANCE;
  const pushPreview = (appearance: Settings["appearance"]) => {
    useSettingsStore.getState().setPreviewAppearance(appearance);
  };
  const patchGrid = (grid: Settings["appearance"]["grid"]) => {
    const next: Settings = { ...draft, appearance: { ...draftAppearance, grid } };
    dirty(next);
    pushPreview(next.appearance);
  };
  // FB2-08（§14.11）：色条设置 —— 与 patchGrid 同形；每个 onChange 都 dirty + pushPreview（即时预览纪律）
  const patchColorStrip = (patch: Partial<Settings["appearance"]["colorStrip"]>) => {
    const next: Settings = {
      ...draft,
      appearance: { ...draftAppearance, colorStrip: { ...draftAppearance.colorStrip, ...patch } },
    };
    dirty(next);
    pushPreview(next.appearance);
  };

  const chooseLibraryRoot = async () => {
    const dir = await pickDir({ directory: true });
    if (typeof dir === "string") dirty({ ...draft, libraryRoot: dir });
  };

  const onClearCache = async () => {
    setNotice(null);
    setError(null);
    try {
      await clearThumbnailCache("hd");
      setNotice("高清缩略图缓存已清除");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const onExportDiagnostics = async () => {
    const stamp = new Date().toISOString().replace(/[-:]/g, "").slice(0, 13);
    const target = await saveDialog({
      title: "导出诊断包",
      defaultPath: `teacell-diagnostics-${stamp}.zip`,
      filters: [{ name: "ZIP 压缩包", extensions: ["zip"] }],
    });
    if (!target) return;
    setExportingDiagnostics(true);
    setError(null);
    try {
      const report = await exportDiagnostics(target);
      const truncation = report.truncatedLogs > 0 ? `，${report.truncatedLogs} 个文件保留尾部` : "";
      setNotice(
        `诊断包已导出：${report.logFiles} 个日志文件${truncation}，${(report.bytes / 1024).toFixed(0)} KB`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setExportingDiagnostics(false);
    }
  };

  const onOpenHelp = async () => {
    setError(null);
    try {
      await openHelpPage();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const onOpenExternalPage = async (open: () => Promise<void>, getUrl: () => Promise<string>) => {
    setFailedExternalUrl(null);
    try {
      await open();
    } catch {
      try {
        setFailedExternalUrl(await getUrl());
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  };

  const aiRoute: "ai.superSearch" | "ai.tagging" | null =
    route === "ai.superSearch" || route === "ai.tagging" ? route : null;

  return (
    <div className="flex h-full">
      {/* 左侧分组导航（§13 FB-07：220~260px） */}
      <aside className="w-[224px] shrink-0 border-r border-[var(--color-border)] lg:w-[248px]">
        {onBack && (
          <button
            onClick={onBack}
            className="w-full border-b border-[var(--color-border)] px-3 py-2 text-left text-sm text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
          >
            ← 返回
          </button>
        )}
        <div className="p-2">
          {GROUPS.map((g) => {
            const active = route === g.key || (g.children?.some((c) => c.key === route) ?? false);
            const aiExpanded = g.key === "ai" && route.startsWith("ai.");
            return (
              <div key={g.key}>
                <button
                  onClick={() => setRoute(g.children ? (g.children[0].key as SettingsRoute) : (g.key as SettingsRoute))}
                  className={clsx(
                    "block w-full rounded px-2 py-1.5 text-left text-sm transition-colors",
                    active
                      ? "bg-[var(--color-surface)] font-medium text-[var(--color-text)]"
                      : "text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]",
                  )}
                >
                  {g.label}
                </button>
                {g.children && aiExpanded && (
                  <div className="ml-2 flex flex-col border-l border-[var(--color-border)] pl-2">
                    {g.children.map((c) => (
                      <button
                        key={c.key}
                        onClick={() => setRoute(c.key)}
                        className={clsx(
                          "block w-full rounded px-2 py-1 text-left text-sm transition-colors",
                          route === c.key
                            ? "bg-[var(--color-surface)] font-medium text-[var(--color-text)]"
                            : "text-[var(--color-text-secondary)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]",
                        )}
                      >
                        {c.label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </aside>

      {/* 右侧分组内容（§13 FB-07：取消 max-w-xl 小框，宽屏充分利用） */}
      <div className="relative min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[1040px] flex-col gap-6 px-6 py-6">
          {route !== "about" && (
            <div className="flex min-h-4 justify-end text-xs text-[var(--color-text-secondary)]" role="status" aria-live="polite">
              {autosaveStatus === "pending" && "更改将在短暂间隔后自动保存…"}
              {autosaveStatus === "saving" && "正在自动保存…"}
              {autosaveStatus === "saved" && "已自动保存"}
              {autosaveStatus === "error" && (
                <span className="text-[var(--color-danger)]">自动保存失败：{autosaveError}。修改设置后会重试。</span>
              )}
            </div>
          )}
          {notice && <p className="text-xs text-[var(--color-text-secondary)]" role="status">{notice}</p>}
          {error && <p className="text-xs text-[var(--color-danger)]" role="alert">{error}</p>}
          {route === "library" && (
            <>
            <PageHeader title="素材库与入库" description="设置总库位置，以及 RAW/JPG 等同源文件的显示和打标行为。" />
            <Group title="素材库位置">
              <Field label="总库位置" hint="选择后，导入的文件会复制到总库统一管理；未选择时仅建立索引，文件保留在原位置。">
                <div className="flex items-center gap-2">
                  <input
                    readOnly
                    value={draft.libraryRoot}
                    placeholder="未配置"
                    className="ui-control w-52 rounded-md px-2 py-1.5 text-sm outline-none"
                  />
                  <Button onClick={chooseLibraryRoot}>选择…</Button>
                  {draft.libraryRoot && (
                    <Button onClick={() => dirty({ ...draft, libraryRoot: "" })}>清除</Button>
                  )}
                </div>
              </Field>
              {/* W4-5：删「分库与改名」说明行（那是入库页的操作说明，不是设置） */}
            </Group>
            <Group title="同源文件">
              <Field
                label="同源文件打标同步"
                hint="同一张照片的 RAW 与 JPG 版本只需打标一次，确认后标签会自动同步到另一版本。"
              >
                <Toggle
                  checked={draftAppearance.kinship?.syncTagsToSiblings ?? true}
                  onChange={(v) => {
                    const next: Settings = {
                      ...draft,
                      appearance: { ...draftAppearance, kinship: { ...draftAppearance.kinship, syncTagsToSiblings: v } },
                    };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                />
              </Field>
              <Field
                label="素材库合并显示同源文件"
                hint="开启后，同一照片的 RAW 与 JPG 只显示一项（优先显示非 RAW）；关闭后分别显示。合并显示时，顶部数量仍按文件总数计算。"
              >
                <Toggle
                  checked={draftAppearance.kinship?.mergeInLibrary ?? false}
                  onChange={(v) => {
                    const next: Settings = {
                      ...draft,
                      appearance: { ...draftAppearance, kinship: { ...draftAppearance.kinship, mergeInLibrary: v } },
                    };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                />
              </Field>
            </Group>
            </>
          )}

          {aiRoute && (
            <AiPurposePanel
              usage={aiRoute}
              draft={draft}
              patchAi={patchAi}
              notify={setNotice}
              fail={setError}
            />
          )}

          {route === "ai.services" && (
            <Group title="服务管理">
              <ServiceManagement
                draft={draft}
                onPatchAi={patchAi}
                onPatchSettings={(patch) => dirty({ ...draft, ...patch })}
                notify={setNotice}
                fail={setError}
              />
            </Group>
          )}

          {route === "tags" && (
            <div className="flex flex-col gap-6">
              <PageHeader title="标签与分类" description="管理 AI 自动打标、手工填写和已停用的分类；分类行内可直接进入编辑或词条设置。" />
              {/* §9.2/§9.5：分面结构 + AI 行为 + 分类词条在同一个分面详情内完成；
                   说明标题置于容器外，描边容器只承载可编辑的分类行。 */}
              <FacetManagePanel />
            </div>
          )}

          {route === "general" && (
            <>
              <PageHeader title="外观与浏览" description="调整主题、素材框、悬停预览和主色色条的显示方式。" />
              <Group title="基础外观">
              <Field label="主题" hint="跟随系统 / 浅色 / 深色；切换即时预览并自动保存">
                <select
                  value={draft.theme}
                  onChange={(e) => {
                    const t = e.target.value as Settings["theme"];
                    applyTheme(t); // R-24：即时预览，不等保存
                    dirty({ ...draft, theme: t });
                  }}
                  className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                >
                  <option value="system">跟随系统</option>
                  <option value="light">浅色</option>
                  <option value="dark">深色</option>
                </select>
              </Field>
            </Group>

            {/* FB2-02（§8.4）：素材框 —— 统一比例 + 填充方式 + 双边格子大小。 */}
            <Group title="素材框">
              <Field label="统一比例" hint="素材库与导入页使用相同的缩略图比例。">
                <select
                  value={draft.appearance?.grid.cellAspect ?? "4:3"}
                  onChange={(e) => {
                    const next: Settings = {
                      ...draft,
                      appearance: { ...draftAppearance, grid: { ...draftAppearance.grid, cellAspect: e.target.value as CellAspect } },
                    };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                  className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                >
                  {CELL_ASPECTS.map((a) => (
                    <option key={a.value} value={a.value}>{a.label}</option>
                  ))}
                </select>
              </Field>
              <Field label="填充方式" hint="裁切填满、完整显示或按内容自动适应；不会拉伸素材。">
                <select
                  value={draftAppearance.grid.cellFit}
                  onChange={(e) => {
                    const next: Settings = {
                      ...draft,
                      appearance: { ...draftAppearance, grid: { ...draftAppearance.grid, cellFit: e.target.value as CellFit } },
                    };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                  className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                >
                  {CELL_FITS.map((f) => (
                    <option key={f.value} value={f.value}>{f.label}</option>
                  ))}
                </select>
              </Field>
              <Field label="留边区域填充主色" hint="完整显示产生留边时，用素材主色的浅色版本填充留边区域。">
                <Toggle
                  checked={draft.appearance.grid.matchDominantColor}
                  onChange={() => {
                    const next: Settings = {
                      ...draft,
                      appearance: { ...draftAppearance, grid: { ...draftAppearance.grid, matchDominantColor: !draftAppearance.grid.matchDominantColor } },
                    };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                />
              </Field>
              <Field label="素材库格子大小" hint="调整素材库缩略图大小；也可使用滚轮或快捷键缩放。">
                <RangeSteps value={draftAppearance.grid.libraryCellStep} max={CELL_STEPS.length - 1} labelForStep={(i) => `${CELL_STEPS[i]}px`} onChange={(v) => patchGrid({ ...draftAppearance.grid, libraryCellStep: v })} />
              </Field>
              <Field label="入库格子大小" hint="设置导入页缩略图的默认大小。">
                <RangeSteps value={draftAppearance.grid.importCellStep} max={CELL_STEPS.length - 1} labelForStep={(v) => `${CELL_STEPS[v]}px`} onChange={(v) => patchGrid({ ...draftAppearance.grid, importCellStep: v })} />
              </Field>
            </Group>

            <Group title="悬停预览">
              <Field label="悬停自动播放" hint="鼠标停留在视频卡片上时，在卡片内静音播放；移开后停止。">
                <Toggle
                  checked={draftAppearance.hoverPreview.enabled}
                  onChange={() => {
                    const hp = { ...draftAppearance.hoverPreview, enabled: !draftAppearance.hoverPreview.enabled };
                    const next: Settings = { ...draft, appearance: { ...draftAppearance, hoverPreview: hp } };
                    dirty(next);
                    pushPreview(next.appearance);
                  }}
                />
              </Field>
              {draftAppearance.hoverPreview.enabled && (
                <Field label="预览时长" hint="预览播放的片段长度（秒），2–10">
                  <input
                    type="number"
                    min={2}
                    max={10}
                    className="ui-control w-20 rounded-md px-2 py-1.5 text-sm outline-none"
                    value={String(draftAppearance.hoverPreview.previewSeconds)}
                    onChange={(v) => {
                      const n = Math.max(2, Math.min(10, Number(v.target.value) || 2));
                      const next: Settings = { ...draft, appearance: { ...draftAppearance, hoverPreview: { ...draftAppearance.hoverPreview, previewSeconds: n } } };
                      dirty(next);
                      pushPreview(next.appearance);
                    }}
                  />
                </Field>
              )}
              {draftAppearance.hoverPreview.enabled && (
                <Field label="素材库也启用悬停预览" hint="关闭后，素材库中的视频只显示封面；查看器中的播放不受影响。">
                  <Toggle
                    checked={draftAppearance.hoverPreview.inLibraryGrid}
                    onChange={() => {
                      const next: Settings = { ...draft, appearance: { ...draftAppearance, hoverPreview: { ...draftAppearance.hoverPreview, inLibraryGrid: !draftAppearance.hoverPreview.inLibraryGrid } } };
                      dirty(next);
                      pushPreview(next.appearance);
                    }}
                  />
                </Field>
              )}
            </Group>

            {/* FB2-08（§14.11）+ FB3-10（§12.2）+ FB4-03（§4.5）：算法主色色条设置。
                总开关关闭时位置/样式行不渲染；状态行（色条数据）即使总开关关闭也显示。 */}
            <Group title="主色色条">
              <Field label="显示主色色条" hint="只控制色条展示，不影响色板数据、色板计算或颜色搜索；关闭后仍保留数据与各位置设置。">
                <Toggle
                  checked={draftAppearance.colorStrip.enabled}
                  onChange={(v) => patchColorStrip({ enabled: v })}
                />
              </Field>
              {draftAppearance.colorStrip.enabled &&
                !draftAppearance.colorStrip.showInLibraryGrid &&
                !draftAppearance.colorStrip.showInViewer && (
                  <p className="px-4 text-xs text-[var(--color-text-secondary)]">已开启色条，但尚未选择显示位置。</p>
                )}
              {/* FB4-03：色板状态行 + 生成缺失色条（不随总开关隐藏；让用户先知道库里是否有可用色板） */}
              <Field label="色条数据" hint="只处理尚未生成或数据损坏的素材，不重复计算已有有效色板">
                <div className="flex min-w-0 flex-wrap items-center gap-2">
                  {paletteStatusError ? (
                    <span className="text-xs text-[var(--color-danger)]">
                      {paletteStatusError}
                      <button
                        type="button"
                        onClick={() => void refreshPaletteStatus()}
                        className="ml-2 underline decoration-dotted underline-offset-2"
                      >
                        重试
                      </button>
                    </span>
                  ) : paletteStatus === null ? (
                    <span className="text-xs text-[var(--color-text-secondary)]">正在检查色条数据…</span>
                  ) : (
                    <>
                      <span className="text-xs text-[var(--color-text-secondary)]">
                        {paletteStatus.missing > 0
                          ? `已生成 ${paletteStatus.ready} / 可生成 ${paletteStatus.eligible}；另有 ${paletteStatus.unavailable} 项暂不可生成`
                          : paletteStatus.eligible === 0
                            ? "当前没有可生成色条的图片或视频封面"
                            : `已生成 ${paletteStatus.ready} / 可生成 ${paletteStatus.eligible}；所有可生成素材均已完成`}
                      </span>
                      {paletteStatus.missing > 0 && !generatingMissing && (
                        <Button
                          disabled={paletteRunning || refilling}
                          onClick={() => void onGenerateMissingPalette()}
                        >
                          生成缺失色条（{paletteStatus.missing}）
                        </Button>
                      )}
                      {generatingMissing && (
                        <Button
                          onClick={() => {
                            void cancelMediaRefill().catch(() => undefined);
                            setGenerateResult("正在取消…");
                          }}
                        >
                          取消
                        </Button>
                      )}
                    </>
                  )}
                </div>
              </Field>
              {generateProgress && generatingMissing && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">
                  生成中 {generateProgress.done}/{generateProgress.total}（成功 {generateProgress.success} · 跳过 {generateProgress.skipped} · 失败 {generateProgress.failed}）
                </p>
              )}
              {generateResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{generateResult}</p>
              )}
              {/* W4-5：色条细节折叠（7 控件压成 1 开关 + 折叠，外观与浏览可见控件 ≤12） */}
              {draftAppearance.colorStrip.enabled && (
                <div className="px-4 py-1">
                  <button
                    type="button"
                    onClick={() => setShowColorDetails((v) => !v)}
                    className="text-xs text-[var(--color-text-secondary)] hover:text-[var(--color-text)]"
                  >
                    ▸ 色条细节{showColorDetails ? "（收起）" : ""}
                  </button>
                  {showColorDetails && (
                    <div className="mt-1 flex flex-col gap-1">
                <Field label="素材库卡片显示" hint="仅控制素材卡片上的显示位置，受上方总开关控制。">
                    <Toggle
                      checked={draftAppearance.colorStrip.showInLibraryGrid}
                      onChange={(v) => patchColorStrip({ showInLibraryGrid: v })}
                    />
                  </Field>
                <Field label="大图浏览显示" hint="在大图浏览的标签栏上方显示色条；全屏时隐藏">
                    <Toggle
                      checked={draftAppearance.colorStrip.showInViewer}
                      onChange={(v) => patchColorStrip({ showInViewer: v })}
                    />
                  </Field>
                <Field label="色条高度" hint="网格用细、大图用厚">
                    <select
                      value={draftAppearance.colorStrip.height}
                      onChange={(e) => patchColorStrip({ height: e.target.value as Settings["appearance"]["colorStrip"]["height"] })}
                      className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                    >
                      <option value="thin">细 6px</option>
                      <option value="normal">标准 10px</option>
                      <option value="thick">厚 16px</option>
                    </select>
                  </Field>
                <Field label="分段方式" hint="按占比更能体现调性；等宽接近调色参考站的观感">
                    <select
                      value={draftAppearance.colorStrip.mode}
                      onChange={(e) => patchColorStrip({ mode: e.target.value as Settings["appearance"]["colorStrip"]["mode"] })}
                      className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                    >
                      <option value="ratio">按占比</option>
                      <option value="equal">等宽</option>
                    </select>
                  </Field>
                <Field label="显示条数" hint="色条最多显示前 N 个主色">
                    <select
                      value={String(draftAppearance.colorStrip.count)}
                      onChange={(e) => patchColorStrip({ count: Number(e.target.value) as Settings["appearance"]["colorStrip"]["count"] })}
                      className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                    >
                      <option value="4">4</option>
                      <option value="6">6</option>
                      <option value="8">8</option>
                    </select>
                  </Field>
                    </div>
                  )}
                </div>
              )}
            </Group>
            </>
          )}

          {route === "data" && (
            <>
            <PageHeader title="存储与维护" description="查看软件数据位置，管理缓存、索引、备份和重置操作。" />
            <Group title="存储位置">
              <Field label="软件数据保存位置" hint="数据库与缩略图所在目录；备份或转移素材库时，请复制此目录。">
                <div className="flex items-center gap-2">
                  <span className="max-w-52 truncate text-xs text-[var(--color-text-secondary)]" title={dataDir}>
                    {dataDirError ? "暂不可用" : dataDir || "…"}
                  </span>
                  <Button onClick={() => void openDataDir()}>打开文件夹</Button>
                </div>
              </Field>
            </Group>
            <Group title="缓存管理">
              <Field label="高清缩略图缓存" hint="浏览大图时生成的清晰版缩略图，不影响原文件。删除后会按需重新生成；超过容量上限时，会自动清理最久未使用的内容。">
                <div className="flex items-center gap-2">
                  <input
                    type="number"
                    className="ui-control w-20 rounded-md px-2 py-1.5 text-sm outline-none"
                    value={String(draft.thumbnailCacheMb)}
                    onChange={(v) => dirty({ ...draft, thumbnailCacheMb: Math.max(0, Number(v.target.value) || 0) })}
                  />
                  <Button onClick={() => void onClearCache()}>立即清除</Button>
                </div>
              </Field>
              <Field
                label="Ollama 安装包缓存"
                hint={
                  installerInfo
                    ? `约 ${(installerInfo.size / 1024 / 1024).toFixed(0)} MB，供离线重装；删除后需重新下载`
                    : "未缓存安装包（一键安装时自动下载）"
                }
              >
                {installerInfo && (
                  <Button variant="danger" disabled={removingInstaller} onClick={() => void onRemoveInstaller()}>
                    {removingInstaller ? "删除中…" : "删除安装包"}
                  </Button>
                )}
              </Field>
              <Field label="回收站" hint="超过保留期限后，会在启动时自动清理；设为 0 时不自动清理。">
                <input
                  type="number"
                  className="ui-control w-20 rounded-md px-2 py-1.5 text-sm outline-none"
                  value={String(draft.trashRetentionDays)}
                  onChange={(v) => dirty({ ...draft, trashRetentionDays: Math.max(0, Number(v.target.value) || 0) })}
                />
              </Field>
              <Field
                label="视频代理缓存"
                hint={
                  proxyStats && proxyStats.count > 0
                    ? `当原视频无法直接播放时生成的兼容副本（当前 ${proxyStats.count} 个，约 ${(proxyStats.bytes / 1024 / 1024).toFixed(1)} MB）。清理不会删除原视频，需要时会重新生成。`
                    : "当原视频无法直接播放时，会自动生成兼容副本。清理不会删除原视频；当前没有可清理的副本。"
                }
              >
                <Button variant="danger" disabled={clearingProxies} onClick={() => void onClearVideoProxies()}>
                  {clearingProxies ? "清理中…" : "清理全部"}
                </Button>
              </Field>
            </Group>
            <Group title="数据维护">
              <Field
                label="媒体信息更新"
                hint="重新读取分辨率、时长、编码、帧率和拍摄参数等信息，不修改原文件或标签。适用于旧素材信息缺失，或软件升级后新增了信息类型。失败的项目会记录原因，不影响其他素材。"
              >
                <div className="flex items-center gap-2">
                  {/* 与色板回算互斥（FX-12 后端也会拒绝）；前端 disabled 是为了不让用户点了才知道 */}
                  <Button disabled={refilling || paletteRunning} onClick={() => void onRefill("missing")}>
                    仅补充缺失信息
                  </Button>
                  <Button disabled={refilling || paletteRunning} onClick={() => void onRefill("all")}>
                    重新读取全部视频
                  </Button>
                  {refilling ? (
                    <Button onClick={onCancelRefill}>取消</Button>
                  ) : null}
                </div>
              </Field>
              {refillProgress && refilling && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">
                  更新中 {refillProgress.done}/{refillProgress.total}（成功 {refillProgress.success} · 失败 {refillProgress.failed} · 跳过 {refillProgress.skipped}）
                </p>
              )}
              {refillResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{refillResult}</p>
              )}
              {/* FB2-08（§14.7）+ FB3-11（§13.2）：算法色板回算（白话说明 + 危险性写清） */}
              <Field
                label="色板重新计算"
                hint="从图片或视频封面中提取主要颜色，用于生成色条和按颜色筛选。不会调用 AI，也不会创建标签。仅补充缺失项会跳过已有色板；全部重新计算会覆盖旧色板，适合结果明显不准确时使用。视频需先生成封面才能处理。"
              >
                <div className="flex items-center gap-2">
                  <Button aria-label="补充缺失的色板" disabled={paletteRunning || refilling} onClick={() => void onRescanPalette("missing")}>
                    仅补充缺失项
                  </Button>
                  <Button aria-label="重新计算全部色板" disabled={paletteRunning || refilling} onClick={() => void onRescanPalette("all")}>
                    全部重新计算
                  </Button>
                  {paletteRunning ? (
                    <Button
                      onClick={() => {
                        void cancelMediaRefill().catch(() => undefined);
                        setPaletteResult("正在取消…");
                      }}
                    >
                      取消
                    </Button>
                  ) : null}
                </div>
              </Field>
              {paletteProgress && paletteRunning && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">
                  色板计算中 {paletteProgress.done}/{paletteProgress.total}（成功 {paletteProgress.success} · 跳过 {paletteProgress.skipped} · 失败 {paletteProgress.failed}）
                </p>
              )}
              {paletteResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{paletteResult}</p>
              )}
              {/* R1-2：色板关系表重建（从 palette_json 重灌 asset_palette_colors，不解码图片）
                  —— 「前三色包含红」类筛选的表源；老库/重置后为空时点一次即可补齐 */}
              <Field
                label="颜色筛选索引重建"
                hint="根据素材现有的色板数据，重新生成按颜色筛选所需的索引，不会重新计算色板。色板数据更新后如有遗漏，可再次运行。"
              >
                <div className="flex items-center gap-2">
                  <Button disabled={paletteColorsRunning || paletteRunning || refilling} onClick={() => void onRescanPaletteColors()}>
                    {paletteColorsRunning ? "重建中…" : "重建颜色索引"}
                  </Button>
                </div>
              </Field>
              {paletteColorsResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{paletteColorsResult}</p>
              )}
              {/* W5d（§W5d）：感知哈希回填（相似图去重的前提；新导入的图片已自动计算，这里只补存量） */}
              <Field
                label="相似图识别数据"
                hint="为图片生成相似度识别所需的数据，用于查找重复素材和相似画面。新导入的图片会自动处理，此处用于补齐旧素材；全部重新计算会覆盖已有结果。"
              >
                <div className="flex items-center gap-2">
                  <Button aria-label="补充缺失的相似图数据" disabled={phashRunning || paletteRunning || refilling} onClick={() => void onRescanPhash("missing")}>
                    仅补充缺失项
                  </Button>
                  <Button aria-label="重新计算全部相似图数据" disabled={phashRunning || paletteRunning || refilling} onClick={() => void onRescanPhash("all")}>
                    全部重新计算
                  </Button>
                  {phashRunning ? (
                    <Button
                      onClick={() => {
                        void cancelMediaRefill().catch(() => undefined);
                        setPhashResult("正在取消…");
                      }}
                    >
                      取消
                    </Button>
                  ) : null}
                </div>
              </Field>
              {phashProgress && phashRunning && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">
                  相似图数据生成中 {phashProgress.done}/{phashProgress.total}（成功 {phashProgress.success} · 跳过 {phashProgress.skipped} · 失败 {phashProgress.failed}）
                </p>
              )}
              {phashResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{phashResult}</p>
              )}
              {/* R1-2：图片宽高存量回填（命令已注册但此前前端不可达；与其余回填共用互斥闸） */}
              <Field
                label="图片分辨率回填"
                hint="重新读取图片的宽高信息，供分辨率筛选使用。仅处理未读取到宽高的旧素材，新导入的图片会自动读取；全部重新计算会覆盖已有结果。"
              >
                <div className="flex items-center gap-2">
                  <Button aria-label="补充缺失的图片分辨率" disabled={dimRunning || phashRunning || paletteRunning || refilling} onClick={() => void onRescanDimensions("missing")}>
                    仅补充缺失项
                  </Button>
                  <Button aria-label="重新计算全部图片分辨率" disabled={dimRunning || phashRunning || paletteRunning || refilling} onClick={() => void onRescanDimensions("all")}>
                    全部重新计算
                  </Button>
                  {dimRunning ? (
                    <Button
                      onClick={() => {
                        void cancelMediaRefill().catch(() => undefined);
                        setDimResult("正在取消…");
                      }}
                    >
                      取消
                    </Button>
                  ) : null}
                </div>
              </Field>
              {dimProgress && dimRunning && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">
                  分辨率更新中 {dimProgress.done}/{dimProgress.total}（成功 {dimProgress.success} · 跳过 {dimProgress.skipped} · 失败 {dimProgress.failed}）
                </p>
              )}
              {dimResult && (
                <p className="px-4 py-2 text-xs text-[var(--color-text-secondary)]">{dimResult}</p>
              )}
              {/* W5c：数据库备份/恢复（指导书 §W5c）——你的库唯一的副本入口 */}
            </Group>
            <Group title="备份与恢复">
              <Field
                label="数据库备份与恢复"
                hint="备份会将素材记录、标签和 AI 配置导出为一个 .db 文件，建议保存到移动硬盘或网盘。恢复会整体替换当前数据库，并自动重启软件。"
              >
                <BackupRestorePanel notify={setNotice} fail={setError} />
              </Field>
            </Group>
            <Group title="危险操作">
              <ResetDataPanel
                notify={setNotice}
                fail={setError}
                onDataReset={async (sel) => {
                  // 偏好设置被重置：重新拉取设置并替换 draft（draft 已存在，load 后需手动同步）
                  if (sel.preferences) {
                    await load();
                    const fresh = useSettingsStore.getState().settings;
                    if (fresh) setDraft(structuredClone(fresh));
                  }
                  // 素材删除后旧选中 id 已失效；标签删除后筛选条与标签树也必须同步失效
                  if (sel.assets || sel.assetFiles) useSelectionStore.getState().clear();
                  if (sel.tags) {
                    useLibraryStore.getState().clearTagFilters();
                    useTagStore.getState().clear();
                  }
                  const refreshes: Promise<void>[] = [];
                  if (sel.assets || sel.assetFiles || sel.tags) refreshes.push(useLibraryStore.getState().refresh());
                  if (sel.tags) refreshes.push(useTagStore.getState().refresh());
                  if (sel.assets || sel.assetFiles) refreshes.push(useMetadataStore.getState().refresh());
                  await Promise.all(refreshes);
                }}
              />
            </Group>
            </>
          )}

          {route === "about" && (
            <>
            <PageHeader title="关于" description="查看应用信息、项目地址与作者主页。" />
            <Group title="应用信息">
              <Field
                label="茶馆AI素材管理 / TeaCell AI Media Manager"
                hint="本地优先的图片与视频素材管理 / Local-first photo and video asset manager"
              >
                <span className="text-sm text-[var(--color-text-secondary)]">V1.0.2 公测版</span>
              </Field>
              <Field label="项目地址" hint="查看源代码、版本与项目说明。">
                <Button onClick={() => void onOpenExternalPage(openProjectPage, getProjectPageUrl)}>
                  GitHub · SSRYLJRSS/TeaCell_AiMedias
                </Button>
              </Field>
              <Field label="关于作者" hint="打开作者的小红书主页。">
                <Button onClick={() => void onOpenExternalPage(openAuthorPage, getAuthorPageUrl)}>
                  小红书主页
                </Button>
              </Field>
              <Field label="许可证" hint="本项目按仓库根目录的许可证文件发布。">
                <Button onClick={() => void onOpenExternalPage(openLicensePage, getLicensePageUrl)}>MIT License</Button>
              </Field>
            </Group>
            {failedExternalUrl && (
              <p className="select-all break-all px-4 pb-3 text-xs text-[var(--color-text-secondary)]" role="status">
                浏览器未能打开，请复制此地址后手动访问：{failedExternalUrl}
              </p>
            )}
            </>
          )}

          {route === "diagnostics" && (
            <>
            <PageHeader title="诊断与支持" description="查看日志、导出诊断包并获取问题反馈所需的信息。" />
            <Group title="日志与诊断">
              <Field label="数据与日志目录" hint="数据库、缩略图与日志所在目录">
                <div className="flex items-center gap-2">
                  <span className="max-w-52 truncate text-xs text-[var(--color-text-secondary)]" title={dataDir}>
                    {dataDirError ? "暂不可用" : dataDir || "…"}
                  </span>
                  <Button onClick={() => void openDataDir()}>打开文件夹</Button>
                </div>
              </Field>
              <Field label="诊断日志级别" hint="默认 info；排障时可临时切到 debug 或 trace，完成后建议切回 info 以控制日志体积。">
                <select
                  aria-label="诊断日志级别"
                  value={draft.logLevel}
                  onChange={(e) => dirty({ ...draft, logLevel: e.target.value as Settings["logLevel"] })}
                  className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
                >
                  <option value="info">info（日常）</option>
                  <option value="debug">debug（详细）</option>
                  <option value="trace">trace（极详细）</option>
                </select>
              </Field>
              <Field label="运行日志" hint="日志保留 30 天；诊断包只包含日志、版本、系统与数据库摘要，不包含 API Key 或素材内容。">
                <div className="flex items-center gap-2">
                  <Button onClick={() => void openLogsDir()}>打开日志目录</Button>
                  <Button disabled={exportingDiagnostics} onClick={() => void onExportDiagnostics()}>
                    {exportingDiagnostics ? "导出中…" : "导出诊断包…"}
                  </Button>
                </div>
              </Field>
            </Group>
            <Group title="支持">
              <Field label="使用帮助" hint="在系统默认浏览器中打开使用说明和操作指南。">
                <Button onClick={() => void onOpenHelp()}>打开使用帮助</Button>
              </Field>
              <Field label="反馈" hint="提交功能建议或问题反馈">
                <Button onClick={() => void onOpenExternalPage(openFeedbackPage, getFeedbackPageUrl)}>
                  在 GitHub 提交问题
                </Button>
              </Field>
              {failedExternalUrl && (
                <p className="select-all break-all px-4 pb-3 text-xs text-[var(--color-text-secondary)]" role="status">
                  浏览器未能打开，请复制此地址后手动访问：{failedExternalUrl}
                </p>
              )}
            </Group>
            </>
          )}

        </div>
      </div>
    </div>
  );
}

/** §6.2 AI 用途面板：只选择「此功能使用的服务」+ 功能参数，不重复渲染服务管理（§8.7）。 */
function AiPurposePanel({
  usage,
  draft,
  patchAi,
  notify,
  fail,
}: {
  usage: "ai.superSearch" | "ai.tagging";
  draft: Settings;
  patchAi: (patch: Partial<Settings["ai"]>) => void;
  notify: (msg: string) => void;
  fail: (msg: string) => void;
}) {
  const isSuperSearch = usage === "ai.superSearch";
  const title = isSuperSearch ? "超级搜索" : "自动打标";

  return (
    <Group title={title}>
      <div className="px-4 py-3">
        <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
          {isSuperSearch
            ? "默认自动选择已配置的在线 AI 服务；也可在下方手动指定服务。使用在线服务时，搜索内容会发送给服务提供方。"
            : "通过所选服务分析素材并建议标签；使用在线服务时，素材会发送给服务提供方。"}
        </p>
      </div>

      {/* §8.2 此功能使用的服务：与另一功能可共用或独立选择；修改一个不影响另一个 */}
      <UsageBindingLine
        usage={usage === "ai.superSearch" ? "super_search" : "tagging"}
        defaultProfile={draft.ai.profiles.find((profile) => profile.id === draft.ai.activeProfile)}
        notify={notify}
        fail={fail}
      />

      {/* W0-6：删「打标时机」死配置（auto_tagging 后端零消费点，选「自动」无任何效果）。
          替代品为 W5g「一键送打标」。 */}
      {!isSuperSearch && (
        <Field label="视频 AI 打标" hint="允许 AI 分析视频，具体方式由「视频打标模式」决定。">
          <span className="flex items-center gap-2">
            <Toggle checked={draft.ai.videoTagging} onChange={(v) => patchAi({ videoTagging: v })} />
            <span className="w-16 text-xs whitespace-nowrap text-[var(--color-text-secondary)]">
              {draft.ai.videoTagging ? "已开启" : "未开启"}
            </span>
          </span>
        </Field>
      )}
      {/* FB2-07（§13.6）：视频打标子模式与帧数 —— 仅当视频打标开启时显示，关闭时无意义避免误导 */}
      {!isSuperSearch && draft.ai.videoTagging && (
        <>
          <Field
            label="视频打标模式"
            hint={
              draft.ai.videoTaggingMode === "frames"
                ? "从视频中抽取多帧分别识别，结果更全面，但处理时间更长。"
                : "每个视频只分析一张封面，速度更快。未生成封面的视频会使用第一帧。"
            }
          >
            <select
              value={draft.ai.videoTaggingMode === "frames" ? "frames" : "cover"}
              onChange={(e) => patchAi({ videoTaggingMode: e.target.value as Settings["ai"]["videoTaggingMode"] })}
              className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
            >
              <option value="cover">封面打标（更快）</option>
              <option value="frames">抽帧打标（更准确）</option>
            </select>
          </Field>
          {draft.ai.videoTaggingMode === "frames" && (
            <Field label="抽帧数" hint="每个视频抽取的帧数（2–8）；数量越多，识别更全面，处理时间也越长。">
              <TextInput
                type="number"
                value={String(draft.ai.videoFrameCount)}
                onChange={(v) => patchAi({ videoFrameCount: Math.max(2, Math.min(8, Number(v) || 3)) })}
              />
            </Field>
          )}
        </>
      )}
      {!isSuperSearch && (
        <Field
          label="在线服务每批处理数量"
          hint="不会改变任务中的素材总数，只调整在线服务每批处理的数量（10–50）。"
        >
          <div className="flex items-center gap-2">
            <input
              type="range"
              min={10}
              max={50}
              step={1}
              value={Math.max(10, Math.min(50, draft.ai.batchLimit))}
              onChange={(e) => patchAi({ batchLimit: Math.max(10, Math.min(50, Number(e.target.value) || 30)) })}
              className="ui-range w-40"
              aria-label="在线服务每批处理数量"
            />
            <span className="w-10 text-right text-xs tabular-nums text-[var(--color-text-secondary)]">
              {Math.max(10, Math.min(50, draft.ai.batchLimit))}
            </span>
          </div>
        </Field>
      )}
      {!isSuperSearch && (
        <Field
          label="本机服务每批处理数量"
          hint="不会改变任务中的素材总数。配置较低时可减少处理压力（1–20）。"
        >
          <div className="flex items-center gap-2">
            <input
              type="range"
              min={1}
              max={20}
              step={1}
              value={Math.max(1, Math.min(20, draft.ai.localBatchLimit ?? 5))}
              onChange={(e) => patchAi({ localBatchLimit: Math.max(1, Math.min(20, Number(e.target.value) || 5)) })}
              className="ui-range w-40"
              aria-label="本机服务每批处理数量"
            />
            <span className="w-10 text-right text-xs tabular-nums text-[var(--color-text-secondary)]">
              {Math.max(1, Math.min(20, draft.ai.localBatchLimit ?? 5))}
            </span>
          </div>
        </Field>
      )}
    </Group>
  );
}

function PageHeader({ title, description }: { title: string; description: string }) {
  return (
    <header>
      <h1 className="text-base font-medium text-[var(--color-text)]">{title}</h1>
      <p className="mt-1 text-sm leading-5 text-[var(--color-text-secondary)]">{description}</p>
    </header>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h2 className="mb-2 text-xs font-medium tracking-wide text-[var(--color-text-secondary)] uppercase">{title}</h2>
      <div className="divide-y divide-[var(--color-border)] rounded-lg border border-[var(--color-border)]">
        {children}
      </div>
    </section>
  );
}

/** FB2-01：档位化 slider（0..max 离散档）。只值 0..7，步子固定；拖动即时预览。 */
function RangeSteps({
  value,
  max,
  labelForStep,
  onChange,
}: {
  value: number;
  max: number;
  labelForStep: (v: number) => string;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <input
        type="range"
        min={0}
        max={max}
        step={1}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="ui-range w-48"
        aria-label="格子大小档位"
      />
      <span className="w-14 shrink-0 text-xs text-[var(--color-text-secondary)]">{labelForStep(value)}</span>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-6">
      <div className="min-w-0 sm:max-w-[46%]">
        <p className="text-sm text-[var(--color-text)]">{label}</p>
        {hint && <p className="mt-0.5 text-xs leading-5 text-[var(--color-text-secondary)]">{hint}</p>}
      </div>
      {/* 控件列：窄屏整行、宽屏靠右且可换行，挤压时缩进一行 */}
      <div className="flex min-w-0 flex-wrap items-center gap-2 sm:justify-end">{children}</div>
    </div>
  );
}

function TextInput({
  value,
  onChange,
  placeholder,
  type = "text",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: string;
}) {
  return (
    <input
      type={type}
      value={value}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
      className="ui-control w-52 rounded-md px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)]"
    />
  );
}

function Toggle({
  checked,
  onChange,
  disabled = false,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`h-5 w-9 rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${checked ? "bg-[var(--color-accent)]" : "bg-[var(--color-border)]"}`}
    >
      <span
        className={`block h-4 w-4 translate-x-0.5 rounded-full bg-white transition-transform ${checked ? "translate-x-[18px]" : ""}`}
      />
    </button>
  );
}

/** W5c 备份/恢复面板（存储与维护）：备份 = save 对话框 → backupDb；
 *  恢复 = open 对话框 → 两步强警告确认 → restoreDb（成功后应用自动重启，Promise 不返回）。
 *  运行中任务阻断在 后端命令层（入库/回填/导出/AI 批次）。 */
function BackupRestorePanel({ notify, fail }: { notify: (m: string) => void; fail: (m: string) => void }) {
  const [backing, setBacking] = useState(false);
  const [confirmStep, setConfirmStep] = useState<0 | 1 | 2>(0);
  const [picked, setPicked] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);

  const onBackup = async () => {
    const target = await saveDialog({
      title: "备份数据库",
      defaultPath: `library-backup-${new Date().toISOString().slice(0, 10)}.db`,
      filters: [{ name: "SQLite 数据库", extensions: ["db"] }],
    });
    if (!target) return;
    setBacking(true);
    try {
      await backupDb(target);
      notify("备份完成");
    } catch (e) {
      fail(e instanceof Error ? e.message : String(e));
    } finally {
      setBacking(false);
    }
  };

  const onPick = async () => {
    const source = await pickDir({
      title: "选择备份文件",
      multiple: false,
      directory: false,
      filters: [{ name: "SQLite 数据库", extensions: ["db"] }],
    });
    if (!source || Array.isArray(source)) return;
    setPicked(source);
    setConfirmStep(1);
  };

  const onRestore = async () => {
    if (!picked) return;
    setRestoring(true);
    try {
      await restoreDb(picked); // 成功 → 后端 app.restart()，本 Promise 永不 resolve
      notify("恢复完成");
    } catch (e) {
      fail(e instanceof Error ? e.message : String(e));
    } finally {
      setRestoring(false);
      setConfirmStep(0);
      setPicked(null);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <Button disabled={backing || restoring} onClick={() => void onBackup()}>
          {backing ? "备份中…" : "备份数据库…"}
        </Button>
        <Button variant="danger" disabled={backing || restoring} onClick={() => void onPick()}>
          从备份恢复…
        </Button>
      </div>
      {confirmStep === 1 && picked && (
        <div className="rounded-md border border-[var(--color-danger)] px-3 py-2 text-xs leading-5">
          <p className="font-medium text-[var(--color-danger)]">
            即将用备份文件覆盖当前素材库：{displayBasename(picked)}
          </p>
          <p className="mt-1 text-[var(--color-text-secondary)]">
            当前库里「备份之后」新做的入库、打标、评级等改动会全部丢失。有运行中的入库/导出/打标任务时恢复会被拒绝。建议先点「备份数据库」存一份当前状态。
          </p>
          <div className="mt-2 flex items-center gap-2">
            <Button variant="danger" onClick={() => setConfirmStep(2)}>
              我已了解，继续
            </Button>
            <Button
              onClick={() => {
                setConfirmStep(0);
                setPicked(null);
              }}
            >
              取消
            </Button>
          </div>
        </div>
      )}
      {confirmStep === 2 && picked && (
        <div className="rounded-md border border-[var(--color-danger)] px-3 py-2 text-xs leading-5">
          <p className="font-medium text-[var(--color-danger)]">最后确认：恢复后软件会立即自动重启</p>
          <p className="mt-1 text-[var(--color-text-secondary)]">此操作不可撤销（当前库会先存为 library.db.old 保底，但请勿依赖）。</p>
          <div className="mt-2 flex items-center gap-2">
            <Button variant="danger" disabled={restoring} onClick={() => void onRestore()}>
              {restoring ? "恢复中…" : "开始恢复并重启"}
            </Button>
            <Button
              disabled={restoring}
              onClick={() => {
                setConfirmStep(0);
                setPicked(null);
              }}
            >
              取消
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

/** 重置数据面板（存储与维护）：勾选分类 → 两步确认 → resetAppData。
 *  “素材库记录”只清索引与派生数据；原始文件单列并要求输入确认短语。 */
const RESET_ITEMS: { key: keyof ResetDataSelection; label: string; hint: string; danger?: boolean }[] = [
  { key: "assets", label: "素材库记录", hint: "所有素材记录、搜索索引、导出任务，以及缩略图、预览和代理缓存；不影响原始文件" },
  {
    key: "assetFiles",
    label: "原始素材文件",
    hint: "永久删除磁盘上的图片/视频；成功删除的文件会同时移除素材记录，删除失败的记录会保留",
    danger: true,
  },
  { key: "exportTasks", label: "导出任务记录", hint: "导出队列和历史记录，不影响素材和原始文件" },
  { key: "tags", label: "标签与分类", hint: "所有标签、分类结构、别名和打标记录" },
  { key: "aiTasks", label: "AI 打标任务", hint: "打标批次与建议记录" },
  { key: "aiConnections", label: "AI 服务配置", hint: "服务配置、功能绑定，以及系统里保存的 API 密钥" },
  { key: "preferences", label: "偏好设置", hint: "恢复全部默认设置（主题、外观、总库位置、缓存上限等）" },
  { key: "searchState", label: "搜索条件与界面草稿", hint: "清除超级搜索条件和最近使用的筛选字段（仅本机浏览器数据）" },
  { key: "caches", label: "缓存文件", hint: "缩略图/预览/视频代理缓存文件（不影响素材记录）" },
  { key: "logs", label: "诊断日志", hint: "删除本软件生成的运行日志；正在占用的日志文件可能会保留" },
];
const RESET_NONE: ResetDataSelection = {
  assets: false,
  assetFiles: false,
  exportTasks: false,
  tags: false,
  aiTasks: false,
  aiConnections: false,
  preferences: false,
  searchState: false,
  caches: false,
  logs: false,
};

function ResetDataPanel({
  notify,
  fail,
  onDataReset,
}: {
  notify: (msg: string) => void;
  fail: (msg: string) => void;
  onDataReset: (sel: ResetDataSelection) => Promise<void>;
}) {
  const [sel, setSel] = useState<ResetDataSelection>(RESET_NONE);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [confirmationText, setConfirmationText] = useState("");
  const any = Object.values(sel).some(Boolean);
  const allSelected = RESET_ITEMS.every((item) => sel[item.key]);
  const destructive = sel.assetFiles || allSelected;
  const requiredPhrase = allSelected ? "恢复出厂设置" : "删除原文件";
  const confirmationReady = !destructive || confirmationText.trim() === requiredPhrase;

  const toggleAll = () => {
    const next = !allSelected;
    const nextSelection: ResetDataSelection = { ...RESET_NONE };
    RESET_ITEMS.forEach((item) => {
      nextSelection[item.key] = next;
    });
    setSel(nextSelection);
    setConfirming(false);
    setConfirmationText("");
  };

  const onReset = async () => {
    const done = sel;
    setBusy(true);
    setResult(null);
    try {
      const r = await resetAppData(done);
      const parts: string[] = [];
      if (r.assetsDeleted > 0) parts.push(`素材 ${r.assetsDeleted} 条`);
      if (r.assetFilesDeleted > 0) parts.push(`原始文件 ${r.assetFilesDeleted} 个`);
      if (r.assetFilesFailed > 0) parts.push(`原始文件删除失败 ${r.assetFilesFailed} 个（记录已保留）`);
      if (r.exportTasksDeleted > 0) parts.push(`导出任务记录 ${r.exportTasksDeleted} 条`);
      if (r.tagsDeleted > 0) parts.push(`标签 ${r.tagsDeleted} 条`);
      if (r.aiTasksDeleted > 0) parts.push(`AI 任务记录 ${r.aiTasksDeleted} 条`);
      if (r.connectionsDeleted > 0) parts.push(`AI 服务配置 ${r.connectionsDeleted} 个`);
      if (r.preferencesReset) parts.push("设置已恢复默认");
      if (r.searchStateReset) parts.push("搜索条件已清除");
      if (r.cacheFilesDeleted > 0) parts.push(`缓存文件 ${r.cacheFilesDeleted} 个`);
      if (r.logFilesDeleted > 0) parts.push(`诊断日志 ${r.logFilesDeleted} 个`);
      const msg = parts.length > 0 ? `重置完成：已清除${parts.join("，")}` : "重置完成：所选数据本来就是空的";
      setResult(msg);
      notify(msg);
      setSel(RESET_NONE);
      setConfirming(false);
      setConfirmationText("");
      // localStorage 不在数据库事务里。用户明确选择、标签结构失效或偏好恢复默认时一并清掉，
      // 避免陈旧条件继续 hydrate；清缓存等无关操作不误删用户已保存的搜索条件。
      if (done.searchState || done.tags || done.preferences) {
        try {
          localStorage.removeItem("super-search-conditions");
          localStorage.removeItem("qb:recent-fields");
        } catch {
          /* localStorage 不可用时静默跳过（非阻断步骤） */
        }
      }
      await onDataReset(done);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setResult(msg);
      fail(msg);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-2 px-4 py-3">
      <div>
        <p className="text-sm text-[var(--color-text)]">重置数据</p>
        <p className="mt-0.5 text-xs leading-5 text-[var(--color-text-secondary)]">
          按需勾选要清空的数据。默认不会删除图片、视频原文件；勾选「原始素材文件」后会永久删除对应磁盘文件。全选等同恢复出厂设置，请先备份数据库和原始素材。
        </p>
      </div>
      <div className="grid gap-1.5 sm:grid-cols-2">
        {RESET_ITEMS.map((item) => (
          <label key={item.key} className="flex items-start gap-2 text-sm">
            <input
              type="checkbox"
              className="mt-1 accent-[var(--color-accent)]"
              checked={sel[item.key]}
              disabled={busy}
              onChange={(e) => {
                setSel((s) => ({ ...s, [item.key]: e.target.checked }));
                setConfirming(false);
                setConfirmationText("");
              }}
            />
            <span className="min-w-0">
              <span className={item.danger ? "text-[var(--color-danger)]" : undefined}>{item.label}</span>
              <span className="block text-xs leading-4 text-[var(--color-text-secondary)]">{item.hint}</span>
            </span>
          </label>
        ))}
      </div>
      <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
        本地 AI 模型和 Ollama 安装包由服务管理/缓存管理单独管理，不纳入此处全选；模型空间请到「AI 与模型 → 服务管理」逐个删除，安装包请在「缓存管理」清理。
      </p>
      <div className="flex flex-wrap items-center gap-2">
        {confirming ? (
          <>
            <div className="w-full rounded-md border border-[var(--color-danger)] px-3 py-2 text-xs leading-5">
              <p className="font-medium text-[var(--color-danger)]">
                {allSelected
                  ? "这是恢复出厂设置：会清空软件数据，并永久删除所选原始素材文件。"
                  : sel.assetFiles
                    ? "即将永久删除在库与回收站中的全部图片/视频原文件，成功项无法恢复。"
                    : "确认清空所选软件数据？此操作不可撤销。"}
              </p>
              <p className="mt-1 text-[var(--color-text-secondary)]">
                {destructive
                  ? `请输入“${requiredPhrase}”后继续。删除失败的文件会保留素材记录，便于修复后重试。`
                  : "请先确认没有需要备份的内容。"}
              </p>
              {destructive && (
                <input
                  aria-label="确认短语"
                  value={confirmationText}
                  disabled={busy}
                  onChange={(e) => setConfirmationText(e.target.value)}
                  placeholder={requiredPhrase}
                  className="ui-control mt-2 w-56 rounded-md px-2 py-1.5 text-sm outline-none focus:border-[var(--color-danger)]"
                />
              )}
            </div>
            <Button variant="danger" disabled={busy || !any || !confirmationReady} onClick={() => void onReset()}>
              {busy ? "重置中…" : "确认重置"}
            </Button>
            <Button
              disabled={busy}
              onClick={() => {
                setConfirming(false);
                setConfirmationText("");
              }}
            >
              取消
            </Button>
          </>
        ) : (
          <>
            <Button variant="danger" disabled={busy || !any} onClick={() => setConfirming(true)}>
              重置所选数据
            </Button>
            <Button disabled={busy} onClick={toggleAll}>
              {allSelected ? "取消全选" : "全选（恢复出厂设置）"}
            </Button>
          </>
        )}
        {result && <span className="text-xs text-[var(--color-text-secondary)]">{result}</span>}
      </div>
    </div>
  );
}

/** §6.2 用途绑定行：该用途当前绑定哪个连接档案；独立下拉，修改不影响另一用途。
 *  连接档案保存在 ai_connections 表（API Key 走系统凭据）；超级搜索无绑定时自动选择在线服务。 */
function UsageBindingLine({
  usage,
  defaultProfile,
  notify,
  fail,
}: {
  usage: "super_search" | "tagging";
  defaultProfile?: { name: string; model: string };
  notify: (m: string) => void;
  fail: (m: string) => void;
}) {
  const [connections, setConnections] = useState<AiConnection[]>([]);
  const [binding, setBinding] = useState<string | null>(null);
  const [superSearchResolution, setSuperSearchResolution] = useState<SuperSearchServiceResolution | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let active = true;
    Promise.all([
      listAiConnections(),
      getAiUsageBindings(),
      usage === "super_search" ? getSuperSearchServiceResolution() : Promise.resolve(null),
    ])
      .then(([conns, binds, resolution]) => {
        if (!active) return;
        setConnections(conns);
        setBinding(binds[usage] ?? null);
        setSuperSearchResolution(resolution);
      })
      .catch(() => {
        /* 非 Tauri / 表未存在：静默降级为「跟随默认档案」 */
      })
      .finally(() => active && setLoaded(true));
    return () => {
      active = false;
    };
  }, [usage]);

  const onSelect = async (connectionId: string) => {
    const next = connectionId === "" ? null : connectionId;
    setSaving(true);
    try {
      await setAiUsageBinding(usage, next);
      setBinding(next);
      if (usage === "super_search") {
        setSuperSearchResolution(await getSuperSearchServiceResolution());
      }
      notify(next ? "已选择此服务" : usage === "super_search" ? "已恢复自动选择在线服务" : "已回退到跟随默认服务");
    } catch (e) {
      fail(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!loaded) return null;
  const selectedConnection = connections.find((connection) => connection.id === binding);
  const effectiveService = usage === "super_search"
    ? superSearchResolution?.ready
      ? `${superSearchResolution.name} · ${superSearchResolution.model}（${superSearchResolution.deployment === "cloud" ? "在线" : "本地"}）`
      : superSearchResolution?.message ?? "正在读取服务状态…"
    : binding
      ? selectedConnection
        ? `${selectedConnection.name} · ${selectedConnection.model}`
        : "已绑定服务不可用"
      : defaultProfile
        ? `${defaultProfile.name} · ${defaultProfile.model}`
        : "尚未配置默认服务";
  const serviceHint = usage === "super_search"
    ? binding
      ? `当前实际使用：${effectiveService}。绑定只影响此功能。`
      : `未手动绑定；${superSearchResolution?.ready ? `自动选择在线服务，当前使用：${effectiveService}。` : effectiveService}`
    : binding
      ? `当前实际使用：${effectiveService}。绑定只影响此功能。`
      : `当前实际使用默认服务：${effectiveService}。你可以为此功能单独选择服务。`;
  return (
    <Field
      label="此功能使用的服务"
      hint={serviceHint}
    >
      <select
        value={binding ?? ""}
        disabled={saving}
        onChange={(e) => void onSelect(e.target.value)}
        className="ui-control rounded-md px-2 py-1.5 text-sm outline-none"
      >
        <option value="">
          {usage === "super_search"
            ? "自动选择在线服务"
            : `跟随默认服务（${defaultProfile ? `${defaultProfile.name} · ${defaultProfile.model}` : "未配置"}）`}
        </option>
        {connections.map((c) => (
          <option key={c.id} value={c.id} disabled={!c.enabled}>
            {c.name} · {c.model}（{c.deployment === "cloud" ? "在线" : "本地"}{c.enabled ? "" : "，已停用"}{c.hasKey ? "，已配置密钥" : "，未配置密钥"}）
          </option>
        ))}
      </select>
    </Field>
  );
}
