/**
 * 导出弹窗（A4-01）验收测试。
 *
 * 覆盖卡片要求的关键行为：
 * 1. 只提供「复制导出」，没有移动选项；
 * 2. 目标目录经目录授权取得，不向后端传裸路径；
 * 3. 完成摘要给出目标位置与逐条失败项，只有已落盘且核验成功的文件才算成功；
 * 4. 失败项折叠：超过 5 条时给出剩余计数。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import ExportDialog, { exportErrorLabel, failedItems } from "@/components/dialogs/ExportDialog";
import type { ExportItem, ExportReport } from "@/types/export";

vi.mock("@/api/export", () => ({
  exportLocalFiles: vi.fn(),
  exportReport: vi.fn(),
  exportCsvManifest: vi.fn(),
  onExportProgress: vi.fn(async () => () => undefined),
}));

vi.mock("@/api/authorization", () => ({
  pickDirectoryGrant: vi.fn(),
}));

vi.mock("@/hooks/hooks", () => ({
  useTauriEvent: vi.fn(),
}));

import { exportLocalFiles, exportReport, exportCsvManifest } from "@/api/export";
import { pickDirectoryGrant } from "@/api/authorization";
import { useSelectionStore } from "@/stores/selectionStore";

const GRANT = {
  grantRef: "grant_0000000000000001",
  directoryRef: "directory_0000000000000001",
  displayName: "导出目录",
  displayPath: "F:/out",
  purpose: "export_destination" as const,
  recursive: true,
  remembered: false,
  available: true,
  access: { read: true, write: true, export: true },
};

function mkItem(over: Partial<ExportItem> = {}): ExportItem {
  return {
    id: 1,
    taskId: 7,
    ordinal: 0,
    assetId: 11,
    fileName: "a1.jpg",
    destPath: "F:/out/a1.jpg",
    state: "copied",
    errorCode: null,
    errorMessage: null,
    createdAt: 0,
    ...over,
  };
}

function mkReport(items: ExportItem[]): ExportReport {
  const counts = {
    copied: items.filter((i) => i.state === "copied").length,
    failed: items.filter((i) => i.state === "failed").length,
    skipped: items.filter((i) => i.state === "skipped").length,
  };
  return {
    task: {
      id: 7,
      target: "local",
      status: counts.failed > 0 ? "failed" : "done",
      total: items.length,
      done: counts.copied,
      destDir: "F:/out",
      shareUrl: null,
      error: null,
      createdAt: 0,
      warning: null,
    },
    counts,
    items,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  useSelectionStore.setState({ selected: new Set([11, 12]) });
});

describe("ExportDialog（A4-01 复制导出）", () => {
  it("只提供复制导出：没有移动选项，标题与按钮都写明复制", () => {
    render(<ExportDialog open onClose={vi.fn()} />);
    const dialog = screen.getByRole("dialog", { name: /复制导出/ });
    expect(dialog).toBeTruthy();
    expect(screen.queryByText("移动到目标目录")).toBeNull();
    expect(screen.getByRole("button", { name: "开始复制导出" })).toBeTruthy();
    // 明确告诉用户库内原件保留、重名不覆盖
    expect(screen.getByText(/库内原文件、原路径与标签保持不变/)).toBeTruthy();
  });

  it("目标目录未授权时不能开始复制", () => {
    render(<ExportDialog open onClose={vi.fn()} />);
    expect(screen.getByRole("button", { name: "开始复制导出" })).toHaveProperty("disabled", true);
  });

  it("浏览目录走目录授权，并把授权句柄而不是裸路径交给后端", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue(GRANT);
    vi.mocked(exportLocalFiles).mockResolvedValue(mkReport([mkItem()]).task);
    vi.mocked(exportReport).mockResolvedValue(mkReport([mkItem()]));

    render(<ExportDialog open onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "浏览…" }));

    await waitFor(() => expect(pickDirectoryGrant).toHaveBeenCalledWith("export_destination", false));
    await waitFor(() =>
      expect(screen.getByLabelText("目标目录")).toHaveProperty("value", "F:/out"),
    );

    fireEvent.click(screen.getByRole("button", { name: "开始复制导出" }));
    await waitFor(() => expect(exportLocalFiles).toHaveBeenCalled());
    const args = vi.mocked(exportLocalFiles).mock.calls[0];
    expect(args[1]).toBe(GRANT.directoryRef);
    expect(args[2]).toBe("copy");
  });

  it("完成摘要给出目标位置与逐条失败项", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue(GRANT);
    const ok = mkItem();
    const bad = mkItem({
      id: 2,
      ordinal: 1,
      assetId: 12,
      fileName: "b1.mov",
      destPath: null,
      state: "failed",
      errorCode: "DISK_FULL",
      errorMessage: "磁盘已满",
    });
    const report = mkReport([ok, bad]);
    vi.mocked(exportLocalFiles).mockResolvedValue(report.task);
    vi.mocked(exportReport).mockResolvedValue(report);

    render(<ExportDialog open onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "浏览…" }));
    await waitFor(() => expect(screen.getByLabelText("目标目录")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始复制导出" }));

    const summary = await screen.findByTestId("export-summary");
    expect(summary.textContent).toContain("已复制 1 个文件到 F:/out");
    expect(summary.textContent).toContain("1 个失败");
    expect(summary.textContent).toContain("b1.mov");
    expect(summary.textContent).toContain("目标磁盘空间不足");
  });

  it("取消导出：给出未开始项计数，不谎报成功", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue(GRANT);
    const cancelled: ExportReport = {
      task: {
        id: 8,
        target: "local",
        status: "cancelled",
        total: 2,
        done: 0,
        destDir: "F:/out",
        shareUrl: null,
        error: null,
        createdAt: 0,
        warning: null,
      },
      counts: { copied: 0, failed: 0, skipped: 2 },
      items: [
        mkItem({ state: "skipped", destPath: null, errorCode: "CANCELLED" }),
        mkItem({ id: 2, ordinal: 1, state: "skipped", destPath: null, errorCode: "CANCELLED" }),
      ],
    };
    vi.mocked(exportLocalFiles).mockResolvedValue(cancelled.task);
    vi.mocked(exportReport).mockResolvedValue(cancelled);

    render(<ExportDialog open onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "浏览…" }));
    await waitFor(() => expect(screen.getByLabelText("目标目录")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始复制导出" }));

    const summary = await screen.findByTestId("export-summary");
    expect(summary.textContent).toContain("没有文件被复制");
    expect(summary.textContent).toContain("2 个未开始（已取消）");
  });

  it("CSV 清单只在有文件落盘时生成，并复用同一目录授权", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue(GRANT);
    vi.mocked(exportLocalFiles).mockResolvedValue(mkReport([mkItem()]).task);
    vi.mocked(exportReport).mockResolvedValue(mkReport([mkItem()]));
    vi.mocked(exportCsvManifest).mockResolvedValue("F:/out/导出清单.csv");

    render(<ExportDialog open onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "浏览…" }));
    await waitFor(() => expect(screen.getByLabelText("目标目录")).toBeTruthy());
    fireEvent.click(screen.getByLabelText(/附 CSV 清单/));
    fireEvent.click(screen.getByRole("button", { name: "开始复制导出" }));

    await waitFor(() => expect(exportCsvManifest).toHaveBeenCalled());
    expect(vi.mocked(exportCsvManifest).mock.calls[0][1]).toBe(GRANT.directoryRef);
    const summary = await screen.findByTestId("export-summary");
    expect(summary.textContent).toContain("清单：F:/out/导出清单.csv");
  });

  it("后端拒绝（move 已停用 / 目标未授权）时原样显示错误", async () => {
    vi.mocked(pickDirectoryGrant).mockResolvedValue(GRANT);
    vi.mocked(exportLocalFiles).mockRejectedValue(
      new Error("导出已统一为复制：不会移动或删除库内原文件。"),
    );

    render(<ExportDialog open onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "浏览…" }));
    await waitFor(() => expect(screen.getByLabelText("目标目录")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始复制导出" }));

    await waitFor(() =>
      expect(screen.getByText(/导出已统一为复制/)).toBeTruthy(),
    );
  });
});

describe("导出摘要辅助函数", () => {
  it("失败原因按码映射，未知码原样回显并附带后端消息", () => {
    expect(exportErrorLabel(mkItem({ state: "failed", errorCode: "TARGET_EXISTS" }))).toBe(
      "目标目录同名文件过多",
    );
    expect(
      exportErrorLabel(mkItem({ state: "failed", errorCode: "IO_ERROR", errorMessage: "句柄无效" })),
    ).toBe("读写失败（句柄无效）");
    expect(
      exportErrorLabel(mkItem({ state: "failed", errorCode: "WEIRD_CODE" as never })),
    ).toBe("WEIRD_CODE");
  });

  it("目标子目录被拒绝时给出可读原因，不把后端错误码直接摆给用户", () => {
    // A4-01：后端逐项复验会产出 PATH_REJECTED（越界 / 子目录被换成链接或重解析点）。
    // 这条失败只影响单个文件，用户必须看得懂它为什么没落盘。
    expect(exportErrorLabel(mkItem({ state: "failed", errorCode: "PATH_REJECTED" }))).toBe(
      "目标子目录不安全，该文件未写入",
    );
    expect(
      exportErrorLabel(
        mkItem({
          state: "failed",
          errorCode: "PATH_REJECTED",
          errorMessage: "导出子目录是链接或重解析点：风光",
        }),
      ),
    ).toBe("目标子目录不安全，该文件未写入（导出子目录是链接或重解析点：风光）");
  });

  it("失败项最多列 5 条", () => {
    const items = Array.from({ length: 8 }, (_, i) =>
      mkItem({ id: i + 1, ordinal: i, state: "failed", errorCode: "IO_ERROR" }),
    );
    const report = mkReport(items);
    expect(failedItems(report)).toHaveLength(5);
    expect(report.counts.failed).toBe(8);
  });
});
