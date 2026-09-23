/** W7-2：ImportPage 页面级测试（W7-2 要求补页面级测试）。
 *  渲染 + 空库引导 + 选文件取消不崩溃 + 空清单报错提示。 */
import { act, render, screen, waitFor, within } from "@testing-library/react";
import { fireEvent } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ImportPage from "@/pages/ImportPage";
import { useSettingsStore } from "@/stores/settingsStore";
import { useLibraryStore } from "@/stores/libraryStore";
import { upsertImport, useTaskStore } from "@/stores/taskStore";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    listen: () => Promise.resolve(() => {}),
    onDragDropEvent: () => Promise.resolve(() => {}),
  }),
}));
vi.mock("@/api/import", () => ({
  inspectImport: vi.fn(),
  importFiles: vi.fn(),
  cancelImport: vi.fn(),
  onImportProgress: vi.fn().mockResolvedValue(() => {}),
  renderNamePreview: vi.fn(),
  openFileExternal: vi.fn(),
  markImportCancelling: vi.fn(),
}));
vi.mock("@/api/assets", () => ({
  listMetadataFacets: vi.fn().mockResolvedValue([]),
}));

import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import { importFiles, inspectImport } from "@/api/import";

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({
    loaded: true,
    settings: { libraryRoot: "d:/库" } as never,
  });
  useLibraryStore.setState({ items: [], total: 0, loading: false });
  useTaskStore.setState({ tasks: [] });
});

