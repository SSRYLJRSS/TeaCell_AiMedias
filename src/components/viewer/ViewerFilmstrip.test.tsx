/** FB4-01（§4.3/§10.2）：胶片条布局语义 —— 单行、80px 外层、56px 缩略图、只横向滚动、点击定位 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import ViewerFilmstrip from "@/components/viewer/ViewerFilmstrip";
import type { Asset } from "@/types/asset";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@/utils/thumbnailCache", () => ({
  requestThumbnail: vi.fn().mockResolvedValue("asset://placeholder.webp"),
}));

function mkAsset(id: number): Asset {
  return {
    id,
    filePath: `d:/lib/a${id}.jpg`,
    fileName: `a${id}.jpg`,
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
    placeholderPath: `thumb${id}.jpg`,
    hdThumbnailPath: null,
    camera: null,
    lens: null,
    iso: null,
    aperture: null,
    shutter: null,
    focal: null,
    tags: [],
  };
}

function getStrip(container: HTMLElement): HTMLElement {
  return container.querySelector("[data-filmstrip]") as HTMLElement;
}

/** 缩略图区是 filmstrip 内第二个子节点（prev 按钮 → 缩略图区 → next 按钮） */
function getThumbArea(container: HTMLElement): HTMLElement {
  return getStrip(container).children[1] as HTMLElement;
}

describe("ViewerFilmstrip（FB4-01 单行胶片条）", () => {
  it("外层固定 h-20（80px）", () => {
    const { container } = render(
      <ViewerFilmstrip items={[mkAsset(1)]} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    expect(getStrip(container).className).toContain("h-20");
  });

  it("缩略图区为单行 flex，不存在双行 grid 结构", () => {
    const { container } = render(
      <ViewerFilmstrip items={[mkAsset(1), mkAsset(2), mkAsset(3)]} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    const area = getThumbArea(container);
    expect(area.className).toContain("flex");
    expect(area.style.gridTemplateRows).not.toBe("repeat(2, minmax(0, 1fr))");
    expect(area.style.gridAutoFlow).not.toBe("column");
  });

  it("缩略图固定 56x56px（size-14）", () => {
    const { container } = render(
      <ViewerFilmstrip items={[mkAsset(1), mkAsset(2)]} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    const thumbs = getThumbArea(container).querySelectorAll("button");
    expect(thumbs).toHaveLength(2);
    for (const t of thumbs) {
      expect(t.className).toContain("size-14");
      expect((t as HTMLElement).style.width).toBe("56px");
      expect((t as HTMLElement).style.height).toBe("56px");
    }
  });

  it("左右导航按钮保持 size-9", () => {
    const { container } = render(
      <ViewerFilmstrip items={[mkAsset(1)]} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    const strip = getStrip(container);
    expect(strip.children[0].className).toContain("size-9");
    expect(strip.children[2].className).toContain("size-9");
  });

  it("中间区 overflow-x-auto 且 overflow-y-hidden（只横向滚动）", () => {
    const { container } = render(
      <ViewerFilmstrip items={Array.from({ length: 20 }, (_, i) => mkAsset(i + 1))} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    const area = getThumbArea(container);
    expect(area.className).toContain("overflow-x-auto");
    expect(area.className).toContain("overflow-y-hidden");
    expect(area.className).toContain("min-w-0");
    expect(area.className).toContain("flex-1");
  });

  it("1 / 2 / 3 个素材都保持单行结构（缩略图数量正确）", () => {
    for (const n of [1, 2, 3]) {
      const { container } = render(
        <ViewerFilmstrip items={Array.from({ length: n }, (_, i) => mkAsset(i + 1))} currentId={1} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
      );
      expect(getThumbArea(container).querySelectorAll("button")).toHaveLength(n);
    }
  });

  it("点击缩略图调用 onJump(index)；左右按钮调用 onPrev/onNext", () => {
    const onJump = vi.fn();
    const onPrev = vi.fn();
    const onNext = vi.fn();
    render(
      <ViewerFilmstrip items={[mkAsset(1), mkAsset(2), mkAsset(3)]} currentId={1} onJump={onJump} onPrev={onPrev} onNext={onNext} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "第 3 张：a3.jpg" }));
    expect(onJump).toHaveBeenCalledWith(2);
    fireEvent.click(screen.getByRole("button", { name: "上一张" }));
    expect(onPrev).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "下一张" }));
    expect(onNext).toHaveBeenCalledTimes(1);
  });

  it("当前素材高亮（accent 边框）", () => {
    render(
      <ViewerFilmstrip items={[mkAsset(1), mkAsset(2)]} currentId={2} onJump={vi.fn()} onPrev={vi.fn()} onNext={vi.fn()} />,
    );
    const current = screen.getByRole("button", { name: "第 2 张：a2.jpg" });
    expect(current.className).toContain("border-[var(--color-accent)]");
  });
});
