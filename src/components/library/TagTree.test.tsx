/**
 * FB6 需求四：TagTree 测试——统一标题行 + 全部展开/全部收起。
 *  - 有可展开节点时显示「全部展开」；点击后所有有子节点的节点出现在 flattenVisible 结果中；
 *  - 再点「全部收起」恢复；单节点折叠后全局按钮文案正确；
 *  - 空树按钮隐藏；按钮只改展开状态，不改选中筛选。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import TagTree from "@/components/library/TagTree";
import { useTagStore } from "@/stores/tagStore";
import { useLibraryStore } from "@/stores/libraryStore";
import type { Tag, TagNode } from "@/types/tag";

vi.mock("@/api/tags", () => ({
  listTags: vi.fn().mockResolvedValue([]),
  listTagFacets: vi.fn().mockResolvedValue([]),
  listTagsByFacet: vi.fn().mockResolvedValue([]),
  searchTagCandidates: vi.fn().mockResolvedValue([]),
}));

const mkTag = (id: number, name: string, facetKey = "subject", parentId: number | null = null): Tag => ({
  id,
  name,
  canonicalName: name,
  normalizedName: name,
  facetKey,
  parentId,
  status: "active",
  isSystem: false,
  isPreset: false,
  sortOrder: id,
  assetCount: 1,
  totalCount: 1,
  aliases: [],
  path: name,
  facetEffective: true,
});

/** 两级树：1(人像) → 2(特写)；3(风景) → 4(海岸)。 */
const TREE: TagNode[] = [
  { tag: mkTag(1, "人像"), children: [{ tag: mkTag(2, "特写", "subject", 1), children: [] }] },
  { tag: mkTag(3, "风景"), children: [{ tag: mkTag(4, "海岸", "subject", 3), children: [] }] },
];

beforeEach(() => {
  useTagStore.setState({
    tree: TREE,
    facets: [],
    treesByFacet: {},
    candidates: [],
    loading: false,
    expanded: new Set([1]),
  });
  useLibraryStore.setState((s) => ({
    ...s,
    filter: {
      ...s.filter,
      assetType: "all",
      untaggedOnly: false,
      tagId: null,
      facetFilters: [],
      excludeTagIds: [],
      trashOnly: false,
    },
  }));
});

describe("TagTree 分面折叠（与文件属性统一）", () => {
  it("默认收起分面，标题行可全部展开再全部收起", async () => {
    await act(async () => { render(<TagTree />); });
    expect(screen.getByText("智能标签")).toBeInTheDocument();
    expect(screen.queryByText("人像")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    expect(screen.getByText("人像")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "全部收起" }));
    expect(screen.queryByText("人像")).not.toBeInTheDocument();
  });

  it("分面标题可独立收起，且标签树节点仍可展开", async () => {
    await act(async () => { render(<TagTree />); });
    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    fireEvent.click(screen.getByRole("button", { name: "标签" }));
    expect(screen.queryByText("人像")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "标签" }));
    fireEvent.click(screen.getAllByRole("button", { name: "展开" })[0]);
    expect(screen.getByText("特写")).toBeVisible();
  });

  it("标签项使用无圆角的连续列表样式", async () => {
    await act(async () => { render(<TagTree />); });
    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    expect(screen.getByText("人像").closest("button")).toHaveClass("ui-nav-list-item");
  });

  it("空树时展开控制按钮隐藏", async () => {
    useTagStore.setState({ tree: [], expanded: new Set() });
    await act(async () => { render(<TagTree />); });
    expect(screen.queryByRole("button", { name: "全部展开" })).toBeNull();
    expect(screen.queryByRole("button", { name: "全部收起" })).toBeNull();
    expect(screen.getByText("智能标签")).toBeInTheDocument();
  });

  it("点击全局按钮不改变选中筛选", async () => {
    await act(async () => { render(<TagTree />); });
    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    const { filter } = useLibraryStore.getState();
    expect(filter.facetFilters).toEqual([]);
    expect(filter.untaggedOnly).toBe(false);
  });

  // M-13：长中文标签在窄侧栏里会被 truncate，必须能 hover 看到全量
  it("标签行带 title 全量提示，长中文标签被截断后仍可看到完整内容", async () => {
    await act(async () => { render(<TagTree />); });
    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    expect(screen.getByText("人像").closest("button")).toHaveAttribute("title", "人像");
    expect(screen.getByText("风景").closest("button")).toHaveAttribute("title", "风景");
    // 子节点同样带全量提示
    fireEvent.click(screen.getAllByRole("button", { name: "展开" })[0]);
    expect(screen.getByText("特写").closest("button")).toHaveAttribute("title", "特写");
  });
});
