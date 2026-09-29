/** AI 打标状态：批次列表 + 当前批次建议 + 从素材库带过来的待打标选择 */
import { create } from "zustand";
import {
  aiCancelBatch,
  aiConfirmAll,
  aiConfirmSuggestion,
  aiCreateBatch,
  aiListBatches,
  aiListSuggestions,
  aiRejectSuggestion,
  aiRestoreSuggestion,
  aiStartBatch,
} from "@/api/ai";
import type { AiBatch, AiSuggestion, CategorizedTags } from "@/types/ai";

interface AiState {
  batches: AiBatch[];
  currentBatchId: number | null;
  suggestions: AiSuggestion[];
  running: boolean;
  /** 取消已受理、当前请求尚未结束（P2-01：诚实告知用户取消语义） */
  cancelling: boolean;
  error: string | null;
  /** 当前一次执行的范围；与持久化批次累计计数分开保存。 */
  runProgress: {
    batchId: number;
    processed: number;
    total: number;
    currentAssetId: number | null;
    batchProcessedAtStart: number;
    requestedLimit: number | null;
  } | null;
  /** 最近一次进度事件上报的当前素材 id（FB6 需求一：侧栏 LED 提示用它查当前素材名；无事件时为 null） */
  lastProgressAssetId: number | null;
  /** 素材库「打标」带过来的选中素材（跨页传递） */
  pendingAssetIds: number[];
  setPendingAssets: (ids: number[]) => void;
  refreshBatches: () => Promise<void>;
  openBatch: (batchId: number) => Promise<void>;
  /** 只建批次不调 AI：跳转后图片立即上胶片条，AI 建议与手工填写共用工作台 */
  createBatch: () => Promise<void>;
  /** 云端批次手动启动（左栏「开始打标」按钮） */
  startBatch: (limit?: number) => Promise<void>;
  /** 撤销拒绝（v2.11） */
  restore: (id: number) => Promise<void>;
  cancel: () => Promise<void>;
  confirm: (id: number, tags: CategorizedTags, description?: string) => Promise<void>;
  reject: (id: number) => Promise<void>;
  confirmAll: () => Promise<void>;
  patchProgress: (progress: { batchId: number; processed: number; total: number; currentAssetId: number }) => void;
}

