import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "./client";
import { saveTagFacet } from "./tags";

vi.mock("./client", () => ({ invoke: vi.fn() }));

describe("saveTagFacet", () => {
  beforeEach(() => vi.clearAllMocks());

  it("通过单个 DTO 参数提交完整分类配置", async () => {
    const input = {
      key: "brand_info",
      displayName: "品牌信息",
      description: "记录画面中可读出的品牌标识",
      inputMode: "ai_and_manual" as const,
      selectionMode: "multi" as const,
      maxItems: null,
      appliesTo: "all" as const,
      facetKind: "tag" as const,
      numMin: null,
      numMax: null,
      numUnit: "",
      numDecimals: 0,
      numStep: 1,
    };

    await saveTagFacet(input);

    expect(invoke).toHaveBeenCalledWith("save_tag_facet", { input });
  });
});
