/** A3-08：结果视图的「离开/返回保留上下文」。
 *
 *  这里最关键的一条是**分页与滚动位置不能因为进了一次查看器就丢**：结果视图在
 *  打开 Viewer 时会被整体卸载（与素材库同一种页面级替换），交接来的批次往往
 *  很长，如果回来只剩首屏，用户就得重新翻一遍。位置因此存在 store 里。 */
import { describe, expect, it, beforeEach, vi } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  pageSelection: vi.fn(),
}));

vi.mock("@/api/selections", () => ({
  pageSelection: mocks.pageSelection,
}));

vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: () => "asset://thumb.jpg",
  toFileUrl: (path: string) => `asset://${path}`,
}));

vi.mock("@/utils/logger", () => ({
  logger: { error: vi.fn(), warn: vi.fn(), info: vi.fn() },
}));

import SelectionResultView from "./SelectionResultView";
import { useSelectionResultStore } from "@/stores/selectionResultStore";
import type { Asset } from "@/types/asset";

// jsdom 没有 IntersectionObserver，缩略图懒加载会直接抛错（与 LibraryPage.test 同样的处理）。
class NoopIntersectionObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
}
Object.defineProperty(globalThis, "IntersectionObserver", {
  configurable: true,
  value: NoopIntersectionObserver,
});

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
    createdAt: id,
    modifiedAt: id,
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

const props = {
  selectionRef: "selection_aaaaaaaaaaaaaaaa",
  title: "交接的 4 项",
  query: "素材库中的选择",
  onExit: () => undefined,
  onPreview: () => undefined,
};

beforeEach(() => {
  mocks.pageSelection.mockReset();
  useSelectionResultStore.setState({ target: null, progressBySelection: {} });
});

describe("SelectionResultView", () => {
  it("来源与标题都用后端给的口径，不在前端另编一份", async () => {
    mocks.pageSelection.mockResolvedValue({
      selectionRef: props.selectionRef,
      total: 4,
      truncated: false,
      snapshot: 4,
      available: 4,
      missing: 0,
      query: "搜索「猫」的结果",
      items: [mkAsset(1)],
      nextOffset: 1,
    });

    render(<SelectionResultView {...props} />);
    await waitFor(() => expect(screen.getByText(/来自搜索「猫」的结果的固定快照/)).toBeInTheDocument());
    expect(screen.getByText(/共 4 项/)).toBeInTheDocument();
    expect(screen.getByText(/可用 4 项/)).toBeInTheDocument();
  });

  it("不可用项如实报数而不是静默缩短列表", async () => {
    mocks.pageSelection.mockResolvedValue({
      selectionRef: props.selectionRef,
      total: 4,
      truncated: false,
      snapshot: 4,
      available: 1,
      missing: 3,
      query: "素材库中的选择",
      items: [mkAsset(1)],
    });

    render(<SelectionResultView {...props} />);
    await waitFor(() =>
      expect(screen.getByText(/3 项已不可用（被删除或已移出总库），不会显示/)).toBeInTheDocument(),
    );
  });

  it("重新挂载时按已存页数读回后续页，并恢复滚动位置", async () => {
    // 第一次进入：读到两页，然后卸载（模拟双击进查看器）。
    mocks.pageSelection
      .mockResolvedValueOnce({
        selectionRef: props.selectionRef,
        total: 4,
        truncated: false,
        snapshot: 4,
        available: 4,
        missing: 0,
        query: "素材库中的选择",
        items: [mkAsset(1), mkAsset(2)],
        nextOffset: 2,
      })
      .mockResolvedValueOnce({
        selectionRef: props.selectionRef,
        total: 4,
        truncated: false,
        snapshot: 4,
        available: 4,
        missing: 0,
        query: "素材库中的选择",
        items: [mkAsset(3), mkAsset(4)],
      });

    const first = render(<SelectionResultView {...props} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "加载更多" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "加载更多" }));
    await waitFor(() => expect(screen.getByText(/已显示全部 4 项/)).toBeInTheDocument());
    const scroller = document.querySelector(".overflow-y-auto") as HTMLElement;
    Object.defineProperty(scroller, "scrollTop", { value: 480, writable: true });
    fireEvent.scroll(scroller);
    first.unmount();

    expect(useSelectionResultStore.getState().progressBySelection[props.selectionRef]).toEqual({
      loadedPages: 2,
      scrollTop: 480,
    });

    // 第二次进入：必须一次读回两页，而不是只剩首屏。
    mocks.pageSelection.mockResolvedValueOnce({
      selectionRef: props.selectionRef,
      total: 4,
      truncated: false,
      snapshot: 4,
      available: 4,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(1), mkAsset(2)],
      nextOffset: 2,
    });
    mocks.pageSelection.mockResolvedValueOnce({
      selectionRef: props.selectionRef,
      total: 4,
      truncated: false,
      snapshot: 4,
      available: 4,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(3), mkAsset(4)],
    });

    render(<SelectionResultView {...props} />);
    await waitFor(() => expect(screen.getByText(/已显示全部 4 项/)).toBeInTheDocument());
    expect(mocks.pageSelection).toHaveBeenCalledTimes(4);
    await waitFor(() =>
      expect((document.querySelector(".overflow-y-auto") as HTMLElement).scrollTop).toBe(480),
    );
  });

  it("失败时给出可重试的提示，不假装快照为空", async () => {
    mocks.pageSelection.mockRejectedValueOnce(new Error("读取超时"));

    render(<SelectionResultView {...props} />);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("结果读取失败：读取超时"));
    expect(screen.queryByText(/这份快照里的素材都已不可用/)).not.toBeInTheDocument();
  });

  it("快照属于别的会话时明说来源不同，不冒充当前会话的结果", async () => {
    mocks.pageSelection.mockResolvedValue({
      selectionRef: props.selectionRef,
      total: 2,
      truncated: false,
      snapshot: 2,
      available: 2,
      missing: 0,
      query: "搜索「猫」的结果",
      items: [mkAsset(1), mkAsset(2)],
    });

    render(<SelectionResultView {...props} fromOtherConversation />);
    await waitFor(() =>
      expect(
        screen.getByText(/这份结果来自另一个助手会话的交接，不是当前会话的素材集合/),
      ).toBeInTheDocument(),
    );
  });

  it("没有跨会话时不出这条标注，避免凭空提示", async () => {
    mocks.pageSelection.mockResolvedValue({
      selectionRef: props.selectionRef,
      total: 2,
      truncated: false,
      snapshot: 2,
      available: 2,
      missing: 0,
      query: "素材库中的选择",
      items: [mkAsset(1), mkAsset(2)],
    });

    render(<SelectionResultView {...props} />);
    await waitFor(() => expect(screen.getByText(/共 2 项/)).toBeInTheDocument());
    expect(screen.queryByText(/来自另一个助手会话/)).not.toBeInTheDocument();
  });
});