describe("ImportPage", () => {
  it("渲染空库引导（拖拽区 + 选择按钮）", () => {
    render(<ImportPage />);
    expect(screen.getByText("把图片 / 视频拖到这里")).toBeTruthy();
    expect(screen.getByText("选择文件…")).toBeTruthy();
    expect(screen.getByText("选择文件夹…")).toBeTruthy();
  });

  it("选文件取消（返回 null）不崩溃、不产生错误", async () => {
    vi.mocked(pickFiles).mockResolvedValue(null);
    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));
    expect(inspectImport).not.toHaveBeenCalled();
    expect(screen.queryByText(/未发现可入库/)).toBeNull();
  });

  it("选到空清单时给出明确错误提示", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/x.exe"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [],
      images: 0,
      videos: 0,
      totalSize: 0,
    });
    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));
    await waitFor(() => {
      expect(screen.getByText("未发现可入库的图片/视频文件")).toBeTruthy();
    });
  });

  it("发现无法生成缩略图的文件时先拦截，并可剔除后保留可导入项", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/raw/mixed"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [
        { path: "d:/raw/ok.jpg", kind: "image", size: 1024, previewStatus: "ready" },
        {
          path: "d:/raw/unsupported.x3f",
          kind: "image",
          size: 2048,
          previewStatus: "unsupported",
          previewMessage: "无法解析缩略图",
        },
      ],
      images: 2,
      videos: 0,
      totalSize: 3072,
    });
    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));

    expect(await screen.findByRole("dialog", { name: "有文件无法生成缩略图" })).toBeInTheDocument();
    expect(screen.getByText("d:/raw/unsupported.x3f")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "剔除并保留可导入项" }));

    expect(screen.queryByRole("dialog", { name: "有文件无法生成缩略图" })).toBeNull();
    expect(screen.getByText("开始入库（1）")).toBeInTheDocument();
  });

  it("可解析但可能受限的特殊格式允许入队并显示提示", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/raw/limited.cr2"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [{
        path: "d:/raw/limited.cr2",
        kind: "image",
        size: 1024,
        previewStatus: "limited",
        previewMessage: "可解析缩略图但后续功能可能受限",
      }],
      images: 1,
      videos: 0,
      totalSize: 1024,
    });
    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));

    expect(await screen.findByText(/高清预览、元数据或 AI 功能可能受限/)).toBeInTheDocument();
    expect(screen.getByText("开始入库（1）")).toBeInTheDocument();
  });

  it("入库命令级失败后解除 busy，允许重试且不再显示取消状态", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/raw/X3F"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [{ path: "d:/raw/X3F", kind: "image", size: 1024, previewStatus: "ready" }],
      images: 1,
      videos: 0,
      totalSize: 1024,
    });
    vi.mocked(importFiles).mockRejectedValue(new Error("入库线程异常: task 124 panicked"));

    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));
    const start = await screen.findByText("开始入库（1）");
    fireEvent.click(start);

    await waitFor(() => {
      expect(screen.getByText(/入库线程异常: task 124 panicked/)).toBeTruthy();
    });
    expect(screen.getByText("开始入库（1）")).toBeTruthy();
    expect(screen.queryByText("取消入库")).toBeNull();
  });

  it("计算/入库阶段只展示真实阶段进度，不显示尚未产生的结果计数", () => {
    upsertImport({
      taskId: "t1",
      phase: "hashing",
      phaseCurrent: 12,
      phaseTotal: 100,
      file: "IMG_001.RW2",
      imported: 3,
      duplicates: 2,
      failed: 2,
    });
    render(<ImportPage />);

    const panel = screen.getByLabelText("入库进度");
    expect(within(panel).getByText("正在计算指纹")).toBeInTheDocument();
    expect(within(panel).getByText("8%")).toBeInTheDocument();
    expect(within(panel).getByText("IMG_001.RW2")).toBeInTheDocument();
    expect(within(panel).getByText("当前阶段 12/100")).toBeInTheDocument();
    expect(within(panel).queryByText(/成功/)).toBeNull();
    expect(within(panel).queryByText(/重复/)).toBeNull();
    expect(within(panel).queryByText(/失败/)).toBeNull();
    expect(within(panel).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "8");
  });

  it("进度区始终占位，完成后保留最终进度", () => {
    render(<ImportPage />);

    let panel = screen.getByLabelText("入库进度");
    expect(panel).toHaveAttribute("data-state", "idle");
    expect(within(panel).getByText("等待入库")).toBeInTheDocument();
    expect(within(panel).queryByText(/成功|重复|失败|当前阶段|未开始/)).toBeNull();

    act(() => {
      upsertImport({
        taskId: "t-done",
        phase: "done",
        phaseCurrent: 10,
        phaseTotal: 10,
        imported: 8,
        duplicates: 1,
        failed: 1,
      });
    });

    panel = screen.getByLabelText("入库进度");
    expect(panel).toHaveAttribute("data-state", "done");
    expect(within(panel).getByText("已完成")).toBeInTheDocument();
    expect(within(panel).getByText("100%")).toBeInTheDocument();
    expect(within(panel).getByText("成功 8")).toBeInTheDocument();
    expect(within(panel).getByText("重复 1")).toBeInTheDocument();
    expect(within(panel).getByText("失败 1")).toBeInTheDocument();
    expect(within(panel).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
    expect(screen.queryByText("成功 8 · 重复 1 · 失败 1")).toBeNull();
  });

  it("扫描阶段显示已发现数量，预览阶段开始显示有效结果计数", () => {
    const { rerender } = render(<ImportPage />);

    act(() => {
      upsertImport({
        taskId: "t-scan",
        phase: "scanning",
        phaseCurrent: 17,
        phaseTotal: null,
        imported: 0,
        duplicates: 0,
        failed: 0,
      });
    });
    let panel = screen.getByLabelText("入库进度");
    expect(within(panel).getByText("已发现 17 项")).toBeInTheDocument();
    expect(within(panel).queryByText(/成功|重复|失败/)).toBeNull();

    act(() => {
      upsertImport({
        taskId: "t-scan",
        phase: "previewing",
        phaseCurrent: 4,
        phaseTotal: 10,
        imported: 7,
        duplicates: 2,
        failed: 1,
      });
    });
    rerender(<ImportPage />);
    panel = screen.getByLabelText("入库进度");
    expect(within(panel).getByText("当前阶段 4/10")).toBeInTheDocument();
    expect(within(panel).getByText("成功 7")).toBeInTheDocument();
    expect(within(panel).getByText("重复 2")).toBeInTheDocument();
    expect(within(panel).getByText("失败 1")).toBeInTheDocument();
  });
});
