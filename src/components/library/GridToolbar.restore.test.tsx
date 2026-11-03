/** A4-03 恢复路径测试：逐项结果、失败项必须留在回收站，不能谎报成功 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import GridToolbar from "@/components/library/GridToolbar";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";
import type { RestoreReport } from "@/api/trash";

const mocks = vi.hoisted(() => ({
  restoreAssets: vi.fn(),
  listTrash: vi.fn(),
  trashRetention: vi.fn(),
}));

vi.mock("@/api/settings", () => ({
  getSettings: vi.fn().mockResolvedValue(undefined),
  saveSettings: vi.fn().mockResolvedValue(undefined),
}));

// 只替换网络调用，保留真实的状态映射与纯函数，测试才检查得到真实文案。
vi.mock("@/api/trash", async () => {
  const actual = await vi.importActual<typeof import("@/api/trash")>("@/api/trash");
  return {
    ...actual,
    listTrash: mocks.listTrash,
    trashRetention: mocks.trashRetention,
    restoreAssets: mocks.restoreAssets,
  };
});

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

function report(overrides: Partial<RestoreReport> = {}): RestoreReport {
  return {
    operationRef: "operation_restore_1",
    restored: 0,
    failed: 0,
    unknown: 0,
    skipped: 0,
    cancelled: false,
    items: [],
    ...overrides,
  };
}

const noop = () => {};

function renderToolbar() {
  return render(
    <GridToolbar onTag={noop} onExport={noop} onMove={noop} onDelete={noop} onPurge={noop} />,
  );
}

beforeEach(() => {
  mocks.restoreAssets.mockReset();
  mocks.listTrash.mockReset().mockResolvedValue([]);
  mocks.trashRetention.mockReset().mockResolvedValue([]);
  useSettingsStore.setState({ settings: mkSettings(), previewAppearance: null });
  useLibraryStore.setState((s) => ({
    total: 3,
    items: [],
    viewItems: [],
    filter: { ...s.filter, sortBy: "created_at", sortDir: "desc", trashOnly: true },
  }));
  useSelectionStore.setState({ selected: new Set([7, 8]) });
});

describe("GridToolbar 回收站恢复", () => {
  it("全部成功时清空选择并显示已恢复数量", async () => {
    mocks.restoreAssets.mockResolvedValue(
      report({
        restored: 2,
        items: [
          { itemId: "restore_7", assetId: 7, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          { itemId: "restore_8", assetId: 8, fileName: "b.jpg", state: "succeeded", errorCode: null, detail: null },
        ],
      }),
    );
    renderToolbar();
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await waitFor(() => expect(mocks.restoreAssets).toHaveBeenCalledWith([7, 8]));
    await waitFor(() => expect(useSelectionStore.getState().selected.size).toBe(0));
    // 全部成功不渲染失败摘要
    expect(screen.queryByTestId("restore-summary")).toBeNull();
  });

  it("部分失败时失败项仍留在回收站并保持选中，摘要点出文件名与原因", async () => {
    mocks.restoreAssets.mockResolvedValue(
      report({
        restored: 1,
        failed: 1,
        items: [
          { itemId: "restore_7", assetId: 7, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          {
            itemId: "restore_8",
            assetId: 8,
            fileName: "b.jpg",
            state: "failed",
            errorCode: "FILE_MISSING",
            detail: "「b.jpg」的文件已不在原位置",
          },
        ],
      }),
    );
    renderToolbar();
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    const summary = await screen.findByTestId("restore-summary");
    expect(summary.textContent).toContain("已恢复 1 项");
    expect(summary.textContent).toContain("1 项失败（仍留在回收站）");
    expect(summary.textContent).toContain("b.jpg");
    expect(summary.textContent).toContain("文件已不在原位置");
    // 只有失败项保持选中：可以立刻重试或手动移出
    await waitFor(() => expect(Array.from(useSelectionStore.getState().selected)).toEqual([8]));
  });

  it("结果不明时明确要求核对，既不算成功也不清空选中态", async () => {
    mocks.restoreAssets.mockResolvedValue(
      report({
        restored: 1,
        unknown: 1,
        items: [
          { itemId: "restore_7", assetId: 7, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          {
            itemId: "restore_8",
            assetId: 8,
            fileName: "b.jpg",
            state: "outcome_unknown",
            errorCode: "INTERRUPTED",
            detail: "恢复收尾失败，无法确认该记录是否已回到库中",
          },
        ],
      }),
    );
    renderToolbar();
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    const summary = await screen.findByTestId("restore-summary");
    expect(summary.textContent).toContain("已恢复 1 项");
    expect(summary.textContent).toContain("1 项结果不明（可能已恢复，需核对）");
    expect(summary.textContent).toContain("需核对：b.jpg");
    // 不确定项不能被当作「可直接重试的失败」，所以既不报失败也不清选择。
    expect(summary.textContent).not.toContain("项失败");
    await waitFor(() => expect(Array.from(useSelectionStore.getState().selected)).toEqual([8]));
  });

  it("后端整体拒绝时原样显示错误且不清空选择", async () => {
    mocks.restoreAssets.mockRejectedValue(new Error("其他长任务正在执行，请稍后重试"));
    renderToolbar();
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("恢复失败：其他长任务正在执行，请稍后重试");
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([7, 8]);
  });

  it("回收站空选时按保留天数提示自动清理", () => {
    useSelectionStore.setState({ selected: new Set() });
    renderToolbar();
    expect(screen.getByText("回收站 · 保留 30 天后自动清理（可在设置中调整）")).toBeInTheDocument();
  });
});

describe("restoreErrorLabel / failedRestores", () => {
  it("已知码翻成中文，未知码原样回显", async () => {
    const { restoreErrorLabel } = await import("@/api/trash");
    expect(
      restoreErrorLabel({
        itemId: "restore_1",
        assetId: 1,
        fileName: "a.jpg",
        state: "failed",
        errorCode: "FILE_CHANGED",
        detail: null,
      }),
    ).toBe("原位置已被其他文件替换");
    expect(
      restoreErrorLabel({
        itemId: "restore_1",
        assetId: 1,
        fileName: "a.jpg",
        state: "failed",
        errorCode: "SOMETHING_NEW",
        detail: "细节",
      }),
    ).toBe("SOMETHING_NEW：细节");
  });

  it("失败项最多列 5 条", async () => {
    const { failedRestores } = await import("@/api/trash");
    const items = Array.from({ length: 8 }, (_, index) => ({
      itemId: `restore_${index}`,
      assetId: index,
      fileName: `${index}.jpg`,
      state: "failed",
      errorCode: "FILE_MISSING",
      detail: null,
    }));
    expect(failedRestores(report({ items }))).toHaveLength(5);
    expect(failedRestores(report({ items }), 2)).toHaveLength(2);
  });
});
