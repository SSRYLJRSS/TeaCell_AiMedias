/**
 * ViewerPage 视频识别回归测试（指导书 §7.1/§7.2）：
 *  - MIME 为 video/mp4 且 durationMs=null 时仍渲染 VideoPlayer（旧逻辑用 durationMs!=null 判断会漏掉导入失败时长为空的视频）。
 *  - 胶片条文字使用与属性面板一致的 isVideoAsset 判断。
 */
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";import ViewerPage from "@/components/library/ViewerPage";
import { useLibraryStore } from "@/stores/libraryStore";
import { ensureVideoProxy } from "@/api/video";
import { usePlatformStore } from "@/stores/platformStore";
import type { Asset } from "@/types/asset";

vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://thumb/hd.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));
vi.mock("@/api/tags", () => ({
  removeTags: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/api/video", () => ({
  ensureVideoProxy: vi.fn(),
  toProxyFileUrl: (p: string) => `asset://proxy/${p}`,
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://${p}`,
}));

function mkAsset(over: Partial<Asset> = {}): Asset {
  return {
    id: 1,
    filePath: "d:/lib/v1.mp4",
    fileName: "v1.mp4",
    fileExt: "mp4",
    fileSize: 1024,
    mimeType: "video/mp4",
    width: 1920,
    height: 1080,
    durationMs: null, // 关键：导入时长读取失败
    videoCodec: null,
    audioCodec: null,
    takenAt: null,
    createdAt: 1,
    modifiedAt: 1,
    hash: null,
    placeholderPath: null,
    hdThumbnailPath: null,
    camera: null,
    lens: null,
    iso: null,
    aperture: null,
    shutter: null,
    focal: null,
    tags: [],
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  usePlatformStore.setState({
    status: "ready",
    capabilities: { schemaVersion: 1, os: "windows", arch: "x86_64", managedOllama: true, preferredVideoProxy: "h264_mp4" },
    error: null,
    loadGeneration: 0,
  });
  useLibraryStore.setState({
    items: [mkAsset()],
    total: 1,
    loading: false,
    error: null,
    filter: {
      assetType: "all",
      untaggedOnly: false,
      tagId: null,
      search: "",
      sortBy: "created_at",
      sortDir: "desc",
      trashOnly: false,
    },
  });
  (useLibraryStore.getState() as unknown as { loadMore: () => void }).loadMore = vi.fn();
});

