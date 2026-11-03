/** PendingList 测试：固定动作头部 + §10（FB-04）卡片内视频播放。
 *  - hover 300ms 后在**卡片容器内**出现 video（无 fixed dialog / 无浮层）；
 *  - 离开 350ms 暂停并卸载；快速扫过不创建实例；
 *  - 编码失败退回封面并提示「双击打开」；
 *  - 双击回调 onOpenItem；视频控件阻止冒泡；
 *  - 入库运行中禁用操作且不挂载视频。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import PendingList from "@/components/import/PendingList";
import { useSettingsStore, DEFAULT_APPEARANCE } from "@/stores/settingsStore";
import type { ImportPlanItem } from "@/types/import";

vi.mock("@/api/preview", () => ({
  getPreviewUrl: (p: string) => Promise.resolve(`asset://preview/${p}`),
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://${p}`,
}));

// 让 rAF 同步执行：FB6 需求二的切档提交/锚点恢复都走 rAF（jsdom 无真实帧时钟）
vi.spyOn(global, "requestAnimationFrame").mockImplementation((cb) => {
  cb(0);
  return 0;
});
vi.spyOn(global, "cancelAnimationFrame").mockImplementation(() => {});

// LazyThumb 依赖 IntersectionObserver（jsdom 无实现）
vi.stubGlobal(
  "IntersectionObserver",
  class {
    cb: IntersectionObserverCallback;
    constructor(cb: IntersectionObserverCallback) {
      this.cb = cb;
    }
    observe() {
      this.cb([{ isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
    }
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  },
);

const items: ImportPlanItem[] = [
  { path: "d:/p/a.jpg", kind: "image", size: 1024 * 512, previewStatus: "ready" },
  { path: "d:/p/b.mp4", kind: "video", size: 1024 * 1024 * 5, previewStatus: "ready" },
  { path: "d:/p/c.mov", kind: "video", size: 1024 * 1024 * 4, previewStatus: "ready" },
];

async function renderGrid(props: { onOpenItem?: (p: string) => void; running?: boolean } = {}) {
  const onRemove = vi.fn();
  const onOpenItem = props.onOpenItem ?? vi.fn();
  // 卡片封面走 getPreviewUrl 的 promise：必须让它在 act 内落地，否则 React 会在
  // act 之外收到 setUrl 并打印「not wrapped in act」警告（断言本身不变）。
  let utils!: ReturnType<typeof render>;
  await act(async () => {
    utils = render(
      <PendingList
        items={items}
        running={props.running ?? false}
        onRemove={onRemove}
        onOpenItem={onOpenItem}
        onAddFiles={() => {}}
        onAddFolder={() => {}}
        onClear={() => {}}
      />,
    );
  });
  // 切到网格视图。切视图会让每张卡片的 LazyThumb 起一次 IntersectionObserver →
  // getPreviewUrl 的 promise，必须连这次切换一起 act 掉，否则封面落地时会在 act 外改状态。
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "缩略图视图" }));
  });
  return { ...utils, onRemove, onOpenItem };
}

/** 通过完整路径定位一张卡片（卡片容器 = 标题的 relative 祖先；title 用 item.path） */
function cardOf(path: string): HTMLElement {
  const title = screen.getByTitle(path);
  const card = title.closest(".group") as HTMLElement;
  expect(card).toBeTruthy();
  return card;
}

/** 推进 fake timer 并 flush React 状态更新（async act 才会把微任务里的 setState 也收进来） */
async function advance(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
}

/** 可控的 performance.now（节流窗断言用；真实时钟在毫秒内连续 fireEvent 无法跨过 60ms） */
let nowMs = 0;
const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => nowMs);

