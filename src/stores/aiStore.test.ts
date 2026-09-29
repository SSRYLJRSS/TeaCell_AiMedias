/**
 * aiStore 测试：统一打标状态机
 * - createBatch 统一建批：不调 AI、建议占位载入
 * - confirm/reject 后重载建议（openBatch）
 * mock 掉 @/api/ai 的 invoke 封装。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  aiCancelBatch,
  aiConfirmAll,
  aiConfirmSuggestion,
  aiCreateBatch,
  aiListSuggestions,
  aiRejectSuggestion,
  aiRestoreSuggestion,
  aiStartBatch,
} from "@/api/ai";
import { useAiStore } from "@/stores/aiStore";
import { useSettingsStore } from "@/stores/settingsStore";
import type { AiBatch, AiSuggestion } from "@/types/ai";
import type { Settings } from "@/types/settings";

vi.mock("@/api/ai", () => ({
  aiCancelBatch: vi.fn(),
  aiConfirmAll: vi.fn(),
  aiConfirmSuggestion: vi.fn(),
  aiCreateBatch: vi.fn(),
  aiListSuggestions: vi.fn(),
  aiRejectSuggestion: vi.fn(),
  aiRestoreSuggestion: vi.fn(),
  aiStartBatch: vi.fn(),
}));

const mkBatch = (id: number, over: Partial<AiBatch> = {}): AiBatch => ({
  id,
  status: "pending",
  mode: "cloud",
  total: 1,
  processed: 0,
  confirmed: 0,
  createdAt: id,
  ...over,
});

const mkSuggestion = (id: number, over: Partial<AiSuggestion> = {}): AiSuggestion => ({
  id,
  batchId: 1,
  assetId: 100 + id,
  assetPath: `d:/lib/s${id}.jpg`,
  mimeType: "image/jpeg",
  suggestedTags: {},
  status: "pending",
  confirmedTags: {},
  lastError: null,
  createdAt: id,
  ...over,
});

const IDs = [1, 2, 3];

/** 设置夹具：batchLimit 可覆盖 */
const mkSettings = (over: Partial<Settings["ai"]> = {}): Settings => ({
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
    ...over,
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
});

beforeEach(() => {
  vi.clearAllMocks();
  useAiStore.setState({
    batches: [],
    currentBatchId: null,
    suggestions: [],
    running: false,
    cancelling: false,
    error: null,
    runProgress: null,
    pendingAssetIds: [],
  });
  useSettingsStore.setState({ settings: mkSettings(), loaded: true, loadError: null, saving: false });
});

