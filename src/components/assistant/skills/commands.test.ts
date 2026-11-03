import { describe, expect, it } from "vitest";
import {
  filterPaletteItems,
  shouldOpenPalette,
  slashToken,
  type PaletteItem,
} from "@/components/assistant/skills/commands";

const items: PaletteItem[] = [
  { id: "skill:batch-tag-plan", kind: "skill", title: "插入技能：批量打标", description: "", keywords: ["打标"] },
  { id: "skill:asset-ingest", kind: "skill", title: "插入技能：素材入库", description: "", keywords: ["入库"] },
];

describe("slashToken", () => {
  it("解析行首 /token 并给出可删除区间", () => {
    expect(slashToken("/", 1)).toEqual({ query: "", start: 0, end: 1 });
    expect(slashToken("abc\n/权限", 7)).toEqual({ query: "权限", start: 4, end: 7 });
  });

  it("不是行首的 / 不当作命令", () => {
    expect(slashToken("abc /x", 6)).toBeNull();
  });

  it("第二个 / 之后不再匹配（路径不误判为命令 token）", () => {
    expect(slashToken("/home", 5)).toEqual({ query: "home", start: 0, end: 5 });
    expect(slashToken("/a/b", 4)).toBeNull();
  });
});

describe("shouldOpenPalette", () => {
  it("空 query 展开；命中技能名称/关键词展开", () => {
    expect(shouldOpenPalette(items, "")).toBe(true);
    expect(shouldOpenPalette(items, "打标")).toBe(true);
    expect(shouldOpenPalette(items, "入库")).toBe(true);
  });

  it("/home、/etc 这类路径不误触", () => {
    expect(shouldOpenPalette(items, "home")).toBe(false);
    expect(shouldOpenPalette(items, "etc")).toBe(false);
  });
});

describe("filterPaletteItems", () => {
  it("按名称与关键词过滤，并支持子序列", () => {
    expect(filterPaletteItems(items, "打标").map((i) => i.id)).toEqual(["skill:batch-tag-plan"]);
    expect(filterPaletteItems(items, "入库").map((i) => i.id)).toEqual(["skill:asset-ingest"]);
  });
});
