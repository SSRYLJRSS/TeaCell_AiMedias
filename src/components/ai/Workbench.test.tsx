/**
 * Workbench 一句话描述字段测试（FB5-05 §7.6）：
 *  - 单行 input，不限字数，不显示字数计数或上限提示；
 *  - 确认按钮在「标签为空但描述非空」时仍可点击（§7.5）；
 *  - 已确认/已拒绝张只读展示（空描述显示「未生成描述」）。
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import Workbench from "@/components/ai/Workbench";
import type { AiSuggestion } from "@/types/ai";
import type { WorkbenchFacet } from "@/types/tag";

// V24：数值建议测试点击后触发图片降级链 → toFileUrl 需要 Tauri 内部环境，mock 掉
vi.mock("@/api/thumbnail", () => ({
  getThumbnailUrl: vi.fn().mockResolvedValue("asset://thumb/hd.webp"),
  toFileUrl: (p: string) => `asset://${p}`,
}));
vi.mock("@/api/assets", () => ({
  getAsset: vi.fn().mockResolvedValue(null),
  getAssetUrls: vi.fn().mockResolvedValue([]),
}));

const mkSuggestion = (over: Partial<AiSuggestion> = {}): AiSuggestion => ({
  id: 1,
  batchId: 1,
  assetId: 1,
  assetPath: "/a.jpg",
  mimeType: "image/jpeg",
  suggestedTags: { subject: ["猫"] },
  status: "pending",
  confirmedTags: {},
  lastError: null,
  createdAt: 1,
  suggestedDescription: "",
  confirmedDescription: null,
  currentDescription: "",
  ...over,
});

const facets: WorkbenchFacet[] = [
  {
    key: "subject",
    displayName: "主体对象",
    description: "",
    inputMode: "ai_and_manual",
    selectionMode: "multi",
    maxItems: 5,
  },
];

function renderWorkbench(over: Partial<AiSuggestion> = {}, description = "夜晚树下多人合影") {
  const onTagsChange = vi.fn();
  const onDescriptionChange = vi.fn();
  const onConfirm = vi.fn().mockResolvedValue(undefined);
  const view = render(
    <Workbench
      suggestion={mkSuggestion(over)}
      aiGroup={facets}
      manualGroup={[]}
      tags={{ subject: ["猫"] }}
      onTagsChange={onTagsChange}
      description={description}
      onDescriptionChange={onDescriptionChange}
      index={0}
      total={1}
      onGoto={vi.fn()}
      onConfirm={onConfirm}
      onReject={vi.fn().mockResolvedValue(undefined)}
      onRestore={vi.fn().mockResolvedValue(undefined)}
    />,
  );
  return { ...view, onTagsChange, onDescriptionChange, onConfirm };
}

describe("Workbench 一句话描述（FB5-05 §7.6）", () => {
  it("描述不限字数，长文本编辑完整传递且不显示字数上限", () => {
    const { onDescriptionChange } = renderWorkbench({}, "夜晚树下多人合影");
    const input = screen.getByRole("textbox", { name: "一句话描述" }) as HTMLInputElement;
    expect(input).toBeInTheDocument();
    expect(input).not.toHaveAttribute("maxlength");
    expect(input.placeholder).not.toMatch(/字|30/);
    expect(screen.queryByText(/\d+\/30/)).not.toBeInTheDocument();
    const longDescription = "🌅女子站在湖边树下回头张望，远处的树林和水面映着柔和光线。".repeat(4);
    fireEvent.change(input, { target: { value: longDescription } });
    expect(onDescriptionChange).toHaveBeenCalledWith(longDescription);
  });

  it("标签为空但描述非空：确认按钮可点击（§7.5）", () => {
    const { onConfirm } = renderWorkbench({}, "纯红底色");
    const btn = screen.getByRole("button", { name: "确认写入" });
    expect(btn.hasAttribute("disabled")).toBe(false);
    fireEvent.click(btn);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("主体为空时显示「未识别」占位，不把占位词写成标签", () => {
    const onTagsChange = vi.fn();
    render(
      <Workbench
        suggestion={mkSuggestion({ suggestedTags: {} })}
        aiGroup={facets}
        manualGroup={[]}
        tags={{}}
        onTagsChange={onTagsChange}
        description="城市建筑与树林交接的远景"
        onDescriptionChange={vi.fn()}
        index={0}
        total={1}
        onGoto={vi.fn()}
        onConfirm={vi.fn().mockResolvedValue(undefined)}
        onReject={vi.fn().mockResolvedValue(undefined)}
        onRestore={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    expect(screen.getByTestId("subject-unrecognized")).toHaveTextContent("未识别");
    expect(onTagsChange).not.toHaveBeenCalled();
  });

  it("已确认张：描述只读展示（无 input）；空描述显示「未生成描述」", () => {
    renderWorkbench({ status: "confirmed", confirmedTags: { subject: ["猫"] } }, "");
    expect(screen.queryByRole("textbox", { name: "一句话描述" })).not.toBeInTheDocument();
    expect(screen.getByText("未生成描述")).toBeInTheDocument();
    expect(screen.getByText("✓ 已写入")).toBeInTheDocument();
  });

  it("确认栏在标签滚动区之外并位于工作台底部", () => {
    renderWorkbench();
    const scrollArea = screen.getByTestId("workbench-facet-scroll");
    const confirmBar = screen.getByTestId("workbench-confirm-bar");
    const confirmButton = screen.getByRole("button", { name: "确认写入" });

    expect(confirmBar).toContainElement(confirmButton);
    expect(scrollArea).not.toContainElement(confirmButton);
    expect(confirmBar.previousElementSibling).toBe(scrollArea);
    expect(screen.getByTestId("workbench-facet-panel")).toHaveClass(
      "h-[clamp(15rem,38vh,22rem)]",
      "shrink-0",
    );
  });
});

// ═══════════════ V24（Phase 7-4）：数值建议区 ═══════════════

describe("Workbench 数值建议（V24）", () => {
  const numberItems = [
    { id: 11, facetKey: "people_count", displayName: "人数", numValue: 5, decision: "pending", decisionReason: null },
    { id: 12, facetKey: "people_count", displayName: "人数", numValue: null, decision: "pending", decisionReason: "需人工确认：原文「约5」（约数）" },
  ];

  function renderWithNumbers(over: Partial<Parameters<typeof Workbench>[0]> = {}) {
    const onDecideNumberItem = vi.fn().mockResolvedValue(undefined);
    render(
      <Workbench
        suggestion={mkSuggestion()}
        aiGroup={facets}
        manualGroup={[]}
        tags={{ subject: ["猫"] }}
        onTagsChange={vi.fn()}
        description="夜晚树下多人合影"
        onDescriptionChange={vi.fn()}
        index={0}
        total={1}
        onGoto={vi.fn()}
        onConfirm={vi.fn().mockResolvedValue(undefined)}
        onReject={vi.fn().mockResolvedValue(undefined)}
        onRestore={vi.fn().mockResolvedValue(undefined)}
        numberItems={numberItems}
        onDecideNumberItem={onDecideNumberItem}
        {...over}
      />,
    );
    return { onDecideNumberItem };
  }

  it("workbench_number_items_render：确定值显示数值 + 采纳/拒绝；歧义项提示人工确认（不自动取值）", () => {
    renderWithNumbers();
    expect(screen.getByText("数值建议")).toBeInTheDocument();
    const rows = screen.getAllByTestId("workbench-number-item");
    expect(rows).toHaveLength(2);
    expect(screen.getByLabelText("采纳数值建议 人数")).toBeInTheDocument();
    expect(screen.getByLabelText("拒绝数值建议 人数")).toBeInTheDocument();
    // 歧义项：numValue=null → 无采纳按钮，显示人工确认提示
    expect(screen.getByText(/需人工确认：.*约数/)).toBeInTheDocument();
  });

  it("workbench_number_item_decide：点采纳调用 onDecideNumberItem(id, 'accepted')", async () => {
    const { onDecideNumberItem } = renderWithNumbers();
    fireEvent.click(screen.getByLabelText("采纳数值建议 人数"));
    await waitFor(() => expect(onDecideNumberItem).toHaveBeenCalledWith(11, "accepted"));
  });
});
