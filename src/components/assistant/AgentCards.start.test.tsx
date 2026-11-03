/**
 * 任务卡上的「开始执行」入口（A4-02）。
 *
 * 背景：批准与提交只写库，Pi/Agent 提交的每一个写操作都停在 `queued`。
 * 这条测试守的是「显式启动」这一段：
 * - `queued` 必须出现「开始执行」，并且不能同时出现「取消任务」以外的歧义按钮；
 * - 点击后由 store 调后端命令，前端不自造执行路径、不猜测权限；
 * - 后端拒绝时把原因摊开，不能点了没反应。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { OperationCard } from "@/components/assistant/AgentCards";
import type { OperationSnapshot } from "@/types/plans";

vi.mock("@/api/operations", () => ({
  startOperation: vi.fn(),
  cancelOperation: vi.fn(),
  resumeOperation: vi.fn(),
  listOperationRecovery: vi.fn(),
}));

import { startOperation, cancelOperation } from "@/api/operations";

function snapshotOf(over: Partial<OperationSnapshot> = {}): OperationSnapshot {
  return {
    operationId: "operation_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    capability: "assets.move.submit",
    state: "queued",
    phase: "ready",
    revision: 1,
    completed: 0,
    failed: 0,
    total: 3,
    errorCode: null,
    canCancel: true,
    canResume: false,
    updatedAtUnixMs: 1_700_000_000_000,
    ...over,
  } as OperationSnapshot;
}

beforeEach(() => {
  vi.mocked(startOperation).mockReset();
  vi.mocked(cancelOperation).mockReset();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("OperationCard 的开始执行入口", () => {
  it("待执行的任务给出开始入口，并说明现在还没有改动任何文件", () => {
    render(<OperationCard snapshot={snapshotOf()} />);

    expect(screen.getByRole("button", { name: "开始执行" })).toBeTruthy();
    expect(screen.getByText(/点「开始执行」后才会真正修改文件/)).toBeTruthy();
  });

  it("点击后按操作 ID 调用后端命令，执行结果回写进 store", async () => {
    vi.mocked(startOperation).mockResolvedValue(
      snapshotOf({ state: "running", phase: "executing", revision: 2 }),
    );

    render(<OperationCard snapshot={snapshotOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "开始执行" }));

    await waitFor(() =>
      expect(startOperation).toHaveBeenCalledWith("operation_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );
  });

  it("后端拒绝执行时把原因显示出来，而不是静默无响应", async () => {
    vi.mocked(startOperation).mockRejectedValue(new Error("队列里还有更早的任务没有开始"));

    render(<OperationCard snapshot={snapshotOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "开始执行" }));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("队列里还有更早的任务"));
  });

  it("排队中的任务同时给出开始与取消入口，取消按操作 ID 只调用一次后端命令", async () => {
    // R25：长写任务默认停在队列里，卡片要同时说清「等待你开始」和「可以取消」。
    vi.mocked(cancelOperation).mockResolvedValue(
      snapshotOf({ state: "cancelled", phase: "complete", canCancel: false }),
    );

    render(<OperationCard snapshot={snapshotOf()} />);

    // 等待状态可见：不是失败，也不是已经在改文件。
    expect(screen.getByText("任务 · 排队中")).toBeTruthy();
    expect(screen.getByText(/已提交，等待你开始执行/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "开始执行" })).toBeTruthy();

    const cancel = screen.getByRole("button", { name: "取消任务" });
    fireEvent.click(cancel);

    await waitFor(() => expect(cancelOperation).toHaveBeenCalledTimes(1));
    expect(cancelOperation).toHaveBeenCalledWith("operation_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    // 取消不等于开始：排队中的任务不会因为点取消就被悄悄启动。
    expect(startOperation).not.toHaveBeenCalled();
  });

  it("已经结束的任务不再提供开始入口", () => {
    render(
      <OperationCard
        snapshot={snapshotOf({ state: "succeeded", phase: "complete", completed: 3, canCancel: false })}
      />,
    );

    expect(screen.queryByRole("button", { name: "开始执行" })).toBeNull();
    expect(screen.getByText("任务已结束")).toBeTruthy();
  });
});
