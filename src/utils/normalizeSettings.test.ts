/**
 * normalizeSettings 测试（指导书 A-2 / A-4）：后端缺失字段时安全兜底，不白屏。
 */
import { describe, expect, it } from "vitest";
import { normalizeSettings } from "@/utils/normalizeSettings";
import { DEFAULT_BATCH_LIMIT, DEFAULT_CACHE_MB, DEFAULT_LOCAL_BATCH_LIMIT, DEFAULT_MODEL } from "@/utils/normalizeSettings";
import { CELL_STEPS } from "@/types/settings";

describe("normalizeSettings", () => {
  it("丢弃旧版全局提示词覆盖字段", () => {
    const settings = normalizeSettings({
      ai: { systemPromptTagging: "旧打标覆盖", systemPromptSearch: "旧搜索覆盖" },
    });
    expect(settings.ai.systemPromptTagging).toBeUndefined();
    expect(settings.ai.systemPromptSearch).toBeUndefined();
  });

  it("新设置及旧设置缺少首次教程标记时均会邀请，已处理标记仍保留", () => {
    expect(normalizeSettings({}).tutorialPromptHandled).toBe(false);
    expect(normalizeSettings({ tutorialPromptHandled: false }).tutorialPromptHandled).toBe(false);
    expect(normalizeSettings({ tutorialPromptHandled: true }).tutorialPromptHandled).toBe(true);
  });

  it("对完全缺失的输入返回全默认值", () => {
    const s = normalizeSettings(undefined);
    expect(s.theme).toBe("system");
    expect(s.logLevel).toBe("info");
    expect(s.ai.profiles).toEqual([]);
    expect(s.ai.videoTagging).toBe(false);
    expect(s.ai.batchLimit).toBe(DEFAULT_BATCH_LIMIT);
    expect(s.ai.localBatchLimit).toBe(DEFAULT_LOCAL_BATCH_LIMIT);
    expect(s.ai.activeProfile).toBe("");
    expect(s.tagCategories).toEqual([]);
    expect(s.customDownloadSources).toEqual([]);
    expect(s.libraryRoot).toBe("");
    expect(s.trashRetentionDays).toBe(30);
    expect(s.thumbnailCacheMb).toBe(DEFAULT_CACHE_MB);
  });

  it("null / 非对象输入兜底为默认", () => {
    const s = normalizeSettings(null);
    expect(s.theme).toBe("system");
    const s2 = normalizeSettings(42);
    expect(s2.ai.profiles).toEqual([]);
  });

  it("缺少 ai 字段时提供安全默认（SettingsPage 不会访问 undefined.ai）", () => {
    const s = normalizeSettings({ theme: "light", libraryRoot: "/x" });
    expect(s.ai).toBeDefined();
    expect(s.ai.videoTagging).toBe(false);
    expect(s.theme).toBe("light");
    expect(s.libraryRoot).toBe("/x");
  });

  it("normalize profile 字段：缺省 model 用默认，kind apiMode 兜底", () => {
    const s = normalizeSettings({
      ai: {
        profiles: [{ id: "p1", name: "A" }],
        activeProfile: "p1",
        videoTagging: true,
        batchLimit: 30,
      },
    });
    expect(s.ai.profiles).toHaveLength(1);
    expect(s.ai.profiles[0].model).toBe(DEFAULT_MODEL);
    expect(s.ai.profiles[0].apiMode).toBe("openai");
    expect(s.ai.profiles[0].kind).toBe("cloud");
    expect(s.ai.activeProfile).toBe("p1");
    expect(s.ai.videoTagging).toBe(true);
    expect(s.ai.batchLimit).toBe(30);
  });

  it("activeProfile 指向不存在配置时回退为空（SettingsPage 会回退到第一套）", () => {
    const s = normalizeSettings({
      ai: { profiles: [{ id: "p1" }], activeProfile: "ghost" },
    });
    expect(s.ai.activeProfile).toBe("");
  });

  it("F8：老 JSON 里的 aiFacetConfigs 被直接丢弃（类型层已删）", () => {
    const s = normalizeSettings({
      aiFacetConfigs: [{ facetKey: "color", hint: "主色调", enabledForAi: true }],
    }) as unknown as Record<string, unknown>;
    expect(s.aiFacetConfigs).toBeUndefined();
    expect(s.tagCategories).toEqual([]);
  });

  it("在线与本机每轮处理数量分别校验并保留合法值", () => {
    const s = normalizeSettings({
      theme: "system",
      thumbnailCacheMb: -5,
      trashRetentionDays: -1,
      ai: { batchLimit: 0, localBatchLimit: 0 },
    });
    expect(s.thumbnailCacheMb).toBe(0);
    expect(s.trashRetentionDays).toBe(0);
    // FB3-07：0 越界 → 默认 30（旧语义 Math.max(1,·)→1 会写入运行时必被 clamp 的值）
    expect(s.ai.batchLimit).toBe(DEFAULT_BATCH_LIMIT);
    expect(s.ai.localBatchLimit).toBe(DEFAULT_LOCAL_BATCH_LIMIT);
    // 历史 500（v2.5 遗留）→ 归一默认 30
    const s500 = normalizeSettings({ ai: { batchLimit: 500 } });
    expect(s500.ai.batchLimit).toBe(DEFAULT_BATCH_LIMIT);
    // 合法区间内保留
    const s20 = normalizeSettings({ ai: { batchLimit: 20 } });
    expect(s20.ai.batchLimit).toBe(20);
    const local = normalizeSettings({ ai: { localBatchLimit: 3 } });
    expect(local.ai.localBatchLimit).toBe(3);
    const localTooLarge = normalizeSettings({ ai: { localBatchLimit: 21 } });
    expect(localTooLarge.ai.localBatchLimit).toBe(DEFAULT_LOCAL_BATCH_LIMIT);
  });

  it("未知 theme 值回退 system", () => {
    expect(normalizeSettings({ theme: "blue" }).theme).toBe("system");
  });

  it("未知日志级别回退 info，debug/trace 保留", () => {
    expect(normalizeSettings({ logLevel: "verbose" }).logLevel).toBe("info");
    expect(normalizeSettings({ logLevel: "debug" }).logLevel).toBe("debug");
    expect(normalizeSettings({ logLevel: "trace" }).logLevel).toBe("trace");
  });

  it("FB2-01：appearance 完全缺失 → 全默认（hoverEnabled 默认 true）", () => {
    const s = normalizeSettings(undefined);
    expect(s.appearance.grid.libraryCellStep).toBe(3);
    expect(s.appearance.grid.importCellStep).toBe(1);
    expect(s.appearance.grid.cellAspect).toBe("4:3");
    expect(s.appearance.grid.cellFit).toBe("cover");
    expect(s.appearance.grid.matchDominantColor).toBe(false);
    expect(s.appearance.hoverPreview.enabled).toBe(true);
    expect(s.appearance.hoverPreview.previewSeconds).toBe(3);
    expect(s.appearance.hoverPreview.inLibraryGrid).toBe(true);
    expect(s.appearance.colorStrip.enabled).toBe(true);
    expect(s.appearance.colorStrip.count).toBe(6);
  });

  it("FB2-01：非法 cellAspect → 回落 4:3；cellStep 越界 → 钳制", () => {
    const s = normalizeSettings({
      appearance: {
        grid: { libraryCellStep: 99, importCellStep: -3, cellAspect: "oops", cellFit: "stretch" },
      },
    });
    expect(s.appearance.grid.libraryCellStep).toBe(7);
    expect(s.appearance.grid.importCellStep).toBe(0);
    expect(s.appearance.grid.cellAspect).toBe("4:3");
    expect(s.appearance.grid.cellFit).toBe("cover");
  });

  it("FB2-03：previewSeconds=99 → 钳制 10；hoverPreview.enabled 缺失 → true", () => {
    const s = normalizeSettings({
      appearance: { hoverPreview: { previewSeconds: 99 } },
    });
    expect(s.appearance.hoverPreview.previewSeconds).toBe(10);
    expect(s.appearance.hoverPreview.enabled).toBe(true);
  });

  it("FB2-07：videoTaggingMode 非法 → cover；videoFrameCount 钳制 2..=8", () => {
    const s = normalizeSettings({
      ai: { videoTaggingMode: "bad", videoFrameCount: 99, videoTagging: true },
    });
    expect(s.ai.videoTaggingMode).toBe("cover");
    expect(s.ai.videoFrameCount).toBe(8);
  });

  it("FB2-01：cellStep 越界钳制、非法 cellFit 回落 cover", () => {
    const s = normalizeSettings({
      appearance: {
        grid: { libraryCellStep: 99, importCellStep: -5, cellFit: "stretch" },
      },
    });
    expect(s.appearance.grid.libraryCellStep).toBe(CELL_STEPS.length - 1);
    expect(s.appearance.grid.importCellStep).toBe(0);
    expect(s.appearance.grid.cellFit).toBe("cover");
  });

  it("FB2-08：colorStrip 默认值（grid 关/viewer 开/count 6），非法 count → 6、mode 回落 ratio", () => {
    const s = normalizeSettings({});
    const cs = s.appearance.colorStrip;
    expect(cs.showInLibraryGrid).toBe(false);
    expect(cs.showInViewer).toBe(true);
    expect(cs.count).toBe(6);
    const s2 = normalizeSettings({ appearance: { colorStrip: { count: 99, mode: "bad" } } });
    expect(s2.appearance.colorStrip.count).toBe(6);
    expect(s2.appearance.colorStrip.mode).toBe("ratio");
  });
});

describe("W0-6 死配置清理", () => {
  it("老 JSON 含 autoTagging/localModelTier 时安全忽略，不再出现在产物中", () => {
    const s = normalizeSettings({
      ai: {
        profiles: [],
        activeProfile: "",
        autoTagging: true,
        localModelTier: "standard",
        videoTagging: false,
        batchLimit: 30,
      },
    });
    expect("autoTagging" in s.ai).toBe(false);
    expect("localModelTier" in s.ai).toBe(false);
    expect(s.ai.batchLimit).toBe(30);
  });
});
