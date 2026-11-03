/**
 * AssetCard 回归测试（指导书 §6.1/§13.1）：
 *  - 渲染树不含 <video>（素材库禁止视频 hover/隐藏播放）；
 *  - 不含 fixed 大图 popover（role=dialog）；不创建任何媒体浮层；
 *  - 双击仍调用 onPreview；
 *  - 缩略图、格式/时长角标、选中勾选正常显示。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import AssetCard from "@/components/library/AssetCard";
import { useSettingsStore, DEFAULT_APPEARANCE, DEFAULT_NAVIGATION } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";
import type { Asset } from "@/types/asset";

vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://hd.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));

// Thumbnail 用 IntersectionObserver 触发高清生成；jsdom 缺失，补最小桩
vi.stubGlobal("IntersectionObserver", class {
  cb: IntersectionObserverCallback;
  constructor(cb: IntersectionObserverCallback) {
    this.cb = cb;
  }
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
});

const mkAsset = (over: Partial<Asset> = {}): Asset => ({
  id: 1,
  filePath: "d:/lib/a1.jpg",
  fileName: "a1.jpg",
  fileExt: "jpg",
  fileSize: 100,
  mimeType: "image/jpeg",
  width: 800,
  height: 600,
  durationMs: null,
  videoCodec: null,
  audioCodec: null,
  takenAt: null,
  createdAt: 1,
  modifiedAt: 1,
  hash: null,
  placeholderPath: "thumb1.jpg",
  hdThumbnailPath: null,
  camera: null,
  lens: null,
  iso: null,
  aperture: null,
  shutter: null,
  focal: null,
  tags: [],
  ...over,
});

const noop = () => {};

/** §12：让 hover 预览处于关闭态（素材库两级开关都关）。构造最小可用 settings，不依赖 store 已加载。 */
function disableHoverPreview() {
  const base = useSettingsStore.getState().settings;
  const appearance = base?.appearance ?? DEFAULT_APPEARANCE;
  useSettingsStore.setState({
    settings: {
      ...(base ?? mkMinimalSettings()),
      appearance: { ...appearance, hoverPreview: { ...appearance.hoverPreview, enabled: false, inLibraryGrid: false } },
    },
  });
}

/** 最小可用完整 Settings（测试用，字段不全会触发 TS 但仍走 normalize 兜底）。 */
function mkMinimalSettings(): Settings {
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
    appearance: DEFAULT_APPEARANCE,
    navigation: DEFAULT_NAVIGATION,
  };
}

