/** M-14b（QA_PLAYBOOK U-04）：收藏必须在【网格右键、工具栏/批量操作栏、查看器】四处行为一致。
 *  右键与查看器已由 AssetGrid.test.tsx / ViewerPage.test.tsx 覆盖，这里钉住批量操作栏这一处：
 *  同一取反语义、同一命令、失败回滚与如实提示。
 */
import { describe, expect, it, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { fireEvent } from "@testing-library/react";
import ContextActionBar from "@/components/library/ContextActionBar";
import { setFavorite } from "@/api/assets";
import { useLibraryStore } from "@/stores/libraryStore";
import { useSelectionStore } from "@/stores/selectionStore";
import type { Asset } from "@/types/asset";

vi.mock("@/api/assets", () => ({
  setFavorite: vi.fn().mockResolvedValue(2),
  setRating: vi.fn().mockResolvedValue(2),
}));

function mkAsset(id: number, favorite = 0): Asset {
  return {
    id,
    path: `d:/p/a${id}.jpg`,
    fileName: `a${id}.jpg`,
    ext: "jpg",
    mime: "image/jpeg",
    size: 1024,
    width: 100,
    height: 100,
    rating: 0,
    favorite,
    favoriteX: 0,
    createdAt: "2026-01-01T00:00:00Z",
    takenAt: null,
    modifiedAt: "2026-01-01T00:00:00Z",
  } as unknown as Asset;
}

function renderBar() {
  return render(
    <ContextActionBar onTag={vi.fn()} onExport={vi.fn()} onDelete={vi.fn()} />,
  );
}

describe("ContextActionBar 批量收藏（M-14b / U-04）", () => {
  beforeEach(() => {
    vi.mocked(setFavorite).mockReset();
    vi.mocked(setFavorite).mockResolvedValue(2);
    useLibraryStore.setState({ items: [mkAsset(1), mkAsset(2)] });
    useSelectionStore.setState({
      selected: new Set([1, 2]),
      anchorIndex: null,
      truncated: false,
      selectionTotal: 0,
    });
  });

  it("批量操作栏提供与右键一致的收藏入口，选中全部未收藏的素材后整体收藏", async () => {
    renderBar();
    fireEvent.click(screen.getByRole("button", { name: "收藏 / 取消收藏" }));
    await waitFor(() => expect(setFavorite).toHaveBeenCalledWith([1, 2], true));
    // 本地星标同步，不让界面停在没写进库的状态
    await waitFor(() =>
      expect(useLibraryStore.getState().items.map((a) => a.favorite)).toEqual([1, 1]),
    );
  });

  it("选中项全部已收藏时再次点击是取消收藏，取反语义与右键菜单一致", async () => {
    useLibraryStore.setState({ items: [mkAsset(1, 1), mkAsset(2, 1)] });
    renderBar();
    fireEvent.click(screen.getByRole("button", { name: "收藏 / 取消收藏" }));
    await waitFor(() => expect(setFavorite).toHaveBeenCalledWith([1, 2], false));
    await waitFor(() =>
      expect(useLibraryStore.getState().items.map((a) => a.favorite)).toEqual([0, 0]),
    );
  });

  it("收藏写入失败时回滚本地星标并用 role=alert 如实说明，不假装保存成功", async () => {
    vi.mocked(setFavorite).mockRejectedValue(new Error("磁盘只读"));
    renderBar();
    fireEvent.click(screen.getByRole("button", { name: "收藏 / 取消收藏" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("收藏没有保存");
    expect(alert.textContent).toContain("磁盘只读");
    await waitFor(() =>
      expect(useLibraryStore.getState().items.map((a) => a.favorite)).toEqual([0, 0]),
    );
  });
});
