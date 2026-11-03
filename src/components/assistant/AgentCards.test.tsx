/**
 * 任务卡的进度语义（R28「未知总量不编百分比」）。
 *
 * 契约：任务卡只展示后端快照里的真实进度。总量未知（`total` 缺省或为 0）时必须
 * 渲染不确定进度条，已完成数只写「已完成 N」，不能自己补一个百分比或 `N / 总数`；
 * 只有拿到真实总量时才显示 `已完成 N / total` 与由它算出的进度。
 */
import { describe, expect, it, vi, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { OperationCard } from "@/components/assistant/AgentCards";
import type { OperationSnapshot } from "@/types/plans";

vi.mock("@/api/operations", () => ({
  startOperation: vi.fn(),
  cancelOperation: vi.fn(),
  resumeOperation: vi.fn(),
  listOperationRecovery: vi.fn(),
}));

function snapshotOf(over: Partial<OperationSnapshot> = {}): OperationSnapshot {
  return {
    operationId: "operation_dddddddddddddddddddddddddddddddd",
    capability: "assets.trash.submit",
    state: "running",
    phase: "executing",
    revision: 2,
    completed: 3,
    failed: 0,
    total: 3,
    errorCode: null,
    canCancel: true,
    canResume: false,
    updatedAtUnixMs: 1_700_000_000_000,
    ...over,
  } as OperationSnapshot;
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("OperationCard 在总量未知时不编造百分比（R28）", () => {
  it("total 缺省时渲染不确定进度条，已完成数只写「已完成 N」", () => {
    render(<OperationCard snapshot={snapshotOf({ total: undefined, completed: 3 })} />);

    const bar = screen.getByRole("progressbar");
    // 不确定进度条没有 aria-valuenow：界面不声称知道 3 项占全部的百分之几。
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(bar).toHaveAttribute("aria-valuemin", "0");
    expect(bar).toHaveAttribute("aria-valuemax", "100");

    const card = screen.getByLabelText("任务：执行中");
    expect(card.textContent).toContain("已完成 3");
    // 不能出现编造的「50%」或「3 / 10」这类文案。
    expect(card.textContent).not.toMatch(/\d+\s*%/);
    expect(card.textContent).not.toMatch(/\d+\s*\/\s*\d+/);
    expect(screen.queryByText(/已完成\s*3\s*\/\s*\d+/)).toBeNull();
  });

  it("total 为 0（总量未知的另一种快照）同样不编造百分比", () => {
    render(<OperationCard snapshot={snapshotOf({ total: 0, completed: 0 })} />);

    const bar = screen.getByRole("progressbar");
    expect(bar).not.toHaveAttribute("aria-valuenow");

    const card = screen.getByLabelText("任务：执行中");
    expect(card.textContent).toContain("已完成 0");
    expect(card.textContent).not.toMatch(/\d+\s*%/);
    // 快照显式给了总量 0，分母就只能用这个 0，不能被替换成任何编造的总数。
    expect(card.textContent?.match(/\d+\s*\/\s*\d+/g)).toEqual(["0 / 0"]);
  });

  it("只有拿到真实总量时才显示「已完成 N / total」与真实进度", () => {
    // 对照组：确定进度仍然按快照算，证明上面的「不出现」不是靠一律不显示换来的。
    render(<OperationCard snapshot={snapshotOf({ total: 10, completed: 1 })} />);

    const card = screen.getByLabelText("任务：执行中");
    expect(card.textContent).toContain("已完成 1 / 10");
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "10");
  });
});
