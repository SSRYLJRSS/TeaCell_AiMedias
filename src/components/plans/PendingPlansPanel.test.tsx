/**
 * PendingPlansPanel 测试（A5-02）。
 *
 * 这一套测的是「真实 UI 数据入口」：`docs/PROJECT_PLAN.md` §9.1 对 A5-02 的关闭
 * 证据要求「测试必须经过实际 UI 数据入口」，而 `src-tauri/tests/mcp_interop.rs`
 * 现有用例都是直接调 `plans::approve_plan(db, &gui, ..)`。所以这里渲染真实面板，
 * 只把 `@/api/plans` 的三个命令函数替换成 vi.fn()，其余（标签、过期文案、
 * permanent_trash 只读判定）全走真实实现。
 *
 * 验收口径：
 * - 外部客户端准备的计划出现在列表里，并如实显示发起方（不是「本机界面」）；
 * - 确认只调 approvePlan(planId, reviewRef)，拒绝只调 rejectPlan，二者都不带
 *   任何「已批准」标记参数；
 * - 动作后必须回读后端列表，不能在本地推断结论；
 * - `reviewKind === "permanent_trash"` 只读：清空回收站有自己的专属确认交互；
 * - 读取失败要给可读错误并清空列表，不能留一份过期列表假装还在等待。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import PendingPlansPanel from "@/components/plans/PendingPlansPanel";
import { usePendingPlansStore } from "@/stores/pendingPlansStore";
import type { PlanPreview } from "@/types/plans";

const mocks = vi.hoisted(() => ({
  listPendingPlans: vi.fn(),
  approvePlan: vi.fn(),
  rejectPlan: vi.fn(),
}));

vi.mock("@/api/plans", () => ({
  listPendingPlans: mocks.listPendingPlans,
  approvePlan: mocks.approvePlan,
  rejectPlan: mocks.rejectPlan,
}));

function planOf(over: Partial<PlanPreview> = {}): PlanPreview {
  return {
    schemaVersion: 1,
    planId: "plan_aaaaaaaaaaaaaaaa",
    capability: "assets.trash.prepare",
    library: { libraryId: "lib_1", libraryEpoch: "epoch_1" },
    reviewStatus: "pending",
    reviewKind: "ordinary",
    originKind: "mcp",
    originLabel: "本地编辑器",
    itemCount: 3,
    summary: "把这 3 个素材移入回收站",
    reviewRef: "review_aaaaaaaaaaaaaaaa",
    expiresAtUnixSeconds: Math.floor(Date.now() / 1000) + 600,
    scope: { scopeRef: "selection_aaaaaaaaaaaaaaaa" },
    budget: { maxRequests: 0, maxTokens: 0, maxSeconds: 0, maxItems: 3 },
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  usePendingPlansStore.setState({ open: true, plans: [], loading: false, error: null });
  mocks.listPendingPlans.mockResolvedValue([planOf()]);
  mocks.approvePlan.mockResolvedValue({
    schemaVersion: 1,
    planId: "plan_aaaaaaaaaaaaaaaa",
    reviewStatus: "approved",
  });
  mocks.rejectPlan.mockResolvedValue({
    schemaVersion: 1,
    planId: "plan_aaaaaaaaaaaaaaaa",
    reviewStatus: "invalidated",
  });
});

describe("待审计划面板", () => {
  it("列出外部客户端准备的计划，并如实显示发起方与范围", async () => {
    render(<PendingPlansPanel />);

    const list = await screen.findByTestId("pending-plans-list");
    await waitFor(() => expect(mocks.listPendingPlans).toHaveBeenCalledTimes(1));

    const row = await waitFor(() => {
      const found = list.querySelector('[data-plan-id="plan_aaaaaaaaaaaaaaaa"]');
      expect(found).toBeTruthy();
      return found as HTMLElement;
    });
    // 发起方必须是外部客户端自己的名字，不能笼统显示「本机界面」。
    expect(row.textContent).toContain("外部客户端：本地编辑器");
    expect(row.textContent).toContain("准备移入回收站");
    expect(row.textContent).toContain("3 项");
    expect(row.getAttribute("data-plan-origin")).toBe("mcp");
  });

  it("确认只调 approvePlan(planId, reviewRef)，不带任何批准标记参数", async () => {
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("确认此计划"));

    await waitFor(() =>
      expect(mocks.approvePlan).toHaveBeenCalledWith("plan_aaaaaaaaaaaaaaaa", "review_aaaaaaaaaaaaaaaa"),
    );
    // 只传两个位置参数：客户端自报的 approved=true 之类必须在结构上无处可放。
    expect(mocks.approvePlan.mock.calls[0]).toHaveLength(2);
    expect(mocks.rejectPlan).not.toHaveBeenCalled();
    // 动作后必须回读后端，不能在本地推断结论。
    await waitFor(() => expect(mocks.listPendingPlans).toHaveBeenCalledTimes(2));
  });

  it("拒绝只调 rejectPlan，且不产生写入动作", async () => {
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("拒绝"));

    await waitFor(() =>
      expect(mocks.rejectPlan).toHaveBeenCalledWith("plan_aaaaaaaaaaaaaaaa", "review_aaaaaaaaaaaaaaaa"),
    );
    expect(mocks.approvePlan).not.toHaveBeenCalled();
  });

  it("永久清空回收站的计划只读，不给确认按钮", async () => {
    mocks.listPendingPlans.mockResolvedValue([
      planOf({
        planId: "plan_bbbbbbbbbbbbbbbb",
        capability: "trash.empty.submit",
        reviewKind: "permanent_trash",
        summary: "清空回收站中的 8 项",
      }),
    ]);
    render(<PendingPlansPanel />);

    expect(await screen.findByText(/永久清空回收站必须在删除对话框里单独确认/)).toBeTruthy();
    expect(screen.queryByText("确认此计划")).toBeNull();
    expect(screen.queryByText("拒绝")).toBeNull();
  });

  it("审批失败时给出后端原因并重读列表", async () => {
    mocks.approvePlan.mockRejectedValue(new Error("计划已过期，请重新准备"));
    render(<PendingPlansPanel />);
    fireEvent.click(await screen.findByText("确认此计划"));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("计划已过期"));
    await waitFor(() => expect(mocks.listPendingPlans).toHaveBeenCalledTimes(2));
  });

  it("读取失败给出可读错误并清空列表，不留过期计划假装还在等待", async () => {
    usePendingPlansStore.setState({ plans: [planOf()] });
    mocks.listPendingPlans.mockRejectedValue(new Error("库已离线"));
    render(<PendingPlansPanel />);

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("库已离线"));
    expect(screen.queryByText("确认此计划")).toBeNull();
  });

  it("没有待审计划时说明是空的，而不是静默空白", async () => {
    mocks.listPendingPlans.mockResolvedValue([]);
    render(<PendingPlansPanel />);

    expect(await screen.findByTestId("pending-plans-empty")).toBeTruthy();
  });
});

describe("待审计划面板嵌入助手右侧栏", () => {
  it("默认（不传 embedded）仍是模态框，关闭按钮收起 store", async () => {
    render(<PendingPlansPanel />);
    expect(screen.getByRole("dialog", { name: "待审计划" })).toBeInTheDocument();
    expect(await screen.findByTestId("pending-plans-list")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(usePendingPlansStore.getState().open).toBe(false);
  });

  it("embedded 时渲染成侧栏内容而不是对话框，且不重复提供关闭按钮", async () => {
    render(<PendingPlansPanel embedded />);

    expect(screen.queryByRole("dialog")).toBeNull();
    // 标题由外壳的选项卡承担，面板自己不再重复一遍
    expect(screen.queryByRole("heading", { name: "待审计划" })).toBeNull();

    // 内容照常：真实列表仍在，不是空壳
    const list = await screen.findByTestId("pending-plans-list");
    expect(list.querySelector('[data-plan-id="plan_aaaaaaaaaaaaaaaa"]')).not.toBeNull();
    // 「刷新」保留，但关闭由外壳负责（同一屏不能出现两个「关闭」）
    expect(screen.getByRole("button", { name: "刷新" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "关闭" })).toBeNull();
    expect(screen.queryByRole("button", { name: "收起" })).toBeNull();
  });
});