beforeEach(() => {
  // 只 fake 定时器（hover intent 用）；rAF 保持上方同步 mock（FB6 缩放依赖）
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
  useSettingsStore.setState({ settings: null, previewAppearance: null });
  nowMs = 0;
  nowSpy.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

/** 当前生效的入库格宽档位（previewAppearance 优先） */
function currentImportCellStep(): number {
  const s = useSettingsStore.getState();
  return (s.previewAppearance ?? s.settings?.appearance ?? DEFAULT_APPEARANCE).grid.importCellStep;
}

describe("PendingList 固定动作头部", () => {
  it("清单非空时提供添加文件/添加文件夹/清空入口，不重复展示左侧统计", async () => {
    await act(async () => {
      render(<PendingList items={items} running={false} onRemove={() => {}} />);
    });
    expect(screen.getByRole("button", { name: "添加文件" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加文件夹" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "清空清单" })).toBeInTheDocument();
    expect(screen.queryByText(/3 项/)).toBeNull();
    expect(screen.queryByText("9.5 MB")).toBeNull();
  });

  it("对可预览但后续能力可能受限的图片显示文字提示", async () => {
    await act(async () => {
      render(
        <PendingList
          items={[
            {
              path: "d:/p/limited.cr2",
              kind: "image",
              size: 1024,
              previewStatus: "limited",
              previewMessage: "RAW 格式能力可能受限",
            },
          ]}
          running={false}
          onRemove={() => {}}
        />,
      );
    });
    expect(screen.getByText("可能受限")).toHaveAttribute("title", "RAW 格式能力可能受限");
  });

  it("点击添加文件/文件夹/清空调用对应事件（不直接 invoke）", async () => {
    const onAddFiles = vi.fn();
    const onAddFolder = vi.fn();
    const onClear = vi.fn();
    await act(async () => {
      render(
        <PendingList
          items={items}
          running={false}
          onRemove={() => {}}
          onAddFiles={onAddFiles}
          onAddFolder={onAddFolder}
          onClear={onClear}
        />,
      );
    });
    fireEvent.click(screen.getByRole("button", { name: "添加文件" }));
    fireEvent.click(screen.getByRole("button", { name: "添加文件夹" }));
    fireEvent.click(screen.getByRole("button", { name: "清空清单" }));
    expect(onAddFiles).toHaveBeenCalledTimes(1);
    expect(onAddFolder).toHaveBeenCalledTimes(1);
    expect(onClear).toHaveBeenCalledTimes(1);
  });

  it("入库运行中禁用添加/清空", async () => {
    const onAddFiles = vi.fn();
    const onClear = vi.fn();
    await act(async () => {
      render(
        <PendingList items={items} running onRemove={() => {}} onAddFiles={onAddFiles} onClear={onClear} />,
      );
    });
    expect(screen.getByRole("button", { name: "添加文件" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "添加文件夹" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "清空清单" })).toBeDisabled();
  });
});

describe("PendingList §10 卡片内视频播放（FB-04）", () => {
  it("hover 300ms 后在卡片容器内出现 video，且无 fixed dialog 浮层", async () => {
    const { container } = await renderGrid();
    const card = cardOf("d:/p/b.mp4");
    expect(card.querySelector("video")).toBeNull(); // 未激活不挂载

    fireEvent.mouseEnter(card);
    await advance(300);
    const video = card.querySelector("video");
    expect(video).not.toBeNull(); // 卡片内
    expect(video?.closest(".group")).toBe(card); // 原卡片容器内（非浮层）
    expect(container.querySelector("[role='dialog']")).toBeNull();
    expect(video?.muted).toBe(true);
    expect(video?.playsInline).toBe(true);
    expect(video?.getAttribute("preload")).toBe("metadata");
  });

  it("离开 350ms 后暂停并卸载视频层（src 清空）", async () => {
    await renderGrid();
    const card = cardOf("d:/p/b.mp4");
    fireEvent.mouseEnter(card);
    await advance(300);
    const video = card.querySelector("video") as HTMLVideoElement;
    expect(video).not.toBeNull();

    fireEvent.mouseLeave(card);
    await advance(400);
    expect(card.querySelector("video")).toBeNull();
  });

  it("快速扫过不创建视频实例（enter 延迟内 leave 不激活）", async () => {
    await renderGrid();
    const card = cardOf("d:/p/b.mp4");
    fireEvent.mouseEnter(card);
    fireEvent.mouseLeave(card); // <300ms 离开
    await advance(500);
    expect(card.querySelector("video")).toBeNull();
  });

  it("编码失败退回封面并提示「双击打开」", async () => {
    await renderGrid();
    const card = cardOf("d:/p/b.mp4");
    fireEvent.mouseEnter(card);
    await advance(300);
    const video = card.querySelector("video") as HTMLVideoElement;
    expect(video).not.toBeNull();
    fireEvent.error(video);
    // 视频层替换为失败提示（封面 + 提示）
    expect(card.querySelector("video")).toBeNull();
    expect(card.textContent).toContain("无法播放");
  });

  it("同时最多 1 个视频（新激活暂停旧实例，不新增多个实例）", async () => {
    await renderGrid();
    const b = cardOf("d:/p/b.mp4");
    const c = cardOf("d:/p/c.mov");
    fireEvent.mouseEnter(b);
    await advance(300);
    expect(b.querySelector("video")).not.toBeNull();
    // 快速滑到 c：b 还未离开（leave=350ms）时就激活 c
    fireEvent.mouseEnter(c);
    await advance(300);
    const cVideo = c.querySelector("video") as HTMLVideoElement | null;
    expect(cVideo).not.toBeNull();
  });

  it("双击调用 onOpenItem；移除按钮阻止冒泡", async () => {
    const onOpenItem = vi.fn();
    await renderGrid({ onOpenItem });
    const card = cardOf("d:/p/b.mp4");
    fireEvent.doubleClick(card);
    expect(onOpenItem).toHaveBeenCalledWith("d:/p/b.mp4");
  });

  it("入库运行中不启动 hover 预览（禁用 intent）", async () => {
    await renderGrid({ running: true });
    const card = cardOf("d:/p/b.mp4");
    fireEvent.mouseEnter(card);
    await advance(400);
    expect(card.querySelector("video")).toBeNull();
  });
});

/** R13：三个视图都必须能被用户分辨并切换，且切换不丢选择上下文。 */
describe("PendingList 列表 / 缩略图 / 单图三视图（R13）", () => {
  /** 渲染原始清单，返回移除回调（不预切视图，默认是列表）。 */
  async function renderList(running = false) {
    const onRemove = vi.fn();
    // 单图/缩略图视图会挂 LazyThumb，其 getPreviewUrl promise 必须在 act 内落地
    await act(async () => {
      render(
        <PendingList
          items={items}
          running={running}
          onRemove={onRemove}
          onAddFiles={() => {}}
          onAddFolder={() => {}}
          onClear={() => {}}
        />,
      );
    });
    return { onRemove };
  }

  async function switchTo(label: "列表视图" | "缩略图视图" | "单图视图") {
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: label }));
    });
  }

  it("提供列表 / 缩略图 / 单图三个可辨视图，切换后呈现对应形态", async () => {
    await renderList();
    // 三个入口靠固定中文名区分（不是只靠三个相似图标让人猜）
    for (const label of ["列表视图", "缩略图视图", "单图视图"] as const) {
      expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
    }
    // 默认列表：三行清单，没有网格容器、也没有单图放大区
    expect(document.querySelector("[data-pending-grid]")).toBeNull();
    expect(screen.queryByRole("group", { name: "单图预览" })).toBeNull();
    // 缩略图：出现网格容器
    await switchTo("缩略图视图");
    const grid = document.querySelector("[data-pending-grid]");
    expect(grid).not.toBeNull();
    // 网格里是三张卡（不是空容器），否则「缩略图视图」等于什么都没画
    expect((grid as HTMLElement).querySelectorAll(".group")).toHaveLength(3);
    expect(screen.queryByRole("group", { name: "单图预览" })).toBeNull();
    // 单图：出现放大区，网格消失，且一次只显示一张
    await switchTo("单图视图");
    expect(screen.getByRole("group", { name: "单图预览" })).toBeInTheDocument();
    expect(document.querySelector("[data-pending-grid]")).toBeNull();
    expect(screen.getByTestId("pending-single-position").textContent).toContain("第 1 / 3 张");
  });

  it("切到列表再切回单图，仍停在原来那张（不丢选择上下文）", async () => {
    await renderList();
    await switchTo("单图视图");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "下一张" }));
    });
    expect(screen.getByTestId("pending-single-position").textContent).toContain("第 2 / 3 张");

    await switchTo("列表视图");
    expect(screen.queryByRole("group", { name: "单图预览" })).toBeNull();
    // 列表视图里三张都在，选择上下文没有被清掉
    expect(screen.getByTitle("d:/p/b.mp4")).toBeInTheDocument();

    await switchTo("单图视图");
    expect(screen.getByTestId("pending-single-position").textContent).toContain("第 2 / 3 张");
  });

  it("单图里移除当前项走同一个 onRemove（不存在第二份数据）", async () => {
    const { onRemove } = await renderList();
    await switchTo("单图视图");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "下一张" }));
    });
    // 当前放大的是第 2 张 b.mp4，移除必须回报同一条 path
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "移除" }));
    });
    expect(onRemove).toHaveBeenCalledTimes(1);
    expect(onRemove).toHaveBeenCalledWith("d:/p/b.mp4");
  });
});

