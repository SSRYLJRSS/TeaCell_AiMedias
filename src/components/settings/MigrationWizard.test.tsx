/**
 * 总仓迁移向导（A4-05）验收测试。
 *
 * 覆盖卡片与实现契约要求的关键行为：
 * 1. 两条入口互不替代（软件迁移 / 已搬好的重新定位），文案与提示各自独立；
 * 2. 预检之前不允许「开始」——卡片禁止用文本框改路径直接提交；
 * 3. 预检结果如实展示数量/容量；`targetFreeBytes === null` 必须说「无法预检」，
 *    绝不能显示成「空间足够」；
 * 4. `switched` / `cleaning` 是可恢复阶段，不是失败：给出「继续收尾」；
 * 5. 失败项逐条列出原因，不折叠成「部分失败」。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import MigrationWizard, { formatBytes } from "@/components/settings/MigrationWizard";
import type {
  LibraryMigrationPreflight,
  LibraryMigrationReport,
} from "@/api/libraryMaintenance";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("@/api/libraryMaintenance", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/libraryMaintenance")>();
  return {
    ...actual,
    preflightLibraryMigration: vi.fn(),
    startLibraryMigration: vi.fn(),
    cancelLibraryMigration: vi.fn(),
    libraryMaintenanceStatus: vi.fn(),
    libraryTaskImpact: vi.fn(),
    resumeLibraryMigration: vi.fn(),
  };
});

import { open as pickDir } from "@tauri-apps/plugin-dialog";
import {
  cancelLibraryMigration,
  libraryMaintenanceStatus,
  libraryTaskImpact,
  preflightLibraryMigration,
  resumeLibraryMigration,
  startLibraryMigration,
} from "@/api/libraryMaintenance";
import type { MigrationTaskImpact } from "@/api/libraryMaintenance";

const preflight = vi.mocked(preflightLibraryMigration);
const start = vi.mocked(startLibraryMigration);
const cancel = vi.mocked(cancelLibraryMigration);
const status = vi.mocked(libraryMaintenanceStatus);
const impact = vi.mocked(libraryTaskImpact);
const resume = vi.mocked(resumeLibraryMigration);
const pick = vi.mocked(pickDir);

/** 维护期一定暂停的写入动作说明。后端把它作为最后一条消息附在影响面里。 */
const SUSPENDED_HINT =
  "迁移期间入库、复制导出、移动、删除/清空回收站、AI 打标和设置保存都会被暂停；浏览、搜索与旧会话仍然可用。";

function mkImpact(over: Partial<MigrationTaskImpact> = {}): MigrationTaskImpact {
  return {
    runningOperations: 0,
    runningByCapability: [],
    pendingAiBatches: 0,
    importRunning: false,
    refillRunning: false,
    runningExports: 0,
    blocking: false,
    messages: [SUSPENDED_HINT],
    ...over,
  };
}

function mkPreflight(over: Partial<LibraryMigrationPreflight> = {}): LibraryMigrationPreflight {
  return {
    runId: "migration_0001",
    kind: "migrate",
    libraryId: "library_0001",
    sourceRoot: "F:/old",
    targetRoot: "G:/new",
    totalItems: 12,
    totalBytes: 3 * 1024 * 1024 * 1024,
    targetFreeBytes: 100 * 1024 * 1024 * 1024,
    notes: ["源目录在核验通过后仍然完整保留；本流程不会删除搬运来源。"],
    warnings: [],
    impact: mkImpact(),
    ...over,
  };
}

function mkReport(over: Partial<LibraryMigrationReport> = {}): LibraryMigrationReport {
  return {
    runId: "migration_0001",
    kind: "migrate",
    phase: "completed",
    sourceRoot: "F:/old",
    targetRoot: "G:/new",
    totalItems: 12,
    verifiedItems: 12,
    failedItems: 0,
    errorCode: null,
    errorMessage: null,
    failed: [],
    items: [],
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  status.mockResolvedValue({ active: false, reason: null, migration: null });
  impact.mockResolvedValue(mkImpact());
});

