import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import LegacyAssetPanel from "./LegacyAssetPanel";
import {
  cancelLegacyResolution,
  confirmLegacyResolution,
  confirmLegacyResolutionsBatch,
  listLegacyAssets,
  listUnlinkedLegacyAssets,
  prepareLegacyCopyBack,
  prepareLegacyCopyBackBatch,
  prepareLegacyUnlink,
  prepareLegacyUnlinkBatch,
  reopenUnlinkedLegacyAsset,
} from "@/api/legacyAssets";
import type {
  LegacyAsset,
  LegacyAssetPage,
  LegacyBatchReport,
  LegacyResolutionPreview,
} from "@/api/legacyAssets";

vi.mock("@/api/legacyAssets", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/legacyAssets")>();
  return {
    ...actual,
    cancelLegacyResolution: vi.fn().mockResolvedValue(undefined),
    confirmLegacyResolution: vi.fn(),
    confirmLegacyResolutionsBatch: vi.fn(),
    listLegacyAssets: vi.fn(),
    listUnlinkedLegacyAssets: vi.fn(),
    prepareLegacyCopyBack: vi.fn(),
    prepareLegacyCopyBackBatch: vi.fn(),
    prepareLegacyUnlink: vi.fn(),
    prepareLegacyUnlinkBatch: vi.fn(),
    reopenUnlinkedLegacyAsset: vi.fn().mockResolvedValue(undefined),
  };
});

function asset(overrides: Partial<LegacyAsset> = {}): LegacyAsset {
  return {
    assetId: 41,
    filePath: "C:/outside/old.jpg",
    fileName: "old.jpg",
    fileSize: 1024,
    mimeType: "image/jpeg",
    modifiedAt: 10,
    hash: null,
    status: "external_pending",
    reason: "outside_managed_root",
    favorite: 1,
    rating: 4,
    activeResolutionId: null,
    activeResolutionAction: null,
    activeResolutionState: null,
    resolutionErrorCode: null,
    matchingAssets: [],
    ...overrides,
  };
}

function page(items: LegacyAsset[], canResolve = true): LegacyAssetPage {
  return { libraryHealth: canResolve ? "ready" : "offline", canResolve, rootPath: "C:/Library", items, nextAfterAssetId: null };
}

function preview(overrides: Partial<LegacyResolutionPreview> = {}): LegacyResolutionPreview {
  return {
    resolutionId: "legacy_preview_0001",
    asset: asset(),
    action: "copy_back",
    matchingAssets: [{
      assetId: 12,
      fileName: "same.jpg",
      favorite: 0,
      rating: 3,
      contentSha256: "a".repeat(64),
      tags: ["人物", "海边"],
    }],
    keepSeparateRequired: true,
    expiresAt: 999,
    ...overrides,
  };
}

function batchReport(overrides: Partial<LegacyBatchReport> = {}): LegacyBatchReport {
  return {
    action: "unlink",
    prepared: 2,
    failed: 0,
    skipped: 0,
    cancelled: false,
    entries: [
      { assetId: 41, state: "prepared", resolutionId: "legacy_preview_0041", errorCode: null, message: null },
      { assetId: 42, state: "prepared", resolutionId: "legacy_preview_0042", errorCode: null, message: null },
    ],
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listLegacyAssets).mockResolvedValue(page([asset()]));
  vi.mocked(listUnlinkedLegacyAssets).mockResolvedValue([]);
  vi.mocked(prepareLegacyCopyBack).mockResolvedValue(preview());
  vi.mocked(prepareLegacyUnlink).mockResolvedValue(preview({ action: "unlink", matchingAssets: [], keepSeparateRequired: false }));
  vi.mocked(confirmLegacyResolution).mockResolvedValue({
    resolutionId: "legacy_preview_0001",
    assetId: 41,
    action: "copy_back",
    state: "completed",
    targetPath: "C:/Library/old.jpg",
    matchingAssets: [],
    errorCode: null,
    expiresAt: 999,
  });
});

