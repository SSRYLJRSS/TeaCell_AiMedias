/** useAppearance 测试（§8.4）：草稿预览优先 → 已落库值 → 编译期默认。 */
import { beforeEach, describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useAppearance } from "@/hooks/useAppearance";
import { useSettingsStore } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";

function mkSettings(): Settings {
  return {
    ai: {
      profiles: [],
      activeProfile: "",
      videoTagging: false,
      videoTaggingMode: "cover",
      videoFrameCount: 3,
      batchLimit: 500,
      ollamaSourceId: "auto",
      systemPromptTagging: "",
      systemPromptSearch: "",
      confidenceMinSuggest: 0.3,
    },
    theme: "system",
    logLevel: "info",
    thumbnailCacheMb: 2048,
    tagCategories: [],
    libraryRoot: "",
    trashRetentionDays: 30,
    customDownloadSources: [],
    modelDownloadProxy: "",
    appearance: {
      grid: { libraryCellStep: 3, importCellStep: 1, cellAspect: "1:1", cellFit: "cover", matchDominantColor: false },
      hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
      colorStrip: { enabled: true, showInLibraryGrid: false, showInViewer: true, showInImportGrid: false, height: "normal", mode: "ratio", count: 6 },
      kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
    },
    navigation: {
      shortcuts: [
        { id: "import", visible: true },
        { id: "library", visible: true },
        { id: "ai", visible: true },
        { id: "treasure", visible: true },
      ],
      settingsPinned: false,
    },
  };
}

beforeEach(() => {
  useSettingsStore.setState({ settings: null, previewAppearance: null });
});

describe("useAppearance", () => {
  it("settings 与 preview 都为 null → 编译期默认（enabled true）", () => {
    const { result } = renderHook(() => useAppearance());
    expect(result.current.hoverPreview.enabled).toBe(true);
    expect(result.current.grid.libraryCellStep).toBe(3);
  });

  it("无预览草稿时读已落库值", () => {
    const s = mkSettings();
    s.appearance.grid.libraryCellStep = 5;
    useSettingsStore.setState({ settings: s });
    const { result } = renderHook(() => useAppearance());
    expect(result.current.grid.libraryCellStep).toBe(5);
    expect(result.current.grid.cellAspect).toBe("1:1");
  });

  it("草稿预览优先于已落库值（SettingsPage 拖动滑块立即跟随）", () => {
    const s = mkSettings();
    s.appearance.grid.libraryCellStep = 5;
    useSettingsStore.setState({ settings: s });
    act(() => {
      useSettingsStore.setState({ previewAppearance: { ...s.appearance, grid: { ...s.appearance.grid, libraryCellStep: 6 } } });
    });
    const { result } = renderHook(() => useAppearance());
    expect(result.current.grid.libraryCellStep).toBe(6);
  });
});
