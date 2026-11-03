/**
 * PendingPlansPanel 的命令边界测试（A5-02）。
 *
 * `docs/PROJECT_PLAN.md` §9.1 对 A5-02 的关闭证据要求「测试必须经过实际 UI 数据
 * 入口」，并点名 `src-tauri/tests/mcp_interop.rs` 里那 5 处直接调
 * `plans::approve_plan(db, &gui, ..)` 是缺口。这里把边界推到最外侧：只 mock
 * Tauri 的 `invoke`，`@/api/plans` 走真实实现，于是断言的是真实的命令名与
 * 真实载荷形状（`{ planRef, reviewRef }`），而不是某个被替换过的中间层。
 *
 * 这组用例能抓到的回归：
 * - 命令名被改（前后端只靠字符串对齐，改名不会让 tsc 报错）；
 * - 载荷键名被改（Rust serde 是 camelCase，snake_case 会 400）；
 * - 有人往审批载荷里塞 `approved: true` 之类的「客户端自报批准」字段。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import PendingPlansPanel from "@/components/plans/PendingPlansPanel";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import type { PlanPreview } from "@/types/plans";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

function planOf(over: Partial<PlanPreview> = {}): PlanPreview {
  return {
    schemaVersion: 1,
    planId: "plan_cccccccccccccccc",
    capability: "assets.trash.prepare",
    library: { libraryId: "lib_1", libraryEpoch: "epoch_1" },
    reviewStatus: "pending",
    reviewKind: "ordinary",
    originKind: "mcp",
    originLabel: "本地编辑器",
    itemCount: 2,
    summary: "把 2 个素材移入回收站",
    reviewRef: "review_cccccccccccccccc",
    expiresAtUnixSeconds: Math.floor(Date.now() / 1000) + 600,
    scope: { scopeRef: "selection_cccccccccccccccc" },
    budget: { maxRequests: 0, maxTokens: 0, maxSeconds: 0, maxItems: 2 },
    ...over,
  };
}

beforeEach(() => {
  invoke.mockReset();
  usePendingPlansStore.setState({ open: true, plans: [], loading: false, error: null });
});

describe("待审计划面板的命令边界", () => {
  it("首载调用的是真实的 list_pending_plans 命令，且不带参数", async () => {
    invoke.mockResolvedValue([planOf()]);
    render(<PendingPlansPanel />);

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("list_pending_plans", undefined));
    expect(await screen.findByText(/把 2 个素材移入回收站/)).toBeTruthy();
  });

  it("确认走真实 approve_plan 命令，载荷只有 planRef 与 reviewRef", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_pending_plans") return [planOf()];
      return { schemaVersion: 1, planId: "plan_cccccccccccccccc", reviewStatus: "approved" };
    });
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("确认此计划"));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("approve_plan", {
        planRef: "plan_cccccccccccccccc",
        reviewRef: "review_cccccccccccccccc",
      }),
    );
    // 载荷键必须是这两个，多一个 approved/principal 之类都会被后端按
    // INVALID_INPUT 拒绝，也会让「客户端不能自报批准」这条不变量失去证据。
    const approval = invoke.mock.calls.find(([cmd]) => cmd === "approve_plan");
    expect(Object.keys(approval?.[1] ?? {}).sort()).toEqual(["planRef", "reviewRef"]);
  });

  it("拒绝走真实 reject_plan 命令，载荷同样只有两个 ref", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_pending_plans") return [planOf()];
      return { schemaVersion: 1, planId: "plan_cccccccccccccccc", reviewStatus: "invalidated" };
    });
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("拒绝"));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("reject_plan", {
        planRef: "plan_cccccccccccccccc",
        reviewRef: "review_cccccccccccccccc",
      }),
    );
    const rejection = invoke.mock.calls.find(([cmd]) => cmd === "reject_plan");
    expect(Object.keys(rejection?.[1] ?? {}).sort()).toEqual(["planRef", "reviewRef"]);
  });

  it("后端拒绝审批时把业务错误原文显示出来，不吞成「成功」", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "list_pending_plans") return [planOf()];
      throw new Error("这份计划已经过期，请让客户端重新准备");
    });
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("确认此计划"));

    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("已经过期"),
    );
  });
});