describe("LegacyAssetPanel", () => {
  it("shows exact-hash candidates and asks before keeping a separate copy", async () => {
    render(<LegacyAssetPanel />);
    await screen.findByText(/ID 41/);
    fireEvent.click(screen.getByRole("button", { name: "复制回仓" }));

    expect(await screen.findByText(/完整 SHA-256 相同/)).toBeInTheDocument();
    expect(screen.getByText("标签：人物、海边")).toBeInTheDocument();
    expect(screen.getByText(/仍保留为独立记录/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "确认复制回仓" }));

    await waitFor(() => expect(confirmLegacyResolution).toHaveBeenCalledWith("legacy_preview_0001"));
    expect(await screen.findByText(/素材 41 已复制回仓/)).toBeInTheDocument();
  });

  it("disables per-item actions when the root is offline and never offers them for unknown rows", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([
      asset({ status: "unknown", reason: "ownership_unverified" }),
    ], false));
    render(<LegacyAssetPanel />);
    const hint = await screen.findByTestId("legacy-offline-hint");
    expect(within(hint).getByText(/总仓暂不可用/)).toBeInTheDocument();
    expect(screen.getByText("归属未确认前，不能复制回仓或解除关联。")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "复制回仓" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "解除关联" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "批量解除关联" })).not.toBeInTheDocument();
    expect(prepareLegacyUnlink).not.toHaveBeenCalled();
    expect(prepareLegacyUnlinkBatch).not.toHaveBeenCalled();
    expect(cancelLegacyResolution).not.toHaveBeenCalled();
  });

  it("offers a way out instead of a dead end when the root is offline", async () => {
    const onRequestRelocation = vi.fn();
    vi.mocked(listLegacyAssets).mockResolvedValue(page([asset()], false));
    render(<LegacyAssetPanel onRequestRelocation={onRequestRelocation} />);

    const hint = await screen.findByTestId("legacy-offline-hint");
    expect(within(hint).getByText(/逐项处置已暂停/)).toBeInTheDocument();
    // 说清「记录和原文件都不会被改」，并给出可点击的重新定位出口。
    expect(within(hint).getByText(/记录与原文件都不会被改动|不会自动解除关联/)).toBeInTheDocument();
    fireEvent.click(within(hint).getByRole("button", { name: "打开「总仓位置」向导…" }));
    expect(onRequestRelocation).toHaveBeenCalledTimes(1);
  });

  it("reopens an unlinked record without deleting it", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([]));
    vi.mocked(listUnlinkedLegacyAssets).mockResolvedValue([asset()]);
    render(<LegacyAssetPanel />);
    await screen.findByText("已解除关联");
    fireEvent.click(screen.getByRole("button", { name: "重新打开待处理" }));
    await waitFor(() => expect(reopenUnlinkedLegacyAsset).toHaveBeenCalledWith(41));
  });

  it("only lets rows that can actually be handled be selected", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([
      asset({ assetId: 41, fileName: "a.jpg" }),
      // 有进行中的处置：不能进批量（否则会给同一条记录准备第二份收据）
      asset({ assetId: 42, fileName: "b.jpg", activeResolutionId: "legacy_preview_0042", activeResolutionAction: "unlink", activeResolutionState: "prepared" }),
      // 归属未确认：批量按钮不能把它带上
      asset({ assetId: 43, fileName: "c.jpg", status: "unknown", reason: "ownership_unverified" }),
    ]));
    render(<LegacyAssetPanel />);
    await screen.findByText(/本页可处理 1 条/);
    expect(screen.queryByRole("checkbox", { name: "选择 b.jpg" })).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "选择 c.jpg" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("checkbox", { name: "选择 a.jpg" }));
    fireEvent.click(screen.getByRole("button", { name: "批量解除关联" }));
    await waitFor(() => expect(prepareLegacyUnlinkBatch).toHaveBeenCalledWith([41]));
  });

  it("reports per-entry results and keeps skipped and failed apart from done", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([
      asset({ assetId: 41, fileName: "a.jpg" }),
      asset({ assetId: 42, fileName: "b.jpg" }),
      asset({ assetId: 43, fileName: "c.jpg" }),
    ]));
    vi.mocked(prepareLegacyCopyBackBatch).mockResolvedValue(batchReport({
      action: "copy_back",
      prepared: 1,
      failed: 1,
      skipped: 1,
      entries: [
        { assetId: 41, state: "prepared", resolutionId: "legacy_preview_0041", errorCode: null, message: "总仓中已有 1 条完整 SHA-256 相同的素材；本次仍保留为独立记录，不合并标签。" },
        { assetId: 42, state: "failed", resolutionId: null, errorCode: "SOURCE_CHANGED", message: "原文件已变化，不能复制回仓" },
        { assetId: 43, state: "skipped", resolutionId: null, errorCode: null, message: "没有为这条记录选择原文件" },
      ],
    }));
    render(<LegacyAssetPanel />);
    await screen.findByText(/本页可处理 3 条/);
    fireEvent.click(screen.getByRole("button", { name: "全选本页" }));
    fireEvent.click(screen.getByRole("button", { name: "批量复制回仓…" }));

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/已准备 1 条复制回仓，1 条未通过检查，1 条未轮到处置/)).toBeInTheDocument();
    expect(within(dialog).getByText(/总仓中已有 1 条完整 SHA-256 相同/)).toBeInTheDocument();
    expect(within(dialog).getByText("错误码：SOURCE_CHANGED")).toBeInTheDocument();
    expect(within(dialog).getByText(/1 条需要先处理：b.jpg/)).toBeInTheDocument();
    // 只有 prepared 的那条会进确认；跳过和失败都不算成功。
    fireEvent.click(within(dialog).getByRole("button", { name: "确认处置这 1 条" }));
    await waitFor(() =>
      expect(confirmLegacyResolutionsBatch).toHaveBeenCalledWith(["legacy_preview_0041"]),
    );
  });

  it("does nothing and says so when the file picker is cancelled", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([asset({ assetId: 41, fileName: "a.jpg" })]));
    vi.mocked(prepareLegacyCopyBackBatch).mockResolvedValue(
      batchReport({ action: "copy_back", prepared: 0, cancelled: true, entries: [] }),
    );
    render(<LegacyAssetPanel />);
    await screen.findByText(/本页可处理 1 条/);
    fireEvent.click(screen.getByRole("checkbox", { name: "选择 a.jpg" }));
    fireEvent.click(screen.getByRole("button", { name: "批量复制回仓…" }));

    expect(await screen.findByText("已取消，没有处置任何记录。")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(confirmLegacyResolutionsBatch).not.toHaveBeenCalled();
  });

  it("未点确认前只做准备检查，确认后执行一次且回仓记录保留身份与元数据", async () => {
    // R39：用户确认后才处置；「复制回仓」按钮本身只是准备阶段（打开预览）。
    render(<LegacyAssetPanel />);
    await screen.findByText(/ID 41/);

    fireEvent.click(screen.getByRole("button", { name: "复制回仓" }));
    const dialog = await screen.findByRole("dialog");

    // 准备阶段：只生成预览，没有调用真正的回仓命令。
    expect(prepareLegacyCopyBack).toHaveBeenCalledWith(41);
    expect(confirmLegacyResolution).not.toHaveBeenCalled();
    expect(within(dialog).getByText(/old\.jpg · ID 41/)).toBeInTheDocument();
    // 预览承诺保留的是同一条记录的身份与整理信息。
    expect(
      within(dialog).getByText(/现有素材 ID、标签、收藏、评分和关系会保留/),
    ).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "确认复制回仓" }));

    await waitFor(() => expect(confirmLegacyResolution).toHaveBeenCalledTimes(1));
    expect(confirmLegacyResolution).toHaveBeenCalledWith("legacy_preview_0001");
    expect(
      await screen.findByText(/素材 41 已复制回仓，原记录与整理信息已保留/),
    ).toBeInTheDocument();

    // 回仓后仍按同一条记录展示：同一个素材 ID、文件名与整理信息都在。
    expect(screen.getByText("old.jpg")).toBeInTheDocument();
    const identity = screen.getByText(/^ID 41 ·/);
    expect(identity.textContent).toContain("评分 4");
    expect(identity.textContent).toContain("已收藏");
  });

  it("批量回仓同样要过确认门：没勾选不准备，勾选后只准备，确认后才逐条执行", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([asset({ assetId: 41, fileName: "a.jpg" })]));
    vi.mocked(prepareLegacyCopyBackBatch).mockResolvedValue(
      batchReport({
        action: "copy_back",
        prepared: 1,
        entries: [
          {
            assetId: 41,
            state: "prepared",
            resolutionId: "legacy_preview_0041",
            errorCode: null,
            message: null,
          },
        ],
      }),
    );
    vi.mocked(confirmLegacyResolutionsBatch).mockResolvedValue(
      batchReport({
        action: "copy_back",
        prepared: 1,
        entries: [
          {
            assetId: 41,
            state: "completed",
            resolutionId: "legacy_preview_0041",
            errorCode: null,
            message: null,
          },
        ],
      }),
    );
    render(<LegacyAssetPanel />);
    await screen.findByText(/本页可处理 1 条/);

    // 没勾选任何行：批量按钮不可点，也不会替用户准备处置。
    const prepareButton = screen.getByRole("button", { name: "批量复制回仓…" }) as HTMLButtonElement;
    expect(prepareButton.disabled).toBe(true);
    fireEvent.click(prepareButton);
    expect(prepareLegacyCopyBackBatch).not.toHaveBeenCalled();
    expect(confirmLegacyResolutionsBatch).not.toHaveBeenCalled();

    // 勾选后点批量：仍然只是准备（生成预览），不执行处置。
    fireEvent.click(screen.getByRole("checkbox", { name: "选择 a.jpg" }));
    fireEvent.click(screen.getByRole("button", { name: "批量复制回仓…" }));
    await waitFor(() => expect(prepareLegacyCopyBackBatch).toHaveBeenCalledWith([41]));
    expect(confirmLegacyResolutionsBatch).not.toHaveBeenCalled();

    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "确认处置这 1 条" }));
    await waitFor(() =>
      expect(confirmLegacyResolutionsBatch).toHaveBeenCalledTimes(1),
    );
    expect(confirmLegacyResolutionsBatch).toHaveBeenCalledWith(["legacy_preview_0041"]);
    // 结论落在页面通知区（同一句也会出现在检查结果弹窗里，所以按 role=status 找）。
    expect(
      screen.getAllByRole("status").some((node) => node.textContent?.includes("已准备 1 条复制回仓。")),
    ).toBe(true);
  });
});
