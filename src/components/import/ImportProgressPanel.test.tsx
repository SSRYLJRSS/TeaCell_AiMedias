/**
 * 入库进度面板的阶段文案与百分比（A6-04 复核发现的状态显示缺陷回归）。
 *
 * 缺陷背景：终态（`partially_succeeded` / `outcome_unknown` 等）可能既拿不到总量，
 * 也没有 `succeeded` 的满进度，于是 `overall == null`。原实现把这当作「还没算出来」，
 * 继续显示「准备中」并渲染不确定进度条——用户看到的是一个已经结束、结果还不明的
 * 任务在无限转圈，无法判断它到底停没停。
 */
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import ImportProgressPanel from "@/components/import/ImportProgressPanel";
import type { TaskItem } from "@/stores/taskStore";
import type { OperationSnapshot } from "@/types/plans";

function snapshot(state: OperationSnapshot["state"], overrides: Partial<OperationSnapshot> = {}): OperationSnapshot {
  return {
    operationId: "operation_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    planId: "plan_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    capability: "imports.submit",
    state,
    revision: 1,
    completed: 0,
    total: null,
    failed: 0,
    errorCode: null,
    phase: null,
    ...overrides,
  } as OperationSnapshot;
}

function taskItem(overrides: Partial<TaskItem>): TaskItem {
  return {
    id: "operation_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    kind: "import",
    label: "入库",
    overall: null,
    done: true,
    ...overrides,
  };
}

function stateBadge() {
  const section = screen.getByRole("region", { name: "入库进度" });
  return section.querySelector("span")?.textContent ?? "";
}

describe("ImportProgressPanel 的终态不得继续显示「准备中」", () => {
  it("结果未知的已结束任务显示「结果待核对」，不再假装还在准备", () => {
    render(
      <ImportProgressPanel
        task={taskItem({ detail: "结果待核对，请先恢复或核对", operationSnapshot: snapshot("outcome_unknown") })}
        starting={false}
      />,
    );
    expect(stateBadge()).toBe("结果待核对");
    expect(stateBadge()).not.toBe("准备中");
    expect(screen.queryByText("准备中")).toBeNull();
  });

  it("部分完成且没有总量时显示「部分完成」，而不是无限进度", () => {
    render(
      <ImportProgressPanel
        task={taskItem({
          detail: "部分完成 · 成功 3 · 失败 1",
          error: "部分完成 · 成功 3 · 失败 1",
          operationSnapshot: snapshot("partially_succeeded", { completed: 3, failed: 1 }),
        })}
        starting={false}
      />,
    );
    expect(stateBadge()).toBe("部分完成");
  });

  it("失败与取消同样按真实终态显示，不落回「准备中」", () => {
    const { rerender } = render(
      <ImportProgressPanel task={taskItem({ detail: "入库失败", operationSnapshot: snapshot("failed") })} starting={false} />,
    );
    expect(stateBadge()).toBe("失败");
    rerender(
      <ImportProgressPanel task={taskItem({ detail: "已取消", operationSnapshot: snapshot("cancelled") })} starting={false} />,
    );
    expect(stateBadge()).toBe("已取消");
    rerender(
      <ImportProgressPanel task={taskItem({ detail: "应用关闭时中断，可恢复", operationSnapshot: snapshot("interrupted") })} starting={false} />,
    );
    expect(stateBadge()).toBe("已中断，可恢复");
  });

  it("成功结束时显示百分比，不显示终态词", () => {
    render(
      <ImportProgressPanel
        task={taskItem({ overall: 1, detail: "入库完成", operationSnapshot: snapshot("succeeded", { completed: 5, total: 5 }) })}
        starting={false}
      />,
    );
    expect(stateBadge()).toBe("100%");
  });

  it("真的在跑且没有总量时仍然显示「准备中」，不确定进度条保留", () => {
    render(
      <ImportProgressPanel
        task={taskItem({ done: false, detail: "正在入库", operationSnapshot: snapshot("running") })}
        starting={false}
      />,
    );
    expect(stateBadge()).toBe("准备中");
    const section = screen.getByRole("region", { name: "入库进度" });
    expect(section.getAttribute("data-state")).toBe("running");
  });

  it("一次都还没开始时显示「等待入库」，不受终态逻辑影响", () => {
    render(<ImportProgressPanel starting={false} />);
    expect(stateBadge()).toBe("等待入库");
    expect(screen.getByRole("region", { name: "入库进度" }).getAttribute("data-state")).toBe("idle");
  });
});
