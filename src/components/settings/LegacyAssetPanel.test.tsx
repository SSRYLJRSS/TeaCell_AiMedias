import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import LegacyAssetPanel from "./LegacyAssetPanel";
import {
  cancelLegacyResolution,
  confirmLegacyResolution,
  listLegacyAssets,
  listUnlinkedLegacyAssets,
  prepareLegacyCopyBack,
  prepareLegacyUnlink,
  reopenUnlinkedLegacyAsset,
} from "@/api/legacyAssets";
import type { LegacyAsset, LegacyAssetPage, LegacyResolutionPreview } from "@/api/legacyAssets";

vi.mock("@/api/legacyAssets", () => ({
  cancelLegacyResolution: vi.fn().mockResolvedValue(undefined),
  confirmLegacyResolution: vi.fn(),
  listLegacyAssets: vi.fn(),
  listUnlinkedLegacyAssets: vi.fn(),
  prepareLegacyCopyBack: vi.fn(),
  prepareLegacyUnlink: vi.fn(),
  reopenUnlinkedLegacyAsset: vi.fn().mockResolvedValue(undefined),
}));

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
    await screen.findByText(/总仓暂不可用/);
    expect(screen.getByText("归属未确认前，不能复制回仓或解除关联。")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "复制回仓" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "解除关联" })).not.toBeInTheDocument();
    expect(prepareLegacyUnlink).not.toHaveBeenCalled();
    expect(cancelLegacyResolution).not.toHaveBeenCalled();
  });

  it("reopens an unlinked record without deleting it", async () => {
    vi.mocked(listLegacyAssets).mockResolvedValue(page([]));
    vi.mocked(listUnlinkedLegacyAssets).mockResolvedValue([asset()]);
    render(<LegacyAssetPanel />);
    await screen.findByText("已解除关联");
    fireEvent.click(screen.getByRole("button", { name: "重新打开待处理" }));
    await waitFor(() => expect(reopenUnlinkedLegacyAsset).toHaveBeenCalledWith(41));
  });
});
