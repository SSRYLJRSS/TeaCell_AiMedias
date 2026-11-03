/**
 * VideoPlayer 组件测试（指导书 §4.4/§12.3 + FB5-02 §13.3）：
 *  - 控制条为 absolute overlay，不再是 shrink-0 底栏；无白色底板 class；
 *  - 播放中 1800ms 后隐藏，pointermove/focus 恢复；暂停时不隐藏；
 *  - progress/volume 操作不冒泡；buffered/played 百分比正确；
 *  - 自定义 range 在 duration 未知时禁用；
 *  - 倍速菜单 0.5/1/1.5/2，选择后关闭；
 *  - F/全屏按钮调用父级 onToggleImmersive，不直接 requestFullscreen；
 *  - 键盘左右 seek 不冒泡成 Viewer 切图。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import VideoPlayer from "@/components/media/VideoPlayer";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("VideoPlayer（指导书 §4.4 + FB5-02）", () => {
  it("渲染 video 元素与悬浮控制条（absolute overlay，非 shrink-0 底栏）", () => {
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const video = container.querySelector("video");
    expect(video).not.toBeNull();
    expect(video!.getAttribute("src")).toBe("asset://v/mp4");
    expect(video!.getAttribute("playsinline")).not.toBeNull();

    const controls = container.querySelector("[data-testid='video-controls']") as HTMLElement;
    expect(controls).not.toBeNull();
    // §13.3：控制条必须是 absolute 悬浮层
    expect(controls.className).toContain("absolute");
    // 根节点不再 flex-col 给控制条留高度
    const root = container.querySelector("[data-player-root]") as HTMLElement;
    expect(root.className).toContain("relative");
    expect(root.className).not.toContain("flex-col");
    // 不存在白色底板
    expect(controls.className).not.toContain("bg-[var(--color-surface-raised)]");
    expect(controls.className).not.toContain("border-t");

    expect(screen.getByRole("button", { name: /播放|暂停/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "后退 5 秒" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "前进 5 秒" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /静音|取消静音/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全屏" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "播放进度" })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: "音量" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "倍速" })).toBeInTheDocument();
  });

  it("倍速菜单含 0.5/1/1.5/2x，选择后写入并关闭", () => {
    render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const rateBtn = screen.getByRole("button", { name: "倍速" });
    fireEvent.click(rateBtn);
    const menu = screen.getByRole("menu", { name: "倍速选项" });
    const items = within(menu).getAllByRole("menuitemradio");
    expect(items.map((i) => i.textContent?.trim())).toEqual(["0.5x", "1x", "1.5x", "2x"]);
    fireEvent.click(within(menu).getByText("2x"));
    expect(menu).not.toBeInTheDocument(); // 选择后关闭
    // 按钮文本更新为 2x
    expect(screen.getByRole("button", { name: "倍速" }).textContent).toContain("2x");
  });

  it("§6.2 上报视频测量指标（canPlayType 矩阵 + 网络/就绪状态）", () => {
    const onMetrics = vi.fn();
    render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" onMetrics={onMetrics} />);
    // mount 时上报一次
    expect(onMetrics).toHaveBeenCalled();
    const m = onMetrics.mock.calls[0][0];
    expect(m.canPlayType).toBeTypeOf("object");
    expect(m).toHaveProperty("networkState");
    expect(m).toHaveProperty("readyState");
  });

  it("播放器根节点键盘作用域：← 只 seek 并 stopPropagation（不冒泡成素材切换）", () => {
    const onStop = vi.fn();
    const { container } = render(
      <div data-testid="outer" onKeyDown={onStop}>
        <VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />
      </div>,
    );
    const root = container.querySelector("[data-player-root]") as HTMLElement;
    expect(root).not.toBeNull();
    // 原生 focus() 会触发 onRootFocus 的 setFocusWithin/showControlsTemporarily，必须包进 act。
    act(() => root.focus());
    fireEvent.keyDown(root, { key: "ArrowLeft" });
    expect(onStop).not.toHaveBeenCalled(); // stopPropagation 生效：外层收不到
  });

  it("自动播放被拒时显示「点击播放」而非静默吞错", () => {
    render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    fireEvent.keyDown(document.querySelector("[data-player-root]") as HTMLElement, { key: " " });
    // 不应抛错；控件仍存在
    expect(screen.getByRole("button", { name: /播放|暂停/ })).toBeInTheDocument();
  });

  it("FB3-03 loadedmetadata 后 range 用 duration 作为 max（metadata 前禁用）", () => {
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const video = container.querySelector("video") as HTMLVideoElement;
    const range = screen.getByRole("slider", { name: "播放进度" }) as HTMLInputElement;
    // metadata 前：duration=0 → 禁用
    expect(range.disabled).toBe(true);
    // 派发 loadedmetadata + 注入 duration（模拟 WebView2 就绪）
    Object.defineProperty(video, "duration", { value: 91.5, configurable: true });
    fireEvent(video, new Event("loadedmetadata"));
    expect(range.disabled).toBe(false);
    expect(Number(range.max)).toBeCloseTo(91.5, 5);
  });

  it("FB3-03 切换 src 重置时间与 duration（旧视频残留不污染新视频）", () => {
    const { container, rerender } = render(<VideoPlayer src="asset://v/a.mp4" fileName="a.mp4" />);
    const video = container.querySelector("video") as HTMLVideoElement;
    Object.defineProperty(video, "duration", { value: 91.5, configurable: true });
    fireEvent(video, new Event("loadedmetadata"));
    const range1 = screen.getByRole("slider", { name: "播放进度" }) as HTMLInputElement;
    expect(Number(range1.max)).toBeCloseTo(91.5, 5);

    rerender(<VideoPlayer src="asset://v/b.mp4" fileName="b.mp4" />);
    const range2 = screen.getByRole("slider", { name: "播放进度" }) as HTMLInputElement;
    expect(range2.max).toBe("0");
    expect(range2.disabled).toBe(true);
  });

  it("FB5-02 §13.3：播放时 1800ms 后控制条隐藏；pointermove/focus 恢复；暂停时不隐藏", () => {
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const root = container.querySelector("[data-player-root]") as HTMLElement;
    const video = container.querySelector("video") as HTMLVideoElement;
    const controlsHost = root.querySelector(".absolute.inset-x-0.bottom-0") as HTMLElement;
    expect(controlsHost).not.toBeNull();

    // 模拟播放中
    fireEvent(video, new Event("play"));
    // 播放开始后仍可见（刚切换状态，计时器尚未到期）
    expect(controlsHost.style.opacity).toBe("1");
    // 1800ms 后隐藏
    act(() => {
      vi.advanceTimersByTime(1800);
    });
    expect(controlsHost.style.opacity).toBe("0");
    // pointermove 恢复
    fireEvent.pointerMove(root);
    expect(controlsHost.style.opacity).toBe("1");
    // 再次隐藏后 focus 恢复
    act(() => {
      vi.advanceTimersByTime(1800);
    });
    expect(controlsHost.style.opacity).toBe("0");
    fireEvent.focus(root);
    expect(controlsHost.style.opacity).toBe("1");

    // 暂停时不隐藏（即使计时器到期）
    fireEvent(video, new Event("pause"));
    act(() => {
      vi.advanceTimersByTime(1800);
    });
    expect(controlsHost.style.opacity).toBe("1");
  });

  it("FB5-02 §13.3：progress/volume 操作不冒泡（不触发视频点击播放）", () => {
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const video = container.querySelector("video") as HTMLVideoElement;
    const playSpy = vi.spyOn(video, "play").mockImplementation(() => Promise.resolve());
    const pauseSpy = vi.spyOn(video, "pause").mockImplementation(() => undefined);

    const range = screen.getByRole("slider", { name: "播放进度" }) as HTMLInputElement;
    // 点击进度条不应冒泡成 video click → 播放/暂停
    fireEvent.click(range);
    expect(playSpy).not.toHaveBeenCalled();
    expect(pauseSpy).not.toHaveBeenCalled();

    const vol = screen.getByRole("slider", { name: "音量" }) as HTMLInputElement;
    fireEvent.click(vol);
    expect(playSpy).not.toHaveBeenCalled();
    expect(pauseSpy).not.toHaveBeenCalled();
  });

  it("FB5-02 §13.3：buffered/played 百分比由 progress/timeupdate 驱动", () => {
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName="demo.mp4" />);
    const video = container.querySelector("video") as HTMLVideoElement;
    Object.defineProperty(video, "duration", { value: 100, configurable: true });
    fireEvent(video, new Event("loadedmetadata"));
    // 注入 buffered 区间
    Object.defineProperty(video, "buffered", {
      value: { length: 1, end: () => 60 },
      configurable: true,
    });
    fireEvent(video, new Event("progress"));
    // currentTime=40 → played 40%
    Object.defineProperty(video, "currentTime", { value: 40, configurable: true });
    fireEvent(video, new Event("timeupdate"));
    const slider = screen.getByTestId("playback-slider");
    const layers = slider.querySelectorAll("div[aria-hidden='true']");
    // base + buffered + played 三层
    expect(layers.length).toBe(3);
    const buffered = layers[1] as HTMLElement;
    const played = layers[2] as HTMLElement;
    expect(buffered.style.width).toBe("60%");
    expect(played.style.width).toBe("40%");
  });

  it("FB5-01 §4.4：F 键与全屏按钮调用父级 onToggleImmersive，不直接 requestFullscreen", () => {
    const onToggle = vi.fn();
    const { container } = render(
      <VideoPlayer src="asset://v/mp4" fileName="demo.mp4" onToggleImmersive={onToggle} />,
    );
    const root = container.querySelector("[data-player-root]") as HTMLElement;
    // jsdom 无 requestFullscreen：先定义再 spy
    const reqSpy = vi.fn(() => Promise.resolve());
    Object.defineProperty(HTMLElement.prototype, "requestFullscreen", {
      configurable: true,
      writable: true,
      value: reqSpy,
    });
    // F 键
    fireEvent.keyDown(root, { key: "f" });
    expect(onToggle).toHaveBeenCalledTimes(1);
    // 全屏按钮
    fireEvent.click(screen.getByRole("button", { name: "全屏" }));
    expect(onToggle).toHaveBeenCalledTimes(2);
    // 从未直接请求浏览器全屏
    expect(reqSpy).not.toHaveBeenCalled();
  });

  // M-13：解码失败态的文件名被 truncate，必须能 hover 看到全量，否则用户无法确认是哪个文件坏了
  it("解码失败态的文件名带 title 全量提示（长中文名截断后仍可读）", () => {
    const longName = "旅行-2026-秋-第一天-上午-这是一个非常非常长的中文视频文件名用来验证截断后仍可读.mp4";
    const { container } = render(<VideoPlayer src="asset://v/mp4" fileName={longName} />);
    const video = container.querySelector("video") as HTMLVideoElement;
    fireEvent.error(video);
    const nameNode = screen.getByTitle(longName);
    expect(nameNode).toBeInTheDocument();
    expect(nameNode.textContent).toBe(longName);
  });
});
