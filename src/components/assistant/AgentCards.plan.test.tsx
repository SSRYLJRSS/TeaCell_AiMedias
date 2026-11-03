/**
 * 计划卡的两个 A5-02 契约：
 * - 必须摊开「谁发起的」：外部 MCP 客户端要显示它在设置里的配对名称，
 *   而不是让审核者从一个不透明的 principal 上猜；
 * - 拒绝是可执行动作：走 reject_plan，成功后回读真实快照，不自行推断状态。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { PlanCard, capabilityLabel, planOriginLabel, planTargetLabel } from "@/components/assistant/AgentCards";
import type { PlanPreview } from "@/types/plans";

vi.mock("@/api/plans", () => ({
  listPendingPlans: vi.fn(),
  approvePlan: vi.fn(),
  rejectPlan: vi.fn(),
  getOperation: vi.fn(),
}));

import { approvePlan, rejectPlan } from "@/api/plans";
import { useAgentSessionStore } from "@/stores/agentSessionStore";

function planOf(over: Partial<PlanPreview> = {}): PlanPreview {
  return {
    schemaVersion: 1,
    planId: "plan_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    capability: "assets.trash.submit",
    library: {
      libraryId: "library_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      libraryEpoch: "epoch_cccccccccccccccccccccccccccccccc",
    },
    reviewStatus: "pending",
    reviewKind: "ordinary",
    originKind: "mcp",
    originLabel: "本地编辑器",
    itemCount: 2,
    summary: "将选中的 2 项素材移入回收站",
    reviewRef: "review_dddddddddddddddddddddddddddddddd",
    expiresAtUnixSeconds: Math.floor(Date.now() / 1000) + 600,
    scope: {
      scopeRef: "selection_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
      selectionRef: "selection_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    },
    budget: { maxRequests: 0, maxTokens: 0, maxSeconds: 0, maxItems: 2 },
    ...over,
  } as PlanPreview;
}

beforeEach(() => {
  vi.mocked(approvePlan).mockReset();
  vi.mocked(rejectPlan).mockReset();
  useAgentSessionStore.setState({
    linkedPlans: { conversation_ref_fixture: [] },
  } as never);
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("计划来源文案", () => {
  it("外部客户端显示配对名称，内置助手与本机界面各有固定说法", () => {
    expect(planOriginLabel(planOf())).toBe("外部客户端：本地编辑器");
    expect(planOriginLabel(planOf({ originKind: "pi" }))).toBe("内置助手");
    expect(planOriginLabel(planOf({ originKind: "gui" }))).toBe("本机界面");
  });

  it("配对已被删除时不说成某个具体客户端，也不编一个名字", () => {
    expect(planOriginLabel(planOf({ originLabel: undefined }))).toBe(
      "外部客户端（已不在配对列表中）",
    );
  });

  it("卡片上直接显示发起方，审核者不用点开就知道是外部请求", () => {
    render(<PlanCard plan={planOf()} />);
    expect(screen.getByText("发起方：外部客户端：本地编辑器")).toBeTruthy();
  });
});

describe("A4-01 导出计划必须写明复制与目标", () => {
  it("导出能力的标签直说「复制导出」，不只写「执行导出」", () => {
    // 旧标签「执行导出」会被读成「把库内文件搬过去」，与「复制、库内不动」的本意相反
    expect(capabilityLabel("exports.prepare")).toBe("准备复制导出");
    expect(capabilityLabel("exports.submit")).toBe("复制导出");
    expect(capabilityLabel("exports.submit")).not.toBe("执行导出");
  });

  it("计划卡把目标位置单列一行，导出这一行写成「复制到」", () => {
    render(
      <PlanCard
        plan={planOf({
          capability: "exports.submit",
          summary: "复制导出：将选中的 2 项素材复制到 D:\\导出（平铺），库内原文件与记录保留、不移动",
          scope: {
            scopeRef: "selection_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            selectionRef: "selection_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            targetRef: "directory_ffffffffffffffffffffffffffffffff",
          },
        })}
      />,
    );
    // 此前 targetRef 完全没渲染，审核者看不到文件会去哪儿
    expect(screen.getByText("复制到：已授权目录")).toBeTruthy();
    // 真实路径来自后端摘要，卡片不自行编造
    expect(screen.getByText(/D:\\导出/)).toBeTruthy();
    expect(screen.getByText("复制导出 · 2 项")).toBeTruthy();
  });

  it("同一个 targetRef 在移动/导入计划里不冒充成导出目标", () => {
    expect(planTargetLabel("assets.move.submit")).toBe("移动到：库内目标位置");
    expect(planTargetLabel("imports.submit")).toBe("入库到：目标分库");
    expect(planTargetLabel("tagging.submit")).toBe("目标：已授权范围");
  });
});

describe("拒绝计划", () => {
  it("点拒绝走 reject_plan，而不是批准", async () => {
    vi.mocked(rejectPlan).mockResolvedValue({
      schemaVersion: 1,
      planId: "plan_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      reviewStatus: "invalidated",
    });

    render(<PlanCard plan={planOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "拒绝" }));

    await waitFor(() =>
      expect(rejectPlan).toHaveBeenCalledWith(
        "plan_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "review_dddddddddddddddddddddddddddddddd",
      ),
    );
    expect(approvePlan).not.toHaveBeenCalled();
  });

  it("拒绝失败时把原因摊开，不假装已经拒绝", async () => {
    vi.mocked(rejectPlan).mockRejectedValue(new Error("这个计划已经被执行"));

    render(<PlanCard plan={planOf()} />);
    fireEvent.click(screen.getByRole("button", { name: "拒绝" }));

    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("这个计划已经被执行"),
    );
  });

  it("已过期的计划两个按钮都禁用，避免点了才发现过期", () => {
    render(<PlanCard plan={planOf({ expiresAtUnixSeconds: 1 })} />);
    expect(
      screen.getByRole("button", { name: "计划已过期" }).hasAttribute("disabled"),
    ).toBe(true);
    expect(screen.getByRole("button", { name: "拒绝" }).hasAttribute("disabled")).toBe(true);
  });
});