describe("aiStore 打标状态机", () => {
  it("createBatch 统一建批：不调 AI、建议占位载入、带过来的选择清空", async () => {
    vi.mocked(aiCreateBatch).mockResolvedValue(mkBatch(1, { status: "pending" }));
    vi.mocked(aiListSuggestions).mockResolvedValue([mkSuggestion(101), mkSuggestion(102)]);

    useAiStore.setState({ pendingAssetIds: IDs });
    await useAiStore.getState().createBatch();

    const s = useAiStore.getState();
    expect(s.currentBatchId).toBe(1);
    expect(s.pendingAssetIds).toEqual([]); // 带过去的选中清空
    expect(s.batches[0].status).toBe("pending");
    expect(s.suggestions).toHaveLength(2);
    expect(aiCreateBatch).toHaveBeenCalledWith(IDs);
    expect(aiStartBatch).not.toHaveBeenCalled();
  });

  it("createBatch 空选中：直接返回，不调后端", async () => {
    await useAiStore.getState().createBatch();
    expect(aiCreateBatch).not.toHaveBeenCalled();
  });

  it("startBatch 云端：置 running、写回批次状态、重载建议", async () => {
    useAiStore.setState({ currentBatchId: 1, batches: [mkBatch(1)] });
    vi.mocked(aiStartBatch).mockResolvedValue(mkBatch(1, { status: "done", processed: 1 }));
    vi.mocked(aiListSuggestions).mockResolvedValue([mkSuggestion(101)]);

    await useAiStore.getState().startBatch();
    const s = useAiStore.getState();
    expect(s.running).toBe(false); // finally 复位
    expect(s.batches[0].status).toBe("done");
    expect(s.suggestions).toHaveLength(1);
    expect(aiStartBatch).toHaveBeenCalledWith(1, undefined);
  });

  it("confirm 成功：调后端后重载当前批次建议", async () => {
    useAiStore.setState({ currentBatchId: 1, suggestions: [mkSuggestion(101)] });
    vi.mocked(aiConfirmSuggestion).mockResolvedValue(undefined);
    vi.mocked(aiListSuggestions).mockResolvedValue([
      mkSuggestion(101, { status: "confirmed" }),
    ]);

    await useAiStore.getState().confirm(101, { 场景: ["公园"] });
    // FB5-05：第三参为可选 description（缺省 undefined → 后端按 None 处理）
    expect(aiConfirmSuggestion).toHaveBeenCalledWith(101, { 场景: ["公园"] }, undefined);
    expect(useAiStore.getState().suggestions[0].status).toBe("confirmed");
  });

  it("reject → restore 恢复 pending（防误触 v2.11）", async () => {
    useAiStore.setState({ currentBatchId: 1, suggestions: [mkSuggestion(101)] });
    vi.mocked(aiRejectSuggestion).mockResolvedValue(undefined);
    vi.mocked(aiListSuggestions).mockResolvedValueOnce([
      mkSuggestion(101, { status: "rejected" }),
    ]);
    await useAiStore.getState().reject(101);
    expect(useAiStore.getState().suggestions[0].status).toBe("rejected");

    vi.mocked(aiRestoreSuggestion).mockResolvedValue(undefined);
    vi.mocked(aiListSuggestions).mockResolvedValueOnce([
      mkSuggestion(101, { status: "pending" }),
    ]);
    await useAiStore.getState().restore(101);
    expect(useAiStore.getState().suggestions[0].status).toBe("pending");
  });

  it("confirmAll 无当前批次：不调后端", async () => {
    await useAiStore.getState().confirmAll();
    expect(aiConfirmAll).not.toHaveBeenCalled();
  });

  it("patchProgress：按批次记录本次执行进度，不覆盖批次累计值", async () => {
    useAiStore.setState({
      currentBatchId: 1,
      running: true,
      batches: [mkBatch(1, { processed: 7, total: 205 })],
      runProgress: {
        batchId: 1,
        processed: 0,
        total: 0,
        currentAssetId: null,
        batchProcessedAtStart: 7,
        requestedLimit: 10,
      },
    });
    vi.mocked(aiListSuggestions).mockResolvedValue([mkSuggestion(101)]);
    useAiStore.getState().patchProgress({ batchId: 1, processed: 3, total: 10, currentAssetId: 105 });
    const s = useAiStore.getState();
    expect(s.batches[0].processed).toBe(7);
    expect(s.runProgress).toMatchObject({ batchId: 1, processed: 3, total: 10, currentAssetId: 105 });
    // 首次 patch 应触发重载（>1s 节流窗口）
    expect(aiListSuggestions).toHaveBeenCalledTimes(1);

    useAiStore.getState().patchProgress({ batchId: 2, processed: 1, total: 10, currentAssetId: 206 });
    expect(useAiStore.getState().runProgress).toMatchObject({ batchId: 1, processed: 3, total: 10 });
  });

  it("cancel：仅当有 currentBatchId 才调后端", async () => {
    useAiStore.setState({ currentBatchId: 1 });
    vi.mocked(aiCancelBatch).mockResolvedValue(undefined);
    await useAiStore.getState().cancel();
    expect(aiCancelBatch).toHaveBeenCalledWith(1);

    vi.clearAllMocks();
    useAiStore.setState({ currentBatchId: null });
    await useAiStore.getState().cancel();
    expect(aiCancelBatch).not.toHaveBeenCalled();
  });

  it("P1-02 竞态回归：快速切批次时旧批次慢响应不覆盖新批次", async () => {
    useAiStore.setState({ currentBatchId: 1 });
    // ① openBatch(1) 挂起（慢）
    let resolveA!: (v: AiSuggestion[]) => void;
    vi.mocked(aiListSuggestions).mockImplementationOnce(
      () => new Promise((res) => (resolveA = res)),
    );
    const openA = useAiStore.getState().openBatch(1);
    // ② 切到批次 2 并立即返回（快）
    vi.mocked(aiListSuggestions).mockResolvedValueOnce([mkSuggestion(201, { batchId: 2 })]);
    await useAiStore.getState().openBatch(2);
    expect(useAiStore.getState().suggestions.map((s) => s.id)).toEqual([201]);
    // ③ 批次 1 的慢响应晚到 → 丢弃
    resolveA([mkSuggestion(101, { batchId: 1 })]);
    await openA;
    expect(useAiStore.getState().suggestions.map((s) => s.id)).toEqual([201]);
  });

  it("同批次竞态回归：较早回载的旧快照晚到时不得覆盖新结果", async () => {
    useAiStore.setState({ currentBatchId: 1 });
    let resolveOld!: (v: AiSuggestion[]) => void;
    vi.mocked(aiListSuggestions).mockImplementationOnce(
      () => new Promise((res) => (resolveOld = res)),
    );
    const oldReload = useAiStore.getState().openBatch(1);

    vi.mocked(aiListSuggestions).mockResolvedValueOnce([
      mkSuggestion(102, { suggestedTags: { scene: ["公园"] } }),
    ]);
    await useAiStore.getState().openBatch(1);
    expect(useAiStore.getState().suggestions.map((s) => s.id)).toEqual([102]);

    resolveOld([mkSuggestion(101, { suggestedTags: {} })]);
    await oldReload;
    expect(useAiStore.getState().suggestions.map((s) => s.id)).toEqual([102]);
  });

  it("阶段5 §8.1：所选素材完整进入逻辑批次，不做静默截断", async () => {
    useSettingsStore.setState({ settings: mkSettings({ batchLimit: 2 }) });
    useAiStore.setState({ pendingAssetIds: [1, 2, 3, 4, 5] });
    vi.mocked(aiCreateBatch).mockResolvedValue(mkBatch(1));
    vi.mocked(aiListSuggestions).mockResolvedValue([]);

    await useAiStore.getState().createBatch();

    // 全部 id 进入批次（batchLimit 仅作执行分块大小，不再是总批次上限）
    expect(aiCreateBatch).toHaveBeenCalledWith([1, 2, 3, 4, 5]);
    expect(useAiStore.getState().error).toBeNull();
  });

  it("P2-01 回归：运行中 cancel 标记 cancelling（当前图完成前 UI 明示）", async () => {
    useAiStore.setState({ currentBatchId: 1, running: true });
    vi.mocked(aiCancelBatch).mockResolvedValue(undefined);
    await useAiStore.getState().cancel();
    expect(useAiStore.getState().cancelling).toBe(true);
  });

  it("P2-01 回归：非运行中 cancel 不置 cancelling（避免状态卡死）", async () => {
    useAiStore.setState({ currentBatchId: 1, running: false });
    vi.mocked(aiCancelBatch).mockResolvedValue(undefined);
    await useAiStore.getState().cancel();
    expect(useAiStore.getState().cancelling).toBe(false);
  });
});
