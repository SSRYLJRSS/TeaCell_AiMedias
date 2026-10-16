import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("@/api/plans", () => ({ getOperation: vi.fn() }));
import {
  useTaskStore,
  importOverall,
  upsertImport,
  clearActiveImportTask,
  trackImportOperation,
} from "@/stores/taskStore";
import type { ImportProgress } from "@/types/import";
import type { OperationSnapshot } from "@/types/plans";
import { getOperation } from "@/api/plans";

beforeEach(() => {
  useTaskStore.setState({ tasks: [], operationSnapshots: {} });
});

afterEach(() => {
  vi.useRealTimers();
});

function ev(partial: Partial<ImportProgress>): ImportProgress {
  return {
    taskId: "t1",
    phase: "scanning",
    phaseCurrent: 0,
    phaseTotal: null,
    imported: 0,
    duplicates: 0,
    failed: 0,
    ...partial,
  };
}

describe("importOverall（阶段权重 → 整体进度）", () => {
  it("queued 无权重 → 不确定", () => {
    expect(importOverall("queued", 0, null)).toBeNull();
  });

  it("scanning 总量未知 → 不确定进度", () => {
    expect(importOverall("scanning", 5, null)).toBeNull();
  });

  it("hashing 按权重计算整体进度", () => {
    // scanning(0.05) 已完成 + hashing(0.25) * 10/100
    expect(importOverall("hashing", 10, 100)).toBeCloseTo(0.05 + 0.25 * 0.1, 5);
  });

  it("processing/previewing 按权重累加", () => {
    // 0.05+0.25 + 0.5 * 0.4 = 0.5
    expect(importOverall("processing", 200, 500)).toBeCloseTo(0.5, 5);
    // 0.05+0.25+0.5 + 0.2 * 0.5 = 0.9
    expect(importOverall("previewing", 50, 100)).toBeCloseTo(0.9, 5);
  });

  it("done → 1", () => {
    expect(importOverall("done", 0, null)).toBe(1);
  });

  it("阶段切换不会造成整体进度倒退", () => {
    const processing = importOverall("processing", 200, 500);
    const next = importOverall("previewing", 0, 100);
    expect(next).toBeGreaterThanOrEqual(processing!);
  });
});