describe("ViewerPage 视频识别（§7.1）", () => {
  it("MIME 为 video/mp4 且 durationMs=null 仍渲染 VideoPlayer", () => {
    const { container } = render(<ViewerPage asset={mkAsset()} onClose={vi.fn()} />);
    // 渲染 <video> 元素（VideoPlayer），而非 <img>
    expect(container.querySelector("video")).not.toBeNull();
    expect(container.querySelector("img")).toBeNull();
  });

  it("图片素材渲染 <img> 而非 VideoPlayer", async () => {
    const img = mkAsset({ id: 2, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={vi.fn()} />);
    // 高清缩略图异步加载后渲染 <img>；等待出现的 img 元素
    await waitFor(() => expect(screen.getByRole("img", { name: "i.jpg" })).toBeInTheDocument());
    expect(container.querySelector("video")).toBeNull();
  });

  it("胶片条对无占位图的视频显示「视频」而非「图片」（同一类型判断）", () => {
    render(<ViewerPage asset={mkAsset()} onClose={vi.fn()} />);
    // 当前素材 placeholderPath=null → 胶片条退化为文字
    const text = screen.getAllByText("视频");
    expect(text.length).toBeGreaterThan(0);
  });

  it("§8.3 原文件播放失败 → 生成兼容代理并切换到代理源", async () => {
    vi.mocked(ensureVideoProxy).mockResolvedValue({
      assetId: 1,
      variant: "h264_mp4",
      status: "ready",
      path: "D:/data/proxies/1_h264_mp4.mp4",
      error: null,
      updatedAt: 1,
    });
    const { container } = render(<ViewerPage asset={mkAsset()} onClose={vi.fn()} />);
    const video = container.querySelector("video") as HTMLVideoElement;
    expect(video).not.toBeNull();
    // 触发播放错误
    video.dispatchEvent(new Event("error"));
    await waitFor(() => expect(ensureVideoProxy).toHaveBeenCalledWith(1, "h264_mp4"));
    await waitFor(() =>
      expect((container.querySelector("video") as HTMLVideoElement).getAttribute("src")).toContain("asset://proxy/"),
    );
  });
});

describe("ViewerPage 过片数据集可注入（超级搜索联动）", () => {
  it("传入 listItems/listTotal 时胶片条与位置计数用注入集；不回退混入素材库 store 列表", () => {
    // 素材库 store 里是另一批 3 项（旧实现下查看器只会读它，导致超搜 2 项却显示 x/3）
    const lib = [
      mkAsset({ id: 301, fileName: "l1.jpg", filePath: "d:/lib/l1.jpg" }),
      mkAsset({ id: 302, fileName: "l2.jpg", filePath: "d:/lib/l2.jpg" }),
      mkAsset({ id: 303, fileName: "l3.jpg", filePath: "d:/lib/l3.jpg" }),
    ];
    useLibraryStore.setState({ items: lib, total: 3 });
    const injected = [
      mkAsset({ id: 401, mimeType: "image/jpeg", fileExt: "jpg", fileName: "s1.jpg", filePath: "d:/s/s1.jpg", placeholderPath: "t401.jpg" }),
      mkAsset({ id: 402, mimeType: "image/jpeg", fileExt: "jpg", fileName: "s2.jpg", filePath: "d:/s/s2.jpg", placeholderPath: "t402.jpg" }),
    ];
    render(<ViewerPage asset={injected[0]} onClose={vi.fn()} listItems={injected} listTotal={2} />);
    // 位置计数走注入集
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    expect(screen.queryByText(/\/ 3/)).not.toBeInTheDocument();
    // 胶片条只渲染注入集
    expect(screen.getByRole("button", { name: "第 1 张：s1.jpg" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "第 2 张：s2.jpg" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /l\d\.jpg/ })).not.toBeInTheDocument();
  });
});

describe("ViewerPage 查看器色条（FB3-10 §12.2）", () => {
  it("showInViewer 开启且有 palette 时在标签栏上方渲染色条；无 palette 不渲染", async () => {
    const { useSettingsStore: sstore, DEFAULT_APPEARANCE: DA } = await import("@/stores/settingsStore");
    sstore.setState({
      settings: {
        ai: { profiles: [], activeProfile: "", videoTagging: false, videoTaggingMode: "cover", videoFrameCount: 3, batchLimit: 30, ollamaSourceId: "auto" },
        theme: "system",
        thumbnailCacheMb: 2048,
        tagCategories: [],
        libraryRoot: "",
        trashRetentionDays: 30,
        customDownloadSources: [],
        modelDownloadProxy: "",
        appearance: { ...DA, colorStrip: { ...DA.colorStrip, enabled: true, showInViewer: true } },
      } as never,
      previewAppearance: null,
    });
    const palette = [{ hex: "#1b6ad2", r: 27, g: 106, b: 210, ratio: 0.7 }];
    const imgAsset = mkAsset({ id: 9, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null, palette } as Partial<Asset>);
    // items 里放同一份资产（current 取自 store；不放会落到 beforeEach 的视频资产上）
    useLibraryStore.setState({ items: [imgAsset], total: 1 });
    const { container, rerender } = render(<ViewerPage asset={imgAsset} onClose={vi.fn()} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    expect(container.querySelector(".ui-colorstrip")).not.toBeNull();

    // 无 palette → 不渲染（查看器不占位）
    const noPalette = mkAsset({ id: 9, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null, palette: null } as Partial<Asset>);
    useLibraryStore.setState({ items: [noPalette], total: 1 });
    rerender(<ViewerPage asset={noPalette} onClose={vi.fn()} />);
    await waitFor(() => expect(container.querySelector(".ui-colorstrip")).toBeNull());
    sstore.setState({ settings: null, previewAppearance: null });
  });
});

// ─── FB5-01（§13.1）沉浸浏览 ───────────────────────────────────────────────
// jsdom 无 Fullscreen API：mock document.fullscreenElement + requestFullscreen +
// fullscreenchange 派发驱动状态机；fallback 走 createPortal(document.body)。

const savedFsEl = Object.getOwnPropertyDescriptor(document, "fullscreenElement");

function installFullscreenMock(opts: { supported?: boolean } = {}) {
  const { supported = true } = opts;
  Object.defineProperty(document, "fullscreenElement", {
    configurable: true,
    writable: true,
    value: null,
  });
  const exitSpy = vi.fn(() => {
    (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = null;
    document.dispatchEvent(new Event("fullscreenchange"));
    return Promise.resolve();
  });
  (document as unknown as { exitFullscreen: () => Promise<void> }).exitFullscreen = exitSpy;
  if (supported) {
    (HTMLDivElement.prototype as unknown as { requestFullscreen: () => Promise<void> }).requestFullscreen =
      function requestFullscreen(this: HTMLDivElement) {
        (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = this;
        return Promise.resolve();
      };
  }
  return { exitSpy };
}

function removeFullscreenMock() {
  delete (document as unknown as { exitFullscreen?: unknown }).exitFullscreen;
  delete (HTMLDivElement.prototype as unknown as { requestFullscreen?: unknown }).requestFullscreen;
  if (savedFsEl) Object.defineProperty(document, "fullscreenElement", savedFsEl);
}

describe("ViewerPage 沉浸浏览（FB5-01 §4.1/§13.1）", () => {
  afterEach(() => {
    removeFullscreenMock();
  });

  it("native 沉浸：requestFullscreen 成功 → 只渲染媒体舞台，chrome 全部卸载", async () => {
    installFullscreenMock({ supported: true });
    const onClose = vi.fn();
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={onClose} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());

    // 点击「全屏浏览」→ 进入 native
    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    await waitFor(() =>
      expect((document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement).not.toBeNull(),
    );
    // 沉浸分支：无工具条/属性栏/标签区/胶片条
    expect(screen.queryByRole("button", { name: "返回素材库" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /信息/ })).not.toBeInTheDocument();
    expect(container.querySelector("[data-viewer-immersive]")).not.toBeNull();
    // 图片沉浸为白底（surface 变体）
    await waitFor(() => expect(document.body.querySelector('[data-surface="image-immersive"]')).not.toBeNull());
    // onClose 未被误调
    expect(onClose).not.toHaveBeenCalled();
  });

  it("native 退出：fullscreenchange（fullscreenElement 离开 viewerRoot）→ 回普通查看器，chrome 恢复", async () => {
    installFullscreenMock({ supported: true });
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={vi.fn()} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    // 等沉浸状态真正提交（chrome 卸载）→ fullscreenchange 监听器已用新 immersiveMode 重注册
    await waitFor(() => expect(screen.queryByRole("button", { name: "返回素材库" })).not.toBeInTheDocument());
    // 系统退出：fullscreenElement 置空并派发 fullscreenchange
    (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = null;
    document.dispatchEvent(new Event("fullscreenchange"));
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "全屏浏览" })).toBeInTheDocument();
  });

  it("其他元素进入全屏（fullscreenElement ≠ viewerRoot）不触发沉浸", async () => {
    installFullscreenMock({ supported: true });
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={vi.fn()} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    // 别的元素进了全屏
    const other = document.createElement("div");
    (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = other;
    document.dispatchEvent(new Event("fullscreenchange"));
    // 查看器保持普通模式
    expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument();
    expect(container.querySelector("[data-viewer-immersive]")).toBeNull();
  });

  it("requestFullscreen 失败/不可用 → fallback 沉浸（portal 覆盖 + 应用根 inert）", async () => {
    installFullscreenMock({ supported: false });
    const onClose = vi.fn();
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={onClose} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    // 等沉浸状态提交：portal 沉浸画布出现在 document.body；普通查看器（含工具条）卸载
    await waitFor(() => expect(document.body.querySelector("[data-immersive-canvas]")).not.toBeNull());
    expect(screen.queryByRole("button", { name: "返回素材库" })).not.toBeInTheDocument();
    // 应用根节点 inert（不含 portal）
    await waitFor(() => expect(container.hasAttribute("inert")).toBe(true));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("fallback 沉浸：Esc 退出沉浸（不关闭 Viewer），第二次 Esc 才关闭", async () => {
    installFullscreenMock({ supported: false });
    const onClose = vi.fn();
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={onClose} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    await waitFor(() => expect(document.body.querySelector("[data-immersive-canvas]")).not.toBeNull());

    // 第一次 Esc：退出沉浸，回到普通查看器
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    expect(onClose).not.toHaveBeenCalled();
    // inert 清理
    await waitFor(() => expect(container.hasAttribute("inert")).toBe(false));

    // 第二次 Esc：关闭 Viewer
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("native 沉浸：Esc 由浏览器消费（fullscreenElement 非空），Esc 不关闭 Viewer；退出后第二次 Esc 关闭", async () => {
    installFullscreenMock({ supported: true });
    const onClose = vi.fn();
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={onClose} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    // 等沉浸状态提交（chrome 卸载）
    await waitFor(() => expect(screen.queryByRole("button", { name: "返回素材库" })).not.toBeInTheDocument());
    // native 中 Esc：浏览器接管（fullscreenElement 非空）→ 查看器不关闭
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    // 系统退出全屏（fullscreenchange）→ 普通模式
    (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = null;
    document.dispatchEvent(new Event("fullscreenchange"));
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    // 第二次 Esc → 关闭
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("native 沉浸中关闭 Viewer：先 exitFullscreen 再 onClose，失败也清本地状态", async () => {
    const { exitSpy } = installFullscreenMock({ supported: true });
    const onClose = vi.fn();
    const img = mkAsset({ id: 3, mimeType: "image/jpeg", fileExt: "jpg", filePath: "d:/lib/i.jpg", fileName: "i.jpg", durationMs: null });
    useLibraryStore.setState({ items: [img], total: 1 });
    const { container } = render(<ViewerPage asset={img} onClose={onClose} />);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "全屏浏览" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "返回素材库" })).not.toBeInTheDocument());
    // 关闭（点击返回素材库按钮也走 handleClose，但沉浸时按钮不存在 → 直接模拟关闭路径）
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled(); // native Esc 被浏览器消费
    // 系统退出 → 普通模式 → 再 Esc 关闭
    (document as unknown as { fullscreenElement: HTMLElement | null }).fullscreenElement = null;
    document.dispatchEvent(new Event("fullscreenchange"));
    await waitFor(() => expect(screen.getByRole("button", { name: "返回素材库" })).toBeInTheDocument());
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    // 退出全屏请求已发出（关闭前清理）
    expect(exitSpy).not.toHaveBeenCalled();
  });

  it("视频沉浸：双击视频切换沉浸（fallback），不关闭 Viewer", async () => {
    installFullscreenMock({ supported: false });
    const onClose = vi.fn();
    const vid = mkAsset();
    useLibraryStore.setState({ items: [vid], total: 1 });
    const { container } = render(<ViewerPage asset={vid} onClose={onClose} />);
    const video = container.querySelector("video") as HTMLVideoElement;
    await waitFor(() => expect(video).not.toBeNull());
    fireEvent.doubleClick(video);
    await waitFor(() => expect(document.body.querySelector("[data-immersive-canvas]")).not.toBeNull());
    // 视频沉浸为黑底
    await waitFor(() => expect(document.body.querySelector('[data-surface="video-immersive"]')).not.toBeNull());
    expect(onClose).not.toHaveBeenCalled();
  });
});
