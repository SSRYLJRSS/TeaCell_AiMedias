/**
 * 「移动到分库」弹窗（A4-02）验收测试。
 *
 * 覆盖卡片要求的关键行为：
 * 1. 目标只能是后端给出的总仓内分库引用，前端不构造路径；
 * 2. 完成后刷新目录分面与当前结果视图；
 * 3. 部分失败时明确报出未移动项，并保留这些素材的选中态；
 * 4. 全部成功才清空选择。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import MoveDialog, { failedMoves, moveErrorLabel } from "@/components/dialogs/MoveDialog";
import type { MoveItem, MoveReport } from "@/api/move";

vi.mock("@/api/move", () => ({
  listMoveTargets: vi.fn(),
  moveAssets: vi.fn(),
}));

import { listMoveTargets, moveAssets } from "@/api/move";
import { useSelectionStore } from "@/stores/selectionStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { useMetadataStore } from "@/stores/metadataStore";

const TARGETS = [
  { collectionRef: "collection_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", relativePath: "", label: "总仓根目录" },
  { collectionRef: "collection_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", relativePath: "2024/旅行", label: "2024/旅行" },
];

function mkItem(over: Partial<MoveItem> = {}): MoveItem {
  return {
    itemId: "move_0000000000000011",
    assetId: 11,
    fileName: "a1.jpg",
    state: "committed",
    errorCode: null,
    sourcePath: "F:/lib/a1.jpg",
    targetPath: "F:/lib/2024/旅行/a1.jpg",
    ...over,
  };
}

function mkReport(items: MoveItem[], collectionPath = "2024/旅行"): MoveReport {
  return {
    operationRef: "operation_0000000000000001",
    collectionPath,
    moved: items.filter((i) => i.state === "committed").length,
    failed: items.filter((i) => i.state === "failed").length,
    unknown: items.filter((i) => i.state === "moved").length,
    skipped: items.filter((i) => i.state === "intent").length,
    cancelled: false,
    items,
  };
}

let libraryRefresh: ReturnType<typeof vi.fn>;
let metadataRefresh: ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  libraryRefresh = vi.fn().mockResolvedValue(undefined);
  metadataRefresh = vi.fn().mockResolvedValue(undefined);
  useLibraryStore.setState({ refresh: libraryRefresh as never });
  useMetadataStore.setState({ refresh: metadataRefresh as never });
  useSelectionStore.setState({ selected: new Set([11, 12]), selectionTotal: 2, truncated: false });
  vi.mocked(listMoveTargets).mockResolvedValue(TARGETS);
});

describe("MoveDialog（A4-02 库内分库移动）", () => {
  it("目标只能从后端分库清单里选，不能手输路径", async () => {
    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(listMoveTargets).toHaveBeenCalledTimes(1));
    // 有下拉选择，没有可编辑的路径输入
    expect(screen.getByLabelText("目标分库")).toBeTruthy();
    expect(screen.queryByLabelText("目标目录")).toBeNull();
    // 未选目标时不能开始
    expect(screen.getByRole("button", { name: "开始移动" })).toHaveProperty("disabled", true);
  });

  it("选好分库后按引用提交，并刷新目录分面与结果视图", async () => {
    vi.mocked(moveAssets).mockResolvedValue(mkReport([mkItem(), mkItem({ assetId: 12 })]));
    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(listMoveTargets).toHaveBeenCalled());

    fireEvent.change(screen.getByLabelText("目标分库"), {
      target: { value: TARGETS[1].collectionRef },
    });
    await screen.findByTestId("move-destination");
    fireEvent.click(screen.getByRole("button", { name: "开始移动" }));

    await waitFor(() => expect(moveAssets).toHaveBeenCalledTimes(1));
    expect(vi.mocked(moveAssets).mock.calls[0][1]).toBe(TARGETS[1].collectionRef);
    expect(Array.from(vi.mocked(moveAssets).mock.calls[0][0] as number[]).sort()).toEqual([11, 12]);

    const summary = await screen.findByTestId("move-summary");
    expect(summary.textContent).toContain("已移动 2 个文件到 2024/旅行");
    // 文件夹归属变了 → 两个分面都必须重算
    await waitFor(() => expect(libraryRefresh).toHaveBeenCalled());
    expect(metadataRefresh).toHaveBeenCalled();
    // 全部成功 → 选择清空
    expect(useSelectionStore.getState().selected.size).toBe(0);
  });

  it("部分失败：报出未移动项并保留这些素材的选中态", async () => {
    const bad = mkItem({
      itemId: "move_0000000000000012",
      assetId: 12,
      fileName: "b1.mov",
      state: "failed",
      errorCode: "TARGET_EXISTS",
      targetPath: "",
    });
    vi.mocked(moveAssets).mockResolvedValue(mkReport([mkItem(), bad]));

    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(listMoveTargets).toHaveBeenCalled());
    fireEvent.change(screen.getByLabelText("目标分库"), { target: { value: TARGETS[1].collectionRef } });
    fireEvent.click(screen.getByRole("button", { name: "开始移动" }));

    const summary = await screen.findByTestId("move-summary");
    expect(summary.textContent).toContain("已移动 1 个文件到 2024/旅行");
    expect(summary.textContent).toContain("1 个失败（仍留在原位置）");
    expect(summary.textContent).toContain("b1.mov");
    expect(summary.textContent).toContain("目标位置已存在同名文件");
    // 未移动项的选中态保留，用户能直接重试
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([12]);
  });

  it("效果不明：提示核对、不算成功，也不清空这些素材的选中态", async () => {
    const unsure = mkItem({
      itemId: "move_0000000000000012",
      assetId: 12,
      fileName: "b1.mov",
      state: "moved",
      errorCode: "OUTCOME_UNKNOWN" as never,
    });
    vi.mocked(moveAssets).mockResolvedValue(mkReport([mkItem(), unsure]));

    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(listMoveTargets).toHaveBeenCalled());
    fireEvent.change(screen.getByLabelText("目标分库"), { target: { value: TARGETS[1].collectionRef } });
    fireEvent.click(screen.getByRole("button", { name: "开始移动" }));

    const summary = await screen.findByTestId("move-summary");
    expect(summary.textContent).toContain("1 项结果不明");
    expect(summary.textContent).toContain("需人工核对");
    // 「结果不明」的项必须留在选中态等用户核对（已成功提交的那项可以退出选择）
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([12]);
  });

  it("后端拒绝时原样显示错误，不清空选择", async () => {
    vi.mocked(moveAssets).mockRejectedValue(
      new Error("总仓身份已变化，请重新核验后再移动"),
    );
    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(listMoveTargets).toHaveBeenCalled());
    fireEvent.change(screen.getByLabelText("目标分库"), { target: { value: TARGETS[1].collectionRef } });
    fireEvent.click(screen.getByRole("button", { name: "开始移动" }));

    await waitFor(() =>
      expect(screen.getByText(/总仓身份已变化/)).toBeTruthy(),
    );
    expect(useSelectionStore.getState().selected.size).toBe(2);
  });

  it("读取分库失败时给出错误且不能开始", async () => {
    vi.mocked(listMoveTargets).mockRejectedValue(new Error("总仓尚未通过身份核验"));
    render(<MoveDialog open onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/总仓尚未通过身份核验/)).toBeTruthy());
    expect(screen.getByRole("button", { name: "开始移动" })).toHaveProperty("disabled", true);
  });
});

describe("移动摘要辅助函数", () => {
  it("失败原因按码映射，未知码原样回显", () => {
    expect(moveErrorLabel(mkItem({ state: "failed", errorCode: "SOURCE_MISSING" }))).toBe(
      "源文件已不在原位置",
    );
    expect(moveErrorLabel(mkItem({ state: "failed", errorCode: "WEIRD" as never }))).toBe("WEIRD");
  });

  it("失败项最多列 5 条", () => {
    const items = Array.from({ length: 8 }, (_, i) =>
      mkItem({ itemId: `move_${i}`, assetId: i + 1, state: "failed", errorCode: "IO_ERROR" }),
    );
    const report = mkReport(items);
    expect(failedMoves(report)).toHaveLength(5);
    expect(report.failed).toBe(8);
  });
});
