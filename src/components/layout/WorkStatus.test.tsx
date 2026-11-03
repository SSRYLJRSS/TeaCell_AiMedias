/**
 * WorkStatus 工作状态区测试（PRD 第 8 节 / A3-05）。
 *
 * 覆盖三个 A3-05 修复点：
 *  - 轮播游标渲染期夹取（`clampWorkStatusIndex`）：列表缩短时不会出现 “3/2” 越界序号；
 *  - 失败语义色：失败条目用 `--color-danger`，与删除/失败/不可逆风险同一语义，
 *    不再用进度橙 `--color-status`；
 *  - 进行中才用脉冲点，失败不用动画。
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import WorkStatus, { clampWorkStatusIndex, projectWorkStatus } from "@/components/layout/WorkStatus";
import { useTaskStore } from "@/stores/taskStore";
import type { TaskItem } from "@/stores/taskStore";

function task(over: Partial<TaskItem>): TaskItem {
  return { id: "t1", kind: "import", label: "入库", overall: 0.5, detail: "正在入库", ...over };
}

describe("clampWorkStatusIndex", () => {
  it("游标落在列表长度之外时夹回最后一条", () => {
    // 三条变两条：旧游标 2 必须夹到 1，否则序号会显示成 3/2
    expect(clampWorkStatusIndex(2, 2)).toBe(1);
    expect(clampWorkStatusIndex(9, 3)).toBe(2);
  });

  it("合法游标原样返回", () => {
    expect(clampWorkStatusIndex(0, 3)).toBe(0);
    expect(clampWorkStatusIndex(2, 3)).toBe(2);
  });

  it("空列表与非法数值都回落到 0，保证 entries[current] 永不为空", () => {
    expect(clampWorkStatusIndex(0, 0)).toBe(0);
    expect(clampWorkStatusIndex(5, 0)).toBe(0);
    expect(clampWorkStatusIndex(-1, 3)).toBe(0);
    expect(clampWorkStatusIndex(Number.NaN, 3)).toBe(0);
    // 非有限值不做“猜意图”的夹取，统一回到第一条（Fail safe）
    expect(clampWorkStatusIndex(Number.POSITIVE_INFINITY, 3)).toBe(0);
  });
});

describe("WorkStatus（PRD 第 8 节 / A3-05）", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
  });

  it("无任务时说明为空而不是假装有进度", () => {
    render(<WorkStatus />);
    expect(screen.getByTestId("work-status").textContent).toContain("暂无进行中的任务");
  });

  it("任务列表缩短后序号不越界，序号与当前条目标签保持一致", () => {
    useTaskStore.setState({
      tasks: [task({ id: "a", label: "任务甲" }), task({ id: "b", label: "任务乙" }), task({ id: "c", label: "任务丙" })],
    });
    render(<WorkStatus />);
    const status = screen.getByTestId("work-status");
    expect(status.textContent).toContain("1/3");

    // 手动切到最后一条
    act(() => {
      fireEvent.wheel(status, { deltaY: 120 });
      fireEvent.wheel(status, { deltaY: 120 });
    });
    expect(screen.getByTestId("work-status").textContent).toContain("3/3");

    // 丙被移除：总数变 2，序号必须跟着夹取，不允许出现 3/2
    act(() => {
      useTaskStore.setState({
        tasks: [task({ id: "a", label: "任务甲" }), task({ id: "b", label: "任务乙" })],
      });
    });
    const after = screen.getByTestId("work-status").textContent ?? "";
    expect(after).not.toContain("3/2");
    expect(after).toContain("2/2");
    expect(after).toContain("任务乙");
  });

  it("失败条目用危险语义色，不用进度橙", () => {
    useTaskStore.setState({ tasks: [task({ id: "a", label: "导入失败", error: "源文件不可读" })] });
    render(<WorkStatus />);
    const status = screen.getByTestId("work-status");
    // 文案与点都用 danger；--color-status（进度橙）不再出现在失败态
    expect(status.innerHTML).toContain("var(--color-danger)");
    expect(status.innerHTML).not.toContain("bg-[var(--color-status)]");
  });

  it("进行中使用脉冲点，完成态不脉冲", () => {
    useTaskStore.setState({ tasks: [task({ id: "a", label: "进行中任务" })] });
    const running = render(<WorkStatus />);
    expect(screen.getByTestId("work-status").innerHTML).toContain("animate-pulse");
    running.unmount();

    useTaskStore.setState({ tasks: [task({ id: "a", label: "已完成任务", done: true, overall: 1 })] });
    render(<WorkStatus />);
    expect(screen.getByTestId("work-status").innerHTML).not.toContain("animate-pulse");
  });

  it("轮播顺序为 进行中 → 未完成 → 已完成", () => {
    const projected = projectWorkStatus([
      task({ id: "done", label: "已完成", done: true, overall: 1 }),
      task({ id: "failed", label: "未完成", error: "中断" }),
      task({ id: "run", label: "进行中" }),
    ]);
    expect(projected.map((e) => e.id)).toEqual(["run", "failed", "done"]);
    expect(projected.map((e) => e.tone)).toEqual(["running", "failed", "done"]);
  });
});

describe("WorkStatus 轮播行为（M-13 长中文与多任务轮播）", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
  });

  function threeTasks() {
    useTaskStore.setState({
      tasks: [task({ id: "a", label: "入库甲" }), task({ id: "b", label: "入库乙" }), task({ id: "c", label: "入库丙" })],
    });
  }

  it("同时有三个任务时按 6 秒缓慢自动轮播，序号循环回到第一条（M-13 轮播）", () => {
    vi.useFakeTimers();
    try {
      threeTasks();
      render(<WorkStatus />);
      expect(screen.getByTestId("work-status").textContent).toContain("1/3");
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("2/3");
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("3/3");
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("1/3");
    } finally {
      vi.useRealTimers();
    }
  });

  it("鼠标悬停时暂停自动轮播，移开后恢复（M-13 悬停暂停）", () => {
    vi.useFakeTimers();
    try {
      threeTasks();
      render(<WorkStatus />);
      const box = screen.getByTestId("work-status");
      fireEvent.mouseEnter(box);
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("1/3");
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("1/3");
      fireEvent.mouseLeave(box);
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("2/3");
    } finally {
      vi.useRealTimers();
    }
  });

  it("方向键可手动切换并在 8 秒保持期内不被自动轮播抢走（M-13 手动滚动）", () => {
    vi.useFakeTimers();
    try {
      threeTasks();
      render(<WorkStatus />);
      const box = screen.getByTestId("work-status");
      fireEvent.keyDown(box, { key: "ArrowRight" });
      expect(screen.getByTestId("work-status").textContent).toContain("2/3");
      // 手动切换后的保持期内，自动轮播不得把画面切走
      act(() => {
        vi.advanceTimersByTime(6000);
      });
      expect(screen.getByTestId("work-status").textContent).toContain("2/3");
      fireEvent.keyDown(box, { key: "ArrowLeft" });
      expect(screen.getByTestId("work-status").textContent).toContain("1/3");
      fireEvent.keyDown(box, { key: "ArrowLeft" });
      expect(screen.getByTestId("work-status").textContent).toContain("3/3");
    } finally {
      vi.useRealTimers();
    }
  });
});