describe("全局任务条（taskStore）", () => {
  it("入库任务按 taskId 去重，只产生一条展示条", () => {
    upsertImport(ev({ phase: "scanning", phaseCurrent: 3, phaseTotal: null }));
    upsertImport(ev({ phase: "hashing", phaseCurrent: 10, phaseTotal: 100 }));
    const tasks = useTaskStore.getState().tasks;
    expect(tasks).toHaveLength(1);
    expect(tasks[0].id).toBe("t1");
    expect(tasks[0].overall).toBeCloseTo(0.05 + 0.25 * 0.1, 5);
    expect(tasks[0].importProgress).toEqual({
      phase: "hashing",
      phaseCurrent: 10,
      phaseTotal: 100,
      file: undefined,
      imported: 0,
      duplicates: 0,
      failed: 0,
    });
  });

  it("旧 taskId 事件不会污染新任务", () => {
    upsertImport(ev({ taskId: "old", phase: "hashing", phaseCurrent: 99, phaseTotal: 100 }));
    upsertImport(ev({ taskId: "new", phase: "queued", phaseCurrent: 0, phaseTotal: null }));
    const ids = useTaskStore.getState().tasks.map((t) => t.id);
    expect(ids).toContain("new");
    expect(ids).toContain("old");
    // 新任务显示不确定（queued），不被旧任务整体进度覆盖
    const newTask = useTaskStore.getState().tasks.find((t) => t.id === "new")!;
    expect(newTask.overall).toBeNull();
  });

  it("失败任务保留文字与状态（done 时错误数 > 0）", () => {
    upsertImport(ev({ phase: "done", phaseCurrent: 100, phaseTotal: 100, imported: 100, duplicates: 0, failed: 2, message: "入库完成" }));
    const task = useTaskStore.getState().tasks.find((t) => t.id === "t1")!;
    expect(task.error).toContain("失败 2");
    expect(task.done).toBe(true);
  });

  it("命令级失败时清理未完成入库任务，不留占位任务", () => {
    upsertImport(ev({ phase: "processing", phaseCurrent: 80, phaseTotal: 100 }));
    expect(useTaskStore.getState().tasks).toHaveLength(1);

    clearActiveImportTask();

    expect(useTaskStore.getState().tasks).toHaveLength(0);
  });

  it("完成入库保留最终进度，下一次新任务开始才替换", () => {
    vi.useFakeTimers();
    upsertImport(ev({ phase: "done", phaseCurrent: 100, phaseTotal: 100, imported: 96, duplicates: 3, failed: 1 }));
    vi.advanceTimersByTime(10_000);

    expect(useTaskStore.getState().tasks).toHaveLength(1);
    expect(useTaskStore.getState().tasks[0].done).toBe(true);

    upsertImport(ev({ taskId: "t2", phase: "queued", phaseCurrent: 0, phaseTotal: null }));
    const tasks = useTaskStore.getState().tasks;
    expect(tasks).toHaveLength(1);
    expect(tasks[0].id).toBe("t2");
    expect(tasks[0].done).toBe(false);
  });

  it("阶段切换不倒退（任务条整体进度单调不减）", () => {
    let prev = -1;
    for (const [phase, current, total] of [["hashing", 0, 100], ["hashing", 100, 100], ["processing", 0, 100], ["processing", 100, 100], ["previewing", 0, 100], ["previewing", 100, 100]] as const) {
      const overall = importOverall(phase as ImportProgress["phase"], current, total!);
      if (overall == null) continue;
      expect(overall).toBeGreaterThanOrEqual(prev);
      prev = overall;
    }
  });

  it("固定计划事件按 operationId 归组，单步 done 不会结束整个操作", () => {
    upsertImport(ev({ taskId: "step-a", operationId: "op-1", phase: "processing", phaseCurrent: 2, phaseTotal: 5 }));
    upsertImport(ev({ taskId: "step-b", operationId: "op-1", phase: "done", phaseCurrent: 5, phaseTotal: 5 }));

    const tasks = useTaskStore.getState().tasks;
    expect(tasks).toHaveLength(1);
    expect(tasks[0].id).toBe("op-1");
    expect(tasks[0].done).toBe(false);
    expect(tasks[0].importProgress?.phase).toBe("done");
  });

  it("持久操作完成态按 operationId 更新并保留失败结果", () => {
    const snapshot: OperationSnapshot = {
      schemaVersion: 1,
      operationId: "op-terminal",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 3,
      state: "partially_succeeded",
      phase: "complete",
      completed: 4,
      failed: 1,
      total: 5,
      canCancel: false,
      canResume: false,
    };
    trackImportOperation(snapshot);

    const task = useTaskStore.getState().tasks[0];
    expect(task.id).toBe("op-terminal");
    expect(task.done).toBe(true);
    expect(task.operationSnapshot).toEqual(snapshot);
    expect(task.error).toContain("部分完成");
    expect(getOperation).not.toHaveBeenCalled();
  });

  it("新任务替换完成条时仍保留旧 operation 的最新持久快照", () => {
    const terminal: OperationSnapshot = {
      schemaVersion: 1,
      operationId: "op-preserved",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 4,
      state: "succeeded",
      phase: "complete",
      completed: 2,
      failed: 0,
      total: 2,
      canCancel: false,
      canResume: false,
    };
    trackImportOperation(terminal);

    upsertImport(ev({ operationId: "op-next", phase: "processing", phaseCurrent: 1, phaseTotal: 3 }));

    expect(useTaskStore.getState().tasks.map((task) => task.id)).toEqual(["op-next"]);
    expect(useTaskStore.getState().operationSnapshots[terminal.operationId]).toEqual(terminal);
  });

  it("已移除终态操作收到迟到导入进度时不复活旧任务卡", () => {
    const terminal: OperationSnapshot = {
      schemaVersion: 1,
      operationId: "op-removed-terminal",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 4,
      state: "succeeded",
      phase: "complete",
      completed: 2,
      failed: 0,
      total: 2,
      canCancel: false,
      canResume: false,
    };
    trackImportOperation(terminal);
    upsertImport(ev({ operationId: "op-next-active", phase: "processing", phaseCurrent: 1, phaseTotal: 3 }));

    upsertImport(ev({ operationId: terminal.operationId, phase: "processing", phaseCurrent: 0, phaseTotal: 2 }));

    expect(useTaskStore.getState().tasks.map((task) => task.id)).toEqual(["op-next-active"]);
    expect(useTaskStore.getState().operationSnapshots[terminal.operationId]).toEqual(terminal);
  });

  it("活动操作只按自己的 operationRef 轮询", async () => {
    vi.useFakeTimers();
    vi.mocked(getOperation).mockResolvedValue({
      schemaVersion: 1,
      operationId: "op-exact",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 2,
      state: "succeeded",
      phase: "complete",
      completed: 1,
      failed: 0,
      total: 1,
      canCancel: false,
      canResume: false,
    });
    trackImportOperation({
      schemaVersion: 1,
      operationId: "op-exact",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 1,
      state: "running",
      phase: "executing",
      completed: 0,
      failed: 0,
      total: 1,
      canCancel: true,
      canResume: false,
    });

    await vi.runOnlyPendingTimersAsync();

    expect(getOperation).toHaveBeenCalledWith("op-exact");
    expect(useTaskStore.getState().tasks[0].operationSnapshot?.state).toBe("succeeded");
  });

  it("恢复后的新快照不被迟到旧终态覆盖，且在途旧查询结束后继续轮询", async () => {
    vi.useFakeTimers();
    vi.mocked(getOperation).mockClear();
    let finishOldRead!: (snapshot: OperationSnapshot) => void;
    vi.mocked(getOperation)
      .mockImplementationOnce(() => new Promise((resolve) => { finishOldRead = resolve; }))
      .mockImplementation(() => new Promise(() => {}));

    const running: OperationSnapshot = {
      schemaVersion: 1,
      operationId: "op-resume-race",
      library: { libraryId: "lib-1", libraryEpoch: "epoch-1" },
      revision: 8,
      state: "running",
      phase: "executing",
      completed: 1,
      failed: 0,
      total: 2,
      canCancel: true,
      canResume: false,
    };
    trackImportOperation(running);
    await vi.advanceTimersByTimeAsync(0);
    expect(getOperation).toHaveBeenCalledTimes(1);

    const resumed: OperationSnapshot = {
      ...running,
      revision: 9,
      state: "queued",
      phase: "ready",
      canCancel: true,
      canResume: false,
    };
    trackImportOperation(resumed);
    await vi.advanceTimersByTimeAsync(0);
    finishOldRead({
      ...running,
      revision: 7,
      state: "interrupted",
      phase: "reconciling",
      canCancel: false,
      canResume: true,
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(useTaskStore.getState().operationSnapshots[resumed.operationId]).toEqual(resumed);
    expect(useTaskStore.getState().tasks[0].operationSnapshot).toEqual(resumed);
    await vi.advanceTimersByTimeAsync(1200);
    expect(getOperation).toHaveBeenCalledTimes(2);
  });
});
