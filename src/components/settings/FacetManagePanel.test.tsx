/**
 * W4 FacetManagePanel 测试：两组列表 + 弹窗化编辑/新建/删除。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, fireEvent, within } from "@testing-library/react";
import FacetManagePanel from "@/components/settings/FacetManagePanel";
import { listAllTagFacets, deleteTagFacet, deactivateTagFacet, getTagFacetImpact, restoreTagFacet, saveTagFacet, convertFacetKind, type ConversionReport } from "@/api/tags";
import type { TagFacet } from "@/types/tag";

vi.mock("@/api/tags", () => ({
  listAllTagFacets: vi.fn(),
  saveTagFacet: vi.fn(),
  reorderTagFacets: vi.fn().mockResolvedValue(undefined),
  deleteTagFacet: vi.fn().mockResolvedValue({ tagsDeleted: 2, unlinked: 3, opsDeleted: 0, itemsDeleted: 0 }),
  deactivateTagFacet: vi.fn().mockResolvedValue(undefined),
  restoreTagFacet: vi.fn().mockResolvedValue(undefined),
  getTagFacetImpact: vi.fn().mockResolvedValue({ tagCount: 2, assetCount: 3, aiSuggestionItemCount: 0, tagOpCount: 0 }),
  listContentDescriptions: vi.fn().mockResolvedValue([]),
  convertFacetKind: vi.fn().mockResolvedValue(undefined),
}));

const renderPanel = () => render(<FacetManagePanel />);

const facet = (over: Partial<TagFacet> = {}): TagFacet => ({
  key: "clothing_color",
  displayName: "衣服颜色",
  description: "描述",
  inputMode: "ai_and_manual",
  selectionMode: "multi",
  maxItems: 3,
  sortOrder: 1,
  isSystem: false,
  status: "active",
  appliesTo: "all",
  createdAt: 1,
  updatedAt: 1,
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(saveTagFacet).mockResolvedValue(facet());
});

describe("W4 facetManagePanel_two_groups", () => {
  it("两组列表：ai_and_manual 进 AI 组、manual_only 进手工组、inactive 折叠", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([
      facet(),
      facet({ key: "auth_state", displayName: "授权状态", inputMode: "manual_only" }),
      facet({ key: "scene", displayName: "场景/地点", status: "inactive", isSystem: true }),
    ]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    // 两个组标题都在
    expect(screen.getByText("AI 自动打标分类")).toBeInTheDocument();
    expect(screen.getByText("手工填写分类")).toBeInTheDocument();
    expect(screen.queryByText("分类标签")).not.toBeInTheDocument();
    expect(screen.getByText("画面摘要（一句话描述）")).toBeInTheDocument();
    expect(screen.getByText("固定生成 · 不属于分类筛选")).toBeInTheDocument();
    expect(screen.getByText(/填写后可用于搜索和筛选/)).toBeInTheDocument();
    // 停用的折叠（默认收起，只显示计数）
    expect(screen.getByText(/已停用的分类（1）/)).toBeInTheDocument();
    expect(screen.queryByText("场景/地点")).not.toBeInTheDocument();
    // 展开停用区后出现 + 有恢复按钮
    fireEvent.click(screen.getByText(/已停用的分类/));
    expect(screen.getByText("场景/地点")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "恢复" })).toBeInTheDocument();
  });

  it("编辑弹窗：全部字段通过一次原子保存", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    const facetRow = screen.getByText("衣服颜色").closest("li");
    expect(facetRow).not.toBeNull();
    fireEvent.click(within(facetRow!).getByRole("button", { name: "编辑" }));
    // 弹窗字段
    expect(screen.getByLabelText("分类名称")).toBeInTheDocument();
    expect(screen.getByLabelText("给 AI 的分类说明")).toBeInTheDocument();
    // 改名 + 改归类 → 一次保存
    fireEvent.change(screen.getByLabelText("分类名称"), { target: { value: "服装颜色" } });
    fireEvent.click(screen.getByLabelText("只手工填写"));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(saveTagFacet).toHaveBeenCalledWith(expect.objectContaining({
        key: "clothing_color",
        displayName: "服装颜色",
        inputMode: "manual_only",
      })),
    );
  });

  it("新建弹窗：2 个必填；纯中文名 slugify 空串时自动生成合法随机标识并创建成功（不再硬报错）", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    fireEvent.click(screen.getAllByRole("button", { name: "+ 新增分类" })[0]);
    // 中文名（slugify 产出空）+ 描述都填 → 不报错，直接创建成功（key 用自动兜底 facet_xxx）
    fireEvent.change(screen.getByLabelText("分类名称"), { target: { value: "人物服装颜色" } });
    fireEvent.change(screen.getByLabelText("给 AI 的分类说明"), { target: { value: "人物服装的主色调" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() =>
      expect(saveTagFacet).toHaveBeenCalledWith(
        expect.objectContaining({
          displayName: "人物服装颜色",
          key: expect.stringMatching(/^facet_[a-z0-9]+$/),
          inputMode: "ai_and_manual",
        }),
      ),
    );
    expect(saveTagFacet).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/请填写英文标识/)).not.toBeInTheDocument();
  });

  it("新建弹窗：用户可改自动 key；显式清空自己输入的标识才报错", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    fireEvent.click(screen.getAllByRole("button", { name: "+ 新增分类" })[0]);
    fireEvent.change(screen.getByLabelText("分类名称"), { target: { value: "人物服装颜色" } });
    fireEvent.change(screen.getByLabelText("给 AI 的分类说明"), { target: { value: "人物服装的主色调" } });
    // 覆盖自动 key
    fireEvent.change(screen.getByLabelText("英文标识"), { target: { value: "clothing_color" } });
    // 又清空 → 明确报错
    fireEvent.change(screen.getByLabelText("英文标识"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    expect(await screen.findByText(/英文标识不能为空/)).toBeInTheDocument();
    expect(saveTagFacet).not.toHaveBeenCalled();
  });

  it("删除确认：显示精确影响数字；输入分类名后才能确认删除", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    // 影响数字（getTagFacetImpact 返回 tagCount=2 assetCount=3）
    expect(await screen.findByText(/将删除 2 个标签/)).toBeInTheDocument();
    expect(screen.getByText(/解除 3 个素材的关联/)).toBeInTheDocument();
    // 未输入名字 → 确认删除禁用
    expect(screen.getByRole("button", { name: "确认删除" })).toBeDisabled();
    // 输入名字 → 启用并调用
    fireEvent.change(screen.getByPlaceholderText("衣服颜色"), { target: { value: "衣服颜色" } });
    expect(screen.getByRole("button", { name: "确认删除" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(deleteTagFacet).toHaveBeenCalledWith("clothing_color"));
  });

  it("停用区恢复按钮调用 restoreTagFacet", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([
      facet({ key: "scene", displayName: "场景/地点", status: "inactive", isSystem: true }),
    ]);
    renderPanel();
    await waitFor(() => expect(screen.getByText(/已停用的分类（1）/)).toBeInTheDocument());
    fireEvent.click(screen.getByText(/已停用的分类/));
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await waitFor(() => expect(restoreTagFacet).toHaveBeenCalledWith("scene"));
  });

  it("内置 AI 分类可停用和恢复，不显示删除且不能切到手工组", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([
      facet({ key: "scene", displayName: "场景", isSystem: true }),
    ]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("场景")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "删除" })).not.toBeInTheDocument();
    const row = screen.getByText("场景").closest("li");
    expect(row).not.toBeNull();
    fireEvent.click(within(row!).getByRole("button", { name: "停用" }));
    await waitFor(() => expect(deactivateTagFacet).toHaveBeenCalledWith("scene"));
    fireEvent.click(within(row!).getByRole("button", { name: "编辑" }));
    expect(screen.getByLabelText("只手工填写")).toBeDisabled();
    expect(getTagFacetImpact).not.toHaveBeenCalled();
  });

  it("停用的用户自建分类仍有删除入口", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([
      facet({ status: "inactive", isSystem: false }),
    ]);
    renderPanel();
    await waitFor(() => expect(screen.getByText(/已停用的分类（1）/)).toBeInTheDocument());
    fireEvent.click(screen.getByText(/已停用的分类/));
    const row = screen.getByText("衣服颜色").closest("li");
    expect(row).not.toBeNull();
    fireEvent.click(within(row!).getByRole("button", { name: "删除" }));
    expect(await screen.findByText(/将删除 2 个标签/)).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("衣服颜色"), { target: { value: "衣服颜色" } });
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(deleteTagFacet).toHaveBeenCalledWith("clothing_color"));
  });

  it("不显示全局提示词覆盖框，分类业务说明仍可编辑", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    expect(screen.queryByRole("textbox", { name: "AI 打标全局说明" })).not.toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "超级搜索提示词" })).not.toBeInTheDocument();
    const facetRow = screen.getByText("衣服颜色").closest("li");
    fireEvent.click(within(facetRow!).getByRole("button", { name: "编辑" }));
    const description = screen.getByLabelText("给 AI 的分类说明");
    fireEvent.change(description, { target: { value: "可见分类业务描述" } });
    expect(description).toHaveValue("可见分类业务描述");
  });
});

// ═══════════════ V24（Phase 7-7）：数值分面类型驱动表单 + 转换预览 ═══════════════

describe("V24 facetManagePanel_number_facet_form", () => {
  it("新建数值分类：类型和范围通过一次事务保存", async () => {
    vi.mocked(listAllTagFacets).mockResolvedValue([facet()]);
    renderPanel();
    await waitFor(() => expect(screen.getByText("衣服颜色")).toBeInTheDocument());
    fireEvent.click(screen.getAllByRole("button", { name: "+ 新增分类" })[0]);
    expect(await screen.findByText("类型")).toBeTruthy();

    // 选「数值」→ 值域配置出现
    const numberRadio = screen.getByLabelText("数值（如人数）");
    fireEvent.click(numberRadio);
    expect(screen.getByLabelText("数值下限")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("数值下限"), { target: { value: "0" } });
    fireEvent.change(screen.getByLabelText("数值上限"), { target: { value: "50" } });
    fireEvent.change(screen.getByLabelText("数值单位"), { target: { value: "人" } });
    fireEvent.change(screen.getByLabelText("分类名称"), { target: { value: "人数" } });
    fireEvent.change(screen.getByLabelText("给 AI 的分类说明"), { target: { value: "画面中的人数" } });

    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(saveTagFacet).toHaveBeenCalled());
    expect(saveTagFacet).toHaveBeenCalledWith(
      expect.objectContaining({
        key: expect.stringMatching(/^facet_[a-z0-9]+$/),
        facetKind: "number",
        numMin: 0,
        numMax: 50,
        numUnit: "人",
      }),
    );
    expect(saveTagFacet).toHaveBeenCalledTimes(1);
  });

  it("编辑数值分面：保存同时更新范围与基础字段", async () => {
    const numberFacet = facet({ key: "people_count", displayName: "人数", facetKind: "number", numMin: 0, numMax: 50, numUnit: "人", numStep: 1 });
    vi.mocked(listAllTagFacets).mockResolvedValue([numberFacet]);
    vi.mocked(saveTagFacet).mockResolvedValue(numberFacet);
    renderPanel();
    await screen.findByText("人数");
    fireEvent.click(screen.getAllByRole("button", { name: "编辑" })[0]);
    expect(await screen.findByText("数值设置")).toBeTruthy();
    expect(screen.getByText(/数值类型创建后不可改回标签类型/)).toBeTruthy();
    fireEvent.change(screen.getByLabelText("数值上限"), { target: { value: "99" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(saveTagFacet).toHaveBeenCalledWith(expect.objectContaining({ key: "people_count", facetKind: "number", numMax: 99, numUnit: "人" })));
  });

  it("转换为数值型：先 dry-run 预览分桶；存在冲突/歧义时执行按钮禁用（不自动裁决）", async () => {
    const report: ConversionReport = {
      facetKey: "clothing_color",
      parsed: [{ tagId: 1, name: "5", value: 5 }],
      ambiguous: [{ tagId: 2, name: "约5", reason: "约数" }],
      unparseable: [{ tagId: 3, name: "很多", assetCount: 2 }],
      conflicts: [{ assetId: 9, candidates: [[1, 5, "manual"], [2, 6, "ai_unreviewed"]] }],
      hierarchyLoss: 0,
      aliasLoss: 1,
      pendingRejected: 0,
    };
    const numberFacet = facet({ key: "people_count", displayName: "人数", facetKind: "number" });
    vi.mocked(listAllTagFacets).mockResolvedValue([facet(), numberFacet]);
    vi.mocked(convertFacetKind).mockResolvedValue(report);
    renderPanel();
    await screen.findByText("衣服颜色");
    fireEvent.click(screen.getAllByRole("button", { name: "编辑" })[0]);
    fireEvent.click(await screen.findByRole("button", { name: /高级/ }));
    fireEvent.click(await screen.findByText("转换为数值型…"));
    expect(await screen.findByText("转换为数值型 · 预览报告")).toBeTruthy();
    await waitFor(() => expect(convertFacetKind).toHaveBeenCalledWith("clothing_color", true));
    expect(await screen.findByText(/冲突 1 处/)).toBeTruthy();
    const execBtn = screen.getByRole("button", { name: "存在冲突/歧义，先处理" }) as HTMLButtonElement;
    expect(execBtn.disabled).toBe(true);
    expect(convertFacetKind).toHaveBeenCalledTimes(1); // 只 dry-run，未执行
  });
});