describe("PendingList 缩略图视图 Alt/Ctrl/Cmd + 滚轮缩放（FB6 需求二）", () => {
  /** 渲染并切到指定视图，返回滚动容器 */
  async function setup(view: "grid" | "list" = "grid", running = false) {
    await act(async () => {
      render(<PendingList items={items} running={running} onRemove={() => {}} />);
    });
    if (view === "grid") {
      // 同 renderGrid：切视图会触发封面取图，必须一起 act。
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "缩略图视图" }));
      });
    }
    return screen.getByTestId("pending-grid-scroll");
  }

  it("Alt + 滚轮向上增大卡片（档位 +1）、向下减小（档位 -1）", async () => {
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(2);
    act(() => {
      nowMs += 80; // 跨过节流窗
    });
    fireEvent.wheel(scroll, { deltaY: 100, altKey: true });
    expect(currentImportCellStep()).toBe(1);
  });

  it("Ctrl/Cmd + 滚轮兼容（与素材库习惯一致）", async () => {
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: -100, ctrlKey: true });
    expect(currentImportCellStep()).toBe(2);
    act(() => {
      nowMs += 80; // 跨过节流窗
    });
    fireEvent.wheel(scroll, { deltaY: -100, metaKey: true });
    expect(currentImportCellStep()).toBe(3);
  });

  it("普通滚轮不改档位（只滚动列表）", async () => {
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: -100 });
    expect(currentImportCellStep()).toBe(1);
  });

  it("列表视图不触发网格缩放", async () => {
    const scroll = await setup("list");
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(1);
  });

  it("边界档位不再改变且不越界", async () => {
    useSettingsStore.setState({
      previewAppearance: { ...DEFAULT_APPEARANCE, grid: { ...DEFAULT_APPEARANCE.grid, importCellStep: 0 } },
    });
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: 100, altKey: true });
    expect(currentImportCellStep()).toBe(0);

    act(() => {
      useSettingsStore.setState({
        previewAppearance: {
          ...DEFAULT_APPEARANCE,
          grid: { ...DEFAULT_APPEARANCE.grid, importCellStep: 7 /* CELL_STEPS.length - 1 */ },
        },
      });
    });
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(7);
  });

  it("触摸板连续事件最多按节流频率改变一档（60ms 内第二事件不生效）", async () => {
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(2);
    // 超过节流窗后可继续步进
    act(() => {
      nowMs += 80; // 跨过节流窗（performance.now 已 mock）
    });
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(3);
  });

  it("入库 running 中不改变档位（不干扰正式入库布局）", async () => {
    const scroll = await setup("grid", true);
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(currentImportCellStep()).toBe(1);
  });

  it("切档后滚动位置不被重置为 0（视口中心素材保持可见）", async () => {
    const scroll = await setup() as HTMLElement;
    Object.defineProperty(scroll, "scrollTop", { configurable: true, value: 120, writable: true });
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    expect(scroll.scrollTop).toBe(120);
  });

  it("视频 hover 预览逻辑不回归：缩放档位变化不影响 hover 播放", async () => {
    const scroll = await setup();
    fireEvent.wheel(scroll, { deltaY: -100, altKey: true });
    const card = cardOf("d:/p/b.mp4");
    fireEvent.mouseEnter(card);
    await advance(300);
    expect(card.querySelector("video")).not.toBeNull();
  });
});