beforeEach(() => {
  useSettingsStore.setState({ settings: null, previewAppearance: null });
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

/** FB2-08（§16.5）：开启色条两级开关。 */
function enableColorStrip() {
  const base = useSettingsStore.getState().settings;
  const appearance = base?.appearance ?? DEFAULT_APPEARANCE;
  useSettingsStore.setState({
    settings: {
      ...(base ?? mkMinimalSettings()),
      appearance: { ...appearance, colorStrip: { ...appearance.colorStrip, enabled: true, showInLibraryGrid: true } },
    },
  });
}

describe("AssetCard §6.1 + FB2-03（素材库 hover 原位视频预览）", () => {
  it("图片卡片渲染树无 <video>、无 role=dialog popover、无 position:fixed 预览层", () => {
    const { container } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "image/jpeg", durationMs: null })}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    expect(container.querySelector("video")).toBeNull();
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    const fixed = Array.from(container.querySelectorAll("*")).filter(
      (el) => (el as HTMLElement).style?.position === "fixed" || (el as HTMLElement).className?.includes?.("fixed"),
    );
    expect(fixed).toHaveLength(0);
    // 缩略图仍在
    expect(container.querySelector("img")).not.toBeNull();
  });

  // FB2-08（FX-07）：色条渲染与设置联动
  it("色条开启且有 palette 时渲染 role=img 色条；palette 为 null 时不渲染", () => {
    enableColorStrip();
    const palette = [
      { hex: "#1b2a3c", r: 27, g: 42, b: 60, ratio: 0.6 },
      { hex: "#e6dfc8", r: 230, g: 223, b: 200, ratio: 0.4 },
    ];
    const { container, rerender } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "image/jpeg", palette } as Partial<Asset>)}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    expect(container.querySelector('[role="img"]')).not.toBeNull();
    expect(container.querySelector(".ui-colorstrip")).not.toBeNull();

    rerender(
      <AssetCard
        asset={mkAsset({ mimeType: "image/jpeg", palette: null } as Partial<Asset>)}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    // FB3-01：palette 未到达 → 不渲染色条内容，但保留固定高度空槽（恒定几何）
    expect(container.querySelector(".ui-colorstrip")).toBeNull();
  });

  // FB3-01（§3.3）：恒定槽位——混排卡片（有/无 palette）的色条区高度完全一致
  it("色条槽位高度恒定：有 palette 与无 palette 的卡片 wrapper 高度相同（HEIGHT_PX）", () => {
    enableColorStrip();
    const palette = [{ hex: "#1b2a3c", r: 27, g: 42, b: 60, ratio: 0.6 }];
    const { container } = render(
      <div>
        <AssetCard
          asset={mkAsset({ mimeType: "image/jpeg", palette } as Partial<Asset>)}
          index={0}
          thumbSize={512}
          selected={false}
          onSelect={noop}
          onPreview={noop}
          onContextMenu={noop}
        />
        <AssetCard
          asset={mkAsset({ id: 2, filePath: "d:/lib/a2.jpg", fileName: "a2.jpg", mimeType: "image/jpeg", palette: null } as Partial<Asset>)}
          index={1}
          thumbSize={512}
          selected={false}
          onSelect={noop}
          onPreview={noop}
          onContextMenu={noop}
        />
      </div>,
    );
    // 两张卡片的色条槽位 wrapper 都存在且高度一致（AssetCard 根 div 的直接子节点中带 style.height）
    const slots = Array.from(container.querySelectorAll<HTMLElement>('[aria-hidden]')).filter(
      (el) => el.style.height,
    );
    expect(slots).toHaveLength(2);
    expect(slots[0].style.height).toBe(slots[1].style.height);
    expect(slots[0].style.height).toBe("10px"); // HEIGHT_PX.normal
  });

  // FB3-01：开关关闭 → 不占色条高度（槽位整个不存在）
  it("色条关闭时不渲染槽位（不占高度）", () => {
    const base = useSettingsStore.getState().settings;
    const appearance = base?.appearance ?? DEFAULT_APPEARANCE;
    useSettingsStore.setState({
      settings: {
        ...(base ?? mkMinimalSettings()),
        appearance: { ...appearance, colorStrip: { ...appearance.colorStrip, enabled: false, showInLibraryGrid: false } },
      },
    });
    const { container } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "image/jpeg", palette: [{ hex: "#1b2a3c", r: 27, g: 42, b: 60, ratio: 0.6 }] } as Partial<Asset>)}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    expect(container.querySelector(".ui-colorstrip")).toBeNull();
    const slots = Array.from(container.querySelectorAll<HTMLElement>('[aria-hidden]')).filter((el) => el.style.height);
    expect(slots).toHaveLength(0);
  });

  it("视频卡片：hover 未触发时无 <video>、无 dialog、无 fixed 浮层（默认）", () => {
    const { container } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "video/mp4", durationMs: 5000, fileExt: "mp4" })}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    expect(container.querySelector("video")).toBeNull();
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    // 时长角标仍显示
    expect(screen.getByText("0:05")).toBeInTheDocument();
  });

  it("视频卡片：hover 300ms 后原位出现 <video>，为 absolute inset-0 而非 fixed；无 fixed 浮层", () => {
    const { container } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "video/mp4", durationMs: 5000, fileExt: "mp4" })}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    const card = screen.getByRole("button");
    fireEvent.mouseEnter(card);
    // 300ms intent 前不出现
    act(() => vi.advanceTimersByTime(280));
    expect(container.querySelector("video")).toBeNull();
    // 300ms 后出现
    act(() => vi.advanceTimersByTime(30));
    const v = container.querySelector("video");
    expect(v).not.toBeNull();
    // §12.4 承诺：卡内原位播放，absolute inset-0，绝非 fixed
    expect(v?.className).toContain("absolute");
    expect(v?.className).toContain("inset-0");
    expect(v?.className).not.toContain("fixed");
    // 不制造 fixed 浮层、无 dialog
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    const fixed = Array.from(container.querySelectorAll("*")).filter(
      (el) => (el as HTMLElement).style?.position === "fixed" || (el as HTMLElement).className?.includes?.("fixed"),
    );
    expect(fixed).toHaveLength(0);
  });

  it("视频卡片：设置关闭 hoverPreview 后，hover 300ms 仍无 <video>", () => {
    disableHoverPreview();
    const { container } = render(
      <AssetCard
        asset={mkAsset({ mimeType: "video/mp4", durationMs: 5000, fileExt: "mp4" })}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    const card = screen.getByRole("button");
    fireEvent.mouseEnter(card);
    act(() => vi.advanceTimersByTime(400));
    expect(container.querySelector("video")).toBeNull();
  });

  it("双击仍调用 onPreview（进入 Viewer 的入口保持不变）", () => {
    const preview = vi.fn();
    render(
      <AssetCard
        asset={mkAsset()}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={preview}
        onContextMenu={noop}
      />,
    );
    const card = screen.getByRole("button");
    fireEvent.doubleClick(card);
    expect(preview).toHaveBeenCalledTimes(1);
  });

  it("单击调用 onSelect，右键调用 onContextMenu", () => {
    const select = vi.fn();
    const ctx = vi.fn();
    render(
      <AssetCard
        asset={mkAsset()}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={select}
        onPreview={noop}
        onContextMenu={ctx}
      />,
    );
    const card = screen.getByRole("button");
    fireEvent.click(card);
    expect(select).toHaveBeenCalledTimes(1);
    fireEvent.contextMenu(card);
    expect(ctx).toHaveBeenCalledTimes(1);
  });

  it("选中态渲染勾选角标与 aria-selected", () => {
    const { container } = render(
      <AssetCard
        asset={mkAsset()}
        index={0}
        thumbSize={512}
        selected
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    expect(screen.getByRole("button").getAttribute("aria-selected")).toBe("true");
    expect(container.querySelector(".rounded-full")).not.toBeNull(); // 勾选圆形角标
  });

  // FB6 需求三：媒体框与色条无缝拼接（底部直角 + 底边圆角，无白缝）
  describe("FB6 需求三：色条拼接几何", () => {
    const palette = [
      { hex: "#1b2a3c", r: 27, g: 42, b: 60, ratio: 0.6 },
      { hex: "#e6dfc8", r: 230, g: 223, b: 200, ratio: 0.4 },
    ];

    it("色条可见时：媒体框顶圆底方（style.borderRadius），色条底部圆角、顶边贴合", () => {
      enableColorStrip();
      const { container } = render(
        <AssetCard
          asset={mkAsset({ mimeType: "image/jpeg", palette } as Partial<Asset>)}
          index={0}
          thumbSize={512}
          selected={false}
          onSelect={noop}
          onPreview={noop}
          onContextMenu={noop}
        />,
      );
      const media = screen.getByRole("button") as HTMLElement;
      expect(media.style.borderRadius).toBe("var(--radius-item) var(--radius-item) 0 0");
      const strip = container.querySelector(".ui-colorstrip") as HTMLElement;
      expect(strip.style.borderRadius).toBe("0 0 4px 4px");
      // 拼接处无缝：媒体框与色条之间无 margin / gap 元素
      expect(media.style.margin).toBe("");
      expect(strip.style.margin).toBe("");
      // FB6 白线修复：媒体框自身不带 inset 描边（底边 hairline 会贴着色条顶边画白线），
      // 描边上移到外层 wrapper（媒体框+色条整体描边）
      expect(media.className).not.toContain("ring-1");
      expect(media.parentElement!.className).toContain("ring-1");
      expect(media.parentElement!.className).toContain("rounded-md");
      expect(media.parentElement!.className).toContain("overflow-hidden");
      // 色条恒定槽位与 rowHeight 共用 HEIGHT_PX（行高不变）
      const slot = Array.from(container.querySelectorAll<HTMLElement>('[aria-hidden]')).find((el) => el.style.height);
      expect(slot?.style.height).toBe("10px");
    });

    it("空 palette（空槽）时媒体框保持整体圆角（不创建空色条占位）", () => {
      enableColorStrip();
      const { container } = render(
        <AssetCard
          asset={mkAsset({ mimeType: "image/jpeg", palette: null } as Partial<Asset>)}
          index={0}
          thumbSize={512}
          selected={false}
          onSelect={noop}
          onPreview={noop}
          onContextMenu={noop}
        />,
      );
      expect((screen.getByRole("button") as HTMLElement).style.borderRadius).toBe("");
      expect(screen.getByRole("button").className).toContain("rounded-md");
      expect(container.querySelector(".ui-colorstrip")).toBeNull();
    });

    it("色条开关关闭时媒体框保持整体圆角", () => {
      const { container } = render(
        <AssetCard
          asset={mkAsset({ mimeType: "image/jpeg", palette } as Partial<Asset>)}
          index={0}
          thumbSize={512}
          selected={false}
          onSelect={noop}
          onPreview={noop}
          onContextMenu={noop}
        />,
      );
      expect((screen.getByRole("button") as HTMLElement).style.borderRadius).toBe("");
      expect(container.querySelector(".ui-colorstrip")).toBeNull();
    });
  });

  // M-13：超长中文文件名只能截断显示，必须能 hover 看到全量，否则用户无法确认是哪个文件
  it("hover 文件名层带 title 全量提示，超长中文文件名不被截断后失去可读途径", () => {
    const longName = "旅行-2026-秋-第一天-上午-这是一个非常非常长的中文文件名用来验证截断后仍可读.png";
    render(
      <AssetCard
        asset={mkAsset({ fileName: longName } as Partial<Asset>)}
        index={0}
        thumbSize={512}
        selected={false}
        onSelect={noop}
        onPreview={noop}
        onContextMenu={noop}
      />,
    );
    const hoverLayer = screen.getByTitle(longName);
    expect(hoverLayer).toBeInTheDocument();
    expect(hoverLayer.className).toContain("truncate");
  });
});
