/** 应用设置状态：启动加载，保存即落库 */
import { create } from "zustand";
import { getSettings, saveSettings } from "@/api/settings";
import { normalizeSettings } from "@/utils/normalizeSettings";
import { markStartup } from "@/utils/startupMarks";
import type { Appearance, Settings } from "@/types/settings";
import { logger } from "@/utils/logger";

/** R-24：主题写入 <html> 的 data-theme（system 时 media query 接管，light/dark 显式生效） */
export function applyTheme(theme: Settings["theme"]) {
  document.documentElement.dataset.theme = theme;
}

interface SettingsState {
  settings: Settings | null;
  loaded: boolean;
  /** 正在加载/重试：指导书 阶段 1 §6.3 状态机（idle → loading → ready/error）。 */
  loading: boolean;
  loadError: string | null; // B28：新增——加载失败时暴露错误，前端可据此禁用保存防覆盖
  saving: boolean;
  /** 外观设置的即时预览通道。设置页自动保存；网格/Viewer 操作也通过下方防抖通道落库。 */
  previewAppearance: Appearance | null;
  setPreviewAppearance: (a: Appearance | null) => void;
  /** 网格档位即时预览 + 800ms 防抖持久化（滚轮连续滚动不高频落库）。
   *  patch 合并进当前生效 appearance 的深拷贝后：① 立即写 previewAppearance 让网格跟随；
   *  ② 防抖 save；卸载时 flush 剩余待写。 */
  commitAppearanceDebounced: (patch: Partial<Appearance> | ((a: Appearance) => Appearance)) => void;
  load: () => Promise<void>;
  save: (s: Settings) => Promise<void>;
}

/** FB2-03 默认外观（与 Rust 端 default_* 对齐）；settings 未加载时用作兜底。 */
export const DEFAULT_APPEARANCE: Appearance = {
  grid: { libraryCellStep: 3, importCellStep: 1, cellAspect: "4:3", cellFit: "cover", matchDominantColor: false },
  hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
  colorStrip: { enabled: true, showInLibraryGrid: false, showInViewer: true, showInImportGrid: false, height: "normal", mode: "ratio", count: 6 },
  kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
};

/**
 * 读取当前生效外观：草稿预览优先，其次已落库值，最后编译期默认。
 * 这样 SettingsPage 拖动滑块时网格立刻跟随，而其他页面读到的是已保存值或默认。
 */
export function currentAppearance(s: Settings | null, preview: Appearance | null): Appearance {
  if (preview) return preview;
  return s?.appearance ?? DEFAULT_APPEARANCE;
}

/**
 * single-flight（指导书 §5.3）：并发调用只产生一次底层请求。
 * App / SettingsPage 等多处「未 loaded 就 load()」的调用共享同一 Promise；
 * 失败后（loadError 非空）允许重试，仍走单飞去重。
 */
let loadPromise: Promise<void> | null = null;

/** 档位/比例即时预览的防抖持久化定时器。 */
let appearanceTimer: ReturnType<typeof setTimeout> | null = null;
/** 等待落库的最新外观值；执行时与最新完整设置合并，避免覆盖其他刚保存的配置。 */
let pendingAppearance: Appearance | null = null;
/** 所有设置写入串行化，避免较慢的旧请求晚于新请求完成并覆盖新值。 */
let settingsSaveQueue: Promise<void> = Promise.resolve();

export const useSettingsStore = create<SettingsState>((set, get) => ({
  settings: null,
  loaded: false,
  loading: false,
  loadError: null,
  saving: false,
  previewAppearance: null,
  setPreviewAppearance: (a) => set({ previewAppearance: a }),

  commitAppearanceDebounced: (patch) => {
    const cur = get().previewAppearance ?? get().settings?.appearance ?? currentAppearance(get().settings, null);
    const next = typeof patch === "function" ? patch(cur) : { ...cur, ...patch };
    // 1) 立即写入 preview → 网格/Viewer 跟随（不动已落库 settings，避免半提交态）
    set({ previewAppearance: next });
    // 2) 仅暂存最新外观值；定时写入时再与最新设置合并。
    pendingAppearance = next;
    if (appearanceTimer) clearTimeout(appearanceTimer);
    appearanceTimer = setTimeout(() => {
      appearanceTimer = null;
      const appearance = pendingAppearance;
      pendingAppearance = null;
      const latest = get().settings;
      if (!appearance || !latest) return;
      void get().save({ ...latest, appearance }).catch((e) => {
        logger.warn(`[settingsStore] 外观自动保存失败：${e instanceof Error ? e.message : String(e)}`);
      });
    }, 800);
  },

  load: () => {
    const s = get();
    // 已成功加载：直接完成；加载失败（loadError）允许重试
    if (s.loaded && !s.loadError) return Promise.resolve();
    if (loadPromise) return loadPromise;
    set({ loading: true, loadError: null });
    loadPromise = (async () => {
      try {
        const raw = await getSettings();
        // A-2：后端返回先做运行时归一化（缺字段兜底），再写入 store，避免 SettingsPage 因缺字段白屏
        const settings = normalizeSettings(raw);
        set({ settings, loaded: true, loading: false, loadError: null });
        applyTheme(settings.theme); // R-24：启动即应用已保存主题
        markStartup("settings_ready"); // §4.1：设置 ready 打点
      } catch (e) {
        // B28：暴露错误状态而非静默吞错（后端异常时用户看到默认设置页，保存后可能覆盖真实配置）
        set({ loaded: true, loading: false, loadError: e instanceof Error ? e.message : String(e) });
      } finally {
        loadPromise = null;
      }
    })();
    return loadPromise;
  },

  save: async (s) => {
    const operation = settingsSaveQueue.catch(() => undefined).then(async () => {
      set({ saving: true });
      try {
        // 有尚未提交的外观预览时，把它合入设置快照，防止一次其他设置保存将预览回滚。
        const preview = get().previewAppearance;
        const settingsToSave = preview ? { ...s, appearance: preview } : s;
        await saveSettings(settingsToSave);
        // 保存后回读 DB 对账，确保设置以持久层归一化结果为准。
        let reconciled = settingsToSave;
        try {
          const raw = await getSettings();
          reconciled = normalizeSettings(raw);
        } catch {
          // 回读失败不阻断保存成功（回读是增强对账，非保存前置条件）
          tracingWarn("保存设置回读对账失败，沿用本地值");
        }
        const latestPreview = get().previewAppearance;
        set({
          settings: reconciled,
          saving: false,
          previewAppearance:
            latestPreview && JSON.stringify(latestPreview) !== JSON.stringify(settingsToSave.appearance)
              ? latestPreview
              : null,
        });
        applyTheme(reconciled.theme);
      } catch (e) {
        set({ saving: false });
        throw e;
      }
    });
    settingsSaveQueue = operation.then(() => undefined, () => undefined);
    return operation;
  },
}));

/** 回读失败仅告警（不把后端异常升级为保存失败） */
function tracingWarn(msg: string) {
  logger.warn(`[settingsStore] ${msg}`);
}
