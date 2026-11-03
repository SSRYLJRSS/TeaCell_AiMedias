/**
 * 任务卡上的「保存为常用流程」入口（A4-07）。
 *
 * 只验证入口本身的行为契约：
 * - 只在任务结束后出现（还在跑的步骤不是「已理解的业务步骤」）；
 * - 点击后由 store 调后端生成草稿，而不是前端自己拼步骤；
 * - 生成失败要把原因摊开，不能点了没反应。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { OperationCard } from "@/components/assistant/AgentCards";
import type { OperationSnapshot } from "@/types/plans";

vi.mock("@/api/workflows", () => ({
  draftWorkflowFromOperation: vi.fn(),
}));

import { draftWorkflowFromOperation } from "@/api/workflows";
import { useWorkflowPanelStore } from "@/stores/workflowPanelStore";

function snapshotOf(over: Partial<OperationSnapshot> = {}): OperationSnapshot {
  return {
    operationId: "operation_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    capability: "assets.trash.submit",
    state: "succeeded",
    phase: "complete",
    revision: 3,
    completed: 2,
    failed: 0,
    total: 2,
    errorCode: null,
    canCancel: false,
    canResume: false,
    updatedAtUnixMs: 1_700_000_000_000,
    ...over,
  } as OperationSnapshot;
}

beforeEach(() => {
  vi.mocked(draftWorkflowFromOperation).mockReset();
  useWorkflowPanelStore.setState({ open: false, draft: null, draftingFrom: null, error: null });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("OperationCard 的保存为常用流程入口", () => {
  it("任务结束后可以点，草稿由后端生成", async () => {
    vi.mocked(draftWorkflowFromOperation).mockResolvedValue({
      sourceOperationRef: "operation_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      name: "移入回收站",
      description: "",
      steps: [{ capability: "assets.trash.submit", dependsOn: null }],
      parameters: [],
      requires: ["移入回收站"],
      runtimeInputs: [],
      notes: [],
      excluded: [],
    });

    render(<OperationCard snapshot={snapshotOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "保存为常用流程" }));

    await waitFor(() =>
      expect(draftWorkflowFromOperation).toHaveBeenCalledWith(
        "operation_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      ),
    );
    await waitFor(() => expect(useWorkflowPanelStore.getState().open).toBe(true));
    expect(useWorkflowPanelStore.getState().draft?.name).toBe("移入回收站");
  });

  it("还在执行的任务不提供这个入口", () => {
    render(<OperationCard snapshot={snapshotOf({ state: "running", phase: "executing", canCancel: true })} />);
    expect(screen.queryByRole("button", { name: "保存为常用流程" })).toBeNull();
    expect(screen.getByRole("button", { name: "取消任务" })).toBeTruthy();
  });

  it("生成草稿失败时把原因告诉用户，而不是静默无响应", async () => {
    vi.mocked(draftWorkflowFromOperation).mockRejectedValue(
      new Error("这次任务没有可复用的业务步骤"),
    );

    render(<OperationCard snapshot={snapshotOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "保存为常用流程" }));

    await waitFor(() =>
      expect(useWorkflowPanelStore.getState().error).toBe("这次任务没有可复用的业务步骤"),
    );
    // 面板仍会打开：错误在面板里显示给用户
    expect(useWorkflowPanelStore.getState().open).toBe(true);
    expect(useWorkflowPanelStore.getState().draft).toBeNull();
  });
});