export const useAiStore = create<AiState>((set, get) => ({
  batches: [],
  currentBatchId: null,
  suggestions: [],
  running: false,
  cancelling: false,
  error: null,
  runProgress: null,
  lastProgressAssetId: null,
  pendingAssetIds: [],

  setPendingAssets: (ids) => set({ pendingAssetIds: ids }),

  createBatch: async () => {
    const ids = get().pendingAssetIds;
    if (ids.length === 0) return;
    // 阶段 5 §8.1/§8.3：所选素材完整进入逻辑批次，不做静默截断。
    // 「批量上限」已改名为「执行分块大小」，仅指执行层内存分块，不再限制总批次。
    set({ pendingAssetIds: [], error: null });
    try {
      const batch = await aiCreateBatch(ids);
      set((s) => ({ batches: [batch, ...s.batches], currentBatchId: batch.id }));
      await get().openBatch(batch.id);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  startBatch: async (limit) => {
    const state = get();
    const batchId = state.currentBatchId;
    if (!batchId) return;
    const batchProcessedAtStart = state.batches.find((batch) => batch.id === batchId)?.processed ?? 0;
    // 页面可立即显示启动状态；真实本次总数由后端在过滤 pending 后的首个事件提供。
    set({
      running: true,
      error: null,
      cancelling: false,
      lastProgressAssetId: null,
      runProgress: {
        batchId,
        processed: 0,
        total: 0,
        currentAssetId: null,
        batchProcessedAtStart,
        requestedLimit: limit ?? null,
      },
    });
    try {
      const batch = await aiStartBatch(batchId, limit);
      set((s) => ({
        batches: s.batches.map((b) => (b.id === batch.id ? batch : b)),
        runProgress:
          s.runProgress?.batchId === batch.id
            ? {
                ...s.runProgress,
                // 命令响应是后端完成态的最终计数快照，弥补事件派发晚于 IPC 响应的竞态。
                processed: Math.max(
                  s.runProgress.processed,
                  Math.max(0, batch.processed - s.runProgress.batchProcessedAtStart),
                ),
              }
            : s.runProgress,
      }));
      await get().openBatch(batchId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    } finally {
      // P2-01：命令返回 = 当前图片已完成、批次已收尾（cancelled/done），取消状态复位
      set({ running: false, cancelling: false });
    }
  },

  refreshBatches: async () => {
    try {
      set({ batches: await aiListBatches() });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  openBatch: async (batchId) => {
    const requestId = ++latestOpenBatchRequest;
    set({ currentBatchId: batchId });
    try {
      const list = await aiListSuggestions(batchId);
      // 同时守住跨批次与同批次竞态：进度事件会连续触发回载，较早请求可能更晚返回，
      // 只有最近发起的请求可以写入，避免旧快照把刚出现的标签反盖成空白。
      if (get().currentBatchId !== batchId || requestId !== latestOpenBatchRequest) return;
      set({ suggestions: list });
    } catch (e) {
      if (get().currentBatchId !== batchId || requestId !== latestOpenBatchRequest) return;
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  cancel: async () => {
    const id = get().currentBatchId;
    if (id == null) return;
    // P2-01：取消只对「执行中」批次有即时语义；标记 cancelling 让 UI 明示
    // 「已受理、当前图片完成后停止」——300s 超时不动（本地 CPU 慢速打标需要），
    // 请求级中断需换 async 客户端（单独立项）
    if (get().running) set({ cancelling: true });
    await aiCancelBatch(id);
  },

  confirm: async (id, tags, description?: string) => {
    await aiConfirmSuggestion(id, tags, description);
    const batchId = get().currentBatchId;
    if (batchId != null) await get().openBatch(batchId);
  },

  reject: async (id) => {
    await aiRejectSuggestion(id);
    const batchId = get().currentBatchId;
    if (batchId != null) await get().openBatch(batchId);
  },

  restore: async (id) => {
    await aiRestoreSuggestion(id);
    const batchId = get().currentBatchId;
    if (batchId != null) await get().openBatch(batchId);
  },

  confirmAll: async () => {
    const batchId = get().currentBatchId;
    if (batchId == null) return;
    try {
      await aiConfirmAll(batchId);
    } finally {
      // 分页确认可能在后续页失败；无论成功失败都回载，展示已经提交的部分。
      await Promise.all([get().refreshBatches(), get().openBatch(batchId)]);
    }
  },

  patchProgress: (progress) => {
    const state = get();
    // 只接受当前执行批次的事件；旧批次或迟到事件不能串入正在查看的另一个批次。
    if (!state.running || state.runProgress?.batchId !== progress.batchId) return;
    set((s) => ({
      runProgress: {
        ...s.runProgress!,
        processed: progress.processed,
        total: progress.total,
        currentAssetId: progress.currentAssetId,
      },
      lastProgressAssetId:
        s.currentBatchId === progress.batchId ? progress.currentAssetId : s.lastProgressAssetId,
    }));
    // 执行中实时回载建议（1s 节流）：否则中间大图一直显示旧的空标签，进度动了却看不到结果
    const now = Date.now();
    const batchId = progress.batchId;
    if (get().currentBatchId === batchId && now - lastLiveReloadAt > 1000) {
      lastLiveReloadAt = now;
      void get().openBatch(batchId);
    }
  },
}));

/** 进度事件驱动的 suggestions 实时回载节流时间戳（模块级，跨 set 调用共享） */
let lastLiveReloadAt = 0;
/** 最近一次建议回载请求序号；防止同一批次的旧快照晚到后覆盖新快照。 */
let latestOpenBatchRequest = 0;
