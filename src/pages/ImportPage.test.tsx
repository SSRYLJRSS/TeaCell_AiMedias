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
  newImportTaskId: vi.fn().mockReturnValue("task-test"),
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

const originalLoadSettings = useSettingsStore.getState().load;

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({
    load: originalLoadSettings,
    loaded: true,
    loading: false,
    loadError: null,
    settings: { libraryRoot: "d:/库" } as never,
  });
  useLibraryStore.setState({ items: [], total: 0, loading: false });
  useTaskStore.setState({ tasks: [] });
});

describe("ImportPage", () => {
  it("设置读取中不误报缺少总库或提供跳转按钮", async () => {
    useSettingsStore.setState({ loaded: false, loading: true, settings: null, load: vi.fn().mockResolvedValue(undefined) });
    render(<ImportPage />);
    expect(screen.getByText("正在读取设置，请稍候。")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "前往设置" })).toBeNull();
    await act(async () => {});
  });

  it("设置读取失败优先显示错误和重试，不冒充加载中", async () => {
    const load = vi.fn().mockResolvedValue(undefined);
    useSettingsStore.setState({ loaded: false, settings: null, loadError: "无法读取数据库", load });
    render(<ImportPage />);
    expect(screen.getByText("设置读取失败：无法读取数据库")).toBeInTheDocument();
    expect(screen.queryByText("正在读取设置，请稍候。")).toBeNull();
    expect(screen.queryByRole("button", { name: "前往设置" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(load).toHaveBeenCalledTimes(2);
    await act(async () => {});
  });
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
      warnings: [],
    });
    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));
    await waitFor(() => {
      expect(screen.getByText("未发现可入库的图片/视频文件")).toBeTruthy();
    });
  });

  it("选择器确认后先绘制左侧检查状态，再等待扫描结果", async () => {
    let resolveInspection!: (value: Awaited<ReturnType<typeof inspectImport>>) => void;
    vi.mocked(pickFiles).mockResolvedValue(["d:/slow/first.jpg"]);
    vi.mocked(inspectImport).mockReturnValue(new Promise((resolve) => { resolveInspection = resolve; }));
    render(<ImportPage />);

    fireEvent.click(screen.getByText("选择文件…"));
    await waitFor(() => expect(inspectImport).toHaveBeenCalledTimes(1));
    expect(screen.getByText("正在检查文件与缩略图")).toBeInTheDocument();
    expect(screen.getByLabelText("入库进度")).toHaveAttribute("data-state", "running");

    await act(async () => resolveInspection({ items: [], images: 0, videos: 0, totalSize: 0, warnings: [] }));
    expect(await screen.findByText("未发现可入库的图片/视频文件")).toBeInTheDocument();
  });

  it("未设置总库时先提示去设置，且不打开选择器或启动扫描", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/x.jpg"]);
    useSettingsStore.setState({ settings: { libraryRoot: "" } as never });
    render(<ImportPage />);
    fireEvent.click(screen.getByRole("button", { name: "前往设置" }));
    expect(screen.getByText(/请先设置素材总库位置/)).toBeInTheDocument();
    fireEvent.click(screen.getByText("选择文件…"));
    expect(screen.getAllByText(/请先设置素材总库位置/)).toHaveLength(1);
    expect(pickFiles).not.toHaveBeenCalled();
    expect(inspectImport).not.toHaveBeenCalled();
  });

  it("无法生成缩略图的文件先拦截，确认剔除后保留可导入项", async () => {
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
      warnings: [],
    });

    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));

    expect(await screen.findByRole("dialog", { name: "有文件无法生成缩略图" })).toBeInTheDocument();
    expect(screen.getByText("d:/raw/unsupported.x3f")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "剔除并保留可导入项" }));

    expect(screen.queryByRole("dialog", { name: "有文件无法生成缩略图" })).toBeNull();
    expect(screen.getByText("开始入库（1）")).toBeInTheDocument();
  });

  it("可解析但可能受限的 RAW 入队时显示非阻断说明", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/raw/limited.cr2"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [
        {
          path: "d:/raw/limited.cr2",
          kind: "image",
          size: 1024,
          previewStatus: "limited",
          previewMessage: "可解析缩略图但后续功能可能受限",
        },
      ],
      images: 1,
      videos: 0,
      totalSize: 1024,
      warnings: [],
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
      warnings: [],
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

  it("入库前显示扫描跳过项，即使没有可入库文件", async () => {
    vi.mocked(pickFiles).mockResolvedValue(["d:/raw"]);
    vi.mocked(inspectImport).mockResolvedValue({
      items: [],
      images: 0,
      videos: 0,
      totalSize: 0,
      warnings: ["d:/raw/bad.jpg: 文件名不是有效 UTF-8，首版暂不支持无损入库"],
    });

    render(<ImportPage />);
    fireEvent.click(screen.getByText("选择文件…"));

    expect(await screen.findByText("扫描时跳过或遇到 1 项，请在导入前确认。")).toBeInTheDocument();
    fireEvent.click(screen.getByText("查看扫描提示"));
    expect(screen.getByText(/文件名不是有效 UTF-8/)).toBeInTheDocument();
    expect(screen.queryByText("开始入库（0）")).toBeNull();
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

  it("扫描和缩略图检查复用左侧唯一进度区", () => {
    upsertImport({
      taskId: "scan-1",
      phase: "checking",
      phaseCurrent: 118,
      phaseTotal: 133,
      file: "IMG_001.JPG",
      imported: 0,
      duplicates: 0,
      failed: 0,
      message: "正在检查文件与缩略图",
    });
    render(<ImportPage />);
    expect(screen.getAllByLabelText("入库进度")).toHaveLength(1);
    const panel = screen.getByLabelText("入库进度");
    expect(within(panel).getByText("正在检查文件与缩略图")).toBeInTheDocument();
    expect(within(panel).getByText("已检查 118/133 项")).toBeInTheDocument();
    expect(within(panel).queryByText(/成功|重复|失败 \d/)).toBeNull();
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
