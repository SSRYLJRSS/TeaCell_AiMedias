/** 永久清空弹窗的行为测试（A4-04）。
 *
 *  与 A4-03 恢复测试的分工：恢复测的是「文件缺失/被替换不能假装成功」，
 *  这里测的是「永久删除必须独立审批、四档结果不能压成一个数字」。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor, within, fireEvent } from "@testing-library/react";
import DeleteDialog from "@/components/dialogs/DeleteDialog";
import { useSelectionStore } from "@/stores/selectionStore";
import { useLibraryStore } from "@/stores/libraryStore";
import type { EmptyReport } from "@/api/trash";

const deleteAssets = vi.fn();
const emptyTrash = vi.fn();
const removeLocal = vi.fn();

vi.mock("@/api/assets", () => ({
  deleteAssets: (...args: unknown[]) => deleteAssets(...args),
}));

vi.mock("@/api/trash", async () => {
  const actual = await vi.importActual<typeof import("@/api/trash")>("@/api/trash");
  return {
    ...actual,
    emptyTrash: (...args: unknown[]) => emptyTrash(...args),
  };
});

function report(overrides: Partial<EmptyReport> = {}): EmptyReport {
  return {
    operationRef: "operation_abc",
    removed: 0,
    failed: 0,
    missing: 0,
    unknown: 0,
    skipped: 0,
    cancelled: false,
    items: [],
    ...overrides,
  };
}

describe("DeleteDialog 永久清空", () => {
  beforeEach(() => {
    deleteAssets.mockReset();
    emptyTrash.mockReset();
    removeLocal.mockReset();
    useLibraryStore.setState({ removeLocal });
    useSelectionStore.setState({ selected: new Set([1, 2]) });
  });

  it("二次确认后才调用 emptyTrash，且从不调用 delete_assets", async () => {
    emptyTrash.mockResolvedValue(
      report({
        removed: 2,
        items: [
          { itemId: "delete_1", assetId: 1, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          { itemId: "delete_2", assetId: 2, fileName: "b.jpg", state: "succeeded", errorCode: null, detail: null },
        ],
      }),
    );
    render(<DeleteDialog open purgeOnly onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    expect(emptyTrash).not.toHaveBeenCalled();
    expect(screen.getByText(/二次确认/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "永久清空这 2 项" }));
    await waitFor(() => expect(emptyTrash).toHaveBeenCalledWith([1, 2]));
    // 永久删除只有一个后端入口：不能退回 delete_assets(delete_file)。
    expect(deleteAssets).not.toHaveBeenCalled();
    await waitFor(() => expect(removeLocal).toHaveBeenCalledWith([1, 2]));
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([]);
  });

  it("部分失败时失败项保持选中，摘要区分缺失与结果不明", async () => {
    emptyTrash.mockResolvedValue(
      report({
        removed: 1,
        missing: 1,
        unknown: 1,
        items: [
          { itemId: "delete_1", assetId: 1, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          {
            itemId: "delete_2",
            assetId: 2,
            fileName: "b.jpg",
            state: "failed",
            errorCode: "FILE_MISSING",
            detail: "「b.jpg」的文件已不在原位置，记录保留在回收站",
          },
        ],
      }),
    );
    render(<DeleteDialog open purgeOnly onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    fireEvent.click(screen.getByRole("button", { name: "永久清空这 2 项" }));

    const summary = await screen.findByTestId("purge-summary");
    expect(summary.textContent).toContain("已永久删除 1 项");
    expect(summary.textContent).toContain("1 项文件本就不在原位置");
    expect(summary.textContent).toContain("1 项结果不明");
    expect(within(summary).getByText(/b\.jpg/)).toBeTruthy();
    // 没删掉的项必须留在选择里，用户还要看见它
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([2]);
    expect(removeLocal).toHaveBeenCalledWith([1]);
  });

  it("后端整体拒绝时原样显示错误且不清空选择", async () => {
    emptyTrash.mockRejectedValue(new Error("其他长任务正在执行或清空操作尚未排到队首，请稍后重试"));
    render(<DeleteDialog open purgeOnly onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    fireEvent.click(screen.getByRole("button", { name: "永久清空这 2 项" }));

    expect(await screen.findByText(/其他长任务正在执行/)).toBeTruthy();
    expect(Array.from(useSelectionStore.getState().selected)).toEqual([1, 2]);
    expect(removeLocal).not.toHaveBeenCalled();
  });

  it("文件缺失与归属未核验的原因文案不同", async () => {
    const { emptyErrorLabel } = await import("@/api/trash");
    expect(
      emptyErrorLabel({
        itemId: "delete_1",
        assetId: 1,
        fileName: "a.jpg",
        state: "failed",
        errorCode: "FILE_MISSING",
        detail: null,
      }),
    ).toContain("文件本来就不在原位置");
    expect(
      emptyErrorLabel({
        itemId: "delete_2",
        assetId: 2,
        fileName: "b.jpg",
        state: "outcome_unknown",
        errorCode: "OWNERSHIP_UNVERIFIED",
        detail: null,
      }),
    ).toContain("定位证据已失效");
    // 未知码原样回显，便于对着日志排查
    expect(
      emptyErrorLabel({
        itemId: "delete_3",
        assetId: 3,
        fileName: "c.jpg",
        state: "failed",
        errorCode: "SOMETHING_NEW",
        detail: "细节",
      }),
    ).toBe("SOMETHING_NEW：细节");
  });

  it("非回收站模式仍走原来的软删/delete_assets 路径", async () => {
    deleteAssets.mockResolvedValue({ deleted: 2, failedFiles: [] });
    render(<DeleteDialog open onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(deleteAssets).toHaveBeenCalledWith([1, 2], "remove_from_library"));
    expect(emptyTrash).not.toHaveBeenCalled();
  });

  it("对「永久清空」连点多次只执行一次，不会产生第二次删除效果（M-14 误点）", async () => {
    // M-14 原文要求「重复点击不产生第二次效果」。危险操作的入口守卫只有按钮
    // disabled，很容易在后续重构里被删掉，所以用真机同款连点行为把它钉住。
    let finish!: (value: EmptyReport) => void;
    emptyTrash.mockImplementation(
      () =>
        new Promise<EmptyReport>((resolve) => {
          finish = resolve;
        }),
    );
    render(<DeleteDialog open purgeOnly onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    const confirm = screen.getByRole("button", { name: "永久清空这 2 项" });
    // 连续三次点击：第一次真正发起，后两次在 await 之前、按钮变 disabled 之后。
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    fireEvent.click(confirm);

    finish(
      report({
        removed: 2,
        items: [
          { itemId: "delete_1", assetId: 1, fileName: "a.jpg", state: "succeeded", errorCode: null, detail: null },
          { itemId: "delete_2", assetId: 2, fileName: "b.jpg", state: "succeeded", errorCode: null, detail: null },
        ],
      }),
    );

    await waitFor(() => expect(emptyTrash).toHaveBeenCalledTimes(1));
    expect(emptyTrash).toHaveBeenCalledWith([1, 2]);
  });
});
