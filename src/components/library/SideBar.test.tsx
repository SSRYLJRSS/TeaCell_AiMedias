/**
 * SideBar 结构回归测试（FB4-02 §4.4/§10.3）：
 *  - 标签头部不是滚动正文的子节点（固定头部与正文为兄弟层）；
 *  - 头部不再有 sticky 类；
 *  - 正文层含 overflow-y-auto overflow-x-hidden；
 *  - 智能标签 / 文件属性切换正常；
 *  - 标签管理入口仍可打开。
 *  结构测试必须能阻止未来把头部重新塞回滚动层。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import SideBar from "@/components/library/SideBar";
import { useLibraryStore } from "@/stores/libraryStore";

vi.mock("@/components/library/TagTree", () => ({
  default: ({ onManage }: { onManage: () => void }) => (
    <div data-testid="tag-tree">
      <button type="button" onClick={onManage}>
        管理标签
      </button>
    </div>
  ),
}));
vi.mock("@/components/library/MetadataPanel", () => ({
  default: () => <div data-testid="metadata-panel">文件属性面板</div>,
}));
vi.mock("@/components/dialogs/TagManageDialog", () => ({
  default: ({ open }: { open: boolean }) =>
    open ? <div data-testid="tag-manage-dialog">标签管理</div> : null,
}));

beforeEach(() => {
  useLibraryStore.setState({
    filter: {
      assetType: "all",
      untaggedOnly: false,
      tagId: null,
      facetFilters: [],
      excludeTagIds: [],
      metadataFilters: [],
      search: "",
      sortBy: "created_at",
      sortDir: "desc",
      trashOnly: false,
    },
  });
});

/** 标签区 = aside 的第二个子区块（类型区之后）；其内部是「固定头部 + 滚动正文」两个兄弟层 */
function getTagSection(): HTMLElement {
  const aside = document.querySelector("aside")!;
  return aside.children[1] as HTMLElement;
}

function getHeader(): HTMLElement {
  return getTagSection().children[0] as HTMLElement;
}

function getBody(): HTMLElement {
  return getTagSection().children[1] as HTMLElement;
}

describe("SideBar 标签区（FB4-02）", () => {
  it("标签头部不是滚动正文的子节点（兄弟层结构）", () => {
    render(<SideBar />);
    const section = getTagSection();
    // 区块为 flex-col：两个直接子节点 = 头部 + 正文
    expect(section.children).toHaveLength(2);
    // 头部含「标签」标题与类型切换，正文含 TagTree
    expect(getHeader().textContent).toContain("标签");
    expect(getHeader().textContent).toContain("智能标签");
    expect(getHeader().textContent).toContain("文件属性");
    expect(getBody().querySelector("[data-testid=tag-tree]")).not.toBeNull();
    // 正文内不得包含类型区标题（FB4-02 结构意图；FB6 后 TagTree/MetadataPanel 自带面板标题属正常）
    expect(getBody().textContent).not.toContain("素材筛选");
  });

  it("头部不再有 sticky 类", () => {
    render(<SideBar />);
    expect(getHeader().className).not.toContain("sticky");
    expect(getHeader().className).not.toContain("top-0");
    expect(getHeader().className).not.toContain("z-10");
  });

  it("正文包含 overflow-y-auto overflow-x-hidden（只纵向滚动）", () => {
    render(<SideBar />);
    const body = getBody();
    expect(body.className).toContain("overflow-y-auto");
    expect(body.className).toContain("overflow-x-hidden");
    // 头部不得是滚动容器
    expect(getHeader().className).not.toContain("overflow-y-auto");
  });

  /**
   * M-11 真机补证（960×640，真实安装包）：收起态的悬浮助手入口是 fixed 浮层、停在左下，
   * 它压在筛选树上时最后一行永远点不到。真机复验还证明只给滚动内容加内边距不够——默认滚动
   * 位置的行仍落在入口底下，所以安全区必须由 aside 容器承担（滚动可视区整体上移）。
   */
  it("左侧栏容器留出悬浮入口安全区，任何滚动位置最后一行都不在入口底下（M-11）", () => {
    render(<SideBar />);
    const panel = screen.getByTestId("library-side-panel");
    expect(panel.className).toContain("pb-32");
    // 安全区不能改成滚动内容里的内边距：那样滚到底只是多一段空白，默认位置仍被遮挡。
    expect(screen.getByTestId("library-tag-scroll").className).not.toContain("pb-32");
  });

  it("智能标签 / 文件属性切换正常", () => {
    render(<SideBar />);
    expect(screen.getByTestId("tag-tree")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "文件属性" }));
    expect(screen.getByTestId("metadata-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("tag-tree")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "智能标签" }));
    expect(screen.getByTestId("tag-tree")).toBeInTheDocument();
  });

  it("标签管理入口仍可打开", () => {
    render(<SideBar />);
    expect(screen.queryByTestId("tag-manage-dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "管理标签" }));
    expect(screen.getByTestId("tag-manage-dialog")).toBeInTheDocument();
  });
});
