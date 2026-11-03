/** FB2-01 顶栏三态大小按钮测试（§9.6） */
import { describe, expect, it, beforeEach, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import GridToolbar from "@/components/library/GridToolbar";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";

vi.mock("@/api/settings", () => ({
  getSettings: vi.fn().mockResolvedValue(undefined),
  saveSettings: vi.fn().mockResolvedValue(undefined),
}));

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

const noop = () => {};

beforeEach(() => {
  useSettingsStore.setState({ settings: mkSettings(), previewAppearance: null });
  useLibraryStore.setState((s) => ({ total: 12, filter: { ...s.filter, sortBy: "created_at", sortDir: "desc", trashOnly: false } }));
  useSelectionStore.setState({ selected: new Set() });
});

describe("GridToolbar FB2-01 三态大小按钮", () => {
  it("渲染小/中/大三个按钮，aria-label 正确，默认「中」为激活态", () => {
    render(
      <GridToolbar onTag={noop} onExport={noop} onMove={noop} onDelete={noop} onPurge={noop} />,
    );
    const small = screen.getByRole("button", { name: "小" });
    const mid = screen.getByRole("button", { name: "中" });
    const large = screen.getByRole("button", { name: "大" });
    expect(small).toBeInTheDocument();
    expect(mid).toBeInTheDocument();
    expect(large).toBeInTheDocument();
    // 默认档位 3 → 「中」激活
    expect(mid.getAttribute("aria-pressed")).toBe("true");
    expect(small.getAttribute("aria-pressed")).toBe("false");
  });

  it("点击「大」改变档位到 5，按钮状态随之切换", () => {
    render(
      <GridToolbar onTag={noop} onExport={noop} onMove={noop} onDelete={noop} onPurge={noop} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "大" }));
    const large = screen.getByRole("button", { name: "大" });
    expect(large.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "中" }).getAttribute("aria-pressed")).toBe("false");
    // 即时预览已写进 previewAppearance（网格跟随）
    expect(useSettingsStore.getState().previewAppearance?.grid.libraryCellStep).toBe(5);
  });

  it("选中操作位于标签左侧，标签弹性滚动，总数和缩略图按钮固定在最右侧", () => {
    useSelectionStore.setState({ selected: new Set([1, 2]) });
    const { container } = render(
      <GridToolbar onTag={noop} onExport={noop} onMove={noop} onDelete={noop} onPurge={noop} onDedup={noop} />,
    );
    const filters = screen.getByTestId("toolbar-filters");
    const actions = screen.getByTestId("toolbar-actions");
    const selection = screen.getByText("已选中 2 项").parentElement!;
    expect(filters).toHaveClass("min-w-0", "flex-1", "overflow-hidden");
    expect(actions).toHaveClass("shrink-0");
    expect(selection.compareDocumentPosition(filters) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(filters.compareDocumentPosition(actions) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    const total = screen.getByText("12 项");
    const small = screen.getByRole("button", { name: "小" });
    expect(total.compareDocumentPosition(small) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(container.querySelector('[aria-label="标签类型"]')).toBeNull();
  });
});
