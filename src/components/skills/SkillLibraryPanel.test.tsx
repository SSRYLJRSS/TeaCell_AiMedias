import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, screen } from "@testing-library/react";
import { actRender } from "@/test/render";
import SkillLibraryPanel from "./SkillLibraryPanel";

afterEach(cleanup);

describe("SkillLibraryPanel", () => {
  it("默认渲染 17 条技能，可按关键词过滤", async () => {
    await actRender(<SkillLibraryPanel />);
    expect(screen.getAllByRole("listitem")).toHaveLength(17);

    fireEvent.change(screen.getByLabelText("搜索技能"), { target: { value: "导出" } });
    const filtered = screen.getAllByRole("listitem");
    expect(filtered.length).toBeGreaterThan(0);
    expect(filtered.length).toBeLessThan(17);
  });

  it("按类型筛选到可执行技能", async () => {
    await actRender(<SkillLibraryPanel />);
    fireEvent.change(screen.getByLabelText("按类型筛选"), { target: { value: "executable" } });
    expect(screen.getAllByRole("listitem")).toHaveLength(7);
  });

  it("点击技能进入详情，展示校验通过", async () => {
    await actRender(<SkillLibraryPanel />);
    fireEvent.click(screen.getByText("语义搜索"));
    expect(await screen.findByText("校验通过")).toBeTruthy();
    expect(screen.getByText("返回")).toBeTruthy();
  });

  it("详情可触发在对话中使用", async () => {
    const onUse = vi.fn();
    await actRender(<SkillLibraryPanel onUse={onUse} />);
    fireEvent.click(screen.getByText("语义搜索"));
    fireEvent.click(screen.getByRole("button", { name: "在对话中使用" }));
    expect(onUse).toHaveBeenCalledTimes(1);
    expect(onUse.mock.calls[0][0].id).toBe("semantic-search");
  });

  it("可停用某条技能", async () => {
    await actRender(<SkillLibraryPanel />);
    fireEvent.click(screen.getByRole("button", { name: "停用技能 语义搜索" }));
    expect(screen.getByText("已停用")).toBeTruthy();
  });

  it("沉淀按钮触发回调", async () => {
    const onOpenDistill = vi.fn();
    await actRender(<SkillLibraryPanel onOpenDistill={onOpenDistill} />);
    fireEvent.click(screen.getByRole("button", { name: "沉淀技能" }));
    expect(onOpenDistill).toHaveBeenCalledTimes(1);
  });
});
