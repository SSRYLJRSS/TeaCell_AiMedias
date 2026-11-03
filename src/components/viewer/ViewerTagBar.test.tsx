/**
 * ViewerTagBar 回归测试（FB4-01 §4.2/§10.1）：
 *  - 展开总高 128px（标题 32 + 正文 96），收起 32px；
 *  - 正文双栏 grid（grid-cols-2）、96px 高度语义；
 *  - 每个分面组只渲染一次，名称与标签同组（grid item）；
 *  - 「添加标签」是标题栏独立图标按钮，点击触发 onAddTag 且不折叠；
 *  - 折叠按钮更新 aria-expanded，正文卸载；
 *  - 删除 chip 仍调用正确 tag id；空标签显示「未打标」。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import ViewerTagBar from "@/components/viewer/ViewerTagBar";
import { useTagStore } from "@/stores/tagStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { Settings } from "@/types/settings";
import type { Tag, TagFacet } from "@/types/tag";

const mkTag = (id: number, name: string, facetKey: string): Tag => ({
  id,
  name,
  canonicalName: name,
  normalizedName: name,
  facetKey,
  parentId: null,
  status: "active",
  isSystem: false,
  isPreset: false,
  sortOrder: 0,
  assetCount: 0,
  totalCount: 0,
  aliases: [],
  path: "",
  facetEffective: true,
});

const mkFacet = (key: string, displayName: string): TagFacet => ({
  key,
  displayName,
  description: "",
  inputMode: "ai_and_manual",
  selectionMode: "multi",
  maxItems: 5,
  sortOrder: 0,
  isSystem: true,
  status: "active",
  appliesTo: "all",
  createdAt: 0,
  updatedAt: 0,
});

const mkSettings = (): Settings => ({
  ai: {
    profiles: [],
    activeProfile: "",
    videoTagging: false,
    videoTaggingMode: "cover",
    videoFrameCount: 3,
    batchLimit: 500,
    ollamaSourceId: "auto",
    systemPromptTagging: "",
    systemPromptSearch: "",
    confidenceMinSuggest: 0.3,
  },
  theme: "system",
  logLevel: "info",
  thumbnailCacheMb: 2048,
  tagCategories: [],
  libraryRoot: "",
  trashRetentionDays: 30,
  customDownloadSources: [],
  modelDownloadProxy: "",
  appearance: {
    grid: { libraryCellStep: 3, importCellStep: 1, cellAspect: "1:1", cellFit: "cover", matchDominantColor: false },
    hoverPreview: { enabled: true, previewSeconds: 3, inLibraryGrid: true },
    colorStrip: { enabled: true, showInLibraryGrid: false, showInViewer: true, showInImportGrid: false, height: "normal", mode: "ratio", count: 6 },
    kinship: { syncTagsToSiblings: true, mergeInLibrary: false },
  },
  navigation: {
    shortcuts: [
      { id: "import", visible: true },
      { id: "library", visible: true },
      { id: "ai", visible: true },
      { id: "treasure", visible: true },
    ],
    settingsPinned: false,
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({ settings: mkSettings(), loaded: true, loading: false, loadError: null, saving: false });
  useTagStore.setState({
    facets: [mkFacet("subject", "主体对象"), mkFacet("scene", "场景/地点")],
  });
});

function renderBar(tags: Tag[], onAddTag = vi.fn(), onRemoveTag = vi.fn(), contentDescription?: string | null) {
  const view = render(
    <ViewerTagBar
      assetId={1}
      tags={tags}
      contentDescription={contentDescription}
      onAddTag={onAddTag}
      onRemoveTag={onRemoveTag}
    />,
  );
  return { ...view, onAddTag, onRemoveTag };
}

describe("ViewerTagBar（FB4-01）", () => {
  it("展开时总高度 128px（标题 32 + 正文 96），收起时 32px", () => {
    const { container } = renderBar([mkTag(1, "猫", "subject")]);
    const root = container.firstChild as HTMLElement;
    expect(root.style.height).toBe("128px");
    // 标题栏与正文
    expect(root.querySelectorAll("div")[0].className).toContain("h-8");
    const body = document.getElementById("viewer-tagbar-body")!;
    expect(body.className).toContain("h-24");

    fireEvent.click(screen.getByRole("button", { name: /收起/ }));
    expect(root.style.height).toBe("32px");
    expect(document.getElementById("viewer-tagbar-body")).toBeNull();
  });

  it("正文是双栏 grid（grid-cols-2），96px 高度语义", () => {
    renderBar([mkTag(1, "猫", "subject"), mkTag(2, "海滩", "scene")]);
    const body = document.getElementById("viewer-tagbar-body")!;
    expect(body.className).toContain("grid-cols-2");
    expect(body.className).toContain("h-24");
  });

  it("每个分面组只渲染一次，名称与标签同组", () => {
    renderBar([
      mkTag(1, "猫", "subject"),
      mkTag(2, "狗", "subject"),
      mkTag(3, "海滩", "scene"),
    ]);
    const body = document.getElementById("viewer-tagbar-body")!;
    // 名称只出现一次（同组内不重复渲染分面名）
    expect(screen.getByText("主体对象")).toBeInTheDocument();
    expect(screen.getByText("场景/地点")).toBeInTheDocument();
    // 两个分面 = 两个 grid item（body 的直接子元素为 2）
    const items = body.children;
    expect(items).toHaveLength(2);
    // 组内：名称与标签同在一个 grid item
    const subjectGroup = items[0] as HTMLElement;
    expect(subjectGroup.textContent).toContain("主体对象");
    expect(subjectGroup.textContent).toContain("猫");
    expect(subjectGroup.textContent).toContain("狗");
    expect(subjectGroup.textContent).not.toContain("海滩");
  });

  it("「添加标签」是标题栏独立图标按钮：点击触发 onAddTag 且不折叠", () => {
    const { onAddTag } = renderBar([mkTag(1, "猫", "subject")]);
    const addBtn = screen.getByRole("button", { name: "添加标签" });
    expect(addBtn.getAttribute("aria-label")).toBe("添加标签");
    expect(addBtn.getAttribute("title")).toBe("添加标签");
    // 点击前展开
    const collapseBtn = screen.getByRole("button", { name: /收起/ });
    expect(collapseBtn.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(addBtn);
    expect(onAddTag).toHaveBeenCalledTimes(1);
    expect(collapseBtn.getAttribute("aria-expanded")).toBe("true");
    expect(document.getElementById("viewer-tagbar-body")).not.toBeNull();
  });

  it("折叠按钮更新 aria-expanded 且正文卸载", () => {
    renderBar([mkTag(1, "猫", "subject")]);
    const collapseBtn = screen.getByRole("button", { name: /收起/ });
    expect(collapseBtn.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(collapseBtn);
    expect(screen.getByRole("button", { name: /展开/ }).getAttribute("aria-expanded")).toBe("false");
    expect(document.getElementById("viewer-tagbar-body")).toBeNull();
  });

  it("删除 chip 调用正确 tag id", () => {
    const { onRemoveTag } = renderBar([
      mkTag(1, "猫", "subject"),
      mkTag(2, "狗", "subject"),
    ]);
    fireEvent.click(screen.getByRole("button", { name: "移除标签 狗" }));
    expect(onRemoveTag).toHaveBeenCalledWith(2);
    expect(onRemoveTag).not.toHaveBeenCalledWith(1);
  });

  it("空标签显示「未打标」", () => {
    renderBar([]);
    expect(screen.getByText("未打标")).toBeInTheDocument();
  });
});

// ── FB5-05（§7.6.1）：一句话描述行 ──

describe("ViewerTagBar 一句话描述（FB5-05 §7.6.1）", () => {
  it("描述行横跨两栏（col-span-2），左列「一句话描述」，正文为普通只读文本（非 TagChip）", () => {
    renderBar([mkTag(1, "猫", "subject")], undefined, undefined, "夜晚树下多人合影");
    const body = document.getElementById("viewer-tagbar-body")!;
    const descRow = body.children[0] as HTMLElement;
    expect(descRow.className).toContain("grid-cols-[76px_minmax(0,1fr)]");
    expect(descRow.textContent).toContain("一句话描述");
    expect(descRow.textContent).toContain("夜晚树下多人合影");
    // 描述是普通文本，不是 TagChip（无删除按钮/圆点）
    expect(descRow.querySelector("[aria-label^='移除标签']")).toBeNull();
    // 标签分面仍在第二项
    expect(body.children[1]?.textContent).toContain("主体对象");
    // 高度契约不变：正文仍 96px
    expect(body.className).toContain("h-24");
  });

  it("无描述（null/undefined/空白）不渲染描述行，空标签时显示「未打标」", () => {
    const { rerender } = render(
      <ViewerTagBar assetId={1} tags={[]} contentDescription={null} onAddTag={vi.fn()} onRemoveTag={vi.fn()} />,
    );
    expect(screen.queryByText("一句话描述")).not.toBeInTheDocument();
    expect(screen.getByText("未打标")).toBeInTheDocument();
    rerender(
      <ViewerTagBar assetId={1} tags={[]} contentDescription={"   "} onAddTag={vi.fn()} onRemoveTag={vi.fn()} />,
    );
    expect(screen.queryByText("一句话描述")).not.toBeInTheDocument();
  });

  it("有描述但无真实标签：只显示描述行，不显示「未打标」", () => {
    renderBar([], undefined, undefined, "纯红底色");
    expect(screen.getByText("一句话描述")).toBeInTheDocument();
    expect(screen.queryByText("未打标")).not.toBeInTheDocument();
  });

  it("标题栏「标签 N 项」只统计 tags.length，描述不计入", () => {
    renderBar([mkTag(1, "猫", "subject")], undefined, undefined, "夜晚树下多人合影");
    expect(screen.getByText("1 项")).toBeInTheDocument();
    expect(screen.queryByText("2 项")).not.toBeInTheDocument();
  });

  it("折叠后描述随正文一起隐藏", () => {
    renderBar([mkTag(1, "猫", "subject")], undefined, undefined, "夜晚树下多人合影");
    expect(screen.getByText("一句话描述")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /收起/ }));
    expect(screen.queryByText("一句话描述")).not.toBeInTheDocument();
    expect(document.getElementById("viewer-tagbar-body")).toBeNull();
  });
});