describe("MigrationWizard", () => {
  it("两条入口互不替代，且没有可手写路径的输入框", async () => {
    render(<MigrationWizard open onClose={() => undefined} />);
    // label 同时包含标题与说明，故用子串匹配而不是全等。
    expect(screen.getByLabelText(/让软件迁移总仓/)).toBeTruthy();
    expect(screen.getByLabelText(/我已经搬好了，重新定位/)).toBeTruthy();
    // 卡片禁止「用文本框改路径直接提交」：目标位置只能是只读展示。
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.getByTestId("migration-target-root").textContent).toContain("尚未选择");
    await waitFor(() => expect(status).toHaveBeenCalled());
  });

  it("预检通过之前「开始迁移」不可用，预检后展示数量与容量", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    render(<MigrationWizard open onClose={() => undefined} />);

    const startButton = screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement;
    expect(startButton.disabled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );

    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    expect(preflight).toHaveBeenCalledWith("G:/new", "migrate");
    expect(screen.getByText("12 个文件")).toBeTruthy();
    expect(screen.getByText("3.0 GiB")).toBeTruthy();
    expect(screen.getByText("100.0 GiB")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("无法预检剩余空间时明确说「无法预检」，不暗示空间足够", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight({ targetFreeBytes: null }));
    render(<MigrationWizard open onClose={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() => expect(preflight.mock.calls.length === 0).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    expect(screen.getByText("当前平台无法预检，请自行确认")).toBeTruthy();
  });

  it("预检警告逐条展示，不折叠", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(
      mkPreflight({
        warnings: [
          { code: "INSUFFICIENT_SPACE", message: "无法读取目标盘剩余空间，请在确认前自行核对容量" },
        ],
      }),
    );
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() => expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"));
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByTestId("migration-warnings")).toBeTruthy());
    expect(
      screen.getByText(/无法读取目标盘剩余空间，请在确认前自行核对容量/).textContent,
    ).toContain("INSUFFICIENT_SPACE");
  });

  it("切换操作类型会作废旧预检，必须重新检查", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() => expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"));
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());

    fireEvent.click(screen.getByLabelText(/我已经搬好了，重新定位/));
    expect(screen.queryByLabelText("迁移预检")).toBeNull();
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("完成后说明源目录仍然保留，并列出失败项原因", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    start.mockResolvedValue(
      mkReport({
        phase: "failed",
        verifiedItems: 10,
        failedItems: 2,
        errorCode: "CHECKSUM_MISMATCH",
        errorMessage: "有 2 个文件未能通过核验，总仓位置未切换；源目录仍完整可用",
        failed: [
          {
            itemId: "path_0000000000000001",
            relativePath: "2024/a.jpg",
            byteSize: 1024,
            state: "failed",
            errorCode: "CHECKSUM_MISMATCH",
          },
          {
            itemId: "path_0000000000000002",
            relativePath: "2024/b.jpg",
            byteSize: 2048,
            state: "failed",
            errorCode: "UNKNOWN_NEW_CODE" as never,
          },
        ],
      }),
    );
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() => expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"));
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始迁移" }));

    await waitFor(() => expect(screen.getByTestId("migration-report")).toBeTruthy());
    expect(start).toHaveBeenCalledWith("G:/new", "migrate");
    expect(screen.getByText("已中止（总仓位置未改变）")).toBeTruthy();
    expect(screen.getByText(/有 2 个文件未能通过核验/)).toBeTruthy();
    expect(screen.getByText("2024/a.jpg · 复制后的内容校验不一致")).toBeTruthy();
    // 未知错误码原样回显，便于用户报障。
    expect(screen.getByText("2024/b.jpg · UNKNOWN_NEW_CODE")).toBeTruthy();
  });

  it("打开时若读到「已切换未收尾」，提供继续收尾而不是重新开始", async () => {
    status.mockResolvedValue({
      active: false,
      reason: null,
      migration: mkReport({ phase: "switched", verifiedItems: 12, totalItems: 12 }),
    });
    resume.mockResolvedValue(["migration_0001"]);
    render(<MigrationWizard open onClose={() => undefined} />);

    await waitFor(() => expect(screen.getByTestId("migration-report")).toBeTruthy());
    // 「阶段：」与阶段文案在同一 <p> 里被 React 拆成两个文本节点，用容器文本断言。
    expect(screen.getByTestId("migration-report").textContent).toContain(
      "已切换总仓位置，正在重建索引",
    );
    // 已切换就不能重来：开始按钮被禁，只留「继续收尾」。
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(true);

    status.mockResolvedValue({ active: false, reason: null, migration: null });
    fireEvent.click(screen.getByRole("button", { name: "继续收尾" }));
    await waitFor(() => expect(resume).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByText("已收尾 1 次未完成的迁移。")).toBeTruthy());
  });

  it("执行中可请求停止，且提示不会留下半截文件", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    let release: (value: LibraryMigrationReport) => void = () => undefined;
    start.mockImplementation(
      () => new Promise<LibraryMigrationReport>((resolve) => {
        release = resolve;
      }),
    );
    cancel.mockResolvedValue(true);
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() => expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"));
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始迁移" }));

    await waitFor(() => expect(screen.getByRole("button", { name: "停止" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith("G:/new"));
    await waitFor(() => expect(screen.getByText(/正在复制的那一个文件会写完再停/)).toBeTruthy());

    release(mkReport({ phase: "aborted", verifiedItems: 3 }));
    await waitFor(() => expect(screen.getByText("已取消（总仓位置未改变）")).toBeTruthy());
  });

  it("展示任务影响，并在有未结束任务时禁用「开始迁移」", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(
      mkPreflight({
        impact: mkImpact({
          runningOperations: 2,
          runningByCapability: [{ capability: "tagging.submit", count: 2 }],
          blocking: true,
          messages: [
            "还有 2 个未结束的任务（AI 打标 × 2）。请等它们结束，或先取消后再开始迁移。",
            SUSPENDED_HINT,
          ],
        }),
      }),
    );
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("任务影响")).toBeTruthy());

    const panel = screen.getByTestId("migration-impact");
    expect(panel.textContent).toContain("还有 2 个未结束的任务（AI 打标 × 2）");
    // 维护期暂停哪些动作也要说清楚，否则用户会以为整块功能被关掉。
    expect(panel.textContent).toContain("浏览、搜索与旧会话仍然可用");
    // 预检通过了，但因为有未结束任务，开始必须仍然不可用。
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("没有未结束任务时「开始迁移」可用，并展示固定说明", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    render(<MigrationWizard open onClose={() => undefined} />);
    await waitFor(() => expect(screen.getByLabelText("任务影响")).toBeTruthy());
    expect(screen.getByTestId("migration-impact").textContent).toContain(SUSPENDED_HINT);

    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("部分失败后给出明确的下一步，而不是只报错", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    start.mockResolvedValue(
      mkReport({
        phase: "completed",
        verifiedItems: 11,
        failedItems: 1,
        failed: [
          {
            itemId: "path_0000000000000003",
            relativePath: "2024/c.jpg",
            byteSize: 4096,
            state: "failed",
            errorCode: "SOURCE_MISSING",
          },
        ],
      }),
    );
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始迁移" }));

    await waitFor(() => expect(screen.getByTestId("migration-next-step")).toBeTruthy());
    expect(screen.getByTestId("migration-next-step").textContent).toContain("定位未核验");
    // 有失败项时不能说「一切正常，可以处置旧目录」。
    expect(screen.getByTestId("migration-report").textContent).not.toContain(
      "确认新位置一切正常后，您可以自行处置旧目录",
    );
  });

  it("中止后可以重新检查并再次开始（失败/取消不是死路）", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    start.mockResolvedValue(mkReport({ phase: "aborted", verifiedItems: 0 }));
    render(<MigrationWizard open onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始迁移" }));
    await waitFor(() => expect(screen.getByTestId("migration-next-step")).toBeTruthy());
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("重新打开或重启后读到「还在复制」，给出停止入口而不是死路", async () => {
    status.mockResolvedValue({
      active: true,
      reason: "正在复制并校验",
      migration: mkReport({ phase: "copying", verifiedItems: 5, totalItems: 12 }),
    });
    cancel.mockResolvedValue(true);
    render(<MigrationWizard open onClose={() => undefined} />);

    await waitFor(() => expect(screen.getByTestId("migration-report")).toBeTruthy());
    // 上次的迁移摘要被恢复（目标位置也一并恢复），不能要求用户重新选目录。
    expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new");
    // 不能新建一次重复迁移，但也不能只剩一个禁用的「开始迁移」。
    expect(screen.queryByRole("button", { name: "开始迁移" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith("G:/new"));
  });

  it("另一个任务正在维护时给可等待的说明，而不是让按钮无响应或报通用错误", async () => {
    status.mockResolvedValue({
      active: true,
      reason: "正在迁移总仓到 G:/new",
      migration: null,
    });
    render(<MigrationWizard open onClose={() => undefined} />);

    const hold = await screen.findByTestId("migration-maintenance-hold");
    expect(hold.textContent).toContain("另一个任务正在维护总仓");
    // 必须说清「在等谁」，否则用户只知道按钮点不动。
    expect(hold.textContent).toContain("正在迁移总仓到 G:/new");
    expect(hold.textContent).toContain("可等待");
    // 这是等一等，不是出错：不能同时给一条红色报错。
    expect(screen.queryByRole("alert")).toBeNull();
    // 检查与开始都必然被后端拒绝，前端先停掉，别让用户白点。
    expect((screen.getByRole("button", { name: "检查" }) as HTMLButtonElement).disabled).toBe(true);
    expect(
      (screen.getByRole("button", { name: "开始迁移" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("维护结束后可原地重新检查，不需要关窗重开", async () => {
    status.mockResolvedValue({
      active: true,
      reason: "正在迁移总仓到 G:/new",
      migration: null,
    });
    pick.mockResolvedValue("G:/new");
    render(<MigrationWizard open onClose={() => undefined} />);
    await screen.findByTestId("migration-maintenance-hold");

    // 另一个任务收尾了。
    status.mockResolvedValue({ active: false, reason: null, migration: null });
    fireEvent.click(screen.getByRole("button", { name: "重新检查维护状态" }));

    await waitFor(() =>
      expect(screen.queryByTestId("migration-maintenance-hold")).toBeNull(),
    );
    expect(
      screen.getByText("现在没有其他任务在维护总仓，可以重新检查并开始。"),
    ).toBeTruthy();

    // 维护结束后检查按钮恢复可用（此时还没选目录，选完就能检查）。
    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    expect((screen.getByRole("button", { name: "检查" }) as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  it("预检撞上维护占用时按可等待处理，不报成目标目录有问题", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockRejectedValue(
      new Error("总仓维护正在进行，暂时无法开始总仓迁移；正在迁移总仓到 G:/new"),
    );
    // 打开时还没有人维护；点检查时另一个任务已经拿走了维护门。
    status.mockResolvedValueOnce({ active: false, reason: null, migration: null });
    status.mockResolvedValue({
      active: true,
      reason: "正在迁移总仓到 G:/new",
      migration: null,
    });
    render(<MigrationWizard open onClose={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));

    await waitFor(() => expect(screen.getByTestId("migration-maintenance-hold")).toBeTruthy());
    // 绝不能说成「预检未通过」：用户没选错，只是现在轮不到他。
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/预检未通过/)).toBeNull();
    // 目标位置要留着，维护结束后不必重新选。
    expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new");
  });

  it("开始被维护门拒绝时同样按可等待展示，不把已通过的预检说成失败", async () => {
    pick.mockResolvedValue("G:/new");
    preflight.mockResolvedValue(mkPreflight());
    start.mockRejectedValue(
      new Error("总仓维护已由另一个任务持有，请等它结束或收尾后再试；正在迁移总仓到 G:/new"),
    );
    status.mockResolvedValueOnce({ active: false, reason: null, migration: null });
    status.mockResolvedValue({
      active: true,
      reason: "正在迁移总仓到 G:/new",
      migration: null,
    });
    render(<MigrationWizard open onClose={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "选择目录…" }));
    await waitFor(() =>
      expect(screen.getByTestId("migration-target-root").textContent).toContain("G:/new"),
    );
    fireEvent.click(screen.getByRole("button", { name: "检查" }));
    await waitFor(() => expect(screen.getByLabelText("迁移预检")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "开始迁移" }));

    await waitFor(() => expect(screen.getByTestId("migration-maintenance-hold")).toBeTruthy());
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/迁移未完成/)).toBeNull();
  });

  it("formatBytes 用二进制单位且不把 0 显示成负数", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(-5)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1024 * 1024 * 1024)).toBe("1.0 GiB");
  });
});
